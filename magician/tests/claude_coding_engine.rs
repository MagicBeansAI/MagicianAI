//! Integration tests for the Claude Code VibeDev adapter.
//!
//! Lives outside `magician` lib tests so a dirty in-crate apps test suite
//! cannot block running these. Uses public adapter APIs only.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use magician::magician_v2::execution::coding_engine::claude::{
    filter_claude_child_env, run_turn_over_stdio, ClaudeCodeAdapter, ClaudeSessionLimits,
};
use magician::magician_v2::execution::coding_engine::factory::ClaudeTurnMode;
use magician::magician_v2::execution::coding_engine::{
    CodingEngineEvent, CodingEngineEventKind, CodingEngineKind, CodingEngineRequest,
};
use magician::magician_v2::execution::file_edit::transaction::TransactionScope;
use serde_json::{json, Value};
use tokio::io::{duplex, AsyncWriteExt};

const NATIVE: &str = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";

fn scope() -> TransactionScope {
    TransactionScope {
        principal: "anonymous".to_string(),
        workspace: "default".to_string(),
    }
}

fn request_for(shadow: &std::path::Path) -> CodingEngineRequest {
    let mut request =
        CodingEngineRequest::new("add a comment", "/tmp/real", shadow, "/tmp/scope", scope());
    request.stage_result = true;
    request.timeout = Duration::from_secs(2);
    request
}

fn happy_lines() -> Vec<Value> {
    vec![
        json!({
            "type": "system",
            "subtype": "init",
            "session_id": NATIVE,
            "tools": ["Read", "Bash"],
            "mcp_servers": [],
            "apiKeySource": "none"
        }),
        json!({
            "type": "assistant",
            "session_id": NATIVE,
            "message": {
                "content": [
                    {"type": "text", "text": "hello "},
                    {"type": "text", "text": "world"}
                ]
            }
        }),
        json!({
            "type": "result",
            "subtype": "success",
            "is_error": false,
            "session_id": NATIVE,
            "total_cost_usd": 0.012,
            "usage": {"input_tokens": 10, "output_tokens": 4},
            "result": "hello world"
        }),
    ]
}

async fn write_lines(stream: tokio::io::DuplexStream, lines: Vec<Value>) {
    let (_reader, mut writer) = tokio::io::split(stream);
    for line in lines {
        let mut payload = serde_json::to_vec(&line).unwrap();
        payload.push(b'\n');
        if writer.write_all(&payload).await.is_err() {
            return;
        }
    }
}

#[tokio::test]
async fn fake_turn_maps_events_and_hides_the_native_session() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(32 * 1024);
    let peer = tokio::spawn(write_lines(server, happy_lines()));
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink_seen = seen.clone();
    let mut request = request_for(dir.path());
    request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
        sink_seen.lock().unwrap().push(event.clone());
    }));
    let capture = Arc::new(Mutex::new(None));
    request.usage_capture = Some(capture.clone());
    let result = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
        .await
        .expect("turn");
    let _ = peer.await;
    assert_eq!(result.engine, CodingEngineKind::ClaudeCode);
    assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
    assert!(result
        .session_id
        .as_deref()
        .is_some_and(|id| id.starts_with("claude-") && !id.contains(NATIVE)));
    let continuation = result.continuation.expect("continuation");
    assert_eq!(continuation.engine, CodingEngineKind::ClaudeCode);
    assert_eq!(continuation.native_session_id, NATIVE);
    let events = seen.lock().unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == CodingEngineEventKind::AgentStart));
    assert!(events
        .iter()
        .any(|event| event.kind == CodingEngineEventKind::AgentSettled));
    let usage = capture.lock().unwrap().clone().expect("usage");
    assert!(usage.cost_known);
    assert_eq!(usage.cost, 0.012);
}

#[tokio::test]
async fn omitted_cost_is_unknown_not_zero_billed() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(
        server,
        vec![
            json!({"type": "system", "subtype": "init", "session_id": NATIVE, "tools": ["Read"], "mcp_servers": []}),
            json!({
                "type": "result",
                "subtype": "success",
                "is_error": false,
                "session_id": NATIVE,
                "usage": {"input_tokens": 2, "output_tokens": 1},
                "result": "ok"
            }),
        ],
    ));
    let mut request = request_for(dir.path());
    let capture = Arc::new(Mutex::new(None));
    request.usage_capture = Some(capture.clone());
    run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
        .await
        .expect("turn");
    let _ = peer.await;
    let usage = capture.lock().unwrap().clone().expect("usage");
    assert!(!usage.cost_known);
}

#[tokio::test]
async fn web_search_on_init_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(
        server,
        vec![json!({
            "type": "system",
            "subtype": "init",
            "session_id": NATIVE,
            "tools": ["Read", "WebSearch"],
            "mcp_servers": []
        })],
    ));
    let error = run_turn_over_stdio(
        &request_for(dir.path()),
        client,
        ClaudeSessionLimits::default(),
        false,
    )
    .await
    .expect_err("web search");
    let _ = peer.await;
    let message = error.to_string();
    assert!(message.contains("isolation"), "unexpected error: {message}");
}

#[test]
fn child_env_drops_api_keys_unless_use_api_key() {
    let filtered = filter_claude_child_env(
        [
            ("PATH", "/usr/bin"),
            ("ANTHROPIC_API_KEY", "sk-secret"),
            ("MAGICIAN_FOO", "secret"),
        ],
        false,
    );
    assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
    assert!(!filtered.contains_key("MAGICIAN_FOO"));
    let with_key = filter_claude_child_env(
        [("PATH", "/usr/bin"), ("ANTHROPIC_API_KEY", "sk-secret")],
        true,
    );
    assert_eq!(
        with_key.get("ANTHROPIC_API_KEY").map(String::as_str),
        Some("sk-secret")
    );
}

#[test]
fn qualify_argv_is_stream_json_input_and_never_bare() {
    use magician::magician_v2::execution::coding_engine::claude_contract::{
        qualify_launch_args, qualify_user_line, CLAUDE_DISALLOWED_TOOLS,
    };
    let args = qualify_launch_args();
    assert_eq!(args[0], "-p");
    assert_eq!(args[1], "--output-format");
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--input-format", "stream-json"]));
    assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
    assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--disallowedTools", CLAUDE_DISALLOWED_TOOLS]));
    assert!(!args.iter().any(|arg| arg == "--bare"));
    assert_eq!(qualify_user_line().last().copied(), Some(b'\n'));
}

#[tokio::test]
async fn qualify_over_stdio_stops_at_init() {
    use magician::magician_v2::execution::coding_engine::claude_qualify_worker::{
        claude_qualify_over_stdio, ClaudeQualifyLimits,
    };
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(
        server,
        vec![
            json!({"type": "system", "subtype": "init", "session_id": NATIVE, "tools": ["Read"], "mcp_servers": []}),
            json!({"type": "result", "subtype": "success", "is_error": false, "session_id": NATIVE}),
        ],
    ));
    let evidence = claude_qualify_over_stdio(
        "bin-1",
        Some("2.1.229".into()),
        false,
        client,
        ClaudeQualifyLimits::default(),
    )
    .await
    .expect("init");
    let _ = peer.await;
    assert_eq!(
        evidence.init.get("subtype").and_then(Value::as_str),
        Some("init")
    );
}

#[test]
fn qualify_evidence_without_tool_list_is_not_ready() {
    use magician::magician_v2::execution::coding_engine::claude_qualification::{
        qualify_from_claude_evidence, ClaudeQualifyEvidence,
    };
    use magician::magician_v2::execution::coding_engine::discovery::ClaudeReadiness;
    let receipt = qualify_from_claude_evidence(ClaudeQualifyEvidence {
        identity: "bin-1".into(),
        version: Some("2.1.229".into()),
        use_api_key: false,
        init: json!({"type": "system", "subtype": "init", "mcp_servers": []}),
        cancelled: true,
        canary_mcp_name: None,
    });
    assert_eq!(receipt.readiness, ClaudeReadiness::Unqualified);
}

#[test]
fn qualify_evidence_with_web_search_is_incompatible() {
    use magician::magician_v2::execution::coding_engine::claude_qualification::{
        qualify_from_claude_evidence, ClaudeQualifyEvidence,
    };
    use magician::magician_v2::execution::coding_engine::discovery::ClaudeReadiness;
    let receipt = qualify_from_claude_evidence(ClaudeQualifyEvidence {
        identity: "bin-1".into(),
        version: Some("2.1.229".into()),
        use_api_key: false,
        init: json!({
            "type": "system",
            "subtype": "init",
            "tools": ["Read", "WebSearch"],
            "mcp_servers": []
        }),
        cancelled: true,
        canary_mcp_name: None,
    });
    assert_eq!(receipt.readiness, ClaudeReadiness::Incompatible);
}

#[tokio::test]
async fn resume_without_init_is_continuation_lost() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(server, Vec::new()));
    let mut request = request_for(dir.path());
    request.claude.resume_session_id = Some(NATIVE.to_string());
    request.timeout = Duration::from_millis(200);
    let error = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
        .await
        .expect_err("resume lost");
    let _ = peer.await;
    let message = error.to_string();
    assert!(
        message.contains("continuation is lost") || message.contains("timed out"),
        "{message}"
    );
}

#[tokio::test]
async fn stop_before_init_on_resume_is_cancelled_not_continuation_lost() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(server, Vec::new()));
    let mut request = request_for(dir.path());
    request.claude.resume_session_id = Some(NATIVE.to_string());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    request.cancel_token = Some(token);
    request.timeout = Duration::from_millis(200);
    let error = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
        .await
        .expect_err("cancelled");
    let _ = peer.await;
    let message = error.to_string();
    assert!(
        message.contains("cancelled"),
        "stop must not be rewritten as continuation_lost: {message}"
    );
    assert!(!message.contains("continuation is lost"), "{message}");
}

#[test]
fn discuss_and_build_argv_never_uses_bare() {
    let build = ClaudeCodeAdapter::launch_args(ClaudeTurnMode::Build, "do it", None);
    let discuss = ClaudeCodeAdapter::launch_args(ClaudeTurnMode::Discuss, "plan it", None);
    assert!(build
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions"));
    assert!(!discuss
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions"));
    for args in [&build, &discuss] {
        assert!(args.iter().any(|arg| arg == "--verbose"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
    }
}

/// Live C2/C3 against this machine's `claude` 2.1.229. Isolation writes one
/// stdin user event then kills at `system/init`. Default `cargo test` skips
/// it so the suite stays free; run with `--ignored`.
#[tokio::test]
#[ignore = "live Claude CLI; isolation probe can start a Max-billed model call"]
async fn live_subscription_qualify_is_ready_without_an_api_key() {
    use magician::config::MagicianClaudeSettings;
    use magician::magician_v2::execution::coding_engine::claude_qualify_worker::tick_claude_version_worker;
    use magician::magician_v2::execution::coding_engine::discovery::{
        ClaudeReadiness, ClaudeSearchPaths,
    };

    let settings = MagicianClaudeSettings {
        enabled: true,
        binary: None,
        use_api_key: false,
    };
    let search = ClaudeSearchPaths::production();
    let snapshot = tick_claude_version_worker(&settings, &search).await;
    let public = snapshot.public_json().to_string();
    eprintln!(
        "live claude readiness={:?} selectable={} version={:?} reason={}",
        snapshot.readiness, snapshot.selectable, snapshot.version, snapshot.reason
    );
    assert!(!public.contains("ANTHROPIC"), "{public}");
    assert!(!public.contains("oauthAccount"), "{public}");
    assert!(!public.contains("sk-"), "{public}");
    assert_eq!(
        snapshot.readiness,
        ClaudeReadiness::Ready,
        "{}",
        snapshot.reason
    );
    assert!(snapshot.selectable);
    assert!(
        snapshot
            .version
            .as_deref()
            .is_some_and(magician::magician_v2::execution::coding_engine::claude_contract::claude_version_meets_minimum),
        "version {:?}",
        snapshot.version
    );
}

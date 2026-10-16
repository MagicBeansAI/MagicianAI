//! Integration tests for the Antigravity (`agy`) VibeDev adapter.
//!
//! Lives outside `magician` lib tests so a dirty in-crate apps test suite
//! cannot block running these. Uses public adapter APIs only.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use magician::magician_v2::execution::coding_engine::agy::{
    filter_agy_child_env, run_turn_over_stdio, AgyCliAdapter, AgySessionLimits,
};
use magician::magician_v2::execution::coding_engine::factory::AgyTurnMode;
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
            "event": "init",
            "conversation_id": NATIVE,
            "init": {
                "cwd": "/tmp",
                "tools": ["run_command", "view_file", "write_to_file", "search_web"],
                "permission_mode": "always-proceed"
            }
        }),
        json!({
            "event": "step_update",
            "step_update": {
                "conversation_id": NATIVE,
                "step_index": 2,
                "state": "DONE",
                "step_type": "agent_response",
                "text_delta": "hello world"
            }
        }),
        json!({
            "event": "result",
            "result": {
                "conversation_id": NATIVE,
                "status": "SUCCESS",
                "response": "hello world",
                "usage": {
                    "input_tokens": 10,
                    "output_tokens": 4,
                    "thinking_tokens": 0,
                    "cache_read_tokens": 0,
                    "total_tokens": 14
                }
            }
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
    let result = run_turn_over_stdio(&request, client, AgySessionLimits::default())
        .await
        .expect("turn");
    let _ = peer.await;
    assert_eq!(result.engine, CodingEngineKind::AgyCli);
    assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
    assert!(result
        .session_id
        .as_deref()
        .is_some_and(|id| id.starts_with("agy-") && !id.contains(NATIVE)));
    let continuation = result.continuation.expect("continuation");
    assert_eq!(continuation.engine, CodingEngineKind::AgyCli);
    assert_eq!(continuation.native_session_id, NATIVE);
    let events = seen.lock().unwrap();
    assert!(events
        .iter()
        .any(|event| event.kind == CodingEngineEventKind::AgentStart));
    assert!(events
        .iter()
        .any(|event| event.kind == CodingEngineEventKind::AgentSettled));
    let usage = capture.lock().unwrap().clone().expect("usage");
    assert!(!usage.cost_known);
    assert_eq!(usage.cost, 0.0);
    assert_eq!(usage.input, 10);
}

#[tokio::test]
async fn search_web_on_a_tool_step_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(
        server,
        vec![
            json!({
                "event": "init",
                "conversation_id": NATIVE,
                "init": {
                    "tools": ["run_command", "search_web"],
                    "permission_mode": "always-proceed"
                }
            }),
            json!({
                "event": "step_update",
                "step_update": {
                    "step_type": "tool",
                    "state": "ACTIVE",
                    "tool_name": "search_web",
                    "step_index": 1
                }
            }),
        ],
    ));
    let error = run_turn_over_stdio(
        &request_for(dir.path()),
        client,
        AgySessionLimits::default(),
    )
    .await
    .expect_err("web search");
    let _ = peer.await;
    assert!(error.to_string().contains("isolation"), "{error}");
}

#[tokio::test]
async fn missing_init_on_resume_is_continuation_lost() {
    let dir = tempfile::tempdir().unwrap();
    let (client, server) = duplex(16 * 1024);
    let peer = tokio::spawn(write_lines(server, Vec::new()));
    let mut request = request_for(dir.path());
    request.agy.resume_session_id = Some(NATIVE.to_string());
    request.timeout = Duration::from_millis(200);
    let error = run_turn_over_stdio(&request, client, AgySessionLimits::default())
        .await
        .expect_err("lost");
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
    request.agy.resume_session_id = Some(NATIVE.to_string());
    let token = tokio_util::sync::CancellationToken::new();
    token.cancel();
    request.cancel_token = Some(token);
    request.timeout = Duration::from_millis(200);
    let error = run_turn_over_stdio(&request, client, AgySessionLimits::default())
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
fn discuss_and_build_argv_never_uses_continue() {
    let build = AgyCliAdapter::launch_args(AgyTurnMode::Build, "do it", None);
    let discuss = AgyCliAdapter::launch_args(AgyTurnMode::Discuss, "plan it", None);
    assert!(build
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions"));
    assert!(!discuss
        .iter()
        .any(|arg| arg == "--dangerously-skip-permissions"));
    assert!(discuss.windows(2).any(|pair| pair == ["--mode", "plan"]));
    for args in [&build, &discuss] {
        assert!(args.iter().any(|arg| arg == "--sandbox"));
        assert!(!args.iter().any(|arg| arg == "--continue" || arg == "-c"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--output-format", "stream-json"]));
    }
}

#[test]
fn child_env_drops_magician_and_keeps_google_key_if_present() {
    let filtered = filter_agy_child_env(
        [
            ("PATH", "/usr/bin"),
            ("MAGICIAN_FOO", "secret"),
            ("GEMINI_API_KEY", "gk-secret"),
            ("ANTHROPIC_API_KEY", "sk-nope"),
        ],
        false,
    );
    assert!(filtered.contains_key("PATH"));
    assert!(!filtered.contains_key("MAGICIAN_FOO"));
    assert!(!filtered.contains_key("GEMINI_API_KEY"));
    assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
}

/// Live A2/A3 against this machine's `agy` 1.1.19. Isolation drains init
/// without a user JSONL event, then kills. Default `cargo test` skips it
/// so the suite stays free; run with `--ignored`.
#[tokio::test]
#[ignore = "live Agy CLI; isolation probe waits for init then kills"]
async fn live_agy_qualify_is_ready_without_writing_a_user_line() {
    use magician::config::MagicianAgySettings;
    use magician::magician_v2::execution::coding_engine::agy_qualify_worker::tick_agy_version_worker;
    use magician::magician_v2::execution::coding_engine::discovery::{
        AgyReadiness, AgySearchPaths,
    };

    let settings = MagicianAgySettings {
        enabled: true,
        binary: None,
        use_api_key: false,
    };
    let search = AgySearchPaths::production();
    let snapshot = tick_agy_version_worker(&settings, &search).await;
    let public = snapshot.public_json().to_string();
    eprintln!(
        "live agy readiness={:?} selectable={} version={:?} reason={}",
        snapshot.readiness, snapshot.selectable, snapshot.version, snapshot.reason
    );
    assert!(!public.contains("antigravity-oauth-token"), "{public}");
    assert!(!public.contains("/Users"), "{public}");
    assert!(!public.contains("GEMINI_API_KEY"), "{public}");
    assert_eq!(
        snapshot.readiness,
        AgyReadiness::Ready,
        "{}",
        snapshot.reason
    );
    assert!(snapshot.selectable);
    assert!(
        snapshot.version.as_deref().is_some_and(
            magician::magician_v2::execution::coding_engine::agy_contract::agy_version_meets_minimum
        ),
        "version {:?}",
        snapshot.version
    );
}

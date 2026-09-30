//! Phase C2/C3: background Claude `--version` tick plus a bounded
//! isolation probe.
//!
//! After version + auth would otherwise be Ready, the worker launches the
//! frozen qualify argv with `--input-format stream-json` in a disposable
//! cwd, writes one stdin user event, drains `system/init`, then kills the
//! process group. Missing MCP/tool lists stay Unqualified. Request handlers
//! never call this. The probe session id is not journaled as a VibeDev
//! continuation.
//!
//! Isolation probes are not free: a user JSONL event can start a model
//! call. The worker therefore backs off 5 minutes per identity and kills
//! immediately after init so Magician never waits for `result`.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use serde_json::Value;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};
use uuid::Uuid;

use super::{
    claude::{apply_claude_child_process_group, filter_claude_child_env, terminate_process_group},
    claude_contract::{qualify_launch_args, qualify_user_line},
    claude_qualification::{
        cache_claude_receipt, claude_needs_isolation_attestation, qualify_from_claude_evidence,
        ClaudeQualifyEvidence,
    },
    discovery::{
        observe_claude_readiness, probe_claude_version_in_background, resolved_claude_executable,
        ClaudeReadinessSnapshot, ClaudeSearchPaths,
    },
    jsonl::{read_bounded_jsonl_value, BoundedJsonlError},
};
use crate::{
    config::MagicianClaudeSettings, magician_v2::execution::coding_engine::coding_budget_settings,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MAX_LINE_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_JSON_DEPTH: usize = 32;
const WORKER_INTERVAL: Duration = Duration::from_secs(30);
const ISOLATION_BACKOFF: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy)]
pub struct ClaudeQualifyLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
}

impl Default for ClaudeQualifyLimits {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            max_json_depth: DEFAULT_MAX_JSON_DEPTH,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeQualifyError {
    Timeout,
    Protocol,
    Io,
}

impl std::fmt::Display for ClaudeQualifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Claude qualification session timed out"),
            Self::Protocol => write!(f, "Claude qualification protocol was malformed"),
            Self::Io => write!(f, "Claude qualification session I/O failed"),
        }
    }
}

impl std::error::Error for ClaudeQualifyError {}

/// Production tick: version + auth, then a bounded init probe if that would
/// be Ready. HTTP never calls this.
pub async fn tick_claude_version_worker(
    settings: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
) -> ClaudeReadinessSnapshot {
    tick_claude_qualify_worker(
        settings,
        search,
        |identity, version, binary, use_api_key| async move {
            claude_qualify_child_stdio(
                identity,
                version,
                use_api_key,
                &binary,
                ClaudeQualifyLimits::default(),
            )
            .await
        },
    )
    .await
}

pub async fn tick_claude_qualify_worker<F, Fut>(
    settings: &MagicianClaudeSettings,
    search: &ClaudeSearchPaths,
    run_session: F,
) -> ClaudeReadinessSnapshot
where
    F: FnOnce(String, Option<String>, PathBuf, bool) -> Fut,
    Fut: std::future::Future<Output = Result<ClaudeQualifyEvidence, ClaudeQualifyError>>,
{
    if !settings.enabled {
        return observe_claude_readiness(settings, search);
    }
    let probe_settings = settings.clone();
    let probe_search = search.clone();
    let _ = tokio::task::spawn_blocking(move || {
        probe_claude_version_in_background(&probe_settings, &probe_search);
    })
    .await;
    let snapshot = observe_claude_readiness(settings, search);
    if claude_needs_isolation_attestation(&snapshot)
        && isolation_backoff_elapsed(snapshot.identity())
    {
        if let Some(binary) = resolved_claude_executable(settings, search) {
            record_isolation_attempt(snapshot.identity());
            match run_session(
                snapshot.identity().to_string(),
                snapshot.version.clone(),
                binary,
                settings.use_api_key,
            )
            .await
            {
                Ok(evidence) => {
                    let receipt = qualify_from_claude_evidence(evidence);
                    // Without this the only trace of a failed qualification is an
                    // engine silently missing from VibeDev. Reasons are public-safe;
                    // each distinct outcome is logged once.
                    let failed = !crate::magician_v2::execution::coding_engine::discovery::claude_is_selectable(
                        receipt.readiness,
                    );
                    let outcome =
                        failed.then(|| format!("{:?}: {}", receipt.readiness, receipt.reason));
                    if failed
                        && super::qualify_worker::qualification_outcome_changed(
                            "claude_code",
                            outcome,
                        )
                    {
                        tracing::warn!(engine = "claude_code", readiness = ?receipt.readiness, reason = %receipt.reason, "coding engine did not qualify");
                    } else if !failed {
                        super::qualify_worker::qualification_outcome_changed("claude_code", None);
                    }
                    cache_claude_receipt(receipt)
                },
                Err(error) => {
                    if super::qualify_worker::qualification_outcome_changed(
                        "claude_code",
                        Some(format!("probe failed: {error:?}")),
                    ) {
                        tracing::warn!(engine = "claude_code", error = ?error, "coding engine qualification probe failed");
                    }
                },
            }
        }
    }
    observe_claude_readiness(settings, search)
}

pub fn spawn_claude_version_worker() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return;
    };
    handle.spawn(async {
        let mut interval = tokio::time::interval(WORKER_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            let settings = coding_budget_settings().claude;
            let search = ClaudeSearchPaths::production();
            let _ = tick_claude_version_worker(&settings, &search).await;
        }
    });
}

/// Drain until `system/init`. Never journals the probe session id.
pub async fn claude_qualify_over_stdio<R>(
    identity: impl Into<String>,
    version: Option<String>,
    use_api_key: bool,
    reader: R,
    limits: ClaudeQualifyLimits,
) -> Result<ClaudeQualifyEvidence, ClaudeQualifyError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    let mut reader = BufReader::new(reader);
    let deadline = tokio::time::Instant::now() + limits.request_timeout;
    loop {
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return Err(ClaudeQualifyError::Timeout);
        }
        let remaining = deadline.saturating_duration_since(now);
        let incoming = timeout(
            remaining,
            read_bounded_jsonl_value(&mut reader, limits.max_line_bytes, limits.max_json_depth),
        )
        .await
        .map_err(|_| ClaudeQualifyError::Timeout)?;
        let value = match incoming {
            Ok(value) => value,
            Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => {
                return Err(ClaudeQualifyError::Protocol)
            },
            Err(
                BoundedJsonlError::Oversized
                | BoundedJsonlError::Malformed
                | BoundedJsonlError::TooDeep,
            ) => return Err(ClaudeQualifyError::Protocol),
        };
        if value.get("type").and_then(Value::as_str) == Some("system")
            && value.get("subtype").and_then(Value::as_str) == Some("init")
        {
            return Ok(ClaudeQualifyEvidence {
                identity: identity.into(),
                version,
                use_api_key,
                init: value,
                cancelled: false,
                canary_mcp_name: None,
            });
        }
    }
}

pub async fn claude_qualify_child_stdio(
    identity: String,
    version: Option<String>,
    use_api_key: bool,
    binary: &Path,
    limits: ClaudeQualifyLimits,
) -> Result<ClaudeQualifyEvidence, ClaudeQualifyError> {
    let (cwd, canary) = qualify_cwd()?;
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = filter_claude_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        use_api_key,
    );
    let args = qualify_launch_args();
    debug_assert!(
        args.iter().any(|arg| arg == "--input-format")
            && args.iter().any(|arg| arg == "--no-session-persistence")
            && !args.iter().any(|arg| arg == "--bare")
    );
    let mut command = Command::new(binary);
    command
        .args(&args)
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    apply_claude_child_process_group(&mut command);
    let mut child = command.spawn().map_err(|_| ClaudeQualifyError::Io)?;
    let mut stdin = child.stdin.take().ok_or(ClaudeQualifyError::Io)?;
    let stdout = child.stdout.take().ok_or(ClaudeQualifyError::Io)?;
    let write_user = stdin.write_all(&qualify_user_line()).await;
    let _ = stdin.flush().await;
    let mut result = if write_user.is_err() {
        Err(ClaudeQualifyError::Io)
    } else {
        claude_qualify_over_stdio(identity, version, use_api_key, stdout, limits).await
    };
    // Kill while stdin is still open. Closing stdin first can let print
    // mode treat EOF as end-of-prompt and start a billed completion.
    terminate_process_group(&mut child).await;
    drop(stdin);
    if let Ok(evidence) = &mut result {
        evidence.canary_mcp_name = Some(canary);
        evidence.cancelled = true;
    }
    let _ = std::fs::remove_dir_all(&cwd);
    result
}

fn qualify_cwd() -> Result<(PathBuf, String), ClaudeQualifyError> {
    let dir = std::env::temp_dir().join(format!("magician-claude-qualify-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|_| ClaudeQualifyError::Io)?;
    let canary = plant_qualify_canary(&dir)?;
    Ok((dir, canary))
}

fn plant_qualify_canary(cwd: &Path) -> Result<String, ClaudeQualifyError> {
    let name = format!("magician-claude-qualify-canary-{}", Uuid::new_v4());
    let mut servers = serde_json::Map::new();
    servers.insert(
        name.clone(),
        serde_json::json!({ "command": "false", "args": [] }),
    );
    let body = serde_json::json!({ "mcpServers": servers });
    let encoded = serde_json::to_vec_pretty(&body).map_err(|_| ClaudeQualifyError::Protocol)?;
    std::fs::write(cwd.join(".mcp.json"), encoded).map_err(|_| ClaudeQualifyError::Io)?;
    Ok(name)
}

fn isolation_backoff_elapsed(identity: &str) -> bool {
    match isolation_backoff_slot().get(identity) {
        None => true,
        Some(at) => at.elapsed() >= ISOLATION_BACKOFF,
    }
}

fn record_isolation_attempt(identity: &str) {
    isolation_backoff_slot().insert(identity.to_string(), Instant::now());
}

fn isolation_backoff_slot() -> std::sync::MutexGuard<'static, HashMap<String, Instant>> {
    static SLOT: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};
    use tokio::io::{duplex, AsyncWriteExt};

    use super::*;
    use crate::magician_v2::execution::coding_engine::claude::filter_claude_child_env;

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
    async fn qualify_over_stdio_stops_at_init_and_ignores_later_result() {
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(write_lines(
            server,
            vec![
                json!({"type": "system", "subtype": "init", "session_id": "sess-probe", "tools": ["Read"], "mcp_servers": []}),
                json!({"type": "result", "subtype": "success", "is_error": false, "session_id": "sess-probe"}),
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
        assert_eq!(evidence.identity, "bin-1");
        assert_eq!(
            evidence.init.get("subtype").and_then(Value::as_str),
            Some("init")
        );
        assert!(!evidence.cancelled);
        let rendered = format!("{evidence:?}");
        // Receipt path never stores this; evidence for the classifier may
        // still hold the init object. The worker must not journal it.
        assert!(rendered.contains("sess-probe"));
    }

    #[test]
    fn qualify_child_env_drops_magician_foo_and_api_keys_by_default() {
        let filtered = filter_claude_child_env(
            [
                ("PATH", "/usr/bin"),
                ("HOME", "/Users/me"),
                ("MAGICIAN_FOO", "secret"),
                ("ANTHROPIC_API_KEY", "sk-secret"),
            ],
            false,
        );
        assert!(filtered.contains_key("PATH"));
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
    }

    #[test]
    fn qualify_launch_argv_requires_stream_json_input_and_never_bare() {
        let args = qualify_launch_args();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--input-format", "stream-json"]));
        assert!(args.iter().any(|arg| arg == "--no-session-persistence"));
        assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
        assert!(!args.iter().any(|arg| arg == "--bare"));
        let source = include_str!("claude_qualify_worker.rs");
        let production = source.split("\n#[cfg").next().expect("production");
        assert!(production.contains("qualify_launch_args"));
        assert!(production.contains("filter_claude_child_env"));
        assert!(
            production.contains("apply_claude_child_process_group"),
            "qualify spawn must join a process group so grandchildren cannot leak"
        );
        assert!(
            production.contains("terminate_process_group"),
            "qualify teardown must kill the process group, not only the claude parent"
        );
        assert!(!production.contains("child.kill()"));
        assert!(production.contains("plant_qualify_canary"));
        assert!(production.contains(".mcp.json"));
        assert!(production.contains("qualify_user_line"));
        assert!(!production.contains("store_coding_ledger"));
        assert!(!production.contains("load_coding_ledger"));
        assert!(!production.contains("for_claude_session"));
        assert!(!production.contains("CLAUDE_CONFIG_DIR"));
        assert!(!production.contains("run_turn_over_stdio"));
        assert!(!production.contains(".arg(\"--bare\")"));
    }

    #[test]
    fn qualify_canary_lives_in_disposable_cwd_not_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = plant_qualify_canary(dir.path()).expect("plant");
        let planted = dir.path().join(".mcp.json");
        assert!(planted.is_file());
        let body = std::fs::read_to_string(&planted).expect("read");
        assert!(body.contains(&name));
        assert!(name.starts_with("magician-claude-qualify-canary-"));
        if let Some(home) = std::env::var_os("HOME") {
            assert!(
                !planted.starts_with(Path::new(&home)),
                "canary must not be planted in HOME: {}",
                planted.display()
            );
        }
    }

    #[test]
    fn probe_session_id_is_not_written_to_the_coding_ledger() {
        let qualify = include_str!("claude_qualify_worker.rs");
        let production = qualify.split("\n#[cfg").next().expect("production");
        assert!(!production.contains("native_session_id"));
        assert!(!production.contains("coding_ledger"));
        let qualification = include_str!("claude_qualification.rs");
        let qual_prod = qualification.split("\n#[cfg").next().expect("production");
        assert!(!qual_prod.contains("native_session_id"));
        assert!(!qual_prod.contains("store_coding_ledger"));
        assert!(!qual_prod.contains("for_claude_session"));
        let ledger = include_str!("ledger.rs");
        assert!(!ledger.contains("claude_qualify_over_stdio"));
        assert!(!ledger.contains("tick_claude_qualify_worker"));
    }

    #[test]
    fn http_handlers_do_not_call_the_claude_probe() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(!web.contains("claude_qualify_over_stdio("));
        assert!(!web.contains("claude_qualify_child_stdio("));
        assert!(!web.contains("tick_claude_qualify_worker("));
        assert!(!web.contains("tick_claude_version_worker("));
        assert!(!web.contains("probe_claude_cli_version("));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(bin.contains("spawn_claude_version_worker()"));
        assert!(!bin.contains("claude_qualify_over_stdio("));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_injected_clean_init_becomes_ready() {
        use std::ffi::OsString;

        use crate::config::MagicianClaudeSettings;
        use crate::magician_v2::execution::coding_engine::discovery::ClaudeSearchPaths;
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("claude");
        std::fs::write(&binary, b"#!/bin/sh\necho 2.1.229\n").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755))
                .expect("chmod");
        }
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::write(
            home.join(".claude.json"),
            br#"{"oauthAccount":{"accountUuid":"not-a-secret-id"}}"#,
        )
        .expect("oauth");
        let settings = MagicianClaudeSettings {
            enabled: true,
            binary: None,
            use_api_key: false,
        };
        let search = ClaudeSearchPaths {
            path: Some(OsString::from("/no-such-claude-path")),
            reviewed: vec![binary],
            home: Some(home),
            env: Some(Vec::new()),
        };
        let captured = Arc::new(Mutex::new(None));
        let capture = captured.clone();
        let snapshot = tick_claude_qualify_worker(
            &settings,
            &search,
            move |identity, version, _, use_api_key| {
                let capture = capture.clone();
                async move {
                    *capture.lock().unwrap() = Some(identity.clone());
                    Ok(ClaudeQualifyEvidence {
                        identity,
                        version,
                        use_api_key,
                        init: json!({
                            "type": "system",
                            "subtype": "init",
                            "tools": ["Read", "Bash"],
                            "mcp_servers": [],
                            "plugins": [],
                            "apiKeySource": "none"
                        }),
                        cancelled: true,
                        canary_mcp_name: None,
                    })
                }
            },
        )
        .await;
        assert_eq!(
            snapshot.readiness,
            crate::magician_v2::execution::coding_engine::discovery::ClaudeReadiness::Ready
        );
        assert!(snapshot.selectable);
        assert!(captured.lock().unwrap().is_some());
    }
}

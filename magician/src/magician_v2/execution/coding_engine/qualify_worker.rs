//! Stage 2c: background stdio qualification worker, plus the Grok version tick.
//!
//! Request handlers never call this. The Codex worker talks JSONL to a local
//! `codex app-server`, builds [`CodexQualifyEvidence`], and caches the 2b
//! receipt. The Grok tick is implemented in [`super::grok_qualify_worker`]:
//! version + auth, then a bounded ACP isolation probe. HTTP reads that
//! snapshot and never waits. Tests drive the same session against a fake peer.

use std::{
    path::{Path, PathBuf},
    process::Stdio,
    sync::OnceLock,
    time::Duration,
};

use serde_json::{json, Value};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::Command,
    time::timeout,
};

use super::{
    codex_contract::{app_server_args, CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED},
    discovery::{
        identity_for, magician_codex_client_info, observe_codex_readiness,
        resolved_codex_executable, CodexReadiness, CodexSearchPaths,
    },
    factory::CodexTurnMode,
    jsonl::{read_bounded_jsonl_value, BoundedJsonlError},
    qualification::{
        cache_receipt, filter_codex_child_env, qualify_from_evidence, CodexQualificationReceipt,
        CodexQualifyEvidence, PersistenceProbe,
    },
};
use crate::{
    config::MagicianCodexSettings, magician_v2::execution::coding_engine::coding_budget_settings,
};

pub use super::agy_qualify_worker::{spawn_agy_version_worker, tick_agy_version_worker};
pub use super::claude_qualify_worker::{spawn_claude_version_worker, tick_claude_version_worker};
pub use super::grok_qualify_worker::{spawn_grok_version_worker, tick_grok_version_worker};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MAX_LINE_BYTES: usize = 256 * 1024;
const WORKER_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct QualifySessionLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
}

impl Default for QualifySessionLimits {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualifySessionError {
    Timeout,
    Protocol,
    Io,
}

impl std::fmt::Display for QualifySessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "qualification session timed out"),
            Self::Protocol => write!(f, "qualification protocol was malformed"),
            Self::Io => write!(f, "qualification session I/O failed"),
        }
    }
}

impl std::error::Error for QualifySessionError {}

/// One qualify tick. Spawns only through `run_session`, never from HTTP.
pub async fn tick_codex_qualify_worker<F, Fut>(
    settings: &MagicianCodexSettings,
    search: &CodexSearchPaths,
    run_session: F,
) -> Option<CodexQualificationReceipt>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<CodexQualifyEvidence, QualifySessionError>>,
{
    if !settings.enabled {
        return None;
    }
    let snapshot = observe_codex_readiness(settings, search);
    if !matches!(
        snapshot.readiness,
        CodexReadiness::Unqualified | CodexReadiness::AuthRequired | CodexReadiness::Incompatible
    ) {
        return None;
    }
    let identity = snapshot.identity().to_string();
    let evidence = match run_session(identity).await {
        Ok(evidence) => evidence,
        Err(_error) => {
            // Do not cache a poison receipt. A timeout or protocol error must
            // stay Unqualified so the next tick can retry.
            return None;
        },
    };
    let receipt = qualify_from_evidence(evidence);
    cache_receipt(receipt.clone());
    let _ = observe_codex_readiness(settings, search);
    Some(receipt)
}

/// Drive initialize / config/read / account/read / thread start+delete.
pub async fn qualify_over_stdio<R, W>(
    identity: impl Into<String>,
    reader: R,
    writer: W,
    limits: QualifySessionLimits,
) -> Result<CodexQualifyEvidence, QualifySessionError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut session = JsonlSession {
        reader: BufReader::new(reader),
        writer,
        next_id: 1,
        limits,
    };
    let initialize = session
        .request(
            "initialize",
            json!({
                "clientInfo": magician_codex_client_info(),
                "capabilities": {
                    "experimentalApi": CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED
                }
            }),
        )
        .await?;
    session.notify("initialized", json!({})).await?;
    let config_result = session.request("config/read", json!({})).await?;
    let account = session.request("account/read", json!({})).await.ok();
    let mut probe = None;
    let mut thread_start_error = None;
    let mut instruction_sources = Vec::new();
    let mut started_thread = None;
    // Probe with the sandbox argument a real run sends. An empty probe cannot
    // observe a schema change in the fields runs actually use, so it reports
    // Ready for an install every run would fail against. One start per distinct
    // spelling `thread_sandbox` can produce, so a change to either is caught
    // here rather than at dispatch. `cwd` stays omitted: the thread then
    // reports the child's own directory exactly as the empty probe did, leaving
    // `project_root` below unchanged.
    for mode in [CodexTurnMode::Build, CodexTurnMode::Discuss] {
        let started = match session
            .request(
                "thread/start",
                json!({
                    "approvalPolicy": "never",
                    "sandbox": mode.thread_sandbox(),
                }),
            )
            .await
        {
            Ok(started) => started,
            Err(error) => {
                thread_start_error = Some(format!("{}: {error}", mode.thread_sandbox()));
                break;
            },
        };
        let thread_id = thread_id_from(&started).unwrap_or_else(|| "qualify".to_string());
        let deleted = session
            .request("thread/delete", json!({ "threadId": thread_id }))
            .await
            .is_ok();
        probe = Some(PersistenceProbe { thread_id, deleted });
        // Instruction sources and the project root describe the install, not
        // the mode, so the first successful start owns them.
        if started_thread.is_none() {
            instruction_sources = instruction_sources_from(&started);
            started_thread = Some(started);
        }
        if !deleted {
            // Leaving one stray thread is already disqualifying; do not create
            // another one to prove it twice.
            break;
        }
    }
    let project_root = started_thread
        .as_ref()
        .and_then(cwd_from_thread)
        .map(PathBuf::from);
    Ok(CodexQualifyEvidence {
        identity: identity.into(),
        cli_version: version_from_initialize(&initialize),
        config: Some(config_from_read(&config_result)),
        account,
        instruction_sources,
        project_root,
        persistence_probe: probe,
        thread_start_error,
    })
}

/// Production spawn used only by the background worker.
///
/// A first pass may still see user-configured MCP servers as enabled. Those
/// are narrowed with process-local `-c` overlays and the session is repeated
/// once so attestation sees the effective config, not the host file.
pub async fn qualify_child_stdio(
    identity: String,
    binary: &Path,
    limits: QualifySessionLimits,
) -> Result<CodexQualifyEvidence, QualifySessionError> {
    let mut extra = Vec::new();
    let mut last = None;
    for _ in 0..2 {
        let evidence = spawn_qualify_child(identity.clone(), binary, limits, &extra).await?;
        let more = evidence
            .config
            .as_ref()
            .map(super::codex_contract::mcp_disable_overlays)
            .unwrap_or_default();
        if more.is_empty() || more == extra {
            return Ok(evidence);
        }
        extra = more;
        last = Some(evidence);
    }
    last.ok_or(QualifySessionError::Io)
}

async fn spawn_qualify_child(
    identity: String,
    binary: &Path,
    limits: QualifySessionLimits,
    extra_args: &[String],
) -> Result<CodexQualifyEvidence, QualifySessionError> {
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = filter_codex_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let mut args = app_server_args();
    args.extend(extra_args.iter().cloned());
    let mut command = Command::new(binary);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .env_clear();
    for (key, value) in &filtered {
        command.env(key, value);
    }
    let mut child = command.spawn().map_err(|_| QualifySessionError::Io)?;
    let stdin = child.stdin.take().ok_or(QualifySessionError::Io)?;
    let stdout = child.stdout.take().ok_or(QualifySessionError::Io)?;
    let result = qualify_over_stdio(identity, stdout, stdin, limits).await;
    let _ = child.kill().await;
    result
}

/// Whether a qualify worker should log `outcome` for `engine`: true only when
/// it differs from the last outcome recorded for that engine. Workers re-probe
/// every 30 s, and Grok has no isolation backoff, so an unchanged failure
/// would otherwise log a warning on every tick. Recording a success (`None`)
/// resets it, so a later failure logs again.
pub(crate) fn qualification_outcome_changed(engine: &'static str, outcome: Option<String>) -> bool {
    static LAST: OnceLock<
        std::sync::Mutex<std::collections::HashMap<&'static str, Option<String>>>,
    > = OnceLock::new();
    let mut last = LAST
        .get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    last.insert(engine, outcome.clone()) != Some(outcome)
}

/// Start the process-wide worker once. Safe to call from startup; no-ops
/// without a Tokio runtime (tests).
pub fn spawn_codex_qualify_worker() {
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
            let settings = coding_budget_settings().codex;
            let search = CodexSearchPaths::production();
            let Some(binary) = resolved_codex_executable(&settings, &search) else {
                continue;
            };
            let identity = identity_for(&binary);
            let _ = tick_codex_qualify_worker(&settings, &search, |_| {
                qualify_child_stdio(identity.clone(), &binary, QualifySessionLimits::default())
            })
            .await;
        }
    });
}

struct JsonlSession<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    limits: QualifySessionLimits,
}

impl<R, W> JsonlSession<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, QualifySessionError> {
        let id = self.next_id;
        self.next_id += 1;
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        timeout(self.limits.request_timeout, self.read_result(id))
            .await
            .map_err(|_| QualifySessionError::Timeout)?
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), QualifySessionError> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_line(&mut self, value: &Value) -> Result<(), QualifySessionError> {
        let mut line = serde_json::to_vec(value).map_err(|_| QualifySessionError::Protocol)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|_| QualifySessionError::Io)?;
        self.writer
            .flush()
            .await
            .map_err(|_| QualifySessionError::Io)
    }

    async fn read_result(&mut self, id: u64) -> Result<Value, QualifySessionError> {
        loop {
            let value =
                match read_bounded_jsonl_value(&mut self.reader, self.limits.max_line_bytes, 32)
                    .await
                {
                    Ok(value) => value,
                    Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => {
                        return Err(QualifySessionError::Io)
                    },
                    Err(
                        BoundedJsonlError::Oversized
                        | BoundedJsonlError::Malformed
                        | BoundedJsonlError::TooDeep,
                    ) => return Err(QualifySessionError::Protocol),
                };
            if message_id(&value) == Some(id) {
                if let Some(result) = value.get("result").cloned() {
                    return Ok(result);
                }
                if value.get("success").and_then(Value::as_bool) == Some(false)
                    || value.get("error").is_some()
                {
                    return Err(QualifySessionError::Protocol);
                }
                return Ok(value);
            }
        }
    }
}

fn message_id(value: &Value) -> Option<u64> {
    value
        .get("id")
        .and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()))
}

fn version_from_initialize(result: &Value) -> Option<String> {
    result
        .pointer("/serverInfo/version")
        .or_else(|| result.pointer("/version"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| version_from_user_agent(result.get("userAgent").and_then(Value::as_str)))
}

/// Live 0.147.0 `initialize` has no `serverInfo.version`. It reports
/// `{clientName}/{codexVersion}` in `userAgent`.
fn version_from_user_agent(user_agent: Option<&str>) -> Option<String> {
    let after_slash = user_agent?.split_once('/')?.1;
    let token = after_slash
        .split(|ch: char| ch.is_whitespace() || ch == '(')
        .next()?
        .trim()
        .trim_start_matches('v');
    semver::Version::parse(token).ok()?;
    Some(token.to_string())
}

fn cwd_from_thread(result: &Value) -> Option<String> {
    result
        .pointer("/cwd")
        .or_else(|| result.pointer("/thread/cwd"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn config_from_read(result: &Value) -> Value {
    result
        .get("config")
        .cloned()
        .unwrap_or_else(|| result.clone())
}

fn thread_id_from(result: &Value) -> Option<String> {
    result
        .pointer("/thread/id")
        .or_else(|| result.pointer("/threadId"))
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn instruction_sources_from(result: &Value) -> Vec<Value> {
    result
        .get("instructionSources")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::ffi::OsString;

    use tokio::io::{duplex, AsyncBufReadExt, AsyncReadExt};

    use super::*;

    #[test]
    fn a_qualification_outcome_is_logged_once_until_it_changes() {
        // Engine names are process-global keys; use one no worker uses.
        let engine = "test_engine_outcome_dedupe";
        let failed = || Some("Incompatible: advertised web search".to_string());
        assert!(
            qualification_outcome_changed(engine, failed()),
            "first failure logs"
        );
        assert!(
            !qualification_outcome_changed(engine, failed()),
            "repeat stays quiet"
        );
        assert!(qualification_outcome_changed(
            engine,
            Some("probe failed: Io".into())
        ));
        assert!(
            qualification_outcome_changed(engine, None),
            "success resets"
        );
        assert!(
            qualification_outcome_changed(engine, failed()),
            "failure after success logs again"
        );
    }
    use crate::{
        config::MagicianCodexSettings,
        magician_v2::execution::coding_engine::{
            codex_contract::CODEX_FEATURES_MUST_BE_OFF, discovery::CodexSearchPaths,
        },
    };

    fn narrowed_config() -> Value {
        let mut features = serde_json::Map::new();
        for feature in CODEX_FEATURES_MUST_BE_OFF {
            features.insert((*feature).to_string(), Value::Bool(false));
        }
        json!({
            "approval_policy": "never",
            "sandbox_mode": "workspace-write",
            "web_search": "disabled",
            "model": "gpt-5.6-terra",
            "model_reasoning_effort": "xhigh",
            "agents": { "enabled": false },
            "features": features
        })
    }

    async fn scripted_peer(
        stream: tokio::io::DuplexStream,
        version: &str,
        config: Value,
        signed_in: bool,
        delete_ok: bool,
    ) {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let Ok(request) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let Some(id) = message_id(&request) else {
                continue;
            };
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            let result = match method {
                "initialize" => json!({
                    "serverInfo": { "name": "codex-app-server", "version": version }
                }),
                "config/read" => json!({ "config": config }),
                "account/read" => json!({ "chatgpt": { "signedIn": signed_in } }),
                "thread/start" => json!({ "thread": { "id": "thread-q" } }),
                "thread/delete" if delete_ok => json!({ "ok": true }),
                _ => continue,
            };
            let mut payload = serde_json::to_vec(&json!({ "id": id, "result": result })).unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                break;
            }
        }
    }

    /// Records every `thread/start` params object, and optionally refuses the
    /// call so a rejection can be exercised instead of a timeout.
    async fn probing_peer(
        stream: tokio::io::DuplexStream,
        refuse_thread_start: bool,
        saw: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    ) {
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                break;
            }
            let Ok(request) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            let Some(id) = message_id(&request) else {
                continue;
            };
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            if method == "thread/start" {
                saw.lock()
                    .unwrap()
                    .push(request.get("params").cloned().unwrap_or(Value::Null));
                if refuse_thread_start {
                    let mut payload = serde_json::to_vec(&json!({
                        "id": id,
                        "error": { "code": -32600, "message": "unknown variant" }
                    }))
                    .unwrap();
                    payload.push(b'\n');
                    if writer.write_all(&payload).await.is_err() {
                        break;
                    }
                    continue;
                }
            }
            let result = match method {
                "initialize" => json!({
                    "serverInfo": { "name": "codex-app-server", "version": "0.147.0" }
                }),
                "config/read" => json!({ "config": narrowed_config() }),
                "account/read" => json!({ "chatgpt": { "signedIn": true } }),
                "thread/start" => json!({ "thread": { "id": "thread-q" } }),
                "thread/delete" => json!({ "ok": true }),
                _ => continue,
            };
            let mut payload = serde_json::to_vec(&json!({ "id": id, "result": result })).unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                break;
            }
        }
    }

    #[tokio::test]
    async fn probe_starts_a_thread_with_the_arguments_a_real_run_sends() {
        let saw = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(probing_peer(server, false, saw.clone()));
        let (reader, writer) = tokio::io::split(client);
        qualify_over_stdio(
            "bin-1",
            reader,
            writer,
            QualifySessionLimits {
                request_timeout: Duration::from_secs(1),
                max_line_bytes: 64 * 1024,
            },
        )
        .await
        .expect("evidence");
        let _ = peer.await;

        let starts = saw.lock().unwrap().clone();
        let sandboxes: Vec<Option<&str>> = starts
            .iter()
            .map(|params| params.get("sandbox").and_then(Value::as_str))
            .collect();
        // Every distinct spelling `thread_sandbox` can emit is exercised, so a
        // change to either is refused here instead of at dispatch.
        assert!(
            sandboxes.contains(&Some(CodexTurnMode::Build.thread_sandbox())),
            "{sandboxes:?}"
        );
        assert!(
            sandboxes.contains(&Some(CodexTurnMode::Discuss.thread_sandbox())),
            "{sandboxes:?}"
        );
        for params in &starts {
            assert_eq!(
                params.get("approvalPolicy").and_then(Value::as_str),
                Some("never")
            );
            // `cwd` stays unsent so the thread keeps reporting the child's own
            // directory and `project_root` is derived exactly as before.
            assert!(params.get("cwd").is_none(), "{params}");
        }
    }

    #[tokio::test]
    async fn a_refused_thread_start_is_recorded_rather_than_swallowed() {
        let saw = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(probing_peer(server, true, saw.clone()));
        let (reader, writer) = tokio::io::split(client);
        let evidence = qualify_over_stdio(
            "bin-1",
            reader,
            writer,
            QualifySessionLimits {
                request_timeout: Duration::from_secs(1),
                max_line_bytes: 64 * 1024,
            },
        )
        .await
        .expect("evidence");
        let _ = peer.await;

        assert!(
            evidence.thread_start_error.is_some(),
            "a refusal must survive into the evidence"
        );
        assert!(evidence.persistence_probe.is_none());
        // And it must disqualify: an install that refuses these arguments
        // cannot serve a run that sends them.
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Incompatible);
    }

    #[tokio::test]
    async fn fake_stdio_ready_session_deletes_the_probe_thread() {
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(scripted_peer(
            server,
            "0.147.0",
            narrowed_config(),
            true,
            true,
        ));
        let (reader, writer) = tokio::io::split(client);
        let evidence = qualify_over_stdio(
            "bin-1",
            reader,
            writer,
            QualifySessionLimits {
                request_timeout: Duration::from_secs(1),
                max_line_bytes: 64 * 1024,
            },
        )
        .await
        .expect("session");
        let _ = server.await;
        assert_eq!(evidence.cli_version.as_deref(), Some("0.147.0"));
        assert_eq!(
            evidence.persistence_probe,
            Some(PersistenceProbe {
                thread_id: "thread-q".to_string(),
                deleted: true
            })
        );
        let receipt = qualify_from_evidence(evidence);
        assert_eq!(receipt.readiness, CodexReadiness::Ready);
        assert!(!receipt.reason.contains("/"), "{}", receipt.reason);
    }

    #[test]
    fn live_initialize_user_agent_supplies_the_cli_version() {
        let version = version_from_initialize(&json!({
            "userAgent": "magician/0.147.0 (Mac OS 26.6.0; arm64) ghostty/1.3.1 (magician; 0.0.0)",
            "codexHome": "/Users/dev/.codex"
        }));
        assert_eq!(version.as_deref(), Some("0.147.0"));
    }

    #[tokio::test]
    async fn malformed_stdio_is_a_protocol_error() {
        let (client, mut server) = duplex(1024);
        tokio::spawn(async move {
            let mut buf = [0u8; 32];
            let _ = server.read(&mut buf).await;
            let _ = server.write_all(b"not-json\n").await;
        });
        let (reader, writer) = tokio::io::split(client);
        let err = qualify_over_stdio(
            "bin-1",
            reader,
            writer,
            QualifySessionLimits {
                request_timeout: Duration::from_secs(1),
                max_line_bytes: 64 * 1024,
            },
        )
        .await
        .expect_err("malformed");
        assert_eq!(err, QualifySessionError::Protocol);
        assert!(!err.to_string().contains("not-json"));
    }

    #[tokio::test]
    async fn silent_peer_times_out() {
        let (client, _server) = duplex(1024);
        let (reader, writer) = tokio::io::split(client);
        let err = qualify_over_stdio(
            "bin-1",
            reader,
            writer,
            QualifySessionLimits {
                request_timeout: Duration::from_millis(30),
                max_line_bytes: 64 * 1024,
            },
        )
        .await
        .expect_err("timeout");
        assert_eq!(err, QualifySessionError::Timeout);
    }

    #[tokio::test]
    async fn worker_tick_skips_when_disabled() {
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let receipt = tick_codex_qualify_worker(
            &MagicianCodexSettings::default(),
            &CodexSearchPaths {
                path: Some(OsString::from("/no-such-codex-path")),
                reviewed: Vec::new(),
            },
            |_| async move {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                Err(QualifySessionError::Io)
            },
        )
        .await;
        assert!(receipt.is_none());
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn worker_tick_runs_once_for_an_unqualified_binary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("codex");
        std::fs::write(&binary, b"ok").expect("write");
        let settings = MagicianCodexSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: Vec::new(),
        };
        let receipt = tick_codex_qualify_worker(&settings, &search, |identity| async move {
            Ok(CodexQualifyEvidence {
                identity,
                cli_version: Some("0.147.0".to_string()),
                config: Some(narrowed_config()),
                account: Some(json!({ "chatgpt": { "signedIn": true } })),
                instruction_sources: Vec::new(),
                project_root: None,
                persistence_probe: Some(PersistenceProbe {
                    thread_id: "t".to_string(),
                    deleted: true,
                }),
                thread_start_error: None,
            })
        })
        .await
        .expect("tick");
        assert_eq!(receipt.readiness, CodexReadiness::Ready);
    }

    #[tokio::test]
    async fn worker_tick_does_not_cache_a_failed_session() {
        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("codex");
        std::fs::write(&binary, b"ok").expect("write");
        let settings = MagicianCodexSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = CodexSearchPaths {
            path: Some(OsString::from("/no-such-codex-path")),
            reviewed: Vec::new(),
        };
        let identity =
            crate::magician_v2::execution::coding_engine::discovery::identity_for(&binary);
        crate::magician_v2::execution::coding_engine::qualification::invalidate_receipt(&identity);
        let receipt = tick_codex_qualify_worker(&settings, &search, |_| async {
            Err(QualifySessionError::Timeout)
        })
        .await;
        assert!(receipt.is_none());
        assert!(
            crate::magician_v2::execution::coding_engine::qualification::cached_receipt(&identity)
                .is_none()
        );
    }

    #[test]
    fn request_handlers_do_not_spawn_the_worker_session() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(!web.contains("qualify_over_stdio("));
        assert!(!web.contains("qualify_child_stdio("));
        assert!(!web.contains("tick_codex_qualify_worker("));
        assert!(!web.contains("tick_grok_version_worker("));
        assert!(!web.contains("tick_grok_qualify_worker("));
        assert!(!web.contains("grok_qualify_over_stdio("));
        assert!(!web.contains("grok_qualify_child_stdio("));
        assert!(!web.contains("probe_grok_cli_version("));
        assert!(!web.contains("probe_grok_version_in_background("));
        assert!(!web.contains("spawn_grok_version_worker("));
        assert!(!web.contains("tick_claude_version_worker("));
        assert!(!web.contains("tick_claude_qualify_worker("));
        assert!(!web.contains("claude_qualify_over_stdio("));
        assert!(!web.contains("claude_qualify_child_stdio("));
        assert!(!web.contains("probe_claude_cli_version("));
        assert!(!web.contains("probe_claude_version_in_background("));
        assert!(!web.contains("spawn_claude_version_worker("));
        assert!(!web.contains("tick_agy_version_worker("));
        assert!(!web.contains("tick_agy_qualify_worker("));
        assert!(!web.contains("agy_qualify_over_stdio("));
        assert!(!web.contains("agy_qualify_child_stdio("));
        assert!(!web.contains("probe_agy_cli_version("));
        assert!(!web.contains("probe_agy_version_in_background("));
        assert!(!web.contains("spawn_agy_version_worker("));
        assert!(!web.contains("overlay_or_probe_grok_version("));
        assert!(!web.contains("attest_grok_isolation("));
        let grok_tick = include_str!("grok_qualify_worker.rs");
        let start = grok_tick
            .find("pub async fn tick_grok_version_worker")
            .expect("tick_grok_version_worker");
        let body = &grok_tick[start..];
        let end = body
            .find("pub fn spawn_grok_version_worker")
            .unwrap_or(body.len());
        assert!(
            body[..end].contains("tick_grok_qualify_worker"),
            "Grok tick must fold ACP attestation into the version worker"
        );
        assert!(body[..end].contains("grok_qualify_child_stdio"));
        let grok_stdio = grok_tick
            .find("pub async fn grok_qualify_over_stdio")
            .expect("grok_qualify_over_stdio");
        assert!(grok_tick[grok_stdio..].contains("initialize"));
        assert!(grok_tick[grok_stdio..].contains("session/new"));
        assert!(grok_tick[grok_stdio..].contains("session/cancel"));
    }
}

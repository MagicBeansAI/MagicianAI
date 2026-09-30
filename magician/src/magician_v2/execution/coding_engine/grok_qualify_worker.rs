//! Phase 3: bounded Grok ACP isolation probe on the version tick.
//!
//! After version + auth would otherwise be Ready, the worker speaks
//! `initialize` + `session/new` (`mcpServers: []`) in a disposable cwd,
//! drains a bounded window of `session/update`, then `session/cancel`
//! and kill. Missing MCP/tool lists stay Unqualified. Request handlers
//! never call this. The probe session id is not journaled as a VibeDev
//! continuation.

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
use uuid::Uuid;

use super::{
    discovery::{
        observe_grok_readiness, probe_grok_version_in_background, resolved_grok_executable,
        GrokReadinessSnapshot, GrokSearchPaths,
    },
    factory::GrokTurnMode,
    grok::{apply_grok_child_process_group, filter_grok_child_env, terminate_process_group},
    grok_contract::{
        agent_stdio_args, grok_session_id_from, initialize_params, session_cancel_params,
        session_new_params,
    },
    grok_qualification::{
        cache_grok_receipt, grok_needs_acp_attestation, qualify_from_grok_evidence,
        GrokQualifyEvidence,
    },
    jsonl::{read_bounded_jsonl_value, BoundedJsonlError},
};
use crate::{
    config::MagicianGrokSettings, magician_v2::execution::coding_engine::coding_budget_settings,
};

const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_MAX_LINE_BYTES: usize = 256 * 1024;
const DEFAULT_UPDATE_DRAIN: Duration = Duration::from_millis(800);
const DEFAULT_MAX_DRAIN_UPDATES: usize = 24;
const WORKER_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct GrokQualifyLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub update_drain: Duration,
    pub max_drain_updates: usize,
}

impl Default for GrokQualifyLimits {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            update_drain: DEFAULT_UPDATE_DRAIN,
            max_drain_updates: DEFAULT_MAX_DRAIN_UPDATES,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokQualifyError {
    Timeout,
    Protocol,
    Io,
}

impl std::fmt::Display for GrokQualifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Grok qualification session timed out"),
            Self::Protocol => write!(f, "Grok qualification protocol was malformed"),
            Self::Io => write!(f, "Grok qualification session I/O failed"),
        }
    }
}

impl std::error::Error for GrokQualifyError {}

/// Production tick: version + auth, then a bounded ACP probe if that would
/// be Ready. HTTP never calls this.
pub async fn tick_grok_version_worker(
    settings: &MagicianGrokSettings,
    search: &GrokSearchPaths,
) -> GrokReadinessSnapshot {
    tick_grok_qualify_worker(settings, search, |identity, binary| async move {
        grok_qualify_child_stdio(identity, &binary, GrokQualifyLimits::default()).await
    })
    .await
}

pub async fn tick_grok_qualify_worker<F, Fut>(
    settings: &MagicianGrokSettings,
    search: &GrokSearchPaths,
    run_session: F,
) -> GrokReadinessSnapshot
where
    F: FnOnce(String, PathBuf) -> Fut,
    Fut: std::future::Future<Output = Result<GrokQualifyEvidence, GrokQualifyError>>,
{
    if !settings.enabled {
        return observe_grok_readiness(settings, search);
    }
    let probe_settings = settings.clone();
    let probe_search = search.clone();
    let _ = tokio::task::spawn_blocking(move || {
        probe_grok_version_in_background(&probe_settings, &probe_search);
    })
    .await;
    let snapshot = observe_grok_readiness(settings, search);
    if grok_needs_acp_attestation(&snapshot) {
        if let Some(binary) = resolved_grok_executable(settings, search) {
            match run_session(snapshot.identity().to_string(), binary).await {
                Ok(evidence) => {
                    let receipt = qualify_from_grok_evidence(evidence);
                    // Without this the only trace of a failed qualification is an
                    // engine silently missing from VibeDev. Reasons are public-safe;
                    // each distinct outcome is logged once.
                    let failed = !crate::magician_v2::execution::coding_engine::discovery::grok_is_selectable(
                        receipt.readiness,
                    );
                    let outcome =
                        failed.then(|| format!("{:?}: {}", receipt.readiness, receipt.reason));
                    if failed
                        && super::qualify_worker::qualification_outcome_changed("grok_acp", outcome)
                    {
                        tracing::warn!(engine = "grok_acp", readiness = ?receipt.readiness, reason = %receipt.reason, "coding engine did not qualify");
                    } else if !failed {
                        super::qualify_worker::qualification_outcome_changed("grok_acp", None);
                    }
                    cache_grok_receipt(receipt)
                },
                Err(error) => {
                    if super::qualify_worker::qualification_outcome_changed(
                        "grok_acp",
                        Some(format!("probe failed: {error:?}")),
                    ) {
                        tracing::warn!(engine = "grok_acp", error = ?error, "coding engine qualification probe failed");
                    }
                },
            }
        }
    }
    observe_grok_readiness(settings, search)
}

pub fn spawn_grok_version_worker() {
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
            let settings = coding_budget_settings().grok;
            let search = GrokSearchPaths::production();
            let _ = tick_grok_version_worker(&settings, &search).await;
        }
    });
}

/// Drive initialize + session/new, drain session/update, then session/cancel.
/// Never journals the probe session id.
pub async fn grok_qualify_over_stdio<R, W>(
    identity: impl Into<String>,
    cwd: impl AsRef<str>,
    reader: R,
    writer: W,
    limits: GrokQualifyLimits,
) -> Result<GrokQualifyEvidence, GrokQualifyError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut session = JsonlSession {
        reader: BufReader::new(reader),
        writer,
        next_id: 1,
        limits,
        updates: Vec::new(),
    };
    let initialize = session.request("initialize", initialize_params()).await?;
    let started = session
        .request("session/new", session_new_params(cwd.as_ref()))
        .await?;
    let session_id = grok_session_id_from(&started).ok_or(GrokQualifyError::Protocol)?;
    let drain_result = session.drain_updates().await;
    let cancelled = session
        .notify("session/cancel", session_cancel_params(&session_id))
        .await
        .is_ok();
    drain_result?;
    let updates = std::mem::take(&mut session.updates);
    Ok(GrokQualifyEvidence {
        identity: identity.into(),
        initialize,
        session: started,
        session_id: Some(session_id),
        cancelled,
        updates,
        canary_mcp_name: None,
    })
}

pub async fn grok_qualify_child_stdio(
    identity: String,
    binary: &Path,
    limits: GrokQualifyLimits,
) -> Result<GrokQualifyEvidence, GrokQualifyError> {
    let (cwd, canary) = qualify_cwd()?;
    let inherited: Vec<(String, String)> = std::env::vars().collect();
    let filtered = filter_grok_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    let args = agent_stdio_args(GrokTurnMode::Build);
    debug_assert!(
        args.iter().any(|arg| arg == "--no-leader") && !args.iter().any(|arg| arg == "--leader")
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
    apply_grok_child_process_group(&mut command);
    let mut child = command.spawn().map_err(|_| GrokQualifyError::Io)?;
    let stdin = child.stdin.take().ok_or(GrokQualifyError::Io)?;
    let stdout = child.stdout.take().ok_or(GrokQualifyError::Io)?;
    let mut result =
        grok_qualify_over_stdio(identity, cwd.display().to_string(), stdout, stdin, limits).await;
    if let Ok(evidence) = &mut result {
        evidence.canary_mcp_name = Some(canary);
    }
    terminate_process_group(&mut child).await;
    let _ = std::fs::remove_dir_all(&cwd);
    result
}

fn qualify_cwd() -> Result<(PathBuf, String), GrokQualifyError> {
    let dir = std::env::temp_dir().join(format!("magician-grok-qualify-{}", Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|_| GrokQualifyError::Io)?;
    let canary = plant_qualify_canary(&dir)?;
    Ok((dir, canary))
}

fn plant_qualify_canary(cwd: &Path) -> Result<String, GrokQualifyError> {
    let name = format!("magician-grok-qualify-canary-{}", Uuid::new_v4());
    let mut servers = serde_json::Map::new();
    servers.insert(name.clone(), json!({ "command": "false", "args": [] }));
    let body = json!({ "mcpServers": servers });
    let encoded = serde_json::to_vec_pretty(&body).map_err(|_| GrokQualifyError::Protocol)?;
    std::fs::write(cwd.join(".mcp.json"), encoded).map_err(|_| GrokQualifyError::Io)?;
    Ok(name)
}

struct JsonlSession<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    limits: GrokQualifyLimits,
    updates: Vec<Value>,
}

impl<R, W> JsonlSession<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    async fn request(&mut self, method: &str, params: Value) -> Result<Value, GrokQualifyError> {
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
            .map_err(|_| GrokQualifyError::Timeout)?
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), GrokQualifyError> {
        self.write_line(&json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn write_line(&mut self, value: &Value) -> Result<(), GrokQualifyError> {
        let mut line = serde_json::to_vec(value).map_err(|_| GrokQualifyError::Protocol)?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|_| GrokQualifyError::Io)?;
        self.writer.flush().await.map_err(|_| GrokQualifyError::Io)
    }

    async fn drain_updates(&mut self) -> Result<(), GrokQualifyError> {
        if self.limits.update_drain.is_zero() || self.limits.max_drain_updates == 0 {
            return Ok(());
        }
        let started = std::time::Instant::now();
        while self.updates.len() < self.limits.max_drain_updates {
            let remaining = self.limits.update_drain.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                break;
            }
            match timeout(remaining, self.read_frame()).await {
                Ok(Ok(value)) => {
                    let _ = self.consider_frame(value, None)?;
                },
                Ok(Err(GrokQualifyError::Io)) => break,
                Ok(Err(err)) => return Err(err),
                Err(_) => break,
            }
        }
        Ok(())
    }

    async fn read_result(&mut self, id: u64) -> Result<Value, GrokQualifyError> {
        loop {
            let value = self.read_frame().await?;
            if let Some(result) = self.consider_frame(value, Some(id))? {
                return Ok(result);
            }
        }
    }

    async fn read_frame(&mut self) -> Result<Value, GrokQualifyError> {
        match read_bounded_jsonl_value(&mut self.reader, self.limits.max_line_bytes, 32).await {
            Ok(value) => Ok(value),
            Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => Err(GrokQualifyError::Io),
            Err(
                BoundedJsonlError::Oversized
                | BoundedJsonlError::Malformed
                | BoundedJsonlError::TooDeep,
            ) => Err(GrokQualifyError::Protocol),
        }
    }

    fn consider_frame(
        &mut self,
        value: Value,
        expected_id: Option<u64>,
    ) -> Result<Option<Value>, GrokQualifyError> {
        if value.get("method").is_some() && jsonrpc_id(&value).is_some() {
            return Err(GrokQualifyError::Protocol);
        }
        if let Some(id) = expected_id {
            if jsonrpc_id(&value).as_ref() == Some(&json!(id)) {
                if let Some(result) = value.get("result").cloned() {
                    return Ok(Some(result));
                }
                if value.get("error").is_some() {
                    return Err(GrokQualifyError::Protocol);
                }
                return Ok(Some(value));
            }
        }
        if jsonrpc_id(&value).is_none() && self.updates.len() < self.limits.max_drain_updates {
            self.updates.push(value);
        }
        Ok(None)
    }
}

fn jsonrpc_id(value: &Value) -> Option<Value> {
    let id = value.get("id")?;
    match id {
        Value::Number(number) if number.is_i64() || number.is_u64() => Some(id.clone()),
        Value::String(text) if !text.is_empty() => Some(id.clone()),
        _ => None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use tokio::io::{duplex, AsyncBufReadExt};

    use super::*;
    use crate::magician_v2::execution::coding_engine::{
        discovery::GrokReadiness, grok::filter_grok_child_env,
        grok_qualification::qualify_from_grok_evidence,
    };

    fn message_id(value: &Value) -> Option<u64> {
        jsonrpc_id(value).and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()))
    }

    fn clean_initialize() -> Value {
        json!({ "protocolVersion": 1 })
    }

    fn clean_session() -> Value {
        json!({
            "sessionId": "sess-probe",
            "mcpServers": [],
            "tools": ["read_file", "bash", "grep_search", "list_dir"],
        })
    }

    fn test_limits() -> GrokQualifyLimits {
        GrokQualifyLimits {
            request_timeout: Duration::from_secs(1),
            max_line_bytes: 64 * 1024,
            update_drain: Duration::ZERO,
            max_drain_updates: 24,
        }
    }

    fn omitted_lists_session() -> Value {
        json!({ "sessionId": "sess-probe" })
    }

    async fn scripted_peer(stream: tokio::io::DuplexStream, initialize: Value, session: Value) {
        scripted_peer_with_updates(stream, initialize, session, Vec::new()).await
    }

    async fn scripted_peer_with_updates(
        stream: tokio::io::DuplexStream,
        initialize: Value,
        session: Value,
        updates: Vec<Value>,
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
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            if request.get("id").is_none() {
                continue;
            }
            let Some(id) = message_id(&request) else {
                continue;
            };
            let result = match method {
                "initialize" => initialize.clone(),
                "session/new" => session.clone(),
                _ => continue,
            };
            let mut payload = serde_json::to_vec(&json!({ "id": id, "result": result })).unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                break;
            }
            if method == "session/new" {
                for update in &updates {
                    let mut line = serde_json::to_vec(update).unwrap();
                    line.push(b'\n');
                    if writer.write_all(&line).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn fake_stdio_with_mcp_servers_is_incompatible() {
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(scripted_peer(
            server,
            json!({
                "protocolVersion": 1,
                "mcpServers": [{ "name": "github" }]
            }),
            clean_session(),
        ));
        let (reader, writer) = tokio::io::split(client);
        let evidence =
            grok_qualify_over_stdio("bin-1", "/tmp/qualify", reader, writer, test_limits())
                .await
                .expect("session");
        let _ = server.await;
        assert_eq!(evidence.session_id.as_deref(), Some("sess-probe"));
        assert!(evidence.cancelled);
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Incompatible);
        assert!(!receipt.reason.contains("github"), "{}", receipt.reason);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[tokio::test]
    async fn fake_stdio_empty_mcp_and_expected_tools_is_ready() {
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(scripted_peer(server, clean_initialize(), clean_session()));
        let (reader, writer) = tokio::io::split(client);
        let evidence =
            grok_qualify_over_stdio("bin-1", "/tmp/qualify", reader, writer, test_limits())
                .await
                .expect("session");
        let _ = server.await;
        assert!(evidence.cancelled);
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Ready);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[tokio::test]
    async fn fake_stdio_omitted_lists_is_not_ready() {
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(scripted_peer(
            server,
            clean_initialize(),
            omitted_lists_session(),
        ));
        let (reader, writer) = tokio::io::split(client);
        let evidence =
            grok_qualify_over_stdio("bin-1", "/tmp/qualify", reader, writer, test_limits())
                .await
                .expect("session");
        let _ = server.await;
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Unqualified);
        assert!(
            receipt.reason.contains("unattested") || receipt.reason.contains("did not advertise"),
            "{}",
            receipt.reason
        );
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[tokio::test]
    async fn fake_stdio_drains_session_update_lists() {
        let (client, server) = duplex(16 * 1024);
        let server = tokio::spawn(scripted_peer_with_updates(
            server,
            clean_initialize(),
            omitted_lists_session(),
            vec![json!({
                "jsonrpc": "2.0",
                "method": "session/update",
                "params": {
                    "sessionId": "sess-probe",
                    "update": {
                        "sessionUpdate": "available_commands_update",
                        "mcpServers": [],
                        "availableCommands": ["read_file", "bash", "grep"]
                    }
                }
            })],
        ));
        let (reader, writer) = tokio::io::split(client);
        let evidence = grok_qualify_over_stdio(
            "bin-1",
            "/tmp/qualify",
            reader,
            writer,
            GrokQualifyLimits {
                update_drain: Duration::from_millis(250),
                ..test_limits()
            },
        )
        .await
        .expect("session");
        let _ = server.await;
        assert!(!evidence.updates.is_empty());
        let receipt = qualify_from_grok_evidence(evidence);
        assert_eq!(receipt.readiness, GrokReadiness::Ready);
        let rendered = format!("{receipt:?}");
        assert!(!rendered.contains("sess-probe"), "{rendered}");
    }

    #[tokio::test]
    async fn qualify_initialize_payload_has_no_fs_or_terminal() {
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = frames.clone();
        let peer = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
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
                seen.lock().unwrap().push(request.clone());
                let Some(id) = message_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let result = match method {
                    "initialize" => clean_initialize(),
                    "session/new" => clean_session(),
                    _ => continue,
                };
                let mut payload =
                    serde_json::to_vec(&json!({ "id": id, "result": result })).unwrap();
                payload.push(b'\n');
                if writer.write_all(&payload).await.is_err() {
                    break;
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let _ = grok_qualify_over_stdio("bin-1", "/tmp/qualify", reader, writer, test_limits())
            .await
            .expect("session");
        let _ = peer.await;
        let frames = frames.lock().unwrap();
        let initialize = frames
            .iter()
            .find(|frame| frame.get("method").and_then(Value::as_str) == Some("initialize"))
            .expect("initialize");
        let params = &initialize["params"];
        assert_eq!(params["clientCapabilities"], json!({}));
        let encoded = params.to_string();
        assert!(!encoded.contains("\"fs\""), "{encoded}");
        assert!(!encoded.contains("terminal"), "{encoded}");
        let session_new = frames
            .iter()
            .find(|frame| frame.get("method").and_then(Value::as_str) == Some("session/new"))
            .expect("session/new");
        assert_eq!(session_new["params"]["mcpServers"], json!([]));
        assert!(frames.iter().any(|frame| {
            frame.get("method").and_then(Value::as_str) == Some("session/cancel")
                && frame.get("id").is_none()
        }));
    }

    #[test]
    fn qualify_child_env_drops_magician_foo() {
        let filtered = filter_grok_child_env([
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/me"),
            ("MAGICIAN_FOO", "secret"),
            ("MAGICIAN_ADMIN_TOKEN", "nope"),
        ]);
        assert!(filtered.contains_key("PATH"));
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("MAGICIAN_ADMIN_TOKEN"));
    }

    #[test]
    fn qualify_launch_argv_requires_no_leader() {
        let args = agent_stdio_args(GrokTurnMode::Build);
        assert_ne!(GrokTurnMode::Build.sandbox_flag(), "off");
        assert!(args.iter().any(|arg| arg == "--no-leader"));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace"]));
        assert!(args.iter().any(|arg| arg == "--disable-web-search"));
        assert!(!args.iter().any(|arg| arg == "--leader"));
        let source = include_str!("grok_qualify_worker.rs");
        let production = source.split("\n#[cfg").next().expect("production");
        assert!(production.contains("agent_stdio_args"));
        assert!(production.contains("GrokTurnMode::Build"));
        assert!(production.contains("filter_grok_child_env"));
        assert!(
            production.contains("apply_grok_child_process_group"),
            "qualify spawn must join a process group so grandchildren cannot leak"
        );
        assert!(
            production.contains("terminate_process_group"),
            "qualify teardown must kill the process group, not only the grok parent"
        );
        assert!(!production.contains("child.kill()"));
        assert!(production.contains("session/cancel"));
        assert!(production.contains("drain_updates"));
        assert!(production.contains("plant_qualify_canary"));
        assert!(production.contains(".mcp.json"));
        assert!(!production.contains(".claude.json"));
        assert!(!production.contains("store_coding_ledger"));
        assert!(!production.contains("load_coding_ledger"));
        assert!(!production.contains("for_grok_session"));
        assert!(!production.contains("GROK_HOME"));
        assert!(!production.contains("run_turn_over_stdio"));
    }

    #[test]
    fn qualify_canary_lives_in_disposable_cwd_not_home() {
        let dir = tempfile::tempdir().expect("tempdir");
        let name = plant_qualify_canary(dir.path()).expect("plant");
        let planted = dir.path().join(".mcp.json");
        assert!(planted.is_file());
        let body = std::fs::read_to_string(&planted).expect("read");
        assert!(body.contains(&name));
        assert!(name.starts_with("magician-grok-qualify-canary-"));
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
        let qualify = include_str!("grok_qualify_worker.rs");
        let production = qualify.split("\n#[cfg").next().expect("production");
        assert!(!production.contains("native_session_id"));
        assert!(!production.contains("coding_ledger"));
        let qualification = include_str!("grok_qualification.rs");
        let qual_prod = qualification.split("\n#[cfg").next().expect("production");
        assert!(!qual_prod.contains("native_session_id"));
        assert!(!qual_prod.contains("store_coding_ledger"));
        assert!(!qual_prod.contains("for_grok_session"));
        let ledger = include_str!("ledger.rs");
        assert!(!ledger.contains("grok_qualify_over_stdio"));
        assert!(!ledger.contains("tick_grok_qualify_worker"));
    }

    #[test]
    fn http_handlers_do_not_call_the_grok_acp_probe() {
        let web = include_str!("../../../../../magician-api/src/web_api.rs");
        assert!(!web.contains("grok_qualify_over_stdio("));
        assert!(!web.contains("grok_qualify_child_stdio("));
        assert!(!web.contains("tick_grok_qualify_worker("));
        assert!(!web.contains("tick_grok_version_worker("));
        assert!(!web.contains("attest_grok_isolation("));
        assert!(!web.contains("probe_grok_cli_version("));
        assert!(!web.contains("qualify_over_stdio("));
        let bin = include_str!("../../../../../magician-bin/src/main.rs");
        assert!(bin.contains("spawn_grok_version_worker()"));
        assert!(!bin.contains("grok_qualify_over_stdio("));
        assert!(!bin.contains("attest_grok_isolation("));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_injected_clean_session_becomes_ready() {
        use std::ffi::OsString;

        use crate::config::MagicianGrokSettings;
        use crate::magician_v2::execution::coding_engine::discovery::GrokSearchPaths;
        use crate::magician_v2::execution::coding_engine::grok_qualification::invalidate_grok_receipt;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        std::fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".grok")).expect("grok dir");
        std::fs::write(home.join(".grok").join("auth.json"), b"{}\n").expect("auth");
        let settings = MagicianGrokSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = GrokSearchPaths {
            path: Some(OsString::from("/no-such-grok-path")),
            reviewed: Vec::new(),
            home: Some(home),
            env: Some(Vec::new()),
        };
        let identity =
            crate::magician_v2::execution::coding_engine::discovery::identity_for(&binary);
        invalidate_grok_receipt(&identity);
        let snapshot = tick_grok_qualify_worker(&settings, &search, |id, _path| async move {
            Ok(GrokQualifyEvidence {
                identity: id,
                initialize: clean_initialize(),
                session: clean_session(),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..GrokQualifyEvidence::default()
            })
        })
        .await;
        assert_eq!(snapshot.readiness, GrokReadiness::Ready);
        assert!(snapshot.selectable);
        assert_eq!(snapshot.version.as_deref(), Some("1.0.5"));
        let public = snapshot.public_json().to_string();
        assert!(!public.contains("sess-probe"), "{public}");
        assert!(!public.contains("auth.json"), "{public}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_injected_mcp_session_is_incompatible() {
        use std::ffi::OsString;

        use crate::config::MagicianGrokSettings;
        use crate::magician_v2::execution::coding_engine::discovery::GrokSearchPaths;
        use crate::magician_v2::execution::coding_engine::grok_qualification::invalidate_grok_receipt;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        std::fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".grok")).expect("grok dir");
        std::fs::write(home.join(".grok").join("auth.json"), b"{}\n").expect("auth");
        let settings = MagicianGrokSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = GrokSearchPaths {
            path: Some(OsString::from("/no-such-grok-path")),
            reviewed: Vec::new(),
            home: Some(home),
            env: Some(Vec::new()),
        };
        let identity =
            crate::magician_v2::execution::coding_engine::discovery::identity_for(&binary);
        invalidate_grok_receipt(&identity);
        let snapshot = tick_grok_qualify_worker(&settings, &search, |id, _path| async move {
            Ok(GrokQualifyEvidence {
                identity: id,
                initialize: json!({
                    "protocolVersion": 1,
                    "mcpServers": [{ "name": "github" }]
                }),
                session: clean_session(),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..GrokQualifyEvidence::default()
            })
        })
        .await;
        assert_eq!(snapshot.readiness, GrokReadiness::Incompatible);
        assert!(!snapshot.selectable);
        assert!(snapshot.reason.contains("MCP"), "{}", snapshot.reason);
        let public = snapshot.public_json().to_string();
        assert!(!public.contains("github"), "{public}");
        assert!(!public.contains("sess-probe"), "{public}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_injected_omitted_lists_stays_unqualified() {
        use std::ffi::OsString;

        use crate::config::MagicianGrokSettings;
        use crate::magician_v2::execution::coding_engine::discovery::GrokSearchPaths;
        use crate::magician_v2::execution::coding_engine::grok_qualification::invalidate_grok_receipt;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        std::fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let home = dir.path().join("home");
        std::fs::create_dir_all(home.join(".grok")).expect("grok dir");
        std::fs::write(home.join(".grok").join("auth.json"), b"{}\n").expect("auth");
        let settings = MagicianGrokSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = GrokSearchPaths {
            path: Some(OsString::from("/no-such-grok-path")),
            reviewed: Vec::new(),
            home: Some(home),
            env: Some(Vec::new()),
        };
        let identity =
            crate::magician_v2::execution::coding_engine::discovery::identity_for(&binary);
        invalidate_grok_receipt(&identity);
        let snapshot = tick_grok_qualify_worker(&settings, &search, |id, _path| async move {
            Ok(GrokQualifyEvidence {
                identity: id,
                initialize: clean_initialize(),
                session: omitted_lists_session(),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..GrokQualifyEvidence::default()
            })
        })
        .await;
        assert_eq!(snapshot.readiness, GrokReadiness::Unqualified);
        assert!(!snapshot.selectable);
        let public = snapshot.public_json().to_string();
        assert!(!public.contains("sess-probe"), "{public}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tick_does_not_acp_probe_when_auth_is_required() {
        use std::ffi::OsString;
        use std::sync::atomic::{AtomicBool, Ordering};

        use crate::config::MagicianGrokSettings;
        use crate::magician_v2::execution::coding_engine::discovery::GrokSearchPaths;

        let dir = tempfile::tempdir().expect("tempdir");
        let binary = dir.path().join("grok");
        std::fs::write(&binary, b"#!/bin/sh\necho 1.0.5\n").expect("write");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let settings = MagicianGrokSettings {
            enabled: true,
            binary: Some(binary.display().to_string()),
        };
        let search = GrokSearchPaths {
            path: Some(OsString::from("/no-such-grok-path")),
            reviewed: Vec::new(),
            home: Some(dir.path().join("empty-home")),
            env: Some(Vec::new()),
        };
        let called = Arc::new(AtomicBool::new(false));
        let flag = called.clone();
        let snapshot = tick_grok_qualify_worker(&settings, &search, |id, _path| async move {
            flag.store(true, Ordering::SeqCst);
            Ok(GrokQualifyEvidence {
                identity: id,
                initialize: clean_initialize(),
                session: clean_session(),
                session_id: Some("sess-probe".to_string()),
                cancelled: true,
                ..GrokQualifyEvidence::default()
            })
        })
        .await;
        assert_eq!(snapshot.readiness, GrokReadiness::AuthRequired);
        assert!(!snapshot.selectable);
        assert!(!called.load(Ordering::SeqCst));
    }
}

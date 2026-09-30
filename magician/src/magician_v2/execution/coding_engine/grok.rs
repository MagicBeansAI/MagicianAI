//! Live bounded Grok ACP adapter (`grok agent stdio`).
//!
//! The factory constructs this when the journaled engine is Grok. Tests
//! drive the same session against a fake peer. Same-engine follow-up in
//! the VibeDev chain `session/load`s the ACP session (id from the
//! chain-root store); otherwise Magician opens `session/new`.
//! Cockpit Stop notifies `session/cancel` and unblocks the drain select.
//! Steer/follow-up share a generation-fenced bounded FIFO.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

use super::budgets::{record_termination_reason, CodingTerminationReason, ProgressWatchdog};
use super::codex_lifecycle::ContinuationFreshReason;
use super::control::{
    coding_control_registry, CodingControlHandle, GrokControlCommand, GrokTurnHandle,
};
use super::factory::GrokCodingOptions;
use super::grok_contract::{
    agent_stdio_args, grok_session_id_from, initialize_params, may_send_method,
    session_cancel_params, session_load_params, session_new_params, GROK_ACP_PROHIBITED_PREFIX,
};
use super::jsonl::{append_bounded_utf8, read_bounded_jsonl_value, BoundedJsonlError};
use super::selection::CodingContinuationRef;
use super::{
    os_sandbox_command, require_outer_fence, CodingEngineAdapter, CodingEngineEvent,
    CodingEngineEventKind, CodingEngineEventSink, CodingEngineKind, CodingEngineRequest,
    CodingEngineRunResult, CodingFenceError, CodingLiveSessionReporter, CodingTurnUsage,
    CodingUsage,
};

const JSONRPC: &str = "2.0";
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_PENDING: usize = 128;
const MAX_EVENT_QUEUE: usize = 1024;
const MAX_ASSISTANT_BYTES: usize = 1024 * 1024;
const STDERR_RING_BYTES: usize = 256 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const GROK_CHILD_ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "TMPDIR",
    "LANG",
    "TERM",
    "GROK_HOME",
    "XAI_API_KEY",
];

#[derive(Debug, Clone, Copy)]
pub struct GrokSessionLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
    pub max_pending: usize,
    pub max_event_queue: usize,
}

impl Default for GrokSessionLimits {
    fn default() -> Self {
        Self {
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_line_bytes: MAX_LINE_BYTES,
            max_json_depth: MAX_JSON_DEPTH,
            max_pending: MAX_PENDING,
            max_event_queue: MAX_EVENT_QUEUE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrokSessionError {
    Timeout,
    Protocol(&'static str),
    Io,
    EventBackpressure,
    UnsupportedServerRequest(String),
    PromptNotCompleted,
    TurnFailed(String),
    FenceRequired,
}

impl std::fmt::Display for GrokSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Grok session timed out"),
            Self::Protocol(reason) => write!(f, "Grok protocol error: {reason}"),
            Self::Io => write!(f, "Grok session I/O failed"),
            Self::EventBackpressure => write!(f, "event_backpressure"),
            Self::UnsupportedServerRequest(method) => {
                write!(f, "unsupported Grok server request `{method}`")
            }
            Self::PromptNotCompleted => {
                write!(f, "Grok turn closed without a session/prompt result")
            }
            Self::TurnFailed(status) => write!(f, "Grok turn ended as {status}"),
            Self::FenceRequired => write!(
                f,
                "Grok dispatch requires the Magician outer coding fence; the Pi unsandboxed fallback is not allowed"
            ),
        }
    }
}

impl std::error::Error for GrokSessionError {}

impl From<CodingFenceError> for GrokSessionError {
    fn from(_: CodingFenceError) -> Self {
        Self::FenceRequired
    }
}

#[derive(Debug, Clone)]
pub struct GrokAcpAdapter {
    binary: PathBuf,
}

impl GrokAcpAdapter {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn from_options(options: GrokCodingOptions) -> Self {
        Self::new(options.binary)
    }

    pub fn launch_args(mode: super::factory::GrokTurnMode) -> Vec<String> {
        agent_stdio_args(mode)
    }
}

#[async_trait]
impl CodingEngineAdapter for GrokAcpAdapter {
    fn engine(&self) -> CodingEngineKind {
        CodingEngineKind::GrokAcp
    }

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult> {
        require_outer_fence().map_err(GrokSessionError::from)?;
        validate_request(&request)?;
        let mut child = OwnedGrokChild {
            child: spawn_agent(self, &request)?,
            reaped: false,
        };
        let stdin = child
            .child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Grok child stdin was not piped"))?;
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Grok child stdout was not piped"))?;
        if let Some(stderr) = child.child.stderr.take() {
            tokio::spawn(retain_stderr_ring(stderr));
        }
        let result =
            run_turn_over_stdio(&request, stdout, stdin, GrokSessionLimits::default()).await;
        child.reap().await;
        result.map_err(|error| anyhow!(error))
    }
}

fn validate_request(request: &CodingEngineRequest) -> Result<()> {
    if request.prompt.trim().is_empty() {
        return Err(anyhow!("Grok coding request prompt is empty"));
    }
    if !request.shadow_workspace_root.is_dir() {
        return Err(anyhow!(
            "Grok shadow workspace does not exist: {}",
            request.shadow_workspace_root.display()
        ));
    }
    Ok(())
}

pub fn filter_grok_child_env<'a, I>(entries: I) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let allowed: BTreeMap<&str, ()> = GROK_CHILD_ENV_ALLOWLIST
        .iter()
        .map(|key| (*key, ()))
        .collect();
    let mut filtered = BTreeMap::new();
    for (key, value) in entries {
        if key.starts_with("MAGICIAN_") {
            continue;
        }
        if allowed.contains_key(key) {
            filtered.insert(key.to_string(), value.to_string());
        }
    }
    filtered
}

fn spawn_agent(
    adapter: &GrokAcpAdapter,
    request: &CodingEngineRequest,
) -> Result<tokio::process::Child> {
    let args: Vec<OsString> = GrokAcpAdapter::launch_args(request.grok.mode)
        .into_iter()
        .map(OsString::from)
        .collect();
    debug_assert!(
        args.iter().any(|arg| arg == "--no-leader") && !args.iter().any(|arg| arg == "--leader")
    );
    let working_dir = request.shadow_workspace_root.as_path();
    let mut command = os_sandbox_command(adapter.binary.as_os_str(), &args);
    command.current_dir(working_dir);
    command.stdin(std::process::Stdio::piped());
    command.stdout(std::process::Stdio::piped());
    command.stderr(std::process::Stdio::piped());
    command.kill_on_drop(true);
    command.env_clear();
    let mut inherited: Vec<(String, String)> = std::env::vars().collect();
    inherited.extend(
        request
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let filtered = filter_grok_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    for (key, value) in filtered {
        command.env(key, value);
    }
    command.env("GIT_CEILING_DIRECTORIES", &request.scope_root);
    apply_grok_child_process_group(&mut command);
    command.spawn().with_context(|| {
        format!(
            "spawn Grok agent `{}` in {}",
            adapter.binary.display(),
            working_dir.display()
        )
    })
}

struct OwnedGrokChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl OwnedGrokChild {
    async fn reap(&mut self) {
        terminate_process_group(&mut self.child).await;
        self.reaped = true;
    }
}

impl Drop for OwnedGrokChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        force_kill_process_group(&mut self.child);
    }
}

async fn retain_stderr_ring<R>(stderr: R)
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut reader = BufReader::new(stderr);
    let mut ring = Vec::with_capacity(STDERR_RING_BYTES);
    let mut chunk = [0_u8; 4096];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let overflow = ring
                    .len()
                    .saturating_add(n)
                    .saturating_sub(STDERR_RING_BYTES);
                if overflow > 0 {
                    ring.drain(..overflow.min(ring.len()));
                }
                ring.extend_from_slice(&chunk[..n]);
            },
        }
    }
    let _ = ring;
}

pub(crate) fn apply_grok_child_process_group(command: &mut tokio::process::Command) {
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

pub(crate) fn force_kill_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

pub(crate) async fn terminate_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::killpg(pid as i32, libc::SIGTERM);
        }
    }
    match timeout(Duration::from_millis(500), child.wait()).await {
        Ok(Ok(_)) => {},
        _ => {
            #[cfg(unix)]
            if let Some(pid) = child.id() {
                unsafe {
                    libc::killpg(pid as i32, libc::SIGKILL);
                }
            }
            let _ = child.start_kill();
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
        },
    }
}

/// Drive one turn over already-connected JSONL streams. Tests use this
/// against a fake peer; the live spawn path is the only caller that
/// creates a process.
pub async fn run_turn_over_stdio<R, W>(
    request: &CodingEngineRequest,
    reader: R,
    writer: W,
    limits: GrokSessionLimits,
) -> Result<CodingEngineRunResult, GrokSessionError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    if request.prompt.trim().is_empty() {
        return Err(GrokSessionError::Protocol("empty prompt"));
    }
    let (control, control_rx) = if request.control_keys.is_empty() {
        (None, None)
    } else {
        let (handle, rx) = GrokTurnHandle::bind(
            1,
            request
                .grok
                .resume_session_id
                .clone()
                .unwrap_or_else(|| "pending".to_string()),
            request
                .run_execution_id
                .clone()
                .unwrap_or_else(|| "grok-exec".to_string()),
        );
        (Some(handle), Some(rx))
    };
    if let Some(handle) = control.as_ref() {
        coding_control_registry()
            .register(
                &request.control_keys,
                CodingControlHandle::Grok(handle.clone()),
            )
            .await;
    }
    let mut session = GrokJsonlSession {
        reader: BufReader::new(reader),
        writer,
        next_id: 1,
        inflight: HashMap::new(),
        limits,
        events: EventQueue::new(request.event_sink.clone(), limits.max_event_queue),
        assistant: String::new(),
        prompt_done: false,
        saw_agent_start: false,
        session_id: None,
        control,
        control_rx,
        cancelled: request.cancel_token.clone(),
        watchdog: request
            .budgets
            .filter(|budgets| budgets.no_progress_enabled)
            .map(|budgets| ProgressWatchdog::new(budgets, std::time::Instant::now())),
        continuation_fresh_reason: None,
        usage: None,
    };
    let outcome = session.drive(request).await;
    if !request.control_keys.is_empty() {
        coding_control_registry()
            .unregister(&request.control_keys)
            .await;
    }
    session.finalize(request, outcome)
}

struct GrokJsonlSession<R, W> {
    reader: BufReader<R>,
    writer: W,
    next_id: u64,
    inflight: HashMap<Value, ()>,
    limits: GrokSessionLimits,
    events: EventQueue,
    assistant: String,
    prompt_done: bool,
    saw_agent_start: bool,
    session_id: Option<String>,
    control: Option<GrokTurnHandle>,
    control_rx: Option<tokio::sync::mpsc::UnboundedReceiver<GrokControlCommand>>,
    cancelled: Option<tokio_util::sync::CancellationToken>,
    watchdog: Option<ProgressWatchdog>,
    continuation_fresh_reason: Option<ContinuationFreshReason>,
    usage: Option<GrokCapturedUsage>,
}

impl<R, W> GrokJsonlSession<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    async fn drive(&mut self, request: &CodingEngineRequest) -> Result<String, GrokSessionError> {
        self.request("initialize", initialize_params()).await?;

        let cwd = request.shadow_workspace_root.display().to_string();
        let session_id = self.open_or_resume_session(request, &cwd).await?;
        self.session_id = Some(session_id.clone());
        if let Some(handle) = self.control.as_mut() {
            handle.native_session_id = session_id.clone();
            handle.set_expected_turn(Some(session_id.clone())).await;
        }
        // `session/new` (or `session/load`) has answered, and `session/prompt`
        // has not been sent yet — so like Codex, the handle is durable before
        // the request that could hang. Note `set_expected_turn` above is handed
        // the SESSION id: ACP gives Grok no per-turn identifier, which is why
        // no `TurnAccepted` is reported here and the invocation stays at
        // `RequestMayHaveStarted`.
        if let Some(reporter) = CodingLiveSessionReporter::from_request(request) {
            reporter.report_live_session(CodingEngineKind::GrokAcp, &session_id);
        }
        if !self.saw_agent_start {
            self.push_event(synthetic_event(
                self.events.next_sequence(),
                CodingEngineEventKind::AgentStart,
                "session/ready",
            ))?;
            self.saw_agent_start = true;
        }

        self.run_prompt(request, &session_id, &request.prompt)
            .await?;
        loop {
            self.fail_if_stopped().await?;
            let Some(follow_up) = self.next_follow_up().await else {
                break;
            };
            self.prompt_done = false;
            self.run_prompt(request, &session_id, &follow_up).await?;
        }
        if let Some(handle) = self.control.as_ref() {
            handle.retire();
        }
        self.push_event(synthetic_event(
            self.events.next_sequence(),
            CodingEngineEventKind::AgentSettled,
            "session/prompt",
        ))?;
        Ok(session_id)
    }

    async fn open_or_resume_session(
        &mut self,
        request: &CodingEngineRequest,
        cwd: &str,
    ) -> Result<String, GrokSessionError> {
        if let Some(resume_id) = request
            .grok
            .resume_session_id
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            self.session_id = Some(resume_id.to_string());
            match self
                .request("session/load", session_load_params(resume_id, cwd))
                .await
            {
                Ok(loaded) => {
                    return grok_session_id_from(&loaded)
                        .or_else(|| Some(resume_id.to_string()))
                        .filter(|id| !id.is_empty())
                        .ok_or(GrokSessionError::Protocol("session/load missing sessionId"));
                },
                Err(GrokSessionError::Protocol("jsonrpc error")) => {
                    self.session_id = None;
                    self.inflight.clear();
                    self.continuation_fresh_reason =
                        Some(ContinuationFreshReason::ContinuationLost);
                    tracing::warn!(
                        target: "coding_engine",
                        "Grok ACP continuation_lost; starting a fresh session"
                    );
                },
                Err(error) => {
                    self.cancel_if_session().await;
                    self.inflight.clear();
                    return Err(error);
                },
            }
        }
        let started = self.request("session/new", session_new_params(cwd)).await?;
        grok_session_id_from(&started)
            .ok_or(GrokSessionError::Protocol("session/new missing sessionId"))
    }

    async fn run_prompt(
        &mut self,
        request: &CodingEngineRequest,
        session_id: &str,
        prompt: &str,
    ) -> Result<(), GrokSessionError> {
        let prompt_result = self
            .request_until(
                "session/prompt",
                session_prompt_params(session_id, prompt),
                request.timeout,
            )
            .await;
        let result = match prompt_result {
            Ok(result) => result,
            Err(GrokSessionError::Timeout) => {
                self.cancel_if_session().await;
                return Err(GrokSessionError::Timeout);
            },
            Err(error) => {
                if matches!(
                    &error,
                    GrokSessionError::TurnFailed(status) if status == "cancelled"
                ) {
                    self.cancel_if_session().await;
                }
                return Err(error);
            },
        };
        self.absorb_usage_from(&result);
        prompt_stop_status(&result)?;
        self.prompt_done = true;
        self.push_event(self.turn_end_event("session/prompt"))?;
        Ok(())
    }

    async fn next_follow_up(&self) -> Option<String> {
        match self.control.as_ref() {
            Some(handle) => handle.take_follow_up().await,
            None => None,
        }
    }

    async fn cancel_if_session(&mut self) {
        let Some(session_id) = self.session_id.clone() else {
            return;
        };
        let _ = self
            .notify("session/cancel", session_cancel_params(&session_id))
            .await;
    }

    /// ACP `session/cancel` is a notification: no `id`, no result wait.
    async fn notify(&mut self, method: &str, params: Value) -> Result<(), GrokSessionError> {
        if !may_send_method(method) {
            return Err(GrokSessionError::Protocol(
                "method is not on the V1 allowlist",
            ));
        }
        self.write_line(&json!({
            "jsonrpc": JSONRPC,
            "method": method,
            "params": params,
        }))
        .await
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, GrokSessionError> {
        self.request_until(method, params, self.limits.request_timeout)
            .await
    }

    async fn request_until(
        &mut self,
        method: &str,
        params: Value,
        bound: Duration,
    ) -> Result<Value, GrokSessionError> {
        if !may_send_method(method) {
            return Err(GrokSessionError::Protocol(
                "method is not on the V1 allowlist",
            ));
        }
        if self.inflight.len() >= self.limits.max_pending {
            return Err(GrokSessionError::Protocol("pending request map exhausted"));
        }
        let id = json!(self.next_id);
        self.next_id += 1;
        if self.inflight.insert(id.clone(), ()).is_some() {
            return Err(GrokSessionError::Protocol("duplicate client request id"));
        }
        self.write_line(&json!({
            "jsonrpc": JSONRPC,
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        self.drain_until_result(id, bound).await
    }

    /// Drain JSONL until `id`'s result. `initialize` / `session/new` /
    /// `session/load` keep the short RPC bound; `session/prompt` uses the
    /// outer turn timeout so a live ACP turn is not killed at 30s. The JSONL
    /// read is `select!`'d against cancel, Grok control Interrupt, and the
    /// next watchdog deadline so a silent peer cannot pin the loop until the
    /// outer bound.
    async fn drain_until_result(
        &mut self,
        id: Value,
        bound: Duration,
    ) -> Result<Value, GrokSessionError> {
        let deadline = tokio::time::Instant::now() + bound;
        loop {
            if let Err(error) = self.check_liveness() {
                return Err(error);
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(GrokSessionError::Timeout);
            }
            let remaining = deadline.saturating_duration_since(now);
            let wait = self
                .watchdog
                .as_ref()
                .and_then(ProgressWatchdog::deadline)
                .map(|at| at.saturating_duration_since(std::time::Instant::now()))
                .map(|until_watchdog| until_watchdog.min(remaining))
                .unwrap_or(remaining)
                .max(Duration::from_millis(1));
            let cancelled = self.cancelled.clone();
            let max_line = self.limits.max_line_bytes;
            let max_depth = self.limits.max_json_depth;
            tokio::select! {
                biased;
                _ = wait_if_cancelled(cancelled) => {
                    return Err(GrokSessionError::TurnFailed("cancelled".into()));
                }
                _ = wait_control_interrupt(self.control_rx.as_mut()) => {
                    return Err(GrokSessionError::TurnFailed("cancelled".into()));
                }
                incoming = read_bounded_jsonl_value(
                    &mut self.reader,
                    max_line,
                    max_depth,
                ) => {
                    let value = map_bounded_jsonl(incoming)?;
                    if let Some(result) = self.handle_incoming(value).await? {
                        if jsonrpc_id(&result).as_ref() == Some(&id) {
                            if let Some(payload) = result.get("result").cloned() {
                                return Ok(payload);
                            }
                            if result.get("error").is_some() {
                                return Err(GrokSessionError::Protocol("jsonrpc error"));
                            }
                            return Ok(result);
                        }
                        return Err(GrokSessionError::Protocol(
                            "response id did not match the in-flight request",
                        ));
                    }
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    async fn write_line(&mut self, value: &Value) -> Result<(), GrokSessionError> {
        let mut line =
            serde_json::to_vec(value).map_err(|_| GrokSessionError::Protocol("encode"))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|_| GrokSessionError::Io)?;
        self.writer.flush().await.map_err(|_| GrokSessionError::Io)
    }

    fn check_liveness(&mut self) -> Result<(), GrokSessionError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(GrokSessionError::TurnFailed("cancelled".into()));
        }
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(GrokSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    async fn fail_if_stopped(&mut self) -> Result<(), GrokSessionError> {
        if let Err(error) = self.check_liveness() {
            self.cancel_if_session().await;
            return Err(error);
        }
        if self.take_pending_interrupt() {
            self.cancel_if_session().await;
            return Err(GrokSessionError::TurnFailed("cancelled".into()));
        }
        Ok(())
    }

    fn take_pending_interrupt(&mut self) -> bool {
        let Some(rx) = self.control_rx.as_mut() else {
            return false;
        };
        match rx.try_recv() {
            Ok(GrokControlCommand::Interrupt { .. }) => true,
            Err(_) => false,
        }
    }

    async fn handle_incoming(&mut self, value: Value) -> Result<Option<Value>, GrokSessionError> {
        if let Some(method) = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if let Some(id) = jsonrpc_id(&value) {
                return self.handle_server_request(id, &method).await.map(|_| None);
            }
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            self.handle_notification(&method, params)?;
            return Ok(None);
        }
        if let Some(id) = jsonrpc_id(&value) {
            if self.inflight.remove(&id).is_none() {
                return Err(GrokSessionError::Protocol(
                    "unknown or duplicate response id",
                ));
            }
            return Ok(Some(value));
        }
        Err(GrokSessionError::Protocol("unclassified JSONL record"))
    }

    async fn handle_server_request(
        &mut self,
        id: Value,
        method: &str,
    ) -> Result<(), GrokSessionError> {
        let _ = self
            .write_line(&json!({
                "jsonrpc": JSONRPC,
                "id": id,
                "error": { "code": -32601, "message": "unsupported" }
            }))
            .await;
        Err(GrokSessionError::UnsupportedServerRequest(
            method.to_string(),
        ))
    }

    fn handle_notification(&mut self, method: &str, params: Value) -> Result<(), GrokSessionError> {
        if method.starts_with(GROK_ACP_PROHIBITED_PREFIX) {
            return Ok(());
        }
        if method == "session/update" {
            append_assistant_text(&mut self.assistant, &params);
        }
        self.absorb_usage_from(&params);
        let mut event = project_notification(self.events.next_sequence(), method, &params);
        if let Some(usage) = self.usage.as_ref() {
            event.usage = Some(usage.to_coding_usage());
            event.cost_total = usage.cost;
        }
        if event.kind == CodingEngineEventKind::AgentStart {
            self.saw_agent_start = true;
        }
        self.push_event(event)?;
        Ok(())
    }

    fn absorb_usage_from(&mut self, value: &Value) {
        let Some(parsed) = parse_grok_acp_usage(value) else {
            return;
        };
        match &mut self.usage {
            None => self.usage = Some(parsed),
            Some(current) => current.merge(parsed),
        }
    }

    fn turn_end_event(&self, method: &str) -> CodingEngineEvent {
        let mut event = synthetic_event(
            self.events.next_sequence(),
            CodingEngineEventKind::TurnEnd,
            method,
        );
        if let Some(usage) = self.usage.as_ref() {
            event.usage = Some(usage.to_coding_usage());
            event.cost_total = usage.cost;
        }
        event
    }

    fn push_event(&mut self, event: CodingEngineEvent) -> Result<(), GrokSessionError> {
        if let Some(detector) = self.watchdog.as_mut() {
            detector.observe(&event, std::time::Instant::now());
        }
        self.events.push(event)?;
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(GrokSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn finalize(
        mut self,
        request: &CodingEngineRequest,
        outcome: Result<String, GrokSessionError>,
    ) -> Result<CodingEngineRunResult, GrokSessionError> {
        let success = matches!(&outcome, Ok(_)) && self.prompt_done;
        write_turn_usage(request, self.usage.as_ref(), success);
        if let Err(error) = &outcome {
            file_session_termination(request, error);
        }
        let events = self.events.finish();
        let session_id = outcome?;
        if !self.prompt_done {
            return Err(GrokSessionError::PromptNotCompleted);
        }
        let invocation_id = format!("grok-{}", Uuid::new_v4());
        let continuation = CodingContinuationRef::for_grok_session(
            session_id,
            &request.scope_root,
            &request.workspace_root,
            request.run_task_id.as_deref(),
        );
        Ok(CodingEngineRunResult {
            engine: CodingEngineKind::GrokAcp,
            session_id: Some(invocation_id),
            session_file: None,
            assistant_text: (!self.assistant.is_empty()).then_some(self.assistant),
            event_count: events,
            continuation: Some(continuation),
            continuation_fresh_reason: self.continuation_fresh_reason,
            proposal: None,
            approval_payload: None,
            session_stats: None,
        })
    }
}

/// ACP / headless usage as Grok reports it. `cost` is `None` when the
/// provider omitted USD — never a billed `$0.00`.
#[derive(Debug, Clone, Default, PartialEq)]
struct GrokCapturedUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total_tokens: u64,
    cost: Option<f64>,
}

impl GrokCapturedUsage {
    fn merge(&mut self, other: Self) {
        if other.input > 0 {
            self.input = other.input;
        }
        if other.output > 0 {
            self.output = other.output;
        }
        if other.cache_read > 0 {
            self.cache_read = other.cache_read;
        }
        if other.cache_write > 0 {
            self.cache_write = other.cache_write;
        }
        if other.total_tokens > 0 {
            self.total_tokens = other.total_tokens;
        }
        if other.cost.is_some() {
            self.cost = other.cost;
        }
    }

    fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.cache_read == 0
            && self.cache_write == 0
            && self.total_tokens == 0
            && self.cost.is_none()
    }

    fn to_coding_usage(&self) -> CodingUsage {
        CodingUsage {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            total_tokens: self.total_tokens,
            cost_total: self.cost,
        }
    }

    fn to_turn_usage(&self, success: bool) -> CodingTurnUsage {
        CodingTurnUsage {
            // Slot default only; billed USD is `cost` when `cost_known`.
            cost: self.cost.unwrap_or(0.0),
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write: self.cache_write,
            success,
            cost_known: self.cost.is_some(),
        }
    }
}

fn parse_grok_acp_usage(value: &Value) -> Option<GrokCapturedUsage> {
    let mut parsed = None;
    for candidate in usage_candidate_objects(value) {
        if let Some(usage) = parse_usage_object(candidate) {
            match &mut parsed {
                None => parsed = Some(usage),
                Some(current) => current.merge(usage),
            }
        }
    }
    for root in [value]
        .into_iter()
        .chain(value.get("update"))
        .chain(value.get("_meta"))
        .chain(value.pointer("/update/_meta"))
        .chain(value.get("result"))
        .chain(value.pointer("/result/_meta"))
    {
        if let Some(cost) = extract_reported_cost(root) {
            parsed.get_or_insert_with(GrokCapturedUsage::default).cost = Some(cost);
            break;
        }
    }
    parsed.filter(|usage| !usage.is_empty())
}

fn usage_candidate_objects(value: &Value) -> Vec<&Value> {
    let mut out = Vec::new();
    for pointer in [
        "/usage",
        "/_meta/usage",
        "/update/usage",
        "/update/_meta/usage",
        "/params/usage",
        "/params/_meta/usage",
        "/result/usage",
        "/result/_meta/usage",
    ] {
        if let Some(candidate) = value.pointer(pointer) {
            out.push(candidate);
        }
    }
    if value.get("input_tokens").is_some()
        || value.get("inputTokens").is_some()
        || value.get("input").is_some()
        || value.get("output_tokens").is_some()
    {
        out.push(value);
    }
    out
}

fn parse_usage_object(value: &Value) -> Option<GrokCapturedUsage> {
    if !value.is_object() {
        return None;
    }
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| json_u64(value.get(*key)?))
            .unwrap_or(0)
    };
    let usage = GrokCapturedUsage {
        input: count(&["input", "inputTokens", "input_tokens"]),
        output: count(&["output", "outputTokens", "output_tokens"]),
        cache_read: count(&[
            "cache_read",
            "cacheRead",
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cachedInputTokens",
        ]),
        cache_write: count(&[
            "cache_write",
            "cacheWrite",
            "cache_creation_input_tokens",
            "cacheCreationInputTokens",
            "cacheWriteTokens",
        ]),
        total_tokens: count(&["total_tokens", "totalTokens", "total"]),
        cost: extract_reported_cost(value),
    };
    if usage.is_empty() {
        None
    } else {
        Some(usage)
    }
}

fn extract_reported_cost(value: &Value) -> Option<f64> {
    if let Some(cost) = json_f64(value.get("total_cost_usd"))
        .or_else(|| json_f64(value.get("costUsd")))
        .or_else(|| json_f64(value.get("costUSD")))
        .or_else(|| json_f64(value.get("cost_usd")))
    {
        return Some(cost);
    }
    if let Some(cost) = value.get("cost") {
        if let Some(amount) = json_f64(Some(cost)) {
            return Some(amount);
        }
        let usd = match cost.get("currency").and_then(Value::as_str) {
            None => true,
            Some(currency) => currency.eq_ignore_ascii_case("usd"),
        };
        if usd {
            if let Some(amount) = json_f64(cost.get("total"))
                .or_else(|| json_f64(cost.get("amount")))
                .or_else(|| json_f64(cost.get("usd")))
            {
                return Some(amount);
            }
        }
    }
    model_usage_cost(value)
}

fn model_usage_cost(value: &Value) -> Option<f64> {
    let models = value
        .get("modelUsage")
        .or_else(|| value.get("model_usage"))?;
    let map = models.as_object()?;
    let mut total = 0.0;
    let mut any = false;
    for entry in map.values() {
        if let Some(cost) = json_f64(entry.get("costUSD"))
            .or_else(|| json_f64(entry.get("costUsd")))
            .or_else(|| json_f64(entry.get("cost_usd")))
            .or_else(|| json_f64(entry.get("cost")))
        {
            total += cost;
            any = true;
        }
    }
    any.then_some(total)
}

fn json_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|n| u64::try_from(n).ok()))
        .or_else(|| value.as_f64().and_then(|n| (n >= 0.0).then_some(n as u64)))
}

fn json_f64(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    value
        .as_f64()
        .or_else(|| value.as_u64().map(|n| n as f64))
        .or_else(|| value.as_i64().map(|n| n as f64))
        .filter(|n| n.is_finite())
}

fn write_turn_usage(
    request: &CodingEngineRequest,
    usage: Option<&GrokCapturedUsage>,
    success: bool,
) {
    let Some(cell) = request.usage_capture.as_ref() else {
        return;
    };
    let Some(usage) = usage else {
        return;
    };
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(usage.to_turn_usage(success));
    }
}

fn file_session_termination(request: &CodingEngineRequest, error: &GrokSessionError) {
    let Some(key) = request.termination_key.as_deref() else {
        return;
    };
    let reason = match error {
        GrokSessionError::Timeout => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        GrokSessionError::TurnFailed(status) if status == "cancelled" => {
            CodingTerminationReason::OwnerCancelled
        },
        GrokSessionError::TurnFailed(status) if status == "no_progress" => {
            CodingTerminationReason::NoProgress {
                phase: super::budgets::CodingProgressPhase::Model,
                elapsed_secs: 0,
                last_substantive_event: None,
            }
        },
        GrokSessionError::EventBackpressure => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        _ => return,
    };
    record_termination_reason(key, reason);
}

struct EventQueue {
    pending_delta: Option<CodingEngineEvent>,
    emitted: u64,
    next_seq: usize,
    max: usize,
    sink: Option<CodingEngineEventSink>,
}

impl EventQueue {
    fn new(sink: Option<CodingEngineEventSink>, max: usize) -> Self {
        Self {
            pending_delta: None,
            emitted: 0,
            next_seq: 1,
            max,
            sink,
        }
    }

    fn next_sequence(&self) -> usize {
        self.next_seq
    }

    fn push(&mut self, event: CodingEngineEvent) -> Result<(), GrokSessionError> {
        let coalescible = matches!(
            event.kind,
            CodingEngineEventKind::MessageUpdate | CodingEngineEventKind::ToolExecutionUpdate
        );
        if coalescible {
            if let Some(pending) = self.pending_delta.as_mut() {
                let same_channel = pending.text_delta.is_some() == event.text_delta.is_some()
                    && pending.thinking_delta.is_some() == event.thinking_delta.is_some();
                if pending.kind == event.kind
                    && pending.tool_call_id == event.tool_call_id
                    && same_channel
                {
                    if pending.kind == CodingEngineEventKind::MessageUpdate {
                        coalesce_message_deltas(pending, event);
                    } else {
                        *pending = event;
                    }
                    return Ok(());
                }
            }
            self.flush_pending()?;
            self.pending_delta = Some(event);
            return Ok(());
        }
        self.flush_pending()?;
        self.emit(event)
    }

    fn flush_pending(&mut self) -> Result<(), GrokSessionError> {
        if let Some(event) = self.pending_delta.take() {
            self.emit(event)?;
        }
        Ok(())
    }

    fn emit(&mut self, event: CodingEngineEvent) -> Result<(), GrokSessionError> {
        if self.emitted >= self.max as u64 {
            return Err(GrokSessionError::EventBackpressure);
        }
        if let Some(sink) = self.sink.as_ref() {
            sink(&event);
        }
        self.next_seq = event.sequence.saturating_add(1);
        self.emitted = self.emitted.saturating_add(1);
        Ok(())
    }

    fn finish(&mut self) -> u64 {
        let _ = self.flush_pending();
        self.emitted
    }
}

fn coalesce_message_deltas(pending: &mut CodingEngineEvent, next: CodingEngineEvent) {
    pending.text_delta = concat_delta(pending.text_delta.take(), next.text_delta);
    pending.thinking_delta = concat_delta(pending.thinking_delta.take(), next.thinking_delta);
}

fn concat_delta(left: Option<String>, right: Option<String>) -> Option<String> {
    match (left, right) {
        (None, None) => None,
        (Some(text), None) | (None, Some(text)) => Some(text),
        (Some(mut left), Some(right)) => {
            append_bounded_utf8(&mut left, &right, MAX_ASSISTANT_BYTES);
            Some(left)
        },
    }
}

fn map_bounded_jsonl(result: Result<Value, BoundedJsonlError>) -> Result<Value, GrokSessionError> {
    match result {
        Ok(value) => Ok(value),
        Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => Err(GrokSessionError::Io),
        Err(BoundedJsonlError::Oversized) => {
            Err(GrokSessionError::Protocol("oversized JSONL line"))
        },
        Err(BoundedJsonlError::Malformed) => Err(GrokSessionError::Protocol("malformed JSON")),
        Err(BoundedJsonlError::TooDeep) => Err(GrokSessionError::Protocol("JSON nesting too deep")),
    }
}

async fn wait_if_cancelled(token: Option<tokio_util::sync::CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

async fn wait_control_interrupt(
    rx: Option<&mut tokio::sync::mpsc::UnboundedReceiver<GrokControlCommand>>,
) {
    let Some(rx) = rx else {
        std::future::pending::<()>().await;
        return;
    };
    loop {
        match rx.recv().await {
            Some(GrokControlCommand::Interrupt { .. }) => return,
            None => std::future::pending::<()>().await,
        }
    }
}

fn session_prompt_params(session_id: &str, prompt: &str) -> Value {
    json!({
        "sessionId": session_id,
        "prompt": [{ "type": "text", "text": prompt }],
    })
}

fn prompt_stop_status(result: &Value) -> Result<(), GrokSessionError> {
    let Some(reason) = stop_reason(result) else {
        return Ok(());
    };
    if reason == "cancelled" || reason == "canceled" {
        return Err(GrokSessionError::TurnFailed("cancelled".into()));
    }
    if reason == "error" || reason == "refused" {
        return Err(GrokSessionError::TurnFailed(reason.to_string()));
    }
    Ok(())
}

fn synthetic_event(
    sequence: usize,
    kind: CodingEngineEventKind,
    method: &str,
) -> CodingEngineEvent {
    CodingEngineEvent {
        sequence,
        kind,
        raw_type: Some(method.to_string()),
        text_delta: None,
        tool_name: None,
        tool_call_id: None,
        thinking_delta: None,
        assistant_event_type: None,
        usage: None,
        cost_total: None,
        stop_reason: None,
        tool_result_is_error: None,
        will_retry: None,
        error_message: None,
        raw: json!({
            "type": kind.as_str(),
            "method": method,
        }),
    }
}

fn project_notification(sequence: usize, method: &str, params: &Value) -> CodingEngineEvent {
    let update = params.get("update").unwrap_or(params);
    let session_update = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let (kind, tool_name, tool_call_id, text_delta, thinking_delta, tool_result_is_error) =
        match (method, session_update) {
            ("session/update", "agent_message_chunk") => (
                CodingEngineEventKind::MessageUpdate,
                None,
                None,
                text_from_update(update),
                None,
                None,
            ),
            ("session/update", "agent_thought_chunk") => (
                CodingEngineEventKind::MessageUpdate,
                None,
                None,
                None,
                text_from_update(update),
                None,
            ),
            ("session/update", "tool_call") => (
                CodingEngineEventKind::ToolExecutionStart,
                tool_name_from(update),
                tool_call_id_from(update),
                None,
                None,
                None,
            ),
            ("session/update", "tool_call_update") if tool_status_completed(update) => (
                CodingEngineEventKind::ToolExecutionEnd,
                tool_name_from(update),
                tool_call_id_from(update),
                None,
                None,
                Some(tool_status_error(update)),
            ),
            ("session/update", "tool_call_update") => (
                CodingEngineEventKind::ToolExecutionUpdate,
                tool_name_from(update),
                tool_call_id_from(update),
                None,
                None,
                None,
            ),
            ("session/update", "plan") => {
                (CodingEngineEventKind::Unknown, None, None, None, None, None)
            },
            _ => (CodingEngineEventKind::Unknown, None, None, None, None, None),
        };
    let digest = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(method.as_bytes());
        hasher.update(b":");
        hasher.update(session_update.as_bytes());
        if let Some(item) = tool_call_id.as_deref() {
            hasher.update(b":");
            hasher.update(item.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    };
    CodingEngineEvent {
        sequence,
        kind,
        raw_type: Some(if session_update.is_empty() {
            method.to_string()
        } else {
            session_update.to_string()
        }),
        text_delta,
        tool_name,
        tool_call_id,
        thinking_delta,
        assistant_event_type: None,
        usage: None,
        cost_total: None,
        stop_reason: None,
        tool_result_is_error,
        will_retry: None,
        error_message: None,
        raw: json!({
            "type": kind.as_str(),
            "method": method,
            "sessionUpdate": session_update,
            "digest": digest,
        }),
    }
}

fn append_assistant_text(buffer: &mut String, params: &Value) {
    let update = params.get("update").unwrap_or(params);
    let session_update = update
        .get("sessionUpdate")
        .or_else(|| update.get("session_update"))
        .and_then(Value::as_str);
    if session_update != Some("agent_message_chunk") {
        return;
    }
    if let Some(text) = text_from_update(update) {
        append_bounded_utf8(buffer, &text, MAX_ASSISTANT_BYTES);
    }
}

fn text_from_update(update: &Value) -> Option<String> {
    let content = update.get("content")?;
    if let Some(text) = content.as_str().filter(|text| !text.is_empty()) {
        return Some(text.to_string());
    }
    if let Some(text) = content
        .get("text")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return Some(text.to_string());
    }
    if let Some(items) = content.as_array() {
        let mut joined = String::new();
        for item in items {
            if let Some(text) = item
                .get("text")
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                joined.push_str(text);
            }
        }
        return (!joined.is_empty()).then_some(joined);
    }
    None
}

fn tool_call_id_from(update: &Value) -> Option<String> {
    update
        .get("toolCallId")
        .or_else(|| update.get("tool_call_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tool_name_from(update: &Value) -> Option<String> {
    update
        .get("title")
        .or_else(|| update.get("kind"))
        .or_else(|| update.get("toolName"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn tool_status(update: &Value) -> Option<&str> {
    update.get("status").and_then(Value::as_str)
}

fn tool_status_completed(update: &Value) -> bool {
    matches!(tool_status(update), Some("completed" | "failed"))
}

fn tool_status_error(update: &Value) -> bool {
    tool_status(update) == Some("failed")
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
fn message_id(value: &Value) -> Option<u64> {
    jsonrpc_id(value).and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()))
}

fn stop_reason(result: &Value) -> Option<&str> {
    result
        .get("stopReason")
        .or_else(|| result.get("stop_reason"))
        .and_then(Value::as_str)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::coding_engine::budgets::ResolvedCodingBudgets;
    use crate::magician_v2::execution::coding_engine::control::{
        coding_control_registry, CodingControlAction,
    };
    use crate::magician_v2::execution::coding_engine::{
        attach_staged_coding_proposal, construct_coding_adapter, CodingAdapterSpec,
        GrokCodingOptions, GrokTurnMode, GrokTurnOptions, PiTurnOptions,
    };
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use std::sync::{Arc, Mutex};
    use tokio::io::{duplex, AsyncBufReadExt};
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

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

    async fn scripted_peer(
        stream: tokio::io::DuplexStream,
        script: impl Fn(&str, u64) -> Option<Value> + Send + 'static,
        notifications: Vec<Value>,
    ) {
        scripted_peer_delayed(stream, script, notifications, Duration::ZERO).await
    }

    async fn scripted_peer_delayed(
        stream: tokio::io::DuplexStream,
        script: impl Fn(&str, u64) -> Option<Value> + Send + 'static,
        notifications: Vec<Value>,
        prompt_delay: Duration,
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
            if request.get("id").is_none() {
                continue;
            }
            let Some(id) = message_id(&request) else {
                continue;
            };
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            if method == "session/prompt" {
                if !prompt_delay.is_zero() {
                    tokio::time::sleep(prompt_delay).await;
                }
                for note in &notifications {
                    let mut payload = serde_json::to_vec(note).unwrap();
                    payload.push(b'\n');
                    if writer.write_all(&payload).await.is_err() {
                        return;
                    }
                }
            }
            let Some(result) = script(method, id) else {
                continue;
            };
            let mut payload = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
            .unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                break;
            }
        }
    }

    fn happy_script(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/new" => Some(json!({ "sessionId": "sess-secret" })),
            "session/prompt" => Some(json!({ "stopReason": "end_turn" })),
            "session/cancel" => Some(json!({})),
            _ => None,
        }
    }

    fn completed_notes() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello "}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world"}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_thought_chunk","content":{"type":"text","text":"hmm"}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"tool_call","toolCallId":"t1","title":"read","status":"pending"}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"tool_call_update","toolCallId":"t1","status":"completed"}}}),
        ]
    }

    fn hello_world_notes() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"hello "}}}}),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"world"}}}}),
        ]
    }

    fn joined_text_deltas(events: &[CodingEngineEvent]) -> String {
        events
            .iter()
            .filter(|event| event.kind == CodingEngineEventKind::MessageUpdate)
            .filter_map(|event| event.text_delta.as_deref())
            .collect()
    }

    fn silent_after_session_new(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/new" => Some(json!({ "sessionId": "sess-secret" })),
            _ => None,
        }
    }

    async fn capture_client_frames(
        stream: tokio::io::DuplexStream,
        script: impl Fn(&str, u64) -> Option<Value> + Send + 'static,
        frames: Arc<Mutex<Vec<Value>>>,
    ) {
        capture_client_frames_delayed(stream, script, frames, Duration::ZERO).await
    }

    async fn capture_client_frames_delayed(
        stream: tokio::io::DuplexStream,
        script: impl Fn(&str, u64) -> Option<Value> + Send + 'static,
        frames: Arc<Mutex<Vec<Value>>>,
        prompt_delay: Duration,
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
            frames.lock().unwrap().push(request.clone());
            if request.get("id").is_none() {
                continue;
            }
            let Some(id) = message_id(&request) else {
                continue;
            };
            let method = request.get("method").and_then(Value::as_str).unwrap_or("");
            if method == "session/prompt" && !prompt_delay.is_zero() {
                tokio::time::sleep(prompt_delay).await;
            }
            let Some(result) = script(method, id) else {
                continue;
            };
            let mut payload = serde_json::to_vec(&json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result
            }))
            .unwrap();
            payload.push(b'\n');
            if writer.write_all(&payload).await.is_err() {
                break;
            }
        }
    }

    fn resume_script(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/load" => Some(json!({ "sessionId": "sess-secret" })),
            "session/new" => Some(json!({ "sessionId": "sess-fresh" })),
            "session/prompt" => Some(json!({ "stopReason": "end_turn" })),
            _ => None,
        }
    }

    fn methods_named(frames: &[Value], name: &str) -> usize {
        frames
            .iter()
            .filter(|frame| frame.get("method").and_then(Value::as_str) == Some(name))
            .count()
    }

    fn prompt_texts(frames: &[Value]) -> Vec<String> {
        frames
            .iter()
            .filter(|frame| frame.get("method").and_then(Value::as_str) == Some("session/prompt"))
            .filter_map(|frame| {
                frame
                    .pointer("/params/prompt/0/text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[tokio::test]
    async fn fake_turn_maps_events_and_hides_the_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(32 * 1024);
        let peer = tokio::spawn(scripted_peer(server, happy_script, completed_notes()));
        let (reader, writer) = tokio::io::split(client);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut request = request_for(dir.path());
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            sink_seen.lock().unwrap().push(event.clone());
        }));
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
        assert!(result.event_count >= 4);
        assert!(result
            .session_id
            .as_deref()
            .is_some_and(|id| id.starts_with("grok-") && !id.contains("sess-secret")));
        let continuation = result.continuation.expect("continuation");
        assert_eq!(continuation.engine, CodingEngineKind::GrokAcp);
        assert_eq!(continuation.native_session_id, "sess-secret");
        let events = seen.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::AgentStart));
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::AgentSettled));
        assert_eq!(
            joined_text_deltas(&events),
            "hello world",
            "coalesced agent_message_chunk text_delta must keep every token, got {events:?}"
        );
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::ToolExecutionStart));
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::ToolExecutionEnd));
        for event in events.iter() {
            let raw = event.raw.to_string();
            assert!(
                !raw.contains("sess-secret"),
                "native session id leaked into event raw: {raw}"
            );
        }
    }

    #[test]
    fn discuss_and_build_send_distinct_sandbox_flags() {
        let build = GrokAcpAdapter::launch_args(GrokTurnMode::Build);
        let discuss = GrokAcpAdapter::launch_args(GrokTurnMode::Discuss);
        assert!(build
            .windows(2)
            .any(|pair| pair == ["--sandbox", "workspace"]));
        assert!(discuss
            .windows(2)
            .any(|pair| pair == ["--sandbox", "read-only"]));
        for args in [&build, &discuss] {
            assert!(args.iter().any(|arg| arg == "--no-leader"));
            assert!(args.iter().any(|arg| arg == "--always-approve"));
            assert!(args.iter().any(|arg| arg == "stdio"));
            assert!(args.iter().any(|arg| arg == "--disable-web-search"));
            assert!(args.iter().any(|arg| arg == "--no-subagents"));
            assert!(!args.iter().any(|arg| arg == "--leader"));
        }
    }

    #[tokio::test]
    async fn missing_prompt_result_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(
            server,
            |method, _id| match method {
                "initialize" => Some(json!({ "protocolVersion": 1 })),
                "session/new" => Some(json!({ "sessionId": "sess-secret" })),
                _ => None,
            },
            vec![],
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut limits = GrokSessionLimits::default();
        limits.request_timeout = Duration::from_millis(80);
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_millis(80);
        let capture = Arc::new(Mutex::new(None));
        request.usage_capture = Some(capture.clone());
        let error = run_turn_over_stdio(&request, reader, writer, limits)
            .await
            .expect_err("no prompt result");
        let _ = peer.await;
        assert!(matches!(
            error,
            GrokSessionError::Timeout | GrokSessionError::Io | GrokSessionError::PromptNotCompleted
        ));
        assert!(
            capture.lock().unwrap().is_none(),
            "omitted ACP usage must not write a zero capture"
        );
    }

    #[tokio::test]
    async fn prompt_result_past_rpc_timeout_still_succeeds_within_turn_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer_delayed(
            server,
            happy_script,
            completed_notes(),
            Duration::from_millis(200),
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut limits = GrokSessionLimits::default();
        limits.request_timeout = Duration::from_millis(50);
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_secs(2);
        let result = run_turn_over_stdio(&request, reader, writer, limits)
            .await
            .expect("turn must use request.timeout for session/prompt");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
    }

    #[tokio::test]
    async fn successful_turn_without_usage_leaves_capture_empty() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, happy_script, completed_notes()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        let capture = Arc::new(Mutex::new(None));
        request.usage_capture = Some(capture.clone());
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        assert!(
            capture.lock().unwrap().is_none(),
            "ACP omitted usage must leave usage_capture empty, not Some(default zeros)"
        );
    }

    fn usage_script(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/new" => Some(json!({ "sessionId": "sess-secret" })),
            "session/prompt" => Some(json!({
                "stopReason": "end_turn",
                "usage": {
                    "input_tokens": 12,
                    "output_tokens": 4,
                    "cache_read_input_tokens": 3,
                    "cache_creation_input_tokens": 1,
                    "total_tokens": 20
                }
            })),
            "session/cancel" => Some(json!({})),
            _ => None,
        }
    }

    fn priced_usage_script(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/new" => Some(json!({ "sessionId": "sess-secret" })),
            "session/prompt" => Some(json!({
                "stopReason": "end_turn",
                "_meta": { "usage": { "input_tokens": 8, "output_tokens": 2 } },
                "total_cost_usd": 0.0125
            })),
            "session/cancel" => Some(json!({})),
            _ => None,
        }
    }

    fn cancelled_script(method: &str, _id: u64) -> Option<Value> {
        match method {
            "initialize" => Some(json!({ "protocolVersion": 1 })),
            "session/new" => Some(json!({ "sessionId": "sess-secret" })),
            "session/prompt" => Some(json!({
                "stopReason": "cancelled",
                "usage": { "input_tokens": 5, "output_tokens": 1 }
            })),
            "session/cancel" => Some(json!({})),
            _ => None,
        }
    }

    #[tokio::test]
    async fn prompt_result_usage_without_cost_is_unknown_not_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, usage_script, completed_notes()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        let capture = Arc::new(Mutex::new(None));
        request.usage_capture = Some(capture.clone());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            sink_seen.lock().unwrap().push(event.clone());
        }));
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        let captured = capture.lock().unwrap().clone().expect("usage");
        assert!(captured.success);
        assert_eq!(captured.input, 12);
        assert_eq!(captured.output, 4);
        assert_eq!(captured.cache_read, 3);
        assert_eq!(captured.cache_write, 1);
        assert_eq!(captured.cost, 0.0);
        assert!(!captured.cost_known);
        let events = seen.lock().unwrap();
        let turn_end = events
            .iter()
            .find(|event| event.kind == CodingEngineEventKind::TurnEnd)
            .expect("turn end");
        assert!(turn_end.cost_total.is_none(), "{turn_end:?}");
        assert_eq!(turn_end.usage.as_ref().map(|usage| usage.input), Some(12));
        assert_eq!(
            turn_end.usage.as_ref().and_then(|usage| usage.cost_total),
            None,
            "token-only Grok usage must not stuff $0.00 into CodingUsage.cost_total"
        );
        assert!(captured.has_reported_spend());
    }

    #[tokio::test]
    async fn prompt_result_cost_is_captured_when_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, priced_usage_script, Vec::new()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        let capture = Arc::new(Mutex::new(None));
        request.usage_capture = Some(capture.clone());
        run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        let captured = capture.lock().unwrap().clone().expect("usage");
        assert!(captured.success);
        assert!(captured.cost_known);
        assert!((captured.cost - 0.0125).abs() < f64::EPSILON);
        assert_eq!(captured.input, 8);
        assert_eq!(captured.output, 2);
        assert!(captured.has_reported_spend());
    }

    #[tokio::test]
    async fn cancelled_prompt_still_writes_usage_capture() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, cancelled_script, Vec::new()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        let capture = Arc::new(Mutex::new(None));
        request.usage_capture = Some(capture.clone());
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("cancelled");
        let _ = peer.await;
        assert!(matches!(
            error,
            GrokSessionError::TurnFailed(status) if status == "cancelled"
        ));
        let captured = capture.lock().unwrap().clone().expect("usage");
        assert!(!captured.success);
        assert_eq!(captured.input, 5);
        assert_eq!(captured.output, 1);
        assert!(!captured.cost_known);
    }

    #[test]
    fn parse_grok_acp_usage_omits_unreported_cost() {
        let tokens = parse_grok_acp_usage(&json!({
            "usage": { "input_tokens": 10, "output_tokens": 2 }
        }))
        .expect("usage");
        assert_eq!(tokens.input, 10);
        assert_eq!(tokens.output, 2);
        assert_eq!(tokens.cost, None);
        let coding = tokens.to_coding_usage();
        assert_eq!(coding.cost_total, None);
        let encoded = serde_json::to_value(&coding).expect("encode");
        assert!(
            encoded.get("cost_total").is_none(),
            "unknown Grok cost must be omitted, not billed as 0.0: {encoded}"
        );
        let turn = tokens.to_turn_usage(true);
        assert!(!turn.cost_known);
        assert!(turn.has_reported_spend());

        let priced = parse_grok_acp_usage(&json!({
            "_meta": { "usage": { "input_tokens": 1, "output_tokens": 1 } },
            "total_cost_usd": 0.0
        }))
        .expect("usage");
        assert_eq!(priced.cost, Some(0.0));
        assert_eq!(priced.to_coding_usage().cost_total, Some(0.0));
        assert!(priced.to_turn_usage(true).cost_known);

        let update = parse_grok_acp_usage(&json!({
            "update": {
                "sessionUpdate": "usage_update",
                "used": 40,
                "size": 128000,
                "cost": { "amount": 0.4, "currency": "USD" }
            }
        }))
        .expect("usage");
        assert_eq!(update.cost, Some(0.4));
        assert_eq!(update.input, 0);

        assert!(parse_grok_acp_usage(&json!({
            "update": {
                "sessionUpdate": "usage_update",
                "cost": { "amount": 1.0, "currency": "EUR" }
            }
        }))
        .is_none());
        assert!(parse_grok_acp_usage(&json!({ "stopReason": "end_turn" })).is_none());
    }

    #[tokio::test]
    async fn consecutive_agent_message_chunks_reach_the_sink_concatenated() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, happy_script, hello_world_notes()));
        let (reader, writer) = tokio::io::split(client);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut request = request_for(dir.path());
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            sink_seen.lock().unwrap().push(event.clone());
        }));
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
        let events = seen.lock().unwrap();
        assert_eq!(
            joined_text_deltas(&events),
            "hello world",
            "sink must see concatenated incremental chunks, not only the last token, got {events:?}"
        );
    }

    fn message_update(text: Option<&str>, thinking: Option<&str>) -> CodingEngineEvent {
        let mut event = synthetic_event(1, CodingEngineEventKind::MessageUpdate, "session/update");
        event.text_delta = text.map(str::to_string);
        event.thinking_delta = thinking.map(str::to_string);
        event
    }

    #[test]
    fn event_queue_concatenates_same_channel_and_does_not_mix_thinking() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut queue = EventQueue::new(
            Some(Arc::new(move |event: &CodingEngineEvent| {
                sink_seen.lock().unwrap().push(event.clone());
            })),
            16,
        );
        queue
            .push(message_update(Some("hello "), None))
            .expect("hello");
        queue
            .push(message_update(Some("world"), None))
            .expect("world");
        queue
            .push(message_update(None, Some("hmm")))
            .expect("thinking");
        queue.finish();
        let events = seen.lock().unwrap();
        assert_eq!(joined_text_deltas(&events), "hello world");
        let thinking: String = events
            .iter()
            .filter_map(|event| event.thinking_delta.as_deref())
            .collect();
        assert_eq!(thinking, "hmm");
        assert!(
            events.iter().any(|event| {
                event.text_delta.as_deref() == Some("hello world") && event.thinking_delta.is_none()
            }),
            "text must not be mixed into thinking, got {events:?}"
        );
    }

    #[tokio::test]
    async fn session_cancel_is_a_jsonrpc_notification() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames(
            server,
            silent_after_session_new,
            frames.clone(),
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_millis(80);
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("silent prompt");
        let _ = peer.await;
        assert!(matches!(error, GrokSessionError::Timeout));
        let captured = frames.lock().unwrap();
        let cancel = captured
            .iter()
            .find(|frame| frame.get("method").and_then(Value::as_str) == Some("session/cancel"))
            .expect("session/cancel must be written");
        assert_eq!(cancel.get("jsonrpc").and_then(Value::as_str), Some("2.0"));
        assert!(
            cancel.get("id").is_none(),
            "ACP session/cancel is a notification and must not carry id: {cancel}"
        );
        assert!(cancel.get("params").is_some(), "{cancel}");
    }

    #[tokio::test]
    async fn silent_peer_cancel_token_unblocks_without_a_further_line() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, silent_after_session_new, vec![]));
        let (reader, writer) = tokio::io::split(client);
        let token = CancellationToken::new();
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_secs(8);
        request.cancel_token = Some(token.clone());
        let cancel_later = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            token.cancel();
        });
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("cancelled");
        let _ = cancel_later.await;
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::TurnFailed(ref status) if status == "cancelled"),
            "silent JSONL must yield to cancel without waiting for the outer turn timeout, got {error:?}"
        );
    }

    #[tokio::test]
    async fn silent_peer_watchdog_fires_before_outer_turn_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, silent_after_session_new, vec![]));
        let (reader, writer) = tokio::io::split(client);
        let mut budgets = ResolvedCodingBudgets::default();
        budgets.model_idle = Duration::from_millis(80);
        budgets.no_progress_enabled = true;
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_secs(8);
        request.budgets = Some(budgets);
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("no_progress");
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::TurnFailed(ref status) if status == "no_progress"),
            "watchdog must trip on a silent peer before the outer turn timeout, got {error:?}"
        );
    }

    #[tokio::test]
    async fn fake_turn_streams_assistant_text_as_message_update() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(
            server,
            happy_script,
            vec![
                json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"sess-secret","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":"streamed"}}}}),
            ],
        ));
        let (reader, writer) = tokio::io::split(client);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut request = request_for(dir.path());
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            sink_seen.lock().unwrap().push(event.clone());
        }));
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.assistant_text.as_deref(), Some("streamed"));
        let events = seen.lock().unwrap();
        assert!(
            events.iter().any(|event| {
                event.kind == CodingEngineEventKind::MessageUpdate
                    && event.text_delta.as_deref() == Some("streamed")
            }),
            "sink must see MessageUpdate with non-empty text_delta, got {events:?}"
        );
    }

    #[tokio::test]
    async fn unexpected_server_request_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
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
                let Some(id) = message_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "session/prompt" {
                    let mut ask = serde_json::to_vec(&json!({
                        "jsonrpc":"2.0",
                        "id": 99,
                        "method":"session/request_permission",
                        "params":{}
                    }))
                    .unwrap();
                    ask.push(b'\n');
                    let _ = writer.write_all(&ask).await;
                }
                if let Some(result) = happy_script(method, id) {
                    let mut payload = serde_json::to_vec(&json!({
                        "jsonrpc":"2.0","id":id,"result":result
                    }))
                    .unwrap();
                    payload.push(b'\n');
                    if writer.write_all(&payload).await.is_err() {
                        break;
                    }
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            GrokSessionLimits::default(),
        )
        .await
        .expect_err("must fail closed");
        let _ = peer.await;
        assert!(matches!(
            error,
            GrokSessionError::UnsupportedServerRequest(method)
                if method == "session/request_permission"
        ));
    }

    #[tokio::test]
    async fn malformed_and_oversized_lines_fail_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (_reader, mut writer) = tokio::io::split(server);
            let _ = writer.write_all(b"{not-json\n").await;
        });
        let (reader, writer) = tokio::io::split(client);
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            GrokSessionLimits::default(),
        )
        .await
        .expect_err("malformed");
        let _ = peer.await;
        assert!(matches!(error, GrokSessionError::Protocol(_)));

        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (_reader, mut writer) = tokio::io::split(server);
            let mut line = vec![b'x'; 64];
            line.push(b'\n');
            let _ = writer.write_all(&line).await;
        });
        let (reader, writer) = tokio::io::split(client);
        let mut limits = GrokSessionLimits::default();
        limits.max_line_bytes = 16;
        let error = run_turn_over_stdio(&request_for(dir.path()), reader, writer, limits)
            .await
            .expect_err("oversized");
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::Protocol(reason) if reason.contains("oversized"))
        );
    }

    #[tokio::test]
    async fn adapter_ignores_pi_turn_options() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames(server, happy_script, frames.clone()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        request.pi = PiTurnOptions {
            session_name: Some("should-not-be-read".into()),
            resume_recent: true,
            resume_session_id: Some("pi-sess".into()),
            ..PiTurnOptions::default()
        };
        request.grok = GrokTurnOptions {
            mode: GrokTurnMode::Build,
            ..GrokTurnOptions::default()
        };
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/new"), 1);
        assert_eq!(methods_named(&captured, "session/load"), 0);
        assert!(
            captured.iter().all(|frame| {
                let encoded = frame.to_string();
                !encoded.contains("pi-sess") && !encoded.contains("should-not-be-read")
            }),
            "Pi session options leaked into Grok ACP frames: {captured:?}"
        );
    }

    #[tokio::test]
    async fn fake_resume_loads_the_acp_session_instead_of_new() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames(server, resume_script, frames.clone()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        request.grok.resume_session_id = Some("sess-secret".into());
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(
            result
                .continuation
                .as_ref()
                .map(|item| item.native_session_id.as_str()),
            Some("sess-secret")
        );
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/load"), 1);
        assert_eq!(methods_named(&captured, "session/new"), 0);
        let load = captured
            .iter()
            .find(|frame| frame.get("method").and_then(Value::as_str) == Some("session/load"))
            .expect("session/load");
        assert_eq!(
            load.pointer("/params/sessionId").and_then(Value::as_str),
            Some("sess-secret")
        );
        assert_eq!(load.pointer("/params/mcpServers"), Some(&json!([])));
    }

    #[tokio::test]
    async fn fake_fresh_turn_opens_session_new_not_load() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames(server, happy_script, frames.clone()));
        let (reader, writer) = tokio::io::split(client);
        let result = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            GrokSessionLimits::default(),
        )
        .await
        .expect("turn");
        let _ = peer.await;
        assert_eq!(
            result
                .continuation
                .as_ref()
                .map(|item| item.native_session_id.as_str()),
            Some("sess-secret")
        );
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/new"), 1);
        assert_eq!(methods_named(&captured, "session/load"), 0);
    }

    #[tokio::test]
    async fn session_load_error_records_continuation_lost_and_starts_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let frames_peer = frames.clone();
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
                frames_peer.lock().unwrap().push(request.clone());
                let Some(id) = message_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let payload = if method == "session/load" {
                    json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": { "code": -32001, "message": "not found" }
                    })
                } else {
                    let Some(result) = resume_script(method, id) else {
                        continue;
                    };
                    json!({ "jsonrpc": "2.0", "id": id, "result": result })
                };
                let mut bytes = serde_json::to_vec(&payload).unwrap();
                bytes.push(b'\n');
                if writer.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        request.grok.resume_session_id = Some("sess-secret".into());
        let result = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect("fresh after continuation_lost");
        let _ = peer.await;
        assert_eq!(
            result
                .continuation
                .as_ref()
                .map(|item| item.native_session_id.as_str()),
            Some("sess-fresh")
        );
        assert_eq!(
            result.continuation_fresh_reason,
            Some(ContinuationFreshReason::ContinuationLost)
        );
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/load"), 1);
        assert_eq!(methods_named(&captured, "session/new"), 1);
    }

    #[tokio::test]
    async fn session_load_protocol_error_does_not_start_fresh() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let frames_peer = frames.clone();
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
                frames_peer.lock().unwrap().push(request.clone());
                let Some(id) = jsonrpc_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "session/load" {
                    if writer.write_all(b"{not-json\n").await.is_err() {
                        break;
                    }
                    continue;
                }
                let Some(numeric) = id.as_u64() else {
                    continue;
                };
                let Some(result) = resume_script(method, numeric) else {
                    continue;
                };
                let mut bytes = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
                .unwrap();
                bytes.push(b'\n');
                if writer.write_all(&bytes).await.is_err() {
                    break;
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        request.grok.resume_session_id = Some("sess-secret".into());
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("malformed session/load must fail the turn");
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::Protocol(reason) if reason.contains("malformed")),
            "got {error:?}"
        );
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/load"), 1);
        assert_eq!(
            methods_named(&captured, "session/new"),
            0,
            "protocol errors must not fall through to session/new"
        );
        assert_eq!(methods_named(&captured, "session/cancel"), 1);
    }

    #[tokio::test]
    async fn string_jsonrpc_id_xai_request_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
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
                let Some(id) = jsonrpc_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                if method == "session/prompt" {
                    let mut probe = serde_json::to_vec(&json!({
                        "jsonrpc": "2.0",
                        "id": "ext-1",
                        "method": "x.ai/foo",
                        "params": {}
                    }))
                    .unwrap();
                    probe.push(b'\n');
                    if writer.write_all(&probe).await.is_err() {
                        return;
                    }
                }
                let Some(numeric) = id.as_u64() else {
                    continue;
                };
                let Some(result) = happy_script(method, numeric) else {
                    continue;
                };
                let mut payload = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result
                }))
                .unwrap();
                payload.push(b'\n');
                if writer.write_all(&payload).await.is_err() {
                    break;
                }
            }
        });
        let (reader, writer) = tokio::io::split(client);
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            GrokSessionLimits::default(),
        )
        .await
        .expect_err("x.ai request");
        let _ = peer.await;
        assert!(
            matches!(
                error,
                GrokSessionError::UnsupportedServerRequest(ref method) if method == "x.ai/foo"
            ),
            "string-id x.ai requests must fail closed, got {error:?}"
        );
    }

    #[test]
    fn jsonrpc_id_accepts_string_and_integer() {
        assert_eq!(jsonrpc_id(&json!({"id": 7})), Some(json!(7)));
        assert_eq!(jsonrpc_id(&json!({"id": "ext-1"})), Some(json!("ext-1")));
        assert!(jsonrpc_id(&json!({"id": ""})).is_none());
        assert!(jsonrpc_id(&json!({"id": null})).is_none());
        assert!(jsonrpc_id(&json!({"method": "session/update"})).is_none());
    }

    #[tokio::test]
    async fn stop_after_prompt_success_does_not_settle() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, happy_script, completed_notes()));
        let (reader, writer) = tokio::io::split(client);
        let token = CancellationToken::new();
        let mut request = request_for(dir.path());
        request.cancel_token = Some(token.clone());
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            if event.kind == CodingEngineEventKind::TurnEnd {
                token.cancel();
            }
        }));
        let error = run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
            .await
            .expect_err("cancelled after prompt");
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::TurnFailed(ref status) if status == "cancelled"),
            "Stop after session/prompt success must not AgentSettle, got {error:?}"
        );
    }

    #[tokio::test]
    async fn cockpit_stop_notifies_session_cancel() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames(
            server,
            silent_after_session_new,
            frames.clone(),
        ));
        let (reader, writer) = tokio::io::split(client);
        let key = format!("grok-stop-{}", Uuid::new_v4());
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_secs(8);
        request.control_keys = vec![key.clone()];
        let run = tokio::spawn(async move {
            run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default()).await
        });
        let registry = coding_control_registry();
        let started = std::time::Instant::now();
        loop {
            let saw_prompt =
                frames.lock().unwrap().iter().any(|frame| {
                    frame.get("method").and_then(Value::as_str) == Some("session/prompt")
                });
            if saw_prompt && registry.is_active(&key).await {
                let delivered = registry
                    .control(&key, CodingControlAction::Stop, None)
                    .await
                    .expect("stop");
                assert!(delivered, "live Grok handle must accept Stop");
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "timed out waiting to Stop a live Grok turn"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let error = run.await.expect("join").expect_err("stopped");
        let _ = peer.await;
        assert!(
            matches!(error, GrokSessionError::TurnFailed(ref status) if status == "cancelled"),
            "cockpit Stop must interrupt the live turn, got {error:?}"
        );
        let captured = frames.lock().unwrap();
        let cancel = captured
            .iter()
            .find(|frame| frame.get("method").and_then(Value::as_str) == Some("session/cancel"))
            .expect("session/cancel must be written");
        assert!(
            cancel.get("id").is_none(),
            "ACP session/cancel is a notification and must not carry id: {cancel}"
        );
    }

    #[tokio::test]
    async fn follow_up_fifo_sends_another_session_prompt() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let frames = Arc::new(Mutex::new(Vec::new()));
        let peer = tokio::spawn(capture_client_frames_delayed(
            server,
            happy_script,
            frames.clone(),
            Duration::from_millis(200),
        ));
        let (reader, writer) = tokio::io::split(client);
        let key = format!("grok-follow-{}", Uuid::new_v4());
        let mut request = request_for(dir.path());
        request.control_keys = vec![key.clone()];
        let run = tokio::spawn(async move {
            run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default()).await
        });
        let registry = coding_control_registry();
        let started = std::time::Instant::now();
        loop {
            if registry.is_active(&key).await {
                let delivered = registry
                    .control(
                        &key,
                        CodingControlAction::FollowUp,
                        Some("also add a comment"),
                    )
                    .await
                    .expect("follow-up");
                assert!(delivered);
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "timed out waiting to queue a Grok follow-up"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let result = run.await.expect("join").expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::GrokAcp);
        let captured = frames.lock().unwrap();
        assert_eq!(methods_named(&captured, "session/new"), 1);
        assert_eq!(methods_named(&captured, "session/prompt"), 2);
        let texts = prompt_texts(&captured);
        assert_eq!(texts.first().map(String::as_str), Some("add a comment"));
        assert_eq!(texts.get(1).map(String::as_str), Some("also add a comment"));
    }

    #[tokio::test]
    async fn attach_proposal_hides_native_session_id() {
        let scope_dir = tempfile::tempdir().unwrap();
        let real = tempfile::tempdir().unwrap();
        let shadow = tempfile::tempdir().unwrap();
        std::fs::write(shadow.path().join("note.txt"), "changed\n").unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(server, happy_script, completed_notes()));
        let (reader, writer) = tokio::io::split(client);
        let mut request = CodingEngineRequest::new(
            "add a comment",
            real.path(),
            shadow.path(),
            scope_dir.path(),
            scope(),
        );
        request.timeout = Duration::from_secs(2);
        let mut result =
            run_turn_over_stdio(&request, reader, writer, GrokSessionLimits::default())
                .await
                .expect("turn");
        let _ = peer.await;
        attach_staged_coding_proposal(&request, &mut result).expect("stage");
        let proposal = result.proposal.expect("proposal");
        assert!(!proposal.files.is_empty());
        assert!(!proposal.id.as_str().contains("sess-secret"));
        assert!(result
            .session_id
            .as_deref()
            .is_some_and(|id| !id.contains("sess-secret")));
    }

    #[test]
    fn spawn_without_the_outer_fence_refuses() {
        assert_eq!(require_outer_fence(), Err(CodingFenceError::Required));
    }

    #[test]
    fn child_env_drops_magician_secrets_and_does_not_invent_grok_home() {
        let filtered = filter_grok_child_env([
            ("PATH", "/usr/bin"),
            ("HOME", "/Users/me"),
            ("GROK_HOME", "/Users/me/.grok"),
            ("XAI_API_KEY", "xai-test"),
            ("MAGICIAN_ADMIN_TOKEN", "nope"),
            ("MAGICIAN_FOO", "secret"),
            ("CODEX_HOME", "/secret"),
        ]);
        assert_eq!(filtered.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert_eq!(
            filtered.get("GROK_HOME").map(String::as_str),
            Some("/Users/me/.grok")
        );
        assert!(filtered.contains_key("XAI_API_KEY"));
        assert!(!filtered.contains_key("MAGICIAN_ADMIN_TOKEN"));
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("CODEX_HOME"));

        let without_home = filter_grok_child_env([("PATH", "/usr/bin"), ("HOME", "/Users/me")]);
        assert!(!without_home.contains_key("GROK_HOME"));
    }

    #[test]
    fn factory_and_adapter_do_not_stage_proposals() {
        let whole = include_str!("grok.rs");
        let source = whole
            .find("\n#[cfg(test)]")
            .into_iter()
            .chain(whole.find("\n#[cfg(any(test"))
            .min()
            .map(|at| &whole[..at])
            .expect("production Grok adapter source");
        assert!(!source.contains("stage_shadow_workspace_patch"));
        assert!(!source.contains("attach_staged_coding_proposal("));
        assert!(
            !source.contains("request.pi"),
            "Grok must not read Pi session/model flags"
        );
        let adapter = construct_coding_adapter(
            CodingEngineKind::GrokAcp,
            CodingAdapterSpec::Grok(GrokCodingOptions::default()),
        )
        .expect("construct");
        assert_eq!(adapter.engine(), CodingEngineKind::GrokAcp);
    }
}

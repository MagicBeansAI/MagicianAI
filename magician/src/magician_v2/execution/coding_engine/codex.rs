//! Stage 3: dormant bounded Codex app-server adapter.
//!
//! The factory constructs this when the journaled engine is Codex. Tests
//! drive the same session against a fake peer.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::{AsyncWriteExt, BufReader};
use tokio::time::timeout;
use uuid::Uuid;

use super::budgets::{record_termination_reason, CodingTerminationReason, ProgressWatchdog};
use super::codex_contract::{
    app_server_args, app_server_args_for, classify_server_request,
    launch_disables_local_code_mode_host, may_send_method, CodexLaunchProfile,
    CodexServerRequestClass, CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED,
};
use super::control::{
    coding_control_registry, CodexControlCommand, CodexTurnHandle, CodingControlHandle,
};
use super::discovery::magician_codex_client_info;
use super::factory::CodexCodingOptions;
use super::jsonl::{append_bounded_utf8, read_bounded_jsonl_value_buffered, BoundedJsonlError};
use super::qualification::filter_codex_child_env;
use super::selection::CodingContinuationRef;
use super::{
    os_sandbox_command, CodingEngineAdapter, CodingEngineEvent, CodingEngineEventKind,
    CodingEngineEventSink, CodingEngineKind, CodingEngineRequest, CodingEngineRunResult,
    CodingEngineTextDeltaSink, CodingLiveSessionReporter, CodingTurnUsage, CodingUsage,
};

const JSONRPC: &str = "2.0";
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_PENDING: usize = 128;
const MAX_EVENT_QUEUE: usize = 1024;
const MAX_ASSISTANT_BYTES: usize = 1024 * 1024;
const STDERR_RING_BYTES: usize = 256 * 1024;
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy)]
pub struct CodexSessionLimits {
    pub request_timeout: Duration,
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
    pub max_pending: usize,
    pub max_event_queue: usize,
}

impl Default for CodexSessionLimits {
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
pub enum CodexSessionError {
    Timeout,
    Protocol(&'static str),
    Io,
    EventBackpressure,
    UnsupportedServerRequest(String),
    TurnNotCompleted,
    TurnFailed(String),
    FenceRequired,
}

impl std::fmt::Display for CodexSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Codex session timed out"),
            Self::Protocol(reason) => write!(f, "Codex protocol error: {reason}"),
            Self::Io => write!(f, "Codex session I/O failed"),
            Self::EventBackpressure => write!(f, "event_backpressure"),
            Self::UnsupportedServerRequest(method) => {
                write!(f, "unsupported Codex server request `{method}`")
            }
            Self::TurnNotCompleted => {
                write!(f, "Codex turn closed without an understood turn/completed")
            }
            Self::TurnFailed(status) => write!(f, "Codex turn ended as {status}"),
            Self::FenceRequired => write!(
                f,
                "Codex dispatch requires the Magician outer coding fence; the Pi unsandboxed fallback is not allowed"
            ),
        }
    }
}

impl std::error::Error for CodexSessionError {}

#[derive(Debug, Clone)]
pub struct CodexAppServerAdapter {
    binary: PathBuf,
}

impl CodexAppServerAdapter {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn from_options(options: CodexCodingOptions) -> Self {
        Self::new(options.binary)
    }

    pub fn launch_args() -> Vec<String> {
        app_server_args()
    }
}

#[async_trait]
impl CodingEngineAdapter for CodexAppServerAdapter {
    fn engine(&self) -> CodingEngineKind {
        CodingEngineKind::CodexAppServer
    }

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult> {
        // The fenced coding path launches under the coding contract only;
        // the plane's profile belongs to the plane's own governance.
        if request.codex.launch_profile != CodexLaunchProfile::Coding {
            return Err(anyhow!(
                "a fenced coding turn launches only the coding profile, not {:?}",
                request.codex.launch_profile
            ));
        }
        require_outer_fence()?;
        self.run_turn_spawned(request).await
    }
}

impl CodexAppServerAdapter {
    /// One spawned app-server child driving exactly one turn, then reaped.
    ///
    /// Public for the **plane's** delegated harness engine (2026-08-31):
    /// the plane's own governance — delegation attenuation, the grant's
    /// tool allowlist, the turn budget — stands in for the coding-loop
    /// outer fence that [`CodingEngineAdapter::run_turn`] enforces for
    /// vibedev runs, so the plane calls this arm directly. Everything else
    /// (validation, spawn discipline, the protocol driver, reap) is shared
    /// with the fenced path.
    pub async fn run_turn_spawned(
        &self,
        request: CodingEngineRequest,
    ) -> Result<CodingEngineRunResult> {
        validate_request(&request)?;
        let mut child = OwnedCodexChild {
            child: spawn_app_server(self, &request)?,
            reaped: false,
        };
        let stdin = child
            .child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Codex child stdin was not piped"))?;
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Codex child stdout was not piped"))?;
        if let Some(stderr) = child.child.stderr.take() {
            tokio::spawn(retain_stderr_ring(stderr));
        }
        let result =
            run_turn_over_stdio(&request, stdout, stdin, CodexSessionLimits::default()).await;
        child.reap().await;
        result.map_err(|error| anyhow!(error))
    }
}

pub fn require_outer_fence() -> Result<(), CodexSessionError> {
    super::require_outer_fence().map_err(|_| CodexSessionError::FenceRequired)
}

fn validate_request(request: &CodingEngineRequest) -> Result<()> {
    if request.prompt.trim().is_empty() {
        return Err(anyhow!("Codex coding request prompt is empty"));
    }
    if !request.shadow_workspace_root.is_dir() {
        return Err(anyhow!(
            "Codex shadow workspace does not exist: {}",
            request.shadow_workspace_root.display()
        ));
    }
    Ok(())
}

fn spawn_app_server(
    adapter: &CodexAppServerAdapter,
    request: &CodingEngineRequest,
) -> Result<tokio::process::Child> {
    let profile = request.codex.launch_profile;
    let args: Vec<OsString> = app_server_args_for(profile)
        .into_iter()
        .map(OsString::from)
        .collect();
    debug_assert!(profile != CodexLaunchProfile::Coding || launch_disables_local_code_mode_host());
    let working_dir = request
        .working_dir
        .as_deref()
        .unwrap_or(request.shadow_workspace_root.as_path());
    let mut command = os_sandbox_command(adapter.binary.as_os_str(), &args);
    command.current_dir(working_dir);
    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);
    command.env_clear();
    let mut inherited: Vec<(String, String)> = std::env::vars().collect();
    inherited.extend(
        request
            .env
            .iter()
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    let filtered = filter_codex_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
    );
    for (key, value) in filtered {
        command.env(key, value);
    }
    command.env("GIT_CEILING_DIRECTORIES", &request.scope_root);
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    command.spawn().with_context(|| {
        format!(
            "spawn Codex app-server `{}` in {}",
            adapter.binary.display(),
            working_dir.display()
        )
    })
}

struct OwnedCodexChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl OwnedCodexChild {
    async fn reap(&mut self) {
        terminate_process_group(&mut self.child).await;
        self.reaped = true;
    }
}

impl Drop for OwnedCodexChild {
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

fn force_kill_process_group(child: &mut tokio::process::Child) {
    #[cfg(unix)]
    if let Some(pid) = child.id() {
        unsafe {
            libc::killpg(pid as i32, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

async fn terminate_process_group(child: &mut tokio::process::Child) {
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
    limits: CodexSessionLimits,
) -> Result<CodingEngineRunResult, CodexSessionError>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    if request.prompt.trim().is_empty() {
        return Err(CodexSessionError::Protocol("empty prompt"));
    }
    let (control, control_rx) = if request.control_keys.is_empty() {
        (None, None)
    } else {
        let (handle, rx) = CodexTurnHandle::bind(
            1,
            request
                .codex
                .resume_thread_id
                .clone()
                .unwrap_or_else(|| "pending".to_string()),
            request
                .run_execution_id
                .clone()
                .unwrap_or_else(|| "codex-exec".to_string()),
        );
        (Some(handle), Some(rx))
    };
    if let Some(handle) = control.as_ref() {
        coding_control_registry()
            .register(
                &request.control_keys,
                CodingControlHandle::Codex(handle.clone()),
            )
            .await;
    }
    let mut session = CodexJsonlSession {
        reader: BufReader::new(reader),
        read_buffer: Vec::new(),
        writer,
        next_id: 1,
        inflight: HashMap::new(),
        limits,
        events: EventQueue::new(request.event_sink.clone(), limits.max_event_queue),
        text_delta_sink: request.text_delta_sink.clone(),
        assistant: String::new(),
        usage: None,
        usage_meter: super::codex_usage::CodexUsageMeter::default(),
        completed: None,
        saw_agent_start: false,
        last_turn_id: None,
        thread_id: None,
        control,
        control_rx,
        cancelled: request.cancel_token.clone(),
        watchdog: request
            .budgets
            .filter(|budgets| budgets.no_progress_enabled)
            .map(|budgets| ProgressWatchdog::new(budgets, std::time::Instant::now())),
    };
    let outcome = session.drive(request).await;
    if !request.control_keys.is_empty() {
        coding_control_registry()
            .unregister(&request.control_keys)
            .await;
    }
    if let Some(handle) = session.control.as_ref() {
        handle.retire();
    }
    session.finalize(request, outcome)
}

struct CodexJsonlSession<R, W> {
    reader: BufReader<R>,
    read_buffer: Vec<u8>,
    writer: W,
    next_id: u64,
    inflight: HashMap<u64, ()>,
    limits: CodexSessionLimits,
    events: EventQueue,
    /// Hears each agent-message delta as it lands, before the queue folds
    /// it: `assistant` and this sink are fed from the one extraction.
    text_delta_sink: Option<CodingEngineTextDeltaSink>,
    assistant: String,
    usage: Option<CodingUsage>,
    usage_meter: super::codex_usage::CodexUsageMeter,
    completed: Option<String>,
    saw_agent_start: bool,
    last_turn_id: Option<String>,
    thread_id: Option<String>,
    control: Option<CodexTurnHandle>,
    control_rx: Option<tokio::sync::mpsc::UnboundedReceiver<CodexControlCommand>>,
    cancelled: Option<tokio_util::sync::CancellationToken>,
    watchdog: Option<ProgressWatchdog>,
}

impl<R, W> CodexJsonlSession<R, W>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    async fn drive(&mut self, request: &CodingEngineRequest) -> Result<String, CodexSessionError> {
        self.request(
            "initialize",
            json!({
                "clientInfo": magician_codex_client_info(),
                "capabilities": {
                    "experimentalApi": CODEX_APP_SERVER_EXPERIMENTAL_API_ENABLED
                }
            }),
        )
        .await?;
        self.notify("initialized", json!({})).await?;

        let cwd = request
            .working_dir
            .as_ref()
            .unwrap_or(&request.shadow_workspace_root)
            .display()
            .to_string();
        let sandbox = request.codex.mode.thread_sandbox();
        let thread = if let Some(thread_id) = request
            .codex
            .resume_thread_id
            .as_deref()
            .filter(|id| !id.is_empty())
        {
            self.request("thread/resume", json!({ "threadId": thread_id }))
                .await?;
            self.request("thread/read", json!({ "threadId": thread_id }))
                .await?;
            thread_id.to_string()
        } else {
            let started = self
                .request(
                    "thread/start",
                    json!({
                        "cwd": cwd,
                        "approvalPolicy": "never",
                        "sandbox": sandbox,
                    }),
                )
                .await?;
            thread_id_from(&started)
                .ok_or(CodexSessionError::Protocol("thread/start missing id"))?
        };

        self.thread_id = Some(thread.clone());
        if let Some(handle) = self.control.as_mut() {
            handle.native_session_id = thread.clone();
        }
        // Earliest of the five, and by a wide margin: the thread is established
        // by the handshake above (`thread/start`, or the id a resume was handed)
        // and is already known before the first `turn/start` is written. So a
        // Codex worker killed at any point after this line — including one
        // killed before its turn ever reached the provider — leaves a thread
        // that can be resumed.
        if let Some(reporter) = CodingLiveSessionReporter::from_request(request) {
            reporter.report_live_session(CodingEngineKind::CodexAppServer, &thread);
        }
        self.start_turn(request, &thread, request.prompt.clone())
            .await?;
        if !self.saw_agent_start {
            self.push_event(synthetic_event(
                self.events.next_sequence(),
                CodingEngineEventKind::AgentStart,
                "thread/ready",
            ))?;
            self.saw_agent_start = true;
        }
        self.drain_until_completed(request.timeout).await?;
        while let Some(follow_up) = self.next_follow_up().await {
            self.completed = None;
            self.start_turn(request, &thread, follow_up).await?;
            self.drain_until_completed(request.timeout).await?;
        }
        if self.completed.as_deref() == Some("completed") {
            self.push_event(synthetic_event(
                self.events.next_sequence(),
                CodingEngineEventKind::AgentSettled,
                "turn/completed",
            ))?;
        }
        Ok(thread)
    }

    async fn start_turn(
        &mut self,
        request: &CodingEngineRequest,
        thread: &str,
        prompt: String,
    ) -> Result<(), CodexSessionError> {
        let sandbox = request.codex.mode.turn_sandbox_policy();
        let mut turn_params = json!({
            "threadId": thread,
            "input": [{ "type": "text", "text": prompt }],
            "approvalPolicy": "never",
            "sandboxPolicy": sandbox,
        });
        if let Some(model) = request
            .codex
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
        {
            turn_params["model"] = json!(model);
        }
        if let Some(effort) = request
            .codex
            .effort
            .as_deref()
            .filter(|effort| !effort.is_empty())
        {
            turn_params["effort"] = json!(effort);
        }
        let started = self.request("turn/start", turn_params).await?;
        self.last_turn_id = turn_id_from(&started);
        // `turn/start` RETURNED, so the provider has the turn and named it —
        // the one point in any of the five adapters where "accepted" is a fact
        // rather than an inference, and the only source of a real
        // `provider_turn_id`. The sink dedupes, which matters here: the
        // follow-up loop calls `start_turn` again on the same invocation.
        if let (Some(reporter), Some(turn_id)) = (
            CodingLiveSessionReporter::from_request(request),
            self.last_turn_id.as_deref(),
        ) {
            reporter.report_turn_accepted(turn_id);
        }
        if let Some(handle) = self.control.as_ref() {
            handle.set_expected_turn(self.last_turn_id.clone()).await;
        }
        Ok(())
    }

    async fn next_follow_up(&self) -> Option<String> {
        match self.control.as_ref() {
            Some(handle) => handle.take_follow_up().await,
            None => None,
        }
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value, CodexSessionError> {
        if !may_send_method(method) {
            return Err(CodexSessionError::Protocol(
                "method is not on the V1 allowlist",
            ));
        }
        if self.inflight.len() >= self.limits.max_pending {
            return Err(CodexSessionError::Protocol("pending request map exhausted"));
        }
        let id = self.next_id;
        self.next_id += 1;
        if self.inflight.insert(id, ()).is_some() {
            return Err(CodexSessionError::Protocol("duplicate client request id"));
        }
        self.write_line(&json!({
            "jsonrpc": JSONRPC,
            "id": id,
            "method": method,
            "params": params,
        }))
        .await?;
        timeout(self.limits.request_timeout, self.read_result(id))
            .await
            .map_err(|_| CodexSessionError::Timeout)?
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<(), CodexSessionError> {
        if !may_send_method(method) {
            return Err(CodexSessionError::Protocol(
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

    async fn write_line(&mut self, value: &Value) -> Result<(), CodexSessionError> {
        let mut line =
            serde_json::to_vec(value).map_err(|_| CodexSessionError::Protocol("encode"))?;
        line.push(b'\n');
        self.writer
            .write_all(&line)
            .await
            .map_err(|_| CodexSessionError::Io)?;
        self.writer.flush().await.map_err(|_| CodexSessionError::Io)
    }

    async fn read_result(&mut self, id: u64) -> Result<Value, CodexSessionError> {
        loop {
            let value = self.read_message().await?;
            if let Some(result) = self.handle_incoming(value).await? {
                if message_id(&result) == Some(id) {
                    if let Some(payload) = result.get("result").cloned() {
                        return Ok(payload);
                    }
                    if result.get("error").is_some() {
                        return Err(CodexSessionError::Protocol("jsonrpc error"));
                    }
                    return Ok(result);
                }
                return Err(CodexSessionError::Protocol(
                    "response id did not match the in-flight request",
                ));
            }
        }
    }

    async fn drain_until_completed(&mut self, bound: Duration) -> Result<(), CodexSessionError> {
        let deadline = tokio::time::Instant::now() + bound;
        while self.completed.is_none() {
            if let Err(error) = self.check_liveness() {
                if matches!(&error, CodexSessionError::TurnFailed(status) if status == "cancelled")
                {
                    let _ = self.interrupt_active_turn().await;
                }
                return Err(error);
            }

            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(CodexSessionError::Timeout);
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
                    let _ = self.interrupt_active_turn().await;
                    return Err(CodexSessionError::TurnFailed("cancelled".into()));
                }
                command = recv_codex_control(&mut self.control_rx) => {
                    match command {
                        Some(CodexControlCommand::Steer {
                            expected_turn_id,
                            input,
                        }) => {
                            self.steer_active_turn(&expected_turn_id, input).await?;
                        }
                        Some(CodexControlCommand::Interrupt { turn_id }) => {
                            let _ = self.interrupt_turn(&turn_id).await;
                            return Err(CodexSessionError::TurnFailed("cancelled".into()));
                        }
                        None => {}
                    }
                }
                incoming = read_bounded_jsonl_value_buffered(
                    &mut self.reader,
                    &mut self.read_buffer,
                    max_line,
                    max_depth,
                ) => {
                    let value = map_bounded_jsonl(incoming)?;
                    let _ = self.handle_incoming(value).await?;
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
        Ok(())
    }

    async fn steer_active_turn(
        &mut self,
        expected_turn_id: &str,
        input: String,
    ) -> Result<(), CodexSessionError> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Err(CodexSessionError::Protocol(
                "Codex steer requires an active thread id",
            ));
        };
        self.request(
            "turn/steer",
            json!({
                "threadId": thread_id,
                "expectedTurnId": expected_turn_id,
                "input": [{ "type": "text", "text": input }],
            }),
        )
        .await?;
        Ok(())
    }

    async fn interrupt_active_turn(&mut self) -> Result<(), CodexSessionError> {
        let Some(turn_id) = self.last_turn_id.clone() else {
            return Ok(());
        };
        self.interrupt_turn(&turn_id).await
    }

    async fn interrupt_turn(&mut self, turn_id: &str) -> Result<(), CodexSessionError> {
        let Some(thread_id) = self.thread_id.clone() else {
            return Ok(());
        };
        let _ = self
            .request(
                "turn/interrupt",
                json!({ "threadId": thread_id, "turnId": turn_id }),
            )
            .await;
        Ok(())
    }

    fn check_liveness(&mut self) -> Result<(), CodexSessionError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(CodexSessionError::TurnFailed("cancelled".into()));
        }
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(CodexSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    async fn read_message(&mut self) -> Result<Value, CodexSessionError> {
        self.check_liveness()?;
        map_bounded_jsonl(
            read_bounded_jsonl_value_buffered(
                &mut self.reader,
                &mut self.read_buffer,
                self.limits.max_line_bytes,
                self.limits.max_json_depth,
            )
            .await,
        )
    }

    async fn handle_incoming(&mut self, value: Value) -> Result<Option<Value>, CodexSessionError> {
        if let Some(method) = value
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_string)
        {
            if let Some(id) = message_id(&value) {
                return self.handle_server_request(id, &method).await.map(|_| None);
            }
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            self.handle_notification(&method, params)?;
            return Ok(None);
        }
        if let Some(id) = message_id(&value) {
            if self.inflight.remove(&id).is_none() {
                return Err(CodexSessionError::Protocol(
                    "unknown or duplicate response id",
                ));
            }
            return Ok(Some(value));
        }
        Err(CodexSessionError::Protocol("unclassified JSONL record"))
    }

    async fn handle_server_request(
        &mut self,
        id: u64,
        method: &str,
    ) -> Result<(), CodexSessionError> {
        match classify_server_request(method) {
            CodexServerRequestClass::AuthRefresh => {
                self.write_line(&json!({
                    "jsonrpc": JSONRPC,
                    "id": id,
                    "result": {}
                }))
                .await
            },
            CodexServerRequestClass::Consequential | CodexServerRequestClass::Unknown => {
                let _ = self
                    .write_line(&json!({
                        "jsonrpc": JSONRPC,
                        "id": id,
                        "error": { "code": -32601, "message": "unsupported" }
                    }))
                    .await;
                Err(CodexSessionError::UnsupportedServerRequest(
                    method.to_string(),
                ))
            },
        }
    }

    fn handle_notification(
        &mut self,
        method: &str,
        params: Value,
    ) -> Result<(), CodexSessionError> {
        if method == "turn/started" {
            self.last_turn_id = turn_id_from(&params);
        }
        if method == "turn/completed" {
            let status = params
                .get("status")
                .or_else(|| params.pointer("/turn/status"))
                .and_then(Value::as_str)
                .unwrap_or("completed")
                .to_string();
            if let Some(usage) = params
                .get("usage")
                .or_else(|| params.pointer("/turn/usage"))
                .and_then(parse_codex_usage)
            {
                self.usage = Some(usage);
            }
            if let Some(turn_id) = turn_id_from(&params) {
                self.last_turn_id = Some(turn_id);
            }
            self.completed = Some(status);
        }
        if method == "thread/tokenUsage/updated" {
            // Ignore other threads/turns, including a resumed conversation's
            // initial snapshot before this request has started its own turn.
            let current_turn =
                self.last_turn_id
                    .as_deref()
                    .is_some_and(|id| params.get("turnId").and_then(Value::as_str) == Some(id))
                    && self.thread_id.as_deref().is_some_and(|id| {
                        params.get("threadId").and_then(Value::as_str) == Some(id)
                    });
            if let Some(usage) = current_turn
                .then(|| self.usage_meter.update(&params))
                .flatten()
            {
                self.usage = Some(usage);
            }
        }
        if let Some(text) = agent_message_delta(method, &params) {
            append_bounded_utf8(&mut self.assistant, &text, MAX_ASSISTANT_BYTES);
            // Uncoalesced, ahead of the queue: a run of these reaches the
            // event sink as one folded event, so a live reader taps here.
            if let Some(sink) = self.text_delta_sink.as_ref() {
                sink(text.as_str());
            }
        }
        let event = project_notification(self.events.next_sequence(), method, &params);
        if event.kind == CodingEngineEventKind::AgentStart {
            self.saw_agent_start = true;
        }
        self.push_event(event)?;
        Ok(())
    }

    fn push_event(&mut self, event: CodingEngineEvent) -> Result<(), CodexSessionError> {
        if let Some(detector) = self.watchdog.as_mut() {
            detector.observe(&event, std::time::Instant::now());
        }
        self.events.push(event)?;
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(CodexSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn finalize(
        mut self,
        request: &CodingEngineRequest,
        outcome: Result<String, CodexSessionError>,
    ) -> Result<CodingEngineRunResult, CodexSessionError> {
        let success = matches!(&outcome, Ok(_) if self.completed.as_deref() == Some("completed"));
        write_turn_usage(request, self.usage.as_ref(), success);
        if let Err(error) = &outcome {
            file_session_termination(request, error);
        }
        let events = self.events.finish();
        let thread_id = outcome?;
        let status = self
            .completed
            .as_deref()
            .ok_or(CodexSessionError::TurnNotCompleted)?;
        if status != "completed" {
            return Err(CodexSessionError::TurnFailed(status.to_string()));
        }
        let invocation_id = format!("codex-{}", Uuid::new_v4());
        let mut continuation = CodingContinuationRef::for_codex_thread(
            thread_id,
            &request.scope_root,
            &request.workspace_root,
            request.run_task_id.as_deref(),
        );
        continuation.last_completed_turn_id = self.last_turn_id.take();
        Ok(CodingEngineRunResult {
            engine: CodingEngineKind::CodexAppServer,
            session_id: Some(invocation_id),
            session_file: None,
            assistant_text: (!self.assistant.is_empty()).then_some(self.assistant),
            event_count: events,
            continuation: Some(continuation),
            continuation_fresh_reason: None,
            proposal: None,
            approval_payload: None,
            session_stats: None,
        })
    }
}

fn map_bounded_jsonl(result: Result<Value, BoundedJsonlError>) -> Result<Value, CodexSessionError> {
    match result {
        Ok(value) => Ok(value),
        Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => Err(CodexSessionError::Io),
        Err(BoundedJsonlError::Oversized) => {
            Err(CodexSessionError::Protocol("oversized JSONL line"))
        },
        Err(BoundedJsonlError::Malformed) => Err(CodexSessionError::Protocol("malformed JSON")),
        Err(BoundedJsonlError::TooDeep) => {
            Err(CodexSessionError::Protocol("JSON nesting too deep"))
        },
    }
}

async fn recv_codex_control(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<CodexControlCommand>>,
) -> Option<CodexControlCommand> {
    match rx.as_mut() {
        Some(rx) => rx.recv().await,
        None => std::future::pending().await,
    }
}

async fn wait_if_cancelled(token: Option<tokio_util::sync::CancellationToken>) {
    match token {
        Some(token) => token.cancelled().await,
        None => std::future::pending().await,
    }
}

fn write_turn_usage(request: &CodingEngineRequest, usage: Option<&CodingUsage>, success: bool) {
    let Some(cell) = request.usage_capture.as_ref() else {
        return;
    };
    let Some(usage) = usage else {
        return;
    };
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(CodingTurnUsage {
            cost: usage.cost_total.unwrap_or(0.0),
            input: usage.input,
            output: usage.output,
            cache_read: usage.cache_read,
            cache_write: usage.cache_write,
            success,
            cost_known: usage.cost_total.is_some(),
        });
    }
}

fn file_session_termination(request: &CodingEngineRequest, error: &CodexSessionError) {
    let Some(key) = request.termination_key.as_deref() else {
        return;
    };
    let reason = match error {
        CodexSessionError::Timeout => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        CodexSessionError::TurnFailed(status) if status == "cancelled" => {
            CodingTerminationReason::OwnerCancelled
        },
        CodexSessionError::TurnFailed(status) if status == "no_progress" => {
            CodingTerminationReason::NoProgress {
                phase: super::budgets::CodingProgressPhase::Model,
                elapsed_secs: 0,
                last_substantive_event: None,
            }
        },
        CodexSessionError::EventBackpressure => CodingTerminationReason::TurnTimeout {
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

    fn push(&mut self, event: CodingEngineEvent) -> Result<(), CodexSessionError> {
        let coalescible = matches!(
            event.kind,
            CodingEngineEventKind::MessageUpdate | CodingEngineEventKind::ToolExecutionUpdate
        );
        if coalescible {
            if let Some(pending) = self.pending_delta.as_mut() {
                // A text delta and a reasoning delta never fold into each
                // other, even on one item.
                let same_channel = pending.text_delta.is_some() == event.text_delta.is_some()
                    && pending.thinking_delta.is_some() == event.thinking_delta.is_some();
                if pending.kind == event.kind
                    && pending.tool_call_id == event.tool_call_id
                    && same_channel
                {
                    if pending.kind == CodingEngineEventKind::MessageUpdate {
                        // Message deltas are true deltas: the folded event
                        // carries the run's whole text, not its last piece.
                        // A tool update is a snapshot, so the latest wins.
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

    fn flush_pending(&mut self) -> Result<(), CodexSessionError> {
        if let Some(event) = self.pending_delta.take() {
            self.emit(event)?;
        }
        Ok(())
    }

    fn emit(&mut self, event: CodingEngineEvent) -> Result<(), CodexSessionError> {
        if self.emitted >= self.max as u64 {
            return Err(CodexSessionError::EventBackpressure);
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
    let (kind, tool_name, tool_call_id, text_delta, thinking_delta) = match method {
        "thread/started" => (CodingEngineEventKind::AgentStart, None, None, None, None),
        "turn/started" => (CodingEngineEventKind::TurnStart, None, None, None, None),
        "turn/completed" => (CodingEngineEventKind::TurnEnd, None, None, None, None),
        "item/agentMessage/delta" => (
            CodingEngineEventKind::MessageUpdate,
            None,
            item_id(params),
            text_from(params),
            None,
        ),
        "item/completed" if item_is(params, "agentMessage") => (
            CodingEngineEventKind::MessageEnd,
            None,
            item_id(params),
            None,
            None,
        ),
        "item/completed" if item_is(params, "fileChange") => (
            CodingEngineEventKind::ToolExecutionEnd,
            Some("file_change".into()),
            item_id(params),
            None,
            None,
        ),
        "item/completed" if item_is(params, "commandExecution") => (
            CodingEngineEventKind::ToolExecutionEnd,
            Some("command".into()),
            item_id(params),
            None,
            None,
        ),
        "item/started" if item_is(params, "fileChange") => (
            CodingEngineEventKind::ToolExecutionStart,
            Some("file_change".into()),
            item_id(params),
            None,
            None,
        ),
        "item/started" if item_is(params, "commandExecution") => (
            CodingEngineEventKind::ToolExecutionStart,
            Some("command".into()),
            item_id(params),
            None,
            None,
        ),
        "item/commandExecution/outputDelta" | "item/fileChange/outputDelta" => (
            CodingEngineEventKind::ToolExecutionUpdate,
            Some(if method.contains("fileChange") {
                "file_change".into()
            } else {
                "command".into()
            }),
            item_id(params),
            None,
            None,
        ),
        "item/fileChange/patchUpdated" => (
            CodingEngineEventKind::ToolExecutionUpdate,
            Some("file_change".into()),
            item_id(params),
            None,
            None,
        ),
        "thread/compacted" => (CodingEngineEventKind::CompactionEnd, None, None, None, None),
        "item/reasoning/textDelta" | "item/reasoning/summaryTextDelta" => (
            CodingEngineEventKind::MessageUpdate,
            None,
            item_id(params),
            None,
            text_from(params),
        ),
        "error" | "warning" | "guardianWarning" => {
            (CodingEngineEventKind::Unknown, None, None, None, None)
        },
        _ => (CodingEngineEventKind::Unknown, None, None, None, None),
    };
    let digest = {
        let mut hasher = blake3::Hasher::new();
        hasher.update(method.as_bytes());
        hasher.update(b":");
        if let Some(item) = item_id(params) {
            hasher.update(item.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    };
    let usage = params.get("usage").and_then(parse_codex_usage);
    let error_message = params
        .get("message")
        .or_else(|| params.get("error"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty() && matches!(method, "error" | "warning"))
        .map(str::to_string);
    CodingEngineEvent {
        sequence,
        kind,
        raw_type: Some(method.to_string()),
        text_delta,
        tool_name,
        tool_call_id,
        thinking_delta,
        assistant_event_type: None,
        usage: usage.clone(),
        cost_total: usage.as_ref().and_then(|usage| usage.cost_total),
        stop_reason: None,
        tool_result_is_error: None,
        will_retry: None,
        error_message,
        raw: json!({
            "type": kind.as_str(),
            "method": method,
            "digest": digest,
        }),
    }
}

/// The assistant text a notification carries: only `item/agentMessage/delta`
/// speaks for the assistant (an agent message's `item/completed` repeats the
/// whole text, which must not be appended or streamed twice).
fn agent_message_delta(method: &str, params: &Value) -> Option<String> {
    if method != "item/agentMessage/delta" {
        return None;
    }
    text_from(params)
}

fn text_from(params: &Value) -> Option<String> {
    params
        .get("delta")
        .or_else(|| params.pointer("/item/text"))
        .or_else(|| params.get("text"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn item_id(params: &Value) -> Option<String> {
    params
        .pointer("/item/id")
        .or_else(|| params.get("itemId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn item_is(params: &Value, expected: &str) -> bool {
    params
        .pointer("/item/type")
        .or_else(|| params.get("type"))
        .and_then(Value::as_str)
        == Some(expected)
}

fn parse_codex_usage(value: &Value) -> Option<CodingUsage> {
    super::codex_usage::parse_usage(value)
}

fn message_id(value: &Value) -> Option<u64> {
    value
        .get("id")
        .and_then(|id| id.as_u64().or_else(|| id.as_str()?.parse().ok()))
}

fn thread_id_from(result: &Value) -> Option<String> {
    result
        .pointer("/thread/id")
        .or_else(|| result.pointer("/threadId"))
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn turn_id_from(result: &Value) -> Option<String> {
    result
        .pointer("/turn/id")
        .or_else(|| result.get("turnId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::coding_engine::{
        attach_staged_coding_proposal, construct_coding_adapter, CodexCodingOptions, CodexTurnMode,
        CodingAdapterSpec, CodingControlAction,
    };
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use std::sync::{Arc, Mutex};
    use tokio::io::{duplex, AsyncBufReadExt};

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
            if method == "turn/start" {
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
            "initialize" => Some(json!({ "serverInfo": { "version": "0.147.0" } })),
            "thread/start" => Some(json!({ "thread": { "id": "thread-secret" } })),
            "thread/resume" | "thread/read" => Some(json!({ "thread": { "id": "thread-secret" } })),
            "turn/start" => Some(json!({ "turn": { "id": "turn-1" } })),
            _ => None,
        }
    }

    fn completed_notes() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","method":"thread/started","params":{"threadId":"thread-secret"}}),
            json!({"jsonrpc":"2.0","method":"turn/started","params":{"turnId":"turn-1","threadId":"thread-secret"}}),
            json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"item":{"id":"m1"},"delta":"hello "}}),
            json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"item":{"id":"m1"},"delta":"world"}}),
            json!({"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"m1","type":"agentMessage","text":""}}}),
            json!({"jsonrpc":"2.0","method":"turn/completed","params":{"status":"completed","usage":{"input":3,"output":5}}}),
        ]
    }

    #[tokio::test]
    async fn fake_turn_maps_events_and_hides_the_native_thread() {
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
        let result = run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::CodexAppServer);
        assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
        assert!(result.event_count >= 4);
        assert!(result
            .session_id
            .as_deref()
            .is_some_and(|id| id.starts_with("codex-") && !id.contains("thread-secret")));
        let continuation = result.continuation.expect("continuation");
        assert_eq!(continuation.engine, CodingEngineKind::CodexAppServer);
        assert_eq!(continuation.native_session_id, "thread-secret");
        let events = seen.lock().unwrap();
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::TurnEnd));
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::AgentSettled));
        for event in events.iter() {
            let raw = event.raw.to_string();
            assert!(
                !raw.contains("thread-secret"),
                "native thread id leaked into event raw: {raw}"
            );
        }
    }

    /// The text sink hears each agent-message delta as its own call, in
    /// order, and nothing else: the event sink folds a run of deltas into
    /// one event carrying the run's whole text (the happy script's two
    /// deltas reach it as one — whole, not the last piece), so a live
    /// reader on that channel would see the reply late. The completed
    /// item's text is not repeated, and both channels agree with the
    /// settled text.
    #[tokio::test]
    async fn the_text_delta_sink_hears_every_agent_message_delta_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(32 * 1024);
        let mut notes = completed_notes();
        // A completed item that repeats the whole text, as the CLI may send.
        notes[4] = json!({"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"m1","type":"agentMessage","text":"hello world"}}});
        let peer = tokio::spawn(scripted_peer(server, happy_script, notes));
        let (reader, writer) = tokio::io::split(client);
        let deltas = Arc::new(Mutex::new(Vec::new()));
        let sink_deltas = deltas.clone();
        let folded = Arc::new(Mutex::new(Vec::new()));
        let sink_folded = folded.clone();
        let mut request = request_for(dir.path());
        request.text_delta_sink = Some(Arc::new(move |delta: &str| {
            sink_deltas.lock().unwrap().push(delta.to_string());
        }));
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            if let Some(delta) = event.text_delta.clone() {
                sink_folded.lock().unwrap().push(delta);
            }
        }));
        let result = run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default())
            .await
            .expect("turn");
        let _ = peer.await;

        assert_eq!(
            deltas.lock().unwrap().clone(),
            vec!["hello ".to_string(), "world".to_string()]
        );
        assert_eq!(result.assistant_text.as_deref(), Some("hello world"));
        assert_eq!(
            folded.lock().unwrap().clone(),
            vec!["hello world".to_string()],
            "the event sink folds the run into one whole event: the text sink is the live channel"
        );
    }

    #[tokio::test]
    async fn registry_steer_and_stop_commands_reach_fake_app_server() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(32 * 1024);
        let steer_params = Arc::new(Mutex::new(None));
        let interrupt_params = Arc::new(Mutex::new(None));
        let peer_steer_params = steer_params.clone();
        let peer_interrupt_params = interrupt_params.clone();
        let (steer_seen_tx, steer_seen_rx) = tokio::sync::oneshot::channel();
        let peer = tokio::spawn(async move {
            let (reader, mut writer) = tokio::io::split(server);
            let mut reader = BufReader::new(reader);
            let mut line = String::new();
            let mut steer_seen_tx = Some(steer_seen_tx);
            loop {
                line.clear();
                if reader.read_line(&mut line).await.unwrap_or(0) == 0 {
                    break;
                }
                let request: Value = serde_json::from_str(line.trim()).expect("client JSON");
                let Some(id) = message_id(&request) else {
                    continue;
                };
                let method = request.get("method").and_then(Value::as_str).unwrap_or("");
                let result = match method {
                    "initialize" => json!({ "serverInfo": { "version": "0.147.0" } }),
                    "thread/start" => json!({ "thread": { "id": "thread-control" } }),
                    "turn/start" => json!({ "turn": { "id": "turn-control" } }),
                    "turn/steer" => {
                        *peer_steer_params.lock().unwrap() = request.get("params").cloned();
                        json!({ "turnId": "turn-control" })
                    },
                    "turn/interrupt" => {
                        *peer_interrupt_params.lock().unwrap() = request.get("params").cloned();
                        json!({})
                    },
                    other => panic!("unexpected client request {other}"),
                };
                if method == "turn/steer" {
                    // Complete the notification whose prefix was consumed by
                    // the drain loop before the steer command won `select!`.
                    // Losing that prefix would corrupt this record and prove
                    // the multiplexed reader is not cancellation-safe.
                    writer
                        .write_all(b"control\"}}\n")
                        .await
                        .expect("fragmented notification suffix");
                }
                let mut payload = serde_json::to_vec(&json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": result,
                }))
                .unwrap();
                payload.push(b'\n');
                writer.write_all(&payload).await.expect("peer response");
                if method == "turn/start" {
                    writer
                        .write_all(
                            b"{\"jsonrpc\":\"2.0\",\"method\":\"turn/started\",\"params\":{\"threadId\":\"thread-control\",\"turnId\":\"turn-",
                        )
                        .await
                        .expect("fragmented notification prefix");
                }
                if method == "turn/steer" {
                    if let Some(tx) = steer_seen_tx.take() {
                        let _ = tx.send(());
                    }
                }
                if method == "turn/interrupt" {
                    break;
                }
            }
        });

        let key = format!("codex-control-{}", Uuid::new_v4());
        let (active_tx, active_rx) = tokio::sync::oneshot::channel();
        let active_tx = Arc::new(Mutex::new(Some(active_tx)));
        let event_active_tx = active_tx.clone();
        let mut request = request_for(dir.path());
        request.control_keys = vec![key.clone()];
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            if event.kind == CodingEngineEventKind::AgentStart {
                if let Some(tx) = event_active_tx.lock().unwrap().take() {
                    let _ = tx.send(());
                }
            }
        }));
        let (reader, writer) = tokio::io::split(client);
        let run = tokio::spawn(async move {
            run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default()).await
        });

        active_rx.await.expect("turn became active");
        assert!(coding_control_registry()
            .control(&key, CodingControlAction::Steer, Some("change direction"))
            .await
            .expect("steer accepted"));
        steer_seen_rx.await.expect("app-server received steer");
        assert!(coding_control_registry()
            .control(&key, CodingControlAction::Stop, None)
            .await
            .expect("stop accepted"));

        let error = run
            .await
            .expect("turn task")
            .expect_err("stop cancels turn");
        assert!(matches!(
            error,
            CodexSessionError::TurnFailed(status) if status == "cancelled"
        ));
        peer.await.expect("peer task");

        let steer = steer_params.lock().unwrap().clone().expect("steer params");
        assert_eq!(steer["threadId"], "thread-control");
        assert_eq!(steer["expectedTurnId"], "turn-control");
        assert_eq!(steer["input"][0]["type"], "text");
        assert_eq!(steer["input"][0]["text"], "change direction");

        let interrupt = interrupt_params
            .lock()
            .unwrap()
            .clone()
            .expect("interrupt params");
        assert_eq!(interrupt["threadId"], "thread-control");
        assert_eq!(interrupt["turnId"], "turn-control");
        assert!(!coding_control_registry().is_active(&key).await);
    }

    #[tokio::test]
    async fn discuss_and_build_send_distinct_sandbox_modes() {
        // Each mode carries TWO wire spellings: `thread/start` takes the kebab
        // string, `turn/start` takes a tagged object holding the camel name.
        // Both are asserted because only the first used to be, and the second
        // drifted unnoticed behind that gap.
        for (mode, thread_expected, turn_expected) in [
            (CodexTurnMode::Discuss, "read-only", "readOnly"),
            (CodexTurnMode::Build, "workspace-write", "workspaceWrite"),
            (
                CodexTurnMode::Autopilot,
                "workspace-write",
                "workspaceWrite",
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let (client, server) = duplex(16 * 1024);
            let saw = Arc::new(Mutex::new(None));
            let saw_clone = saw.clone();
            let saw_turn = Arc::new(Mutex::new(None));
            let saw_turn_clone = saw_turn.clone();
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
                    if method == "thread/start" {
                        *saw_clone.lock().unwrap() = request.get("params").cloned();
                    }
                    if method == "turn/start" {
                        *saw_turn_clone.lock().unwrap() = request.get("params").cloned();
                        let mut done = serde_json::to_vec(&json!({
                            "jsonrpc":"2.0",
                            "method":"turn/completed",
                            "params":{"status":"completed"}
                        }))
                        .unwrap();
                        done.push(b'\n');
                        let _ = writer.write_all(&done).await;
                    }
                    let result = happy_script(method, id).unwrap_or(json!({}));
                    let mut payload = serde_json::to_vec(&json!({
                        "jsonrpc":"2.0","id":id,"result":result
                    }))
                    .unwrap();
                    payload.push(b'\n');
                    if writer.write_all(&payload).await.is_err() {
                        break;
                    }
                }
            });
            let (reader, writer) = tokio::io::split(client);
            let mut request = request_for(dir.path());
            request.codex.mode = mode;
            run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default())
                .await
                .expect("turn");
            let _ = peer.await;
            let params = saw.lock().unwrap().clone().expect("thread/start");
            assert_eq!(
                params.get("sandbox").and_then(Value::as_str),
                Some(thread_expected),
                "thread/start takes the bare kebab spelling"
            );
            assert_eq!(
                params.get("approvalPolicy").and_then(Value::as_str),
                Some("never")
            );

            let turn = saw_turn.lock().unwrap().clone().expect("turn/start");
            let policy = turn.get("sandboxPolicy").expect("sandboxPolicy is sent");
            assert!(
                policy.is_object(),
                "turn/start sandboxPolicy is a tagged object, not a bare string"
            );
            assert_eq!(
                policy.get("type").and_then(Value::as_str),
                Some(turn_expected),
                "the tag carries the camel spelling"
            );
        }
    }

    #[tokio::test]
    async fn unexpected_approval_fails_closed() {
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
                if method == "turn/start" {
                    let mut ask = serde_json::to_vec(&json!({
                        "jsonrpc":"2.0",
                        "id": 99,
                        "method":"item/permissions/requestApproval",
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
            CodexSessionLimits::default(),
        )
        .await
        .expect_err("must fail closed");
        let _ = peer.await;
        assert!(matches!(
            error,
            CodexSessionError::UnsupportedServerRequest(method)
                if method == "item/permissions/requestApproval"
        ));
    }

    #[tokio::test]
    async fn missing_turn_completed_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(
            server,
            happy_script,
            vec![json!({"jsonrpc":"2.0","method":"turn/started","params":{}})],
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut limits = CodexSessionLimits::default();
        limits.request_timeout = Duration::from_millis(80);
        let error = run_turn_over_stdio(&request_for(dir.path()), reader, writer, limits)
            .await
            .expect_err("no completed");
        let _ = peer.await;
        assert!(matches!(
            error,
            CodexSessionError::Timeout
                | CodexSessionError::Io
                | CodexSessionError::TurnNotCompleted
        ));
    }

    #[tokio::test]
    async fn completed_item_text_does_not_double_streamed_deltas() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let notes = vec![
            json!({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"item":{"id":"m1"},"delta":"hello"}}),
            json!({"jsonrpc":"2.0","method":"item/completed","params":{"item":{"id":"m1","type":"agentMessage","text":"hello"}}}),
            json!({"jsonrpc":"2.0","method":"turn/completed","params":{"status":"completed"}}),
        ];
        let peer = tokio::spawn(scripted_peer(server, happy_script, notes));
        let (reader, writer) = tokio::io::split(client);
        let result = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            CodexSessionLimits::default(),
        )
        .await
        .expect("turn");
        let _ = peer.await;
        assert_eq!(result.assistant_text.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn failed_turn_records_usage_without_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(
            server,
            happy_script,
            vec![
                json!({"jsonrpc":"2.0","method":"turn/completed","params":{"status":"failed","usage":{"input":1,"output":2}}}),
            ],
        ));
        let (reader, writer) = tokio::io::split(client);
        let mut request = request_for(dir.path());
        let usage = Arc::new(Mutex::new(None));
        request.usage_capture = Some(usage.clone());
        let error = run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default())
            .await
            .expect_err("failed");
        let _ = peer.await;
        assert!(matches!(error, CodexSessionError::TurnFailed(status) if status == "failed"));
        let captured = usage.lock().unwrap().clone().expect("usage");
        assert!(!captured.success);
        assert_eq!(captured.input, 1);
        assert_eq!(captured.output, 2);
    }

    #[tokio::test]
    async fn failed_turn_never_stages_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(scripted_peer(
            server,
            happy_script,
            vec![json!({"jsonrpc":"2.0","method":"turn/completed","params":{"status":"failed"}})],
        ));
        let (reader, writer) = tokio::io::split(client);
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            CodexSessionLimits::default(),
        )
        .await
        .expect_err("failed");
        let _ = peer.await;
        assert!(matches!(error, CodexSessionError::TurnFailed(status) if status == "failed"));
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
            CodexSessionLimits::default(),
        )
        .await
        .expect_err("malformed");
        let _ = peer.await;
        assert!(matches!(error, CodexSessionError::Protocol(_)));

        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (_reader, mut writer) = tokio::io::split(server);
            let mut line = vec![b'x'; 64];
            line.push(b'\n');
            let _ = writer.write_all(&line).await;
        });
        let (reader, writer) = tokio::io::split(client);
        let mut limits = CodexSessionLimits::default();
        limits.max_line_bytes = 16;
        let error = run_turn_over_stdio(&request_for(dir.path()), reader, writer, limits)
            .await
            .expect_err("oversized");
        let _ = peer.await;
        assert!(
            matches!(error, CodexSessionError::Protocol(reason) if reason.contains("oversized"))
        );
    }

    #[tokio::test]
    async fn eof_before_initialize_is_io() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(1024);
        drop(server);
        let (reader, writer) = tokio::io::split(client);
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            reader,
            writer,
            CodexSessionLimits::default(),
        )
        .await
        .expect_err("eof");
        assert!(matches!(
            error,
            CodexSessionError::Io | CodexSessionError::Timeout
        ));
    }

    #[tokio::test]
    async fn attach_proposal_matches_pi_shape_without_native_id() {
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
            run_turn_over_stdio(&request, reader, writer, CodexSessionLimits::default())
                .await
                .expect("turn");
        let _ = peer.await;
        attach_staged_coding_proposal(&request, &mut result).expect("stage");
        let proposal = result.proposal.expect("proposal");
        assert!(!proposal.files.is_empty());
        assert!(!proposal.id.as_str().contains("thread-secret"));
        assert!(result
            .session_id
            .as_deref()
            .is_some_and(|id| !id.contains("thread-secret")));
    }

    #[test]
    fn launch_args_never_start_a_code_mode_host() {
        let args = CodexAppServerAdapter::launch_args();
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--disable", "code_mode_host"]));
        assert!(!args.iter().any(|arg| arg == "--code-mode-host"));
        assert!(launch_disables_local_code_mode_host());
    }

    #[test]
    fn spawn_without_the_outer_fence_refuses() {
        assert_eq!(require_outer_fence(), Err(CodexSessionError::FenceRequired));
    }

    #[tokio::test]
    async fn the_fenced_turn_refuses_the_plane_launch_profile() {
        let shadow = tempfile::tempdir().unwrap();
        let mut request = request_for(shadow.path());
        request.codex.launch_profile = CodexLaunchProfile::PlaneHarness;
        let error = CodexAppServerAdapter::new("codex")
            .run_turn(request)
            .await
            .expect_err("the plane profile never rides the fenced coding path");
        assert!(error.to_string().contains("PlaneHarness"), "{error}");
    }

    #[test]
    fn factory_and_adapter_do_not_stage_proposals() {
        // Split on the attribute's opening, not on `#[cfg(test)]` exactly: the
        // crate split rewrote this module to
        // `#[cfg(any(test, feature = "test-fixtures"))]`, after which the old
        // literal matched nothing, the whole file counted as production, and
        // this guard tripped over the staging call in its own test module.
        let whole = include_str!("codex.rs");
        let source = whole
            .find("\n#[cfg(test)]")
            .into_iter()
            .chain(whole.find("\n#[cfg(any(test"))
            .min()
            .map(|at| &whole[..at])
            .expect("production Codex adapter source");
        assert!(!source.contains("stage_shadow_workspace_patch"));
        assert!(!source.contains("attach_staged_coding_proposal("));
        let adapter = construct_coding_adapter(
            CodingEngineKind::CodexAppServer,
            CodingAdapterSpec::Codex(CodexCodingOptions::default()),
        )
        .expect("construct");
        assert_eq!(adapter.engine(), CodingEngineKind::CodexAppServer);
    }

    #[test]
    fn adapter_does_not_read_pi_turn_options() {
        let whole = include_str!("codex.rs");
        // See `factory_and_adapter_do_not_stage_proposals` for why this matches
        // the attribute's opening rather than `#[cfg(test)]` exactly.
        let source = whole
            .find("\n#[cfg(test)]")
            .into_iter()
            .chain(whole.find("\n#[cfg(any(test"))
            .min()
            .map(|at| &whole[..at])
            .unwrap();
        assert!(
            !source.contains("request.pi"),
            "Codex must not read Pi session/model flags"
        );
    }

    #[test]
    fn stderr_ring_and_queue_bounds_match_the_plan() {
        assert_eq!(STDERR_RING_BYTES, 256 * 1024);
        assert_eq!(MAX_EVENT_QUEUE, 1024);
        assert_eq!(MAX_LINE_BYTES, 8 * 1024 * 1024);
    }
}

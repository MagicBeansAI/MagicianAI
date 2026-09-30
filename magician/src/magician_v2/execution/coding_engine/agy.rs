//! Live bounded Antigravity headless adapter (`agy -p --output-format stream-json`).
//!
//! stdin is `/dev/null` for print turns (open stdin historically hangs).
//! Native resume is `--conversation <uuid>`. Never `--continue`.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use serde_json::{json, Value};
use tokio::io::BufReader;
use tokio::time::timeout;
use uuid::Uuid;

use super::agy_contract::{
    agy_child_env_allowlist, agy_conversation_id_from, agy_event_name, agy_init_isolation_leak,
    agy_result_is_success, agy_result_is_terminal, agy_result_status, agy_step_isolation_leak,
    launch_args,
};
use super::budgets::{record_termination_reason, CodingTerminationReason, ProgressWatchdog};
use super::codex_lifecycle::ContinuationFreshReason;
use super::control::{
    coding_control_registry, AgyControlCommand, AgyTurnHandle, CodingControlHandle,
};
use super::factory::{AgyCodingOptions, AgyTurnMode};
use super::jsonl::{append_bounded_utf8, read_bounded_jsonl_value, BoundedJsonlError};
use super::selection::CodingContinuationRef;
use super::{
    os_sandbox_command, require_outer_fence, CodingEngineAdapter, CodingEngineEvent,
    CodingEngineEventKind, CodingEngineEventSink, CodingEngineKind, CodingEngineRequest,
    CodingEngineRunResult, CodingFenceError, CodingLiveSessionReporter, CodingTurnUsage,
    CodingUsage,
};

const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 32;
const MAX_EVENT_QUEUE: usize = 1024;
const MAX_ASSISTANT_BYTES: usize = 1024 * 1024;
const STDERR_RING_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone, Copy)]
pub struct AgySessionLimits {
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
    pub max_event_queue: usize,
}

impl Default for AgySessionLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            max_json_depth: MAX_JSON_DEPTH,
            max_event_queue: MAX_EVENT_QUEUE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgySessionError {
    Timeout,
    Protocol(&'static str),
    Io,
    EventBackpressure,
    MissingResult,
    TurnFailed(String),
    FenceRequired,
    ResumeLost,
}

impl std::fmt::Display for AgySessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Agy session timed out"),
            Self::Protocol(reason) => write!(f, "Agy protocol error: {reason}"),
            Self::Io => write!(f, "Agy session I/O failed"),
            Self::EventBackpressure => write!(f, "event_backpressure"),
            Self::MissingResult => write!(
                f,
                "Agy turn closed without a terminal result success object"
            ),
            Self::TurnFailed(status) => write!(f, "Agy turn ended as {status}"),
            Self::FenceRequired => write!(
                f,
                "Agy dispatch requires the Magician outer coding fence; the Pi unsandboxed fallback is not allowed"
            ),
            Self::ResumeLost => write!(
                f,
                "Agy --conversation did not emit init; continuation is lost"
            ),
        }
    }
}

impl std::error::Error for AgySessionError {}

impl From<CodingFenceError> for AgySessionError {
    fn from(_: CodingFenceError) -> Self {
        Self::FenceRequired
    }
}

#[derive(Debug, Clone)]
pub struct AgyCliAdapter {
    binary: PathBuf,
    use_api_key: bool,
}

impl AgyCliAdapter {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            use_api_key: false,
        }
    }

    pub fn from_options(options: AgyCodingOptions) -> Self {
        Self {
            binary: options.binary,
            use_api_key: options.use_api_key,
        }
    }

    pub fn launch_args(
        mode: AgyTurnMode,
        prompt: &str,
        resume_session_id: Option<&str>,
    ) -> Vec<String> {
        launch_args(mode, prompt, resume_session_id)
    }

    async fn run_once(&self, request: &CodingEngineRequest) -> Result<CodingEngineRunResult> {
        let mut child = OwnedAgyChild {
            child: spawn_agy(self, request)?,
            reaped: false,
        };
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Agy child stdout was not piped"))?;
        if let Some(stderr) = child.child.stderr.take() {
            tokio::spawn(retain_stderr_ring(stderr));
        }
        let result = run_turn_over_stdio(request, stdout, AgySessionLimits::default()).await;
        child.reap().await;
        result.map_err(anyhow::Error::from)
    }
}

#[async_trait]
impl CodingEngineAdapter for AgyCliAdapter {
    fn engine(&self) -> CodingEngineKind {
        CodingEngineKind::AgyCli
    }

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult> {
        require_outer_fence().map_err(AgySessionError::from)?;
        validate_request(&request)?;
        match self.run_once(&request).await {
            Ok(result) => Ok(result),
            Err(error)
                if request
                    .agy
                    .resume_session_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
                    && error.downcast_ref::<AgySessionError>()
                        == Some(&AgySessionError::ResumeLost)
                    && !request
                        .cancel_token
                        .as_ref()
                        .is_some_and(|token| token.is_cancelled()) =>
            {
                let mut fresh = request.clone();
                fresh.agy.resume_session_id = None;
                let mut result = self.run_once(&fresh).await?;
                result.continuation_fresh_reason = Some(ContinuationFreshReason::ContinuationLost);
                Ok(result)
            },
            Err(error) => Err(error),
        }
    }
}

fn validate_request(request: &CodingEngineRequest) -> Result<()> {
    if request.prompt.trim().is_empty() {
        return Err(anyhow!("Agy coding request prompt is empty"));
    }
    if !request.shadow_workspace_root.is_dir() {
        return Err(anyhow!(
            "Agy shadow workspace does not exist: {}",
            request.shadow_workspace_root.display()
        ));
    }
    Ok(())
}

const AGY_API_KEY_ENV: &[&str] = &[
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
];

pub fn filter_agy_child_env<'a, I>(entries: I, use_api_key: bool) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let allowed: BTreeMap<&str, ()> = agy_child_env_allowlist()
        .iter()
        .map(|key| (*key, ()))
        .collect();
    let mut filtered = BTreeMap::new();
    for (key, value) in entries {
        if key.starts_with("MAGICIAN_") {
            continue;
        }
        if AGY_API_KEY_ENV.iter().any(|item| *item == key) {
            if use_api_key {
                filtered.insert(key.to_string(), value.to_string());
            }
            continue;
        }
        if allowed.contains_key(key) {
            filtered.insert(key.to_string(), value.to_string());
        }
    }
    filtered
}

fn spawn_agy(
    adapter: &AgyCliAdapter,
    request: &CodingEngineRequest,
) -> Result<tokio::process::Child> {
    let args: Vec<OsString> = AgyCliAdapter::launch_args(
        request.agy.mode,
        &request.prompt,
        request.agy.resume_session_id.as_deref(),
    )
    .into_iter()
    .map(OsString::from)
    .collect();
    debug_assert!(!args.iter().any(|arg| arg == "--continue" || arg == "-c"));
    let working_dir = request.shadow_workspace_root.as_path();
    let mut command = os_sandbox_command(adapter.binary.as_os_str(), &args);
    command.current_dir(working_dir);
    command.stdin(std::process::Stdio::null());
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
    let filtered = filter_agy_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        adapter.use_api_key,
    );
    for (key, value) in filtered {
        command.env(key, value);
    }
    command.env("GIT_CEILING_DIRECTORIES", &request.scope_root);
    apply_agy_child_process_group(&mut command);
    command.spawn().with_context(|| {
        format!(
            "spawn Antigravity `{}` in {}",
            adapter.binary.display(),
            working_dir.display()
        )
    })
}

struct OwnedAgyChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl OwnedAgyChild {
    async fn reap(&mut self) {
        terminate_process_group(&mut self.child).await;
        self.reaped = true;
    }
}

impl Drop for OwnedAgyChild {
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

pub(crate) fn apply_agy_child_process_group(command: &mut tokio::process::Command) {
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

pub async fn run_turn_over_stdio<R>(
    request: &CodingEngineRequest,
    reader: R,
    limits: AgySessionLimits,
) -> Result<CodingEngineRunResult, AgySessionError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    if request.prompt.trim().is_empty() {
        return Err(AgySessionError::Protocol("empty prompt"));
    }
    let (control, control_rx) = if request.control_keys.is_empty() {
        (None, None)
    } else {
        let (handle, rx) = AgyTurnHandle::bind(
            1,
            request
                .agy
                .resume_session_id
                .clone()
                .unwrap_or_else(|| "pending".to_string()),
            request
                .run_execution_id
                .clone()
                .unwrap_or_else(|| "agy-exec".to_string()),
        );
        (Some(handle), Some(rx))
    };
    if let Some(handle) = control.as_ref() {
        coding_control_registry()
            .register(
                &request.control_keys,
                CodingControlHandle::Agy(handle.clone()),
            )
            .await;
    }
    let mut session = AgyJsonlSession {
        reader: BufReader::new(reader),
        limits,
        events: EventQueue::new(request.event_sink.clone(), limits.max_event_queue),
        assistant: String::new(),
        native_session_id: None,
        terminal: None,
        control_rx,
        cancelled: request.cancel_token.clone(),
        watchdog: request
            .budgets
            .filter(|budgets| budgets.no_progress_enabled)
            .map(|budgets| ProgressWatchdog::new(budgets, std::time::Instant::now())),
        usage: None,
        saw_init: false,
        resume_session_id: request
            .agy
            .resume_session_id
            .clone()
            .filter(|id| !id.is_empty()),
        continuation_fresh_reason: None,
        open_tools: BTreeMap::new(),
        live_reporter: CodingLiveSessionReporter::from_request(request),
    };
    session
        .push_event(synthetic_event(
            session.events.next_sequence(),
            CodingEngineEventKind::AgentStart,
            "init",
        ))
        .ok();
    let outcome = session.drive(request).await;
    if !request.control_keys.is_empty() {
        coding_control_registry()
            .unregister(&request.control_keys)
            .await;
    }
    if let Some(handle) = control.as_ref() {
        handle.retire();
    }
    session.finalize(request, outcome)
}

struct AgyJsonlSession<R> {
    reader: BufReader<R>,
    limits: AgySessionLimits,
    events: EventQueue,
    assistant: String,
    native_session_id: Option<String>,
    terminal: Option<bool>,
    control_rx: Option<tokio::sync::mpsc::UnboundedReceiver<AgyControlCommand>>,
    cancelled: Option<tokio_util::sync::CancellationToken>,
    watchdog: Option<ProgressWatchdog>,
    usage: Option<AgyCapturedUsage>,
    saw_init: bool,
    resume_session_id: Option<String>,
    continuation_fresh_reason: Option<ContinuationFreshReason>,
    open_tools: BTreeMap<String, String>,
    /// Carried on the session for the same reason as Claude's: `handle_event`
    /// is where the conversation id appears and it has no request in scope.
    live_reporter: Option<CodingLiveSessionReporter>,
}

impl<R> AgyJsonlSession<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    async fn drive(&mut self, request: &CodingEngineRequest) -> Result<(), AgySessionError> {
        let deadline = tokio::time::Instant::now() + request.timeout;
        loop {
            if let Err(error) = self.check_liveness() {
                return Err(error);
            }
            if self.terminal.is_some() {
                return Ok(());
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Err(AgySessionError::Timeout);
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
                    return Err(AgySessionError::TurnFailed("cancelled".into()));
                }
                command = recv_agy_control(&mut self.control_rx) => {
                    match command {
                        Some(AgyControlCommand::Interrupt) => {
                            return Err(AgySessionError::TurnFailed("cancelled".into()));
                        }
                        None => {}
                    }
                }
                incoming = read_bounded_jsonl_value(
                    &mut self.reader,
                    max_line,
                    max_depth,
                ) => {
                    let value = map_bounded_jsonl(incoming)?;
                    self.handle_event(value)?;
                }
                _ = tokio::time::sleep(wait) => {}
            }
        }
    }

    fn handle_event(&mut self, value: Value) -> Result<(), AgySessionError> {
        if let Some(id) = agy_conversation_id_from(&value) {
            if self.native_session_id.is_none() {
                // Same shape and same window as Claude: the id arrives on the
                // `init` frame of the NDJSON stream, not from a handshake this
                // process drives. Wired for symmetry rather than for a live
                // route — the Agy factory is Unconstructable and nothing
                // spawns this adapter today, so this line runs only in tests.
                if let Some(reporter) = self.live_reporter.as_ref() {
                    reporter.report_live_session(CodingEngineKind::AgyCli, &id);
                }
                self.native_session_id = Some(id);
            }
        }
        match agy_event_name(&value) {
            "init" => {
                self.saw_init = true;
                if let Some(expected) = self.resume_session_id.as_deref() {
                    if self.native_session_id.as_deref() != Some(expected) {
                        self.continuation_fresh_reason =
                            Some(ContinuationFreshReason::ContinuationLost);
                    }
                }
                if let Some(leak) = agy_init_isolation_leak(&value) {
                    return Err(AgySessionError::TurnFailed(format!("isolation:{leak}")));
                }
                Ok(())
            },
            "step_update" => self.handle_step(&value),
            "result" => self.handle_result(&value),
            other if looks_like_permission_prompt(&value, other) => {
                Err(AgySessionError::Protocol("permission prompt"))
            },
            _ => Ok(()),
        }
    }

    fn handle_step(&mut self, value: &Value) -> Result<(), AgySessionError> {
        if let Some(leak) = agy_step_isolation_leak(value) {
            return Err(AgySessionError::TurnFailed(format!("isolation:{leak}")));
        }
        self.absorb_usage_from(value);
        let payload = value.get("step_update").unwrap_or(value);
        if let Some(delta) = payload
            .get("text_delta")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            append_bounded_utf8(&mut self.assistant, delta, MAX_ASSISTANT_BYTES);
            let mut event = synthetic_event(
                self.events.next_sequence(),
                CodingEngineEventKind::MessageUpdate,
                "step_update",
            );
            event.text_delta = Some(delta.to_string());
            event.assistant_event_type = Some("text_delta".to_string());
            self.push_event(event)?;
        }
        let step_type = payload
            .get("step_type")
            .and_then(Value::as_str)
            .unwrap_or("");
        let state = payload.get("state").and_then(Value::as_str).unwrap_or("");
        let tool_name = payload
            .get("tool_name")
            .or_else(|| payload.pointer("/tool_info/name"))
            .and_then(Value::as_str);
        if step_type == "tool" {
            let key = payload
                .get("step_index")
                .map(|v| v.to_string())
                .unwrap_or_else(|| tool_name.unwrap_or("tool").to_string());
            if state == "ACTIVE" {
                if self
                    .open_tools
                    .insert(key.clone(), tool_name.unwrap_or("tool").to_string())
                    .is_none()
                {
                    let mut event = synthetic_event(
                        self.events.next_sequence(),
                        CodingEngineEventKind::ToolExecutionStart,
                        "tool",
                    );
                    event.tool_name = tool_name.map(str::to_string);
                    event.tool_call_id = Some(key);
                    self.push_event(event)?;
                }
            } else if state == "DONE" {
                if self.open_tools.remove(&key).is_some() || tool_name.is_some() {
                    let mut event = synthetic_event(
                        self.events.next_sequence(),
                        CodingEngineEventKind::ToolExecutionEnd,
                        "tool",
                    );
                    event.tool_name = tool_name.map(str::to_string);
                    event.tool_call_id = Some(key);
                    self.push_event(event)?;
                }
            }
        }
        Ok(())
    }

    fn handle_result(&mut self, value: &Value) -> Result<(), AgySessionError> {
        if !agy_result_is_terminal(value) {
            return Ok(());
        }
        self.absorb_usage_from(value);
        if let Some(text) = value
            .pointer("/result/response")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            if self.assistant.is_empty() {
                append_bounded_utf8(&mut self.assistant, text, MAX_ASSISTANT_BYTES);
            }
        }
        let success = agy_result_is_success(value);
        self.terminal = Some(success);
        if !success {
            let status = agy_result_status(value).unwrap_or("ERROR");
            return Err(AgySessionError::TurnFailed(status.to_string()));
        }
        let mut turn_end = synthetic_event(
            self.events.next_sequence(),
            CodingEngineEventKind::TurnEnd,
            "result",
        );
        if let Some(usage) = self.usage.as_ref() {
            turn_end.usage = Some(usage.to_coding_usage());
            turn_end.cost_total = None;
        }
        self.push_event(turn_end)?;
        self.push_event(synthetic_event(
            self.events.next_sequence(),
            CodingEngineEventKind::AgentSettled,
            "result",
        ))?;
        Ok(())
    }

    fn absorb_usage_from(&mut self, value: &Value) {
        let Some(parsed) = parse_agy_usage(value) else {
            return;
        };
        match &mut self.usage {
            None => self.usage = Some(parsed),
            Some(current) => current.merge(parsed),
        }
    }

    fn check_liveness(&mut self) -> Result<(), AgySessionError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(AgySessionError::TurnFailed("cancelled".into()));
        }
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(AgySessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn push_event(&mut self, event: CodingEngineEvent) -> Result<(), AgySessionError> {
        if let Some(detector) = self.watchdog.as_mut() {
            detector.observe(&event, std::time::Instant::now());
        }
        self.events.push(event)?;
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(AgySessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn finalize(
        mut self,
        request: &CodingEngineRequest,
        outcome: Result<(), AgySessionError>,
    ) -> Result<CodingEngineRunResult, AgySessionError> {
        let success = matches!(&outcome, Ok(())) && self.terminal == Some(true);
        write_turn_usage(request, self.usage.as_ref(), success);
        if let Err(error) = &outcome {
            file_session_termination(request, error);
        }
        let events = self.events.finish();
        if self.resume_session_id.is_some()
            && !self.saw_init
            && pre_init_resume_is_lost(&outcome, request)
        {
            return Err(AgySessionError::ResumeLost);
        }
        outcome?;
        if self.terminal != Some(true) {
            return Err(AgySessionError::MissingResult);
        }
        let native = self
            .native_session_id
            .filter(|id| !id.is_empty())
            .ok_or(AgySessionError::Protocol("missing conversation_id"))?;
        let invocation_id = format!("agy-{}", Uuid::new_v4());
        let continuation = CodingContinuationRef::for_agy_session(
            native,
            &request.scope_root,
            &request.workspace_root,
            request.run_task_id.as_deref(),
        );
        Ok(CodingEngineRunResult {
            engine: CodingEngineKind::AgyCli,
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

#[derive(Debug, Clone, Default, PartialEq)]
struct AgyCapturedUsage {
    input: u64,
    output: u64,
    thinking: u64,
    cache_read: u64,
    total_tokens: u64,
}

impl AgyCapturedUsage {
    fn merge(&mut self, other: Self) {
        if other.input > 0 {
            self.input = other.input;
        }
        if other.output > 0 {
            self.output = other.output;
        }
        if other.thinking > 0 {
            self.thinking = other.thinking;
        }
        if other.cache_read > 0 {
            self.cache_read = other.cache_read;
        }
        if other.total_tokens > 0 {
            self.total_tokens = other.total_tokens;
        }
    }

    fn is_empty(&self) -> bool {
        self.input == 0
            && self.output == 0
            && self.thinking == 0
            && self.cache_read == 0
            && self.total_tokens == 0
    }

    fn metered_output(&self) -> u64 {
        let with_thinking = self.output.saturating_add(self.thinking);
        if with_thinking == 0 && self.input == 0 && self.total_tokens > 0 {
            self.total_tokens
        } else {
            with_thinking
        }
    }

    fn to_coding_usage(&self) -> CodingUsage {
        CodingUsage {
            input: self.input,
            output: self.metered_output(),
            cache_read: self.cache_read,
            cache_write: 0,
            total_tokens: self.total_tokens,
            cost_total: None,
        }
    }

    fn to_turn_usage(&self, success: bool) -> CodingTurnUsage {
        CodingTurnUsage {
            cost: 0.0,
            input: self.input,
            output: self.metered_output(),
            cache_read: self.cache_read,
            cache_write: 0,
            success,
            cost_known: false,
        }
    }
}

fn parse_agy_usage(value: &Value) -> Option<AgyCapturedUsage> {
    let usage = value
        .pointer("/result/usage")
        .or_else(|| value.pointer("/step_update/usage"))
        .or_else(|| value.get("usage"))?;
    if !usage.is_object() {
        return None;
    }
    let count = |key: &str| {
        usage
            .get(key)
            .and_then(Value::as_u64)
            .or_else(|| {
                usage
                    .get(key)
                    .and_then(Value::as_i64)
                    .and_then(|n| u64::try_from(n).ok())
            })
            .unwrap_or(0)
    };
    let parsed = AgyCapturedUsage {
        input: count("input_tokens"),
        output: count("output_tokens"),
        thinking: count("thinking_tokens"),
        cache_read: count("cache_read_tokens"),
        total_tokens: count("total_tokens"),
    };
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn write_turn_usage(
    request: &CodingEngineRequest,
    usage: Option<&AgyCapturedUsage>,
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

fn owner_cancelled(outcome: &Result<(), AgySessionError>, request: &CodingEngineRequest) -> bool {
    if request
        .cancel_token
        .as_ref()
        .is_some_and(|token| token.is_cancelled())
    {
        return true;
    }
    matches!(
        outcome,
        Err(AgySessionError::TurnFailed(status)) if status == "cancelled" || status == "CANCELED" || status == "INTERRUPTED"
    )
}

fn pre_init_resume_is_lost(
    outcome: &Result<(), AgySessionError>,
    request: &CodingEngineRequest,
) -> bool {
    if owner_cancelled(outcome, request) {
        return false;
    }
    match outcome {
        Err(AgySessionError::Timeout)
        | Err(AgySessionError::EventBackpressure)
        | Err(AgySessionError::FenceRequired) => false,
        Err(AgySessionError::TurnFailed(status)) if is_budget_or_idle_failure(status) => false,
        _ => true,
    }
}

fn is_budget_or_idle_failure(status: &str) -> bool {
    matches!(
        status,
        "no_progress"
            | "turn_timeout"
            | "task_budget"
            | "parent_deadline"
            | "cost_budget"
            | "service_shutdown"
            | "cancelled"
            | "CANCELED"
            | "INTERRUPTED"
    )
}

fn file_session_termination(request: &CodingEngineRequest, error: &AgySessionError) {
    let Some(key) = request.termination_key.as_deref() else {
        return;
    };
    let reason = match error {
        AgySessionError::Timeout => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        AgySessionError::TurnFailed(status)
            if status == "cancelled" || status == "CANCELED" || status == "INTERRUPTED" =>
        {
            CodingTerminationReason::OwnerCancelled
        },
        AgySessionError::TurnFailed(status) if status == "no_progress" => {
            CodingTerminationReason::NoProgress {
                phase: super::budgets::CodingProgressPhase::Model,
                elapsed_secs: 0,
                last_substantive_event: None,
            }
        },
        AgySessionError::EventBackpressure => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        _ => return,
    };
    record_termination_reason(key, reason);
}

fn looks_like_permission_prompt(value: &Value, event_type: &str) -> bool {
    let normalized = event_type.to_ascii_lowercase();
    if normalized.contains("permission") {
        return true;
    }
    value
        .get("permission_mode")
        .and_then(Value::as_str)
        .is_some_and(|mode| mode.eq_ignore_ascii_case("ask"))
        && event_type == "ask"
}

fn map_bounded_jsonl(result: Result<Value, BoundedJsonlError>) -> Result<Value, AgySessionError> {
    match result {
        Ok(value) => Ok(value),
        Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => Err(AgySessionError::MissingResult),
        Err(BoundedJsonlError::Oversized) => Err(AgySessionError::Protocol("oversized JSONL line")),
        Err(BoundedJsonlError::Malformed) => Err(AgySessionError::Protocol("malformed JSON")),
        Err(BoundedJsonlError::TooDeep) => Err(AgySessionError::Protocol("JSON nesting too deep")),
    }
}

async fn recv_agy_control(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<AgyControlCommand>>,
) -> Option<AgyControlCommand> {
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

fn synthetic_event(
    sequence: usize,
    kind: CodingEngineEventKind,
    agy_type: &str,
) -> CodingEngineEvent {
    CodingEngineEvent {
        sequence,
        kind,
        raw_type: Some(agy_type.to_string()),
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
            "agy_event": agy_type,
        }),
    }
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

    fn push(&mut self, event: CodingEngineEvent) -> Result<(), AgySessionError> {
        let coalescible = event.kind == CodingEngineEventKind::MessageUpdate;
        if coalescible {
            if let Some(pending) = self.pending_delta.as_mut() {
                let same_channel = pending.text_delta.is_some() == event.text_delta.is_some()
                    && pending.thinking_delta.is_some() == event.thinking_delta.is_some();
                if pending.kind == event.kind && same_channel {
                    if let (Some(left), Some(right)) =
                        (pending.text_delta.as_mut(), event.text_delta.as_ref())
                    {
                        left.push_str(right);
                    }
                    if let (Some(left), Some(right)) = (
                        pending.thinking_delta.as_mut(),
                        event.thinking_delta.as_ref(),
                    ) {
                        left.push_str(right);
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

    fn flush_pending(&mut self) -> Result<(), AgySessionError> {
        if let Some(event) = self.pending_delta.take() {
            self.emit(event)?;
        }
        Ok(())
    }

    fn emit(&mut self, event: CodingEngineEvent) -> Result<(), AgySessionError> {
        if self.emitted >= self.max as u64 {
            return Err(AgySessionError::EventBackpressure);
        }
        self.emitted += 1;
        self.next_seq = self.next_seq.saturating_add(1);
        if let Some(sink) = self.sink.as_ref() {
            sink(&event);
        }
        Ok(())
    }

    fn finish(&mut self) -> u64 {
        let _ = self.flush_pending();
        self.emitted
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use std::sync::{Arc, Mutex};
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
                    "tools": ["run_command", "view_file", "write_to_file"],
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
        let with_key = filter_agy_child_env(
            [
                ("PATH", "/usr/bin"),
                ("GEMINI_API_KEY", "gk-secret"),
                ("MAGICIAN_FOO", "secret"),
            ],
            true,
        );
        assert_eq!(
            with_key.get("GEMINI_API_KEY").map(String::as_str),
            Some("gk-secret")
        );
        assert!(!with_key.contains_key("MAGICIAN_FOO"));
    }

    #[test]
    fn discuss_and_build_argv_differ() {
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
        }
    }

    #[tokio::test]
    async fn init_catalog_search_web_is_not_an_isolation_leak() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let mut lines = happy_lines();
        lines[0] = json!({
            "event": "init",
            "conversation_id": NATIVE,
            "init": {
                "cwd": "/tmp",
                "tools": ["run_command", "view_file", "write_to_file", "search_web", "call_mcp_tool"],
                "permission_mode": "always-proceed"
            }
        });
        let peer = tokio::spawn(write_lines(server, lines));
        let result = run_turn_over_stdio(
            &request_for(dir.path()),
            client,
            AgySessionLimits::default(),
        )
        .await
        .expect("catalog names are not Ready-incompatible");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::AgyCli);
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
        assert!(
            matches!(
                error,
                AgySessionError::ResumeLost
                    | AgySessionError::Timeout
                    | AgySessionError::MissingResult
            ),
            "{error}"
        );
        assert!(
            error.to_string().contains("continuation is lost")
                || error.to_string().contains("timed out")
                || error.to_string().contains("terminal result"),
            "{error}"
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
        assert!(
            matches!(error, AgySessionError::TurnFailed(ref status) if status == "cancelled"),
            "{error}"
        );
    }

    #[test]
    fn no_progress_before_init_on_resume_is_not_continuation_lost() {
        let request = request_for(std::path::Path::new("/tmp"));
        let outcome = Err(AgySessionError::TurnFailed("no_progress".into()));
        assert!(!pre_init_resume_is_lost(&outcome, &request));
        let timeout = Err(AgySessionError::Timeout);
        assert!(!pre_init_resume_is_lost(&timeout, &request));
        let missing = Err(AgySessionError::MissingResult);
        assert!(pre_init_resume_is_lost(&missing, &request));
    }

    #[test]
    fn spawn_closes_stdin_and_never_uses_continue() {
        let source = include_str!("agy.rs");
        let production = source.split("\n#[cfg").next().expect("production");
        assert!(production.contains("Stdio::null()"));
        assert!(production.contains("stdin(std::process::Stdio::null())"));
        assert!(!production.contains(".arg(\"--continue\")"));
        assert!(production.contains("require_outer_fence"));
        assert!(production.contains("CodingControlHandle::Agy"));
    }
}

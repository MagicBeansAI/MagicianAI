//! Live bounded Claude Code headless adapter (`claude -p --output-format stream-json`).
//!
//! The factory constructs this when the journaled engine is Claude Code.
//! Tests drive the same drain against a fake NDJSON peer. Spawn uses
//! `Stdio::null()` for stdin. Native resume is `--resume <uuid>` when
//! `resume_session_id` is set.

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

use super::budgets::{record_termination_reason, CodingTerminationReason, ProgressWatchdog};
use super::claude_contract::{
    claude_child_env_allowlist, claude_init_api_key_source_leak, claude_init_isolation_leak,
    claude_result_is_success, claude_result_is_terminal, claude_session_id_from,
    is_claude_api_key_env, launch_args,
};
use super::codex_lifecycle::ContinuationFreshReason;
use super::control::{
    coding_control_registry, ClaudeControlCommand, ClaudeTurnHandle, CodingControlHandle,
};
use super::factory::{ClaudeCodingOptions, ClaudeTurnMode};
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
pub struct ClaudeSessionLimits {
    pub max_line_bytes: usize,
    pub max_json_depth: usize,
    pub max_event_queue: usize,
}

impl Default for ClaudeSessionLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            max_json_depth: MAX_JSON_DEPTH,
            max_event_queue: MAX_EVENT_QUEUE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeSessionError {
    Timeout,
    Protocol(&'static str),
    Io,
    EventBackpressure,
    MissingResult,
    TurnFailed(String),
    FenceRequired,
    ResumeLost,
}

impl std::fmt::Display for ClaudeSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Timeout => write!(f, "Claude session timed out"),
            Self::Protocol(reason) => write!(f, "Claude protocol error: {reason}"),
            Self::Io => write!(f, "Claude session I/O failed"),
            Self::EventBackpressure => write!(f, "event_backpressure"),
            Self::MissingResult => write!(
                f,
                "Claude turn closed without a terminal result success object"
            ),
            Self::TurnFailed(status) => write!(f, "Claude turn ended as {status}"),
            Self::FenceRequired => write!(
                f,
                "Claude dispatch requires the Magician outer coding fence; the Pi unsandboxed fallback is not allowed"
            ),
            Self::ResumeLost => write!(
                f,
                "Claude --resume did not emit system/init; continuation is lost"
            ),
        }
    }
}

impl std::error::Error for ClaudeSessionError {}

impl From<CodingFenceError> for ClaudeSessionError {
    fn from(_: CodingFenceError) -> Self {
        Self::FenceRequired
    }
}

#[derive(Debug, Clone)]
pub struct ClaudeCodeAdapter {
    binary: PathBuf,
    use_api_key: bool,
}

impl ClaudeCodeAdapter {
    pub fn new(binary: impl Into<PathBuf>, use_api_key: bool) -> Self {
        Self {
            binary: binary.into(),
            use_api_key,
        }
    }

    pub fn from_options(options: ClaudeCodingOptions) -> Self {
        Self::new(options.binary, options.use_api_key)
    }

    pub fn launch_args(
        mode: ClaudeTurnMode,
        prompt: &str,
        resume_session_id: Option<&str>,
    ) -> Vec<String> {
        launch_args(mode, prompt, resume_session_id)
    }
}

#[async_trait]
impl CodingEngineAdapter for ClaudeCodeAdapter {
    fn engine(&self) -> CodingEngineKind {
        CodingEngineKind::ClaudeCode
    }

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult> {
        require_outer_fence().map_err(ClaudeSessionError::from)?;
        validate_request(&request)?;
        match self.run_once(&request).await {
            Ok(result) => Ok(result),
            Err(error)
                if request
                    .claude
                    .resume_session_id
                    .as_deref()
                    .is_some_and(|id| !id.is_empty())
                    && error.downcast_ref::<ClaudeSessionError>()
                        == Some(&ClaudeSessionError::ResumeLost)
                    && !request
                        .cancel_token
                        .as_ref()
                        .is_some_and(|token| token.is_cancelled()) =>
            {
                let mut fresh = request.clone();
                fresh.claude.resume_session_id = None;
                let mut result = self.run_once(&fresh).await?;
                result.continuation_fresh_reason = Some(ContinuationFreshReason::ContinuationLost);
                Ok(result)
            },
            Err(error) => Err(error),
        }
    }
}

impl ClaudeCodeAdapter {
    async fn run_once(&self, request: &CodingEngineRequest) -> Result<CodingEngineRunResult> {
        let mut child = OwnedClaudeChild {
            child: spawn_claude(self, request)?,
            reaped: false,
        };
        let stdout = child
            .child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Claude child stdout was not piped"))?;
        if let Some(stderr) = child.child.stderr.take() {
            tokio::spawn(retain_stderr_ring(stderr));
        }
        let result = run_turn_over_stdio(
            request,
            stdout,
            ClaudeSessionLimits::default(),
            self.use_api_key,
        )
        .await;
        child.reap().await;
        result.map_err(anyhow::Error::from)
    }
}

fn validate_request(request: &CodingEngineRequest) -> Result<()> {
    if request.prompt.trim().is_empty() {
        return Err(anyhow!("Claude coding request prompt is empty"));
    }
    if !request.shadow_workspace_root.is_dir() {
        return Err(anyhow!(
            "Claude shadow workspace does not exist: {}",
            request.shadow_workspace_root.display()
        ));
    }
    Ok(())
}

pub fn filter_claude_child_env<'a, I>(entries: I, use_api_key: bool) -> BTreeMap<String, String>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let allowed: BTreeMap<&str, ()> = claude_child_env_allowlist()
        .iter()
        .map(|key| (*key, ()))
        .collect();
    let mut filtered = BTreeMap::new();
    for (key, value) in entries {
        if key.starts_with("MAGICIAN_") {
            continue;
        }
        if is_claude_api_key_env(key) {
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

fn spawn_claude(
    adapter: &ClaudeCodeAdapter,
    request: &CodingEngineRequest,
) -> Result<tokio::process::Child> {
    let args: Vec<OsString> = ClaudeCodeAdapter::launch_args(
        request.claude.mode,
        &request.prompt,
        request.claude.resume_session_id.as_deref(),
    )
    .into_iter()
    .map(OsString::from)
    .collect();
    debug_assert!(!args.iter().any(|arg| arg == "--bare"));
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
    let filtered = filter_claude_child_env(
        inherited
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str())),
        adapter.use_api_key,
    );
    for (key, value) in filtered {
        command.env(key, value);
    }
    command.env("GIT_CEILING_DIRECTORIES", &request.scope_root);
    apply_claude_child_process_group(&mut command);
    command.spawn().with_context(|| {
        format!(
            "spawn Claude Code `{}` in {}",
            adapter.binary.display(),
            working_dir.display()
        )
    })
}

struct OwnedClaudeChild {
    child: tokio::process::Child,
    reaped: bool,
}

impl OwnedClaudeChild {
    async fn reap(&mut self) {
        terminate_process_group(&mut self.child).await;
        self.reaped = true;
    }
}

impl Drop for OwnedClaudeChild {
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

pub(crate) fn apply_claude_child_process_group(command: &mut tokio::process::Command) {
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

/// Drain one turn of Claude NDJSON. Tests use this against a fake peer;
/// the live spawn path is the only caller that creates a process.
pub async fn run_turn_over_stdio<R>(
    request: &CodingEngineRequest,
    reader: R,
    limits: ClaudeSessionLimits,
    use_api_key: bool,
) -> Result<CodingEngineRunResult, ClaudeSessionError>
where
    R: tokio::io::AsyncRead + Unpin,
{
    if request.prompt.trim().is_empty() {
        return Err(ClaudeSessionError::Protocol("empty prompt"));
    }
    let (control, control_rx) = if request.control_keys.is_empty() {
        (None, None)
    } else {
        let (handle, rx) = ClaudeTurnHandle::bind(
            1,
            request
                .claude
                .resume_session_id
                .clone()
                .unwrap_or_else(|| "pending".to_string()),
            request
                .run_execution_id
                .clone()
                .unwrap_or_else(|| "claude-exec".to_string()),
        );
        (Some(handle), Some(rx))
    };
    if let Some(handle) = control.as_ref() {
        coding_control_registry()
            .register(
                &request.control_keys,
                CodingControlHandle::Claude(handle.clone()),
            )
            .await;
    }
    let mut session = ClaudeJsonlSession {
        reader: BufReader::new(reader),
        limits,
        events: EventQueue::new(request.event_sink.clone(), limits.max_event_queue),
        assistant: String::new(),
        native_session_id: None,
        terminal: None,
        control_rx,
        use_api_key,
        cancelled: request.cancel_token.clone(),
        watchdog: request
            .budgets
            .filter(|budgets| budgets.no_progress_enabled)
            .map(|budgets| ProgressWatchdog::new(budgets, std::time::Instant::now())),
        usage: None,
        saw_init: false,
        resume_session_id: request
            .claude
            .resume_session_id
            .clone()
            .filter(|id| !id.is_empty()),
        continuation_fresh_reason: None,
        live_reporter: CodingLiveSessionReporter::from_request(request),
    };
    session
        .push_event(synthetic_event(
            session.events.next_sequence(),
            CodingEngineEventKind::AgentStart,
            "system",
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

struct ClaudeJsonlSession<R> {
    reader: BufReader<R>,
    limits: ClaudeSessionLimits,
    events: EventQueue,
    assistant: String,
    native_session_id: Option<String>,
    terminal: Option<bool>,
    control_rx: Option<tokio::sync::mpsc::UnboundedReceiver<ClaudeControlCommand>>,
    use_api_key: bool,
    cancelled: Option<tokio_util::sync::CancellationToken>,
    watchdog: Option<ProgressWatchdog>,
    usage: Option<ClaudeCapturedUsage>,
    saw_init: bool,
    resume_session_id: Option<String>,
    continuation_fresh_reason: Option<ContinuationFreshReason>,
    /// Carried on the session rather than read off the request because
    /// `handle_event` — the only place Claude's session id ever appears — has
    /// no request in scope. `None` for every caller that installed no sink,
    /// which is every test in this file.
    live_reporter: Option<CodingLiveSessionReporter>,
}

impl<R> ClaudeJsonlSession<R>
where
    R: tokio::io::AsyncRead + Unpin,
{
    async fn drive(&mut self, request: &CodingEngineRequest) -> Result<(), ClaudeSessionError> {
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
                return Err(ClaudeSessionError::Timeout);
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
                    return Err(ClaudeSessionError::TurnFailed("cancelled".into()));
                }
                command = recv_claude_control(&mut self.control_rx) => {
                    match command {
                        Some(ClaudeControlCommand::Interrupt) => {
                            return Err(ClaudeSessionError::TurnFailed("cancelled".into()));
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

    fn handle_event(&mut self, value: Value) -> Result<(), ClaudeSessionError> {
        if let Some(id) = claude_session_id_from(&value) {
            if self.native_session_id.is_none() {
                // The `system/init` frame is the FIRST thing `claude -p
                // --output-format stream-json` writes, so this fires within a
                // frame of the process opening its mouth — but strictly later
                // than Codex's and Grok's, because Claude has no handshake we
                // drive: the prompt goes in on argv and the id comes back out
                // on the stream. A worker killed between spawn and this frame
                // still has no session, and that window is real.
                if let Some(reporter) = self.live_reporter.as_ref() {
                    reporter.report_live_session(CodingEngineKind::ClaudeCode, &id);
                }
                self.native_session_id = Some(id);
            }
        }
        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        match event_type {
            "system" => {
                if value.get("subtype").and_then(Value::as_str) == Some("init") {
                    self.saw_init = true;
                    if let Some(expected) = self.resume_session_id.as_deref() {
                        if self.native_session_id.as_deref() != Some(expected) {
                            self.continuation_fresh_reason =
                                Some(ContinuationFreshReason::ContinuationLost);
                        }
                    }
                    if let Some(leak) = claude_init_isolation_leak(&value) {
                        return Err(ClaudeSessionError::TurnFailed(format!("isolation:{leak}")));
                    }
                    if let Some(leak) = claude_init_api_key_source_leak(&value, self.use_api_key) {
                        return Err(ClaudeSessionError::TurnFailed(format!("isolation:{leak}")));
                    }
                }
                Ok(())
            },
            "rate_limit_event" => Ok(()),
            "assistant" => self.handle_assistant(&value),
            "user" => self.handle_user(&value),
            "result" => self.handle_result(&value),
            "stream_event" => self.handle_stream_event(&value),
            other if looks_like_permission_prompt(&value, other) => {
                Err(ClaudeSessionError::Protocol("permission prompt"))
            },
            "control_request" | "can_use_tool" | "user_interrupt" => {
                Err(ClaudeSessionError::Protocol("unsupported control event"))
            },
            _ => Ok(()),
        }
    }

    fn handle_assistant(&mut self, value: &Value) -> Result<(), ClaudeSessionError> {
        self.absorb_usage_from(value);
        let content = value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        self.emit_content_blocks(&content)
    }

    fn handle_user(&mut self, value: &Value) -> Result<(), ClaudeSessionError> {
        let content = value
            .pointer("/message/content")
            .or_else(|| value.get("content"))
            .cloned()
            .unwrap_or(Value::Null);
        self.emit_content_blocks(&content)
    }

    fn handle_stream_event(&mut self, value: &Value) -> Result<(), ClaudeSessionError> {
        if let Some(delta) = value
            .pointer("/event/delta/text")
            .or_else(|| value.pointer("/delta/text"))
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            append_bounded_utf8(&mut self.assistant, delta, MAX_ASSISTANT_BYTES);
            let mut event = synthetic_event(
                self.events.next_sequence(),
                CodingEngineEventKind::MessageUpdate,
                "stream_event",
            );
            event.text_delta = Some(delta.to_string());
            event.assistant_event_type = Some("text_delta".to_string());
            self.push_event(event)?;
        }
        Ok(())
    }

    fn emit_content_blocks(&mut self, content: &Value) -> Result<(), ClaudeSessionError> {
        let blocks = match content {
            Value::Array(items) => items.clone(),
            Value::Object(_) => vec![content.clone()],
            _ => return Ok(()),
        };
        for block in blocks {
            let kind = block.get("type").and_then(Value::as_str).unwrap_or("");
            match kind {
                "text" => {
                    if let Some(text) = block
                        .get("text")
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty())
                    {
                        append_bounded_utf8(&mut self.assistant, text, MAX_ASSISTANT_BYTES);
                        let mut event = synthetic_event(
                            self.events.next_sequence(),
                            CodingEngineEventKind::MessageUpdate,
                            "assistant",
                        );
                        event.text_delta = Some(text.to_string());
                        event.assistant_event_type = Some("text_delta".to_string());
                        self.push_event(event)?;
                    }
                },
                "thinking" => {
                    let thinking = block
                        .get("thinking")
                        .or_else(|| block.get("text"))
                        .and_then(Value::as_str)
                        .filter(|text| !text.is_empty());
                    if let Some(text) = thinking {
                        let mut event = synthetic_event(
                            self.events.next_sequence(),
                            CodingEngineEventKind::MessageUpdate,
                            "assistant",
                        );
                        event.thinking_delta = Some(text.to_string());
                        event.assistant_event_type = Some("thinking_delta".to_string());
                        self.push_event(event)?;
                    }
                },
                "tool_use" => {
                    let mut event = synthetic_event(
                        self.events.next_sequence(),
                        CodingEngineEventKind::ToolExecutionStart,
                        "tool_use",
                    );
                    event.tool_call_id =
                        block.get("id").and_then(Value::as_str).map(str::to_string);
                    event.tool_name = block
                        .get("name")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    self.push_event(event)?;
                },
                "tool_result" => {
                    let mut event = synthetic_event(
                        self.events.next_sequence(),
                        CodingEngineEventKind::ToolExecutionEnd,
                        "tool_result",
                    );
                    event.tool_call_id = block
                        .get("tool_use_id")
                        .or_else(|| block.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    event.tool_result_is_error = block.get("is_error").and_then(Value::as_bool);
                    self.push_event(event)?;
                },
                _ => {},
            }
        }
        Ok(())
    }

    fn handle_result(&mut self, value: &Value) -> Result<(), ClaudeSessionError> {
        if !claude_result_is_terminal(value) {
            return Ok(());
        }
        self.absorb_usage_from(value);
        if let Some(text) = value
            .get("result")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            if self.assistant.is_empty() {
                append_bounded_utf8(&mut self.assistant, text, MAX_ASSISTANT_BYTES);
            }
        }
        let success = claude_result_is_success(value);
        self.terminal = Some(success);
        if !success {
            let subtype = value
                .get("subtype")
                .and_then(Value::as_str)
                .unwrap_or("error");
            return Err(ClaudeSessionError::TurnFailed(subtype.to_string()));
        }
        let mut turn_end = synthetic_event(
            self.events.next_sequence(),
            CodingEngineEventKind::TurnEnd,
            "result",
        );
        if let Some(usage) = self.usage.as_ref() {
            turn_end.usage = Some(usage.to_coding_usage());
            turn_end.cost_total = usage.cost;
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
        let Some(parsed) = parse_claude_usage(value) else {
            return;
        };
        match &mut self.usage {
            None => self.usage = Some(parsed),
            Some(current) => current.merge(parsed),
        }
    }

    fn check_liveness(&mut self) -> Result<(), ClaudeSessionError> {
        if self
            .cancelled
            .as_ref()
            .is_some_and(|token| token.is_cancelled())
        {
            return Err(ClaudeSessionError::TurnFailed("cancelled".into()));
        }
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(ClaudeSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn push_event(&mut self, event: CodingEngineEvent) -> Result<(), ClaudeSessionError> {
        if let Some(detector) = self.watchdog.as_mut() {
            detector.observe(&event, std::time::Instant::now());
        }
        self.events.push(event)?;
        if let Some(reason) = self
            .watchdog
            .as_ref()
            .and_then(|detector| detector.expired(std::time::Instant::now()))
        {
            return Err(ClaudeSessionError::TurnFailed(reason.kind().to_string()));
        }
        Ok(())
    }

    fn finalize(
        mut self,
        request: &CodingEngineRequest,
        outcome: Result<(), ClaudeSessionError>,
    ) -> Result<CodingEngineRunResult, ClaudeSessionError> {
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
            return Err(ClaudeSessionError::ResumeLost);
        }
        outcome?;
        if self.terminal != Some(true) {
            return Err(ClaudeSessionError::MissingResult);
        }
        let native = self
            .native_session_id
            .filter(|id| !id.is_empty())
            .ok_or(ClaudeSessionError::Protocol("missing session_id"))?;
        let invocation_id = format!("claude-{}", Uuid::new_v4());
        let continuation = CodingContinuationRef::for_claude_session(
            native,
            &request.scope_root,
            &request.workspace_root,
            request.run_task_id.as_deref(),
        );
        Ok(CodingEngineRunResult {
            engine: CodingEngineKind::ClaudeCode,
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
struct ClaudeCapturedUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    total_tokens: u64,
    cost: Option<f64>,
}

impl ClaudeCapturedUsage {
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

fn parse_claude_usage(value: &Value) -> Option<ClaudeCapturedUsage> {
    let usage_obj = value
        .get("usage")
        .or_else(|| value.pointer("/message/usage"))
        .or_else(|| value.pointer("/result/usage"));
    let mut parsed = usage_obj.and_then(parse_usage_object).unwrap_or_default();
    if let Some(cost) = json_f64(value.get("total_cost_usd"))
        .or_else(|| usage_obj.and_then(|obj| json_f64(obj.get("total_cost_usd"))))
    {
        parsed.cost = Some(cost);
    }
    if parsed.is_empty() {
        None
    } else {
        Some(parsed)
    }
}

fn parse_usage_object(value: &Value) -> Option<ClaudeCapturedUsage> {
    if !value.is_object() {
        return None;
    }
    let count = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| json_u64(value.get(*key)?))
            .unwrap_or(0)
    };
    let usage = ClaudeCapturedUsage {
        input: count(&["input_tokens", "inputTokens", "input"]),
        output: count(&["output_tokens", "outputTokens", "output"]),
        cache_read: count(&[
            "cache_read_input_tokens",
            "cacheReadInputTokens",
            "cache_read",
        ]),
        cache_write: count(&[
            "cache_creation_input_tokens",
            "cacheCreationInputTokens",
            "cache_write",
        ]),
        total_tokens: count(&["total_tokens", "totalTokens", "total"]),
        cost: json_f64(value.get("total_cost_usd")),
    };
    if usage.is_empty() {
        None
    } else {
        Some(usage)
    }
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
    usage: Option<&ClaudeCapturedUsage>,
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

fn owner_cancelled(
    outcome: &Result<(), ClaudeSessionError>,
    request: &CodingEngineRequest,
) -> bool {
    if request
        .cancel_token
        .as_ref()
        .is_some_and(|token| token.is_cancelled())
    {
        return true;
    }
    matches!(
        outcome,
        Err(ClaudeSessionError::TurnFailed(status)) if status == "cancelled"
    )
}

/// `--resume` with no `system/init` is lost only on stream-end. Timeout,
/// backpressure, and owner Stop must not spawn a second billed `-p`.
fn pre_init_resume_is_lost(
    outcome: &Result<(), ClaudeSessionError>,
    request: &CodingEngineRequest,
) -> bool {
    if owner_cancelled(outcome, request) {
        return false;
    }
    !matches!(
        outcome,
        Err(ClaudeSessionError::Timeout)
            | Err(ClaudeSessionError::EventBackpressure)
            | Err(ClaudeSessionError::FenceRequired)
    )
}

fn file_session_termination(request: &CodingEngineRequest, error: &ClaudeSessionError) {
    let Some(key) = request.termination_key.as_deref() else {
        return;
    };
    let reason = match error {
        ClaudeSessionError::Timeout => CodingTerminationReason::TurnTimeout {
            limit_secs: request.timeout.as_secs(),
        },
        ClaudeSessionError::TurnFailed(status) if status == "cancelled" => {
            CodingTerminationReason::OwnerCancelled
        },
        ClaudeSessionError::EventBackpressure => CodingTerminationReason::TurnTimeout {
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
        .get("subtype")
        .and_then(Value::as_str)
        .is_some_and(|subtype| subtype.to_ascii_lowercase().contains("permission"))
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

    fn push(&mut self, event: CodingEngineEvent) -> Result<(), ClaudeSessionError> {
        let coalescible = event.kind == CodingEngineEventKind::MessageUpdate;
        if coalescible {
            if let Some(pending) = self.pending_delta.as_mut() {
                let same_channel = pending.text_delta.is_some() == event.text_delta.is_some()
                    && pending.thinking_delta.is_some() == event.thinking_delta.is_some();
                if pending.kind == event.kind && same_channel {
                    coalesce_message_deltas(pending, event);
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

    fn flush_pending(&mut self) -> Result<(), ClaudeSessionError> {
        if let Some(event) = self.pending_delta.take() {
            self.emit(event)?;
        }
        Ok(())
    }

    fn emit(&mut self, event: CodingEngineEvent) -> Result<(), ClaudeSessionError> {
        if self.emitted >= self.max as u64 {
            return Err(ClaudeSessionError::EventBackpressure);
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

fn map_bounded_jsonl(
    result: Result<Value, BoundedJsonlError>,
) -> Result<Value, ClaudeSessionError> {
    match result {
        Ok(value) => Ok(value),
        Err(BoundedJsonlError::Eof | BoundedJsonlError::Io) => {
            Err(ClaudeSessionError::MissingResult)
        },
        Err(BoundedJsonlError::Oversized) => {
            Err(ClaudeSessionError::Protocol("oversized JSONL line"))
        },
        Err(BoundedJsonlError::Malformed) => Err(ClaudeSessionError::Protocol("malformed JSON")),
        Err(BoundedJsonlError::TooDeep) => {
            Err(ClaudeSessionError::Protocol("JSON nesting too deep"))
        },
    }
}

async fn recv_claude_control(
    rx: &mut Option<tokio::sync::mpsc::UnboundedReceiver<ClaudeControlCommand>>,
) -> Option<ClaudeControlCommand> {
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
    claude_type: &str,
) -> CodingEngineEvent {
    CodingEngineEvent {
        sequence,
        kind,
        raw_type: Some(claude_type.to_string()),
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
            "claude_type": claude_type,
        }),
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::super::claude_contract::claude_api_key_env_names;
    use super::*;
    use crate::magician_v2::execution::coding_engine::{
        construct_coding_adapter, ClaudeCodingOptions, ClaudeTurnMode, CodingAdapterSpec,
        PiTurnOptions,
    };
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
                "type": "system",
                "subtype": "init",
                "session_id": NATIVE,
                "tools": ["Read", "Bash"],
                "mcp_servers": [],
                "apiKeySource": "none",
                "permissionMode": "bypassPermissions",
                "claude_code_version": "2.1.229"
            }),
            json!({
                "type": "assistant",
                "session_id": NATIVE,
                "message": {
                    "content": [
                        {"type": "thinking", "thinking": "hmm"},
                        {"type": "text", "text": "hello "},
                        {"type": "text", "text": "world"},
                        {"type": "tool_use", "id": "t1", "name": "Read", "input": {"path": "a.rs"}}
                    ],
                    "usage": {"input_tokens": 10, "output_tokens": 4}
                }
            }),
            json!({
                "type": "user",
                "session_id": NATIVE,
                "message": {
                    "content": [{"type": "tool_result", "tool_use_id": "t1", "content": "ok"}]
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

    fn joined_text_deltas(events: &[CodingEngineEvent]) -> String {
        events
            .iter()
            .filter(|event| event.kind == CodingEngineEventKind::MessageUpdate)
            .filter_map(|event| event.text_delta.as_deref())
            .collect()
    }

    #[tokio::test]
    async fn claude_fake_turn_maps_events_and_hides_the_native_session() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(32 * 1024);
        let peer = tokio::spawn(write_lines(server, happy_lines()));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink_seen = seen.clone();
        let mut request = request_for(dir.path());
        request.event_sink = Some(Arc::new(move |event: &CodingEngineEvent| {
            sink_seen.lock().unwrap().push(event.clone());
        }));
        request.pi = PiTurnOptions {
            session_name: Some("must-not-leak".into()),
            ..PiTurnOptions::default()
        };
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
        assert_eq!(joined_text_deltas(&events), "hello world");
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::ToolExecutionStart));
        assert!(events
            .iter()
            .any(|event| event.kind == CodingEngineEventKind::ToolExecutionEnd));
        assert!(events.iter().any(|event| event.thinking_delta.is_some()));
        for event in events.iter() {
            let raw = event.raw.to_string();
            assert!(
                !raw.contains(NATIVE),
                "native session id leaked into event raw: {raw}"
            );
        }
        let usage = capture.lock().unwrap().clone().expect("usage");
        assert!(usage.cost_known);
        assert_eq!(usage.cost, 0.012);
        assert_eq!(usage.input, 10);
        assert_eq!(usage.output, 4);
    }

    #[tokio::test]
    async fn claude_missing_result_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(write_lines(
            server,
            vec![json!({
                "type": "assistant",
                "session_id": NATIVE,
                "message": {"content": [{"type": "text", "text": "partial"}]}
            })],
        ));
        let mut request = request_for(dir.path());
        request.timeout = Duration::from_millis(200);
        let error = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
            .await
            .expect_err("no result");
        let _ = peer.await;
        assert!(matches!(
            error,
            ClaudeSessionError::MissingResult | ClaudeSessionError::Timeout
        ));
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
        assert!(matches!(
            error,
            ClaudeSessionError::ResumeLost | ClaudeSessionError::Timeout
        ));
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
        assert!(
            matches!(error, ClaudeSessionError::TurnFailed(ref status) if status == "cancelled"),
            "{error}"
        );
    }

    #[tokio::test]
    async fn resume_session_id_mismatch_records_continuation_lost_and_still_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(32 * 1024);
        let peer = tokio::spawn(write_lines(server, happy_lines()));
        let mut request = request_for(dir.path());
        request.claude.resume_session_id = Some("bbbbbbbb-bbbb-cccc-dddd-eeeeeeeeeeee".to_string());
        let result = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(
            result.continuation_fresh_reason,
            Some(ContinuationFreshReason::ContinuationLost)
        );
        assert_eq!(
            result
                .continuation
                .as_ref()
                .map(|c| c.native_session_id.as_str()),
            Some(NATIVE)
        );
    }

    #[tokio::test]
    async fn claude_init_with_web_search_fails_closed() {
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
        assert!(matches!(
            error,
            ClaudeSessionError::TurnFailed(reason) if reason.contains("isolation")
        ));
    }

    #[tokio::test]
    async fn claude_error_result_is_not_success() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(write_lines(
            server,
            vec![json!({
                "type": "result",
                "subtype": "error",
                "is_error": true,
                "session_id": NATIVE,
                "result": "nope"
            })],
        ));
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            client,
            ClaudeSessionLimits::default(),
            false,
        )
        .await
        .expect_err("error result");
        let _ = peer.await;
        assert!(matches!(error, ClaudeSessionError::TurnFailed(_)));
    }

    #[tokio::test]
    async fn claude_omitted_cost_never_becomes_known_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(write_lines(
            server,
            vec![
                json!({"type": "system", "subtype": "init", "session_id": NATIVE, "tools": ["Read"], "mcp_servers": []}),
                json!({
                    "type": "assistant",
                    "session_id": NATIVE,
                    "message": {"content": [{"type": "text", "text": "ok"}]}
                }),
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
        let result = run_turn_over_stdio(&request, client, ClaudeSessionLimits::default(), false)
            .await
            .expect("turn");
        let _ = peer.await;
        assert_eq!(result.engine, CodingEngineKind::ClaudeCode);
        let usage = capture.lock().unwrap().clone().expect("usage");
        assert!(!usage.cost_known);
        assert_eq!(usage.cost, 0.0);
        assert_eq!(usage.input, 2);
    }

    #[tokio::test]
    async fn claude_malformed_and_oversized_lines_fail_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (_reader, mut writer) = tokio::io::split(server);
            let _ = writer.write_all(b"{not-json\n").await;
        });
        let error = run_turn_over_stdio(
            &request_for(dir.path()),
            client,
            ClaudeSessionLimits::default(),
            false,
        )
        .await
        .expect_err("malformed");
        let _ = peer.await;
        assert!(matches!(error, ClaudeSessionError::Protocol(_)));

        let (client, server) = duplex(16 * 1024);
        let peer = tokio::spawn(async move {
            let (_reader, mut writer) = tokio::io::split(server);
            let mut line = vec![b'x'; 64];
            line.push(b'\n');
            let _ = writer.write_all(&line).await;
        });
        let mut limits = ClaudeSessionLimits::default();
        limits.max_line_bytes = 16;
        let error = run_turn_over_stdio(&request_for(dir.path()), client, limits, false)
            .await
            .expect_err("oversized");
        let _ = peer.await;
        assert!(
            matches!(error, ClaudeSessionError::Protocol(reason) if reason.contains("oversized"))
        );
    }

    #[test]
    fn claude_discuss_and_build_send_distinct_permission_flags() {
        let build = ClaudeCodeAdapter::launch_args(ClaudeTurnMode::Build, "do it", None);
        let discuss = ClaudeCodeAdapter::launch_args(ClaudeTurnMode::Discuss, "plan it", None);
        assert!(build
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "bypassPermissions"]));
        assert!(build
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(discuss
            .windows(2)
            .any(|pair| pair == ["--permission-mode", "plan"]));
        assert!(!discuss
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        for args in [&build, &discuss] {
            assert!(args.iter().any(|arg| arg == "--verbose"));
            assert!(args
                .windows(2)
                .any(|pair| pair == ["--output-format", "stream-json"]));
            assert!(args.iter().any(|arg| arg == "--strict-mcp-config"));
            assert!(args.iter().any(|arg| arg.contains("WebSearch")
                || args
                    .windows(2)
                    .any(|pair| pair[0] == "--disallowedTools" && pair[1].contains("WebSearch"))));
            assert!(!args.iter().any(|arg| arg == "--bare"));
            assert!(!args.iter().any(|arg| arg == "-c"));
        }
    }

    #[test]
    fn claude_child_env_drops_magician_and_api_keys_by_default() {
        let filtered = filter_claude_child_env(
            [
                ("PATH", "/usr/bin"),
                ("HOME", "/Users/me"),
                ("ANTHROPIC_API_KEY", "sk-secret"),
                ("CLAUDE_CODE_API_KEY", "sk-other"),
                ("ANTHROPIC_AUTH_TOKEN", "tok"),
                ("MAGICIAN_ADMIN_TOKEN", "nope"),
                ("MAGICIAN_FOO", "secret"),
                ("CODEX_HOME", "/secret"),
            ],
            false,
        );
        assert_eq!(filtered.get("PATH").map(String::as_str), Some("/usr/bin"));
        assert!(filtered.contains_key("HOME"));
        assert!(!filtered.contains_key("ANTHROPIC_API_KEY"));
        assert!(!filtered.contains_key("CLAUDE_CODE_API_KEY"));
        assert!(!filtered.contains_key("ANTHROPIC_AUTH_TOKEN"));
        assert!(!filtered.contains_key("MAGICIAN_ADMIN_TOKEN"));
        assert!(!filtered.contains_key("MAGICIAN_FOO"));
        assert!(!filtered.contains_key("CODEX_HOME"));
        for name in claude_api_key_env_names() {
            assert!(!filtered.contains_key(*name));
        }

        let with_key = filter_claude_child_env(
            [
                ("PATH", "/usr/bin"),
                ("ANTHROPIC_API_KEY", "sk-secret"),
                ("MAGICIAN_FOO", "secret"),
            ],
            true,
        );
        assert_eq!(
            with_key.get("ANTHROPIC_API_KEY").map(String::as_str),
            Some("sk-secret")
        );
        assert!(!with_key.contains_key("MAGICIAN_FOO"));
    }

    #[test]
    fn claude_spawn_without_the_outer_fence_refuses() {
        assert_eq!(require_outer_fence(), Err(CodingFenceError::Required));
    }

    #[test]
    fn claude_factory_and_adapter_do_not_stage_proposals() {
        let whole = include_str!("claude.rs");
        let source = whole
            .find("\n#[cfg(test)]")
            .into_iter()
            .chain(whole.find("\n#[cfg(any(test"))
            .min()
            .map(|at| &whole[..at])
            .expect("production Claude adapter source");
        assert!(!source.contains("stage_shadow_workspace_patch"));
        assert!(!source.contains("attach_staged_coding_proposal("));
        assert!(
            !source.contains("request.pi"),
            "Claude must not read Pi session/model flags"
        );
        assert!(!source.contains("request.codex"));
        assert!(!source.contains("request.grok"));
        assert!(!source.contains("request.agy"));
        let adapter = construct_coding_adapter(
            CodingEngineKind::ClaudeCode,
            CodingAdapterSpec::Claude(ClaudeCodingOptions::default()),
        )
        .expect("construct");
        assert_eq!(adapter.engine(), CodingEngineKind::ClaudeCode);
    }
}

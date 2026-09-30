use std::{
    collections::HashMap,
    path::PathBuf,
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use semver::Version;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
    sync::{mpsc, oneshot, Mutex},
    task::JoinHandle,
    time::timeout,
};
use uuid::Uuid;

use super::{
    budgets::{
        record_termination_reason, CodingTerminated, CodingTerminationReason, ProgressWatchdog,
    },
    coding_event_from_raw, CodingContinuationRef, CodingEngineAdapter, CodingEngineEvent,
    CodingEngineEventKind, CodingEngineEventSink, CodingEngineKind, CodingEngineRequest,
    CodingEngineRunResult, CodingLiveSessionReporter, CodingSessionStats,
};

const STDERR_CAPTURE_LIMIT: usize = 128 * 1024;
const PI_RPC_REQUIRED_VERSION: &str = "0.87.1";
const PI_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// The event that authoritatively closes a Pi prompt. This is deliberately a
/// protocol value rather than an `AgentEnd` special case: Pi (0.83 onward) can emit one
/// or more `agent_end` events before an automatic retry, compaction retry, or
/// queued continuation, and only `agent_settled` means session-level idleness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PiTurnBoundary {
    AgentSettled,
}

impl PiTurnBoundary {
    fn is_terminal(self, event: &CodingEngineEvent) -> bool {
        match self {
            Self::AgentSettled => matches!(event.kind, CodingEngineEventKind::AgentSettled),
        }
    }

    fn event_name(self) -> &'static str {
        match self {
            Self::AgentSettled => "agent_settled",
        }
    }
}

fn parse_pi_version_output(output: &str) -> Result<Version> {
    output
        .split_whitespace()
        .filter_map(|token| {
            let token = token
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric()
                        && character != '.'
                        && character != '-'
                        && character != '+'
                })
                .trim_start_matches('v');
            Version::parse(token).ok()
        })
        .next()
        .ok_or_else(|| anyhow!("could not parse Pi version from `{}`", output.trim()))
}

fn turn_boundary_for_pi_version(version: &Version) -> Result<PiTurnBoundary> {
    let required = Version::parse(PI_RPC_REQUIRED_VERSION)
        .expect("PI_RPC_REQUIRED_VERSION must be valid semver");
    if version == &required {
        return Ok(PiTurnBoundary::AgentSettled);
    }
    Err(anyhow!(
        "unsupported Pi coding agent version {version}; Magician requires exactly \
         {PI_RPC_REQUIRED_VERSION} because its RPC completion boundary is `agent_settled`. Run \
         `make setup-pi-coding-agent` to install the reviewed version"
    ))
}

async fn negotiate_pi_turn_boundary(
    binary: &std::path::Path,
    env: &std::collections::BTreeMap<String, String>,
    plane_harness: bool,
) -> Result<PiTurnBoundary> {
    let mut command = Command::new(binary);
    if plane_harness {
        command.env_clear();
    }
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in env {
        command.env(key, value);
    }
    let output = timeout(PI_VERSION_PROBE_TIMEOUT, command.output())
        .await
        .with_context(|| {
            format!(
                "Pi version probe timed out after {}s for `{}`",
                PI_VERSION_PROBE_TIMEOUT.as_secs(),
                binary.display()
            )
        })?
        .with_context(|| format!("run Pi version probe `{}`", binary.display()))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "Pi version probe `{}` failed with {}: {}",
            binary.display(),
            output.status,
            stderr.trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let version_output = if stdout.trim().is_empty() {
        stderr.as_ref()
    } else {
        stdout.as_ref()
    };
    let version = parse_pi_version_output(version_output)?;
    let boundary = turn_boundary_for_pi_version(&version)?;
    tracing::debug!(
        target: "coding_engine::pi",
        version = %version,
        boundary = boundary.event_name(),
        "negotiated Pi RPC completion boundary"
    );
    Ok(boundary)
}

#[derive(Debug, Clone)]
pub struct PiCodingEngineAdapter {
    binary: PathBuf,
    no_session_by_default: bool,
}

impl Default for PiCodingEngineAdapter {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("pi"),
            no_session_by_default: true,
        }
    }
}

impl PiCodingEngineAdapter {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            ..Self::default()
        }
    }

    pub fn from_options(options: super::factory::PiCodingOptions) -> Self {
        Self::new(options.binary)
    }

    pub fn with_session_persistence(mut self) -> Self {
        self.no_session_by_default = false;
        self
    }

    pub fn command_args(&self, request: &CodingEngineRequest) -> Vec<String> {
        let mut args = vec!["--mode".to_string(), "rpc".to_string()];
        if request.pi.plane_harness {
            args.extend(
                [
                    "--no-builtin-tools",
                    "--no-extensions",
                    "--no-skills",
                    "--no-prompt-templates",
                    "--no-context-files",
                    "--no-themes",
                ]
                .into_iter()
                .map(str::to_string),
            );
            if let Some(path) = &request.pi.system_prompt_path {
                args.push("--system-prompt".to_string());
                args.push(path.display().to_string());
            }
        }
        if self.no_session_by_default && request.pi.session_dir.is_none() {
            args.push("--no-session".to_string());
        }
        if let Some(session_dir) = &request.pi.session_dir {
            args.push("--session-dir".to_string());
            args.push(session_dir.display().to_string());
        }
        if let Some(name) = request
            .pi
            .session_name
            .as_deref()
            .filter(|name| !name.is_empty())
        {
            args.push("--name".to_string());
            args.push(name.to_string());
        }
        // Cold rehydrate. A SPECIFIC session (checkpoint rewind) takes precedence over
        // "most recent"; both are spawn flags (`--session <id>` / `--continue`
        // — neither has an RPC command, contract A.7).
        if request.pi.session_dir.is_some() {
            if let Some(session_id) = request
                .pi
                .resume_session_id
                .as_deref()
                .filter(|id| !id.is_empty())
            {
                args.push("--session".to_string());
                args.push(session_id.to_string());
            } else if request.pi.resume_recent {
                args.push("--continue".to_string());
            }
        }
        if let Some(provider) = request
            .pi
            .provider
            .as_deref()
            .filter(|provider| !provider.is_empty())
        {
            args.push("--provider".to_string());
            args.push(provider.to_string());
        }
        if let Some(model) = request
            .pi
            .model
            .as_deref()
            .filter(|model| !model.is_empty())
        {
            args.push("--model".to_string());
            args.push(model.to_string());
        }
        // Load extra Pi extensions (the bundled Magician Citizen extension, M6),
        // so Magician-native tools are callable in Pi's loop.
        for path in &request.pi.extension_paths {
            args.push("--extension".to_string());
            args.push(path.display().to_string());
        }
        // P1 — layer the executing agent's persona onto Pi's system prompt
        // (`--append-system-prompt <path>`, repeatable; Pi reads each as a file and
        // appends it, PRESERVING its built-in coding base prompt — unlike
        // `--system-prompt`). The handler writes these files outside the shadow
        // tree so they never enter the diff.
        for path in &request.pi.append_system_prompt {
            args.push("--append-system-prompt".to_string());
            args.push(path.display().to_string());
        }
        args
    }

    /// Open a live, warm `pi --mode rpc` session for interactive / multi-turn
    /// use. The caller drives turns via the returned [`PiSession`] + its
    /// [`PiSessionHandle`].
    pub async fn open_session(&self, request: &CodingEngineRequest) -> Result<PiSession> {
        PiSession::spawn(self, request).await
    }
}

/// Fetch Pi's cumulative session stats with a short bound, so a wedged or
/// aborted session can't hang turn teardown. `None` on error / timeout.
async fn bounded_session_stats(control: &PiSessionHandle) -> Option<CodingSessionStats> {
    timeout(Duration::from_secs(5), control.get_session_stats())
        .await
        .ok()
        .and_then(Result::ok)
}

/// Pi's `sessionId`, asked for while the turn is still running.
///
/// `get_state` is the same id-correlated RPC the end of the turn already uses,
/// on the same warm handle as `set_thinking_level`; the single persistent
/// reader demultiplexes responses from the id-less event stream, and
/// `PiSession::handle` documents that the command side may be driven
/// concurrently with `collect_until_idle`. So asking mid-stream is the
/// supported shape rather than a race.
///
/// Bounded exactly like [`bounded_session_stats`], and for the same reason: a
/// wedged session must not get to hold the turn open by not answering. `None`
/// is best-effort — the turn proceeds, and the settled write still lands.
async fn bounded_session_id(control: &PiSessionHandle) -> Option<String> {
    timeout(Duration::from_secs(5), control.get_state())
        .await
        .ok()
        .and_then(Result::ok)
        .and_then(|state| {
            state
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .filter(|id| !id.is_empty())
}

/// Record THIS turn's incremental usage (cumulative `after` − `before`) into
/// the request's `usage_capture` out-cell for the /llm analytics bridge. No-op
/// when the caller didn't ask for the bridge or no `after` sample exists.
/// Resume-safety: if `before` couldn't be sampled on a RESUMED session, `after`
/// is cumulative incl. prior turns — skip rather than double-count.
fn write_turn_usage(
    request: &CodingEngineRequest,
    before: &Option<CodingSessionStats>,
    after: &Option<CodingSessionStats>,
    success: bool,
) {
    let Some(cell) = request.usage_capture.as_ref() else {
        return;
    };
    let Some(after) = after.as_ref() else {
        return;
    };
    let resuming = request.pi.is_resuming();
    if before.is_none() && resuming {
        return;
    }
    let before = before.as_ref();
    let delta = super::CodingTurnUsage {
        cost: (after.cost - before.map(|s| s.cost).unwrap_or(0.0)).max(0.0),
        input: after
            .tokens
            .input
            .saturating_sub(before.map(|s| s.tokens.input).unwrap_or(0)),
        output: after
            .tokens
            .output
            .saturating_sub(before.map(|s| s.tokens.output).unwrap_or(0)),
        cache_read: after
            .tokens
            .cache_read
            .saturating_sub(before.map(|s| s.tokens.cache_read).unwrap_or(0)),
        cache_write: after
            .tokens
            .cache_write
            .saturating_sub(before.map(|s| s.tokens.cache_write).unwrap_or(0)),
        success,
        cost_known: true,
    };
    if let Ok(mut slot) = cell.lock() {
        *slot = Some(delta);
    }
}

/// File the typed cause under this run's execution id, first writer wins.
///
/// The executor's deadline watchdog runs in a spawned task and can only reach
/// the turn through the cancellation token, so the reason travels beside the
/// token rather than inside it. A cause filed there already — a parent
/// deadline, a task budget — is the one that actually fired and is never
/// overwritten.
fn file_termination_reason(request: &CodingEngineRequest, reason: CodingTerminationReason) {
    if let Some(key) = request.termination_key.as_deref() {
        record_termination_reason(key, reason);
    }
}

/// Publish the declared provider backoff this turn slept through, so the task
/// ledger can exclude it from active work.
fn write_turn_backoff(request: &CodingEngineRequest, watchdog: Option<&ProgressWatchdog>) {
    let (Some(cell), Some(watchdog)) = (request.backoff_capture.as_ref(), watchdog) else {
        return;
    };
    if let Ok(mut slot) = cell.lock() {
        *slot = watchdog.backoff_total();
    }
}

#[async_trait]
impl CodingEngineAdapter for PiCodingEngineAdapter {
    fn engine(&self) -> CodingEngineKind {
        CodingEngineKind::Pi
    }

    async fn run_turn(&self, request: CodingEngineRequest) -> Result<CodingEngineRunResult> {
        if request.prompt.trim().is_empty() {
            return Err(anyhow!("Pi coding request prompt is empty"));
        }
        if !request.shadow_workspace_root.is_dir() {
            return Err(anyhow!(
                "Pi shadow workspace does not exist: {}",
                request.shadow_workspace_root.display()
            ));
        }

        let mut session = PiSession::spawn(self, &request).await?;
        let control = session.handle();

        // Expose this live turn to the interactive control plane (cockpit
        // Stop / steer) for exactly its lifetime — registered now, dropped the
        // moment the turn settles below, so a stale handle is never steerable.
        let control_keys = request.control_keys.clone();
        if !control_keys.is_empty() {
            super::control::coding_control_registry()
                .register(
                    &control_keys,
                    super::control::CodingControlHandle::Pi(control.clone()),
                )
                .await;
        }

        // One-shot turn over the warm session: prompt -> drain to idle -> pull
        // the assistant text + state + cumulative cost/token stats. The single
        // persistent reader demultiplexes the id-correlated responses (these
        // `control.*` calls) from the id-less event stream (`collect_until_idle`),
        // so a late event can no longer be swallowed by a state drain.
        // /llm bridge: sample cumulative session stats BEFORE the turn so the caller
        // records THIS turn's delta (after − before), never the cumulative —
        // chained/resumed runs would otherwise double-count the session prefix
        // on every turn.
        let stats_before = bounded_session_stats(&control).await;
        let live_reporter = CodingLiveSessionReporter::from_request(&request);
        // Phase-aware no-progress detection (§4). Armed whenever the caller
        // resolved a budget set; `None` leaves the turn bounded only by its
        // wall clock, which is both the kill-switch path and what every caller
        // that predates the detector gets.
        let mut watchdog = request
            .budgets
            .filter(|budgets| budgets.no_progress_enabled)
            .map(|budgets| ProgressWatchdog::new(budgets, Instant::now()));
        let turn_future = timeout(request.timeout, async {
            // Apply the coding profile's reasoning effort to the warm session
            // before the turn (so the profile's `rhigh` actually drives Pi, which
            // otherwise defaults to `medium`). Best-effort: a model that doesn't
            // support the level keeps Pi's default rather than aborting the turn.
            if let Some(level) = request
                .pi
                .thinking_level
                .as_deref()
                .filter(|level| !level.is_empty())
            {
                if let Err(err) = control.set_thinking_level(level).await {
                    tracing::warn!(
                        target: "coding_engine",
                        error = %err,
                        level,
                        "set_thinking_level failed; continuing at Pi's default thinking level"
                    );
                }
            }
            // Attach images via the RPC `images[]` field — the only channel a
            // vision model can actually SEE (a workspace file path in the prompt
            // text does not convey pixels).
            let images = (!request.pi.images.is_empty()).then(|| request.pi.images.clone());
            control.prompt(&request.prompt, images, None).await?;
            // The session id on disk BEFORE the drain, which is the whole turn.
            //
            // Asked after the prompt is acknowledged rather than after
            // `PiSession::spawn`, because the ack is the first moment Pi has
            // certainly established the session this turn runs on — a
            // `get_state` before it can answer for a session that is not yet
            // the one the prompt lands in, and a mid-turn resume that spoke to
            // the wrong session would be worse than one that could not resume
            // at all. The ack costs milliseconds; the drain is the hours.
            if let Some(reporter) = live_reporter.as_ref() {
                if let Some(id) = bounded_session_id(&control).await {
                    reporter.report_live_session(CodingEngineKind::Pi, &id);
                }
            }
            let events = session.collect_until_idle(watchdog.as_mut()).await?;
            let assistant_text = control.get_last_assistant_text().await?;
            let state = control.get_state().await.ok();
            let session_stats = control.get_session_stats().await.ok();
            Ok::<_, anyhow::Error>((events, assistant_text, state, session_stats))
        });

        // B2 — race the turn against an optional external cancel token so a parent
        // deadline / Stop aborts a hung turn promptly instead of waiting out
        // request.timeout. The control-registry unregister below still runs, so a
        // cancelled turn is unregistered too. `None` token = today's plain await.
        let outcome = match request.cancel_token.clone() {
            Some(cancel) => {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    settled = turn_future => Some(settled),
                }
            },
            None => Some(turn_future.await),
        };

        // Turn has settled — drop it from the control plane before teardown.
        if !control_keys.is_empty() {
            super::control::coding_control_registry()
                .unregister(&control_keys)
                .await;
        }

        // Declared provider backoff is not active work. Capture it on EVERY
        // terminal path, including the bad ones — a turn that timed out after a
        // long rate-limit wait must not charge that wait to the task budget.
        write_turn_backoff(&request, watchdog.as_ref());

        let (events, assistant_text, state, session_stats) = match outcome {
            Some(Ok(Ok(value))) => value,
            Some(Ok(Err(err))) => {
                // Failure still spent tokens — sample the cumulative (bounded so a wedged
                // session can't hang teardown) and record this turn's delta for
                // the /llm bridge.
                let after = bounded_session_stats(&control).await;
                write_turn_usage(&request, &stats_before, &after, false);
                let stderr = session.shutdown_graceful().await;
                // The no-progress detector reports a typed cause rather than a
                // message; file it so the caller can tell a hang from a crash.
                if let Some(terminated) = err.downcast_ref::<CodingTerminated>() {
                    let reason = terminated.reason.clone();
                    file_termination_reason(&request, reason.clone());
                    return Err(anyhow!(
                        "Pi run stopped: {}; stderr: {}",
                        reason.describe(),
                        stderr.trim()
                    ));
                }
                return Err(err.context(format!("Pi RPC failed; stderr: {}", stderr.trim())));
            },
            Some(Err(_)) => {
                let after = bounded_session_stats(&control).await;
                write_turn_usage(&request, &stats_before, &after, false);
                let stderr = session.shutdown_graceful().await;
                file_termination_reason(
                    &request,
                    CodingTerminationReason::TurnTimeout {
                        limit_secs: request.timeout.as_secs(),
                    },
                );
                return Err(anyhow!(
                    "Pi RPC timed out after {}s; stderr: {}",
                    request.timeout.as_secs(),
                    stderr.trim()
                ));
            },
            None => {
                let after = bounded_session_stats(&control).await;
                write_turn_usage(&request, &stats_before, &after, false);
                let stderr = session.shutdown_graceful().await;
                // A token cannot explain why it fired. Whoever cancelled it
                // filed the reason first; an unexplained cancellation is the
                // operator pressing Stop, which is the only cause that reaches
                // the token without passing through a deadline.
                let reason = request
                    .termination_key
                    .as_deref()
                    .and_then(super::budgets::termination_reason)
                    .unwrap_or(CodingTerminationReason::OwnerCancelled);
                return Err(anyhow!(
                    "Pi run stopped: {}; stderr: {}",
                    reason.describe(),
                    stderr.trim()
                ));
            },
        };
        // Success — record THIS turn's usage delta (after − before) for the /llm
        // bridge.
        write_turn_usage(&request, &stats_before, &session_stats, true);

        let stderr = session.shutdown_graceful().await;
        if events.is_empty() && !stderr.trim().is_empty() {
            tracing::debug!("Pi RPC stderr with no events: {}", stderr.trim());
        }

        let event_count = events.len() as u64;
        let streamed_text: String = events
            .iter()
            .filter_map(|event| event.text_delta.as_deref())
            .collect();
        let assistant_text =
            assistant_text.or_else(|| (!streamed_text.is_empty()).then_some(streamed_text));

        let session_id = state
            .as_ref()
            .and_then(|value| value.get("sessionId"))
            .and_then(Value::as_str)
            .map(str::to_string);
        let session_file = state
            .as_ref()
            .and_then(|value| value.get("sessionFile"))
            .and_then(Value::as_str)
            .map(PathBuf::from);
        let continuation = session_id.as_deref().filter(|id| !id.is_empty()).map(|id| {
            CodingContinuationRef::for_pi_session(
                id,
                &request.scope_root,
                &request.workspace_root,
                request.run_task_id.as_deref(),
            )
        });

        let result = CodingEngineRunResult {
            engine: CodingEngineKind::Pi,
            session_id,
            session_file,
            assistant_text,
            event_count,
            continuation,
            continuation_fresh_reason: None,
            proposal: None,
            approval_payload: None,
            session_stats,
        };

        Ok(result)
    }
}

/// How a prompt sent while the assistant is mid-stream should be delivered.
/// Pi rejects a plain `prompt` during streaming unless one of these is set
/// (contract A.5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamingBehavior {
    Steer,
    FollowUp,
}

impl StreamingBehavior {
    fn as_str(self) -> &'static str {
        match self {
            // Pi's `prompt.streamingBehavior` uses camelCase `followUp`.
            Self::Steer => "steer",
            Self::FollowUp => "followUp",
        }
    }
}

/// Cloneable command side of a live Pi session. Sends RPC commands over the
/// shared stdin and awaits each command's id-correlated response. Because it is
/// `Clone` and `&self`, a control task can `steer`/`abort` while another task
/// is draining the event stream — the foundation for interactive coding.
#[derive(Clone)]
pub struct PiSessionHandle {
    stdin: Arc<Mutex<ChildStdin>>,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
}

impl PiSessionHandle {
    /// Send a command (stamping a unique `id`) and await the matching
    /// `{type:"response", id}` (contract A.6). Events (which carry no `id`) are
    /// routed elsewhere by the reader and never block this call.
    async fn request(&self, label: &str, mut command: Value) -> Result<Value> {
        let id = format!("magician-{label}-{}", Uuid::new_v4());
        if let Some(object) = command.as_object_mut() {
            object.insert("id".to_string(), json!(id));
        }
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        {
            let mut stdin = self.stdin.lock().await;
            if let Err(err) = send_rpc(&mut stdin, command).await {
                self.pending.lock().await.remove(&id);
                return Err(err);
            }
        }
        let response = rx
            .await
            .map_err(|_| anyhow!("Pi RPC reader closed before `{label}` response"))?;
        ensure_success_response(&response, label)?;
        Ok(response)
    }

    fn message_command(kind: &str, message: &str, images: Option<Vec<Value>>) -> Value {
        let mut command = json!({ "type": kind, "message": message });
        if let Some(images) = images {
            command["images"] = json!(images);
        }
        command
    }

    /// Start a fresh turn. Returns once Pi accepts the prompt (the preflight
    /// response); turn completion is Pi's `agent_settled` event, not
    /// this response or an intermediate `agent_end` (contract A.6) — drain
    /// it via [`PiSession::collect_until_idle`].
    pub async fn prompt(
        &self,
        message: &str,
        images: Option<Vec<Value>>,
        streaming: Option<StreamingBehavior>,
    ) -> Result<()> {
        let mut command = Self::message_command("prompt", message, images);
        if let Some(streaming) = streaming {
            command["streamingBehavior"] = json!(streaming.as_str());
        }
        self.request("prompt", command).await.map(|_| ())
    }

    /// Redirect the in-flight turn — delivered after the current tool calls,
    /// before the next model call (contract A.5).
    pub async fn steer(&self, message: &str, images: Option<Vec<Value>>) -> Result<()> {
        self.request("steer", Self::message_command("steer", message, images))
            .await
            .map(|_| ())
    }

    /// Queue a message delivered only once the agent fully stops (contract
    /// A.5).
    pub async fn follow_up(&self, message: &str, images: Option<Vec<Value>>) -> Result<()> {
        self.request(
            "follow_up",
            Self::message_command("follow_up", message, images),
        )
        .await
        .map(|_| ())
    }

    /// Cancel the current operation (`session.abort()`).
    pub async fn abort(&self) -> Result<()> {
        self.request("abort", json!({ "type": "abort" }))
            .await
            .map(|_| ())
    }

    /// Set the reasoning/thinking level for models that support it (contract
    /// A.5 `set_thinking_level`; levels:
    /// `off|minimal|low|medium|high|xhigh|max`). Sent on the warm session
    /// before a turn so the coding profile's reasoning effort
    /// actually drives Pi (Pi otherwise defaults to `medium`). Best-effort at
    /// the call site: a model that doesn't support the level fails this,
    /// not the turn.
    pub async fn set_thinking_level(&self, level: &str) -> Result<()> {
        self.request(
            "set_thinking_level",
            json!({ "type": "set_thinking_level", "level": level }),
        )
        .await
        .map(|_| ())
    }

    pub async fn get_last_assistant_text(&self) -> Result<Option<String>> {
        let response = self
            .request(
                "get_last_assistant_text",
                json!({ "type": "get_last_assistant_text" }),
            )
            .await?;
        Ok(response
            .pointer("/data/text")
            .and_then(Value::as_str)
            .map(str::to_string))
    }

    pub async fn get_state(&self) -> Result<Value> {
        let response = self
            .request("get_state", json!({ "type": "get_state" }))
            .await?;
        Ok(response.get("data").cloned().unwrap_or(Value::Null))
    }

    /// Cumulative cost + token + context telemetry (contract A.6
    /// `SessionStats`).
    pub async fn get_session_stats(&self) -> Result<CodingSessionStats> {
        let response = self
            .request("get_session_stats", json!({ "type": "get_session_stats" }))
            .await?;
        let data = response.get("data").cloned().unwrap_or(Value::Null);
        serde_json::from_value(data).context("parse Pi get_session_stats data")
    }
}

/// A live `pi --mode rpc` process with a single persistent stdout reader. The
/// loop never returns (contract A.1), so one session serves many sequential
/// turns. The reader demultiplexes responses (routed to [`PiSessionHandle`])
/// from the id-less event stream (emitted live to the sink and buffered for
/// [`Self::collect_until_idle`]).
pub struct PiSession {
    handle: PiSessionHandle,
    events: mpsc::UnboundedReceiver<CodingEngineEvent>,
    turn_boundary: PiTurnBoundary,
    child: Child,
    reader_task: JoinHandle<()>,
    stderr_task: Option<JoinHandle<String>>,
}

impl PiSession {
    async fn spawn(adapter: &PiCodingEngineAdapter, request: &CodingEngineRequest) -> Result<Self> {
        let working_dir = request
            .working_dir
            .as_deref()
            .unwrap_or(request.shadow_workspace_root.as_path());
        if !working_dir.is_dir() {
            return Err(anyhow!(
                "Pi working directory does not exist: {}",
                working_dir.display()
            ));
        }

        // Fail before opening an RPC process when the executable does not match
        // the reviewed wire contract. Without this gate a legacy Pi would never
        // emit `agent_settled`, while treating `agent_end` as a fallback on a new
        // Pi would reintroduce premature completion during retries/compaction.
        let turn_boundary = negotiate_pi_turn_boundary(
            adapter.binary.as_path(),
            &request.env,
            request.pi.plane_harness,
        )
        .await?;

        // Wrapped in the OS-sandbox gate when enabled (live repo read-only);
        // identical to `Command::new(binary).args(..)` when the gate is off.
        let pi_args: Vec<std::ffi::OsString> = adapter
            .command_args(request)
            .into_iter()
            .map(std::ffi::OsString::from)
            .collect();
        let mut command = super::os_sandbox_command(adapter.binary.as_os_str(), &pi_args);
        command.current_dir(working_dir);
        command.stdin(Stdio::piped());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        if request.pi.plane_harness {
            command.env_clear();
        }
        for (key, value) in &request.env {
            command.env(key, value);
        }
        // Isolation: ceiling git's upward `.git` search at the scope root so a Pi
        // subprocess cannot discover (and operate on) the live repo's `.git`
        // above the shadow workspace. The shadow is nested under scope_root, so
        // this confines git to the shadow's own tree. Set after the request env
        // so the fence value is authoritative.
        command.env("GIT_CEILING_DIRECTORIES", &request.scope_root);

        let mut child = command.spawn().with_context(|| {
            format!(
                "spawn Pi RPC process `{}` in {}",
                adapter.binary.display(),
                working_dir.display()
            )
        })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| anyhow!("Pi RPC child stdin was not piped"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("Pi RPC child stdout was not piped"))?;
        let stderr = child.stderr.take();
        let stderr_task = stderr.map(|stderr| tokio::spawn(read_stderr_capped(stderr)));

        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, events) = mpsc::unbounded_channel();
        let reader_task = tokio::spawn(reader_loop(
            stdout,
            pending.clone(),
            event_tx,
            request.event_sink.clone(),
        ));

        let mut session = Self {
            handle: PiSessionHandle {
                stdin: Arc::new(Mutex::new(stdin)),
                pending,
            },
            events,
            turn_boundary,
            child,
            reader_task,
            stderr_task,
        };
        if let Some(marker) = &request.pi.ready_marker {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !marker.is_file() {
                let cancelled = request
                    .cancel_token
                    .as_ref()
                    .is_some_and(|token| token.is_cancelled());
                let exited = session.child.try_wait().ok().flatten().is_some();
                if cancelled || exited || Instant::now() >= deadline {
                    let stderr = session.shutdown_graceful().await;
                    return Err(anyhow!(
                        "Pi Plane extension did not initialize: {}",
                        stderr.trim()
                    ));
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
        Ok(session)
    }

    /// Cloneable command side — drive prompts/steer/abort/stats from anywhere,
    /// including concurrently with [`Self::collect_until_idle`].
    pub fn handle(&self) -> PiSessionHandle {
        self.handle.clone()
    }

    /// Drain events until Pi's negotiated session-level boundary. For the
    /// reviewed 0.87.1 contract this is `agent_settled`; `agent_end` is always
    /// intermediate even when `willRetry` is false because a queued
    /// continuation or post-run compaction can still follow it. Returns
    /// every observed event.
    pub async fn collect_until_idle(
        &mut self,
        watchdog: Option<&mut ProgressWatchdog>,
    ) -> Result<Vec<CodingEngineEvent>> {
        collect_events_until_boundary(&mut self.events, self.turn_boundary, watchdog).await
    }

    /// Close stdin (EOF → Pi `shutdown`, contract A.1), stop the reader, and
    /// reap the child. Closing the write half directly (rather than relying on
    /// dropping the `Arc<Mutex<ChildStdin>>`) guarantees EOF even if a handle
    /// clone is still alive. Returns captured stderr.
    pub async fn shutdown_graceful(self) -> String {
        let PiSession {
            handle,
            events: _events,
            turn_boundary: _turn_boundary,
            mut child,
            reader_task,
            stderr_task,
        } = self;
        {
            let mut stdin = handle.stdin.lock().await;
            let _ = stdin.shutdown().await;
        }
        terminate_child(&mut child).await;
        reader_task.abort();
        join_stderr(stderr_task).await
    }
}

/// Drain the event stream to the turn boundary, optionally under the
/// phase-aware no-progress detector.
///
/// The detector lives here rather than in a background task because this is the
/// only place that sees every event **and** owns the deadline: a watchdog that
/// cannot see the events cannot tell a sixteen-minute test from a wedged one.
/// When it fires, it returns a typed [`CodingTerminated`] so the caller
/// recovers the phase by downcast instead of parsing a message.
async fn collect_events_until_boundary(
    events_rx: &mut mpsc::UnboundedReceiver<CodingEngineEvent>,
    boundary: PiTurnBoundary,
    mut watchdog: Option<&mut ProgressWatchdog>,
) -> Result<Vec<CodingEngineEvent>> {
    let mut events = Vec::new();
    // ONE timer for the whole turn, polled on a coarse cadence — not a fresh
    // deadline per event. A run streams ~10k events; an exact per-event
    // `sleep_until` would register and tear down a timer for each of them, and
    // would embed a `Sleep` inline in this future's state machine. `Interval`
    // keeps its `Sleep` behind a pointer and survives being dropped un-polled
    // in the `select!`, so the cost here is independent of the event rate. The
    // bounds being guarded are minutes wide, so checking on a tick rather than
    // an exact instant costs nothing but at most one tick of lateness.
    let mut progress_checks = watchdog.as_deref().map(|detector| {
        let period = detector.check_interval();
        let mut ticker = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
        // A slow poll must not queue up a burst of catch-up ticks.
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticker
    });
    loop {
        let received = match progress_checks.as_mut() {
            Some(ticker) => {
                tokio::select! {
                    // Biased so a queued event always wins a tick landing in the
                    // same poll — never kill a turn that had already moved.
                    biased;
                    received = events_rx.recv() => received,
                    _ = ticker.tick() => {
                        match watchdog.as_deref().and_then(|detector| detector.expired(Instant::now())) {
                            Some(reason) => {
                                return Err(anyhow::Error::new(CodingTerminated { reason }))
                            },
                            None => continue,
                        }
                    },
                }
            },
            None => events_rx.recv().await,
        };
        match received {
            Some(event) => {
                let now = Instant::now();
                if let Some(detector) = watchdog.as_deref_mut() {
                    detector.observe(&event, now);
                }
                let idle = boundary.is_terminal(&event);
                events.push(event);
                if idle {
                    return Ok(events);
                }
                // Check here as well as on the tick. `biased` polls `recv`
                // first, so a stream that never goes quiet — precisely what a
                // wedged chatter loop produces — can starve the tick arm
                // indefinitely. One comparison per event closes that hole, and
                // it runs after the terminal check so a settling event always
                // wins.
                if let Some(reason) = watchdog
                    .as_deref()
                    .and_then(|detector| detector.expired(now))
                {
                    return Err(anyhow::Error::new(CodingTerminated { reason }));
                }
            },
            None => {
                return Err(anyhow!(
                    "Pi RPC event stream closed before {}",
                    boundary.event_name()
                ));
            },
        }
    }
}

/// The single persistent reader: one line at a time, route `type=="response"`
/// to the awaiting command by `id`, and turn every other (id-less) line into a
/// [`CodingEngineEvent`] emitted live to the sink and buffered for the drainer.
async fn reader_loop(
    stdout: ChildStdout,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    event_tx: mpsc::UnboundedSender<CodingEngineEvent>,
    event_sink: Option<CodingEngineEventSink>,
) {
    let mut reader = BufReader::new(stdout);
    let mut sequence = 0usize;
    loop {
        let value = match read_jsonl_value(&mut reader).await {
            Ok(value) => value,
            Err(_) => break, // stdout closed / process gone
        };
        if value.get("type").and_then(Value::as_str) == Some("response") {
            if let Some(id) = value.get("id").and_then(Value::as_str).map(str::to_string) {
                if let Some(tx) = pending.lock().await.remove(&id) {
                    let _ = tx.send(value);
                    continue;
                }
            }
            continue; // response with no/unknown id — nothing is awaiting it
        }
        sequence += 1;
        let event = coding_event_from_raw(sequence, value);
        // Tee to magician.log (target `coding_engine::pi`) BEFORE the sink, so a
        // run is debuggable from the log alone — independent of whether a cockpit
        // event sink is attached (batch / non-VibeDev turns have no sink, so they
        // otherwise produce zero observability).
        log_pi_event(&event);
        if let Some(sink) = event_sink.as_ref() {
            sink(&event);
        }
        // If the drainer is gone we keep reading to EOF so the child still
        // shuts down cleanly; events simply stop being buffered.
        let _ = event_tx.send(event);
    }
}

/// Emit every parsed Pi event to the tracing log, leveled by kind so the
/// default (`info`) stays readable while high-volume streaming deltas drop to
/// `debug`/ `trace`. Deliberately does NOT log `event.raw` or tool arg/result
/// bodies — those can carry file contents / secrets (the cockpit rail redacts
/// them separately); we log only structural metadata + a short trimmed delta.
fn log_pi_event(event: &CodingEngineEvent) {
    use CodingEngineEventKind::*;
    let is_failure = event.error_message.is_some()
        || event.stop_reason.as_deref() == Some("error")
        || event.tool_result_is_error == Some(true)
        || matches!(event.kind, ExtensionError);
    if is_failure {
        tracing::warn!(
            target: "coding_engine::pi",
            seq = event.sequence,
            kind = ?event.kind,
            tool = event.tool_name.as_deref().unwrap_or(""),
            stop_reason = event.stop_reason.as_deref().unwrap_or(""),
            error = event.error_message.as_deref().unwrap_or(""),
            "pi event (failure)"
        );
        return;
    }
    match event.kind {
        MessageUpdate => {
            let delta = event
                .text_delta
                .as_deref()
                .or(event.thinking_delta.as_deref())
                .unwrap_or("");
            let trimmed: String = delta.chars().take(200).collect();
            tracing::debug!(
                target: "coding_engine::pi",
                seq = event.sequence,
                thinking = event.thinking_delta.is_some(),
                delta = %trimmed,
                "pi message delta"
            );
        },
        Response | MessageStart | QueueUpdate | Unknown => {
            tracing::trace!(
                target: "coding_engine::pi",
                seq = event.sequence,
                kind = ?event.kind,
                "pi event"
            );
        },
        _ => {
            tracing::info!(
                target: "coding_engine::pi",
                seq = event.sequence,
                kind = ?event.kind,
                tool = event.tool_name.as_deref().unwrap_or(""),
                stop_reason = event.stop_reason.as_deref().unwrap_or(""),
                cost_total = event.cost_total.unwrap_or(0.0),
                "pi event"
            );
        },
    }
}

async fn send_rpc(stdin: &mut ChildStdin, value: Value) -> Result<()> {
    let mut line = serde_json::to_vec(&value).context("serialize Pi RPC command")?;
    line.push(b'\n');
    stdin
        .write_all(&line)
        .await
        .context("write Pi RPC command")?;
    stdin.flush().await.context("flush Pi RPC command")
}

async fn read_jsonl_value(reader: &mut BufReader<ChildStdout>) -> Result<Value> {
    let mut buf = Vec::new();
    let read = reader
        .read_until(b'\n', &mut buf)
        .await
        .context("read Pi RPC JSONL line")?;
    if read == 0 {
        return Err(anyhow!("Pi RPC stdout closed before expected response"));
    }
    if buf.ends_with(b"\n") {
        buf.pop();
    }
    if buf.ends_with(b"\r") {
        buf.pop();
    }
    serde_json::from_slice(&buf).with_context(|| {
        format!(
            "parse Pi RPC JSONL line `{}`",
            String::from_utf8_lossy(&buf)
        )
    })
}

fn ensure_success_response(value: &Value, command: &str) -> Result<()> {
    let success = value
        .get("success")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);
    if success {
        return Ok(());
    }
    let error = value
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown Pi RPC error");
    Err(anyhow!("{command} rejected by Pi RPC: {error}"))
}

async fn read_stderr_capped(stderr: tokio::process::ChildStderr) -> String {
    let mut reader = BufReader::new(stderr);
    let mut out = Vec::new();
    loop {
        let mut buf = Vec::new();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) => break,
            Ok(_) => {
                let remaining = STDERR_CAPTURE_LIMIT.saturating_sub(out.len());
                if remaining == 0 {
                    break;
                }
                if buf.len() <= remaining {
                    out.extend(buf);
                } else {
                    out.extend(&buf[..remaining]);
                    break;
                }
            },
            Err(err) => {
                tracing::warn!("error reading Pi RPC stderr: {err}");
                break;
            },
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

async fn join_stderr(task: Option<JoinHandle<String>>) -> String {
    match task {
        Some(task) => task.await.unwrap_or_default(),
        None => String::new(),
    }
}

async fn terminate_child(child: &mut Child) {
    match timeout(Duration::from_millis(500), child.wait()).await {
        Ok(Ok(_status)) => {},
        _ => {
            let _ = child.start_kill();
            let _ = timeout(Duration::from_secs(2), child.wait()).await;
        },
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

    fn pi_event(sequence: usize, raw: Value) -> CodingEngineEvent {
        coding_event_from_raw(sequence, raw)
    }

    async fn collect_fixture(raw_events: Vec<Value>) -> Result<Vec<CodingEngineEvent>> {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        for (index, raw) in raw_events.into_iter().enumerate() {
            event_tx
                .send(pi_event(index + 1, raw))
                .expect("fixture receiver remains open");
        }
        drop(event_tx);
        collect_events_until_boundary(&mut event_rx, PiTurnBoundary::AgentSettled, None).await
    }

    fn request() -> CodingEngineRequest {
        CodingEngineRequest::new(
            "fix it",
            "/tmp/real",
            "/tmp/shadow",
            "/tmp/scope",
            TransactionScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
        )
    }

    /// Budgets tight enough to fire inside a test, with the phase shape intact.
    fn quick_budgets() -> super::super::budgets::ResolvedCodingBudgets {
        let mut budgets = super::super::budgets::ResolvedCodingBudgets::default();
        budgets.model_idle = Duration::from_millis(60);
        budgets.tool_idle = Duration::from_millis(60);
        budgets.tool_max = Duration::from_millis(400);
        budgets.compaction_max = Duration::from_millis(60);
        budgets.summarization_max = Duration::from_millis(60);
        budgets.retry_grace = Duration::from_millis(20);
        budgets
    }

    #[tokio::test]
    async fn the_watchdog_stops_a_stalled_tool_with_a_typed_cause() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        event_tx
            .send(pi_event(
                1,
                json!({ "type": "tool_execution_start", "toolName": "bash" }),
            ))
            .unwrap();
        // Sender stays alive: a closed stream is a DIFFERENT failure, and the
        // point of this test is that silence alone is enough.
        let mut watchdog =
            super::super::budgets::ProgressWatchdog::new(quick_budgets(), Instant::now());
        let error = collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            Some(&mut watchdog),
        )
        .await
        .expect_err("a silent tool past its bound must not hang the turn");

        let terminated = error
            .downcast_ref::<CodingTerminated>()
            .expect("the cause is typed, not a message to be parsed");
        assert!(
            matches!(
                terminated.reason,
                CodingTerminationReason::NoProgress {
                    phase: super::super::budgets::CodingProgressPhase::Tool,
                    ..
                }
            ),
            "{:?}",
            terminated.reason
        );
        drop(event_tx);
    }

    #[tokio::test]
    async fn the_watchdog_lets_a_healthy_turn_finish() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        for (index, raw) in [
            json!({ "type": "turn_start" }),
            json!({ "type": "tool_execution_start", "toolName": "bash" }),
            json!({ "type": "tool_execution_end", "toolName": "bash" }),
            json!({ "type": "agent_settled" }),
        ]
        .into_iter()
        .enumerate()
        {
            event_tx.send(pi_event(index + 1, raw)).unwrap();
        }
        let mut watchdog =
            super::super::budgets::ProgressWatchdog::new(quick_budgets(), Instant::now());
        let events = collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            Some(&mut watchdog),
        )
        .await
        .expect("a turn that keeps moving settles normally");
        assert_eq!(events.len(), 4);
        drop(event_tx);
    }

    #[tokio::test]
    async fn identical_chatter_does_not_hold_the_drain_loop_open() {
        // The stream never goes quiet; it just never says anything new. A
        // last-event-timestamp detector would wait here forever.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        // The feeder holds a CLONE. Keeping the original alive here means the
        // stream stays open after the feeder stops, so this test can only pass
        // by detecting a stall — never by the channel closing underneath it.
        let feed_tx = event_tx.clone();
        let feeder = tokio::spawn(async move {
            for sequence in 1..=200 {
                if feed_tx
                    .send(pi_event(
                        sequence,
                        json!({ "type": "tool_execution_update", "toolName": "bash" }),
                    ))
                    .is_err()
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        });
        let mut watchdog =
            super::super::budgets::ProgressWatchdog::new(quick_budgets(), Instant::now());
        let error = collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            Some(&mut watchdog),
        )
        .await
        .expect_err("identical updates must not keep a dead process alive");
        assert!(
            error.downcast_ref::<CodingTerminated>().is_some(),
            "expected a stall, not a closed stream: {error:#}"
        );
        feeder.abort();
        drop(event_tx);
    }

    #[tokio::test]
    async fn a_saturating_chatter_flood_cannot_starve_the_stall_check() {
        // `biased` polls `recv` first, so a stream that never goes quiet keeps
        // winning and the tick arm is never reached. A wedged loop emitting
        // buffered identical updates is exactly that shape.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut budgets = quick_budgets();
        budgets.model_idle = Duration::from_millis(10);
        let mut watchdog = super::super::budgets::ProgressWatchdog::new(budgets, Instant::now());
        // Let the model-idle bound lapse, then queue a burst with no gaps at
        // all — `recv` is Ready on every poll.
        tokio::time::sleep(Duration::from_millis(30)).await;
        for sequence in 1..=500 {
            event_tx
                .send(pi_event(sequence, json!({ "type": "queue_update" })))
                .unwrap();
        }

        let started = Instant::now();
        let error = collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            Some(&mut watchdog),
        )
        .await
        .expect_err("a stalled turn under a chatter flood must still be caught");
        assert!(error.downcast_ref::<CodingTerminated>().is_some());
        // The check ran per event rather than waiting out the tick (floored at
        // 100ms); had it waited, this would be ≥100ms.
        assert!(
            started.elapsed() < Duration::from_millis(80),
            "stall was detected only on the tick, not per event: {:?}",
            started.elapsed()
        );
        drop(event_tx);
    }

    #[tokio::test]
    async fn a_closed_stream_is_not_reported_as_a_hang() {
        // Pi died. That is a crash, not a stall, and conflating the two is what
        // the typed cause exists to prevent.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        event_tx
            .send(pi_event(1, json!({ "type": "turn_start" })))
            .unwrap();
        drop(event_tx);
        let mut watchdog =
            super::super::budgets::ProgressWatchdog::new(quick_budgets(), Instant::now());
        let error = collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            Some(&mut watchdog),
        )
        .await
        .expect_err("a closed stream still fails the turn");
        assert!(error.downcast_ref::<CodingTerminated>().is_none());
        assert!(format!("{error:#}").contains("closed before"));
    }

    #[tokio::test]
    async fn no_watchdog_leaves_the_drain_loop_unbounded() {
        // The kill-switch path: without budgets the loop waits exactly as it did
        // before the detector existed.
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        let mut collection = Box::pin(collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            None,
        ));
        assert!(
            timeout(Duration::from_millis(120), &mut collection)
                .await
                .is_err(),
            "an unwatched drain must not time itself out"
        );
        event_tx
            .send(pi_event(1, json!({ "type": "agent_settled" })))
            .unwrap();
        let events = timeout(Duration::from_secs(1), collection)
            .await
            .expect("releases once the boundary arrives")
            .unwrap();
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn a_disabled_detector_is_never_armed() {
        let mut budgets = super::super::budgets::ResolvedCodingBudgets::default();
        budgets.no_progress_enabled = false;
        let watchdog = super::super::budgets::ProgressWatchdog::new(budgets, Instant::now());
        assert_eq!(watchdog.deadline(), None);
    }

    #[test]
    fn command_args_default_to_rpc_no_session() {
        let adapter = PiCodingEngineAdapter::default();
        let args = adapter.command_args(&request());
        assert_eq!(args, vec!["--mode", "rpc", "--no-session"]);
    }

    #[test]
    fn plane_args_strip_native_resources_and_load_only_its_bridge() {
        let mut req = request();
        req.pi.plane_harness = true;
        req.pi.session_dir = Some(PathBuf::from("/private/sessions"));
        req.pi.system_prompt_path = Some(PathBuf::from("/private/system.md"));
        req.pi.extension_paths = vec![PathBuf::from("/private/bridge.js")];
        let args = PiCodingEngineAdapter::default().command_args(&req);
        for flag in [
            "--no-builtin-tools",
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-context-files",
            "--no-themes",
        ] {
            assert!(args.iter().any(|arg| arg == flag), "missing {flag}");
        }
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--system-prompt", "/private/system.md"]));
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--extension", "/private/bridge.js"]));
        assert!(!args.iter().any(|arg| arg == "--no-tools"));
    }

    #[test]
    fn command_args_include_provider_model_session_dir_and_name() {
        let adapter = PiCodingEngineAdapter::default();
        let mut req = request();
        req.pi.session_dir = Some(PathBuf::from("/tmp/pi-sessions"));
        req.pi.session_name = Some("task 123".to_string());
        req.pi.provider = Some("openai".to_string());
        req.pi.model = Some("openai/gpt-5".to_string());

        let args = adapter.command_args(&req);
        assert_eq!(
            args,
            vec![
                "--mode",
                "rpc",
                "--session-dir",
                "/tmp/pi-sessions",
                "--name",
                "task 123",
                "--provider",
                "openai",
                "--model",
                "openai/gpt-5"
            ]
        );
    }

    #[test]
    fn command_args_resume_recent_adds_continue_with_session_dir() {
        let adapter = PiCodingEngineAdapter::default();
        let mut req = request();
        req.pi.session_dir = Some(PathBuf::from("/tmp/pi-sessions"));
        req.pi.session_name = Some("vibedev-root".to_string());
        req.pi.resume_recent = true;

        let args = adapter.command_args(&req);
        assert_eq!(
            args,
            vec![
                "--mode",
                "rpc",
                "--session-dir",
                "/tmp/pi-sessions",
                "--name",
                "vibedev-root",
                "--continue",
            ]
        );
    }

    #[test]
    fn command_args_resume_recent_without_session_dir_is_ignored() {
        let adapter = PiCodingEngineAdapter::default();
        let mut req = request();
        req.pi.resume_recent = true;
        let args = adapter.command_args(&req);
        // No session-dir → nothing to continue; stays the default no-session shape.
        assert_eq!(args, vec!["--mode", "rpc", "--no-session"]);
    }

    #[test]
    fn command_args_include_extensions_and_append_prompts() {
        let adapter = PiCodingEngineAdapter::default();
        let mut req = request();
        req.pi.extension_paths = vec![PathBuf::from("/tmp/citizen.js")];
        req.pi.append_system_prompt = vec![PathBuf::from("/tmp/persona.md")];
        let args = adapter.command_args(&req);
        assert_eq!(
            args,
            vec![
                "--mode",
                "rpc",
                "--no-session",
                "--extension",
                "/tmp/citizen.js",
                "--append-system-prompt",
                "/tmp/persona.md",
            ]
        );
    }

    #[test]
    fn streaming_behavior_uses_pi_camel_case() {
        assert_eq!(StreamingBehavior::Steer.as_str(), "steer");
        assert_eq!(StreamingBehavior::FollowUp.as_str(), "followUp");
    }

    #[test]
    fn failed_response_reports_pi_error() {
        let value = json!({
            "type": "response",
            "command": "prompt",
            "success": false,
            "error": "not authenticated"
        });
        let err = ensure_success_response(&value, "prompt").unwrap_err();
        assert!(err.to_string().contains("not authenticated"));
    }

    #[test]
    fn parses_plain_and_decorated_pi_versions() {
        assert_eq!(
            parse_pi_version_output("0.83.0\n").unwrap(),
            Version::new(0, 83, 0)
        );
        assert_eq!(
            parse_pi_version_output("pi v0.83.0 (npm)\n").unwrap(),
            Version::new(0, 83, 0)
        );
        assert_eq!(
            parse_pi_version_output("pi (v0.83.0)\n").unwrap(),
            Version::new(0, 83, 0)
        );
        assert!(parse_pi_version_output("pi version unknown").is_err());
    }

    #[test]
    fn accepts_only_the_reviewed_pi_rpc_version() {
        assert_eq!(
            turn_boundary_for_pi_version(&Version::new(0, 87, 1)).unwrap(),
            PiTurnBoundary::AgentSettled
        );
        for unsupported in [
            Version::new(0, 79, 2),
            Version::new(0, 83, 0),
            Version::new(0, 87, 0),
            Version::new(0, 87, 2),
            Version::parse("0.87.1-beta.1").unwrap(),
        ] {
            let error = turn_boundary_for_pi_version(&unsupported).unwrap_err();
            assert!(error.to_string().contains(PI_RPC_REQUIRED_VERSION));
            assert!(error.to_string().contains("make setup-pi-coding-agent"));
        }
    }

    #[tokio::test]
    async fn agent_end_is_not_the_pi_terminal_boundary() {
        let events = collect_fixture(vec![
            json!({ "type": "agent_start" }),
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "agent_settled" }),
            // Anything beyond settlement belongs to a later operation and must
            // remain buffered rather than being attributed to this turn.
            json!({ "type": "agent_start" }),
        ])
        .await
        .unwrap();

        assert_eq!(events.len(), 3);
        assert!(matches!(events[1].kind, CodingEngineEventKind::AgentEnd));
        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
    }

    #[tokio::test]
    async fn collector_remains_pending_between_agent_end_and_agent_settled() {
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        event_tx
            .send(pi_event(
                1,
                json!({ "type": "agent_end", "willRetry": false }),
            ))
            .unwrap();

        let mut collection = Box::pin(collect_events_until_boundary(
            &mut event_rx,
            PiTurnBoundary::AgentSettled,
            None,
        ));
        assert!(
            timeout(Duration::from_millis(10), &mut collection)
                .await
                .is_err(),
            "agent_end must not make the turn collector ready"
        );

        event_tx
            .send(pi_event(2, json!({ "type": "agent_settled" })))
            .unwrap();
        let events = timeout(Duration::from_secs(1), collection)
            .await
            .expect("collector should release promptly after agent_settled")
            .unwrap();
        assert_eq!(events.len(), 2);
        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
    }

    #[tokio::test]
    async fn settled_boundary_waits_through_automatic_retry() {
        let events = collect_fixture(vec![
            json!({ "type": "agent_end", "willRetry": true }),
            json!({
                "type": "auto_retry_start",
                "attempt": 1,
                "maxAttempts": 3,
                "delayMs": 10,
                "errorMessage": "transient"
            }),
            json!({ "type": "auto_retry_end", "success": true, "attempt": 1 }),
            json!({ "type": "agent_start" }),
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "agent_settled" }),
        ])
        .await
        .unwrap();

        assert_eq!(events.len(), 6);
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event.kind, CodingEngineEventKind::AgentEnd))
                .count(),
            2
        );
        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
    }

    #[tokio::test]
    async fn settled_boundary_waits_through_compaction_retry_and_reentry() {
        let events = collect_fixture(vec![
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "compaction_start", "reason": "threshold" }),
            json!({
                "type": "summarization_retry_scheduled",
                "attempt": 1,
                "maxAttempts": 3,
                "delayMs": 10,
                "errorMessage": "temporary"
            }),
            json!({
                "type": "summarization_retry_attempt_start",
                "source": "compaction",
                "reason": "threshold"
            }),
            json!({ "type": "summarization_retry_finished" }),
            json!({
                "type": "compaction_end",
                "reason": "threshold",
                "aborted": false,
                "willRetry": true
            }),
            json!({ "type": "agent_start" }),
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "agent_settled" }),
        ])
        .await
        .unwrap();

        assert_eq!(events.len(), 9);
        assert!(events.iter().any(|event| matches!(
            event.kind,
            CodingEngineEventKind::SummarizationRetryScheduled
        )));
        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
    }

    #[tokio::test]
    async fn settled_boundary_waits_through_queued_follow_up() {
        let events = collect_fixture(vec![
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "queue_update", "steering": [], "followUp": ["continue"] }),
            json!({ "type": "agent_start" }),
            json!({ "type": "agent_end", "willRetry": false }),
            json!({ "type": "queue_update", "steering": [], "followUp": [] }),
            json!({ "type": "agent_settled" }),
        ])
        .await
        .unwrap();

        assert_eq!(events.len(), 6);
        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
    }

    #[tokio::test]
    async fn stream_close_before_agent_settled_fails_closed() {
        let error = collect_fixture(vec![
            json!({ "type": "agent_start" }),
            json!({ "type": "agent_end", "willRetry": false }),
        ])
        .await
        .unwrap_err();

        assert!(error.to_string().contains("before agent_settled"));
    }

    /// A real Pi 0.87.1 RPC session (image prompt → bash tool call → answer),
    /// recorded against `gpt-5.6-terra` by driving `pi --mode rpc` the way
    /// this adapter does. Pins the wire shapes the upgrade depends on.
    const PI_RPC_SESSION_0_87_1: &str = include_str!("fixtures/pi_rpc_session_0.87.1.jsonl");

    fn recorded_session() -> Vec<Value> {
        PI_RPC_SESSION_0_87_1
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("fixture line is JSON"))
            .collect()
    }

    #[tokio::test]
    async fn a_recorded_pi_0_87_1_turn_settles_once_and_carries_its_deltas_and_usage() {
        let session = recorded_session();
        let turn: Vec<Value> = session
            .iter()
            .filter(|event| event.get("type").and_then(Value::as_str) != Some("response"))
            .cloned()
            .collect();
        let events = collect_fixture(turn).await.unwrap();

        assert!(matches!(
            events.last().map(|event| event.kind),
            Some(CodingEngineEventKind::AgentSettled)
        ));
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, CodingEngineEventKind::AgentEnd)));
        assert!(events
            .iter()
            .any(|event| matches!(event.kind, CodingEngineEventKind::ToolExecutionEnd)));
        let text: String = events
            .iter()
            .filter_map(|event| event.text_delta.as_deref())
            .collect();
        assert_eq!(text.trim(), "DONE");
        // 0.84 dropped `assistantMessageEvent.partial`; the cumulative usage now
        // rides the top-level `usage` of every message_update.
        let streamed_usage = session
            .iter()
            .filter(|raw| raw.get("type").and_then(Value::as_str) == Some("message_update"))
            .filter(|raw| raw.get("usage").is_some())
            .count();
        assert!(
            streamed_usage > 0,
            "fixture carries top-level message_update usage"
        );
        let updates_with_usage = events
            .iter()
            .filter(|event| event.kind == CodingEngineEventKind::MessageUpdate)
            .filter(|event| event.usage.is_some())
            .count();
        assert_eq!(
            updates_with_usage, streamed_usage,
            "every top-level message_update usage is parsed"
        );
        assert!(
            events
                .iter()
                .filter_map(|event| event.usage.as_ref())
                .filter_map(|usage| usage.cost_total)
                .any(|cost| cost > 0.0),
            "the turn prices to a non-zero cost"
        );
    }

    #[test]
    fn recorded_pi_0_87_1_query_responses_are_successful() {
        let session = recorded_session();
        for command in ["get_session_stats", "get_state", "get_last_assistant_text"] {
            let response = session
                .iter()
                .find(|event| event.get("command").and_then(Value::as_str) == Some(command))
                .unwrap_or_else(|| panic!("fixture has a {command} response"));
            ensure_success_response(response, command).unwrap();
        }
    }

    /// F1 live conformance harness (opt-in). Confirms the source-extracted Pi
    /// contract at RUNTIME, end to end, against a real `pi --mode rpc`: the
    /// warm process accepts a prompt, streams a turn through `agent_end` to
    /// the authoritative `agent_settled` boundary via the single persistent
    /// reader, and answers `get_session_stats` / `get_last_assistant_text`.
    ///
    /// Gated on `MAGICIAN_PI_CONFORMANCE=1` so it runs only where the owner has
    /// `pi` installed AND a coding provider/key configured; a clean no-op pass
    /// otherwise, so it never blocks a contributor without Pi. To run:
    ///   make setup-pi-coding-agent && MAGICIAN_PI_CONFORMANCE=1 \
    ///     cargo test -p magician --lib pi_rpc_conformance -- --nocapture
    #[tokio::test]
    async fn pi_rpc_conformance_when_enabled() {
        if std::env::var("MAGICIAN_PI_CONFORMANCE").ok().as_deref() != Some("1") {
            eprintln!(
                "skip: set MAGICIAN_PI_CONFORMANCE=1 (with pi installed + a coding key) to run"
            );
            return;
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let scope = TransactionScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let mut request = CodingEngineRequest::new(
            "Reply with exactly the word READY and nothing else.",
            tmp.path(),
            tmp.path(),
            tmp.path(),
            scope,
        );
        request.stage_result = false;
        request.timeout = Duration::from_secs(180);

        let adapter = PiCodingEngineAdapter::default();
        let mut session = adapter
            .open_session(&request)
            .await
            .expect("open warm pi --mode rpc session");
        let handle = session.handle();

        handle
            .prompt(&request.prompt, None, None)
            .await
            .expect("pi accepts the prompt (preflight response over the reader)");
        let events = session
            .collect_until_idle(None)
            .await
            .expect("turn streams to agent_settled through the persistent reader");
        let stats = handle.get_session_stats().await;
        let assistant = handle.get_last_assistant_text().await;
        let _ = session.shutdown_graceful().await;

        assert!(!events.is_empty(), "expected events from the turn");
        assert!(
            events
                .iter()
                .any(|event| matches!(event.kind, CodingEngineEventKind::AgentSettled)),
            "expected agent_settled to close the turn"
        );
        assert!(
            stats.is_ok(),
            "get_session_stats should round-trip: {stats:?}"
        );
        assert!(
            assistant.is_ok(),
            "get_last_assistant_text should round-trip: {assistant:?}"
        );
    }
}

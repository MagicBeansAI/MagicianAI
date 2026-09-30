//! One-shot process harness base (plane plan Tasks 7/1b/1c).
//!
//! Claude Code is a warm bidirectional stream: one process, many turns.
//! Codex `exec`, Grok `-p`, and agy are one-shot CLIs — one process per
//! turn, prompt on argv, output on stdout. This base carries the parts they
//! share: a private home directory for MCP config (the session's own temp
//! dir, or a Magician-owned one the request hands in so the CLI's persisted
//! session survives this session — see `HarnessSessionRequest::native_home`),
//! the grant's lifecycle (config at start, revoke at shutdown), a bounded
//! stdout reader,
//! cancellation and wall-clock turn bounds, and a tolerant JSON-lines text
//! collector — tolerant because these CLIs' event schemas are probe-verified
//! for *connectivity*, not documented for machine output, so the collector
//! prefers common string fields and the harness treats process exit as the
//! turn's terminal event. A reply is read per JSON line; when no line
//! carried one, the whole stdout is read as one document (a CLI may print
//! a pretty-printed document, not JSONL). Each line is parsed once and also
//! asked for the CLI's own session id (the engine knows its init line) and
//! for a reply text delta (the engine knows its streaming line): a delta
//! goes to the turn's sink as it arrives, and a turn that streamed settles
//! with the streamed text alone — the CLI's final line repeats it, and
//! joining the two would carry the reply twice — except on a failing exit,
//! where that final line is the CLI's error and is kept after the streamed
//! text. The settled text is bounded (a cut is marked); the sink is not,
//! so the chat turn persists a streamed reply whole from what it collected.
//! A session in a persistent
//! home reports the CLI's id so the next turn resumes
//! it through the CLI's own resume flag, while a session on a temp home of
//! its own reports none — the id would name a session deleted with the
//! home, and a CLI resume has no fallback to a cold start. The child's
//! stderr is kept (its
//! bounded tail) and logged when a turn ends without a reply or with a
//! failing exit — a CLI's own refusal is printed there, and draining it
//! away meant a manual repro to see it. Governance is unaffected: every
//! tool call still crosses the plane, whatever the harness prints.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::execution::coding_engine::claude::{
    apply_claude_child_process_group, terminate_process_group,
};
use crate::magician_v2::execution::plane::engine::{
    revoke_session_grant, HarnessError, HarnessSession, HarnessSessionRequest, HarnessStopReason,
    HarnessStreamSink, HarnessTurnInput, HarnessTurnSettled, HarnessUsage,
};
use crate::magician_v2::execution::plane::grant::PLANE_TURN_RESULT_CUT_MARK;
use crate::magician_v2::execution::plane::usage::usage_from_event;

const MAX_EVENT_BYTES: usize = 1024 * 1024;
/// How much reply text a turn keeps, streamed or collected; a cut is marked
/// with `PLANE_TURN_RESULT_CUT_MARK`, never silent. The sink is not bounded:
/// every delta reaches it, so a chat turn can persist a streamed reply whole
/// from what it collected.
const MAX_REPLY_BYTES: usize = 16 * 1024;
/// How much of the child's stderr a turn keeps: the last bytes, since a CLI
/// prints its refusal last.
const STDERR_TAIL_BYTES: usize = 16 * 1024;
/// How long settle waits for the stderr collector after the child exited.
/// The pipe closes with the child unless a grandchild still holds it; the
/// collector keeps draining past this wait either way.
const STDERR_SETTLE_WAIT: Duration = Duration::from_millis(250);

/// Per-engine shape: how to launch one turn and how the session's MCP config
/// is installed and removed.
pub(crate) struct OneShotEngineSpec {
    pub binary: PathBuf,
    /// Build one turn's argv from the prompt text and, on a warm turn, the
    /// CLI's own session id to resume (element 0 is the binary; the spawner
    /// uses `binary` as the program and argv[1..]).
    pub argv_for_turn: Box<dyn Fn(&str, Option<&str>) -> Vec<String> + Send + Sync>,
    /// The CLI's own session id when this JSON line (or the whole-stdout
    /// document) announces it.
    pub native_session_id_of: fn(&Value) -> Option<String>,
    /// A reply text delta when this JSON line carries one, forwarded to the
    /// turn's sink as it arrives. An engine that streams flips
    /// `streams_text_deltas` too; a turn that streamed settles with the
    /// streamed text, never the final line or document (see `TurnReply`).
    pub text_delta_of: fn(&Value) -> Option<String>,
    /// Environment fixes for every spawn, derived from the session's
    /// private home (e.g. CODEX_HOME/GROK_HOME isolation) and grant.
    pub env_for: fn(&HomeInstall) -> Vec<(String, String)>,
    /// Install the plane MCP server binding for this session's grant
    /// (may create files under `home` or shell the CLI's own config API).
    pub install_config: fn(&HomeInstall) -> io::Result<()>,
    /// Remove the binding at shutdown. Best-effort.
    pub remove_config: fn(&HomeInstall),
    /// Run the child in the isolated home so project `.mcp.json` / vendor
    /// MCP next to Magician's cwd cannot become ungoverned hands.
    pub use_isolated_home_as_cwd: bool,
    /// Adapter supplies the planner system through its native flag or agent file.
    pub planner_system_supplied: bool,
    /// A completed assistant response can finish a proposal-only turn.
    pub planner_response_boundary: Option<fn(&Value) -> Option<(u64, bool)>>,
}

/// What an engine's config installer sees.
pub(crate) struct HomeInstall {
    pub home: PathBuf,
    pub endpoint_url: String,
    pub grant: String,
    pub cwd: PathBuf,
    pub planner_system: Option<String>,
}

/// Shared session for one-shot CLIs.
pub(crate) struct OneShotSession {
    spec: OneShotEngineSpec,
    grant: String,
    endpoint_url: String,
    cwd: PathBuf,
    cancel: Option<CancellationToken>,
    turn_timeout: Duration,
    turn_idle_timeout: Option<Duration>,
    home: PathBuf,
    /// The home is the session's own temp dir, removed with it. A home the
    /// request handed in belongs to the caller's continuation and is never
    /// removed here.
    owns_home: bool,
    env_allowlist: Vec<String>,
    /// The request's model, riding the CLI's model flag each turn
    /// (`None`/`default` = the CLI's own choice).
    model: Option<String>,
    planning_only: bool,
    /// One-shot adapters have no shared system-message flag. Planner requests
    /// must still carry the engine's proposal-only instructions into the CLI.
    planner_system: Option<String>,
    revoked: bool,
    /// Config is installed once per session: `mcp add` on a duplicate
    /// registration can error, and one binding per session is the contract.
    installed: bool,
    /// The CLI session the next spawn resumes: the request's, then whatever
    /// the last turn reported. Only passed to argv in a persistent home.
    resume_session_id: Option<String>,
    /// The CLI's own session id as announced by the turn in flight.
    native_session_id: Option<String>,
    turn_usage: Option<HarnessUsage>,
    /// The last exited turn's stderr tail, grant-redacted.
    last_stderr_tail: String,
}

impl OneShotSession {
    /// What the last exited turn's child printed on stderr (bounded tail,
    /// the session's grant redacted). Empty until a turn's child exits.
    #[cfg(test)]
    pub(crate) fn last_stderr_tail(&self) -> &str {
        &self.last_stderr_tail
    }

    fn env_for_install(&self) -> Vec<(String, String)> {
        let install = HomeInstall {
            home: self.home.clone(),
            endpoint_url: self.endpoint_url.clone(),
            grant: self.grant.clone(),
            cwd: self.cwd.clone(),
            planner_system: self.planner_system.clone(),
        };
        (self.spec.env_for)(&install)
    }
}

impl OneShotSession {
    pub(crate) fn new(spec: OneShotEngineSpec, req: &HarnessSessionRequest) -> Self {
        // An unpredictable private home: no shared path for a sibling
        // process to pre-place config. The session's own temp dir persists
        // nothing beyond its lifetime; a home the request hands in outlives
        // it so the CLI can resume its own session next turn.
        let owns_home = req.native_home.is_none();
        let home = req.native_home.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("magician-plane-{}", uuid::Uuid::new_v4().simple()))
        });
        let _ = std::fs::create_dir_all(&home);
        chmod_private_dir(&home);
        Self {
            spec,
            grant: req.grant.clone(),
            endpoint_url: req.endpoint.url.clone(),
            cwd: req.cwd.clone(),
            cancel: req.cancel.clone(),
            turn_timeout: req.turn_timeout,
            turn_idle_timeout: req.turn_idle_timeout,
            home,
            owns_home,
            env_allowlist: req.env_allowlist.clone(),
            model: req.model.clone().filter(|m| m != "default"),
            planning_only: req.planning_only,
            planner_system: req
                .planning_only
                .then(|| req.system_prompt.clone())
                .filter(|prompt| !prompt.trim().is_empty()),
            revoked: false,
            installed: false,
            resume_session_id: req.resume_session_id.clone().filter(|id| !id.is_empty()),
            native_session_id: None,
            turn_usage: None,
            last_stderr_tail: String::new(),
        }
    }

    /// The session id the next spawn resumes. A resume into a temp home
    /// cannot be found by the CLI (the home held no earlier turn), so only
    /// a persistent home passes one.
    fn resume_id_for_spawn(&self) -> Option<&str> {
        if self.owns_home {
            return None;
        }
        self.resume_session_id.as_deref()
    }

    /// The native id a settling turn reports. Only a session in a persistent
    /// home reports one: on a temp home the CLI's session goes with the
    /// directory. A refused turn reports only the id learned this turn —
    /// never the seeded one, so a failed resume goes cold next turn instead
    /// of refusing the same way forever (a CLI resume has no fallback). Every
    /// other stop falls back to the seeded id: the session it names still
    /// lives in the home.
    fn reported_native_id(&self, stop: HarnessStopReason) -> Option<String> {
        if self.owns_home {
            return None;
        }
        match stop {
            HarnessStopReason::Refused => self.native_session_id.clone(),
            _ => self
                .native_session_id
                .clone()
                .or_else(|| self.resume_session_id.clone()),
        }
    }

    /// Settle the turn: the reply text follows `TurnReply::settle`'s rule
    /// for this stop reason. The reported id is also what a further turn on
    /// this session resumes — the same rule the chat side applies when it
    /// stores the continuation.
    fn settle_turn(
        &mut self,
        reply: TurnReply,
        stop_reason: HarnessStopReason,
    ) -> HarnessTurnSettled {
        let assistant_text = if self.planning_only {
            reply.settle_planner(stop_reason)
        } else {
            reply.settle(stop_reason)
        };
        // Tripwire for CLI schema drift: a settled turn in a persistent home
        // should always have learned its id; when it did not, the engine's
        // init-line shape has moved and every turn silently runs cold.
        if !self.owns_home
            && stop_reason == HarnessStopReason::Settled
            && self.native_session_id.is_none()
        {
            tracing::debug!(
                binary = %self.spec.binary.display(),
                seeded = self.resume_session_id.is_some(),
                "one-shot harness settled in a persistent home without announcing its \
                 native session id; the next turn cannot warm-resume"
            );
        }
        let native_session_id = self.reported_native_id(stop_reason);
        self.resume_session_id = native_session_id.clone();
        HarnessTurnSettled {
            assistant_text,
            stop_reason,
            usage: self.turn_usage.clone(),
            native_session_id,
        }
    }

    async fn install(&self) -> Result<(), HarnessError> {
        let install = HomeInstall {
            home: self.home.clone(),
            endpoint_url: self.endpoint_url.clone(),
            grant: self.grant.clone(),
            cwd: self.cwd.clone(),
            planner_system: self.planner_system.clone(),
        };
        if let Err(error) = (self.spec.install_config)(&install) {
            revoke_session_grant(&self.grant).await;
            return Err(HarnessError::Message(format!(
                "install plane MCP config: {error}"
            )));
        }
        Ok(())
    }

    /// Spawn with an explicit model: the request's model rides the CLI's
    /// model flag (`claude --model`, `codex`/`grok` `-m`, `agy --model`);
    /// `None`/`default`/empty emits no flag. The binary name decides the
    /// flag shape — the one-shot engines each drive exactly one CLI.
    fn spawn_turn_with_model(
        &self,
        text: &str,
        model: Option<&str>,
    ) -> io::Result<tokio::process::Child> {
        // The env allowlist below re-sets PATH on the child; a bare binary
        // name combined with that would force std onto `fork` instead of
        // `posix_spawn`, so resolve it against the same PATH first (see
        // `runtime_core::process`).
        let mut command = tokio::process::Command::new(runtime_core::process::resolve_program(
            self.spec.binary.as_os_str(),
            None,
        ));
        let planner_text = self
            .planner_system
            .as_ref()
            .filter(|_| !self.spec.planner_system_supplied)
            .map(|system| format!("{system}\n\n{text}"));
        let mut argv = (self.spec.argv_for_turn)(
            planner_text.as_deref().unwrap_or(text),
            self.resume_id_for_spawn(),
        );
        if let Some(model) = model
            .map(str::trim)
            .filter(|model| !model.is_empty() && *model != "default")
        {
            let binary = self
                .spec
                .binary
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            let flag = match binary.as_str() {
                "codex" | "grok" => "-m",
                _ => "--model",
            };
            // Codex ends with its positional prompt. Other CLIs can end
            // with a flag VALUE (Agy's JSON schema or timeout), which must
            // stay adjacent to its flag.
            let index = if binary == "codex" {
                argv.len() - 1
            } else {
                argv.len()
            };
            argv.splice(index..index, [flag.to_string(), model.to_string()]);
        }
        command.args(&argv[1..]);
        if self.spec.use_isolated_home_as_cwd {
            command.current_dir(&self.home);
        } else {
            command.current_dir(&self.cwd);
        }
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.kill_on_drop(true);
        command.env_clear();
        for key in ["PATH", "HOME", "USER", "TMPDIR", "LANG"] {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for key in &self.env_allowlist {
            if let Ok(value) = std::env::var(key) {
                command.env(key, value);
            }
        }
        for (key, value) in self.env_for_install() {
            command.env(key, value);
        }
        apply_claude_child_process_group(&mut command);
        command.spawn()
    }
}

#[async_trait]
impl HarnessSession for OneShotSession {
    fn service_health(
        &self,
        outcome: &HarnessTurnSettled,
    ) -> Option<Result<(), crate::magician_v2::realtime_events::ServiceFailure>> {
        outcome.service_health().or_else(|| {
            if outcome.stop_reason != HarnessStopReason::Refused {
                return None;
            }
            crate::magician_v2::realtime_events::ServiceFailure::from_error(&self.last_stderr_tail)
                .map(Err)
        })
    }

    async fn turn(
        &mut self,
        input: &HarnessTurnInput,
        sink: &HarnessStreamSink,
    ) -> Result<HarnessTurnSettled, HarnessError> {
        // The id is learned afresh per turn: what an earlier turn learned
        // is already the resume id, and a refused turn must not report it.
        self.native_session_id = None;
        self.turn_usage = None;
        self.last_stderr_tail.clear();
        // A released session is terminal: the grant is revoked (and a home
        // of its own is gone) — a further turn could only spawn a child
        // with no governed path back to the plane. Guards every engine on
        // this base (claude_code, codex, grok, agy).
        if self.revoked {
            return Ok(self.settle_turn(TurnReply::default(), HarnessStopReason::Cancelled));
        }
        let cancelled = || {
            self.cancel
                .as_ref()
                .is_some_and(|token| token.is_cancelled())
        };
        if cancelled() {
            self.release().await;
            return Ok(self.settle_turn(TurnReply::default(), HarnessStopReason::Cancelled));
        }
        if !self.installed {
            self.install().await?;
            self.installed = true;
        }

        let mut child = match self.spawn_turn_with_model(&input.text, self.model.as_deref()) {
            Ok(child) => child,
            Err(error) => {
                revoke_session_grant(&self.grant).await;
                return Err(HarnessError::Message(format!("spawn harness: {error}")));
            },
        };
        let mut stdout = child
            .stdout
            .take()
            .map(tokio::io::BufReader::new)
            .ok_or_else(|| HarnessError::Message("harness did not expose stdout".to_string()))?;
        let mut stderr_tail = StderrTail::collect(child.stderr.take(), self.grant.len());

        let deadline =
            (!self.turn_timeout.is_zero()).then(|| tokio::time::Instant::now() + self.turn_timeout);
        // A harness that stops making progress must not hold the turn to its
        // whole ceiling. A bench run's codex typed a query into a background
        // Chrome, pressed Return four times with nothing submitting, tried two
        // focus actions its driver refuses, and then went idle — 1.5s of CPU
        // over eight minutes, no stdout, no tool call — with 40 minutes of
        // wall clock still to burn and no error anywhere. Silence longer than
        // the idle bound ends the turn as stalled, which the loop reports.
        let idle_timeout = self.turn_idle_timeout;
        let mut last_progress = tokio::time::Instant::now();
        let mut reply = TurnReply::default();
        let mut planner_response = PlannerResponse::default();
        let mut buf = Vec::new();
        // The whole stdout, bounded like a single event, for the one-document
        // fallback at exit; `None` once the bound is exceeded (a truncated
        // document cannot parse, and a partial one must not be mistaken for
        // the reply).
        let mut whole_stdout = Some(Vec::new());
        loop {
            if cancelled() {
                terminate_process_group(&mut child).await;
                self.release().await;
                return Ok(self.settle_turn(reply, HarnessStopReason::Cancelled));
            }
            if deadline.is_some_and(|deadline| {
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .is_zero()
            }) {
                return Ok(self
                    .settle_budget_spent(&mut child, &mut stderr_tail, reply)
                    .await);
            }
            tokio::select! {
                biased;
                _ = async {
                    match self.cancel.as_ref() {
                        Some(token) => token.cancelled().await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    terminate_process_group(&mut child).await;
                    self.release().await;
                    return Ok(self.settle_turn(reply, HarnessStopReason::Cancelled));
                }
                read = read_bounded_line(&mut stdout, &mut buf) => {
                    match read {
                        Ok(0) | Err(_) => {
                            // Process exit is the one-shot turn's terminal
                            // event; a read error settles like exit. The
                            // session stays installed — the Claude engine's
                            // contract keeps a settled session usable for a
                            // next turn, and release here would strand the
                            // grant mid-session.
                            let status = child.wait().await.ok();
                            let success = status
                                .as_ref()
                                .is_some_and(|status| status.success());
                            let _ = child.kill().await;
                            // The whole stdout as one document, for what no
                            // line carried: the reply (a turn that streamed
                            // has one already), and the CLI's session id (a
                            // pretty-printed document names its own on a
                            // field no line of it carries) — the latter only
                            // where it would be reported.
                            let wants_id = !self.owns_home && self.native_session_id.is_none();
                            if reply.is_empty() || wants_id || self.turn_usage.is_none() {
                                match whole_stdout.as_deref() {
                                    Some(whole) => {
                                        if let Some(value) = whole_stdout_document(whole) {
                                            if let Some(usage) = usage_from_event(&value) {
                                                self.turn_usage = Some(usage);
                                            }
                                            if reply.is_empty() {
                                                if let Some(text) = extract_turn_text(&value, self.planning_only) {
                                                    if self.planning_only {
                                                        reply.set_terminal(&text);
                                                    }
                                                    reply.push_text(&text);
                                                }
                                            }
                                            if wants_id {
                                                self.native_session_id =
                                                    (self.spec.native_session_id_of)(&value)
                                                        .filter(|id| !id.is_empty());
                                            }
                                        }
                                    },
                                    // The document was dropped above for
                                    // exceeding the bound and no line carried
                                    // a reply: say so once per turn rather
                                    // than settling with a silent empty reply.
                                    None if reply.is_empty() => tracing::warn!(
                                        binary = %self.spec.binary.display(),
                                        bound_bytes = MAX_EVENT_BYTES,
                                        "one-shot harness reply could not be read: the stdout \
                                         document exceeded the event bound"
                                    ),
                                    None => {},
                                }
                            }
                            let stderr_text = stderr_tail.settle(&self.grant).await;
                            self.log_stderr_tail(
                                status.as_ref().and_then(|status| status.code()),
                                success,
                                reply.is_empty(),
                                &stderr_text,
                            );
                            self.last_stderr_tail = stderr_text;
                            let stop_reason = if success {
                                HarnessStopReason::Settled
                            } else {
                                HarnessStopReason::Refused
                            };
                            return Ok(self.settle_turn(reply, stop_reason));
                        },
                        Ok(_) => {
                            last_progress = tokio::time::Instant::now();
                            whole_stdout = whole_stdout.take().filter(|whole| {
                                whole.len().saturating_add(buf.len()) <= MAX_EVENT_BYTES
                            });
                            if let Some(whole) = whole_stdout.as_mut() {
                                whole.extend_from_slice(&buf);
                            }
                            // One parse per line, shared by every reader.
                            let Some(value) = parse_json_line(&buf) else {
                                continue;
                            };
                            if let Some(usage) = usage_from_event(&value) {
                                self.turn_usage = Some(usage);
                            }
                            if let Some(id) = (self.spec.native_session_id_of)(&value)
                                .filter(|id| !id.is_empty())
                            {
                                self.native_session_id = Some(id);
                            }
                            if let Some(delta) = (self.spec.text_delta_of)(&value) {
                                sink.emit(&delta);
                                reply.push_delta(&delta);
                            }
                            if self.planning_only {
                                if let Some(boundary) = self.spec.planner_response_boundary {
                                    if let Some(text) = planner_response.observe(
                                        &value, boundary, self.spec.text_delta_of,
                                    ) {
                                        // The completed assistant message is a proposal, not
                                        // completed work. Stop before the CLI starts another
                                        // native loop; the Decision Engine still validates it.
                                        terminate_process_group(&mut child).await;
                                        self.last_stderr_tail = stderr_tail.settle(&self.grant).await;
                                        self.turn_usage = if planner_response.missing_usage { None } else { planner_response.usage.clone() };
                                        reply.set_terminal(&text);
                                        let reason = if cancelled() {
                                            HarnessStopReason::Cancelled
                                        } else {
                                            HarnessStopReason::Settled
                                        };
                                        return Ok(self.settle_turn(reply, reason));
                                    }
                                }
                            }
                            if let Some(text) = extract_turn_text(&value, self.planning_only) {
                                // A planner needs the final proposal, not the
                                // commentary streamed before an MCP call.
                                if self.planning_only && (
                                    value["type"] == "result" || value["event"] == "result"
                                    || (value["type"] == "item.completed" && value["item"]["type"] == "agent_message")
                                ) {
                                    reply.set_terminal(&text);
                                }
                                reply.push_text(&text);
                            }
                        },
                    }
                }
                _ = async {
                    match deadline {
                        Some(deadline) => tokio::time::sleep_until(deadline).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    // The ceiling must fire even while the harness is silent:
                    // without this arm, a quiet process blocks the read and
                    // the bound is unreachable between lines.
                    return Ok(self
                        .settle_budget_spent(&mut child, &mut stderr_tail, reply)
                        .await);
                },
                _ = async {
                    match idle_timeout {
                        Some(idle) => tokio::time::sleep_until(last_progress + idle).await,
                        None => std::future::pending::<()>().await,
                    }
                } => {
                    tracing::warn!(
                        binary = %self.spec.binary.display(),
                        idle_secs = idle_timeout.map(|idle| idle.as_secs()).unwrap_or_default(),
                        has_reply = !reply.is_empty(),
                        "harness turn made no progress within its idle bound; ending it as stalled"
                    );
                    return Ok(self
                        .settle_stalled(&mut child, &mut stderr_tail, reply)
                        .await);
                },
            }
        }
    }

    async fn shutdown(&mut self) {
        self.release().await;
    }
}

impl OneShotSession {
    /// The wall clock ended the turn. Stop the child, then keep its stderr
    /// tail: a turn that hung until the ceiling is exactly where the
    /// diagnosis lives, and only the exit path read it before. Bounded by the
    /// same settle wait as an exit.
    async fn settle_budget_spent(
        &mut self,
        child: &mut tokio::process::Child,
        stderr_tail: &mut StderrTail,
        reply: TurnReply,
    ) -> HarnessTurnSettled {
        terminate_process_group(child).await;
        let stderr_text = stderr_tail.settle(&self.grant).await;
        self.log_stderr_tail(None, false, reply.is_empty(), &stderr_text);
        self.last_stderr_tail = stderr_text;
        self.release().await;
        self.settle_turn(reply, HarnessStopReason::TurnBudgetSpent)
    }

    /// A turn that went silent. Distinct from the wall-clock ceiling so the
    /// loop's failure reason says which bound ended it, and distinct from
    /// `Settled` so a partial reply left by a stuck harness is never published
    /// as the run's deliverable.
    async fn settle_stalled(
        &mut self,
        child: &mut tokio::process::Child,
        stderr_tail: &mut StderrTail,
        reply: TurnReply,
    ) -> HarnessTurnSettled {
        terminate_process_group(child).await;
        let stderr_text = stderr_tail.settle(&self.grant).await;
        self.log_stderr_tail(None, false, reply.is_empty(), &stderr_text);
        self.last_stderr_tail = stderr_text;
        self.release().await;
        self.settle_turn(reply, HarnessStopReason::Stalled)
    }

    /// A one-shot CLI's own refusal is on stderr. Warn when the turn went
    /// wrong — no reply, or a failing exit — and keep it at debug otherwise.
    /// The tail is the child's, spawned by this session, so nothing in it is
    /// withheld beyond the session's own grant (a URL error can echo it).
    fn log_stderr_tail(&self, exit: Option<i32>, success: bool, no_reply: bool, tail: &str) {
        if tail.is_empty() {
            return;
        }
        if no_reply || !success {
            tracing::warn!(
                binary = %self.spec.binary.display(),
                exit = ?exit,
                no_reply,
                stderr_tail = %tail,
                "one-shot harness turn ended without a reply or with a failing exit"
            );
        } else {
            tracing::debug!(
                binary = %self.spec.binary.display(),
                exit = ?exit,
                stderr_tail = %tail,
                "one-shot harness stderr"
            );
        }
    }

    async fn release(&mut self) {
        let install = HomeInstall {
            home: self.home.clone(),
            endpoint_url: self.endpoint_url.clone(),
            grant: self.grant.clone(),
            cwd: self.cwd.clone(),
            planner_system: self.planner_system.clone(),
        };
        (self.spec.remove_config)(&install);
        if self.owns_home {
            let _ = std::fs::remove_dir_all(&self.home);
        }
        if !self.revoked {
            revoke_session_grant(&self.grant).await;
            self.revoked = true;
        }
    }
}

impl Drop for OneShotSession {
    fn drop(&mut self) {
        // Config cleanup is synchronous filesystem work; the grant revoke
        // needs a runtime, which may not exist here — the turn-engine's
        // continuation revoke covers a dropped-paused session, and grants
        // are expiring credentials regardless. A persistent home is the
        // continuation's to remove, so only the session's own goes.
        let install = HomeInstall {
            home: self.home.clone(),
            endpoint_url: self.endpoint_url.clone(),
            grant: self.grant.clone(),
            cwd: self.cwd.clone(),
            planner_system: self.planner_system.clone(),
        };
        (self.spec.remove_config)(&install);
        if self.owns_home {
            let _ = std::fs::remove_dir_all(&self.home);
        }
    }
}

/// Capture a bounded, completed assistant message on transports that continue
/// their native loop after emitting a valid proposal. Never use thinking text,
/// tool output, partial JSON, or multiple messages as a completed reply.
#[derive(Default)]
struct PlannerResponse {
    id: Option<u64>,
    text: String,
    cut: bool,
    seen_usage: std::collections::HashSet<u64>,
    missing_usage: bool,
    usage: Option<HarnessUsage>,
}

impl PlannerResponse {
    fn observe(
        &mut self,
        value: &Value,
        boundary: fn(&Value) -> Option<(u64, bool)>,
        delta: fn(&Value) -> Option<String>,
    ) -> Option<String> {
        let (id, done) = boundary(value)?;
        if self.id != Some(id) {
            self.id = Some(id);
            self.text.clear();
            self.cut = false;
        }
        if let Some(delta) = delta(value) {
            push_bounded(&mut self.text, &mut self.cut, &delta);
        }
        if !done {
            return None;
        }
        if self.seen_usage.insert(id) {
            if let Some(usage) = super::super::usage::usage_from_agy_completed_response(value) {
                match self.usage.as_mut() {
                    Some(total) => total.accumulate(usage),
                    None => self.usage = Some(usage),
                }
            } else {
                self.missing_usage = true;
            }
        }
        if self.cut {
            return None;
        }
        let mut text = self.text.trim();
        if let Some(fenced) = text
            .strip_prefix("```json")
            .or_else(|| text.strip_prefix("```"))
        {
            text = fenced.trim().strip_suffix("```")?.trim();
        }
        let mut value: Value = serde_json::from_str(text).ok()?;
        let fields = value.as_object_mut()?;
        for key in ["toolAction", "toolSummary", "reason"] {
            if fields.remove(key).is_some_and(|value| !value.is_string()) {
                return None;
            }
        }
        let valid = if fields.contains_key("answer") {
            fields.len() == 1
                && fields["answer"]
                    .as_str()
                    .is_some_and(|s| !s.trim().is_empty())
        } else {
            serde_json::from_value::<decision_engine_contract::action::ActionPlan>(value.clone())
                .is_ok_and(|plan| !plan.steps.is_empty() && plan.steps.len() <= 32)
        };
        valid.then(|| value.to_string())
    }
}

/// One turn's reply as it accumulates: what the engine streamed through the
/// sink, and what the per-line collector (or the whole-document fallback)
/// read. Both are bounded like the collected text always was, and a cut is
/// marked (the head is kept, the rest dropped — it still reached the sink).
/// At settle the streamed text wins whenever there is any — a streaming
/// CLI's final line repeats what it streamed, and joining the two would
/// carry the reply twice; a turn that streamed nothing settles with the
/// collected text, so an engine that does not stream is unchanged. A
/// failing exit is the exception: the final line of a failed turn is the
/// CLI's error, not the reply repeated, so it is kept after the streamed
/// text.
#[derive(Default)]
struct TurnReply {
    streamed: String,
    streamed_cut: bool,
    collected: String,
    collected_cut: bool,
    terminal: Option<String>,
}

impl TurnReply {
    fn set_terminal(&mut self, text: &str) {
        let mut terminal = String::new();
        let mut cut = false;
        push_bounded(&mut terminal, &mut cut, text);
        self.terminal = Some(terminal);
    }

    fn settle_planner(mut self, stop_reason: HarnessStopReason) -> String {
        if stop_reason == HarnessStopReason::Settled {
            if let Some(terminal) = self.terminal.take() {
                return terminal;
            }
        }
        self.settle(stop_reason)
    }

    fn push_delta(&mut self, delta: &str) {
        push_bounded(&mut self.streamed, &mut self.streamed_cut, delta);
    }

    /// One line's (or the document's) reply text, joined to what earlier
    /// lines carried. Bounded so a chatty CLI cannot flood the decision
    /// summary.
    fn push_text(&mut self, text: &str) {
        if text.is_empty() || self.collected_cut {
            return;
        }
        if !self.collected.is_empty() {
            self.collected.push('\n');
        }
        push_bounded(&mut self.collected, &mut self.collected_cut, text);
    }

    /// Whether any reply has been read yet, streamed or collected.
    /// Whitespace alone (a stream that only ever sent a newline) is none.
    fn is_empty(&self) -> bool {
        self.streamed.trim().is_empty() && self.collected.trim().is_empty()
    }

    /// The settled text: streamed when the turn streamed, else collected.
    /// On a failing exit the collected text is kept after the streamed
    /// text — it is the CLI's final error — unless it only repeats what was
    /// streamed (or the head of it, when the bound cut the stream).
    fn settle(self, stop_reason: HarnessStopReason) -> String {
        if self.streamed.trim().is_empty() {
            return self.collected;
        }
        let mut text = self.streamed;
        if stop_reason != HarnessStopReason::Refused || self.collected.trim().is_empty() {
            return text;
        }
        let streamed_head = if self.streamed_cut {
            &text[..text.len() - PLANE_TURN_RESULT_CUT_MARK.len()]
        } else {
            text.as_str()
        };
        let repeats_streamed =
            text.contains(&self.collected) || self.collected.starts_with(streamed_head);
        if !repeats_streamed {
            text.push('\n');
            text.push_str(&self.collected);
        }
        text
    }
}

/// Append `text` to `kept` under the reply bound. Once the bound cuts, the
/// head is kept and marked, `cut` is set, and nothing further is appended.
fn push_bounded(kept: &mut String, cut: &mut bool, text: &str) {
    if *cut {
        return;
    }
    kept.push_str(text);
    if kept.len() > MAX_REPLY_BYTES {
        *kept = bounded_text(std::mem::take(kept));
        kept.push_str(PLANE_TURN_RESULT_CUT_MARK);
        *cut = true;
    }
}

/// The tail of a child's stderr, collected while the turn runs. Every byte
/// is drained so a chatty child never blocks on a full pipe; only the last
/// `STDERR_TAIL_BYTES` are kept, since a CLI prints its refusal last.
///
/// The raw window is one grant longer than the kept tail and the final cut
/// happens after redaction, so a grant straddling the cut is whole when it is
/// replaced rather than surviving as a fragment.
struct StderrTail {
    raw: Arc<StdMutex<RawStderrTail>>,
    drain: Option<tokio::task::JoinHandle<()>>,
}

/// The raw bytes and whether the window ever dropped its head.
#[derive(Default)]
struct RawStderrTail {
    bytes: Vec<u8>,
    cap: usize,
    head_dropped: bool,
}

impl StderrTail {
    fn collect(stderr: Option<tokio::process::ChildStderr>, grant_len: usize) -> Self {
        let raw = Arc::new(StdMutex::new(RawStderrTail {
            cap: STDERR_TAIL_BYTES.saturating_add(grant_len),
            ..RawStderrTail::default()
        }));
        let drain = stderr.map(|mut stderr| {
            let sink = Arc::clone(&raw);
            tokio::spawn(async move {
                use tokio::io::AsyncReadExt;
                let mut chunk = [0u8; 4096];
                loop {
                    match stderr.read(&mut chunk).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => keep_tail(&sink, &chunk[..read]),
                    }
                }
            })
        });
        Self { raw, drain }
    }

    /// The tail once the child has exited: waits briefly for the collector to
    /// read what the child wrote last, then takes what is there. A collector
    /// still draining (a grandchild holds the pipe) is left to finish.
    async fn settle(&mut self, grant: &str) -> String {
        if let Some(mut drain) = self.drain.take() {
            let _ = tokio::time::timeout(STDERR_SETTLE_WAIT, &mut drain).await;
        }
        let (raw, head_dropped) = {
            let raw = self
                .raw
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                String::from_utf8_lossy(&raw.bytes).into_owned(),
                raw.head_dropped,
            )
        };
        // The session's own grant first, literally — the generic redactor
        // does not know it — then the same credential redaction a
        // provider-bound text gets.
        let without_grant = if grant.is_empty() {
            raw
        } else {
            raw.replace(grant, "[grant]")
        };
        // A window that dropped its head can start inside a grant that began
        // before it. Whole occurrences are gone now, so a grant suffix at the
        // head is that fragment.
        let head_safe = if head_dropped {
            drop_grant_fragment(&without_grant, grant)
        } else {
            without_grant.as_str()
        };
        let sanitized = crate::magician_v2::secrets::sanitize_text_for_provider(head_safe);
        keep_text_tail(&sanitized, STDERR_TAIL_BYTES)
            .trim()
            .to_owned()
    }
}

fn keep_tail(sink: &StdMutex<RawStderrTail>, chunk: &[u8]) {
    let mut raw = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    raw.bytes.extend_from_slice(chunk);
    if raw.bytes.len() > raw.cap {
        let excess = raw.bytes.len() - raw.cap;
        raw.bytes.drain(..excess);
        raw.head_dropped = true;
    }
}

/// The last `max_bytes` of `text`, cut on a character boundary.
fn keep_text_tail(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut start = text.len() - max_bytes;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// `text` without a leading proper suffix of `grant`: what a raw cut through
/// the grant leaves at the head of the window once whole occurrences are
/// replaced. The longest matching suffix is the fragment.
fn drop_grant_fragment<'a>(text: &'a str, grant: &str) -> &'a str {
    for start in 1..grant.len() {
        if !grant.is_char_boundary(start) {
            continue;
        }
        let suffix = &grant[start..];
        if text.starts_with(suffix) {
            return &text[suffix.len()..];
        }
    }
    text
}

/// One stdout line as JSON. A blank or non-JSON line (progress noise, one
/// line of a pretty-printed document) is nothing to read.
fn parse_json_line(line: &[u8]) -> Option<Value> {
    let trimmed = std::str::from_utf8(line).ok()?.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

/// The document when stdout was not JSONL: the whole output as one JSON
/// value, else the last top-level object in it (a CLI may print progress
/// around its document). Read with the same key rules as a line — a
/// reasoning field is never the reply.
fn whole_stdout_document(stdout: &[u8]) -> Option<Value> {
    let text = std::str::from_utf8(stdout).ok()?.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    let start = text
        .rmatch_indices('{')
        .map(|(index, _)| index)
        .find(|index| *index == 0 || text.as_bytes()[index - 1] == b'\n')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    serde_json::from_str(&text[start..=end]).ok()
}

/// Cap reply text (streamed or collected) so a chatty CLI cannot flood the
/// decision summary; the cut floors to a char boundary because `truncate`
/// panics on a mid-character cut.
fn bounded_text(mut text: String) -> String {
    if text.len() > MAX_REPLY_BYTES {
        let mut cut = MAX_REPLY_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// Tolerant text extraction: the key rules shared by the per-line and
/// whole-document paths. One-shot CLIs do not share a schema; missing a
/// documented field here is a silent empty chat reply.
fn extract_turn_text(value: &Value, planning_only: bool) -> Option<String> {
    // Agy mixes presentation text with JSON in `response`; its structured
    // terminal field carries the actual schema-constrained proposal/reply.
    // It is still untrusted data and must pass the ordinary plan parser.
    if planning_only && value["event"] == "result" {
        if let Some(output) = value
            .pointer("/result/structured_output")
            .filter(|v| v.is_object())
        {
            return Some(output.to_string());
        }
    }
    extract_text_value(value)
}

fn extract_text_value(value: &Value) -> Option<String> {
    let type_name = value.get("type").and_then(Value::as_str).unwrap_or("");
    let event_name = value.get("event").and_then(Value::as_str).unwrap_or("");

    if let Some(item) = value.get("item") {
        if item.get("type").and_then(Value::as_str) == Some("agent_message") {
            if let Some(text) = nonempty_str(item.get("text")) {
                return Some(text);
            }
        }
    }
    if type_name == "text" {
        if let Some(text) = nonempty_str(value.get("data")) {
            return Some(text);
        }
    }
    if event_name == "result" {
        if let Some(text) = nonempty_str(value.pointer("/result/response"))
            .or_else(|| nonempty_str(value.pointer("/result/text")))
        {
            return Some(text);
        }
    }
    for key in ["result", "text", "message", "output", "response"] {
        if let Some(text) = nonempty_str(value.get(key)) {
            return Some(text);
        }
    }
    None
}

fn nonempty_str(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// Read one newline-bounded line without unbounded allocation (mirrors the
/// Claude engine's bounded reader).
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    output: &mut Vec<u8>,
) -> io::Result<usize> {
    output.clear();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Ok(output.len());
        }
        let end = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if output.len().saturating_add(end) > MAX_EVENT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("harness event exceeds {MAX_EVENT_BYTES} byte limit"),
            ));
        }
        output.extend_from_slice(&available[..end]);
        let ended = available[end - 1] == b'\n';
        reader.consume(end);
        if ended {
            return Ok(output.len());
        }
    }
}

pub(crate) fn chmod_private_dir(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700));
    }
    let _ = path;
}

/// Operator CLI home (`$ENV` or `~/.$dot_dir`). Isolation copies auth from
/// here; it must not be the session's temp home.
pub(crate) fn operator_cli_home(env_key: &str, dot_dir: &str) -> PathBuf {
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::var("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(dot_dir)
        })
}

/// Copy one named file into the isolated home (mode 0600). Missing source
/// is success: API-key env may still authenticate.
pub(crate) fn seed_named_file(from_home: &Path, into_home: &Path, name: &str) -> io::Result<()> {
    if from_home == into_home {
        return Ok(());
    }
    let src = from_home.join(name);
    if !src.is_file() {
        return Ok(());
    }
    let bytes = std::fs::read(&src)?;
    write_private(&into_home.join(name), &bytes)
}

/// Fail closed before a long spawn when the operator has neither a
/// session file nor the API-key fallback. Codex in particular will 401-
/// retry until the turn wall-clock if both are missing.
pub(crate) fn require_cli_auth(
    env_key: &str,
    dot_dir: &str,
    api_key_env: &str,
    label: &str,
) -> io::Result<()> {
    let home = operator_cli_home(env_key, dot_dir);
    if home.join("auth.json").is_file() {
        return Ok(());
    }
    if std::env::var(api_key_env)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .is_some()
    {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "{label} is not signed in ({api_key_env} unset and no auth.json)"
    )))
}

pub(crate) fn inherit_env(keys: &[&str]) -> Vec<(String, String)> {
    keys.iter()
        .filter_map(|key| {
            std::env::var(key)
                .ok()
                .map(|value| ((*key).to_string(), value))
        })
        .collect()
}

/// Write a file creating parents, mode 0600 where supported.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut options = std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(false)
        .truncate(true)
        .create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::plane::engine::HarnessEngine;

    #[tokio::test]
    async fn decision_planner_stops_at_complete_response_before_cli_continues() {
        let dir = tempfile::tempdir().unwrap();
        let event = serde_json::json!({"event":"step_update","step_update":{
            "step_index":1,"step_type":"agent_response","state":"DONE",
            "text_delta":"{\"steps\":[{\"id\":\"read\",\"call\":{\"tool\":\"lookup\",\"arguments\":{}}}]}",
            "usage":{"input_tokens":20,"cache_read_tokens":80,"output_tokens":4}
        }});
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{}'\nsleep 5\necho 'native loop should never run'\n",
                event
            ),
        );
        let mut spec = spec_with(&binary);
        spec.text_delta_of = |v| {
            v.pointer("/step_update/text_delta")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        spec.planner_response_boundary = Some(|v| {
            Some((
                v.pointer("/step_update/step_index")?.as_u64()?,
                v.pointer("/step_update/state")? == "DONE",
            ))
        });
        let mut req = request(Duration::from_secs(10));
        req.planning_only = true;
        let mut session = OneShotSession::new(spec, &req);
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            session.turn(
                &HarnessTurnInput {
                    text: "propose".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("must stop before native continuation")
        .unwrap();
        assert_eq!(result.stop_reason, HarnessStopReason::Settled);
        let plan: decision_engine_contract::action::ActionPlan =
            serde_json::from_str(&result.assistant_text).unwrap();
        assert_eq!(plan.steps[0].call.tool, "lookup");
        let usage = result.usage.unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.output_tokens,
                usage.cached_input_tokens
            ),
            (100, 4, 80)
        );
        session.shutdown().await;
    }

    #[test]
    fn decision_planner_complete_message_rejects_partial_or_ambiguous_json() {
        fn boundary(v: &Value) -> Option<(u64, bool)> {
            Some((
                v["step_update"]["step_index"].as_u64()?,
                v["step_update"]["state"] == "DONE",
            ))
        }
        fn delta(v: &Value) -> Option<String> {
            v["step_update"]["text_delta"].as_str().map(str::to_owned)
        }
        for (text, done) in [
            ("{\"steps\":[", true),
            ("{\"answer\":\"done\",\"steps\":[]}", true),
            ("{\"answer\":\"done\"}", false),
            ("{\"answer\":\"done\"} {\"answer\":\"different\"}", true),
        ] {
            let event = serde_json::json!({"event":"step_update","step_update":{"step_index":1,"state":if done {"DONE"} else {"ACTIVE"},"text_delta":text}});
            assert!(PlannerResponse::default()
                .observe(&event, boundary, delta)
                .is_none());
        }
        let mut reply = PlannerResponse::default();
        let event = serde_json::json!({"event":"step_update","step_update":{"step_index":1,"state":"DONE","text_delta":"{\"answer\":\"done\"}"}});
        assert!(reply.observe(&event, boundary, delta).is_some());
        assert!(reply.missing_usage, "missing metering must remain unknown");
    }

    fn spec_with(binary: &str) -> OneShotEngineSpec {
        let program = binary.to_string();
        OneShotEngineSpec {
            binary: std::path::PathBuf::from(binary),
            argv_for_turn: Box::new(move |text, _resume| vec![program.clone(), text.to_string()]),
            native_session_id_of: |_| None,
            text_delta_of: |_| None,
            env_for: |_| Vec::new(),
            install_config: |_| Ok(()),
            remove_config: |_| {},
            use_isolated_home_as_cwd: false,
            planner_system_supplied: false,
            planner_response_boundary: None,
        }
    }

    /// A spec whose CLI announces its session on an init line (`type` =
    /// `init`, `id`) and, warm, is handed the resume id as a trailing
    /// argument — the shape the fake child echoes back.
    fn resuming_spec(binary: &str) -> OneShotEngineSpec {
        let program = binary.to_string();
        OneShotEngineSpec {
            argv_for_turn: Box::new(move |text, resume| {
                let mut argv = vec![program.clone(), text.to_string()];
                if let Some(id) = resume {
                    argv.push("--resume".to_string());
                    argv.push(id.to_string());
                }
                argv
            }),
            native_session_id_of: |value| {
                (value.get("type").and_then(Value::as_str) == Some("init"))
                    .then(|| value.get("id").and_then(Value::as_str).map(str::to_string))
                    .flatten()
            },
            ..spec_with(binary)
        }
    }

    /// A spec whose CLI streams the reply as `delta` lines before a final
    /// line that repeats it whole — the shape of a streaming one-shot CLI.
    fn streaming_spec(binary: &str) -> OneShotEngineSpec {
        OneShotEngineSpec {
            text_delta_of: |value| {
                value
                    .get("delta")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            },
            ..spec_with(binary)
        }
    }

    /// The per-line reader as the turn loop applies it: one parse, then the
    /// shared key rules.
    fn extract_text(line: &[u8]) -> Option<String> {
        parse_json_line(line).and_then(|value| extract_text_value(&value))
    }

    /// The whole-stdout fallback as the exit path applies it.
    fn reply_from_whole_stdout(stdout: &[u8]) -> Option<String> {
        whole_stdout_document(stdout).and_then(|value| extract_text_value(&value))
    }

    fn fake_engine(dir: &std::path::Path, script: &str) -> String {
        fake_engine_named(dir, "fake-harness", script)
    }

    /// A fake CLI under a chosen name: the model flag's shape keys on the
    /// binary's file name.
    fn fake_engine_named(dir: &std::path::Path, name: &str, script: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, script).expect("write fake");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut perms = std::fs::metadata(&path).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&path, perms).unwrap();
        }
        path.display().to_string()
    }

    fn request(turn_timeout: Duration) -> HarnessSessionRequest {
        request_with_idle(turn_timeout, None)
    }

    fn request_with_idle(
        turn_timeout: Duration,
        turn_idle_timeout: Option<Duration>,
    ) -> HarnessSessionRequest {
        HarnessSessionRequest {
            planning_only: false,
            turn_idle_timeout,
            endpoint: crate::magician_v2::execution::plane::engine::PlaneEndpoint {
                url: "http://127.0.0.1:8899/mcp".to_string(),
            },
            grant: "plt_test".to_string(),
            system_prompt: String::new(),
            model: None,
            pi_profile: None,
            pi_images: Vec::new(),
            cwd: std::env::temp_dir(),
            env_allowlist: Vec::new(),
            cancel: None,
            resume_session_id: None,
            turn_timeout,
            native_home: None,
        }
    }

    #[test]
    fn require_cli_auth_fails_closed_without_file_or_key() {
        let err = require_cli_auth(
            "MAGICIAN_TEST_NO_SUCH_HOME",
            ".magician-no-such-dotdir",
            "MAGICIAN_TEST_NO_SUCH_API_KEY",
            "test-cli",
        )
        .unwrap_err();
        assert!(err.to_string().contains("not signed in"));
    }

    #[test]
    fn extract_text_reads_documented_one_shot_shapes() {
        assert_eq!(
            extract_text(br#"{"text":"grok json"}"#).as_deref(),
            Some("grok json")
        );
        assert_eq!(
            extract_text(br#"{"type":"text","data":"grok stream"}"#).as_deref(),
            Some("grok stream")
        );
        assert_eq!(extract_text(br#"{"type":"thought","data":"hidden"}"#), None);
        assert_eq!(
            extract_text(
                br#"{"type":"item.completed","item":{"id":"i","type":"agent_message","text":"codex"}}"#
            )
            .as_deref(),
            Some("codex")
        );
        assert_eq!(
            extract_text(
                br#"{"type":"item.completed","item":{"id":"i","type":"reasoning","text":"think"}}"#
            ),
            None
        );
        assert_eq!(
            extract_text(br#"{"event":"result","result":{"status":"SUCCESS","response":"agy"}}"#)
                .as_deref(),
            Some("agy")
        );
    }

    /// grok --output-format json prints one pretty-printed document, not
    /// JSONL: no single line parses, so the reply must come from the whole
    /// stdout — and never from the `thought` field.
    const PRETTY_PRINTED_REPLY: &str = r#"{
  "text": "PONG",
  "stopReason": "end_turn",
  "sessionId": "sess-1",
  "requestId": "req-1",
  "thought": "the user wants a ping reply",
  "usage": {
    "input_tokens": 10,
    "output_tokens": 2
  },
  "num_turns": 1
}
"#;

    #[test]
    fn extract_text_value_reads_the_pretty_printed_one_shot_document() {
        let value: Value = serde_json::from_str(PRETTY_PRINTED_REPLY).unwrap();
        assert_eq!(extract_text_value(&value).as_deref(), Some("PONG"));
        let thought_only: Value =
            serde_json::from_str(r#"{"thought":"hidden","stopReason":"end_turn","usage":{}}"#)
                .unwrap();
        assert_eq!(extract_text_value(&thought_only), None);
        for line in PRETTY_PRINTED_REPLY.lines() {
            assert_eq!(
                extract_text(line.as_bytes()),
                None,
                "no single line of the document is a reply"
            );
        }
    }

    #[test]
    fn reply_from_whole_stdout_falls_back_to_one_document() {
        assert_eq!(
            reply_from_whole_stdout(PRETTY_PRINTED_REPLY.as_bytes()).as_deref(),
            Some("PONG")
        );
        // Noise around the document: the last top-level object wins.
        let with_noise = format!(
            "warming up\n{{\"text\": \"earlier\"}}\n{PRETTY_PRINTED_REPLY}trailing noise\n"
        );
        assert_eq!(
            reply_from_whole_stdout(with_noise.as_bytes()).as_deref(),
            Some("PONG")
        );
        // A JSONL stream is handled line-wise; the fallback must not
        // misparse it into a different reply.
        let jsonl = "{\"type\":\"thought\",\"data\":\"hidden\"}\n{\"text\":\"line reply\"}\n";
        assert_eq!(
            reply_from_whole_stdout(jsonl.as_bytes()).as_deref(),
            Some("line reply")
        );
        assert_eq!(reply_from_whole_stdout(b"not json at all"), None);
        assert_eq!(reply_from_whole_stdout(b""), None);
        assert_eq!(reply_from_whole_stdout(b"{\"thought\":\"only\"}\n"), None);
    }

    /// Session-level: a child that prints the pretty-printed document
    /// settles with its `text`, not without a reply.
    #[tokio::test]
    async fn a_pretty_printed_document_settles_with_its_text() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reply.json"), PRETTY_PRINTED_REPLY).unwrap();
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\ncat '{}'\n",
                dir.path().join("reply.json").display()
            ),
        );
        let engine = FakeEngine(binary);
        let mut session = engine
            .start(&request(Duration::from_secs(10)))
            .await
            .unwrap();
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "ping".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "PONG");
    }

    /// Exit 0 after emitting JSON lines: Settled with the tolerant
    /// extraction's collected text.
    #[tokio::test]
    async fn a_successful_exit_settles_with_collected_text() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho '{\"result\":\"did the thing\"}'\necho 'noise line'\n",
        );
        let engine = FakeEngine(binary.clone());
        let mut session = engine
            .start(&request(Duration::from_secs(10)))
            .await
            .unwrap();
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "did the thing");
        assert_eq!(settled.native_session_id, None);
    }

    /// Exit failure: Refused, and the session survives for a next turn
    /// (the contract review fixed — no release on exit paths).
    #[tokio::test]
    async fn a_failing_exit_refuses_and_keeps_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho '{\"text\":\"partial\"}'\nexit 3\n",
        );
        let engine = FakeEngine(binary);
        let mut session = engine
            .start(&request(Duration::from_secs(10)))
            .await
            .unwrap();
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.assistant_text, "partial");
        // A settled-or-refused session must still be usable — the Claude
        // engine's warm-turn contract.
        let again = session
            .turn(
                &HarnessTurnInput {
                    text: "again".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap();
        assert_eq!(again.stop_reason, HarnessStopReason::Refused);
    }

    /// A CLI's own refusal is on stderr. The turn keeps it (bounded to the
    /// tail) instead of draining it away, so a failing exit or an empty
    /// reply can be diagnosed from Magician's own log — with the session's
    /// grant redacted, since a URL error can echo it.
    #[tokio::test]
    async fn service_health_reads_failing_child_stderr_without_exposing_it() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho 'warming up' >&2\necho 'ERROR: code-mode host is disabled' >&2\n\
             echo 'HTTP 401 invalid_grant for plt_test' >&2\nexit 2\n",
        );
        let mut session =
            FakeEngineWithSpec::start_with(spec_with(&binary), &request(Duration::from_secs(10)));
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.assistant_text, "", "stderr never becomes the reply");
        assert_eq!(
            session.service_health(&settled),
            Some(Err(
                crate::magician_v2::realtime_events::ServiceFailure::Authentication
            ))
        );
        assert_eq!(
            session.last_stderr_tail(),
            "warming up\nERROR: code-mode host is disabled\nHTTP 401 invalid_grant for [grant]"
        );
    }

    #[tokio::test]
    async fn service_health_preserves_stderr_only_quota_after_planner_shutdown() {
        for planning_only in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let binary = fake_engine(
                dir.path(),
                "#!/bin/sh\necho 'Individual quota reached. Resets in 29m14s.' >&2\nexit 3\n",
            );
            let mut req = request(Duration::from_secs(10));
            req.planning_only = planning_only;
            let mut session = FakeEngineWithSpec::start_with(spec_with(&binary), &req);
            let settled = session
                .turn(
                    &HarnessTurnInput {
                        text: "go".into(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
                .unwrap();
            session.shutdown().await;
            assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
            assert!(settled.assistant_text.is_empty());
            assert_eq!(
                session.service_health(&settled),
                Some(Err(
                    crate::magician_v2::realtime_events::ServiceFailure::RateLimit
                ))
            );
        }
    }

    /// Only the last `STDERR_TAIL_BYTES` survive a chatty child, and a
    /// child that floods stderr is still drained to exit rather than
    /// blocked on a full pipe.
    #[tokio::test]
    async fn a_chatty_child_keeps_only_the_stderr_tail_and_still_exits() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\ni=0\nwhile [ $i -lt 4000 ]; do echo \"line $i of a long stderr flood\" >&2; \
             i=$((i+1)); done\necho 'last words' >&2\necho '{\"text\":\"fine\"}'\n",
        );
        let mut session =
            FakeEngineWithSpec::start_with(spec_with(&binary), &request(Duration::from_secs(10)));
        let settled = tokio::time::timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("a flooding child must not block the turn")
        .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "fine");
        let tail = session.last_stderr_tail();
        assert!(
            tail.ends_with("last words"),
            "the end of stderr survives: {tail}"
        );
        assert!(!tail.contains("line 0 of"), "the start is dropped");
        assert!(tail.len() <= STDERR_TAIL_BYTES);
    }

    /// Run a fake engine that prints exactly `stderr` on stderr, then a reply,
    /// and return the tail the session kept.
    async fn stderr_tail_after_exit(stderr: &[u8]) -> String {
        let dir = tempfile::tempdir().unwrap();
        let stderr_path = dir.path().join("stderr.bin");
        std::fs::write(&stderr_path, stderr).unwrap();
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\ncat '{}' >&2\necho '{{\"text\":\"fine\"}}'\n",
                stderr_path.display()
            ),
        );
        let mut session =
            FakeEngineWithSpec::start_with(spec_with(&binary), &request(Duration::from_secs(10)));
        let settled = tokio::time::timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("the child must exit")
        .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        session.last_stderr_tail().to_owned()
    }

    /// A grant straddling the tail cut is redacted whole, not left as a
    /// fragment: the raw window is one grant longer and the final cut lands
    /// after redaction.
    #[tokio::test]
    async fn a_grant_straddling_the_stderr_cut_is_redacted_not_split() {
        let grant = "plt_test";
        // The grant ends seven bytes into the last `STDERR_TAIL_BYTES`, so a
        // raw cut at that bound would keep only its last seven bytes.
        let straddle = 7;
        let mut stderr = vec![b'a'; 64];
        stderr.extend_from_slice(grant.as_bytes());
        stderr.extend(std::iter::repeat_n(b'b', STDERR_TAIL_BYTES - straddle));
        let tail = stderr_tail_after_exit(&stderr).await;
        assert!(tail.len() <= STDERR_TAIL_BYTES, "{}", tail.len());
        assert!(
            tail.starts_with("[grant]"),
            "the whole grant is replaced: {}",
            &tail[..tail.len().min(32)]
        );
        assert!(!tail.contains("test"), "no fragment of the grant survives");
        assert!(tail.ends_with("bbbb"));
    }

    /// A grant straddling the raw window's own head — begun before the
    /// window, ending inside it — leaves a suffix the literal replace cannot
    /// see; that fragment is dropped too.
    #[tokio::test]
    async fn a_grant_fragment_at_the_windows_head_is_dropped() {
        let grant = "plt_test";
        let raw_window = STDERR_TAIL_BYTES + grant.len();
        let mut stderr = vec![b'a'; 64];
        stderr.extend_from_slice(grant.as_bytes());
        // Five bytes of the grant fall inside the raw window.
        stderr.extend(std::iter::repeat_n(b'b', raw_window - 5));
        let tail = stderr_tail_after_exit(&stderr).await;
        assert!(tail.len() <= STDERR_TAIL_BYTES, "{}", tail.len());
        assert!(!tail.contains("test"), "no fragment of the grant survives");
        assert!(
            !tail.contains("[grant]"),
            "nothing whole was there to replace"
        );
        assert!(tail.chars().all(|c| c == 'b'), "only the filler remains");
    }

    #[test]
    fn a_grant_fragment_is_dropped_only_when_it_leads_the_text() {
        assert_eq!(
            drop_grant_fragment("_test and more", "plt_test"),
            " and more"
        );
        assert_eq!(drop_grant_fragment("t rest", "plt_test"), " rest");
        assert_eq!(
            drop_grant_fragment("plt_test whole", "plt_test"),
            "plt_test whole"
        );
        assert_eq!(drop_grant_fragment("clean", "plt_test"), "clean");
        assert_eq!(drop_grant_fragment("", "plt_test"), "");
        assert_eq!(drop_grant_fragment("anything", ""), "anything");
    }

    /// A turn that hangs until the wall clock is exactly where stderr
    /// matters: the ceiling stops the child and still keeps its tail. The
    /// ceiling is generous so a loaded machine still starts the shell and
    /// writes the line before it fires.
    #[tokio::test]
    async fn the_turn_timeout_keeps_the_stderr_tail() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho 'waiting on auth for plt_test' >&2\nsleep 60\n",
        );
        let mut session =
            FakeEngineWithSpec::start_with(spec_with(&binary), &request(Duration::from_secs(2)));
        let settled = tokio::time::timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("must settle on the ceiling")
        .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::TurnBudgetSpent);
        assert_eq!(
            session.last_stderr_tail(),
            "waiting on auth for [grant]",
            "the ceiling path keeps the tail, grant redacted"
        );
    }

    /// A harness that goes quiet without exiting holds the turn to its whole
    /// wall-clock ceiling; the idle bound ends it, and never as `Settled`, so
    /// a stuck child's partial text cannot become the run's deliverable.
    #[tokio::test]
    async fn a_silent_turn_ends_at_the_idle_bound_not_the_ceiling() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho '{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"partial\"}}'\nsleep 60\n",
        );
        let mut session = FakeEngineWithSpec::start_with(
            spec_with(&binary),
            // 2s, not 400ms: the idle window has to outlast a shell spawn and a
            // pipe read, and under a loaded machine (three rustc processes is
            // ordinary here) it did not — the bound fired before the `partial`
            // line was ever consumed, failing the text assertion below. The
            // proof is unchanged: 2s against a 60s ceiling still shows which
            // bound ended the turn.
            &request_with_idle(Duration::from_secs(60), Some(Duration::from_secs(2))),
        );
        let started = std::time::Instant::now();
        let settled = tokio::time::timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("the idle bound must settle the turn")
        .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::Stalled);
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the idle bound must fire long before the 60s ceiling"
        );
        // The text it managed to emit is kept for diagnosis; the stop reason
        // keeps the loop from publishing it as an answer.
        assert!(settled.assistant_text.contains("partial"));
    }

    /// Wall-clock bound: TurnBudgetSpent, promptly.
    #[tokio::test]
    async fn the_turn_timeout_ends_the_turn() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "#!/bin/sh\nsleep 60\n");
        let engine = FakeEngine(binary);
        let mut session = engine
            .start(&request(Duration::from_millis(150)))
            .await
            .unwrap();
        let settled = tokio::time::timeout(
            Duration::from_secs(10),
            session.turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            ),
        )
        .await
        .expect("must settle on the ceiling")
        .unwrap();
        assert_eq!(settled.stop_reason, HarnessStopReason::TurnBudgetSpent);
    }

    /// Config installs exactly once per session (duplicate `mcp add` is the
    /// failure mode the once-flag exists to prevent).
    #[tokio::test]
    async fn config_installs_once_per_session() {
        fn record_install(install: &HomeInstall) -> io::Result<()> {
            let marker = install.cwd.join("installs");
            let count = std::fs::read_to_string(&marker)
                .ok()
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0)
                + 1;
            std::fs::write(marker, count.to_string())
        }

        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "#!/bin/sh\necho '{}'\n");
        let marker = dir.path().join("installs");
        let program = binary.clone();
        let mut session = {
            let spec = OneShotEngineSpec {
                binary: std::path::PathBuf::from(&binary),
                argv_for_turn: Box::new(move |text, _resume| {
                    vec![program.clone(), text.to_string()]
                }),
                native_session_id_of: |_| None,
                text_delta_of: |_| None,
                env_for: |_| Vec::new(),
                install_config: record_install,
                remove_config: |_| {},
                use_isolated_home_as_cwd: false,
                planner_system_supplied: false,
                planner_response_boundary: None,
            };
            let mut install_request = request(Duration::from_secs(10));
            install_request.cwd = dir.path().to_path_buf();
            FakeEngineWithSpec::start_with(spec, &install_request)
        };
        for text in ["one", "two", "three"] {
            let _ = session
                .turn(
                    &HarnessTurnInput {
                        text: text.into(),
                        operator_steer: Vec::new(),
                    },
                    &HarnessStreamSink::drain(),
                )
                .await
                .unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(&marker).unwrap(),
            "1",
            "three turns, one install"
        );
    }

    /// A home the request hands in belongs to the conversation, not the
    /// session: release and Drop both leave it (and what the CLI persisted
    /// in it) for the next turn to resume into.
    #[tokio::test]
    async fn persistent_home_survives_release() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("conversation-home");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::write(home.join("session-state"), b"persisted by the cli").unwrap();
        let mut req = request(Duration::from_secs(10));
        req.native_home = Some(home.clone());
        let mut session = FakeEngineWithSpec::start_with(spec_with("fake"), &req);
        assert_eq!(session.home, home, "the session runs in the handed-in home");
        assert!(!session.owns_home);

        session.release().await;
        assert!(home.is_dir(), "release must not remove a persistent home");
        assert!(
            home.join("session-state").is_file(),
            "nor what the CLI persisted in it"
        );
        drop(session);
        assert!(
            home.is_dir(),
            "Drop must not remove a persistent home either"
        );
    }

    /// Without a handed-in home the pre-parity shape holds: a private temp
    /// dir per session, gone at release.
    #[tokio::test]
    async fn ephemeral_home_is_removed_on_release() {
        let mut session =
            FakeEngineWithSpec::start_with(spec_with("fake"), &request(Duration::from_secs(10)));
        let home = session.home.clone();
        assert!(session.owns_home);
        assert!(home.is_dir(), "the session creates its own home");
        assert!(home.starts_with(std::env::temp_dir()));

        session.release().await;
        assert!(!home.exists(), "release removes the session's own home");
    }

    /// Same for Drop without an explicit release (a dropped-paused session).
    #[test]
    fn ephemeral_home_is_removed_on_drop() {
        let session =
            FakeEngineWithSpec::start_with(spec_with("fake"), &request(Duration::from_secs(10)));
        let home = session.home.clone();
        assert!(home.is_dir());
        drop(session);
        assert!(!home.exists(), "Drop removes the session's own home");
    }

    /// A request that runs in a persistent home under `dir`, seeded with
    /// `resume` as the id to resume.
    fn warm_request(dir: &std::path::Path, resume: Option<&str>) -> HarnessSessionRequest {
        let mut req = request(Duration::from_secs(10));
        req.native_home = Some(dir.join("conversation-home"));
        req.resume_session_id = resume.map(str::to_string);
        req
    }

    async fn one_turn(session: &mut OneShotSession, text: &str) -> HarnessTurnSettled {
        session
            .turn(
                &HarnessTurnInput {
                    text: text.into(),
                    operator_steer: Vec::new(),
                },
                &HarnessStreamSink::drain(),
            )
            .await
            .unwrap()
    }

    /// A fake CLI that announces its session on an init line, then replies.
    const INIT_THEN_REPLY: &str =
        "#!/bin/sh\necho '{\"type\":\"init\",\"id\":\"sess-1\"}'\necho '{\"text\":\"hello\"}'\n";

    /// A fake CLI that echoes its argv back as the reply.
    const ECHO_ARGV: &str = "#!/bin/sh\nprintf '{\"text\":\"%s\"}\\n' \"$*\"\n";

    #[tokio::test]
    async fn decision_planner_instructions_reach_the_one_shot_cli() {
        let dir = tempfile::tempdir().unwrap();
        let captured = dir.path().join("prompt.txt");
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s' \"$1\" > '{}'\nprintf '{{\"text\":\"ok\"}}\\n'\n",
                captured.display()
            ),
        );
        for planning in [true, false] {
            let mut req = request(Duration::from_secs(10));
            req.planning_only = planning;
            req.system_prompt = "Propose calls only. Do not execute work.".into();
            let mut session = FakeEngineWithSpec::start_with(spec_with(&binary), &req);
            assert_eq!(
                one_turn(&mut session, "Read record 42")
                    .await
                    .assistant_text,
                "ok"
            );
            let actual = std::fs::read_to_string(&captured).unwrap();
            assert_eq!(
                actual,
                if planning {
                    "Propose calls only. Do not execute work.\n\nRead record 42"
                } else {
                    "Read record 42"
                }
            );
            session.shutdown().await;
        }
    }

    #[tokio::test]
    async fn decision_planner_uses_the_terminal_reply_without_streamed_commentary() {
        let dir = tempfile::tempdir().unwrap();
        let plan = serde_json::json!({"steps":[{"id":"one","call":{"tool":"lookup","arguments":{"key":42}}}]}).to_string();
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' '{}' '{}'\n",
                serde_json::json!({"delta":"I will propose the next call."}),
                serde_json::json!({"type":"result","result":plan}),
            ),
        );
        for planning in [true, false] {
            let mut req = request(Duration::from_secs(10));
            req.planning_only = planning;
            let mut session = FakeEngineWithSpec::start_with(streaming_spec(&binary), &req);
            let settled = one_turn(&mut session, "propose a call").await;
            assert_eq!(
                settled.assistant_text,
                if planning {
                    plan.as_str()
                } else {
                    "I will propose the next call."
                }
            );
            session.shutdown().await;
        }
    }

    /// In a persistent home the id the CLI announced is the turn's native
    /// session id, and the init line is never mistaken for the reply.
    #[tokio::test]
    async fn turn_reports_the_native_session_id_it_learned() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), INIT_THEN_REPLY);
        let mut session =
            FakeEngineWithSpec::start_with(resuming_spec(&binary), &warm_request(dir.path(), None));
        let settled = one_turn(&mut session, "go").await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "hello");
        assert_eq!(settled.native_session_id.as_deref(), Some("sess-1"));
        assert_eq!(
            session.resume_id_for_spawn(),
            Some("sess-1"),
            "a further turn on this session resumes what this one learned"
        );
    }

    /// The same CLI on a temp home reports no id: the session it names is
    /// deleted with the home, and a resume into a fresh home has no fallback.
    /// A seeded id is not passed to argv either.
    #[tokio::test]
    async fn a_temp_home_session_reports_no_native_id() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), INIT_THEN_REPLY);
        let mut req = request(Duration::from_secs(10));
        req.resume_session_id = Some("seeded-1".to_string());
        let mut session = FakeEngineWithSpec::start_with(resuming_spec(&binary), &req);
        assert!(session.owns_home);
        assert_eq!(session.resume_id_for_spawn(), None);
        let settled = one_turn(&mut session, "go").await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "hello");
        assert_eq!(settled.native_session_id, None);
    }

    /// A refused turn reports only the id learned this turn, never the
    /// seeded one: a seeded id the CLI could not resume must not be handed
    /// back, or every later turn refuses the same way. Every other stop
    /// falls back to the seeded id.
    #[tokio::test]
    async fn a_refused_turn_reports_only_the_id_learned_this_turn() {
        let dir = tempfile::tempdir().unwrap();
        let mut seeded = FakeEngineWithSpec::start_with(
            resuming_spec("fake"),
            &warm_request(dir.path(), Some("seeded-1")),
        );
        assert_eq!(seeded.reported_native_id(HarnessStopReason::Refused), None);
        for stop in [
            HarnessStopReason::Settled,
            HarnessStopReason::Cancelled,
            HarnessStopReason::TurnBudgetSpent,
        ] {
            assert_eq!(
                seeded.reported_native_id(stop).as_deref(),
                Some("seeded-1"),
                "{stop:?} keeps the seeded id"
            );
        }
        seeded.native_session_id = Some("learned-1".to_string());
        for stop in [
            HarnessStopReason::Refused,
            HarnessStopReason::Settled,
            HarnessStopReason::Cancelled,
            HarnessStopReason::TurnBudgetSpent,
        ] {
            assert_eq!(
                seeded.reported_native_id(stop).as_deref(),
                Some("learned-1"),
                "{stop:?} prefers the id learned this turn"
            );
        }
        let mut ephemeral = FakeEngineWithSpec::start_with(
            resuming_spec("fake"),
            &request(Duration::from_secs(10)),
        );
        ephemeral.resume_session_id = Some("seeded-1".to_string());
        ephemeral.native_session_id = Some("learned-1".to_string());
        for stop in [
            HarnessStopReason::Refused,
            HarnessStopReason::Settled,
            HarnessStopReason::Cancelled,
            HarnessStopReason::TurnBudgetSpent,
        ] {
            assert_eq!(
                ephemeral.reported_native_id(stop),
                None,
                "{stop:?} on a temp home reports nothing"
            );
        }

        // Through `turn`: a failing exit with no init line (the resume the
        // CLI refused) reports none and the next turn on this session runs
        // cold; a failing exit after the init line reports what it learned.
        let refused = fake_engine(dir.path(), "#!/bin/sh\nexit 3\n");
        let mut session = FakeEngineWithSpec::start_with(
            resuming_spec(&refused),
            &warm_request(dir.path(), Some("seeded-1")),
        );
        let settled = one_turn(&mut session, "go").await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.native_session_id, None);
        assert_eq!(
            session.resume_id_for_spawn(),
            None,
            "the seeded id is dropped"
        );

        let refused_after_init = fake_engine_named(
            dir.path(),
            "fake-refused-after-init",
            "#!/bin/sh\necho '{\"type\":\"init\",\"id\":\"sess-2\"}'\nexit 3\n",
        );
        let mut session = FakeEngineWithSpec::start_with(
            resuming_spec(&refused_after_init),
            &warm_request(dir.path(), Some("seeded-1")),
        );
        let settled = one_turn(&mut session, "go").await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.native_session_id.as_deref(), Some("sess-2"));
    }

    /// The CLI's resume flag rides argv only in a persistent home: a resume
    /// into a temp home names a session the CLI cannot find.
    #[tokio::test]
    async fn a_warm_turn_passes_the_resume_id_to_argv_only_in_a_persistent_home() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), ECHO_ARGV);

        let mut warm = FakeEngineWithSpec::start_with(
            resuming_spec(&binary),
            &warm_request(dir.path(), Some("warm-1")),
        );
        assert_eq!(warm.resume_id_for_spawn(), Some("warm-1"));
        assert_eq!(
            one_turn(&mut warm, "go").await.assistant_text,
            "go --resume warm-1"
        );

        let mut cold =
            FakeEngineWithSpec::start_with(resuming_spec(&binary), &warm_request(dir.path(), None));
        assert_eq!(one_turn(&mut cold, "go").await.assistant_text, "go");

        let mut req = request(Duration::from_secs(10));
        req.resume_session_id = Some("warm-1".to_string());
        let mut ephemeral = FakeEngineWithSpec::start_with(resuming_spec(&binary), &req);
        assert_eq!(
            one_turn(&mut ephemeral, "go").await.assistant_text,
            "go",
            "a temp-home session never resumes"
        );

        let mut empty = FakeEngineWithSpec::start_with(
            resuming_spec(&binary),
            &warm_request(dir.path(), Some("")),
        );
        assert_eq!(
            one_turn(&mut empty, "go").await.assistant_text,
            "go",
            "an empty seeded id is no id"
        );
    }

    /// The model flag is inserted before the final argv element. Codex's
    /// warm shape ends `… <resume id> <prompt>`, so the flag still lands
    /// after the id and before the prompt.
    #[tokio::test]
    async fn the_model_flag_lands_before_the_prompt_on_a_warm_codex_shape() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine_named(dir.path(), "codex", ECHO_ARGV);
        let program = binary.clone();
        let spec = OneShotEngineSpec {
            argv_for_turn: Box::new(move |text, resume| {
                let mut argv = vec![program.clone(), "exec".to_string()];
                if let Some(id) = resume {
                    argv.push("resume".to_string());
                    argv.push(id.to_string());
                }
                argv.push(text.to_string());
                argv
            }),
            ..resuming_spec(&binary)
        };
        let mut req = warm_request(dir.path(), Some("thread-1"));
        req.model = Some("model-x".to_string());
        let mut session = FakeEngineWithSpec::start_with(spec, &req);
        assert_eq!(
            one_turn(&mut session, "go").await.assistant_text,
            "exec resume thread-1 -m model-x go"
        );
    }

    #[tokio::test]
    async fn decision_planner_model_does_not_split_agy_schema_or_timeout() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine_named(dir.path(), "agy", ECHO_ARGV);
        let program = binary.clone();
        let spec = OneShotEngineSpec {
            argv_for_turn: Box::new(move |text, _| {
                vec![
                    program.clone(),
                    "-p".into(),
                    text.into(),
                    "--print-timeout".into(),
                    "0".into(),
                    "--json-schema".into(),
                    "{}".into(),
                ]
            }),
            ..spec_with(&binary)
        };
        let mut req = request(Duration::from_secs(10));
        req.model = Some("gemini-3.7-flash-high".into());
        let mut session = FakeEngineWithSpec::start_with(spec, &req);
        assert_eq!(
            one_turn(&mut session, "go").await.assistant_text,
            "-p go --print-timeout 0 --json-schema {} --model gemini-3.7-flash-high"
        );
    }

    #[test]
    fn decision_planner_reads_structured_terminal_output_without_presentation_text() {
        let value = serde_json::json!({"event":"result","result":{
            "response":"Done\\n{presentation noise}", "structured_output":{"answer":"Done"}
        }});
        assert_eq!(
            extract_turn_text(&value, true).unwrap(),
            r#"{"answer":"Done"}"#
        );
        assert_eq!(
            extract_turn_text(&value, false).unwrap(),
            "Done\\n{presentation noise}"
        );
    }

    /// When no line announced the id, the whole-stdout document is asked
    /// too: grok's `--output-format json` document names its own session.
    #[tokio::test]
    async fn the_native_id_is_read_from_the_whole_document_when_no_line_announced_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("reply.json"), PRETTY_PRINTED_REPLY).unwrap();
        let binary = fake_engine(
            dir.path(),
            &format!(
                "#!/bin/sh\ncat '{}'\n",
                dir.path().join("reply.json").display()
            ),
        );
        let spec = OneShotEngineSpec {
            native_session_id_of: |value| {
                value
                    .get("sessionId")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            },
            ..spec_with(&binary)
        };
        let mut session = FakeEngineWithSpec::start_with(spec, &warm_request(dir.path(), None));
        let settled = one_turn(&mut session, "ping").await;
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "PONG");
        assert_eq!(settled.native_session_id.as_deref(), Some("sess-1"));
    }

    /// The settle rule on its own: streamed text wins whenever there is
    /// any, collected lines join as before, whitespace alone is no stream,
    /// and both stay bounded with the cut marked rather than silent.
    #[test]
    fn a_streamed_reply_wins_at_settle_and_stays_bounded() {
        let mut collected = TurnReply::default();
        assert!(collected.is_empty());
        collected.push_text("line one");
        collected.push_text("");
        collected.push_text("line two");
        assert!(!collected.is_empty());
        assert_eq!(
            collected.settle(HarnessStopReason::Settled),
            "line one\nline two"
        );

        let mut streamed = TurnReply::default();
        streamed.push_delta("o");
        streamed.push_delta("k");
        assert!(!streamed.is_empty());
        streamed.push_text("ok");
        assert_eq!(
            streamed.settle(HarnessStopReason::Settled),
            "ok",
            "the final line is not joined on"
        );

        let mut newline_only = TurnReply::default();
        newline_only.push_delta("\n");
        assert!(newline_only.is_empty(), "whitespace alone is no stream");
        newline_only.push_text("ok");
        assert_eq!(
            newline_only.settle(HarnessStopReason::Settled),
            "ok",
            "a stream of whitespace settles with the collected text"
        );

        let mut flood = TurnReply::default();
        flood.push_delta(&"x".repeat(MAX_REPLY_BYTES + 1));
        flood.push_delta("more");
        let text = flood.settle(HarnessStopReason::Settled);
        assert!(
            text.ends_with(PLANE_TURN_RESULT_CUT_MARK),
            "a cut is marked"
        );
        assert_eq!(
            text.len(),
            MAX_REPLY_BYTES + PLANE_TURN_RESULT_CUT_MARK.len()
        );
        assert!(!text.contains("more"), "past the cut nothing is appended");

        let mut chatty = TurnReply::default();
        chatty.push_text(&"y".repeat(MAX_REPLY_BYTES + 1));
        chatty.push_text("more");
        let text = chatty.settle(HarnessStopReason::Settled);
        assert!(text.ends_with(PLANE_TURN_RESULT_CUT_MARK));
        assert!(!text.contains("more"));
    }

    /// A failing exit keeps the CLI's final error text after the streamed
    /// text; a final line that only repeats the stream (or the head of a
    /// cut one) is not joined on, and a settled turn never appends.
    #[test]
    fn a_failing_settle_keeps_the_final_text_after_the_stream() {
        let mut failed = TurnReply::default();
        failed.push_delta("partial ");
        failed.push_delta("answer");
        failed.push_text("error: rate limited");
        assert_eq!(
            failed.settle(HarnessStopReason::Refused),
            "partial answer\nerror: rate limited"
        );

        let mut repeated = TurnReply::default();
        repeated.push_delta("same ");
        repeated.push_delta("text\n");
        repeated.push_text("same text");
        assert_eq!(
            repeated.settle(HarnessStopReason::Refused),
            "same text\n",
            "a final line repeating the stream is not joined on"
        );

        let mut cut = TurnReply::default();
        let long = "z".repeat(MAX_REPLY_BYTES + 100);
        cut.push_delta(&long);
        cut.push_text(&long);
        let text = cut.settle(HarnessStopReason::Refused);
        assert_eq!(
            text.len(),
            MAX_REPLY_BYTES + PLANE_TURN_RESULT_CUT_MARK.len(),
            "the final line repeating a cut stream's head is not joined on"
        );

        let mut settled = TurnReply::default();
        settled.push_delta("ok");
        settled.push_text("something else");
        assert_eq!(settled.settle(HarnessStopReason::Settled), "ok");
        let mut ceiling = TurnReply::default();
        ceiling.push_delta("ok");
        ceiling.push_text("something else");
        assert_eq!(ceiling.settle(HarnessStopReason::TurnBudgetSpent), "ok");
    }

    /// Run one turn on `spec` with a channel sink; the deltas the sink
    /// received, in order, alongside what settled.
    async fn one_turn_with_sink(spec: OneShotEngineSpec) -> (HarnessTurnSettled, Vec<String>) {
        let mut session = FakeEngineWithSpec::start_with(spec, &request(Duration::from_secs(10)));
        let (sink, mut rx) = HarnessStreamSink::channel();
        let settled = session
            .turn(
                &HarnessTurnInput {
                    text: "go".into(),
                    operator_steer: Vec::new(),
                },
                &sink,
            )
            .await
            .unwrap();
        drop(sink);
        let mut deltas = Vec::new();
        while let Some(delta) = rx.recv().await {
            deltas.push(delta);
        }
        (settled, deltas)
    }

    /// A turn that streamed settles with exactly the streamed text: the
    /// sink saw each delta once, in order, and the final line that repeats
    /// the reply whole is not joined onto it.
    #[tokio::test]
    async fn a_streamed_turn_settles_with_the_streamed_text_once() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho '{\"delta\":\"o\"}'\necho '{\"delta\":\"k\"}'\n\
             echo '{\"result\":\"ok\"}'\n",
        );
        let (settled, deltas) = one_turn_with_sink(streaming_spec(&binary)).await;
        assert_eq!(deltas, ["o", "k"], "each delta once, in order");
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(
            settled.assistant_text, "ok",
            "the streamed text alone, never joined with the final line"
        );
    }

    /// The same streaming-capable spec on a child that streamed nothing
    /// settles the way it always did: the final line's text, and the sink
    /// stays silent.
    #[tokio::test]
    async fn an_unstreamed_turn_still_reads_the_final_line() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(dir.path(), "#!/bin/sh\necho '{\"result\":\"ok\"}'\n");
        let (settled, deltas) = one_turn_with_sink(streaming_spec(&binary)).await;
        assert!(deltas.is_empty(), "nothing was streamed: {deltas:?}");
        assert_eq!(settled.stop_reason, HarnessStopReason::Settled);
        assert_eq!(settled.assistant_text, "ok");
    }

    /// A child that streamed, then printed its error as the final line and
    /// exited non-zero: the turn is refused, the sink saw the deltas, and
    /// the error text survives after the streamed text instead of being
    /// dropped as a repeat.
    #[tokio::test]
    async fn a_streamed_turn_that_fails_keeps_the_final_error_text() {
        let dir = tempfile::tempdir().unwrap();
        let binary = fake_engine(
            dir.path(),
            "#!/bin/sh\necho '{\"delta\":\"o\"}'\necho '{\"delta\":\"k\"}'\n\
             echo '{\"result\":\"error: rate limited\"}'\nexit 3\n",
        );
        let (settled, deltas) = one_turn_with_sink(streaming_spec(&binary)).await;
        assert_eq!(deltas, ["o", "k"]);
        assert_eq!(settled.stop_reason, HarnessStopReason::Refused);
        assert_eq!(settled.assistant_text, "ok\nerror: rate limited");
    }

    /// The per-line collector reads nothing from a streaming CLI's delta
    /// and whole-message lines: only its final line carries the reply, and
    /// the streamed text wins over that.
    #[test]
    fn extract_text_ignores_streaming_lines() {
        assert_eq!(
            extract_text(
                br#"{"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"PO"}},"session_id":"s"}"#
            ),
            None
        );
        assert_eq!(
            extract_text(
                br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"PONG"}]}}"#
            ),
            None
        );
        assert_eq!(
            extract_text(
                br#"{"event":"step_update","step_update":{"step_type":"agent_response","text_delta":"ok"}}"#
            ),
            None
        );
        assert_eq!(
            extract_text(br#"{"type":"result","subtype":"success","result":"PONG"}"#).as_deref(),
            Some("PONG")
        );
    }

    /// A minimal engine to drive the shared spec in tests.
    #[derive(Debug)]
    struct FakeEngine(String);
    #[async_trait]
    impl HarnessEngine for FakeEngine {
        fn name(&self) -> &'static str {
            "fake"
        }
        fn capabilities(
            &self,
        ) -> crate::magician_v2::execution::plane::engine::HarnessCapabilities {
            crate::magician_v2::execution::plane::engine::HarnessCapabilities {
                supports_resume: false,
                tools_list_changed: false,
                streams_text_deltas: false,
                native_tool_posture:
                    crate::magician_v2::execution::plane::engine::NativeToolPosture::Live,
            }
        }
        async fn start(
            &self,
            req: &HarnessSessionRequest,
        ) -> Result<Box<dyn HarnessSession>, HarnessError> {
            Ok(Box::new(OneShotSession::new(spec_with(&self.0), req)))
        }
    }

    struct FakeEngineWithSpec;
    impl FakeEngineWithSpec {
        fn start_with(spec: OneShotEngineSpec, req: &HarnessSessionRequest) -> OneShotSession {
            OneShotSession::new(spec, req)
        }
    }
}

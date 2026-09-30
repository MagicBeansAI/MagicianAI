//! The production [`EvalExecutor`]: a lane runs as an ordinary magician
//! execution.
//!
//! [`super::runner`] deliberately talks to the task system through a trait so it
//! can be tested without a runtime. This module is the other half — the one that
//! actually calls into the execution API. It is a thin adapter and nothing more:
//! every scheduling, cancellation and spend decision stays where it already
//! lives.
//!
//! # Why it mirrors `create_execution` rather than inventing a path
//!
//! `web_api::create_execution`'s `skip_planning` branch is the reference
//! implementation, and this follows it step for step —
//! [`create_task_backed_direct_root_run`] to mint the task + execution shell,
//! then `execute_agentic_direct_with_outcome` to drive it, then
//! `persist_runtime_execution_outcome_by_execution_id` to close the V3 record.
//! Any divergence would be a second, subtly different way to create an execution
//! and would rot against the first.
//!
//! The execution is created `internal: true` (it is machinery, not a commitment
//! the user is tracking, so it belongs in `internal_tasks/` and out of `/tasks`)
//! and `skip_planning: true` (there is nothing to plan — the goal is one
//! command).
//!
//! # Why the title is passed through untouched
//!
//! [`super::runner::execution_title_for_lane`] is the ONLY key from a live
//! execution back to a lane, because the title is the only lane-shaped field an
//! execution listing carries. `direct_root_task_title` returns a non-empty title
//! verbatim, so what the runner built is what lands on the execution record and
//! what [`lane_id_from_execution_title`] can read back. Reformatting, truncating
//! or prefixing it here would silently disable concurrency detection — and the
//! symptom would be a lane billed twice, not an error.
//!
//! # Why an exit code is earned rather than assumed
//!
//! The single most expensive mistake this module could make is reporting
//! "passed" for a run nobody judged. Two things make that easy to do by
//! accident:
//!
//! * `ShellState::last_exit_code` is `Some(0)` whenever the *tool call*
//!   succeeded — pack and DuckDB actions hardcode it — so it is NOT the make
//!   command's exit code and can never be read as one on its own.
//! * an `AgenticOutcome::Success` means the agent finished its goal, which was
//!   "report the exit code", not "the eval passed".
//!
//! So the exit code is recovered from evidence, in confidence order, by
//! [`exit_code_from_evidence`], and when no branch of that function is
//! satisfied the outcome is [`EvalCommandOutcome::indeterminate`] — which the
//! runner records as `Interrupted`. An eval that was not judged reads as not
//! judged.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tracing::{debug, warn};

use magician::magician_v2::artifact_v2::service::ArtifactV2Service;
use magician::magician_v2::execution::agentic::{AgenticOutcome, EnvironmentState};
use magician::magician_v2::execution::AgenticContextOverrides;
use magician::magician_v2::orchestrator::MagicianV2Orchestrator;
use magician::magician_v2::storage::{PaginationParams, TaskCreatedBy, TurnDirection};
use magician::magician_v2::task_run_factory::create_task_backed_direct_root_run;

use super::runner::{
    lane_id_from_execution_title, EvalCommandOutcome, EvalExecutionSpec, EvalExecutor,
    StartedExecution,
};

/// Executions read per page while looking for a lane's live run. The store's
/// own ceiling is 200 (`PaginationParams::new`), so asking for more silently
/// gets 200 anyway.
const EXECUTION_SCAN_PAGE_SIZE: usize = 200;

/// How many of those pages are read before the scan gives up.
///
/// The listing is ordered by `updated_at` descending, so a run that is actually
/// in flight — one whose execution record is being written to continuously —
/// sits at the very front of the first page. Anything this far down is an
/// execution that stopped being touched thousands of executions ago: a crashed
/// process, not a live run, and starting the lane again is the correct answer
/// for one of those.
const MAX_EXECUTION_SCAN_PAGES: usize = 25;

/// Outbound turns folded into the run's report text. The last one carries the
/// agent's verdict; a couple more give the tail of its reasoning for a failing
/// lane without dragging the whole conversation into the record.
const MAX_REPORT_TURNS: usize = 3;

/// The marker the native shell executor puts on a non-zero exit
/// (`native_executors.rs`: `Command failed with exit code {code}: …`). This is
/// the one exit code in the whole pipeline that is machine-generated from the
/// process's own status, which is why it is trusted first.
const EXECUTOR_FAILURE_MARKER: &str = "command failed with exit code ";

/// What one started-but-not-yet-awaited lane needs at `wait` time.
///
/// [`EvalExecutor::wait`] is handed only an execution id, so everything else the
/// agentic call needs has to be remembered from `start`. Keeping it here rather
/// than widening the trait keeps the runner's seam narrow and testable.
#[derive(Debug, Clone)]
struct PendingEvalRun {
    /// The `make` target, used to prove that the command whose exit code we are
    /// about to read is actually this lane's command.
    target: String,
    goal: String,
    max_iterations: usize,
    task_id: String,
    /// The V3 execution id. Equal to the runtime execution id today
    /// (`create_v3_task_execution_shell` creates the runtime execution *with*
    /// the V3 id), kept separate so that is an observation rather than a
    /// dependency.
    v3_execution_id: String,
}

/// Runs eval lanes through the ordinary task system.
pub struct TaskBackedEvalExecutor {
    orchestrator: Arc<MagicianV2Orchestrator>,
    artifact_v2: Arc<ArtifactV2Service>,
    /// Runtime execution id -> what `wait` will need. A `std::sync::Mutex` is
    /// safe here because it is never held across an `await`.
    pending: Mutex<HashMap<String, PendingEvalRun>>,
}

impl TaskBackedEvalExecutor {
    pub fn new(
        orchestrator: Arc<MagicianV2Orchestrator>,
        artifact_v2: Arc<ArtifactV2Service>,
    ) -> Self {
        Self {
            orchestrator,
            artifact_v2,
            pending: Mutex::new(HashMap::new()),
        }
    }

    /// The agent the eval execution is owned by — the same resolution
    /// `create_execution` performs, so an eval run is owned by whoever owns
    /// ordinary work in this install.
    async fn owner_agent_id(&self) -> String {
        let fallback = || magician::magician_v2::chat::DEFAULT_AGENT_ID.to_string();
        match self.orchestrator.get_definition_store() {
            Some(store) => match store.get_primary_agent().await {
                Ok(Some(record)) => record.definition.agent_id,
                _ => fallback(),
            },
            None => fallback(),
        }
    }

    /// The tail of what the agent said, which is where it reports the verdict.
    ///
    /// Best effort by construction: an unreadable conversation costs a legible
    /// log, not a run record. Turns are stored in document order and paginated
    /// from the front, so the *last* few need the total first.
    async fn final_report_text(&self, execution_id: &str) -> String {
        let probe = self
            .orchestrator
            .get_turns(
                execution_id,
                PaginationParams::new(Some(1), Some(0)),
                Some(TurnDirection::Outbound),
            )
            .await;
        let probe = match probe {
            Ok(page) => page,
            Err(error) => {
                debug!(
                    execution_id = %execution_id,
                    %error,
                    "eval run: could not read the agent's turns; the log tail will be state-only"
                );
                return String::new();
            },
        };
        let total = probe.pagination.total;
        if total == 0 {
            return String::new();
        }
        let offset = total.saturating_sub(MAX_REPORT_TURNS);
        let tail = self
            .orchestrator
            .get_turns(
                execution_id,
                PaginationParams::new(Some(MAX_REPORT_TURNS), Some(offset)),
                Some(TurnDirection::Outbound),
            )
            .await;
        let mut turns = match tail {
            Ok(page) => page.items,
            Err(_) => probe.items,
        };
        turns.sort_by_key(|turn| turn.created_at);
        turns
            .into_iter()
            .map(|turn| turn.text)
            .filter(|text| !text.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[async_trait]
impl EvalExecutor for TaskBackedEvalExecutor {
    /// Walks the scope's executions newest-first, keying them back to lanes
    /// through their titles.
    ///
    /// `ExecutionSummary` carries no `task_id` — only the heavier
    /// `ExecutionRun` does — so a title match is re-fetched to get the id the
    /// caller needs. Every query failure becomes `Err`, never `Ok(None)`:
    /// "I could not see whether this lane is running" must not be answered as
    /// "it is idle", because that answer bills an expensive lane twice.
    async fn running_task_for_lane(
        &self,
        principal: &str,
        workspace: &str,
        lane_id: &str,
    ) -> Result<Option<String>, String> {
        let mut offset = 0usize;
        for _ in 0..MAX_EXECUTION_SCAN_PAGES {
            let page = self
                .orchestrator
                .list_executions(
                    principal,
                    workspace,
                    PaginationParams::new(Some(EXECUTION_SCAN_PAGE_SIZE), Some(offset)),
                )
                .await
                .map_err(|error| format!("could not list executions: {error}"))?;

            for summary in &page.items {
                if summary.waiting_state.is_terminal() {
                    continue;
                }
                let Some(title) = summary.title.as_deref() else {
                    continue;
                };
                if lane_id_from_execution_title(title) != Some(lane_id) {
                    continue;
                }
                let execution =
                    self.orchestrator
                        .get_execution(&summary.id)
                        .await
                        .map_err(|error| {
                            format!(
                                "could not read execution `{}` while checking whether lane \
                             `{lane_id}` is running: {error}",
                                summary.id
                            )
                        })?;
                // Re-checked against the freshly loaded record: the listing may
                // be a moment stale, and refusing a run because of a stale
                // listing is a nuisance we can cheaply avoid.
                if execution.waiting_state.is_terminal() {
                    continue;
                }
                // Every eval execution is task-backed, so the fallback is
                // unreachable. It exists because the alternative to naming
                // *something* is returning `None`, which would start a second
                // run of a lane that is demonstrably already running.
                return Ok(Some(
                    execution
                        .task_id
                        .clone()
                        .unwrap_or_else(|| execution.id.clone()),
                ));
            }

            if !page.pagination.has_more {
                return Ok(None);
            }
            offset = offset.saturating_add(page.items.len().max(1));
        }

        warn!(
            lane_id = %lane_id,
            scanned = MAX_EXECUTION_SCAN_PAGES * EXECUTION_SCAN_PAGE_SIZE,
            "eval in-flight check stopped short; a live run would have sorted to the front"
        );
        Ok(None)
    }

    async fn start(&self, spec: EvalExecutionSpec) -> Result<StartedExecution, String> {
        let owner_agent_id = self.owner_agent_id().await;
        let ui_thread_id = magician::magician_v2::artifact_v2::models::default_ui_thread_id();
        let (task_id, v3_execution_id, runtime_execution_id) = create_task_backed_direct_root_run(
            self.artifact_v2.as_ref(),
            &self.orchestrator,
            &spec.principal,
            &spec.workspace,
            &ui_thread_id,
            // Verbatim: this is what makes the execution findable as this
            // lane's run. `direct_root_task_title` returns a non-empty title
            // unchanged, so nothing between here and the execution record
            // rewrites it.
            Some(spec.title.as_str()),
            &spec.goal,
            &owner_agent_id,
            TaskCreatedBy::User,
            // Not a debug-page run: no `__system__` creator marker.
            false,
            // Internal: eval machinery is not one of the user's commitments, so
            // it belongs in `internal_tasks/` rather than `/tasks`.
            true,
        )
        .await?;

        self.pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(
                runtime_execution_id.clone(),
                PendingEvalRun {
                    target: spec.target.clone(),
                    goal: spec.goal.clone(),
                    max_iterations: spec.max_iterations,
                    task_id: task_id.clone(),
                    v3_execution_id,
                },
            );

        Ok(StartedExecution {
            task_id,
            execution_id: runtime_execution_id,
        })
    }

    async fn wait(&self, execution_id: &str) -> Result<EvalCommandOutcome, String> {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(execution_id)
            .ok_or_else(|| {
                format!("no eval run was started for execution `{execution_id}` in this process")
            })?;

        let overrides = AgenticContextOverrides {
            task_id: Some(pending.task_id.clone()),
            execution_id: Some(pending.v3_execution_id.clone()),
            ..AgenticContextOverrides::default()
        };
        let outcome = self
            .orchestrator
            .execute_agentic_direct_with_outcome(
                execution_id,
                &pending.goal,
                Some(pending.max_iterations),
                None,
                Some(overrides),
            )
            .await;

        // Closed before the outcome is unwrapped: a run that errored still has
        // a V3 record that must stop reading as in-flight.
        if let Err(error) = self
            .artifact_v2
            .persist_runtime_execution_outcome_by_execution_id(&pending.v3_execution_id)
            .await
        {
            warn!(
                execution_id = %pending.v3_execution_id,
                %error,
                "eval run: failed to persist the V3 execution outcome"
            );
        }

        // An `Err` here is "we lost track of the run", which the runner records
        // as `Interrupted`. It is never a verdict about the eval.
        let outcome = outcome?;
        let report_text = self.final_report_text(execution_id).await;
        let evidence = RunEvidence::from_outcome(&outcome, report_text);
        Ok(EvalCommandOutcome {
            exit_code: exit_code_from_evidence(&evidence, &pending.target),
            log: evidence.render_log(),
        })
    }
}

/// Everything observable about a finished lane run, flattened out of the
/// agentic types so the verdict logic is pure and testable.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunEvidence {
    /// The `AgenticOutcome` variant, for the log.
    pub outcome_kind: String,
    /// Whether that variant was `Success`. NOT "the eval passed" — it means the
    /// agent finished the goal it was given, which was to *report* a verdict.
    pub succeeded: bool,
    pub last_command: Option<String>,
    pub last_stdout: Option<String>,
    pub last_stderr: Option<String>,
    /// The tool's own exit code field. Unreliable on its own — several action
    /// kinds hardcode `Some(0)` on tool success — so it is only ever read
    /// together with `last_command`.
    pub last_exit_code: Option<i32>,
    /// The tail of what the agent said.
    pub report_text: String,
}

impl RunEvidence {
    pub fn from_outcome(outcome: &AgenticOutcome, report_text: String) -> Self {
        let (outcome_kind, state) = match outcome {
            AgenticOutcome::Success { final_state, .. } => ("success", Some(final_state)),
            AgenticOutcome::Failed { last_state, .. } => ("failed", Some(last_state)),
            AgenticOutcome::MaxIterationsReached { last_state, .. } => {
                ("max_iterations_reached", Some(last_state))
            },
            AgenticOutcome::LoopDetected { last_state, .. } => ("loop_detected", Some(last_state)),
            // Pauses and budget stops carry no environment state worth reading.
            // They are not verdicts either way, which the empty evidence below
            // makes true by construction.
            _ => ("not_terminal", None),
        };

        let mut evidence = Self {
            outcome_kind: outcome_kind.to_string(),
            succeeded: outcome_kind == "success",
            report_text,
            ..Self::default()
        };
        if let Some(EnvironmentState::Shell(shell)) = state {
            evidence.last_command = shell.last_command.clone();
            evidence.last_stdout = shell.last_stdout.clone();
            evidence.last_stderr = shell.last_stderr.clone();
            evidence.last_exit_code = shell.last_exit_code;
        }
        evidence
    }

    /// The diagnostic excerpt stored on the run. Bounded by the store, so this
    /// does not have to pre-trim.
    pub fn render_log(&self) -> String {
        let mut log = format!("[eval] agentic outcome: {}\n", self.outcome_kind);
        if let Some(command) = self.last_command.as_deref() {
            log.push_str(&format!("[eval] last command: {command}\n"));
        }
        if let Some(code) = self.last_exit_code {
            // Labelled as the *tool's* code so nobody later reads it as the
            // make command's verdict.
            log.push_str(&format!("[eval] tool-reported exit code: {code}\n"));
        }
        for (label, text) in [
            ("stdout", self.last_stdout.as_deref()),
            ("stderr", self.last_stderr.as_deref()),
            ("agent report", Some(self.report_text.as_str())),
        ] {
            let Some(text) = text else { continue };
            if text.trim().is_empty() {
                continue;
            }
            log.push_str(&format!("--- {label} ---\n{text}\n"));
        }
        log
    }
}

/// The exit code of this lane's `make` command, or `None` when nothing proved
/// one.
///
/// Confidence order, and each step exists because the step after it is weaker:
///
/// 1. the native shell executor's own failure string, which is generated from
///    the process's exit status and appears nowhere else;
/// 2. the agent's report, which the goal explicitly asks for — accepted only
///    when every exit code it mentions agrees, so a report that says both `0`
///    and `2` yields nothing rather than a coin flip;
/// 3. a `Success` outcome whose last command was *this lane's* target and whose
///    tool exit code was zero. The command check is load-bearing: without it
///    any pack action's hardcoded `Some(0)` would read as a passing eval.
///
/// `None` means the run is [`EvalCommandOutcome::indeterminate`] — recorded as
/// `Interrupted`, never as a pass.
pub fn exit_code_from_evidence(evidence: &RunEvidence, target: &str) -> Option<i32> {
    if let Some(code) = evidence
        .last_stderr
        .as_deref()
        .and_then(exit_code_from_executor_error)
    {
        return Some(code);
    }
    if let Some(code) = sole_reported_exit_code(&evidence.report_text) {
        return Some(code);
    }
    if evidence.succeeded
        && evidence.last_exit_code == Some(0)
        && evidence
            .last_command
            .as_deref()
            .is_some_and(|command| command_ran_target(command, target))
    {
        return Some(0);
    }
    None
}

/// The exit code out of `Command failed with exit code {code}: …`.
fn exit_code_from_executor_error(text: &str) -> Option<i32> {
    let lower = text.to_ascii_lowercase();
    let at = lower.find(EXECUTOR_FAILURE_MARKER)? + EXECUTOR_FAILURE_MARKER.len();
    parse_leading_i32(&lower[at..])
}

/// The one exit code the report states, or `None` when it states none — or more
/// than one.
///
/// Ambiguity is refused rather than resolved. A report mentioning two different
/// codes is a report we did not understand, and guessing which one was the
/// verdict is exactly the kind of confident wrongness this feature exists to
/// avoid.
fn sole_reported_exit_code(text: &str) -> Option<i32> {
    // ASCII-lowercased, so every byte index below is still a char boundary of
    // the lowered string.
    let lower = text.to_ascii_lowercase();
    let mut found: Vec<i32> = Vec::new();
    for marker in ["exit code", "exit status", "exited with"] {
        let mut from = 0usize;
        while let Some(index) = lower[from..].find(marker) {
            let after = from + index + marker.len();
            if let Some(code) = parse_exit_code_after(&lower[after..]) {
                found.push(code);
            }
            from = after;
        }
    }
    found.sort_unstable();
    found.dedup();
    match found.as_slice() {
        [only] => Some(*only),
        _ => None,
    }
}

/// The number following an exit-code phrase, allowing the punctuation and the
/// one filler word English puts there (`exit code: 2`, `exit code of 2`,
/// `exit status was 1`, ``exit code `0` ``).
fn parse_exit_code_after(rest: &str) -> Option<i32> {
    let rest = rest.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '=' | '`' | '*' | '"' | '\'' | ',')
    });
    if let Some(code) = parse_leading_i32(rest) {
        return Some(code);
    }
    let split_at = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let (word, tail) = rest.split_at(split_at);
    if !matches!(word, "of" | "was" | "is" | "returned" | "value") {
        return None;
    }
    let tail = tail.trim_start_matches(|c: char| {
        c.is_whitespace() || matches!(c, ':' | '=' | '`' | '*' | '"' | '\'')
    });
    parse_leading_i32(tail)
}

/// A leading (optionally negative) integer, or `None`.
fn parse_leading_i32(text: &str) -> Option<i32> {
    let text = text.trim_start();
    let bytes = text.as_bytes();
    let mut end = usize::from(bytes.first() == Some(&b'-'));
    let digits_start = end;
    while end < bytes.len() && bytes[end].is_ascii_digit() {
        end += 1;
    }
    if end == digits_start {
        return None;
    }
    text[..end].parse().ok()
}

/// Whether `command` is plausibly the invocation of this lane's target.
///
/// A containment check, because the recipe is wrapped (`cd … && make target`,
/// `make -C … target`) in ways this module should not try to model. What it
/// rules out is the case that matters: a *different* action's synthetic
/// `Some(0)` being read as this lane's pass.
fn command_ran_target(command: &str, target: &str) -> bool {
    !target.is_empty() && command.contains(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn evidence() -> RunEvidence {
        RunEvidence {
            outcome_kind: "success".to_string(),
            succeeded: true,
            ..RunEvidence::default()
        }
    }

    /// The whole point: a run nobody judged must not read as a pass.
    #[test]
    fn no_evidence_is_no_exit_code() {
        assert_eq!(
            exit_code_from_evidence(&RunEvidence::default(), "eval-x"),
            None
        );
        assert_eq!(exit_code_from_evidence(&evidence(), "eval-x"), None);
    }

    /// Several action kinds hardcode `last_exit_code: Some(0)` when the *tool
    /// call* succeeded. Without the command check that would be a green lane
    /// nobody earned.
    #[test]
    fn a_tool_success_on_someone_elses_command_is_not_a_pass() {
        let mut run = evidence();
        run.last_exit_code = Some(0);
        run.last_command = Some("pack:browser".to_string());
        assert_eq!(exit_code_from_evidence(&run, "eval-monitor-golden"), None);

        run.last_command = Some("make eval-monitor-golden".to_string());
        assert_eq!(
            exit_code_from_evidence(&run, "eval-monitor-golden"),
            Some(0)
        );
    }

    /// A zero from the tool only counts when the agent actually finished.
    #[test]
    fn a_zero_needs_a_successful_outcome_too() {
        let mut run = evidence();
        run.succeeded = false;
        run.outcome_kind = "max_iterations_reached".to_string();
        run.last_exit_code = Some(0);
        run.last_command = Some("make eval-monitor-golden".to_string());
        assert_eq!(exit_code_from_evidence(&run, "eval-monitor-golden"), None);
    }

    /// The native executor turns a non-zero exit into an error string. That
    /// string is the only machine-generated exit code in the pipeline.
    #[test]
    fn the_executors_failure_string_is_the_strongest_evidence() {
        let mut run = evidence();
        run.last_stderr =
            Some("Command failed with exit code 2: make: *** [eval-x] Error 2".to_string());
        // Even though the tool-level fields look like a clean success.
        run.last_exit_code = Some(0);
        run.last_command = Some("make eval-x".to_string());
        assert_eq!(exit_code_from_evidence(&run, "eval-x"), Some(2));
    }

    #[test]
    fn the_agents_reported_exit_code_is_read_in_the_shapes_it_writes() {
        for (report, expected) in [
            ("The command exited with exit code 0.", Some(0)),
            ("exit code: 1", Some(1)),
            ("It finished with an exit code of 3", Some(3)),
            ("exit status was 137", Some(137)),
            ("exited with 2", Some(2)),
            ("exit code `0`", Some(0)),
            ("no verdict was reached", None),
        ] {
            let mut run = evidence();
            run.report_text = report.to_string();
            assert_eq!(
                exit_code_from_evidence(&run, "eval-x"),
                expected,
                "report: {report}"
            );
        }
    }

    /// Two different codes in one report is a report we did not understand.
    /// Picking one would be a verdict invented by a parser.
    #[test]
    fn a_report_naming_two_different_exit_codes_yields_none() {
        let mut run = evidence();
        run.report_text =
            "exit code 0 would mean success, but the command gave exit code 2".to_string();
        assert_eq!(exit_code_from_evidence(&run, "eval-x"), None);

        // The same code twice is not ambiguous.
        run.report_text = "exit code 2. To repeat: exit code 2.".to_string();
        assert_eq!(exit_code_from_evidence(&run, "eval-x"), Some(2));
    }

    #[test]
    fn a_negative_exit_code_survives_parsing() {
        let mut run = evidence();
        run.report_text = "exit code -1".to_string();
        assert_eq!(exit_code_from_evidence(&run, "eval-x"), Some(-1));
    }

    /// The log is a diagnostic, so it has to say which outcome produced it and
    /// must never present the tool's exit code as the command's verdict.
    #[test]
    fn the_log_labels_the_tool_exit_code_as_the_tools() {
        let mut run = evidence();
        run.last_exit_code = Some(0);
        run.last_stdout = Some("all 12 cases passed\n".to_string());
        run.report_text = "exit code 0".to_string();
        let log = run.render_log();
        assert!(log.contains("[eval] agentic outcome: success"));
        assert!(log.contains("tool-reported exit code: 0"));
        assert!(log.contains("all 12 cases passed"));
        assert!(log.contains("--- agent report ---"));
    }

    #[test]
    fn an_empty_section_is_omitted_from_the_log() {
        let log = RunEvidence::default().render_log();
        assert!(log.contains("[eval] agentic outcome: "));
        assert!(!log.contains("--- stdout ---"));
        assert!(!log.contains("--- agent report ---"));
    }
}

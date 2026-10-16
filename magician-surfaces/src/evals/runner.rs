//! Starting a lane, and guaranteeing that every run leaves a record.
//!
//! # Why a lane runs as an ordinary execution
//!
//! A run is a plain magician execution — `internal: true`, `skip_planning:
//! true` — whose goal is to shell one `make` target. Cancel, progress, HITL and
//! spend gating already exist there and are already wired to the UI, so a lane
//! inherits all four for free. The alternative, a dedicated eval runner, reads
//! cleaner on a whiteboard and then has to grow its own queue, its own cancel,
//! its own progress and its own budget checks: a second execution-control plane
//! to keep correct forever. There is deliberately no queue, lease, worker pool,
//! concurrency limiter or retry loop in this module. Concurrency is the task
//! system's job.
//!
//! # Why "is this lane running?" is a query, not state
//!
//! [`super::store`] is *history*: a record exists only once a run has ended, and
//! [`super::run::EvalRunStatus`] has no `Running` on purpose. So in-flight state
//! is not kept here either — an in-memory registry of live runs dies with the
//! process and is invisible to every other client, which are the two ways a lane
//! gets wedged into looking permanently busy. Instead the question is asked of
//! the task system, which already knows, via
//! [`EvalExecutor::running_task_for_lane`]. The key back from an execution to a
//! lane is the execution *title* ([`execution_title_for_lane`]) — see that
//! function for why, and for what it costs.
//!
//! # The invariant: no run may end without a record
//!
//! Success, non-zero exit, backend error, cancellation, panic — every one of
//! them must leave a terminal [`EvalRun`]. A run that ended with no record is
//! indistinguishable from a lane that never ran, and one that wrongly read
//! `Passed` would be worse than either.
//!
//! That is not enforced by writing the record on each branch and remembering to
//! cover the new one. It is enforced by [`RunRecord`], whose *only* write site is
//! its `Drop`: the record is moved into the supervising future before that
//! future exists, so an early return, a `?`, a panic unwinding through it, or
//! tokio dropping the task all converge on the same single line of code. There
//! is no `finish()` that can be forgotten, because finishing only *mutates* the
//! record — persisting it is not something a caller does at all.
//!
//! The one thing this cannot survive is the process dying without unwinding
//! (`SIGKILL`, an abort). Nothing in-process can, and a run lost that way is
//! recoverable in the only way that matters: it left no record, so it reads as
//! never having run, which is true.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use async_trait::async_trait;
use tracing::error;

use magician::magician_v2::execution::runtime_boundary::spawn_execution_job;

use super::options::{self, EvalRunOptions};
use super::readiness::{lane_readiness, ProbeSnapshot};
use super::registry::{EvalLane, EvalRequirement};
use super::run::{EvalRun, EvalRunStatus};
use super::store::EvalRunStore;

/// Prefix that makes an execution's title identify its lane.
pub const EVAL_EXECUTION_TITLE_PREFIX: &str = "eval lane: ";

/// Iteration ceiling for a lane's execution.
///
/// A run is one shell command and a report of its exit code. The budget is
/// small not to save tokens but to bound what an agent can *do* with a failing
/// eval: given room to iterate, the obvious agentic move is to diagnose and fix
/// the failure, which would turn a red lane green without the code under eval
/// having changed. That is precisely the lie this whole feature exists to
/// prevent, so the room is not given.
pub const EVAL_MAX_ITERATIONS: usize = 8;

/// How deep [`CoverageReportLocator`] walks a lane's report directory looking
/// for evidence that the lane wrote something. Lanes nest a level or two
/// (`.../memory-temperature/latest/report.json`); nothing needs more, and a cap
/// keeps a stray symlink or a huge tree from stalling a page load.
const MAX_REPORT_DEPTH: u32 = 4;

/// What a caller gets back the moment a lane starts: enough to poll the run and
/// to attribute its spend, and nothing that implies the run has finished.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalRunHandle {
    pub run_id: String,
    pub task_id: String,
}

/// Why a lane was not started.
///
/// Every variant is a refusal that happened *before* an execution existed, with
/// one exception: [`EvalRunError::Backend`] can also come from the execution
/// creation itself. In all cases nothing was spent and no run record is written
/// — there is no run to record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvalRunError {
    /// No lane with this id. The Makefile is the registry, so this usually means
    /// the target was renamed or its annotation was lost.
    UnknownLane(String),
    /// The lane's `kind` never parsed, so it has no runnable representation at
    /// all — see [`super::registry::EvalKind::runnable`].
    NotRunnable { lane: String },
    /// The annotation parsed into a lane but carries a defect, so what it
    /// declares cannot be trusted.
    ///
    /// This is a fail-closed refusal, and the expensive case is the reason for
    /// it: a misspelled `requires=` token drops that requirement silently, and a
    /// lane that quietly declares *fewer* preconditions than it has is a lane
    /// readiness will happily wave through, into a run that dies halfway. The
    /// repo's own lanes cannot hit this — `eval_lane_contract` asserts every
    /// checked-in annotation parses clean — so refusing here costs nothing that
    /// is not already broken and visible on the page.
    Unparseable { lane: String, problem: String },
    /// A required service was not proven up. `missing` names every unsatisfied
    /// requirement, not just the first.
    NotReady {
        lane: String,
        missing: Vec<EvalRequirement>,
    },
    /// This lane already has a live execution. The id is returned so the caller
    /// can link to the run in flight rather than merely saying "no".
    AlreadyRunning { lane: String, task_id: String },
    /// The task system could not be asked, or could not start the execution.
    Backend(String),
}

impl std::fmt::Display for EvalRunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownLane(lane) => write!(f, "no eval lane named `{lane}`"),
            Self::NotRunnable { lane } => write!(
                f,
                "lane `{lane}` never declared a `kind`, so there is no way to run it"
            ),
            Self::Unparseable { lane, problem } => {
                write!(f, "lane `{lane}` has a malformed annotation: {problem}")
            },
            Self::NotReady { lane, missing } => write!(
                f,
                "lane `{lane}` is not ready; unsatisfied requirements: {missing:?}"
            ),
            Self::AlreadyRunning { lane, task_id } => {
                write!(f, "lane `{lane}` is already running as task `{task_id}`")
            },
            Self::Backend(message) => {
                write!(f, "the task system could not run this lane: {message}")
            },
        }
    }
}

impl std::error::Error for EvalRunError {}

/// What the lane's `make` invocation did, as observed from outside the lane.
///
/// This is the whole uniform layer: 31 lanes with 31 report formats all reduce
/// to an exit code and some output, which is why all 31 gain status and history
/// without any of them being touched.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct EvalCommandOutcome {
    /// `None` when no exit code could be recovered — the process was signalled,
    /// or the execution ended without ever reporting one. It maps to
    /// [`EvalRunStatus::Interrupted`], never to a judgement: an execution that
    /// finished is not the same claim as an eval that was evaluated.
    pub exit_code: Option<i32>,
    /// The run's output. Bounded on its way into the record, so the caller does
    /// not have to pre-trim it.
    pub log: String,
}

impl EvalCommandOutcome {
    pub fn new(exit_code: i32, log: impl Into<String>) -> Self {
        Self {
            exit_code: Some(exit_code),
            log: log.into(),
        }
    }

    /// An ending with no recoverable exit code: cancelled, signalled, or an
    /// execution that stopped without reporting one.
    pub fn indeterminate(log: impl Into<String>) -> Self {
        Self {
            exit_code: None,
            log: log.into(),
        }
    }
}

/// The execution the task system created to run one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StartedExecution {
    /// Recorded on the run, because the LLM ledger is keyed by task and this is
    /// the only way the run's spend can be attributed later.
    pub task_id: String,
    pub execution_id: String,
}

/// Everything the task system needs to run one lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvalExecutionSpec {
    pub principal: String,
    pub workspace: String,
    pub lane_id: String,
    /// The `make` target. Present alongside `goal` so an implementation can log
    /// or gate on it without parsing prose back out of the goal.
    pub target: String,
    /// Built by [`execution_title_for_lane`]. This is what makes the execution
    /// findable as *this lane's* run, so an implementation must set it verbatim.
    pub title: String,
    pub goal: String,
    pub max_iterations: usize,
}

/// The task system, as much of it as this module needs.
///
/// Implemented over the real execution API at the wiring layer, which is what
/// keeps this module testable without a runtime, a Makefile, or a `make` that
/// actually runs. An implementation is expected to create the execution with
/// `internal: true` (it is machinery, not work the user is tracking) and
/// `skip_planning: true` (there is nothing to plan — the goal is one command).
#[async_trait]
pub trait EvalExecutor: Send + Sync {
    /// The task id of a live execution of this lane, or `None` if there is
    /// none.
    ///
    /// "Live" means an execution that is not in a terminal state. An
    /// implementation answers this by listing the scope's executions and keying
    /// them back to lanes through their titles — see
    /// [`lane_id_from_execution_title`]. There is deliberately nothing to
    /// consult here besides the task system.
    async fn running_task_for_lane(
        &self,
        principal: &str,
        workspace: &str,
        lane_id: &str,
    ) -> Result<Option<String>, String>;

    /// Creates the execution. Returns as soon as it exists, before it has run.
    async fn start(&self, spec: EvalExecutionSpec) -> Result<StartedExecution, String>;

    /// Waits for that execution to end, and reports what the lane's command did.
    ///
    /// An implementation that cannot recover an exit code must return
    /// [`EvalCommandOutcome::indeterminate`] rather than guessing zero: the run
    /// then reads `Interrupted`, which is true, instead of `Passed`, which would
    /// be a green mark nothing earned.
    async fn wait(&self, execution_id: &str) -> Result<EvalCommandOutcome, String>;
}

/// Live service readiness, probed at the moment a run is requested.
///
/// Probing sits behind a trait, and inside [`EvalRunner::begin_lane`] rather
/// than in its arguments, so that "readiness was checked before anything was
/// spent" is a property of the call graph rather than of every caller
/// remembering to probe first.
#[async_trait]
pub trait ReadinessSource: Send + Sync {
    async fn probe(&self) -> ProbeSnapshot;
}

/// Where a lane's own (non-uniform) report ended up, if it wrote one.
pub trait ReportLocator: Send + Sync {
    /// A link to the report **this** run produced, or `None`.
    ///
    /// `started_at_ms` is what makes that "this run" rather than "some run":
    /// lanes write to fixed paths, so a directory that exists proves only that
    /// the lane succeeded *once*, possibly last month. Attaching that report to
    /// today's failed run would be the page vouching for a result today's run
    /// did not produce.
    fn report_href(&self, lane: &EvalLane, started_at_ms: i64) -> Option<String>;

    fn report_href_for_run(
        &self,
        lane: &EvalLane,
        started_at_ms: i64,
        _run_id: &str,
    ) -> Option<String> {
        self.report_href(lane, started_at_ms)
    }
}

/// Wall-clock time, injectable so a test can assert a duration instead of
/// asserting that some number is not negative.
pub trait Clock: Send + Sync {
    fn now_ms(&self) -> i64;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> i64 {
        chrono::Utc::now().timestamp_millis()
    }
}

/// The supervising future for one run: it waits for the lane to end and carries
/// the record that must be written when it does.
///
/// Handed back by [`EvalRunner::begin_lane`] instead of always being spawned so
/// that a caller with its own runtime policy can place it, and — more usefully —
/// so a test can drop it, which is the only way to prove the record survives
/// cancellation. It is inert only in the sense that it has not been polled: the
/// record is already inside it, so dropping it writes.
pub type SupervisedRun = Pin<Box<dyn Future<Output = ()> + Send>>;

/// Starts lanes and makes sure every started lane ends up in the store.
pub struct EvalRunner {
    executor: Arc<dyn EvalExecutor>,
    store: Arc<EvalRunStore>,
    readiness: Arc<dyn ReadinessSource>,
    reports: Arc<dyn ReportLocator>,
    clock: Arc<dyn Clock>,
}

impl EvalRunner {
    pub fn new(
        executor: Arc<dyn EvalExecutor>,
        store: Arc<EvalRunStore>,
        readiness: Arc<dyn ReadinessSource>,
        reports: Arc<dyn ReportLocator>,
    ) -> Self {
        Self {
            executor,
            store,
            readiness,
            reports,
            clock: Arc::new(SystemClock),
        }
    }

    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Starts `lane_id` and returns as soon as its execution exists.
    ///
    /// The run continues in the background; its record appears in the store when
    /// it ends. `lanes` is the parsed registry, passed in rather than read here
    /// so that caching the Makefile stays one layer up.
    pub async fn start_lane(
        &self,
        principal: &str,
        workspace: &str,
        lanes: &[EvalLane],
        lane_id: &str,
    ) -> Result<EvalRunHandle, EvalRunError> {
        self.start_lane_with_options(
            principal,
            workspace,
            lanes,
            lane_id,
            &EvalRunOptions::default(),
        )
        .await
    }

    pub async fn start_lane_with_options(
        &self,
        principal: &str,
        workspace: &str,
        lanes: &[EvalLane],
        lane_id: &str,
        options: &EvalRunOptions,
    ) -> Result<EvalRunHandle, EvalRunError> {
        let (handle, supervisor) = self
            .begin_lane_with_options(principal, workspace, lanes, lane_id, options)
            .await?;
        // The same runtime the rest of the execution machinery uses: the lane's
        // run is agentic work and is stack-heavy for the same reasons.
        spawn_execution_job(move || supervisor);
        Ok(handle)
    }

    /// [`EvalRunner::start_lane`] without the spawn.
    ///
    /// Everything that can refuse a run happens here, in this order: an unknown
    /// lane, a malformed one, one with no runnable kind, one already in flight,
    /// one whose services are not up. Only then is an execution created — so
    /// every refusal is free, and the readiness check is unambiguously before
    /// the spending rather than a moment after it.
    ///
    /// The in-flight check comes before the readiness probe because it is both
    /// cheaper and more informative: a lane that is running right now does not
    /// need to be told its services might be down.
    pub async fn begin_lane(
        &self,
        principal: &str,
        workspace: &str,
        lanes: &[EvalLane],
        lane_id: &str,
    ) -> Result<(EvalRunHandle, SupervisedRun), EvalRunError> {
        self.begin_lane_with_options(
            principal,
            workspace,
            lanes,
            lane_id,
            &EvalRunOptions::default(),
        )
        .await
    }

    pub async fn begin_lane_with_options(
        &self,
        principal: &str,
        workspace: &str,
        lanes: &[EvalLane],
        lane_id: &str,
        options: &EvalRunOptions,
    ) -> Result<(EvalRunHandle, SupervisedRun), EvalRunError> {
        options.validate(lane_id).map_err(EvalRunError::Backend)?;
        let lane = lanes
            .iter()
            .find(|lane| lane.id == lane_id)
            .ok_or_else(|| EvalRunError::UnknownLane(lane_id.to_string()))?;

        if let Some(problem) = lane.parse_error.as_deref() {
            return Err(EvalRunError::Unparseable {
                lane: lane.id.clone(),
                problem: problem.to_string(),
            });
        }
        // The value is discarded, but taking it is not pointless: `runnable()`
        // is what makes "a lane nobody understood cannot start" a fact about
        // this function rather than a rule someone has to keep remembering.
        let _runnable = lane
            .kind
            .runnable()
            .ok_or_else(|| EvalRunError::NotRunnable {
                lane: lane.id.clone(),
            })?;

        // Fail closed on a query failure. Starting a second run of a lane whose
        // first run we simply could not see is how one lane gets billed twice.
        match self
            .executor
            .running_task_for_lane(principal, workspace, &lane.id)
            .await
        {
            Ok(Some(task_id)) => {
                return Err(EvalRunError::AlreadyRunning {
                    lane: lane.id.clone(),
                    task_id,
                })
            },
            Ok(None) => {},
            Err(message) => {
                return Err(EvalRunError::Backend(format!(
                    "could not determine whether lane `{}` is already running: {message}",
                    lane.id
                )))
            },
        }

        let readiness = lane_readiness(&lane.requires, &self.readiness.probe().await);
        if !readiness.ready {
            return Err(EvalRunError::NotReady {
                lane: lane.id.clone(),
                missing: readiness.missing,
            });
        }

        let started_at_ms = self.clock.now_ms();
        let run_id = EvalRun::new_run_id(started_at_ms);
        let arguments = options.make_arguments(&lane.id, &run_id);
        let started = self
            .executor
            .start(EvalExecutionSpec {
                principal: principal.to_string(),
                workspace: workspace.to_string(),
                lane_id: lane.id.clone(),
                target: lane.target.clone(),
                title: execution_title_for_lane(&lane.id),
                goal: goal_for_lane_with_arguments(lane, &arguments),
                max_iterations: EVAL_MAX_ITERATIONS,
            })
            .await
            .map_err(EvalRunError::Backend)?;

        // The record is minted here, after the execution exists and before the
        // supervising future does. Both halves of that matter: it can only carry
        // a real `task_id` (a run whose spend cannot be attributed is not
        // representable), and it is already inside the future by the time anyone
        // could drop it, so a future dropped before its first poll still writes.
        let mut run = EvalRun::new(
            run_id.clone(),
            lane.id.clone(),
            principal,
            workspace,
            started_at_ms,
        );
        run.task_id = Some(started.task_id.clone());
        // Snapshotted, so editing the annotation later cannot rewrite what this
        // run needed.
        run.services = lane.requires.clone();
        if lane.id == options::LIFECYCLE_LIVE {
            run.options = Some(options.clone());
        }

        let handle = EvalRunHandle {
            run_id,
            task_id: started.task_id.clone(),
        };
        let record = RunRecord {
            run: Some(run),
            store: Arc::clone(&self.store),
            clock: Arc::clone(&self.clock),
        };

        let executor = Arc::clone(&self.executor);
        let reports = Arc::clone(&self.reports);
        let lane = lane.clone();
        let execution_id = started.execution_id;
        let supervisor: SupervisedRun = Box::pin(async move {
            supervise(executor, reports, lane, execution_id, started_at_ms, record).await;
        });

        Ok((handle, supervisor))
    }
}

/// Waits for one lane's execution and describes what happened on the record.
///
/// Note what this function does NOT do: write anything. It only mutates the
/// record it was handed. Persistence happens when `record` goes out of scope —
/// including when it goes out of scope because this future was cancelled or a
/// panic is unwinding through it.
async fn supervise(
    executor: Arc<dyn EvalExecutor>,
    reports: Arc<dyn ReportLocator>,
    lane: EvalLane,
    execution_id: String,
    started_at_ms: i64,
    mut record: RunRecord,
) {
    match executor.wait(&execution_id).await {
        Ok(outcome) => record.record_outcome(&outcome),
        Err(message) => record.record_supervision_failure(&message),
    }
    // Consulted whatever the outcome: a failing lane usually still writes a
    // report, and that report is the most useful thing on the page for a red
    // row. The `started_at_ms` filter is what stops it linking an older one.
    let run_id = record
        .run
        .as_ref()
        .map(|run| run.run_id.as_str())
        .unwrap_or("");
    let href = reports.report_href_for_run(&lane, started_at_ms, run_id);
    record.set_report_href(href);
}

/// The run record in flight, and the only thing that ever writes one.
///
/// There is no `write`, `flush` or `commit` on this type. `Drop` is the write,
/// which is what makes "every path records a run" true by construction rather
/// than by review: an early return, a `?`, a panic, and tokio dropping the task
/// are all the same code path from here.
///
/// The store is deliberately synchronous ([`EvalRunStore`]), which is what makes
/// this possible at all — `Drop` cannot await. The cost is one small JSON write
/// on whatever thread the run ended on, once per run.
struct RunRecord {
    /// `Some` for this type's entire life; the `Option` exists only so `Drop`,
    /// which gets `&mut self`, can move the record out to write it.
    run: Option<EvalRun>,
    store: Arc<EvalRunStore>,
    clock: Arc<dyn Clock>,
}

impl RunRecord {
    fn record_outcome(&mut self, outcome: &EvalCommandOutcome) {
        if let Some(run) = self.run.as_mut() {
            run.status = status_for_exit_code(outcome.exit_code);
            run.exit_code = outcome.exit_code;
            run.set_log_tail(&outcome.log);
        }
    }

    /// The run could not be watched to its end. The status stays
    /// [`EvalRunStatus::Interrupted`] — [`EvalRun::new`]'s default — because
    /// that is exactly what happened: the lane may well have passed, but nothing
    /// here saw it, and "we lost track of it" must never be recorded as a
    /// judgement in either direction.
    fn record_supervision_failure(&mut self, message: &str) {
        if let Some(run) = self.run.as_mut() {
            run.set_log_tail(&format!(
                "the eval run could not be followed to completion: {message}\n"
            ));
        }
    }

    fn set_report_href(&mut self, href: Option<String>) {
        if let Some(run) = self.run.as_mut() {
            run.report_href = href;
        }
    }
}

impl Drop for RunRecord {
    fn drop(&mut self) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        // Wall-clock subtraction, so a clock stepped backwards mid-run would
        // otherwise produce a negative duration. Clamping loses a few seconds of
        // accuracy in a case that should not happen; not clamping puts an
        // impossible number on a chart.
        run.duration_ms = self.clock.now_ms().saturating_sub(run.started_at_ms).max(0);
        if let Err(error) = self.store.append(&run) {
            // Nothing above can react to this — the run is over and the caller
            // is long gone — but it must not be silent: this is the one failure
            // that makes a run that really happened read as one that never did.
            error!(
                lane_id = %run.lane_id,
                run_id = %run.run_id,
                status = ?run.status,
                %error,
                "eval run record could not be written; this run is now invisible to the page"
            );
        }
    }
}

/// The exit code, read as a verdict.
///
/// `None` is `Interrupted` rather than `Failed`: a run with no exit code was not
/// judged at all, and marking a lane red for a run that never reached a verdict
/// invents a regression exactly as surely as marking it green would hide one.
pub fn status_for_exit_code(exit_code: Option<i32>) -> EvalRunStatus {
    match exit_code {
        Some(0) => EvalRunStatus::Passed,
        Some(_) => EvalRunStatus::Failed,
        None => EvalRunStatus::Interrupted,
    }
}

/// The title that makes an execution findable as this lane's run.
///
/// The title is load-bearing: it is the *only* thing keying a live execution
/// back to a lane, because that is the only lane-shaped field an execution
/// listing carries. The honest cost: renaming a run's title from the UI orphans
/// it, and the lane would then accept a second concurrent run. That is a visible
/// nuisance rather than a silent wrong answer, and it is a better trade than the
/// alternative — an in-memory index of live runs, which would be wrong after
/// every restart and invisible to every other client.
pub fn execution_title_for_lane(lane_id: &str) -> String {
    format!("{EVAL_EXECUTION_TITLE_PREFIX}{lane_id}")
}

/// The lane an execution title names, or `None` if it names no lane.
///
/// The inverse of [`execution_title_for_lane`], for an implementation walking an
/// execution listing. Ordinary executions — the overwhelming majority — return
/// `None` here and are skipped.
pub fn lane_id_from_execution_title(title: &str) -> Option<&str> {
    title
        .strip_prefix(EVAL_EXECUTION_TITLE_PREFIX)
        .map(str::trim)
        .filter(|lane_id| !lane_id.is_empty())
}

/// The goal the lane's execution is given.
///
/// It says "do not fix this" for a reason. Handed a failing eval and any room to
/// act, the natural agentic move is to diagnose and repair it — and a repaired
/// eval reports `Passed` for code that has not changed, which is the single
/// worst thing this page could display. The run's job is to obtain a verdict,
/// not to earn one.
pub fn goal_for_lane(lane: &EvalLane) -> String {
    goal_for_lane_with_arguments(lane, "")
}

fn goal_for_lane_with_arguments(lane: &EvalLane, arguments: &str) -> String {
    format!(
        "Run the eval lane `{}`. Execute exactly this command from the repository \
         root and nothing else:\n\n    make {}{}\n\n\
         Then report the command's exit code and the tail of its output verbatim.\n\n\
         Do NOT edit any file, do NOT retry the command, and do NOT substitute a \
         different one. A non-zero exit is this eval's verdict: it is the result \
         being measured, not a problem to fix. Report it as-is.",
        lane.id, lane.target, arguments
    )
}

/// Finds a lane's report under the reports root — conventionally
/// `COVERAGE_BASE_DIR`, which is what the `report=` annotations are written
/// relative to (`Makefile:406-447`).
pub struct CoverageReportLocator {
    reports_root: PathBuf,
    href_prefix: String,
}

impl CoverageReportLocator {
    pub fn new(reports_root: impl Into<PathBuf>, href_prefix: impl Into<String>) -> Self {
        Self {
            reports_root: reports_root.into(),
            href_prefix: href_prefix.into(),
        }
    }
}

impl ReportLocator for CoverageReportLocator {
    fn report_href_for_run(
        &self,
        lane: &EvalLane,
        started_at_ms: i64,
        run_id: &str,
    ) -> Option<String> {
        if !options::has_run_reports(&lane.id) {
            return self.report_href(lane, started_at_ms);
        }
        if !options::safe_token(run_id) {
            return None;
        }
        let relative = format!("{}/runs/{run_id}", lane.report_dir.as_deref()?);
        let path = self.reports_root.join(&relative).join("report.html");
        let modified = path
            .metadata()
            .ok()?
            .modified()
            .ok()
            .and_then(system_time_ms)?;
        if modified < started_at_ms {
            return None;
        }
        Some(format!(
            "{}/{relative}/",
            self.href_prefix.trim_end_matches('/')
        ))
    }

    fn report_href(&self, lane: &EvalLane, started_at_ms: i64) -> Option<String> {
        let report_dir = lane.report_dir.as_deref()?;
        let newest = newest_mtime_ms(&self.reports_root.join(report_dir), MAX_REPORT_DEPTH)?;
        // Strictly at-or-after the run's start. The bias is deliberate: failing
        // to link a report the lane did write is a missing convenience, while
        // linking one it did not write is the page attributing someone else's
        // result to this run.
        if newest < started_at_ms {
            return None;
        }
        Some(format!(
            "{}/{report_dir}/",
            self.href_prefix.trim_end_matches('/')
        ))
    }
}

/// The newest modification time under `dir`, in epoch milliseconds, or `None`
/// when it holds no files (or cannot be read — an unreadable report directory
/// is not evidence that a report was written).
fn newest_mtime_ms(dir: &Path, depth: u32) -> Option<i64> {
    if depth == 0 {
        return None;
    }
    let mut newest: Option<i64> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let candidate = if file_type.is_dir() {
            newest_mtime_ms(&entry.path(), depth - 1)
        } else {
            entry
                .metadata()
                .ok()
                .and_then(|metadata| metadata.modified().ok())
                .and_then(system_time_ms)
        };
        if let Some(candidate) = candidate {
            newest = Some(newest.map_or(candidate, |best: i64| best.max(candidate)));
        }
    }
    newest
}

fn system_time_ms(time: std::time::SystemTime) -> Option<i64> {
    let since_epoch = time.duration_since(std::time::UNIX_EPOCH).ok()?;
    i64::try_from(since_epoch.as_millis()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Mutex;

    use crate::evals::readiness::ProbeResult;
    use crate::evals::registry::EvalKind;
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

    const PRINCIPAL: &str = "anonymous";
    const WORKSPACE: &str = "default";

    fn lane(id: &str, kind: EvalKind, requires: Vec<EvalRequirement>) -> EvalLane {
        EvalLane {
            id: id.to_string(),
            target: id.to_string(),
            kind,
            requires,
            report_dir: None,
            desc: None,
            line: 1,
            parse_error: None,
        }
    }

    fn harness_lane() -> EvalLane {
        lane("eval-monitor-golden", EvalKind::Harness, Vec::new())
    }

    /// Ticks a fixed amount per read, so a duration assertion can be exact
    /// instead of "not negative".
    struct FakeClock {
        now: AtomicI64,
        step: i64,
    }

    impl FakeClock {
        fn new(start: i64, step: i64) -> Arc<Self> {
            Arc::new(Self {
                now: AtomicI64::new(start),
                step,
            })
        }
    }

    impl Clock for FakeClock {
        fn now_ms(&self) -> i64 {
            self.now.fetch_add(self.step, Ordering::SeqCst)
        }
    }

    struct FixedReadiness(ProbeSnapshot);

    #[async_trait]
    impl ReadinessSource for FixedReadiness {
        async fn probe(&self) -> ProbeSnapshot {
            self.0.clone()
        }
    }

    fn everything_up() -> Arc<dyn ReadinessSource> {
        Arc::new(FixedReadiness(
            ProbeSnapshot::default()
                .with(EvalRequirement::Ollama, ProbeResult::Up)
                .with(EvalRequirement::Magician, ProbeResult::Up)
                .with(EvalRequirement::MagicianBinary, ProbeResult::Up)
                .with(EvalRequirement::Magicutor, ProbeResult::Up)
                .with(EvalRequirement::ProviderKeys, ProbeResult::Up),
        ))
    }

    struct NoReports;

    impl ReportLocator for NoReports {
        fn report_href(&self, _lane: &EvalLane, _started_at_ms: i64) -> Option<String> {
            None
        }
    }

    struct FixedReport(&'static str);

    impl ReportLocator for FixedReport {
        fn report_href(&self, _lane: &EvalLane, _started_at_ms: i64) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    /// What `wait` does, which is where every interesting terminal path is.
    enum WaitBehaviour {
        Outcome(EvalCommandOutcome),
        Error(String),
        Panic,
        Forever,
    }

    struct FakeExecutor {
        inflight: Result<Option<String>, String>,
        start: Result<StartedExecution, String>,
        wait: WaitBehaviour,
        started: Mutex<Vec<EvalExecutionSpec>>,
    }

    impl FakeExecutor {
        fn new(wait: WaitBehaviour) -> Arc<Self> {
            Arc::new(Self {
                inflight: Ok(None),
                start: Ok(StartedExecution {
                    task_id: "task_eval_1".to_string(),
                    execution_id: "exec_eval_1".to_string(),
                }),
                wait,
                started: Mutex::new(Vec::new()),
            })
        }

        fn exiting(code: i32) -> Arc<Self> {
            Self::new(WaitBehaviour::Outcome(EvalCommandOutcome::new(
                code,
                "make output\n",
            )))
        }

        fn already_running(task_id: &str) -> Arc<Self> {
            let mut executor = Self::exiting(0);
            Arc::get_mut(&mut executor).unwrap().inflight = Ok(Some(task_id.to_string()));
            executor
        }

        fn started_specs(&self) -> Vec<EvalExecutionSpec> {
            self.started.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl EvalExecutor for FakeExecutor {
        async fn running_task_for_lane(
            &self,
            _principal: &str,
            _workspace: &str,
            _lane_id: &str,
        ) -> Result<Option<String>, String> {
            self.inflight.clone()
        }

        async fn start(&self, spec: EvalExecutionSpec) -> Result<StartedExecution, String> {
            self.started.lock().unwrap().push(spec);
            self.start.clone()
        }

        async fn wait(&self, _execution_id: &str) -> Result<EvalCommandOutcome, String> {
            match &self.wait {
                WaitBehaviour::Outcome(outcome) => Ok(outcome.clone()),
                WaitBehaviour::Error(message) => Err(message.clone()),
                WaitBehaviour::Panic => panic!("the eval worker died mid-run"),
                WaitBehaviour::Forever => {
                    std::future::pending::<()>().await;
                    unreachable!("pending never resolves")
                },
            }
        }
    }

    struct Harness {
        _dir: tempfile::TempDir,
        store: Arc<EvalRunStore>,
        runner: EvalRunner,
        executor: Arc<FakeExecutor>,
    }

    impl Harness {
        fn new(executor: Arc<FakeExecutor>) -> Self {
            Self::with_reports(executor, Arc::new(NoReports))
        }

        fn with_reports(executor: Arc<FakeExecutor>, reports: Arc<dyn ReportLocator>) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Arc::new(EvalRunStore::new(ArtifactV2Workspace::new(dir.path())));
            let runner = EvalRunner::new(
                executor.clone(),
                Arc::clone(&store),
                everything_up(),
                reports,
            )
            .with_clock(FakeClock::new(1_700_000_000_000, 250));
            Self {
                _dir: dir,
                store,
                runner,
                executor,
            }
        }

        fn runs(&self) -> Vec<EvalRun> {
            self.store.list(PRINCIPAL, WORKSPACE, None).unwrap()
        }

        /// The single recorded run, insisting there is exactly one. Every test
        /// here is as much about "one record" as about what is in it.
        fn only_run(&self) -> EvalRun {
            let runs = self.runs();
            assert_eq!(runs.len(), 1, "expected exactly one run record: {runs:#?}");
            runs.into_iter().next().unwrap()
        }

        async fn begin(
            &self,
            lanes: &[EvalLane],
            lane_id: &str,
        ) -> Result<(EvalRunHandle, SupervisedRun), EvalRunError> {
            self.runner
                .begin_lane(PRINCIPAL, WORKSPACE, lanes, lane_id)
                .await
        }

        /// Starts a lane and drives it to completion, which is what a spawned
        /// supervisor does in production.
        async fn run_to_completion(
            &self,
            lanes: &[EvalLane],
            lane_id: &str,
        ) -> Result<EvalRunHandle, EvalRunError> {
            let (handle, supervisor) = self.begin(lanes, lane_id).await?;
            supervisor.await;
            Ok(handle)
        }

        /// The record for a run that was spawned rather than awaited. Bounded so
        /// a regression that never writes fails the test instead of hanging it.
        async fn await_recorded_run(&self) -> EvalRun {
            for _ in 0..200 {
                if let Some(run) = self.runs().into_iter().next() {
                    return run;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
            panic!("no run record appeared; a spawned run ended without one");
        }
    }

    #[tokio::test]
    async fn lifecycle_options_and_reports_survive_history_without_repointing() {
        let reports = tempfile::tempdir().unwrap();
        let harness = Harness::with_reports(
            FakeExecutor::exiting(1),
            Arc::new(CoverageReportLocator::new(reports.path(), "/reports")),
        );
        let mut lane = lane(
            options::LIFECYCLE_LIVE,
            EvalKind::Live,
            vec![EvalRequirement::ProviderKeys],
        );
        lane.report_dir = Some("evals/memory-lifecycle/live".into());
        let lanes = vec![lane];
        let options = EvalRunOptions {
            profiles: vec!["candidate".into()],
            repeats: Some(2),
            partition: Some("validation".into()),
        };
        let invalid = EvalRunOptions {
            repeats: Some(0),
            ..Default::default()
        };
        assert!(harness
            .runner
            .begin_lane_with_options(
                PRINCIPAL,
                WORKSPACE,
                &lanes,
                options::LIFECYCLE_LIVE,
                &invalid
            )
            .await
            .is_err());
        assert!(harness.executor.started_specs().is_empty());
        let mut handles = Vec::new();
        for _ in 0..2 {
            let (handle, supervisor) = harness
                .runner
                .begin_lane_with_options(
                    PRINCIPAL,
                    WORKSPACE,
                    &lanes,
                    options::LIFECYCLE_LIVE,
                    &options,
                )
                .await
                .unwrap();
            let directory = reports
                .path()
                .join("evals/memory-lifecycle/live/runs")
                .join(&handle.run_id);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join("report.html"), "failed fixture evidence").unwrap();
            supervisor.await;
            handles.push(handle);
        }
        let runs = harness.runs();
        assert_eq!(runs.len(), 2);
        for handle in handles {
            let run = runs.iter().find(|r| r.run_id == handle.run_id).unwrap();
            assert_eq!(run.status, EvalRunStatus::Failed);
            assert_eq!(run.options.as_ref(), Some(&options));
            assert_eq!(
                run.report_href.as_deref(),
                Some(
                    format!(
                        "/reports/evals/memory-lifecycle/live/runs/{}/",
                        handle.run_id
                    )
                    .as_str()
                )
            );
            assert!(harness.executor.started_specs().iter().any(|s| s
                .goal
                .contains(&format!("EVAL_RUN_ID={}", handle.run_id))
                && s.goal.contains("MEMORY_LIFECYCLE_EVAL_PROFILES=candidate")));
        }
        let locator = CoverageReportLocator::new(reports.path(), "/reports");
        assert_eq!(locator.report_href_for_run(&lanes[0], 0, "absent"), None);
        assert_eq!(locator.report_href_for_run(&lanes[0], 0, "../escape"), None);
    }

    #[tokio::test]
    async fn a_successful_run_records_passed() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));

        let handle = harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .expect("a ready harness lane starts");

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Passed);
        assert_eq!(run.exit_code, Some(0));
        assert_eq!(run.run_id, handle.run_id);
        assert_eq!(run.lane_id, "eval-monitor-golden");
        assert_eq!(run.log_tail.as_deref(), Some("make output\n"));
        // The clock ticks 250ms per read: one read for `started_at_ms`, one in
        // the guard. A duration of zero would mean nothing was measured.
        assert_eq!(run.duration_ms, 250);
    }

    /// `start_lane` is the entry point everything else uses, and the only part
    /// of it not covered by driving the supervisor by hand is that it actually
    /// spawns one. A run that is started and never recorded is the whole failure
    /// this module exists to prevent, so it is asserted through the real
    /// entry point too, not only through `begin_lane`.
    #[tokio::test]
    async fn start_lane_spawns_the_supervisor_and_the_run_still_lands_in_the_store() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));

        let handle = harness
            .runner
            .start_lane(PRINCIPAL, WORKSPACE, &lanes, "eval-monitor-golden")
            .await
            .expect("a ready harness lane starts");

        let run = harness.await_recorded_run().await;
        assert_eq!(run.run_id, handle.run_id);
        assert_eq!(run.status, EvalRunStatus::Passed);
    }

    #[tokio::test]
    async fn a_failing_run_records_failed_with_the_exit_code() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(2));

        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .expect("a failing lane still starts");

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Failed);
        assert_eq!(run.exit_code, Some(2), "the exit code is the verdict");
        assert!(run.status.is_judgement(), "a real failure IS a judgement");
    }

    /// A crash mid-run must still produce a terminal record. A run that left no
    /// record reads as "never ran"; one that read Passed would be a lie.
    #[tokio::test]
    async fn a_crashed_run_records_interrupted() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::new(WaitBehaviour::Error(
            "the execution vanished".to_string(),
        )));

        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .expect("the lane starts; it is the watching that fails");

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Interrupted);
        assert_eq!(run.exit_code, None);
        assert!(
            !run.status.is_judgement(),
            "nothing was judged, so this must not count toward a pass rate"
        );
        assert!(
            run.log_tail
                .as_deref()
                .unwrap_or_default()
                .contains("the execution vanished"),
            "the record must say why it was interrupted: {:?}",
            run.log_tail
        );
    }

    /// The harshest version of the same invariant: the supervising future is
    /// unwound by a panic rather than returning at all.
    #[tokio::test]
    async fn a_panicking_run_still_records_interrupted() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::new(WaitBehaviour::Panic));

        let (_handle, supervisor) = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .expect("the lane starts");
        let joined = tokio::spawn(supervisor).await;
        assert!(joined.is_err(), "the supervisor was expected to panic");

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Interrupted);
        assert_eq!(run.exit_code, None);
    }

    /// Cancellation as the task system reports it: the execution ended, but with
    /// no exit code, so nothing was judged.
    #[tokio::test]
    async fn a_cancelled_run_records_interrupted() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::new(WaitBehaviour::Outcome(
            EvalCommandOutcome::indeterminate("cancelled by the operator\n"),
        )));

        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .expect("the lane starts");

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Interrupted);
        assert_eq!(run.exit_code, None);
    }

    /// Cancellation as the runtime performs it: the supervising future is
    /// dropped out from under a run that is still going. This is the case a
    /// "write the record on each branch" implementation silently loses, because
    /// there is no branch to write it on.
    #[tokio::test]
    async fn a_run_whose_supervisor_is_dropped_mid_flight_still_records_interrupted() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::new(WaitBehaviour::Forever));

        let (handle, supervisor) = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .expect("the lane starts");
        let task = tokio::spawn(supervisor);
        // Let it reach the point where it is waiting on the lane.
        tokio::task::yield_now().await;
        task.abort();
        let _ = task.await;

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Interrupted);
        assert_eq!(run.task_id.as_deref(), Some(handle.task_id.as_str()));
    }

    /// The same drop, one step earlier: a supervisor that is never polled at
    /// all. The record has to already be inside the future for this to work,
    /// which is why it is built before the future rather than inside it.
    #[tokio::test]
    async fn a_supervisor_dropped_before_it_ever_runs_still_records_the_run() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));

        let (handle, supervisor) = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .expect("the lane starts");
        drop(supervisor);

        let run = harness.only_run();
        assert_eq!(run.status, EvalRunStatus::Interrupted);
        assert_eq!(run.run_id, handle.run_id);
    }

    #[tokio::test]
    async fn every_run_carries_the_task_id_that_produced_it() {
        let lanes = vec![harness_lane()];

        // Spend has to be attributable however the run ended, so this is
        // asserted across every terminal shape, not just the happy one.
        for wait in [
            WaitBehaviour::Outcome(EvalCommandOutcome::new(0, "ok\n")),
            WaitBehaviour::Outcome(EvalCommandOutcome::new(1, "boom\n")),
            WaitBehaviour::Outcome(EvalCommandOutcome::indeterminate("cancelled\n")),
            WaitBehaviour::Error("gone".to_string()),
        ] {
            let harness = Harness::new(FakeExecutor::new(wait));
            let handle = harness
                .run_to_completion(&lanes, "eval-monitor-golden")
                .await
                .expect("the lane starts");

            let run = harness.only_run();
            assert_eq!(
                run.task_id.as_deref(),
                Some(handle.task_id.as_str()),
                "a run with no task id has spend nothing can attribute"
            );
            assert!(!handle.task_id.is_empty());
        }
    }

    #[tokio::test]
    async fn the_report_href_is_none_when_the_lane_wrote_no_report() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));

        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();
        assert_eq!(harness.only_run().report_href, None);

        // And it IS carried when the lane wrote one, so the `None` above is a
        // real absence rather than a field nobody ever sets.
        let harness = Harness::with_reports(
            FakeExecutor::exiting(0),
            Arc::new(FixedReport("/r/monitor")),
        );
        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();
        assert_eq!(
            harness.only_run().report_href.as_deref(),
            Some("/r/monitor")
        );
    }

    #[tokio::test]
    async fn a_second_concurrent_run_of_one_lane_is_rejected_with_the_inflight_task_id() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::already_running("task_already_live"));

        let error = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .err()
            .expect("a lane already in flight must not start again");
        assert_eq!(
            error,
            EvalRunError::AlreadyRunning {
                lane: "eval-monitor-golden".to_string(),
                task_id: "task_already_live".to_string(),
            }
        );
        assert!(
            harness.executor.started_specs().is_empty(),
            "no second execution may be created"
        );
        assert!(
            harness.runs().is_empty(),
            "a refused start is not a run, so it records nothing"
        );
    }

    /// A lane whose annotation never declared a kind has no runnable
    /// representation and must be refused before an execution is created.
    #[tokio::test]
    async fn a_lane_with_unknown_kind_is_refused() {
        let lanes = vec![lane("eval-mystery", EvalKind::Unknown, Vec::new())];
        let harness = Harness::new(FakeExecutor::exiting(0));

        let error = harness.begin(&lanes, "eval-mystery").await.err().unwrap();
        assert_eq!(
            error,
            EvalRunError::NotRunnable {
                lane: "eval-mystery".to_string()
            }
        );
        assert!(harness.executor.started_specs().is_empty());
        assert!(harness.runs().is_empty());
    }

    /// Readiness is checked BEFORE spending, not after.
    #[tokio::test]
    async fn an_unready_lane_is_refused_and_names_the_missing_services() {
        let lanes = vec![lane(
            "eval-live",
            EvalKind::Live,
            vec![EvalRequirement::Ollama, EvalRequirement::ProviderKeys],
        )];
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(EvalRunStore::new(ArtifactV2Workspace::new(dir.path())));
        let executor = FakeExecutor::exiting(0);
        let runner = EvalRunner::new(
            executor.clone(),
            Arc::clone(&store),
            // Ollama is down and provider keys were never probed. Both block,
            // and both must be named — otherwise fixing one only reveals the
            // next on the following page load.
            Arc::new(FixedReadiness(
                ProbeSnapshot::default().with(EvalRequirement::Ollama, ProbeResult::Down),
            )),
            Arc::new(NoReports),
        );

        let error = runner
            .begin_lane(PRINCIPAL, WORKSPACE, &lanes, "eval-live")
            .await
            .err()
            .expect("an unready lane must not start");
        assert_eq!(
            error,
            EvalRunError::NotReady {
                lane: "eval-live".to_string(),
                missing: vec![EvalRequirement::Ollama, EvalRequirement::ProviderKeys],
            }
        );
        assert!(
            executor.started_specs().is_empty(),
            "the refusal must come before the execution, or it saved nothing"
        );
        assert!(store.list(PRINCIPAL, WORKSPACE, None).unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unknown_lane_is_refused() {
        let harness = Harness::new(FakeExecutor::exiting(0));
        let error = harness.begin(&[], "eval-nowhere").await.err().unwrap();
        assert_eq!(error, EvalRunError::UnknownLane("eval-nowhere".to_string()));
        assert!(harness.executor.started_specs().is_empty());
    }

    /// Fail closed on a malformed annotation. The expensive case is a misspelled
    /// `requires=` token, which silently drops the requirement — so readiness
    /// waves the lane through and the run dies partway.
    #[tokio::test]
    async fn a_lane_with_a_malformed_annotation_is_refused() {
        let mut broken = lane("eval-typo", EvalKind::Harness, Vec::new());
        broken.parse_error = Some("unknown requirement `magicain`".to_string());
        let harness = Harness::new(FakeExecutor::exiting(0));

        let error = harness.begin(&[broken], "eval-typo").await.err().unwrap();
        assert_eq!(
            error,
            EvalRunError::Unparseable {
                lane: "eval-typo".to_string(),
                problem: "unknown requirement `magicain`".to_string(),
            }
        );
        assert!(harness.executor.started_specs().is_empty());
    }

    /// If the task system cannot say whether a lane is running, refuse. The
    /// alternative — assume it is idle — bills an expensive lane twice.
    #[tokio::test]
    async fn an_unanswerable_inflight_query_refuses_rather_than_starting_a_second_run() {
        let lanes = vec![harness_lane()];
        let mut executor = FakeExecutor::exiting(0);
        Arc::get_mut(&mut executor).unwrap().inflight = Err("execution store offline".to_string());
        let harness = Harness::new(executor);

        let error = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .err()
            .expect("an unanswerable query must fail closed");
        assert!(matches!(error, EvalRunError::Backend(_)), "{error:?}");
        assert!(harness.executor.started_specs().is_empty());
        assert!(harness.runs().is_empty());
    }

    /// An execution that could not be created spent nothing and has no task id,
    /// so there is nothing to record — and recording a run without one would
    /// produce exactly the unattributable-spend row the record shape forbids.
    #[tokio::test]
    async fn a_lane_whose_execution_cannot_be_created_records_nothing() {
        let lanes = vec![harness_lane()];
        let mut executor = FakeExecutor::exiting(0);
        Arc::get_mut(&mut executor).unwrap().start = Err("no runtime".to_string());
        let harness = Harness::new(executor);

        let error = harness
            .begin(&lanes, "eval-monitor-golden")
            .await
            .err()
            .unwrap();
        assert!(matches!(error, EvalRunError::Backend(_)), "{error:?}");
        assert!(harness.runs().is_empty());
    }

    #[tokio::test]
    async fn the_execution_carries_the_lane_title_the_goal_and_a_bounded_budget() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));
        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();

        let specs = harness.executor.started_specs();
        assert_eq!(specs.len(), 1);
        let spec = &specs[0];
        assert_eq!(spec.title, "eval lane: eval-monitor-golden");
        assert_eq!(
            lane_id_from_execution_title(&spec.title),
            Some("eval-monitor-golden"),
            "the title is the only key back from an execution to its lane"
        );
        assert!(spec.goal.contains("make eval-monitor-golden"));
        assert_eq!(spec.principal, PRINCIPAL);
        assert_eq!(spec.workspace, WORKSPACE);
        assert_eq!(spec.max_iterations, EVAL_MAX_ITERATIONS);
    }

    /// The run snapshots what the lane declared, so editing the annotation later
    /// cannot rewrite what an old run needed.
    #[tokio::test]
    async fn a_run_snapshots_the_requirements_the_lane_declared() {
        let lanes = vec![lane(
            "eval-live",
            EvalKind::Live,
            vec![EvalRequirement::Ollama, EvalRequirement::Magician],
        )];
        let harness = Harness::new(FakeExecutor::exiting(0));

        harness
            .run_to_completion(&lanes, "eval-live")
            .await
            .unwrap();
        assert_eq!(
            harness.only_run().services,
            vec![EvalRequirement::Ollama, EvalRequirement::Magician]
        );
    }

    /// Two runs of one lane must accumulate rather than collide: the run id is
    /// what keeps history append-only, and a collision would cost a data point.
    #[tokio::test]
    async fn consecutive_runs_of_one_lane_each_get_their_own_record() {
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::exiting(0));

        let first = harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();
        let second = harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();

        assert_ne!(first.run_id, second.run_id);
        assert_eq!(harness.runs().len(), 2);
    }

    /// The log tail is bounded on the way in, so a lane that printed a gigabyte
    /// cannot land a gigabyte in the scope.
    #[tokio::test]
    async fn an_enormous_log_is_bounded_before_it_reaches_the_store() {
        use crate::evals::run::MAX_LOG_TAIL_BYTES;
        let lanes = vec![harness_lane()];
        let harness = Harness::new(FakeExecutor::new(WaitBehaviour::Outcome(
            EvalCommandOutcome::new(1, "x".repeat(MAX_LOG_TAIL_BYTES * 3)),
        )));

        harness
            .run_to_completion(&lanes, "eval-monitor-golden")
            .await
            .unwrap();
        let stored = harness.only_run().log_tail.unwrap();
        assert!(stored.len() <= MAX_LOG_TAIL_BYTES, "{}", stored.len());
    }

    #[test]
    fn an_exit_code_reads_as_a_verdict_and_its_absence_does_not() {
        assert_eq!(status_for_exit_code(Some(0)), EvalRunStatus::Passed);
        assert_eq!(status_for_exit_code(Some(1)), EvalRunStatus::Failed);
        assert_eq!(status_for_exit_code(Some(-9)), EvalRunStatus::Failed);
        // Not `Failed`: a signalled or abandoned run judged nothing, and a red
        // mark for it invents a regression just as a green one would hide it.
        assert_eq!(status_for_exit_code(None), EvalRunStatus::Interrupted);
    }

    #[test]
    fn a_lane_title_round_trips_and_ordinary_titles_name_no_lane() {
        assert_eq!(
            lane_id_from_execution_title(&execution_title_for_lane("eval-monitor-golden")),
            Some("eval-monitor-golden")
        );
        assert_eq!(lane_id_from_execution_title("Summarize example.com"), None);
        assert_eq!(lane_id_from_execution_title(""), None);
        // A prefix with nothing after it names no lane, and must not resolve to
        // an empty lane id that would match nothing (or, worse, everything).
        assert_eq!(
            lane_id_from_execution_title(&format!("{EVAL_EXECUTION_TITLE_PREFIX}   ")),
            None
        );
    }

    /// The goal must forbid repair. An agent that "fixes" a failing eval turns a
    /// red lane green without the code under eval having changed.
    #[test]
    fn the_goal_forbids_repairing_a_failing_lane() {
        let goal = goal_for_lane(&harness_lane());
        assert!(goal.contains("make eval-monitor-golden"));
        assert!(goal.contains("Do NOT edit any file"));
        assert!(goal.contains("verdict"));
    }

    #[test]
    fn a_lane_that_declared_no_report_dir_has_no_href() {
        let dir = tempfile::tempdir().unwrap();
        let locator = CoverageReportLocator::new(dir.path(), "/evals/reports");
        assert_eq!(locator.report_href(&harness_lane(), 0), None);
    }

    #[test]
    fn a_report_is_linked_only_when_this_run_wrote_it() {
        let dir = tempfile::tempdir().unwrap();
        let locator = CoverageReportLocator::new(dir.path(), "/evals/reports/");
        let mut reporting_lane = harness_lane();
        reporting_lane.report_dir = Some("evals/monitor".to_string());

        // Declared, but the lane has never written anything.
        assert_eq!(locator.report_href(&reporting_lane, 0), None);

        // Nested one level, the way the lanes actually write (`.../latest/`).
        let nested = dir.path().join("evals/monitor/latest");
        std::fs::create_dir_all(&nested).unwrap();
        // An empty directory tree is not a report.
        assert_eq!(locator.report_href(&reporting_lane, 0), None);

        std::fs::write(nested.join("report.json"), b"{}").unwrap();
        assert_eq!(
            locator.report_href(&reporting_lane, 0).as_deref(),
            Some("/evals/reports/evals/monitor/"),
            "a trailing slash on the prefix must not double up"
        );

        // A report that predates this run belongs to a previous one. Linking it
        // would have the page vouch for a result this run did not produce.
        let far_future = chrono::Utc::now().timestamp_millis() + 60_000;
        assert_eq!(locator.report_href(&reporting_lane, far_future), None);
    }
}

//! The agentic loop, as state and phases rather than as a resident tokio task.
//!
//! Implements the archived
//! `docs/archive/plans/2026-08-25-stateless-loop-design.md`. The former loop was
//! one `for iteration in 1..=ctx.max_iterations` whose continuation *was* the
//! Rust stack of a resident task. This module makes the loop a durable value a
//! worker can load, advance by one bounded phase, and commit. Production cold
//! pickup is owned by the execution lifecycle, not by an authority-blind store
//! sweep: Artifact-backed roots are recomposed from their immutable V3 binding,
//! delegated children from their integrity-sealed recovery binding, and legacy
//! planning roots only from a durable PlanGraph. A loop-state cursor by itself
//! is never treated as permission to invent a host.
//!
//! # Historical landing order
//!
//! The design fixes it, and the reason is worth keeping in view: *"the scratch
//! extraction lands first and is verifiable on its own (the in-process loop keeps
//! working, state just lives somewhere honest). Phases land next. The store and
//! worker land last, on top of a loop that is already a state machine."*
//!
//! That sequence kept each increment independently reviewable: state moved out
//! of ambient memory before the two drivers arrived, then the stateless arm was
//! wired and ultimately became the default.
//!
//! # The durability layer, landed 2026-08-26
//!
//! [`journal`], [`effects`] and [`store`] are the durability substrate used by
//! the active stateless worker and its bounded runner/reconciler passes.
//!
//! Three rules they exist to enforce, each of which is a bug if it is only a
//! convention:
//!
//! - **The committed watermark, not the log, is authoritative.** A worker that
//!   appends and then dies leaves records ahead of the committed state. They are
//!   never replayed, never projected, and are swept *before* the next attempt
//!   appends — because appending after them would let the next commit's
//!   watermark bury them below itself.
//! - **A missing effect result is not evidence.** `retry_safety` defaults to
//!   not-safe, and an effect with no recorded outcome routes to the outward
//!   record that already knows, never to a blind re-fire.
//! - **Nothing that widens authority is written down.** [`grants::RunGrants`]
//!   derives neither `Serialize` nor `Deserialize`, so a boundary record that
//!   tried to carry one would not compile — and a test in that module asserts
//!   the derives stay absent, because a claim about what does not compile is
//!   exactly the kind that rots silently.
//!
//! # The two drivers, landed 2026-08-27
//!
//! [`driver_inproc`] is the strangler's rollback/control arm: the sequencer that
//! was written inline in `executor.rs`, moved without a behavior change.
//! [`driver_worker`] is the active default. [`ExecutionDriver`] chooses between
//! them once per run from `MAGICIAN_EXECUTION_DRIVER`; only the exact
//! `inprocess` value selects the rollback arm, while unset, blank, `stateless`,
//! and unknown values select stateless. Both hand back an [`IterationStep`] so
//! the executor's routing does not know which one it called.
//!
//! The parity claim rests on one property and nothing else: **both drivers call
//! the same six [`phases`] entry points**. A driver that grows a decision of its
//! own breaks the flip gate silently, because the differential attributes the
//! difference to the arm under test rather than to the arm that changed.
//!
//! # The two arms return the same type and do NOT do the same thing
//!
//! `driver_inproc::run_iteration` advances one ITERATION.
//! `driver_worker::advance_once` advances one PHASE, and commits it. Both ends
//! of the executor's match hand back an [`IterationStep`] because that is what
//! the caller acts on — take another iteration, or return — and it would be a
//! serious misreading to conclude from the shared return type that one is a call
//! swap for the other.
//!
//! What bridges them is a **claim-loop**, in `executor.rs`'s `StatelessArm`:
//! claim, advance one phase, commit, release, repeat, until the committed cursor
//! reaches the next iteration or a phase ends the run. Collapsing that loop into
//! a single `advance_once` would delete the per-phase commit, which is the whole
//! product of this refactor.
//!
//! # The generic runner and the host-free terminal lifecycle
//!
//! [`worker_runner`] remains the bounded scheduler primitive for a caller that
//! already owns exact [`worker_runner::HostFleet`] hosts. Production restart
//! recovery intentionally does not place a generic fleet above the loop-state
//! store. It enumerates canonical runtime executions, revalidates their durable
//! composition, and then re-enters the ordinary executor under the same id. A
//! run with no Artifact binding or durable PlanGraph is failed closed rather
//! than left behind by a no-op scan or guessed from cursor data.
//!
//! Terminal outbox debt does not need a phase host at all. [`terminal_outbox`]
//! scans committed endings with a bounded durable cursor, claims and re-verifies
//! each exact key, projects its journal through the Artifact service broadcaster,
//! saves the projector cursor fenced, and releases. It never runs a phase and it
//! ignores placement only during discovery; the claim and authoritative journal
//! re-read remain mandatory.
//!
//! This distinction closes the production composition boundary without widening
//! authority: every supported durable execution has a lifecycle composer, while
//! arbitrary store-only rows remain deliberately non-executable. Adding a global
//! phase fleet would be a new scheduling architecture, not completion of the
//! shipped recovery path, and it must preserve the same Artifact/PlanGraph and
//! sealed-child admission proofs before lending a host.
//!
//! # Where the worker's host lives, and why it is not here
//!
//! [`driver_worker::WorkerHost`] is declared in that module — it is the worker
//! driver's own seam — and **implemented in `executor.rs`**, because
//! `phases::*::run` are `pub(in ..::agentic)` and take `TrustDispatchGuard` and
//! `AgenticToolLineageState`, which nothing in this directory can name. That is
//! not a workaround for the extraction being unfinished; it is where the seam has
//! to be even after it finishes, because a worker must be able to run a phase
//! without knowing which phase's parameter list it is.
//!
//! # Why `run_loop` and not `loop`
//!
//! The design names this directory `loop/`. `loop` is a Rust keyword, so that
//! module can only be spelled `r#loop`, and every path through it — `agentic::
//! r#loop::phases::decide` — would carry the escape. `run_loop` says the same
//! thing and reads at the call site, which is where the cost would otherwise be
//! paid forever.

pub mod api_mining;
pub mod browser;
pub mod context_scratch;
pub mod controls;
pub mod driver_inproc;
pub mod driver_worker;
pub mod effects;
pub mod grants;
pub mod journal;
pub(crate) mod manual_resume_tx;
pub mod outcome;
pub mod outputs;
pub mod phases;
pub mod reconciler;
pub mod state;
pub mod steer_inbox;
pub mod store;
pub mod terminal_outbox;
pub mod worker_runner;

/// Which driver advances an execution, chosen once per run from
/// [`EXECUTION_DRIVER_ENV`].
///
/// This is the strangler switch the design's *Cutover* section asks for. Both
/// arms drive the **same six** [`phases`] entry points; what differs is where
/// the loop's state lives between iterations and who is allowed to pick the run
/// up next.
///
/// # The durable arm is the default and every mistake fails toward it
///
/// Unset, empty, or a value this build does not recognise all resolve to
/// [`Stateless`](Self::Stateless). The resident arm has no journal drain, so
/// falling back to it silently loses outbox-only activity. `inprocess` remains
/// an explicit rollback spelling during the cutover; it is never an implicit
/// selection.
///
/// An unrecognised value is *not* silent about it. [`ExecutionDriver::select`]
/// hands back the offending string in [`DriverSelection::unrecognised`] and
/// [`ExecutionDriver::from_env`] logs it at `error`. A typo that quietly picks
/// the safe arm is how a flag stops being exercised: the operator believes the
/// new path is under test, the old path runs, and nothing anywhere disagrees.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionDriver {
    /// The resident driver: [`driver_inproc::run_iteration`]. State is the Rust
    /// stack of `execute_agentically_inner`, exactly as it always was.
    ///
    /// This explicit rollback arm has no durable outbox drain. It is therefore
    /// never selected implicitly: unset, blank, and unknown configuration all
    /// choose [`Stateless`](Self::Stateless). Keeping the variant allows an
    /// operator to retreat during the cutover without pretending its event and
    /// restart guarantees match the default arm.
    Inprocess,
    /// The worker driver: a `LoopState` loaded from [`store::LoopStateStore`],
    /// advanced by one bounded unit of work, and committed.
    Stateless,
}

/// The environment variable that chooses the driver.
pub const EXECUTION_DRIVER_ENV: &str = "MAGICIAN_EXECUTION_DRIVER";

/// What [`ExecutionDriver::select`] resolved, and whether it had to complain to
/// get there.
///
/// Two fields rather than one so the *loudness* is a value a test can assert
/// on. A `select` that returned only the driver would be satisfied by an
/// implementation that silently swallowed a typo, which is the failure this
/// flag most needs to avoid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverSelection {
    /// The arm to run. Never fails to resolve — see the type docs on
    /// [`ExecutionDriver`] for why the fallback is `Stateless`.
    pub driver: ExecutionDriver,
    /// The raw value, when it named something this build does not know.
    /// `None` for unset, for empty, and for both recognised spellings.
    pub unrecognised: Option<String>,
}

impl ExecutionDriver {
    /// The spelling that selects [`Inprocess`](Self::Inprocess).
    pub const INPROCESS: &'static str = "inprocess";
    /// The spelling that selects [`Stateless`](Self::Stateless).
    pub const STATELESS: &'static str = "stateless";

    /// Resolve a raw environment value. Pure — no reads, no logging — so the
    /// resolution rules are testable without an ambient process environment.
    ///
    /// Trimmed and lowercased first: `MAGICIAN_EXECUTION_DRIVER=" Stateless "`
    /// is an operator who meant `stateless`, and refusing it would teach nobody
    /// anything. An empty value after trimming counts as unset rather than as a
    /// typo, which is the ordinary shell meaning of `VAR=`.
    pub fn select(raw: Option<&str>) -> DriverSelection {
        let Some(raw) = raw else {
            return DriverSelection {
                driver: Self::Stateless,
                unrecognised: None,
            };
        };
        let normalised = raw.trim().to_ascii_lowercase();
        if normalised.is_empty() {
            return DriverSelection {
                driver: Self::Stateless,
                unrecognised: None,
            };
        }
        match normalised.as_str() {
            Self::INPROCESS => DriverSelection {
                driver: Self::Inprocess,
                unrecognised: None,
            },
            Self::STATELESS => DriverSelection {
                driver: Self::Stateless,
                unrecognised: None,
            },
            _ => DriverSelection {
                driver: Self::Stateless,
                unrecognised: Some(raw.to_string()),
            },
        }
    }

    /// [`select`](Self::select) against the process environment, saying loudly
    /// when it had to fall back.
    ///
    /// Called once per execution rather than once per iteration: a flag that can
    /// change under a running loop would let one run be driven two ways, and the
    /// phase-differential the flip gate takes would be comparing a run against
    /// itself.
    pub fn from_env() -> Self {
        let raw = std::env::var(EXECUTION_DRIVER_ENV).ok();
        let selection = Self::select(raw.as_deref());
        if let Some(unrecognised) = selection.unrecognised.as_deref() {
            tracing::error!(
                target: "agentic.driver",
                variable = EXECUTION_DRIVER_ENV,
                value = %unrecognised,
                known = %format!("{}|{}", Self::INPROCESS, Self::STATELESS),
                "[AGENTIC-DRIVER] the execution-driver variable names a driver \
                 this build does not know; falling back to the stateless \
                 driver so the durable outbox remains active"
            );
        }
        selection.driver
    }
}

/// Runs this process has driven on the in-process arm.
static INPROCESS_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Runs this process has driven on the stateless arm.
static STATELESS_RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Resolve the arm for one run **and say which one it is**.
///
/// # Why the selection surface is still an environment variable, decided
///
/// The design says the flip *"has to be staged behind live canaries"*, and a
/// canary needs three things: a bounded population, a way to see what happened
/// to it, and a way back. Taking them in order against what this build actually
/// has:
///
/// **Population — the env var is the right grain, because the arm is
/// process-local.** What `stateless` selects today is `executor.rs`'s
/// `StatelessArm`: a per-phase commit for the runs *this* process is already
/// running. It is not a handoff — the host is borrows of one stack, and every
/// run it seeds is `Placement::Pinned` to its own worker. So there is no fleet
/// to spread a percentage across, and the deployment unit and the flag's unit
/// are the same object: one process, which is exactly one canary cell.
///
/// A per-run config surface (a percentage, a principal allowlist) was
/// considered and **not** built, for a reason rather than for effort: it would
/// create two populations inside one process whose only difference is that one
/// writes its cursor to disk — and nothing in this build can compare them. The
/// shadow-run gate was **declined** (see *Testing → Shadow-run isolation* in the
/// design) and there is no execution recording, so the readings a percentage
/// rollout exists to take cannot be taken. It would be a knob with no dial.
///
/// **Visibility — this was genuinely missing, and it is what this function
/// adds.** [`ExecutionDriver::from_env`] logged only when the value was
/// *unrecognised*. An operator who set `stateless` and watched had no way to
/// tell whether any run took the arm, how many did, or how many refused on it;
/// the only evidence was `[LOOP_WORKER]` lines at `debug`. A canary you cannot
/// read is a deploy. So every run now says which arm it took, at `info`, with
/// its execution id, and [`driver_run_counts`] holds the per-arm totals.
///
/// **A way back — this is the gap, and it is stated rather than closed.** The
/// variable is read from the process environment, so reverting needs a restart,
/// and a restart kills every in-flight resident execution — which is the very
/// problem this design exists to fix, so the revert is not free. It is
/// survivable for a canary precisely because the flag is resolved *once per
/// run*: runs already in flight keep the arm they started on and no run changes
/// arm underneath itself. A live-revertible switch needs a config surface
/// `execute_agentically_inner` can read per run, which is real work and is not
/// in this file.
pub fn driver_for_run(execution_id: Option<&str>) -> ExecutionDriver {
    use std::sync::atomic::Ordering;

    let driver = ExecutionDriver::from_env();
    let counter = match driver {
        ExecutionDriver::Inprocess => &INPROCESS_RUNS,
        ExecutionDriver::Stateless => &STATELESS_RUNS,
    };
    let nth = counter.fetch_add(1, Ordering::Relaxed) + 1;
    tracing::info!(
        target: "agentic.driver",
        execution = execution_id.unwrap_or("<unkeyed>"),
        driver = ?driver,
        variable = EXECUTION_DRIVER_ENV,
        runs_on_this_arm = nth,
        "[AGENTIC-DRIVER] this run is driven by the {} arm",
        match driver {
            ExecutionDriver::Inprocess => ExecutionDriver::INPROCESS,
            ExecutionDriver::Stateless => ExecutionDriver::STATELESS,
        }
    );
    driver
}

/// `(in-process runs, stateless runs)` since this process started.
///
/// The number a canary is read from. ~~**Nothing reads it yet.**~~ **Read since
/// 2026-08-28** by `GET /health/execution-driver`, which also reports the
/// resolved arm and any unrecognised value of [`EXECUTION_DRIVER_ENV`].
///
/// That surface is in `magician-bin`, not here, and the dependency direction is
/// why: `/health` is served by a handler in `magician_api`, and `magician`
/// depends on `magician_api` rather than the reverse — so the aggregated health
/// response cannot reach these counters, and the binary is the one layer that
/// composes both. It is a sibling route rather than new fields on `/health`
/// because that response's consumers parse fixed fields and a cutover dial is
/// not liveness.
pub fn driver_run_counts() -> (u64, u64) {
    use std::sync::atomic::Ordering;
    (
        INPROCESS_RUNS.load(Ordering::Relaxed),
        STATELESS_RUNS.load(Ordering::Relaxed),
    )
}

/// What a driver hands back after advancing an execution by one iteration.
///
/// The one type both drivers share, and deliberately the only one: it is what
/// lets `execute_agentically_inner`'s routing match be driver-agnostic. It says
/// nothing about *how* the iteration was advanced, which is the entire
/// difference between the two arms.
///
/// It is not a [`outcome::BoundaryOutcome`] and must not be collapsed into one.
/// All four boundary outcomes are [`Boundary`](Self::Boundary) here — the driver
/// has already re-performed whatever they asked for, including the backoff — and
/// the distinction that survives to the caller is the one the caller acts on:
/// take another iteration, or return.
pub enum IterationStep {
    /// The iteration ended at the turn boundary. Whatever the phase asked for
    /// has been re-performed and the epilogue has run; the loop takes the next
    /// iteration.
    Boundary,
    /// A phase ended the *run*. The caller returns this outcome verbatim, and
    /// the epilogue is skipped — which is what every run-ending path has always
    /// done, so the stuck detector and `AgenticIterationCompleted` do not fire
    /// for a turn that terminated.
    ///
    /// Boxed for the reason [`outcome::PhaseStep::Return`] is: an inline
    /// `AgenticOutcome` makes every poll frame on this path reserve the full
    /// outcome layout.
    RunEnded(Box<crate::magician_v2::execution::agentic::types::AgenticOutcome>),
    /// The run has spent its iteration ceiling. Stop taking iterations and
    /// conclude the way the loop concludes when its `for` runs out.
    ///
    /// # Only a durable driver can produce this, and that is the point
    ///
    /// [`driver_inproc`] never does, and could not: the resident arm's iteration
    /// counter *is* `for iteration in 1..=ctx.max_iterations`, so the ceiling is
    /// reached by the loop ending, and there is nothing to hand back. A durable
    /// driver has a second counter — [`state::LoopCursor::iteration`], committed
    /// — and the two can disagree, so the committed one can be past the ceiling
    /// on a turn the executor still believes it has budget for.
    ///
    /// # Which disagreements produce it, since only one direction can
    ///
    /// `skip_iterations` is **not** one of them, and an earlier version of this
    /// paragraph named it. `execute_agentically_inner` decrements it and
    /// `continue`s before either driver runs, so a skipped turn advances the
    /// executor's counter and not the committed cursor: the cursor falls
    /// *behind*. A cursor behind the counter reaches a ceiling later, never
    /// sooner. And `advance_iteration` returns `Boundary` the moment the cursor
    /// lands on `Phase::first()`, so the cursor advances at most one iteration
    /// per call — `cursor.iteration <= iteration <= ctx.max_iterations` holds
    /// throughout the `for`, and `Advanced::IterationCeiling` cannot fire from a
    /// skip at all.
    ///
    /// What does produce it is a cursor that is **ahead**, which means a
    /// [`state::LoopState`] this invocation did not start:
    ///
    /// - A run resumed under the same key. `seed_if_absent` finds a committed
    ///   state and leaves its cursor alone, so a run that paused at iteration 30
    ///   comes back with the executor's counter at 1 and the cursor at 30 —
    ///   twenty-nine iterations of budget the executor thinks it still has.
    /// - `ctx.max_iterations` lowered between invocations, which moves the
    ///   ceiling under a cursor that never moved.
    ///
    /// Neither is exercised by a test today, and saying so is more use than a
    /// cause that cannot happen: the differential harness reaches this path
    /// through no fixture it currently builds.
    ///
    /// Handing that back is what keeps the two arms honest with each other. The
    /// alternative — failing the execution from inside the worker — would lose
    /// the pause record, the `AgenticMaxIterationsReached` event and the
    /// continue-or-cancel offer that the resident arm produces for the same
    /// condition, and the flip gate would read the missing pause state as a
    /// worker bug. Re-implementing that ceremony inside the driver would be
    /// worse still: two spellings of one block, of which the pause record is the
    /// half that rots quietly.
    ///
    /// The caller ends its loop and falls into the ceremony it already has.
    CeilingReached,
}

pub(crate) const TERMINAL_SETTLEMENT_PENDING_ERROR: &str = "stateless_terminal_settlement_pending:";

pub(crate) fn is_terminal_settlement_pending_error(error: &str) -> bool {
    // This is an internal control signal, not a diagnostic substring. A model,
    // provider, or tool error that merely quotes the token must still take the
    // ordinary failure path rather than suppressing runtime/Artifact settlement.
    error.starts_with(TERMINAL_SETTLEMENT_PENDING_ERROR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_settlement_pending_signal_must_be_the_error_prefix() {
        assert!(is_terminal_settlement_pending_error(
            "stateless_terminal_settlement_pending:scope/workspace/execution:4:WaitingForUser"
        ));
        assert!(!is_terminal_settlement_pending_error(
            "provider echoed stateless_terminal_settlement_pending: but failed normally"
        ));
    }

    /// The driver source, scanned from here rather than from inside itself.
    ///
    /// A gate that reads the file it lives in counts its own assertion literals
    /// as matches, so every occurrence-count it takes is off by the number of
    /// times the test mentions the string. Scanning a sibling is the only way
    /// these numbers mean what they say.
    const DRIVER_INPROC: &str = include_str!("driver_inproc.rs");
    const EXECUTOR: &str = include_str!("../executor.rs");
    const WORKER_RUNNER: &str = include_str!("worker_runner.rs");
    const OUTBOX: &str = include_str!("phases/outbox.rs");
    const DRIVER_WORKER: &str = include_str!("driver_worker.rs");
    const RECONCILER: &str = include_str!("reconciler.rs");

    /// The first catch-all arm in a match body, as `(line number, line)`.
    ///
    /// # A BINDING catch-all is the one somebody actually writes
    ///
    /// `other => Err(anyhow!("{other:?}"))` is irrefutable, silences rustc's
    /// exhaustiveness check for every variant invented after it, reads as tidy,
    /// and contains no underscore at all — so a scan for `_ =>` waves it through
    /// while `EffectIndeterminate` and `Quarantined` quietly become one generic
    /// error.
    ///
    /// The rule: at the head of an arm, a bare lowercase identifier is a binding
    /// pattern. Every legitimate arm in the matches this gate is pointed at
    /// starts with a path — `Advanced::…`, `Some(…)`, `RecordedStep::…`,
    /// `StoreError::…` — whose first character is upper-case. `x @ ..` and a
    /// guarded binding are the same hazard spelled differently, and the same
    /// rule catches them.
    ///
    /// Shared by the two gates below rather than written twice, because two
    /// copies of a predicate is how one of them stops catching what it says it
    /// catches.
    fn first_catch_all(match_body: &str) -> Option<(usize, &str)> {
        let is_catch_all = |trimmed: &str| -> bool {
            if trimmed.starts_with("//") {
                return false;
            }
            let head: String = trimmed
                .chars()
                .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
                .collect();
            let Some(first) = head.chars().next() else {
                // Punctuation — a tuple, a slice, a reference pattern, or the
                // body of an arm rather than its head. Not a catch-all.
                return false;
            };
            if !(first == '_' || first.is_lowercase()) {
                return false;
            }
            let rest = trimmed[head.len()..].trim_start();
            rest.starts_with("=>") || rest.starts_with('@') || rest.starts_with("if ")
        };
        match_body
            .lines()
            .enumerate()
            .find(|(_, line)| is_catch_all(line.trim_start()))
            .map(|(index, line)| (index + 1, line.trim()))
    }

    /// Whether `Path::Variant` begins an **arm head** in this match body.
    ///
    /// # Why a head and not a mention, which is what this used to check
    ///
    /// `body.contains("Advanced::RunEnded")` is satisfied by any occurrence of
    /// those characters, and both callers of this gate contain plenty that are
    /// not arms. `executor.rs`'s `Committed` arm names `` `Advanced::RunEnded` ``
    /// inside an error string, so the anchor for `RunEnded` was already
    /// satisfied by text belonging to a different arm — the gate would have gone
    /// on passing with the real arm deleted.
    ///
    /// A head is also what makes the gate catch the edit it is there for. rustc
    /// enforces exhaustiveness on its own, so the only edits it does *not*
    /// refuse are the ones that keep every variant covered under a different
    /// shape: a fold into a neighbour's or-pattern
    /// (`Advanced::A { .. } | Advanced::B { .. } =>`), where the folded name is
    /// no longer at the start of a line, and an alias import, where the name in
    /// the source is not the name in the list. Both leave `contains` green.
    ///
    /// The trailing character is checked so a variant cannot be satisfied by a
    /// longer one that starts with its name.
    fn names_at_arm_head(body: &str, path: &str, variant: &str) -> bool {
        let anchor = format!("{path}::{variant}");
        body.lines().any(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") {
                return false;
            }
            let Some(rest) = trimmed.strip_prefix(&anchor) else {
                return false;
            };
            !rest
                .chars()
                .next()
                .is_some_and(|next| next.is_alphanumeric() || next == '_')
        })
    }

    /// The body of the first `match <scrutinee> {` in `source`, bounded by the
    /// next column-zero `}`.
    ///
    /// The bound matters: scanning past the enclosing item would count arms in
    /// functions that are entitled to a catch-all.
    fn match_body<'a>(source: &'a str, scrutinee: &str, what: &str) -> &'a str {
        source
            .split_once(scrutinee)
            .unwrap_or_else(|| panic!("{what} must exist, or this gate passes vacuously"))
            .1
            .split_once("\n}\n")
            .unwrap_or_else(|| panic!("{what}'s enclosing item must close at column zero"))
            .0
    }

    #[test]
    fn an_unset_driver_runs_the_stateless_arm() {
        let selection = ExecutionDriver::select(None);
        assert_eq!(
            selection.driver,
            ExecutionDriver::Stateless,
            "an unset {EXECUTION_DRIVER_ENV} must keep the journal drain active"
        );
        assert_eq!(
            selection.unrecognised, None,
            "not setting a flag is not a mistake and must not be reported as one"
        );

        // `VAR=` is the shell's way of saying nothing, not a typo.
        assert_eq!(
            ExecutionDriver::select(Some("")),
            DriverSelection {
                driver: ExecutionDriver::Stateless,
                unrecognised: None,
            }
        );
        assert_eq!(
            ExecutionDriver::select(Some("   ")),
            DriverSelection {
                driver: ExecutionDriver::Stateless,
                unrecognised: None,
            }
        );
    }

    #[test]
    fn both_arms_are_reachable_by_name() {
        assert_eq!(
            ExecutionDriver::select(Some(ExecutionDriver::INPROCESS)).driver,
            ExecutionDriver::Inprocess
        );
        assert_eq!(
            ExecutionDriver::select(Some(ExecutionDriver::STATELESS)).driver,
            ExecutionDriver::Stateless,
            "the stateless arm must be selectable, or the strangler has one arm"
        );

        // The constants are what the docs and the error message quote, so they
        // are pinned against literals rather than against themselves — two
        // spellings of one rename agree with each other while every operator
        // runbook stops working.
        assert_eq!(ExecutionDriver::INPROCESS, "inprocess");
        assert_eq!(ExecutionDriver::STATELESS, "stateless");
        assert_eq!(EXECUTION_DRIVER_ENV, "MAGICIAN_EXECUTION_DRIVER");

        // Case and stray whitespace are an operator, not an attacker.
        assert_eq!(
            ExecutionDriver::select(Some("  STATELESS \n")),
            DriverSelection {
                driver: ExecutionDriver::Stateless,
                unrecognised: None,
            }
        );
    }

    #[test]
    fn an_unrecognised_driver_falls_back_and_says_so() {
        // Both halves matter and they fail in opposite directions. Without the
        // first, a typo would fail an execution that had a perfectly good arm to
        // run. Without the second, a typo silently runs the arm the operator
        // thought they had switched off — and the stateless path stops being
        // exercised on exactly the day someone believes it is.
        let selection = ExecutionDriver::select(Some("statelesss"));
        assert_eq!(
            selection.driver,
            ExecutionDriver::Stateless,
            "an unknown driver name must fall back to the durable arm"
        );
        assert_eq!(
            selection.unrecognised.as_deref(),
            Some("statelesss"),
            "the fallback must hand back the value it refused; a silent default \
             is how a flag stops being tested"
        );

        // The raw text, not the normalised one: an operator greps the log for
        // what they typed.
        assert_eq!(
            ExecutionDriver::select(Some(" In-Process "))
                .unrecognised
                .as_deref(),
            Some(" In-Process "),
        );
    }

    #[test]
    fn every_run_is_counted_on_the_arm_it_took() {
        // The canary's only reading. Before `driver_for_run` existed, an
        // operator who set the flag and watched had nothing to look at: the
        // resolution logged only when it FAILED to recognise a value, so a
        // correctly-set flag and an ignored one produced identical output.
        //
        // Deliberately does NOT set `MAGICIAN_EXECUTION_DRIVER`. The variable is
        // process-global and this crate's unit tests share a binary, so setting
        // it here would flip the driver under every concurrently-running test
        // that drives `execute_agentically` — which is why the phase
        // differential lives in a separate binary. The property under test does
        // not need it: whichever arm this build resolves to, the count for THAT
        // arm must move.
        let (inprocess_before, stateless_before) = driver_run_counts();
        let driver = driver_for_run(Some("exec-under-test"));
        let (inprocess_after, stateless_after) = driver_run_counts();

        // Strictly greater rather than exactly one more: the counters are
        // process-global and any other test in this binary that drives a run
        // increments them too. An equality here would be a flake, and a flake
        // is how a gate gets deleted.
        match driver {
            ExecutionDriver::Inprocess => {
                assert!(
                    inprocess_after > inprocess_before,
                    "a run driven by the in-process arm must be counted on it"
                );
                assert_eq!(
                    stateless_after, stateless_before,
                    "and must not be counted on the other one — a canary reading both arms as \
                     busy tells an operator nothing"
                );
            },
            ExecutionDriver::Stateless => {
                assert!(
                    stateless_after > stateless_before,
                    "a run driven by the stateless arm must be counted on it"
                );
                assert_eq!(inprocess_after, inprocess_before);
            },
        }

        assert_eq!(
            driver,
            ExecutionDriver::from_env(),
            "`driver_for_run` must resolve exactly what `from_env` does; a second resolution rule \
             is how the arm an operator reads in the log stops being the arm that ran"
        );
    }

    /// Where the worker driver's host seam begins in `executor.rs`.
    ///
    /// The split point the phase-call gate below is written against. If this
    /// string stops matching, that gate fails with "must implement the worker
    /// driver's host seam" rather than passing vacuously — which is the right
    /// failure, because a host that moved is exactly the change the gate exists
    /// to notice.
    const HOST_IMPL: &str =
        "impl<'h> super::run_loop::driver_worker::WorkerHost for InProcessWorkerHost<'h>";

    /// Accepted records and inline fallback stay mutually exclusive.
    ///
    /// The stateless host projects records it accepted, while the producer emits
    /// inline only when journalling returned `false`. That includes the explicit
    /// in-process rollback arm (no drain) and stateless records refused for
    /// address, size, or capacity. An unconditional inline call would duplicate
    /// accepted records; removing the fallback would blank the rollback arm.
    ///
    /// A test rather than a type because neither half can name the other: the
    /// flag is a trait method in `executor.rs` and the emit is a statement in a
    /// phase module, and nothing in the language relates them. Writing the
    /// call's spelling HERE is safe — the self-matching hazard the census
    /// records applies to the file being counted, and nobody greps this one.
    #[test]
    fn accepted_records_project_and_refused_records_emit_inline() {
        let producer = OUTBOX
            .split_once("fn journal_and_emit(")
            .expect("outbox must expose its transport producer")
            .1
            .split_once("\n}")
            .expect("transport producer must have a body")
            .0;
        assert!(
            producer.contains("let journalled = journal(")
                && producer.contains("if !journalled {")
                && producer.contains("executors.emit_event(event);"),
            "the producer must emit inline only when the record was not accepted"
        );

        let (_, the_host) = EXECUTOR
            .split_once(HOST_IMPL)
            .expect("executor.rs must implement the worker driver's host seam");
        let projection_declared = the_host.contains("fn emits_projected_events");

        assert!(
            projection_declared,
            "the stateless host must project accepted records; inline delivery is reserved for \
             the producer's `!journalled` fallback"
        );
    }

    /// A children wake is only a receipt after the old segment is terminal.
    ///
    /// A committed `WaitReason::Children` park never resumes its own cursor.
    ///
    /// - The delegating run resumes
    ///   through the pause record its outcome carries, and that resume gets its
    ///   OWN execution key, so it never claims the parked state.
    /// - The lifecycle owner commits `RunEnded { HandedOff }` and clears the old
    ///   wait before publishing the wake resolution. The resolution is a
    ///   durable readiness receipt, not scheduler permission for the old key.
    /// - Only after the checkpoint dispatch is owned does the lifecycle consume
    ///   that exact receipt. A retry may restore the receipt, but never append a
    ///   second handoff terminal or re-enter the old mid-`Apply` cursor.
    #[test]
    fn only_the_terminal_first_handoff_protocol_may_resolve_a_children_wake() {
        // PRODUCTION ONLY. Every one of these files calls `resolve_wake` from
        // its own tests — spy wrappers that delegate to an inner store, and
        // fixtures that resolve a wake to prove the store honours it — so a scan
        // that did not cut the test module reports four callers on a tree that
        // has none. It did, on the first version of this test, which is why the
        // split is here rather than trusted to a `//` check.
        //
        // Cut at `mod tests {` at column zero, and not at `#[cfg(test)]`:
        // `executor.rs` carries `#[cfg(any(test, feature = "test-fixtures"))]`
        // on individual items thousands of lines above its test module, so
        // splitting there would discard most of the production file and make
        // this gate blind in the direction that matters.
        let production_of = |name: &str, source: &'static str| -> &'static str {
            let marker = "\nmod tests {";
            let (production, _) = source.split_once(marker).unwrap_or_else(|| {
                panic!(
                    "{name} has no `mod tests {{` at column zero, so this gate cannot tell its \
                     production code from its tests. It scanned the whole file until now; if the \
                     file was restructured, teach this split the new shape rather than deleting \
                     it — a gate that silently scans test code reports callers that do not exist"
                )
            });
            production
        };

        let has_call = |name: &str, source: &'static str| {
            production_of(name, source).lines().any(|line| {
                let code = line.trim();
                !code.starts_with("//") && code.contains(".resolve_wake(")
            })
        };

        let callers = [
            ("executor.rs", EXECUTOR),
            ("driver_worker.rs", DRIVER_WORKER),
            ("worker_runner.rs", WORKER_RUNNER),
            ("reconciler.rs", RECONCILER),
        ];
        let found: Vec<&str> = callers
            .iter()
            .filter(|(name, source)| has_call(name, source))
            .map(|(name, _)| *name)
            .collect();

        assert_eq!(
            found,
            vec!["reconciler.rs"],
            "only the exact-key handoff protocol may publish a children wake; any other caller \
             can make the old cursor independently runnable and create a second continuation"
        );
    }

    #[test]
    fn a_phase_is_reached_only_through_a_driver_or_the_worker_host() {
        // The strangler's precondition, restated for the shape that landed.
        //
        // Until 2026-08-27 this asserted `executor.rs` called no phase at all,
        // and that was right while the stateless arm refused: any phase call in
        // the executor would have been a step outside both drivers.
        //
        // It cannot stay that way, and the reason is structural rather than a
        // convenience. `phases::*::run` are `pub(in ..::agentic)` and take
        // `TrustDispatchGuard` and `AgenticToolLineageState`, which
        // `driver_worker` cannot name — so `WorkerHost` has to be implemented in
        // `executor.rs`, and a host that runs a phase is a phase call in that
        // file by construction.
        //
        // The property worth gating is therefore narrower and is still exactly
        // the one that matters: **no phase call may sit above that host**, where
        // it would be the executor's own loop running a phase, and the two arms
        // would silently share a step — the one way a phase-differential
        // comparison can look clean while being meaningless.
        let (above_the_host, host_and_tests) = EXECUTOR.split_once(HOST_IMPL).expect(
            "executor.rs must implement the worker driver's host seam; without one the \
             stateless arm has no way to run a phase and this gate cannot say where a phase \
             call is allowed",
        );
        let tests_start = host_and_tests
            .find("\n#[cfg(test)]\nmod tests")
            .into_iter()
            .chain(host_and_tests.find("\n#[cfg(any(test"))
            .min()
            .unwrap_or(host_and_tests.len());
        let the_host = &host_and_tests[..tests_start];

        // ── the two projection rails travel together, or the projector wedges ──
        //
        // A host that implements only the unnamed rail COMPILES, because
        // `emit_projected_named_event` has a default — and that default is a
        // REFUSAL, not a no-op.
        // `ProjectorCursor::project` answers a refusal by stopping the walk with
        // its mark still BELOW the refused record, so the next boundary reloads
        // the same cursor and refuses the same record, forever. The named rail
        // carries `plan.step.started` and `tool.result.projected`, so the first
        // tool call of essentially every run reaches it.
        //
        // This is a text scan because the compiler cannot express it: the whole
        // point of a defaulted trait method is that omitting it is legal. It is
        // checked HERE rather than in a `driver_worker` test because every host
        // in that module's tests is a fake, and the fakes implement BOTH rails —
        // which is exactly how the production host came to be missing one while
        // the entire suite stayed green. A fake richer than production certifies
        // a capability production does not have.
        // The guard is the UNNAMED rail, not `emits_projected_events`. Keying it
        // on the declaration would have been the natural reading of the hazard
        // and is the wrong test: `emits_projected_events` is what the final
        // switch ADDS, so a gate guarded on it asserts nothing until the day of
        // the flip and cannot vouch for the tree it actually ships in. Verified
        // by deleting the named rail — the gate stayed green. Keyed on
        // `emit_projected_event_with_key`, which the host implements TODAY, it fails on
        // that same deletion, which is the only version of this gate worth
        // having: a host that projects at all must project both rails.
        if the_host.contains("fn emit_projected_event_with_key") {
            assert!(
                the_host.contains("fn emit_projected_named_event"),
                "the worker host implements `emit_projected_event_with_key` without implementing \
                 `emit_projected_named_event`. The trait's default REFUSES, and a refusal \
                 stops the projector's walk below the refused record permanently — so the \
                 first journalled `plan.step.*` or `tool.result.projected` would wedge every \
                 later boundary on every run. Implement the named rail beside the unnamed one"
            );
        }

        // `apply` is reached through TWO entry points, not one. The phase split
        // its gate from its dispatch so a driver can commit each effect's intent
        // between them — `phases::apply::ApplyGate` carries the argument — so
        // the host calls `apply::gate(` and `apply::dispatch(` where it used to
        // call `apply::run(`.
        //
        // The "exactly one" rule below is unchanged and is what still has teeth:
        // it is now asserted of each half separately, so a host that gates once
        // and dispatches twice fires the same batch twice per claim and this
        // fails — which "at least one" over the pair would not catch.
        for anchor in [
            "phases::prepare::run(",
            "phases::observe::run(",
            "phases::decide::run(",
            "phases::resolve::run(",
            "phases::apply::gate(",
            "phases::apply::dispatch(",
            "phases::epilogue::run(",
        ] {
            let offender = above_the_host.lines().enumerate().find(|(_, line)| {
                let trimmed = line.trim_start();
                !trimmed.starts_with("//") && line.contains(anchor)
            });
            assert!(
                offender.is_none(),
                "executor.rs:{} calls `{anchor}` above the worker host; a phase the \
                 executor's own loop runs is a phase no driver owns",
                offender.map(|(index, _)| index + 1).unwrap_or_default()
            );
            // Exactly one, not "at least one": two calls to the same phase entry
            // point from one host means that entry point runs twice per claim,
            // which is the failure the one-phase-per-commit rule exists to
            // prevent and which no type in this directory would catch.
            assert_eq!(
                the_host.matches(anchor).count(),
                1,
                "the worker host must call `{anchor}` exactly once"
            );
        }
        // And the two halves of `Apply` travel together. A host that gates and
        // never dispatches decides a whole turn and fires none of it; one that
        // dispatches without gating cannot compile, but can be arrived at by
        // deleting the gate — and then this is what says so.
        assert_eq!(
            the_host.contains("phases::apply::gate("),
            the_host.contains("phases::apply::dispatch("),
            "the worker host implements one half of Apply's gate/dispatch seam and not the \
             other. The gate decides an entire turn without firing any of it and the dispatch \
             is what fires it; a host with only the first runs no effects at all"
        );

        assert!(
            EXECUTOR.contains("driver_inproc::run_iteration("),
            "the executor must advance an iteration through the in-process \
             driver; a loop that no longer calls it has lost the control arm of \
             the strangler"
        );
        assert!(
            EXECUTOR.contains("ExecutionDriver::Stateless"),
            "the executor must have a `Stateless` arm to route to; an \
             unreachable arm is a flag with one setting"
        );
        assert!(
            EXECUTOR.contains("driver_worker::advance_once("),
            "the stateless arm must reach the worker driver's one-phase entry \
             point. An arm that called `driver_inproc::run_iteration` instead \
             would compile, pass the flag test, and quietly have no per-phase \
             commit at all — which is the entire product of this refactor"
        );
        assert!(
            !EXECUTOR.contains("execution_driver_not_wired"),
            "the stateless arm's placeholder refusal is still in the executor; \
             the flag now has two working settings and the old error would make \
             an operator believe otherwise"
        );
    }

    #[test]
    fn the_stateless_arm_answers_every_advance_outcome_by_name() {
        // Exhaustiveness itself is rustc's job: the claim-loop's `match advanced`
        // has no catch-all, so a variant added to `Advanced` fails the build.
        //
        // What rustc will NEVER catch is somebody ADDING one. A `_ =>` arm
        // compiles forever and silently swallows every outcome invented after it
        // — including the two that must never be swallowed, `EffectIndeterminate`
        // (an effect with no result and no licence to fire) and `Quarantined` (a
        // run held for an operator). Both would become "keep going".
        let arm = EXECUTOR
            .split_once("match advanced {")
            .expect(
                "the stateless arm must match on what one advance returned; without that match \
                 there is no claim-loop and `advance_once`'s outcomes are being discarded",
            )
            .1;
        // Bounded to the end of `impl StatelessArm`, which is the next
        // column-zero `}` in the file. The claim-loop contains no column-zero
        // brace, and scanning past it would count arms in functions that are
        // entitled to a catch-all.
        let arm = arm
            .split_once("\n}\n")
            .expect("`impl StatelessArm` must close")
            .0;

        // A BINDING catch-all is the one this used to miss, and it is the one
        // somebody actually writes — see `first_catch_all` for the argument and
        // for why the predicate is shared with the runner's gate below.
        let offender = first_catch_all(arm);
        assert!(
            offender.is_none(),
            "the claim-loop grew a catch-all arm at line {} of the match (`{}`); a driver that \
             cannot say what it does with `Quarantined` has not implemented it, and a BINDING \
             catch-all hides that just as completely as `_ =>`",
            offender
                .map(|(line_number, _)| line_number)
                .unwrap_or_default(),
            offender.map(|(_, line)| line).unwrap_or_default()
        );

        // The scan enforces a NEGATIVE rule — "no arm swallows the rest" — and a
        // negative rule is satisfied by a match with no arms at all. So the
        // positive half: every variant, each beginning an ARM HEAD.
        //
        // A head rather than a mention, and the difference is not pedantry: this
        // arm's `Committed` branch names `Advanced::RunEnded` inside an error
        // string, so a `contains` check for `RunEnded` was already satisfied by
        // prose belonging to a different arm and would have stayed green with
        // the real arm deleted. See `names_at_arm_head` for what the head form
        // catches that rustc does not.
        for outcome in ADVANCE_OUTCOMES {
            assert!(
                names_at_arm_head(arm, "Advanced", outcome),
                "the claim-loop must answer `Advanced::{outcome}` at an arm head of its own; a \
                 name that appears only inside a comment, a string or another arm's or-pattern \
                 is not an answer"
            );
        }
    }

    /// Every `Advanced` variant, named against literals.
    ///
    /// Against literals rather than against the enum, because the point is that
    /// each caller *reads* them: a spelling that agrees with itself while a
    /// caller stopped handling one is the failure mode. Shared by the executor's
    /// claim-loop gate and the runner's, so a variant can never be added to the
    /// list for one and forgotten for the other.
    const ADVANCE_OUTCOMES: [&str; 17] = [
        "Committed",
        "RecoveryRewound",
        "RunEnded",
        "LeftPark",
        "NothingToAdvance",
        "LeaseHeld",
        "NotClaimable",
        "Parked",
        "NotYetRunnable",
        "RunAlreadyEnded",
        "TerminalSettlementPending",
        "IterationCeiling",
        "EffectIndeterminate",
        "ExecutionFailed",
        "PhaseFailed",
        "Quarantined",
        "Refused",
    ];

    /// The four `IterationCarry` members on `executor.rs`'s
    /// `InProcessWorkerHost`, spelled here as data.
    ///
    /// The epilogue's two entries in [`state::in_memory_inputs_of`] are **not**
    /// carry members — the host measures them at iteration entry and Prepare
    /// publishes them in [`state::IterationCheckpoint`] — so they have no
    /// corresponding `carry_missing` site in `executor.rs` and are excluded
    /// from the count below rather than silently inflating it.
    ///
    /// # This is the THIRD hand-maintained list, and it gates the other two
    ///
    /// `state::in_memory_inputs_of` and the refusal sites in `executor.rs` are
    /// the two the gate compares. This const is what decides which of the
    /// declared names are expected to *have* a refusal site, so a fifth carry
    /// value added to both of those lists still passes the count until it is
    /// added here as well. Recorded rather than removed: deriving it would mean
    /// parsing `executor.rs`'s struct, which is the same class of reading the
    /// gate already does badly.
    const CARRY_MEMBERS: [&str; 4] = [
        "browser_primitive_enabled",
        "observed_state",
        "decided",
        "resolved",
    ];

    /// The refusal `executor.rs` raises when a phase is entered without the
    /// value an earlier phase of the same iteration produced, as a literal.
    ///
    /// Spelled once here and never repeated in prose anywhere `EXECUTOR` can see
    /// it: this gate COUNTS occurrences in that file, so a comment over there
    /// quoting the call would inflate the count and fail a correct tree.
    ///
    /// # It sees ONE spelling, which bounds what the count below can claim
    ///
    /// A phase that starts reading a carry value through `unwrap_or_default()`,
    /// through `if let Some(..)`, or through a refusal worded any other way adds
    /// no occurrence of this literal. The count stays at four, the gate stays
    /// green, and the entry that nothing refuses is live. That is a real hole
    /// and it is stated rather than implied, because the assertion's own message
    /// used to describe the count as if it closed it.
    const CARRY_REFUSAL: &str = "carry_missing(phase, \"";

    #[test]
    fn the_names_a_phase_carries_in_memory_have_one_spelling() {
        // Two lists describe the same hand-off and they are read by different
        // parties: `state::in_memory_inputs_of` is what a HOLDER consults before
        // deciding it may enter a phase; the refusal sites in `executor.rs` are
        // what actually reads those values and fails when one is absent.
        //
        // The dangerous direction is the state-side list going SHORT. A name
        // dropped there makes a phase look enterable that will fail on entry —
        // and it fails from inside the claim, so the run is charged a lease, a
        // fence and an attempt against its quarantine counter for a defect that
        // belongs to the pickup. The other direction merely refuses a pickup
        // that would have worked.
        let declared: Vec<&'static str> = outcome::Phase::ORDER
            .iter()
            .flat_map(|phase| state::in_memory_inputs_of(*phase).iter().copied())
            .collect();

        for member in CARRY_MEMBERS {
            assert!(
                declared.contains(&member),
                "`state::in_memory_inputs_of` does not name the carry member `{member}`, so a \
                 holder asking it whether a phase is enterable gets `yes` for a phase that will \
                 refuse the moment it reads that value"
            );
            assert!(
                EXECUTOR.contains(&format!("{}{}\"", CARRY_REFUSAL, member)),
                "`executor.rs` no longer refuses a phase entered without `{member}`; either the \
                 read moved or the name changed, and the state-side list now describes a \
                 hand-off that does not exist"
            );
        }

        // A count over ONE literal, and what it can conclude is bounded by
        // that. It catches a refusal site deleted, duplicated or moved while
        // the state-side list stayed put — the drift between two lists that
        // already exist. It cannot catch a carry value acquired through a read
        // spelled some other way, because such a read produces no occurrence to
        // count; nor can it see a fifth carry member until `CARRY_MEMBERS`
        // above names it. Both holes are in the doc comments on the two consts.
        let sites = EXECUTOR.matches(CARRY_REFUSAL).count();
        let expected = declared
            .iter()
            .filter(|name| CARRY_MEMBERS.contains(*name))
            .count();
        assert_eq!(
            sites, expected,
            "`executor.rs` spells `{CARRY_REFUSAL}` {sites} times and `CARRY_MEMBERS` names \
             {expected} of the values `state::in_memory_inputs_of` declares. The two lists have \
             drifted: a refusal site was added, removed or renamed without the state-side list \
             following, or the reverse. This compares SPELLINGS — a carry value read without \
             this refusal is invisible to it, so a green count is not a proof that every \
             in-memory input refuses a cold entry"
        );
    }

    #[test]
    fn the_runner_classifies_every_advance_outcome_by_name() {
        // The same gate as the claim-loop's, pointed at the OTHER caller of
        // `advance_once`. It is worth having twice because the two callers fail
        // differently: the executor's arm turns an unhandled outcome into a
        // failed run, and the runner's would turn one into a key it keeps
        // claiming — a spin nobody is watching, on work nobody asked it to do.
        //
        // `worker_runner::classify` is private, so this reads the source rather
        // than calling it. The classifier being pure and total is what makes the
        // source honest about the behaviour: there is nowhere else in that file
        // an `Advanced` is interpreted.
        let arm = match_body(
            WORKER_RUNNER,
            "match advanced {",
            "the runner must match on what one advance returned",
        );

        let offender = first_catch_all(arm);
        assert!(
            offender.is_none(),
            "the runner's classifier grew a catch-all arm at line {} (`{}`); a runner that \
             cannot say what it does with `EffectIndeterminate` has not implemented it, and the \
             default a catch-all lands on is `KeepGoing` on a key that will never advance",
            offender
                .map(|(line_number, _)| line_number)
                .unwrap_or_default(),
            offender.map(|(_, line)| line).unwrap_or_default()
        );

        for outcome in ADVANCE_OUTCOMES {
            assert!(
                names_at_arm_head(arm, "Advanced", outcome),
                "the runner must classify `Advanced::{outcome}` at an arm head of its own"
            );
        }

        // The nested matches the classifier delegates to, held to the same rule.
        // `Refusal` is where the fail-closed decision lives and `StoreError` is
        // where it is actually taken, so a catch-all in either is how "the
        // substrate is down" quietly becomes "hold this one key".
        //
        // `match projection {` and `match terminal {` are here because both were
        // once folded into one answer a level ABOVE, where this gate cannot see:
        // `Refusal::Projection(_)` gave five `LoopStateRefusal` variants a hold
        // lasting the worker's life, one of which carries its own expiry, and
        // `terminal.is_resumable()` gave a paused run the same re-check interval
        // as a park. An arm head starting upper-case is not a catch-all by this
        // scan's definition, so the only thing that stops the next such fold is
        // naming the inner match here.
        for (scrutinee, what) in [
            (
                "match refusal {",
                "the runner must classify every boundary refusal",
            ),
            (
                "match projection {",
                "the runner must classify every projection refusal",
            ),
            (
                "match terminal {",
                "the runner must decide how every terminal is resumed",
            ),
            (
                "match error {",
                "the runner must classify every store error",
            ),
        ] {
            let body = match_body(WORKER_RUNNER, scrutinee, what);
            let offender = first_catch_all(body);
            assert!(
                offender.is_none(),
                "`{scrutinee}` grew a catch-all arm at line {} (`{}`)",
                offender
                    .map(|(line_number, _)| line_number)
                    .unwrap_or_default(),
                offender.map(|(_, line)| line).unwrap_or_default()
            );
        }
    }

    #[test]
    fn the_runner_never_sleeps_while_it_holds_a_claim() {
        // `BoundaryOutcome::Retry` exists because sleeping holds a worker for the
        // whole backoff — the resident-task cost this design exists to remove. A
        // runner that slept inside a turn would reintroduce it at the layer
        // above, and would do it while holding a lease, which is strictly worse
        // than what the loop used to do.
        //
        // Source-scanned rather than measured, for the same reason the backoff
        // contract test in `outcome.rs` is: a behavioural test would need a real
        // clock and would pass on a sleep short enough not to be noticed.
        // The runner's PRODUCTION half only. Bounded at `#[cfg(test)]` because
        // this gate is a claim about what a worker does while it holds a claim,
        // and its own fixtures — a test that waits out a holdoff, say — are not
        // that. Unbounded, the count included the test module and the rule
        // became "nobody in this file may wait for anything", which is a
        // different and wrong rule.
        let production = WORKER_RUNNER
            .rsplit_once("\n#[cfg(test)]\nmod tests")
            .expect("the runner must have a test module")
            .0;
        let (before_run, from_run) = production
            .split_once("    pub async fn run(")
            .expect("the runner must expose the resident sweep loop");
        // Comments filtered, because both halves of this file DISCUSS sleeping —
        // a scan that counted prose would fail on the doc comment that explains
        // why the sleep is where it is.
        let sleeps = |source: &str| -> usize {
            source
                .lines()
                .filter(|line| !line.trim_start().starts_with("//"))
                .filter(|line| line.contains("tokio::time::sleep"))
                .count()
        };
        assert_eq!(
            sleeps(before_run),
            0,
            "the runner sleeps somewhere above `run`; the only place a sweep may wait is \
             BETWEEN sweeps, where it holds no lease and no key"
        );
        assert_eq!(
            sleeps(from_run),
            1,
            "`run` must wait in exactly one place — a second one is a second policy for how \
             long a worker idles, and the two would disagree"
        );
        assert!(
            from_run.contains("cancel.cancelled()"),
            "the one sleep must race the cancellation token; a worker that cannot be stopped \
             during its idle backoff takes the whole backoff to shut down"
        );
    }

    #[test]
    fn the_two_conditions_the_stateless_arm_got_wrong_stay_fixed() {
        // Both of these were wrong in a first draft and corrected on re-read,
        // and neither has a cheap unit test that could exist: they are reachable
        // only through a live `InProcessWorkerHost`, which is made of borrows of
        // `execute_agentically_inner`'s stack. So they are pinned as source
        // text, in the style the rest of this module's gates use — a weaker
        // guarantee than a behavioural test and a much stronger one than the
        // nothing that was here.

        // 1. THE TURN BOUNDARY IS READ OFF THE CURSOR.
        //
        // `execute_agentically_inner` decrements `loop_protective.skip_iterations`
        // and `continue`s before either driver runs, so a skipped turn advances
        // the executor's counter and NOT the committed cursor — permanently, by
        // one per skip. A boundary test of `cursor.iteration > iteration` is
        // therefore false at the real boundary after the first skip, and the
        // claim-loop runs a SECOND iteration's phases inside one call. No phase
        // reports that, and the resident arm never does it.
        let arm = EXECUTOR
            .split_once("match advanced {")
            .expect("the stateless arm must match on what one advance returned")
            .1
            .split_once("\n}\n")
            .expect("`impl StatelessArm` must close")
            .0;
        let executable: Vec<&str> = arm
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        assert!(
            executable
                .iter()
                .any(|line| line.contains("cursor.phase == Phase::first()")),
            "the turn boundary must be detected from the committed CURSOR reaching the first \
             phase of the next iteration; `skip_iterations` desynchronises the executor's counter \
             from it and a counter comparison is silently wrong from the first skip onward"
        );
        assert!(
            !executable
                .iter()
                .any(|line| line.contains("cursor.iteration > iteration")),
            "the boundary is being decided by comparing the committed cursor against the \
             executor's iteration counter; those two disagree permanently after one \
             `skip_iterations`, and the claim-loop would run two iterations' phases per call"
        );

        // 2. `adopt_owner` VERIFIES AND REFUSES, IT DOES NOT INSTALL.
        //
        // `RunIdentity::apply_context` derives `agent_id` as
        // `ctx.agent_id.or(ctx.active_owner_agent_id)`. A delegated child
        // legitimately runs with `ctx.agent_id == None` while its owner snapshot
        // knows the agent, so writing the identity's value back turns that
        // `None` into `Some(<owner>)` on the FIRST phase entry of every
        // stateless run — a change the resident arm never makes, on a field that
        // feeds prompt identity, event envelopes and `llm_task_ref_for_context`.
        // Installing `browser_transports` is the same shape of mistake in the
        // other direction: the committed ceiling is not the narrower one, it is
        // merely older.
        const ADOPT: &str = "fn adopt_owner(";
        let adopt_owner = EXECUTOR
            .split_once(ADOPT)
            .expect(
                "the worker host must implement `adopt_owner`; if its signature moved, this gate \
                 fails loudly rather than passing vacuously, which is the right failure",
            )
            .1
            .split_once("\n    }\n")
            .expect("`adopt_owner` must close")
            .0;
        for installed in ["self.ctx.agent_id =", "self.ctx.browser_transports ="] {
            assert!(
                !adopt_owner.contains(installed),
                "`adopt_owner` installs `{installed}` onto the live context. It must only \
                 VERIFY: a half-adopt — new agent, old tool scope, old policy fingerprint — is \
                 the exact pairing `browser_transports` exists to prevent, and only \
                 `apply_owner_execution_profile` can re-resolve the other half"
            );
        }
        assert!(
            adopt_owner.contains("Err(format!("),
            "an owner mismatch must be returned to the driver, not merely logged; continuing \
             under the ambient owner executes the committed run with uncommitted authority"
        );
        assert!(adopt_owner.contains("complete_run_identity_matches(identity, live.as_ref())"));
        let identity_match = EXECUTOR
            .split_once("fn complete_run_identity_matches(")
            .expect("the adoption comparison helper must exist")
            .1
            .split_once("\n}")
            .expect("the adoption comparison helper must close")
            .0;
        assert!(
            identity_match.contains("committed == live"),
            "same-agent pickup must compare the complete identity value; agent id alone is not \
             an authority/cache fingerprint"
        );
    }

    #[test]
    fn cold_checkpoint_refuses_primitive_authority_without_a_stable_identity() {
        let eligibility = EXECUTOR
            .split_once("fn checkpoint_decision_contains_primitive(")
            .expect("Primitive checkpoint eligibility must be decided explicitly")
            .1
            .split_once("\nfn checkpoint_capability_contracts(")
            .expect("the Primitive eligibility helper must close")
            .0;
        assert!(eligibility.contains("ImplementationType::Primitive { .. }"));

        let contracts = EXECUTOR
            .split_once("fn checkpoint_capability_contracts(")
            .expect("the checkpoint must bind governed capability contracts")
            .1
            .split_once("\nfn build_resolve_checkpoint(")
            .expect("the capability-contract helper must close before checkpoint construction")
            .0;
        assert!(contracts.contains("canonical_json_bytes"));
        assert!(contracts.contains("blake3::hash"));
        let normalized_contracts = contracts.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            contracts.contains("ImplementationType::Primitive")
                && normalized_contracts.contains("has no first-class stable runtime identity")
                && normalized_contracts.contains("cannot cross the cold")
                && normalized_contracts.contains("checkpoint boundary"),
            "a Primitive must be refused rather than restoring its serde-skipped runtime proof by name"
        );
    }

    #[test]
    fn a_run_ending_outcome_never_reaches_the_epilogue() {
        // The failure-mode table's rule, as a gate. Today every run-ending
        // `return` in the loop bypasses the stuck detector and
        // `AgenticIterationCompleted`, and it does so because those two live
        // after the boundary. Moving the epilogue above the phase matches — or
        // adding a run-ending return below it — would start reporting completed
        // iterations for turns that terminated, and nothing about either edit
        // looks wrong on its own.
        let (before, after) = DRIVER_INPROC
            .split_once("    phases::epilogue::run(")
            .expect("the in-process driver must call the epilogue");
        assert_eq!(
            before.matches("return Ok(IterationStep::RunEnded(").count(),
            2,
            "the driver re-performs a run-ending step for each of the two \
             exit-bearing phases; a third or a missing one means a phase's \
             `PhaseStep::Return` is handled somewhere else or not at all"
        );
        assert!(
            !after.contains("IterationStep::RunEnded"),
            "the driver ends a run after running the epilogue; a terminated turn \
             must not emit `AgenticIterationCompleted` or feed the stuck detector"
        );

        // And the epilogue is outside the boundary, which is what makes it the
        // one phase a run-ending path can skip by falling past it.
        let boundary_close = DRIVER_INPROC
            .split_once("} // end 'iteration_body")
            .expect("the driver's labelled block must be closed")
            .1;
        assert!(
            boundary_close.contains("phases::epilogue::run("),
            "the epilogue moved inside the turn boundary; from there it would \
             run for iterations that ended the run and be skipped by none"
        );
    }
}

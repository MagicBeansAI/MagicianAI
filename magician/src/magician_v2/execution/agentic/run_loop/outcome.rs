//! Where an iteration is, and how it may leave.
//!
//! The loop's continuation is a Rust stack today, which is exactly why a restart
//! loses it. A worker resuming an execution cannot be handed a stack; it has to
//! be handed a **cursor** — which phase comes next — and that is what [`Phase`]
//! is. See `docs/archive/plans/2026-08-25-stateless-loop-design.md`.

use std::time::Duration;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::magician_v2::execution::agentic::types::AgenticOutcome;

/// The five stages of an iteration, plus its epilogue.
///
/// # This is not the shape an earlier draft assumed
///
/// The design's first cut proposed a six-stage linear pipeline
/// `Prepare → Observe → Decide → Gate → Dispatch → Settle`, inferred from a
/// ~150-line sample. Reading the iteration body end to end disproved it:
/// **`Gate`, `Dispatch` and `Settle` are not peers of the others.** They exist
/// only inside `Decision::Execute`, one arm of an eight-way match, and modelling
/// them as top-level phases would give five of the other seven arms phases they
/// never enter.
///
/// The real shape is five stages whose last branches eight ways, with the effect
/// sub-pipeline living inside exactly one of those branches. `Apply` is 59% of
/// the iteration by line count; the four stages before it are 39% together —
/// measured across the six phase bodies, not across this block, which since the
/// extraction is a ~200-line driver held under 260 by
/// `tests::the_iteration_body_is_a_driver_and_not_the_loop`.
///
/// # Why the cursor is coarse
///
/// One variant per stage, not per statement. A worker commits at phase
/// boundaries, so the cursor only has to name a point the run can be **resumed
/// from** — and every such point is a place where the design already requires
/// state to be complete. A finer cursor would imply resumability the boundary
/// contract does not provide.
///
/// # This enum is a wire format
///
/// [`super::journal`] writes a `Phase` into every record it appends — into
/// `EventKey`, `JournalRecord` and `ReplayedCursor` — so the serde spelling of
/// these variants is already on disk in journals a resume has to read back.
/// Three consequences, in decreasing obviousness:
///
/// - **Renaming a variant renames it on disk**, because `rename_all` derives the
///   wire name from the identifier. Every journal written before the rename then
///   fails to deserialize, and it surfaces as *"a resumed run cannot load"*,
///   nowhere near the edit that caused it.
/// - **Dropping `rename_all` is that same edit in disguise**: `Prepare` would go
///   out as `"Prepare"` and every existing `"prepare"` would stop parsing.
/// - **Reordering the variants is safe.** Serde writes the *name* of a unit
///   variant and never a discriminant, so declaration order reaches no reader.
///   Order is not free — [`ORDER`](Self::ORDER) and
///   [`next_in_iteration`](Self::next_in_iteration) both encode it — only the
///   journal is not what would notice.
///
/// `tests::the_cursor_names_on_disk_are_pinned` holds those names as literals
/// rather than asking `Display` or serde what they are, because either would
/// agree with itself after a rename.
///
/// Adding a variant is the change to expect. It does not compile until
/// `next_in_iteration` and that test's name table each grow an arm, which is the
/// moment to ask the question neither can answer: a journal written before the
/// new phase existed has no record for it, so replaying an old file steps
/// straight past it. That is a migration question, not a test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    // The sizes below are the `run` body in `phases/<name>.rs`, rounded,
    // re-measured 2026-08-26. They are here to say which phases dominate; the
    // figures the design estimated before the lift were up to 17% out, so read
    // them as magnitudes rather than as a count anything checks.
    //
    /// Shell stream labels, the previous iteration's step events, checkpoint
    /// hydration, reset `focused_tool`, emit `IterationStarted`. ~110 lines.
    Prepare,
    /// `observed_state` and the observation decision. ~90 lines.
    Observe,
    /// The decision LLM call. ~950 lines, and **not effect-free**. The provider
    /// call is itself a billed outward request that spends a shared execution
    /// token meter; beside it the phase dispatches `browser__screenshot` at a
    /// live browser session for Yutori vision, and writes the decision into the
    /// observation file. A phase model that assumed `Decide` was pure would be
    /// wrong about what a retry of it costs.
    Decide,
    /// Decision unwrap and retry, the work-budget check, owner and trust-policy
    /// refresh, control-decision pressure. ~470 lines.
    Resolve,
    /// The eight-way `match decision`. ~2,450 lines — the loop.
    Apply,
    /// No-action counters, the stuck warning, `IterationCompleted`. ~85 lines.
    Epilogue,
}

impl Phase {
    /// Every phase, in the order an iteration runs them.
    pub const ORDER: [Phase; 6] = [
        Phase::Prepare,
        Phase::Observe,
        Phase::Decide,
        Phase::Resolve,
        Phase::Apply,
        Phase::Epilogue,
    ];

    /// The phase an iteration starts at.
    pub const fn first() -> Self {
        Phase::Prepare
    }

    /// What runs after this one **within the same iteration**.
    ///
    /// `None` at `Epilogue`, which is deliberate and is the whole reason this is
    /// not a plain `next()`: the step from one iteration to the next is not a
    /// phase transition. It increments the iteration counter, and the counter is
    /// what the iteration ceiling is checked against. Folding the two together
    /// would let a run walk off the end of its budget one phase at a time.
    pub const fn next_in_iteration(self) -> Option<Self> {
        match self {
            Phase::Prepare => Some(Phase::Observe),
            Phase::Observe => Some(Phase::Decide),
            Phase::Decide => Some(Phase::Resolve),
            Phase::Resolve => Some(Phase::Apply),
            Phase::Apply => Some(Phase::Epilogue),
            Phase::Epilogue => None,
        }
    }

    /// Whether reaching this phase means an **outward act** may already have
    /// left the process.
    ///
    /// # What counts, stated so the predicate can be wrong
    ///
    /// An act performed outside this process that re-running the phase would
    /// perform again, and that no dedupe covers. [`super::effects`] holds the
    /// loop-side view of those, and `reconcile_outward_effect` is what a resumed
    /// worker must call instead of assuming nothing happened.
    ///
    /// It does **not** mean "mutates anything observable". Under that reading all
    /// six qualify — every phase touches in-process state, and every one but
    /// `Observe` emits transport events — and a predicate that every value
    /// satisfies decides nothing.
    ///
    /// It does not mean "expensive" either: `Decide` is the costliest phase to
    /// re-run and `Observe` is nearly free, and cost is not the axis a worker
    /// deciding whether to reconcile needs.
    ///
    /// # The two that are true
    ///
    /// `Apply` gates, dispatches and settles inside `Decision::Execute`, through
    /// `execute_direct_path_on_scheduler_root`, so a run resumed at or after
    /// `Apply` must reconcile.
    ///
    /// `Decide` counts too, and that is the trap worth naming: it *looks* like a
    /// pure "ask the model" step and is not. The provider call is a billed
    /// request against a shared token meter; on the Yutori path it dispatches
    /// `browser__screenshot` at a live browser session; and it writes the
    /// decision into the observation file. A worker that retried `Decide`
    /// believing it free would repeat all three.
    ///
    /// # Why the other four are false, having been read rather than assumed
    ///
    /// - `Prepare` and `Epilogue` emit transport events and move in-process
    ///   counters. Events are outbox entries addressed by `journal::EventKey`,
    ///   and suppressing a duplicate is the projector's job through that key —
    ///   a different mechanism from this one, which is why they are not folded
    ///   in here. The same rule covers the emissions `Decide`, `Resolve` and
    ///   `Apply` make — `Decide` and `Apply` are effectful for reasons that have
    ///   nothing to do with the events they emit.
    /// - `Observe` no longer re-captures anything. `perform_observation_with_policy`
    ///   retired per-iteration capture; every arm now passes through the state
    ///   the previous action already produced. It was the closest of the four to
    ///   being true and it is the one to re-read if that helper ever grows a
    ///   capture back.
    /// - `Resolve` reads. It reloads the owner profile and the trust policy to
    ///   close the TOCTOU window the model call opens, and its writes are
    ///   in-memory history. The one durable write it makes,
    ///   `persist_paused_execution_summary`, is followed on the same path by the
    ///   `return` that ends the run, so no resume re-runs it.
    ///
    /// `tests::the_effect_predicate_agrees_with_the_phase_bodies` checks the
    /// dispatch anchors in the phase sources rather than restating that list,
    /// because a predicate checked only against itself agrees with itself.
    pub const fn may_have_fired_an_effect(self) -> bool {
        matches!(self, Phase::Decide | Phase::Apply)
    }
}

impl Default for Phase {
    fn default() -> Self {
        Self::first()
    }
}

impl std::fmt::Display for Phase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Phase::Prepare => "prepare",
            Phase::Observe => "observe",
            Phase::Decide => "decide",
            Phase::Resolve => "resolve",
            Phase::Apply => "apply",
            Phase::Epilogue => "epilogue",
        };
        f.write_str(name)
    }
}

/// The ways control leaves `'iteration_body`.
///
/// The vocabulary §1 of the turn-boundary contract asks for
/// (`docs/archive/plans/2026-08-25-iteration-turn-boundary-contract.md`), narrowed to the
/// outcomes that actually occur. Each `// EXIT:` note inside the body names one
/// of these, and `every_way_out_of_the_iteration_body_is_classified` checks that
/// the name is a real variant — so this is a checked vocabulary, not a type
/// waiting for a use.
///
/// # Why fewer variants than the contract proposed
///
/// The contract sketches `Terminal`, `Pause`, `Park` and `PushFrame` alongside
/// these. None of them has a producer among the exits, because **a `break` never
/// ends a run** — it leaves the block, runs the epilogue, and starts the next
/// iteration. Terminating, pausing and parking are `return Ok(..)`, and they sit
/// right beside breaks that mean something else entirely:
///
/// ```ignore
/// if handle_terminal_evidence_rejection(..) {
///     return Ok(AgenticOutcome::Failed { .. });   // the run ends here
/// }
/// break 'iteration_body;                          // rejection recorded, re-decide
/// ```
///
/// `PushFrame` has no producer either: spawning a sub-goal pushes a continuation
/// frame onto the CHILD's context, not this run's, so the exit that follows is an
/// ordinary `NextIteration`.
///
/// Declaring the four anyway would repeat a mistake this codebase already made
/// once — `LlmToolSideEffectState::Remained` sat in an enum with no producer
/// until it was found, and everything downstream had quietly assumed it could
/// never occur. A variant earns its place by being reachable. When a consumer
/// makes these exits return values instead of breaking, the ones it genuinely
/// needs can be added against real call sites.
///
/// # Every variant now has a producer
///
/// When this type was declared, nothing constructed it: the loop still left by
/// `break`, and the enum existed as the vocabulary those breaks were classified
/// against. Lifting `Resolve` and `Apply` out of the body turned all twenty of
/// those breaks into values of this type, so the `allow(dead_code)` that stood
/// here is gone and the variants are load-bearing at runtime rather than only in
/// a source scan.
///
/// # It is not a wire format, unlike [`Phase`]
///
/// This enum derives no serde, and that is load-bearing rather than an omission.
/// `journal::RecordedBoundary` mirrors it for the log, which is what lets
/// [`Retry`](Self::Retry) keep a `Duration` here — a `Duration` serializes as
/// `{secs, nanos}`, which is not a shape a log line should carry, so the record
/// carries `after_ms: u64` instead. The conversion is exhaustive both ways, so a
/// variant added here is a build error there rather than an approximation on
/// disk. Deriving `Serialize` on this type would create a second on-disk
/// spelling of the same vocabulary, and the two would drift.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryOutcome {
    /// Stay in this run and decide again. The iteration produced no usable
    /// action — a rejection, a policy change, a parse failure under its limit.
    Advance,
    /// The iteration is complete; take the next one.
    NextIteration,
    /// A transient failure asked for a delay first. The resident driver sleeps;
    /// a holder that does not own the executor should requeue instead.
    ///
    /// The wait travels **with** the outcome rather than through a
    /// `&mut Option<Duration>` the phase writes on its way out. That side
    /// channel is the ambient-state pattern this whole refactor exists to
    /// remove: a phase setting it and then returning `Exit` states its intent
    /// twice, and a caller that honours only the second half drops the backoff
    /// silently — which is exactly how a failing provider gets hammered.
    Retry(Duration),
    /// Control returned to the owner that delegated this stretch.
    PopFrame,
}

impl BoundaryOutcome {
    /// The spelling used in `// EXIT:` notes, so the gate compares like for like.
    pub const NAMES: [&'static str; 4] = ["Advance", "NextIteration", "Retry", "PopFrame"];
}

/// What a lifted phase returns in place of the control flow it used to perform.
///
/// `break 'iteration_body` and `return` do not cross a function boundary. That
/// single fact is why `Prepare`, `Observe` and `Decide` could be lifted as plain
/// async fns — they contain no exits — and why `Resolve` and `Apply` could not
/// until their exits became values. This is that value.
///
/// # Three variants, because two of them are constantly confused
///
/// [`Exit`](Self::Exit) and [`Return`](Self::Return) are not degrees of the same
/// thing. **A `break` never ends a run.** It leaves the block, the epilogue
/// runs, and the next iteration starts. Terminating, pausing and parking are
/// `return Ok(..)`. In the original body the two sat line-adjacent:
///
/// ```ignore
/// if handle_terminal_evidence_rejection(..) {
///     return Ok(AgenticOutcome::Failed { .. });   // the run ends here
/// }
/// break 'iteration_body;                          // rejection recorded, re-decide
/// ```
///
/// Mapping that `break` to `Return` would end a run that should have taken
/// another turn; mapping that `return` to `Exit` would keep a failed run alive.
/// Neither shows up as a compile error, which is why every exit was classified
/// at its own site rather than from the design's summary table — the table had
/// this backwards.
///
/// # Why `Return` boxes
///
/// The same reason `ExecutePathControl::Return` does, and its comment is the
/// evidence: embedding an `AgenticOutcome` inline "made each debug poll frame
/// reserve the full outcome layout, exhausting ordinary Tokio and test worker
/// stacks." These phases are polled from inside that same future.
pub enum PhaseStep<T> {
    /// The phase produced its output and the iteration continues.
    Continue(T),
    /// The phase ended the *iteration*. The caller re-performs the exit —
    /// today by breaking the labelled block, tomorrow by committing a cursor.
    Exit(BoundaryOutcome),
    /// The phase ended the *run*. The caller returns this outcome verbatim.
    Return(Box<AgenticOutcome>),
}

impl<T> PhaseStep<T> {
    /// The run ends with `outcome`.
    ///
    /// A constructor rather than a plain variant so a converted exit reads as
    /// one edit to the head of the statement — `return Ok(x)` becomes
    /// `return PhaseStep::ends_run(x)` — leaving the outcome expression, which
    /// is frequently forty lines of struct literal, untouched. A rewrite that
    /// had to wrap both ends of those expressions is a rewrite with somewhere to
    /// go wrong.
    pub fn ends_run(outcome: AgenticOutcome) -> Result<Self> {
        Ok(PhaseStep::Return(Box::new(outcome)))
    }

    /// The iteration ends with `boundary`; the run continues.
    pub fn exits(boundary: BoundaryOutcome) -> Result<Self> {
        Ok(PhaseStep::Exit(boundary))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::types::EnvironmentState;

    /// Every place an iteration can decide to leave early, wherever it now lives.
    ///
    /// Twenty when this enumeration was first taken against the 4,042-line
    /// `'iteration_body`. The repeat-failed-action gate adds an Advance exit.
    /// Lifting `Resolve` and `Apply` moved
    /// **all** of them out of the body: five into `phases/resolve.rs` and
    /// fifteen into `phases/apply.rs`, where a `break` nobody could type-check
    /// became a `BoundaryOutcome` the compiler does.
    ///
    /// The count is pinned rather than derived because a *dropped* exit is the
    /// failure this guards: a run that should have taken another turn instead
    /// falls through, and nothing about that looks wrong at the call site.
    ///
    /// [`PHASE_SOURCES`] pins the same population per file, and the gate asserts
    /// the two agree. That redundancy is the point: the per-file numbers say
    /// which phase moved, and this one says the total did.
    const ITERATION_EARLY_EXITS: usize = 21;

    /// The `break`s still inside the body, which are not exits of their own.
    ///
    /// They are the resident driver re-performing an exit a phase already
    /// decided — four per exit-bearing phase call, one per `BoundaryOutcome`
    /// variant, so the driver states in code what it does with each. A worker
    /// driver replaces exactly these eight lines and nothing else.
    const DRIVER_REPERFORMED_EXITS: usize = 8;

    // The driver moved to `driver_inproc.rs` on 2026-08-27, so the gates that
    // scan it moved with it. They are gates on the DRIVER, not on `executor.rs`
    // — pointing them at the file the driver used to live in would leave them
    // passing while checking nothing.
    const DRIVER_INPROC: &str = include_str!("driver_inproc.rs");
    const RESOLVE: &str = include_str!("phases/resolve.rs");
    const APPLY: &str = include_str!("phases/apply.rs");
    const PREPARE: &str = include_str!("phases/prepare.rs");
    const OBSERVE: &str = include_str!("phases/observe.rs");
    const DECIDE: &str = include_str!("phases/decide.rs");
    const EPILOGUE: &str = include_str!("phases/epilogue.rs");

    fn iteration_body() -> &'static str {
        DRIVER_INPROC
            .split_once("'iteration_body: {")
            .expect("the labeled iteration body must exist")
            .1
            .split_once("} // end 'iteration_body")
            .expect("the labeled iteration body must be closed")
            .0
    }

    /// The `// EXIT:` note attached to the statement on `line`, if any.
    ///
    /// Read from the nearest preceding non-blank line rather than by counting
    /// `// EXIT:` occurrences in the file, so an annotation that drifts away
    /// from the statement it describes fails instead of still tallying.
    fn exit_note_above(lines: &[&str], index: usize) -> Option<String> {
        let preceding = lines[..index]
            .iter()
            .rev()
            .find(|candidate| !candidate.trim().is_empty())?
            .trim();
        let note = preceding.strip_prefix("// EXIT:")?.trim();
        Some(
            note.split(|c: char| !c.is_alphanumeric())
                .next()
                .unwrap_or("")
                .to_string(),
        )
    }

    /// Every way a phase or the body could take a wait inline.
    ///
    /// A gate that scans for one spelling asserts a string, not a property. See
    /// `no_phase_and_no_iteration_blocks_on_a_backoff_of_its_own`.
    const SLEEP_SPELLINGS: [&str; 4] = [
        "tokio::time::sleep",
        "tokio::time::sleep_until",
        // Reachable via `use tokio::time::sleep;` — the import makes the crate
        // path disappear from the call site entirely.
        "sleep(",
        // Blocks the worker thread rather than yielding it.
        "thread::sleep",
    ];

    /// Both ways an exit is spelled today.
    const EXIT_SPELLINGS: [&str; 2] = [
        "PhaseStep::exits(BoundaryOutcome::",
        "PhaseStep::Exit(BoundaryOutcome::",
    ];

    /// Every phase file: its name, its source, the cursor it implements, and
    /// how many boundary exits it holds.
    ///
    /// One table because three gates below need the same six files, and three
    /// hand-kept lists of the same six are three chances for a seventh phase to
    /// be added to two of them.
    ///
    /// The exit counts are per file rather than only in total, and that is the
    /// hole this closed: a scan that looked only at `resolve.rs` and `apply.rs`
    /// — where all twenty live today — would count a future `PhaseStep` exit on
    /// `Observe` as zero and say nothing. The four zeroes here are assertions,
    /// not omissions. A drift now also names the phase that moved rather than
    /// only the total.
    const PHASE_SOURCES: [(&str, &str, Phase, usize); 6] = [
        ("prepare.rs", PREPARE, Phase::Prepare, 0),
        ("observe.rs", OBSERVE, Phase::Observe, 0),
        ("decide.rs", DECIDE, Phase::Decide, 0),
        ("resolve.rs", RESOLVE, Phase::Resolve, 5),
        ("apply.rs", APPLY, Phase::Apply, 16),
        ("epilogue.rs", EPILOGUE, Phase::Epilogue, 0),
    ];

    /// Where a phase reaches outside the process.
    ///
    /// Two anchors because the two effect-bearing phases reach the world by
    /// different routes: `Decide` calls the flat dispatcher directly for its
    /// Yutori screenshot, and `Apply` goes through the direct execute path.
    const OUTWARD_DISPATCH: [&str; 2] = [
        "dispatch_flat_action(",
        "execute_direct_path_on_scheduler_root(",
    ];

    #[test]
    fn every_way_out_of_the_iteration_body_is_classified() {
        // The exhaustiveness gate the stateless design asks for
        // (`docs/archive/plans/2026-08-25-stateless-loop-design.md`), which until the
        // phase extraction existed only as a table in prose. Its own warning:
        // "a dropped exit means a run that should have terminated keeps going",
        // and "if that test is wrong, dropped exits are silent".
        //
        // What changed when the exits moved. In the body an exit was a `break`
        // with a comment, and a comment is all the classification there was —
        // so this test checked that every `break` carried one naming a real
        // variant. In a phase an exit is a `BoundaryOutcome` value, so the
        // *classification* is now the compiler's job and this test's job is
        // narrower and sharper: that the note and the constructed variant agree,
        // and that the population has not silently changed size.
        let body = iteration_body();
        let lines: Vec<&str> = body.lines().collect();

        let mut body_variants = Vec::new();
        for (index, line) in lines.iter().enumerate() {
            let trimmed = line.trim();
            if !(trimmed.starts_with("break 'iteration_body")
                || trimmed.ends_with("=> break 'iteration_body,"))
            {
                continue;
            }
            let named = exit_note_above(&lines, index).unwrap_or_else(|| {
                panic!(
                    "an exit from the iteration body must say what it does; the \
                     one before `{trimmed}` does not carry an `// EXIT:` note"
                )
            });
            assert!(
                BoundaryOutcome::NAMES.contains(&named.as_str()),
                "`// EXIT: {named}` is not a `BoundaryOutcome`; the exit before \
                 `{trimmed}` must name one of {:?}",
                BoundaryOutcome::NAMES
            );
            body_variants.push(named);
        }
        assert_eq!(
            body_variants.len(),
            DRIVER_REPERFORMED_EXITS,
            "the driver's re-performance of a phase exit changed shape; it \
             should be one annotated `break` per `BoundaryOutcome` variant per \
             exit-bearing phase call"
        );
        for variant in BoundaryOutcome::NAMES {
            assert!(
                body_variants.iter().any(|named| named == variant),
                "the driver does not say what it does with \
                 `BoundaryOutcome::{variant}`; a boundary contract with an \
                 unhandled outcome is not implemented"
            );
        }

        // And the exits themselves, now typed, in the phases that decide them.
        let mut phase_variants = Vec::new();
        for (file, source, _, pinned) in PHASE_SOURCES {
            let lines: Vec<&str> = source.lines().collect();
            let mut found_here = 0usize;
            for (index, line) in lines.iter().enumerate() {
                // Both spellings. `PhaseStep::exits(BoundaryOutcome::` was the
                // only one this scan knew, and the sibling form
                // `PhaseStep::Exit(BoundaryOutcome::` is already in active use in
                // these same files for `PhaseStep::Return`. An exit written the
                // other way would have been invisible here.
                let Some(rest) = EXIT_SPELLINGS
                    .iter()
                    .find_map(|spelling| line.split_once(spelling))
                else {
                    // A third spelling would be invisible above, and invisible
                    // is not silent only by luck: the total would fall below
                    // `ITERATION_EARLY_EXITS` and fire an assertion pointing at
                    // a number rather than at the exit. So no mention of the
                    // enum outside the known forms is allowed to pass. Comment
                    // lines are exempt, because doc links legitimately name
                    // variants — `apply.rs` links `PopFrame` in its module docs.
                    let trimmed = line.trim();
                    assert!(
                        !trimmed.contains("BoundaryOutcome::") || trimmed.starts_with("//"),
                        "{file}:{} builds a `BoundaryOutcome` in a spelling this \
                         gate does not know: `{trimmed}`. Write the exit the way \
                         its neighbours are written, or add the form to \
                         EXIT_SPELLINGS — an exit this scan cannot see is an \
                         exit nothing classifies",
                        index + 1
                    );
                    continue;
                };
                let constructed: String =
                    rest.1.chars().take_while(|c| c.is_alphanumeric()).collect();
                let named = exit_note_above(&lines, index).unwrap_or_else(|| {
                    panic!(
                        "{file}:{} constructs `BoundaryOutcome::{constructed}` \
                         without an `// EXIT:` note; the note is what survived \
                         the move out of the body and it must move with it",
                        index + 1
                    )
                });
                assert_eq!(
                    named,
                    constructed,
                    "{file}:{} says `// EXIT: {named}` and constructs \
                     `BoundaryOutcome::{constructed}`; an annotation that \
                     disagrees with the value is worse than none",
                    index + 1
                );
                found_here += 1;
                phase_variants.push(constructed);
            }
            assert_eq!(
                found_here, pinned,
                "{file} holds {found_here} boundary exits where this gate pins \
                 {pinned}; classify the new exit with an `// EXIT:` note and \
                 move the count, or drop the stale one — an unclassified exit \
                 is how a run that should have ended keeps going"
            );
        }
        assert_eq!(
            phase_variants.len(),
            ITERATION_EARLY_EXITS,
            "the per-phase exit counts no longer sum to ITERATION_EARLY_EXITS; \
             the headline number and the table that backs it must move together"
        );
        for variant in BoundaryOutcome::NAMES {
            assert!(
                phase_variants.iter().any(|named| named == variant),
                "`BoundaryOutcome::{variant}` has no exit that produces it — \
                 either an exit was reclassified or the variant should not exist"
            );
        }

        // The twenty-first exit — which, it turns out, does not exist. The
        // design describes it as the fall-through of the body's final `match`:
        // an exit with no statement, so nothing above can see it. Turning
        // `Apply` into a function made rustc check that, and it answered
        // `unreachable_expression`: all eight arms of `match decision` diverge,
        // so control never reaches the end of the match. The body's last
        // construct is still a `match` and its `PhaseStep::Continue` arm still
        // falls through — that arm is simply not reachable at runtime today.
        // The structural assertion is kept because it is what would notice the
        // day the body stops ending on a match.
        let tail = body.trim_end();
        assert!(
            tail.ends_with('}'),
            "the iteration body must still end on its match, whose fall-through \
             is the one exit that is not a `break`"
        );
        // What replaced a check that could not fail. The assertion here used to
        // be `!tail.ends_with("break 'iteration_body;")`, which the line above
        // had already made impossible — a string ending in `}` cannot end in
        // `;` — so it never could have fired while reading as coverage.
        //
        // The property it was reaching for is checkable: after the last exit the
        // driver re-performs there is only the match's own punctuation. A
        // statement appended there would be an exit with no `// EXIT:` note and
        // no phase, which is precisely how the block grew the first time.
        let after_last_break = body
            .rsplit_once("break 'iteration_body;")
            .expect("the driver must still re-perform at least one exit")
            .1;
        let residue: String = after_last_break
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with("//"))
            .collect();
        assert!(
            residue.chars().all(|c| matches!(c, '}' | ',')),
            "the iteration body runs `{residue}` after its last exit; the body \
             is a driver, and new work belongs in the phase it belongs to"
        );
    }

    #[test]
    fn no_phase_and_no_iteration_blocks_on_a_backoff_of_its_own() {
        // The turn-boundary contract's `Retry` demand, as a gate rather than a
        // convention. Two transient-failure exits used to `sleep(backoff).await`
        // inline and then break; that is correct for a loop owning its executor
        // and wrong for both consumers of the contract, because an in-place
        // sleep holds a worker — and holds a foreign harness session — for the
        // whole backoff.
        //
        // Widened when the exits moved: the rule was "not inside the body", and
        // the body is under 200 lines, so a sleep would simply relocate into a
        // phase and satisfy the old assertion. The rule that survives the move
        // is "not inside the iteration at all".
        //
        // A source scan, because the property is structural: it is about where a
        // wait may appear, and no runtime assertion can observe "this delay was
        // taken inside a phase rather than at the boundary". Anchored on the
        // block's own markers rather than line numbers, since the file is under
        // constant edit. Same idiom as the stream-reader contracts in
        // `chat/service.rs` and `artifact_v2/service.rs`.
        // Every spelling, not one. `tokio::time::sleep` was the only form this
        // scan knew, so `use tokio::time::sleep; sleep(d).await`, `sleep_until`
        // and `std::thread::sleep` all walked straight past a gate named for the
        // property rather than the string. None of them is in the tree today —
        // which is exactly when a scan like this rots unnoticed.
        //
        // `thread::sleep` is the worst of the three and the easiest to reach for:
        // it blocks the executor thread itself rather than yielding, so it would
        // stall every other task on that worker, not merely this run.
        for spelling in SLEEP_SPELLINGS {
            assert!(
                !iteration_body().contains(spelling),
                "the iteration body contains `{spelling}`; an iteration must ask \
                 for its wait at the boundary, not take it inline"
            );
        }
        for (file, source, _, _) in PHASE_SOURCES {
            for spelling in SLEEP_SPELLINGS {
                assert!(
                    !source.contains(spelling),
                    "{file} contains `{spelling}`; the wait belongs to whoever \
                     holds the executor, which is why `BoundaryOutcome::Retry` \
                     carries it rather than a phase taking it"
                );
            }
        }

        // The exits still ask. Deleting the request rather than the sleep would
        // satisfy everything above while removing the backoff entirely.
        assert_eq!(
            RESOLVE.matches("BoundaryOutcome::Retry(backoff)").count()
                + APPLY.matches("BoundaryOutcome::Retry(backoff)").count(),
            2,
            "both transient-failure exits must still request their backoff"
        );

        // And the driver honours it, immediately after the boundary, so the
        // order the inline sleeps produced is preserved. Without this half the
        // assertions above are satisfied by an iteration that requests a wait
        // nobody takes.
        assert!(
            iteration_body().contains("boundary_retry_after = Some(after);"),
            "the driver must record the wait a phase asked for"
        );
        let after_body = DRIVER_INPROC
            .split_once("} // end 'iteration_body")
            .expect("the labeled iteration body must be closed")
            .1;
        let honoured = after_body
            .split_once("// PHASE: Epilogue")
            .expect("the epilogue must follow the boundary")
            .0;
        assert!(
            honoured.contains("boundary_retry_after.take()")
                && honoured.contains("tokio::time::sleep"),
            "the resident driver must honour the requested wait immediately \
             after the boundary, before the epilogue reads the iteration's \
             duration"
        );
    }

    #[test]
    fn the_iteration_body_is_a_driver_and_not_the_loop() {
        // The property Stage 3 was for, as a gate. `'iteration_body` was 4,042
        // lines holding the decision call, the eight-way apply and every exit;
        // it is now a sequence of six phase calls plus the re-performance of
        // whatever they decide. Nothing stops code drifting back in one `if` at
        // a time, and each addition looks reasonable on its own — which is how
        // the block reached 4,042 lines the first time.
        let body = iteration_body();
        let lines = body.lines().count();
        assert!(
            lines < 260,
            "the iteration body is {lines} lines; it drives six phases and \
             should stay close to that. New work belongs in the phase it \
             belongs to, not in the block that sequences them"
        );

        for phase in [
            "prepare::run(",
            "observe::run(",
            "decide::run(",
            "resolve::run(",
        ] {
            assert!(
                body.contains(phase),
                "the iteration body no longer calls `{phase}`; a phase that is \
                 not invoked is a phase that does not run"
            );
        }
        // `Apply` is TWO calls, and both are checked. The phase split its gate
        // from its dispatch so a driver could commit each effect's intent in
        // between — see `phases::apply::ApplyGate` — and that makes the property
        // this loop is asserting a two-part one:
        //
        // - a body that calls neither runs no effects at all;
        // - a body that gates and never dispatches **also** runs no effects,
        //   silently, having decided everything and fired none of it.
        //
        // The second is the new way to break it and the reason this is not
        // folded into the loop above as `"apply::"`. That spelling passes on a
        // body holding only the gate, which is exactly the shape a bad merge
        // leaves behind.
        for call in ["apply::gate(", "apply::dispatch("] {
            assert!(
                body.contains(call),
                "the iteration body no longer calls `{call}`; `Apply` is a gate and a dispatch \
                 and a driver owes both. A body that gates without dispatching decides an \
                 entire turn and fires none of it"
            );
        }
        // The epilogue is the one phase outside the boundary — that is what it
        // means — so it is checked against the code after the block.
        let after_body = DRIVER_INPROC
            .split_once("} // end 'iteration_body")
            .expect("the labeled iteration body must be closed")
            .1;
        assert!(
            after_body.contains("epilogue::run("),
            "the epilogue must still run after the boundary, for every \
             iteration that ended by leaving the block"
        );
    }

    #[test]
    fn ending_the_run_and_ending_the_iteration_are_not_the_same_step() {
        // The confusion `PhaseStep` exists to make impossible, checked against
        // real values rather than against the type's shape. The turn-boundary
        // contract's own table mapped several `break`s to terminal outcomes; a
        // phase built from that table would end runs that should have taken
        // another turn, and nothing would fail to compile.
        let ended = PhaseStep::<()>::ends_run(AgenticOutcome::Failed {
            reason: "provider gave up".to_string(),
            last_state: EnvironmentState::Uninitialized,
            iterations_used: 7,
        })
        .expect("constructing a step is infallible");
        match ended {
            PhaseStep::Return(outcome) => match *outcome {
                AgenticOutcome::Failed {
                    reason,
                    iterations_used,
                    ..
                } => {
                    assert_eq!(reason, "provider gave up");
                    assert_eq!(iterations_used, 7);
                },
                other => panic!("the outcome was rebuilt, not carried: {other:?}"),
            },
            PhaseStep::Exit(_) => panic!("a run that ended became an iteration that ended"),
            PhaseStep::Continue(()) => panic!("a run that ended became a continue"),
        }

        // And the wait rides along with the outcome rather than being written
        // through a side channel — the half a caller can forget.
        let waited = PhaseStep::<()>::exits(BoundaryOutcome::Retry(Duration::from_millis(1500)))
            .expect("constructing a step is infallible");
        match waited {
            PhaseStep::Exit(BoundaryOutcome::Retry(after)) => {
                assert_eq!(after, Duration::from_millis(1500));
            },
            PhaseStep::Exit(other) => panic!("the backoff was reclassified as {other:?}"),
            PhaseStep::Return(_) => panic!("an iteration that ended became a run that ended"),
            PhaseStep::Continue(()) => panic!("an iteration that ended became a continue"),
        }
    }

    #[test]
    fn the_phases_form_one_chain_that_ends_at_the_epilogue() {
        // Walk it rather than assert the table back at itself: a chain with a
        // gap, a cycle, or a second terminal would still satisfy a per-arm
        // assertion while being unwalkable by a driver.
        let mut walked = vec![Phase::first()];
        let mut cursor = Phase::first();
        while let Some(next) = cursor.next_in_iteration() {
            assert!(
                !walked.contains(&next),
                "phase {next} repeats; the chain must not cycle"
            );
            walked.push(next);
            cursor = next;
            assert!(
                walked.len() <= Phase::ORDER.len(),
                "the chain outran the phase list"
            );
        }

        assert_eq!(
            walked,
            Phase::ORDER.to_vec(),
            "walking from the first phase must visit every phase in order"
        );
        assert_eq!(
            cursor,
            Phase::Epilogue,
            "the chain must end at the epilogue, not somewhere in the middle"
        );

        // And the source files the gates scan are those same phases, in that
        // same order. Without this `PHASE_SOURCES` is a second, unchecked
        // opinion about what the phases are: a variant added to `Phase` with no
        // file beside it would be scanned by nothing, and a file added with no
        // variant would be scanned as though it were a phase when it is not.
        assert_eq!(
            PHASE_SOURCES.map(|(_, _, phase, _)| phase).to_vec(),
            Phase::ORDER.to_vec(),
            "the phase files the source gates scan no longer match `ORDER`"
        );
    }

    #[test]
    fn the_iteration_step_is_not_a_phase_transition() {
        // REGRESSION GUARD. The temptation is to make `Epilogue.next()` return
        // `Prepare` so a driver can loop without a special case. That is wrong:
        // the step between iterations increments the counter the iteration
        // ceiling is checked against, so a phase transition that silently
        // performed it would let a run walk past `max_iterations` one phase at a
        // time, with nothing to notice.
        assert_eq!(
            Phase::Epilogue.next_in_iteration(),
            None,
            "the epilogue must not chain back to prepare"
        );
    }

    #[test]
    fn the_effect_predicate_agrees_with_the_phase_bodies() {
        // Not a restatement of `matches!(self, Decide | Apply)`. A test that
        // asserted the predicate's own arms back at it would pass for any
        // predicate anyone wrote, including a wrong one. The claim worth
        // checking is about the phase sources: exactly the two phases called
        // effectful are the two that reach outside the process.
        //
        // The failure is one-directional and expensive. A false negative — a
        // phase that dispatches and is called effect-free — makes a resumed
        // worker skip reconciliation and re-fire a live effect. A false positive
        // costs an unnecessary reconcile.
        for (file, source, phase, _) in PHASE_SOURCES {
            let dispatches = source.lines().any(|line| {
                !line.trim_start().starts_with("//")
                    && OUTWARD_DISPATCH
                        .into_iter()
                        .any(|anchor| line.contains(anchor))
            });
            let claimed = phase.may_have_fired_an_effect();
            assert_eq!(
                dispatches,
                claimed,
                "{file} {} an outward dispatch while `may_have_fired_an_effect` \
                 says {claimed}. Either the phase changed what it does, or an \
                 anchor in OUTWARD_DISPATCH was renamed and this scan has gone \
                 blind — check which before moving either",
                if dispatches {
                    "contains"
                } else {
                    "contains no"
                }
            );
        }

        // And the trap by name. The scan above is also satisfied by a `Decide`
        // that had quietly stopped dispatching and been reclassified with it;
        // this says which way that argument has to go.
        assert!(
            Phase::Decide.may_have_fired_an_effect(),
            "Decide is NOT effect-free: the provider call is billed against a \
             shared token meter, the Yutori path dispatches a screenshot at a \
             live browser session, and the decision is written into the \
             observation file"
        );
        assert!(Phase::Apply.may_have_fired_an_effect());
    }

    /// The name each phase carries **on disk**.
    ///
    /// Exhaustive on purpose: a variant added to `Phase` has no arm here and
    /// this file stops compiling until someone writes one, which is the moment
    /// to ask whether journals written before it can still be replayed.
    ///
    /// It is a third list rather than a read of `Display` or of serde, because
    /// those two are what is being checked. The test this feeds used to compare
    /// them only to each other, and two implementations of one rename agree with
    /// each other perfectly while every journal on disk stops loading.
    fn wire_name(phase: Phase) -> &'static str {
        match phase {
            Phase::Prepare => "prepare",
            Phase::Observe => "observe",
            Phase::Decide => "decide",
            Phase::Resolve => "resolve",
            Phase::Apply => "apply",
            Phase::Epilogue => "epilogue",
        }
    }

    #[test]
    fn the_cursor_names_on_disk_are_pinned() {
        // A worker is handed this; it cannot inherit it. It is handed it by a
        // journal file, possibly written by an earlier build — see the note on
        // `Phase` for why that makes these names a format rather than a detail.
        for phase in Phase::ORDER {
            let expected = wire_name(phase);
            let encoded = serde_json::to_string(&phase).expect("serialize");
            assert_eq!(
                encoded,
                format!("\"{expected}\""),
                "the on-disk name of {phase:?} moved; every journal written \
                 before this edit now fails to load, and it will surface far \
                 from here as a resumed run that cannot start"
            );
            let restored: Phase =
                serde_json::from_str(&encoded).expect("a pinned name must read back");
            assert_eq!(restored, phase);
            assert_eq!(
                phase.to_string(),
                expected,
                "`Display` and the wire name must stay one vocabulary. \
                 `EventKey` and `JournalError` render a phase through `Display` \
                 into the address an operator then greps for in the journal \
                 file, where it is spelled by serde — give one phase two \
                 spellings and the two stop matching"
            );
        }

        // `ORDER` is what every gate in this file enumerates, so a variant
        // missing from it is a variant nothing above pins. Checked against a
        // literal for the same reason the names are.
        assert_eq!(
            Phase::ORDER
                .into_iter()
                .map(wire_name)
                .collect::<Vec<_>>()
                .join(","),
            "prepare,observe,decide,resolve,apply,epilogue",
            "the phase list changed shape; a phase added to the enum but not to \
             `ORDER` is enumerated by nothing, and one added to `ORDER` but not \
             handled by `journal::replay_each` resumes at the wrong place"
        );

        // An unrecognised name is refused rather than absorbed. A journal from a
        // build with one more phase than this one must fail at the line it
        // cannot read, not deserialize into a neighbouring phase and resume an
        // iteration somewhere it never was.
        assert!(
            serde_json::from_str::<Phase>("\"verify\"").is_err(),
            "an unknown phase name deserialized; a `#[serde(other)]` fallback on \
             a resume cursor turns a version skew into a silent misresume"
        );
    }

    #[test]
    fn a_run_with_no_cursor_starts_at_the_beginning() {
        assert_eq!(Phase::default(), Phase::Prepare);
    }
}

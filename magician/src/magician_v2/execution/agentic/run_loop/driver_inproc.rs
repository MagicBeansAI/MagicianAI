//! The in-process driver — the control arm of the strangler.
//!
//! `MAGICIAN_EXECUTION_DRIVER=inprocess` is the explicit rollback arm; unset,
//! blank, and unrecognised values select the stateless driver. When selected,
//! this arm routes `execute_agentically_inner` through [`run_iteration`]. This
//! is the same sequencer that was written inline
//! in `executor.rs` until 2026-08-27, moved without a behaviour change: same
//! phase order, same handling of all three [`PhaseStep`] variants, same four
//! [`BoundaryOutcome`] arms, the backoff still taken after the boundary, and
//! `Epilogue` still skipped by every path that ends the run.
//!
//! # Why it has to stay a trivial sequencer
//!
//! The flip gate in `docs/archive/plans/2026-08-25-stateless-loop-design.md` compares
//! this arm against `driver_worker`'s phase for phase. That comparison is only
//! evidence because both drivers call the *same six* [`super::phases`] entry
//! points and add nothing of their own. A decision taken here — a retry this
//! driver chose, a phase it skipped, a counter it moved — is a decision the
//! worker does not take, and the differential would report the gap as a worker
//! bug. So nothing in this file may be anything other than the re-performance of
//! something a phase returned.
//!
//! # What it does with what a phase returns
//!
//! | [`PhaseStep`] | What this driver does |
//! |---|---|
//! | `Continue(T)` | takes `T` and runs the next phase |
//! | `Return(outcome)` | hands back [`IterationStep::RunEnded`]; the caller returns it verbatim |
//! | `Exit(Advance)`, `Exit(NextIteration)`, `Exit(PopFrame)` | leaves the labelled block; the epilogue runs; the loop takes the next iteration |
//! | `Exit(Retry(after))` | records `after`, leaves the block, and sleeps **after** the boundary rather than inside the phase |
//!
//! `Continue` is unreachable from `Apply` today — every arm of that phase's
//! eight-way `match decision` diverges and rustc is what proves it — and the arm
//! is written out anyway, because the contract is per-phase: `Resolve` produces
//! `Continue`, and a driver that cannot handle it from one phase has not
//! implemented the contract.
//!
//! # What it deliberately does not do
//!
//! It never touches [`super::store::LoopStateStore`], [`super::journal`] or
//! [`super::effects`]. Every piece of this run's state arrives as a borrow of a
//! local owned by `execute_agentically_inner` — which is exactly why a restart
//! loses the run, and exactly what the stateless arm replaces with a load and a
//! commit. A commit added here would stop this arm being the control: the thing
//! the flip gate is measuring is what durability changes, and an arm that is
//! already durable answers nothing.
//!
//! # WHY THIS ARM HAS NO OUTBOX DRAIN, and why giving it one would be a lie
//!
//! `super::phases::outbox` is the event outbox: a phase appends the transport
//! event it wants and a projector emits it, so a phase that re-runs after a
//! crash does not emit twice. Its records reach a commit through exactly one
//! channel — `driver_worker::PhaseReport::records`, drained by
//! `executor.rs::InProcessWorkerHost::run_phase`. **This driver assembles no
//! `PhaseReport`, and it should not grow one.**
//!
//! It was asked whether this file owes a drain. It does not. The outbox producer
//! observes that this arm has no drain and emits each refused record inline;
//! accepted records exist only on the stateless arm and are projector-owned.
//! This makes the delivery paths mutually exclusive without adding durable
//! machinery to the resident control arm. The reason this file still must not
//! grow a drain is the same one the section above gives for not committing here:
//!
//! - **There is nothing for a drain to hand the records to.** A drain is
//!   `take(execution_id)` followed by *put them somewhere*, and the somewhere is
//!   the journal — which this arm never touches, by design. A drain here would
//!   take the records and drop them.
//! - **Deduplication is the outbox's whole purpose and this arm cannot need
//!   it.** Its continuation is the Rust stack. It cannot crash and resume, so no
//!   phase ever re-runs, so there is no second emission to suppress. Records
//!   produced here would answer a question this arm does not ask.
//! - **It would stop this arm being the control.** The flip gate compares the
//!   two drivers phase for phase, and that comparison is evidence only while
//!   this file re-performs what a phase returned and adds nothing of its own.
//!
//! So `outbox::journalling_has_a_drain` refusing to buffer on this arm, followed
//! by the producer's inline fallback, is not a stopgap waiting on this file. For
//! as long as this arm exists it is the correct shape. Per-event records a
//! flip-gate differential might want to compare still do not exist on this arm,
//! which bounds what such a comparison can say; transport delivery does exist.
//!
//! ## The check that must NOT be run against this file — IT NOW PASSES, FALSELY
//!
//! `outbox::journal_and_emit`'s *THE LAST SWITCH* states its first condition's
//! check as *"`driver_inproc.rs` mentions `outbox`, or that arm is gone"*.
//! **`grep -c outbox` over this file answered zero before the commit that added
//! this section and answers a handful after it, and nothing about the arm
//! changed in between.** Every one of those matches is prose — no number is
//! quoted here, because a count in a comment rots the next time somebody
//! rewords a paragraph.
//!
//! A grep for a word cannot tell a real drain from a discarding one, and a
//! discarding one is the only kind this file could hold — so the check as
//! spelled can now be passed by an edit that deletes
//! `executors.emit_event(event)` and leaves chat activity, the deep-work panel
//! and `/debug` timelines blank for every run on the DEFAULT arm. Nine events,
//! no other producer.
//!
//! The wording in `phases/outbox.rs` is the one that has to change; this
//! section cannot reach it, and deleting the section to make the grep red again
//! would trade a misleading check for no explanation at all.
//!
//! The condition that says what is meant is: **no arm of
//! [`super::ExecutionDriver`] answers `false` from `outbox::arm_has_a_drain`**.
//! `Inprocess` answers `false`, correctly, so the condition is satisfiable in one
//! way only — this arm goes away. See `executor.rs::journal_and_emit_at`.
//!
//! # The one per-iteration step that is not in here
//!
//! `refresh_active_procedure_skill_from_tier` still runs in
//! `execute_agentically_inner`, immediately before the driver call and *outside*
//! the flag's match, so both arms get it. It is not a phase, and handing it to
//! one driver and not the other would have been a difference between them by
//! construction.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use tokio_util::sync::CancellationToken;

use crate::magician_v2::execution::agentic::executor::{
    ActionExecutors, AgenticToolLineageState, HeapAwaitExt, TrustDispatchGuard,
};
use crate::magician_v2::execution::agentic::run_loop::effects::{
    ApplyIntents, EffectId, EffectOutcome, EffectPlan, PendingBatch,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::{BoundaryOutcome, PhaseStep};
use crate::magician_v2::execution::agentic::run_loop::phases::apply::ApplyGate;
use crate::magician_v2::execution::agentic::run_loop::{phases, IterationStep};
use crate::magician_v2::execution::agentic::types::{
    AgenticContext, ApprovedConfirmationAction, EnvironmentState, ExecutionHistory,
    LoopProtectiveState,
};
use crate::magician_v2::execution::merkle::PageMerkleTree;

/// Run one iteration to its turn boundary, in this process, on this task.
///
/// Seventeen parameters, and the count is the point rather than a smell: they
/// are what the six phases need that the iteration does not produce for itself,
/// and [`super::phases`]'s module docs explain why an ambient bundle was
/// rejected in their favour. Every one is a borrow of a local that
/// `execute_agentically_inner` owns — which is to say, this list *is* the loop
/// state that dies with the process, and the stateless arm's job is to make the
/// same list survive a handoff.
///
/// Returns [`IterationStep::Boundary`] when the iteration ended at the turn
/// boundary — any of the four [`BoundaryOutcome`]s, all of which run the
/// epilogue — and [`IterationStep::RunEnded`] when a phase ended the run, in
/// which case the epilogue is skipped, which is what every run-ending path in
/// this loop has always done.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn run_iteration(
    ctx: &mut AgenticContext,
    executors: &Arc<ActionExecutors>,
    history: &mut ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    trust_dispatch_guard: &mut Option<TrustDispatchGuard>,
    approved_confirmation_actions: &mut Vec<ApprovedConfirmationAction>,
    pending_agentic_tool_lineages: &mut Vec<AgenticToolLineageState>,
    pending_operator_steer: &mut Vec<String>,
    previous_merkle_tree: &mut Option<PageMerkleTree>,
    current_state: &mut EnvironmentState,
    semantic_checkpoint_hints: &[String],
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    ephemeral_scope_id: &str,
    history_iterations_len_at_iter_start: usize,
    iteration_started_at: &Instant,
    iteration: usize,
) -> Result<IterationStep> {
    // The wait this iteration asked for on its way out, honoured at the
    // boundary rather than taken inside the body.
    //
    // The first member of the `BoundaryOutcome` the turn-boundary contract
    // describes (`docs/archive/plans/2026-08-25-iteration-turn-boundary-contract.md`
    // §1, *"`Retry` is not optional"*). Two transient-failure exits used to
    // `sleep(backoff).await` inline and then break. That is correct for a
    // loop that owns its executor and wrong for both consumers of the
    // contract: an in-place sleep holds a worker, and holds a foreign
    // harness session, for the whole backoff.
    //
    // Making the delay a VALUE is the point. The resident driver below still
    // honours it by sleeping, so behaviour is unchanged today — but a driver
    // that does not own the executor can requeue after this delay instead of
    // blocking for it, and neither transient path has to be found and
    // rewritten to make that switch.
    //
    // Since the phase extraction the delay arrives as
    // `BoundaryOutcome::Retry(after)` from the phase that decided it, rather
    // than being written here by the exit itself. This local is only where
    // the resident driver parks it between the boundary and the sleep.
    //
    // A driver local rather than a phase output, for the same reason
    // `iteration_started_at` is a parameter: it is set inside the labelled
    // block and read after it, which is the one thing a `break` out of that
    // block cannot carry.
    let mut boundary_retry_after: Option<Duration> = None;

    // Wrap the iteration body in a labeled block so the many `continue`
    // sites that formerly jumped directly to the next outer-loop turn can
    // instead exit the block and let the stuck-iteration bookkeeping run.
    // Exact execution semantics are preserved: `break 'iteration_body`
    // behaves identically to the old `continue` (both end the current
    // iteration), `return`/early continues above this point are unchanged.
    'iteration_body: {
        // PHASE: Prepare — see `run_loop::phases::prepare`.
        //
        // Lifted out on 2026-08-26. It was the only stretch of this body with
        // no `break 'iteration_body` and no `return`, so it could move as a
        // plain async fn; every other phase has to turn its control flow into
        // a returned value first.
        let phases::prepare::PrepareOutput {
            browser_primitive_enabled,
        } = phases::prepare::run(
            ctx,
            executors,
            history,
            loop_protective,
            current_state,
            semantic_checkpoint_hints,
            cancellation_token.as_ref(),
            iteration,
        )
        .heap_boxed()
        .await;

        // PHASE: Observe — see `run_loop::phases::observe`.
        //
        // Lifted 2026-08-26. Its one exit is a hard failure (a browser state
        // without the primitive pack), so it returns `Result` and the `?`
        // here reproduces the `return Err(..)` it used to perform inline.
        let phases::observe::ObserveOutput { mut observed_state } = phases::observe::run(
            ctx,
            executors,
            loop_protective,
            current_state,
            browser_primitive_enabled,
            iteration,
        )
        .heap_boxed()
        .await?;
        // PHASE: Decide — see `run_loop::phases::decide`.
        //
        // Lifted 2026-08-26. 941 lines with no `break` and no outer
        // `return`, so like `Prepare` it moved as-is; the run-ending checks
        // that surround it (token budget, retry ladder) stayed behind in
        // `Resolve`, which is where they were already written.
        //
        // Its output is handed to `Resolve` whole rather than destructured
        // here: every field is consumed by that phase or forwarded by it, so
        // spreading them out would put seven locals in this body whose only
        // purpose is to be passed straight on.
        let decided = phases::decide::run(
            ctx,
            executors,
            history,
            loop_protective,
            &mut observed_state,
            pending_operator_steer,
            cancellation_token.as_ref(),
            browser_primitive_enabled,
            iteration,
        )
        .heap_boxed()
        .await?;
        // PHASE: Resolve — see `run_loop::phases::resolve`.
        //
        // Lifted 2026-08-26, and the first phase that could not move as it
        // stood. It holds eleven exits — five `break`s that re-decide and
        // six `return`s that end the run — and neither crosses a function
        // boundary, so each had to become a value first. They arrive back
        // as `PhaseStep`, and this `match` is where the driver re-performs
        // them.
        let phases::resolve::ResolveOutput {
            decision,
            observed_state,
            task_state_action_for_decision,
            decision_is_synthetic_stuck,
        } = match phases::resolve::run(
            ctx,
            executors,
            history,
            loop_protective,
            trust_dispatch_guard,
            approved_confirmation_actions,
            pending_agentic_tool_lineages,
            pending_operator_steer,
            observed_state,
            current_state,
            decided,
            cancellation_token,
            execution_start,
            iteration,
        )
        .heap_boxed()
        .await?
        {
            PhaseStep::Continue(resolved) => resolved,
            PhaseStep::Return(outcome) => return Ok(IterationStep::RunEnded(outcome)),
            // The four ways a phase ends an iteration without ending the run.
            // Spelled out one variant at a time rather than folded into a single
            // catch-all: this match IS the resident driver's half of the turn-boundary
            // contract, and a driver that cannot say what it does with `PopFrame` has
            // not implemented it. A worker replaces exactly these four arms.
            PhaseStep::Exit(BoundaryOutcome::Advance) => {
                // EXIT: Advance — the decision gate recorded a rejection or pressure; decide again
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::NextIteration) => {
                // EXIT: NextIteration — the decision gate finished this turn's work
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::Retry(after)) => {
                // The wait the phase asked for, handed to the driver rather than
                // taken inside it. Recorded here and slept on immediately after the
                // boundary, which is where the two inline sleeps used to be.
                boundary_retry_after = Some(after);
                // EXIT: Retry — after the backoff the decision gate asked for
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::PopFrame) => {
                // EXIT: PopFrame — control returned to the owner that delegated this stretch
                break 'iteration_body;
            },
        };

        // PHASE: Apply — see `run_loop::phases::apply`.
        //
        // Lifted 2026-08-26. The eight-way `match decision` — larger than
        // the other five phases put together — with fifteen `break`s and
        // twenty-five run-ending `return`s. Nine further `return`s inside its
        // `Decision::Execute` arm belong to that arm's own
        // `Box::pin(async { … })` and stayed where they were — see the
        // module docs for how the two were told apart.
        // The batch `Apply` gated, and this arm's answer to it is deliberately
        // to drop it. There is nowhere for it to go and nothing that would read
        // it: this driver's continuation IS the Rust stack, so there is no
        // committed state for a later worker to reconcile against, and a phase
        // that fails here propagates straight out with `?` exactly as it did
        // before the parameter existed. Naming the local rather than passing
        // `&mut None` inline is what makes the drop a visible decision — and
        // keeps a `let _ = ...` read of it one line away if this arm ever grows
        // somewhere to put it.
        //
        // This is also the whole reason the parameter is an out-parameter: the
        // stateless arm records it on the phase's ERROR path, which a return
        // value cannot carry. See `phases::apply::gate`'s own docs.
        let mut committed_batch: Option<PendingBatch> = None;
        // The same answer, for the same reason, to the other half of the seam.
        let mut settled: Vec<(EffectId, EffectOutcome)> = Vec::new();
        // PHASE: Apply, first half — gate, and fire nothing.
        //
        // This arm calls both halves back to back with nothing in between, and
        // that IS its implementation of the seam rather than a shortcut past it.
        // The step the seam exists for is a durable intent commit, and this
        // driver has nothing durable to commit to — so the honest thing is to
        // say so in the type and continue, which is what
        // `ApplyIntents::no_durable_ledger` is. Recording something here would
        // make this arm durable, and an arm that is already durable answers
        // nothing the flip gate is asking.
        let gated = match phases::apply::gate(
            ctx,
            executors,
            history,
            loop_protective,
            // Shared, not exclusive: `Apply` only ever reads this. `Resolve`
            // above genuinely refreshes it and keeps its `&mut`.
            trust_dispatch_guard,
            approved_confirmation_actions,
            pending_agentic_tool_lineages,
            previous_merkle_tree,
            current_state,
            observed_state,
            decision,
            task_state_action_for_decision,
            decision_is_synthetic_stuck,
            cancellation_token,
            execution_start,
            iteration,
            &mut committed_batch,
            phases::apply::EffectIdentityPosture::InProcess,
        )
        .heap_boxed()
        .await?
        {
            ApplyGate::Settled(step) => step,
            // PHASE: Apply, second half — fire the batch just gated.
            ApplyGate::Gated(gated) => {
                phases::apply::dispatch(
                    gated,
                    // The whole of this arm's ledger posture, stated rather than
                    // omitted. See the note above the gate call.
                    ApplyIntents::no_durable_ledger(),
                    // The same answer to the plan, for the same reason: no
                    // earlier attempt's effects can outlive a Rust stack, so
                    // every member is owed a fire. See `EffectPlan`.
                    EffectPlan::nothing_resolved(),
                    ctx,
                    executors,
                    history,
                    loop_protective,
                    trust_dispatch_guard,
                    approved_confirmation_actions,
                    pending_agentic_tool_lineages,
                    current_state,
                    cancellation_token,
                    execution_start,
                    ephemeral_scope_id,
                    iteration,
                    &mut settled,
                )
                .heap_boxed()
                .await?
            },
        };
        match gated {
            // Unreachable today, and the compiler is what says so: every
            // arm of the eight-way `match decision` inside `Apply`
            // diverges, so it has no continue path. The arm stays because
            // the contract is per-phase — a driver must handle `Continue`
            // from a phase, and `Resolve` produces it — and because the day
            // an arm stops diverging this is where it lands.
            PhaseStep::Continue(()) => {},
            PhaseStep::Return(outcome) => return Ok(IterationStep::RunEnded(outcome)),
            // The four ways a phase ends an iteration without ending the run.
            // Spelled out one variant at a time rather than folded into a single
            // catch-all: this match IS the resident driver's half of the turn-boundary
            // contract, and a driver that cannot say what it does with `PopFrame` has
            // not implemented it. A worker replaces exactly these four arms.
            PhaseStep::Exit(BoundaryOutcome::Advance) => {
                // EXIT: Advance — the applied decision recorded a rejection or pressure; decide again
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::NextIteration) => {
                // EXIT: NextIteration — the applied decision finished this turn's work
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::Retry(after)) => {
                // The wait the phase asked for, handed to the driver rather than
                // taken inside it. Recorded here and slept on immediately after the
                // boundary, which is where the two inline sleeps used to be.
                boundary_retry_after = Some(after);
                // EXIT: Retry — after the backoff the applied decision asked for
                break 'iteration_body;
            },
            PhaseStep::Exit(BoundaryOutcome::PopFrame) => {
                // EXIT: PopFrame — control returned to the owner that delegated this stretch
                break 'iteration_body;
            },
        }
    } // end 'iteration_body

    // The resident driver's implementation of `Retry { after }`: sleep.
    //
    // Placed here, first thing after the boundary, so the order the old
    // inline sleeps produced is preserved exactly — wait, then run the
    // iteration's bookkeeping. `duration_ms` is read further down from
    // `iteration_started_at`, so a backed-off iteration still reports the
    // wait inside its own duration, as it always has.
    //
    // Still `heap_boxed`, as both call sites were. This future lives in the
    // outer loop body of a function with a stack-depth contract
    // (`default_stack_` regressions); letting it sit inline would grow the
    // agentic state machine on a Tokio worker stack for no reason.
    if let Some(after) = boundary_retry_after.take() {
        tokio::time::sleep(after).heap_boxed().await;
    }

    // PHASE: Epilogue — see `run_loop::phases::epilogue`.
    //
    // Lifted 2026-08-26. The only phase outside the turn boundary, which is
    // exactly what it means: it runs for every iteration that ended by
    // leaving the block and is skipped by every path that ended the run.
    phases::epilogue::run(
        ctx,
        executors,
        history,
        loop_protective,
        history_iterations_len_at_iter_start,
        u64::try_from(iteration_started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
        iteration,
    );

    Ok(IterationStep::Boundary)
}

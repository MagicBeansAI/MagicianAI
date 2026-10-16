//! `Epilogue` — what an iteration records about itself on the way out.
//!
//! The sixth and last phase, and the only one that runs *after* the turn
//! boundary rather than inside it. That placement is the phase's entire
//! meaning: it runs for every iteration that ended by leaving the block, and is
//! deliberately skipped by every path that ended the run. A terminal outcome
//! reports itself through `AgenticExecutionCompleted` / `AgenticWaitingForUser`
//! instead, so counting it here as well would double-count it — and, worse,
//! would let the stuck detector accrue a strike against a run that had already
//! finished.
//!
//! # Two things, and the second one pairs
//!
//! The stuck detector is observability only: it counts iterations that landed no
//! tool, keeps the last few action summaries, and warns at three. The
//! `AgenticIterationCompleted` event pairs with the `AgenticIterationStarted`
//! that [`super::prepare`] emits, which is why both live at the ends of the
//! iteration rather than beside each other.
//!
//! `duration_ms` is supplied by the driver, so an iteration that took a
//! `BoundaryOutcome::Retry` backoff still reports the wait inside its own
//! duration. The in-process driver derives it from its monotonic clock; the
//! stateless driver derives it from the durable iteration checkpoint.

use chrono::Utc;
use tracing::warn;

use crate::magician_v2::execution::agentic::executor::{
    action_summary_for_diagnostics, iteration_landed_a_tool, push_recent_summary,
    runtime_execution_id, transport_scope, ActionExecutors,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
use crate::magician_v2::execution::agentic::types::{
    AgenticContext, ExecutionHistory, LoopProtectiveState,
};
use crate::magician_v2::RuntimeTransportEvent;

/// How many consecutive tool-less iterations before the run is called stuck.
///
/// Moved here from a `const` inside `execute_agentically_inner` when this phase
/// was lifted: the detector was its only reader, and a constant left behind in
/// the driver would have been dead there while the code that gives it meaning
/// lived somewhere else.
///
/// Observability only. Reaching it emits `AgenticStepStuckWarning` on every
/// iteration the run stays above it, and alters no control flow.
const STUCK_WARNING_THRESHOLD: usize = 3;

/// Run the epilogue phase.
///
/// Synchronous, unlike every phase before it. Nothing here awaits — the counters
/// are arithmetic and both emissions are fire-and-forget on the transport — so
/// making it `async` would add a state machine to a function with a stack-depth
/// contract and buy nothing.
///
/// Seven parameters, which is exactly `clippy.toml`'s
/// `too-many-arguments-threshold` and so needs no `allow` — an `allow` that
/// suppresses nothing is a suppression nobody can tell from a real one.
pub(in crate::magician_v2::execution::agentic) fn run(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    history_iterations_len_at_iter_start: usize,
    duration_ms: u64,
    iteration: usize,
) {
    // ──────────────────────────────────────────────────────────────
    // Stuck-iteration detector (Fix #3, Unit B). Observability only.
    // Runs at the end of every iteration that exited via
    // `break 'iteration_body`. `return` / the early `continue` for
    // `skip_iterations > 0` / cancellation bypass this logic, which
    // is intentional (those paths are not real iterations).
    // ──────────────────────────────────────────────────────────────
    if iteration_landed_a_tool(history, history_iterations_len_at_iter_start) {
        loop_protective.consecutive_no_action_iterations = 0;
        loop_protective.recent_no_action_summaries.clear();
    } else {
        loop_protective.consecutive_no_action_iterations += 1;
        // Project the most-recently-pushed IterationRecord (if any) into
        // a short action-summary string. Prefers `action_signature`, the
        // same helper used by loop detection and agentic decision events.
        // Fallback covers the "no record pushed at all this iteration"
        // case (e.g. an early parse-failure path that did push, or an
        // ExecutePathControl::Continue path that pushed nothing).
        // Future work: richer action summaries could carry structured
        // context (step goal, constraint violated) when Fix #2 lands.
        let summary = if history.iterations.len() > history_iterations_len_at_iter_start {
            history
                .iterations
                .last()
                .map(|r| {
                    action_summary_for_diagnostics(&r.action, ctx.app_disclosure_guard.is_some())
                })
                .unwrap_or_else(|| format!("iter {}: no decision recorded", iteration))
        } else {
            format!("iter {}: no decision recorded", iteration)
        };
        push_recent_summary(
            &mut loop_protective.recent_no_action_summaries,
            summary,
            STUCK_WARNING_THRESHOLD,
        );

        if loop_protective.consecutive_no_action_iterations >= STUCK_WARNING_THRESHOLD {
            warn!(
                iteration = iteration,
                consecutive = loop_protective.consecutive_no_action_iterations,
                "[STUCK-WARNING] step has produced no successful action for 3 or more \
                 consecutive iterations",
            );
            if ctx.has_observability() {
                let (principal, workspace) = transport_scope(ctx);
                super::outbox::journal_and_emit(
                    ctx,
                    executors,
                    iteration,
                    Phase::Epilogue,
                    RuntimeTransportEvent::AgenticStepStuckWarning {
                        execution_id: runtime_execution_id(ctx),
                        principal,
                        workspace,
                        plan_id: ctx.plan_id.clone().unwrap_or_default(),
                        step_id: ctx.step_id.clone().unwrap_or_default(),
                        iteration,
                        is_preflight: false,
                        consecutive_count: loop_protective.consecutive_no_action_iterations,
                        recent_actions: loop_protective
                            .recent_no_action_summaries
                            .iter()
                            .cloned()
                            .collect(),
                        timestamp: Utc::now().timestamp_millis(),
                    },
                );
            }
        }
    }

    // Emit iteration completed at the natural end of the loop body so
    // subscribers (chat, observability, /debug timeline) can pair it with
    // the matching `AgenticIterationStarted`. Early `return` exits skip
    // this — those iterations are captured by terminal events
    // (AgenticExecutionCompleted, AgenticWaitingForUser, etc.) instead.
    if ctx.has_observability() {
        let (principal, workspace) = transport_scope(ctx);
        super::outbox::journal_and_emit(
            ctx,
            executors,
            iteration,
            Phase::Epilogue,
            RuntimeTransportEvent::AgenticIterationCompleted {
                execution_id: runtime_execution_id(ctx),
                principal,
                workspace,
                plan_id: ctx.plan_id.clone().unwrap_or_default(),
                step_id: ctx.step_id.clone().unwrap_or_default(),
                iteration,
                duration_ms,
                outcome: "loop_continue".to_string(),
                timestamp: Utc::now().timestamp_millis(),
            },
        );
    }
}

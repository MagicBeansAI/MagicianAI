//! `Prepare` — everything an iteration does before it looks at the world.
//!
//! The first phase lifted out of `'iteration_body`, and deliberately the first:
//! it is one of only two stretches of the body with **zero
//! `break 'iteration_body` and zero `return`**. The other is `Decide`, ten times
//! its size. That matters more than its size. Neither of those crosses a
//! function boundary, so every phase that held one had to hand it back as a
//! value before it could move — `Result` for `Observe`'s single run-ending
//! exit, `PhaseStep` for the dozens in `Resolve` and `Apply`. This one held
//! none, and it is small enough to establish the shape on rather than discover
//! it on 940 lines.
//!
//! The stretch it replaces was 98 lines of the body, against the design's
//! estimate of 97.

use crate::magician_v2::execution::agentic::executor::{
    browser_pack_uses_primitive, emit_agent_execution_mapping, emit_step_events_if_signaled,
    hydrate_prompt_context_after_owner_transition_with_cancellation, runtime_execution_id,
    transport_scope, ActionExecutors, HeapAwaitExt,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
use crate::magician_v2::execution::agentic::types::{
    AgenticContext, EnvironmentState, ExecutionHistory, LoopProtectiveState,
};
use tokio_util::sync::CancellationToken;
use tracing::debug;

/// What the rest of the iteration needs from `Prepare`.
///
/// The first real instance of the design's `PhaseOutput`, and it arrived the way
/// these things actually arrive: by extraction going wrong. Lifting `Prepare`
/// out took `browser_primitive_enabled` with it, leaving three later uses in the
/// body with no binding. The lesson generalises to every remaining phase — a
/// phase does not merely *do* things, it **yields** what the phases after it
/// read, and a lift that ignores that compiles only by accident.
///
/// Recomputing it at the use sites would have been the smaller diff and the
/// wrong answer: the original computes it once, here, and an owner transition in
/// `Apply` can change what `browser_pack_uses_primitive` returns. Three
/// recomputed reads could then disagree with each other inside one iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrepareOutput {
    /// Whether the browser pack declares `implementation.type: primitive`,
    /// resolved once for this iteration.
    pub browser_primitive_enabled: bool,
}

/// Run the prepare phase.
///
/// Returns no `PhaseOutcome`: this phase cannot end an iteration or a run, which
/// is precisely why it was the one extractable without converting control flow
/// first.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn run(
    ctx: &mut AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    loop_protective: &mut LoopProtectiveState,
    current_state: &EnvironmentState,
    semantic_checkpoint_hints: &[String],
    cancellation_token: Option<&CancellationToken>,
    iteration: usize,
) -> PrepareOutput {
    // Update shell streaming step_index and step_id so each iteration's
    // chunks carry the correct index/id instead of a hardcoded 0 (I1/I5 fix).
    executors.controls.set_shell_stream_iteration(iteration);

    // Emit canonical step events from the previous iteration's decision.
    // These remain as compatibility signals for V3/UI state, but prompt
    // state now comes from the mutable live taskplan itself.
    if iteration > 1 {
        let previous_iteration_record = history
            .iterations
            .iter()
            .rev()
            .find(|record| record.iteration == iteration - 1);
        emit_step_events_if_signaled(
            executors,
            // THIS phase, not the iteration the events describe. The records
            // this helper produces are swept by the drain that runs when
            // `prepare` returns, so their address is `Prepare` of the CURRENT
            // iteration even though the step events themselves report
            // `iteration - 1` below.
            Some((iteration, Phase::Prepare)),
            ctx,
            &mut loop_protective.pending_step_completed,
            &mut loop_protective.pending_step_failed,
            &mut loop_protective.started_step_ids,
            iteration - 1,
            previous_iteration_record,
        )
        .heap_boxed()
        .await;
    }

    if !semantic_checkpoint_hints.is_empty() {
        ctx.advance_task_prompt_context_checkpoint(semantic_checkpoint_hints.join("\n"));
        hydrate_prompt_context_after_owner_transition_with_cancellation(
            ctx,
            executors,
            cancellation_token,
        )
        .heap_boxed()
        .await;
    }

    // Reset focused_tool each iteration to prevent stale carry-over.
    // Runtime-context execution defaults to the browser pack below when
    // the current agent owns browser capability and it is not an inner-loop tool.
    ctx.scratch.clear_focused_tool();
    let browser_primitive_enabled = browser_pack_uses_primitive(executors);

    // Default focused_tool to browser pack when nothing else is focused AND the
    // agent actually has browser in its tool set. This ensures
    // {capabilities_section} injects browser action documentation for
    // browser-capable agents, while agents without browser (e.g.,
    // bash-only or http-only) don't get misleading browser docs.
    if ctx
        .scratch
        .focused_tool
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .is_none()
    {
        let agent_has_browser = ctx.merged_agent_tools.iter().any(|t| t.name == "browser");
        if agent_has_browser && !browser_primitive_enabled {
            if let Some(registry) = executors.effective_capability_registry_snapshot() {
                let browser_focused = registry
                    .get_pack_definition("browser")
                    .and_then(|pack| serde_yaml::to_string(&pack).ok())
                    .map(|yaml| ("browser".to_string(), yaml));
                *ctx.scratch
                    .focused_tool
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()) = browser_focused;
            }
        }
    }

    debug!("Agentic iteration {}/{}", iteration, ctx.max_iterations);

    // Emit iteration started event (paired with `AgenticIterationCompleted`
    // emitted at the end of the labeled body — see `iteration_started_at`
    // declared above the block for duration tracking).
    if ctx.has_observability() {
        let (principal, workspace) = transport_scope(ctx);
        emit_agent_execution_mapping(ctx, executors, Some((iteration, Phase::Prepare)));
        super::outbox::journal_and_emit(
            ctx,
            executors,
            iteration,
            Phase::Prepare,
            crate::magician_v2::RuntimeTransportEvent::AgenticIterationStarted {
                execution_id: runtime_execution_id(ctx),
                principal,
                workspace,
                plan_id: ctx.plan_id.clone().unwrap_or_default(),
                step_id: ctx.step_id.clone().unwrap_or_default(),
                iteration,
                environment_type: current_state.type_name().to_string(),
                timestamp: chrono::Utc::now().timestamp_millis(),
            },
        );
    }

    PrepareOutput {
        browser_primitive_enabled,
    }
}

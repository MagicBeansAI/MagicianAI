//! Pre-decided decisions — chat pack direct dispatch.
//!
//! A run whose context carries a [`PreDecidedPackCall`] already knows its one
//! call: the caller (a chat turn that named one pack leaf and every argument)
//! decided it. Asking a model to re-derive it would only add a round-trip and
//! a chance to get it wrong, so `Decide` returns it as-is.
//!
//! # Which gates apply, and why they all do
//!
//! A model turn's `Decide` also projects the owner's effective tool policy
//! (`native_integration::decision_rail_catalog`), and that projection is what
//! fills the policy-snapshot slots `Apply` checks a dispatch against — and
//! what fails closed on a mismatched invocation/owner frame, an unresolvable
//! external policy, or an engagement ceiling. [`authorize`] runs that same
//! projection before the call is returned, promoting the leaf into the working
//! set the way a `tool_search` selection does when the leaf is authorized but
//! deferred. A leaf outside the owner's policy therefore stays out of the
//! snapshot, `Apply` refuses it and records the refusal, and the next `Decide`
//! fails the run; it is never dispatched. An owner whose policy cannot be
//! projected at all fails here. Everything after `Decide` — `Resolve`,
//! `Apply` (policy snapshot, trust, approval rules, allowlist, lineage, effect
//! ids) and `Observe` — runs exactly as it does for a model's call.
//!
//! Once the call has settled, the terminal is synthesized from its result —
//! again without a model, and without a policy projection, since no tool is
//! dispatched. Ordinarily that happens in the same `Apply` that dispatched the
//! call, once its effect outcome is recorded ([`fold_settled_terminal`]), so a
//! pre-decided run ends in one iteration; a `Decide` that finds the call
//! settled (a resumed segment, a fold that stood aside or failed, a refused
//! terminal) synthesizes the same terminal.
//!
//! # Why the decision is rebuilt, never cached
//!
//! An approval pause resumes by re-running `Decide`, and `Apply` waives the
//! approval only when the re-emitted action's stable serialization matches the
//! one the user approved. The action is rebuilt from the persisted
//! [`PreDecidedPackCall`] through the same lowering both times, so the resumed
//! iteration re-emits the approved call instead of silently asking a model.
//!
//! # Why the identity is synthetic
//!
//! The default stateless driver refuses a governed or outward dispatch that
//! has no effect id, and the effect id is minted from the last assistant
//! turn's `llm_call_id` plus the candidate's `tool_call_id`. A
//! `synthetic:predecided:…` trace on a synthetic assistant turn whose single
//! tool call carries the caller's id supplies both, and `Resolve` already
//! treats `synthetic:` call ids as loop-synthesized.

use std::sync::Arc;
use std::time::Instant;

use anyhow::Result;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use super::decide::DecideOutput;
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::execution::agentic::decision::Decision;
use crate::magician_v2::execution::agentic::executor::{
    promote_predecided_leaf, response_kind_from_decision, running_as_in_context_delegate,
    runtime_execution_id, transport_scope, truncate_utf8_boundary, AgenticToolLineageState,
    HeapAwaitExt, TrustDispatchGuard,
};
use crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call;
use crate::magician_v2::execution::agentic::native_types::{
    ExecutionToolCall, NativeDecisionOutcome,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::{
    BoundaryOutcome, Phase, PhaseStep,
};
use crate::magician_v2::execution::agentic::types::{
    action_type_and_tool_name, AgenticAssistantToolCallRecord, AgenticAssistantTurnRecord,
    AgenticContext, ApprovedConfirmationAction, Artifact, EnvironmentState, ExecutionHistory,
    LoopProtectiveState, PreDecidedOutcome, PreDecidedPackCall, PreDecidedProgress,
};
use crate::magician_v2::execution::agentic::ActionExecutors;
use crate::magician_v2::execution::durable_task_state::TaskStateActionEnvelope;
use crate::magician_v2::execution::merkle::PageMerkleTree;
use crate::magician_v2::RuntimeTransportEvent;

/// `synthetic:{kind}:…` trace kind for a caller-decided decision.
const PREDECIDED_TRACE_KIND: &str = "predecided";
/// Trace kind for the terminal folded into the dispatching `Apply`. Its own
/// kind because the fold happens in the iteration whose `Execute` already
/// holds `synthetic:predecided:…:{n}`: one kind would give the call that
/// produced the result and the terminal that consumes it one call id, where
/// the two-iteration run always gave them two.
const PREDECIDED_SETTLED_TRACE_KIND: &str = "predecided_settled";
/// Operation recorded on the synthetic assistant turn.
const PREDECIDED_OPERATION: &str = "agentic_decision_predecided";
/// Event text stays well under the journal's record cap.
const MAX_EVENT_TEXT_BYTES: usize = 2_048;

/// The caller-decided decision for this iteration, or `None` when the run has
/// no pre-decided call and `Decide` must proceed as usual.
///
/// - Not yet settled: `Execute` the leaf.
/// - A single proven pre-I/O admission expiry: `Execute` it once more, exactly
///   as a model's call gets one fresh attempt.
/// - Settled successfully: `Completed`, the leaf's raw result as the verified
///   task deliverable.
/// - Settled with a failure, or any failed record after the leaf's last
///   result (`Apply` refusing the synthesized terminal): `Failed` with that
///   reason.
///
/// "Settled" also covers an outcome a resumed segment carried in
/// (`ctx.predecided_settled`): its history starts empty, and the call must
/// never fire twice.
pub(super) fn maybe_predecided_decide(
    ctx: &AgenticContext,
    history: &ExecutionHistory,
    iteration: usize,
) -> Option<DecideOutput> {
    let call = ctx.predecided_call.as_ref()?;
    let trace =
        super::decision_rail_events::synthetic_trace_context(ctx, PREDECIDED_TRACE_KIND, iteration);
    Some(
        match call.progress(history, ctx.predecided_settled.as_ref()) {
            PreDecidedProgress::Unsettled | PreDecidedProgress::RetryAdmission => {
                execute_output(call, trace)
            },
            PreDecidedProgress::Settled(outcome) => {
                terminal_output(settled_terminal(call, outcome), trace)
            },
        },
    )
}

/// The terminal a pre-decided call settles to, read straight after its
/// dispatch, or `None` when there is nothing to fold.
///
/// `Some` only for [`PreDecidedProgress::Settled`] — the same outcome, and the
/// same `Completed` / `Failed`, that [`maybe_predecided_decide`] would return
/// from the next `Decide`. An unsettled call and a single pre-I/O admission
/// expiry ([`PreDecidedProgress::RetryAdmission`], which keeps its one fresh
/// attempt for the next iteration) are `None`, as is a run with no
/// pre-decided call.
pub(in crate::magician_v2::execution::agentic) fn settled_terminal_after_dispatch(
    ctx: &AgenticContext,
    history: &ExecutionHistory,
    iteration: usize,
) -> Option<DecideOutput> {
    let call = ctx.predecided_call.as_ref()?;
    match call.progress(history, ctx.predecided_settled.as_ref()) {
        PreDecidedProgress::Settled(outcome) => {
            let trace = super::decision_rail_events::synthetic_trace_context(
                ctx,
                PREDECIDED_SETTLED_TRACE_KIND,
                iteration,
            );
            Some(terminal_output(settled_terminal(call, outcome), trace))
        },
        PreDecidedProgress::Unsettled | PreDecidedProgress::RetryAdmission => None,
    }
}

/// The terminal to fold into this `Apply`, when its dispatch ended on
/// `NextIteration`, the pre-decided call has settled, and this process has
/// not been asked to cancel or pause. `None` otherwise.
///
/// Also `None` for a run with an owner to yield back to: its terminal arm
/// pops the frame through `yield_back_to_previous_owner`, which can fail
/// after the durable task-state commit — an error the fold could not hand
/// back to a clean iteration 2. Such a run keeps its second iteration.
///
/// A cheap, synchronous check: the stateless host asks it before paying for
/// its deadline, budget, steer, cancel, pause and scope reads.
pub(in crate::magician_v2::execution::agentic) fn terminal_to_fold(
    step: &PhaseStep<()>,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    history: &ExecutionHistory,
    cancellation_token: &Option<CancellationToken>,
    iteration: usize,
) -> Option<DecideOutput> {
    if !matches!(step, PhaseStep::Exit(BoundaryOutcome::NextIteration)) {
        return None;
    }
    if cancellation_token
        .as_ref()
        .is_some_and(CancellationToken::is_cancelled)
        || executors.manual_pause_requested()
    {
        return None;
    }
    if !ctx.owner_stack.is_empty() && !running_as_in_context_delegate(ctx) {
        return None;
    }
    settled_terminal_after_dispatch(ctx, history, iteration)
}

/// End a settled pre-decided run in the `Apply` that dispatched its call.
///
/// Without this, the dispatch's `NextIteration` buys a whole second iteration
/// — `Prepare`, `Observe`, `Decide`, `Resolve`, `Apply`, each a durable commit
/// under the stateless driver — whose only work is to read the settled result
/// back and synthesize the terminal. Measured live at 2–2.8 s of a 4–6 s loop.
///
/// The folded terminal takes the path any terminal decision takes:
/// `AgenticDecisionMade` journalled (addressed to `Apply`, the phase that now
/// makes it), then `Resolve` run whole — the token-budget check, the synthetic
/// assistant turn and local-turn keeping, the owner/trust refresh, the
/// structural-approval gate — and then the `Completed` / `Failed` arm through
/// [`super::apply::settle_terminal`], with its terminal gates. Only the
/// no-dispatch arm runs: re-entering `apply::gate` would emit the plan-step
/// boundary twice for one `Apply`.
///
/// `output` is what [`terminal_to_fold`] returned. The stateless host calls
/// this from `WorkerHost::fold_after_dispatch`, which the driver runs only
/// after it has recorded the dispatch's effect outcomes. The fold stands
/// aside on a cancel, a manual pause, a lost scope, a pending operator steer,
/// an exhausted budget or a passed deadline; on an error it restores the
/// in-memory state and the run falls back to iteration 2.
///
/// # What an `Err` can have done
///
/// The `Completed` arm's fallible steps — `Resolve`'s owner/trust refresh,
/// the protected-app readiness read, and the task-state commit itself
/// (documented as atomic) — all come before or are that arm's one task-state
/// write. Its one failure after the commit, `yield_back_to_previous_owner`,
/// is not reachable because [`terminal_to_fold`] does not fold a run with an
/// owner to yield to. So an `Err` here has not committed task state. What it
/// may have done is outside what this can undo: an event the outbox refused
/// to journal was emitted inline at once and cannot be withdrawn, and a
/// refreshed owner profile stays in `ctx` (only a cache the next `Resolve`
/// refreshes again). Everything else it touched — history, protective
/// counters, the trust guard, approvals, lineages, the steer buffer, the
/// current state — is restored, and the host discards what it journalled, so
/// iteration 2 starts where it would have without the fold. A lease lost
/// while the fold is awaited drops this future without the restore, as it
/// drops any phase.
#[allow(clippy::too_many_arguments)]
pub(in crate::magician_v2::execution::agentic) async fn fold_settled_terminal(
    output: DecideOutput,
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
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    iteration: usize,
) -> Result<PhaseStep<()>> {
    let before = (
        history.clone(),
        loop_protective.clone(),
        trust_dispatch_guard.clone(),
        approved_confirmation_actions.clone(),
        pending_agentic_tool_lineages.clone(),
        pending_operator_steer.clone(),
        current_state.clone(),
    );
    let folded = fold_terminal(
        output,
        ctx,
        executors,
        history,
        loop_protective,
        trust_dispatch_guard,
        approved_confirmation_actions,
        pending_agentic_tool_lineages,
        pending_operator_steer,
        previous_merkle_tree,
        current_state,
        cancellation_token,
        execution_start,
        iteration,
    )
    .heap_boxed()
    .await;
    if folded.is_err() {
        (
            *history,
            *loop_protective,
            *trust_dispatch_guard,
            *approved_confirmation_actions,
            *pending_agentic_tool_lineages,
            *pending_operator_steer,
            *current_state,
        ) = before;
    }
    folded
}

#[allow(clippy::too_many_arguments)]
async fn fold_terminal(
    output: DecideOutput,
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
    cancellation_token: &Option<CancellationToken>,
    execution_start: &Instant,
    iteration: usize,
) -> Result<PhaseStep<()>> {
    emit_decision_made(ctx, executors, iteration, Phase::Apply, &output);
    // What `Observe` hands the next iteration for a non-browser state; a pack
    // call leaves the state where its result put it.
    let observed_state = current_state.clone();
    let resolved = match super::resolve::run(
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
        output,
        cancellation_token,
        execution_start,
        iteration,
    )
    .heap_boxed()
    .await?
    {
        PhaseStep::Continue(resolved) => resolved,
        PhaseStep::Exit(boundary) => return Ok(PhaseStep::Exit(boundary)),
        PhaseStep::Return(outcome) => return Ok(PhaseStep::Return(outcome)),
    };
    super::apply::settle_terminal(
        ctx,
        executors,
        history,
        loop_protective,
        trust_dispatch_guard,
        approved_confirmation_actions,
        pending_agentic_tool_lineages,
        previous_merkle_tree,
        current_state,
        resolved.observed_state,
        resolved.decision,
        resolved.task_state_action_for_decision,
        cancellation_token,
        execution_start,
        iteration,
    )
    .heap_boxed()
    .await
}

/// Project the owner's effective tool policy for an `Execute` of the
/// pre-decided call, exactly as a model turn's `Decide` does, so `Apply`
/// checks the dispatch against the same snapshot. Terminal decisions pass
/// through untouched.
///
/// An authorized but deferred leaf is promoted into the working set the way a
/// `tool_search` selection promotes one, then the policy is projected again.
/// An unauthorized leaf is not promoted (promotion intersects with the
/// owner's authorized names), so `Apply` refuses it. An owner whose policy
/// cannot be projected at all fails the run here.
pub(super) async fn authorize(
    ctx: &AgenticContext,
    iteration: usize,
    output: DecideOutput,
) -> DecideOutput {
    let Some(call) = ctx.predecided_call.as_ref() else {
        return output;
    };
    if !matches!(output.decision_result, Ok(Decision::Execute { .. })) {
        return output;
    }
    let _ = crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog(
        ctx, iteration,
    )
    .await;
    if dispatch_snapshot_lacks(ctx, &call.leaf) == Some(true) {
        promote_predecided_leaf(ctx, &call.leaf);
        let _ = crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog(
            ctx, iteration,
        )
        .await;
    }
    if ctx.owner_definition.is_some() && dispatch_snapshot_lacks(ctx, &call.leaf).is_none() {
        let trace = output
            .llm_trace_context_for_decision
            .clone()
            .unwrap_or_else(|| {
                super::decision_rail_events::synthetic_trace_context(
                    ctx,
                    PREDECIDED_TRACE_KIND,
                    iteration,
                )
            });
        return terminal_output(
            Decision::Failed {
                reason: format!(
                    "the owner's effective tool policy could not be resolved; the pre-decided \
                     call `{}` was refused",
                    call.leaf
                ),
            },
            trace,
        );
    }
    output
}

/// `Some(true)` when an owner-scoped policy snapshot is active and does not
/// list `leaf` as callable, `Some(false)` when it does, `None` when no
/// snapshot is active (detached/legacy execution) or the slot is unreadable.
pub(super) fn dispatch_snapshot_lacks(ctx: &AgenticContext, leaf: &str) -> Option<bool> {
    let guard = ctx.policy_dispatch_tool_names.lock().ok()?;
    guard.as_ref().map(|names| !names.contains(leaf))
}

/// Journal and emit `AgenticDecisionMade` for a caller-decided decision, the
/// way the Decision Engine rail does for its model-free selections, so the
/// run's decision trail does not go silent for the pre-decided turn.
///
/// `phase` is the phase that made the decision: `Decide` for the `Execute`,
/// `Apply` for a terminal [`fold_settled_terminal`] folds in after dispatch.
pub(super) fn emit_decision_made(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    phase: Phase,
    output: &DecideOutput,
) {
    let (Some(call), Ok(decision)) = (
        ctx.predecided_call.as_ref(),
        output.decision_result.as_ref(),
    ) else {
        return;
    };
    if !ctx.has_observability() {
        return;
    }
    let (principal, workspace) = transport_scope(ctx);
    let (action_type, candidates_count, evidence) = match decision {
        Decision::Execute { candidates, .. } => (
            candidates
                .candidates
                .first()
                .map(|candidate| action_type_and_tool_name(&candidate.action).0),
            Some(candidates.candidates.len()),
            None,
        ),
        // The deliverable can be up to 256 KiB; the event stays a pointer to
        // it, as the Decision Engine rail's does, so it fits the journal cap.
        Decision::Completed { .. } => (None, None, None),
        Decision::Failed { reason } => (
            None,
            None,
            Some(truncate_utf8_boundary(reason, MAX_EVENT_TEXT_BYTES).to_string()),
        ),
        _ => (None, None, None),
    };
    let event = RuntimeTransportEvent::AgenticDecisionMade {
        execution_id: runtime_execution_id(ctx),
        principal,
        workspace,
        plan_id: ctx.plan_id.clone().unwrap_or_default(),
        step_id: ctx.step_id.clone().unwrap_or_default(),
        iteration,
        decision_type: response_kind_from_decision(decision).into(),
        action_summary: Some(call.leaf.clone()),
        reasoning: format!("decision:{PREDECIDED_TRACE_KIND}:caller-decided"),
        confidence: 1.0,
        thinking: Some("Caller-decided pack call (pre-decided, no model)".to_string()),
        evidence,
        tool_name: Some(call.leaf.clone()),
        action_type,
        element_id: None,
        candidates_count,
        raw_decision: Some(serde_json::json!({
            "operation": PREDECIDED_OPERATION,
            "leaf": call.leaf,
            "tool_call_id": call.tool_call_id,
        })),
        timestamp: Utc::now().timestamp_millis(),
    };
    super::outbox::journal_and_emit(ctx, executors, iteration, phase, event);
}

/// `Execute` the leaf through the same lowering a model's native call takes.
/// Anything that does not lower to exactly one pack call for this leaf fails
/// the run rather than letting a caller-decided call reach a control tool (a
/// terminal, a delegation) that eligibility never meant to allow.
fn execute_output(call: &PreDecidedPackCall, trace: magicllm::LlmTraceContext) -> DecideOutput {
    let tool_call = ExecutionToolCall {
        id: call.tool_call_id.clone(),
        name: call.leaf.clone(),
        arguments: call.args.clone(),
    };
    let refuse = |reason: String, trace| terminal_output(Decision::Failed { reason }, trace);
    let envelope = match lower_native_tool_call(&tool_call) {
        NativeDecisionOutcome::Valid(envelope)
            if is_single_pack_call(&envelope.decision, &call.leaf) =>
        {
            envelope
        },
        NativeDecisionOutcome::Valid(_) => {
            return refuse(
                format!(
                    "the pre-decided call `{}` did not lower to a single pack call",
                    call.leaf
                ),
                trace,
            );
        },
        NativeDecisionOutcome::InvalidArguments { error, .. } => {
            return refuse(
                format!(
                    "the pre-decided call `{}` has invalid arguments: {error}",
                    call.leaf
                ),
                trace,
            );
        },
        NativeDecisionOutcome::UnknownTool { tool_name, .. } => {
            return refuse(
                format!("the pre-decided call names an unknown tool `{tool_name}`"),
                trace,
            );
        },
        NativeDecisionOutcome::ZeroToolCalls { .. }
        | NativeDecisionOutcome::MultipleToolCalls { .. } => {
            return refuse(
                format!(
                    "the pre-decided call `{}` did not lower to one tool call",
                    call.leaf
                ),
                trace,
            );
        },
    };
    let turn = AgenticAssistantTurnRecord {
        iteration: 0,
        operation: PREDECIDED_OPERATION.into(),
        llm_trace_context: Some(trace.clone()),
        text: Some(format!("Pre-decided call: {}", call.leaf)),
        tool_calls: vec![AgenticAssistantToolCallRecord {
            id: tool_call.id,
            name: tool_call.name,
            arguments: tool_call.arguments,
        }],
        finish_reason: Some("tool_calls".into()),
        prompt_tokens: None,
        completion_tokens: None,
        reasoning: None,
        timestamp: Utc::now(),
    };
    DecideOutput {
        decision_result: Ok(envelope.decision),
        task_state_action_for_decision: envelope.task_state_action,
        assistant_turn_for_decision: Some(turn),
        response_id_for_decision: None,
        has_images_for_decision: false,
        llm_trace_context_for_decision: Some(trace),
        decision_is_synthetic_stuck: false,
    }
}

fn is_single_pack_call(decision: &Decision, leaf: &str) -> bool {
    matches!(
        decision,
        Decision::Execute { candidates, .. }
            if candidates.candidates.len() == 1
                && matches!(
                    &candidates.candidates[0].action,
                    ExecutableAction::Pack { capability_name, .. } if capability_name == leaf
                )
    )
}

/// The terminal a settled call ends the run with. A success's raw output is
/// the verified task deliverable, which is also what keeps V3 output
/// finalization from calling a model to restate it; an empty one still has to
/// say something a terminal evidence check can accept.
fn settled_terminal(call: &PreDecidedPackCall, outcome: PreDecidedOutcome) -> Decision {
    if !outcome.success {
        return Decision::Failed {
            reason: outcome
                .failure_reason
                .unwrap_or_else(|| "the tool call failed".to_string()),
        };
    }
    let body = outcome.raw_output.unwrap_or_default();
    let body = if body.trim().is_empty() {
        format!("`{}` completed with no output.", call.leaf)
    } else {
        body
    };
    Decision::Completed {
        evidence: Some(body.clone()),
        artifacts: vec![Artifact::task_deliverable(body)],
    }
}

/// A synthesized terminal. No assistant turn: no call was made, and the
/// settled tool result is already in history.
fn terminal_output(decision: Decision, trace: magicllm::LlmTraceContext) -> DecideOutput {
    DecideOutput {
        decision_result: Ok(decision),
        task_state_action_for_decision: TaskStateActionEnvelope::default(),
        assistant_turn_for_decision: None,
        response_id_for_decision: None,
        has_images_for_decision: false,
        llm_trace_context_for_decision: Some(trace),
        decision_is_synthetic_stuck: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::agentic::types::{
        ActionOutcomeCategory, ActionResultRecord, EnvironmentState, IterationRecord,
    };
    use serde_json::json;

    const LEAF: &str = "orders__create";
    const CALL_ID: &str = "chat-predecided-call-1";

    fn predecided_ctx(args: serde_json::Value) -> AgenticContext {
        let mut ctx = AgenticContext::new("Create the order", "Order created");
        ctx.execution_id = Some(format!("predecided-test-{}", uuid::Uuid::new_v4()));
        ctx.predecided_call = Some(PreDecidedPackCall {
            leaf: LEAF.to_string(),
            args,
            tool_call_id: CALL_ID.to_string(),
        });
        ctx
    }

    fn pack_action(leaf: &str) -> ExecutableAction {
        ExecutableAction::Pack {
            capability_name: leaf.to_string(),
            implementation:
                crate::magician_v2::execution::capability::ImplementationType::Compiled {
                    provider_name: leaf.to_string(),
                },
            resolved_params: std::collections::HashMap::new(),
        }
    }

    fn settled_history(result: ActionResultRecord) -> ExecutionHistory {
        let mut history = ExecutionHistory::new();
        history.iterations.push(IterationRecord {
            iteration: 1,
            state_before: EnvironmentState::Uninitialized,
            action: pack_action(LEAF),
            result,
            state_after: EnvironmentState::Uninitialized,
            timestamp: Utc::now(),
            verification: None,
            llm_reasoning: None,
        });
        history
    }

    fn success(output: &str) -> ActionResultRecord {
        ActionResultRecord::new(
            true,
            Some(output.to_string()),
            None,
            3,
            Some(ActionOutcomeCategory::GoalReached),
        )
    }

    fn decide(ctx: &AgenticContext, history: &ExecutionHistory) -> DecideOutput {
        maybe_predecided_decide(ctx, history, 1).expect("a pre-decided call always decides")
    }

    fn deliverable(output: &DecideOutput) -> String {
        let Ok(Decision::Completed {
            evidence,
            artifacts,
        }) = &output.decision_result
        else {
            panic!("expected Completed, got {:?}", output.decision_result);
        };
        let body = String::from_utf8(artifacts[0].data.clone()).unwrap();
        assert_eq!(evidence.as_deref(), Some(body.as_str()));
        assert_eq!(
            artifacts[0].artifact_type.as_deref(),
            Some("task_deliverable")
        );
        body
    }

    fn failure(output: &DecideOutput) -> String {
        let Ok(Decision::Failed { reason }) = &output.decision_result else {
            panic!("expected Failed, got {:?}", output.decision_result);
        };
        reason.clone()
    }

    #[test]
    fn no_predecided_call_defers_to_the_model() {
        let ctx = AgenticContext::new("goal", "criteria");
        assert!(maybe_predecided_decide(&ctx, &ExecutionHistory::new(), 1).is_none());
    }

    #[test]
    fn fresh_history_executes_the_leaf_with_synthetic_identity() {
        let args = json!({"item": "tea", "quantity": 2});
        let ctx = predecided_ctx(args.clone());
        let output = decide(&ctx, &ExecutionHistory::new());

        let Ok(Decision::Execute { candidates, .. }) = &output.decision_result else {
            panic!("expected Execute, got {:?}", output.decision_result);
        };
        assert_eq!(candidates.candidates.len(), 1);
        let candidate = &candidates.candidates[0];
        let ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } = &candidate.action
        else {
            panic!("expected a Pack action, got {:?}", candidate.action);
        };
        assert_eq!(capability_name, LEAF);
        assert_eq!(serde_json::to_value(resolved_params).unwrap(), args);
        assert_eq!(candidate.tool_call_id.as_deref(), Some(CALL_ID));

        let trace = output
            .llm_trace_context_for_decision
            .as_ref()
            .expect("synthetic trace");
        assert!(
            trace.llm_call_id.starts_with("synthetic:predecided:"),
            "{}",
            trace.llm_call_id
        );
        let turn = output
            .assistant_turn_for_decision
            .as_ref()
            .expect("synthetic assistant turn");
        assert_eq!(turn.operation, PREDECIDED_OPERATION);
        assert_eq!(
            turn.llm_trace_context
                .as_ref()
                .map(|t| t.llm_call_id.as_str()),
            Some(trace.llm_call_id.as_str())
        );
        assert_eq!(turn.tool_calls.len(), 1);
        assert_eq!(turn.tool_calls[0].id, CALL_ID);
        assert_eq!(turn.tool_calls[0].name, LEAF);
        assert_eq!(turn.tool_calls[0].arguments, args);
    }

    #[test]
    fn successful_result_completes_with_its_output_as_deliverable() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let output = decide(&ctx, &settled_history(success("{\"order\":1}")));
        assert_eq!(deliverable(&output), "{\"order\":1}");
        assert!(output.assistant_turn_for_decision.is_none());
        assert!(output
            .llm_trace_context_for_decision
            .as_ref()
            .is_some_and(|trace| trace.llm_call_id.starts_with("synthetic:predecided:")));
    }

    #[test]
    fn deliverable_is_the_raw_result_not_the_bounded_history_text() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        // Over the 4 KB history bound: the record's `output` is the typed
        // omission marker the history formatter writes, never the result.
        let raw = format!("{{\"rows\":\"{}\"}}", "x".repeat(6_000));
        let marker = json!({
            "kind": "text_omission",
            "size_bytes": raw.len(),
            "complete_value_included": false,
        })
        .to_string();
        let mut history = settled_history(success(&marker));
        history.predecided_raw_output = Some(raw.clone());
        let body = deliverable(&decide(&ctx, &history));
        assert_eq!(body, raw);
        assert!(!body.contains("text_omission"));
    }

    #[test]
    fn failed_result_fails_with_the_error_text() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let history = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("upstream rejected the order".to_string()),
            3,
            Some(ActionOutcomeCategory::Failed),
        ));
        let output = decide(&ctx, &history);
        assert!(failure(&output).contains("upstream rejected the order"));
        assert!(output.assistant_turn_for_decision.is_none());
    }

    #[test]
    fn single_pre_io_admission_expiry_earns_one_fresh_attempt() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let history = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("admission expired".to_string()),
            0,
            Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo),
        ));
        let output = decide(&ctx, &history);
        assert!(matches!(
            output.decision_result,
            Ok(Decision::Execute { .. })
        ));
    }

    #[test]
    fn settled_after_dispatch_folds_to_the_terminal() {
        // What the dispatching Apply folds in is exactly what the next Decide
        // would have returned — the same deliverable, the same failure — under
        // a trace of its own: the iteration's `Execute` already holds
        // `synthetic:predecided:…`.
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let history = settled_history(success("{\"order\":1}"));
        let folded = settled_terminal_after_dispatch(&ctx, &history, 1)
            .expect("a settled call folds into its Apply");
        assert_eq!(deliverable(&folded), "{\"order\":1}");
        assert_eq!(deliverable(&folded), deliverable(&decide(&ctx, &history)));
        assert!(folded.assistant_turn_for_decision.is_none());
        assert!(!folded.decision_is_synthetic_stuck);
        let folded_call = folded
            .llm_trace_context_for_decision
            .as_ref()
            .expect("synthetic trace")
            .llm_call_id
            .clone();
        assert!(
            folded_call.starts_with("synthetic:predecided_settled:"),
            "{folded_call}"
        );
        let execute_call = decide(&ctx, &ExecutionHistory::new())
            .llm_trace_context_for_decision
            .expect("synthetic trace")
            .llm_call_id;
        assert_ne!(
            folded_call, execute_call,
            "the folded terminal and the call it consumes must not share a call id"
        );

        let failed = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("upstream rejected the order".to_string()),
            3,
            Some(ActionOutcomeCategory::Failed),
        ));
        let folded =
            settled_terminal_after_dispatch(&ctx, &failed, 1).expect("a settled failure folds too");
        assert!(failure(&folded).contains("upstream rejected the order"));
    }

    #[test]
    fn retry_admission_does_not_fold() {
        // A single proven pre-I/O admission expiry keeps its one fresh attempt,
        // which the next iteration's Decide re-emits.
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let history = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("admission expired".to_string()),
            0,
            Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo),
        ));
        assert!(settled_terminal_after_dispatch(&ctx, &history, 1).is_none());
        assert!(matches!(
            decide(&ctx, &history).decision_result,
            Ok(Decision::Execute { .. })
        ));
    }

    #[test]
    fn unsettled_does_not_fold() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        assert!(
            settled_terminal_after_dispatch(&ctx, &ExecutionHistory::new(), 1).is_none(),
            "nothing landed, so there is no terminal to synthesize"
        );
        let mut unrelated = settled_history(success("{\"rows\":[]}"));
        unrelated.iterations[0].action = pack_action("shell");
        assert!(
            settled_terminal_after_dispatch(&ctx, &unrelated, 1).is_none(),
            "another tool's result does not settle the pre-decided call"
        );
        let model_run = AgenticContext::new("goal", "criteria");
        assert!(
            settled_terminal_after_dispatch(&model_run, &settled_history(success("x")), 1)
                .is_none(),
            "a run without a pre-decided call never folds"
        );
    }

    fn test_executors() -> ActionExecutors {
        use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
        ActionExecutors::new(
            std::sync::Arc::new(
                crate::magician_v2::test_utils::ConfigurableMockLlm::with_response("{}"),
            ),
            std::sync::Arc::new(PromptManager::new(std::sync::Arc::new(
                JsonPromptStorage::with_default_config().unwrap(),
            ))),
        )
    }

    #[test]
    fn terminal_to_fold_folds_only_a_settled_call_after_a_next_iteration_dispatch() {
        let executors = test_executors();
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let settled = settled_history(success("{\"order\":1}"));
        let next = PhaseStep::Exit(BoundaryOutcome::NextIteration);
        let uncancelled: Option<CancellationToken> = None;

        let folded = terminal_to_fold(&next, &ctx, &executors, &settled, &uncancelled, 1)
            .expect("a settled call after a NextIteration dispatch folds");
        assert_eq!(deliverable(&folded), "{\"order\":1}");

        // Not the dispatch's ordinary boundary: nothing to fold.
        let advance = PhaseStep::Exit(BoundaryOutcome::Advance);
        assert!(terminal_to_fold(&advance, &ctx, &executors, &settled, &uncancelled, 1).is_none());

        // A single pre-I/O admission expiry keeps its fresh attempt.
        let expired = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("admission expired".to_string()),
            0,
            Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo),
        ));
        assert!(terminal_to_fold(&next, &ctx, &executors, &expired, &uncancelled, 1).is_none());

        // A local cancel is the next phase entry's to conclude.
        let cancelled = Some(CancellationToken::new());
        cancelled.as_ref().unwrap().cancel();
        assert!(terminal_to_fold(&next, &ctx, &executors, &settled, &cancelled, 1).is_none());

        // An owner to yield back to: its terminal can fail after the
        // task-state commit, so it keeps its second iteration.
        let mut delegated = ctx.clone();
        delegated.owner_stack = vec!["personal-assistant".to_string()];
        assert!(
            terminal_to_fold(&next, &delegated, &executors, &settled, &uncancelled, 1).is_none()
        );
    }

    #[test]
    fn a_rejected_terminal_fails_at_once_instead_of_repeating() {
        let ctx = predecided_ctx(json!({"item": "tea"}));
        let mut history = settled_history(success("{\"order\":1}"));
        // What `Apply` records when it refuses the synthesized `Completed`.
        let mut strikes = 0usize;
        crate::magician_v2::execution::agentic::executor::handle_terminal_evidence_rejection(
            &mut history,
            &EnvironmentState::Uninitialized,
            2,
            "Proposed terminal task-state transition was rejected without mutation: stale",
            None,
            &mut strikes,
        );
        assert_eq!(strikes, 1);
        let reason = failure(&decide(&ctx, &history));
        assert!(
            reason.contains("rejected without mutation: stale"),
            "{reason}"
        );
    }

    #[test]
    fn a_carried_outcome_is_never_executed_again() {
        // A resumed segment's history starts empty; the outcome it carried in
        // is what it ends with.
        let mut ctx = predecided_ctx(json!({"item": "tea"}));
        ctx.predecided_settled = Some(PreDecidedOutcome {
            success: true,
            raw_output: Some("{\"order\":7}".to_string()),
            failure_reason: None,
        });
        assert_eq!(
            deliverable(&decide(&ctx, &ExecutionHistory::new())),
            "{\"order\":7}"
        );

        ctx.predecided_settled = Some(PreDecidedOutcome {
            success: false,
            raw_output: None,
            failure_reason: Some("quota exhausted".to_string()),
        });
        assert_eq!(
            failure(&decide(&ctx, &ExecutionHistory::new())),
            "quota exhausted"
        );
    }

    #[test]
    fn redeciding_after_a_pause_re_emits_the_identical_action() {
        // Several parameters: `resolved_params` is a `HashMap`, so each
        // re-emission orders its keys per instance. The approval waiver
        // compares the stable form, which must be byte-identical.
        let stable = |output: &DecideOutput| {
            let Ok(Decision::Execute { candidates, .. }) = &output.decision_result else {
                panic!("expected Execute, got {:?}", output.decision_result);
            };
            crate::magician_v2::execution::agentic::stable_confirmation_action_json(
                &candidates.candidates[0].action,
            )
            .unwrap()
        };
        let ctx =
            predecided_ctx(json!({"item": "tea", "quantity": 2, "note": "hot", "rush": true}));
        let first = decide(&ctx, &ExecutionHistory::new());
        let resumed_call: PreDecidedPackCall = serde_json::from_str(
            &serde_json::to_string(ctx.predecided_call.as_ref().unwrap()).unwrap(),
        )
        .unwrap();
        let mut resumed = ctx.clone();
        resumed.predecided_call = Some(resumed_call);
        let second = decide(&resumed, &ExecutionHistory::new());
        assert_eq!(stable(&first), stable(&second));
        assert_eq!(
            first
                .assistant_turn_for_decision
                .as_ref()
                .unwrap()
                .tool_calls,
            second
                .assistant_turn_for_decision
                .as_ref()
                .unwrap()
                .tool_calls
        );
    }

    /// The real pause path: `build_full_pause_state` (which runs
    /// `enrich_pause_state_from_context`) → JSON → `restore_context_from_pause`
    /// → decide. Before the leaf ran the resumed run re-emits it; after it
    /// settled the resumed run ends with its outcome and never re-executes.
    #[test]
    fn pause_round_trip_through_the_real_pause_functions() {
        use crate::magician_v2::execution::agentic::executor::{
            build_full_pause_state, restore_context_from_pause,
        };
        use crate::magician_v2::execution::agentic::types::LoopProtectiveState;

        let round_trip = |ctx: &AgenticContext, history: &ExecutionHistory| {
            let pause = build_full_pause_state(
                1,
                ctx,
                &EnvironmentState::Uninitialized,
                history,
                &LoopProtectiveState::default(),
                &[],
            );
            let encoded = serde_json::to_string(&pause).expect("serialize pause");
            let decoded: crate::magician_v2::execution::agentic::AgenticPauseState =
                serde_json::from_str(&encoded).expect("deserialize pause");
            restore_context_from_pause(
                &decoded,
                &decoded.goal,
                &decoded.success_criteria,
                decoded.max_iterations,
            )
        };
        let ctx = predecided_ctx(json!({"item": "tea", "quantity": 2}));

        // Before the leaf ran (an approval pause): the call survives and is
        // re-emitted, nothing is settled.
        let resumed = round_trip(&ctx, &ExecutionHistory::new());
        assert_eq!(resumed.predecided_call, ctx.predecided_call);
        assert!(resumed.predecided_settled.is_none());
        assert!(matches!(
            decide(&resumed, &ExecutionHistory::new()).decision_result,
            Ok(Decision::Execute { .. })
        ));

        // After it settled: the resumed run (empty history) ends with the
        // carried raw output.
        let mut history = settled_history(success("bounded history text"));
        history.predecided_raw_output = Some("{\"order\":1}".to_string());
        let resumed = round_trip(&ctx, &history);
        assert!(resumed.predecided_settled.is_some());
        assert_eq!(
            deliverable(&decide(&resumed, &ExecutionHistory::new())),
            "{\"order\":1}"
        );

        // A pre-I/O admission expiry is not settled: still retryable.
        let expired = settled_history(ActionResultRecord::new(
            false,
            None,
            Some("admission expired".to_string()),
            0,
            Some(ActionOutcomeCategory::AdmissionExpiredBeforeIo),
        ));
        let resumed = round_trip(&ctx, &expired);
        assert!(resumed.predecided_settled.is_none());
        assert!(matches!(
            decide(&resumed, &ExecutionHistory::new()).decision_result,
            Ok(Decision::Execute { .. })
        ));
    }

    #[test]
    fn a_leaf_that_lowers_to_a_control_call_fails_instead_of_running_it() {
        let mut ctx = predecided_ctx(json!({}));
        ctx.predecided_call.as_mut().unwrap().leaf = "yield".to_string();
        let output = decide(&ctx, &ExecutionHistory::new());
        assert!(
            matches!(output.decision_result, Ok(Decision::Failed { .. })),
            "{:?}",
            output.decision_result
        );
    }

    fn tool(name: &str) -> runtime_core::ToolInfo {
        runtime_core::ToolInfo {
            name: name.to_string(),
            description: format!("{name} tool"),
            category: "custom".to_string(),
            categories: Vec::new(),
            parameters: Vec::new(),
            enhanced_description: None,
            keywords: Vec::new(),
            use_cases: Vec::new(),
            composition_category: None,
            providing_agent_id: None,
        }
    }

    /// An owner-scoped context whose pre-decided call is the `orders` pack.
    /// `owner_tools` is the owner definition's tool list and also its merged
    /// direct tools; `orders` is never in the always-hot set, so when the
    /// owner holds it, it lands in the deferred tier.
    fn owner_ctx(owner_tools: &[&str]) -> AgenticContext {
        use crate::magician_v2::execution::capability::CapabilityPackDefinition;
        use crate::magician_v2::execution::flat_loop::build_tool_index;

        let packs = [
            "name: tool_search\ndescription: search tools\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: tool_search\n",
            "name: shell\ndescription: run shell\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: shell\n",
            "name: orders\ndescription: place orders\nparameters: []\nimplementation:\n  type: compiled\n  provider_name: orders\n",
        ]
        .into_iter()
        .map(|yaml| serde_yaml::from_str::<CapabilityPackDefinition>(yaml).expect("pack"))
        .collect::<Vec<_>>();
        let tools_yaml = owner_tools
            .iter()
            .map(|name| format!("  - {name}\n"))
            .collect::<String>();
        let definition = crate::magician_v2::agents::AgentDefinition::from_yaml_str(&format!(
            "agent_id: worker\nname: Worker\npersona: Test\ntrust_level: local\ntools:\n{tools_yaml}"
        ))
        .expect("definition");
        let mut ctx = predecided_ctx(json!({}));
        ctx.predecided_call.as_mut().unwrap().leaf = "orders".to_string();
        ctx.principal = Some("owner".to_string());
        ctx.workspace = Some("default".to_string());
        ctx.agent_id = Some("worker".to_string());
        ctx.active_owner_agent_id = Some("worker".to_string());
        ctx.owner_definition = Some(std::sync::Arc::new(definition));
        ctx.merged_agent_tools = owner_tools.iter().map(|name| tool(name)).collect();
        ctx.tool_index = Some(std::sync::Arc::new(build_tool_index(&packs)));
        ctx
    }

    fn executed_action(output: &DecideOutput) -> ExecutableAction {
        let Ok(Decision::Execute { candidates, .. }) = &output.decision_result else {
            panic!("expected Execute, got {:?}", output.decision_result);
        };
        candidates.candidates[0].action.clone()
    }

    /// An owner whose policy does not include the leaf: the policy projection
    /// leaves it out of the dispatch snapshot (promotion cannot load what the
    /// owner is not authorized for), `Apply`'s snapshot check refuses it, and
    /// the next decide fails the run. It is never dispatched.
    #[tokio::test]
    async fn a_leaf_outside_the_owner_policy_is_refused_before_dispatch() {
        let ctx = owner_ctx(&["tool_search", "shell"]);
        let authorized = authorize(&ctx, 1, decide(&ctx, &ExecutionHistory::new())).await;
        let action = executed_action(&authorized);
        assert_eq!(
            dispatch_snapshot_lacks(&ctx, "orders"),
            Some(true),
            "the owner's snapshot must be active and must not list the leaf"
        );
        let refusal =
            crate::magician_v2::execution::agentic::executor::validate_action_against_allowlists(
                &action, &ctx,
            )
            .expect("Apply's snapshot check refuses the leaf");
        assert!(refusal.contains("not callable"), "{refusal}");

        // What Apply's allowlist refusal records; the run then fails.
        let mut history = settled_history(ActionResultRecord::new(
            false,
            Some(refusal),
            Some("ActionNotAllowed".to_string()),
            0,
            Some(ActionOutcomeCategory::Failed),
        ));
        history.iterations[0].action = pack_action("orders");
        let reason = failure(&decide(&ctx, &history));
        assert!(reason.contains("not callable"), "{reason}");
    }

    /// An authorized leaf in the deferred tier is promoted the way a
    /// `tool_search` selection promotes it, so the snapshot lists it and
    /// Apply's allowlist passes it.
    #[tokio::test]
    async fn an_authorized_deferred_leaf_is_promoted_and_passes_apply() {
        let ctx = owner_ctx(&["tool_search", "shell", "orders"]);
        // Fixture check: without promotion the leaf is deferred, not callable.
        let _ = crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog(
            &ctx, 1,
        )
        .await;
        assert_eq!(
            dispatch_snapshot_lacks(&ctx, "orders"),
            Some(true),
            "fixture: `orders` must start in the deferred tier"
        );

        let authorized = authorize(&ctx, 1, decide(&ctx, &ExecutionHistory::new())).await;
        let action = executed_action(&authorized);
        assert_eq!(dispatch_snapshot_lacks(&ctx, "orders"), Some(false));
        assert!(
            crate::magician_v2::execution::agentic::native_integration::snapshot_loaded_tools(&ctx)
                .iter()
                .any(|name| name == "orders"),
            "the leaf is in the loaded working set"
        );
        assert_eq!(
            crate::magician_v2::execution::agentic::executor::validate_action_against_allowlists(
                &action, &ctx,
            ),
            None
        );
    }

    /// An owner whose policy cannot be projected (here: a trust level with no
    /// readable policy) fails the run instead of falling back to the legacy
    /// detached checks.
    #[tokio::test]
    async fn an_unresolvable_owner_policy_fails_the_call() {
        let mut ctx = owner_ctx(&["tool_search", "shell", "orders"]);
        ctx.trust_level = Some("reviewed".to_string());
        ctx.trust_policies_path = None;
        let authorized = authorize(&ctx, 1, decide(&ctx, &ExecutionHistory::new())).await;
        assert_eq!(dispatch_snapshot_lacks(&ctx, "orders"), None);
        let reason = failure(&authorized);
        assert!(reason.contains("could not be resolved"), "{reason}");
    }

    /// The pause / continuation checkpoint carries a bounded form of a large
    /// raw output (the checkpoint has a hard size cap), and the resumed run's
    /// terminal still reads as a well-formed deliverable.
    #[test]
    fn a_large_raw_output_is_bounded_in_the_checkpoint() {
        use crate::magician_v2::execution::agentic::executor::{
            build_full_pause_state, restore_context_from_pause,
        };
        use crate::magician_v2::execution::agentic::types::{
            LoopProtectiveState, PREDECIDED_CHECKPOINT_RAW_MAX_BYTES,
        };

        let ctx = predecided_ctx(json!({"item": "tea"}));
        let raw = "r".repeat(100 * 1024);
        let mut history = settled_history(success("bounded history text"));
        history.predecided_raw_output = Some(raw.clone());

        // Same process: the terminal reads the whole raw text.
        assert_eq!(deliverable(&decide(&ctx, &history)), raw);

        let pause = build_full_pause_state(
            1,
            &ctx,
            &EnvironmentState::Uninitialized,
            &history,
            &LoopProtectiveState::default(),
            &[],
        );
        let carried = pause
            .predecided_settled
            .as_ref()
            .and_then(|outcome| outcome.raw_output.clone())
            .expect("a settled success carries its output");
        assert!(
            carried.len() <= PREDECIDED_CHECKPOINT_RAW_MAX_BYTES,
            "{}",
            carried.len()
        );
        assert!(carried.starts_with("rrrr"));
        assert!(carried.contains("Truncated for the checkpoint"));

        let decoded: crate::magician_v2::execution::agentic::AgenticPauseState =
            serde_json::from_str(&serde_json::to_string(&pause).unwrap()).unwrap();
        let resumed = restore_context_from_pause(
            &decoded,
            &decoded.goal,
            &decoded.success_criteria,
            decoded.max_iterations,
        );
        let body = deliverable(&decide(&resumed, &ExecutionHistory::new()));
        assert_eq!(body, carried);
    }
}

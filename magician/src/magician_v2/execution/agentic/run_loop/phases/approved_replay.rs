//! Approved-call replay — the first `Decide` after an approving resume.
//!
//! A confirmation pause holds one gated action. When the owner approves it,
//! the run resumes and `Decide` runs again; `Apply` waives the approval only
//! when the re-emitted action serializes exactly like the approved one
//! (`take_matching_approved_confirmation`). Asking the model to re-emit a call
//! byte for byte is fragile: a tool whose arguments carry prose (a full
//! program document and its rationale, a message body) comes back reworded,
//! the match fails, and the owner is asked again — every resume, without end.
//!
//! So the pause records the model's tool call behind the gated action
//! (`AgenticPauseState::confirmation_replay_call`, kept only when that call
//! lowers to exactly the gated action), the approving resume attaches it to
//! the approval (`ApprovedConfirmationAction::replay_call_json`), and the first
//! `Decide` returns that call instead of asking the model. It takes the same
//! lowering a model's native call takes and the same owner policy projection a
//! model turn makes, so `Resolve`, `Apply` (policy snapshot, trust, approval
//! rules, the approval waiver itself, allowlist, lineage, effect ids) and
//! `Observe` run unchanged. The model decides every later iteration, seeing
//! the call and its result in its transcript.
//!
//! A replay is one-shot (`LoopProtectiveState::approved_replays_attempted`):
//! if `Apply` does not consume the approval — a policy that changed, an action
//! that no longer lowers to the approved one — the next `Decide` asks the
//! model as before. Anything that does not lower to exactly the approved
//! action is never replayed; the run falls back to the model.

use chrono::Utc;
use tracing::{info, warn};

use super::decide::DecideOutput;
use crate::magician_v2::execution::agentic::decision::Decision;
use crate::magician_v2::execution::agentic::executor::{
    promote_predecided_leaf, response_kind_from_decision, runtime_execution_id,
    stable_confirmation_action_json, transport_scope,
};
use crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call;
use crate::magician_v2::execution::agentic::native_types::{
    ExecutionToolCall, NativeDecisionOutcome,
};
use crate::magician_v2::execution::agentic::run_loop::outcome::Phase;
use crate::magician_v2::execution::agentic::types::{
    action_type_and_tool_name, AgenticAssistantToolCallRecord, AgenticAssistantTurnRecord,
    AgenticContext, ApprovedConfirmationAction, LoopProtectiveState,
};
use crate::magician_v2::execution::agentic::ActionExecutors;
use crate::magician_v2::RuntimeTransportEvent;

/// `synthetic:{kind}:…` trace kind for a replayed approved call.
const APPROVED_REPLAY_TRACE_KIND: &str = "approved_replay";
/// Operation recorded on the synthetic assistant turn.
const APPROVED_REPLAY_OPERATION: &str = "agentic_decision_approved_replay";
/// How many replayed approvals a run remembers. Each entry is consumed by one
/// approval, so a run only exceeds this by approving that many gated calls;
/// the oldest are dropped first.
pub(in crate::magician_v2::execution::agentic) const MAX_APPROVED_REPLAYS_REMEMBERED: usize = 32;

/// The approved call to run for this iteration, or `None` when there is no
/// approval waiting to be replayed (or it cannot be replayed exactly) and the
/// model must decide.
pub(super) async fn maybe_replay_approved(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    approved_confirmation_actions: &[ApprovedConfirmationAction],
    loop_protective: &mut LoopProtectiveState,
    pending_operator_steer: &[String],
    iteration: usize,
) -> Option<DecideOutput> {
    let (approved, raw_call) = take_pending_replay(
        approved_confirmation_actions,
        loop_protective,
        pending_operator_steer,
    )?;

    let call: AgenticAssistantToolCallRecord = match serde_json::from_str(raw_call) {
        Ok(call) => call,
        Err(error) => {
            warn!(%error, "[APPROVED-REPLAY] unreadable approved call; the model decides");
            return None;
        },
    };
    let output = replay_output(ctx, &call, &approved.action_json, iteration)?;
    // Project the owner's effective tool policy exactly as a model turn's
    // `Decide` does, so `Apply` checks the dispatch against the same snapshot.
    // A leaf that was promoted by a `tool_search` before the pause is promoted
    // again; one outside the owner's policy stays out and `Apply` refuses it.
    let _ = crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog(
        ctx, iteration,
    )
    .await;
    if super::predecided_decision::dispatch_snapshot_lacks(ctx, &call.name) == Some(true) {
        promote_predecided_leaf(ctx, &call.name);
        let _ = crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog(
            ctx, iteration,
        )
        .await;
    }
    info!(
        tool = %call.name,
        iteration,
        "[APPROVED-REPLAY] running the call the owner approved"
    );
    emit_decision_made(ctx, executors, iteration, &call, &output);
    Some(output)
}

/// The first approval that carries a replayable call this run has not replayed
/// yet, with that call's JSON.
fn pending_replay<'a>(
    approved_confirmation_actions: &'a [ApprovedConfirmationAction],
    loop_protective: &LoopProtectiveState,
) -> Option<(&'a ApprovedConfirmationAction, &'a str)> {
    approved_confirmation_actions.iter().find_map(|approved| {
        let call = approved.replay_call_json.as_deref()?;
        (!loop_protective
            .approved_replays_attempted
            .contains(&approved.action_json))
        .then_some((approved, call))
    })
}

fn take_pending_replay<'a>(
    approvals: &'a [ApprovedConfirmationAction],
    protective: &mut LoopProtectiveState,
    steer: &[String],
) -> Option<(&'a ApprovedConfirmationAction, &'a str)> {
    let selected = pending_replay(approvals, protective)?;
    let steered = steer.iter().any(|message| !message.trim().is_empty());
    // New guidance must reach a model before an approved action can run.
    // Retire the shortcut, including other pending replays, so it cannot fire
    // a turn later after Resolve clears the steer. The actual approval receipts
    // remain subject to Apply's exact-action matching and normal policy gates.
    if steered {
        for approval in approvals {
            if approval.replay_call_json.is_some()
                && !protective
                    .approved_replays_attempted
                    .contains(&approval.action_json)
            {
                protective
                    .approved_replays_attempted
                    .push(approval.action_json.clone());
            }
        }
    } else {
        protective
            .approved_replays_attempted
            .push(selected.0.action_json.clone());
    }
    let excess = protective
        .approved_replays_attempted
        .len()
        .saturating_sub(MAX_APPROVED_REPLAYS_REMEMBERED);
    protective.approved_replays_attempted.drain(..excess);
    (!steered).then_some(selected)
}

/// Lower the approved call and keep it only when it is exactly the approved
/// action. The synthetic assistant turn carries the call under its own id, so
/// the transcript pairs it with its result and the effect ledger mints a fresh
/// id for it.
fn replay_output(
    ctx: &AgenticContext,
    call: &AgenticAssistantToolCallRecord,
    approved_action_json: &str,
    iteration: usize,
) -> Option<DecideOutput> {
    let tool_call = ExecutionToolCall {
        id: replay_call_id(&call.id),
        name: call.name.clone(),
        arguments: call.arguments.clone(),
    };
    let NativeDecisionOutcome::Valid(envelope) = lower_native_tool_call(&tool_call) else {
        warn!(tool = %call.name, "[APPROVED-REPLAY] the approved call no longer lowers; the model decides");
        return None;
    };
    // An alignment hold's approval is a receipt: a purpose prefix on the
    // same canonical action token.
    let approved_canonical =
        crate::magician_v2::preference_alignment::receipts::canonical_of(approved_action_json)
            .unwrap_or(approved_action_json);
    let exact = match &envelope.decision {
        Decision::Execute { candidates, .. } => {
            candidates.candidates.len() == 1
                && stable_confirmation_action_json(&candidates.candidates[0].action)
                    .is_ok_and(|json| json == approved_canonical)
        },
        _ => false,
    };
    if !exact {
        warn!(tool = %call.name, "[APPROVED-REPLAY] the call does not lower to the approved action; the model decides");
        return None;
    }
    let trace = super::decision_rail_events::synthetic_trace_context(
        ctx,
        APPROVED_REPLAY_TRACE_KIND,
        iteration,
    );
    let turn = AgenticAssistantTurnRecord {
        iteration: 0,
        operation: APPROVED_REPLAY_OPERATION.into(),
        llm_trace_context: Some(trace.clone()),
        text: Some(format!(
            "Running the call the owner approved: {}",
            call.name
        )),
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
    Some(DecideOutput {
        decision_result: Ok(envelope.decision),
        task_state_action_for_decision: envelope.task_state_action,
        assistant_turn_for_decision: Some(turn),
        response_id_for_decision: None,
        has_images_for_decision: false,
        llm_trace_context_for_decision: Some(trace),
        decision_is_synthetic_stuck: false,
    })
}

/// The replayed call's id. Distinct from the paused call's, which stays
/// unanswered in the transcript exactly as it did before replay existed;
/// deterministic, so a recovered segment re-mints the same effect id.
fn replay_call_id(original: &str) -> String {
    if original.is_empty() {
        "approved".to_string()
    } else {
        format!("{original}-approved")
    }
}

/// Journal and emit `AgenticDecisionMade` for the replayed call, so the run's
/// decision trail shows the turn no model made.
fn emit_decision_made(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    call: &AgenticAssistantToolCallRecord,
    output: &DecideOutput,
) {
    let Ok(decision) = output.decision_result.as_ref() else {
        return;
    };
    if !ctx.has_observability() {
        return;
    }
    let (principal, workspace) = transport_scope(ctx);
    let (action_type, candidates_count) = match decision {
        Decision::Execute { candidates, .. } => (
            candidates
                .candidates
                .first()
                .map(|candidate| action_type_and_tool_name(&candidate.action).0),
            Some(candidates.candidates.len()),
        ),
        _ => (None, None),
    };
    let event = RuntimeTransportEvent::AgenticDecisionMade {
        execution_id: runtime_execution_id(ctx),
        principal,
        workspace,
        plan_id: ctx.plan_id.clone().unwrap_or_default(),
        step_id: ctx.step_id.clone().unwrap_or_default(),
        iteration,
        decision_type: response_kind_from_decision(decision).into(),
        action_summary: Some(call.name.clone()),
        reasoning: format!("decision:{APPROVED_REPLAY_TRACE_KIND}:owner-approved"),
        confidence: 1.0,
        thinking: Some("Replaying the call the owner approved (no model)".to_string()),
        evidence: None,
        tool_name: Some(call.name.clone()),
        action_type,
        element_id: None,
        candidates_count,
        raw_decision: Some(serde_json::json!({
            "operation": APPROVED_REPLAY_OPERATION,
            "tool": call.name,
            "replayed_tool_call_id": call.id,
        })),
        timestamp: Utc::now().timestamp_millis(),
    };
    super::outbox::journal_and_emit(ctx, executors, iteration, Phase::Decide, event);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::execution::actions::ExecutableAction;
    use serde_json::json;

    const LEAF: &str = "orders__create";

    #[test]
    fn operator_steer_retires_approval_replay_so_the_model_decides_first() {
        let call = call(json!({"quantity": 1}));
        let approvals = vec![approval(lowered_action_json(&call), Some(&call))];
        let mut protective = LoopProtectiveState::default();
        assert!(take_pending_replay(
            &approvals,
            &mut protective,
            &["Wait; change the quantity".to_owned()]
        )
        .is_none());
        assert!(
            take_pending_replay(&approvals, &mut protective, &[]).is_none(),
            "the obsolete shortcut must not replay after the steer is consumed"
        );
        assert!(
            approvals[0].replay_call_json.is_some(),
            "normal exact-action approval remains intact"
        );

        let mut protective = LoopProtectiveState::default();
        assert!(take_pending_replay(&approvals, &mut protective, &[]).is_some());
        assert!(take_pending_replay(&approvals, &mut protective, &[]).is_none());
    }

    fn call(arguments: serde_json::Value) -> AgenticAssistantToolCallRecord {
        AgenticAssistantToolCallRecord {
            id: "call_1".into(),
            name: LEAF.into(),
            arguments,
        }
    }

    fn lowered_action_json(call: &AgenticAssistantToolCallRecord) -> String {
        let NativeDecisionOutcome::Valid(envelope) = lower_native_tool_call(&ExecutionToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
        }) else {
            panic!("the test call lowers");
        };
        let Decision::Execute { candidates, .. } = envelope.decision else {
            panic!("the test call executes");
        };
        stable_confirmation_action_json(&candidates.candidates[0].action).unwrap()
    }

    fn approval(
        action_json: String,
        call: Option<&AgenticAssistantToolCallRecord>,
    ) -> ApprovedConfirmationAction {
        let ctx = AgenticContext::new("goal", "done");
        ApprovedConfirmationAction {
            action_json,
            owner_snapshot: ctx.current_owner_snapshot().unwrap_or_else(|| {
                crate::magician_v2::execution::agentic::types::OwnerSnapshot {
                    active_owner_agent_id: "agent".into(),
                    owner_stack: Vec::new(),
                }
            }),
            replay_call_json: call.map(|call| serde_json::to_string(call).unwrap()),
        }
    }

    #[test]
    fn the_approved_call_is_replayed_once_as_exactly_the_approved_action() {
        let ctx = AgenticContext::new("Create the order", "Order created");
        let call = call(json!({ "sku": "A-1", "note": "the owner read this" }));
        let action_json = lowered_action_json(&call);
        let approved = vec![approval(action_json.clone(), Some(&call))];
        let mut protective = LoopProtectiveState::default();

        let (pending, raw) = pending_replay(&approved, &protective).expect("a replay is pending");
        let replayed: AgenticAssistantToolCallRecord = serde_json::from_str(raw).unwrap();
        let output = replay_output(&ctx, &replayed, &pending.action_json, 1).expect("replays");
        let Ok(Decision::Execute { candidates, .. }) = &output.decision_result else {
            panic!("an approved replay executes");
        };
        assert_eq!(candidates.candidates.len(), 1);
        assert_eq!(
            stable_confirmation_action_json(&candidates.candidates[0].action).unwrap(),
            action_json,
            "the replay is the approved action, byte for byte"
        );
        let turn = output
            .assistant_turn_for_decision
            .expect("a synthetic turn");
        assert_eq!(turn.operation, APPROVED_REPLAY_OPERATION);
        assert_eq!(turn.tool_calls[0].id, "call_1-approved");
        assert_eq!(turn.tool_calls[0].arguments, call.arguments);
        assert!(matches!(
            &candidates.candidates[0].action,
            ExecutableAction::Pack { capability_name, .. } if capability_name == LEAF
        ));

        protective.approved_replays_attempted.push(action_json);
        assert!(
            pending_replay(&approved, &protective).is_none(),
            "a replay is one-shot"
        );
    }

    #[test]
    fn an_approval_without_a_call_or_with_a_different_call_is_not_replayed() {
        let ctx = AgenticContext::new("Create the order", "Order created");
        let approved_call = call(json!({ "sku": "A-1" }));
        let action_json = lowered_action_json(&approved_call);
        let protective = LoopProtectiveState::default();

        // No recorded call: the model decides, as before.
        assert!(pending_replay(&[approval(action_json.clone(), None)], &protective).is_none());

        // A recorded call that does not lower to the approved action is never
        // run in its place.
        let other = call(json!({ "sku": "B-2" }));
        assert!(replay_output(&ctx, &other, &action_json, 1).is_none());
    }

    #[test]
    fn an_alignment_receipt_replays_its_canonical_action() {
        use crate::magician_v2::preference_alignment::receipts::{
            receipt_token, AlignmentReceiptPurpose,
        };
        let ctx = AgenticContext::new("Create the order", "Order created");
        let approved_call = call(json!({ "sku": "A-1" }));
        let action_json = lowered_action_json(&approved_call);
        for purpose in [
            AlignmentReceiptPurpose::Exception,
            AlignmentReceiptPurpose::CheckWaiver,
        ] {
            let receipt = receipt_token(purpose, &action_json).expect("receipt");
            assert!(
                replay_output(&ctx, &approved_call, &receipt, 1).is_some(),
                "an owner-consented alignment hold replays like a plain approval"
            );
            let other = call(json!({ "sku": "B-2" }));
            assert!(replay_output(&ctx, &other, &receipt, 1).is_none());
        }
    }

    #[test]
    fn replayed_call_ids_are_distinct_and_deterministic() {
        assert_eq!(replay_call_id("toolu_9"), "toolu_9-approved");
        assert_eq!(replay_call_id("toolu_9"), replay_call_id("toolu_9"));
        assert_eq!(replay_call_id(""), "approved");
    }
}

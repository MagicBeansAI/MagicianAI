//! One action-selection policy for every tool and every host engine.

use std::collections::BTreeMap;
use std::time::Instant;

use decision_engine_contract::action::{
    ActionOrigin, ActionPhase, ActionPlan, ActionRequest, ActionResponse, ActionVerdict,
    ACTION_OPERATION,
};
use decision_engine_contract::wire::CONTRACT_VERSION;
use serde_json::json;
use sha2::{Digest, Sha256};

fn scope(request: &ActionRequest) -> String {
    let bytes = serde_json::to_vec(&(
        &request.context.goal,
        &request.context.success_criteria,
        &request.context.instructions,
        request.context.planner_mode,
    ))
    .expect("strings serialize");
    format!("{:x}", Sha256::digest(bytes))
}

use super::{candidates, planner, validation};
use crate::config::DecisionOperationConfig;
use crate::{set_choice_candidates, DecisionRuntime, DecisionState, OptionId, QuestionId};

fn number(response: &crate::DecisionResponse, key: &str) -> Option<f64> {
    response
        .answer(&QuestionId::new(key))
        .and_then(|answer| answer.noul_value())
}

fn threshold(thresholds: &BTreeMap<String, f64>, key: &str, default: f64) -> f64 {
    thresholds.get(key).copied().unwrap_or(default)
}

fn finite_probability(value: Option<f64>, minimum: f64) -> bool {
    minimum.is_finite()
        && (0.0..=1.0).contains(&minimum)
        && value.is_some_and(|v| v.is_finite() && (0.0..=1.0).contains(&v) && v >= minimum)
}

pub async fn decide(
    request: ActionRequest,
    operation: Option<&DecisionOperationConfig>,
    runtime: Option<&DecisionRuntime>,
    enabled: bool,
) -> ActionResponse {
    let mut response = decide_inner(request, operation, runtime, enabled).await;
    if enabled && operation.is_some() {
        if let Some(runtime) = runtime {
            response.model_health = runtime.health_for(ACTION_OPERATION);
        }
    }
    response
}

async fn decide_inner(
    request: ActionRequest,
    operation: Option<&DecisionOperationConfig>,
    runtime: Option<&DecisionRuntime>,
    enabled: bool,
) -> ActionResponse {
    let started = Instant::now();
    let mut reply = ActionResponse {
        model_calls: Vec::new(),
        contract_version: CONTRACT_VERSION,
        snapshot: request.snapshot.clone(),
        verdict: ActionVerdict::Rejected {
            reason: "unresolved".into(),
        },
        reason: String::new(),
        model: None,
        review_model: None,
        usage: None,
        latency_ms: 0,
        model_health: Vec::new(),
    };
    if request.contract_version != CONTRACT_VERSION {
        return ActionResponse::rejected(request.snapshot, "contract_mismatch");
    }
    if let Err(reason) = validation::validate_request(&request) {
        return ActionResponse::rejected(request.snapshot, reason);
    }
    let Some(operation) = operation.filter(|_| enabled) else {
        reply.verdict = ActionVerdict::Disabled;
        reply.reason = "operation_disabled".into();
        return reply;
    };
    // A generative planner is consulted only after an explicit escalation.
    // Its first call is validated by the engine and remains subject to the
    // host's normal authority/approval checks. It cannot smuggle a tool name.
    if request.phase == ActionPhase::ResolvePlanner {
        let Some(first) = request.plan.steps.first() else {
            return ActionResponse::rejected(request.snapshot, "planner_returned_no_steps");
        };
        let call = match candidates::materialize(&request, first) {
            Ok(call) => call,
            Err(reason) => return ActionResponse::rejected(request.snapshot, reason),
        };
        reply.verdict = ActionVerdict::Execute {
            candidate_id: first.id.clone(),
            call,
            origin: ActionOrigin::Planner,
            confidence: None,
            continuation: ActionPlan {
                scope: Some(scope(&request)),
                steps: request.plan.steps.iter().skip(1).cloned().collect(),
            },
        };
        reply.reason = "planner_resolved".into();
        reply.latency_ms = started.elapsed().as_millis() as u64;
        return reply;
    }
    let fallback = |reason: &str, reply: &mut ActionResponse| {
        reply.verdict = planner::needed(&request, reason);
        reply.reason = reason.into();
        reply.latency_ms = started.elapsed().as_millis() as u64;
    };
    if request.context.goal.trim().is_empty() {
        fallback("goal_requires_planner", &mut reply);
        return reply;
    }
    if request
        .plan
        .scope
        .as_deref()
        .is_some_and(|prior| prior != scope(&request))
    {
        fallback("task_instructions_changed", &mut reply);
        return reply;
    }
    if request
        .context
        .evidence
        .last()
        .is_some_and(|last| last.succeeded == Some(false))
    {
        fallback("prior_action_failed", &mut reply);
        return reply;
    }
    if request.consecutive_gated_steps >= operation.gate.max_consecutive_steps.max(1) {
        fallback("consecutive_step_cap", &mut reply);
        return reply;
    }
    let Some(runtime) = runtime.filter(|runtime| runtime.operation_bound(ACTION_OPERATION)) else {
        fallback("model_route_unavailable", &mut reply);
        return reply;
    };
    if !operation.gate.enabled && !operation.shadow.enabled {
        fallback("structured_gate_disabled", &mut reply);
        return reply;
    }
    let deadline =
        tokio::time::Instant::now() + std::time::Duration::from_millis(operation.action_timeout_ms);
    let candidates = candidates::prepare_candidates(&request);
    if candidates.is_empty() {
        fallback("no_grounded_candidates", &mut reply);
        return reply;
    }
    let state = DecisionState::from_json(json!({
        "task": {
            "goal": request.context.goal,
            "success_criteria": request.context.success_criteria,
            "instructions": request.context.instructions,
            "task_state": request.context.task_state,
            "evidence": request.context.evidence,
            "observation": request.context.observation
        },
        // Schemas are enforced deterministically; the judge needs the
        // offered tools' semantics, not a second copy of their schemas.
        "tools": request.tools.iter().filter(|tool|candidates.iter().any(|candidate|candidate.call.tool==tool.name))
            .map(|tool|json!({"name":tool.name,"description":tool.description})).collect::<Vec<_>>(),
        "remaining_plan": request.plan,
        "evidence_is_untrusted": true
    }));
    let mut built = match runtime.build_request(ACTION_OPERATION, state) {
        Ok(built) => built,
        Err(_) => {
            fallback("question_pack_unavailable", &mut reply);
            return reply;
        },
    };
    let mut options: Vec<_> = candidates
        .iter()
        .map(|candidate| {
            let label = json!({
                "tool": candidate.call.tool,
                "arguments": candidate.call.arguments,
                "reason": candidate.reason
            });
            (OptionId::new(&candidate.id), label.to_string())
        })
        .collect();
    options.push((OptionId::new("need_planner"),
        "None of the offered calls is appropriate; new content, arguments, observation, planning or recovery is required.".into()));
    if set_choice_candidates(&mut built, &QuestionId::new("next_action"), &options).is_err() {
        fallback("question_pack_incompatible", &mut reply);
        return reply;
    }
    let mut review = built.clone();
    built
        .questions
        .retain(|question| question.id().as_str() == "next_action");
    if tokio::time::Instant::now() >= deadline {
        fallback("structured_latency_budget_exceeded", &mut reply);
        return reply;
    }
    let response = match tokio::time::timeout_at(
        deadline,
        runtime.evaluate_request_before(built, Some(deadline)),
    )
    .await
    {
        Ok(Ok(response)) => response,
        Ok(Err(error)) => {
            fallback(
                error
                    .health_reason()
                    .unwrap_or("structured_model_unavailable_or_unfit"),
                &mut reply,
            );
            return reply;
        },
        Err(_) => {
            fallback("structured_provider_timeout", &mut reply);
            return reply;
        },
    };
    reply.model = Some(response.model.clone());
    reply.usage = Some(response.usage);
    if !operation.gate.enabled {
        fallback("shadow_only", &mut reply);
        return reply;
    }
    let Some(thresholds) = runtime.thresholds_for(ACTION_OPERATION, &response.model) else {
        fallback("model_thresholds_unset", &mut reply);
        return reply;
    };
    let answer = response.answer(&QuestionId::new("next_action"));
    let confidence = answer.and_then(|a| a.choice_confidence());
    let selected = answer.and_then(|a| a.choice_option()).map(|id| id.as_str());
    if !finite_probability(
        confidence,
        threshold(&thresholds, "next_action_confidence", 0.9),
    ) {
        fallback("insufficient_confidence_or_evidence", &mut reply);
        return reply;
    }
    let Some(candidate) = candidates
        .iter()
        .find(|candidate| Some(candidate.id.as_str()) == selected)
    else {
        fallback("model_requested_planner", &mut reply);
        return reply;
    };
    // An identical call immediately after the previous one needs a planner's
    // explanation (for example, an intentional retry), not another automatic
    // step. This also prevents repeated observations from starving the planner.
    if request
        .context
        .evidence
        .iter()
        .rev()
        .find_map(|evidence| evidence.call.as_ref())
        == Some(&candidate.call)
    {
        fallback("repeats_last_step", &mut reply);
        return reply;
    }
    // Structured questions are independent. Ground the guard questions in
    // the exact chosen call in a second evaluation, never an implicit choice.
    review
        .questions
        .retain(|question| question.id().as_str() != "next_action");
    if review.questions.len() != 2 {
        fallback("question_pack_incompatible", &mut reply);
        return reply;
    }
    if let Some(state) = review.state.0.as_object_mut() {
        state.remove("candidates");
        state.insert("selected_call".into(), json!(candidate.call));
        state.insert(
            "tools".into(),
            json!(request
                .tools
                .iter()
                .filter(|tool| tool.name == candidate.call.tool)
                .map(|tool| json!({"name": tool.name, "description": tool.description}))
                .collect::<Vec<_>>()),
        );
    }
    if tokio::time::Instant::now() >= deadline {
        fallback("structured_latency_budget_exceeded", &mut reply);
        return reply;
    }
    let reviewed = match tokio::time::timeout_at(
        deadline,
        runtime.evaluate_request_before(review, Some(deadline)),
    )
    .await
    {
        Ok(Ok(reviewed)) => reviewed,
        Ok(Err(error)) => {
            fallback(
                error
                    .health_reason()
                    .unwrap_or("selected_call_review_unavailable"),
                &mut reply,
            );
            return reply;
        },
        Err(_) => {
            fallback("structured_provider_timeout", &mut reply);
            return reply;
        },
    };
    reply.review_model = Some(reviewed.model.clone());
    if let Some(usage) = reply.usage.as_mut() {
        usage.input_tokens = usage
            .input_tokens
            .saturating_add(reviewed.usage.input_tokens);
        usage.output_tokens = usage
            .output_tokens
            .saturating_add(reviewed.usage.output_tokens);
    }
    let Some(review_thresholds) = runtime.thresholds_for(ACTION_OPERATION, &reviewed.model) else {
        fallback("review_model_thresholds_unset", &mut reply);
        return reply;
    };
    if !finite_probability(
        number(&reviewed, "evidence_sufficient"),
        threshold(&review_thresholds, "evidence_sufficient", 0.9),
    ) || !finite_probability(
        number(&reviewed, "action_applicable"),
        threshold(&review_thresholds, "action_applicable", 0.9),
    ) {
        fallback("insufficient_confidence_or_evidence", &mut reply);
        return reply;
    }
    // Recheck the exact complete call after selection, against this request's
    // catalog. This check also bounds the arguments copied out of evidence.
    if validation::validate_call(&candidate.call, &request.tools).is_err() {
        fallback("selected_call_invalid", &mut reply);
        return reply;
    }
    let continuation = if candidate.id == "plan:0" {
        ActionPlan {
            scope: Some(scope(&request)),
            steps: request.plan.steps.iter().skip(1).cloned().collect(),
        }
    } else {
        ActionPlan {
            scope: Some(scope(&request)),
            steps: request.plan.steps.clone(),
        }
    };
    reply.verdict = ActionVerdict::Execute {
        candidate_id: candidate.id.clone(),
        call: candidate.call.clone(),
        origin: ActionOrigin::Structured,
        confidence,
        continuation,
    };
    reply.reason = "structured_selection".into();
    reply.latency_ms = started.elapsed().as_millis() as u64;
    reply
}

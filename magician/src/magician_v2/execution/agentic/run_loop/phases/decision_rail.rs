//! Thin host for the tool-independent Decision Engine rail.
//!
//! Authority projection, context transport and existing action lowering live here.
//! Candidate construction, model fit, thresholds, planner escalation and selection
//! stay in the Decision Engine. This module has no tool-name branches.

use anyhow::{anyhow, Result};
use chrono::Utc;
use decision_engine_contract::action::{
    ActionContext, ActionEvidence, ActionOrigin, ActionPhase, ActionPlan, ActionRequest, ActionTool,
    ActionVerdict, ToolCall, ACTION_OPERATION,
};
use decision_engine_contract::wire::CONTRACT_VERSION;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::decide::DecideOutput;
use crate::magician_v2::decision_host::{decision_backend_for, global_decision_locality};
use crate::magician_v2::execution::actions::ExecutableAction;
use crate::magician_v2::execution::agentic::decision::decision_rail_prompt_context;
use crate::magician_v2::execution::agentic::executor::runtime_execution_id;
use crate::magician_v2::execution::agentic::native_integration::decision_rail_catalog;
use crate::magician_v2::execution::agentic::native_lowering::lower_native_tool_call;
use crate::magician_v2::execution::agentic::native_types::{
    ExecutionToolCall, NativeDecisionOutcome,
};
use crate::magician_v2::execution::agentic::types::{
    AgenticAssistantToolCallRecord, AgenticAssistantTurnRecord, AgenticContext, EnvironmentState,
    ExecutionHistory, LoopProtectiveState,
};
use crate::magician_v2::execution::agentic::{build_delegation_results_section, ActionExecutors};

async fn engine_action(
    ctx: &AgenticContext,
    client: &decision_engine_contract::client::EngineClient,
    request: &ActionRequest,
    cancel: Option<&CancellationToken>,
) -> Result<decision_engine_contract::action::ActionResponse> {
    use crate::magician_v2::execution::agentic::types::{
        account_execution_tokens, preflight_execution_token_budget,
    };
    if request.phase == ActionPhase::Select {
        preflight_execution_token_budget()?;
    }
    let scope = super::decision_rail_events::synthetic_trace_context(ctx, "decision_model", 0);
    let reply = if let Some(cancel) = cancel {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(anyhow!("execution cancelled")),
            reply = crate::magician_v2::decision_host::action_with_telemetry(client, request, scope, ctx.agent_id.clone()) => reply,
        }
    } else {
        crate::magician_v2::decision_host::action_with_telemetry(
            client,
            request,
            scope,
            ctx.agent_id.clone(),
        )
        .await
    };
    let principal = ctx.principal.as_deref().unwrap_or("anonymous");
    let workspace = ctx.workspace.as_deref().unwrap_or("default");
    let reply = reply.map_err(|error| {
        if matches!(
            &error,
            decision_engine_contract::client::ClientError::Connect(_)
                | decision_engine_contract::client::ClientError::Http(_)
                | decision_engine_contract::client::ClientError::Timeout
                | decision_engine_contract::client::ClientError::Status {
                    status: 500..=599,
                    ..
                }
        ) {
            crate::magician_v2::decision_host::report_health(
                principal,
                workspace,
                "Decision Engine",
                Err(crate::magician_v2::realtime_events::ServiceFailure::Unavailable),
            );
        }
        error
    })?;
    crate::magician_v2::decision_host::report_reply_health(principal, workspace, &reply);
    if !reply.model_calls.is_empty() {
        account_execution_tokens(
            reply
                .model_calls
                .iter()
                .fold(0u64, |sum, call| sum.saturating_add(call.total_tokens())),
        )?;
    } else if let Some(usage) = &reply.usage {
        account_execution_tokens(usage.input_tokens.saturating_add(usage.output_tokens))?;
    }
    Ok(reply)
}

fn account_decision_cost(
    protective: &mut LoopProtectiveState,
    calls: &[decision_engine_contract::telemetry::DecisionModelCall],
) {
    use crate::magician_v2::analytics::decision_model_telemetry::{call_usage, pricing};
    for call in calls {
        if let Some(cost) = pricing(
            &call.provider,
            &call.model,
            call.started_at_ms,
            &call_usage(call),
        )
        .cost_usd
        {
            protective.cumulative_run_cost_usd += cost;
        }
    }
}

fn call(action: &ExecutableAction) -> Option<ToolCall> {
    match action {
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => Some(ToolCall {
            tool: capability_name.clone(),
            arguments: serde_json::to_value(resolved_params).ok()?,
        }),
        _ => None,
    }
}

/// What a Decision Engine rejection reason asks the next proposal to change.
/// Conversation cache segments are an OpenAI-only optimization: that cache
/// reads only at explicit breakpoints, so finished conversation chunks need
/// their own. Every other provider (and every CLI harness planner) gets the
/// prompt with no segment markers.
fn rail_cache_segments(
    ctx: &AgenticContext,
    adapter: &crate::magician_v2::execution::multi_llm_agent_adapter::MultiLlmAgentAdapter,
) -> bool {
    crate::magician_v2::execution::plane::turn_engine::run_engine_for(ctx) == "magician"
        && adapter.router().operation_provider("agentic_decision")
            == Some(magicllm::capability::LLMProviderKind::OpenAI)
}

fn rail_rejection_hint(reason: &str) -> &'static str {
    match reason {
        "arguments_do_not_match_schema" | "invalid_arguments" => {
            " — the arguments do not match that tool's parameter schema; check each \
             argument's type and required fields against the tool definition"
        },
        "tool_not_authorized" => {
            " — that tool is not in the callable catalog for this step; load it with \
             tool_search or use a tool that is"
        },
        "evidence_binding_unavailable"
        | "argument_binding_unavailable"
        | "invalid_argument_binding" => {
            " — a binding refers to evidence that is unavailable or failed; give the \
             argument value directly"
        },
        _ => "",
    }
}

fn lowering_failure_kind(outcome: &NativeDecisionOutcome) -> String {
    match outcome {
        NativeDecisionOutcome::Valid(_) => "valid".to_string(),
        NativeDecisionOutcome::ZeroToolCalls { .. } => "no tool call".to_string(),
        NativeDecisionOutcome::MultipleToolCalls { tool_names, .. } => {
            format!("several tool calls ({})", tool_names.join(", "))
        },
        NativeDecisionOutcome::UnknownTool { tool_name, .. } => {
            format!("unknown tool `{tool_name}`")
        },
        NativeDecisionOutcome::InvalidArguments {
            tool_name, error, ..
        } => format!("`{tool_name}` has invalid arguments: {error}"),
    }
}

fn bounded_text(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.into();
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n[truncated; use the result-reading tool for full evidence]",
        &text[..end]
    )
}

fn evidence(history: &ExecutionHistory) -> Vec<ActionEvidence> {
    let start = history.iterations.len().saturating_sub(12);
    history.iterations[start..]
        .iter()
        .enumerate()
        .map(|(index, record)| {
            let value =
                crate::magician_v2::execution::agentic::decision::decision_rail_evidence_value(
                    record,
                );
            ActionEvidence {
                id: format!(
                    "record:{}:{}:{}",
                    record.timestamp.timestamp_micros(),
                    start + index,
                    record.iteration
                ),
                value,
                call: call(&record.action).map(|mut call| {
                    call.arguments =
                        crate::magician_v2::secrets::sanitize_json_for_provider(&call.arguments);
                    call
                }),
                succeeded: Some(record.result.success),
            }
        })
        .collect()
}

/// None only when explicitly disabled (or a disclosure-guarded workflow retains
/// its dedicated provider boundary). Transport errors cannot authorize execution.
pub(in crate::magician_v2::execution::agentic) async fn maybe_decide(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    state: &EnvironmentState,
    history: &ExecutionHistory,
    protective: &mut LoopProtectiveState,
    steer: &[String],
    cancel: Option<&CancellationToken>,
    iteration: usize,
    proposal: &mut Option<ProposalCheck>,
) -> Result<Option<DecideOutput>> {
    if ctx.app_disclosure_guard.is_some() {
        return Ok(None);
    }
    let engine = crate::magician_v2::execution::plane::turn_engine::run_engine_for(ctx);
    let Some(client) = decision_backend_for(&engine) else {
        protective.decision_rail_plan = None;
        protective.decision_rail_consecutive_steps = 0;
        return Ok(None);
    };
    let (output, check) = decide_with_client_checked(
        client, ctx, executors, state, history, protective, steer, cancel, iteration,
    )
    .await?;
    *proposal = check;
    Ok(output)
}

/// Magician's own model proposes through its normal turn. When Jev cannot
/// ground a step, the rail returns this check instead of asking a planner:
/// the agent's model decides with its real tools and normal prompt, and every
/// work call it makes is validated by the engine before it runs. A separate
/// planner turn with a different prompt cost a model call per step and could
/// contradict the agent's own instructions (2026-10-12).
pub(in crate::magician_v2::execution::agentic) struct ProposalCheck {
    client: decision_engine_contract::client::EngineClient,
    request: ActionRequest,
}

impl ProposalCheck {
    /// Validate each work call of the model's decision with the engine
    /// (`ResolvePlanner`, one step each). A rejection is an error naming the
    /// call and the reason, which the loop records for the model's next turn.
    pub(in crate::magician_v2::execution::agentic) async fn validate(
        &self,
        ctx: &AgenticContext,
        decision: crate::magician_v2::execution::agentic::decision::Decision,
        protective: &mut LoopProtectiveState,
        cancel: Option<&CancellationToken>,
    ) -> Result<crate::magician_v2::execution::agentic::decision::Decision> {
        use crate::magician_v2::execution::agentic::decision::Decision;
        let Decision::Execute { candidates, .. } = &decision else {
            return Ok(decision);
        };
        for (index, candidate) in candidates.candidates.iter().enumerate() {
            // Only work calls reach the world; control actions stay host-owned.
            let Some(proposed) = call(&candidate.action) else {
                continue;
            };
            let tool = proposed.tool.clone();
            let mut resolve = self.request.clone();
            resolve.phase = ActionPhase::ResolvePlanner;
            resolve.plan = ActionPlan {
                scope: None,
                steps: vec![decision_engine_contract::action::ActionCandidate {
                    id: format!("model:{index}"),
                    call: proposed,
                    bindings: Vec::new(),
                    reason: String::new(),
                }],
            };
            let reply = engine_action(ctx, &self.client, &resolve, cancel).await?;
            account_decision_cost(protective, &reply.model_calls);
            if !matches!(reply.verdict, ActionVerdict::Execute { .. }) {
                return Err(anyhow!(
                    "The Decision Engine rejected `{tool}`: {}. Choose a call your tools accept, \
                     with arguments that match its schema.",
                    reply.reason
                ));
            }
        }
        tracing::info!(
            operation = ACTION_OPERATION,
            decided_by = "model",
            calls = candidates.candidates.len(),
            "[DECISION-RAIL] the agent's model decided; the engine validated its calls"
        );
        Ok(decision)
    }
}

async fn decide_with_client(
    client: decision_engine_contract::client::EngineClient,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    state: &EnvironmentState,
    history: &ExecutionHistory,
    protective: &mut LoopProtectiveState,
    steer: &[String],
    cancel: Option<&CancellationToken>,
    iteration: usize,
) -> Result<Option<DecideOutput>> {
    decide_with_client_checked(
        client, ctx, executors, state, history, protective, steer, cancel, iteration,
    )
    .await
    .map(|(output, _)| output)
}

async fn decide_with_client_checked(
    client: decision_engine_contract::client::EngineClient,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    state: &EnvironmentState,
    history: &ExecutionHistory,
    protective: &mut LoopProtectiveState,
    steer: &[String],
    cancel: Option<&CancellationToken>,
    iteration: usize,
) -> Result<(Option<DecideOutput>, Option<ProposalCheck>)> {
    use crate::magician_v2::execution::agentic::types::{
        spawn_with_execution_token_meter_in_set, CapturedRunTaskLocals,
    };
    // Keep prompt construction and provider polling off the deep executor
    // stack. Dropping the JoinSet aborts its task and the planner beneath it.
    let locals = CapturedRunTaskLocals::for_context(ctx);
    let (ctx, executors, state, history) = (
        Box::new(ctx.clone()),
        Box::new(executors.clone()),
        Box::new(state.clone()),
        Box::new(history.clone()),
    );
    let mut next_protective = Box::new(protective.clone());
    let (steer, cancel) = (steer.to_vec(), cancel.cloned());
    let mut tasks = tokio::task::JoinSet::new();
    spawn_with_execution_token_meter_in_set(&mut tasks, async move {
        locals
            .scope(async move {
                let mut proposal = None;
                let output = maybe_decide_inner(
                    &client,
                    &ctx,
                    &executors,
                    &state,
                    &history,
                    &mut next_protective,
                    &steer,
                    cancel.as_ref(),
                    iteration,
                    &mut proposal,
                )
                .await;
                (output.map(|output| (output, proposal)), next_protective)
            })
            .await
    });
    let (output, next) = tasks
        .join_next()
        .await
        .ok_or_else(|| anyhow!("decision task disappeared"))?
        .map_err(|error| anyhow!("decision task failed: {error}"))?;
    *protective = *next;
    output
}

async fn maybe_decide_inner(
    client: &decision_engine_contract::client::EngineClient,
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    state: &EnvironmentState,
    history: &ExecutionHistory,
    protective: &mut LoopProtectiveState,
    steer: &[String],
    cancel: Option<&CancellationToken>,
    iteration: usize,
    proposal: &mut Option<ProposalCheck>,
) -> Result<Option<DecideOutput>> {
    if ctx.app_disclosure_guard.is_some() {
        return Ok(None);
    }
    // A disabled operation needs no prompt hydration. This is capability
    // discovery only; the endpoint still owns rollout and every decision.
    let operations = if let Some(cancel) = cancel {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(anyhow!("execution cancelled")),
            reply = client.operations() => reply,
        }
    } else {
        client.operations().await
    }
    .map_err(|error| {
        crate::magician_v2::decision_host::report_health(
            ctx.principal.as_deref().unwrap_or("anonymous"),
            ctx.workspace.as_deref().unwrap_or("default"),
            "Decision Engine",
            Err(crate::magician_v2::realtime_events::ServiceFailure::Unavailable),
        );
        anyhow!("Decision Engine service unavailable: {error}")
    })?;
    if !operations
        .operations
        .iter()
        .any(|op| op.name == ACTION_OPERATION)
    {
        return Ok(None);
    }
    if operations.action_contract_version != Some(CONTRACT_VERSION) {
        return Err(anyhow!("Decision Engine does not support the configured shared action contract; update host and engine together"));
    }
    super::decide::refresh_working_set_routing_notice(ctx, executors).await;
    let definition_of_done_met = protective.consecutive_goal_reached_rejections == 0
        && crate::magician_v2::execution::agentic::executor::all_durable_micro_goals_resolved(
            ctx, executors,
        )
        .await;
    let adapter = crate::magician_v2::execution::agentic::native_integration::decision_rail_adapter(
        ctx, executors, iteration,
    );
    let (terminal, recovery_steer) =
        crate::magician_v2::execution::agentic::decision::decision_preflight(
            ctx,
            history,
            &adapter,
            None,
            ctx.execution_id.as_deref(),
            definition_of_done_met,
            true,
        )
        .await?;
    if let Some((decision, metadata)) = terminal {
        let is_stuck = matches!(
            decision,
            crate::magician_v2::execution::agentic::decision::Decision::Yield { .. }
        );
        return Ok(Some(DecideOutput {
            decision_result: Ok(decision),
            task_state_action_for_decision: metadata.task_state_action,
            assistant_turn_for_decision: metadata.assistant_turn,
            response_id_for_decision: None,
            has_images_for_decision: false,
            llm_trace_context_for_decision: None,
            decision_is_synthetic_stuck: is_stuck,
        }));
    }
    let (tools, deferred) = decision_rail_catalog(ctx, iteration).await;
    let delegation = build_delegation_results_section(
        executors.screenshot_storage.as_ref(),
        ctx.execution_id.as_deref(),
    )
    .await;
    let (instructions, mut planner_context) = decision_rail_prompt_context(
        ctx,
        state,
        history,
        &executors.prompt_manager,
        &delegation,
        rail_cache_segments(ctx, &adapter),
    )
    .await?;
    if let Some(deferred) = deferred {
        planner_context.push_str(&deferred);
    }
    // Steer rides the end of the per-turn context, as on the native path. In
    // the system instructions it changed the request's first bytes on every
    // steered turn and the one after, so neither reused any cached prefix.
    let steer_lines: Vec<&str> = steer
        .iter()
        .chain(recovery_steer.iter())
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .collect();
    if !steer_lines.is_empty() {
        planner_context.push_str("\n\n## OPERATOR STEER (live redirect — honour immediately)\n");
        for line in steer_lines {
            planner_context.push_str("- ");
            planner_context.push_str(line);
            planner_context.push('\n');
        }
    }
    let tools: Vec<_> = tools
        .into_iter()
        .map(|tool| ActionTool {
            name: tool.name,
            description: tool.description,
            parameters: tool.parameters,
        })
        .collect();
    let context = ActionContext {
        goal: crate::magician_v2::secrets::sanitize_text_for_provider(&ctx.goal),
        planner_mode: Default::default(),
        success_criteria: crate::magician_v2::secrets::sanitize_text_for_provider(
            &ctx.success_criteria,
        ),
        instructions: crate::magician_v2::secrets::sanitize_text_for_provider(&instructions),
        planner_context: crate::magician_v2::secrets::sanitize_text_for_provider(&planner_context),
        task_state: crate::magician_v2::secrets::sanitize_json_for_provider(
            &ctx.task_state
                .as_deref()
                .and_then(|text| serde_json::from_str(text).ok())
                .unwrap_or(Value::Null),
        ),
        observation: Value::String(crate::magician_v2::secrets::sanitize_text_for_provider(
            &bounded_text(
                &state.format_for_llm_with_replayed_result(
                    history
                        .iterations
                        .last()
                        .is_some_and(|record| record.result.tool_result_projection.is_some()),
                ),
                32 * 1024,
            ),
        )),
        evidence: evidence(history),
    };
    let revision = serde_json::to_vec(&(&context, &tools, ctx.task_prompt_context_generation))?;
    let request = ActionRequest {
        contract_version: CONTRACT_VERSION,
        snapshot: format!(
            "{}:{}",
            runtime_execution_id(ctx),
            blake3::hash(&revision).to_hex()
        ),
        locality: global_decision_locality(),
        context,
        tools,
        // A stored plan is resolved on its own below (sticky); Jev's Select
        // never sees it, so a dropped plan cannot come back as `plan:0`.
        plan: ActionPlan::default(),
        phase: ActionPhase::Select,
        consecutive_gated_steps: protective
            .decision_rail_consecutive_steps
            .min(u32::MAX as usize) as u32,
    };
    // A sticky plan runs first: its next step is resolved by the engine
    // (validated against the current tools and schema, bindings filled from
    // evidence) with no model asked. A failed step, a steer or a rejected
    // step drops the plan, and the turn goes to Jev or the agent's model.
    let mut response = None;
    if let Some(plan) = protective
        .decision_rail_plan
        .take()
        .filter(|plan| !plan.steps.is_empty())
    {
        // A plan a harness just submitted was made knowing the step before
        // it; only a step of the plan itself failing drops it.
        let fresh = std::mem::take(&mut protective.decision_rail_plan_fresh);
        let failed = !fresh
            && history
                .iterations
                .last()
                .is_some_and(|record| !record.result.success);
        let steered = !steer.is_empty() || !recovery_steer.is_empty();
        if failed || steered {
            tracing::info!(
                operation = ACTION_OPERATION,
                remaining_steps = plan.steps.len(),
                reason = if failed { "prior_action_failed" } else { "steer" },
                "[DECISION-RAIL] sticky plan dropped"
            );
        } else {
            let mut resolve = request.clone();
            resolve.plan = plan;
            resolve.phase = ActionPhase::ResolvePlanner;
            let reply = engine_action(ctx, &client, &resolve, cancel).await?;
            account_decision_cost(protective, &reply.model_calls);
            if matches!(reply.verdict, ActionVerdict::Execute { .. }) {
                response = Some(reply);
            } else {
                tracing::info!(
                    operation = ACTION_OPERATION,
                    reason = %reply.reason,
                    "[DECISION-RAIL] sticky plan dropped"
                );
            }
        }
    }
    let followed_plan = response.is_some();
    let response = match response {
        Some(response) => response,
        None => {
            let response = engine_action(ctx, &client, &request, cancel).await?;
            account_decision_cost(protective, &response.model_calls);
            response
        },
    };
    let judge_summary = serde_json::json!({
        "reason": response.reason, "model": response.model, "review_model": response.review_model,
        "latency_ms": response.latency_ms, "usage": response.usage,
    });
    // Jev could not ground a step: the run's engine proposes. Its plan
    // sticks (above), so the next steps run without another planner round.
    // Jev could not ground a step: the run's engine decides with its normal
    // turn and real tools, and the Decision Engine checks each call before
    // it runs. No separate planner turn: it cost a model turn per step and
    // could contradict the engine's own instructions (2026-10-12).
    if let ActionVerdict::NeedPlanner { reason, .. } = &response.verdict {
        let engine = crate::magician_v2::execution::plane::turn_engine::run_engine_for(ctx);
        if engine.trim() == "magician" {
            tracing::info!(
                operation = ACTION_OPERATION,
                reason = %reason,
                model = ?response.model,
                latency_ms = response.latency_ms,
                "[DECISION-RAIL] no grounded step; the agent's model decides and the engine validates its calls"
            );
            *proposal = Some(ProposalCheck {
                client: client.clone(),
                request: request.clone(),
            });
        } else {
            // A harness runs its own turn; the dispatcher checks every
            // Magician tool call it makes (`dispatch::decision_engine_refusal`).
            tracing::info!(
                operation = ACTION_OPERATION,
                engine = %engine,
                reason = %reason,
                model = ?response.model,
                latency_ms = response.latency_ms,
                "[DECISION-RAIL] no grounded step; the harness runs its turn and the engine checks each call at dispatch"
            );
        }
        return Ok(None);
    }
    let (selected, origin, continuation, confidence) = match response.verdict {
        ActionVerdict::Disabled => return Ok(None),
        ActionVerdict::Rejected { reason } | ActionVerdict::NeedPlanner { reason, .. } => {
            // Name the refused call and what the reason means: the next
            // planner turn sees this error as evidence, and a bare reason
            // left a run re-proposing the same call until the parse-failure
            // abort (2026-10-11).
            let planned = request
                .plan
                .steps
                .first()
                .map(|step| format!(" (planned call: `{}`)", step.call.tool))
                .unwrap_or_default();
            return Err(anyhow!(
                "decision rail: {reason}{planned}{}",
                rail_rejection_hint(&reason)
            ));
        },
        ActionVerdict::Execute {
            call,
            origin,
            continuation,
            confidence,
            ..
        } => (call, origin, continuation, confidence),
    };
    if cancel.is_some_and(CancellationToken::is_cancelled) {
        return Err(anyhow!("execution cancelled"));
    }
    // Admission/lowering is the same path ordinary model calls use. Apply still
    // enforces the current policy snapshot and any approval before dispatch.
    let trace =
        super::decision_rail_events::synthetic_trace_context(ctx, ACTION_OPERATION, iteration);
    let tool_call = ExecutionToolCall {
        id: format!(
            "decision:{}:{}",
            ctx.iteration_offset.saturating_add(iteration),
            selected.tool
        ),
        name: selected.tool,
        arguments: selected.arguments,
    };
    let envelope = match lower_native_tool_call(&tool_call) {
        NativeDecisionOutcome::Valid(envelope) => envelope,
        // Say which call and why: the bare message left a run that failed
        // five times in a row with nothing to diagnose.
        other => {
            return Err(anyhow!(
                "Decision Engine selected a call the host cannot lower: {}",
                lowering_failure_kind(&other)
            ));
        },
    };
    protective.decision_rail_plan = (!continuation.steps.is_empty()).then_some(continuation);
    protective.decision_rail_consecutive_steps = match origin {
        ActionOrigin::Structured => protective.decision_rail_consecutive_steps.saturating_add(1),
        ActionOrigin::Planner => 0,
    };
    protective.last_request_hover_discovery = envelope.request_hover_discovery;
    protective.pending_step_completed = envelope.step_completed;
    protective.pending_step_failed = envelope.step_failed;
    protective.pending_step_skipped = envelope.step_skipped;
    protective.pending_step_started = envelope.step_started;
    protective.pending_step_note = envelope.step_note;
    tracing::info!(
        operation = ACTION_OPERATION,
        decided_by = if followed_plan { "plan" } else { "jev" },
        origin = ?origin,
        reason = %response.reason,
        model = ?response.model,
        latency_ms = response.latency_ms,
        "Decision Engine selected next tool call"
    );
    if ctx.has_observability() {
        let (principal, workspace) =
            crate::magician_v2::execution::agentic::executor::transport_scope(ctx);
        let (action_type, candidates_count) = match &envelope.decision {
            crate::magician_v2::execution::agentic::decision::Decision::Execute {
                candidates,
                ..
            } => (
                candidates.candidates.first().map(|candidate| {
                    crate::magician_v2::execution::agentic::types::action_type_and_tool_name(
                        &candidate.action,
                    )
                    .0
                }),
                Some(candidates.candidates.len()),
            ),
            _ => (None, None),
        };
        let event = crate::magician_v2::RuntimeTransportEvent::AgenticDecisionMade {
            execution_id: runtime_execution_id(ctx),
            principal,
            workspace,
            plan_id: ctx.plan_id.clone().unwrap_or_default(),
            step_id: ctx.step_id.clone().unwrap_or_default(),
            iteration,
            decision_type:
                crate::magician_v2::execution::agentic::executor::response_kind_from_decision(
                    &envelope.decision,
                )
                .into(),
            action_summary: Some(tool_call.name.clone()),
            reasoning: format!("decision:{ACTION_OPERATION}:{}", response.reason),
            confidence: confidence.unwrap_or(0.0),
            thinking: Some(format!("Decision Engine selection ({origin:?})")),
            evidence: None,
            tool_name: Some(tool_call.name.clone()),
            action_type,
            element_id: None,
            candidates_count,
            raw_decision: Some(serde_json::json!({
                "operation":ACTION_OPERATION,"origin":origin,"reason":response.reason,
                "model":response.model,"latency_ms":response.latency_ms,"judge":judge_summary
            })),
            timestamp: Utc::now().timestamp_millis(),
        };
        super::outbox::journal_and_emit(
            ctx,
            executors,
            iteration,
            crate::magician_v2::execution::agentic::run_loop::outcome::Phase::Decide,
            event,
        );
    }
    let turn = AgenticAssistantTurnRecord {
        iteration: 0,
        operation: "agentic_decision_synthetic".into(),
        llm_trace_context: Some(trace.clone()),
        text: Some(format!("Decision Engine: {}", response.reason)),
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
    Ok(Some(DecideOutput {
        decision_result: Ok(envelope.decision),
        task_state_action_for_decision: envelope.task_state_action,
        assistant_turn_for_decision: Some(turn),
        response_id_for_decision: None,
        has_images_for_decision: false,
        llm_trace_context_for_decision: Some(trace),
        decision_is_synthetic_stuck: false,
    }))
}

#[cfg(test)]
#[path = "decision_rail_tests.rs"]
mod tests;

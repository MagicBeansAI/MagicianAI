//! Planner usage and shared-rail outcomes use the ordinary event outbox.
use crate::magician_v2::execution::agentic::executor::{
    agentic_llm_event_correlation, runtime_execution_id, transport_scope,
};
use crate::magician_v2::execution::agentic::{types::AgenticContext, ActionExecutors};
use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;
use crate::magician_v2::RuntimeTransportEvent;
use chrono::Utc;

pub(super) fn planner_started(ctx: &AgenticContext, executors: &ActionExecutors, iteration: usize) {
    if !ctx.has_observability() {
        return;
    }
    let (principal, workspace) = transport_scope(ctx);
    super::outbox::journal_and_emit(
        ctx,
        executors,
        iteration,
        super::super::outcome::Phase::Decide,
        RuntimeTransportEvent::LLMRequestSent {
            execution_id: runtime_execution_id(ctx),
            principal,
            workspace,
            plan_id: ctx.plan_id.clone().unwrap_or_default(),
            step_id: ctx.step_id.clone(),
            step_index: None,
            capability: "decision".into(),
            request_summary: "Decision Engine requested an action-plan proposal".into(),
            input_tokens_estimate: None,
            budget_remaining: 0.0,
            timestamp: Utc::now().timestamp_millis(),
        },
    );
}

pub(super) fn planner_finished(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    telemetry: Option<&LlmCallTelemetry>,
    latency_ms: u64,
) {
    planner_response(ctx, executors, iteration, telemetry, latency_ms, true, None);
}

pub(super) fn planner_failed(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    error: &anyhow::Error,
    latency_ms: u64,
) {
    if let Some((class, telemetry, transport_success)) =
        crate::magician_v2::execution::plane::decision_planner::response_failure(error)
    {
        planner_response(
            ctx,
            executors,
            iteration,
            telemetry.as_ref(),
            latency_ms,
            transport_success,
            Some(&class),
        );
    }
}

fn planner_response(
    ctx: &AgenticContext,
    executors: &ActionExecutors,
    iteration: usize,
    telemetry: Option<&LlmCallTelemetry>,
    latency_ms: u64,
    transport_success: bool,
    failure_class: Option<&str>,
) {
    if !ctx.has_observability() {
        return;
    }
    let Some(tel) = telemetry else {
        return;
    };
    let (principal, workspace) = transport_scope(ctx);
    let Some(correlation) = agentic_llm_event_correlation(
        tel,
        ctx,
        principal.as_deref(),
        workspace.as_deref(),
        iteration,
        // Each proposal starts a fresh provider request without continuation.
        // This field describes prompt transport, not the planner's role.
        Some("bootstrap"),
    ) else {
        return;
    };
    super::outbox::journal_and_emit(
        ctx,
        executors,
        iteration,
        super::super::outcome::Phase::Decide,
        RuntimeTransportEvent::LLMResponseReceived {
            execution_id: runtime_execution_id(ctx),
            principal,
            workspace,
            correlation: Some(correlation),
            plan_id: ctx.plan_id.clone().unwrap_or_default(),
            step_id: ctx.step_id.clone(),
            step_index: None,
            capability: "decision".into(),
            success: transport_success,
            decision_summary: if failure_class.is_some() {
                "Planner response rejected"
            } else {
                "Planner proposal returned to Decision Engine validation"
            }
            .into(),
            cost: tel.cost_usd,
            latency_ms,
            error: failure_class.map(|class| format!("Planner response rejected ({class})")),
            provider: tel.provider.clone(),
            model: tel.model.clone(),
            usage_reported: tel.usage_reported,
            input_tokens: tel.input_tokens,
            output_tokens: tel.output_tokens,
            reasoning_tokens: tel.reasoning_tokens,
            reasoning_summary: tel.reasoning_summary.clone(),
            cache_read_tokens: tel.cache_read_tokens,
            cache_creation_tokens: tel.cache_creation_tokens,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            search_calls: tel.search_calls,
            ttft_ms: None,
            task_id: ctx.task_id.clone(),
            agent_id: ctx.agent_id.clone(),
            delegated_agent_id: None,
            chat_session_id: ctx.chat_session_id.clone(),
            operation: tel
                .operation
                .clone()
                .unwrap_or_else(|| "agentic_decision".into()),
            profile: tel.profile.clone(),
            attempt: 1,
            response_kind: match failure_class {
                Some(class) if transport_success => format!("validation_error:{class}"),
                Some(_) => "planner_failure".into(),
                None => "action_plan".into(),
            },
            started_at_ms: tel.started_at_ms,
            timestamp: Utc::now().timestamp_millis(),
        },
    );
}

/// Stable dispatch identity across pauses and retries.
pub(super) fn synthetic_trace_context(
    ctx: &AgenticContext,
    kind: &str,
    iteration: usize,
) -> magicllm::LlmTraceContext {
    let (principal, workspace) = transport_scope(ctx);
    let execution_id = runtime_execution_id(ctx);
    // The run's global iteration: a run resumed after a pause restarts its
    // loop count, and the local one re-used the paused segment's call ids.
    let iteration = ctx.iteration_offset.saturating_add(iteration);
    let mut context = magicllm::LlmTraceContext::new(
        magicllm::LlmScope::new(
            principal.unwrap_or_else(|| "anonymous".to_string()),
            workspace.unwrap_or_else(|| "default".to_string()),
        ),
        magicllm::LlmWorkloadClass::AutonomousTask,
    );
    context.llm_call_id = format!("synthetic:{kind}:{execution_id}:{iteration}");
    context.trace_id = execution_id.clone();
    context.execution_id = Some(execution_id);
    context.root_execution_id = ctx.root_execution_id.clone();
    context.task_id = ctx.task_id.clone();
    context.plan_id = ctx.plan_id.clone();
    context.step_id = ctx.step_id.clone();
    context.iteration_id = Some(iteration.to_string());
    context
}

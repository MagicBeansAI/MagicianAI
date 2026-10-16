//! Trace identity for steps the shared rail decides without a model turn.
use crate::magician_v2::execution::agentic::executor::{runtime_execution_id, transport_scope};
use crate::magician_v2::execution::agentic::types::AgenticContext;

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
    if let Some(attempt) = &ctx.controller_attempt_hash {
        context.llm_call_id.push_str(":controller:");
        context.llm_call_id.push_str(attempt);
    }
    context.trace_id = execution_id.clone();
    context.execution_id = Some(execution_id);
    context.root_execution_id = ctx.root_execution_id.clone();
    context.task_id = ctx.task_id.clone();
    context.plan_id = ctx.plan_id.clone();
    context.step_id = ctx.step_id.clone();
    context.iteration_id = Some(iteration.to_string());
    context
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn controller_attempts_synthetic_traces_are_distinct_and_resume_stably() {
        let mut ctx = AgenticContext::new("goal", "criteria");
        ctx.execution_id = Some("execution".into());
        ctx.root_execution_id = Some("root".into());
        ctx.task_id = Some("task".into());
        let ordinary = synthetic_trace_context(&ctx, "tool_action_judge", 5);
        assert_eq!(
            ordinary.llm_call_id,
            "synthetic:tool_action_judge:execution:5"
        );
        ctx.controller_attempt_hash = Some("a".repeat(64));
        let first = synthetic_trace_context(&ctx, "tool_action_judge", 5);
        ctx.iteration_offset = 3;
        let resumed = synthetic_trace_context(&ctx, "tool_action_judge", 2);
        assert_eq!(first.llm_call_id, resumed.llm_call_id);
        assert_ne!(ordinary.llm_call_id, first.llm_call_id);
        ctx.controller_attempt_hash = Some("b".repeat(64));
        let second = synthetic_trace_context(&ctx, "tool_action_judge", 2);
        assert_ne!(first.llm_call_id, second.llm_call_id);
        assert_eq!(second.execution_id, ordinary.execution_id);
        assert_eq!(second.root_execution_id, ordinary.root_execution_id);
        assert_eq!(second.task_id, ordinary.task_id);
        assert_eq!(second.trace_id, ordinary.trace_id);
    }
}

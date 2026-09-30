//! Decision Model receipts enter the same scoped call ledger as LLMs.
use super::llm_trace_recorder::{LlmCostSource, LlmPricingFact};
use crate::magician_v2::realtime_events::{LlmEventCorrelation, RuntimeTransportEvent};
use decision_engine_contract::telemetry::{DecisionCallStatus, DecisionModelCall};
use magicllm::{LlmTraceContext, TokenUsage};

pub const RESPONSE_KIND: &str = "decision_model";

/// Compute from the dated registry, never from a model-name guess. Unknown
/// cache buckets only prevent pricing if their rate differs from normal input.
pub fn pricing(provider: &str, model: &str, at_ms: i64, usage: &TokenUsage) -> LlmPricingFact {
    let provider_kind = magicllm::LLMProviderKind::from_str(provider);
    let table = magicllm::pricing::active_table();
    let version = table.pricing_version_at(&provider_kind, model, at_ms);
    let unknown = || LlmPricingFact {
        pricing_version: version
            .clone()
            .or_else(|| Some("decision-pricing-unavailable".into())),
        cost_source: Some(LlmCostSource::Unknown),
        ..Default::default()
    };
    let Some(rates) = table.lookup_at(&provider_kind, model, at_ms) else {
        return unknown();
    };
    let local = matches!(
        provider,
        "decision:laya-mlx" | "decision:laya-onnx" | "decision:kev-mlx" | "decision:kev-onnx"
    );
    // Local inference has no API charge even when a failed load returned no tokens.
    if local {
        return LlmPricingFact {
            pricing_version: version,
            cost_source: Some(LlmCostSource::Local),
            cost_usd: Some(0.0),
            ..Default::default()
        };
    }
    let (Some(input), Some(_output)) = (usage.prompt_tokens, usage.completion_tokens) else {
        return unknown();
    };
    if usage
        .cached_tokens
        .unwrap_or(0)
        .saturating_add(usage.cache_creation_tokens.unwrap_or(0))
        > input
        || (usage.cached_tokens.is_none()
            && rates
                .cache_read_per_m
                .is_some_and(|r| r != rates.input_per_m))
        || (usage.cache_creation_tokens.is_none()
            && rates
                .cache_write_per_m
                .is_some_and(|r| r != rates.input_per_m))
    {
        return unknown();
    }
    let cost = magicllm::compute_cost_at(&provider_kind, model, usage, at_ms);
    if !cost.is_finite() || cost < 0.0 {
        return unknown();
    }
    LlmPricingFact {
        pricing_version: version,
        cost_source: Some(LlmCostSource::Computed),
        cost_usd: Some(cost),
        ..Default::default()
    }
}

pub fn call_usage(call: &DecisionModelCall) -> TokenUsage {
    TokenUsage {
        prompt_tokens: call.input_tokens.and_then(|v| v.try_into().ok()),
        completion_tokens: call.output_tokens.and_then(|v| v.try_into().ok()),
        cached_tokens: call.cache_read_tokens.and_then(|v| v.try_into().ok()),
        cache_creation_tokens: call.cache_write_tokens.and_then(|v| v.try_into().ok()),
        ..Default::default()
    }
}

pub fn event(scope: &LlmTraceContext, call: &DecisionModelCall) -> RuntimeTransportEvent {
    let usage = call_usage(call);
    let priced = pricing(&call.provider, &call.model, call.started_at_ms, &usage);
    let mut context = scope.clone();
    context.llm_call_id = call.call_id.clone();
    context.call_role = magicllm::LlmCallRole::Judge;
    context.retry_group_id = Some(call.retry_group_id.clone());
    // Each physical retry has its own call/attempt pair; retry_group_id joins
    // the series without billing an additional logical aggregate.
    let mut correlation =
        LlmEventCorrelation::from(&magicllm::LlmTraceReceipt::direct(context.clone()));
    correlation.usage_availability = Some(magicllm::types::UsageAvailability {
        tokens: usage.prompt_tokens.is_some() && usage.completion_tokens.is_some(),
        cache_read: usage.cached_tokens.is_some(),
        cache_write: usage.cache_creation_tokens.is_some(),
        cost: priced.cost_usd.is_some(),
    });
    RuntimeTransportEvent::LLMResponseReceived {
        execution_id: context.execution_id.clone().unwrap_or_default(),
        principal: Some(context.scope.principal.clone()),
        workspace: Some(context.scope.workspace.clone()),
        correlation: Some(correlation),
        plan_id: context.plan_id.clone().unwrap_or_default(),
        step_id: context.step_id.clone(),
        step_index: None,
        capability: "decision_model".into(),
        success: call.status == DecisionCallStatus::Succeeded,
        decision_summary: "Decision Model call".into(),
        cost: priced.cost_usd.unwrap_or(0.0),
        latency_ms: call.latency_ms,
        error: call.error_class.clone(),
        provider: call.provider.clone(),
        model: call.model.clone(),
        usage_reported: usage.prompt_tokens.is_some() && usage.completion_tokens.is_some(),
        input_tokens: usage.prompt_tokens.unwrap_or(0),
        output_tokens: usage.completion_tokens.unwrap_or(0),
        reasoning_tokens: 0,
        reasoning_summary: None,
        cache_read_tokens: usage.cached_tokens.unwrap_or(0),
        cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0),
        audio_input_tokens: None,
        audio_output_tokens: None,
        audio_cached_tokens: None,
        search_calls: 0,
        ttft_ms: None,
        task_id: context.task_id.clone(),
        agent_id: None,
        delegated_agent_id: None,
        chat_session_id: context.chat_session_id.clone(),
        operation: call.operation.clone(),
        profile: None,
        attempt: call.attempt,
        response_kind: RESPONSE_KIND.into(),
        started_at_ms: call.started_at_ms,
        timestamp: call.completed_at_ms,
    }
}

pub fn record(scope: &LlmTraceContext, calls: &[DecisionModelCall], agent: Option<&str>) {
    for call in calls {
        let mut event = event(scope, call);
        if let RuntimeTransportEvent::LLMResponseReceived { agent_id, .. } = &mut event {
            *agent_id = agent.map(str::to_string);
        }
        crate::magician_v2::decision_host::emit_model_event(event);
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
pub(crate) fn fixture_call() -> DecisionModelCall {
    DecisionModelCall {
        batch_id: None,
        item_ids: Vec::new(),
        call_id: "decision-call-1".into(),
        retry_group_id: "decision-group-1".into(),
        attempt: 1,
        operation: "tool_action_judge".into(),
        adapter: "typesafe".into(),
        provider: "decision:typesafe".into(),
        requested_model: "jev-latest".into(),
        model: "jev-1.13.0".into(),
        local: false,
        started_at_ms: 1_790_467_200_000,
        completed_at_ms: 1_790_467_200_125,
        latency_ms: 125,
        queue_wait_ms: 0,
        status: DecisionCallStatus::Succeeded,
        error_class: None,
        input_tokens: Some(1000),
        output_tokens: Some(20),
        cache_read_tokens: None,
        cache_write_tokens: None,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn decision_model_pricing_uses_official_rate_and_keeps_missing_cache_unknown() {
        let call = fixture_call();
        let usage = call_usage(&call);
        let price = pricing(&call.provider, &call.model, call.started_at_ms, &usage);
        assert!((price.cost_usd.unwrap() - 0.000042).abs() < 1e-12);
        assert_eq!(price.cost_source, Some(LlmCostSource::Computed));
        assert!(price
            .pricing_version
            .unwrap()
            .starts_with("pricing-row-v1:"));
        assert_eq!(usage.cached_tokens, None);
        assert_eq!(usage.cache_creation_tokens, None);
        assert_eq!(
            pricing(
                "decision:systemone:typesafe",
                &call.model,
                call.started_at_ms,
                &usage
            )
            .cost_usd,
            None
        );
        assert_eq!(
            pricing(&call.provider, "jev-2.0", call.started_at_ms, &usage).cost_usd,
            None
        );
        assert_eq!(
            pricing(&call.provider, &call.model, 0, &usage).cost_usd,
            None
        );
        assert_eq!(
            pricing(
                &call.provider,
                &call.model,
                call.started_at_ms,
                &TokenUsage::default()
            )
            .cost_usd,
            None
        );
    }

    #[test]
    fn decision_model_local_work_is_free_without_inventing_token_measurements() {
        for adapter in ["laya-mlx", "laya-onnx", "kev-mlx", "kev-onnx"] {
            let price = pricing(
                &format!("decision:{adapter}"),
                "local-model",
                1_790_467_200_000,
                &TokenUsage::default(),
            );
            assert_eq!(price.cost_usd, Some(0.0));
            assert_eq!(price.cost_source, Some(LlmCostSource::Local));
        }
    }

    #[test]
    fn decision_model_chat_total_includes_cost_and_never_turns_unknown_into_zero() {
        let mut total = crate::magician_v2::chat::models::ChatTurnUsage::default();
        total.record_call("openai", "test", None, 10, 5, 0, 0, 0, 0.01);
        total.record_decision_calls(&[fixture_call()]);
        assert_eq!(total.calls, 2);
        assert_eq!(total.input_tokens, 1010);
        assert!((total.cost_usd.unwrap() - 0.010042).abs() < 1e-12);
        assert!(!total.usage_availability.unwrap().cache_read);
        let mut failed = fixture_call();
        failed.input_tokens = None;
        failed.output_tokens = None;
        total.record_decision_calls(&[failed, fixture_call()]);
        assert_eq!(total.cost_usd, None);
        assert!(!total.usage_availability.unwrap().cost);
    }
}

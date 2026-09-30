//! Cache/reference identity tracks the actual explicitly bound incumbent profile.
use crate::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;

pub(crate) fn version(
    router: &OperationLlmRouter,
    operation: &str,
    prompt_version: &str,
) -> Option<String> {
    let config = router.router_config_snapshot()?;
    let selector = config.operation_mapping.get(operation)?;
    let name =
        selector.profile_for_locality(&magicllm::config::RequestShape::NONE, config.locality);
    let profile = config.profiles.get(name)?;
    let effective = router
        .get_config_for_operation(
            &crate::magician_v2::query_analysis::operation_llm_router::LLMOperation::from_str(
                operation,
            ),
        )
        .ok()?;
    let value = serde_json::to_value((
        operation,
        prompt_version,
        name,
        profile,
        effective,
        config.locality,
    ))
    .ok()?;
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
                serde_json::Value::Object(
                    sorted.into_iter().map(|(k, v)| (k, canonical(v))).collect(),
                )
            },
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            },
            other => other,
        }
    }
    Some(
        blake3::hash(&serde_json::to_vec(&canonical(value)).ok()?)
            .to_hex()
            .to_string(),
    )
}

/// Reserve a conservative upper cost for one replay before admission. Unknown
/// prices or output ceilings cannot be treated as free. The queue's live retry
/// ceiling is included; the additional factor covers provider-internal
/// max-token retries (up to three physical attempts in supported adapters).
pub(crate) fn observation_cost_upper_microusd(
    router: &OperationLlmRouter,
    operation: &str,
    snapshot_bytes: usize,
) -> Result<u64, &'static str> {
    if !router.observation_dispatch_available() {
        return Err("dispatch_unavailable");
    }
    let binding = crate::magician_v2::llm_dispatch_seam::resolve_bound_provider_for_operation(
        Some(router),
        operation,
    )
    .map_err(|_| "reference_unbound")?;
    if binding.kind == magicllm::LLMProviderKind::Ollama {
        return Ok(0);
    }
    let config = router.router_config_snapshot().ok_or("reference_unbound")?;
    let profile = config
        .profiles
        .get(&binding.profile)
        .ok_or("reference_unbound")?;
    if profile
        .chunking
        .as_ref()
        .is_some_and(|chunking| chunking.enabled)
    {
        // Logical map/reduce can produce several physical calls. A single
        // output ceiling cannot reserve their total cost safely.
        return Err("cost_unknown");
    }
    let output = profile.max_output_tokens.ok_or("cost_unknown")?;
    let input = snapshot_bytes
        .checked_mul(8)
        .and_then(|n| n.checked_add(16_384))
        .and_then(|n| u32::try_from(n).ok())
        .ok_or("cost_unknown")?;
    let window = profile
        .context_window_tokens
        .unwrap_or(1_000_000)
        .max(input);
    let quote = magicllm::pricing::active_table()
        .physical_attempt_quote_at(
            &binding.kind,
            &profile.model,
            window,
            chrono::Utc::now().timestamp_millis(),
        )
        .ok_or("cost_unknown")?;
    let attempts = router
        .observation_dispatch_attempt_ceiling()
        .ok_or("dispatch_unavailable")?
        .checked_mul(3)
        .ok_or("cost_unknown")?;
    quote
        .cost_upper_microusd(input.into(), output.into())
        .ok_or("cost_unknown")?
        .checked_mul(attempts)
        .ok_or("cost_unknown")
}

/// Explicit profile pin and JSON-format parity with the incumbent distill seam,
/// retaining the actual call receipt for comparison and cost reconciliation.
pub(crate) async fn pinned_json(
    router: &OperationLlmRouter,
    operation: &str,
    principal: &str,
    workspace: &str,
    system: &str,
    user: &str,
) -> anyhow::Result<crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse>
{
    use crate::magician_v2::{
        llm_dispatch_seam::resolve_bound_provider_for_operation,
        query_analysis::operation_llm_router::LLMOperation,
    };
    let binding = resolve_bound_provider_for_operation(Some(router), operation)
        .map_err(|error| anyhow::anyhow!("incumbent binding unavailable: {error:?}"))?;
    let scoped = router.with_scope_context(Some(magicllm::LlmScope::new(principal, workspace)));
    let operation = LLMOperation::Other(operation.into());
    let started = std::time::Instant::now();
    let response = if binding.kind == magicllm::LLMProviderKind::Ollama {
        scoped
            .generate_for_operation_with_system_pinned(
                &operation,
                Some(system),
                user,
                &binding.profile,
                Some(binding.kind),
            )
            .await
    } else {
        scoped
            .generate_for_operation_with_system_pinned_and_response_format(
                &operation,
                Some(system),
                user,
                &binding.profile,
                Some(binding.kind),
                magicllm::LLMResponseFormat::JsonObject,
            )
            .await
    }?;
    record_response(
        &response,
        principal,
        workspace,
        operation.as_str(),
        started.elapsed(),
    );
    Ok(response)
}

/// Replays always enter the dispatch queue's background lane. The production
/// pinned call above keeps its owner's normal priority.
pub(crate) async fn pinned_json_observation(
    router: &OperationLlmRouter,
    operation: &str,
    principal: &str,
    workspace: &str,
    system: &str,
    user: &str,
) -> anyhow::Result<crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse>
{
    anyhow::ensure!(
        router.observation_dispatch_available(),
        "observation requires the active LLM dispatch queue"
    );
    pinned_json(
        &router.with_dispatch_priority(magicllm::dispatch::Priority::Background),
        operation,
        principal,
        workspace,
        system,
        user,
    )
    .await
}

/// Direct memory callers without an owner telemetry bridge must publish their
/// actual provider receipt once, including when later output validation fails.
pub(crate) fn record_response(
    response: &crate::magician_v2::query_analysis::operation_llm_router::SimplifiedLLMResponse,
    principal: &str,
    workspace: &str,
    operation: &str,
    elapsed: std::time::Duration,
) {
    if let Some(bus) = crate::magician_v2::decision_host::telemetry_broadcaster() {
        // The compatibility event fields must agree with the real receipt.
        // An invented execution ID makes the canonical ledger reject a valid
        // paid call when the router already carries execution/chat lineage.
        let attribution = response.telemetry.as_ref()
            .and_then(|telemetry| telemetry.trace_receipt.as_ref())
            .map(|receipt| crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                task_id: receipt.context.task_id.clone(),
                root_execution_id: receipt.context.root_execution_id.clone(),
                execution_id: receipt.context.execution_id.clone(),
                chat_session_id: receipt.context.chat_session_id.clone(),
                ..Default::default()
            }).unwrap_or_default();
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
            bus,
            principal,
            workspace,
            "memory_decision_incumbent",
        )
        .emit_success(operation, response, elapsed.as_millis() as u64, attribution);
    }
}

/// Recheck qualification at the existing mutation boundary, after any awaits.
#[derive(Clone)]
pub(crate) struct ApplyGuard {
    authority: crate::magician_v2::decision_host::classification::Participation,
    router: OperationLlmRouter,
    operation: String,
    prompt_version: String,
    reference: String,
    source_check: Option<std::sync::Arc<dyn Fn() -> bool + Send + Sync>>,
}
impl std::fmt::Debug for ApplyGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DecisionApplyGuard")
    }
}
impl ApplyGuard {
    pub fn new(
        authority: crate::magician_v2::decision_host::classification::Participation,
        router: &OperationLlmRouter,
        operation: &str,
        prompt_version: &str,
        reference: &str,
    ) -> Self {
        Self {
            authority,
            router: router.clone(),
            operation: operation.into(),
            prompt_version: prompt_version.into(),
            reference: reference.into(),
            source_check: None,
        }
    }
    pub fn with_source_check(mut self, check: impl Fn() -> bool + Send + Sync + 'static) -> Self {
        self.source_check = Some(std::sync::Arc::new(check));
        self
    }
    pub async fn revalidate(&self) -> bool {
        self.authority.revalidate().await && self.current()
    }
    pub fn current(&self) -> bool {
        self.authority.is_current()
            && self.source_check.as_ref().is_none_or(|check| check())
            && version(&self.router, &self.operation, &self.prompt_version).as_ref()
                == Some(&self.reference)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::query_analysis::operation_llm_router::{
        OperationRoutingEndpoint, OperationRoutingOverrides,
    };
    #[test]
    fn observation_refuses_to_bypass_dispatch_queue() {
        let router = OperationLlmRouter::new(None);
        assert_eq!(
            observation_cost_upper_microusd(&router, "memory_utility_review", 100),
            Err("dispatch_unavailable")
        );
    }
    #[test]
    fn memory_decision_reference_tracks_explicit_and_effective_profiles_and_reload() {
        let mut config = magicllm::config::LLMRouterConfig::default();
        config.profiles.insert(
            "base".into(),
            serde_json::from_value(serde_json::json!({"provider":"ollama","model":"base"}))
                .unwrap(),
        );
        config.profiles.insert(
            "override".into(),
            serde_json::from_value(serde_json::json!({"provider":"ollama","model":"override"}))
                .unwrap(),
        );
        config.default_profile = "base".into();
        config.operation_mapping.insert(
            "memory_conflict_review".into(),
            serde_json::from_value(serde_json::json!("base")).unwrap(),
        );
        let router = OperationLlmRouter::new(Some(config.clone()));
        let first = version(&router, "memory_conflict_review", "p1").unwrap();
        assert_eq!(
            Some(first.clone()),
            version(&router, "memory_conflict_review", "p1")
        );
        assert!(version(&router, "unbound", "p1").is_none());
        let mut overrides = OperationRoutingOverrides::default();
        overrides.operations.insert(
            "memory_conflict_review".into(),
            OperationRoutingEndpoint::for_profile("override").unwrap(),
        );
        let scoped = router.with_routing_overrides(Some(overrides));
        assert_ne!(
            Some(first.clone()),
            version(&scoped, "memory_conflict_review", "p1")
        );
        assert_ne!(
            Some(first.clone()),
            version(&router, "memory_conflict_review", "p2")
        );
        let mut family_override = OperationRoutingOverrides::default();
        family_override.memory_consolidation = OperationRoutingEndpoint::for_profile("override");
        let family_router = router.with_routing_overrides(Some(family_override));
        assert_ne!(
            Some(first.clone()),
            version(&family_router, "memory_conflict_review", "p1"),
            "typed memory-family overrides must invalidate the reference too"
        );

        config.profiles.get_mut("base").unwrap().model = "reloaded".into();
        router.reload_from_config(Some(config));
        assert_ne!(
            Some(first),
            version(&router, "memory_conflict_review", "p1")
        );
    }
}

use magicllm::{
    config::OperationProfileSelector, plan_logical_request, ChunkBudget, ChunkFallbackPolicy,
    ConservativeOllamaEstimator, LLMProviderKind, LLMRequest, LLMRouterConfig, LogicalLlmRequest,
};
use serde::Serialize;
use serde_json::Value;

use magician::magician_v2::llm_chunking::{
    ChunkAdapterInventoryEntry, ChunkDomainAdapterRegistry, CHUNK_RELEASE_CANDIDATES,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkCandidateDiagnostics {
    pub operation: String,
    pub authoritative_profile: Option<String>,
    pub expected_baseline_profile: String,
    pub candidate_profile: String,
    pub adapter_id: String,
    pub profile_present: bool,
    pub profile_enabled: Option<bool>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub physical_window_tokens: Option<u32>,
    pub logical_window_tokens: Option<u32>,
    pub target_payload_tokens: Option<u32>,
    pub safety_margin_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub fallback_policy: Option<ChunkFallbackPolicy>,
    pub candidate_is_authoritative: bool,
    pub shadow_ready: bool,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkReleaseReadiness {
    pub status: &'static str,
    pub activation_permitted: bool,
    pub production_behavior_unchanged: bool,
    pub shadow_ready: bool,
    pub adapter_count: usize,
    pub adapters: Vec<ChunkAdapterInventoryEntry>,
    pub candidates: Vec<ChunkCandidateDiagnostics>,
    pub issues: Vec<String>,
}

/// Validate release candidates more strictly than normal router startup.
/// A candidate is coherent when it is either fully dormant (disabled and
/// baseline-mapped) or fully activated (enabled and authoritative). Mixed
/// profile/mapping states fail closed so a reload cannot expose a monolithic
/// request to an enabled physical-context policy.
pub fn build_release_readiness(
    config: &LLMRouterConfig,
    registry: &ChunkDomainAdapterRegistry,
) -> ChunkReleaseReadiness {
    let adapters = registry.inventory();
    let mut candidates = Vec::with_capacity(CHUNK_RELEASE_CANDIDATES.len());
    let mut all_issues = Vec::new();

    for spec in CHUNK_RELEASE_CANDIDATES {
        let authoritative_profile = config
            .operation_mapping
            .get(spec.operation)
            .map(|selector| selector.default_profile().to_string());
        let candidate_is_authoritative = selector_references_profile(
            config.operation_mapping.get(spec.operation),
            spec.candidate_profile,
        );
        let profile = config.profiles.get(spec.candidate_profile);
        let chunking = profile.and_then(|profile| profile.chunking.as_ref());
        let mut issues = Vec::new();

        let Some(profile) = profile else {
            issues.push("candidate profile is missing".to_string());
            all_issues.extend(
                issues
                    .iter()
                    .map(|issue| format!("{}: {issue}", spec.operation)),
            );
            candidates.push(ChunkCandidateDiagnostics {
                operation: spec.operation.to_string(),
                authoritative_profile,
                expected_baseline_profile: spec.baseline_profile.to_string(),
                candidate_profile: spec.candidate_profile.to_string(),
                adapter_id: spec.adapter_id.to_string(),
                profile_present: false,
                profile_enabled: None,
                provider: None,
                model: None,
                physical_window_tokens: None,
                logical_window_tokens: None,
                target_payload_tokens: None,
                safety_margin_tokens: None,
                max_output_tokens: None,
                fallback_policy: None,
                candidate_is_authoritative,
                shadow_ready: false,
                issues,
            });
            continue;
        };

        if profile.provider != LLMProviderKind::Ollama {
            issues.push("candidate provider must be ollama".to_string());
        }
        if profile.model != "gemma4:12b" {
            issues.push("candidate model must be exactly gemma4:12b".to_string());
        }
        if profile.api_base_url.as_deref() != Some("http://localhost:11434/api/generate") {
            issues.push(
                "candidate api_base_url must be exactly http://localhost:11434/api/generate"
                    .to_string(),
            );
        }
        if profile.context_window_tokens != Some(32_768) {
            issues.push("physical context must be exactly 32768 tokens".to_string());
        }
        if profile.max_output_tokens != Some(4_096) {
            issues.push("reserved output must be exactly 4096 tokens".to_string());
        }
        if profile.timeout_secs != Some(300) {
            issues.push("candidate timeout must be exactly 300 seconds".to_string());
        }
        let metadata = profile.metadata.as_ref();
        if metadata
            .and_then(|values| values.get("format"))
            .and_then(Value::as_str)
            != Some("json")
        {
            issues.push("candidate metadata.format must be exactly json".to_string());
        }
        if metadata
            .and_then(|values| values.get("options"))
            .and_then(|options| options.get("num_ctx"))
            .and_then(Value::as_u64)
            != Some(32_768)
        {
            issues.push("candidate metadata.options.num_ctx must be exactly 32768".to_string());
        }
        if metadata
            .and_then(|values| values.get("options"))
            .and_then(|options| options.get("draft_num_predict"))
            .and_then(Value::as_u64)
            != Some(4)
        {
            issues
                .push("candidate metadata.options.draft_num_predict must be exactly 4".to_string());
        }
        if metadata
            .and_then(|values| values.get("tool_choice"))
            .and_then(|choice| choice.get("type"))
            .and_then(Value::as_str)
            != Some("none")
        {
            issues.push("candidate metadata.tool_choice.type must be exactly none".to_string());
        }
        match chunking {
            Some(policy) => {
                if policy.enabled && !candidate_is_authoritative {
                    issues.push(
                        "enabled candidate is not the authoritative operation mapping".to_string(),
                    );
                }
                if !policy.enabled
                    && authoritative_profile.as_deref() != Some(spec.baseline_profile)
                {
                    issues.push(format!(
                        "disabled candidate requires baseline mapping `{}`; found {:?}",
                        spec.baseline_profile, authoritative_profile
                    ));
                }
                if candidate_is_authoritative && !policy.enabled {
                    issues
                        .push("authoritative candidate has logical chunking disabled".to_string());
                }
                if policy.adapter.as_deref() != Some(spec.adapter_id) {
                    issues.push(format!(
                        "adapter is {:?}; expected `{}`",
                        policy.adapter, spec.adapter_id
                    ));
                }
                if policy.logical_window_tokens != Some(262_144) {
                    issues.push("logical window must be exactly 262144 tokens".to_string());
                }
                if policy.target_payload_tokens != Some(24_576) {
                    issues.push("target payload must be exactly 24576 tokens".to_string());
                }
                if policy.safety_margin_tokens != 2_048 {
                    issues.push("safety margin must be exactly 2048 tokens".to_string());
                }
                if policy.fallback_policy != ChunkFallbackPolicy::SameProviderOnly {
                    issues.push("fallback must be same_provider_only".to_string());
                }
                if policy.fallback_profile.is_some() {
                    issues.push(
                        "same-provider local profile must not name a fallback profile".to_string(),
                    );
                }
            },
            None => issues.push("chunking policy is missing".to_string()),
        }
        match registry.get(spec.adapter_id) {
            Some(adapter) if adapter.supported_operations().contains(&spec.operation) => {},
            Some(_) => issues.push("registered adapter does not advertise operation".to_string()),
            None => issues.push("adapter is not registered".to_string()),
        }

        let shadow_ready = issues.is_empty();
        all_issues.extend(
            issues
                .iter()
                .map(|issue| format!("{}: {issue}", spec.operation)),
        );
        candidates.push(ChunkCandidateDiagnostics {
            operation: spec.operation.to_string(),
            authoritative_profile,
            expected_baseline_profile: spec.baseline_profile.to_string(),
            candidate_profile: spec.candidate_profile.to_string(),
            adapter_id: spec.adapter_id.to_string(),
            profile_present: true,
            profile_enabled: chunking.map(|policy| policy.enabled),
            provider: Some(format!("{:?}", profile.provider).to_lowercase()),
            model: Some(profile.model.clone()),
            physical_window_tokens: profile.context_window_tokens,
            logical_window_tokens: chunking.and_then(|policy| policy.logical_window_tokens),
            target_payload_tokens: chunking.and_then(|policy| policy.target_payload_tokens),
            safety_margin_tokens: chunking.map(|policy| policy.safety_margin_tokens),
            max_output_tokens: profile.max_output_tokens,
            fallback_policy: chunking.map(|policy| policy.fallback_policy),
            candidate_is_authoritative,
            shadow_ready,
            issues,
        });
    }

    all_issues.sort();
    let production_behavior_unchanged = candidates.iter().all(|candidate| {
        !candidate.candidate_is_authoritative && candidate.profile_enabled == Some(false)
    });
    let shadow_ready = all_issues.is_empty();
    let active_count = candidates
        .iter()
        .filter(|candidate| {
            candidate.candidate_is_authoritative && candidate.profile_enabled == Some(true)
        })
        .count();
    ChunkReleaseReadiness {
        status: if !shadow_ready {
            "not_ready"
        } else if active_count == candidates.len() {
            "activated"
        } else if active_count > 0 {
            "partial_activation"
        } else {
            "dormant_shadow_ready"
        },
        // Configuration coherence is necessary but not sufficient for release
        // approval. The checked-in activation manifest remains unapproved
        // until its live evidence and operational gates are satisfied.
        activation_permitted: false,
        production_behavior_unchanged,
        shadow_ready,
        adapter_count: adapters.len(),
        adapters,
        candidates,
        issues: all_issues,
    }
}

fn selector_references_profile(
    selector: Option<&OperationProfileSelector>,
    profile_name: &str,
) -> bool {
    selector.is_some_and(|selector| selector.default_profile() == profile_name)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanOnlyDiagnostics {
    pub adapter_id: String,
    pub adapter_version: String,
    pub operation: String,
    pub model: String,
    pub estimator: String,
    pub estimated_logical_tokens: u32,
    pub source_item_count: usize,
    pub terminal_item_count: usize,
    pub oversized_split_count: usize,
    pub chunk_count: usize,
    pub estimated_chunk_payload_tokens: Vec<u32>,
    pub physical_window_tokens: u32,
    pub logical_window_tokens: u32,
    pub effective_payload_tokens: u32,
    pub provider_calls: u32,
    pub durable_writes: u32,
}

/// Exercise the real adapter and generic planner without a provider call or a
/// persistence handle. This is the safe first lane in the Phase 6 evaluator.
pub fn build_plan_only_diagnostics(
    registry: &ChunkDomainAdapterRegistry,
    adapter_id: &str,
    operation: &str,
    input: Value,
    model: &str,
    budget: ChunkBudget,
) -> Result<PlanOnlyDiagnostics, String> {
    let adapter = registry
        .get(adapter_id)
        .ok_or_else(|| format!("adapter `{adapter_id}` is not registered"))?;
    let request = LogicalLlmRequest {
        operation: operation.to_string(),
        input,
        base_request: LLMRequest {
            model: model.to_string(),
            ..LLMRequest::default()
        },
    };
    let plan = plan_logical_request(
        adapter.as_ref(),
        &request,
        budget,
        &ConservativeOllamaEstimator,
    )
    .map_err(|error| error.to_string())?;
    Ok(PlanOnlyDiagnostics {
        adapter_id: plan.adapter_id,
        adapter_version: plan.adapter_version,
        operation: plan.operation,
        model: plan.model,
        estimator: plan.estimator,
        estimated_logical_tokens: plan.estimated_logical_tokens,
        source_item_count: plan.source_identities.len(),
        terminal_item_count: plan.leaf_identities.len(),
        oversized_split_count: plan
            .leaf_identities
            .len()
            .saturating_sub(plan.source_identities.len()),
        chunk_count: plan.chunks.len(),
        estimated_chunk_payload_tokens: plan
            .chunks
            .iter()
            .map(|chunk| chunk.estimated_payload_tokens)
            .collect(),
        physical_window_tokens: plan.budget.physical_window_tokens,
        logical_window_tokens: plan.budget.logical_window_tokens,
        effective_payload_tokens: plan.budget.effective_payload_tokens,
        provider_calls: 0,
        durable_writes: 0,
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use magician::config::MagicianConfig;

    fn ensure_builtins_registered() {
        // The lib-side global starts empty (builtins register at bin boot);
        // in-crate tests register them directly — same lib instance, so the
        // registry the assertions read is the one this call mutates.
        crate::register_builtin_chunk_adapters().expect("register builtins");
    }

    #[test]
    fn shipped_template_is_fully_activated_and_coherent() {
        let config: MagicianConfig =
            serde_yaml::from_str(&magician::config::shipped_repo_config_yaml())
                .expect("template config parses");
        let router = config.router_config().expect("template router");
        ensure_builtins_registered();
        let registry = magician::magician_v2::llm_chunking::global_chunk_adapter_registry()
            .read()
            .expect("chunk registry");
        let readiness = build_release_readiness(router, &registry);
        assert!(readiness.shadow_ready, "{:?}", readiness.issues);
        assert!(!readiness.production_behavior_unchanged);
        assert!(!readiness.activation_permitted);
        assert_eq!(readiness.status, "activated");
        assert_eq!(readiness.candidates.len(), CHUNK_RELEASE_CANDIDATES.len());
        assert!(readiness
            .candidates
            .iter()
            .all(|candidate| candidate.profile_enabled == Some(true)
                && candidate.candidate_is_authoritative));
    }

    #[test]
    fn unreviewed_candidate_model_fails_readiness() {
        let config: MagicianConfig =
            serde_yaml::from_str(&magician::config::shipped_repo_config_yaml()).unwrap();
        let mut router = config.router_config().unwrap().clone();
        let candidate = &CHUNK_RELEASE_CANDIDATES[0];
        router
            .profiles
            .get_mut(candidate.candidate_profile)
            .unwrap()
            .model = "unreviewed-model".into();
        ensure_builtins_registered();
        let registry = magician::magician_v2::llm_chunking::global_chunk_adapter_registry()
            .read()
            .unwrap();
        let readiness = build_release_readiness(&router, &registry);
        assert!(!readiness.shadow_ready);
        assert!(readiness.issues.iter().any(|issue| {
            issue.contains(candidate.operation)
                && issue.contains("model must be exactly gemma4:12b")
        }));
    }

    #[test]
    fn enabled_profile_without_authoritative_mapping_fails_readiness() {
        let config: MagicianConfig =
            serde_yaml::from_str(&magician::config::shipped_repo_config_yaml())
                .expect("template config parses");
        let mut router = config.router_config().expect("template router").clone();
        router.operation_mapping.insert(
            "memory_entity_extraction".to_string(),
            OperationProfileSelector::Simple("op-memory-entity-extraction-fast".to_string()),
        );
        ensure_builtins_registered();
        let registry = magician::magician_v2::llm_chunking::global_chunk_adapter_registry()
            .read()
            .expect("chunk registry");
        let readiness = build_release_readiness(&router, &registry);
        assert!(!readiness.shadow_ready);
        assert!(readiness.issues.iter().any(|issue| {
            issue.contains("memory_entity_extraction")
                && issue.contains("not the authoritative operation mapping")
        }));
        let startup_error =
            magician::magician_v2::llm_chunking::validate_router_chunking_config_with_registry(
                &router, &registry,
            )
            .expect_err("startup validation rejects the same half-state");
        assert!(startup_error.to_string().contains("half-activated"));
    }

    #[test]
    fn runtime_inventory_exposes_every_phase6_contract() {
        ensure_builtins_registered();
        let registry = magician::magician_v2::llm_chunking::global_chunk_adapter_registry()
            .read()
            .expect("chunk registry");
        let inventory = registry.inventory();
        for expected in CHUNK_RELEASE_CANDIDATES {
            let adapter = inventory
                .iter()
                .find(|entry| entry.adapter_id == expected.adapter_id)
                .expect("candidate adapter is registered");
            assert!(adapter.final_validator_available);
            assert!(adapter
                .supported_operations
                .iter()
                .any(|operation| operation == expected.operation));
        }
    }
}

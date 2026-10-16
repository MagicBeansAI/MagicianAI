use std::{
    collections::HashMap,
    sync::{Arc, OnceLock, RwLock},
};

use magicllm::{
    config::OperationProfileSelector, ChunkDomainAdapter, ChunkFallbackPolicy,
    FinalValidationContract, LLMProfile, LLMProviderKind, LLMRouterConfig,
};
use serde::Serialize;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChunkAdapterRegistryError {
    #[error("invalid chunk adapter registration: {0}")]
    InvalidRegistration(String),
    #[error("chunk adapter `{0}` is already registered")]
    DuplicateAdapter(String),
    #[error("chunk adapter registry lock is poisoned")]
    RegistryPoisoned,
    #[error("invalid chunk adapter configuration: {issues:?}")]
    InvalidConfiguration { issues: Vec<String> },
}

/// Safe runtime inventory for diagnostics and release-readiness reports.
/// Adapter implementations remain process-local; only their declared
/// compatibility contract is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ChunkAdapterInventoryEntry {
    pub adapter_id: String,
    pub version: String,
    pub supported_operations: Vec<String>,
    pub final_validator_available: bool,
}

/// Explicit adapter registry. Configuration plus the production structured
/// request boundary decide whether a registered adapter is dormant or active.
#[derive(Clone, Default)]
pub struct ChunkDomainAdapterRegistry {
    adapters: HashMap<String, Arc<dyn ChunkDomainAdapter>>,
}

impl ChunkDomainAdapterRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        adapter: Arc<dyn ChunkDomainAdapter>,
    ) -> Result<(), ChunkAdapterRegistryError> {
        let id = adapter.id().trim();
        if id.is_empty() {
            return Err(ChunkAdapterRegistryError::InvalidRegistration(
                "adapter id must not be empty".to_string(),
            ));
        }
        if adapter.version().trim().is_empty() {
            return Err(ChunkAdapterRegistryError::InvalidRegistration(format!(
                "adapter `{id}` version must not be empty"
            )));
        }
        if adapter.supported_operations().is_empty()
            || adapter
                .supported_operations()
                .iter()
                .any(|operation| operation.trim().is_empty())
        {
            return Err(ChunkAdapterRegistryError::InvalidRegistration(format!(
                "adapter `{id}` must advertise at least one non-empty operation"
            )));
        }
        if self.adapters.contains_key(id) {
            return Err(ChunkAdapterRegistryError::DuplicateAdapter(id.to_string()));
        }
        self.adapters.insert(id.to_string(), adapter);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<Arc<dyn ChunkDomainAdapter>> {
        self.adapters.get(id).cloned()
    }

    pub fn contains(&self, id: &str) -> bool {
        self.adapters.contains_key(id)
    }

    pub fn len(&self) -> usize {
        self.adapters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }

    /// Return a stable, non-sensitive adapter inventory. Sorting makes the
    /// endpoint and saved readiness evidence diffable across releases.
    pub fn inventory(&self) -> Vec<ChunkAdapterInventoryEntry> {
        let mut entries = self
            .adapters
            .values()
            .map(|adapter| {
                let mut supported_operations = adapter
                    .supported_operations()
                    .iter()
                    .map(|operation| (*operation).to_string())
                    .collect::<Vec<_>>();
                supported_operations.sort();
                ChunkAdapterInventoryEntry {
                    adapter_id: adapter.id().to_string(),
                    version: adapter.version().to_string(),
                    supported_operations,
                    final_validator_available: adapter.final_validation_contract()
                        == FinalValidationContract::Available,
                }
            })
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.adapter_id.cmp(&right.adapter_id));
        entries
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkReleaseCandidateSpec {
    pub operation: &'static str,
    pub baseline_profile: &'static str,
    pub candidate_profile: &'static str,
    pub adapter_id: &'static str,
}

pub const CHUNK_RELEASE_CANDIDATES: &[ChunkReleaseCandidateSpec] = &[
    ChunkReleaseCandidateSpec {
        operation: "memory_temperature_utility_review",
        baseline_profile: "op-memory-episode-quality-standard",
        candidate_profile: "op-memory-utility-review-local-chunked",
        adapter_id: "memory_utility_review_v1",
    },
    ChunkReleaseCandidateSpec {
        operation: "memory_episode_quality_classification",
        baseline_profile: "op-memory-episode-quality-standard",
        candidate_profile: "op-memory-episode-quality-local-chunked",
        adapter_id: "memory_episode_quality_v1",
    },
    ChunkReleaseCandidateSpec {
        operation: "memory_entity_extraction",
        baseline_profile: "op-memory-entity-extraction-fast",
        candidate_profile: "op-memory-entity-extraction-local-chunked",
        adapter_id: "memory_entities_v1",
    },
    ChunkReleaseCandidateSpec {
        operation: "distill_evidence",
        baseline_profile: "op-memory-evidence-distillation-fast",
        candidate_profile: "op-memory-evidence-distillation-local-chunked",
        adapter_id: "evidence_distill_v1",
    },
    ChunkReleaseCandidateSpec {
        operation: "memory_environment_knowledge_extraction",
        baseline_profile: "op-memory-environment-extraction-fast",
        candidate_profile: "op-memory-environment-extraction-local-chunked",
        adapter_id: "memory_environment_v1",
    },
    ChunkReleaseCandidateSpec {
        operation: "memory_archive_summary",
        baseline_profile: "op-memory-archive-summary-fast",
        candidate_profile: "op-memory-archive-summary-local-chunked",
        adapter_id: "memory_archive_v1",
    },
];

static GLOBAL_CHUNK_ADAPTER_REGISTRY: OnceLock<RwLock<ChunkDomainAdapterRegistry>> =
    OnceLock::new();

pub fn global_chunk_adapter_registry() -> &'static RwLock<ChunkDomainAdapterRegistry> {
    // Built-in domain adapters live in the magician-chunking crate and are
    // registered at boot via `register_builtin_chunk_adapters()`; the lib-side
    // registry starts empty so it never depends on the satellite crate.
    GLOBAL_CHUNK_ADAPTER_REGISTRY.get_or_init(|| RwLock::new(ChunkDomainAdapterRegistry::new()))
}

pub fn register_global_chunk_adapter(
    adapter: Arc<dyn ChunkDomainAdapter>,
) -> Result<(), ChunkAdapterRegistryError> {
    global_chunk_adapter_registry()
        .write()
        .map_err(|_| ChunkAdapterRegistryError::RegistryPoisoned)?
        .register(adapter)
}

/// Validate enabled chunking profiles against the process registry.
pub fn validate_router_chunking_config(
    config: &LLMRouterConfig,
) -> Result<(), ChunkAdapterRegistryError> {
    let registry = global_chunk_adapter_registry()
        .read()
        .map_err(|_| ChunkAdapterRegistryError::RegistryPoisoned)?;
    validate_router_chunking_config_with_registry(config, &registry)
}

/// Testable/config-loader form of global validation.
pub fn validate_router_chunking_config_with_registry(
    config: &LLMRouterConfig,
    registry: &ChunkDomainAdapterRegistry,
) -> Result<(), ChunkAdapterRegistryError> {
    let mut issues = Vec::new();

    for (profile_name, profile) in &config.profiles {
        let Some(chunking) = profile.chunking.as_ref().filter(|policy| policy.enabled) else {
            continue;
        };
        if profile.provider != LLMProviderKind::Ollama {
            issues.push(format!(
                "profile `{profile_name}` enables logical chunking for non-Ollama provider {:?}",
                profile.provider
            ));
        }
        if chunking.fallback_policy == ChunkFallbackPolicy::MappedProfile {
            if let Some(fallback_name) = chunking.fallback_profile.as_deref() {
                if config
                    .profiles
                    .get(fallback_name)
                    .and_then(|fallback| fallback.chunking.as_ref())
                    .is_some_and(|fallback| fallback.enabled)
                {
                    issues.push(format!(
                        "profile `{profile_name}` maps chunk recovery to chunk-enabled profile `{fallback_name}`; recursive logical chunking is not allowed"
                    ));
                }
            }
        }
        let Some(adapter_id) = chunking
            .adapter
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        else {
            issues.push(format!(
                "profile `{profile_name}` enables logical chunking without an adapter"
            ));
            continue;
        };
        match registry.get(adapter_id) {
            Some(adapter)
                if adapter.final_validation_contract() == FinalValidationContract::Unavailable =>
            {
                issues.push(format!(
                    "profile `{profile_name}` references adapter `{adapter_id}` without a final validator"
                ));
            },
            Some(_) => {},
            None => issues.push(format!(
                "profile `{profile_name}` references unregistered adapter `{adapter_id}`"
            )),
        }
    }

    for (operation, selector) in &config.operation_mapping {
        let mut selected_names = Vec::with_capacity(2);
        match selector {
            OperationProfileSelector::Simple(name) => selected_names.push(name.as_str()),
            OperationProfileSelector::Conditional {
                default,
                when_has_images,
                when_cloud,
                ..
            } => {
                selected_names.push(default.as_str());
                if let Some(name) = when_has_images.as_deref() {
                    selected_names.push(name);
                }
                if let Some(name) = when_cloud.as_deref() {
                    selected_names.push(name);
                }
            },
        }
        for selected_name in selected_names {
            for (resolved_name, profile) in resolve_standard_profiles(config, selected_name) {
                validate_mapped_profile(operation, resolved_name, profile, registry, &mut issues);
            }
        }
    }

    for (resolved_name, profile) in resolve_standard_profiles(config, &config.default_profile) {
        let Some(chunking) = profile.chunking.as_ref().filter(|policy| policy.enabled) else {
            continue;
        };
        let Some(adapter_id) = chunking.adapter.as_deref() else {
            continue;
        };
        let Some(adapter) = registry.get(adapter_id) else {
            continue;
        };
        if !adapter.supported_operations().contains(&"*") {
            issues.push(format!(
                "chunk-enabled profile `{resolved_name}` cannot be the router default because adapter `{adapter_id}` does not advertise wildcard operation `*`"
            ));
        }
    }

    // Phase 7 release candidates must cross the behavior boundary atomically.
    // Enabling without mapping would leave dormant code unexpectedly live;
    // mapping without enabling would send the original monolithic request to
    // a 32K candidate. Reject both half-states during startup and hot reload.
    for spec in CHUNK_RELEASE_CANDIDATES {
        let Some(profile) = config.profiles.get(spec.candidate_profile) else {
            continue;
        };
        let enabled = profile
            .chunking
            .as_ref()
            .is_some_and(|policy| policy.enabled);
        let mapped = config
            .operation_mapping
            .get(spec.operation)
            .is_some_and(|selector| selector.default_profile() == spec.candidate_profile);
        if enabled != mapped {
            issues.push(format!(
                "Phase 7 candidate `{}` for operation `{}` is half-activated: enabled={}, mapped={}",
                spec.candidate_profile, spec.operation, enabled, mapped
            ));
        }
    }

    issues.sort();
    issues.dedup();
    if issues.is_empty() {
        Ok(())
    } else {
        Err(ChunkAdapterRegistryError::InvalidConfiguration { issues })
    }
}

fn resolve_standard_profiles<'a>(
    config: &'a LLMRouterConfig,
    selected_name: &'a str,
) -> Vec<(&'a str, &'a LLMProfile)> {
    if let Some(profile) = config.profiles.get(selected_name) {
        return vec![(selected_name, profile)];
    }
    let Some(adaptive) = config.adaptive_profiles.get(selected_name) else {
        return Vec::new();
    };
    let mut profiles = Vec::with_capacity(2);
    if let Some(profile) = config.profiles.get(&adaptive.fast_profile) {
        profiles.push((adaptive.fast_profile.as_str(), profile));
    }
    if let Some(profile) = config.profiles.get(&adaptive.thinking_profile) {
        profiles.push((adaptive.thinking_profile.as_str(), profile));
    }
    profiles
}

fn validate_mapped_profile(
    operation: &str,
    profile_name: &str,
    profile: &LLMProfile,
    registry: &ChunkDomainAdapterRegistry,
    issues: &mut Vec<String>,
) {
    let Some(chunking) = profile.chunking.as_ref().filter(|policy| policy.enabled) else {
        return;
    };
    let Some(adapter_id) = chunking.adapter.as_deref() else {
        return;
    };
    let Some(adapter) = registry.get(adapter_id) else {
        return;
    };
    if !adapter.supported_operations().contains(&operation) {
        issues.push(format!(
            "operation `{operation}` maps to chunk-enabled profile `{profile_name}`, but adapter `{adapter_id}` does not advertise that operation"
        ));
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::sync::Arc;

    use magicllm::{
        ChunkBudget, ChunkDomainAdapter, ChunkError, ChunkValidationError, FinalValidationContract,
        LLMRouterConfig, LogicalItem,
    };
    use serde_json::Value;

    use super::{
        validate_router_chunking_config_with_registry, ChunkAdapterRegistryError,
        ChunkDomainAdapterRegistry,
    };

    struct ValidFakeAdapter;

    impl ChunkDomainAdapter for ValidFakeAdapter {
        fn id(&self) -> &'static str {
            "fake_memory_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["memory_entity_extraction"]
        }

        fn final_validation_contract(&self) -> FinalValidationContract {
            FinalValidationContract::Available
        }

        fn logical_items(&self, _input: &Value) -> Result<Vec<LogicalItem>, ChunkError> {
            Ok(vec![LogicalItem::root("source-1", 0, Value::Null)])
        }

        fn split_oversized_item(
            &self,
            item: &LogicalItem,
            budget: &ChunkBudget,
        ) -> Result<Vec<LogicalItem>, ChunkError> {
            Err(ChunkError::ChunkItemExceedsContextWindow {
                adapter: self.id().to_string(),
                item_id: item.identity.id.clone(),
                estimated_tokens: budget.effective_payload_tokens.saturating_add(1),
                effective_payload_tokens: budget.effective_payload_tokens,
            })
        }

        fn validate_final(&self, _value: &Value) -> Result<(), ChunkValidationError> {
            Ok(())
        }

        fn validate_map_value(
            &self,
            _value: &Value,
            _chunk: &magicllm::ChunkDescriptor,
        ) -> Result<(), ChunkValidationError> {
            Ok(())
        }
    }

    struct WrongOperationAdapter;

    impl ChunkDomainAdapter for WrongOperationAdapter {
        fn id(&self) -> &'static str {
            "wrong_operation_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["memory_archive_summary"]
        }

        fn final_validation_contract(&self) -> FinalValidationContract {
            FinalValidationContract::Available
        }

        fn logical_items(&self, _input: &Value) -> Result<Vec<LogicalItem>, ChunkError> {
            Ok(Vec::new())
        }

        fn split_oversized_item(
            &self,
            _item: &LogicalItem,
            _budget: &ChunkBudget,
        ) -> Result<Vec<LogicalItem>, ChunkError> {
            Ok(Vec::new())
        }

        fn validate_final(&self, _value: &Value) -> Result<(), ChunkValidationError> {
            Ok(())
        }

        fn validate_map_value(
            &self,
            _value: &Value,
            _chunk: &magicllm::ChunkDescriptor,
        ) -> Result<(), ChunkValidationError> {
            Ok(())
        }
    }

    struct NoFinalValidatorAdapter;

    impl ChunkDomainAdapter for NoFinalValidatorAdapter {
        fn id(&self) -> &'static str {
            "no_final_validator_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["memory_entity_extraction"]
        }

        fn final_validation_contract(&self) -> FinalValidationContract {
            FinalValidationContract::Unavailable
        }

        fn logical_items(&self, _input: &Value) -> Result<Vec<LogicalItem>, ChunkError> {
            Ok(Vec::new())
        }

        fn split_oversized_item(
            &self,
            _item: &LogicalItem,
            _budget: &ChunkBudget,
        ) -> Result<Vec<LogicalItem>, ChunkError> {
            Ok(Vec::new())
        }

        fn validate_final(&self, _value: &Value) -> Result<(), ChunkValidationError> {
            Err(ChunkValidationError::HookUnavailable {
                adapter: self.id().to_string(),
                hook: "validate_final",
            })
        }

        fn validate_map_value(
            &self,
            _value: &Value,
            _chunk: &magicllm::ChunkDescriptor,
        ) -> Result<(), ChunkValidationError> {
            Ok(())
        }
    }

    fn enabled_config(adapter: &str) -> LLMRouterConfig {
        serde_yaml::from_str(&format!(
            r#"
profiles:
  baseline:
    provider: openai
    model: gpt-5.6-terra
  local_chunked:
    provider: ollama
    model: gemma4:12b
    context_window_tokens: 32768
    chunking:
      enabled: true
      adapter: {adapter}
      logical_window_tokens: 262144
      target_payload_tokens: 24576
      safety_margin_tokens: 2048
operation_mapping:
  memory_entity_extraction: local_chunked
default_profile: baseline
"#
        ))
        .unwrap()
    }

    #[test]
    fn valid_enabled_mapping_resolves_registered_adapter_and_operation() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry.register(Arc::new(ValidFakeAdapter)).unwrap();

        validate_router_chunking_config_with_registry(&enabled_config("fake_memory_v1"), &registry)
            .unwrap();
    }

    #[test]
    fn enabled_profile_rejects_missing_adapter() {
        let error = validate_router_chunking_config_with_registry(
            &enabled_config("missing_v1"),
            &ChunkDomainAdapterRegistry::new(),
        )
        .unwrap_err();

        assert!(matches!(
            error,
            ChunkAdapterRegistryError::InvalidConfiguration { .. }
        ));
        assert!(error
            .to_string()
            .contains("unregistered adapter `missing_v1`"));
    }

    #[test]
    fn mapped_operation_must_be_advertised_by_adapter() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry.register(Arc::new(WrongOperationAdapter)).unwrap();

        let error = validate_router_chunking_config_with_registry(
            &enabled_config("wrong_operation_v1"),
            &registry,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("does not advertise that operation"));
    }

    #[test]
    fn domain_adapter_cannot_back_unmapped_default_operations() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry.register(Arc::new(ValidFakeAdapter)).unwrap();
        let mut config = enabled_config("fake_memory_v1");
        config.default_profile = "local_chunked".to_string();

        let error = validate_router_chunking_config_with_registry(&config, &registry).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not advertise wildcard operation `*`"));
    }

    #[test]
    fn enabled_profile_requires_final_validator_contract() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry
            .register(Arc::new(NoFinalValidatorAdapter))
            .unwrap();

        let error = validate_router_chunking_config_with_registry(
            &enabled_config("no_final_validator_v1"),
            &registry,
        )
        .unwrap_err();
        assert!(error.to_string().contains("without a final validator"));
    }

    #[test]
    fn disabled_policy_does_not_require_registered_adapter() {
        let config: LLMRouterConfig = serde_yaml::from_str(
            r#"
profiles:
  local_observe_only:
    provider: ollama
    model: gemma4:12b
    context_window_tokens: 32768
    chunking:
      enabled: false
      adapter: not_registered_yet
operation_mapping:
  memory_entity_extraction: local_observe_only
default_profile: local_observe_only
"#,
        )
        .unwrap();

        validate_router_chunking_config_with_registry(&config, &ChunkDomainAdapterRegistry::new())
            .unwrap();
    }

    #[test]
    fn mapped_fallback_cannot_target_another_chunk_enabled_profile() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry.register(Arc::new(ValidFakeAdapter)).unwrap();
        let config: LLMRouterConfig = serde_yaml::from_str(
            r#"
profiles:
  baseline:
    provider: openai
    model: gpt-5.6-terra
  local_primary:
    provider: ollama
    model: gemma4:12b
    context_window_tokens: 32768
    chunking:
      enabled: true
      adapter: fake_memory_v1
      logical_window_tokens: 262144
      target_payload_tokens: 24576
      fallback_policy: mapped_profile
      fallback_profile: local_recursive
  local_recursive:
    provider: ollama
    model: gemma4:12b
    context_window_tokens: 32768
    chunking:
      enabled: true
      adapter: fake_memory_v1
      logical_window_tokens: 262144
      target_payload_tokens: 24576
operation_mapping:
  memory_entity_extraction: local_primary
default_profile: baseline
"#,
        )
        .unwrap();

        let error = validate_router_chunking_config_with_registry(&config, &registry).unwrap_err();
        assert!(error
            .to_string()
            .contains("recursive logical chunking is not allowed"));
    }

    #[test]
    fn duplicate_adapter_ids_are_rejected() {
        let mut registry = ChunkDomainAdapterRegistry::new();
        registry.register(Arc::new(ValidFakeAdapter)).unwrap();
        assert!(matches!(
            registry.register(Arc::new(ValidFakeAdapter)),
            Err(ChunkAdapterRegistryError::DuplicateAdapter(id)) if id == "fake_memory_v1"
        ));
    }
}

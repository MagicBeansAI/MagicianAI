// Multi-LLM service for operation-based model selection
// Enables using different LLM models for different operations (e.g., large
// model for decomposition, small model for analysis)

use std::{
    collections::HashMap,
    sync::atomic::{AtomicU32, Ordering},
    sync::{Arc, OnceLock, RwLock, RwLockReadGuard},
};

use anyhow::{anyhow, Result};
use async_trait::async_trait;
use magicllm::capability::LLMProviderKind;
use magicllm::prelude::{
    as_anthropic_raw_content_block, ConfiguredRouter, ContentBlock as RouterContentBlock,
    LLMMessage as RouterMessage, LLMProfile, LLMRequest as RouterRequest,
    LLMResponse as RouterResponse, LLMRouterConfig, LLMToolCall as RouterToolCall,
    LLMToolSpec as RouterToolSpec, PromptCacheConfig, ReasoningConfig as RouterReasoningConfig,
    ReasoningDefaults, RequestMetadata as RouterMetadata, StreamDelta,
    TokenUsage as RouterTokenUsage,
};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::magician_v2::analytics::runtime_activity_layer::current_activity_id;
use crate::magician_v2::llm_chunking::validate_router_chunking_config;
use crate::magician_v2::slot_graph::extraction::LlmCallTelemetry;

use magicllm::prelude::LlmConfig;

/// Resolve a chat-eligible Magician profile. Adaptive choices use their fast
/// profile for callers that do not implement chat's thinking-mode escalation.
pub fn resolve_chat_profile_config(config: &LLMRouterConfig, name: &str) -> Result<LlmConfig> {
    let resolved = config
        .resolve_profile(name)
        .ok_or_else(|| anyhow!("LLM profile '{}' not found", name))?;
    let profile = match resolved {
        magicllm::config::ResolvedProfile::Standard(profile) => profile,
        magicllm::config::ResolvedProfile::Adaptive { fast, .. } => fast,
    };
    let result = llm_config_from_profile(profile);
    if !MultiLLMService::is_chat_profile_eligible(&result) {
        return Err(anyhow!("LLM profile '{}' is not chat-eligible", name));
    }
    Ok(result)
}

/// Resolve a chat profile that Pi can install in its isolated Plane process.
pub fn resolve_pi_profile_config(config: &LLMRouterConfig, name: &str) -> Result<LlmConfig> {
    let result = resolve_chat_profile_config(config, name)?;
    if result.model.trim().is_empty() {
        return Err(anyhow!("LLM profile '{}' has no model for Pi", name));
    }
    if !matches!(
        result.provider.as_str(),
        "openai"
            | "anthropic"
            | "gemini"
            | "openrouter"
            | "deepseek"
            | "minimax"
            | "xai"
            | "ollama"
    ) && result.api_base_url.is_none()
    {
        return Err(anyhow!(
            "LLM profile '{}' needs an API base URL for Pi provider '{}'",
            name,
            result.provider
        ));
    }
    Ok(result)
}

fn llm_config_from_profile(profile: &LLMProfile) -> LlmConfig {
    LlmConfig {
        provider: profile.provider.clone(),
        model: profile.model.clone(),
        api_key_env: profile.api_key_env.clone(),
        api_base_url: profile.api_base_url.clone(),
        max_tokens: profile.max_output_tokens,
        temperature: profile.temperature,
        reasoning_effort: profile.reasoning.as_ref().map(|r| r.effort.clone()),
        verbosity: profile
            .metadata
            .as_ref()
            .and_then(|m| m.get("verbosity"))
            .and_then(Value::as_str)
            .map(str::to_string),
        additional_params: profile.metadata.clone(),
        supports_vision: profile.supports_vision,
        supports_reasoning: profile.supports_reasoning,
        supports_tool_calling: profile.supports_tool_calling,
        supports_computer_use: profile.supports_computer_use,
    }
}

/// Info about a chat-eligible LLM profile.
///
/// `is_adaptive` distinguishes composite adaptive profiles
/// (`fast_profile` + `thinking_profile` pair) from standard ones. The UI
/// surfaces adaptive entries in their own section so users can pick
/// "Adaptive (auto-escalates to thinking)" vs a fixed standard profile.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatProfileInfo {
    pub name: String,
    pub provider: String,
    pub model: String,
    pub is_default: bool,
    pub supports_user_image_inputs: bool,
    /// True iff this profile is in `LLMRouterConfig::adaptive_profiles`.
    /// Adaptive profiles auto-escalate from a fast base profile to a
    /// thinking variant when the LLM calls `request_thinking_mode`.
    #[serde(default)]
    pub is_adaptive: bool,
    /// Free-form description from the adaptive composite spec. None for
    /// standard profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_description: Option<String>,
    /// Optional tier label from the adaptive composite spec (e.g.
    /// `instant` / `normal` / `advanced`). UI renders this as a small
    /// chip alongside the `Adaptive` badge so users can see the
    /// size / capability tier without exposing raw model names.
    /// None for standard profiles and adaptive composites that
    /// haven't opted into tiering.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adaptive_tier: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ChatProfileWarning {
    pub profile_name: String,
    pub message: String,
}

/// Token usage metadata from LLM API responses
#[derive(Debug, Clone, Default)]
pub struct LLMUsage {
    /// Number of tokens in the prompt
    pub prompt_tokens: u32,
    /// Number of tokens in the completion/response
    pub completion_tokens: u32,
    /// Total tokens used (prompt + completion)
    pub total_tokens: u32,
    /// Reasoning / thinking tokens when reported by the provider
    /// (Anthropic extended-thinking, OpenAI reasoning items).
    pub reasoning_tokens: u32,
    /// Cache-read tokens — input we got at the discounted prefix-cache rate.
    /// Subset of `prompt_tokens` on Anthropic / Minimax / OpenAI prefix cache.
    pub cache_read_tokens: u32,
    /// Cache-write tokens — premium-priced cache create on Anthropic / Minimax.
    /// Also a subset of `prompt_tokens`; 0 on providers without explicit writes.
    pub cache_creation_tokens: u32,
}

/// LLM response with content and usage metadata
#[derive(Debug, Clone)]
pub struct LLMResponse {
    /// The generated text content
    pub content: String,
    /// Token usage statistics (if available from provider)
    pub usage: Option<LLMUsage>,
}

impl LLMResponse {
    /// Create a response with content only (no usage data)
    pub fn content_only(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            usage: None,
        }
    }

    /// Create a response with content and usage metadata
    pub fn with_usage(content: impl Into<String>, usage: LLMUsage) -> Self {
        Self {
            content: content.into(),
            usage: Some(usage),
        }
    }
}

/// Richer response type for chat completions that preserves tool calls.
///
/// Unlike `LLMResponse` which only carries text content, this includes
/// `tool_calls` from the LLM so the caller can dispatch them.
#[derive(Debug, Clone)]
pub struct ChatCompletionResponse {
    /// Text content from the LLM (may be empty when only tool calls are returned).
    pub content: Option<String>,
    /// Tool calls requested by the LLM.
    pub tool_calls: Vec<RouterToolCall>,
    /// Token usage statistics (if available from provider).
    pub usage: Option<LLMUsage>,
    /// Provider-native payload needed for exact continuation on some providers.
    pub raw_response: Option<Value>,
    /// Provider-emitted reasoning / chain-of-thought text. Forwarded
    /// from `magicllm::LLMResponse.reasoning_text` (re-exported here
    /// as `RouterResponse`) so chat-side consumers can emit
    /// `reasoning.start/content/end` events without reaching back
    /// into the raw provider payload.
    pub reasoning_text: Option<String>,
    /// Resolved provider/model/profile and cost/token telemetry for this
    /// chat completion. The chat inline loop forwards this to
    /// `LLMResponseReceived` so `/llm` rows do not need to guess from a
    /// profile label after the fact.
    pub telemetry: Option<LlmCallTelemetry>,
}

/// Move the normalized response lanes into the chat boundary whenever this
/// consumer is the final owner. Idempotency/cache consumers may still share an
/// Arc; only that uncommon case performs a heap-framed clone. The former
/// unconditional `.as_ref().clone()` recursively copied every tool argument
/// and the complete provider-native response on every ordinary chat call.
fn into_owned_router_tool_calls(calls: Arc<Vec<RouterToolCall>>) -> Vec<RouterToolCall> {
    Arc::try_unwrap(calls).unwrap_or_else(|shared| {
        shared
            .iter()
            .map(|call| RouterToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: crate::magician_v2::json_traversal::clone_json_iteratively(
                    &call.arguments,
                ),
            })
            .collect()
    })
}

fn into_owned_provider_response(response: Arc<Value>) -> Value {
    Arc::try_unwrap(response).unwrap_or_else(|shared| {
        crate::magician_v2::json_traversal::clone_json_iteratively(shared.as_ref())
    })
}

#[derive(Debug, Clone, Default)]
pub struct ChatCompletionRequestOverrides {
    pub extra: Option<Value>,
    /// Local-only identity for this logical chat-model call. This deliberately
    /// stays outside `extra`, so scope and product lineage are never sent to a
    /// provider.
    pub trace_context: Option<magicllm::LlmTraceContext>,
    /// Caller-owned physical-attempt counter paired with `trace_context`.
    /// Keeping this handle outside provider payloads lets an outer
    /// cancellation boundary close the exact call after dropping a stream.
    pub provider_attempt_counter: Option<Arc<AtomicU32>>,
    /// Runtime-only labeled-content route/capture fence. This never enters
    /// `extra` or provider JSON; MagicLLM checks it at the physical attempt.
    pub disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatCompletionTelemetryHint {
    pub provider: String,
    pub model: String,
    pub profile: Option<String>,
}

/// Simple trait for LLM-based query analysis
#[async_trait]
pub trait QueryAnalysisLLM: Send + Sync {
    /// Generate a response for query analysis
    /// Takes a prompt and returns the LLM response with content and usage metadata
    async fn generate_analysis(&self, prompt: &str) -> Result<LLMResponse>;

    /// Check if the LLM service is available
    async fn is_available(&self) -> bool {
        true
    }

    /// Get the provider name for this LLM service (e.g., "openai", "anthropic", "ollama")
    fn provider_name(&self) -> String {
        "unknown".to_string()
    }
}

/// Mock implementation for testing/development
pub struct MockQueryAnalysisLLM;

#[async_trait]
impl QueryAnalysisLLM for MockQueryAnalysisLLM {
    async fn generate_analysis(&self, _prompt: &str) -> Result<LLMResponse> {
        // Return a mock JSON response for query analysis that matches the
        // current UnifiedAnalysisResult schema
        let content = serde_json::json!({
            "complexity": {
                "score": 0.5,
                "factors": ["mock_analysis"],
                "reasoning": "Mock analysis - medium complexity"
            },
            "categories": {
                "categories": ["automation", "productivity"],
                "reasoning": "Mock category reasoning"
            },
            "dependencies": {
                "is_multi_step": false,
                "dependencies": [],
                "workflow_steps": [],
                "reasoning": "Mock dependency reasoning",
                "required_capabilities": []
            },
            "extracted_entities": {
                "entities": {},
                "typed_entities": {},
                "extraction_confidence": 0.5,
                "extraction_reasoning": "Mock entity extraction"
            },
            "intent": "new_task",
            "slot_match": null
        })
        .to_string();

        // Mock usage data
        let usage = LLMUsage {
            prompt_tokens: 100,
            completion_tokens: 150,
            total_tokens: 250,
            ..Default::default()
        };

        Ok(LLMResponse::with_usage(content, usage))
    }
}

/// Operation types that can be mapped to specific LLM configs
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum LLMOperation {
    TaskDecomposition,
    QueryAnalysis,
    EntityMapping,
    AtomicComposition,
    AtomicCompositionOutline,
    SlotExtraction,
    QuestionRewriting,
    QuestionCuration,
    ToolEvaluation,   // For LLM evaluator in V2 tool matcher
    CategoryMatching, // For fuzzy category validation in CategoryFuzzyMatcher

    // Progressive elicitation operations
    ParameterInference,        // Generic parameter inference fallback
    ParameterExtraction,       // Extract explicit parameter values from message (nano)
    RecipeMatch,               // Confirm a task-recipe shape and extract inputs (nano)
    ParameterDefaultInference, // Infer context-aware defaults (small)
    ParameterSafetyCheck,      // Safety evaluation for discovery actions (small)
    ParameterRefinement,       // Refine inferred values
    DiscoveryPlanning,         // Plan autonomous discovery
    DiscoverySafety, // Safety check for discovery actions (legacy, use ParameterSafetyCheck)

    // Chat mode operations
    ChatCompletion, // Conversational chat completions

    // API Mining Phase 2: workflow compilation
    WorkflowCompilation, // Compile N CapabilitySequences -> one WorkflowGraph
    RecipeCompilation,   // Shape-only Task Recipe refinement

    Other(String),
}

impl LLMOperation {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "task_decomposition" => LLMOperation::TaskDecomposition,
            "query_analysis" => LLMOperation::QueryAnalysis,
            "entity_mapping" => LLMOperation::EntityMapping,
            "atomic_composition" => LLMOperation::AtomicComposition,
            "atomic_composition_outline" => LLMOperation::AtomicCompositionOutline,
            "slot_extraction" => LLMOperation::SlotExtraction,
            "question_rewriting" => LLMOperation::QuestionRewriting,
            "question_curation" => LLMOperation::QuestionCuration,
            "tool_evaluation" => LLMOperation::ToolEvaluation,
            "category_matching" => LLMOperation::CategoryMatching,
            "parameter_inference" => LLMOperation::ParameterInference,
            "parameter_extraction" => LLMOperation::ParameterExtraction,
            "recipe_match" => LLMOperation::RecipeMatch,
            "parameter_default_inference" => LLMOperation::ParameterDefaultInference,
            "parameter_safety_check" => LLMOperation::ParameterSafetyCheck,
            "parameter_refinement" => LLMOperation::ParameterRefinement,
            "discovery_planning" => LLMOperation::DiscoveryPlanning,
            "discovery_safety" => LLMOperation::DiscoverySafety,
            "chat_completion" => LLMOperation::ChatCompletion,
            "workflow_compilation" => LLMOperation::WorkflowCompilation,
            "recipe_compilation" => LLMOperation::RecipeCompilation,
            other => LLMOperation::Other(other.to_string()),
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            LLMOperation::TaskDecomposition => "task_decomposition",
            LLMOperation::QueryAnalysis => "query_analysis",
            LLMOperation::EntityMapping => "entity_mapping",
            LLMOperation::AtomicComposition => "atomic_composition",
            LLMOperation::AtomicCompositionOutline => "atomic_composition_outline",
            LLMOperation::SlotExtraction => "slot_extraction",
            LLMOperation::QuestionRewriting => "question_rewriting",
            LLMOperation::QuestionCuration => "question_curation",
            LLMOperation::ToolEvaluation => "tool_evaluation",
            LLMOperation::CategoryMatching => "category_matching",
            LLMOperation::ParameterInference => "parameter_inference",
            LLMOperation::ParameterExtraction => "parameter_extraction",
            LLMOperation::RecipeMatch => "recipe_match",
            LLMOperation::ParameterDefaultInference => "parameter_default_inference",
            LLMOperation::ParameterSafetyCheck => "parameter_safety_check",
            LLMOperation::ParameterRefinement => "parameter_refinement",
            LLMOperation::DiscoveryPlanning => "discovery_planning",
            LLMOperation::DiscoverySafety => "discovery_safety",
            LLMOperation::ChatCompletion => "chat_completion",
            LLMOperation::WorkflowCompilation => "workflow_compilation",
            LLMOperation::RecipeCompilation => "recipe_compilation",
            LLMOperation::Other(s) => s.as_str(),
        }
    }

    /// Get the timeout duration for this operation
    /// AtomicComposition gets 8+ minutes for reasoning models, others get 60
    /// seconds
    pub fn timeout_seconds(&self) -> u64 {
        match self {
            LLMOperation::AtomicComposition => 500, // 8.3 minutes for reasoning models
            // (o1-preview, gpt-5, etc.)
            LLMOperation::AtomicCompositionOutline => 120,
            LLMOperation::SlotExtraction => 60,
            LLMOperation::QuestionRewriting => 60,
            LLMOperation::QuestionCuration => 30,

            // Progressive elicitation operations
            LLMOperation::ParameterInference => 30, // Generic fallback
            LLMOperation::ParameterExtraction => 20, // Fast extraction with nano
            LLMOperation::RecipeMatch => 20,
            LLMOperation::ParameterDefaultInference => 30, // Context-aware defaults with small
            LLMOperation::ParameterSafetyCheck => 30,      // Security evaluation with small
            LLMOperation::ParameterRefinement => 30,
            LLMOperation::DiscoveryPlanning => 45,
            LLMOperation::DiscoverySafety => 20, // Legacy, use ParameterSafetyCheck

            // Chat mode operations
            LLMOperation::ChatCompletion => 120, // Conversational chat (generous timeout)

            _ => 60, // 60 seconds for all other operations
        }
    }
}

/// Service that selects the appropriate LLM based on operation type.
///
/// Wraps `ConfiguredRouter` from magicllm with application-level concerns:
/// operation-specific timeouts, config lookup, and simplified response types.
#[derive(Clone)]
struct MultiLLMServiceState {
    /// Named LLM configurations from magician-config.yaml
    llm_configs: HashMap<String, LlmConfig>,
    /// Operation-to-config mapping
    operation_mapping: HashMap<String, String>,
    /// Exact router config used to build the live router.
    router_config: Option<LLMRouterConfig>,
    /// Configured router with providers auto-registered from profiles
    configured_router: Option<Arc<ConfiguredRouter>>,
    /// Default config name to use when operation not mapped
    default_config: String,
}

#[derive(Clone)]
pub struct MultiLLMService {
    state: Arc<RwLock<MultiLLMServiceState>>,
    /// Shared, set-once handle to the global LLM dispatch queue. Installed at
    /// boot via [`Self::set_dispatch_queue`] when `llm.dispatch.enabled`.
    /// Streaming chat submits through the queue; when absent, it falls back
    /// to `ConfiguredRouter::route_stream` and records
    /// `magician::metrics::llm_dispatch_bypass`.
    dispatch_queue: Arc<OnceLock<Arc<magicllm::LlmDispatchQueue>>>,
}

/// A direct-router failure paired with the exact logical-call identity and
/// physical-provider-attempt count observed before the failure escaped.
///
/// `ConfiguredRouter` necessarily returns only `LLMError` on failure, so it
/// cannot attach a receipt to an `LLMResponse`. Keeping this typed error in
/// the `anyhow` source chain lets outer product surfaces record the failed
/// logical call without inventing a new call id or a provider attempt.
#[derive(Debug, thiserror::Error)]
#[error("LLM route failed: {source}")]
pub struct TracedLlmRouteError {
    #[source]
    source: magicllm::LLMError,
    trace_receipt: magicllm::LlmTraceReceipt,
}

impl MultiLLMService {
    /// Recover trace identity from a direct routing failure even after callers
    /// have added `anyhow::Context` layers.
    pub fn trace_receipt_from_error(error: &anyhow::Error) -> Option<magicllm::LlmTraceReceipt> {
        error.chain().find_map(|source| {
            source
                .downcast_ref::<TracedLlmRouteError>()
                .map(|error| error.trace_receipt.clone())
        })
    }

    /// Recover the concrete terminal route from a failed direct call. This is
    /// distinct from a configuration-derived hint: fallback traversal may have
    /// changed profile/provider/model before the terminal error surfaced.
    pub fn route_identity_from_error(error: &anyhow::Error) -> Option<ChatCompletionTelemetryHint> {
        error.chain().find_map(|source| {
            let traced = source.downcast_ref::<TracedLlmRouteError>()?;
            let (profile, provider, model) = traced.source.effective_route()?;
            Some(ChatCompletionTelemetryHint {
                provider: provider.to_string(),
                model: model.to_string(),
                profile: Some(profile.to_string()),
            })
        })
    }

    pub fn traced_route_error(
        source: magicllm::LLMError,
        trace_context: magicllm::LlmTraceContext,
        provider_attempt_count: u32,
    ) -> anyhow::Error {
        anyhow::Error::new(TracedLlmRouteError {
            source,
            trace_receipt: magicllm::LlmTraceReceipt::direct_with_attempt_count(
                trace_context,
                provider_attempt_count,
            ),
        })
    }

    fn ensure_trace_context_activity_id(trace_context: &mut magicllm::LlmTraceContext) {
        if trace_context.activity_id.is_none() {
            trace_context.set_activity_id(current_activity_id().map(|id| id.to_string()));
        }
    }

    /// True when the router config explicitly binds both routed embedding
    /// operations. Only then does the embedding seam route; otherwise
    /// vector-index keeps its direct-HTTP fallback (no embedding profile ⇒
    /// behavior identical to pre-migration).
    fn router_binds_embedding_operations(config: Option<&LLMRouterConfig>) -> bool {
        config.is_some_and(|config| {
            config
                .operation_mapping
                .contains_key(magician_vector_index::embedding_router::EMBED_DOCUMENTS_OPERATION)
                && config
                    .operation_mapping
                    .contains_key(magician_vector_index::embedding_router::EMBED_QUERY_OPERATION)
        })
    }

    fn build_state(
        llm_configs: HashMap<String, LlmConfig>,
        router_config: Option<LLMRouterConfig>,
    ) -> MultiLLMServiceState {
        // multi_llm_service is a transport-level service that doesn't see
        // request shape, so it stores the *unconditional* default profile
        // name for each operation. The shape-aware alternative
        // (`when_has_images`) is used by the higher-level
        // `operation_llm_router` which knows the request.
        let operation_mapping: HashMap<String, String> = router_config
            .as_ref()
            .map(|config| {
                config
                    .operation_mapping
                    .iter()
                    .map(|(op, selector)| (op.clone(), selector.default_profile().to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let configured_router = if let Some(config) = router_config.clone() {
            match validate_router_chunking_config(&config) {
                Err(err) => {
                    warn!(
                        error = %err,
                        "[MAGICIAN-V2-LLM] invalid logical-context adapter mapping; MultiLLMService disabled"
                    );
                    magician_vector_index::uninstall_embedding_router();
                    None
                },
                Ok(()) => match ConfiguredRouter::from_router_config(config) {
                    Ok(router) => {
                        info!("[MAGICIAN-V2-LLM] ConfiguredRouter initialized");
                        let router = Arc::new(router);
                        // The same router serves the routed embedding seam when
                        // the config explicitly binds the embedding operations;
                        // without those bindings vector-index keeps its direct
                        // fallback, so an install without an embedding profile
                        // behaves exactly as before.
                        if Self::router_binds_embedding_operations(router_config.as_ref()) {
                            magician_vector_index::install_embedding_router(Arc::clone(&router));
                        } else {
                            magician_vector_index::uninstall_embedding_router();
                        }
                        Some(router)
                    },
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-LLM] Failed to initialize ConfiguredRouter: {}",
                            err
                        );
                        magician_vector_index::uninstall_embedding_router();
                        None
                    },
                },
            }
        } else {
            magician_vector_index::uninstall_embedding_router();
            None
        };

        let default_config = router_config
            .as_ref()
            .map(|config| config.default_profile.clone())
            .or_else(|| {
                if llm_configs.contains_key("llm-openai-small") {
                    Some("llm-openai-small".to_string())
                } else {
                    llm_configs.keys().next().map(|k| k.to_string())
                }
            })
            .unwrap_or_else(|| "default".to_string());

        debug!(
            "[MAGICIAN-V2-QUERY] Initialized MultiLLMService with {} configs and {} operation \
             mappings",
            llm_configs.len(),
            operation_mapping.len()
        );

        MultiLLMServiceState {
            llm_configs,
            operation_mapping,
            router_config,
            configured_router,
            default_config,
        }
    }

    fn read_state(&self) -> RwLockReadGuard<'_, MultiLLMServiceState> {
        self.state
            .read()
            .expect("multi_llm_service state lock poisoned")
    }

    async fn route_direct_with_observability(
        &self,
        configured_router: &ConfiguredRouter,
        mut request: RouterRequest,
        caller: &'static str,
    ) -> Result<RouterResponse> {
        let operation = request.metadata.operation.clone();
        let model = request.model.clone();
        let mut trace_context = request
            .metadata
            .ensure_trace_context(None, llm_workload_for_operation_name(&operation));
        Self::ensure_trace_context_activity_id(&mut trace_context);
        request.metadata.set_trace_context(trace_context.clone());
        let provider_attempt_counter = request.metadata.ensure_provider_attempt_counter();
        let count = crate::magician_v2::local_resource_governor::record_llm_direct_route(false);
        if count == 1 {
            info!(
                target: "magician::metrics::llm_dispatch_bypass",
                caller,
                operation = %operation,
                model = %model,
                direct_route_count = count,
                "MultiLLMService direct ConfiguredRouter route observed outside the LLM dispatch queue"
            );
        } else if count % 100 == 0 {
            warn!(
                target: "magician::metrics::llm_dispatch_bypass",
                caller,
                operation = %operation,
                model = %model,
                direct_route_count = count,
                "MultiLLMService direct ConfiguredRouter route volume reached periodic threshold"
            );
        }
        configured_router.route(request).await.map_err(|source| {
            Self::traced_route_error(
                source,
                trace_context,
                provider_attempt_counter.load(Ordering::Relaxed),
            )
        })
    }

    async fn route_stream_direct_with_observability(
        &self,
        configured_router: &ConfiguredRouter,
        mut request: RouterRequest,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        caller: &'static str,
    ) -> Result<()> {
        let operation = request.metadata.operation.clone();
        let model = request.model.clone();
        let mut trace_context = request
            .metadata
            .ensure_trace_context(None, llm_workload_for_operation_name(&operation));
        Self::ensure_trace_context_activity_id(&mut trace_context);
        request.metadata.set_trace_context(trace_context.clone());
        let provider_attempt_counter = request.metadata.ensure_provider_attempt_counter();
        let count = crate::magician_v2::local_resource_governor::record_llm_direct_route(true);
        if count == 1 {
            info!(
                target: "magician::metrics::llm_dispatch_bypass",
                caller,
                operation = %operation,
                model = %model,
                direct_stream_route_count = count,
                "MultiLLMService direct streaming ConfiguredRouter route observed outside the LLM dispatch queue"
            );
        } else if count % 100 == 0 {
            warn!(
                target: "magician::metrics::llm_dispatch_bypass",
                caller,
                operation = %operation,
                model = %model,
                direct_stream_route_count = count,
                "MultiLLMService direct streaming ConfiguredRouter route volume reached periodic threshold"
            );
        }
        configured_router
            .route_stream(request, tx)
            .await
            .map_err(|source| {
                Self::traced_route_error(
                    source,
                    trace_context,
                    provider_attempt_counter.load(Ordering::Relaxed),
                )
            })
    }

    /// Install the global LLM dispatch queue (set-once, at boot). After this,
    /// streaming chat submits through the queue instead of calling
    /// `route_stream` directly. No-op if already set.
    pub fn set_dispatch_queue(&self, queue: Arc<magicllm::LlmDispatchQueue>) {
        let _ = self.dispatch_queue.set(queue);
    }

    async fn route_stream_queued_or_direct(
        &self,
        configured_router: Arc<ConfiguredRouter>,
        request: RouterRequest,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        caller: &'static str,
    ) -> Result<()> {
        if let Some(queue) = self.dispatch_queue.get() {
            self.route_stream_via_dispatch_queue(
                queue.as_ref(),
                configured_router,
                request,
                tx,
                caller,
            )
            .await
        } else {
            self.route_stream_direct_with_observability(
                configured_router.as_ref(),
                request,
                tx,
                caller,
            )
            .await
        }
    }

    async fn route_stream_via_dispatch_queue(
        &self,
        queue: &magicllm::LlmDispatchQueue,
        configured_router: Arc<ConfiguredRouter>,
        mut request: RouterRequest,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        caller: &'static str,
    ) -> Result<()> {
        let operation = request.metadata.operation.clone();
        let mut trace_context = request
            .metadata
            .ensure_trace_context(None, llm_workload_for_operation_name(&operation));
        Self::ensure_trace_context_activity_id(&mut trace_context);
        request.metadata.set_trace_context(trace_context.clone());
        request.metadata.ensure_provider_attempt_counter();

        let origin = magicllm::dispatch::JobOrigin::op(&operation)
            .with_caller(caller)
            .with_activity_id(current_activity_id().map(|id| id.to_string()));
        let (mut job, mut rx) = magicllm::LlmStreamJob::new(request, origin, 32);
        job = job.with_priority(magicllm::dispatch::Priority::High);
        let routing_snapshot: Arc<dyn magicllm::dispatch::DispatchRouter> = configured_router;
        job = job.with_router_snapshot(routing_snapshot);
        if let Some(task_ref) = chat_task_ref_from_trace(&job.trace_context) {
            job = job.with_task(task_ref);
        }
        if let Err(err) = queue.submit_stream(job).await {
            while let Some(delta) = rx.recv().await {
                if tx.send(delta).await.is_err() {
                    break;
                }
            }
            return Err(anyhow!(err));
        }
        let mut terminal_error = None;
        while let Some(delta) = rx.recv().await {
            if let StreamDelta::Error(message) = &delta {
                terminal_error = Some(message.clone());
            }
            if tx.send(delta).await.is_err() {
                // Caller dropped the stream; the queue job tombstones when
                // its own sender sees the closed receiver.
                return Ok(());
            }
        }
        match terminal_error {
            Some(message) => Err(anyhow!(message)),
            None => Ok(()),
        }
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn current_state(&self) -> MultiLLMServiceState {
        self.state
            .read()
            .expect("multi_llm_service state lock poisoned")
            .clone()
    }

    pub fn reload_from_router_config(&self, config: &LLMRouterConfig) -> bool {
        if let Err(error) = validate_router_chunking_config(config) {
            warn!(
                error = %error,
                "[MAGICIAN-V2-LLM] rejected config reload with invalid logical-context adapter mapping; retaining current MultiLLMService"
            );
            return false;
        }
        let next = Self::build_state_from_router_config(config);
        let live_router = next.configured_router.is_some();
        let mut state = self
            .state
            .write()
            .expect("multi_llm_service state lock poisoned");
        *state = next;
        live_router
    }

    fn build_state_from_router_config(config: &LLMRouterConfig) -> MultiLLMServiceState {
        // Build llm_configs for config lookup (get_config_for_operation).
        let mut llm_configs = HashMap::new();
        for (name, profile) in &config.profiles {
            llm_configs.insert(name.clone(), llm_config_from_profile(profile));
        }

        let state = Self::build_state(llm_configs, Some(config.clone()));
        Self::warn_on_misconfigured_adaptive_profiles(&state);
        state
    }

    /// Chat-eligibility check for `thinking_profile`. The picker filters
    /// adaptive composites by their `fast_profile`'s chat-eligibility
    /// (`tool_choice: auto`, not `openai_api_mode: chat` for OpenAI), so
    /// the user only sees composites whose fast variant the chat surface
    /// can actually dispatch. The `thinking_profile` is dispatched only
    /// at escalation time, so a misconfigured thinking variant would
    /// surface as a router error mid-turn — confusing for the operator.
    /// This warns at startup so the misconfiguration is visible before
    /// the first chat turn ever runs. Schema-level validation (missing
    /// references, nested composites, identical pairs) is already done
    /// inside `magicllm::LLMRouterConfig::validate_adaptive_profiles`.
    fn warn_on_misconfigured_adaptive_profiles(state: &MultiLLMServiceState) {
        let Some(router_config) = state.router_config.as_ref() else {
            return;
        };
        let mut names: Vec<&String> = router_config.adaptive_profiles.keys().collect();
        names.sort();
        for name in names {
            let Some(adaptive) = router_config.adaptive_profiles.get(name) else {
                continue;
            };
            if let Some(thinking_config) = state.llm_configs.get(&adaptive.thinking_profile) {
                if !Self::is_chat_profile_eligible(thinking_config) {
                    warn!(
                        adaptive_profile = %name,
                        thinking_profile = %adaptive.thinking_profile,
                        "[MAGICIAN-V2-LLM] adaptive thinking_profile is not chat-eligible \
                         (requires `tool_choice: auto`, and for OpenAI not \
                         `openai_api_mode: chat`). Escalation will fail at runtime."
                    );
                }
            }
            if let Some(fast_config) = state.llm_configs.get(&adaptive.fast_profile) {
                if !Self::is_chat_profile_eligible(fast_config) {
                    warn!(
                        adaptive_profile = %name,
                        fast_profile = %adaptive.fast_profile,
                        "[MAGICIAN-V2-LLM] adaptive fast_profile is not chat-eligible — \
                         composite will be hidden from the chat profile picker."
                    );
                }
            }
        }
    }

    fn merge_extra(
        llm_config: &LlmConfig,
        overrides: Option<&ChatCompletionRequestOverrides>,
    ) -> Option<Value> {
        let mut extra_map = serde_json::Map::new();
        if let Some(params) = llm_config.additional_params.as_ref() {
            for (key, value) in params {
                extra_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(verbosity) = llm_config.verbosity.as_ref() {
            extra_map.insert("verbosity".to_string(), Value::String(verbosity.clone()));
        }
        if let Some(extra_override) = overrides
            .and_then(|overrides| overrides.extra.as_ref())
            .and_then(Value::as_object)
        {
            for (key, value) in extra_override {
                extra_map.insert(key.clone(), value.clone());
            }
        }

        if extra_map.is_empty() {
            None
        } else {
            Some(Value::Object(extra_map))
        }
    }

    fn apply_trace_override(
        request: &mut RouterRequest,
        overrides: Option<&ChatCompletionRequestOverrides>,
    ) {
        if let Some(trace_context) = overrides.and_then(|value| value.trace_context.clone()) {
            request.metadata.set_trace_context(trace_context);
        }
        if let Some(counter) = overrides.and_then(|value| value.provider_attempt_counter.clone()) {
            request.metadata.provider_attempt_counter = Some(counter);
        }
        if let Some(guard) = overrides.and_then(|value| value.disclosure_guard.clone()) {
            request.metadata.set_disclosure_guard(guard);
        }
    }

    /// Create a new MultiLLMService from legacy config maps.
    ///
    /// Converts `LlmConfig` entries into an `LLMRouterConfig` and delegates
    /// provider bootstrap to `ConfiguredRouter`.
    pub fn new(
        llm_configs: HashMap<String, LlmConfig>,
        operation_mapping: HashMap<String, String>,
    ) -> Self {
        let router_config = if llm_configs.is_empty() {
            None
        } else {
            Some(build_router_config(&llm_configs, &operation_mapping))
        };
        Self {
            state: Arc::new(RwLock::new(Self::build_state(llm_configs, router_config))),
            dispatch_queue: Arc::new(OnceLock::new()),
        }
    }

    /// Create a `MultiLLMService` from an `LLMRouterConfig`.
    ///
    /// Provider bootstrap is delegated entirely to `ConfiguredRouter` —
    /// no duplicate `instantiate_provider` logic.
    pub fn from_router_config(config: &LLMRouterConfig) -> Self {
        Self {
            state: Arc::new(RwLock::new(Self::build_state_from_router_config(config))),
            dispatch_queue: Arc::new(OnceLock::new()),
        }
    }

    /// Get the LLM config for a specific operation
    pub fn get_config_for_operation(&self, operation: &LLMOperation) -> Result<LlmConfig> {
        let state = self.read_state();
        let operation_str = operation.as_str();

        // First try to find in operation mapping. The router config side
        // returns a selector (which can be conditional); transport-level
        // dispatch always picks the unconditional default profile.
        let router_default = state
            .router_config
            .as_ref()
            .and_then(|config| config.operation_mapping.get(operation_str))
            .map(|selector| selector.default_profile().to_string());
        let cached_default = state.operation_mapping.get(operation_str).cloned();
        if let Some(config_name) = router_default.or(cached_default) {
            // Transparently resolve adaptive composite → fast_profile.
            // Adaptive composites live in their own map and don't have a
            // direct `LlmConfig` row; non-adaptive-aware callers see the
            // fast variant, which is the right default for shape probes
            // and capability sniffing.
            let resolved_name = Self::resolve_adaptive_to_fast(&state, &config_name);
            debug!(
                "[MAGICIAN-V2-QUERY] Operation '{}' mapped to config '{}' (resolved: '{}')",
                operation_str, config_name, resolved_name
            );
            return state
                .llm_configs
                .get(resolved_name.as_str())
                .cloned()
                .ok_or_else(|| {
                    anyhow!(
                        "Config '{}' not found for operation '{}'",
                        resolved_name,
                        operation_str
                    )
                });
        }

        // Fall back to default config (also resolved through adaptive map).
        let default_config_raw = state
            .router_config
            .as_ref()
            .map(|config| config.default_profile.clone())
            .unwrap_or_else(|| state.default_config.clone());
        let default_config = Self::resolve_adaptive_to_fast(&state, &default_config_raw);
        debug!(
            "[MAGICIAN-V2-QUERY] Operation '{}' not mapped, using default config '{}'",
            operation_str, default_config
        );

        state
            .llm_configs
            .get(default_config.as_str())
            .cloned()
            .ok_or_else(|| anyhow!("Default config '{}' not found", default_config))
    }

    /// If `name` is an adaptive composite, resolve to its `fast_profile`.
    /// Otherwise return `name` unchanged. Used by adaptive-unaware
    /// callers (shape probes, capability sniffing) that only need a
    /// concrete `LlmConfig` and don't care about the thinking variant.
    fn resolve_adaptive_to_fast(state: &MultiLLMServiceState, name: &str) -> String {
        state
            .router_config
            .as_ref()
            .and_then(|config| config.adaptive_profiles.get(name))
            .map(|adaptive| adaptive.fast_profile.clone())
            .unwrap_or_else(|| name.to_string())
    }

    /// Generate analysis for a specific operation type
    pub async fn generate_for_operation(
        &self,
        operation: &LLMOperation,
        prompt: &str,
    ) -> Result<LLMResponse> {
        let llm_config = self.get_config_for_operation(operation)?;
        let timeout_secs = operation.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-QUERY] Generating analysis for operation '{}' using model '{}' with \
             timeout {}s",
            operation.as_str(),
            llm_config.model,
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        if !configured_router.has_provider_for_operation(operation.as_str()) {
            return Err(anyhow!(
                "Provider '{}' is not registered; please update the configuration.",
                llm_config.provider
            ));
        }

        self.generate_via_router(
            configured_router.as_ref(),
            operation,
            prompt,
            &llm_config,
            timeout_secs,
        )
        .await
    }

    async fn generate_via_router(
        &self,
        configured_router: &ConfiguredRouter,
        operation: &LLMOperation,
        prompt: &str,
        llm_config: &LlmConfig,
        timeout_secs: u64,
    ) -> Result<LLMResponse> {
        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: vec![RouterMessage::user(prompt.to_string())].into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        let mut extra_map = serde_json::Map::new();
        if let Some(params) = llm_config.additional_params.as_ref() {
            for (key, value) in params {
                extra_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(verbosity) = llm_config.verbosity.as_ref() {
            extra_map.insert("verbosity".to_string(), Value::String(verbosity.clone()));
        }
        if !extra_map.is_empty() {
            request.set_extra(Value::Object(extra_map));
        }

        let response: RouterResponse = self
            .route_direct_with_observability(configured_router, request, "generate_via_router")
            .await?;

        let mut content = response.text.as_deref().map(str::to_owned);
        if content.is_none() {
            content = extract_text_from_messages(&response.messages);
        }

        let content = content.ok_or_else(|| {
            anyhow!(
                "LLM provider returned no text content for operation '{}'",
                operation.as_str()
            )
        })?;

        let usage = response.usage.as_ref().map(convert_usage);

        Ok(LLMResponse { content, usage })
    }

    /// Generate a chat completion with a full messages array (system + history + user).
    pub async fn generate_chat_completion(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
    ) -> Result<LLMResponse> {
        let llm_config = self.get_config_for_operation(operation)?;
        let timeout_secs = operation.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-LLM] Generating chat completion for operation '{}' using model '{}' \
             with {} messages, timeout {}s",
            operation.as_str(),
            llm_config.model,
            messages.len(),
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        if !configured_router.has_provider_for_operation(operation.as_str()) {
            return Err(anyhow!(
                "Provider '{}' is not registered",
                llm_config.provider
            ));
        }

        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: messages.into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        let mut extra_map = serde_json::Map::new();
        if let Some(params) = llm_config.additional_params.as_ref() {
            for (key, value) in params {
                extra_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(verbosity) = llm_config.verbosity.as_ref() {
            extra_map.insert("verbosity".to_string(), Value::String(verbosity.clone()));
        }
        if !extra_map.is_empty() {
            request.set_extra(Value::Object(extra_map));
        }

        let response: RouterResponse = self
            .route_direct_with_observability(
                configured_router.as_ref(),
                request,
                "generate_chat_completion",
            )
            .await?;

        let mut content = response.text.as_deref().map(str::to_owned);
        if content.is_none() {
            content = extract_text_from_messages(&response.messages);
        }

        let content = content.ok_or_else(|| {
            anyhow!(
                "LLM provider returned no text content for operation '{}'",
                operation.as_str()
            )
        })?;

        let usage = response.usage.as_ref().map(convert_usage);

        Ok(LLMResponse { content, usage })
    }

    /// Generate a chat completion with tool support.
    ///
    /// Like `generate_chat_completion`, but also accepts tool definitions and
    /// returns a `ChatCompletionResponse` that preserves tool calls from the
    /// LLM response. This enables the tool-use loop in chat mode.
    pub async fn generate_chat_completion_with_tools(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        overrides: Option<ChatCompletionRequestOverrides>,
    ) -> Result<ChatCompletionResponse> {
        let llm_config = self.get_config_for_operation(operation)?;
        let timeout_secs = operation.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-LLM] Generating chat completion with tools for operation '{}' using \
             model '{}' with {} messages, {} tools, timeout {}s",
            operation.as_str(),
            llm_config.model,
            messages.len(),
            tools.len(),
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        if !configured_router.has_provider_for_operation(operation.as_str()) {
            return Err(anyhow!(
                "Provider '{}' is not registered",
                llm_config.provider
            ));
        }

        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: messages.into(),
            tools: tools.into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        if let Some(extra) = Self::merge_extra(&llm_config, overrides.as_ref()) {
            request.set_extra(extra);
        }
        Self::apply_trace_override(&mut request, overrides.as_ref());

        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let response: RouterResponse = self
            .route_direct_with_observability(
                configured_router.as_ref(),
                request,
                "generate_chat_completion_with_tools",
            )
            .await?;

        let mut content = response.text.as_deref().map(str::to_owned);
        if content.is_none() {
            content = extract_text_from_messages(&response.messages);
        }

        let usage = response.usage.as_ref().map(convert_usage);
        let reasoning_text = response.reasoning_text.as_deref().map(str::to_owned);
        let telemetry = self.build_chat_completion_telemetry(
            None,
            response.usage.as_ref(),
            started_at_ms,
            reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            response.trace_receipt.clone(),
            response.route_identity.as_ref(),
        );

        Ok(ChatCompletionResponse {
            content,
            tool_calls: into_owned_router_tool_calls(response.tool_calls),
            usage,
            raw_response: response.raw_response.map(into_owned_provider_response),
            reasoning_text,
            telemetry,
        })
    }

    /// Generate a streaming chat completion with tool support.
    ///
    /// Identical to `generate_chat_completion_with_tools` except:
    /// - Sets `request.stream = true`
    /// - Calls the streaming router path instead of the non-streaming `route`
    /// - Sends `StreamDelta` values through the provided channel
    pub async fn generate_chat_completion_streaming(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        overrides: Option<ChatCompletionRequestOverrides>,
    ) -> Result<()> {
        let llm_config = self.get_config_for_operation(operation)?;
        let timeout_secs = operation.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-LLM] Generating streaming chat completion for operation '{}' using \
             model '{}' with {} messages, {} tools, timeout {}s",
            operation.as_str(),
            llm_config.model,
            messages.len(),
            tools.len(),
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        if !configured_router.has_provider_for_operation(operation.as_str()) {
            return Err(anyhow!(
                "Provider '{}' is not registered",
                llm_config.provider
            ));
        }

        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: messages.into(),
            tools: tools.into(),
            stream: true,
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        if let Some(extra) = Self::merge_extra(&llm_config, overrides.as_ref()) {
            request.set_extra(extra);
        }
        Self::apply_trace_override(&mut request, overrides.as_ref());

        self.route_stream_queued_or_direct(
            configured_router,
            request,
            tx,
            "generate_chat_completion_streaming",
        )
        .await?;

        Ok(())
    }

    /// List all chat-eligible LLM profiles.
    ///
    /// A profile is chat-eligible if its `additional_params` metadata contains
    /// `tool_choice` set to `"auto"` (either as a string or as `{"type": "auto"}`).
    /// OpenAI profiles pinned to `openai_api_mode: chat` are intentionally
    /// excluded because they cannot safely support rich multimodal chat turns.
    ///
    /// Returns profiles sorted with the default chat profile first, then
    /// alphabetical by name.
    pub fn list_chat_profiles(&self) -> Vec<ChatProfileInfo> {
        let (profiles, _) = self.build_chat_profile_catalog();
        profiles
    }

    /// List warnings for configured profiles intentionally hidden from the
    /// chat profile chooser.
    pub fn list_chat_profile_warnings(&self) -> Vec<ChatProfileWarning> {
        let (_, warnings) = self.build_chat_profile_catalog();
        warnings
    }

    fn build_chat_profile_catalog(&self) -> (Vec<ChatProfileInfo>, Vec<ChatProfileWarning>) {
        let state = self.read_state();
        let default_profile: String = state
            .router_config
            .as_ref()
            .and_then(|config| config.operation_mapping.get("chat_completion"))
            .map(|selector| selector.default_profile().to_string())
            .or_else(|| state.operation_mapping.get("chat_completion").cloned())
            .unwrap_or_default();

        let mut profiles: Vec<ChatProfileInfo> = state
            .llm_configs
            .iter()
            .filter(|(_, config)| Self::is_chat_profile_eligible(config))
            .map(|(name, config)| ChatProfileInfo {
                name: name.clone(),
                provider: config.provider.to_string(),
                model: config.model.clone(),
                is_default: name == &default_profile,
                supports_user_image_inputs: Self::chat_profile_supports_user_image_inputs(config),
                is_adaptive: false,
                adaptive_description: None,
                adaptive_tier: None,
            })
            .collect();

        // Surface adaptive composites in the same catalog. The composite's
        // provider/model/image-support are inherited from its `fast_profile`
        // because the fast profile is what runs by default — the LLM
        // self-escalates to `thinking_profile` only when it calls
        // `request_thinking_mode`. Adaptive entries whose `fast_profile`
        // isn't chat-eligible (no `tool_choice: auto`) are skipped — the
        // chat surface can't dispatch them anyway.
        if let Some(router_config) = state.router_config.as_ref() {
            for (name, adaptive) in &router_config.adaptive_profiles {
                let Some(fast_config) = state.llm_configs.get(&adaptive.fast_profile) else {
                    continue;
                };
                if !Self::is_chat_profile_eligible(fast_config) {
                    continue;
                }
                profiles.push(ChatProfileInfo {
                    name: name.clone(),
                    provider: fast_config.provider.to_string(),
                    model: fast_config.model.clone(),
                    is_default: name == &default_profile,
                    supports_user_image_inputs: Self::chat_profile_supports_user_image_inputs(
                        fast_config,
                    ),
                    is_adaptive: true,
                    adaptive_description: adaptive
                        .description
                        .as_ref()
                        .map(|d| d.trim().to_string())
                        .filter(|d| !d.is_empty()),
                    adaptive_tier: adaptive
                        .tier
                        .as_ref()
                        .map(|t| t.trim().to_string())
                        .filter(|t| !t.is_empty()),
                });
            }
        }

        // Picker order:
        //   1. Default profile always first (so the active selection is
        //      visible at the top regardless of provider / tier).
        //   2. Adaptive composites cluster before standard profiles.
        //   3. Within adaptives, group by provider in a stable
        //      operator-facing order: OpenAI → Anthropic → Gemini →
        //      DeepSeek → MiniMax → xAI → Sarvam → other.
        //   4. Within each provider, sort by tier (instant → normal →
        //      advanced → frontier → untagged) so the cheapest option leads.
        //   5. Final tiebreak: alphabetical name.
        // The tier ordering is keyed off the optional `adaptive_tier`
        // string, with a tier-rank helper that pins known tiers to
        // small indices and pushes unknown / unset to the end.
        fn provider_rank(p: &str) -> u8 {
            match p {
                "openai" => 0,
                "anthropic" => 1,
                "gemini" => 2,
                "deepseek" => 3,
                "minimax" => 4,
                "xai" => 5,
                "sarvam" => 6,
                _ => 7,
            }
        }
        fn tier_rank(t: Option<&str>) -> u8 {
            match t.map(|s| s.trim()) {
                Some("instant") => 0,
                Some("normal") => 1,
                Some("advanced") => 2,
                Some("frontier") => 3,
                _ => 4,
            }
        }
        profiles.sort_by(|a, b| {
            b.is_default
                .cmp(&a.is_default)
                .then(b.is_adaptive.cmp(&a.is_adaptive))
                .then(provider_rank(&a.provider).cmp(&provider_rank(&b.provider)))
                .then(
                    tier_rank(a.adaptive_tier.as_deref())
                        .cmp(&tier_rank(b.adaptive_tier.as_deref())),
                )
                .then(a.name.cmp(&b.name))
        });

        let mut warnings: Vec<ChatProfileWarning> = state
            .llm_configs
            .iter()
            .filter_map(|(name, config)| Self::chat_profile_warning(name, config))
            .collect();
        warnings.sort_by(|a, b| a.profile_name.cmp(&b.profile_name));

        (profiles, warnings)
    }

    /// Look up the (fast, thinking) profile pair for an adaptive
    /// composite by name. Returns `None` for standard profiles or
    /// unknown names. Used by the chat-inline runtime to detect that
    /// `process_chat_inline_turn`'s effective profile is adaptive and
    /// to swap profile names mid-turn on `request_thinking_mode`.
    pub fn adaptive_pair(&self, profile_name: &str) -> Option<(String, String)> {
        let state = self.read_state();
        state
            .router_config
            .as_ref()
            .and_then(|config| config.adaptive_profiles.get(profile_name))
            .map(|adaptive| {
                (
                    adaptive.fast_profile.clone(),
                    adaptive.thinking_profile.clone(),
                )
            })
    }

    /// Operation-mapped default profile name for the given operation key.
    /// Used by the chat runtime to resolve adaptive composites when the
    /// caller passed no explicit `profile_override`.
    /// The profile name that WOULD serve this operation under the current
    /// `privacy.processing.mode` — the locality-effective arm (`when_cloud`
    /// under cloud, the default otherwise). Dispatch-side callers want this
    /// view; callers with a fixed on-device contract (the LocalOnly
    /// app-memory credential) must use
    /// [`Self::local_default_profile_for_operation`] instead.
    pub fn default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        let state = self.read_state();
        if let Some(config) = state.router_config.as_ref() {
            if let Some(selector) = config.operation_mapping.get(operation_key) {
                return Some(
                    selector
                        .profile_for_locality(
                            &magicllm::config::RequestShape::NONE,
                            config.locality,
                        )
                        .to_string(),
                );
            }
        }
        state.operation_mapping.get(operation_key).cloned()
    }

    /// The LOCAL (`default`) arm for an operation regardless of the current
    /// locality mode. For contracts pinned to the on-device profile — e.g.
    /// `AppLocalOnlyMemoryProviderCredential`, which requires the loopback
    /// Ollama endpoint in both modes.
    pub fn local_default_profile_for_operation(&self, operation_key: &str) -> Option<String> {
        let state = self.read_state();
        if let Some(name) = state
            .router_config
            .as_ref()
            .and_then(|config| config.operation_mapping.get(operation_key))
            .map(|selector| selector.default_profile().to_string())
        {
            return Some(name);
        }
        state.operation_mapping.get(operation_key).cloned()
    }

    fn chat_profile_tool_choice_is_auto(config: &LlmConfig) -> bool {
        let tool_choice = config
            .additional_params
            .as_ref()
            .and_then(|params| params.get("tool_choice"));
        match tool_choice {
            Some(Value::String(value)) => value == "auto",
            Some(value) => value.get("type").and_then(|t| t.as_str()) == Some("auto"),
            None => false,
        }
    }

    fn openai_api_mode(config: &LlmConfig) -> Option<&str> {
        config
            .additional_params
            .as_ref()
            .and_then(|params| params.get("openai_api_mode"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn is_chat_profile_eligible(config: &LlmConfig) -> bool {
        if !Self::chat_profile_tool_choice_is_auto(config) {
            return false;
        }

        if config.provider == LLMProviderKind::OpenAI {
            return !matches!(Self::openai_api_mode(config), Some("chat"));
        }

        true
    }

    pub(crate) fn chat_profile_supports_user_image_inputs(config: &LlmConfig) -> bool {
        config
            .supports_vision
            .unwrap_or_else(|| match config.provider {
                LLMProviderKind::Anthropic => true,
                LLMProviderKind::Minimax => false,
                LLMProviderKind::DeepSeek => false,
                LLMProviderKind::OpenAI => {
                    matches!(Self::openai_api_mode(config), Some("responses" | "auto"))
                },
                LLMProviderKind::Xai => magicllm::XaiProvider::model_supports_vision(&config.model),
                _ => false,
            })
    }

    fn chat_profile_warning(name: &str, config: &LlmConfig) -> Option<ChatProfileWarning> {
        if !Self::chat_profile_tool_choice_is_auto(config) {
            return None;
        }

        if config.provider != LLMProviderKind::OpenAI {
            return None;
        }

        match Self::openai_api_mode(config) {
            Some("chat") => Some(ChatProfileWarning {
                profile_name: name.to_string(),
                message: format!(
                    "Hidden chat profile `{}` uses `openai_api_mode: chat`. Rich chat requires `auto` or `responses`.",
                    name
                ),
            }),
            Some(mode) if !matches!(mode, "auto" | "responses") => Some(ChatProfileWarning {
                profile_name: name.to_string(),
                message: format!(
                    "Hidden chat profile `{}` uses unsupported `openai_api_mode: {}`. Use `auto` or `responses`.",
                    name, mode
                ),
            }),
            _ => None,
        }
    }

    /// Look up a config by its profile name (the key in `llm_configs`).
    ///
    /// Adaptive composite names transparently resolve to their
    /// `fast_profile` so adaptive-unaware callers (provider capability
    /// probes, shape-only lookups) get a working `LlmConfig` instead of
    /// `LLM profile not found`. Adaptive-aware callers
    /// (`process_chat_inline_turn`) already pre-resolve via
    /// `adaptive_pair` and pass standard profile names directly.
    pub fn get_config_by_profile_name(&self, profile_name: &str) -> Result<LlmConfig> {
        let state = self.read_state();
        let resolved = Self::resolve_adaptive_to_fast(&state, profile_name);
        state
            .llm_configs
            .get(resolved.as_str())
            .cloned()
            .ok_or_else(|| anyhow!("LLM profile '{}' not found", profile_name))
    }

    pub fn chat_completion_telemetry_hint(
        &self,
        profile_override: Option<&str>,
    ) -> Option<ChatCompletionTelemetryHint> {
        let state = self.read_state();
        Self::chat_completion_telemetry_hint_from_state(&state, profile_override)
    }

    fn chat_completion_telemetry_hint_from_state(
        state: &MultiLLMServiceState,
        profile_override: Option<&str>,
    ) -> Option<ChatCompletionTelemetryHint> {
        let operation = LLMOperation::ChatCompletion.as_str();
        let raw_profile = profile_override
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .or_else(|| {
                state
                    .router_config
                    .as_ref()
                    .and_then(|config| config.operation_mapping.get(operation))
                    .map(|selector| selector.default_profile().to_string())
            })
            .or_else(|| state.operation_mapping.get(operation).cloned())
            .unwrap_or_else(|| state.default_config.clone());
        let resolved_profile = Self::resolve_adaptive_to_fast(state, &raw_profile);
        let config = state.llm_configs.get(resolved_profile.as_str())?;
        Some(ChatCompletionTelemetryHint {
            provider: config.provider.to_string(),
            model: config.model.clone(),
            profile: Some(resolved_profile),
        })
    }

    pub fn build_chat_completion_telemetry(
        &self,
        profile_override: Option<&str>,
        usage: Option<&RouterTokenUsage>,
        started_at_ms: i64,
        reasoning_summary: Option<String>,
        trace_receipt: Option<magicllm::LlmTraceReceipt>,
        route_identity: Option<&magicllm::LlmRouteIdentity>,
    ) -> Option<LlmCallTelemetry> {
        // A configured profile is only a pre-dispatch hint. Fallback may have
        // changed provider, model, and profile, so successful telemetry and
        // pricing must prefer the route stamped on the actual response.
        let exact_route = route_identity.map(|identity| ChatCompletionTelemetryHint {
            provider: identity.provider.to_string(),
            model: identity.model.clone(),
            profile: Some(identity.profile.clone()),
        });
        let may_use_requested_route = trace_receipt
            .as_ref()
            .is_none_or(|receipt| receipt.provider_attempt_count == 0);
        let hint = exact_route
            .or_else(|| {
                may_use_requested_route
                    .then(|| self.chat_completion_telemetry_hint(profile_override))
                    .flatten()
            })
            // Keep exact call identity even if a malformed successful response
            // omitted its effective route. Empty route fields make the
            // canonical mapper retain the call and emit call-owned attempt-gap
            // evidence instead of falsely attributing the configured profile.
            .or_else(|| {
                trace_receipt
                    .is_some()
                    .then(ChatCompletionTelemetryHint::default)
            })?;
        // Identity must not disappear merely because a provider omitted token
        // accounting. Preserve the call receipt and emit zero/unknown economics
        // so Phase 1 joins remain complete; later phases can record usage
        // missingness explicitly.
        let usage_reported = usage.is_some();
        let usage = usage.cloned().unwrap_or_default();
        let provider_kind = LLMProviderKind::from_str(&hint.provider);
        let route_known = !hint.provider.trim().is_empty() && !hint.model.trim().is_empty();
        Some(LlmCallTelemetry {
            provider: hint.provider,
            model: hint.model.clone(),
            usage_reported,
            usage_availability: None,
            input_tokens: usage.prompt_tokens.unwrap_or(0),
            output_tokens: usage.completion_tokens.unwrap_or(0),
            reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
            cache_read_tokens: usage.cached_tokens.unwrap_or(0),
            cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0),
            search_calls: 0,
            cost_usd: if route_known {
                magicllm::compute_cost_at(&provider_kind, &hint.model, &usage, started_at_ms)
            } else {
                0.0
            },
            reasoning_summary,
            profile: hint.profile,
            operation: Some(LLMOperation::ChatCompletion.as_str().to_string()),
            started_at_ms,
            trace_receipt,
            prompt_projection_mode: None,
        })
    }

    /// Generate a chat completion using a specific named profile instead of
    /// the operation-mapped profile.
    ///
    /// Sets `router_profile_override` in the request extras so the router
    /// selects the exact profile.
    pub async fn generate_chat_completion_with_profile(
        &self,
        profile_name: &str,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        overrides: Option<ChatCompletionRequestOverrides>,
    ) -> Result<ChatCompletionResponse> {
        let llm_config = self.get_config_by_profile_name(profile_name)?;
        let timeout_secs = LLMOperation::ChatCompletion.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-LLM] Generating chat completion with profile override '{}' using \
             model '{}' with {} messages, {} tools, timeout {}s",
            profile_name,
            llm_config.model,
            messages.len(),
            tools.len(),
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: messages.into(),
            tools: tools.into(),
            metadata: RouterMetadata {
                operation: LLMOperation::ChatCompletion.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        let mut extra_map = Self::merge_extra(&llm_config, overrides.as_ref())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        // Set the profile override so the router uses this exact profile
        extra_map.insert(
            "router_profile_override".to_string(),
            Value::String(profile_name.to_string()),
        );
        request.set_extra(Value::Object(extra_map));
        Self::apply_trace_override(&mut request, overrides.as_ref());

        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let response: RouterResponse = self
            .route_direct_with_observability(
                configured_router.as_ref(),
                request,
                "generate_chat_completion_with_profile",
            )
            .await?;

        let mut content = response.text.as_deref().map(str::to_owned);
        if content.is_none() {
            content = extract_text_from_messages(&response.messages);
        }

        let usage = response.usage.as_ref().map(convert_usage);
        let reasoning_text = response.reasoning_text.as_deref().map(str::to_owned);
        let telemetry = self.build_chat_completion_telemetry(
            Some(profile_name),
            response.usage.as_ref(),
            started_at_ms,
            response
                .reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            response.trace_receipt.clone(),
            response.route_identity.as_ref(),
        );

        Ok(ChatCompletionResponse {
            content,
            tool_calls: into_owned_router_tool_calls(response.tool_calls),
            usage,
            raw_response: response.raw_response.map(into_owned_provider_response),
            reasoning_text,
            telemetry,
        })
    }

    /// Streaming variant of `generate_chat_completion_with_profile`.
    ///
    /// Uses the named profile instead of the operation-mapped one and streams
    /// `StreamDelta` values through the provided channel.
    pub async fn generate_chat_completion_streaming_with_profile(
        &self,
        profile_name: &str,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        tx: tokio::sync::mpsc::Sender<StreamDelta>,
        overrides: Option<ChatCompletionRequestOverrides>,
    ) -> Result<()> {
        let llm_config = self.get_config_by_profile_name(profile_name)?;
        let timeout_secs = LLMOperation::ChatCompletion.timeout_seconds();

        debug!(
            "[MAGICIAN-V2-LLM] Generating streaming chat completion with profile override '{}' \
             using model '{}' with {} messages, {} tools, timeout {}s",
            profile_name,
            llm_config.model,
            messages.len(),
            tools.len(),
            timeout_secs
        );

        let configured_router = self
            .read_state()
            .configured_router
            .clone()
            .ok_or_else(|| anyhow!("ConfiguredRouter not initialized"))?;

        let mut request = RouterRequest {
            model: llm_config.model.clone(),
            messages: messages.into(),
            tools: tools.into(),
            stream: true,
            metadata: RouterMetadata {
                operation: LLMOperation::ChatCompletion.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: llm_config.temperature,
            max_output_tokens: llm_config.max_tokens,
            ..Default::default()
        };
        request.prompt_cache = chat_prompt_cache_config_for_operation(&request.metadata.operation);

        if let Some(effort) = llm_config.reasoning_effort.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(effort.clone()),
                ..Default::default()
            });
        }

        let mut extra_map = Self::merge_extra(&llm_config, overrides.as_ref())
            .and_then(|value| value.as_object().cloned())
            .unwrap_or_default();
        // Set the profile override so the router uses this exact profile
        extra_map.insert(
            "router_profile_override".to_string(),
            Value::String(profile_name.to_string()),
        );
        request.set_extra(Value::Object(extra_map));
        Self::apply_trace_override(&mut request, overrides.as_ref());

        self.route_stream_queued_or_direct(
            configured_router,
            request,
            tx,
            "generate_chat_completion_streaming_with_profile",
        )
        .await?;

        Ok(())
    }

    pub fn provider_for_operation(&self, operation: &LLMOperation) -> Option<String> {
        if let Some(configured_router) = self.read_state().configured_router.clone() {
            return configured_router
                .provider_for_operation(operation.as_str())
                .map(|kind| kind.to_string());
        }

        self.get_config_for_operation(operation)
            .ok()
            .map(|config| config.provider.to_string())
    }
}

fn llm_workload_for_operation_name(operation: &str) -> magicllm::LlmWorkloadClass {
    let operation = operation.trim().to_ascii_lowercase();
    if operation == "chat_completion" || operation.starts_with("chat_") {
        magicllm::LlmWorkloadClass::ForegroundChat
    } else if operation.starts_with("memory_") || operation.contains("memory_temperature") {
        magicllm::LlmWorkloadClass::Memory
    } else if operation.starts_with("channel_")
        || operation.contains("mail")
        || operation.contains("gmail")
    {
        magicllm::LlmWorkloadClass::CommsAssist
    } else if operation.starts_with("ambient_") || operation.starts_with("screen_") {
        magicllm::LlmWorkloadClass::Ambient
    } else if operation.contains("eval") || operation.contains("judge") {
        magicllm::LlmWorkloadClass::Evaluation
    } else if operation.contains("agentic") || operation.contains("workflow") {
        magicllm::LlmWorkloadClass::AutonomousTask
    } else {
        magicllm::LlmWorkloadClass::System
    }
}

fn chat_task_ref_from_trace(
    trace: &magicllm::LlmTraceContext,
) -> Option<magicllm::dispatch::TaskRef> {
    let session = trace
        .chat_session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let task_id = trace
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| session.clone())?;
    let mut task_ref = magicllm::dispatch::TaskRef::task(task_id);
    if let Some(session) = session {
        task_ref = task_ref.with_chat_session(session);
    }
    if let (Some(root), Some(execution)) = (
        optional_trace_id(&trace.root_execution_id),
        optional_trace_id(&trace.execution_id),
    ) {
        task_ref = task_ref.with_execution(root, execution);
    }
    if let Some(turn) = optional_trace_id(&trace.chat_turn_id) {
        task_ref = task_ref.with_chat_turn(turn);
    }
    if let Some(user_message) = optional_trace_id(&trace.user_message_id) {
        task_ref = task_ref.with_user_message(user_message);
    }
    if let Some(iteration) = optional_trace_id(&trace.iteration_id) {
        task_ref = task_ref.with_iteration(iteration);
    }
    if trace.scope.is_valid() {
        task_ref.scope = Some(trace.scope.clone());
    }
    Some(task_ref)
}

fn optional_trace_id(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn extract_text_from_messages(messages: &[RouterMessage]) -> Option<String> {
    for message in messages {
        for block in &message.content {
            match block {
                RouterContentBlock::Text { text } if !text.trim().is_empty() => {
                    return Some(text.clone());
                },
                RouterContentBlock::Json { value }
                    if as_anthropic_raw_content_block(value).is_none() =>
                {
                    return Some(value.to_string());
                },
                _ => continue,
            }
        }
    }
    None
}

/// Returns the prompt-cache config to attach to chat-completion-bound
/// `RouterRequest`s. 1-hour TTL trades a 2x cache-write premium (vs the
/// default 5-min ephemeral) for cache survival across natural chat
/// pauses — users often go 5-30 minutes between turns and the default
/// TTL would expire the cached prefix repeatedly. Break-even is ~3
/// cache reads per write (write costs 2x, read costs 0.1x on Anthropic);
/// every chat session with 3+ turns within an hour comes out ahead, and
/// in practice most do. Returns `None` for non-chat operations
/// (one-shot extraction / classification calls) where caching ROI is
/// already covered by the default 5-min ephemeral or by zero benefit.
fn chat_prompt_cache_config_for_operation(operation: &str) -> Option<PromptCacheConfig> {
    if operation == LLMOperation::ChatCompletion.as_str() {
        Some(PromptCacheConfig::enabled_with_ttl("1h"))
    } else {
        None
    }
}

fn convert_usage(usage: &RouterTokenUsage) -> LLMUsage {
    // Forward the full provider usage breakdown — including cache-read,
    // cache-write, and reasoning — so the chat outer-loop and analytics
    // sinks can observe prompt-cache efficiency end-to-end. The earlier
    // shape collapsed cache + reasoning to zero, which hid the very
    // metrics the prompt-cache plan exists to optimize.
    let prompt_tokens = usage.prompt_tokens.unwrap_or(0);
    let completion_tokens = usage.completion_tokens.unwrap_or(0);
    let total_tokens = usage
        .total_tokens
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));

    LLMUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
        reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
        cache_read_tokens: usage.cached_tokens.unwrap_or(0),
        cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0),
    }
}

/// Convert legacy `LlmConfig` maps into an `LLMRouterConfig` that
/// `ConfiguredRouter` can bootstrap (single source of provider instantiation).
fn build_router_config(
    llm_configs: &HashMap<String, LlmConfig>,
    operation_mapping: &HashMap<String, String>,
) -> LLMRouterConfig {
    let default_profile = if llm_configs.contains_key("llm-openai-small") {
        "llm-openai-small".to_string()
    } else {
        llm_configs
            .keys()
            .next()
            .cloned()
            .unwrap_or_else(|| "default".to_string())
    };

    let mut profiles = HashMap::new();
    for (name, config) in llm_configs {
        profiles.insert(name.clone(), convert_to_profile(config));
    }

    // Convert legacy `HashMap<String, String>` operation_mapping into
    // the new `OperationProfileSelector` shape (Simple variant). All
    // tests/internal callers passing a string mapping continue to work.
    let operation_mapping = operation_mapping
        .iter()
        .map(|(op, profile)| {
            (
                op.clone(),
                magicllm::config::OperationProfileSelector::from(profile.clone()),
            )
        })
        .collect();

    LLMRouterConfig {
        profiles,
        adaptive_profiles: std::collections::HashMap::new(),
        operation_mapping,
        default_profile,
        // `realtime_voice` was added to `LLMRouterConfig` as part of
        // the 2026-05-19 voice refactor: realtime voice providers are
        // now resolved per-call through `OperationLlmRouter` against
        // this config section. Existing construction sites that don't
        // configure realtime voice get the default (which is an empty
        // profile set — realtime voice will be unavailable unless an
        // operator explicitly configures it in
        // `magician-config.yaml`).
        realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
        locality: Default::default(),
    }
}

fn convert_to_profile(config: &LlmConfig) -> LLMProfile {
    let mut metadata_map = config.additional_params.clone().unwrap_or_default();

    let reasoning_strategy = metadata_map
        .remove("reasoning_strategy")
        .or_else(|| metadata_map.remove("thinking_strategy"))
        .and_then(|value| value.as_str().map(|s| s.to_string()));

    let reasoning_max_tokens = metadata_map
        .remove("reasoning_max_tokens")
        .and_then(|value| value.as_u64().map(|v| v as u32));

    if let Some(verbosity) = config.verbosity.as_ref() {
        metadata_map
            .entry("verbosity".to_string())
            .or_insert_with(|| Value::String(verbosity.clone()));
    }

    let metadata = if metadata_map.is_empty() {
        None
    } else {
        Some(metadata_map)
    };

    let reasoning_effort = config.reasoning_effort.clone();
    let reasoning = if reasoning_effort.is_some()
        || reasoning_strategy.is_some()
        || reasoning_max_tokens.is_some()
    {
        Some(ReasoningDefaults {
            effort: reasoning_effort.unwrap_or_else(|| "default".to_string()),
            max_reasoning_tokens: reasoning_max_tokens,
            strategy: reasoning_strategy,
            summary: None,
        })
    } else {
        None
    };

    LLMProfile {
        provider: config.provider.clone(),
        model: config.model.clone(),
        api_key_env: config.api_key_env.clone(),
        api_base_url: config.api_base_url.clone(),
        temperature: config.temperature,
        max_output_tokens: config.max_tokens,
        default_modality: None,
        reasoning,
        metadata,
        supports_vision: config.supports_vision,
        supports_reasoning: config.supports_reasoning,
        supports_tool_calling: config.supports_tool_calling,
        supports_computer_use: config.supports_computer_use,
        timeout_secs: None,
        context_window_tokens: None,
        chunking: None,
    }
}

/// Wrapper that implements QueryAnalysisLLM for a specific operation
pub struct OperationLLMWrapper {
    service: Arc<MultiLLMService>,
    operation: LLMOperation,
}

impl OperationLLMWrapper {
    pub fn new(service: Arc<MultiLLMService>, operation: LLMOperation) -> Self {
        Self { service, operation }
    }
}

#[async_trait]
impl QueryAnalysisLLM for OperationLLMWrapper {
    async fn generate_analysis(&self, prompt: &str) -> Result<LLMResponse> {
        self.service
            .generate_for_operation(&self.operation, prompt)
            .await
    }

    async fn is_available(&self) -> bool {
        self.service
            .get_config_for_operation(&self.operation)
            .is_ok()
    }

    fn provider_name(&self) -> String {
        self.service
            .provider_for_operation(&self.operation)
            .unwrap_or_else(|| "unknown".to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use anyhow::Context as _;
    use serde_json::json;

    #[test]
    fn pi_run_profile_uses_the_chat_profile_mapping() {
        let mut selected = LlmConfig {
            model: "model-for-pi".into(),
            api_key_env: Some("PI_RUN_TEST_KEY".into()),
            api_base_url: Some("https://example.test/v1".into()),
            reasoning_effort: Some("high".into()),
            ..LlmConfig::default()
        };
        selected.additional_params =
            Some(HashMap::from([("tool_choice".to_string(), json!("auto"))]));
        let router = build_router_config(
            &HashMap::from([("selected".to_string(), selected)]),
            &HashMap::new(),
        );
        let resolved = resolve_pi_profile_config(&router, "selected").unwrap();
        assert_eq!(resolved.model, "model-for-pi");
        assert_eq!(resolved.api_key_env.as_deref(), Some("PI_RUN_TEST_KEY"));
        assert_eq!(
            resolved.api_base_url.as_deref(),
            Some("https://example.test/v1")
        );
        assert_eq!(resolved.reasoning_effort.as_deref(), Some("high"));
        assert!(resolve_pi_profile_config(&router, "missing").is_err());
    }

    #[test]
    fn chat_response_boundary_moves_uniquely_owned_large_lanes() {
        let arguments = Value::String("argument".repeat(8_192));
        let argument_ptr = arguments.as_str().expect("string argument").as_ptr();
        let calls = Arc::new(vec![RouterToolCall {
            id: "call-1".to_string(),
            name: "lookup".to_string(),
            arguments,
        }]);
        let raw = Value::String("provider".repeat(8_192));
        let raw_ptr = raw.as_str().expect("string response").as_ptr();

        let calls = into_owned_router_tool_calls(calls);
        let raw = into_owned_provider_response(Arc::new(raw));

        assert_eq!(
            calls[0]
                .arguments
                .as_str()
                .expect("retained argument")
                .as_ptr(),
            argument_ptr,
            "the ordinary final-owner boundary must move, not clone, arguments"
        );
        assert_eq!(
            raw.as_str().expect("retained response").as_ptr(),
            raw_ptr,
            "the ordinary final-owner boundary must move, not clone, raw response"
        );
    }

    #[test]
    fn shared_chat_response_json_fallback_is_heap_framed_on_a_small_stack() {
        std::thread::Builder::new()
            .name("chat-response-cow-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut deep = Value::Null;
                for _ in 0..10_000 {
                    deep = Value::Array(vec![deep]);
                }
                let shared = Arc::new(deep);
                let mut copied = into_owned_provider_response(Arc::clone(&shared));
                crate::magician_v2::json_traversal::discard_json_iteratively(std::mem::take(
                    &mut copied,
                ));
                let original = Arc::try_unwrap(shared).expect("only original owner remains");
                crate::magician_v2::json_traversal::discard_json_iteratively(original);
            })
            .expect("spawn response COW regression")
            .join()
            .expect("response COW regression completes");
    }

    #[test]
    fn chat_trace_override_preserves_the_caller_owned_attempt_counter() {
        let trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let counter = Arc::new(AtomicU32::new(3));
        let overrides = ChatCompletionRequestOverrides {
            extra: None,
            trace_context: Some(trace.clone()),
            provider_attempt_counter: Some(Arc::clone(&counter)),
            disclosure_guard: None,
        };
        let mut request = RouterRequest::default();

        MultiLLMService::apply_trace_override(&mut request, Some(&overrides));

        assert_eq!(request.metadata.trace_context, Some(trace));
        assert!(Arc::ptr_eq(
            request
                .metadata
                .provider_attempt_counter
                .as_ref()
                .expect("attempt counter"),
            &counter,
        ));
        assert_eq!(request.metadata.provider_attempt_count(), 3);
    }

    #[test]
    fn chat_task_ref_uses_session_when_task_id_is_absent() {
        let mut trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        trace.chat_session_id = Some("chat-session-1".to_string());
        trace.chat_turn_id = Some("turn-9".to_string());
        let task = chat_task_ref_from_trace(&trace).expect("session-backed task ref");
        assert_eq!(task.task_id, "chat-session-1");
        assert_eq!(task.chat_session_id.as_deref(), Some("chat-session-1"));
        assert_eq!(task.chat_turn_id.as_deref(), Some("turn-9"));
        assert_eq!(
            task.scope.as_ref().map(|scope| scope.principal.as_str()),
            Some("owner")
        );
    }

    #[test]
    fn chat_task_ref_prefers_trace_task_id() {
        let mut trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        trace.task_id = Some("artifact-task".to_string());
        trace.chat_session_id = Some("chat-session-1".to_string());
        let task = chat_task_ref_from_trace(&trace).expect("task-backed task ref");
        assert_eq!(task.task_id, "artifact-task");
        assert_eq!(task.chat_session_id.as_deref(), Some("chat-session-1"));
    }

    #[test]
    fn chat_task_ref_is_absent_without_task_or_session() {
        let trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        assert!(chat_task_ref_from_trace(&trace).is_none());
    }

    #[test]
    fn dispatch_queue_starts_unwired_so_chat_streams_stay_direct() {
        let service = MultiLLMService::new(HashMap::new(), HashMap::new());
        assert!(
            service.dispatch_queue.get().is_none(),
            "chat must keep the direct ConfiguredRouter path until boot wires the queue"
        );
    }

    #[test]
    fn traced_route_failure_survives_context_layers_without_inventing_attempts() {
        for attempt_count in [0, 2] {
            let context = magicllm::LlmTraceContext::new(
                magicllm::LlmScope::new("owner", "workspace"),
                magicllm::LlmWorkloadClass::ForegroundChat,
            );
            let expected_call_id = context.llm_call_id.clone();
            let error = MultiLLMService::traced_route_error(
                magicllm::LLMError::Configuration("missing API key".to_string()),
                context,
                attempt_count,
            );
            let wrapped = Err::<(), _>(error)
                .context("streaming chat completion failed")
                .expect_err("wrapped error");

            let receipt = MultiLLMService::trace_receipt_from_error(&wrapped)
                .expect("receipt in anyhow source chain");
            assert_eq!(receipt.context.llm_call_id, expected_call_id);
            assert_eq!(receipt.provider_attempt_count, attempt_count);
            assert_eq!(
                receipt.provider_attempt_id,
                (attempt_count > 0).then(|| receipt.context.provider_attempt_id(attempt_count))
            );
        }
    }

    #[test]
    fn traced_route_failure_exposes_the_effective_fallback_route() {
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let error = Err::<(), _>(MultiLLMService::traced_route_error(
            magicllm::LLMError::Provider {
                provider: "fallback-provider".to_string(),
                message: "request failed".to_string(),
            }
            .with_route(
                "fallback-profile",
                LLMProviderKind::Custom("fallback-provider".to_string()),
                "fallback-model",
            ),
            context,
            2,
        ))
        .context("chat completion failed")
        .expect_err("wrapped route failure");

        let route = MultiLLMService::route_identity_from_error(&error)
            .expect("effective route in source chain");
        assert_eq!(route.profile.as_deref(), Some("fallback-profile"));
        assert_eq!(route.provider, "fallback-provider");
        assert_eq!(route.model, "fallback-model");
    }

    #[test]
    fn test_llm_operation_from_str() {
        assert_eq!(
            LLMOperation::from_str("task_decomposition"),
            LLMOperation::TaskDecomposition
        );
        assert_eq!(
            LLMOperation::from_str("QUERY_ANALYSIS"),
            LLMOperation::QueryAnalysis
        );
        assert_eq!(
            LLMOperation::from_str("entity_mapping"),
            LLMOperation::EntityMapping
        );
        assert_eq!(
            LLMOperation::from_str("slot_extraction"),
            LLMOperation::SlotExtraction
        );

        match LLMOperation::from_str("custom_operation") {
            LLMOperation::Other(s) => assert_eq!(s, "custom_operation"),
            _ => panic!("Expected Other variant"),
        }
    }

    #[test]
    fn test_llm_config_deserializes_from_legacy_yaml() {
        let yaml = r#"
provider: openai
model: gpt-5.6-terra
api_key_env: OPENAI_API_KEY
max_tokens: 2000
"#;

        let config: LlmConfig = serde_yaml::from_str(yaml).expect("config should parse");
        assert_eq!(config.provider, LLMProviderKind::OpenAI);
        assert_eq!(config.model, "gpt-5.6-terra");
        assert_eq!(config.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(config.max_tokens, Some(2000));
    }

    #[test]
    fn test_multi_llm_service_creation() {
        let mut llm_configs = HashMap::new();

        let small_config = LlmConfig {
            provider: LLMProviderKind::OpenAI,
            model: "gpt-5.6-terra".to_string(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            ..Default::default()
        };

        llm_configs.insert("llm-openai-small".to_string(), small_config);

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("query_analysis".to_string(), "llm-openai-small".to_string());

        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let state = service.current_state();

        assert_eq!(state.default_config, "llm-openai-small");
    }

    #[test]
    fn chat_completion_telemetry_resolves_profile_provider_model_and_cost() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "chat-gpt5".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5".to_string(),
                ..Default::default()
            },
        );

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("chat_completion".to_string(), "chat-gpt5".to_string());

        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let usage = RouterTokenUsage {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(500),
            total_tokens: Some(1_500),
            reasoning_tokens: Some(25),
            cached_tokens: Some(200),
            cache_creation_tokens: Some(0),
        };

        let telemetry = service
            .build_chat_completion_telemetry(None, Some(&usage), 12345, None, None, None)
            .expect("telemetry");

        assert_eq!(telemetry.provider, "openai");
        assert_eq!(telemetry.model, "gpt-5");
        assert!(telemetry.usage_reported);
        assert_eq!(telemetry.profile.as_deref(), Some("chat-gpt5"));
        assert_eq!(telemetry.input_tokens, 1_000);
        assert_eq!(telemetry.output_tokens, 500);
        assert_eq!(telemetry.reasoning_tokens, 25);
        assert_eq!(telemetry.cache_read_tokens, 200);
        assert_eq!(telemetry.started_at_ms, 12345);
        assert_eq!(
            telemetry.cost_usd,
            magicllm::compute_cost(&LLMProviderKind::OpenAI, "gpt-5", &usage)
        );

        let hint = service
            .chat_completion_telemetry_hint(Some("chat-gpt5"))
            .expect("hint");
        assert_eq!(hint.provider, "openai");
        assert_eq!(hint.model, "gpt-5");
        assert_eq!(hint.profile.as_deref(), Some("chat-gpt5"));
    }

    #[test]
    fn identity_only_chat_telemetry_marks_provider_usage_as_unreported() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "chat-gpt5".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5".to_string(),
                ..Default::default()
            },
        );
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("chat_completion".to_string(), "chat-gpt5".to_string());
        let service = MultiLLMService::new(llm_configs, operation_mapping);

        let telemetry = service
            .build_chat_completion_telemetry(None, None, 12345, None, None, None)
            .expect("identity telemetry");

        assert!(!telemetry.usage_reported);
        assert_eq!(telemetry.input_tokens, 0);
        assert_eq!(telemetry.output_tokens, 0);
    }

    #[test]
    fn chat_telemetry_prefers_effective_fallback_route_for_identity_and_pricing() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "requested-chat".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                ..Default::default()
            },
        );
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("chat_completion".to_string(), "requested-chat".to_string());
        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let usage = RouterTokenUsage {
            prompt_tokens: Some(1_000),
            completion_tokens: Some(100),
            total_tokens: Some(1_100),
            ..RouterTokenUsage::default()
        };
        let effective = magicllm::LlmRouteIdentity {
            profile: "fallback-anthropic".to_string(),
            provider: LLMProviderKind::Anthropic,
            model: "claude-sonnet-4-6".to_string(),
        };

        let telemetry = service
            .build_chat_completion_telemetry(
                None,
                Some(&usage),
                12345,
                None,
                None,
                Some(&effective),
            )
            .expect("fallback telemetry");

        assert_eq!(telemetry.provider, "anthropic");
        assert_eq!(telemetry.model, "claude-sonnet-4-6");
        assert_eq!(telemetry.profile.as_deref(), Some("fallback-anthropic"));
        assert_eq!(
            telemetry.cost_usd,
            magicllm::compute_cost_at(
                &LLMProviderKind::Anthropic,
                "claude-sonnet-4-6",
                &usage,
                12345,
            )
        );
    }

    #[test]
    fn attempted_chat_response_never_uses_requested_route_as_effective_route() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "requested-chat".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "requested-model".to_string(),
                ..Default::default()
            },
        );
        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("chat_completion".to_string(), "requested-chat".to_string());
        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::ForegroundChat,
        );
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(trace.clone(), 1);

        let telemetry = service
            .build_chat_completion_telemetry(
                Some("requested-chat"),
                None,
                12345,
                None,
                Some(receipt),
                None,
            )
            .expect("identity-only telemetry");

        assert!(telemetry.provider.is_empty());
        assert!(telemetry.model.is_empty());
        assert_eq!(telemetry.profile, None);
        assert_eq!(telemetry.cost_usd, 0.0);
        assert_eq!(
            telemetry
                .trace_receipt
                .as_ref()
                .map(|receipt| receipt.context.llm_call_id.as_str()),
            Some(trace.llm_call_id.as_str()),
        );
    }

    #[test]
    fn test_router_handles_all_providers() {
        std::env::set_var("OPENAI_API_KEY", "test-openai");
        std::env::set_var("ANTHROPIC_API_KEY", "test-anthropic");
        std::env::set_var("OPENROUTER_API_KEY", "test-openrouter");

        let mut llm_configs = HashMap::new();

        let mut openai = LlmConfig {
            provider: LLMProviderKind::OpenAI,
            model: "gpt-5.6-terra".to_string(),
            api_key_env: Some("OPENAI_API_KEY".to_string()),
            ..Default::default()
        };
        openai.additional_params = Some(HashMap::from([(
            "api_version".to_string(),
            serde_json::json!("responses"),
        )]));

        let anthropic = LlmConfig {
            provider: LLMProviderKind::Anthropic,
            model: "claude-sonnet-4-5".to_string(),
            api_key_env: Some("ANTHROPIC_API_KEY".to_string()),
            ..Default::default()
        };

        let openrouter = LlmConfig {
            provider: LLMProviderKind::OpenRouter,
            model: "openrouter/anthropic/claude-haiku-4-5".to_string(),
            api_key_env: Some("OPENROUTER_API_KEY".to_string()),
            ..Default::default()
        };

        let ollama = LlmConfig {
            provider: LLMProviderKind::Ollama,
            model: "llama3".to_string(),
            api_key_env: None,
            api_base_url: Some("http://localhost:11434/api/generate".to_string()),
            ..Default::default()
        };

        llm_configs.insert("router-openai".into(), openai);
        llm_configs.insert("router-anthropic".into(), anthropic);
        llm_configs.insert("router-openrouter".into(), openrouter);
        llm_configs.insert("router-ollama".into(), ollama);

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert("analysis".into(), "router-openai".into());
        operation_mapping.insert("anthropic".into(), "router-anthropic".into());
        operation_mapping.insert("proxy".into(), "router-openrouter".into());
        operation_mapping.insert("local".into(), "router-ollama".into());

        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let state = service.current_state();
        assert!(
            state.configured_router.is_some(),
            "router should initialize"
        );

        let provider = service.provider_for_operation(&LLMOperation::Other("local".into()));
        assert_eq!(provider.as_deref(), Some("ollama"));

        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENROUTER_API_KEY");
    }

    #[test]
    fn list_chat_profiles_excludes_openai_chat_mode_profiles() {
        let mut llm_configs = HashMap::new();

        llm_configs.insert(
            "llm-openai-chat-legacy".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                additional_params: Some(HashMap::from([
                    ("tool_choice".to_string(), json!("auto")),
                    ("openai_api_mode".to_string(), json!("chat")),
                ])),
                ..Default::default()
            },
        );
        llm_configs.insert(
            "llm-openai-chat-auto".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                additional_params: Some(HashMap::from([
                    ("tool_choice".to_string(), json!("auto")),
                    ("openai_api_mode".to_string(), json!("auto")),
                ])),
                ..Default::default()
            },
        );
        llm_configs.insert(
            "llm-openai-chat-responses".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                additional_params: Some(HashMap::from([
                    ("tool_choice".to_string(), json!("auto")),
                    ("openai_api_mode".to_string(), json!("responses")),
                ])),
                ..Default::default()
            },
        );
        llm_configs.insert(
            "llm-claude-chat".to_string(),
            LlmConfig {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                additional_params: Some(HashMap::from([(
                    "tool_choice".to_string(),
                    json!({ "type": "auto" }),
                )])),
                ..Default::default()
            },
        );

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "chat_completion".to_string(),
            "llm-openai-chat-legacy".to_string(),
        );

        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let profiles = service.list_chat_profiles();
        let warnings = service.list_chat_profile_warnings();

        // Picker order is provider-bucketed (OpenAI → Anthropic → Gemini …
        // see the `provider_rank` helper in `compute_chat_profiles_and_warnings`),
        // then alphabetical within a bucket. The eligible names here cross
        // two buckets (openai + anthropic), so we expect the two OpenAI
        // entries first, then the Claude entry.
        assert_eq!(
            profiles
                .iter()
                .map(|profile| profile.name.as_str())
                .collect::<Vec<_>>(),
            vec![
                "llm-openai-chat-auto",
                "llm-openai-chat-responses",
                "llm-claude-chat",
            ]
        );
        assert!(profiles.iter().all(|profile| !profile.is_default));
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].profile_name, "llm-openai-chat-legacy");
        assert!(warnings[0].message.contains("openai_api_mode: chat"));
    }

    #[test]
    fn list_chat_profiles_keeps_openai_auto_profile_as_default() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "llm-openai-chat-auto".to_string(),
            LlmConfig {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                additional_params: Some(HashMap::from([
                    ("tool_choice".to_string(), json!("auto")),
                    ("openai_api_mode".to_string(), json!("auto")),
                ])),
                ..Default::default()
            },
        );

        let mut operation_mapping = HashMap::new();
        operation_mapping.insert(
            "chat_completion".to_string(),
            "llm-openai-chat-auto".to_string(),
        );

        let service = MultiLLMService::new(llm_configs, operation_mapping);
        let profiles = service.list_chat_profiles();

        assert_eq!(profiles.len(), 1);
        assert_eq!(profiles[0].name, "llm-openai-chat-auto");
        assert!(profiles[0].is_default);
        assert!(profiles[0].supports_user_image_inputs);
        assert!(service.list_chat_profile_warnings().is_empty());
    }

    #[test]
    fn list_chat_profiles_exposes_image_input_capability() {
        let mut llm_configs = HashMap::new();
        llm_configs.insert(
            "llm-claude-chat".to_string(),
            LlmConfig {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                additional_params: Some(HashMap::from([(
                    "tool_choice".to_string(),
                    json!({ "type": "auto" }),
                )])),
                ..Default::default()
            },
        );
        llm_configs.insert(
            "chat-m27-toolsauto-rnone".to_string(),
            LlmConfig {
                provider: LLMProviderKind::Minimax,
                model: "MiniMax-M2.7".to_string(),
                supports_vision: Some(false),
                additional_params: Some(HashMap::from([(
                    "tool_choice".to_string(),
                    json!({ "type": "auto" }),
                )])),
                ..Default::default()
            },
        );

        let service = MultiLLMService::new(llm_configs, HashMap::new());
        let profiles = service.list_chat_profiles();

        let claude = profiles
            .iter()
            .find(|profile| profile.name == "llm-claude-chat")
            .expect("claude profile");
        let minimax = profiles
            .iter()
            .find(|profile| profile.name == "chat-m27-toolsauto-rnone")
            .expect("minimax profile");

        assert!(claude.supports_user_image_inputs);
        assert!(!minimax.supports_user_image_inputs);
    }

    #[test]
    fn reload_from_router_config_preserves_default_profile_and_router_metadata() {
        let router_config: LLMRouterConfig = serde_yaml::from_str(
            r#"
default_profile: llm-secondary
profiles:
  llm-primary:
    provider: openai
    model: gpt-5.6-terra
    metadata:
      fallback_profile: llm-secondary
  llm-secondary:
    provider: anthropic
    model: claude-sonnet-4-6
    timeout_secs: 123
    default_modality: text
    reasoning:
      effort: high
      max_reasoning_tokens: 456
      strategy: visible
operation_mapping:
  chat_completion: llm-primary
"#,
        )
        .expect("router config should parse");

        let service = MultiLLMService::from_router_config(&router_config);
        let state = service.current_state();

        assert_eq!(state.default_config, "llm-secondary");
        assert_eq!(
            state
                .router_config
                .as_ref()
                .map(|config| config.default_profile.as_str()),
            Some("llm-secondary")
        );

        let preserved_profile = state
            .router_config
            .as_ref()
            .and_then(|config| config.profiles.get("llm-secondary"))
            .expect("secondary profile should exist");
        assert_eq!(preserved_profile.timeout_secs, Some(123));
        assert_eq!(
            preserved_profile.default_modality,
            Some(magicllm::capability::LLMModality::Text)
        );
        assert_eq!(
            preserved_profile
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.max_reasoning_tokens),
            Some(456)
        );
        assert_eq!(
            preserved_profile
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.strategy.as_deref()),
            Some("visible")
        );
        assert_eq!(
            service
                .provider_for_operation(&LLMOperation::ChatCompletion)
                .as_deref(),
            Some("openai")
        );
    }
}

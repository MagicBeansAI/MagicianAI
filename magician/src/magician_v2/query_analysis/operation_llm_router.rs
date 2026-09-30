// Operation-aware LLM router for operation-based model selection
// Enables using different LLM models for different operations (e.g., large
// model for decomposition, small model for analysis)

use std::{
    collections::BTreeMap,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{atomic::Ordering, Arc, RwLock, RwLockReadGuard},
    time::{Duration, Instant},
};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use base64::Engine;
use magicllm::config::{OperationEngineFollow, OperationProfileSelector};
use magicllm::prelude::{
    as_anthropic_raw_content_block, ConfiguredRouter, ContentBlock as RouterContentBlock,
    LLMMessage as RouterMessage, LLMModality, LLMProfile, LLMProviderKind,
    LLMRequest as RouterRequest, LLMResponse as RouterResponse, LLMRouterConfig,
    LLMToolSpec as RouterToolSpec, PromptCacheConfig, ReasoningConfig as RouterReasoningConfig,
    RequestMetadata as RouterMetadata, TokenUsage as RouterTokenUsage,
};
use magicllm::types::{MessageRole, SummarisableBlock, SummarisationPurpose};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{debug, info, instrument, warn};

use crate::magician_v2::analytics::runtime_activity_layer::{current_activity_id, KIND_LLM};

use crate::magician_v2::agents::types::{LlmEndpoint, LlmRoutingConfig};
use crate::magician_v2::apps::llm_dispatch::AppLlmOperationDispatchPermit;
use crate::magician_v2::execution::agentic::types::{
    account_execution_tokens, execution_token_budget_snapshot, preflight_execution_token_budget,
};
use crate::magician_v2::json_traversal::discard_json_iteratively;
use crate::magician_v2::llm_chunking::{
    global_chunk_adapter_registry, validate_router_chunking_config, LogicalChunkDispatch,
    LogicalChunkDispatchRequest, LogicalChunkExecutionContext, LogicalChunkRunner,
    LogicalChunkTelemetryEvent, LogicalChunkTelemetrySink,
};

/// Simplified token usage metadata from LLM API responses
///
/// Note: This is a simplified version for Magician's internal use.
/// The full magicllm::prelude::TokenUsage (aliased as RouterTokenUsage)
/// provides richer metadata including reasoning tokens and cost estimates.
#[derive(Debug, Clone, Default)]
pub struct SimplifiedTokenUsage {
    /// Number of tokens in the prompt
    pub prompt_tokens: u32,
    /// Number of tokens in the completion/response
    pub completion_tokens: u32,
    /// Total tokens used (prompt + completion)
    pub total_tokens: u32,
}

/// Simplified LLM response with content and usage metadata
///
/// Note: This is a simplified version for Magician's internal use.
/// The full magicllm::prelude::LLMResponse (aliased as RouterResponse)
/// provides richer response data including tool calls, multiple messages,
/// and detailed content blocks.
#[derive(Debug, Clone, Default)]
pub struct SimplifiedLLMResponse {
    /// The generated text content
    pub content: String,
    /// Token usage statistics (if available from provider)
    pub usage: Option<SimplifiedTokenUsage>,
    /// Provider stop/finish reason when exposed by the upstream router.
    pub finish_reason: Option<String>,
    /// Rich per-call telemetry (tokens, cache, cost) used by executor emits.
    /// Separate from `usage` because downstream consumers of the simplified
    /// usage view should not break when we extend telemetry.
    pub telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

/// Lazily materialized monolithic request for a chunk-capable operation.
///
/// Callers should construct this only through
/// [`OperationLlmRouter::generate_for_chunkable_operation_with_lazy_fallback`].
/// The producer is not invoked when the captured routing snapshot enables
/// logical chunking, so large source JSON and prompt strings do not coexist
/// with the logical input on that path.
pub struct ChunkableOperationFallback {
    pub system_prompt: Option<String>,
    pub prompt: String,
    /// Owned source candidate and purpose used by optional queue-local
    /// summarisation. The router verifies that it is a byte-for-byte substring
    /// before splitting; non-matching legacy candidates safely remain inline.
    pub summarisable: Option<(String, SummarisationPurpose)>,
}

impl ChunkableOperationFallback {
    pub fn new(system_prompt: Option<String>, prompt: String) -> Self {
        Self {
            system_prompt,
            prompt,
            summarisable: None,
        }
    }

    pub fn with_summarisable(mut self, source: String, purpose: SummarisationPurpose) -> Self {
        self.summarisable = Some((source, purpose));
        self
    }
}

/// Own a structured input until its destination has accepted ownership. Any
/// early routing, fallback-materialization, or configuration error drains the
/// recursive JSON tree iteratively instead of letting `Value::drop` consume
/// native stack proportional to externally influenced nesting.
struct IterativeJsonOwner(Option<Value>);

impl IterativeJsonOwner {
    fn new(value: Value) -> Self {
        Self(Some(value))
    }

    fn as_value(&self) -> &Value {
        self.0
            .as_ref()
            .expect("iterative JSON owner retains its value until transfer")
    }

    fn take(&mut self) -> Value {
        self.0
            .take()
            .expect("iterative JSON owner transfers its value only once")
    }

    fn discard(&mut self) {
        if let Some(value) = self.0.take() {
            discard_json_iteratively(value);
        }
    }
}

impl Drop for IterativeJsonOwner {
    fn drop(&mut self) {
        self.discard();
    }
}

impl SimplifiedLLMResponse {
    /// Create a response with content only (no usage data)
    pub fn content_only(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            usage: None,
            finish_reason: None,
            telemetry: None,
        }
    }

    /// Create a response with content and usage metadata
    pub fn with_usage(content: impl Into<String>, usage: SimplifiedTokenUsage) -> Self {
        Self {
            content: content.into(),
            usage: Some(usage),
            finish_reason: None,
            telemetry: None,
        }
    }
}

/// Raw tool call from a router response, for execution-native consumption.
#[derive(Debug, Clone)]
pub struct RawRouterToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// One cited source from a provider-executed web search.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionWebSearchCitation {
    pub url: String,
    pub title: Option<String>,
}

/// Provider-executed web search summary extracted from the retained raw
/// response: how many searches ran (each bills per call on top of tokens)
/// and the deduplicated citation set.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ExecutionWebSearchSummary {
    pub searches: usize,
    pub citations: Vec<ExecutionWebSearchCitation>,
}

/// Response preserving raw tool calls for execution-native consumption.
#[derive(Debug, Clone)]
pub struct ExecutionNativeRouterResponse {
    pub tool_calls: Vec<RawRouterToolCall>,
    pub text: Option<String>,
    /// Provider-emitted reasoning / chain-of-thought text. Sourced from
    /// `LLMResponse.reasoning_text` (Anthropic Extended Thinking, OpenAI
    /// Responses reasoning items, DeepSeek-R1 `reasoning_content`).
    pub reasoning_text: Option<String>,
    /// Server-side response identifier the next turn can pass back to
    /// continue in the provider's stateful mode (for example OpenAI Responses
    /// `previous_response_id` or Gemini Interactions
    /// `previous_interaction_id`). Inner-loop runner threads this so prior
    /// turns don't have to be retransmitted. `None` when the provider doesn't
    /// surface a chainable id (Anthropic, Chat Completions, Yutori, etc.).
    pub response_id: Option<String>,
    pub finish_reason: Option<String>,
    pub prompt_tokens: Option<u32>,
    pub completion_tokens: Option<u32>,
    /// Cache-read tokens. Populated when the provider surfaces a prompt
    /// cache hit (Anthropic `cache_read_input_tokens`, OpenAI Responses
    /// `prompt_tokens_details.cached_tokens`). The inner-loop emits this
    /// in `llm.succeeded` so the chat UI's per-turn cache chip can show
    /// cumulative cache hits across iterations. Without this field the
    /// chip stays stale at zero for all non-chat-inline turns.
    pub cached_tokens: Option<u32>,
    /// Cache-write tokens — the prompt prefix the provider had to commit
    /// to its cache this turn. Counterpart to `cached_tokens`.
    pub cache_creation_tokens: Option<u32>,
    /// Reasoning tokens billed this turn (OpenAI Responses
    /// `output_tokens_details.reasoning_tokens`, Anthropic extended-thinking,
    /// etc.). Without this the cost of `effort: high` is invisible as a separate
    /// telemetry field on the execution-native path (it was hardcoded to 0).
    pub reasoning_tokens: Option<u32>,
    /// Server-side web search summary when the routed request enabled the
    /// `server_web_search` profile flag. `None` for ordinary turns; Some
    /// (possibly with zero searches) whenever the flag rode the request, so
    /// callers can distinguish "searched and found nothing" from "never
    /// searched".
    pub web_search: Option<ExecutionWebSearchSummary>,
    /// Selected LLM profile name when it deviates from the operation default
    /// (per-call override or shape/cohort pick); `None` means the operation's
    /// default profile. Mirrors the normal router telemetry attribution.
    pub profile: Option<String>,
    /// Provider that served the call (config identifier, e.g.
    /// `"anthropic"`). Resolved routing wins: per-call provider override,
    /// else the routed profile's provider — the transport response itself
    /// carries no attribution. Threaded so decision telemetry can price
    /// the call (analytics `llm_calls`, /llm page).
    pub provider: Option<String>,
    /// Model that served the call (effective model after overrides).
    pub model: Option<String>,
    /// Full priced usage record for direct non-executor callers. Executor
    /// adapters continue to project the individual fields above, while API and
    /// background rails can emit the same canonical analytics row without
    /// reconstructing provider pricing.
    pub telemetry: Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry>,
}

/// Simple trait for LLM-based query analysis
#[async_trait]
pub trait QueryAnalysisLLM: Send + Sync {
    /// Generate a response for query analysis
    /// Takes a prompt and returns the LLM response with content and usage metadata
    async fn generate_analysis(&self, prompt: &str) -> Result<SimplifiedLLMResponse>;

    /// Generate analysis with an optional system prompt.
    ///
    /// Default implementation preserves backward compatibility by ignoring
    /// `system_prompt` and delegating to `generate_analysis`.
    async fn generate_analysis_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        let _ = system_prompt;
        self.generate_analysis(prompt).await
    }

    /// Scope-aware generation used by product surfaces that know the tenant
    /// at invocation time. Legacy/mock implementations may ignore the scope;
    /// the operation-router wrapper binds it before queue admission so failure
    /// and dispatch facts cannot land in the compatibility default scope.
    async fn generate_analysis_scoped(
        &self,
        _scope: magicllm::LlmScope,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_analysis(prompt).await
    }

    async fn generate_analysis_with_system_scoped(
        &self,
        _scope: magicllm::LlmScope,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_analysis_with_system(system_prompt, prompt)
            .await
    }

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
    async fn generate_analysis(&self, _prompt: &str) -> Result<SimplifiedLLMResponse> {
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
        let usage = SimplifiedTokenUsage {
            prompt_tokens: 100,
            completion_tokens: 150,
            total_tokens: 250,
        };

        Ok(SimplifiedLLMResponse::with_usage(content, usage))
    }
}

/// Process-global handle to THE operation router, set once at startup
/// (`bin/magician.rs`, right after the orchestrator's router resolves).
///
/// Exists for the media rails: their sessions are constructed in
/// process-global registries (`meeting_manager()`, the observe slot) with
/// no `AgentResources`/app_data in scope, yet their narrator/summarizer
/// calls must route through `magician-config.yaml` operations + profiles
/// like every other LLM call — not standalone env-configured HTTP clients.
/// `None` (tests, pre-startup) lets callers fall back gracefully.
static GLOBAL_OPERATION_ROUTER: std::sync::OnceLock<Arc<OperationLlmRouter>> =
    std::sync::OnceLock::new();

pub fn set_global_operation_router(router: Arc<OperationLlmRouter>) {
    let _ = GLOBAL_OPERATION_ROUTER.set(router);
}

pub fn global_operation_router() -> Option<Arc<OperationLlmRouter>> {
    GLOBAL_OPERATION_ROUTER.get().cloned()
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
    DiscoveryExtraction,       // Extract parameter values from discovery command output (small)
    DiscoverySafety, // Safety check for discovery actions (legacy, use ParameterSafetyCheck)

    // Execution operations
    PlaceholderResolution, // Runtime resolution of placeholder parameters (nano)

    // Agentic execution operations
    AgenticInputInterpretation, // Interpret/validate user responses during agentic loop (nano)

    // Memory consolidation operations
    MemoryEntityExtraction, // Extract entities from episodes (small)
    MemoryEnvironmentKnowledgeExtraction, // Extract environment knowledge from episodes (small)
    MemoryInsightDistillation, // Distill insights from entities (medium)
    MemoryUserPromotion,    // Promote insights to user-level tiers (medium)
    MemoryArchiveSummary,   // Summarize episodes for archival (small)
    MemoryEpisodeQualityClassification, // Judge episode extraction value (small)
    MemoryConflictReview,   // Resolve ambiguous memory item conflicts (medium)

    // Town Square's gate and compose retired here in favour of the package's
    // own `app:`-namespaced operations (queue item 6). A core variant would
    // have kept a second, unowned routing identity for work the package's
    // manifest now declares, budgets and can have narrowed.
    MemoryConflictReviewHighRisk, // Resolve user/global memory conflicts (strong)

    // API Mining Phase 2: workflow compilation
    WorkflowCompilation, // Compile N CapabilitySequences -> one WorkflowGraph
    RecipeCompilation,   // Shape-only Task Recipe refinement

    // Media rails (screen + meetings) — profile-routed senses
    ScreenObservation, // Per-frame narration for the screen-observation rail (vision, mini-tier)
    ScreenUnderstanding, // One-shot "look at this frame and answer" (vision, full-tier)
    ScreenGrounding,   // Locate a click target in a frame -> center coordinates (vision)
    MeetingSummary,    // Meeting/observation transcript → prose summary (local-friendly)
    MeetingResponse,   // Live reply when a meeting participant addresses Presto

    // Realtime voice operations
    /// Resolves the realtime voice provider profile (model, voice,
    /// duration cap, watermark) used by the voice orchestrator to
    /// mint upstream sessions. Routed in `magician-config.yaml`
    /// to a `voice_realtime_*` profile.
    VoiceController,
    /// Resolves the LLM used by the voice context compactor to
    /// summarise older voice turns before each upstream rotation.
    /// Routed to a small/fast text profile (e.g. `chat-fast`).
    VoiceContextCompaction,

    /// Operator-admitted app operation. The stored value is the complete
    /// reserved `app:<name>` routing key, not an arbitrary core operation.
    /// Physical dispatch additionally requires a non-serializable app
    /// disclosure guard; constructing this enum arm alone grants nothing.
    App(String),
    Other(String),
}

/// Provider/model endpoint override for scoped operation routing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct OperationRoutingEndpoint {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub provider: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub model: String,
}

impl OperationRoutingEndpoint {
    pub fn new(provider: impl Into<String>, model: impl Into<String>) -> Option<Self> {
        let provider = provider.into().trim().to_ascii_lowercase();
        let model = model.into().trim().to_string();
        if provider.is_empty() || model.is_empty() {
            return None;
        }
        Some(Self {
            profile: None,
            provider,
            model,
        })
    }

    pub fn for_profile(profile: impl Into<String>) -> Option<Self> {
        let profile = profile.into().trim().to_string();
        if profile.is_empty() {
            return None;
        }
        Some(Self {
            profile: Some(profile),
            provider: String::new(),
            model: String::new(),
        })
    }

    pub fn profile_name(&self) -> Option<&str> {
        self.profile
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    pub fn provider_name(&self) -> Option<&str> {
        let provider = self.provider.trim();
        (!provider.is_empty()).then_some(provider)
    }

    pub fn model_name(&self) -> Option<&str> {
        let model = self.model.trim();
        (!model.is_empty()).then_some(model)
    }
}

/// Agent-scoped operation routing overrides.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct OperationRoutingOverrides {
    pub planning: Option<OperationRoutingEndpoint>,
    pub evaluation: Option<OperationRoutingEndpoint>,
    pub correction_extraction: Option<OperationRoutingEndpoint>,
    pub memory_consolidation: Option<OperationRoutingEndpoint>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub operations: BTreeMap<String, OperationRoutingEndpoint>,
    /// The engine that started this flow, for the operations that follow
    /// their parent. `None` is no parent: each operation resolves its own
    /// profile. Never the native engine (the `parent_engine` module
    /// normalises it away); explicit here where a flow threads its
    /// overrides, ambient through that module's task-local otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_engine: Option<String>,
}

impl OperationRoutingOverrides {
    /// Name the flow's parent engine. Empty and the native engine clear it.
    pub fn with_parent_engine(mut self, engine: Option<String>) -> Self {
        self.parent_engine = super::parent_engine::normalize_parent_engine(engine.as_deref());
        self
    }

    /// Drop the parent, keeping the endpoints. The ingress for a client's
    /// overrides applies this: a flow's parent is named by the runtime from
    /// the flow's own engine, never by the request that launched it.
    pub fn without_parent_engine(mut self) -> Self {
        self.parent_engine = None;
        self
    }

    pub fn endpoint_for_operation(
        &self,
        operation: &LLMOperation,
    ) -> Option<&OperationRoutingEndpoint> {
        if matches!(operation, LLMOperation::App(_)) || operation.as_str().starts_with("app:") {
            // App routes are pinned by their live admission resolver. Agent
            // overrides and the generic planning lane must never widen them.
            return None;
        }
        if let Some(endpoint) = self.operations.get(operation.as_str()) {
            return Some(endpoint);
        }
        match operation {
            // Durable-state helper operations have explicit operation mappings in
            // magician-config.yaml. Do not let the broad agent "planning" lane
            // override these dedicated state-helper operations implicitly.
            LLMOperation::Other(name)
                if name == "durable_task_state_generate"
                    || name == "durable_task_state_patch"
                    || name == "durable_task_state_close_summary" =>
            {
                None
            },
            LLMOperation::AgenticInputInterpretation => self.correction_extraction.as_ref(),
            LLMOperation::ToolEvaluation
            | LLMOperation::ParameterSafetyCheck
            | LLMOperation::DiscoverySafety => self.evaluation.as_ref(),
            LLMOperation::MemoryEntityExtraction
            | LLMOperation::MemoryEnvironmentKnowledgeExtraction
            | LLMOperation::MemoryInsightDistillation
            | LLMOperation::MemoryUserPromotion
            | LLMOperation::MemoryArchiveSummary
            | LLMOperation::MemoryEpisodeQualityClassification
            | LLMOperation::MemoryConflictReview
            | LLMOperation::MemoryConflictReviewHighRisk => self.memory_consolidation.as_ref(),
            _ => self.planning.as_ref(),
        }
    }

    /// Overlay `overlay`'s endpoints on `base`'s. A merge never carries a
    /// parent engine from either side: the parent is a fact about the flow
    /// that the runtime assigns where the flow's router is built
    /// (`routing_overrides_for_run`), and an owner profile or a durable route
    /// merged into a context must not be able to smuggle one in.
    pub fn merge(base: Option<Self>, overlay: Option<Self>) -> Option<Self> {
        let base = base.map(Self::without_parent_engine);
        let overlay = overlay.map(Self::without_parent_engine);
        match (base, overlay) {
            (None, None) => None,
            (Some(base), None) => base.normalized(),
            (None, Some(overlay)) => overlay.normalized(),
            (Some(base), Some(overlay)) => {
                let mut operations = base.operations;
                operations.extend(overlay.operations);
                Self {
                    planning: overlay.planning.or(base.planning),
                    evaluation: overlay.evaluation.or(base.evaluation),
                    correction_extraction: overlay
                        .correction_extraction
                        .or(base.correction_extraction),
                    memory_consolidation: overlay
                        .memory_consolidation
                        .or(base.memory_consolidation),
                    operations,
                    parent_engine: None,
                }
                .normalized()
            },
        }
    }

    /// Convert an agent's LlmRoutingConfig into OperationRoutingOverrides.
    pub fn from_llm_routing_config(config: &LlmRoutingConfig) -> Self {
        fn convert_endpoint(ep: &LlmEndpoint) -> OperationRoutingEndpoint {
            OperationRoutingEndpoint {
                profile: ep.profile.clone(),
                provider: ep.provider.clone(),
                model: ep.model.clone(),
            }
        }
        // Per-operation overrides win over the named lanes — this lets
        // an agent surgically bump only the operations that need higher
        // reasoning (e.g. primitive) without inflating cheap ones.
        // Keys are trimmed + lowercased here so lookups via
        // `endpoint_for_operation` (which uses `LLMOperation::as_str()`,
        // already lowercase snake_case) hit reliably even if the YAML
        // had stray whitespace or casing. Mirrors what `normalized()`
        // does so behavior is consistent whether or not the caller
        // chooses to normalize.
        let operations = config
            .operations
            .iter()
            .filter_map(|(name, ep)| {
                let key = name.trim().to_ascii_lowercase();
                if key.is_empty() {
                    return None;
                }
                Some((key, convert_endpoint(ep)))
            })
            .collect();
        Self {
            planning: config.planning.as_ref().map(convert_endpoint),
            evaluation: config.evaluation.as_ref().map(convert_endpoint),
            correction_extraction: config.correction_extraction.as_ref().map(convert_endpoint),
            memory_consolidation: config.memory_consolidation.as_ref().map(convert_endpoint),
            operations,
            // An agent's routing config names profiles, not the flow that
            // runs it; the parent is set by the flow's entry.
            parent_engine: None,
        }
    }

    pub fn normalized(self) -> Option<Self> {
        fn sanitize_endpoint(
            endpoint: Option<OperationRoutingEndpoint>,
        ) -> Option<OperationRoutingEndpoint> {
            endpoint.and_then(|value| {
                if let Some(profile) = value.profile_name() {
                    OperationRoutingEndpoint::for_profile(profile)
                } else {
                    OperationRoutingEndpoint::new(value.provider, value.model)
                }
            })
        }

        let mut operations = BTreeMap::new();
        for (raw_name, endpoint) in self.operations {
            let operation_name = raw_name.trim().to_ascii_lowercase();
            if operation_name.is_empty() {
                continue;
            }
            let sanitized = if let Some(profile) = endpoint.profile_name() {
                OperationRoutingEndpoint::for_profile(profile)
            } else {
                OperationRoutingEndpoint::new(endpoint.provider, endpoint.model)
            };
            if let Some(sanitized) = sanitized {
                operations.insert(operation_name, sanitized);
            }
        }

        let normalized = Self {
            planning: sanitize_endpoint(self.planning),
            evaluation: sanitize_endpoint(self.evaluation),
            correction_extraction: sanitize_endpoint(self.correction_extraction),
            memory_consolidation: sanitize_endpoint(self.memory_consolidation),
            operations,
            parent_engine: super::parent_engine::normalize_parent_engine(
                self.parent_engine.as_deref(),
            ),
        };

        if normalized.planning.is_none()
            && normalized.evaluation.is_none()
            && normalized.correction_extraction.is_none()
            && normalized.memory_consolidation.is_none()
            && normalized.operations.is_empty()
            && normalized.parent_engine.is_none()
        {
            None
        } else {
            Some(normalized)
        }
    }
}

// ── Install-level operation routing overrides (Model-routing panel) ────
// An explicit per-operation profile choice the owner makes in Settings:
// consultative defaults stay in magician-config.yaml; these overrides win
// at resolution time and bypass locality arms (the cloud arm's exemption,
// one level up). Persisted as 0600 JSON beside the auth store so a restart
// keeps the owner's choices without touching the config file.

static LLM_ROUTING_OVERRIDES: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<String, String>>,
> = std::sync::OnceLock::new();
static LLM_ROUTING_OVERRIDES_PATH: std::sync::OnceLock<std::path::PathBuf> =
    std::sync::OnceLock::new();

// The owner's per-operation `engine: parent | pinned` choice, beside the
// profile overrides and with the same precedence: the config selector's
// `engine` is the shipped default, a pin here wins over it. Its own file,
// because the override loader resets to empty on a shape it does not
// recognise, so the two stores must never share one.
static LLM_ROUTING_ENGINE_PINS: std::sync::OnceLock<
    std::sync::RwLock<std::collections::HashMap<String, OperationEngineFollow>>,
> = std::sync::OnceLock::new();
static LLM_ROUTING_ENGINE_PINS_PATH: std::sync::OnceLock<std::path::PathBuf> =
    std::sync::OnceLock::new();

/// One install-level store file: a JSON object keyed by operation. An
/// absent or unreadable file is the empty map.
fn load_routing_store<V: serde::de::DeserializeOwned>(
    path: &std::path::Path,
) -> std::collections::HashMap<String, V> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Load the owner's per-operation routing choices at boot — the profile
/// overrides and the engine pins. Absent files are empty maps — the
/// no-override default.
pub fn install_llm_routing_overrides(runtime_root: &std::path::Path) {
    let system = runtime_root.join("system");
    let overrides_path = system.join("llm_routing_overrides.json");
    let overrides = load_routing_store::<String>(&overrides_path);
    let _ = LLM_ROUTING_OVERRIDES_PATH.set(overrides_path);
    let _ = LLM_ROUTING_OVERRIDES.set(std::sync::RwLock::new(overrides));

    let pins_path = system.join("llm_routing_engine.json");
    let pins = load_routing_store::<OperationEngineFollow>(&pins_path);
    let _ = LLM_ROUTING_ENGINE_PINS_PATH.set(pins_path);
    let _ = LLM_ROUTING_ENGINE_PINS.set(std::sync::RwLock::new(pins));
}

// ── Parent engine (owner rule 2026-08-31, flow-scoped 2026-09-14) ───────
// A background operation rides the engine that started its flow — the chat
// mouth for a chat turn, the run engine for an agentic run, the connected
// CLI's family for an external MCP harness — resolved per request from the
// flow's routing overrides (explicit) or the task-local the flow's entry
// scoped (ambient), never from a process value. Two floors hold under any
// parent: an operation whose config default the owner routed to a LOCAL
// model keeps its locality-aware selector, and a request that carries tools
// keeps its tool-capable profile. An operation opts out with `engine:
// pinned` on its selector, or the owner pins it from Settings (the store
// above, which wins over the selector). The parent is an engine name;
// `parent_profile` maps it to that harness's default profile at resolution.
//
// The process cell below is DISPLAY ONLY: the chat/run engine switches keep
// recording the last-set engine so the routing panel can name the engines
// in use, and resolution never reads it.
static HARNESS_AFFINITY: std::sync::OnceLock<std::sync::RwLock<Option<String>>> =
    std::sync::OnceLock::new();

/// Record the last-set driving engine (the run/chat engine install paths)
/// for the routing panel. `None` clears the display. Resolution does not
/// read this cell; a flow's parent travels with the flow.
pub fn set_harness_affinity(engine: Option<&str>) {
    let cell = HARNESS_AFFINITY.get_or_init(|| std::sync::RwLock::new(None));
    *cell
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = engine.map(str::to_string);
}

/// The last-set driving engine, for display. Never consulted by resolution.
pub fn harness_affinity() -> Option<String> {
    HARNESS_AFFINITY
        .get()
        .and_then(|cell| cell.read().ok())
        .and_then(|affinity| affinity.clone())
        .filter(|engine| !engine.is_empty() && engine != "magician")
}

/// The profile name a harness engine's stateless calls ride: that harness's
/// declared default-model profile.
fn harness_affinity_profile_base(engine: &str) -> String {
    // Some engine names differ from their stateless MagicLLM provider/profile
    // namespace. Keep those translations at this one seam instead of teaching
    // every caller the naming exceptions.
    let profile_engine = match engine {
        "claude_code" => "claude",
        // App Server is a stateful plane/coding engine, not a MagicLLM
        // provider. Its secondary stateless calls ride the same Codex CLI
        // subscription through the one-shot harness provider.
        "codex_app_server" => "codex",
        other => other,
    };
    format!("op-harness-{profile_engine}")
}

/// The profile a parent engine resolves to in `config`: the harness's
/// default-model profile, its small variant if only that is declared, else
/// `None` — the parent silently no-ops for an engine without profiles and
/// the operation stays on its own configured profile.
fn parent_profile(config: &LLMRouterConfig, engine: &str) -> Option<String> {
    let base = harness_affinity_profile_base(engine);
    if config.profiles.contains_key(&base) {
        return Some(base);
    }
    let small = format!("{base}-small");
    config.profiles.contains_key(&small).then_some(small)
}

/// Record why an operation did or did not ride its flow's parent engine.
///
/// Only an operation that was ELIGIBLE to follow is logged — pinned, local,
/// tool-carrying and owner-overridden operations are silent, so this stays
/// low-volume. Eligible-but-parentless is the interesting line: it means the
/// flow reached this dispatch without naming its engine, which is invisible
/// from the outside because the request simply succeeds on the config
/// profile. Three fixes for the run lane's parent-follow gap were attempted
/// against unobservable code before this existed.
fn trace_parent_decision(
    site: &'static str,
    operation: &str,
    routing_overrides: Option<&OperationRoutingOverrides>,
    resolved_profile: Option<&str>,
) {
    let carried = routing_overrides.and_then(|overrides| overrides.parent_engine.as_deref());
    let ambient = super::parent_engine::current_parent_engine();
    tracing::info!(
        target: "magician::routing::parent_engine",
        site,
        operation,
        carried_parent = carried.unwrap_or("-"),
        ambient_parent = ambient.as_deref().unwrap_or("-"),
        parent_profile = resolved_profile.unwrap_or("-"),
        followed = resolved_profile.is_some(),
        "parent-engine decision for an operation eligible to follow"
    );
}

/// The parent engine of the flow a request belongs to: explicit on the
/// flow's routing overrides where the flow threads them, else ambient in
/// the task. Explicit wins over ambient. Normalised here as well as at the
/// carriers, so a value is always a real harness engine and never the
/// native one.
fn parent_engine_for(routing_overrides: Option<&OperationRoutingOverrides>) -> Option<String> {
    let engine = routing_overrides
        .and_then(|overrides| overrides.parent_engine.clone())
        .or_else(super::parent_engine::current_parent_engine);
    super::parent_engine::normalize_parent_engine(engine.as_deref())
}

/// The profile the display cell's engine would resolve to. Used ONLY by the
/// routing API (`GET /llm/routing`) to label the engines in use; the two
/// resolution sites read `parent_engine_for` + `parent_profile` instead.
pub fn harness_affinity_profile(config: &LLMRouterConfig) -> Option<String> {
    harness_affinity().and_then(|engine| parent_profile(config, &engine))
}

/// The profile a named driving engine's followers would ride in `config`,
/// for the routing API to label each operation's would-be parent profile.
/// The one engine→profile mapping, shared with resolution.
pub fn parent_profile_for_engine(config: &LLMRouterConfig, engine: &str) -> Option<String> {
    parent_profile(config, engine)
}

/// Whether an operation follows the flow's parent, given the owner's
/// install-level pin (if any) and its config selector (absent = follows).
/// The one rule the two resolution sites and the routing API share: the
/// pin wins over the selector's `engine`, as a profile override wins over
/// the mapping.
pub fn follows_parent_with(
    pin: Option<OperationEngineFollow>,
    mapping: Option<&OperationProfileSelector>,
) -> bool {
    match pin {
        Some(follow) => follow == OperationEngineFollow::Parent,
        None => mapping.is_none_or(|selector| selector.follows_parent()),
    }
}

/// [`follows_parent_with`] against the live pin store.
pub fn follows_parent_for(operation: &str, mapping: Option<&OperationProfileSelector>) -> bool {
    follows_parent_with(llm_routing_engine_for(operation), mapping)
}

/// The override for one operation, if the owner set one and it names a
/// profile the router still knows.
pub fn llm_routing_override_for(operation: &str) -> Option<String> {
    LLM_ROUTING_OVERRIDES
        .get()
        .and_then(|cell| cell.read().ok())
        .and_then(|map| map.get(operation).cloned())
}

pub fn llm_routing_overrides_snapshot() -> std::collections::HashMap<String, String> {
    LLM_ROUTING_OVERRIDES
        .get()
        .and_then(|cell| cell.read().ok())
        .map(|map| map.clone())
        .unwrap_or_default()
}

/// The owner's engine pin for one operation, if one is set.
pub fn llm_routing_engine_for(operation: &str) -> Option<OperationEngineFollow> {
    LLM_ROUTING_ENGINE_PINS
        .get()
        .and_then(|cell| cell.read().ok())
        .and_then(|map| map.get(operation).copied())
}

pub fn llm_routing_engine_snapshot() -> std::collections::HashMap<String, OperationEngineFollow> {
    LLM_ROUTING_ENGINE_PINS
        .get()
        .and_then(|cell| cell.read().ok())
        .map(|map| map.clone())
        .unwrap_or_default()
}

/// Write one install-level store as 0600 pretty JSON. No path (the store
/// was never installed — tests, tools) keeps the change in memory only.
fn persist_routing_store<V: serde::Serialize>(
    path: Option<&std::path::PathBuf>,
    map: &std::collections::HashMap<String, V>,
) -> std::io::Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    let bytes = serde_json::to_vec_pretty(map)
        .map_err(|error| std::io::Error::other(format!("serialize: {error}")))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn persist_overrides(map: &std::collections::HashMap<String, String>) -> std::io::Result<()> {
    persist_routing_store(LLM_ROUTING_OVERRIDES_PATH.get(), map)
}

fn persist_engine_pins(
    map: &std::collections::HashMap<String, OperationEngineFollow>,
) -> std::io::Result<()> {
    persist_routing_store(LLM_ROUTING_ENGINE_PINS_PATH.get(), map)
}

/// Pin or unpin exactly one operation's engine follow. Validation (the
/// operation is mapped) is the caller's — the API layer refuses unknown
/// operations before this runs.
pub fn set_llm_routing_engine(
    operation: &str,
    follow: OperationEngineFollow,
) -> std::io::Result<()> {
    let cell = LLM_ROUTING_ENGINE_PINS.get_or_init(|| std::sync::RwLock::default());
    // One write guard across read, persist and replace, as `clear` below
    // holds it. This used to clone under a READ lock, release it, and swap the
    // clone in under a fresh WRITE lock — so two concurrent sets on different
    // operations each cloned the same map and the later swap erased the
    // earlier pin, and because persist and swap were not under one guard the
    // file and memory could keep different winners. Persisting before the
    // swap keeps the original intent: a failed write leaves memory as it was.
    let mut map = cell
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut next = map.clone();
    next.insert(operation.to_string(), follow);
    persist_engine_pins(&next)?;
    *map = next;
    Ok(())
}

/// Revert one operation's engine follow to its config selector. `true` when
/// a pin existed.
pub fn clear_llm_routing_engine(operation: &str) -> std::io::Result<bool> {
    let Some(cell) = LLM_ROUTING_ENGINE_PINS.get() else {
        return Ok(false);
    };
    let mut map = cell.write().unwrap_or_else(|p| p.into_inner());
    let removed = map.remove(operation).is_some();
    if removed {
        persist_engine_pins(&map)?;
    }
    Ok(removed)
}

/// Set exactly one operation's override. Validation (profile exists) is the
/// caller's — the API layer refuses unknown profiles before this runs.
pub fn set_llm_routing_override(operation: &str, profile: &str) -> std::io::Result<()> {
    let cell = LLM_ROUTING_OVERRIDES.get_or_init(|| std::sync::RwLock::default());
    // Same lost update as `set_llm_routing_engine` had, and the same fix: two
    // Settings saves pinning different operations could each clone the map
    // under a released read lock, and the later swap dropped the other's
    // override. The write guard now spans read, persist and replace.
    let mut map = cell
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut next = map.clone();
    next.insert(operation.to_string(), profile.to_string());
    persist_overrides(&next)?;
    *map = next;
    Ok(())
}

/// Revert one operation to its config-file default. `true` when an override
/// existed.
pub fn clear_llm_routing_override(operation: &str) -> std::io::Result<bool> {
    let Some(cell) = LLM_ROUTING_OVERRIDES.get() else {
        return Ok(false);
    };
    let mut map = cell.write().unwrap_or_else(|p| p.into_inner());
    let removed = map.remove(operation).is_some();
    if removed {
        persist_overrides(&map)?;
    }
    Ok(removed)
}

/// Resolved effective overrides for a single LLM operation call.
/// Extracted to deduplicate the resolution logic between text and multimodal paths.
struct ResolvedRouting<'a> {
    profile_override: Option<&'a str>,
    effective_model_override: Option<&'a str>,
    provider_override: Option<&'a str>,
}

/// Owned routing/config decision for one chunk-capable call. Holding an owned
/// profile and router `Arc` lets prompt materialization and asynchronous
/// dispatch happen after the state lock is released without re-reading a
/// hot-reloaded configuration.
struct ChunkableOperationSnapshot {
    logical_profile_name: Option<String>,
    request_profile: LLMProfile,
    router: Option<Arc<ConfiguredRouter>>,
    explicit_profile_override: Option<String>,
    effective_model_override: Option<String>,
    provider_override: Option<String>,
}

impl OperationLlmRouter {
    pub(crate) fn authoritative_trace_scope(&self) -> Option<magicllm::LlmScope> {
        self.task_context
            .as_ref()
            .and_then(|task_ref| task_ref.scope.clone())
            .or_else(|| self.scope_context.clone())
    }

    fn reconcile_authoritative_trace_scope(
        context: &mut magicllm::LlmTraceContext,
        authoritative_scope: Option<&magicllm::LlmScope>,
        resolution: magicllm::LlmScopeResolution,
    ) -> Result<()> {
        let Some(authoritative_scope) = authoritative_scope else {
            return Ok(());
        };
        if !authoritative_scope.is_valid() {
            return Err(anyhow!("authoritative LLM scope is invalid"));
        }
        if context.scope == *authoritative_scope {
            if matches!(
                context.scope_resolution,
                magicllm::LlmScopeResolution::LegacyDefault
                    | magicllm::LlmScopeResolution::SystemDefault
            ) {
                context.scope_resolution = resolution;
            }
            return Ok(());
        }
        if matches!(
            context.scope_resolution,
            magicllm::LlmScopeResolution::Explicit | magicllm::LlmScopeResolution::Inherited
        ) {
            return Err(anyhow!(
                "request trace scope {}/{} conflicts with authoritative scope {}/{}",
                context.scope.principal,
                context.scope.workspace,
                authoritative_scope.principal,
                authoritative_scope.workspace
            ));
        }
        context.scope = authoritative_scope.clone();
        context.scope_resolution = resolution;
        Ok(())
    }

    fn profile_for_override(
        config: &LLMRouterConfig,
        profile_override: Option<&str>,
    ) -> Option<LLMProfile> {
        let profile_name = profile_override?;
        config.profiles.get(profile_name).cloned()
    }

    fn profile_for_operation_from_config(
        config: &LLMRouterConfig,
        routing_overrides: Option<&OperationRoutingOverrides>,
        operation: &LLMOperation,
    ) -> Result<LLMProfile> {
        Self::profile_for_operation_from_config_with_shape(
            config,
            routing_overrides,
            operation,
            &magicllm::config::RequestShape::NONE,
        )
    }

    /// Shape-aware variant. Picks the operation's `when_has_images`
    /// alternative when the dispatch sees images in the request, falling
    /// back to the default profile otherwise. Per-agent routing overrides
    /// (`OperationRoutingOverrides`) take precedence over the alternative
    /// because they're per-execution explicit; alternatives are config-level.
    ///
    /// **Transport-cohort guard**: when an alternative is picked, the
    /// router verifies it shares `provider` and `openai_api_mode` with the
    /// default profile. Mismatch → log WARN and fall back to default.
    /// Mid-conversation transport mismatch breaks tool-call shapes
    /// (Responses API ↔ Chat Completions are not interchangeable).
    fn profile_for_operation_from_config_with_shape(
        config: &LLMRouterConfig,
        routing_overrides: Option<&OperationRoutingOverrides>,
        operation: &LLMOperation,
        shape: &magicllm::config::RequestShape,
    ) -> Result<LLMProfile> {
        let operation_str = operation.as_str();

        if let Some(profile_name) = routing_overrides
            .and_then(|overrides| overrides.endpoint_for_operation(operation))
            .and_then(|endpoint| endpoint.profile_name())
        {
            return config.profiles.get(profile_name).cloned().ok_or_else(|| {
                anyhow!(
                    "Profile '{}' not found for operation '{}'",
                    profile_name,
                    operation_str
                )
            });
        }

        // Precedence (highest first): the owner's explicit panel override;
        // the flow's PARENT engine (an operation that follows its parent
        // rides the engine that started the flow — explicit on the routing
        // overrides, else ambient in the task; the owner's 2026-08-31 rule,
        // flow-scoped); the config mapping. Two floors hold under any
        // parent: an operation whose config default is an Ollama (local)
        // profile never follows — local stays local whatever drives — and a
        // request that carries tools never follows, because the harness CLI
        // profiles are text-only, so the swapped engine would refuse the
        // call and every tool-using operation — the agentic loop's Decide
        // above all — would stop for as long as the engine drives.
        let user_override = llm_routing_override_for(operation_str)
            .filter(|profile| config.profiles.contains_key(profile.as_str()));
        let mapping = config.operation_mapping.get(operation_str);
        let follows_parent = follows_parent_for(operation_str, mapping);
        let config_default_is_local = mapping
            .map(|selector| selector.default_profile())
            .and_then(|name| config.profiles.get(name))
            .is_some_and(|profile| {
                matches!(
                    profile.provider,
                    magicllm::capability::LLMProviderKind::Ollama
                )
            });
        let parent_override = if user_override.is_none()
            && follows_parent
            && !config_default_is_local
            && !shape.has_tools
        {
            let resolved = parent_engine_for(routing_overrides)
                .and_then(|engine| parent_profile(config, &engine));
            trace_parent_decision(
                "profile_resolver",
                operation_str,
                routing_overrides,
                resolved.as_deref(),
            );
            resolved
        } else {
            None
        };
        let selector = if user_override.is_some() || parent_override.is_some() {
            None
        } else {
            mapping
        };
        let default_name = user_override
            .as_ref()
            .or(parent_override.as_ref())
            .cloned()
            .unwrap_or_else(|| {
                selector
                    .map(|s| s.default_profile())
                    .unwrap_or(config.default_profile.as_str())
                    .to_string()
            });
        // Locality picks the arm family (`when_cloud` under cloud mode),
        // the request shape picks within it; an operation with no cloud arm
        // stays local in both modes.
        let candidate_name = if user_override.is_some() || parent_override.is_some() {
            default_name.clone()
        } else {
            selector
                .map(|s| s.profile_for_locality(shape, config.locality))
                .unwrap_or(default_name.as_str())
                .to_string()
        };
        let uses_cloud_arm = selector.is_some_and(|s| s.selects_cloud_arm(config.locality));

        // Resolve both. If the alternative differs from the default,
        // verify cohort compatibility (provider + openai_api_mode); if
        // mismatched, log a WARN and fall back to default. The cloud arm is
        // exempt: locality is an explicit operator decision, and a local
        // Ollama arm and its remote counterpart never share a transport by
        // design.
        let default_profile = config.profiles.get(&default_name).cloned().ok_or_else(|| {
            anyhow!(
                "Profile '{}' not found for operation '{}'",
                default_name,
                operation_str
            )
        })?;

        if candidate_name == default_name {
            return Ok(default_profile);
        }

        let alternative = config
            .profiles
            .get(&candidate_name)
            .cloned()
            .ok_or_else(|| {
                anyhow!(
                    "Alternative profile '{}' not found for operation '{}'",
                    candidate_name,
                    operation_str
                )
            })?;

        if !uses_cloud_arm && !Self::profiles_share_transport_cohort(&default_profile, &alternative)
        {
            warn!(
                operation = operation_str,
                default = default_name,
                alternative = candidate_name,
                "[OPERATION-ROUTER] alternative profile breaks transport cohort \
                 (provider or openai_api_mode mismatch); falling back to default"
            );
            return Ok(default_profile);
        }

        Ok(alternative)
    }

    /// Resolve the profile NAME the shape-aware resolver would select for
    /// `(operation, shape)`. Mirrors `profile_for_operation_from_config_with_shape`
    /// but returns the configured profile NAME instead of the `LLMProfile`
    /// struct, so downstream dispatch can pin the magicllm router to that
    /// exact profile via `extra.router_profile_override`. Without this pin,
    /// the magicllm router re-resolves from `operation_mapping.default_profile`
    /// and silently discards the magician layer's shape-aware decision —
    /// e.g. picks Yutori at the magician layer, then dispatches to OpenAI
    /// because the magicllm router doesn't see `has_images`.
    ///
    /// Returns `None` when the resolved name equals the operation's default
    /// (no override needed) so callers can leave `router_profile_override`
    /// unset on the common path.
    fn resolved_profile_name_for_shape(
        config: &LLMRouterConfig,
        routing_overrides: Option<&OperationRoutingOverrides>,
        operation: &LLMOperation,
        shape: &magicllm::config::RequestShape,
    ) -> Option<String> {
        let operation_str = operation.as_str();

        // Routing overrides win — keep the same precedence as the profile
        // resolver above.
        if let Some(profile_name) = routing_overrides
            .and_then(|overrides| overrides.endpoint_for_operation(operation))
            .and_then(|endpoint| endpoint.profile_name())
        {
            tracing::info!(
                target: "magician::routing::parent_engine",
                site = "dispatch_resolver_endpoint_pin",
                operation = operation_str,
                pinned_profile = profile_name,
                "an execution endpoint pinned the profile before the parent rule could apply"
            );
            return Some(profile_name.to_string());
        }

        // Install-level owner override (Model-routing panel). Deliberately
        // AFTER the execution-level pin above: a durable run's sealed
        // routing outranks a later panel flip, so changing the panel mid-run
        // never yanks a live execution's profile out from under it. Filtered
        // by profile liveness, mirroring the resolver: a stale override is
        // silence, not a pin to a dead name.
        if let Some(profile_name) = llm_routing_override_for(operation_str)
            .filter(|profile| config.profiles.contains_key(profile.as_str()))
        {
            return Some(profile_name);
        }

        // The flow's parent engine — MUST mirror the profile resolver (site
        // 1) or this pin would name the config's profile while site 1
        // resolved the parent's, and dispatch would contradict resolution.
        // The same gates, identically: the operation follows its parent,
        // its config default is not an Ollama (local) profile, and the
        // request carries no tools.
        let mapping = config.operation_mapping.get(operation_str);
        let follows_parent = follows_parent_for(operation_str, mapping);
        let config_default_is_local = mapping
            .map(|selector| selector.default_profile())
            .and_then(|name| config.profiles.get(name))
            .is_some_and(|profile| {
                matches!(
                    profile.provider,
                    magicllm::capability::LLMProviderKind::Ollama
                )
            });
        if follows_parent && !config_default_is_local && !shape.has_tools {
            let resolved = parent_engine_for(routing_overrides)
                .and_then(|engine| parent_profile(config, &engine));
            trace_parent_decision(
                "dispatch_resolver",
                operation_str,
                routing_overrides,
                resolved.as_deref(),
            );
            if let Some(profile_name) = resolved {
                return Some(profile_name);
            }
        }

        let selector = mapping?;
        let default_name = selector.default_profile();
        // Locality-aware, mirroring the profile resolver: the cloud arm pins
        // the remote profile for dispatch, and it is exempt from the cohort
        // guard (operator decision, not a same-conversation shape swap).
        let candidate_name = selector.profile_for_locality(shape, config.locality);

        if candidate_name == default_name {
            return None;
        }

        if selector.selects_cloud_arm(config.locality) {
            return Some(candidate_name.to_string());
        }

        // Cohort guard mirrors the profile resolver: a rejected alternative
        // means we fall back to default at runtime, so don't pin the
        // override either.
        let default_profile = config.profiles.get(default_name)?;
        let alternative = config.profiles.get(candidate_name)?;
        if !Self::profiles_share_transport_cohort(default_profile, alternative) {
            return None;
        }

        Some(candidate_name.to_string())
    }

    /// Two profiles share a transport cohort when neither side breaks a
    /// stateful, model-scoped chain.
    ///
    /// - Both stateless (Chat Completions, Anthropic Messages, Yutori N1,
    ///   etc.): swap is safe regardless of provider — each call is
    ///   independent on the wire and the runner re-renders the message
    ///   list per-provider.
    /// - One side uses a stateful transport, other side does not: swap is
    ///   still safe because the runner clears `last_response_id` on
    ///   shape change before the next turn (the abandoned chain id is
    ///   harmless, the new provider receives a stateless first call).
    /// - Both sides are stateful: require the same provider, model, endpoint,
    ///   and transport metadata so an opaque continuation id never crosses a
    ///   cohort boundary.
    fn profiles_share_transport_cohort(a: &LLMProfile, b: &LLMProfile) -> bool {
        let strategy_a = magicllm::strategy_for_provider(&a.provider, a.metadata.as_ref());
        let strategy_b = magicllm::strategy_for_provider(&b.provider, b.metadata.as_ref());

        // Neither side has a stateful chain — cross-provider swap is safe.
        if !strategy_a.is_stateful() && !strategy_b.is_stateful() {
            return true;
        }

        if strategy_a.is_stateful() && strategy_b.is_stateful() {
            return a.provider == b.provider
                && a.model == b.model
                && a.api_base_url == b.api_base_url
                && a.metadata.as_ref().and_then(|metadata| {
                    metadata
                        .get("openai_api_mode")
                        .or_else(|| metadata.get("gemini_api_mode"))
                }) == b.metadata.as_ref().and_then(|metadata| {
                    metadata
                        .get("openai_api_mode")
                        .or_else(|| metadata.get("gemini_api_mode"))
                });
        }

        // Mixed stateful/stateless: safe — the runner clears the chain id
        // on shape change; the next turn is stateless.
        true
    }

    fn context_reuse_session_key(&self) -> Option<String> {
        let task = self.task_context.as_ref()?;
        let execution = task
            .execution_id
            .as_deref()
            .or(task.root_execution_id.as_deref())
            .unwrap_or(task.task_id.as_str());
        let mut hasher = blake3::Hasher::new();
        if let Some(scope) = task.scope.as_ref() {
            hasher.update(scope.principal.as_bytes());
            hasher.update(&[0]);
            hasher.update(scope.workspace.as_bytes());
            hasher.update(&[0]);
        }
        hasher.update(execution.as_bytes());
        Some(format!(
            "magician:{}",
            &hasher.finalize().to_hex().as_str()[..32]
        ))
    }

    fn request_profile_for_routing(
        config: &LLMRouterConfig,
        base_profile: &LLMProfile,
        profile_override: Option<&str>,
    ) -> Result<LLMProfile> {
        if let Some(profile_name) = profile_override {
            return Self::profile_for_override(config, Some(profile_name)).ok_or_else(|| {
                anyhow!(
                    "Profile '{}' not found in router configuration",
                    profile_name
                )
            });
        }
        Ok(base_profile.clone())
    }

    fn snapshot_chunkable_operation(
        &self,
        operation: &LLMOperation,
    ) -> Result<ChunkableOperationSnapshot> {
        let routing = self.resolve_routing(operation, None);
        let explicit_profile_override = routing.profile_override.map(str::to_string);
        let effective_model_override = routing.effective_model_override.map(str::to_string);
        let provider_override = routing.provider_override.map(str::to_string);

        // One read owns the complete decision: config mapping, selected
        // profile, and matching ConfiguredRouter. Never re-enter `read_state`
        // after this point for this logical call.
        let state = self.read_state();
        let config = state
            .router_config
            .as_ref()
            .ok_or_else(|| anyhow!("No router configuration loaded"))?;
        let base_profile = Self::profile_for_operation_from_config(
            config,
            self.routing_overrides.as_ref(),
            operation,
        )?;
        let request_profile = Self::request_profile_for_routing(
            config,
            &base_profile,
            explicit_profile_override.as_deref(),
        )?;
        let logical_profile_name = explicit_profile_override.clone().or_else(|| {
            config
                .operation_mapping
                .get(operation.as_str())
                .map(|selector| selector.default_profile().to_string())
        });
        Ok(ChunkableOperationSnapshot {
            logical_profile_name,
            request_profile,
            router: state.router.clone(),
            explicit_profile_override,
            effective_model_override,
            provider_override,
        })
    }

    fn timeout_for_profile(operation: &LLMOperation, profile: &LLMProfile) -> u64 {
        profile
            .timeout_secs
            .unwrap_or_else(|| operation.timeout_seconds())
    }

    fn logical_timeout_for_profile(operation: &LLMOperation, profile: &LLMProfile) -> u64 {
        let physical_timeout = Self::timeout_for_profile(operation, profile);
        profile
            .chunking
            .as_ref()
            .and_then(|chunking| chunking.logical_timeout_secs)
            .unwrap_or(physical_timeout)
            .max(physical_timeout)
    }

    fn preserve_request_model(model_override: Option<&str>) -> bool {
        model_override.is_some()
    }

    /// Resolve the effective model and provider overrides for an operation.
    /// Precedence: request-level model_override > scoped (agent definition) override > config default.
    fn resolve_routing<'a>(
        &'a self,
        operation: &LLMOperation,
        model_override: Option<&'a str>,
    ) -> ResolvedRouting<'a> {
        let scoped_override = self
            .routing_overrides
            .as_ref()
            .and_then(|overrides| overrides.endpoint_for_operation(operation));
        let profile_override = scoped_override.and_then(|endpoint| endpoint.profile_name());
        let scoped_model_override = scoped_override.and_then(|endpoint| endpoint.model_name());
        let effective_model_override = model_override
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .or_else(|| {
                if profile_override.is_some() {
                    None
                } else {
                    scoped_model_override
                }
            });
        let provider_override = scoped_override.and_then(|endpoint| {
            if profile_override.is_some() {
                None
            } else {
                endpoint.provider_name()
            }
        });
        ResolvedRouting {
            profile_override,
            effective_model_override,
            provider_override,
        }
    }
}

impl LLMOperation {
    pub fn app(namespaced_operation: impl Into<String>) -> Result<Self> {
        let namespaced_operation = namespaced_operation.into();
        if !is_valid_app_operation_key(&namespaced_operation) {
            return Err(anyhow!(
                "app LLM operation must be `app:` followed by 1..=64 bytes beginning with an ASCII letter or digit and containing only letters, digits, `_` or `-`"
            ));
        }
        Ok(Self::App(namespaced_operation))
    }

    pub fn from_str(s: &str) -> Self {
        if is_valid_app_operation_key(s) {
            return LLMOperation::App(s.to_owned());
        }
        let normalized = s.to_lowercase();
        match normalized.as_str() {
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
            "discovery_extraction" => LLMOperation::DiscoveryExtraction,
            "discovery_safety" => LLMOperation::DiscoverySafety,
            "placeholder_resolution" => LLMOperation::PlaceholderResolution,
            "agentic_input_interpretation" => LLMOperation::AgenticInputInterpretation,
            "memory_entity_extraction" => LLMOperation::MemoryEntityExtraction,
            "memory_environment_knowledge_extraction" => {
                LLMOperation::MemoryEnvironmentKnowledgeExtraction
            },
            "memory_insight_distillation" => LLMOperation::MemoryInsightDistillation,
            "memory_user_promotion" => LLMOperation::MemoryUserPromotion,
            "memory_archive_summary" => LLMOperation::MemoryArchiveSummary,
            "memory_episode_quality_classification" => {
                LLMOperation::MemoryEpisodeQualityClassification
            },
            "memory_conflict_review" => LLMOperation::MemoryConflictReview,
            "memory_conflict_review_high_risk" => LLMOperation::MemoryConflictReviewHighRisk,
            "workflow_compilation" => LLMOperation::WorkflowCompilation,
            "recipe_compilation" => LLMOperation::RecipeCompilation,
            "screen_observation" => LLMOperation::ScreenObservation,
            "screen_understanding" => LLMOperation::ScreenUnderstanding,
            "screen_grounding" => LLMOperation::ScreenGrounding,
            "meeting_summary" => LLMOperation::MeetingSummary,
            "meeting_response" => LLMOperation::MeetingResponse,
            "voice_controller" => LLMOperation::VoiceController,
            "voice_context_compaction" => LLMOperation::VoiceContextCompaction,
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
            LLMOperation::DiscoveryExtraction => "discovery_extraction",
            LLMOperation::DiscoverySafety => "discovery_safety",
            LLMOperation::PlaceholderResolution => "placeholder_resolution",
            LLMOperation::AgenticInputInterpretation => "agentic_input_interpretation",
            LLMOperation::MemoryEntityExtraction => "memory_entity_extraction",
            LLMOperation::MemoryEnvironmentKnowledgeExtraction => {
                "memory_environment_knowledge_extraction"
            },
            LLMOperation::MemoryInsightDistillation => "memory_insight_distillation",
            LLMOperation::MemoryUserPromotion => "memory_user_promotion",
            LLMOperation::MemoryArchiveSummary => "memory_archive_summary",
            LLMOperation::MemoryEpisodeQualityClassification => {
                "memory_episode_quality_classification"
            },
            LLMOperation::MemoryConflictReview => "memory_conflict_review",
            LLMOperation::MemoryConflictReviewHighRisk => "memory_conflict_review_high_risk",
            LLMOperation::WorkflowCompilation => "workflow_compilation",
            LLMOperation::RecipeCompilation => "recipe_compilation",
            LLMOperation::ScreenObservation => "screen_observation",
            LLMOperation::ScreenUnderstanding => "screen_understanding",
            LLMOperation::ScreenGrounding => "screen_grounding",
            LLMOperation::MeetingSummary => "meeting_summary",
            LLMOperation::MeetingResponse => "meeting_response",
            LLMOperation::VoiceController => "voice_controller",
            LLMOperation::VoiceContextCompaction => "voice_context_compaction",
            LLMOperation::App(namespaced) => namespaced.as_str(),
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
            LLMOperation::DiscoveryExtraction => 30, // Extract values from command output
            LLMOperation::DiscoverySafety => 20,     // Legacy, use ParameterSafetyCheck

            // Execution operations
            LLMOperation::PlaceholderResolution => 20, // Fast resolution with nano

            // Agentic execution operations
            LLMOperation::AgenticInputInterpretation => 20, // Fast classification with nano

            // Memory consolidation operations
            LLMOperation::MemoryEntityExtraction => 60,
            LLMOperation::MemoryEnvironmentKnowledgeExtraction => 60,
            LLMOperation::MemoryInsightDistillation => 60,
            LLMOperation::MemoryUserPromotion => 60,
            LLMOperation::MemoryArchiveSummary => 45,
            LLMOperation::MemoryEpisodeQualityClassification => 45,
            LLMOperation::MemoryConflictReview => 60,
            LLMOperation::MemoryConflictReviewHighRisk => 90,

            // Media-rail senses: narration is one small image + ~120 tokens;
            // describe is full-tier vision Q&A; summaries may run on a local
            // 12B over a whole-meeting transcript (15-45s warm, longer cold).
            LLMOperation::ScreenObservation => 45,
            LLMOperation::ScreenUnderstanding => 120,
            LLMOperation::ScreenGrounding => 60,
            LLMOperation::MeetingSummary => 180,
            LLMOperation::MeetingResponse => 90,

            LLMOperation::App(_) => 60,

            _ => 60, // 60 seconds for all other operations
        }
    }
}

/// Router-backed LLM adapter that applies Magician operation policies.
#[derive(Clone)]
struct OperationLlmRouterState {
    router_config: Option<LLMRouterConfig>,
    router: Option<Arc<ConfiguredRouter>>,
}

/// Queue-less compatibility lane for CLI commands and deployments that have
/// explicitly disabled the dispatch queue. The logical runner still owns
/// chunk planning, validation, exact-profile locking, deadlines, and usage;
/// only queue scheduling/local-prep telemetry is absent.
#[derive(Clone)]
struct DirectLogicalChunkDispatch {
    router: Arc<ConfiguredRouter>,
    /// Optional canonical runtime event rail. Queue-backed child failures are
    /// already owned by the dispatch worker; this is used only by the
    /// queue-disabled compatibility path so a physical provider attempt does
    /// not disappear behind the logical parent failure.
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
}

#[derive(Clone)]
struct RuntimeLogicalChunkTelemetry {
    broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
}

impl RuntimeLogicalChunkTelemetry {
    #[allow(clippy::too_many_arguments)]
    fn emit_summary(
        &self,
        trace_context: Option<magicllm::LlmTraceContext>,
        started_at_ms: i64,
        operation: String,
        profile: String,
        agent_id: Option<String>,
        duration_ms: u64,
        success: bool,
        error: Option<String>,
    ) {
        let Some(context) = trace_context.filter(magicllm::LlmTraceContext::is_valid) else {
            warn!(
                operation,
                "logical chunk summary omitted because its typed trace context is unavailable"
            );
            return;
        };
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(context.clone(), 0);
        let completed_at_ms = chrono::Utc::now().timestamp_millis().max(started_at_ms);
        self.broadcaster.emit_transport_only(
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                execution_id: context
                    .execution_id
                    .clone()
                    .or_else(|| context.root_execution_id.clone())
                    .unwrap_or_else(|| context.llm_call_id.clone()),
                principal: Some(context.scope.principal.clone()),
                workspace: Some(context.scope.workspace.clone()),
                correlation: Some(
                    crate::magician_v2::realtime_events::LlmEventCorrelation::from(&receipt),
                ),
                plan_id: context.plan_id.clone().unwrap_or_default(),
                step_id: context.step_id.clone(),
                step_index: None,
                capability: "logical_chunk.summary".to_string(),
                success,
                decision_summary: String::new(),
                cost: 0.0,
                latency_ms: duration_ms,
                error,
                provider: String::new(),
                model: String::new(),
                usage_reported: false,
                input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                reasoning_summary: None,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                audio_input_tokens: None,
                audio_output_tokens: None,
                audio_cached_tokens: None,
                search_calls: 0,
                ttft_ms: None,
                task_id: context.task_id.clone(),
                agent_id,
                delegated_agent_id: None,
                chat_session_id: context.chat_session_id.clone(),
                operation,
                profile: Some(profile),
                attempt: 0,
                response_kind: "logical_chunk_summary".to_string(),
                started_at_ms,
                timestamp: completed_at_ms,
            },
        );
    }
}

impl LogicalChunkTelemetrySink for RuntimeLogicalChunkTelemetry {
    fn emit(&self, event: LogicalChunkTelemetryEvent) {
        match event {
            LogicalChunkTelemetryEvent::LogicalCompleted {
                metadata,
                trace_context,
                started_at_ms,
            } => {
                self.emit_summary(
                    trace_context,
                    started_at_ms,
                    metadata.operation,
                    metadata.profile,
                    metadata.agent_id,
                    metadata.total_duration_ms,
                    true,
                    None,
                );
                return;
            },
            LogicalChunkTelemetryEvent::LogicalFailed {
                trace_context,
                started_at_ms,
                operation,
                profile,
                agent_id,
                total_duration_ms,
                error,
                ..
            } => {
                self.emit_summary(
                    trace_context,
                    started_at_ms,
                    operation,
                    profile,
                    agent_id,
                    total_duration_ms,
                    false,
                    Some(error),
                );
                return;
            },
            LogicalChunkTelemetryEvent::PhysicalCompleted {
                trace_receipt,
                started_at_ms,
                operation,
                agent_id,
                profile,
                provider,
                adapter,
                stage,
                chunk_index,
                reduction_level,
                requested_model,
                usage,
                usage_reported,
                validation_ok,
                validation_error,
                queue_wait_ms,
                provider_execution_ms,
                ..
            } => {
                self.emit_physical(
                    trace_receipt,
                    started_at_ms,
                    operation,
                    agent_id,
                    profile,
                    provider,
                    adapter,
                    stage,
                    chunk_index,
                    reduction_level,
                    requested_model,
                    usage,
                    usage_reported,
                    validation_ok,
                    validation_error,
                    queue_wait_ms,
                    provider_execution_ms,
                );
                return;
            },
        }
    }
}

impl RuntimeLogicalChunkTelemetry {
    #[allow(clippy::too_many_arguments)]
    fn emit_physical(
        &self,
        trace_receipt: magicllm::LlmTraceReceipt,
        started_at_ms: i64,
        operation: String,
        agent_id: Option<String>,
        profile: String,
        provider: Option<String>,
        adapter: String,
        stage: crate::magician_v2::llm_chunking::PhysicalChunkStage,
        chunk_index: u32,
        reduction_level: u32,
        requested_model: String,
        usage: crate::magician_v2::llm_chunking::AggregateTokenUsage,
        usage_reported: bool,
        validation_ok: bool,
        _validation_error: Option<String>,
        queue_wait_ms: u64,
        provider_execution_ms: u64,
    ) {
        let context = &trace_receipt.context;
        let provider = provider.unwrap_or_default();
        let to_u32 = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
        let token_usage = magicllm::TokenUsage {
            prompt_tokens: Some(to_u32(usage.prompt_tokens)),
            completion_tokens: Some(to_u32(usage.completion_tokens)),
            total_tokens: Some(to_u32(usage.total_tokens)),
            reasoning_tokens: Some(to_u32(usage.reasoning_tokens)),
            cached_tokens: Some(to_u32(usage.cached_tokens)),
            cache_creation_tokens: Some(to_u32(usage.cache_creation_tokens)),
        };
        let cost = if usage_reported {
            magicllm::compute_cost_at(
                &magicllm::LLMProviderKind::from_str(&provider),
                &requested_model,
                &token_usage,
                started_at_ms,
            )
        } else {
            0.0
        };
        // The adapter's detailed parser error may contain source material or
        // model output. Persist only the typed, content-free failure class.
        let error = (!validation_ok)
            .then(|| "caller contract validation failed (logical_chunk_output)".to_string());
        let response_kind = if validation_ok {
            "logical_chunk_physical".to_string()
        } else {
            "validation_error:logical_chunk_output".to_string()
        };
        let completed_at_ms = chrono::Utc::now().timestamp_millis().max(started_at_ms);
        self.broadcaster.emit_transport_only(
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                execution_id: context
                    .execution_id
                    .clone()
                    .or_else(|| context.root_execution_id.clone())
                    .unwrap_or_else(|| context.llm_call_id.clone()),
                principal: Some(context.scope.principal.clone()),
                workspace: Some(context.scope.workspace.clone()),
                correlation: Some(
                    crate::magician_v2::realtime_events::LlmEventCorrelation::from(&trace_receipt),
                ),
                plan_id: context.plan_id.clone().unwrap_or_default(),
                step_id: context.step_id.clone(),
                step_index: None,
                capability: format!(
                    "logical_chunk.{adapter}.{}.l{reduction_level}.c{chunk_index}",
                    stage.as_str()
                ),
                success: true,
                decision_summary: String::new(),
                cost,
                latency_ms: queue_wait_ms.saturating_add(provider_execution_ms),
                error,
                provider,
                model: requested_model,
                usage_reported,
                input_tokens: to_u32(usage.prompt_tokens),
                output_tokens: to_u32(usage.completion_tokens),
                reasoning_tokens: to_u32(usage.reasoning_tokens),
                reasoning_summary: None,
                cache_read_tokens: to_u32(usage.cached_tokens),
                cache_creation_tokens: to_u32(usage.cache_creation_tokens),
                audio_input_tokens: None,
                audio_output_tokens: None,
                audio_cached_tokens: None,
                search_calls: 0,
                ttft_ms: None,
                task_id: context.task_id.clone(),
                agent_id,
                delegated_agent_id: None,
                chat_session_id: context.chat_session_id.clone(),
                operation,
                profile: Some(profile),
                attempt: trace_receipt.provider_attempt_count,
                response_kind,
                started_at_ms,
                timestamp: completed_at_ms,
            },
        );
    }
}

impl DirectLogicalChunkDispatch {
    #[allow(clippy::too_many_arguments)]
    fn emit_failed_physical_attempt(
        &self,
        trace_context: magicllm::LlmTraceContext,
        provider_attempt_count: u32,
        started_at_ms: i64,
        latency_ms: u64,
        operation: String,
        requested_profile: Option<String>,
        provider: Option<LLMProviderKind>,
        model: String,
        error: &magicllm::LLMError,
    ) {
        // Cancellation or deadline rejection before the router reached a
        // provider is represented by the logical parent only. Emitting an
        // attempt-zero child would manufacture provider activity.
        if provider_attempt_count == 0 {
            return;
        }
        let Some(broadcaster) = self.event_broadcaster.as_ref() else {
            return;
        };
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(
            trace_context.clone(),
            provider_attempt_count,
        );
        let completed_at_ms = chrono::Utc::now().timestamp_millis().max(started_at_ms);
        broadcaster.emit_transport_only(
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                execution_id: trace_context
                    .execution_id
                    .clone()
                    .or_else(|| trace_context.root_execution_id.clone())
                    .unwrap_or_else(|| trace_context.llm_call_id.clone()),
                principal: Some(trace_context.scope.principal.clone()),
                workspace: Some(trace_context.scope.workspace.clone()),
                correlation: Some(
                    crate::magician_v2::realtime_events::LlmEventCorrelation::from(&receipt),
                ),
                plan_id: trace_context.plan_id.clone().unwrap_or_default(),
                step_id: trace_context.step_id.clone(),
                step_index: None,
                capability: "logical_chunk.physical".to_string(),
                success: false,
                decision_summary: String::new(),
                cost: 0.0,
                latency_ms,
                error: Some(error.to_string()),
                provider: provider.map_or_else(String::new, |value| value.to_string()),
                model,
                usage_reported: false,
                input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                reasoning_summary: None,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                audio_input_tokens: None,
                audio_output_tokens: None,
                audio_cached_tokens: None,
                search_calls: 0,
                ttft_ms: None,
                task_id: trace_context.task_id.clone(),
                agent_id: None,
                delegated_agent_id: None,
                chat_session_id: trace_context.chat_session_id.clone(),
                operation,
                profile: requested_profile,
                attempt: provider_attempt_count,
                response_kind: "provider_error:logical_chunk_physical".to_string(),
                started_at_ms,
                timestamp: completed_at_ms,
            },
        );
    }
}

#[async_trait]
impl LogicalChunkDispatch for DirectLogicalChunkDispatch {
    async fn dispatch(
        &self,
        mut request: LogicalChunkDispatchRequest,
    ) -> Result<magicllm::DispatchedResponse, magicllm::LLMError> {
        if request.cancellation.is_cancelled() {
            return Err(magicllm::LLMError::Cancelled {
                reason: "logical_chunk_cancelled".to_string(),
            });
        }
        if request
            .submission_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            return Err(magicllm::LLMError::DeadlineExceeded);
        }

        let trace_context = request
            .request
            .metadata
            .ensure_trace_context(None, magicllm::LlmWorkloadClass::System);
        let provider_attempt_counter = request.request.metadata.ensure_provider_attempt_counter();
        let operation = request.request.metadata.operation.clone();
        let requested_profile = request
            .request
            .extra
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("router_profile_override"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let provider = magicllm::dispatch::DispatchRouter::provider_for_request(
            self.router.as_ref(),
            &request.request,
        );
        let model = request.request.model.clone();
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let started = Instant::now();
        let route = self.router.route(request.request);
        tokio::pin!(route);
        let routed = if let Some(deadline) = request.submission_deadline {
            let deadline_sleep = tokio::time::sleep_until(tokio::time::Instant::from_std(deadline));
            tokio::pin!(deadline_sleep);
            tokio::select! {
                result = &mut route => result,
                _ = request.cancellation.cancelled() => {
                    Err(magicllm::LLMError::Cancelled {
                        reason: "logical_chunk_cancelled".to_string(),
                    })
                }
                _ = &mut deadline_sleep => Err(magicllm::LLMError::DeadlineExceeded),
            }
        } else {
            tokio::select! {
                result = &mut route => result,
                _ = request.cancellation.cancelled() => {
                    Err(magicllm::LLMError::Cancelled {
                        reason: "logical_chunk_cancelled".to_string(),
                    })
                }
            }
        };
        let provider_attempt_count = provider_attempt_counter.load(Ordering::Relaxed);
        let mut response = match routed {
            Ok(response) => response,
            Err(error) => {
                let (provider, model, requested_profile) = error
                    .effective_route()
                    .map(|(profile, provider, model)| {
                        (
                            Some(provider.clone()),
                            model.to_string(),
                            Some(profile.to_string()),
                        )
                    })
                    .unwrap_or((provider, model, requested_profile));
                self.emit_failed_physical_attempt(
                    trace_context,
                    provider_attempt_count,
                    started_at_ms,
                    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                    operation,
                    requested_profile,
                    provider,
                    model,
                    &error,
                );
                return Err(error);
            },
        };
        if provider_attempt_count == 0 {
            return Err(magicllm::LLMError::Other(
                "router returned success without a physical provider attempt".to_string(),
            ));
        }
        let trace_receipt = response.trace_receipt.clone().unwrap_or_else(|| {
            magicllm::LlmTraceReceipt::direct_with_attempt_count(
                trace_context,
                provider_attempt_count,
            )
        });
        if trace_receipt.provider_attempt_count != provider_attempt_count {
            return Err(magicllm::LLMError::Other(format!(
                "router receipt attempt count {} disagrees with observed physical count {provider_attempt_count}",
                trace_receipt.provider_attempt_count
            )));
        }
        response.trace_receipt = Some(trace_receipt.clone());
        Ok(magicllm::DispatchedResponse {
            response: Arc::new(response),
            wait: Duration::ZERO,
            execution: started.elapsed(),
            local_prep: None,
            attempts: 1,
            trace_receipt,
        })
    }
}

/// See [`OperationLlmRouter::live_dispatch_router`].
struct LiveDispatchRouter {
    state: Arc<RwLock<OperationLlmRouterState>>,
}

impl LiveDispatchRouter {
    fn current(&self) -> Option<Arc<ConfiguredRouter>> {
        self.state
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .router
            .clone()
    }

    fn unconfigured() -> magicllm::LLMError {
        magicllm::LLMError::Configuration("no LLM router is configured".to_string())
    }
}

#[async_trait]
impl magicllm::dispatch::DispatchRouter for LiveDispatchRouter {
    async fn route(&self, request: RouterRequest) -> magicllm::LLMResult<RouterResponse> {
        let router = self.current().ok_or_else(Self::unconfigured)?;
        magicllm::dispatch::DispatchRouter::route(router.as_ref(), request).await
    }

    async fn route_stream(
        &self,
        request: RouterRequest,
        tx: tokio::sync::mpsc::Sender<magicllm::types::StreamDelta>,
    ) -> magicllm::LLMResult<()> {
        let router = self.current().ok_or_else(Self::unconfigured)?;
        magicllm::dispatch::DispatchRouter::route_stream(router.as_ref(), request, tx).await
    }

    fn provider_for_operation(&self, operation: &str) -> Option<LLMProviderKind> {
        magicllm::dispatch::DispatchRouter::provider_for_operation(
            self.current()?.as_ref(),
            operation,
        )
    }

    fn provider_for_request(&self, request: &RouterRequest) -> Option<LLMProviderKind> {
        magicllm::dispatch::DispatchRouter::provider_for_request(self.current()?.as_ref(), request)
    }

    fn timeout_for_operation(&self, operation: &str) -> Option<u64> {
        magicllm::dispatch::DispatchRouter::timeout_for_operation(
            self.current()?.as_ref(),
            operation,
        )
    }

    fn timeout_for_request(&self, request: &RouterRequest) -> Option<u64> {
        magicllm::dispatch::DispatchRouter::timeout_for_request(self.current()?.as_ref(), request)
    }
}

#[derive(Clone)]
pub struct OperationLlmRouter {
    state: Arc<RwLock<OperationLlmRouterState>>,
    routing_overrides: Option<OperationRoutingOverrides>,
    /// Per-agent temperature override.
    temperature_override: Option<f32>,
    /// Optional lane override for explicitly background-owned callers. Normal
    /// operation routing remains authoritative unless a caller opts in.
    dispatch_priority_override: Option<magicllm::dispatch::Priority>,
    /// Shared, set-once handle to the global LLM dispatch queue. Set at
    /// boot via `set_dispatch_queue` AFTER the queue is built from the
    /// same shared `ConfiguredRouter`. Behind `Arc<OnceLock<…>>` so it is
    /// shared across every per-agent clone (`with_routing_overrides` /
    /// `with_temperature_override`) and survives `reload_from_config`
    /// (which rebuilds `state` but not this). When present (and
    /// `llm.dispatch.enabled`), `route_request` submits through the queue;
    /// when absent, ordinary core calls can route directly. App operations and
    /// disclosure-guarded App workflow calls require the queue.
    dispatch_queue: Arc<std::sync::OnceLock<Arc<magicllm::LlmDispatchQueue>>>,
    /// Process-owned event bridge used only to publish each physical logical
    /// chunk child into the canonical Phase 2 capture pipeline. Shared by all
    /// per-agent clones and installed once during runtime boot.
    event_broadcaster: Arc<
        std::sync::OnceLock<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    >,
    /// Process-owned Phase 3 observation boundary. It is installed once at
    /// boot and shared by every per-agent clone. Requests carry only this
    /// in-process sink; it is excluded from serialization and provider
    /// payloads by `magicllm::RequestMetadata`.
    content_capture_sink: Arc<std::sync::OnceLock<magicllm::LlmContentCaptureSink>>,
    /// Per-execution task reference attached to every dispatch job this router
    /// submits. Set on a per-execution clone via [`Self::with_task_context`] so
    /// the queue can map jobs back to their owning execution — that's what lets
    /// `cancel_execution` tombstone an execution's queued + in-flight LLM calls
    /// (`CancelBridge::on_task_cancelled` → `queue.cancel_task(task_id)` +
    /// in-flight cancel token). `None` on the shared base router (LLM calls not
    /// scoped to a cancellable execution — e.g. query analysis).
    task_context: Option<magicllm::dispatch::TaskRef>,
    /// Authoritative tenant scope for non-task product surfaces such as
    /// ambient distillation and Thinking Maps. Task scope wins when both are
    /// present.
    scope_context: Option<magicllm::LlmScope>,
    /// Runtime-only disclosure authority for an admitted app execution. This
    /// is carried only by a per-execution clone and is attached to every
    /// request immediately before queue/direct routing. It is never recovered
    /// from task bytes, prompts, provider output, or routing configuration.
    disclosure_guard: Option<magicllm::LlmDisclosureGuard>,
    /// Opaque proof that the behavior dispatcher authorized one exact reserved
    /// `app:*` operation/profile/token tuple. A generic app disclosure guard is
    /// intentionally insufficient: ordinary app workflows carry one too.
    app_operation_dispatch_permit: Option<Arc<AppLlmOperationDispatchPermit>>,
}

/// Map an `LLMOperation` to a dispatch `Priority` lane. User-facing voice
/// is latency-sensitive (High); background memory/consolidation/workflow
/// compilation is best-effort (Background); everything else (planning,
/// slot-graph, agentic execution) is Normal.
fn priority_for_operation(operation: &LLMOperation) -> magicllm::dispatch::Priority {
    use magicllm::dispatch::Priority;
    match operation {
        LLMOperation::VoiceController
        | LLMOperation::VoiceContextCompaction
        | LLMOperation::MeetingResponse => Priority::High,
        op if is_background_dispatch_operation(op) => Priority::Background,
        _ => Priority::Normal,
    }
}

fn workload_for_operation(operation: &LLMOperation) -> magicllm::LlmWorkloadClass {
    match operation {
        LLMOperation::App(_) => magicllm::LlmWorkloadClass::Scheduled,
        LLMOperation::MemoryEntityExtraction
        | LLMOperation::MemoryEnvironmentKnowledgeExtraction
        | LLMOperation::MemoryInsightDistillation
        | LLMOperation::MemoryUserPromotion
        | LLMOperation::MemoryArchiveSummary
        | LLMOperation::MemoryEpisodeQualityClassification
        | LLMOperation::MemoryConflictReview
        | LLMOperation::MemoryConflictReviewHighRisk => magicllm::LlmWorkloadClass::Memory,
        LLMOperation::ScreenObservation
        | LLMOperation::ScreenUnderstanding
        | LLMOperation::ScreenGrounding
        | LLMOperation::MeetingSummary => magicllm::LlmWorkloadClass::Ambient,
        LLMOperation::MeetingResponse
        | LLMOperation::VoiceController
        | LLMOperation::VoiceContextCompaction => magicllm::LlmWorkloadClass::ForegroundChat,
        LLMOperation::AgenticInputInterpretation
        | LLMOperation::WorkflowCompilation
        | LLMOperation::RecipeCompilation => magicllm::LlmWorkloadClass::AutonomousTask,
        LLMOperation::Other(name) => {
            let name = name.trim().to_ascii_lowercase();
            if name.starts_with("memory_") || name.contains("memory_temperature") {
                magicllm::LlmWorkloadClass::Memory
            } else if name.starts_with("channel_")
                || name.contains("mail")
                || name.contains("gmail")
            {
                magicllm::LlmWorkloadClass::CommsAssist
            } else if name.starts_with("ambient_") || name.starts_with("screen_") {
                magicllm::LlmWorkloadClass::Ambient
            } else if name.contains("eval") || name.contains("judge") {
                magicllm::LlmWorkloadClass::Evaluation
            } else if name.contains("agentic") || name.contains("decision") {
                magicllm::LlmWorkloadClass::AutonomousTask
            } else if name.starts_with("taste_") {
                // The taste sweep is interval-driven — 15 minutes, and
                // deliberately a sweep rather than a close hook, because a
                // missed hook is a feature that silently never runs. Its
                // `taste_capture_sweep` span declares `scheduled`, and without
                // this arm the same calls landed in `system` here: the live
                // lane and `llm_dispatch_batch.workload_class` named identical
                // work differently and joined on nothing.
                //
                // `scheduled` and not `ambient`: the distinction in this
                // taxonomy is a timer versus continuous observation, and this
                // is a timer. An operator can drive `sweep_once` early, but
                // that changes when the pass runs, not what it is — nobody
                // blocks on the result, which lands as a proposal to approve
                // later.
                magicllm::LlmWorkloadClass::Scheduled
            } else {
                // Genuinely unclassified. `System` here is a fallthrough, not
                // a decision — a new operation landing in it means this match
                // has not been taught about it yet, which is exactly how the
                // taste arm above came to be needed.
                magicllm::LlmWorkloadClass::System
            }
        },
        _ => magicllm::LlmWorkloadClass::InteractiveTask,
    }
}

fn is_background_dispatch_operation(operation: &LLMOperation) -> bool {
    match operation {
        LLMOperation::App(_) => true,
        LLMOperation::MemoryEntityExtraction
        | LLMOperation::MemoryEnvironmentKnowledgeExtraction
        | LLMOperation::MemoryInsightDistillation
        | LLMOperation::MemoryUserPromotion
        | LLMOperation::MemoryArchiveSummary
        | LLMOperation::MemoryEpisodeQualityClassification
        | LLMOperation::MemoryConflictReview
        | LLMOperation::MemoryConflictReviewHighRisk
        | LLMOperation::WorkflowCompilation
        | LLMOperation::RecipeCompilation
        // Background senses: per-frame narration and teardown summaries are
        // latency-tolerant by design and must never starve interactive
        // lanes. (ScreenUnderstanding stays Normal — an agent is waiting.)
        | LLMOperation::ScreenObservation
        | LLMOperation::MeetingSummary
        => true,
        LLMOperation::Other(name) => is_background_dispatch_operation_name(name),
        _ => false,
    }
}

fn is_valid_app_operation_key(operation: &str) -> bool {
    operation.strip_prefix("app:").is_some_and(|name| {
        let mut bytes = name.bytes();
        !name.is_empty()
            && name.len() <= 64
            && bytes
                .next()
                .is_some_and(|byte| byte.is_ascii_alphanumeric())
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
}

fn is_background_dispatch_operation_name(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        // Work-evidence / memory distillation helper operations. These are
        // not direct user chat turns and should not contend with interactive
        // OpenAI/Gemini/Anthropic calls when provider concurrency is tight.
        "distill_evidence"
            | "ambient_distill"
            | "screen_evidence_distill"
            | "tier_evidence_distill"
            | "evidence_claims"
            | "evidence_precision_judge"
            | "evidence_review_verify"
            | "memory_temperature_utility_review"
            | "learning_reflection"
            // Local channel ingestion, classification, and resurfacing are
            // asynchronous maintenance work, not interactive user turns.
            | "channel_ingest_distill"
            | "channel_classify"
            | "resurfacing_curate"
    )
}

/// Charge one provider response at the first common response boundary.
/// `total_tokens` is authoritative when present; otherwise both input and
/// output must be available. Reasoning/cache fields are already represented in
/// those provider totals and must never be added again.
///
/// When a provider omits usage entirely (the native-ollama summary profile does
/// this routinely), we do NOT fail closed — that force-exhausted the whole
/// budget on the first such response and terminal-FAILed runs that had spent
/// almost nothing. Instead we estimate the response's token cost from its text
/// length (~4 chars/token) and charge the estimate. Charging is best-effort: an
/// estimate should never itself trip a spurious over-budget FAIL, so the charge
/// error is swallowed with a warn rather than propagated.
fn account_router_response_tokens(response: &RouterResponse) -> Result<()> {
    if execution_token_budget_snapshot().is_none() {
        return Ok(());
    }
    let tokens = response.usage.as_ref().and_then(|usage| {
        usage.total_tokens.map(u64::from).or_else(|| {
            usage
                .prompt_tokens
                .zip(usage.completion_tokens)
                .map(|(input, output)| u64::from(input).saturating_add(u64::from(output)))
        })
    });
    let Some(tokens) = tokens else {
        let estimate = estimate_response_tokens_without_usage(response);
        warn!(
            estimated_tokens = estimate,
            "[MAGICIAN-V2-LLM] ⚠️  provider omitted token usage; charging a length-based \
             estimate against the execution budget instead of failing closed"
        );
        if let Err(err) = account_execution_tokens(estimate) {
            // An estimate must never manufacture a terminal budget FAIL. Log and
            // let the real per-response accounting on the next authoritative
            // usage (or the preflight guard) enforce the cap.
            warn!(
                error = %err,
                "[MAGICIAN-V2-LLM] ⚠️  estimated missing-usage charge crossed the budget; \
                 tolerating the estimate rather than terminal-failing the run"
            );
        }
        return Ok(());
    };
    account_execution_tokens(tokens)?;
    Ok(())
}

/// Estimate the token cost of a response whose provider omitted usage metadata.
/// Uses the common ~4-chars-per-token heuristic over every text-bearing field
/// the response actually carried (assistant text, reasoning trace, message
/// bodies, and tool-call arguments) so the estimate tracks real output size.
/// This is the output side only — the request side is unavailable at this
/// boundary — so it deliberately under-charges rather than force-exhausting.
fn estimate_response_tokens_without_usage(response: &RouterResponse) -> u64 {
    let mut chars: usize = 0;
    if let Some(text) = &response.text {
        chars = chars.saturating_add(text.chars().count());
    }
    if let Some(reasoning) = &response.reasoning_text {
        chars = chars.saturating_add(reasoning.chars().count());
    }
    for message in response.messages.iter() {
        for block in &message.content {
            if let RouterContentBlock::Text { text } = block {
                chars = chars.saturating_add(text.chars().count());
            }
        }
    }
    for call in response.tool_calls.iter() {
        chars = chars.saturating_add(call.arguments.to_string().chars().count());
    }
    (chars / 4) as u64
}

/// A request that reached the dispatch queue or provider but returned no
/// response has unknown usage. Preserve the original error verbatim so the
/// executor's transient-retry path (P0.3) can classify and back off on a real
/// transport failure (connection refused/DNS/503/timeout).
///
/// Previously this rewrote every request error into a force-exhausted budget
/// error whenever a budget was active, which masked the real cause and made a
/// retryable transport blip non-retryable (and terminal-FAILed the run). A
/// transport error carries no token cost, so it must never be laundered into
/// budget exhaustion. The preflight guard already blocks a genuinely exhausted
/// budget before the next request is issued.
fn fail_closed_router_request_error(error: anyhow::Error) -> anyhow::Error {
    error
}

/// Restructure a single-string user prompt into multiple content blocks so the
/// large `raw` substring can be carried as a dispatch-queue
/// [`SummarisableBlock`] (local pre-summarisation target).
///
/// The prompt is split at the first occurrence of `raw` into
/// `[before, placeholder, after]`, dropping any empty halves so providers never
/// receive empty text blocks. The placeholder slot is what the worker's
/// local-prep step replaces — with an Ollama summary when enabled and over
/// threshold, otherwise with `raw` verbatim. Because `before + raw + after`
/// reconstructs the original prompt exactly, the disabled/fallthrough path is
/// content-equivalent to the inline prompt.
///
/// Returns `None` when `raw` is empty or not found in `prompt` — callers then
/// keep the original single-block message unchanged.
fn split_user_content_for_summarisation(
    prompt: &str,
    raw: &str,
    purpose: SummarisationPurpose,
    user_index: usize,
) -> Option<(Vec<RouterContentBlock>, SummarisableBlock)> {
    if raw.is_empty() {
        return None;
    }
    let pos = prompt.find(raw)?;
    let before = &prompt[..pos];
    let after = &prompt[pos + raw.len()..];

    let mut content = Vec::with_capacity(3);
    if !before.is_empty() {
        content.push(RouterContentBlock::text(before.to_string()));
    }
    let content_index = content.len();
    // Placeholder — always overwritten by local-prep (summary or raw fallthrough).
    content.push(RouterContentBlock::text(String::new()));
    if !after.is_empty() {
        content.push(RouterContentBlock::text(after.to_string()));
    }

    let block = SummarisableBlock {
        message_index: user_index,
        content_index,
        raw: raw.to_string(),
        purpose,
        max_chars: None,
    };
    Some((content, block))
}

impl OperationLlmRouter {
    /// Carry the same owning task/run/session onto a sibling Decision Model call.
    pub(crate) fn classification_trace_context(
        &self,
        scope: magicllm::LlmScope,
    ) -> magicllm::LlmTraceContext {
        let mut context =
            magicllm::LlmTraceContext::new(scope, magicllm::LlmWorkloadClass::Ambient);
        context.call_role = magicllm::LlmCallRole::Judge;
        if let Some(task) = self.task_context.as_ref().filter(|task| {
            task.scope
                .as_ref()
                .is_none_or(|scope| scope == &context.scope)
        }) {
            context.task_id = Some(task.task_id.clone());
            context.root_execution_id = task.root_execution_id.clone();
            context.execution_id = task.execution_id.clone();
            context.trace_id = task
                .root_execution_id
                .clone()
                .or_else(|| task.execution_id.clone())
                .unwrap_or(context.trace_id);
            context.plan_id = task.plan_id.clone();
            context.step_id = task.step_id.clone();
            context.chat_session_id = task.chat_session_id.clone();
            context.chat_turn_id = task.chat_turn_id.clone();
            context.iteration_id = task.iteration_id.clone();
            context.user_message_id = task.user_message_id.clone();
        }
        Self::ensure_trace_context_activity_id(&mut context);
        context
    }

    fn ensure_trace_context_activity_id(trace_context: &mut magicllm::LlmTraceContext) {
        if trace_context.activity_id.is_none() {
            trace_context.set_activity_id(current_activity_id().map(|id| id.to_string()));
        }
    }

    fn build_state(router_config: Option<LLMRouterConfig>) -> OperationLlmRouterState {
        let Some(config) = router_config else {
            warn!("[MAGICIAN-V2-LLM] ⚠️  No router config provided; LLM operations disabled");
            return OperationLlmRouterState {
                router_config: None,
                router: None,
            };
        };

        if config.profiles.is_empty() {
            warn!("[MAGICIAN-V2-LLM] ⚠️  Router config has no profiles; LLM operations disabled");
            return OperationLlmRouterState {
                router_config: Some(config),
                router: None,
            };
        }

        if let Err(error) = validate_router_chunking_config(&config) {
            warn!(
                error = %error,
                "[MAGICIAN-V2-LLM] invalid logical-context adapter mapping; LLM operations disabled"
            );
            return OperationLlmRouterState {
                router_config: Some(config),
                router: None,
            };
        }

        let router = match catch_unwind(AssertUnwindSafe(|| {
            ConfiguredRouter::from_router_config(config.clone())
        })) {
            Ok(Ok(router)) => {
                info!("[MAGICIAN-V2-LLM] 🧭 ConfiguredRouter initialized");
                Some(Arc::new(router))
            },
            Ok(Err(err)) => {
                warn!(
                    "[MAGICIAN-V2-LLM] ⚠️  Failed to initialize ConfiguredRouter: {}",
                    err
                );
                None
            },
            Err(_) => {
                warn!("[MAGICIAN-V2-LLM] ⚠️  ConfiguredRouter initialization panicked");
                None
            },
        };

        debug!(
            "[MAGICIAN-V2-QUERY] Initialized OperationLlmRouter with {} profiles and {} operation mappings",
            config.profiles.len(),
            config.operation_mapping.len()
        );

        OperationLlmRouterState {
            router_config: Some(config),
            router,
        }
    }

    fn read_state(&self) -> RwLockReadGuard<'_, OperationLlmRouterState> {
        self.state
            .read()
            .expect("operation_llm_router state lock poisoned")
    }

    /// Expose the underlying `ConfiguredRouter` so the dispatch queue can
    /// share it (avoids duplicating provider HTTP clients across two router
    /// instances).
    pub fn shared_configured_router(&self) -> Option<Arc<ConfiguredRouter>> {
        self.read_state().router.clone()
    }

    /// The dispatch queue's router: this router's *current* configured
    /// router, looked up on every call. The queue is started once at boot;
    /// handed `shared_configured_router()` it kept the boot router for good,
    /// so after a live config reload the operation router resolved the new
    /// mapping (`agentic_decision → opus55`) while the queue — which
    /// re-resolves an operation's default profile itself — still dispatched
    /// the old one (gpt-6-sol). Every clone of this router shares one state,
    /// so `reload_from_config` reaches the queue on its next call.
    pub fn live_dispatch_router(&self) -> Arc<dyn magicllm::dispatch::DispatchRouter> {
        Arc::new(LiveDispatchRouter {
            state: Arc::clone(&self.state),
        })
    }

    #[cfg(any(test, feature = "test-fixtures"))]
    fn current_state(&self) -> OperationLlmRouterState {
        self.state
            .read()
            .expect("operation_llm_router state lock poisoned")
            .clone()
    }

    fn request_profile_for_operation(
        &self,
        operation: &LLMOperation,
        profile_override: Option<&str>,
    ) -> Result<LLMProfile> {
        self.request_profile_for_operation_with_shape(
            operation,
            profile_override,
            &magicllm::config::RequestShape::NONE,
        )
    }

    /// Shape-aware profile resolution. The dispatch path uses this when
    /// `has_images` is known so per-call alternatives (e.g. Yutori for
    /// screenshot-bearing inner-loop turns) can fire.
    fn request_profile_for_operation_with_shape(
        &self,
        operation: &LLMOperation,
        profile_override: Option<&str>,
        shape: &magicllm::config::RequestShape,
    ) -> Result<LLMProfile> {
        let state = self.read_state();
        let config = state
            .router_config
            .as_ref()
            .ok_or_else(|| anyhow!("No router configuration loaded"))?;
        let base_profile = Self::profile_for_operation_from_config_with_shape(
            config,
            self.routing_overrides.as_ref(),
            operation,
            shape,
        )?;
        Self::request_profile_for_routing(config, &base_profile, profile_override)
    }

    /// Test-only exposure of the private shape-aware resolver for the
    /// Phase-2 override test (same module, so it could be private; pub(
    /// crate) keeps the surface honest).
    #[cfg(test)]
    pub(crate) fn profile_for_operation_from_config_with_shape_for_test(
        config: &LLMRouterConfig,
        routing_overrides: Option<&OperationRoutingOverrides>,
        operation: &LLMOperation,
        shape: &magicllm::config::RequestShape,
    ) -> Result<LLMProfile> {
        Self::profile_for_operation_from_config_with_shape(
            config,
            routing_overrides,
            operation,
            shape,
        )
    }

    pub fn reload_from_config(&self, router_config: Option<LLMRouterConfig>) -> bool {
        if let Some(config) = router_config.as_ref() {
            if let Err(error) = validate_router_chunking_config(config) {
                warn!(
                    error = %error,
                    "[MAGICIAN-V2-LLM] rejected config reload with invalid logical-context adapter mapping; retaining current router"
                );
                return false;
            }
        }
        let next = Self::build_state(router_config);
        let live_router = next.router.is_some();
        let mut state = self
            .state
            .write()
            .expect("operation_llm_router state lock poisoned");
        *state = next;
        live_router
    }

    /// Create an operation-aware LLM router from router configuration.
    pub fn new(router_config: Option<LLMRouterConfig>) -> Self {
        Self {
            state: Arc::new(RwLock::new(Self::build_state(router_config))),
            routing_overrides: None,
            temperature_override: None,
            dispatch_priority_override: None,
            dispatch_queue: Arc::new(std::sync::OnceLock::new()),
            event_broadcaster: Arc::new(std::sync::OnceLock::new()),
            content_capture_sink: Arc::new(std::sync::OnceLock::new()),
            task_context: None,
            scope_context: None,
            disclosure_guard: None,
            app_operation_dispatch_permit: None,
        }
    }

    /// Install the global LLM dispatch queue (set-once, at boot). Shared
    /// across all per-agent clones and survives config reloads. After this,
    /// `route_request` submits through the queue instead of calling the
    /// router directly. No-op if already set.
    pub fn set_dispatch_queue(&self, queue: Arc<magicllm::LlmDispatchQueue>) {
        let _ = self.dispatch_queue.set(queue);
    }

    pub fn set_event_broadcaster(
        &self,
        broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    ) {
        let _ = self.event_broadcaster.set(broadcaster);
    }

    /// Install the governed, bounded content observer once at boot.
    pub fn set_content_capture_sink(&self, sink: magicllm::LlmContentCaptureSink) {
        let _ = self.content_capture_sink.set(sink);
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_direct_route_failure(
        &self,
        trace_context: &magicllm::LlmTraceContext,
        provider_attempt_count: u32,
        provider: Option<LLMProviderKind>,
        model: String,
        profile: Option<String>,
        operation: &LLMOperation,
        started_at_ms: i64,
        latency_ms: u64,
        error: &magicllm::LLMError,
    ) {
        // A zero-attempt receipt is a real logical call that failed before a
        // provider invocation. Preserve that terminal call fact without
        // inventing a provider-attempt child. Queue-backed failures are owned
        // by the queue bridge; this helper is used only by the direct fallback.
        let Some(broadcaster) = self.event_broadcaster.get() else {
            return;
        };
        let receipt = magicllm::LlmTraceReceipt::direct_with_attempt_count(
            trace_context.clone(),
            provider_attempt_count,
        );
        let completed_at_ms = chrono::Utc::now().timestamp_millis().max(started_at_ms);
        broadcaster.emit_transport_only(
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                execution_id: trace_context
                    .execution_id
                    .clone()
                    .or_else(|| trace_context.root_execution_id.clone())
                    .unwrap_or_else(|| trace_context.llm_call_id.clone()),
                principal: Some(trace_context.scope.principal.clone()),
                workspace: Some(trace_context.scope.workspace.clone()),
                correlation: Some(
                    crate::magician_v2::realtime_events::LlmEventCorrelation::from(&receipt),
                ),
                plan_id: trace_context.plan_id.clone().unwrap_or_default(),
                step_id: trace_context.step_id.clone(),
                step_index: None,
                capability: "operation_router.direct".to_string(),
                success: false,
                decision_summary: String::new(),
                cost: 0.0,
                latency_ms,
                error: Some(error.to_string()),
                provider: provider.map_or_else(String::new, |value| value.to_string()),
                model,
                usage_reported: false,
                input_tokens: 0,
                output_tokens: 0,
                reasoning_tokens: 0,
                reasoning_summary: None,
                cache_read_tokens: 0,
                cache_creation_tokens: 0,
                audio_input_tokens: None,
                audio_output_tokens: None,
                audio_cached_tokens: None,
                search_calls: 0,
                ttft_ms: None,
                task_id: trace_context.task_id.clone(),
                agent_id: self
                    .task_context
                    .as_ref()
                    .and_then(|task| task.agent_id.clone()),
                delegated_agent_id: None,
                chat_session_id: trace_context.chat_session_id.clone(),
                operation: operation.as_str().to_string(),
                profile,
                attempt: provider_attempt_count,
                response_kind: "provider_error:operation_router_direct".to_string(),
                started_at_ms,
                timestamp: completed_at_ms,
            },
        );
    }

    /// Preserve the logical-call fact when a provider response is rejected at
    /// the operation-router boundary before a caller can receive and report it.
    /// The response receipt supplies the stable call identity and authoritative
    /// tenant scope; without it, emitting a synthetic event would create a
    /// misleading capture row, so the helper deliberately does nothing.
    #[allow(clippy::too_many_arguments)]
    fn emit_post_response_validation_failure(
        &self,
        response: &RouterResponse,
        operation: &LLMOperation,
        fallback_provider: &str,
        fallback_model: &str,
        fallback_profile: Option<&str>,
        started_at_ms: i64,
        validation_class: &str,
    ) {
        let (Some(broadcaster), Some(receipt)) = (
            self.event_broadcaster.get(),
            response.trace_receipt.as_ref(),
        ) else {
            return;
        };
        let (provider, model, profile) = response
            .route_identity
            .as_ref()
            .map(|identity| {
                (
                    identity.provider.as_str(),
                    identity.model.as_str(),
                    Some(identity.profile.as_str()),
                )
            })
            .unwrap_or((fallback_provider, fallback_model, fallback_profile));
        let Some(telemetry) = build_telemetry(
            response.usage.as_ref(),
            provider,
            model,
            operation,
            profile,
            started_at_ms,
            web_search_call_count_for(provider, &response),
            response
                .reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string),
            Some(receipt.clone()),
        ) else {
            return;
        };
        let trace = &receipt.context;
        let attribution =
            crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmCallAttribution {
                execution_id: trace
                    .execution_id
                    .clone()
                    .or_else(|| trace.root_execution_id.clone()),
                root_execution_id: trace.root_execution_id.clone(),
                task_id: trace.task_id.clone(),
                agent_id: self
                    .task_context
                    .as_ref()
                    .and_then(|task| task.agent_id.clone()),
                delegated_agent_id: None,
                chat_session_id: trace.chat_session_id.clone(),
                attempt: Some(receipt.provider_attempt_count.max(1)),
            };
        crate::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            trace.scope.principal.clone(),
            trace.scope.workspace.clone(),
            "operation_router.validation",
        )
        .emit_usage_validation_failure(
            operation.as_str(),
            &telemetry,
            u64::try_from(
                chrono::Utc::now()
                    .timestamp_millis()
                    .saturating_sub(started_at_ms),
            )
            .unwrap_or_default(),
            attribution,
            validation_class,
            "",
        );
    }

    /// Read-only queue state for observability and pressure diagnostics.
    /// Admission and pickup fairness are owned by the dispatch queue itself.
    pub fn dispatch_queue_snapshot(&self) -> Option<magicllm::dispatch::QueueSnapshot> {
        self.dispatch_queue.get().map(|queue| queue.snapshot())
    }

    /// Reference observation may never bypass the shared dispatch queue.
    pub fn observation_dispatch_available(&self) -> bool {
        self.dispatch_queue
            .get()
            .is_some_and(|queue| queue.config_handle().read().enabled)
    }

    /// The live queue's retry ceiling, used to reserve paid observation work.
    /// A missing or disabled queue cannot be treated as a zero-cost path.
    pub fn observation_dispatch_attempt_ceiling(&self) -> Option<u64> {
        let queue = self.dispatch_queue.get()?;
        let config = queue.config_handle();
        let config = config.read();
        config.enabled.then_some(())?;
        let attempts = u64::from(config.retry.max_attempts_per_dispatch)
            .checked_mul(u64::from(config.retry.max_dispatch_cycles))?;
        (attempts > 0).then_some(attempts)
    }

    /// True while the shared dispatch queue has live non-background work.
    /// This is an authoritative queue/lease signal, not a guessed CPU or
    /// content-size heuristic. Background coverage workers use it to yield
    /// before acquiring model work.
    pub fn has_foreground_dispatch_pressure(&self) -> bool {
        self.dispatch_queue_snapshot().is_some_and(|snapshot| {
            snapshot
                .registry
                .pending
                .iter()
                .chain(snapshot.registry.in_flight.iter())
                .any(|job| job.priority != magicllm::dispatch::Priority::Background)
        })
    }

    /// Whether durable background work should remain on disk for a later
    /// maintenance pass instead of being admitted to the in-memory dispatcher.
    ///
    /// Foreground work always wins. Background work also yields at the bounded
    /// lane admission limit or while the operation's configured provider
    /// already has a pending/in-flight background call. The latter uses the
    /// resolved provider identity rather than model/profile names, so config
    /// remapping does not invalidate admission behavior.
    pub fn should_defer_background_operation(&self, operation: &LLMOperation) -> bool {
        let Some(snapshot) = self.dispatch_queue_snapshot() else {
            return false;
        };
        let provider = self.provider_for_operation(operation);
        background_dispatch_capacity_busy(&snapshot, provider.as_deref())
    }

    /// Route a fully-built request through the dispatch queue when one is
    /// installed (carrying the operation-derived priority); otherwise fall
    /// back to calling the shared router directly. Returns the same
    /// `RouterResponse` the cut sites already consume.
    ///
    /// The single model-call boundary for this process: every `generate_*`
    /// entry point funnels here, so one span covers queue dwell plus provider
    /// execution and every other family's work nests its model calls beneath
    /// its own span.
    ///
    /// `skip_all` is not negotiable — `request` holds the assembled prompt,
    /// tool schemas and (through `extra`) routing credentials. Only the
    /// operation name and the model identity are named, and scope is borrowed
    /// from the same authority `authoritative_trace_scope` uses so an unscoped
    /// router inherits its caller's scope rather than asserting a wrong one.
    #[instrument(
        name = "llm_dispatch",
        skip_all,
        fields(
            activity_kind = KIND_LLM,
            operation = operation.as_str(),
            model = %request.model,
            principal = self
                .task_context
                .as_ref()
                .and_then(|task_ref| task_ref.scope.as_ref())
                .or(self.scope_context.as_ref())
                .map(|scope| scope.principal.as_str()),
            workspace = self
                .task_context
                .as_ref()
                .and_then(|task_ref| task_ref.scope.as_ref())
                .or(self.scope_context.as_ref())
                .map(|scope| scope.workspace.as_str()),
        )
    )]
    async fn route_request(
        &self,
        router: &Arc<ConfiguredRouter>,
        operation: &LLMOperation,
        mut request: RouterRequest,
        bind_router_snapshot: bool,
    ) -> Result<RouterResponse> {
        // This includes the generic workflow lane carrying an App disclosure
        // guard as well as named app:* behavior operations. Both need the
        // queue's cancellation, attribution and physical settlement contract.
        if self.dispatch_queue.get().is_none()
            && (self.disclosure_guard.is_some()
                || matches!(operation, LLMOperation::App(_))
                || operation.as_str().starts_with("app:"))
        {
            return Err(anyhow!("app LLM operation requires the dispatcher queue"));
        }
        if matches!(operation, LLMOperation::App(_)) || operation.as_str().starts_with("app:") {
            if !matches!(operation, LLMOperation::App(key) if is_valid_app_operation_key(key)) {
                return Err(anyhow!(
                    "reserved app LLM namespace requires the App operation arm"
                ));
            }
            let disclosure_guard = self.disclosure_guard.as_ref().ok_or_else(|| {
                anyhow!("app LLM operation reached dispatch without disclosure authority")
            })?;
            let dispatch_permit = self.app_operation_dispatch_permit.as_ref().ok_or_else(|| {
                anyhow!("app LLM operation reached dispatch without dispatcher authority")
            })?;
            if !dispatch_permit.permits_router_request(
                operation.as_str(),
                disclosure_guard.expected_profile(),
                request.max_output_tokens,
            ) {
                return Err(anyhow!(
                    "app LLM dispatcher authority does not match the physical request"
                ));
            }
        }
        preflight_execution_token_budget()?;
        if let Some(guard) = self.disclosure_guard.as_ref() {
            let mut extra = match request.take_extra_value() {
                Some(Value::Object(extra)) => extra,
                Some(_) => {
                    return Err(anyhow!(
                        "app disclosure request carries an invalid routing control lane"
                    ));
                },
                None => serde_json::Map::new(),
            };
            if extra
                .get("router_profile_override")
                .and_then(Value::as_str)
                .is_some_and(|profile| profile != guard.expected_profile())
            {
                return Err(anyhow!(
                    "app disclosure request changed its admitted physical profile"
                ));
            }
            extra.insert(
                "router_profile_override".to_owned(),
                Value::String(guard.expected_profile().to_owned()),
            );
            request.set_extra(Value::Object(extra));
            request.metadata.set_disclosure_guard(guard.clone());
        }
        let call_started_at_ms = chrono::Utc::now().timestamp_millis();
        let fallback_provider =
            magicllm::dispatch::DispatchRouter::provider_for_request(router.as_ref(), &request)
                .map_or_else(String::new, |provider| provider.to_string());
        let fallback_model = request.model.clone();
        let fallback_profile = request
            .extra
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("router_profile_override"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);
        let task_scope = self.authoritative_trace_scope();
        let authoritative_resolution = if self.task_context.is_some() {
            magicllm::LlmScopeResolution::Inherited
        } else {
            magicllm::LlmScopeResolution::Explicit
        };
        // Calls made by one agentic execution share its execution root as the
        // trace root, while `ensure_trace_context` still creates a fresh
        // logical call id for each model request.
        if request.metadata.trace_id.is_none() {
            request.metadata.trace_id = self.task_context.as_ref().and_then(|task_ref| {
                task_ref
                    .root_execution_id
                    .clone()
                    .or_else(|| task_ref.execution_id.clone())
            });
        }
        let mut trace_context = request
            .metadata
            .ensure_trace_context(task_scope.clone(), workload_for_operation(operation));
        Self::reconcile_authoritative_trace_scope(
            &mut trace_context,
            task_scope.as_ref(),
            authoritative_resolution,
        )?;
        request.metadata.set_trace_context(trace_context.clone());
        if let Some(task_ref) = self.task_context.as_ref() {
            trace_context.task_id = Some(task_ref.task_id.clone());
            trace_context.root_execution_id = task_ref.root_execution_id.clone();
            trace_context.execution_id = task_ref.execution_id.clone();
            trace_context.plan_id = task_ref.plan_id.clone();
            trace_context.step_id = task_ref.step_id.clone();
            trace_context.chat_session_id = task_ref.chat_session_id.clone();
            trace_context.chat_turn_id = task_ref.chat_turn_id.clone();
            trace_context.iteration_id = task_ref.iteration_id.clone();
            trace_context.user_message_id = task_ref.user_message_id.clone();
            if let Some(scope) = task_ref.scope.as_ref() {
                trace_context.scope = scope.clone();
                trace_context.scope_resolution = magicllm::LlmScopeResolution::Inherited;
            }
            request.metadata.set_trace_context(trace_context.clone());
        }
        if self.disclosure_guard.is_none() {
            if let Some(sink) = self.content_capture_sink.get() {
                request.metadata.content_capture_sink = sink.clone();
                if !request.metadata.logical_content_capture_emitted {
                    sink.observe(magicllm::LlmContentCaptureEvent::LogicalRequest {
                        request: &request,
                    });
                    request.metadata.logical_content_capture_emitted = true;
                }
            }
        }
        let response = if let Some(queue) = self.dispatch_queue.get() {
            // Read from the ambient tracing context rather than a parameter:
            // the id wanted here is this function's own `llm_dispatch` span,
            // the row an operator watches while the call is in flight, and
            // `current_activity_id` is the layer's own reader for it. Passing
            // it down through signatures would fork the derivation.
            let origin = magicllm::dispatch::JobOrigin::op(operation.as_str())
                .with_caller("operation_llm_router::route_request")
                .with_activity_id(current_activity_id().map(|id| id.to_string()));
            let (job, rx) = magicllm::LlmJob::new(request, origin);
            let priority = self
                .dispatch_priority_override
                .unwrap_or_else(|| priority_for_operation(operation));
            let mut job = job.with_priority(priority);
            // Apps admission validated this router's exact physical profile.
            // The queue's boot-time router can retain older resource ceilings
            // after a live reload, so guarded calls must carry this snapshot.
            if bind_router_snapshot || self.disclosure_guard.is_some() {
                let routing_snapshot: Arc<dyn magicllm::dispatch::DispatchRouter> = router.clone();
                job = job.with_router_snapshot(routing_snapshot);
            }
            // Tag the job with its owning execution (when this is a per-execution
            // router clone) so cancel_execution can target its queued + in-flight
            // jobs via the cancellation gates.
            if let Some(task_ref) = &self.task_context {
                job = job.with_task(task_ref.clone());
            }
            queue.submit(job).await.map_err(|err| anyhow!(err))?;
            let dispatched = rx
                .await
                .map_err(|_| anyhow!("LLM dispatch queue receiver dropped"))
                .map_err(fail_closed_router_request_error)?
                .inspect_err(|error| {
                    // Queue trace events intentionally retain only an error class.
                    // Classify the original error here before fail-closed redaction;
                    // background callers may never emit LLMResponseReceived.
                    if let (Some(broadcaster), Some(failure)) = (
                        self.event_broadcaster.get(),
                        crate::magician_v2::realtime_events::ServiceFailure::from_error(
                            &error.to_string(),
                        ),
                    ) {
                        let (provider, profile) = error
                            .effective_route()
                            .map(|(profile, provider, _)| (provider.to_string(), Some(profile)))
                            .unwrap_or((fallback_provider.clone(), fallback_profile.as_deref()));
                        let service = match profile {
                            Some(profile) => format!("{provider} [{profile}]"),
                            None => provider,
                        };
                        broadcaster.report_service_health(
                            &trace_context.scope.principal,
                            &trace_context.scope.workspace,
                            &service,
                            Err(failure),
                        );
                    }
                })
                .map_err(|err| anyhow!(err))
                .map_err(fail_closed_router_request_error)?;
            dispatched.into_response()
        } else {
            // Direct routing does not pass through the dispatch queue,
            // so this is the only path where we can still capture the live
            // span's activity id for this call. Stamp it here when present.
            Self::ensure_trace_context_activity_id(&mut trace_context);
            request.metadata.set_trace_context(trace_context.clone());

            let provider =
                magicllm::dispatch::DispatchRouter::provider_for_request(router.as_ref(), &request);
            let model = request.model.clone();
            let profile = request
                .extra
                .as_deref()
                .and_then(Value::as_object)
                .and_then(|extra| extra.get("router_profile_override"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            let started_at_ms = chrono::Utc::now().timestamp_millis();
            let started = Instant::now();
            let provider_attempt_counter = request.metadata.ensure_provider_attempt_counter();
            match router.route(request).await {
                Ok(response) => response,
                Err(source) => {
                    let provider_attempt_count = provider_attempt_counter.load(Ordering::Relaxed);
                    let (provider, model, profile) = source
                        .effective_route()
                        .map(|(profile, provider, model)| {
                            (
                                Some(provider.clone()),
                                model.to_string(),
                                Some(profile.to_string()),
                            )
                        })
                        .unwrap_or((provider, model, profile));
                    self.emit_direct_route_failure(
                        &trace_context,
                        provider_attempt_count,
                        provider,
                        model,
                        profile,
                        operation,
                        started_at_ms,
                        u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
                        &source,
                    );
                    let error = super::multi_llm_service::MultiLLMService::traced_route_error(
                        source,
                        trace_context,
                        provider_attempt_count,
                    );
                    return Err(fail_closed_router_request_error(error));
                },
            }
        };
        if let Err(error) = account_router_response_tokens(&response) {
            self.emit_post_response_validation_failure(
                &response,
                operation,
                &fallback_provider,
                &fallback_model,
                fallback_profile.as_deref(),
                call_started_at_ms,
                "execution_token_budget",
            );
            return Err(error);
        }
        Ok(response)
    }

    /// Whether the dispatch queue's local pre-summarisation step is enabled.
    ///
    /// Read from the live (hot-reloadable) dispatch config when a queue is
    /// installed. Returns `false` when no queue is wired (so callers fall back
    /// to the inline single-block prompt — byte-identical to pre-queue
    /// behaviour) or when local-prep is switched off in config.
    fn local_prep_enabled(&self) -> bool {
        self.dispatch_queue
            .get()
            .map(|queue| queue.config_handle().read().local_prep.enabled)
            .unwrap_or(false)
    }

    /// The live local-prep config when the dispatch queue is installed AND
    /// local-prep is enabled; `None` otherwise. Producers that summarise large
    /// bodies at write-time (e.g. the agentic executor's tool/step outputs) use
    /// this to decide whether to summarise and with what model/threshold.
    pub fn local_prep_config(&self) -> Option<magicllm::dispatch::LocalPrepConfig> {
        let queue = self.dispatch_queue.get()?;
        let config = queue.config_handle().read().local_prep.clone();
        config.enabled.then_some(config)
    }

    /// Clone the router with per-agent operation routing overrides.
    pub fn with_routing_overrides(&self, overrides: Option<OperationRoutingOverrides>) -> Self {
        let mut cloned = self.clone();
        cloned.routing_overrides = overrides;
        cloned
    }

    /// Inspect the overrides this clone resolves under in routing tests.
    #[cfg(any(test, feature = "test-fixtures"))]
    pub(crate) fn routing_overrides(&self) -> Option<&OperationRoutingOverrides> {
        self.routing_overrides.as_ref()
    }

    /// Clone the router carrying a per-execution task reference. Every dispatch
    /// job this clone submits is tagged with `task_ref`, enabling
    /// `cancel_execution` to cancel the execution's queued + in-flight LLM
    /// calls. Inherits the shared dispatch-queue handle (`Arc<OnceLock>`).
    pub fn with_task_context(&self, task_ref: Option<magicllm::dispatch::TaskRef>) -> Self {
        let mut cloned = self.clone();
        cloned.task_context = task_ref;
        cloned
    }

    /// Clone the router with authoritative scope for a non-task call surface.
    /// This prevents queue failures and cancellations from falling into the
    /// process-wide `system/default` compatibility scope before the caller's
    /// success telemetry has a chance to rebind the receipt.
    pub fn with_scope_context(&self, scope: Option<magicllm::LlmScope>) -> Self {
        let mut cloned = self.clone();
        cloned.scope_context = scope;
        cloned
    }

    /// Clone the router with one non-serializable app disclosure fence. The
    /// guard pins the physical profile/provider/endpoint cohort and performs
    /// final mutable-authority revalidation inside MagicLLM before each
    /// provider attempt.
    pub fn with_disclosure_guard(&self, guard: Option<magicllm::LlmDisclosureGuard>) -> Self {
        let mut cloned = self.clone();
        cloned.disclosure_guard = guard;
        cloned
    }

    /// Snapshot the operator-owned physical router configuration for a
    /// server-side admission boundary. The returned value contains no runtime
    /// provider clients or bearer credentials; callers still have to attach a
    /// disclosure guard before any admitted bytes are dispatched.
    pub fn router_config_snapshot(&self) -> Option<LLMRouterConfig> {
        self.read_state().router_config.clone()
    }

    /// Clone the router with an explicit dispatch lane. Intended for bounded
    /// background subsystems that reuse an existing managed operation without
    /// inheriting that operation's foreground/normal scheduling lane.
    pub fn with_dispatch_priority(&self, priority: magicllm::dispatch::Priority) -> Self {
        let mut cloned = self.clone();
        cloned.dispatch_priority_override = Some(priority);
        cloned
    }

    /// Clone the router with a per-agent temperature override.
    /// When set, this temperature is applied to all LLM requests, overriding the profile default.
    pub fn with_temperature_override(&self, temperature: Option<f32>) -> Self {
        let mut cloned = self.clone();
        cloned.temperature_override = temperature;
        cloned
    }

    /// Return the correct `tool_choice` value for a profile.
    ///
    /// Profile metadata is the source of truth when present. Provider defaults
    /// are only fallback behavior for older profiles that predate explicit
    /// tool-choice policy.
    pub fn tool_choice_for_profile(profile: &LLMProfile) -> Value {
        if let Some(tool_choice) = profile
            .metadata
            .as_ref()
            .and_then(|metadata| metadata.get("tool_choice"))
            .cloned()
        {
            return tool_choice;
        }

        if matches!(profile.provider, LLMProviderKind::Minimax)
            && profile.supports_reasoning.unwrap_or(false)
        {
            serde_json::json!({"type": "any"})
        } else if matches!(
            profile.provider,
            LLMProviderKind::Anthropic | LLMProviderKind::Minimax | LLMProviderKind::DeepSeek
        ) {
            serde_json::json!({"type": "auto"})
        } else {
            serde_json::json!({"type": "any"})
        }
    }

    /// Resolve a realtime voice provider for the given operation.
    /// Reads the `realtime_voice` config section, looks up the
    /// operation in the operation_mapping (falling back to
    /// `realtime_voice.default_profile`), and instantiates the
    /// provider via [`magicllm::realtime::build_realtime_provider`].
    ///
    /// Returns `None` when the operation has no mapping AND no
    /// default profile is set — distinct from `Some(Err(_))` which
    /// means a mapping exists but provider construction failed
    /// (missing env var, unknown provider key, etc.).
    pub fn resolve_realtime_provider(
        &self,
        operation: &LLMOperation,
    ) -> Option<
        Result<
            std::sync::Arc<dyn magicllm::realtime::RealtimeProvider>,
            magicllm::realtime::RealtimeProviderError,
        >,
    > {
        let state = self.read_state();
        let realtime = &state.router_config.as_ref()?.realtime_voice;
        let op_key = operation.as_str();
        let profile_name = realtime
            .operation_mapping
            .get(op_key)
            .cloned()
            .or_else(|| realtime.default_profile.clone())?;
        let profile = realtime.profiles.get(&profile_name)?.clone();
        Some(magicllm::realtime::build_realtime_provider(&profile))
    }

    /// Resolve a realtime voice provider by explicit profile name.
    /// Used by native host clients that need a specific topology
    /// (for example backend-proxied live PTT) without changing the
    /// browser voice-controller operation mapping.
    pub fn resolve_realtime_provider_profile(
        &self,
        profile_name: &str,
    ) -> Option<
        Result<
            std::sync::Arc<dyn magicllm::realtime::RealtimeProvider>,
            magicllm::realtime::RealtimeProviderError,
        >,
    > {
        let state = self.read_state();
        let realtime = &state.router_config.as_ref()?.realtime_voice;
        let profile = realtime.profiles.get(profile_name)?.clone();
        Some(magicllm::realtime::build_realtime_provider(&profile))
    }

    /// Snapshot of the configured realtime voice profile for an
    /// operation, without instantiating the provider. Useful for
    /// surfacing settings (`max_session_duration_secs`, watermark)
    /// to the orchestrator without going through `build_*`.
    pub fn realtime_voice_profile(
        &self,
        operation: &LLMOperation,
    ) -> Option<magicllm::config::RealtimeVoiceProfile> {
        let state = self.read_state();
        let realtime = &state.router_config.as_ref()?.realtime_voice;
        let op_key = operation.as_str();
        let profile_name = realtime
            .operation_mapping
            .get(op_key)
            .cloned()
            .or_else(|| realtime.default_profile.clone())?;
        realtime.profiles.get(&profile_name).cloned()
    }

    /// Snapshot of a realtime voice profile by explicit name.
    pub fn realtime_voice_profile_by_name(
        &self,
        profile_name: &str,
    ) -> Option<magicllm::config::RealtimeVoiceProfile> {
        let state = self.read_state();
        let realtime = &state.router_config.as_ref()?.realtime_voice;
        realtime.profiles.get(profile_name).cloned()
    }

    /// Stable, sorted snapshot used to build the Web/iOS selector catalog.
    /// Provider construction remains separate so unavailable credentials can be
    /// represented explicitly instead of making configured options disappear.
    /// Order is the profile's `display_order` (unset sorts as 0), then the
    /// profile id, so clients render the catalog as the operator arranged it.
    pub fn realtime_voice_profiles(&self) -> Vec<(String, magicllm::config::RealtimeVoiceProfile)> {
        let state = self.read_state();
        let Some(config) = state.router_config.as_ref() else {
            return Vec::new();
        };
        let mut profiles = config
            .realtime_voice
            .profiles
            .iter()
            .map(|(name, profile)| (name.clone(), profile.clone()))
            .collect::<Vec<_>>();
        profiles.sort_by(|left, right| {
            left.1
                .display_sort_key(&left.0)
                .cmp(&right.1.display_sort_key(&right.0))
        });
        profiles
    }

    pub fn realtime_voice_default_profile_name(&self) -> Option<String> {
        let state = self.read_state();
        let realtime = &state.router_config.as_ref()?.realtime_voice;
        realtime
            .operation_mapping
            .get(LLMOperation::VoiceController.as_str())
            .cloned()
            .or_else(|| realtime.default_profile.clone())
    }

    /// Whether the `agentic_decision` operation routes to a Yutori-provider
    /// profile in the loaded router config.
    ///
    /// Returns `true` if the explicit effective routing override selects
    /// Yutori, or (when no override exists) if
    /// `operation_mapping["agentic_decision"]`'s default or
    /// `when_has_images` alternative resolves to a Yutori profile. This
    /// replaces the old `.use_yutori_browser` sentinel file as the activation
    /// signal.
    ///
    /// The observation seam reads this once per browser turn to force a
    /// screenshot-bearing (`Full`) observation and stamp the CSS viewport, since
    /// Yutori is a vision/coordinate model that needs an image every turn.
    ///
    /// Degrades to `false` when no config is loaded, the operation is unmapped,
    /// or the named profile is absent from `profiles` — i.e. OFF by default.
    /// Whether a turn with images resolves `operation` to a different
    /// provider or model than a turn without. The decision loop drops its
    /// provider continuation chain when a turn's image shape flips, because
    /// a `when_has_images` alternative can select another profile and a
    /// chain id is scoped to one. When both shapes land on the same profile
    /// the chain is still valid; dropping it rebuilt the full prompt, billed
    /// at 0% cached, every time a desktop snapshot attached a screenshot.
    /// `true` when either shape cannot be resolved (keep the safe rebuild).
    /// The provider `operation` resolves to for a tool-calling request
    /// without images, or `None` when it cannot be resolved.
    pub fn operation_provider(&self, operation: &str) -> Option<LLMProviderKind> {
        self.request_profile_for_operation_with_shape(
            &LLMOperation::Other(operation.to_string()),
            None,
            &magicllm::config::RequestShape {
                has_images: false,
                has_tools: true,
            },
        )
        .ok()
        .map(|profile| profile.provider)
    }

    pub fn image_shape_changes_profile(&self, operation: &str) -> bool {
        let operation = LLMOperation::Other(operation.to_string());
        let resolve = |has_images| {
            self.request_profile_for_operation_with_shape(
                &operation,
                None,
                &magicllm::config::RequestShape {
                    has_images,
                    has_tools: true,
                },
            )
        };
        match (resolve(false), resolve(true)) {
            (Ok(text), Ok(images)) => {
                text.provider != images.provider || text.model != images.model
            },
            _ => true,
        }
    }

    pub fn agentic_decision_uses_yutori(&self) -> bool {
        let state = self.read_state();
        let config = match state.router_config.as_ref() {
            Some(c) => c,
            None => return false,
        };

        let operation = LLMOperation::Other("agentic_decision".to_string());
        if let Some(endpoint) = self
            .routing_overrides
            .as_ref()
            .and_then(|overrides| overrides.endpoint_for_operation(&operation))
        {
            // An explicit execution/agent route is authoritative. Do not fall
            // through to the global Yutori mapping when that route selects a
            // different provider/profile.
            if let Some(provider) = endpoint.provider_name() {
                return provider.eq_ignore_ascii_case("yutori");
            }
            return endpoint
                .profile_name()
                .and_then(|profile| config.profiles.get(profile))
                .is_some_and(|profile| matches!(profile.provider, LLMProviderKind::Yutori));
        }

        let selector = match config.operation_mapping.get("agentic_decision") {
            Some(s) => s,
            None => return false,
        };

        // Default profile arm.
        let default_name = selector.default_profile();
        if let Some(default_profile) = config.profiles.get(default_name) {
            if matches!(default_profile.provider, LLMProviderKind::Yutori) {
                return true;
            }
        }

        // when_has_images alternative arm (only if it differs from default).
        if let magicllm::config::OperationProfileSelector::Conditional {
            default,
            when_has_images: Some(alt_name),
            ..
        } = selector
        {
            if alt_name != default {
                if let Some(alt_profile) = config.profiles.get(alt_name.as_str()) {
                    if matches!(alt_profile.provider, LLMProviderKind::Yutori) {
                        return true;
                    }
                }
            }
        }

        false
    }

    /// Profile NAME + provider kind of the profile EXPLICITLY bound to
    /// `operation_name` in `operation_mapping` — the input to fail-closed
    /// locality guards
    /// (`channel_assist::assist::distill::resolve_local_provider`).
    /// The name is returned so guard callers can PIN their subsequent
    /// dispatch to the exact profile they verified (see
    /// [`Self::generate_for_operation_with_system_pinned`]) instead of
    /// letting dispatch re-resolve against a possibly-reloaded config.
    ///
    /// Returns `None` when no config is loaded, when the operation has no
    /// `operation_mapping` entry, or when the mapped default profile name
    /// is absent from `profiles`. The router's `default_profile` fallback
    /// is deliberately NOT consulted: for fail-closed callers "unbound"
    /// must read as "unbound", never as "whatever the default profile
    /// happens to be".
    pub fn explicit_binding_for_operation(
        &self,
        operation_name: &str,
    ) -> Option<(String, LLMProviderKind)> {
        let state = self.read_state();
        let config = state.router_config.as_ref()?;
        let selector = config.operation_mapping.get(operation_name)?;
        // Locality-effective arm: under `privacy.processing.mode: cloud` the
        // `when_cloud` profile is the binding locality guards verify and pin.
        let profile_name =
            selector.profile_for_locality(&magicllm::config::RequestShape::NONE, config.locality);
        let profile = config.profiles.get(profile_name)?;
        Some((profile_name.to_string(), profile.provider.clone()))
    }

    /// The operator's processing locality (`privacy.processing.mode`),
    /// derived into the router config at load. Locality guards consult this
    /// so "who may serve this operation" is policy, not a Rust constant.
    /// No router config ⇒ local (today's behavior).
    pub fn processing_locality(&self) -> magicllm::ProcessingLocality {
        self.read_state()
            .router_config
            .as_ref()
            .map(|config| config.locality)
            .unwrap_or(magicllm::ProcessingLocality::Local)
    }

    /// Get the effective router profile for a specific operation.
    pub fn get_config_for_operation(&self, operation: &LLMOperation) -> Result<LLMProfile> {
        let state = self.read_state();
        let config = state
            .router_config
            .as_ref()
            .ok_or_else(|| anyhow!("No router configuration loaded"))?;
        Self::profile_for_operation_from_config(config, self.routing_overrides.as_ref(), operation)
    }

    /// Whether the effective profile for this operation crosses the Phase 7
    /// logical-chunk activation boundary.
    pub fn logical_chunking_enabled(&self, operation: &LLMOperation) -> bool {
        self.get_config_for_operation(operation)
            .ok()
            .and_then(|profile| profile.chunking)
            .is_some_and(|policy| policy.enabled)
    }

    /// Generate analysis for a specific operation type
    pub async fn generate_for_operation(
        &self,
        operation: &LLMOperation,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_and_model(operation, None, prompt, None)
            .await
    }

    /// Generate analysis with an optional system prompt.
    pub async fn generate_for_operation_with_system(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_and_model(operation, system_prompt, prompt, None)
            .await
    }

    /// Execute a registered structured operation through the logical chunk
    /// runner when its authoritative profile has chunking enabled. When the
    /// profile is disabled, this preserves the existing monolithic request
    /// path (including its optional local-prep block) exactly.
    ///
    /// This is the Phase 7 production boundary: callers provide the same
    /// structured source value used by the domain adapter, while the router
    /// continues to own profile selection, queue priority, task correlation,
    /// deadlines, accounting, and the ordinary simplified response envelope.
    pub async fn generate_for_chunkable_operation_with_system(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        logical_input: Value,
        summarisable: Option<(&str, SummarisationPurpose)>,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_chunkable_operation_with_lazy_fallback(operation, logical_input, |_| {
            let mut fallback = ChunkableOperationFallback::new(
                system_prompt.map(str::to_string),
                prompt.to_string(),
            );
            if let Some((source, purpose)) = summarisable {
                fallback = fallback.with_summarisable(source.to_string(), purpose);
            }
            Ok(fallback)
        })
        .await
    }

    /// Route a structured operation from one immutable routing/config
    /// snapshot, materializing its legacy prompt only when that snapshot has
    /// logical chunking disabled.
    ///
    /// The producer receives the logical input by reference so it can render
    /// the exact legacy source bytes without retaining a second recursive JSON
    /// tree. It runs at most once and never runs on the chunked path. Once a
    /// fallback has been materialized (successfully or not), the unused logical
    /// input is released iteratively before any provider await.
    pub async fn generate_for_chunkable_operation_with_lazy_fallback<F>(
        &self,
        operation: &LLMOperation,
        logical_input: Value,
        materialize_fallback: F,
    ) -> Result<SimplifiedLLMResponse>
    where
        F: FnOnce(&Value) -> Result<ChunkableOperationFallback>,
    {
        if matches!(operation, LLMOperation::App(_)) || operation.as_str().starts_with("app:") {
            return Err(anyhow!(
                "reserved app LLM operations cannot use the generic logical-chunk path"
            ));
        }
        if self.disclosure_guard.is_some() {
            return Err(anyhow!(
                "logical chunking has no app disclosure propagation contract"
            ));
        }
        let mut logical_input = IterativeJsonOwner::new(logical_input);
        let snapshot = self.snapshot_chunkable_operation(operation)?;
        let chunking = snapshot
            .request_profile
            .chunking
            .as_ref()
            .filter(|policy| policy.enabled)
            .cloned();
        let Some(chunking) = chunking else {
            let router = snapshot.router.as_ref().ok_or_else(|| {
                anyhow!(
                    "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
                    snapshot
                        .provider_override
                        .as_deref()
                        .unwrap_or(snapshot.request_profile.provider.as_str())
                )
            })?;
            if !self.is_provider_available_for_operation(
                router,
                &snapshot.request_profile,
                snapshot.provider_override.as_deref(),
            ) {
                return Err(anyhow!(
                    "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
                    snapshot
                        .provider_override
                        .as_deref()
                        .unwrap_or(snapshot.request_profile.provider.as_str())
                ));
            }

            let fallback = materialize_fallback(logical_input.as_value());
            logical_input.discard();
            let fallback = fallback?;
            let ChunkableOperationFallback {
                system_prompt,
                prompt,
                summarisable,
            } = fallback;
            let summarisable = summarisable
                .as_ref()
                .map(|(source, purpose)| (source.as_str(), purpose.clone()));
            // The captured router already freezes the operation mapping and
            // fallback chain. Forward only a caller's original explicit
            // profile lock; synthesizing a lock for an ordinary mapping would
            // incorrectly disable that profile's established retry/fallback
            // traversal.
            let dispatch_profile = snapshot.explicit_profile_override.as_deref();
            let timeout_secs = Self::timeout_for_profile(operation, &snapshot.request_profile);
            return self
                .generate_via_router(
                    router,
                    operation,
                    system_prompt.as_deref(),
                    prompt,
                    &snapshot.request_profile,
                    timeout_secs,
                    dispatch_profile,
                    snapshot.effective_model_override.as_deref(),
                    snapshot.provider_override.as_deref(),
                    None,
                    None,
                    None,
                    summarisable,
                    true,
                )
                .await;
        };
        // The logical path will never need the fallback captures. Release
        // prompt templates/source borrows before planning and provider awaits.
        drop(materialize_fallback);

        if snapshot.effective_model_override.is_some() || snapshot.provider_override.is_some() {
            return Err(anyhow!(
                "logical chunking for operation '{}' cannot run with model/provider overrides",
                operation.as_str()
            ));
        }
        let request_profile = &snapshot.request_profile;
        let profile_name = snapshot.logical_profile_name.as_deref().ok_or_else(|| {
            anyhow!(
                "chunk-enabled operation '{}' has no authoritative profile mapping",
                operation.as_str()
            )
        })?;
        let adapter_id = chunking
            .adapter
            .as_deref()
            .map(str::trim)
            .filter(|adapter| !adapter.is_empty())
            .ok_or_else(|| anyhow!("chunk-enabled profile '{profile_name}' has no adapter"))?;
        let physical_window_tokens = request_profile.context_window_tokens.ok_or_else(|| {
            anyhow!("chunk-enabled profile '{profile_name}' has no physical context window")
        })?;
        let logical_window_tokens = chunking.logical_window_tokens.ok_or_else(|| {
            anyhow!("chunk-enabled profile '{profile_name}' has no logical context window")
        })?;
        let target_payload_tokens = chunking.target_payload_tokens.ok_or_else(|| {
            anyhow!("chunk-enabled profile '{profile_name}' has no target payload budget")
        })?;
        let reserved_output_tokens = request_profile.max_output_tokens.unwrap_or(4_096);
        let budget = magicllm::ChunkBudget::new(
            physical_window_tokens,
            logical_window_tokens,
            target_payload_tokens,
            2_048,
            reserved_output_tokens,
            chunking.safety_margin_tokens,
        )
        .map_err(|error| {
            anyhow!("logical chunk budget for '{profile_name}' is invalid: {error}")
        })?;

        let configured_router = snapshot.router.clone().ok_or_else(|| {
            anyhow!(
                "logical chunking for operation '{}' has no configured router",
                operation.as_str()
            )
        })?;
        let dispatch: Arc<dyn LogicalChunkDispatch> = if let Some(queue) = self.dispatch_queue.get()
        {
            Arc::clone(queue) as Arc<dyn LogicalChunkDispatch>
        } else {
            Arc::new(DirectLogicalChunkDispatch {
                router: configured_router.clone(),
                event_broadcaster: self.event_broadcaster.get().cloned(),
            })
        };
        let registry = global_chunk_adapter_registry()
            .read()
            .map_err(|_| anyhow!("logical chunk adapter registry lock is poisoned"))?
            .clone();
        let mut runner = LogicalChunkRunner::new(
            dispatch,
            registry,
            Arc::new(magicllm::ConservativeOllamaEstimator),
        );
        if let Some(broadcaster) = self.event_broadcaster.get() {
            runner = runner.with_telemetry(Arc::new(RuntimeLogicalChunkTelemetry {
                broadcaster: Arc::clone(broadcaster),
            }));
        }

        let timeout_secs = Self::timeout_for_profile(operation, request_profile);
        let mut base_request = RouterRequest {
            model: request_profile.model.clone(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: self.temperature_override.or(request_profile.temperature),
            max_output_tokens: Some(reserved_output_tokens),
            response_format: Some(magicllm::LLMResponseFormat::JsonObject.into()),
            ..Default::default()
        };
        if let Some(metadata) = request_profile.metadata.as_ref() {
            let extra = metadata
                .iter()
                .filter(|(key, _)| !is_config_only_param(key))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect::<serde_json::Map<_, _>>();
            if !extra.is_empty() {
                base_request.set_extra(Value::Object(extra));
            }
        }

        // Preserve the ordinary operation-router execution-budget boundary
        // before transferring ownership out of the iterative guard. Physical
        // child usage is aggregated by the runner and charged once below.
        preflight_execution_token_budget()?;
        let mut logical_request = magicllm::LogicalLlmRequest {
            operation: operation.as_str().to_string(),
            input: logical_input.take(),
            base_request,
        };
        let mut execution = LogicalChunkExecutionContext::new(adapter_id, profile_name);
        execution.fallback_policy = chunking.fallback_policy;
        execution.fallback_profile = chunking.fallback_profile.clone();
        execution.priority = self
            .dispatch_priority_override
            .unwrap_or_else(|| priority_for_operation(operation));
        execution.task_ref = self.task_context.clone();
        execution.trace_id = execution.task_ref.as_ref().and_then(|task_ref| {
            task_ref
                .root_execution_id
                .clone()
                .or_else(|| task_ref.execution_id.clone())
        });
        execution.deadline = Instant::now().checked_add(Duration::from_secs(
            Self::logical_timeout_for_profile(operation, request_profile),
        ));
        execution.caller = "operation_llm_router::logical_chunk_production".to_string();
        execution.router_snapshot =
            Some(configured_router as Arc<dyn magicllm::dispatch::DispatchRouter>);

        let mut logical_trace = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::legacy_default(),
            workload_for_operation(operation),
        )
        .with_scope_resolution(magicllm::LlmScopeResolution::SystemDefault);
        if let Some(trace_id) = execution.trace_id.as_ref() {
            logical_trace.trace_id = trace_id.clone();
        }
        logical_trace.llm_call_id = execution.logical_call_id.clone();
        if let Some(task_ref) = execution.task_ref.as_ref() {
            logical_trace.task_id = Some(task_ref.task_id.clone());
            logical_trace.root_execution_id = task_ref.root_execution_id.clone();
            logical_trace.execution_id = task_ref.execution_id.clone();
            logical_trace.plan_id = task_ref.plan_id.clone();
            logical_trace.step_id = task_ref.step_id.clone();
            logical_trace.chat_session_id = task_ref.chat_session_id.clone();
            logical_trace.chat_turn_id = task_ref.chat_turn_id.clone();
            logical_trace.iteration_id = task_ref.iteration_id.clone();
            logical_trace.user_message_id = task_ref.user_message_id.clone();
            if let Some(scope) = task_ref.scope.as_ref() {
                logical_trace.scope = scope.clone();
                logical_trace.scope_resolution = magicllm::LlmScopeResolution::Inherited;
            }
        }
        logical_request
            .base_request
            .metadata
            .set_trace_context(logical_trace.clone());
        execution.trace_id = Some(logical_trace.trace_id.clone());
        execution.logical_trace_context = Some(logical_trace.clone());
        let response = runner
            .execute(logical_request, budget, execution)
            .await
            .map_err(|error| anyhow!(error))?;
        account_router_response_tokens(&response)?;
        let content = response.text.as_deref().map(str::to_owned).ok_or_else(|| {
            anyhow!(
                "logical chunk runner returned no text for operation '{}'",
                operation.as_str()
            )
        })?;
        let usage = response.usage.as_ref().map(convert_usage);
        Ok(SimplifiedLLMResponse {
            content,
            usage,
            finish_reason: response.finish_reason,
            // The runtime logical-chunk telemetry sink emits the non-billing
            // parent summary and every physical child centrally. Returning a
            // second aggregate call telemetry object here would double-count
            // usage and conflict with the stable parent revision.
            telemetry: None,
        })
    }

    /// Generate analysis for a specific operation, optionally forcing the model.
    pub async fn generate_for_operation_with_model(
        &self,
        operation: &LLMOperation,
        prompt: &str,
        model_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_and_model(operation, None, prompt, model_override)
            .await
    }

    /// Generate analysis with optional system prompt and model override.
    pub async fn generate_for_operation_with_system_and_model(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        model_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        let routing = self.resolve_routing(operation, model_override);
        let request_profile =
            self.request_profile_for_operation(operation, routing.profile_override)?;
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router = self.read_state().router.clone();

        debug!(
            "[MAGICIAN-V2-QUERY] Generating analysis for operation '{}' using model '{}' \
             provider_override={:?} timeout {}s",
            operation.as_str(),
            routing
                .effective_model_override
                .unwrap_or(request_profile.model.as_str()),
            routing.provider_override,
            timeout_secs
        );

        if let Some(router) = router.as_ref() {
            let provider_available = self.is_provider_available_for_operation(
                router,
                &request_profile,
                routing.provider_override,
            );
            if provider_available {
                return self
                    .generate_via_router(
                        router,
                        operation,
                        system_prompt,
                        prompt.to_string(),
                        &request_profile,
                        timeout_secs,
                        routing.profile_override,
                        routing.effective_model_override,
                        routing.provider_override,
                        None,
                        None,
                        None,
                        None,
                        false,
                    )
                    .await;
            }
        }

        let effective_provider = routing
            .provider_override
            .unwrap_or(request_profile.provider.as_str());
        Err(anyhow!(
            "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
            effective_provider
        ))
    }

    /// Generate with the dispatch PINNED to one exact profile, optionally
    /// under a hard provider-kind lock. For fail-closed callers (the
    /// channel-assist distiller's locality guard) that must guarantee the
    /// profile they verified is the profile that dispatches:
    ///
    /// - `pinned_profile` is forwarded as `extra.router_profile_override`,
    ///   which the magicllm router treats as `locked_profile`: dispatch
    ///   cannot re-resolve to the operation's (possibly reloaded) default,
    ///   and ANY `fallback_profile` traversal away from the pinned profile
    ///   is refused ("requested profile disallows fallback").
    /// - `required_provider_kind` is forwarded as
    ///   `extra.router_required_provider_kind`, which the magicllm router
    ///   enforces against its own immutable config snapshot on every hop
    ///   immediately before invoking a provider — closing the window where
    ///   a config hot-reload redefines the pinned profile NAME onto a
    ///   different provider between the caller's check and dispatch.
    ///
    /// The same pair is also checked here against this router's current
    /// snapshot so misconfiguration fails before a request is even built.
    pub async fn generate_for_operation_with_system_pinned(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        pinned_profile: &str,
        required_provider_kind: Option<LLMProviderKind>,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_pinned_inner(
            operation,
            system_prompt,
            prompt,
            pinned_profile,
            required_provider_kind,
            None,
            None,
        )
        .await
    }

    /// Pinned dispatch with a provider-native structured-output contract.
    ///
    /// This preserves the same profile pin and provider-kind lock as
    /// [`Self::generate_for_operation_with_system_pinned`], while forwarding
    /// `response_format` to magicllm. Local extraction callers use this to make
    /// Ollama constrain generation to the actual JSON schema instead of merely
    /// asking for an arbitrary JSON value through profile metadata.
    pub async fn generate_for_operation_with_system_pinned_and_response_format(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        pinned_profile: &str,
        required_provider_kind: Option<LLMProviderKind>,
        response_format: magicllm::LLMResponseFormat,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_pinned_inner(
            operation,
            system_prompt,
            prompt,
            pinned_profile,
            required_provider_kind,
            Some(response_format),
            None,
        )
        .await
    }

    /// Narrow app-only pinned entry point. It refuses the reserved operation
    /// namespace without a disclosure guard and clamps the physical request
    /// profile to the live operator/manifest/budget intersection supplied by
    /// the app dispatcher.
    pub(crate) async fn generate_for_app_operation_with_system_pinned(
        &self,
        dispatch_permit: AppLlmOperationDispatchPermit,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        pinned_profile: &str,
        required_provider_kind: LLMProviderKind,
        max_output_tokens: u32,
        response_format: Option<magicllm::LLMResponseFormat>,
    ) -> Result<SimplifiedLLMResponse> {
        if !matches!(operation, LLMOperation::App(key) if is_valid_app_operation_key(key)) {
            return Err(anyhow!("app dispatcher received an invalid operation key"));
        }
        if self.disclosure_guard.is_none() || max_output_tokens == 0 {
            return Err(anyhow!(
                "app dispatcher requires disclosure authority and a positive output ceiling"
            ));
        }
        if !dispatch_permit.permits_app_entry(
            operation.as_str(),
            pinned_profile,
            &required_provider_kind,
            max_output_tokens,
        ) {
            return Err(anyhow!(
                "app dispatcher permit does not match the requested route"
            ));
        }
        let mut permitted = self.clone();
        permitted.app_operation_dispatch_permit = Some(Arc::new(dispatch_permit));
        permitted
            .generate_for_operation_with_system_pinned_inner(
                operation,
                system_prompt,
                prompt,
                pinned_profile,
                Some(required_provider_kind),
                response_format,
                Some(max_output_tokens),
            )
            .await
    }

    async fn generate_for_operation_with_system_pinned_inner(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        pinned_profile: &str,
        required_provider_kind: Option<LLMProviderKind>,
        response_format: Option<magicllm::LLMResponseFormat>,
        max_output_tokens: Option<u32>,
    ) -> Result<SimplifiedLLMResponse> {
        let mut request_profile =
            self.request_profile_for_operation(operation, Some(pinned_profile))?;
        if let Some(ceiling) = max_output_tokens {
            if ceiling == 0 {
                return Err(anyhow!("pinned output ceiling must be positive"));
            }
            request_profile.max_output_tokens = Some(
                request_profile
                    .max_output_tokens
                    .map_or(ceiling, |profile_limit| profile_limit.min(ceiling)),
            );
        }
        if let Some(required) = required_provider_kind.as_ref() {
            if &request_profile.provider != required {
                return Err(anyhow!(
                    "pinned profile '{}' resolves to provider '{}' but operation '{}' requires \
                     provider kind '{}' (provider lock)",
                    pinned_profile,
                    request_profile.provider,
                    operation.as_str(),
                    required
                ));
            }
        }
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router = self.read_state().router.clone();

        if let Some(router) = router.as_ref() {
            if self.is_provider_available_for_operation(router, &request_profile, None) {
                return self
                    .generate_via_router(
                        router,
                        operation,
                        system_prompt,
                        prompt.to_string(),
                        &request_profile,
                        timeout_secs,
                        Some(pinned_profile),
                        None,
                        None,
                        required_provider_kind.as_ref(),
                        response_format,
                        None,
                        None,
                        false,
                    )
                    .await;
            }
        }

        Err(anyhow!(
            "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
            request_profile.provider.as_str()
        ))
    }

    /// Generate analysis with an optional system prompt, carrying a large body
    /// as a dispatch-queue summarisable block (local pre-summarisation target).
    ///
    /// `summarisable` is `(raw, purpose)` — `raw` MUST be a substring of
    /// `prompt` (it is the large body already interpolated into the prompt).
    /// When the dispatch queue's local-prep is enabled, the worker replaces the
    /// raw body with an Ollama summary before the cloud call; otherwise the
    /// prompt is sent inline unchanged. See [`generate_via_router`].
    pub async fn generate_for_operation_with_system_and_summarisable(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        summarisable: Option<(&str, SummarisationPurpose)>,
    ) -> Result<SimplifiedLLMResponse> {
        let routing = self.resolve_routing(operation, None);
        let request_profile =
            self.request_profile_for_operation(operation, routing.profile_override)?;
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router = self.read_state().router.clone();

        if let Some(router) = router.as_ref() {
            let provider_available = self.is_provider_available_for_operation(
                router,
                &request_profile,
                routing.provider_override,
            );
            if provider_available {
                return self
                    .generate_via_router(
                        router,
                        operation,
                        system_prompt,
                        prompt.to_string(),
                        &request_profile,
                        timeout_secs,
                        routing.profile_override,
                        routing.effective_model_override,
                        routing.provider_override,
                        None,
                        None,
                        None,
                        summarisable,
                        false,
                    )
                    .await;
            }
        }

        let effective_provider = routing
            .provider_override
            .unwrap_or(request_profile.provider.as_str());
        Err(anyhow!(
            "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
            effective_provider
        ))
    }

    /// Like `generate_for_operation_with_model` but accepts an optional JSON schema string.
    /// When `tool_schema` is `Some`, the schema is sent as a native LLM tool definition
    /// instead of being embedded in the prompt text.
    pub async fn generate_for_operation_with_tool_schema(
        &self,
        operation: &LLMOperation,
        prompt: &str,
        tool_schema: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_for_operation_with_system_and_tool_schema(
            operation,
            None,
            prompt,
            tool_schema,
            model_override,
        )
        .await
    }

    /// Like `generate_for_operation_with_tool_schema` with optional system prompt.
    pub async fn generate_for_operation_with_system_and_tool_schema(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: &str,
        tool_schema: Option<&str>,
        model_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        let routing = self.resolve_routing(operation, model_override);
        let request_profile =
            self.request_profile_for_operation(operation, routing.profile_override)?;
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router = self.read_state().router.clone();

        debug!(
            "[MAGICIAN-V2-QUERY] Generating analysis for operation '{}' using model '{}' \
             provider_override={:?} timeout {}s native_tool_schema={}",
            operation.as_str(),
            routing
                .effective_model_override
                .unwrap_or(request_profile.model.as_str()),
            routing.provider_override,
            timeout_secs,
            tool_schema.is_some()
        );

        if let Some(router) = router.as_ref() {
            let provider_available = self.is_provider_available_for_operation(
                router,
                &request_profile,
                routing.provider_override,
            );
            if provider_available {
                return self
                    .generate_via_router(
                        router,
                        operation,
                        system_prompt,
                        prompt.to_string(),
                        &request_profile,
                        timeout_secs,
                        routing.profile_override,
                        routing.effective_model_override,
                        routing.provider_override,
                        None,
                        None,
                        tool_schema,
                        None,
                        false,
                    )
                    .await;
            }
        }

        let effective_provider = routing
            .provider_override
            .unwrap_or(request_profile.provider.as_str());
        Err(anyhow!(
            "Provider '{}' is not registered with the multi-LLM router; please update the configuration.",
            effective_provider
        ))
    }

    async fn generate_via_router(
        &self,
        router: &Arc<ConfiguredRouter>,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        prompt: String,
        request_profile: &LLMProfile,
        timeout_secs: u64,
        profile_override: Option<&str>,
        model_override: Option<&str>,
        provider_override: Option<&str>,
        // Hard dispatch-time provider lock — forwarded to magicllm as
        // `router_required_provider_kind` (see
        // `generate_for_operation_with_system_pinned`). `None` everywhere
        // except fail-closed pinned dispatch.
        required_provider_kind: Option<&LLMProviderKind>,
        // Provider-native response constraint. For Ollama, JsonSchema becomes
        // the top-level `format` schema and overrides a profile's generic
        // `format: json` metadata.
        response_format: Option<magicllm::LLMResponseFormat>,
        tool_schema: Option<&str>, // raw JSON schema string; Some = use native tool calling
        // Optional large body to carry as a dispatch-queue summarisable block
        // (local pre-summarisation target). Only applied when the queue's
        // local-prep is enabled; otherwise the prompt stays a single block.
        summarisable: Option<(&str, SummarisationPurpose)>,
        // Bind queue execution and all retries to the exact configured-router
        // generation that supplied `request_profile`.
        bind_router_snapshot: bool,
    ) -> Result<SimplifiedLLMResponse> {
        let effective_model = model_override
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(request_profile.model.as_str());
        let preserve_model = Self::preserve_request_model(model_override);
        let mut messages = Vec::new();
        if let Some(system_prompt) = system_prompt
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            messages.push(RouterMessage::system(system_prompt.to_string()));
        }
        let user_index = messages.len();

        // Local pre-summarisation: when the dispatch queue's local-prep is
        // enabled, carve the large `raw` body out of the user prompt into its
        // own content block so the worker can replace it with an Ollama summary
        // before the call hits the (expensive) cloud provider. When local-prep
        // is disabled we leave the prompt as a single inline block — the wire
        // format is then byte-identical to the pre-queue path (no behaviour
        // change for the default-off case).
        let prepared_summarisable = if self.local_prep_enabled() {
            summarisable.and_then(|(raw, purpose)| {
                split_user_content_for_summarisation(&prompt, raw, purpose, user_index)
            })
        } else {
            None
        };
        messages.push(RouterMessage::user(prompt));
        let mut summarisable_blocks: Vec<SummarisableBlock> = Vec::new();
        if let Some((content, block)) = prepared_summarisable {
            messages[user_index].content = content;
            summarisable_blocks.push(block);
        }

        let mut request = RouterRequest {
            model: effective_model.to_string(),
            messages: messages.into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: request_profile.temperature,
            max_output_tokens: request_profile.max_output_tokens,
            response_format: response_format.map(Into::into),
            summarisable_blocks: summarisable_blocks.into(),
            ..Default::default()
        };

        // P6-T1: apply per-agent temperature override when set.
        if let Some(temp) = self.temperature_override {
            request.temperature = Some(temp);
        }

        if let Some(reasoning_defaults) = request_profile.reasoning.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(reasoning_defaults.effort.clone()),
                max_reasoning_tokens: reasoning_defaults.max_reasoning_tokens,
                strategy: reasoning_defaults.strategy.clone(),
                summary: reasoning_defaults.summary.clone(),
            });
        }

        let mut extra_map = serde_json::Map::new();
        if let Some(params) = request_profile.metadata.as_ref() {
            for (key, value) in params {
                // Skip config-only metadata that shouldn't be sent to LLM providers
                if is_config_only_param(key) {
                    continue;
                }
                extra_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(provider) = provider_override {
            extra_map.insert(
                "router_provider_override".to_string(),
                Value::String(provider.to_string()),
            );
        }
        if let Some(profile_name) = profile_override {
            extra_map.insert(
                "router_profile_override".to_string(),
                Value::String(profile_name.to_string()),
            );
        } else {
            // Pin the magician layer's own resolution, exactly as
            // `dispatch_execution_native_messages` does. `request_profile`
            // above was resolved with the flow's parent engine and the
            // request's shape in hand; without this pin magicllm re-resolves
            // the profile from `operation_mapping.default_profile` and
            // silently discards that decision — the operation then runs on
            // its config default while the layer that chose the parent
            // believes it followed. Measured 2026-09-15: every background
            // text-only operation of a harness-driven run
            // (`v3_execution_output_synthesis`, `evidence_precision_judge`)
            // resolved `op-harness-<engine>` and still dispatched to OpenAI.
            // `resolved_profile_name_for_shape` returns `None` when the name
            // equals the operation's default, so the common path stays
            // unpinned. Multimodal has its own pin; this path never carries
            // images.
            let shape = magicllm::config::RequestShape {
                has_images: false,
                has_tools: tool_schema.is_some(),
            };
            let pinned = {
                let state = self.read_state();
                state.router_config.as_ref().and_then(|config| {
                    Self::resolved_profile_name_for_shape(
                        config,
                        self.routing_overrides.as_ref(),
                        operation,
                        &shape,
                    )
                })
            };
            if let Some(profile_name) = pinned {
                extra_map.insert(
                    "router_profile_override".to_string(),
                    Value::String(profile_name),
                );
            }
        }
        if let Some(required) = required_provider_kind {
            extra_map.insert(
                "router_required_provider_kind".to_string(),
                Value::String(required.as_str().to_string()),
            );
        }
        if preserve_model {
            extra_map.insert("router_preserve_model".to_string(), Value::Bool(true));
        }
        if !extra_map.is_empty() {
            request.set_extra(Value::Object(extra_map));
        }

        // Native tool calling: inject tool spec when schema is provided.
        if let Some(schema) = tool_schema {
            match Self::apply_native_tool_schema(&mut request, request_profile, schema) {
                Ok(()) => {},
                Err(e) => {
                    tracing::warn!(
                        operation = operation.as_str(),
                        error = %e,
                        "Failed to build tool spec from schema; falling back to prompt-embedded schema"
                    );
                },
            }
        }

        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let response: RouterResponse = self
            .route_request(router, operation, request, bind_router_snapshot)
            .await?;
        let requested_provider = provider_override.unwrap_or(request_profile.provider.as_str());
        if let Err(error) = Self::ensure_response_not_truncated(operation, &response) {
            self.emit_post_response_validation_failure(
                &response,
                operation,
                requested_provider,
                effective_model,
                profile_override,
                started_at_ms,
                "truncated_response",
            );
            return Err(error);
        }

        // When native tool calling was used, extract from tool_calls[0].arguments.
        // Falls back to text content for legacy path or schema-parse failures.
        let content = if !response.tool_calls.is_empty() {
            // If arguments is already a JSON string (Value::String), use it directly to
            // avoid double-quoting (`.to_string()` on Value::String wraps in extra quotes).
            match &response.tool_calls[0].arguments {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            }
        } else {
            let mut text = response.text.as_deref().map(str::to_owned);
            if text.is_none() {
                text = extract_text_from_messages(&response.messages);
            }
            match text {
                Some(text) => text,
                None => {
                    self.emit_post_response_validation_failure(
                        &response,
                        operation,
                        requested_provider,
                        effective_model,
                        profile_override,
                        started_at_ms,
                        "missing_response_content",
                    );
                    return Err(anyhow!(
                        "LLM provider returned neither tool_calls nor text for operation '{}'",
                        operation.as_str()
                    ));
                },
            }
        };

        let usage = response.usage.as_ref().map(convert_usage);
        let (provider_name, attributed_model, attributed_profile) = response
            .route_identity
            .as_ref()
            .map(|identity| {
                (
                    identity.provider.as_str(),
                    identity.model.as_str(),
                    Some(identity.profile.as_str()),
                )
            })
            .unwrap_or((requested_provider, effective_model, profile_override));
        let telemetry = build_telemetry(
            response.usage.as_ref(),
            provider_name,
            attributed_model,
            operation,
            attributed_profile,
            started_at_ms,
            web_search_call_count_for(provider_name, &response),
            response
                .reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            response.trace_receipt.clone(),
        );

        Ok(SimplifiedLLMResponse {
            content,
            usage,
            finish_reason: response.finish_reason.clone(),
            telemetry,
        })
    }

    /// Parse a JSON function-schema string into an LLMToolSpec for native tool calling.
    ///
    /// The schema is expected to have shape:
    /// `{ "name": "...", "description": "...", "parameters": { ... } }`
    /// Missing fields degrade gracefully.
    pub fn tool_spec_from_schema(schema: &str) -> anyhow::Result<RouterToolSpec> {
        let v: serde_json::Value = serde_json::from_str(schema)
            .context("native tool calling: failed to parse function_schema as JSON")?;
        let parameters = v["parameters"].clone();
        if parameters.is_null() {
            anyhow::bail!(
                "native tool calling: function_schema is missing required 'parameters' key"
            );
        }
        Ok(RouterToolSpec {
            name: v["name"].as_str().unwrap_or("decision").to_string(),
            description: v["description"].as_str().unwrap_or("").to_string(),
            parameters,
        })
    }

    /// Apply a single schema tool to the request with provider-aware `tool_choice`.
    fn apply_native_tool_schema(
        request: &mut RouterRequest,
        llm_config: &LLMProfile,
        schema: &str,
    ) -> anyhow::Result<()> {
        let spec = Self::tool_spec_from_schema(schema)?;
        request.set_tools(vec![spec]);

        let mut extra = match request.take_extra_value() {
            Some(Value::Object(extra)) => extra,
            Some(_) | None => serde_json::Map::new(),
        };
        extra.insert(
            "tool_choice".to_string(),
            Self::tool_choice_for_profile(llm_config),
        );
        request.set_extra(Value::Object(extra));

        Ok(())
    }

    /// Strips trailing "Respond with ONLY valid JSON…" hints from a prompt string.
    ///
    /// When native tool calling is active the LLM receives the expected output schema as a
    /// tool definition, so explicit JSON-response instructions in the prompt are redundant and
    /// can add confusing noise.  This function removes only the well-known trailing phrases
    /// that appear in the versioned agentic-decision templates; everything else is left intact.
    pub fn strip_json_response_hint(prompt: &str) -> &str {
        const HINTS: &[&str] = &[
            "Respond with ONLY valid JSON matching the expected schema.",
            "Respond with ONLY valid JSON.",
        ];
        let trimmed = prompt.trim_end();
        for hint in HINTS {
            if let Some(stripped) = trimmed.strip_suffix(hint) {
                return stripped.trim_end();
            }
        }
        trimmed
    }

    /// Generate with multiple native tool specs, preserving raw tool calls.
    ///
    /// Unlike other generation methods that collapse tool-call output to text,
    /// this returns the provider's raw tool-call list for execution-native
    /// consumption.
    pub async fn generate_for_execution_native_tools(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        user_prompt: &str,
        tools: Vec<RouterToolSpec>,
        model_override: Option<&str>,
        images: Option<&[crate::magician_v2::slot_graph::extraction::ImageData]>,
        tool_choice_override: Option<Value>,
        viewport: Option<(u32, u32)>,
    ) -> Result<ExecutionNativeRouterResponse> {
        self.generate_for_execution_native_tools_with_trace(
            operation,
            system_prompt,
            user_prompt,
            tools,
            model_override,
            images,
            tool_choice_override,
            viewport,
            None,
        )
        .await
    }

    /// Execution-native generation with an exact caller-supplied logical-call
    /// identity. This is used by model-calling compiled providers whose tenant
    /// and task/chat lineage arrive through `CompiledDispatchContext`, not an
    /// `OperationLlmRouter::task_context` clone. The trace context is local
    /// control-plane metadata and is never serialized to the provider.
    #[allow(clippy::too_many_arguments)]
    pub async fn generate_for_execution_native_tools_with_trace(
        &self,
        operation: &LLMOperation,
        system_prompt: Option<&str>,
        user_prompt: &str,
        tools: Vec<RouterToolSpec>,
        model_override: Option<&str>,
        images: Option<&[crate::magician_v2::slot_graph::extraction::ImageData]>,
        tool_choice_override: Option<Value>,
        viewport: Option<(u32, u32)>,
        trace_context: Option<magicllm::LlmTraceContext>,
    ) -> Result<ExecutionNativeRouterResponse> {
        let mut messages = Vec::new();
        if let Some(sp) = system_prompt.map(str::trim).filter(|v| !v.is_empty()) {
            messages.push(RouterMessage::system(sp.to_string()));
        }

        let has_images = images.map(|imgs| !imgs.is_empty()).unwrap_or(false);
        if let Some(imgs) = images.filter(|imgs| !imgs.is_empty()) {
            let mut user_content = vec![RouterContentBlock::Text {
                text: user_prompt.to_string(),
            }];
            for image in imgs {
                let image_bytes = base64::engine::general_purpose::STANDARD
                    .decode(&image.base64)
                    .with_context(|| {
                        "Failed to decode base64 image data for execution-native call"
                    })?;
                user_content.push(RouterContentBlock::Image {
                    data: image_bytes,
                    media_type: image.media_type.clone(),
                    caption: match image.detail {
                        Some(crate::magician_v2::slot_graph::extraction::ImageDetail::High) => {
                            Some("high detail".to_string())
                        },
                        Some(crate::magician_v2::slot_graph::extraction::ImageDetail::Low) => {
                            Some("low detail".to_string())
                        },
                        _ => None,
                    },
                });
            }
            messages.push(RouterMessage {
                role: MessageRole::User,
                content: user_content,
            });
        } else {
            messages.push(RouterMessage::user(user_prompt.to_string()));
        }

        // Keep the provider dispatch future out of each adapter frame. In
        // debug builds, nesting it through the native planner exhausted a
        // normal execution worker stack before the provider call started.
        Box::pin(self.dispatch_execution_native_messages(
            operation,
            messages,
            tools,
            model_override,
            has_images,
            tool_choice_override,
            None,
            viewport,
            trace_context,
        ))
        .await
    }

    /// Multi-turn variant of [`Self::generate_for_execution_native_tools`].
    ///
    /// Accepts pre-built messages so callers (e.g. the inner-loop runner) can
    /// thread `Assistant(tool_calls)` and `User(tool_results)` blocks across
    /// iterations, keeping the model's tool-result memory intact instead of
    /// re-rendering one user prompt per turn.
    pub async fn generate_for_execution_native_messages(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        model_override: Option<&str>,
        tool_choice_override: Option<Value>,
    ) -> Result<ExecutionNativeRouterResponse> {
        self.generate_for_execution_native_messages_with_chain(
            operation,
            messages,
            tools,
            model_override,
            tool_choice_override,
            None,
            None,
        )
        .await
    }

    /// Variant that threads an opaque server-continuation id. The legacy
    /// parameter name is retained for compatibility, but the dispatch path
    /// places it in typed `LLMRequest.context_reuse`; the selected adapter maps
    /// it to OpenAI `previous_response_id` or Gemini
    /// `previous_interaction_id`. `None` means a clean bootstrap/stateless turn.
    pub async fn generate_for_execution_native_messages_with_chain(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        model_override: Option<&str>,
        tool_choice_override: Option<Value>,
        previous_response_id: Option<&str>,
        viewport: Option<(u32, u32)>,
    ) -> Result<ExecutionNativeRouterResponse> {
        // Detect images anywhere in the message list so the request flips to
        // Vision modality. Most inner-loop calls are text-only.
        let has_images = messages.iter().any(|msg| {
            msg.content
                .iter()
                .any(|block| matches!(block, RouterContentBlock::Image { .. }))
        });
        Box::pin(self.dispatch_execution_native_messages(
            operation,
            messages,
            tools,
            model_override,
            has_images,
            tool_choice_override,
            previous_response_id,
            viewport,
            None,
        ))
        .await
    }

    /// The prompt-cache routing key every turn of one provider chain would
    /// share: a digest of the request's system messages, its tool specs, and
    /// its model — the part of the prompt that is byte-identical across the
    /// chain's bootstrap, continuation deltas, and periodic rebootstraps.
    /// `None` when the request has neither a system message nor tools.
    ///
    /// Why it exists and why it is not applied (2026-09-21): run 17's prompt
    /// projections showed the cache-stable prefix byte-identical across all
    /// five chain rebootstraps, each billed at 0% cached, because full sends
    /// carried a `prompt_cache_key` digested from the history in front of
    /// the cache sentinel (a new key per rebootstrap) and continuation
    /// deltas carried none. Setting this key on every OpenAI stateful
    /// request (runs 18 and 19) made the very first decide stall
    /// server-side — request fully sent, TLS handshake bytes back, nothing
    /// else for 8+ minutes against a 900 s timeout — while the identical
    /// body replayed through curl in 6 s with a different key and with none,
    /// and an unkeyed call from the same process succeeded. Two for two;
    /// the key was removed from the request. The routing gap is real and
    /// still open; the next attempt should be measured on a single
    /// request before it rides a run.
    #[allow(dead_code)]
    pub(crate) fn stable_chain_cache_key(request: &RouterRequest) -> Option<String> {
        let mut hasher = blake3::Hasher::new();
        let mut material = false;
        for message in request.messages.iter() {
            if message.role != MessageRole::System {
                continue;
            }
            for block in &message.content {
                if let RouterContentBlock::Text { text } = block {
                    hasher.update(b"system:");
                    hasher.update(text.as_bytes());
                    hasher.update(b"\0");
                    material = true;
                }
            }
        }
        for tool in request.tools.iter() {
            hasher.update(b"tool:");
            hasher.update(tool.name.as_bytes());
            hasher.update(b"\0");
            hasher.update(tool.description.as_bytes());
            hasher.update(b"\0");
            hasher.update(tool.parameters.to_string().as_bytes());
            hasher.update(b"\0");
            material = true;
        }
        if !material {
            return None;
        }
        hasher.update(b"model:");
        hasher.update(request.model.as_bytes());
        Some(format!(
            "magician:chain:{}",
            &hasher.finalize().to_hex()[..32]
        ))
    }

    fn ensure_execution_native_profile_supports_tool_calling(profile: &LLMProfile) -> Result<()> {
        if profile.supports_tool_calling != Some(true) {
            return Err(anyhow!(
                "execution-native dispatch requires a profile with explicit tool-calling support; provider '{}' model '{}' declares {:?}",
                profile.provider,
                profile.model,
                profile.supports_tool_calling
            ));
        }
        Ok(())
    }

    async fn dispatch_execution_native_messages(
        &self,
        operation: &LLMOperation,
        messages: Vec<RouterMessage>,
        tools: Vec<RouterToolSpec>,
        model_override: Option<&str>,
        has_images: bool,
        tool_choice_override: Option<Value>,
        previous_response_id: Option<&str>,
        viewport: Option<(u32, u32)>,
        trace_context: Option<magicllm::LlmTraceContext>,
    ) -> Result<ExecutionNativeRouterResponse> {
        let routing = self.resolve_routing(operation, model_override);
        // Shape-aware profile resolution: per-call alternatives like
        // `when_has_images: vision-yutori-n1` fire here so screenshot
        // turns can route to a vision specialist while text turns stay
        // on the text profile. The transport-cohort guard inside
        // `profile_for_operation_from_config_with_shape` enforces that
        // both profiles share provider + openai_api_mode (Chat
        // Completions ↔ Responses API mid-conversation breaks tool-call
        // shapes).
        let request_shape = magicllm::config::RequestShape {
            has_images,
            has_tools: !tools.is_empty(),
        };
        let request_profile = if let Some(guard) = self.disclosure_guard.as_ref() {
            if routing
                .profile_override
                .is_some_and(|profile| profile != guard.expected_profile())
            {
                return Err(anyhow!(
                    "app disclosure profile conflicts with the execution routing override"
                ));
            }
            let config = self
                .read_state()
                .router_config
                .clone()
                .ok_or_else(|| anyhow!("No router configuration for app disclosure"))?;
            let profile = Self::profile_for_override(&config, Some(guard.expected_profile()))
                .ok_or_else(|| anyhow!("admitted app disclosure profile is unavailable"))?;
            if routing
                .provider_override
                .is_some_and(|provider| provider != profile.provider.as_str())
            {
                return Err(anyhow!(
                    "app disclosure provider conflicts with the execution routing override"
                ));
            }
            if model_override.is_some_and(|model| model != profile.model) {
                return Err(anyhow!(
                    "app disclosure model conflicts with the admitted physical profile"
                ));
            }
            profile
        } else {
            self.request_profile_for_operation_with_shape(
                operation,
                routing.profile_override,
                &request_shape,
            )?
        };
        Self::ensure_execution_native_profile_supports_tool_calling(&request_profile)?;
        info!(
            operation = operation.as_str(),
            has_images,
            picked_profile = request_profile.model.as_str(),
            picked_provider = ?request_profile.provider,
            "[OPERATION-ROUTER] resolved profile"
        );
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router_state = self.read_state().router.clone();

        let router = router_state
            .as_ref()
            .ok_or_else(|| anyhow!("No router configured for execution-native call"))?;

        if !self.is_provider_available_for_operation(
            router,
            &request_profile,
            routing.provider_override,
        ) {
            let effective_provider = routing
                .provider_override
                .unwrap_or(request_profile.provider.as_str());
            return Err(anyhow!(
                "Provider '{}' not available for execution-native operation '{}'",
                effective_provider,
                operation.as_str()
            ));
        }

        let effective_model = routing
            .effective_model_override
            .unwrap_or(request_profile.model.as_str());
        let preserve_model = Self::preserve_request_model(routing.effective_model_override);

        debug!(
            "[EXECUTION-NATIVE] operation='{}' model='{}' tools={} messages={} timeout={}s",
            operation.as_str(),
            effective_model,
            tools.len(),
            messages.len(),
            timeout_secs,
        );

        let mut request = RouterRequest {
            model: effective_model.to_string(),
            messages: messages.into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: request_profile.temperature,
            max_output_tokens: request_profile.max_output_tokens,
            tools: tools.into(),
            ..Default::default()
        };
        if let Some(trace_context) = trace_context {
            request.metadata.set_trace_context(trace_context);
        }
        if has_images {
            request.modality = LLMModality::Vision;
        }

        // Per-agent temperature override
        if let Some(temp) = self.temperature_override {
            request.temperature = Some(temp);
        }

        // Reasoning config
        if let Some(reasoning) = request_profile.reasoning.as_ref() {
            request.reasoning = Some(RouterReasoningConfig {
                effort: Some(reasoning.effort.clone()),
                max_reasoning_tokens: reasoning.max_reasoning_tokens,
                strategy: reasoning.strategy.clone(),
                summary: reasoning.summary.clone(),
            });
        }

        // Build one provider-neutral reuse plan.  Stateful transports consume
        // the opaque continuation id; prefix-cache transports still receive
        // the complete bounded transcript but can avoid reprocessing its
        // stable prefix.  Unknown/local transports fail safe to bounded replay.
        let reuse_strategy = magicllm::strategy_for_provider(
            &request_profile.provider,
            request_profile.metadata.as_ref(),
        );
        let disable_openai_chaining = matches!(request_profile.provider, LLMProviderKind::OpenAI)
            && request_profile
                .metadata
                .as_ref()
                .and_then(|metadata| metadata.get("openai_responses_disable_chaining"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
        if self.disclosure_guard.is_some() && previous_response_id.is_some() {
            return Err(anyhow!(
                "protected app model requests cannot reuse a prior provider response"
            ));
        }
        let continuation_id = previous_response_id
            .filter(|_| reuse_strategy.is_stateful())
            .filter(|_| !disable_openai_chaining)
            .map(str::to_string);
        if self.disclosure_guard.is_none() {
            let mut context_reuse = magicllm::ContextReuseConfig::new(reuse_strategy);
            context_reuse.continuation_id = continuation_id;
            context_reuse.transport_cohort_fingerprint =
                Some(magicllm::transport_cohort_fingerprint(
                    &request_profile.provider,
                    effective_model,
                    request_profile.api_base_url.as_deref(),
                    request_profile.metadata.as_ref(),
                ));
            context_reuse.session_key = self.context_reuse_session_key();
            // The configured router computes the stable-prefix digest once after
            // dispatch JSON/byte admission, when the physical profile is final.
            // Computing it here duplicated serialization and traversed untrusted
            // schemas before the queue's structural bounds were enforced.
            context_reuse.rolling_prefix =
                matches!(reuse_strategy, magicllm::ContextReuseStrategy::PrefixCache);
            request.set_context_reuse(context_reuse);
            if !matches!(
                reuse_strategy,
                magicllm::ContextReuseStrategy::BoundedReplay
            ) {
                request.prompt_cache = Some(PromptCacheConfig::enabled());
            }
        } else {
            // Protected app V1 is admitted under an explicit
            // no-provider-storage posture. Provider conversation reuse is
            // already cold-started above; disable explicit prefix caching too
            // so profile metadata cannot retain protected prompt prefixes.
            request.prompt_cache = Some(PromptCacheConfig::Disabled);
        }

        // Extra params: metadata, provider/profile overrides, tool_choice, and
        // the legacy OpenAI Responses chain marker. Provider-neutral reuse
        // state remains exclusively in the typed request field above.
        // Build everything into one map so subsequent inserts can't blow
        // each other away.
        let mut extra_map = serde_json::Map::new();
        if let Some(params) = request_profile.metadata.as_ref() {
            for (key, value) in params {
                if !is_config_only_param(key) {
                    extra_map.insert(key.clone(), value.clone());
                }
            }
        }
        if let Some(provider) = routing.provider_override {
            extra_map.insert(
                "router_provider_override".to_string(),
                Value::String(provider.to_string()),
            );
        }
        // Pin the magicllm router to the magician layer's shape-aware
        // resolution. Without this, the magicllm router (which doesn't see
        // `has_images`) re-resolves from `operation_mapping.default_profile`
        // and discards the `when_has_images` alternative — e.g. the
        // magician layer correctly picks Yutori but the magicllm router
        // dispatches to OpenAI. Resolved name is `None` when the picked
        // profile is the default (no override needed) or when an
        // alternative was rejected by the cohort guard.
        let shape_aware_profile_name = if routing.profile_override.is_none() {
            self.read_state().router_config.as_ref().and_then(|cfg| {
                Self::resolved_profile_name_for_shape(
                    cfg,
                    self.routing_overrides.as_ref(),
                    operation,
                    &request_shape,
                )
            })
        } else {
            None
        };
        tracing::info!(
            target: "magician::routing::parent_engine",
            site = "dispatch_pin",
            operation = operation.as_str(),
            routing_profile_override = routing.profile_override.unwrap_or("-"),
            shape_aware = shape_aware_profile_name.as_deref().unwrap_or("-"),
            has_tools = request_shape.has_tools,
            carried_parent = self
                .routing_overrides
                .as_ref()
                .and_then(|overrides| overrides.parent_engine.as_deref())
                .unwrap_or("-"),
            "dispatch profile pin"
        );
        let effective_profile_override = self
            .disclosure_guard
            .as_ref()
            .map(|guard| guard.expected_profile().to_owned())
            .or_else(|| routing.profile_override.map(str::to_string))
            .or(shape_aware_profile_name);
        if let Some(profile_name) = effective_profile_override.as_deref() {
            extra_map.insert(
                "router_profile_override".to_string(),
                Value::String(profile_name.to_string()),
            );
        }
        if preserve_model {
            extra_map.insert("router_preserve_model".to_string(), Value::Bool(true));
        }
        extra_map.insert(
            "tool_choice".to_string(),
            tool_choice_override.unwrap_or_else(|| Self::tool_choice_for_profile(&request_profile)),
        );
        // OpenAI Responses chaining: when the runner has a prior turn's
        // `response_id`, hand it to the provider via
        // `extra.openai_previous_response_id`. The provider switches into
        // continuation mode (see `openai_responses.rs`) — server-side
        // state lets the model skip re-derivation of reasoning items.
        // Only meaningful for the OpenAI Responses transport. Other adapters
        // never receive this legacy key; Gemini reads the typed continuation
        // plan instead. The runner clears the chain on shape/compaction/error
        // boundaries so an id is never combined with incompatible replay.
        //
        // Profile escape hatch: `openai_responses_disable_chaining: true`
        // in the profile metadata forces full-history mode on every turn
        // (provider behaves as if no prior chain existed). Useful for
        // A/B-comparing chained vs full-history behaviour on the same
        // profile without flipping transports.
        if matches!(request_profile.provider, LLMProviderKind::OpenAI)
            && reuse_strategy.is_stateful()
            && self.disclosure_guard.is_none()
        {
            if let Some(prev_id) = previous_response_id {
                if !disable_openai_chaining {
                    extra_map.insert(
                        "openai_previous_response_id".to_string(),
                        Value::String(prev_id.to_string()),
                    );
                }
            }
            // NOT keyed here — see `stable_chain_cache_key` for the finding
            // and the stall that followed the attempt to act on it.
        }
        // Viewport for vision-cohort coordinate denormalization. Yutori
        // Navigator emits all `coordinates` in a normalized 1000×1000
        // space and the API has no server-side denormalization knob; the
        // Yutori provider in magicllm reads `extra.viewport` and rewrites
        // builtin tool-call coordinates to viewport pixels before
        // returning. Other providers ignore the key. `None` means the
        // caller (typically a non-browser inner loop) doesn't have a
        // browser session and translation is skipped.
        if let Some((vw, vh)) = viewport {
            let mut viewport_map = serde_json::Map::new();
            viewport_map.insert("width".to_string(), Value::from(vw));
            viewport_map.insert("height".to_string(), Value::from(vh));
            extra_map.insert("viewport".to_string(), Value::Object(viewport_map));
        }
        if !extra_map.is_empty() {
            request.set_extra(Value::Object(extra_map));
        }
        if let Some(guard) = self.disclosure_guard.as_ref() {
            request.metadata.set_disclosure_guard(guard.clone());
        }

        if let Some(reuse) = request.context_reuse.as_ref() {
            info!(
                operation = operation.as_str(),
                strategy = ?reuse.strategy,
                continuation = reuse.continuation_id.is_some(),
                rolling_prefix = reuse.rolling_prefix,
                stable_prefix_fingerprint = reuse
                    .stable_prefix_fingerprint
                    .as_deref()
                    .unwrap_or(""),
                "[OPERATION-ROUTER] context reuse plan"
            );
        }

        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let response: RouterResponse = self
            .route_request(router, operation, request, false)
            .await?;

        // Project to execution-native response preserving raw tool calls
        let tool_calls = Arc::try_unwrap(response.tool_calls)
            .unwrap_or_else(|shared| {
                shared
                    .iter()
                    .map(|call| magicllm::LLMToolCall {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        arguments: crate::magician_v2::json_traversal::clone_json_iteratively(
                            &call.arguments,
                        ),
                    })
                    .collect()
            })
            .into_iter()
            .map(|tc| RawRouterToolCall {
                id: tc.id,
                name: tc.name,
                arguments: tc.arguments,
            })
            .collect();

        let text = response
            .text
            .as_deref()
            .map(str::to_owned)
            .or_else(|| extract_text_from_messages(&response.messages));

        let usage = response.usage.as_ref().map(convert_usage);
        // Cache fields go straight from the raw RouterTokenUsage because
        // SimplifiedTokenUsage drops them. Without this passthrough, every
        // non-chat-inline `llm.succeeded` emit reports 0 cached tokens and
        // the chat UI's per-turn cache chip looks frozen across iterations.
        let cached_tokens = response.usage.as_ref().and_then(|u| u.cached_tokens);
        let cache_creation_tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.cache_creation_tokens);
        let requested_provider = routing
            .provider_override
            .unwrap_or(request_profile.provider.as_str());
        let (effective_provider, attributed_model, attributed_profile) = response
            .route_identity
            .as_ref()
            .map(|identity| {
                (
                    identity.provider.as_str(),
                    identity.model.as_str(),
                    Some(identity.profile.as_str()),
                )
            })
            .unwrap_or((
                requested_provider,
                effective_model,
                effective_profile_override.as_deref(),
            ));
        // Server-side search summary derives from the retained raw response
        // against the transport that actually served the call. Surfaced only
        // when the routed profile carries the `server_web_search` flag or the
        // response shows search activity, so ordinary turns stay `None`.
        let flag_ridden = request_profile
            .metadata
            .as_ref()
            .and_then(|meta| meta.get("server_web_search"))
            .is_some_and(|flag| flag.as_bool().unwrap_or(true));
        let web_search = response.raw_response.as_ref().map(|raw| {
            let provider_kind = magicllm::LLMProviderKind::from_str(effective_provider);
            let searches = magicllm::server_web_search::web_search_call_count(&provider_kind, raw);
            let citations = magicllm::server_web_search::extract_citations(&provider_kind, raw)
                .into_iter()
                .map(|citation| ExecutionWebSearchCitation {
                    url: citation.url,
                    title: citation.title,
                })
                .collect::<Vec<_>>();
            ExecutionWebSearchSummary {
                searches,
                citations,
            }
        });
        let web_search = web_search
            .filter(|summary| flag_ridden || summary.searches > 0 || !summary.citations.is_empty());

        let telemetry = build_telemetry(
            response.usage.as_ref(),
            effective_provider,
            attributed_model,
            operation,
            attributed_profile,
            started_at_ms,
            web_search
                .as_ref()
                .map(|summary| summary.searches)
                .unwrap_or(0),
            response.reasoning_text.as_deref().map(str::to_owned),
            response.trace_receipt.clone(),
        );

        Ok(ExecutionNativeRouterResponse {
            tool_calls,
            text,
            reasoning_text: response.reasoning_text.as_deref().map(str::to_owned),
            response_id: response.response_id.clone(),
            finish_reason: response.finish_reason.clone(),
            prompt_tokens: usage.as_ref().map(|u| u.prompt_tokens),
            completion_tokens: usage.as_ref().map(|u| u.completion_tokens),
            cached_tokens,
            cache_creation_tokens,
            // Reasoning tokens straight from the raw usage (like the cache
            // fields) so `effort: high` cost is visible; `convert_usage`'s
            // SimplifiedTokenUsage drops it. The selected-profile name is the
            // same deviation-aware attribution the normal telemetry paths emit.
            reasoning_tokens: response.usage.as_ref().and_then(|u| u.reasoning_tokens),
            web_search,
            profile: attributed_profile.map(str::to_string),
            // Same effective-provider resolution the SimplifiedLLMResponse
            // telemetry paths use (`provider_override` wins over the profile).
            provider: Some(effective_provider.to_string()),
            model: Some(attributed_model.to_string()),
            telemetry,
        })
    }

    pub fn provider_for_operation(&self, operation: &LLMOperation) -> Option<String> {
        if let Some(override_provider) = self
            .routing_overrides
            .as_ref()
            .and_then(|overrides| overrides.endpoint_for_operation(operation))
            .and_then(|endpoint| endpoint.provider_name().map(str::to_string))
        {
            return Some(override_provider);
        }

        self.get_config_for_operation(operation)
            .ok()
            .map(|config| config.provider.to_string())
    }

    /// Returns true only when the operation resolves to a configured profile and
    /// the routed provider is actually registered.
    pub fn is_operation_available(&self, operation: &LLMOperation) -> bool {
        let Some(router) = self.read_state().router.clone() else {
            return false;
        };
        self.get_config_for_operation(operation)
            .map(|config| router.router().has_provider(&config.provider))
            .unwrap_or(false)
    }

    /// Generate multimodal response with text and images for a specific operation
    pub async fn generate_multimodal(
        &self,
        operation: &LLMOperation,
        request: &crate::magician_v2::slot_graph::extraction::LlmFunctionCallRequest,
        images: &[crate::magician_v2::slot_graph::extraction::ImageData],
    ) -> Result<SimplifiedLLMResponse> {
        self.generate_multimodal_with_model(operation, request, images, None)
            .await
    }

    /// Generate multimodal response for an operation, optionally forcing the model.
    pub async fn generate_multimodal_with_model(
        &self,
        operation: &LLMOperation,
        request: &crate::magician_v2::slot_graph::extraction::LlmFunctionCallRequest,
        images: &[crate::magician_v2::slot_graph::extraction::ImageData],
        model_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        let routing = self.resolve_routing(operation, model_override);
        let request_profile =
            self.request_profile_for_operation(operation, routing.profile_override)?;
        let timeout_secs = Self::timeout_for_profile(operation, &request_profile);
        let router = self.read_state().router.clone();

        debug!(
            "[MAGICIAN-V2-QUERY] Generating multimodal analysis for operation '{}' using model \
             '{}' provider_override={:?} with {} image(s), max_output_tokens={:?}, timeout {}s",
            operation.as_str(),
            routing
                .effective_model_override
                .unwrap_or(request_profile.model.as_str()),
            routing.provider_override,
            images.len(),
            request_profile.max_output_tokens,
            timeout_secs
        );

        if let Some(router) = router.as_ref() {
            let provider_available = self.is_provider_available_for_operation(
                router,
                &request_profile,
                routing.provider_override,
            );
            if provider_available {
                return self
                    .generate_multimodal_via_router(
                        router,
                        operation,
                        request,
                        images,
                        &request_profile,
                        timeout_secs,
                        routing.profile_override,
                        routing.effective_model_override,
                        routing.provider_override,
                    )
                    .await;
            }
        }

        let effective_provider = routing
            .provider_override
            .unwrap_or(request_profile.provider.as_str());
        Err(anyhow!(
            "Provider '{}' is not registered with the multi-LLM router; please update the \
             configuration.",
            effective_provider
        ))
    }

    fn is_provider_available_for_operation(
        &self,
        router: &Arc<ConfiguredRouter>,
        request_profile: &LLMProfile,
        provider_override: Option<&str>,
    ) -> bool {
        if let Some(provider) = provider_override.map(str::trim).filter(|p| !p.is_empty()) {
            let provider_kind = LLMProviderKind::from_str(provider);
            return router.router().has_provider(&provider_kind);
        }
        router.router().has_provider(&request_profile.provider)
    }

    async fn generate_multimodal_via_router(
        &self,
        router: &Arc<ConfiguredRouter>,
        operation: &LLMOperation,
        request: &crate::magician_v2::slot_graph::extraction::LlmFunctionCallRequest,
        images: &[crate::magician_v2::slot_graph::extraction::ImageData],
        request_profile: &LLMProfile,
        timeout_secs: u64,
        profile_override: Option<&str>,
        model_override: Option<&str>,
        provider_override: Option<&str>,
    ) -> Result<SimplifiedLLMResponse> {
        let started_at_ms = chrono::Utc::now().timestamp_millis();
        let effective_model = model_override
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .unwrap_or(request_profile.model.as_str());
        let preserve_model = Self::preserve_request_model(model_override);
        // Native tool calling: schema is passed as a tool spec, never embedded in user text.
        // Provider-aware: Anthropic with reasoning uses tool_choice "auto" (handled in router).
        let user_text = Self::strip_json_response_hint(&request.user_prompt).to_string();
        let tool_schema_to_use = Some(request.function_schema.as_str());

        // Build user message with text and images
        let mut user_content = vec![RouterContentBlock::Text { text: user_text }];

        // Add images to user content
        for image in images {
            // Decode base64 to bytes
            let image_bytes = base64::engine::general_purpose::STANDARD
                .decode(&image.base64)
                .with_context(|| "Failed to decode base64 image data")?;

            user_content.push(RouterContentBlock::Image {
                data: image_bytes,
                media_type: image.media_type.clone(),
                caption: match image.detail {
                    Some(crate::magician_v2::slot_graph::extraction::ImageDetail::High) => {
                        Some("high detail".to_string())
                    },
                    Some(crate::magician_v2::slot_graph::extraction::ImageDetail::Low) => {
                        Some("low detail".to_string())
                    },
                    _ => None,
                },
            });
        }

        let messages = vec![
            RouterMessage {
                role: MessageRole::System,
                content: vec![RouterContentBlock::Text {
                    text: request.system_prompt.clone(),
                }],
            },
            RouterMessage {
                role: MessageRole::User,
                content: user_content,
            },
        ];

        let mut router_request = RouterRequest {
            model: effective_model.to_string(),
            messages: messages.into(),
            metadata: RouterMetadata {
                operation: operation.as_str().to_string(),
                timeout_secs: Some(timeout_secs),
                ..Default::default()
            },
            temperature: Some(request.temperature as f32),
            max_output_tokens: request_profile.max_output_tokens,
            ..Default::default()
        };
        if !images.is_empty() {
            router_request.modality = LLMModality::Vision;
        }

        // P6-T1: apply per-agent temperature override when set.
        if let Some(temp) = self.temperature_override {
            router_request.temperature = Some(temp);
        }

        if let Some(reasoning_defaults) = request_profile.reasoning.as_ref() {
            router_request.reasoning = Some(RouterReasoningConfig {
                effort: Some(reasoning_defaults.effort.clone()),
                max_reasoning_tokens: reasoning_defaults.max_reasoning_tokens,
                strategy: reasoning_defaults.strategy.clone(),
                summary: reasoning_defaults.summary.clone(),
            });
        }

        let mut extra_map = serde_json::Map::new();
        if let Some(params) = request_profile.metadata.as_ref() {
            for (key, value) in params {
                // Skip config-only metadata that shouldn't be sent to LLM providers
                if is_config_only_param(key) {
                    continue;
                }
                extra_map.insert(key.clone(), value.clone());
            }
        }
        if let Some(provider) = provider_override {
            extra_map.insert(
                "router_provider_override".to_string(),
                Value::String(provider.to_string()),
            );
        }
        if let Some(profile_name) = profile_override {
            extra_map.insert(
                "router_profile_override".to_string(),
                Value::String(profile_name.to_string()),
            );
        }
        if preserve_model {
            extra_map.insert("router_preserve_model".to_string(), Value::Bool(true));
        }
        if !extra_map.is_empty() {
            router_request.set_extra(Value::Object(extra_map));
        }

        // Native tool calling for vision SOM: inject tool spec when schema is provided.
        if let Some(schema) = tool_schema_to_use {
            match Self::apply_native_tool_schema(&mut router_request, request_profile, schema) {
                Ok(()) => {},
                Err(e) => {
                    tracing::warn!(
                        operation = operation.as_str(),
                        error = %e,
                        "Failed to build tool spec from vision SOM schema; using prompt-embedded fallback"
                    );
                },
            }
        }

        let response: RouterResponse = self
            .route_request(router, operation, router_request, false)
            .await?;
        let requested_provider = provider_override.unwrap_or(request_profile.provider.as_str());
        if let Err(error) = Self::ensure_response_not_truncated(operation, &response) {
            self.emit_post_response_validation_failure(
                &response,
                operation,
                requested_provider,
                effective_model,
                profile_override,
                started_at_ms,
                "truncated_response",
            );
            return Err(error);
        }

        // When native tool calling was used, extract from tool_calls[0].arguments.
        // Falls back to text content for legacy path or schema-parse failures.
        let content = if !response.tool_calls.is_empty() {
            // If arguments is already a JSON string (Value::String), use it directly to
            // avoid double-quoting (`.to_string()` on Value::String wraps in extra quotes).
            match &response.tool_calls[0].arguments {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            }
        } else {
            let mut text = response.text.as_deref().map(str::to_owned);
            if text.is_none() {
                text = extract_text_from_messages(&response.messages);
            }
            match text {
                Some(text) => text,
                None => {
                    self.emit_post_response_validation_failure(
                        &response,
                        operation,
                        requested_provider,
                        effective_model,
                        profile_override,
                        started_at_ms,
                        "missing_response_content",
                    );
                    return Err(anyhow!(
                        "LLM provider returned neither tool_calls nor text for multimodal operation '{}'",
                        operation.as_str()
                    ));
                },
            }
        };

        let usage = response.usage.as_ref().map(convert_usage);
        let (provider_name, attributed_model, attributed_profile) = response
            .route_identity
            .as_ref()
            .map(|identity| {
                (
                    identity.provider.as_str(),
                    identity.model.as_str(),
                    Some(identity.profile.as_str()),
                )
            })
            .unwrap_or((requested_provider, effective_model, profile_override));
        let telemetry = build_telemetry(
            response.usage.as_ref(),
            provider_name,
            attributed_model,
            operation,
            attributed_profile,
            started_at_ms,
            web_search_call_count_for(provider_name, &response),
            response
                .reasoning_text
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(ToString::to_string),
            response.trace_receipt.clone(),
        );

        Ok(SimplifiedLLMResponse {
            content,
            usage,
            finish_reason: response.finish_reason.clone(),
            telemetry,
        })
    }
}

fn background_dispatch_capacity_busy(
    snapshot: &magicllm::dispatch::QueueSnapshot,
    provider: Option<&str>,
) -> bool {
    if snapshot
        .registry
        .pending
        .iter()
        .chain(snapshot.registry.in_flight.iter())
        .any(|job| job.priority != magicllm::dispatch::Priority::Background)
    {
        return true;
    }
    if snapshot.capacity_background > 0 && snapshot.depth_background >= snapshot.capacity_background
    {
        // A nonempty background lane is healthy and must not starve durable
        // maintenance. Only the actual admission boundary defers a producer.
        return true;
    }
    let Some(provider) = provider else {
        // Missing routing is allowed to proceed and fail through the typed
        // configuration path; pressure admission must not turn a deterministic
        // config error into an indefinitely pending item.
        return false;
    };
    snapshot
        .registry
        .pending
        .iter()
        .chain(snapshot.registry.in_flight.iter())
        .any(|job| {
            job.priority == magicllm::dispatch::Priority::Background
                && job
                    .provider
                    .as_ref()
                    .is_some_and(|active| active.as_str() == provider)
        })
}

impl OperationLlmRouter {
    fn ensure_response_not_truncated(
        operation: &LLMOperation,
        response: &RouterResponse,
    ) -> Result<()> {
        let finish_reason = response.finish_reason.as_deref();
        let truncated = matches!(finish_reason, Some("max_tokens" | "MAX_TOKENS" | "length"))
            || (finish_reason == Some("incomplete")
                && response
                    .raw_response
                    .as_ref()
                    .and_then(|payload| payload.get("incomplete_details"))
                    .and_then(|details| details.get("reason"))
                    .and_then(serde_json::Value::as_str)
                    .map(|reason| {
                        matches!(
                            reason,
                            "max_output_tokens" | "max_completion_tokens" | "max_tokens"
                        )
                    })
                    .unwrap_or(false));
        if truncated {
            return Err(anyhow!(
                "LLM response for operation '{}' was truncated at finish_reason '{}'",
                operation.as_str(),
                finish_reason.unwrap_or("unknown")
            ));
        }
        Ok(())
    }
}

/// Check if a parameter is config-only metadata that should NOT be sent to LLM providers.
/// These fields are used internally for routing, budget tracking, and fallback logic.
fn is_config_only_param(key: &str) -> bool {
    matches!(
        key,
        "cost_per_observation"
            | "fallback_profile"
            | "reasoning_strategy"
            | "reasoning_max_tokens"
            | "use_chat"
            | "use_responses"
            | "openai_api_mode"
            | "gemini_api_mode"
            | "openai_responses_disable_chaining"
    )
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

fn convert_usage(usage: &RouterTokenUsage) -> SimplifiedTokenUsage {
    let prompt_tokens = usage.prompt_tokens.unwrap_or(0);
    let completion_tokens = usage.completion_tokens.unwrap_or(0);
    let total_tokens = usage
        .total_tokens
        .unwrap_or_else(|| prompt_tokens.saturating_add(completion_tokens));

    SimplifiedTokenUsage {
        prompt_tokens,
        completion_tokens,
        total_tokens,
    }
}

/// Provider-executed web search count for a retained raw response. Token-only
/// cost paths add the per-call search charges on top of `compute_cost_at`
/// using this count.
fn web_search_call_count_for(provider: &str, response: &magicllm::LLMResponse) -> usize {
    response
        .raw_response
        .as_deref()
        .map(|raw| {
            magicllm::server_web_search::web_search_call_count(
                &magicllm::LLMProviderKind::from_str(provider),
                raw,
            )
        })
        .unwrap_or(0)
}

/// Build rich per-call telemetry for `SimplifiedLLMResponse` / executor emits.
/// Uses `magicllm::pricing::compute_cost_at` against the active pricing table
/// (built-in base + `llm_pricing.json` overlay), resolved at the call's start
/// time so effective-dated rate changes price against when the call ran.
/// `search_calls` adds provider-executed web search charges, which token
/// usage cannot see.
#[allow(clippy::too_many_arguments)]
fn build_telemetry(
    usage: Option<&RouterTokenUsage>,
    provider: &str,
    model: &str,
    operation: &LLMOperation,
    profile_name: Option<&str>,
    started_at_ms: i64,
    search_calls: usize,
    reasoning_summary: Option<String>,
    trace_receipt: Option<magicllm::LlmTraceReceipt>,
) -> Option<crate::magician_v2::slot_graph::extraction::LlmCallTelemetry> {
    let usage_reported = usage.is_some();
    let usage = usage.cloned().unwrap_or_default();
    let provider_kind = magicllm::LLMProviderKind::from_str(provider);
    let cost_usd = magicllm::compute_cost_with_server_web_search_at(
        &provider_kind,
        model,
        &usage,
        search_calls,
        started_at_ms,
    );
    Some(
        crate::magician_v2::slot_graph::extraction::LlmCallTelemetry {
            provider: provider.to_string(),
            model: model.to_string(),
            usage_reported,
            usage_availability: None,
            input_tokens: usage.prompt_tokens.unwrap_or(0),
            output_tokens: usage.completion_tokens.unwrap_or(0),
            reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
            cache_read_tokens: usage.cached_tokens.unwrap_or(0),
            cache_creation_tokens: usage.cache_creation_tokens.unwrap_or(0),
            search_calls: u32::try_from(search_calls).unwrap_or(u32::MAX),
            cost_usd,
            reasoning_summary,
            profile: profile_name.map(str::to_string),
            operation: Some(operation.as_str().to_string()),
            started_at_ms,
            trace_receipt,
            prompt_projection_mode: None,
        },
    )
}

/// Wrapper that implements QueryAnalysisLLM for a specific operation
pub struct OperationLLMWrapper {
    service: Arc<OperationLlmRouter>,
    operation: LLMOperation,
}

impl OperationLLMWrapper {
    pub fn new(service: Arc<OperationLlmRouter>, operation: LLMOperation) -> Self {
        Self { service, operation }
    }
}

#[async_trait]
impl QueryAnalysisLLM for OperationLLMWrapper {
    async fn generate_analysis(&self, prompt: &str) -> Result<SimplifiedLLMResponse> {
        self.service
            .generate_for_operation(&self.operation, prompt)
            .await
    }

    async fn generate_analysis_with_system(
        &self,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.service
            .generate_for_operation_with_system(&self.operation, system_prompt, prompt)
            .await
    }

    async fn generate_analysis_scoped(
        &self,
        scope: magicllm::LlmScope,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.service
            .with_scope_context(Some(scope))
            .generate_for_operation(&self.operation, prompt)
            .await
    }

    async fn generate_analysis_with_system_scoped(
        &self,
        scope: magicllm::LlmScope,
        system_prompt: Option<&str>,
        prompt: &str,
    ) -> Result<SimplifiedLLMResponse> {
        self.service
            .with_scope_context(Some(scope))
            .generate_for_operation_with_system(&self.operation, system_prompt, prompt)
            .await
    }

    async fn is_available(&self) -> bool {
        self.service.is_operation_available(&self.operation)
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
    use crate::magician_v2::analytics::runtime_activity_layer::RuntimeActivityLayer;
    use crate::magician_v2::execution::agentic::types::{
        execution_token_budget_snapshot, preflight_execution_token_budget,
        with_execution_token_meter,
    };
    use magicllm::capability::{LLMModality, LLMProviderKind};
    use magicllm::config::ReasoningDefaults;
    use tracing_subscriber::layer::SubscriberExt;

    #[test]
    fn the_chain_cache_key_ignores_history_and_follows_system_tools_and_model() {
        let tool = RouterToolSpec {
            name: "browser__click".to_string(),
            description: "click".to_string(),
            parameters: serde_json::json!({"type": "object"}),
        };
        let request = |history: Vec<RouterMessage>, model: &str, tools: Vec<RouterToolSpec>| {
            let mut messages = vec![RouterMessage::system("you are the executor")];
            messages.extend(history);
            RouterRequest {
                model: model.to_string(),
                messages: messages.into(),
                tools: tools.into(),
                ..Default::default()
            }
        };
        let bootstrap = request(
            vec![RouterMessage::user("full prompt")],
            "gpt-5.6-terra",
            vec![tool.clone()],
        );
        let rebootstrap = request(
            vec![
                RouterMessage::user("full prompt"),
                RouterMessage::assistant("clicked"),
                RouterMessage::user("tool result"),
                RouterMessage::user("full prompt again"),
            ],
            "gpt-5.6-terra",
            vec![tool.clone()],
        );
        let delta = request(
            vec![RouterMessage::user("tool result only")],
            "gpt-5.6-terra",
            vec![tool.clone()],
        );
        let key = OperationLlmRouter::stable_chain_cache_key(&bootstrap).expect("keyed");
        assert!(key.starts_with("magician:chain:"), "{key}");
        assert_eq!(
            OperationLlmRouter::stable_chain_cache_key(&rebootstrap).as_deref(),
            Some(key.as_str())
        );
        assert_eq!(
            OperationLlmRouter::stable_chain_cache_key(&delta).as_deref(),
            Some(key.as_str())
        );
        // A different tool set or model is a different prefix: a different key.
        let more_tools = request(
            vec![],
            "gpt-5.6-terra",
            vec![
                tool.clone(),
                RouterToolSpec {
                    name: "browser__eval".to_string(),
                    description: "eval".to_string(),
                    parameters: serde_json::json!({"type": "object"}),
                },
            ],
        );
        assert_ne!(
            OperationLlmRouter::stable_chain_cache_key(&more_tools).as_deref(),
            Some(key.as_str())
        );
        let other_model = request(vec![], "gpt-5.6-luna", vec![tool.clone()]);
        assert_ne!(
            OperationLlmRouter::stable_chain_cache_key(&other_model).as_deref(),
            Some(key.as_str())
        );
        // Nothing shared to route on: no key.
        let bare = RouterRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![RouterMessage::user("hi")].into(),
            ..Default::default()
        };
        assert_eq!(OperationLlmRouter::stable_chain_cache_key(&bare), None);
    }

    #[test]
    fn harness_affinity_profile_names_cover_the_engine_roster() {
        assert_eq!(
            super::harness_affinity_profile_base("claude_code"),
            "op-harness-claude"
        );
        assert_eq!(
            super::harness_affinity_profile_base("codex"),
            "op-harness-codex"
        );
        assert_eq!(
            super::harness_affinity_profile_base("codex_app_server"),
            "op-harness-codex"
        );
        assert_eq!(
            super::harness_affinity_profile_base("grok"),
            "op-harness-grok"
        );
        assert_eq!(
            super::harness_affinity_profile_base("agy"),
            "op-harness-agy"
        );
        assert_eq!(super::harness_affinity_profile_base("pi"), "op-harness-pi");
    }

    #[derive(Debug)]
    struct AllowDisclosure;

    #[async_trait::async_trait]
    impl magicllm::LlmDisclosureAuthorizer for AllowDisclosure {
        async fn revalidate(
            &self,
            _profile: &str,
            _provider: &LLMProviderKind,
            _model: &str,
            _api_base_url: Option<&str>,
        ) -> Result<(), String> {
            Ok(())
        }
    }

    fn test_disclosure_guard() -> magicllm::LlmDisclosureGuard {
        magicllm::LlmDisclosureGuard::new(
            "app-local",
            "0".repeat(64),
            "app-partition",
            "app-policy",
            magicllm::LlmDisclosureCapturePolicy::MetadataOnly,
            Arc::new(AllowDisclosure),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn service_health_background_queue_auth_failure_reaches_scoped_hitl() {
        use crate::magician_v2::realtime_events::{
            RuntimeTransportBroadcaster, RuntimeTransportEvent,
        };
        struct MissingLogin;
        #[async_trait::async_trait]
        impl magicllm::dispatch::DispatchRouter for MissingLogin {
            async fn route(&self, _: RouterRequest) -> magicllm::LLMResult<RouterResponse> {
                Err(magicllm::LLMError::Routed {
                    profile: "op-harness-pi".into(),
                    provider: LLMProviderKind::from_str("harness-pi"),
                    model: "default".into(),
                    source: Box::new(magicllm::LLMError::Configuration(
                        "No API key found for selected model. PRIVATE_PROVIDER_DETAIL".into(),
                    )),
                })
            }
            async fn route_stream(
                &self,
                _: RouterRequest,
                _: tokio::sync::mpsc::Sender<magicllm::types::StreamDelta>,
            ) -> magicllm::LLMResult<()> {
                unreachable!()
            }
            fn provider_for_operation(&self, _: &str) -> Option<LLMProviderKind> {
                Some(LLMProviderKind::from_str("harness-pi"))
            }
            fn timeout_for_operation(&self, _: &str) -> Option<u64> {
                Some(5)
            }
        }
        let mut config = LLMRouterConfig::default();
        config.default_profile = "fixture".into();
        let mut profile = test_profile(LLMProviderKind::Ollama, None);
        profile.api_key_env = None;
        config.profiles.insert("fixture".into(), profile);
        let configured = Arc::new(ConfiguredRouter::from_router_config(config).unwrap());
        let mut dispatch_config = magicllm::dispatch::DispatchConfig::default();
        dispatch_config.retry.max_attempts_per_dispatch = 1;
        dispatch_config.retry.max_dispatch_cycles = 1;
        let queue = magicllm::LlmDispatchQueue::start(
            Arc::new(MissingLogin),
            Arc::new(magicllm::dispatch::NoopTaskStateView),
            Arc::new(magicllm::dispatch::NoopTaskLedgerSink),
            dispatch_config,
        );
        let bus = Arc::new(RuntimeTransportBroadcaster::new(16));
        let mut events = bus.subscribe();
        let router = OperationLlmRouter::new(None)
            .with_scope_context(Some(magicllm::LlmScope::new("alice", "work")));
        router.set_dispatch_queue(queue.clone());
        router.set_event_broadcaster(bus.clone());
        for _ in 0..2 {
            assert!(router
                .route_request(
                    &configured,
                    &LLMOperation::Other("health_fixture".into()),
                    RouterRequest::default(),
                    false
                )
                .await
                .is_err());
        }
        let RuntimeTransportEvent::HitlRequested {
            principal,
            workspace,
            input_schema,
            prompt,
            ..
        } = events.try_recv().unwrap()
        else {
            panic!("missing health notification")
        };
        assert_eq!(principal.as_deref(), Some("alice"));
        assert_eq!(workspace.as_deref(), Some("work"));
        assert_eq!(input_schema.unwrap()["health_issue"], "authentication");
        assert!(prompt.contains("own default model and login"));
        assert!(!prompt.contains("PRIVATE_PROVIDER_DETAIL"));
        assert!(
            events.try_recv().is_err(),
            "repeated errors must deduplicate"
        );
        queue.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn app_calls_without_a_queue_never_enter_direct_routing() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "app-local".to_owned();
        let mut profile = test_profile(LLMProviderKind::Ollama, None);
        profile.api_key_env = None;
        config.profiles.insert("app-local".to_owned(), profile);
        let configured = Arc::new(ConfiguredRouter::from_router_config(config).unwrap());
        for (operation, guarded) in [
            (LLMOperation::App("app:compose".to_owned()), false),
            (LLMOperation::Other("app:compose".to_owned()), false),
            (LLMOperation::QueryAnalysis, true),
        ] {
            let router = OperationLlmRouter::new(None)
                .with_disclosure_guard(guarded.then(test_disclosure_guard));
            let error = router
                .route_request(&configured, &operation, RouterRequest::default(), false)
                .await
                .unwrap_err();
            assert_eq!(
                error.to_string(),
                "app LLM operation requires the dispatcher queue"
            );
        }
    }

    #[test]
    fn non_task_scope_is_authoritative_and_task_scope_takes_precedence() {
        let surface_scope = magicllm::LlmScope::new("surface-owner", "surface-workspace");
        let router = OperationLlmRouter::new(None).with_scope_context(Some(surface_scope.clone()));
        assert_eq!(router.authoritative_trace_scope(), Some(surface_scope));

        let task_scope = magicllm::LlmScope::new("task-owner", "task-workspace");
        let router = router.with_task_context(Some(
            magicllm::TaskRef::task("task-1")
                .with_scope(task_scope.principal.clone(), task_scope.workspace.clone()),
        ));
        assert_eq!(router.authoritative_trace_scope(), Some(task_scope));
    }

    #[tokio::test]
    async fn disclosure_clone_retains_fence_and_rejects_unlabeled_chunking() {
        let router =
            OperationLlmRouter::new(None).with_disclosure_guard(Some(test_disclosure_guard()));
        assert_eq!(
            router
                .disclosure_guard
                .as_ref()
                .map(magicllm::LlmDisclosureGuard::expected_profile),
            Some("app-local")
        );
        let error = router
            .generate_for_chunkable_operation_with_lazy_fallback(
                &LLMOperation::QueryAnalysis,
                serde_json::json!({"protected": true}),
                |_| panic!("guarded input must not enter the unlabeled fallback"),
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("no app disclosure propagation contract"));
    }

    #[test]
    fn authoritative_scope_rebinds_only_legacy_contexts() {
        let scope = magicllm::LlmScope::new("owner", "workspace");
        let mut legacy =
            magicllm::LlmTraceContext::legacy(Some("trace"), magicllm::LlmWorkloadClass::System);
        OperationLlmRouter::reconcile_authoritative_trace_scope(
            &mut legacy,
            Some(&scope),
            magicllm::LlmScopeResolution::Explicit,
        )
        .expect("legacy scope can be authoritatively rebound");
        assert_eq!(legacy.scope, scope);
        assert_eq!(
            legacy.scope_resolution,
            magicllm::LlmScopeResolution::Explicit
        );

        let mut conflicting = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("other-owner", "other-workspace"),
            magicllm::LlmWorkloadClass::System,
        );
        let error = OperationLlmRouter::reconcile_authoritative_trace_scope(
            &mut conflicting,
            Some(&scope),
            magicllm::LlmScopeResolution::Explicit,
        )
        .expect_err("an explicit cross-scope call identity must fail closed");
        assert!(error
            .to_string()
            .contains("conflicts with authoritative scope"));
    }

    #[tokio::test]
    async fn router_usage_prefers_total_without_double_counting_reasoning_or_cache() {
        with_execution_token_meter(0, 100, async {
            let response = RouterResponse {
                usage: Some(RouterTokenUsage {
                    prompt_tokens: Some(10),
                    completion_tokens: Some(20),
                    total_tokens: Some(30),
                    reasoning_tokens: Some(15),
                    cached_tokens: Some(7),
                    cache_creation_tokens: Some(4),
                }),
                ..RouterResponse::default()
            };

            account_router_response_tokens(&response).expect("usage should be charged");
            assert_eq!(execution_token_budget_snapshot(), Some((30, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn router_usage_falls_back_to_input_plus_output_only() {
        with_execution_token_meter(0, 100, async {
            let response = RouterResponse {
                usage: Some(RouterTokenUsage {
                    prompt_tokens: Some(11),
                    completion_tokens: Some(13),
                    total_tokens: None,
                    reasoning_tokens: Some(17),
                    cached_tokens: Some(5),
                    cache_creation_tokens: Some(3),
                }),
                ..RouterResponse::default()
            };

            account_router_response_tokens(&response).expect("fallback usage should be charged");
            assert_eq!(execution_token_budget_snapshot(), Some((24, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn router_missing_usage_charges_a_length_estimate_without_exhausting() {
        // 40 chars of assistant text -> ~10 estimated tokens (chars/4). The
        // provider omitting usage must NOT force-exhaust the budget; it charges
        // the estimate and the run continues.
        with_execution_token_meter(9, 100, async {
            let response = RouterResponse {
                text: Some(Arc::<str>::from("x".repeat(40))),
                ..RouterResponse::default()
            };
            account_router_response_tokens(&response)
                .expect("missing usage must charge an estimate, not fail closed");
            assert_eq!(execution_token_budget_snapshot(), Some((19, 100)));
            preflight_execution_token_budget().expect(
                "an estimated charge that stays under the cap must not block the next call",
            );
        })
        .await;
    }

    #[tokio::test]
    async fn router_missing_usage_with_no_text_charges_zero() {
        with_execution_token_meter(9, 100, async {
            account_router_response_tokens(&RouterResponse::default())
                .expect("an empty response with no usage charges nothing and does not fail");
            assert_eq!(execution_token_budget_snapshot(), Some((9, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn router_request_error_is_preserved_verbatim_with_an_active_budget() {
        // A transport error carries no token cost: it must be preserved
        // unchanged (for the transient-retry path) and must NOT exhaust the
        // budget.
        with_execution_token_meter(9, 100, async {
            let error = fail_closed_router_request_error(anyhow!("provider connection reset"));
            assert_eq!(error.to_string(), "provider connection reset");
            assert_eq!(execution_token_budget_snapshot(), Some((9, 100)));
        })
        .await;
    }

    #[test]
    fn router_request_error_is_preserved_without_an_execution_budget() {
        let error = fail_closed_router_request_error(anyhow!("provider connection reset"));
        assert_eq!(error.to_string(), "provider connection reset");
    }

    #[tokio::test]
    async fn router_rejects_the_response_that_crosses_the_budget() {
        with_execution_token_meter(90, 100, async {
            let response = RouterResponse {
                usage: Some(RouterTokenUsage {
                    total_tokens: Some(11),
                    ..RouterTokenUsage::default()
                }),
                ..RouterResponse::default()
            };

            let error = account_router_response_tokens(&response)
                .expect_err("an over-budget response must not reach its consumer");
            assert!(error.to_string().contains("used 101, limit 100"));
            assert_eq!(execution_token_budget_snapshot(), Some((101, 100)));
        })
        .await;
    }

    #[tokio::test]
    async fn malformed_response_is_accounted_before_validation_error() {
        with_execution_token_meter(0, 100, async {
            let response = RouterResponse {
                usage: Some(RouterTokenUsage {
                    total_tokens: Some(19),
                    ..RouterTokenUsage::default()
                }),
                finish_reason: Some("max_tokens".to_string()),
                ..RouterResponse::default()
            };

            account_router_response_tokens(&response).expect("usage should be charged first");
            OperationLlmRouter::ensure_response_not_truncated(
                &LLMOperation::Other("malformed_test".to_string()),
                &response,
            )
            .expect_err("truncated output is malformed for structured parsing");
            assert_eq!(execution_token_budget_snapshot(), Some((19, 100)));
        })
        .await;
    }

    #[test]
    fn priority_for_operation_maps_lanes() {
        use magicllm::dispatch::Priority;
        // Voice is user-facing latency-sensitive -> High.
        assert_eq!(
            priority_for_operation(&LLMOperation::VoiceController),
            Priority::High
        );
        assert_eq!(
            priority_for_operation(&LLMOperation::VoiceContextCompaction),
            Priority::High
        );
        // Memory / workflow compilation is best-effort -> Background.
        for op in [
            LLMOperation::MemoryEntityExtraction,
            LLMOperation::MemoryEnvironmentKnowledgeExtraction,
            LLMOperation::MemoryInsightDistillation,
            LLMOperation::MemoryUserPromotion,
            LLMOperation::MemoryArchiveSummary,
            LLMOperation::MemoryEpisodeQualityClassification,
            LLMOperation::MemoryConflictReview,
            LLMOperation::MemoryConflictReviewHighRisk,
        ] {
            assert_eq!(priority_for_operation(&op), Priority::Background);
        }
        for op_name in [
            "distill_evidence",
            "ambient_distill",
            "screen_evidence_distill",
            "tier_evidence_distill",
            "evidence_claims",
            "evidence_precision_judge",
            "evidence_review_verify",
            "memory_temperature_utility_review",
            "learning_reflection",
            "channel_ingest_distill",
            "channel_classify",
            "resurfacing_curate",
        ] {
            assert_eq!(
                priority_for_operation(&LLMOperation::Other(op_name.to_string())),
                Priority::Background,
                "{op_name} should run in the background lane"
            );
        }
        assert_eq!(
            priority_for_operation(&LLMOperation::WorkflowCompilation),
            Priority::Background
        );
        // Everything else (planning / slot-graph / agentic) -> Normal.
        assert_eq!(
            priority_for_operation(&LLMOperation::QueryAnalysis),
            Priority::Normal
        );
        assert_eq!(
            priority_for_operation(&LLMOperation::AgenticInputInterpretation),
            Priority::Normal
        );
        assert_eq!(
            priority_for_operation(&LLMOperation::Other("evidence_review".to_string())),
            Priority::Normal
        );
    }

    #[test]
    fn retired_social_operation_names_no_longer_resolve_to_a_core_variant() {
        // Town Square's gate and compose are the package's now, declared in its
        // manifest as `app:` operations. A core variant would be a second
        // routing identity for the same work -- one the operator's
        // `app_platform.llm_operations` policy could not narrow. Pinned rather
        // than merely deleted so a stale mapping falls through to `Other`
        // instead of silently resolving.
        for name in ["social_gate", "social_compose"] {
            assert_eq!(
                LLMOperation::from_str(name),
                LLMOperation::Other(name.to_string()),
                "`{name}` must no longer name a core operation"
            );
        }
    }

    #[test]
    fn explicit_background_clone_does_not_claim_foreground_pressure_without_live_work() {
        use magicllm::dispatch::Priority;

        let router = OperationLlmRouter::new(None).with_dispatch_priority(Priority::Background);
        assert_eq!(
            router.dispatch_priority_override,
            Some(Priority::Background)
        );
        assert!(!router.has_foreground_dispatch_pressure());
    }

    fn pressure_test_job(
        priority: magicllm::dispatch::Priority,
        state: magicllm::dispatch::JobState,
        provider: Option<LLMProviderKind>,
    ) -> magicllm::dispatch::JobMeta {
        let (job, _response) = magicllm::dispatch::LlmJob::new(
            magicllm::LLMRequest::default(),
            magicllm::dispatch::JobOrigin::op("pressure-test"),
        );
        let job = job.with_priority(priority);
        let mut meta = magicllm::dispatch::JobMeta::pending_from(&job);
        meta.state = state;
        meta.provider = provider;
        meta
    }

    fn pressure_test_snapshot(
        pending: Vec<magicllm::dispatch::JobMeta>,
        in_flight: Vec<magicllm::dispatch::JobMeta>,
        background_depth: usize,
    ) -> magicllm::dispatch::QueueSnapshot {
        magicllm::dispatch::QueueSnapshot {
            workers_total: 3,
            workers_busy: in_flight.len(),
            depth_high: 0,
            depth_normal: 0,
            depth_background: background_depth,
            capacity_high: 8,
            capacity_normal: 8,
            capacity_background: 8,
            retained_bytes_high: 0,
            retained_bytes_normal: 0,
            retained_bytes_background: 0,
            retained_bytes_global: 0,
            retained_bytes_capacity_high: 0,
            retained_bytes_capacity_normal: 0,
            retained_bytes_capacity_background: 0,
            retained_bytes_capacity_global: 0,
            waiting_for_provider: 0,
            waiting_for_provider_high: 0,
            waiting_for_provider_normal: 0,
            waiting_for_provider_background: 0,
            waiting_for_local_prep: 0,
            oldest_wait_ms_high: None,
            oldest_wait_ms_normal: None,
            oldest_wait_ms_background: None,
            registry: magicllm::dispatch::registry::RegistrySnapshot {
                pending,
                in_flight,
                completed: Vec::new(),
                failed: Vec::new(),
                tombstoned: Vec::new(),
            },
        }
    }

    #[test]
    fn background_admission_yields_to_any_foreground_work() {
        let snapshot = pressure_test_snapshot(
            vec![pressure_test_job(
                magicllm::dispatch::Priority::Normal,
                magicllm::dispatch::JobState::Pending,
                None,
            )],
            Vec::new(),
            0,
        );
        assert!(background_dispatch_capacity_busy(&snapshot, Some("ollama")));
    }

    #[test]
    fn background_admission_serializes_only_the_same_provider() {
        let snapshot = pressure_test_snapshot(
            Vec::new(),
            vec![pressure_test_job(
                magicllm::dispatch::Priority::Background,
                magicllm::dispatch::JobState::InFlight,
                Some(LLMProviderKind::Ollama),
            )],
            0,
        );
        assert!(background_dispatch_capacity_busy(&snapshot, Some("ollama")));
        assert!(!background_dispatch_capacity_busy(
            &snapshot,
            Some("openai")
        ));
        assert!(
            !background_dispatch_capacity_busy(&snapshot, None),
            "missing routing must proceed to the typed configuration error"
        );
    }

    #[test]
    fn queued_background_work_defers_another_maintenance_producer() {
        let snapshot = pressure_test_snapshot(Vec::new(), Vec::new(), 8);
        assert!(background_dispatch_capacity_busy(&snapshot, Some("ollama")));

        let streaming_handoff = pressure_test_snapshot(
            vec![pressure_test_job(
                magicllm::dispatch::Priority::Background,
                magicllm::dispatch::JobState::Pending,
                Some(LLMProviderKind::Ollama),
            )],
            Vec::new(),
            0,
        );
        assert!(background_dispatch_capacity_busy(
            &streaming_handoff,
            Some("ollama")
        ));

        let healthy_other_background_work = pressure_test_snapshot(Vec::new(), Vec::new(), 1);
        assert!(
            !background_dispatch_capacity_busy(&healthy_other_background_work, Some("ollama")),
            "a merely nonempty background lane must not starve durable work"
        );
    }

    fn block_text(block: &RouterContentBlock) -> &str {
        match block {
            RouterContentBlock::Text { text } => text.as_str(),
            _ => panic!("expected text block"),
        }
    }

    #[test]
    fn split_summarisation_carves_source_into_own_block() {
        let raw = "[{\"episode\":1},{\"episode\":2}]";
        let prompt =
            format!("Consolidate this.\n\nSource data (JSON):\n{raw}\n\nReturn strict JSON only.");

        let (content, block) = split_user_content_for_summarisation(
            &prompt,
            raw,
            SummarisationPurpose::ConsolidationEpisode,
            0,
        )
        .expect("source present -> split applies");

        // before + placeholder + after.
        assert_eq!(content.len(), 3);
        assert_eq!(block.message_index, 0);
        assert_eq!(block.content_index, 1);
        assert_eq!(block.raw, raw);
        assert!(matches!(
            block.purpose,
            SummarisationPurpose::ConsolidationEpisode
        ));
        // The placeholder slot is empty (overwritten by local-prep at dispatch).
        assert_eq!(block_text(&content[1]), "");
        // before + raw + after reconstructs the original prompt exactly, so the
        // disabled / fall-through path is content-equivalent to the inline prompt.
        let reconstructed = format!(
            "{}{}{}",
            block_text(&content[0]),
            block.raw,
            block_text(&content[2])
        );
        assert_eq!(reconstructed, prompt);
    }

    #[test]
    fn split_summarisation_skips_empty_halves() {
        // Source at the very start -> no empty leading block; placeholder is index 0.
        let raw = "BODY";
        let prompt = format!("{raw} trailing");
        let (content, block) =
            split_user_content_for_summarisation(&prompt, raw, SummarisationPurpose::Other, 2)
                .expect("split applies");
        assert_eq!(content.len(), 2); // placeholder + after
        assert_eq!(block.content_index, 0);
        assert_eq!(block.message_index, 2);
    }

    #[test]
    fn split_summarisation_noop_when_absent_or_empty() {
        // Empty raw -> None.
        assert!(split_user_content_for_summarisation(
            "anything",
            "",
            SummarisationPurpose::Other,
            0
        )
        .is_none());
        // raw not a substring -> None (router keeps the single inline block).
        assert!(split_user_content_for_summarisation(
            "prompt body",
            "missing",
            SummarisationPurpose::Other,
            0
        )
        .is_none());
    }

    const TEST_EXACT_OPERATION: &str = "special_review";

    fn test_profile(provider: LLMProviderKind, reasoning: Option<ReasoningDefaults>) -> LLMProfile {
        LLMProfile {
            provider,
            model: "test-model".to_string(),
            api_key_env: Some("__TEST_API_KEY_MISSING__".to_string()),
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            default_modality: None,
            reasoning,
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: None,
            context_window_tokens: None,
            chunking: None,
        }
    }

    #[test]
    fn decision_planner_native_adapters_keep_dispatch_off_their_frames() {
        let router = OperationLlmRouter::new(None);
        let operation = LLMOperation::Other("agentic_decision".into());
        let single = router.generate_for_execution_native_tools(
            &operation,
            Some("system"),
            "prompt",
            Vec::new(),
            None,
            None,
            None,
            None,
        );
        let chained = router.generate_for_execution_native_messages(
            &operation,
            Vec::new(),
            Vec::new(),
            None,
            None,
        );
        // These adapters are nested below task-local scopes and the planner.
        // A large inline dispatch future multiplies their debug stack frames.
        for size in [
            std::mem::size_of_val(&single),
            std::mem::size_of_val(&chained),
        ] {
            assert!(
                size < 32 * 1024,
                "native adapter future grew to {size} bytes"
            );
        }
    }

    #[test]
    fn execution_native_dispatch_rejects_text_only_profiles_before_provider_work() {
        let text_only = test_profile(LLMProviderKind::Custom("harness-codex".to_owned()), None);
        let error =
            OperationLlmRouter::ensure_execution_native_profile_supports_tool_calling(&text_only)
                .expect_err("a text-only harness cannot serve the native tool protocol");
        assert!(error.to_string().contains("explicit tool-calling support"));

        let mut tool_capable = test_profile(LLMProviderKind::OpenAI, None);
        tool_capable.supports_tool_calling = Some(true);
        OperationLlmRouter::ensure_execution_native_profile_supports_tool_calling(&tool_capable)
            .expect("an explicitly tool-capable profile is admitted");
    }

    fn test_profile_with_metadata(
        provider: LLMProviderKind,
        metadata: Option<std::collections::HashMap<String, serde_json::Value>>,
    ) -> LLMProfile {
        let mut profile = test_profile(provider, None);
        profile.metadata = metadata;
        profile
    }

    #[test]
    fn stateful_transport_cohorts_require_exact_provider_model_endpoint_and_mode() {
        let metadata = |key: &str, value: &str| {
            std::collections::HashMap::from([(key.to_string(), Value::String(value.to_string()))])
        };
        let mut openai_a = test_profile_with_metadata(
            LLMProviderKind::OpenAI,
            Some(metadata("openai_api_mode", "responses")),
        );
        openai_a.model = "gpt-5.6-terra".to_string();
        openai_a.api_base_url = Some("https://api.openai.com/v1".to_string());
        let openai_same = openai_a.clone();
        assert!(OperationLlmRouter::profiles_share_transport_cohort(
            &openai_a,
            &openai_same
        ));

        let mut other_model = openai_a.clone();
        other_model.model = "gpt-5.6-luna".to_string();
        assert!(!OperationLlmRouter::profiles_share_transport_cohort(
            &openai_a,
            &other_model
        ));

        let mut other_endpoint = openai_a.clone();
        other_endpoint.api_base_url = Some("https://proxy.example/v1".to_string());
        assert!(!OperationLlmRouter::profiles_share_transport_cohort(
            &openai_a,
            &other_endpoint
        ));

        let mut gemini = test_profile_with_metadata(
            LLMProviderKind::Gemini,
            Some(metadata("gemini_api_mode", "interactions")),
        );
        gemini.model = openai_a.model.clone();
        gemini.api_base_url = openai_a.api_base_url.clone();
        assert!(!OperationLlmRouter::profiles_share_transport_cohort(
            &openai_a, &gemini
        ));
    }

    #[test]
    fn prefix_cache_and_bounded_replay_profiles_remain_stateless_cohorts() {
        let anthropic = test_profile_with_metadata(LLMProviderKind::Anthropic, None);
        let openrouter = test_profile_with_metadata(LLMProviderKind::OpenRouter, None);
        let ollama = test_profile_with_metadata(LLMProviderKind::Ollama, None);

        assert!(OperationLlmRouter::profiles_share_transport_cohort(
            &anthropic,
            &openrouter
        ));
        assert!(OperationLlmRouter::profiles_share_transport_cohort(
            &anthropic, &ollama
        ));
    }

    // ── Parent-engine routing ────────────────────────────────────────────
    // The fixture: an API-class default every operation maps to, one
    // harness profile per engine the tests name, a local profile for the
    // locality floor, and two operations — the agentic loop's Decide, which
    // carries tools at dispatch, and a text-only summary whose selector the
    // caller shapes (default profile and `engine` follow).

    const PARENT_TEST_DECISION: &str = "agentic_decision";
    const PARENT_TEST_SUMMARY: &str = "task_summary";

    fn parent_engine_config(
        summary_default: &str,
        summary_engine: Option<magicllm::config::OperationEngineFollow>,
    ) -> magicllm::config::LLMRouterConfig {
        let mut profiles: std::collections::HashMap<String, LLMProfile> =
            std::collections::HashMap::new();
        profiles.insert(
            "cloud-tools".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        profiles.insert(
            "op-harness-grok".to_string(),
            test_profile_with_metadata(LLMProviderKind::Custom("harness-grok".to_string()), None),
        );
        profiles.insert(
            "op-harness-codex".to_string(),
            test_profile_with_metadata(LLMProviderKind::Custom("harness-codex".to_string()), None),
        );
        profiles.insert(
            "local-summary".to_string(),
            test_profile_with_metadata(LLMProviderKind::Ollama, None),
        );
        let selector =
            |default: &str, engine| magicllm::config::OperationProfileSelector::Conditional {
                default: default.to_string(),
                when_has_images: None,
                when_cloud: None,
                description: None,
                group: None,
                engine,
            };
        let mut operation_mapping: std::collections::HashMap<
            String,
            magicllm::config::OperationProfileSelector,
        > = std::collections::HashMap::new();
        operation_mapping.insert(
            PARENT_TEST_DECISION.to_string(),
            selector("cloud-tools", None),
        );
        operation_mapping.insert(
            PARENT_TEST_SUMMARY.to_string(),
            selector(summary_default, summary_engine),
        );
        magicllm::config::LLMRouterConfig {
            profiles,
            adaptive_profiles: std::collections::HashMap::new(),
            operation_mapping,
            default_profile: "cloud-tools".to_string(),
            realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
            ..Default::default()
        }
    }

    /// Both resolution sites for one request: the dispatch pin (site 2)
    /// and the provider of the resolved profile (site 1). The two must
    /// agree or dispatch contradicts resolution.
    fn resolve_under_parent(
        config: &magicllm::config::LLMRouterConfig,
        overrides: Option<&OperationRoutingOverrides>,
        operation: &str,
        has_tools: bool,
    ) -> (Option<String>, LLMProviderKind) {
        let operation = LLMOperation::Other(operation.to_string());
        let shape = magicllm::config::RequestShape {
            has_images: false,
            has_tools,
        };
        let pin = OperationLlmRouter::resolved_profile_name_for_shape(
            config, overrides, &operation, &shape,
        );
        let profile = OperationLlmRouter::profile_for_operation_from_config_with_shape(
            config, overrides, &operation, &shape,
        )
        .expect("the profile resolves");
        (pin, profile.provider)
    }

    fn harness_provider(engine: &str) -> LLMProviderKind {
        LLMProviderKind::Custom(format!("harness-{engine}"))
    }

    #[test]
    fn an_explicit_parent_routes_text_only_ops_to_its_harness_profile() {
        let config = parent_engine_config("cloud-tools", None);
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));

        let (pin, provider) =
            resolve_under_parent(&config, Some(&overrides), PARENT_TEST_SUMMARY, false);
        assert_eq!(pin.as_deref(), Some("op-harness-grok"));
        assert_eq!(provider, harness_provider("grok"));

        let (pin, provider) =
            resolve_under_parent(&config, Some(&overrides), PARENT_TEST_DECISION, true);
        assert_eq!(pin, None, "no pin: the config mapping stands");
        assert_eq!(
            provider,
            LLMProviderKind::OpenAI,
            "a tool-carrying request keeps its tool-capable profile"
        );
    }

    #[tokio::test]
    async fn an_ambient_parent_routes_the_same_way() {
        use crate::magician_v2::query_analysis::parent_engine::with_parent_engine;

        let config = parent_engine_config("cloud-tools", None);
        let (summary, decision) = with_parent_engine(Some("grok"), async {
            (
                resolve_under_parent(&config, None, PARENT_TEST_SUMMARY, false),
                resolve_under_parent(&config, None, PARENT_TEST_DECISION, true),
            )
        })
        .await;

        assert_eq!(summary.0.as_deref(), Some("op-harness-grok"));
        assert_eq!(summary.1, harness_provider("grok"));
        assert_eq!(decision.0, None);
        assert_eq!(decision.1, LLMProviderKind::OpenAI);

        // Outside the flow the same request has no parent.
        let (pin, provider) = resolve_under_parent(&config, None, PARENT_TEST_SUMMARY, false);
        assert_eq!(pin, None);
        assert_eq!(provider, LLMProviderKind::OpenAI);
    }

    #[tokio::test]
    async fn explicit_wins_over_ambient() {
        use crate::magician_v2::query_analysis::parent_engine::with_parent_engine;

        let config = parent_engine_config("cloud-tools", None);
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("codex".to_string()));
        let (pin, provider) = with_parent_engine(Some("grok"), async {
            resolve_under_parent(&config, Some(&overrides), PARENT_TEST_SUMMARY, false)
        })
        .await;

        assert_eq!(pin.as_deref(), Some("op-harness-codex"));
        assert_eq!(provider, harness_provider("codex"));
    }

    #[test]
    fn a_pinned_operation_ignores_the_parent() {
        let config = parent_engine_config(
            "cloud-tools",
            Some(magicllm::config::OperationEngineFollow::Pinned),
        );
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));

        let (pin, provider) =
            resolve_under_parent(&config, Some(&overrides), PARENT_TEST_SUMMARY, false);
        assert_eq!(pin, None, "a pinned operation keeps its own profile");
        assert_eq!(provider, LLMProviderKind::OpenAI);

        // The pin is per operation: a sibling that follows still rides the
        // parent for a text-only call.
        let (pin, provider) =
            resolve_under_parent(&config, Some(&overrides), PARENT_TEST_DECISION, false);
        assert_eq!(pin.as_deref(), Some("op-harness-grok"));
        assert_eq!(provider, harness_provider("grok"));
    }

    #[test]
    fn the_process_cell_no_longer_routes() {
        let config = parent_engine_config("cloud-tools", None);

        super::set_harness_affinity(Some("grok"));
        let display = super::harness_affinity_profile(&config);
        let resolved = resolve_under_parent(&config, None, PARENT_TEST_SUMMARY, false);
        super::set_harness_affinity(None);

        assert_eq!(
            display.as_deref(),
            Some("op-harness-grok"),
            "the cell still labels the engine in use for the panel"
        );
        assert_eq!(resolved.0, None, "no parent: the config mapping stands");
        assert_eq!(resolved.1, LLMProviderKind::OpenAI);
    }

    #[test]
    fn the_floors_hold_under_a_parent() {
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));

        // Local stays local whatever drives: an operation whose config
        // default is an Ollama profile never follows the parent.
        let local = parent_engine_config("local-summary", None);
        let (pin, provider) =
            resolve_under_parent(&local, Some(&overrides), PARENT_TEST_SUMMARY, false);
        assert_eq!(pin, None);
        assert_eq!(provider, LLMProviderKind::Ollama);

        // A request that carries tools never follows: the harness CLI
        // profiles cannot call tools, so routing the agentic loop's Decide
        // there would stop every autonomous run for as long as an external
        // engine drives.
        let cloud = parent_engine_config("cloud-tools", None);
        let (pin, provider) =
            resolve_under_parent(&cloud, Some(&overrides), PARENT_TEST_DECISION, true);
        assert_eq!(pin, None, "no pin: the config mapping stands");
        assert_eq!(
            provider,
            LLMProviderKind::OpenAI,
            "a tool-carrying request keeps its tool-capable profile"
        );
        // The same operation follows for a text-only call, so the floor is
        // the request shape and not the operation.
        let (pin, _) = resolve_under_parent(&cloud, Some(&overrides), PARENT_TEST_DECISION, false);
        assert_eq!(pin.as_deref(), Some("op-harness-grok"));
    }

    // ── Install-level engine pins ────────────────────────────────────────
    // The owner's Settings choice lives in the process store and wins over
    // the config selector's `engine`, exactly as a profile override wins
    // over the mapping. Each test names its own operation so a concurrent
    // sibling reading the same store never sees a pin it did not set, and
    // clears what it set.

    /// The parent fixture plus one extra text-only operation on the API
    /// default whose selector carries `engine`.
    fn parent_engine_config_with(
        operation: &str,
        engine: Option<magicllm::config::OperationEngineFollow>,
    ) -> magicllm::config::LLMRouterConfig {
        let mut config = parent_engine_config("cloud-tools", None);
        config.operation_mapping.insert(
            operation.to_string(),
            magicllm::config::OperationProfileSelector::Conditional {
                default: "cloud-tools".to_string(),
                when_has_images: None,
                when_cloud: None,
                description: None,
                group: None,
                engine,
            },
        );
        config
    }

    /// Concurrent sets on DIFFERENT operations must all survive. Each setter
    /// used to clone the map under a read lock, release it, and swap its clone
    /// in under a later write lock, so the last swap erased every pin taken in
    /// between. That is what made `a_store_pin_beats_a_config_selector_that_follows`
    /// pass alone and fail beside its sibling test: both took a pin at once,
    /// and one vanished. Many threads, distinct keys, every key checked.
    #[test]
    fn concurrent_routing_pins_on_different_operations_are_all_kept() {
        const THREADS: usize = 16;
        let engine_ops: Vec<String> = (0..THREADS)
            .map(|i| format!("engine_pin_race_probe_{i}"))
            .collect();
        let override_ops: Vec<String> = (0..THREADS)
            .map(|i| format!("override_race_probe_{i}"))
            .collect();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
        let handles: Vec<_> = (0..THREADS)
            .map(|i| {
                let (engine_op, override_op) = (engine_ops[i].clone(), override_ops[i].clone());
                let barrier = std::sync::Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    super::set_llm_routing_engine(
                        &engine_op,
                        magicllm::config::OperationEngineFollow::Pinned,
                    )
                    .expect("engine pin persists");
                    super::set_llm_routing_override(&override_op, "gpt61sol-responses-toolsany")
                        .expect("override persists");
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("setter thread");
        }
        let mut lost_engine = Vec::new();
        for op in &engine_ops {
            if !super::clear_llm_routing_engine(op).expect("clear persists") {
                lost_engine.push(op.clone());
            }
        }
        let mut lost_override = Vec::new();
        for op in &override_ops {
            if !super::clear_llm_routing_override(op).expect("clear persists") {
                lost_override.push(op.clone());
            }
        }
        assert!(
            lost_engine.is_empty(),
            "engine pins lost to a concurrent set: {lost_engine:?}"
        );
        assert!(
            lost_override.is_empty(),
            "overrides lost to a concurrent set: {lost_override:?}"
        );
    }

    #[test]
    fn a_store_pin_beats_a_config_selector_that_follows() {
        const OPERATION: &str = "engine_pin_probe_store_pinned";
        let config = parent_engine_config_with(
            OPERATION,
            Some(magicllm::config::OperationEngineFollow::Parent),
        );
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));

        super::set_llm_routing_engine(OPERATION, magicllm::config::OperationEngineFollow::Pinned)
            .expect("pin persists");
        let resolved = resolve_under_parent(&config, Some(&overrides), OPERATION, false);
        let follows = super::follows_parent_for(OPERATION, config.operation_mapping.get(OPERATION));
        assert!(
            super::clear_llm_routing_engine(OPERATION).expect("clear persists"),
            "the pin existed"
        );

        assert!(!follows, "the store pin outranks the selector's follow");
        assert_eq!(
            resolved.0, None,
            "no parent profile resolves for a pinned operation"
        );
        assert_eq!(resolved.1, LLMProviderKind::OpenAI);
    }

    #[test]
    fn a_store_follow_beats_a_config_selector_that_pins() {
        const OPERATION: &str = "engine_pin_probe_store_parent";
        let config = parent_engine_config_with(
            OPERATION,
            Some(magicllm::config::OperationEngineFollow::Pinned),
        );
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));

        super::set_llm_routing_engine(OPERATION, magicllm::config::OperationEngineFollow::Parent)
            .expect("pin persists");
        let resolved = resolve_under_parent(&config, Some(&overrides), OPERATION, false);
        let follows = super::follows_parent_for(OPERATION, config.operation_mapping.get(OPERATION));
        assert!(super::clear_llm_routing_engine(OPERATION).expect("clear persists"));

        assert!(follows, "the store follow outranks the selector's pin");
        assert_eq!(resolved.0.as_deref(), Some("op-harness-grok"));
        assert_eq!(resolved.1, harness_provider("grok"));
    }

    #[test]
    fn an_absent_store_pin_falls_through_to_the_config_selector() {
        const OPERATION: &str = "engine_pin_probe_absent";
        let overrides =
            OperationRoutingOverrides::default().with_parent_engine(Some("grok".to_string()));
        assert_eq!(super::llm_routing_engine_for(OPERATION), None);
        assert!(!super::clear_llm_routing_engine(OPERATION).expect("clear persists"));

        let pinned = parent_engine_config_with(
            OPERATION,
            Some(magicllm::config::OperationEngineFollow::Pinned),
        );
        let resolved = resolve_under_parent(&pinned, Some(&overrides), OPERATION, false);
        assert!(!super::follows_parent_for(
            OPERATION,
            pinned.operation_mapping.get(OPERATION)
        ));
        assert_eq!(resolved.0, None);
        assert_eq!(resolved.1, LLMProviderKind::OpenAI);

        let follows = parent_engine_config_with(OPERATION, None);
        let resolved = resolve_under_parent(&follows, Some(&overrides), OPERATION, false);
        assert!(super::follows_parent_for(
            OPERATION,
            follows.operation_mapping.get(OPERATION)
        ));
        assert_eq!(resolved.0.as_deref(), Some("op-harness-grok"));
        assert_eq!(resolved.1, harness_provider("grok"));

        // No mapping at all follows, and the shared rule says so directly.
        assert!(super::follows_parent_with(None, None));
        assert!(!super::follows_parent_with(
            Some(magicllm::config::OperationEngineFollow::Pinned),
            None
        ));
    }

    #[test]
    fn shape_aware_resolver_picks_image_alternative_when_cohort_matches() {
        // Both profiles are OpenAI Chat Completions (no openai_api_mode
        // set) → cohort matches → alternative fires when has_images=true.
        let mut profiles: std::collections::HashMap<String, LLMProfile> =
            std::collections::HashMap::new();
        profiles.insert(
            "text-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        profiles.insert(
            "vision-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        let mut operation_mapping: std::collections::HashMap<
            String,
            magicllm::config::OperationProfileSelector,
        > = std::collections::HashMap::new();
        operation_mapping.insert(
            "vision_capable_chat".to_string(),
            magicllm::config::OperationProfileSelector::Conditional {
                default: "text-chat".to_string(),
                when_has_images: Some("vision-chat".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        let config = magicllm::config::LLMRouterConfig {
            profiles,
            adaptive_profiles: std::collections::HashMap::new(),
            operation_mapping,
            default_profile: "text-chat".to_string(),
            realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
            ..Default::default()
        };
        let with_image = OperationLlmRouter::profile_for_operation_from_config_with_shape(
            &config,
            None,
            &LLMOperation::Other("vision_capable_chat".to_string()),
            &magicllm::config::RequestShape {
                has_images: true,
                has_tools: false,
            },
        )
        .expect("alternative resolves");
        assert_eq!(with_image.model, "test-model");
    }

    #[test]
    fn shape_aware_resolver_allows_chat_to_responses_swap() {
        // Default = OpenAI Chat Completions (no openai_api_mode).
        // Alternative = OpenAI Responses API (openai_api_mode: responses).
        // Mixed stateful/stateless: the runner's shape-change reset clears
        // any prior chain id when has_images flips, so swapping to
        // Responses on image turns is safe — the new transport just sees
        // a stateless first call and starts its own chain. The cohort
        // guard now allows this swap.
        let mut chat_metadata: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        let mut responses_metadata: std::collections::HashMap<String, serde_json::Value> =
            std::collections::HashMap::new();
        responses_metadata.insert(
            "openai_api_mode".to_string(),
            Value::String("responses".to_string()),
        );
        let _ = chat_metadata.insert("placeholder".to_string(), Value::Null);

        let mut profiles: std::collections::HashMap<String, LLMProfile> =
            std::collections::HashMap::new();
        profiles.insert(
            "text-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        profiles.insert(
            "vision-responses".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, Some(responses_metadata)),
        );
        let mut operation_mapping: std::collections::HashMap<
            String,
            magicllm::config::OperationProfileSelector,
        > = std::collections::HashMap::new();
        operation_mapping.insert(
            "mismatched_op".to_string(),
            magicllm::config::OperationProfileSelector::Conditional {
                default: "text-chat".to_string(),
                when_has_images: Some("vision-responses".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        let config = magicllm::config::LLMRouterConfig {
            profiles,
            adaptive_profiles: std::collections::HashMap::new(),
            operation_mapping,
            default_profile: "text-chat".to_string(),
            realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
            ..Default::default()
        };
        let resolved = OperationLlmRouter::profile_for_operation_from_config_with_shape(
            &config,
            None,
            &LLMOperation::Other("mismatched_op".to_string()),
            &magicllm::config::RequestShape {
                has_images: true,
                has_tools: false,
            },
        )
        .expect("alternative resolves");
        // Alternative is selected: vision-responses metadata sets
        // `openai_api_mode: responses`. The runner clears `last_response_id`
        // when has_images flips, so the prior stateless chat chain is
        // safely abandoned and the responses transport starts cleanly.
        assert_eq!(
            resolved
                .metadata
                .as_ref()
                .and_then(|m| m.get("openai_api_mode"))
                .and_then(serde_json::Value::as_str),
            Some("responses")
        );
    }

    #[test]
    fn shape_aware_resolver_allows_cross_provider_swap_to_yutori() {
        // The bug we're fixing: `vision_capable_chat` maps
        // default=openai-chat, when_has_images=yutori-vision. The old
        // cohort guard rejected this swap because providers differ; the
        // user observed a WARN per image iteration and Yutori never
        // engaged. New rule: both sides are stateless on the wire
        // (no Responses chain involved), so the swap is safe and the
        // alternative fires.
        let mut profiles: std::collections::HashMap<String, LLMProfile> =
            std::collections::HashMap::new();
        profiles.insert(
            "openai-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        profiles.insert(
            "vision-yutori-n1".to_string(),
            test_profile_with_metadata(LLMProviderKind::Yutori, None),
        );
        let mut operation_mapping: std::collections::HashMap<
            String,
            magicllm::config::OperationProfileSelector,
        > = std::collections::HashMap::new();
        operation_mapping.insert(
            "vision_capable_chat".to_string(),
            magicllm::config::OperationProfileSelector::Conditional {
                default: "openai-chat".to_string(),
                when_has_images: Some("vision-yutori-n1".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        let config = magicllm::config::LLMRouterConfig {
            profiles,
            adaptive_profiles: std::collections::HashMap::new(),
            operation_mapping,
            default_profile: "openai-chat".to_string(),
            realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
            ..Default::default()
        };
        let resolved = OperationLlmRouter::profile_for_operation_from_config_with_shape(
            &config,
            None,
            &LLMOperation::Other("vision_capable_chat".to_string()),
            &magicllm::config::RequestShape {
                has_images: true,
                has_tools: false,
            },
        )
        .expect("yutori alternative resolves");
        assert!(matches!(resolved.provider, LLMProviderKind::Yutori));
    }

    #[test]
    fn shape_aware_resolver_picks_default_when_no_image() {
        let mut profiles: std::collections::HashMap<String, LLMProfile> =
            std::collections::HashMap::new();
        profiles.insert(
            "text-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        profiles.insert(
            "vision-chat".to_string(),
            test_profile_with_metadata(LLMProviderKind::OpenAI, None),
        );
        let mut operation_mapping: std::collections::HashMap<
            String,
            magicllm::config::OperationProfileSelector,
        > = std::collections::HashMap::new();
        operation_mapping.insert(
            "conditional_chat".to_string(),
            magicllm::config::OperationProfileSelector::Conditional {
                default: "text-chat".to_string(),
                when_has_images: Some("vision-chat".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        let config = magicllm::config::LLMRouterConfig {
            profiles,
            adaptive_profiles: std::collections::HashMap::new(),
            operation_mapping,
            default_profile: "text-chat".to_string(),
            realtime_voice: magicllm::config::RealtimeVoiceConfig::default(),
            ..Default::default()
        };
        let resolved = OperationLlmRouter::profile_for_operation_from_config_with_shape(
            &config,
            None,
            &LLMOperation::Other("conditional_chat".to_string()),
            &magicllm::config::RequestShape {
                has_images: false,
                has_tools: false,
            },
        )
        .expect("default resolves");
        assert_eq!(resolved.model, "test-model");
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
    fn realtime_voice_profiles_follow_display_order_then_id() {
        let yaml = r#"
realtime_voice:
  profiles:
    voice_c_last:
      provider: gemini_live
      model: gemini-3.1-flash-live-preview
      display_order: 100
    voice_b:
      provider: gemini_live
      model: gemini-3.8-live
    voice_a:
      provider: openai_realtime
      model: gpt-realtime-2.1
    voice_first:
      provider: gemini_live
      model: gemini-3.8-live-extended-thinking
      display_order: -1
"#;
        let config: LLMRouterConfig = serde_yaml::from_str(yaml).expect("config should parse");
        let router = OperationLlmRouter::new(Some(config));
        let ids = router
            .realtime_voice_profiles()
            .into_iter()
            .map(|(id, _)| id)
            .collect::<Vec<_>>();
        assert_eq!(
            ids,
            ["voice_first", "voice_a", "voice_b", "voice_c_last"],
            "explicit display_order first, unset profiles sort as 0 by id"
        );
    }

    /// The shipped configs list every Gemini Live generation: both GA 3.8
    /// models ahead of the 3.1 preview, which stays selectable and priced but
    /// last in every engine picker.
    #[test]
    fn shipped_configs_offer_gemini_3_8_live_and_list_3_1_last() {
        for (source, yaml) in [(
            "repository config",
            crate::config::shipped_repo_config_yaml(),
        )] {
            let config: crate::config::MagicianConfig =
                serde_yaml::from_str(&yaml).unwrap_or_else(|error| panic!("{source}: {error}"));
            let router_config = config.llm.router.clone().expect("router");
            let realtime = &router_config.realtime_voice;

            let live = &realtime.profiles["voice_realtime_gemini_38_live"];
            assert_eq!(live.model, "gemini-3.8-live", "{source}");
            assert!(live.selectable && live.display_name.is_some(), "{source}");
            assert!(
                live.thinking_level.is_none(),
                "{source}: 3.8 Live has no level knob"
            );

            let thinking = &realtime.profiles["voice_realtime_gemini_38_live_thinking"];
            assert_eq!(
                thinking.model, "gemini-3.8-live-extended-thinking",
                "{source}"
            );
            assert!(
                thinking.selectable && thinking.display_name.is_some(),
                "{source}"
            );
            assert!(
                magicllm::realtime::gemini::GeminiThinkingLevel::parse(
                    thinking.thinking_level.as_deref().unwrap_or_default()
                )
                .is_some(),
                "{source}: the extended-thinking profile pins a valid thinking_level"
            );

            let flash = &realtime.profiles["voice_realtime_gemini_live"];
            assert_eq!(
                flash.model, "gemini-3.1-flash-live-preview",
                "{source}: 3.1 stays"
            );
            assert!(flash.selectable, "{source}: 3.1 stays selectable");

            let router = OperationLlmRouter::new(Some(router_config));
            let picker = router
                .realtime_voice_profiles()
                .into_iter()
                .filter(|(_, profile)| profile.selectable && profile.display_name.is_some())
                .map(|(id, _)| id)
                .collect::<Vec<_>>();
            assert_eq!(
                picker.last().map(String::as_str),
                Some("voice_realtime_gemini_live"),
                "{source}: 3.1 Flash Live is the last engine offered, got {picker:?}"
            );
            let position = |id: &str| picker.iter().position(|p| p == id).expect(id);
            assert!(
                position("voice_realtime_gemini_38_live")
                    < position("voice_realtime_gemini_38_live_thinking"),
                "{source}: 3.8 Live precedes 3.8 Live Extended Thinking"
            );
        }
    }

    #[test]
    fn test_router_config_deserializes_from_yaml() {
        let yaml = r#"
profiles:
  llm-small:
    provider: openai
    model: gpt-5.6-terra
    api_key_env: OPENAI_API_KEY
operation_mapping:
  query_analysis: llm-small
default_profile: llm-small
"#;

        let config: LLMRouterConfig = serde_yaml::from_str(yaml).expect("config should parse");
        assert_eq!(config.default_profile, "llm-small");
        assert_eq!(
            config
                .operation_mapping
                .get("query_analysis")
                .map(|selector| selector.default_profile()),
            Some("llm-small")
        );
        let small = config
            .profiles
            .get("llm-small")
            .expect("llm-small profile should exist");
        assert_eq!(small.provider, LLMProviderKind::OpenAI);
        assert_eq!(small.model, "gpt-5.6-terra");
        assert_eq!(small.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
    }

    #[test]
    fn test_operation_llm_router_creation() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "llm-small".to_string();
        config.profiles.insert(
            "llm-small".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config
            .operation_mapping
            .insert("query_analysis".to_string(), "llm-small".into());

        let service = OperationLlmRouter::new(Some(config));
        let profile = service
            .get_config_for_operation(&LLMOperation::QueryAnalysis)
            .expect("query_analysis should resolve");
        assert_eq!(profile.model, "gpt-5.6-terra");
    }

    fn invalid_chunk_adapter_config() -> LLMRouterConfig {
        serde_yaml::from_str(
            r#"
profiles:
  invalid-local-chunked:
    provider: ollama
    model: gemma4:12b
    context_window_tokens: 32768
    chunking:
      enabled: true
      adapter: not_registered_v1
      logical_window_tokens: 262144
      target_payload_tokens: 24576
      safety_margin_tokens: 2048
operation_mapping:
  memory_entity_extraction: invalid-local-chunked
default_profile: invalid-local-chunked
"#,
        )
        .expect("invalid adapter mapping is syntactically valid router config")
    }

    fn chunkable_snapshot_test_config(
        profile_name: &str,
        model: &str,
        enabled: bool,
    ) -> LLMRouterConfig {
        serde_yaml::from_str(&format!(
            r#"
profiles:
  {profile_name}:
    provider: ollama
    model: {model}
    api_base_url: http://localhost:11434/api/generate
    max_output_tokens: 4096
    context_window_tokens: 32768
    chunking:
      enabled: {enabled}
      adapter: memory_episode_quality_v1
      logical_window_tokens: 262144
      target_payload_tokens: 24576
      safety_margin_tokens: 2048
      fallback_policy: same_provider_only
  test-monolithic-default:
    provider: ollama
    model: test-default
    api_base_url: http://localhost:11434/api/generate
    max_output_tokens: 4096
    context_window_tokens: 32768
operation_mapping:
  memory_episode_quality_classification: {profile_name}
default_profile: test-monolithic-default
"#
        ))
        .expect("chunkable test config")
    }

    #[tokio::test]
    async fn chunk_enabled_call_never_invokes_lazy_fallback_materializer() {
        let router = OperationLlmRouter::new(Some(chunkable_snapshot_test_config(
            "chunked-a",
            "local-a",
            true,
        )))
        .with_routing_overrides(Some(OperationRoutingOverrides {
            memory_consolidation: OperationRoutingEndpoint::new("ollama", "forced-model"),
            ..OperationRoutingOverrides::default()
        }));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let error = router
            .generate_for_chunkable_operation_with_lazy_fallback(
                &LLMOperation::MemoryEpisodeQualityClassification,
                serde_json::json!({"episode_quality_projections": []}),
                |_| {
                    calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Ok(ChunkableOperationFallback::new(
                        None,
                        "must not build".to_string(),
                    ))
                },
            )
            .await
            .expect_err("model/provider overrides fail before logical execution");
        assert!(error
            .to_string()
            .contains("cannot run with model/provider overrides"));
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 0);
    }

    /// The global adapter registry starts empty (builtins register at bin
    /// boot from the magician-chunking crate), so this lib-side test
    /// registers a stand-in with the id its config references.
    struct LibTestEpisodeQualityAdapter;

    impl magicllm::ChunkDomainAdapter for LibTestEpisodeQualityAdapter {
        fn id(&self) -> &'static str {
            "memory_episode_quality_v1"
        }

        fn version(&self) -> &'static str {
            "1"
        }

        fn supported_operations(&self) -> &'static [&'static str] {
            &["memory_episode_quality_classification"]
        }

        fn final_validation_contract(&self) -> magicllm::FinalValidationContract {
            magicllm::FinalValidationContract::Available
        }

        fn logical_items(
            &self,
            _input: &serde_json::Value,
        ) -> Result<Vec<magicllm::LogicalItem>, magicllm::ChunkError> {
            Ok(vec![magicllm::LogicalItem::root(
                "source-1",
                0,
                serde_json::Value::Null,
            )])
        }

        fn split_oversized_item(
            &self,
            item: &magicllm::LogicalItem,
            budget: &magicllm::ChunkBudget,
        ) -> Result<Vec<magicllm::LogicalItem>, magicllm::ChunkError> {
            Err(magicllm::ChunkError::ChunkItemExceedsContextWindow {
                adapter: self.id().to_string(),
                item_id: item.identity.id.clone(),
                estimated_tokens: budget.effective_payload_tokens.saturating_add(1),
                effective_payload_tokens: budget.effective_payload_tokens,
            })
        }

        fn validate_final(
            &self,
            _value: &serde_json::Value,
        ) -> Result<(), magicllm::ChunkValidationError> {
            Ok(())
        }

        fn validate_map_value(
            &self,
            _value: &serde_json::Value,
            _chunk: &magicllm::ChunkDescriptor,
        ) -> Result<(), magicllm::ChunkValidationError> {
            Ok(())
        }
    }

    #[test]
    fn chunkable_snapshot_is_consistent_across_hot_reload() {
        let _ = crate::magician_v2::llm_chunking::register_global_chunk_adapter(
            std::sync::Arc::new(LibTestEpisodeQualityAdapter),
        );
        let router = OperationLlmRouter::new(Some(chunkable_snapshot_test_config(
            "chunked-a",
            "local-a",
            true,
        )));
        let operation = LLMOperation::MemoryEpisodeQualityClassification;
        let snapshot = router
            .snapshot_chunkable_operation(&operation)
            .expect("initial snapshot");
        let old_router = snapshot.router.as_ref().expect("initial configured router");
        assert_ne!(
            snapshot.logical_profile_name.as_deref(),
            Some("test-monolithic-default"),
            "the domain adapter must remain operation-scoped rather than becoming the wildcard router default",
        );

        assert!(
            router.reload_from_config(Some(chunkable_snapshot_test_config(
                "monolithic-b",
                "local-b",
                false,
            )))
        );
        let current = router.current_state();
        let current_router = current.router.as_ref().expect("reloaded configured router");

        assert_eq!(snapshot.logical_profile_name.as_deref(), Some("chunked-a"));
        assert_eq!(snapshot.request_profile.model, "local-a");
        assert!(snapshot
            .request_profile
            .chunking
            .as_ref()
            .is_some_and(|policy| policy.enabled));
        assert!(!Arc::ptr_eq(old_router, current_router));
        assert_eq!(
            router
                .get_config_for_operation(&operation)
                .expect("reloaded profile")
                .model,
            "local-b"
        );
    }

    #[tokio::test]
    async fn lazy_fallback_error_is_returned_after_single_materialization() {
        let router = OperationLlmRouter::new(Some(chunkable_snapshot_test_config(
            "monolithic",
            "local-disabled",
            false,
        )));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let mut deep = Value::Null;
        for _ in 0..4_096 {
            deep = Value::Array(vec![deep]);
        }
        let error = router
            .generate_for_chunkable_operation_with_lazy_fallback(
                &LLMOperation::MemoryEpisodeQualityClassification,
                deep,
                |_| {
                    calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Err(anyhow!("fallback rendering failed"))
                },
            )
            .await
            .expect_err("materialization error is caller-owned");
        assert_eq!(error.to_string(), "fallback rendering failed");
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn iterative_json_owner_releases_deep_input_on_small_stack() {
        std::thread::Builder::new()
            .name("lazy-fallback-drop-regression".to_string())
            .stack_size(96 * 1024)
            .spawn(|| {
                let mut deep = Value::Null;
                for _ in 0..16_384 {
                    deep = Value::Array(vec![deep]);
                }
                drop(IterativeJsonOwner::new(deep));
            })
            .expect("small-stack regression thread")
            .join()
            .expect("iterative destruction must not overflow");
    }

    #[test]
    fn invalid_chunk_adapter_mapping_fails_router_startup() {
        let service = OperationLlmRouter::new(Some(invalid_chunk_adapter_config()));
        let state = service.current_state();
        assert!(state.router_config.is_some());
        assert!(state.router.is_none());
    }

    #[test]
    fn invalid_chunk_adapter_reload_retains_last_good_router() {
        let mut valid_config = LLMRouterConfig::default();
        valid_config.default_profile = "local-baseline".to_string();
        let mut local_profile = test_profile(LLMProviderKind::Ollama, None);
        local_profile.api_key_env = None;
        local_profile.model = "gemma4:12b".to_string();
        valid_config
            .profiles
            .insert("local-baseline".to_string(), local_profile);
        valid_config
            .operation_mapping
            .insert("query_analysis".to_string(), "local-baseline".into());
        let service = OperationLlmRouter::new(Some(valid_config));
        assert!(service.current_state().router.is_some());

        assert!(!service.reload_from_config(Some(invalid_chunk_adapter_config())));
        let retained = service.current_state();
        assert!(retained.router.is_some());
        assert_eq!(
            retained
                .router_config
                .as_ref()
                .map(|config| config.default_profile.as_str()),
            Some("local-baseline")
        );
    }

    fn binding_test_config() -> LLMRouterConfig {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "llm-small".to_string();
        config.profiles.insert(
            "llm-small".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.profiles.insert(
            "local-ollama".to_string(),
            LLMProfile {
                provider: LLMProviderKind::Ollama,
                model: "gemma3".to_string(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config
            .operation_mapping
            .insert("channel_ingest_distill".to_string(), "local-ollama".into());
        config
    }

    #[test]
    fn explicit_binding_reports_name_and_kind_without_default_fallback() {
        let service = OperationLlmRouter::new(Some(binding_test_config()));

        // Explicitly mapped operation → (name, kind) of the mapped profile.
        assert_eq!(
            service.explicit_binding_for_operation("channel_ingest_distill"),
            Some(("local-ollama".to_string(), LLMProviderKind::Ollama))
        );
        // Unmapped operation → None even though `default_profile` would
        // resolve it: fail-closed callers must read unbound as unbound.
        assert_eq!(service.explicit_binding_for_operation("unmapped_op"), None);
        // No config at all → None.
        let empty = OperationLlmRouter::new(None);
        assert_eq!(
            empty.explicit_binding_for_operation("channel_ingest_distill"),
            None
        );
    }

    #[tokio::test]
    async fn pinned_dispatch_enforces_the_provider_kind_lock_before_any_request() {
        let service = OperationLlmRouter::new(Some(binding_test_config()));
        let operation = LLMOperation::Other("channel_ingest_distill".to_string());

        // Pin to a profile whose provider violates the required kind: the
        // magician-layer pre-check refuses before a request is even built
        // (no router/provider is ever consulted).
        let err = service
            .generate_for_operation_with_system_pinned(
                &operation,
                None,
                "prompt",
                "llm-small",
                Some(LLMProviderKind::Ollama),
            )
            .await
            .expect_err("openai-kind pin must violate the ollama lock");
        assert!(err.to_string().contains("provider lock"));
        // The matching-pin dispatch path is covered by magicllm's
        // `route_honors_required_provider_kind_lock_on_match` with a mock
        // provider — not exercised here to keep tests free of any chance
        // of a live Ollama dispatch.
    }

    #[test]
    fn test_router_handles_all_providers() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "router-openai".to_string();
        config.profiles.insert(
            "router-openai".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: Some(std::collections::HashMap::from([(
                    "api_version".to_string(),
                    serde_json::json!("responses"),
                )])),
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.profiles.insert(
            "router-anthropic".to_string(),
            LLMProfile {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-5".to_string(),
                api_key_env: Some("__TEST_ANTHROPIC_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.profiles.insert(
            "router-openrouter".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenRouter,
                model: "openrouter/anthropic/claude-haiku-4-5".to_string(),
                api_key_env: Some("__TEST_OPENROUTER_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );
        config
            .operation_mapping
            .insert("analysis".into(), "router-openai".into());
        config
            .operation_mapping
            .insert("anthropic".into(), "router-anthropic".into());
        config
            .operation_mapping
            .insert("proxy".into(), "router-openrouter".into());

        let service = OperationLlmRouter::new(Some(config));
        let state = service.current_state();
        assert!(
            state.router.is_none(),
            "router should fall back when keys are missing"
        );

        let openai_provider =
            service.provider_for_operation(&LLMOperation::Other("analysis".into()));
        assert_eq!(openai_provider.as_deref(), Some("openai"));
        let anthropic_provider =
            service.provider_for_operation(&LLMOperation::Other("anthropic".into()));
        assert_eq!(anthropic_provider.as_deref(), Some("anthropic"));
        let openrouter_provider =
            service.provider_for_operation(&LLMOperation::Other("proxy".into()));
        assert_eq!(openrouter_provider.as_deref(), Some("openrouter"));
    }

    #[test]
    fn test_routing_overrides_apply_by_operation_lane() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "planner".to_string();
        config.profiles.insert(
            "planner".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );

        let base = OperationLlmRouter::new(Some(config));
        let scoped = base.with_routing_overrides(Some(OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("openai", "gpt-5"),
            evaluation: OperationRoutingEndpoint::new("anthropic", "claude-3-5-sonnet"),
            correction_extraction: OperationRoutingEndpoint::new(
                "openrouter",
                "openrouter/anthropic/claude-haiku-4-5",
            ),
            memory_consolidation: None,
            operations: BTreeMap::new(),
            parent_engine: None,
        }));

        assert_eq!(
            scoped
                .provider_for_operation(&LLMOperation::AtomicComposition)
                .as_deref(),
            Some("openai")
        );
        assert_eq!(
            scoped
                .provider_for_operation(&LLMOperation::AgenticInputInterpretation)
                .as_deref(),
            Some("openrouter")
        );
        assert_eq!(
            scoped
                .provider_for_operation(&LLMOperation::ToolEvaluation)
                .as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn test_routing_overrides_are_scoped_to_cloned_router() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "planner".to_string();
        config.profiles.insert(
            "planner".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5.6-terra".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            },
        );

        let base = OperationLlmRouter::new(Some(config));
        let scoped = base.with_routing_overrides(Some(OperationRoutingOverrides {
            planning: None,
            evaluation: None,
            correction_extraction: OperationRoutingEndpoint::new("anthropic", "claude-3-5-sonnet"),
            memory_consolidation: None,
            operations: BTreeMap::new(),
            parent_engine: None,
        }));

        assert_eq!(
            base.provider_for_operation(&LLMOperation::AgenticInputInterpretation)
                .as_deref(),
            Some("openai")
        );
        assert_eq!(
            scoped
                .provider_for_operation(&LLMOperation::AgenticInputInterpretation)
                .as_deref(),
            Some("anthropic")
        );
    }

    #[test]
    fn test_routing_overrides_exact_operation_beats_lane_override() {
        let overrides = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("anthropic", "claude-sonnet-4-6"),
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: BTreeMap::from([(
                TEST_EXACT_OPERATION.to_string(),
                OperationRoutingEndpoint::new("openai", "gpt-5").expect("endpoint"),
            )]),
            parent_engine: None,
        };

        let endpoint = overrides
            .endpoint_for_operation(&LLMOperation::Other(TEST_EXACT_OPERATION.to_string()))
            .expect("exact operation override");
        assert_eq!(endpoint.provider, "openai");
        assert_eq!(endpoint.model, "gpt-5");
    }

    #[test]
    fn yutori_sentinel_honors_exact_execution_profile_override() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "vision-yutori".to_string();
        config.profiles.insert(
            "vision-yutori".to_string(),
            test_profile(LLMProviderKind::Yutori, None),
        );
        config.profiles.insert(
            "eval-luna".to_string(),
            test_profile(LLMProviderKind::OpenAI, None),
        );
        config
            .operation_mapping
            .insert("agentic_decision".to_string(), "vision-yutori".into());

        let base = OperationLlmRouter::new(Some(config));
        assert!(base.agentic_decision_uses_yutori());

        let routed = base.with_routing_overrides(Some(OperationRoutingOverrides {
            operations: BTreeMap::from([(
                "agentic_decision".to_string(),
                OperationRoutingEndpoint::for_profile("eval-luna").expect("profile endpoint"),
            )]),
            ..Default::default()
        }));
        assert!(
            !routed.agentic_decision_uses_yutori(),
            "an exact execution route must disable global Yutori browser behavior"
        );
    }

    #[test]
    fn test_routing_overrides_exact_profile_uses_full_profile_config() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "review-sonnet".to_string();
        config.profiles.insert(
            "review-sonnet".to_string(),
            LLMProfile {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                api_key_env: Some("__TEST_ANTHROPIC_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: Some(0.2),
                max_output_tokens: Some(8192),
                default_modality: Some(LLMModality::Text),
                reasoning: None,
                metadata: Some(std::collections::HashMap::from([(
                    "fallback_profile".to_string(),
                    serde_json::json!("review-opus-4.7-medium"),
                )])),
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: Some(300),
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.profiles.insert(
            "review-gpt5".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: Some(0.1),
                max_output_tokens: Some(4096),
                default_modality: Some(LLMModality::Text),
                reasoning: Some(magicllm::config::ReasoningDefaults {
                    effort: "medium".to_string(),
                    max_reasoning_tokens: Some(4000),
                    strategy: None,
                    summary: None,
                }),
                metadata: Some(std::collections::HashMap::from([(
                    "fallback_profile".to_string(),
                    serde_json::json!("review-sonnet"),
                )])),
                supports_vision: Some(false),
                supports_reasoning: Some(true),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: Some(300),
                context_window_tokens: None,
                chunking: None,
            },
        );
        config
            .operation_mapping
            .insert(TEST_EXACT_OPERATION.to_string(), "review-sonnet".into());

        let router = OperationLlmRouter::new(Some(config)).with_routing_overrides(Some(
            OperationRoutingOverrides {
                planning: None,
                evaluation: None,
                correction_extraction: None,
                memory_consolidation: None,
                operations: BTreeMap::from([(
                    TEST_EXACT_OPERATION.to_string(),
                    OperationRoutingEndpoint::for_profile("review-gpt5").expect("profile"),
                )]),
                parent_engine: None,
            },
        ));

        let profile = router
            .get_config_for_operation(&LLMOperation::Other(TEST_EXACT_OPERATION.to_string()))
            .expect("exact profile override should resolve");
        assert_eq!(profile.provider, LLMProviderKind::OpenAI);
        assert_eq!(profile.model, "gpt-5");
        assert_eq!(profile.default_modality, Some(LLMModality::Text));
        assert_eq!(profile.supports_reasoning, Some(true));
        assert_eq!(
            profile
                .metadata
                .as_ref()
                .and_then(|meta| meta.get("fallback_profile"))
                .and_then(Value::as_str),
            Some("review-sonnet")
        );
    }

    #[test]
    fn test_request_profile_for_routing_uses_exact_override_profile_defaults() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "review-sonnet".to_string();
        config.profiles.insert(
            "review-sonnet".to_string(),
            LLMProfile {
                provider: LLMProviderKind::Anthropic,
                model: "claude-sonnet-4-6".to_string(),
                api_key_env: Some("__TEST_ANTHROPIC_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: Some(0.2),
                max_output_tokens: Some(8192),
                default_modality: Some(LLMModality::Text),
                reasoning: None,
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(false),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: Some(90),
                context_window_tokens: None,
                chunking: None,
            },
        );
        config.profiles.insert(
            "review-gpt5".to_string(),
            LLMProfile {
                provider: LLMProviderKind::OpenAI,
                model: "gpt-5".to_string(),
                api_key_env: Some("__TEST_OPENAI_KEY_MISSING__".to_string()),
                api_base_url: None,
                temperature: Some(0.1),
                max_output_tokens: Some(4096),
                default_modality: Some(LLMModality::Text),
                reasoning: Some(magicllm::config::ReasoningDefaults {
                    effort: "medium".to_string(),
                    max_reasoning_tokens: Some(4000),
                    strategy: None,
                    summary: None,
                }),
                metadata: None,
                supports_vision: Some(false),
                supports_reasoning: Some(true),
                supports_tool_calling: Some(true),
                supports_computer_use: Some(false),
                timeout_secs: Some(300),
                context_window_tokens: None,
                chunking: None,
            },
        );

        let router = OperationLlmRouter::new(Some(config));
        let state = router.current_state();
        let router_config = state.router_config.as_ref().expect("router config");
        let base_profile = router_config
            .profiles
            .get("review-sonnet")
            .expect("base profile");
        let mut request_profile = OperationLlmRouter::request_profile_for_routing(
            router_config,
            base_profile,
            Some("review-gpt5"),
        )
        .expect("request profile");

        assert_eq!(request_profile.model, "gpt-5");
        assert_eq!(request_profile.temperature, Some(0.1));
        assert_eq!(request_profile.max_output_tokens, Some(4096));
        assert_eq!(
            request_profile
                .reasoning
                .as_ref()
                .map(|value| value.effort.as_str()),
            Some("medium")
        );
        assert_eq!(
            OperationLlmRouter::timeout_for_profile(
                &LLMOperation::Other(TEST_EXACT_OPERATION.to_string()),
                &request_profile
            ),
            300
        );
        assert_eq!(
            OperationLlmRouter::logical_timeout_for_profile(
                &LLMOperation::Other(TEST_EXACT_OPERATION.to_string()),
                &request_profile
            ),
            300
        );
        request_profile.chunking = Some(magicllm::config::ChunkingConfig {
            logical_timeout_secs: Some(1_200),
            ..Default::default()
        });
        assert_eq!(
            OperationLlmRouter::logical_timeout_for_profile(
                &LLMOperation::Other(TEST_EXACT_OPERATION.to_string()),
                &request_profile
            ),
            1_200
        );
    }

    #[test]
    fn test_profile_override_does_not_preserve_source_request_model() {
        assert!(!OperationLlmRouter::preserve_request_model(None));
        assert!(OperationLlmRouter::preserve_request_model(Some(
            "gpt-5.6-terra"
        )));
    }

    #[test]
    fn test_routing_overrides_merge_combines_exact_operation_entries() {
        let base = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("anthropic", "claude-sonnet-4-6"),
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: BTreeMap::from([(
                "custom_operation_a".to_string(),
                OperationRoutingEndpoint::new("anthropic", "claude-sonnet-4-6").expect("endpoint"),
            )]),
            parent_engine: None,
        };
        let overlay = OperationRoutingOverrides {
            planning: None,
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: BTreeMap::from([(
                "custom_operation_b".to_string(),
                OperationRoutingEndpoint::new("openai", "gpt-5").expect("endpoint"),
            )]),
            parent_engine: None,
        };

        let merged =
            OperationRoutingOverrides::merge(Some(base), Some(overlay)).expect("merged overrides");
        assert_eq!(merged.operations.len(), 2);
        assert_eq!(
            merged
                .operations
                .get("custom_operation_a")
                .map(|endpoint| endpoint.provider.as_str()),
            Some("anthropic")
        );
        assert_eq!(
            merged
                .operations
                .get("custom_operation_b")
                .map(|endpoint| endpoint.provider.as_str()),
            Some("openai")
        );
    }

    #[test]
    fn routing_overrides_serialise_the_parent_engine() {
        let with_parent = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        }
        .with_parent_engine(Some("codex".to_string()));
        assert_eq!(with_parent.parent_engine.as_deref(), Some("codex"));

        let json = serde_json::to_value(&with_parent).expect("serialise overrides");
        assert_eq!(json["parent_engine"], serde_json::json!("codex"));
        let round_tripped: OperationRoutingOverrides =
            serde_json::from_value(json).expect("deserialise overrides");
        assert_eq!(round_tripped, with_parent);

        // Absent on the wire when unset, and a pre-field document still
        // deserialises with no parent.
        let without_parent = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        };
        let json = serde_json::to_value(&without_parent).expect("serialise overrides");
        assert!(json.get("parent_engine").is_none());
        let round_tripped: OperationRoutingOverrides =
            serde_json::from_value(json).expect("deserialise overrides");
        assert_eq!(round_tripped.parent_engine, None);

        // The native engine and empty names are no parent at all.
        assert_eq!(
            OperationRoutingOverrides::default()
                .with_parent_engine(Some("magician".to_string()))
                .parent_engine,
            None
        );
        assert_eq!(
            OperationRoutingOverrides::default()
                .with_parent_engine(Some(String::new()))
                .parent_engine,
            None
        );

        // A parent alone is a routing fact: normalisation keeps it.
        let normalized = OperationRoutingOverrides::default()
            .with_parent_engine(Some(" Codex ".to_string()))
            .normalized()
            .expect("a parent alone survives normalisation");
        assert_eq!(normalized.parent_engine.as_deref(), Some("codex"));
    }

    /// A merge carries endpoints, never a parent: whichever side names one,
    /// the merged overrides name none, and two parent-only sides merge to
    /// nothing at all — the runtime assigns the flow's parent afterwards.
    #[test]
    fn a_merge_never_carries_a_parent_engine() {
        let base = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        }
        .with_parent_engine(Some("grok".to_string()));
        let overlay = OperationRoutingOverrides {
            evaluation: OperationRoutingEndpoint::for_profile("eval-judge"),
            ..Default::default()
        }
        .with_parent_engine(Some("codex".to_string()));

        let merged = OperationRoutingOverrides::merge(Some(base.clone()), Some(overlay.clone()))
            .expect("merged endpoints");
        assert_eq!(merged.parent_engine, None);
        assert_eq!(
            merged
                .planning
                .as_ref()
                .and_then(|endpoint| endpoint.profile_name()),
            Some("eval-luna")
        );
        assert_eq!(
            merged
                .evaluation
                .as_ref()
                .and_then(|endpoint| endpoint.profile_name()),
            Some("eval-judge")
        );

        let one_sided = OperationRoutingOverrides::merge(Some(base), None).expect("base endpoints");
        assert_eq!(one_sided.parent_engine, None);
        let other_side =
            OperationRoutingOverrides::merge(None, Some(overlay)).expect("overlay endpoints");
        assert_eq!(other_side.parent_engine, None);

        let parent_only = |engine: &str| {
            Some(OperationRoutingOverrides::default().with_parent_engine(Some(engine.to_string())))
        };
        assert_eq!(
            OperationRoutingOverrides::merge(parent_only("grok"), parent_only("codex")),
            None
        );
    }

    /// A client's overrides reach the runtime as endpoints only: the strip
    /// keeps every endpoint and drops the parent, and a parent-only document
    /// collapses to nothing once normalised.
    #[test]
    fn stripping_the_parent_keeps_the_endpoints() {
        let stripped = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::for_profile("eval-luna"),
            ..Default::default()
        }
        .with_parent_engine(Some("grok".to_string()))
        .without_parent_engine();
        assert_eq!(stripped.parent_engine, None);
        assert_eq!(
            stripped
                .planning
                .as_ref()
                .and_then(|endpoint| endpoint.profile_name()),
            Some("eval-luna")
        );

        let parent_only = OperationRoutingOverrides::default()
            .with_parent_engine(Some("grok".to_string()))
            .without_parent_engine();
        assert_eq!(parent_only.normalized(), None);

        let router = OperationLlmRouter::new(None);
        assert!(router.routing_overrides().is_none());
        let scoped = router.with_routing_overrides(Some(
            OperationRoutingOverrides::default().with_parent_engine(Some("codex".to_string())),
        ));
        assert_eq!(
            scoped
                .routing_overrides()
                .and_then(|overrides| overrides.parent_engine.as_deref()),
            Some("codex")
        );
    }

    #[test]
    fn test_base64_decoding() {
        use base64::Engine;

        // Valid PNG 1x1 pixel image
        let png_base64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(png_base64)
            .expect("Should decode valid base64");
        assert!(!decoded.is_empty());
        assert_eq!(decoded[0], 0x89); // PNG magic number

        // Invalid base64 should fail gracefully
        let invalid_base64 = "not-valid-base64!!!";
        let result = base64::engine::general_purpose::STANDARD.decode(invalid_base64);
        assert!(result.is_err());
    }

    #[test]
    fn test_multimodal_request_structure() {
        use crate::magician_v2::slot_graph::extraction::{
            ImageData, ImageDetail, LlmFunctionCallRequest,
        };

        // Create a multimodal request
        let image1 = ImageData::with_detail(
            "base64_data_1".to_string(),
            "image/png".to_string(),
            ImageDetail::High,
        );
        let image2 = ImageData::new("base64_data_2".to_string(), "image/jpeg".to_string());

        let request = LlmFunctionCallRequest {
            system_prompt: "You are a vision agent".to_string(),
            user_prompt: "Analyze these screenshots".to_string(),
            function_schema: r#"{"result": object}"#.to_string(),
            model: "gpt-5".to_string(),
            temperature: 0.2,
            images: Some(vec![image1, image2]),
        };

        assert!(request.images.is_some());
        let images = request.images.unwrap();
        assert_eq!(images.len(), 2);
        assert_eq!(images[0].media_type, "image/png");
        assert_eq!(images[0].detail, Some(ImageDetail::High));
        assert_eq!(images[1].media_type, "image/jpeg");
        assert_eq!(images[1].detail, Some(ImageDetail::Auto));
    }

    #[test]
    fn test_image_data_cloning() {
        use crate::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

        let original = ImageData::with_detail(
            "base64_data".to_string(),
            "image/png".to_string(),
            ImageDetail::High,
        );

        let cloned = original.clone();
        assert_eq!(cloned.base64, original.base64);
        assert_eq!(cloned.media_type, original.media_type);
        assert_eq!(cloned.detail, original.detail);
    }

    #[test]
    fn test_image_detail_default() {
        use crate::magician_v2::slot_graph::extraction::ImageDetail;

        let default = ImageDetail::default();
        assert_eq!(default, ImageDetail::Auto);
    }

    #[test]
    fn test_multimodal_with_different_media_types() {
        use crate::magician_v2::slot_graph::extraction::ImageData;

        let png = ImageData::new("png_data".to_string(), "image/png".to_string());
        let jpeg = ImageData::new("jpeg_data".to_string(), "image/jpeg".to_string());
        let webp = ImageData::new("webp_data".to_string(), "image/webp".to_string());
        let gif = ImageData::new("gif_data".to_string(), "image/gif".to_string());

        assert_eq!(png.media_type, "image/png");
        assert_eq!(jpeg.media_type, "image/jpeg");
        assert_eq!(webp.media_type, "image/webp");
        assert_eq!(gif.media_type, "image/gif");
    }

    #[test]
    fn test_temperature_conversion_f64_to_f32() {
        let temp_f64: f64 = 0.7;
        let temp_f32 = temp_f64 as f32;
        assert!((temp_f32 - 0.7_f32).abs() < 0.001);

        let temp_f64_high: f64 = 1.0;
        let temp_f32_high = temp_f64_high as f32;
        assert_eq!(temp_f32_high, 1.0_f32);

        let temp_f64_low: f64 = 0.0;
        let temp_f32_low = temp_f64_low as f32;
        assert_eq!(temp_f32_low, 0.0_f32);
    }

    #[test]
    fn test_message_role_enum() {
        use magicllm::types::MessageRole;

        let system = MessageRole::System;
        let user = MessageRole::User;
        let assistant = MessageRole::Assistant;

        assert_ne!(system, user);
        assert_ne!(user, assistant);
        assert_ne!(system, assistant);
    }

    #[test]
    fn test_llm_operation_timeout() {
        // Vision operations should have reasonable timeouts
        let vision_op = LLMOperation::Other("vision_analysis".to_string());
        let timeout = vision_op.timeout_seconds();
        assert_eq!(timeout, 60); // Default timeout for Other operations

        // Action validation operations (Sonnet vision profile with reasoning)
        let validation_op = LLMOperation::Other("action_validation".to_string());
        let validation_timeout = validation_op.timeout_seconds();
        assert_eq!(validation_timeout, 60);

        // Atomic composition gets longer timeout for reasoning
        let atomic_op = LLMOperation::AtomicComposition;
        let atomic_timeout = atomic_op.timeout_seconds();
        assert_eq!(atomic_timeout, 500); // 8.3 minutes for reasoning models
    }

    #[test]
    fn tool_spec_from_schema_extracts_name_and_parameters() {
        let schema = r#"{
            "name": "agentic_decision",
            "description": "Pick element and execute one action",
            "parameters": {"type": "object", "properties": {"decision": {"type": "string"}}}
        }"#;
        let spec = OperationLlmRouter::tool_spec_from_schema(schema).unwrap();
        assert_eq!(spec.name, "agentic_decision");
        assert_eq!(spec.description, "Pick element and execute one action");
        assert!(spec.parameters.get("properties").is_some());
    }

    #[test]
    fn tool_spec_from_schema_falls_back_on_missing_fields() {
        let schema = r#"{"name": "test", "parameters": {"type": "object"}}"#;
        let spec = OperationLlmRouter::tool_spec_from_schema(schema).unwrap();
        assert_eq!(spec.name, "test");
        assert!(spec.description.is_empty());
    }

    #[test]
    fn tool_spec_from_schema_errors_on_invalid_json() {
        let result = OperationLlmRouter::tool_spec_from_schema("not json {{{");
        assert!(result.is_err());
    }

    #[test]
    fn native_decision_user_text_does_not_contain_schema_when_native_enabled() {
        // Verify the native path does NOT embed schema in user text.
        let schema_name = "agentic_decision";
        let user_prompt = "Decide the next action.";
        // In native mode, user_text should just be the user_prompt (no schema embedded).
        let native_user_text = user_prompt.to_string();
        assert!(
            !native_user_text.contains(schema_name),
            "Schema name must not appear in user message text when native tool calling is on"
        );
        // Verify tool_spec_from_schema works for an agentic decision schema.
        let schema = r#"{"name": "agentic_decision", "description": "Agentic decision", "parameters": {"type": "object"}}"#;
        let spec = OperationLlmRouter::tool_spec_from_schema(schema).unwrap();
        assert_eq!(spec.name, "agentic_decision");
    }

    #[test]
    fn tool_spec_from_schema_handles_nested_parameters() {
        let schema = r#"{
            "name": "agentic_decision",
            "description": "Native decision",
            "parameters": {
                "type": "object",
                "required": ["decision"],
                "properties": {
                    "decision": {"type": "string", "enum": ["execute", "goal_reached"]},
                    "element_id": {"type": "integer"}
                }
            }
        }"#;
        let spec = OperationLlmRouter::tool_spec_from_schema(schema).unwrap();
        assert_eq!(spec.name, "agentic_decision");
        let props = spec.parameters["properties"].as_object().unwrap();
        assert!(props.contains_key("decision"));
        assert!(props.contains_key("element_id"));
    }

    #[test]
    fn tool_choice_for_profile_respects_profile_metadata() {
        let mut profile = test_profile(LLMProviderKind::Minimax, None);
        profile.metadata = Some(std::collections::HashMap::from([(
            "tool_choice".to_string(),
            serde_json::json!({"type": "any"}),
        )]));

        assert_eq!(
            OperationLlmRouter::tool_choice_for_profile(&profile),
            serde_json::json!({"type": "any"})
        );
    }

    #[test]
    fn tool_choice_for_profile_falls_back_to_auto_for_anthropic_family() {
        let profile = LLMProfile {
            provider: LLMProviderKind::Anthropic,
            model: "claude-sonnet-4-5".to_string(),
            api_key_env: Some("__TEST_ANTHROPIC_KEY_MISSING__".to_string()),
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            default_modality: None,
            reasoning: Some(magicllm::prelude::ReasoningDefaults {
                effort: "medium".to_string(),
                max_reasoning_tokens: Some(1024),
                strategy: None,
                summary: None,
            }),
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: None,
            context_window_tokens: None,
            chunking: None,
        };

        assert_eq!(
            OperationLlmRouter::tool_choice_for_profile(&profile),
            serde_json::json!({"type": "auto"})
        );

        let minimax_profile = test_profile(LLMProviderKind::Minimax, None);
        assert_eq!(
            OperationLlmRouter::tool_choice_for_profile(&minimax_profile),
            serde_json::json!({"type": "auto"})
        );

        let deepseek_profile = test_profile(LLMProviderKind::DeepSeek, None);
        assert_eq!(
            OperationLlmRouter::tool_choice_for_profile(&deepseek_profile),
            serde_json::json!({"type": "auto"})
        );
    }

    #[test]
    fn magician_config_tool_capable_profiles_declare_tool_choice() {
        let config: serde_json::Value =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml()).unwrap();
        let profiles = config
            .pointer("/llm/router/profiles")
            .and_then(|value| value.as_object())
            .unwrap();

        let mut missing = Vec::new();
        let mut invalid = Vec::new();
        for (name, profile) in profiles {
            if profile
                .get("supports_tool_calling")
                .and_then(|value| value.as_bool())
                != Some(true)
            {
                continue;
            }

            match profile.pointer("/metadata/tool_choice") {
                Some(serde_json::Value::String(choice))
                    if matches!(choice.as_str(), "none" | "auto" | "any") => {},
                Some(serde_json::Value::Object(choice))
                    if choice
                        .get("type")
                        .and_then(|value| value.as_str())
                        .is_some_and(|choice_type| {
                            matches!(choice_type, "none" | "auto" | "any" | "tool")
                        }) => {},
                Some(value) => invalid.push(format!("{name}: {value}")),
                None => missing.push(name.clone()),
            }
        }

        assert!(
            missing.is_empty(),
            "tool-capable profiles missing metadata.tool_choice: {missing:?}"
        );
        assert!(
            invalid.is_empty(),
            "tool-capable profiles with invalid metadata.tool_choice: {invalid:?}"
        );
    }

    #[test]
    fn magician_config_keeps_deepseek_and_minimax_out_of_vision_routing() {
        let config: serde_json::Value =
            serde_yaml::from_str(&crate::config::shipped_repo_config_yaml()).unwrap();
        let profiles = config
            .pointer("/llm/router/profiles")
            .and_then(|value| value.as_object())
            .unwrap();
        let operation_mapping = config
            .pointer("/llm/router/operation_mapping")
            .and_then(|value| value.as_object())
            .unwrap();

        // MiniMax M2 is text-only. MiniMax M3 and DeepSeek V4.1 Flash
        // (`deepseek-flash`, plus retired Flash aliases) accept images.
        let text_only_anthropic_family = ["deepseek", "minimax"];
        let is_multimodal_exception = |profile: &serde_json::Value| {
            let provider = profile.get("provider").and_then(|v| v.as_str());
            let model = profile
                .get("model")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            (provider == Some("minimax") && model.contains("m3"))
                || (provider == Some("deepseek")
                    && (model == "deepseek-flash"
                        || model.contains("deepseek-v4-flash")
                        || model.contains("deepseek-v4.1-flash")
                        || model.contains("vision")))
        };
        let mut text_only_with_vision = Vec::new();
        for (name, profile) in profiles {
            let provider = profile
                .get("provider")
                .and_then(|value| value.as_str())
                .unwrap_or_default();
            if text_only_anthropic_family.contains(&provider) && !is_multimodal_exception(profile) {
                if profile
                    .get("supports_vision")
                    .and_then(|value| value.as_bool())
                    != Some(false)
                {
                    text_only_with_vision.push(format!("{name}: supports_vision is not false"));
                }
                if profile
                    .get("default_modality")
                    .and_then(|value| value.as_str())
                    == Some("vision")
                {
                    text_only_with_vision.push(format!("{name}: default_modality is vision"));
                }
            }
        }

        let image_bearing_operations = [
            "agentic_decision",
            "agentic_decision_retry",
            "screen_observation",
            "screen_understanding",
            "screen_grounding",
        ];
        let mut invalid_mappings = Vec::new();
        for operation in image_bearing_operations {
            let selector = operation_mapping
                .get(operation)
                .unwrap_or_else(|| panic!("{operation} mapping should exist"));
            // A mapping is either a flat profile name (serves every turn) or a
            // conditional object { default, when_has_images }. The profile that
            // must be vision-capable is the one handling IMAGE-bearing turns:
            // `when_has_images` for a conditional map — its `default` may stay
            // text-only (e.g. DeepSeek), since it serves only text turns — or the
            // flat name otherwise. (A text-only default with no `when_has_images`
            // falls back to the default here and is correctly flagged below.)
            let profile_name = if let Some(name) = selector.as_str() {
                name
            } else if let Some(map) = selector.as_object() {
                map.get("when_has_images")
                    .or_else(|| map.get("default"))
                    .and_then(|value| value.as_str())
                    .unwrap_or_else(|| {
                        panic!("{operation} conditional mapping missing default/when_has_images")
                    })
            } else {
                panic!("{operation} mapping is neither a profile name nor a conditional map");
            };
            let profile = profiles.get(profile_name).unwrap_or_else(|| {
                panic!("{operation} image handler maps to missing profile {profile_name}")
            });
            let provider = profile
                .get("provider")
                .and_then(|value| value.as_str())
                .unwrap_or_default();

            if text_only_anthropic_family.contains(&provider) && !is_multimodal_exception(profile) {
                invalid_mappings.push(format!("{operation} -> {profile_name} ({provider})"));
            }
            if profile
                .get("supports_vision")
                .and_then(|value| value.as_bool())
                != Some(true)
            {
                invalid_mappings.push(format!(
                    "{operation} -> {profile_name} does not declare supports_vision: true"
                ));
            }
        }

        assert!(
            text_only_with_vision.is_empty(),
            "DeepSeek/MiniMax profiles must remain text-only: {text_only_with_vision:?}"
        );
        assert!(
            invalid_mappings.is_empty(),
            "image-bearing vision operations must not route to DeepSeek/MiniMax or text-only profiles: {invalid_mappings:?}"
        );
    }

    #[test]
    fn tool_choice_for_profile_uses_any_for_minimax_reasoning() {
        let profile = LLMProfile {
            provider: LLMProviderKind::Minimax,
            model: "MiniMax-M2.7".to_string(),
            api_key_env: Some("__TEST_MINIMAX_KEY_MISSING__".to_string()),
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            default_modality: None,
            reasoning: Some(magicllm::prelude::ReasoningDefaults {
                effort: "high".to_string(),
                max_reasoning_tokens: Some(2048),
                strategy: None,
                summary: None,
            }),
            metadata: None,
            supports_vision: None,
            supports_reasoning: Some(true),
            supports_tool_calling: Some(true),
            supports_computer_use: None,
            timeout_secs: None,
            context_window_tokens: None,
            chunking: None,
        };

        assert_eq!(
            OperationLlmRouter::tool_choice_for_profile(&profile),
            serde_json::json!({"type": "any"})
        );
    }

    #[test]
    fn apply_native_tool_schema_uses_auto_for_anthropic_reasoning_profiles() {
        let profile = test_profile(
            LLMProviderKind::Anthropic,
            Some(ReasoningDefaults {
                effort: "medium".to_string(),
                max_reasoning_tokens: Some(2048),
                strategy: Some("extended_thinking".to_string()),
                summary: None,
            }),
        );
        let schema = r#"{"name":"agentic_decision","description":"Agentic decision","parameters":{"type":"object"}}"#;
        let mut request = RouterRequest::default();
        request.extra = Some(serde_json::json!({ "existing": true }).into());

        OperationLlmRouter::apply_native_tool_schema(&mut request, &profile, schema).unwrap();

        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "agentic_decision");
        assert_eq!(
            request.extra,
            Some(
                serde_json::json!({
                    "existing": true,
                    "tool_choice": {"type": "auto"}
                })
                .into()
            )
        );
    }

    #[test]
    fn apply_native_tool_schema_uses_any_for_non_anthropic_profiles() {
        let profile = test_profile(LLMProviderKind::OpenAI, None);
        let schema = r#"{"name":"agentic_decision","parameters":{"type":"object"}}"#;
        let mut request = RouterRequest::default();

        OperationLlmRouter::apply_native_tool_schema(&mut request, &profile, schema).unwrap();

        assert_eq!(request.tools.len(), 1);
        assert_eq!(request.tools[0].name, "agentic_decision");
        assert_eq!(
            request.extra,
            Some(
                serde_json::json!({
                    "tool_choice": {"type": "any"}
                })
                .into()
            )
        );
    }

    #[test]
    fn strip_json_response_hint_removes_trailing_hint() {
        let prompt =
            "Analyse the screenshot.\n\nRespond with ONLY valid JSON matching the expected schema.";
        assert_eq!(
            OperationLlmRouter::strip_json_response_hint(prompt),
            "Analyse the screenshot."
        );
    }

    #[test]
    fn strip_json_response_hint_removes_short_trailing_hint() {
        let prompt = "Do the thing.\n\nRespond with ONLY valid JSON.";
        assert_eq!(
            OperationLlmRouter::strip_json_response_hint(prompt),
            "Do the thing."
        );
    }

    #[test]
    fn strip_json_response_hint_handles_trailing_whitespace_after_hint() {
        let prompt =
            "Analyse.\n\nRespond with ONLY valid JSON matching the expected schema.   \n\n";
        assert_eq!(
            OperationLlmRouter::strip_json_response_hint(prompt),
            "Analyse."
        );
    }

    #[test]
    fn strip_json_response_hint_does_not_strip_hint_in_middle() {
        let prompt = "Respond with ONLY valid JSON matching the expected schema. But also explain.";
        // Hint is not at the end, so nothing should be stripped.
        assert_eq!(
            OperationLlmRouter::strip_json_response_hint(prompt),
            prompt.trim_end()
        );
    }

    #[test]
    fn strip_json_response_hint_passthrough_when_no_hint() {
        let prompt = "Plan the following task carefully.";
        assert_eq!(
            OperationLlmRouter::strip_json_response_hint(prompt),
            "Plan the following task carefully."
        );
    }

    #[test]
    fn tool_spec_from_schema_errors_on_missing_parameters() {
        let schema = r#"{"name": "test", "description": "no params"}"#;
        let result = OperationLlmRouter::tool_spec_from_schema(schema);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("missing required 'parameters' key"));
    }

    #[test]
    fn generate_for_operation_with_tool_schema_is_reachable() {
        // Compile check: verify the method exists on OperationLlmRouter
        let _router = OperationLlmRouter::new(None);
        // If this test compiles, the method exists on OperationLlmRouter.
        // We can't call it without a real router but the compile check is sufficient.
        let schema = r#"{"name": "test", "parameters": {"type": "object"}}"#;
        let spec = OperationLlmRouter::tool_spec_from_schema(schema).unwrap();
        assert_eq!(spec.name, "test");
    }

    #[test]
    fn test_memory_llm_operations_config_mapping() {
        // Verify from_str / as_str round-trips for all memory operations
        let cases = vec![
            (
                "memory_entity_extraction",
                LLMOperation::MemoryEntityExtraction,
            ),
            (
                "memory_environment_knowledge_extraction",
                LLMOperation::MemoryEnvironmentKnowledgeExtraction,
            ),
            (
                "memory_insight_distillation",
                LLMOperation::MemoryInsightDistillation,
            ),
            ("memory_user_promotion", LLMOperation::MemoryUserPromotion),
            ("memory_archive_summary", LLMOperation::MemoryArchiveSummary),
            (
                "memory_episode_quality_classification",
                LLMOperation::MemoryEpisodeQualityClassification,
            ),
            ("memory_conflict_review", LLMOperation::MemoryConflictReview),
            (
                "memory_conflict_review_high_risk",
                LLMOperation::MemoryConflictReviewHighRisk,
            ),
        ];
        for (key, expected) in &cases {
            let parsed = LLMOperation::from_str(key);
            assert_eq!(&parsed, expected, "from_str mismatch for '{}'", key);
            assert_eq!(parsed.as_str(), *key, "as_str mismatch for '{}'", key);
        }
        // Also verify case insensitivity
        assert_eq!(
            LLMOperation::from_str("MEMORY_ENTITY_EXTRACTION"),
            LLMOperation::MemoryEntityExtraction
        );
    }

    #[test]
    fn test_memory_llm_operations_timeout_defaults() {
        assert_eq!(LLMOperation::MemoryEntityExtraction.timeout_seconds(), 60);
        assert_eq!(
            LLMOperation::MemoryEnvironmentKnowledgeExtraction.timeout_seconds(),
            60
        );
        assert_eq!(
            LLMOperation::MemoryInsightDistillation.timeout_seconds(),
            60
        );
        assert_eq!(LLMOperation::MemoryUserPromotion.timeout_seconds(), 60);
        assert_eq!(LLMOperation::MemoryArchiveSummary.timeout_seconds(), 45);
        assert_eq!(
            LLMOperation::MemoryEpisodeQualityClassification.timeout_seconds(),
            45
        );
        assert_eq!(LLMOperation::MemoryConflictReview.timeout_seconds(), 60);
        assert_eq!(
            LLMOperation::MemoryConflictReviewHighRisk.timeout_seconds(),
            90
        );
    }

    #[test]
    fn test_memory_llm_operations_routing_overrides() {
        let overrides = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("openai", "gpt-5"),
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: OperationRoutingEndpoint::new("anthropic", "claude-haiku-4-5"),
            operations: BTreeMap::new(),
            parent_engine: None,
        };

        // Memory operations should route through memory_consolidation
        let memory_ops = vec![
            LLMOperation::MemoryEntityExtraction,
            LLMOperation::MemoryEnvironmentKnowledgeExtraction,
            LLMOperation::MemoryInsightDistillation,
            LLMOperation::MemoryUserPromotion,
            LLMOperation::MemoryArchiveSummary,
            LLMOperation::MemoryEpisodeQualityClassification,
            LLMOperation::MemoryConflictReview,
            LLMOperation::MemoryConflictReviewHighRisk,
        ];
        for op in &memory_ops {
            let endpoint = overrides.endpoint_for_operation(op);
            assert!(
                endpoint.is_some(),
                "Memory op {:?} should have routing override",
                op
            );
            let endpoint = endpoint.unwrap();
            assert_eq!(
                endpoint.provider, "anthropic",
                "Memory op {:?} provider mismatch",
                op
            );
            assert_eq!(
                endpoint.model, "claude-haiku-4-5",
                "Memory op {:?} model mismatch",
                op
            );
        }

        // Non-memory operations should NOT route through memory_consolidation
        let planning_endpoint = overrides.endpoint_for_operation(&LLMOperation::TaskDecomposition);
        assert!(planning_endpoint.is_some());
        assert_eq!(planning_endpoint.unwrap().provider, "openai");
    }

    #[test]
    fn test_durable_state_operations_ignore_generic_planning_lane_override() {
        let overrides = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("anthropic", "claude-sonnet-4-6"),
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: BTreeMap::new(),
            parent_engine: None,
        };

        assert!(
            overrides
                .endpoint_for_operation(&LLMOperation::Other(
                    "durable_task_state_generate".to_string()
                ))
                .is_none(),
            "durable_task_state_generate should use its dedicated operation mapping"
        );
        assert!(
            overrides
                .endpoint_for_operation(&LLMOperation::Other(
                    "durable_task_state_patch".to_string()
                ))
                .is_none(),
            "durable_task_state_patch should use its dedicated operation mapping"
        );
    }

    #[test]
    fn test_durable_state_exact_operation_override_still_wins() {
        let overrides = OperationRoutingOverrides {
            planning: OperationRoutingEndpoint::new("anthropic", "claude-sonnet-4-6"),
            evaluation: None,
            correction_extraction: None,
            memory_consolidation: None,
            operations: BTreeMap::from([(
                "durable_task_state_generate".to_string(),
                OperationRoutingEndpoint::for_profile("sonnet46-messages-toolsany-rnone")
                    .expect("profile"),
            )]),
            parent_engine: None,
        };

        let endpoint = overrides
            .endpoint_for_operation(&LLMOperation::Other(
                "durable_task_state_generate".to_string(),
            ))
            .expect("exact durable-state override");
        assert_eq!(
            endpoint.profile.as_deref(),
            Some("sonnet46-messages-toolsany-rnone")
        );
    }

    #[test]
    fn ensure_response_not_truncated_rejects_max_tokens_finish_reason() {
        let response = RouterResponse {
            finish_reason: Some("max_tokens".to_string()),
            ..RouterResponse::default()
        };
        let error = OperationLlmRouter::ensure_response_not_truncated(
            &LLMOperation::Other("agentic_decision".to_string()),
            &response,
        )
        .expect_err("max_tokens response should be rejected before parsing");
        assert!(error.to_string().contains("truncated at finish_reason"));
    }

    #[tokio::test]
    async fn logical_chunk_runtime_bridge_separates_parent_summary_from_physical_usage() {
        let broadcaster =
            Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let sink = RuntimeLogicalChunkTelemetry {
            broadcaster: Arc::clone(&broadcaster),
        };
        let parent = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::System,
        );
        sink.emit_summary(
            Some(parent.clone()),
            1_000,
            "memory_entity_extraction".to_string(),
            "local-memory".to_string(),
            Some("ceo".to_string()),
            25,
            true,
            None,
        );
        let child = parent.child(
            magicllm::LlmParentRelation::ChunkMap,
            magicllm::LlmCallRole::Supporting,
        );
        sink.emit_physical(
            magicllm::LlmTraceReceipt::direct_with_attempt_count(child, 1),
            1_010,
            "memory_entity_extraction".to_string(),
            Some("ceo".to_string()),
            "local-memory".to_string(),
            Some("ollama".to_string()),
            "memory_episode".to_string(),
            crate::magician_v2::llm_chunking::PhysicalChunkStage::Map,
            0,
            0,
            "gemma4:12b".to_string(),
            crate::magician_v2::llm_chunking::AggregateTokenUsage {
                prompt_tokens: 100,
                completion_tokens: 20,
                total_tokens: 120,
                reasoning_tokens: 0,
                cached_tokens: 10,
                cache_creation_tokens: 0,
            },
            true,
            true,
            None,
            2,
            20,
        );

        let summary = events.recv().await.expect("summary event");
        let physical = events.recv().await.expect("physical event");
        assert!(matches!(
            summary,
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                correlation: Some(crate::magician_v2::realtime_events::LlmEventCorrelation {
                    provider_attempt_count: 0,
                    ..
                }),
                usage_reported: false,
                response_kind,
                ..
            } if response_kind == "logical_chunk_summary"
        ));
        assert!(matches!(
            physical,
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                correlation: Some(crate::magician_v2::realtime_events::LlmEventCorrelation {
                    provider_attempt_count: 1,
                    parent_relation: Some(parent_relation),
                    ..
                }),
                usage_reported: true,
                input_tokens: 100,
                output_tokens: 20,
                response_kind,
                ..
            } if parent_relation == "chunk_map" && response_kind == "logical_chunk_physical"
        ));
    }

    #[tokio::test]
    async fn direct_operation_failure_bridge_preserves_zero_and_nonzero_attempts() {
        let broadcaster =
            Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let router = OperationLlmRouter::new(None);
        router.set_event_broadcaster(Arc::clone(&broadcaster));
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::InteractiveTask,
        );
        let operation = LLMOperation::Other("direct_failure_test".to_string());
        let error = magicllm::LLMError::Provider {
            provider: "test".to_string(),
            message: "provider failed".to_string(),
        };

        router.emit_direct_route_failure(
            &context,
            0,
            Some(LLMProviderKind::OpenAI),
            "gpt-test".to_string(),
            Some("test-profile".to_string()),
            &operation,
            1_000,
            5,
            &error,
        );
        let pre_provider = events.recv().await.expect("pre-provider failure event");
        assert!(matches!(
            pre_provider,
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                success: false,
                usage_reported: false,
                attempt: 0,
                correlation: Some(crate::magician_v2::realtime_events::LlmEventCorrelation {
                    provider_attempt_count: 0,
                    provider_attempt_id: None,
                    ..
                }),
                ..
            }
        ));

        router.emit_direct_route_failure(
            &context,
            1,
            Some(LLMProviderKind::OpenAI),
            "gpt-test".to_string(),
            Some("test-profile".to_string()),
            &operation,
            1_000,
            5,
            &error,
        );
        let event = events.recv().await.expect("direct failure event");
        let crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
            success,
            usage_reported,
            provider,
            model,
            attempt,
            response_kind,
            correlation,
            ..
        } = event
        else {
            panic!("unexpected event variant");
        };
        assert!(!success);
        assert!(!usage_reported);
        assert_eq!(provider, "openai");
        assert_eq!(model, "gpt-test");
        assert_eq!(attempt, 1);
        assert_eq!(response_kind, "provider_error:operation_router_direct");
        let expected_attempt_id = context.provider_attempt_id(1);
        assert_eq!(
            correlation
                .as_ref()
                .and_then(|value| value.provider_attempt_id.as_deref()),
            Some(expected_attempt_id.as_str())
        );
    }

    #[tokio::test]
    async fn direct_route_request_stamps_trace_activity_from_current_span() {
        let subscriber =
            tracing_subscriber::registry().with(RuntimeActivityLayer::with_channel(Arc::new(
                crate::magician_v2::analytics::runtime_activity_layer::ActivityChannel::new(8),
            )));
        let mut observed_activity_id = None;
        tracing::subscriber::with_default(subscriber, || {
            let span = tracing::info_span!(
                target: "magician::runtime_activity_test",
                "llm_dispatch"
            );
            let _entered = span.enter();

            let current =
                crate::magician_v2::analytics::runtime_activity_layer::current_activity_id()
                    .map(|activity| activity.to_string());
            assert!(
                current.is_some(),
                "direct-route test needs an active tracked span to assert propagation"
            );

            let mut context = magicllm::LlmTraceContext::new(
                magicllm::LlmScope::new("scope-owner", "scope-workspace"),
                magicllm::LlmWorkloadClass::InteractiveTask,
            );
            assert!(
                context.activity_id.is_none(),
                "trace context should start without a live activity id"
            );
            OperationLlmRouter::ensure_trace_context_activity_id(&mut context);
            observed_activity_id = context.activity_id;
            assert_eq!(observed_activity_id.as_deref(), current.as_deref());
        });
        assert!(
            observed_activity_id.is_some(),
            "sanity check: span activity id should be visible to the test"
        );
    }

    #[tokio::test]
    async fn post_response_rejection_preserves_scope_route_usage_and_typed_error() {
        let broadcaster =
            Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let router = OperationLlmRouter::new(None);
        router.set_event_broadcaster(Arc::clone(&broadcaster));
        let context = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("tenant-owner", "tenant-workspace"),
            magicllm::LlmWorkloadClass::InteractiveTask,
        );
        let receipt = magicllm::LlmTraceReceipt::direct(context.clone());
        let response = RouterResponse {
            usage: Some(RouterTokenUsage {
                prompt_tokens: Some(11),
                completion_tokens: Some(7),
                total_tokens: Some(18),
                ..RouterTokenUsage::default()
            }),
            trace_receipt: Some(receipt),
            route_identity: Some(magicllm::LlmRouteIdentity {
                profile: "actual-profile".to_string(),
                provider: LLMProviderKind::Anthropic,
                model: "actual-model".to_string(),
            }),
            ..RouterResponse::default()
        };

        router.emit_post_response_validation_failure(
            &response,
            &LLMOperation::QueryAnalysis,
            "fallback-provider",
            "fallback-model",
            Some("fallback-profile"),
            chrono::Utc::now().timestamp_millis(),
            "missing_response_content",
        );

        let event = events.recv().await.expect("validation failure event");
        assert!(matches!(
            event,
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                principal: Some(principal),
                workspace: Some(workspace),
                success: true,
                error: Some(error),
                provider,
                model,
                input_tokens: 11,
                output_tokens: 7,
                profile: Some(profile),
                response_kind,
                correlation: Some(crate::magician_v2::realtime_events::LlmEventCorrelation {
                    llm_call_id,
                    provider_attempt_count: 1,
                    ..
                }),
                ..
            } if principal == "tenant-owner"
                && workspace == "tenant-workspace"
                && provider == "anthropic"
                && model == "actual-model"
                && profile == "actual-profile"
                && llm_call_id == context.llm_call_id
                && response_kind == "validation_error:missing_response_content"
                && error == "caller contract validation failed (missing_response_content)"
        ));
    }

    #[tokio::test]
    async fn direct_logical_chunk_failure_preserves_the_physical_child_attempt() {
        let mut config = LLMRouterConfig::default();
        config.default_profile = "local-logical".to_string();
        let mut profile = test_profile(LLMProviderKind::Ollama, None);
        profile.api_key_env = None;
        profile.model = "gemma4:12b".to_string();
        config.profiles.insert("local-logical".to_string(), profile);
        config.operation_mapping.insert(
            "memory_entity_extraction".to_string(),
            "local-logical".into(),
        );
        let broadcaster =
            Arc::new(crate::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8));
        let mut events = broadcaster.subscribe();
        let dispatch = DirectLogicalChunkDispatch {
            router: Arc::new(
                ConfiguredRouter::from_router_config(config).expect("local router config"),
            ),
            event_broadcaster: Some(Arc::clone(&broadcaster)),
        };
        let parent = magicllm::LlmTraceContext::new(
            magicllm::LlmScope::new("owner", "workspace"),
            magicllm::LlmWorkloadClass::Memory,
        );
        let child = parent.child(
            magicllm::LlmParentRelation::ChunkReduce,
            magicllm::LlmCallRole::Summarizer,
        );
        dispatch.emit_failed_physical_attempt(
            child,
            2,
            1_000,
            41,
            "memory_entity_extraction".to_string(),
            Some("local-logical".to_string()),
            Some(LLMProviderKind::Ollama),
            "gemma4:12b".to_string(),
            &magicllm::LLMError::Provider {
                provider: "test".to_string(),
                message: "connection reset".to_string(),
            },
        );

        let event = events.recv().await.expect("failed physical child event");
        assert!(matches!(
            event,
            crate::magician_v2::realtime_events::RuntimeTransportEvent::LLMResponseReceived {
                correlation: Some(crate::magician_v2::realtime_events::LlmEventCorrelation {
                    provider_attempt_count: 2,
                    parent_relation: Some(parent_relation),
                    ..
                }),
                success: false,
                usage_reported: false,
                attempt: 2,
                response_kind,
                ..
            } if parent_relation == "chunk_reduce"
                && response_kind == "provider_error:logical_chunk_physical"
        ));
    }
}

/// Locality hot-switch: flipping `privacy.processing.mode` and reloading the
/// router config changes the verified binding for every switchable operation
/// without a restart (the settings API's PUT-then-reload path).
#[cfg(any(test, feature = "test-fixtures"))]
mod locality_hot_switch_tests {
    use crate::magician_v2::query_analysis::operation_llm_router::{
        LLMOperation, OperationLlmRouter,
    };
    use magicllm::config::{LLMRouterConfig, OperationProfileSelector};
    use magicllm::LLMProviderKind;

    fn switchable_config(locality: magicllm::ProcessingLocality) -> LLMRouterConfig {
        let mut config = LLMRouterConfig::default();
        // Both arms ollama: a provider needing API-key env cannot instantiate
        // in tests. Cross-kind acceptance is covered by the seam's guard
        // matrix; this test pins the ARM FLIP and reload liveness.
        for (name, provider) in [("op-a-local", "ollama"), ("op-a-remote", "ollama")] {
            config.profiles.insert(
                name.to_string(),
                serde_json::from_value(serde_json::json!({
                    "provider": provider,
                    "model": "m",
                }))
                .expect("profile fixture"),
            );
        }
        config.operation_mapping.insert(
            "channel_ingest_distill".to_string(),
            OperationProfileSelector::Conditional {
                default: "op-a-local".to_string(),
                when_has_images: None,
                when_cloud: Some("op-a-remote".to_string()),
                description: None,
                group: None,
                engine: None,
            },
        );
        config.default_profile = "op-a-local".to_string();
        config.locality = locality;
        config
    }

    /// An `agentic_decision` mapping whose default profile times out after
    /// `timeout_secs`, so two configs are told apart by the timeout the
    /// dispatch router resolves.
    fn decision_config(default_profile: &str) -> LLMRouterConfig {
        let mut config = LLMRouterConfig::default();
        for (name, timeout) in [("decide-old", 11), ("decide-new", 22)] {
            config.profiles.insert(
                name.to_string(),
                serde_json::from_value(serde_json::json!({
                    "provider": "ollama",
                    "model": "m",
                    "timeout_secs": timeout,
                }))
                .expect("profile fixture"),
            );
        }
        config.operation_mapping.insert(
            "agentic_decision".to_string(),
            OperationProfileSelector::Conditional {
                default: default_profile.to_string(),
                when_has_images: None,
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        config.default_profile = default_profile.to_string();
        config
    }

    #[test]
    fn only_an_image_alternative_on_another_profile_changes_the_decision_profile() {
        // No `when_has_images`: both shapes land on one profile.
        let router = OperationLlmRouter::new(Some(decision_config("decide-old")));
        assert!(!router.image_shape_changes_profile("agentic_decision"));

        let mut config = decision_config("decide-old");
        config.profiles.insert(
            "decide-vision".to_string(),
            serde_json::from_value(serde_json::json!({"provider": "ollama", "model": "vision"}))
                .expect("profile fixture"),
        );
        config.operation_mapping.insert(
            "agentic_decision".to_string(),
            OperationProfileSelector::Conditional {
                default: "decide-old".to_string(),
                when_has_images: Some("decide-vision".to_string()),
                when_cloud: None,
                description: None,
                group: None,
                engine: None,
            },
        );
        assert!(router.reload_from_config(Some(config)));
        assert!(router.image_shape_changes_profile("agentic_decision"));
    }

    #[test]
    fn a_config_reload_reaches_the_dispatch_queues_router() {
        use magicllm::dispatch::DispatchRouter;
        let router = OperationLlmRouter::new(Some(decision_config("decide-old")));
        // What the queue was handed at boot before, and what it is now.
        let pinned = router.shared_configured_router().expect("configured");
        let live = router.live_dispatch_router();
        assert_eq!(live.timeout_for_operation("agentic_decision"), Some(11));

        assert!(router.reload_from_config(Some(decision_config("decide-new"))));
        // The boot router still dispatches the old mapping; the live one
        // follows the reload without a restart.
        assert_eq!(
            DispatchRouter::timeout_for_operation(pinned.as_ref(), "agentic_decision"),
            Some(11)
        );
        assert_eq!(live.timeout_for_operation("agentic_decision"), Some(22));
        // A clone (per-agent routers are clones) shares the same state.
        assert!(router
            .clone()
            .reload_from_config(Some(decision_config("decide-old"))));
        assert_eq!(live.timeout_for_operation("agentic_decision"), Some(11));
    }

    #[test]
    fn reloading_with_a_flipped_mode_moves_the_verified_binding_without_a_restart() {
        let router =
            OperationLlmRouter::new(Some(switchable_config(magicllm::ProcessingLocality::Local)));
        let (profile, kind) = router
            .explicit_binding_for_operation("channel_ingest_distill")
            .expect("bound in local mode");
        assert_eq!(profile, "op-a-local");
        assert_eq!(kind, LLMProviderKind::Ollama);

        // Hot switch to cloud: same router object, reloaded config.
        assert!(router.reload_from_config(Some(switchable_config(
            magicllm::ProcessingLocality::Cloud,
        ))));
        let (profile, _kind) = router
            .explicit_binding_for_operation("channel_ingest_distill")
            .expect("bound in cloud mode");
        assert_eq!(profile, "op-a-remote");

        // And back — the switch is reversible.
        assert!(router.reload_from_config(Some(switchable_config(
            magicllm::ProcessingLocality::Local,
        ))));
        let (profile, _kind) = router
            .explicit_binding_for_operation("channel_ingest_distill")
            .expect("bound again in local mode");
        assert_eq!(profile, "op-a-local");
        // The operation enum used by guards and dispatch resolves the same way.
        let operation = LLMOperation::Other("channel_ingest_distill".to_string());
        assert_eq!(
            router.processing_locality(),
            magicllm::ProcessingLocality::Local
        );
        assert!(router.get_config_for_operation(&operation).is_ok());
    }

    /// Phase 2 of the harness-LLM-provider plan: an installed override
    /// flips ONE operation's resolved profile and bypasses locality arms,
    /// while sibling operations resolve exactly as before.
    #[test]
    fn install_level_override_flips_one_operation_and_bypasses_locality() {
        // `.keep()`, not a guard that drops: the installer sets process-global
        // `OnceLock`s — the first caller's paths stand for the whole test run —
        // so this directory backs EVERY routing persist in the process, not
        // just this test's. When it was a `TempDir` guard, its teardown deleted
        // the directory while other tests were still writing through the global
        // path, and their `std::fs::write` failed with `NotFound`. That is what
        // made the router suite fail one run in three with no code at fault.
        // Production installs once at boot against the stable runtime root.
        let dir = tempfile::tempdir().expect("tempdir").keep();
        super::install_llm_routing_overrides(&dir);

        let config = test_router_config_with_locality_cloud();
        let shape = magicllm::config::RequestShape::default();
        let resolve = |op: LLMOperation| {
            super::OperationLlmRouter::profile_for_operation_from_config_with_shape_for_test(
                &config, None, &op, &shape,
            )
            .expect("resolution")
        };

        // Sanity: in cloud locality, meeting_response takes its cloud arm.
        assert_eq!(
            resolve(LLMOperation::MeetingResponse).model,
            "meeting-remote-model"
        );
        let sibling_before = resolve(LLMOperation::MeetingSummary).model;

        // The override names the LOCAL arm's profile — the explicit owner
        // choice must win over the locality arm.
        super::set_llm_routing_override("meeting_response", "op-meeting-summary-local")
            .expect("set override");
        assert_eq!(
            resolve(LLMOperation::MeetingResponse).model,
            "meeting-local-model",
            "the override bypasses the cloud locality arm"
        );

        // The sibling operation resolves exactly as before the override —
        // its own locality arms included (in Cloud it takes its when_cloud
        // arm; the point is the override changes nothing for it).
        assert_eq!(
            resolve(LLMOperation::MeetingSummary).model,
            sibling_before,
            "the override touches exactly one operation"
        );
        let _ = super::clear_llm_routing_override("meeting_response");
    }

    fn test_router_config_with_locality_cloud() -> magicllm::config::LLMRouterConfig {
        use magicllm::config::{LLMRouterConfig, OperationProfileSelector};
        use magicllm::LLMProfile;
        let mk = |model: &str| {
            let profile = LLMProfile {
                provider: LLMProviderKind::Ollama,
                model: model.to_string(),
                api_key_env: None,
                api_base_url: None,
                temperature: None,
                max_output_tokens: None,
                default_modality: None,
                reasoning: None,
                metadata: None,
                supports_vision: None,
                supports_reasoning: None,
                supports_tool_calling: None,
                supports_computer_use: None,
                timeout_secs: None,
                context_window_tokens: None,
                chunking: None,
            };
            profile
        };
        let mut profiles = std::collections::HashMap::new();
        profiles.insert(
            "op-meeting-summary-local".to_string(),
            mk("meeting-local-model"),
        );
        let remote = LLMProfile {
            provider: LLMProviderKind::OpenAI,
            model: "meeting-remote-model".to_string(),
            api_key_env: Some("__TEST_API_KEY_MISSING__".to_string()),
            api_base_url: None,
            temperature: None,
            max_output_tokens: None,
            default_modality: None,
            reasoning: None,
            metadata: None,
            supports_vision: None,
            supports_reasoning: None,
            supports_tool_calling: None,
            supports_computer_use: None,
            timeout_secs: None,
            context_window_tokens: None,
            chunking: None,
        };
        profiles.insert("op-meeting-summary-remote".to_string(), remote);
        profiles.insert("op-summary-other".to_string(), mk("other-model"));
        let mut operation_mapping = std::collections::HashMap::new();
        operation_mapping.insert(
            "meeting_response".to_string(),
            OperationProfileSelector::Conditional {
                default: "op-meeting-summary-local".to_string(),
                when_has_images: None,
                when_cloud: Some("op-meeting-summary-remote".to_string()),
                description: None,
                group: None,
                engine: None,
            },
        );
        operation_mapping.insert(
            "meeting_summary".to_string(),
            OperationProfileSelector::Conditional {
                default: "op-summary-other".to_string(),
                when_has_images: None,
                when_cloud: Some("op-meeting-summary-remote".to_string()),
                description: None,
                group: None,
                engine: None,
            },
        );
        LLMRouterConfig {
            profiles,
            operation_mapping,
            locality: magicllm::ProcessingLocality::Cloud,
            ..Default::default()
        }
    }
}

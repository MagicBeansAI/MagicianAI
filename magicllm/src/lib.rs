//! Core abstractions for multi-provider large language model integration.
//!
//! This crate defines the shared request/response types, provider traits, and
//! configuration primitives used across the Magician and Magictunnel projects.

pub mod bootstrap;
pub mod capability;
pub mod catalog;
pub mod chunking;
pub mod config;
pub mod context_reuse;
pub mod dispatch;
pub mod error;
pub mod model_discovery;
mod ollama_keep_alive;
pub mod pricing;
pub mod pricing_file;
pub mod provider;
pub mod providers;
pub mod realtime;
pub mod router;
pub mod server_web_search;
pub mod speech_voices;
pub mod spend_gate;
pub mod tool_schema;
pub mod trace;
pub mod types;

#[cfg(test)]
mod resource_guard_tests;

pub use bootstrap::ConfiguredRouter;
pub use capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning};
pub use chunking::{
    calculate_effective_payload, plan_logical_request, preflight_request, validate_logical_context,
    validate_plan_completeness, ChunkBudget, ChunkDescriptor, ChunkDomainAdapter, ChunkError,
    ChunkPlan, ChunkValidationError, ConservativeOllamaEstimator, ContextError, ContextPreflight,
    ContextPreflightMode, FinalValidationContract, LogicalItem, LogicalItemIdentity,
    LogicalLlmRequest, ReductionContext, ReductionPlan, ReductionRequest, ReductionStrategy,
    TokenEstimate, TokenEstimator, ValidatedChunkOutput,
};
pub use config::{
    ChunkFallbackPolicy, ChunkingConfig, LLMProfile, LLMRouterConfig, LlmConfig,
    ProcessingLocality, RealtimeVoiceConfig, RealtimeVoiceMode, RealtimeVoiceProfile,
    ReasoningDefaults, DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS,
};
pub use context_reuse::{
    stable_prefix_fingerprint, strategy_for_provider, transport_cohort_fingerprint,
    ContextReuseConfig, ContextReuseStrategy,
};
pub use dispatch::{
    AttemptError, AttemptHistory, BreakerConfig, CancellationToken, DispatchCapacityError,
    DispatchCapacityPlan, DispatchConfig, DispatchEngine, DispatchedResponse, ErrorClass, EventBus,
    JobId, JobMeta, JobOrigin, JobRegistry, JobState, LlmCallLedgerEvent, LlmDispatchQueue, LlmJob,
    LlmQueueEvent, LlmStreamJob, LocalPrepCallStat, LocalPrepConfig, LocalPrepStat,
    NoopTaskLedgerSink, NoopTaskStateView, Priority, ProviderConcurrencyConfig, ProviderQuota,
    ProviderQuotaMap, QueueSnapshot, RetryConfig, ShutdownStats, TaskLedgerSink, TaskRef,
    TaskSnapshot, TaskStateView, TokenSummary, TombstoneReason,
};
pub use error::{LLMError, LLMResult, SPEND_ENVELOPE_EXHAUSTED_MARKER};
pub use ollama_keep_alive::{
    request_keep_alive_value, set_default_ollama_keep_alive, DEFAULT_OLLAMA_KEEP_ALIVE,
};
pub use pricing::{
    active_table, compute_cost, compute_cost_at, compute_cost_with, compute_cost_with_at,
    compute_cost_with_server_web_search_at, compute_media_cost_at, compute_realtime_cost,
    compute_realtime_cost_at, estimate_unpriced_cost_microusd, install_pricing_file,
    install_pricing_table, media_pricing_version_at, model_is_free_at, model_is_priced_at,
    realtime_is_duration_billed_at, server_web_search_cost_per_call_at, try_compute_cost_at,
    try_compute_cost_with_at, LongContextPricing, MediaKind, MediaPricing, MediaPricingRow,
    MediaUsage, PricingRow, PricingTable, ProviderPricing, RealtimePricing, RealtimePricingRow,
    WebSearchPricingRow,
};
pub use pricing_file::{parse_pricing_file, PricingFileRows, SEED_PRICING_JSON};
pub use provider::LLMProvider;
pub use providers::{
    AnthropicMessagesProvider, DeepSeekProvider, GeminiProvider, MinimaxProvider, OllamaProvider,
    OpenAIChatProvider, OpenAIResponsesProvider, OpenRouterProvider, SarvamProvider, XaiProvider,
    YutoriN1Provider,
};
pub use router::MultiLLMRouter;
pub use spend_gate::{
    install_spend_gate, installed_spend_gate, SpendGate, SpendSettlement,
    DEFAULT_OUTPUT_CEILING_TOKENS,
};
pub use trace::{
    LlmCallRole, LlmParentRelation, LlmScope, LlmScopeResolution, LlmTraceContext, LlmTraceReceipt,
    LlmWorkloadClass,
};
pub use types::{
    anthropic_raw_content_block, as_anthropic_raw_content_block, current_physical_resource_guard,
    current_stream_event_sink, scoped_stream_event_sink, split_on_cache_sentinel,
    with_physical_resource_guard, ContentBlock, EmbeddingOptions, EmbeddingRequest,
    EmbeddingResponse, LLMMessage, LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall,
    LLMToolResult, LLMToolSpec, LlmContentCaptureEvent, LlmContentCaptureObserver,
    LlmContentCaptureSink, LlmDisclosureAuthorizer, LlmDisclosureCapturePolicy,
    LlmDisclosureCheckpoint, LlmDisclosureGuard, LlmPhysicalAttemptObservation,
    LlmPhysicalAttemptOutcome, LlmPhysicalAttemptPermit, LlmPhysicalAttemptPlan,
    LlmPhysicalAttemptStart, LlmPhysicalResourceAuthorizer, LlmPhysicalResourceGuard,
    LlmRouteIdentity, MediaContent, MessageRole, ReasoningConfig, RequestMetadata, StreamDelta,
    StreamEventSink, SummarisableBlock, SummarisationPurpose, TokenUsage,
    ANTHROPIC_RAW_CONTENT_BLOCK_MARKER, CACHE_BREAKPOINT_SENTINEL, CACHE_SEGMENT_SENTINEL,
};

/// Convenience exports for downstream crates.
pub mod prelude {
    pub use crate::bootstrap::ConfiguredRouter;
    pub use crate::capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning};
    pub use crate::chunking::{
        calculate_effective_payload, plan_logical_request, preflight_request,
        validate_logical_context, validate_plan_completeness, ChunkBudget, ChunkDescriptor,
        ChunkDomainAdapter, ChunkError, ChunkPlan, ChunkValidationError,
        ConservativeOllamaEstimator, ContextError, ContextPreflight, ContextPreflightMode,
        FinalValidationContract, LogicalItem, LogicalItemIdentity, LogicalLlmRequest,
        ReductionContext, ReductionPlan, ReductionRequest, ReductionStrategy, TokenEstimate,
        TokenEstimator, ValidatedChunkOutput,
    };
    pub use crate::config::{
        ChunkFallbackPolicy, ChunkingConfig, LLMProfile, LLMRouterConfig, LlmConfig,
        ProcessingLocality, RealtimeVoiceConfig, RealtimeVoiceMode, RealtimeVoiceProfile,
        ReasoningDefaults, DEFAULT_CONTEXT_SAFETY_MARGIN_TOKENS,
    };
    pub use crate::context_reuse::{
        stable_prefix_fingerprint, strategy_for_provider, transport_cohort_fingerprint,
        ContextReuseConfig, ContextReuseStrategy,
    };
    pub use crate::error::{LLMError, LLMResult};
    pub use crate::provider::LLMProvider;
    pub use crate::providers::{
        AnthropicMessagesProvider, DeepSeekProvider, GeminiProvider, MinimaxProvider,
        OllamaProvider, OpenAIChatProvider, OpenAIResponsesProvider, OpenRouterProvider,
        SarvamProvider, XaiProvider, YutoriN1Provider,
    };
    pub use crate::realtime::{
        build_realtime_provider, AudioStreamChannel, GeminiLiveProvider, OpenAiRealtimeProvider,
        RealtimeAudioControl, RealtimeAudioTopology, RealtimeProvider, RealtimeProviderError,
        RealtimeProviderEvent, RealtimeProviderKind, RealtimeSessionDescriptor,
        RealtimeSpeechSegment,
    };
    pub use crate::router::MultiLLMRouter;
    pub use crate::trace::{
        LlmCallRole, LlmParentRelation, LlmScope, LlmScopeResolution, LlmTraceContext,
        LlmTraceReceipt, LlmWorkloadClass,
    };
    pub use crate::types::{
        anthropic_raw_content_block, as_anthropic_raw_content_block, ContentBlock,
        EmbeddingOptions, EmbeddingRequest, EmbeddingResponse, LLMMessage, LLMRequest, LLMResponse,
        LLMResponseFormat, LLMToolCall, LLMToolResult, LLMToolSpec, LlmContentCaptureEvent,
        LlmContentCaptureObserver, LlmContentCaptureSink, LlmDisclosureAuthorizer,
        LlmDisclosureCapturePolicy, LlmDisclosureCheckpoint, LlmDisclosureGuard, LlmRouteIdentity,
        MediaContent, MessageRole, PromptCacheConfig, ReasoningConfig, RequestMetadata,
        StreamDelta, TokenUsage, ANTHROPIC_RAW_CONTENT_BLOCK_MARKER,
    };
}

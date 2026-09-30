use crate::capability::{LLMModality, LLMProviderKind};
use crate::error::{LLMError, LLMResult};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex, Weak,
};

use crate::trace::{LlmScope, LlmTraceContext, LlmWorkloadClass};

/// Line inserted into prompt templates to mark the stable/volatile boundary
/// for prompt-cache placement. All magicllm providers strip this line before
/// sending text upstream. Anthropic-family providers additionally split on
/// it and emit two content blocks with `cache_control` on the first, so that
/// Anthropic's cache anchors on the stable prefix instead of the volatile
/// tail of the message.
pub const CACHE_BREAKPOINT_SENTINEL: &str = "<!--MAGICIAN_CACHE_BREAKPOINT-->";

/// Marker used inside `ContentBlock::Json` to preserve provider-native
/// Anthropic-family content blocks that must round-trip exactly, such as
/// `thinking` blocks preceding tool calls.
pub const ANTHROPIC_RAW_CONTENT_BLOCK_MARKER: &str = "_magicllm_anthropic_content_block";

/// Wrap an Anthropic-family raw content block in the provider-agnostic content
/// enum without flattening it to text.
pub fn anthropic_raw_content_block(block: Value) -> ContentBlock {
    let mut wrapper = Map::new();
    wrapper.insert(
        ANTHROPIC_RAW_CONTENT_BLOCK_MARKER.to_string(),
        Value::Bool(true),
    );
    wrapper.insert("block".to_string(), block);
    ContentBlock::Json {
        value: Value::Object(wrapper),
    }
}

/// Return the preserved Anthropic-family raw content block, when this JSON
/// value was produced by `anthropic_raw_content_block`.
pub fn as_anthropic_raw_content_block(value: &Value) -> Option<&Value> {
    value
        .get(ANTHROPIC_RAW_CONTENT_BLOCK_MARKER)
        .and_then(Value::as_bool)
        .filter(|enabled| *enabled)
        .and_then(|_| value.get("block"))
}

/// Split `text` on the first occurrence of `CACHE_BREAKPOINT_SENTINEL` and
/// return `(prefix, Some(suffix))` with the sentinel line removed from both
/// parts. Returns `(original, None)` when the sentinel is absent so the
/// caller can fast-path non-sentinel prompts without mutation.
///
/// The sentinel is expected to sit on its own line. The returned prefix is
/// everything up to (but not including) the `\n` that immediately precedes
/// the sentinel when present, or up to the sentinel itself if it is at
/// column 0. The suffix starts immediately after the newline that follows
/// the sentinel, or at the end of input when the sentinel is the last line.
pub fn split_on_cache_sentinel(text: &str) -> (String, Option<String>) {
    let Some(sentinel_start) = text.find(CACHE_BREAKPOINT_SENTINEL) else {
        return (text.to_string(), None);
    };
    let sentinel_end = sentinel_start + CACHE_BREAKPOINT_SENTINEL.len();

    // Trim the trailing '\n' that preceded the sentinel line, if any.
    // Compare on the raw byte so the helper is safe when a multi-byte UTF-8
    // char (e.g. `é`, emoji, CJK) sits immediately before the sentinel —
    // str-slicing `text[sentinel_start - 1..sentinel_start]` would panic on
    // a non-char-boundary byte index.
    let prefix_end = if sentinel_start > 0 && text.as_bytes()[sentinel_start - 1] == b'\n' {
        sentinel_start - 1
    } else {
        sentinel_start
    };

    // Skip the trailing '\n' that follows the sentinel, if any.
    let suffix_start = if text[sentinel_end..].starts_with('\n') {
        sentinel_end + 1
    } else {
        sentinel_end
    };

    (
        text[..prefix_end].to_string(),
        Some(text[suffix_start..].to_string()),
    )
}

/// Actor role in a request/response message exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
    Tool,
}

/// Content blocks included in a message.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text content.
    Text { text: String },
    /// Inline image data.
    Image {
        data: Vec<u8>,
        media_type: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caption: Option<String>,
    },
    /// Reference to an image accessible via URL.
    ImageUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prompt: Option<String>,
    },
    /// Represents a tool invocation returned by the assistant.
    ToolCall {
        id: String,
        name: String,
        arguments: Value,
    },
    /// Represents the result of a tool invocation supplied by the caller.
    ToolResult {
        tool_call_id: String,
        content: Value,
    },
    /// Opaque JSON payload for structured responses.
    Json { value: Value },
}

/// Purpose tag for `SummarisableBlock` — drives prompt template selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummarisationPurpose {
    /// Episode body fed into memory consolidation.
    ConsolidationEpisode,
    /// Tool result body in an inner-loop turn.
    LargeStepOutput,
    /// Anything else.
    Other,
}

impl Default for SummarisationPurpose {
    fn default() -> Self {
        Self::Other
    }
}

impl SummarisationPurpose {
    /// Canonical name used in metrics + prompt template lookup.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ConsolidationEpisode => "consolidation_episode",
            Self::LargeStepOutput => "large_step_output",
            Self::Other => "other",
        }
    }
}

/// One summarisable block attached to an `LLMRequest`. Lives outside the
/// `ContentBlock` enum so providers don't need to grow a new variant.
///
/// Caller pattern: put a small placeholder in
/// `messages[message_index].content[content_index]` (or leave the slot empty)
/// and stash the real text in `raw`. The dispatch queue's local-prep step
/// replaces the placeholder with either an Ollama-generated summary (when
/// local-prep is enabled and succeeds) or the raw text itself (fallback).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummarisableBlock {
    /// Index of the message in `LLMRequest.messages`.
    pub message_index: usize,
    /// Index of the content block in `messages[message_index].content`.
    pub content_index: usize,
    /// Raw text — local-prep summarises this and inlines the result.
    pub raw: String,
    /// Purpose tag selecting the prompt template.
    #[serde(default)]
    pub purpose: SummarisationPurpose,
    /// Optional cap on summary length. `None` uses the dispatch config default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_chars: Option<usize>,
}

/// Convenience content constructor helpers.
impl ContentBlock {
    /// Creates a text content block from a string-like value.
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// Structured media payload (e.g., screenshots) used alongside messages.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaContent {
    pub media_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<u8>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Specification for a callable tool/function exposed to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LLMToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

/// Tool invocation returned by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
}

/// Tool execution result supplied back to the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMToolResult {
    pub tool_call_id: String,
    pub output: Value,
}

/// Describes the expected output structure from a model invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LLMResponseFormat {
    Text,
    JsonObject,
    JsonSchema { schema: Value },
}

/// Reasoning configuration for advanced models.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ReasoningConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    /// Optional reasoning-summary mode for providers that emit a
    /// separate summary stream (currently OpenAI Responses: `auto` /
    /// `concise` / `detailed`). Other providers ignore this field.
    /// Defaults to `auto` at the provider layer when reasoning is
    /// enabled and this field is unset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

impl ReasoningConfig {
    pub fn effort_disables_reasoning(effort: &str) -> bool {
        matches!(
            effort.trim().to_ascii_lowercase().as_str(),
            "none" | "off" | "no" | "disabled" | "disable" | "false"
        )
    }

    pub fn is_disabled(&self) -> bool {
        self.effort
            .as_deref()
            .map(Self::effort_disables_reasoning)
            .unwrap_or(false)
    }
}

/// Provider-aware prompt caching configuration.
///
/// `None` on `LLMRequest.prompt_cache` means "use the provider's default behavior":
/// - Anthropic: enable automatic prompt caching via top-level `cache_control`
/// - OpenAI: no request change required; caching is automatic upstream
/// - Gemini: no request change required for implicit caching on supported models
/// - OpenRouter: no request change by default to avoid forcing provider-specific routing
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum PromptCacheConfig {
    Enabled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttl: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cached_content: Option<String>,
    },
    Disabled,
}

impl PromptCacheConfig {
    pub fn enabled() -> Self {
        Self::Enabled {
            ttl: None,
            cached_content: None,
        }
    }

    pub fn enabled_with_ttl(ttl: impl Into<String>) -> Self {
        Self::Enabled {
            ttl: Some(ttl.into()),
            cached_content: None,
        }
    }

    pub fn enabled_with_cached_content(name: impl Into<String>) -> Self {
        Self::Enabled {
            ttl: None,
            cached_content: Some(name.into()),
        }
    }

    pub fn ttl(&self) -> Option<&str> {
        match self {
            Self::Enabled { ttl, .. } => ttl.as_deref(),
            Self::Disabled => None,
        }
    }

    pub fn cached_content(&self) -> Option<&str> {
        match self {
            Self::Enabled { cached_content, .. } => cached_content.as_deref(),
            Self::Disabled => None,
        }
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::Enabled { .. })
    }
}

/// Metadata supplied with each request for tracing and routing.
///
/// Content observers are deliberately in-process and local-only. They let an
/// embedding runtime capture the logical request, each provider-effective
/// request, and the normalized response without teaching provider adapters
/// about persistence, policy, or redaction. Implementations must return
/// quickly; durable/sanitized capture belongs on a bounded background lane.
pub enum LlmContentCaptureEvent<'a> {
    LogicalRequest {
        request: &'a LLMRequest,
    },
    EffectiveRequest {
        request: &'a LLMRequest,
        profile: &'a str,
        provider: &'a str,
        provider_attempt_index: u32,
    },
    NormalizedResponse {
        response: &'a LLMResponse,
        trace_context: Option<&'a LlmTraceContext>,
        operation: &'a str,
        profile: &'a str,
        provider: &'a str,
        model: &'a str,
        provider_attempt_index: u32,
    },
}

pub trait LlmContentCaptureObserver: Send + Sync + 'static {
    fn observe(&self, event: LlmContentCaptureEvent<'_>);
}

#[derive(Clone, Default)]
pub struct LlmContentCaptureSink(
    pub Option<Arc<dyn LlmContentCaptureObserver + Send + Sync + 'static>>,
);

impl LlmContentCaptureSink {
    pub fn new(observer: Arc<dyn LlmContentCaptureObserver + Send + Sync + 'static>) -> Self {
        Self(Some(observer))
    }

    pub fn observe(&self, event: LlmContentCaptureEvent<'_>) {
        if let Some(observer) = self.0.as_ref() {
            observer.observe(event);
        }
    }

    pub fn is_attached(&self) -> bool {
        self.0.is_some()
    }
}

/// Payload-capture posture for a disclosure-bound request.
///
/// Disclosure-bound request/response bodies must not enter debug, trace or
/// eval capture merely because a general observer is installed. A future
/// protected-retention transport must introduce a distinct, non-forgeable
/// permit instead of adding a permissive flag here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmDisclosureCapturePolicy {
    MetadataOnly,
}

/// Serializable, content-free marker stored beside paused labeled content.
/// It is evidence for exact re-admission comparison only and never grants
/// provider authority by itself.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LlmDisclosureCheckpoint {
    expected_profile: String,
    expected_transport_cohort: String,
    continuation_partition: String,
    policy_digest: String,
}

impl LlmDisclosureCheckpoint {
    pub fn matches_guard(&self, guard: &LlmDisclosureGuard) -> bool {
        self.expected_profile == guard.expected_profile()
            && self.expected_transport_cohort == guard.expected_transport_cohort()
            && self.continuation_partition == guard.continuation_partition()
            && self.policy_digest == guard.policy_digest()
    }
}

/// Final, content-free revalidation hook for a disclosure-bound provider call.
///
/// The transport router owns physical route validation, while the embedding
/// service owns mutable authority (grant/lifecycle/policy) state. This hook is
/// invoked immediately before every provider attempt and receives route
/// metadata only; it must not retain or inspect prompt bytes.
#[async_trait::async_trait]
pub trait LlmDisclosureAuthorizer: std::fmt::Debug + Send + Sync {
    async fn revalidate(
        &self,
        profile: &str,
        provider: &LLMProviderKind,
        model: &str,
        api_base_url: Option<&str>,
    ) -> Result<(), String>;
}

#[derive(Debug)]
struct ChainedLlmDisclosureAuthorizer {
    first: Arc<dyn LlmDisclosureAuthorizer>,
    second: Arc<dyn LlmDisclosureAuthorizer>,
}

#[async_trait::async_trait]
impl LlmDisclosureAuthorizer for ChainedLlmDisclosureAuthorizer {
    async fn revalidate(
        &self,
        profile: &str,
        provider: &LLMProviderKind,
        model: &str,
        api_base_url: Option<&str>,
    ) -> Result<(), String> {
        self.first
            .revalidate(profile, provider, model, api_base_url)
            .await?;
        self.second
            .revalidate(profile, provider, model, api_base_url)
            .await
    }
}

/// Content-free identity and conservative resource ceiling for one exact
/// physical provider attempt. This is runtime control-plane state: it is not
/// serializable and cannot be supplied by request JSON or queue persistence.
#[derive(Clone, PartialEq, Eq)]
pub struct LlmPhysicalAttemptPlan {
    llm_call_id: String,
    task_id: String,
    root_execution_id: String,
    execution_id: String,
    iteration_id: Option<String>,
    profile: String,
    provider: LLMProviderKind,
    model: String,
    transport_cohort: String,
    pricing_version: String,
    effective_request_digest: String,
    attempt_index: u32,
    input_token_upper: u64,
    cached_input_token_upper: u64,
    output_token_upper: u64,
    cost_upper_microusd: u64,
    started_at_unix_ms: i64,
    timeout_secs: u64,
}

impl std::fmt::Debug for LlmPhysicalAttemptPlan {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmPhysicalAttemptPlan")
            .field("llm_call_id", &self.llm_call_id)
            .field("task_id", &self.task_id)
            .field("root_execution_id", &self.root_execution_id)
            .field("execution_id", &self.execution_id)
            .field("iteration_id", &self.iteration_id)
            .field("profile", &self.profile)
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("transport_cohort", &self.transport_cohort)
            .field("pricing_version", &self.pricing_version)
            .field("effective_request_digest", &self.effective_request_digest)
            .field("attempt_index", &self.attempt_index)
            .field("input_token_upper", &self.input_token_upper)
            .field("cached_input_token_upper", &self.cached_input_token_upper)
            .field("output_token_upper", &self.output_token_upper)
            .field("cost_upper_microusd", &self.cost_upper_microusd)
            .field("started_at_unix_ms", &self.started_at_unix_ms)
            .field("timeout_secs", &self.timeout_secs)
            .finish()
    }
}

impl LlmPhysicalAttemptPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_router(
        llm_call_id: String,
        task_id: String,
        root_execution_id: String,
        execution_id: String,
        iteration_id: Option<String>,
        profile: String,
        provider: LLMProviderKind,
        model: String,
        transport_cohort: String,
        pricing_version: String,
        effective_request_digest: String,
        attempt_index: u32,
        input_token_upper: u64,
        cached_input_token_upper: u64,
        output_token_upper: u64,
        cost_upper_microusd: u64,
        started_at_unix_ms: i64,
        timeout_secs: u64,
    ) -> Result<Self, String> {
        for (field, value) in [
            ("llm_call_id", llm_call_id.as_str()),
            ("task_id", task_id.as_str()),
            ("root_execution_id", root_execution_id.as_str()),
            ("execution_id", execution_id.as_str()),
            ("profile", profile.as_str()),
            ("model", model.as_str()),
            ("transport_cohort", transport_cohort.as_str()),
            ("pricing_version", pricing_version.as_str()),
        ] {
            if value.is_empty()
                || value.len() > 512
                || value.bytes().any(|byte| byte.is_ascii_control())
            {
                return Err(format!("invalid physical-attempt {field}"));
            }
        }
        if effective_request_digest.len() != 64
            || !effective_request_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err("invalid physical-attempt effective request digest".to_owned());
        }
        if attempt_index == 0
            || input_token_upper == 0
            || output_token_upper == 0
            || cached_input_token_upper > input_token_upper
            || timeout_secs == 0
        {
            return Err("invalid physical-attempt resource ceiling".to_owned());
        }
        Ok(Self {
            llm_call_id,
            task_id,
            root_execution_id,
            execution_id,
            iteration_id,
            profile,
            provider,
            model,
            transport_cohort,
            pricing_version,
            effective_request_digest,
            attempt_index,
            input_token_upper,
            cached_input_token_upper,
            output_token_upper,
            cost_upper_microusd,
            started_at_unix_ms,
            timeout_secs,
        })
    }

    pub fn llm_call_id(&self) -> &str {
        &self.llm_call_id
    }
    pub fn task_id(&self) -> &str {
        &self.task_id
    }
    pub fn root_execution_id(&self) -> &str {
        &self.root_execution_id
    }
    pub fn execution_id(&self) -> &str {
        &self.execution_id
    }
    pub fn iteration_id(&self) -> Option<&str> {
        self.iteration_id.as_deref()
    }
    pub fn profile(&self) -> &str {
        &self.profile
    }
    pub fn provider(&self) -> &LLMProviderKind {
        &self.provider
    }
    pub fn model(&self) -> &str {
        &self.model
    }
    pub fn transport_cohort(&self) -> &str {
        &self.transport_cohort
    }
    pub fn pricing_version(&self) -> &str {
        &self.pricing_version
    }
    pub fn effective_request_digest(&self) -> &str {
        &self.effective_request_digest
    }
    pub fn attempt_index(&self) -> u32 {
        self.attempt_index
    }
    pub fn input_token_upper(&self) -> u64 {
        self.input_token_upper
    }
    pub fn cached_input_token_upper(&self) -> u64 {
        self.cached_input_token_upper
    }
    pub fn output_token_upper(&self) -> u64 {
        self.output_token_upper
    }
    pub fn cost_upper_microusd(&self) -> u64 {
        self.cost_upper_microusd
    }
    pub fn started_at_unix_ms(&self) -> i64 {
        self.started_at_unix_ms
    }
    pub fn timeout_secs(&self) -> u64 {
        self.timeout_secs
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LlmPhysicalAttemptOutcome {
    Committed,
    OutcomeUncertain,
}

/// Exact provider-boundary observation. It contains quantities and route
/// identity only; response text and prompt bytes never enter this carrier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmPhysicalAttemptObservation {
    outcome: LlmPhysicalAttemptOutcome,
    input_tokens: u64,
    cached_input_tokens: u64,
    output_tokens: u64,
    cost_microusd: u64,
    started_at_unix_ms: i64,
    completed_at_unix_ms: i64,
}

impl LlmPhysicalAttemptObservation {
    pub(crate) fn committed(
        input_tokens: u64,
        cached_input_tokens: u64,
        output_tokens: u64,
        cost_microusd: u64,
        started_at_unix_ms: i64,
        completed_at_unix_ms: i64,
    ) -> Result<Self, String> {
        if cached_input_tokens > input_tokens || completed_at_unix_ms < started_at_unix_ms {
            return Err("invalid physical-attempt observation".to_owned());
        }
        Ok(Self {
            outcome: LlmPhysicalAttemptOutcome::Committed,
            input_tokens,
            cached_input_tokens,
            output_tokens,
            cost_microusd,
            started_at_unix_ms,
            completed_at_unix_ms,
        })
    }

    pub(crate) fn outcome_uncertain(started_at_unix_ms: i64, completed_at_unix_ms: i64) -> Self {
        Self {
            outcome: LlmPhysicalAttemptOutcome::OutcomeUncertain,
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            cost_microusd: 0,
            started_at_unix_ms,
            completed_at_unix_ms: completed_at_unix_ms.max(started_at_unix_ms),
        }
    }

    pub fn outcome(&self) -> LlmPhysicalAttemptOutcome {
        self.outcome
    }
    pub fn input_tokens(&self) -> u64 {
        self.input_tokens
    }
    pub fn cached_input_tokens(&self) -> u64 {
        self.cached_input_tokens
    }
    pub fn output_tokens(&self) -> u64 {
        self.output_tokens
    }
    pub fn cost_microusd(&self) -> u64 {
        self.cost_microusd
    }
    pub fn started_at_unix_ms(&self) -> i64 {
        self.started_at_unix_ms
    }
    pub fn completed_at_unix_ms(&self) -> i64 {
        self.completed_at_unix_ms
    }
}

#[async_trait::async_trait]
pub trait LlmPhysicalAttemptPermit: std::fmt::Debug + Send {
    /// Complete fallible resource-lock/expiry readiness before the final
    /// disclosure revalidation. This does not claim provider I/O began and
    /// therefore remains eligible for proven-unspent release.
    async fn mark_dispatched(&mut self) -> Result<(), String>;

    /// Synchronously bind the exact physical-I/O start after every awaited
    /// gate. The caller must invoke the provider immediately after this mark.
    /// Keeping this method synchronous makes an implementation incapable of
    /// opening a fresh authority/expiry race after the final disclosure check.
    fn mark_physical_started(&mut self) -> Result<LlmPhysicalAttemptStart, String>;

    /// Consume a reservation when a synchronous gate proves provider I/O did
    /// not begin. Once `mark_physical_started` succeeds this path fails closed.
    async fn release_pre_io(self: Box<Self>, released_at_unix_ms: i64) -> Result<(), String>;

    async fn settle(
        self: Box<Self>,
        observation: LlmPhysicalAttemptObservation,
    ) -> Result<(), String>;
}

/// Exact final physical-attempt start and its remaining absolute resource
/// window. This is runtime-only control-plane state: the provider request
/// still owns its reviewed timeout, while the router additionally cancels the
/// attempt when the earlier resource deadline is reached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LlmPhysicalAttemptStart {
    started_at_unix_ms: i64,
    remaining_resource_ms: u64,
}

impl LlmPhysicalAttemptStart {
    pub fn from_resource_permit(
        started_at_unix_ms: i64,
        remaining_resource_ms: u64,
    ) -> Result<Self, String> {
        if started_at_unix_ms <= 0 || remaining_resource_ms == 0 {
            return Err("invalid physical-attempt resource deadline".to_owned());
        }
        Ok(Self {
            started_at_unix_ms,
            remaining_resource_ms,
        })
    }

    pub fn started_at_unix_ms(self) -> i64 {
        self.started_at_unix_ms
    }

    pub fn remaining_resource_ms(self) -> u64 {
        self.remaining_resource_ms
    }
}

#[async_trait::async_trait]
pub trait LlmPhysicalResourceAuthorizer: std::fmt::Debug + Send + Sync {
    async fn reserve(
        &self,
        plan: LlmPhysicalAttemptPlan,
    ) -> Result<Box<dyn LlmPhysicalAttemptPermit>, String>;
}

/// Non-serializable, exact route and continuation fence for labeled content.
///
/// This value is local control-plane state. Provider adapters never receive it
/// and request JSON cannot mint it through deserialization. The transport
/// router checks the expected profile and physical cohort immediately before
/// every provider attempt, including fallbacks and streaming calls.
#[derive(Clone)]
pub struct LlmDisclosureGuard {
    expected_profile: Arc<str>,
    expected_transport_cohort: Arc<str>,
    continuation_partition: Arc<str>,
    policy_digest: Arc<str>,
    capture_policy: LlmDisclosureCapturePolicy,
    authorizer: Arc<dyn LlmDisclosureAuthorizer>,
    physical_resource_authorizer: Option<Arc<dyn LlmPhysicalResourceAuthorizer>>,
}

impl std::fmt::Debug for LlmDisclosureGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmDisclosureGuard")
            .field("expected_profile", &self.expected_profile)
            .field("expected_transport_cohort", &self.expected_transport_cohort)
            .field("continuation_partition", &self.continuation_partition)
            .field("policy_digest", &self.policy_digest)
            .field("capture_policy", &self.capture_policy)
            // The authorizer may retain current authenticated scope, resolved
            // authority, and physical profile configuration. It is executable
            // control-plane state, not diagnostic payload.
            .field("authorizer", &"<opaque>")
            .field(
                "physical_resource_authorizer",
                &self
                    .physical_resource_authorizer
                    .as_ref()
                    .map(|_| "<opaque>"),
            )
            .finish()
    }
}

impl LlmDisclosureGuard {
    const MAX_IDENTITY_BYTES: usize = 256;

    pub fn new(
        expected_profile: impl Into<String>,
        expected_transport_cohort: impl Into<String>,
        continuation_partition: impl Into<String>,
        policy_digest: impl Into<String>,
        capture_policy: LlmDisclosureCapturePolicy,
        authorizer: Arc<dyn LlmDisclosureAuthorizer>,
    ) -> Result<Self, String> {
        let expected_profile = expected_profile.into();
        let expected_transport_cohort = expected_transport_cohort.into();
        let continuation_partition = continuation_partition.into();
        let policy_digest = policy_digest.into();
        for (field, value) in [
            ("expected_profile", expected_profile.as_str()),
            (
                "expected_transport_cohort",
                expected_transport_cohort.as_str(),
            ),
            ("continuation_partition", continuation_partition.as_str()),
            ("policy_digest", policy_digest.as_str()),
        ] {
            if value.is_empty() || value.len() > Self::MAX_IDENTITY_BYTES {
                return Err(format!(
                    "{field} must contain 1..={} bytes",
                    Self::MAX_IDENTITY_BYTES
                ));
            }
            if value.bytes().any(|byte| byte.is_ascii_control()) {
                return Err(format!("{field} must not contain control bytes"));
            }
        }
        if expected_transport_cohort.len() != 64
            || !expected_transport_cohort
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(
                "expected_transport_cohort must be a lowercase 64-byte hex digest".to_string(),
            );
        }
        Ok(Self {
            expected_profile: Arc::from(expected_profile),
            expected_transport_cohort: Arc::from(expected_transport_cohort),
            continuation_partition: Arc::from(continuation_partition),
            policy_digest: Arc::from(policy_digest),
            capture_policy,
            authorizer,
            physical_resource_authorizer: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn new_with_physical_resource_authorizer(
        expected_profile: impl Into<String>,
        expected_transport_cohort: impl Into<String>,
        continuation_partition: impl Into<String>,
        policy_digest: impl Into<String>,
        capture_policy: LlmDisclosureCapturePolicy,
        authorizer: Arc<dyn LlmDisclosureAuthorizer>,
        physical_resource_authorizer: Arc<dyn LlmPhysicalResourceAuthorizer>,
    ) -> Result<Self, String> {
        let mut guard = Self::new(
            expected_profile,
            expected_transport_cohort,
            continuation_partition,
            policy_digest,
            capture_policy,
            authorizer,
        )?;
        guard.physical_resource_authorizer = Some(physical_resource_authorizer);
        Ok(guard)
    }

    pub fn expected_profile(&self) -> &str {
        &self.expected_profile
    }

    pub fn expected_transport_cohort(&self) -> &str {
        &self.expected_transport_cohort
    }

    pub fn continuation_partition(&self) -> &str {
        &self.continuation_partition
    }

    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    pub fn capture_policy(&self) -> LlmDisclosureCapturePolicy {
        self.capture_policy
    }

    pub fn checkpoint(&self) -> LlmDisclosureCheckpoint {
        LlmDisclosureCheckpoint {
            expected_profile: self.expected_profile.to_string(),
            expected_transport_cohort: self.expected_transport_cohort.to_string(),
            continuation_partition: self.continuation_partition.to_string(),
            policy_digest: self.policy_digest.to_string(),
        }
    }

    /// Retain every existing disclosure/resource fence while adding one more
    /// live authority check at the physical provider boundary. This is used
    /// by narrower product lanes (for example an admitted app operation) that
    /// must intersect their mutable policy with an already-admitted labeled
    /// content guard rather than replacing it.
    pub fn with_additional_authorizer(
        mut self,
        authorizer: Arc<dyn LlmDisclosureAuthorizer>,
    ) -> Self {
        self.authorizer = Arc::new(ChainedLlmDisclosureAuthorizer {
            first: self.authorizer,
            second: authorizer,
        });
        self
    }

    pub async fn revalidate(
        &self,
        profile: &str,
        provider: &LLMProviderKind,
        model: &str,
        api_base_url: Option<&str>,
    ) -> Result<(), String> {
        self.authorizer
            .revalidate(profile, provider, model, api_base_url)
            .await
    }

    pub fn requires_physical_resource_authority(&self) -> bool {
        self.physical_resource_authorizer.is_some()
    }

    pub async fn reserve_physical_attempt(
        &self,
        plan: LlmPhysicalAttemptPlan,
    ) -> Result<Box<dyn LlmPhysicalAttemptPermit>, String> {
        self.physical_resource_authorizer
            .as_ref()
            .ok_or_else(|| "physical resource authority is unavailable".to_owned())?
            .reserve(plan)
            .await
    }

    fn retained_bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            .saturating_add(self.expected_profile.len())
            .saturating_add(self.expected_transport_cohort.len())
            .saturating_add(self.continuation_partition.len())
            .saturating_add(self.policy_digest.len())
    }
}

impl std::fmt::Debug for LlmContentCaptureSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LlmContentCaptureSink")
            .field("attached", &self.is_attached())
            .finish()
    }
}

#[derive(Debug, Clone, Copy)]
enum JsonLaneAdmission {
    Unknown,
    Admitted(usize),
    Rejected,
}

impl Default for JsonLaneAdmission {
    fn default() -> Self {
        Self::Unknown
    }
}

#[derive(Debug, Clone, Default)]
struct JsonAdmissionCache {
    messages: JsonLaneAdmission,
    messages_owner: Option<Weak<Vec<LLMMessage>>>,
    tools: JsonLaneAdmission,
    tools_owner: Option<Weak<Vec<LLMToolSpec>>>,
    response_format: JsonLaneAdmission,
    response_format_owner: Option<Weak<LLMResponseFormat>>,
    extra: JsonLaneAdmission,
    extra_owner: Option<Weak<Value>>,
    scan_passes: u32,
}

#[derive(Debug, Clone, Default)]
#[doc(hidden)]
pub struct JsonAdmissionAuthority {
    inner: Arc<Mutex<JsonAdmissionCache>>,
}

#[derive(Clone, Default)]
#[doc(hidden)]
pub struct JsonDropGuard {
    inner: Arc<Mutex<Vec<SafeJsonLaneOwner>>>,
}

impl std::fmt::Debug for JsonDropGuard {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("JsonDropGuard")
    }
}

enum SafeJsonLaneOwner {
    Messages(Option<Arc<Vec<LLMMessage>>>),
    Tools(Option<Arc<Vec<LLMToolSpec>>>),
    ResponseFormat(Option<Arc<LLMResponseFormat>>),
    Extra(Option<Arc<Value>>),
}

impl SafeJsonLaneOwner {
    fn has_external_owner(&self) -> bool {
        match self {
            Self::Messages(Some(owner)) => Arc::strong_count(owner) > 1,
            Self::Tools(Some(owner)) => Arc::strong_count(owner) > 1,
            Self::ResponseFormat(Some(owner)) => Arc::strong_count(owner) > 1,
            Self::Extra(Some(owner)) => Arc::strong_count(owner) > 1,
            _ => false,
        }
    }
}

impl Drop for SafeJsonLaneOwner {
    fn drop(&mut self) {
        match self {
            Self::Messages(owner) => {
                if let Some(owner) = owner.take() {
                    discard_shared_messages(owner);
                }
            },
            Self::Tools(owner) => {
                if let Some(owner) = owner.take() {
                    discard_shared_tools(owner);
                }
            },
            Self::ResponseFormat(owner) => {
                if let Some(owner) = owner.take() {
                    discard_shared_response_format(owner);
                }
            },
            Self::Extra(owner) => {
                if let Some(owner) = owner.take() {
                    discard_shared_extra(owner);
                }
            },
        }
    }
}

#[derive(Clone, Copy)]
enum JsonAdmissionLane {
    Messages,
    Tools,
    ResponseFormat,
    Extra,
}

#[derive(Clone, Copy, Default)]
struct JsonRejectedLanes {
    messages: bool,
    tools: bool,
    response_format: bool,
    extra: bool,
}

impl JsonAdmissionAuthority {
    fn invalidate_lane(&mut self, lane: JsonAdmissionLane) {
        let mut cache = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        match lane {
            JsonAdmissionLane::Messages => {
                cache.messages = JsonLaneAdmission::Unknown;
                cache.messages_owner = None;
            },
            JsonAdmissionLane::Tools => {
                cache.tools = JsonLaneAdmission::Unknown;
                cache.tools_owner = None;
            },
            JsonAdmissionLane::ResponseFormat => {
                cache.response_format = JsonLaneAdmission::Unknown;
                cache.response_format_owner = None;
            },
            JsonAdmissionLane::Extra => {
                cache.extra = JsonLaneAdmission::Unknown;
                cache.extra_owner = None;
            },
        }
        self.inner = Arc::new(Mutex::new(cache));
    }

    #[cfg(test)]
    fn scan_passes(&self) -> u32 {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scan_passes
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMetadata {
    #[serde(default)]
    pub operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Local-only product/call identity. Provider request serializers must not
    /// receive this field; only the legacy `trace_id` may be forwarded.
    #[serde(skip, default)]
    pub trace_context: Option<LlmTraceContext>,
    /// Shared, local-only count of physical provider invocations made while
    /// routing this logical call. Cloned requests retain the same counter so
    /// fallback profiles and dispatch retries contribute to one sequence.
    #[serde(skip, default)]
    pub provider_attempt_counter: Option<Arc<AtomicU32>>,
    /// Local-only normalized-content observer. Never serialized to providers
    /// or queue persistence and never treated as an authorization grant.
    #[serde(skip, default)]
    pub content_capture_sink: LlmContentCaptureSink,
    /// Local-only disclosure route/capture fence for labeled content. It is
    /// deliberately skipped by serde so queue persistence, provider payloads
    /// and model-controlled request JSON cannot manufacture authority.
    #[serde(skip, default)]
    pub disclosure_guard: Option<Arc<LlmDisclosureGuard>>,
    /// Local-only V1 fence used by disclosure-bound app calls. Provider
    /// adapters must not retry or recursively re-invoke while this is set;
    /// each physical attempt requires its own move-only resource permit.
    #[serde(skip, default)]
    pub single_physical_attempt: bool,
    /// Prevents the queue-backed path from reporting the post-local-prep
    /// request as a second logical request after its owner already reported
    /// the pre-prep request.
    #[serde(skip, default)]
    pub logical_content_capture_emitted: bool,
    /// Local proof that arbitrary JSON lanes have crossed bounded admission.
    /// Clones share the proof; COW mutation forks it and invalidates only the
    /// changed lane, so queue -> router does not rescan full history/catalogs.
    #[serde(skip, default)]
    #[doc(hidden)]
    pub json_admission: JsonAdmissionAuthority,
}

impl Default for RequestMetadata {
    fn default() -> Self {
        Self {
            operation: "default".to_string(),
            trace_id: None,
            timeout_secs: None,
            tags: None,
            trace_context: None,
            provider_attempt_counter: None,
            content_capture_sink: LlmContentCaptureSink::default(),
            disclosure_guard: None,
            single_physical_attempt: false,
            logical_content_capture_emitted: false,
            json_admission: JsonAdmissionAuthority::default(),
        }
    }
}

/// Ollama-shaped embedding request carried through the provider seam. Field
/// names mirror the wire body so the provider serializes without translation;
/// `keep_alive` is a pre-encoded `serde_json::Value` because Ollama requires
/// `-1`/`0` sentinels as JSON *numbers* while accepting duration strings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingRequest {
    pub model: String,
    pub inputs: Vec<String>,
    /// Must stay `false` for index writers: logical input splitting owns the
    /// complete-coverage contract, so an oversized input must error rather
    /// than be silently truncated.
    pub truncate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_alive: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<EmbeddingOptions>,
    /// Millisecond-precision request deadline. Takes precedence over
    /// `metadata.timeout_secs` (which quantizes to whole seconds); embedding
    /// callers carry sub-second remainder budgets that must survive the seam.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub metadata: RequestMetadata,
}

/// Ollama `options` passthrough for embedding requests.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct EmbeddingOptions {
    #[serde(rename = "num_ctx", default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    #[serde(rename = "num_batch", default, skip_serializing_if = "Option::is_none")]
    pub batch_tokens: Option<u32>,
}

/// Embedding response in Ollama wire shape (`{"embeddings": [[...], ...]}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingResponse {
    pub embeddings: Vec<Vec<f32>>,
}

impl RequestMetadata {
    pub fn set_disclosure_guard(&mut self, guard: LlmDisclosureGuard) {
        self.single_physical_attempt = guard.requires_physical_resource_authority();
        self.disclosure_guard = Some(Arc::new(guard));
    }

    pub fn disclosure_guard(&self) -> Option<&LlmDisclosureGuard> {
        self.disclosure_guard.as_deref()
    }

    /// Suppress body capture before the first logical capture event. Metadata
    /// telemetry remains available through the normal trace path.
    pub(crate) fn enforce_disclosure_capture_policy(&mut self) {
        if self
            .disclosure_guard()
            .is_some_and(|guard| guard.capture_policy() == LlmDisclosureCapturePolicy::MetadataOnly)
        {
            self.content_capture_sink = LlmContentCaptureSink::default();
        }
    }

    pub fn set_trace_context(&mut self, context: LlmTraceContext) {
        // The physical-attempt counter belongs to one logical call. Request
        // clones deliberately share it while fallback profiles and dispatch
        // retries retain the same call id, but a child/caller retry is a new
        // logical call and must begin again at attempt one.
        if self
            .trace_context
            .as_ref()
            .is_some_and(|current| current.llm_call_id != context.llm_call_id)
        {
            self.provider_attempt_counter = None;
        }
        self.trace_id = Some(context.trace_id.clone());
        self.trace_context = Some(context);
    }

    pub fn ensure_trace_context(
        &mut self,
        scope: Option<LlmScope>,
        workload_class: LlmWorkloadClass,
    ) -> LlmTraceContext {
        if let Some(existing) = self.trace_context.as_ref() {
            return existing.clone();
        }
        let context = match scope {
            Some(scope) => {
                let mut context = LlmTraceContext::new(scope, workload_class);
                if let Some(trace_id) = self
                    .trace_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    context.trace_id = trace_id.to_string();
                }
                context
            },
            None => {
                let mut context = LlmTraceContext::new(LlmScope::legacy_default(), workload_class);
                context.scope_resolution = crate::trace::LlmScopeResolution::SystemDefault;
                if let Some(trace_id) = self
                    .trace_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                {
                    context.trace_id = trace_id.to_string();
                }
                context
            },
        };
        self.set_trace_context(context.clone());
        context
    }

    /// Return the shared physical-attempt counter, creating it when this is
    /// the first routing layer to observe the request.
    pub fn ensure_provider_attempt_counter(&mut self) -> Arc<AtomicU32> {
        self.provider_attempt_counter
            .get_or_insert_with(|| Arc::new(AtomicU32::new(0)))
            .clone()
    }

    /// Record one actual provider invocation and return its one-based ordinal.
    pub fn record_provider_attempt(&mut self) -> u32 {
        self.ensure_provider_attempt_counter()
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1)
    }

    /// Number of physical provider invocations observed for this request.
    pub fn provider_attempt_count(&self) -> u32 {
        self.provider_attempt_counter
            .as_ref()
            .map_or(0, |counter| counter.load(Ordering::Relaxed))
    }
}

/// Normalised message format sent to providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMMessage {
    pub role: MessageRole,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentBlock>,
}

impl LLMMessage {
    /// Creates a system message with text content.
    pub fn system(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::System,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Creates a user message with text content.
    pub fn user(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::User,
            content: vec![ContentBlock::text(text)],
        }
    }

    /// Creates an assistant message with text content.
    pub fn assistant(text: impl Into<String>) -> Self {
        Self {
            role: MessageRole::Assistant,
            content: vec![ContentBlock::text(text)],
        }
    }
}

/// Availability of aggregate harness metering; false means unknown, not zero.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageAvailability {
    pub tokens: bool,
    pub cache_read: bool,
    pub cache_write: bool,
    pub cost: bool,
}

/// Token usage statistics returned by providers.
///
/// `cached_tokens` is cache-READ (charged at a discount).
/// `cache_creation_tokens` is cache-WRITE (charged at a premium on Anthropic
/// and GPT-5.6+; OpenAI's `cache_write_tokens` is normalized into this field).
/// Both are a subset of `prompt_tokens`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TokenUsage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_tokens: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u32>,
}

/// Token counts for a Realtime (voice) response/session, split by modality.
///
/// Realtime billing has separate TEXT and AUDIO streams, each with input,
/// cached-input, and output buckets — a shape [`TokenUsage`] can't represent.
/// The `*_input_tokens` fields are the UNCACHED input (cached counted separately
/// in `*_cached_input_tokens`), matching the OpenAI realtime `usage` object after
/// subtracting `input_token_details.cached_tokens`. Costed by
/// `crate::pricing::compute_realtime_cost`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize)]
pub struct RealtimeUsage {
    pub text_input_tokens: u64,
    pub text_cached_input_tokens: u64,
    pub text_output_tokens: u64,
    pub audio_input_tokens: u64,
    pub audio_cached_input_tokens: u64,
    pub audio_output_tokens: u64,
    /// Billable session seconds, for a realtime model priced by the clock
    /// rather than by tokens — GPT-Live charges per second of voice session
    /// and reports no per-turn tokens at all. Zero for a token-priced model.
    #[serde(default)]
    pub billed_seconds: f64,
}

impl RealtimeUsage {
    /// Total tokens across every modality/bucket — for coarse logging/watermarks.
    pub fn total_tokens(&self) -> u64 {
        self.text_input_tokens
            + self.text_cached_input_tokens
            + self.text_output_tokens
            + self.audio_input_tokens
            + self.audio_cached_input_tokens
            + self.audio_output_tokens
    }

    /// Accumulate another response's usage into this running session total.
    pub fn add(&mut self, other: &RealtimeUsage) {
        self.text_input_tokens += other.text_input_tokens;
        self.text_cached_input_tokens += other.text_cached_input_tokens;
        self.text_output_tokens += other.text_output_tokens;
        self.audio_input_tokens += other.audio_input_tokens;
        self.audio_cached_input_tokens += other.audio_cached_input_tokens;
        self.audio_output_tokens += other.audio_output_tokens;
        self.billed_seconds += other.billed_seconds;
    }
}

/// Normalised request envelope used by all providers.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LLMRequest {
    pub model: String,
    /// Large immutable logical history shared copy-on-write across queue,
    /// router fallback and provider retry envelopes.
    #[serde(
        default = "default_shared_vec",
        skip_serializing_if = "shared_vec_is_empty"
    )]
    pub messages: Arc<Vec<LLMMessage>>,
    #[serde(default = "default_modality")]
    pub modality: LLMModality,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<Arc<Vec<u8>>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_media: Option<Arc<Vec<MediaContent>>>,
    #[serde(
        default = "default_shared_vec",
        skip_serializing_if = "shared_vec_is_empty"
    )]
    pub tools: Arc<Vec<LLMToolSpec>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    /// Potentially large structured-output schema shared across dispatch,
    /// router fallback and provider retry envelopes. Providers materialize it
    /// only when constructing the final wire body.
    pub response_format: Option<Arc<LLMResponseFormat>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<ReasoningConfig>,
    #[serde(default)]
    pub metadata: RequestMetadata,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub top_p: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    #[serde(default)]
    pub stream: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<PromptCacheConfig>,
    /// Provider-neutral context reuse plan. Provider adapters consume this
    /// locally; it is excluded from generic request serialization and never
    /// copied wholesale into an upstream request body.
    #[serde(skip)]
    pub context_reuse: Option<Arc<crate::context_reuse::ContextReuseConfig>>,
    /// Arbitrary provider-state lane shared across queue/router/retry clones.
    /// Mutation is explicit copy-on-write through [`Self::take_extra_value`]
    /// and [`Self::set_extra`]; ordinary request cloning never traverses the
    /// provider JSON tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<Arc<Value>>,
    /// Optional side-channel sink for fine-grained streaming deltas
    /// (`ReasoningStart` / `Delta` / `End`, `ToolCallStart` /
    /// `ArgsDelta` / `End`). When attached, providers fire it inline
    /// while parsing the SSE stream. Skipped during ser/de — the
    /// callback isn't transportable across process boundaries.
    #[serde(skip)]
    pub stream_event_sink: StreamEventSink,
    /// Blocks the dispatch queue's local-prep step should summarise (via
    /// Ollama) and inline back into `messages` before the provider call.
    /// Providers never see this field — by the time the request reaches
    /// `provider.invoke`, the queue has cleared it and replaced the
    /// referenced content positions with `ContentBlock::Text`. Default
    /// empty so existing callers are unaffected.
    #[serde(
        default = "default_shared_vec",
        skip_serializing_if = "shared_vec_is_empty"
    )]
    pub summarisable_blocks: Arc<Vec<SummarisableBlock>>,
    /// Shared last-owner guard for recursively nested JSON lanes. It is kept
    /// last so ordinary request fields release first; the final shallow clone
    /// then drains the last Value owner iteratively.
    #[serde(skip, default)]
    #[doc(hidden)]
    pub json_drop_guard: JsonDropGuard,
}

fn default_shared_vec<T>() -> Arc<Vec<T>> {
    Arc::new(Vec::new())
}

fn shared_vec_is_empty<T>(values: &Arc<Vec<T>>) -> bool {
    values.is_empty()
}

enum JsonCloneFrame<'a> {
    Array {
        remaining: std::slice::Iter<'a, Value>,
        output: Vec<Value>,
    },
    Object {
        remaining: serde_json::map::Iter<'a>,
        output: serde_json::Map<String, Value>,
        active_key: Option<String>,
    },
}

enum JsonInspectFrame<'a> {
    Array {
        remaining: std::slice::Iter<'a, Value>,
        child_depth: usize,
    },
    Object {
        remaining: serde_json::map::Values<'a>,
        child_depth: usize,
    },
}

enum JsonDropFrame {
    Array(std::vec::IntoIter<Value>),
    Object(serde_json::map::IntoIter),
}

/// Shared admission contract for every JSON tree retained in an LLM request.
/// The ceiling is intentionally compatible with large tool catalogs while
/// bounding both recursive destruction depth and aggregate provider-state
/// allocation before queue/router ownership.
pub(crate) const MAX_LLM_REQUEST_JSON_DEPTH: usize = 64;
pub(crate) const MAX_LLM_REQUEST_JSON_NODES: usize = 1_000_000;
pub(crate) const MAX_PROVIDER_RESPONSE_JSON_BYTES: usize = 64 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_RESPONSE_JSON_NODES: usize = 1_000_000;
pub(crate) const MAX_PROVIDER_RESPONSE_JSON_DEPTH: usize = 64;
/// Aggregate compact-JSON headroom for a normalized response. One lane still
/// cannot exceed the physical 64 MiB provider-body ceiling; 3x permits a raw
/// body plus normalized subtree projections without wire-compat drift.
pub(crate) const MAX_NORMALIZED_RESPONSE_JSON_BYTES: usize = 3 * MAX_PROVIDER_RESPONSE_JSON_BYTES;
/// Independent heap-retention ceiling for one normalized response. The 3x
/// headroom accommodates the ordinary raw-body + normalized-projection case
/// plus conservative typed/Value overhead; pathologically expansion-heavy
/// shapes still fail this independent memory boundary even when their compact
/// wire representation fits.
pub(crate) const MAX_NORMALIZED_RESPONSE_RETAINED_BYTES: usize =
    3 * MAX_PROVIDER_RESPONSE_JSON_BYTES;
pub(crate) const MAX_TOOL_ARGUMENT_JSON_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_TOOL_ARGUMENT_JSON_NODES: usize = 200_000;
pub(crate) const MAX_TOOL_ARGUMENT_JSON_DEPTH: usize = 64;

fn encoded_json_is_bounded(data: &[u8], max_depth: usize, max_nodes: usize) -> bool {
    let mut containers = Vec::<u8>::with_capacity(max_depth.min(64));
    let mut expects_value = true;
    let mut nodes = 0usize;
    let mut index = 0usize;
    while index < data.len() {
        match data[index] {
            b' ' | b'\n' | b'\r' | b'\t' => index += 1,
            b'{' | b'[' => {
                // Every opening delimiter consumes structural stack capacity,
                // even when the input is malformed and a value is not legal
                // at this position. Serde is the syntax authority, but it only
                // runs after this admission scan; leaving this check inside
                // `expects_value` lets inputs such as `{{{{...` grow this Vec
                // without bound before syntax rejection.
                if containers.len() >= max_depth {
                    return false;
                }
                if expects_value {
                    if nodes >= max_nodes {
                        return false;
                    }
                    nodes += 1;
                }
                containers.push(data[index]);
                expects_value = data[index] == b'[';
                index += 1;
            },
            b'}' | b']' => {
                containers.pop();
                expects_value = false;
                index += 1;
            },
            b':' => {
                expects_value = true;
                index += 1;
            },
            b',' => {
                expects_value = containers.last().copied() == Some(b'[');
                index += 1;
            },
            b'"' => {
                if expects_value {
                    if nodes >= max_nodes {
                        return false;
                    }
                    nodes += 1;
                }
                expects_value = false;
                index += 1;
                let mut escaped = false;
                while index < data.len() {
                    let byte = data[index];
                    index += 1;
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        break;
                    }
                }
            },
            _ => {
                if expects_value {
                    if nodes >= max_nodes {
                        return false;
                    }
                    nodes += 1;
                }
                expects_value = false;
                while index < data.len()
                    && !matches!(
                        data[index],
                        b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}'
                    )
                {
                    index += 1;
                }
            },
        }
    }
    true
}

fn starts_with_json_container(raw: &str) -> bool {
    raw.as_bytes()
        .iter()
        .copied()
        .find(|byte| !matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
        .is_some_and(|byte| matches!(byte, b'{' | b'['))
}

/// Parse one complete provider response/event only after a raw byte, depth and
/// node admission pass. Serde remains the syntax authority; the lexical scan
/// exists solely to prevent a wide/deep response from allocating a recursive
/// `Value` tree first.
pub(crate) fn parse_provider_json_value(raw: &str) -> LLMResult<Value> {
    if raw.len() > MAX_PROVIDER_RESPONSE_JSON_BYTES
        || !encoded_json_is_bounded(
            raw.as_bytes(),
            MAX_PROVIDER_RESPONSE_JSON_DEPTH,
            MAX_PROVIDER_RESPONSE_JSON_NODES,
        )
    {
        return Err(LLMError::Validation(format!(
            "provider JSON exceeds the admitted {}-byte/{}-level/{}-node ceiling",
            MAX_PROVIDER_RESPONSE_JSON_BYTES,
            MAX_PROVIDER_RESPONSE_JSON_DEPTH,
            MAX_PROVIDER_RESPONSE_JSON_NODES,
        )));
    }
    serde_json::from_str(raw).map_err(LLMError::Serialization)
}

pub(crate) fn parse_provider_json_or_string(raw: &str) -> LLMResult<Value> {
    if raw.len() > MAX_PROVIDER_RESPONSE_JSON_BYTES {
        return Err(LLMError::Validation(format!(
            "provider response exceeds the admitted {}-byte ceiling",
            MAX_PROVIDER_RESPONSE_JSON_BYTES
        )));
    }
    if !starts_with_json_container(raw)
        || encoded_json_is_bounded(
            raw.as_bytes(),
            MAX_PROVIDER_RESPONSE_JSON_DEPTH,
            MAX_PROVIDER_RESPONSE_JSON_NODES,
        )
    {
        return Ok(serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string())));
    }
    Err(LLMError::Validation(format!(
        "provider JSON exceeds the admitted {}-level/{}-node ceiling",
        MAX_PROVIDER_RESPONSE_JSON_DEPTH, MAX_PROVIDER_RESPONSE_JSON_NODES
    )))
}

pub(crate) fn parse_tool_argument_json_or_string(raw: &str) -> LLMResult<Value> {
    if raw.len() > MAX_TOOL_ARGUMENT_JSON_BYTES {
        return Err(LLMError::Validation(format!(
            "tool arguments exceed the admitted {}-byte ceiling",
            MAX_TOOL_ARGUMENT_JSON_BYTES
        )));
    }
    if !starts_with_json_container(raw)
        || encoded_json_is_bounded(
            raw.as_bytes(),
            MAX_TOOL_ARGUMENT_JSON_DEPTH,
            MAX_TOOL_ARGUMENT_JSON_NODES,
        )
    {
        return Ok(serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string())));
    }

    // Container-looking input fails closed only when its lexical depth/node
    // budget is crossed. Malformed containers within those budgets, scalar
    // JSON, and non-container prose still reach Serde and retain the legacy
    // string fallback when parsing fails.
    Err(LLMError::Validation(format!(
        "tool arguments exceed the admitted {}-level/{}-node ceiling",
        MAX_TOOL_ARGUMENT_JSON_DEPTH, MAX_TOOL_ARGUMENT_JSON_NODES
    )))
}

pub(crate) fn append_tool_argument_fragment(target: &mut String, fragment: &str) -> LLMResult<()> {
    if target.len().saturating_add(fragment.len()) > MAX_TOOL_ARGUMENT_JSON_BYTES {
        return Err(LLMError::Validation(format!(
            "streamed tool arguments exceed the admitted {}-byte ceiling",
            MAX_TOOL_ARGUMENT_JSON_BYTES
        )));
    }
    target.push_str(fragment);
    Ok(())
}

pub(crate) fn clone_admitted_tool_argument(value: &Value) -> LLMResult<Value> {
    let mut remaining = MAX_TOOL_ARGUMENT_JSON_NODES;
    if estimated_json_bytes(value) > MAX_TOOL_ARGUMENT_JSON_BYTES
        || !json_value_is_bounded(value, MAX_TOOL_ARGUMENT_JSON_DEPTH, &mut remaining)
    {
        return Err(LLMError::Validation(format!(
            "tool arguments exceed the admitted {}-byte/{}-level/{}-node ceiling",
            MAX_TOOL_ARGUMENT_JSON_BYTES,
            MAX_TOOL_ARGUMENT_JSON_DEPTH,
            MAX_TOOL_ARGUMENT_JSON_NODES
        )));
    }
    Ok(clone_json_value_iteratively(value))
}

fn json_value_is_bounded(root: &Value, max_depth: usize, remaining_nodes: &mut usize) -> bool {
    let mut frames = Vec::<JsonInspectFrame<'_>>::new();
    let mut current = Some((root, 1usize));
    loop {
        if let Some((value, depth)) = current.take() {
            if depth > max_depth || *remaining_nodes == 0 {
                return false;
            }
            *remaining_nodes -= 1;
            match value {
                Value::Array(values) if !values.is_empty() => {
                    let mut remaining = values.iter();
                    current = remaining.next().map(|child| (child, depth + 1));
                    frames.push(JsonInspectFrame::Array {
                        remaining,
                        child_depth: depth + 1,
                    });
                    continue;
                },
                Value::Object(values) if !values.is_empty() => {
                    let mut remaining = values.values();
                    current = remaining.next().map(|child| (child, depth + 1));
                    frames.push(JsonInspectFrame::Object {
                        remaining,
                        child_depth: depth + 1,
                    });
                    continue;
                },
                _ => {},
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return true;
            };
            let next = match frame {
                JsonInspectFrame::Array {
                    remaining,
                    child_depth,
                } => remaining.next().map(|value| (value, *child_depth)),
                JsonInspectFrame::Object {
                    remaining,
                    child_depth,
                } => remaining.next().map(|value| (value, *child_depth)),
            };
            if next.is_some() {
                current = next;
                break;
            }
            frames.pop();
        }
    }
}

/// In-memory counterpart to `encoded_json_is_bounded` for normalized provider
/// responses. Depth counts JSON containers, not the scalar leaf below them, so
/// an exact `MAX_PROVIDER_RESPONSE_JSON_DEPTH` wire value remains accepted at
/// the programmatic/custom-provider boundary too.
fn json_value_is_structurally_bounded(
    root: &Value,
    max_container_depth: usize,
    remaining_nodes: &mut usize,
) -> bool {
    let mut frames = Vec::<JsonInspectFrame<'_>>::new();
    // The carried depth is the number of enclosing containers. Container
    // values consume the next level; scalar values consume no depth.
    let mut current = Some((root, 0usize));
    loop {
        if let Some((value, enclosing_depth)) = current.take() {
            if *remaining_nodes == 0 {
                return false;
            }
            *remaining_nodes -= 1;
            match value {
                Value::Array(values) => {
                    let container_depth = enclosing_depth.saturating_add(1);
                    if container_depth > max_container_depth {
                        return false;
                    }
                    if !values.is_empty() {
                        let mut remaining = values.iter();
                        current = remaining.next().map(|child| (child, container_depth));
                        frames.push(JsonInspectFrame::Array {
                            remaining,
                            child_depth: container_depth,
                        });
                        continue;
                    }
                },
                Value::Object(values) => {
                    let container_depth = enclosing_depth.saturating_add(1);
                    if container_depth > max_container_depth {
                        return false;
                    }
                    if !values.is_empty() {
                        let mut remaining = values.values();
                        current = remaining.next().map(|child| (child, container_depth));
                        frames.push(JsonInspectFrame::Object {
                            remaining,
                            child_depth: container_depth,
                        });
                        continue;
                    }
                },
                _ => {},
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return true;
            };
            let next = match frame {
                JsonInspectFrame::Array {
                    remaining,
                    child_depth,
                } => remaining.next().map(|value| (value, *child_depth)),
                JsonInspectFrame::Object {
                    remaining,
                    child_depth,
                } => remaining.next().map(|value| (value, *child_depth)),
            };
            if next.is_some() {
                current = next;
                break;
            }
            frames.pop();
        }
    }
}

struct BoundedJsonLengthWriter {
    bytes: usize,
    max_bytes: usize,
}

impl BoundedJsonLengthWriter {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            max_bytes,
        }
    }
}

impl std::io::Write for BoundedJsonLengthWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.saturating_add(buffer.len());
        if next > self.max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                "aggregate response JSON byte ceiling exceeded",
            ));
        }
        self.bytes = next;
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Destroy an owned JSON tree with heap-framed iteration. This is the final
/// rejection/unwind safeguard for a programmatically constructed request that
/// bypassed Serde's recursion limit.
pub(crate) fn discard_json_value_iteratively(root: Value) {
    let mut frames = Vec::<JsonDropFrame>::new();
    let mut current = Some(root);
    loop {
        if let Some(value) = current.take() {
            match value {
                Value::Array(values) if !values.is_empty() => {
                    let mut remaining = values.into_iter();
                    current = remaining.next();
                    frames.push(JsonDropFrame::Array(remaining));
                    continue;
                },
                Value::Object(values) if !values.is_empty() => {
                    let mut remaining = values.into_iter();
                    current = remaining.next().map(|(_, value)| value);
                    frames.push(JsonDropFrame::Object(remaining));
                    continue;
                },
                _ => {},
            }
        }
        loop {
            let Some(frame) = frames.last_mut() else {
                return;
            };
            let next = match frame {
                JsonDropFrame::Array(remaining) => remaining.next(),
                JsonDropFrame::Object(remaining) => remaining.next().map(|(_, value)| value),
            };
            if next.is_some() {
                current = next;
                break;
            }
            frames.pop();
        }
    }
}

/// Clone provider JSON without recursive `Value::clone`. This is used only at
/// an actual COW/wire-materialization boundary; ordinary `LLMRequest::clone`
/// retains the shared `Arc<Value>`.
pub(crate) fn clone_json_value_iteratively(root: &Value) -> Value {
    let mut frames = Vec::<JsonCloneFrame<'_>>::new();
    let mut current = root;
    let mut produced: Option<Value> = None;

    loop {
        if produced.is_none() {
            match current {
                Value::Array(values) if !values.is_empty() => {
                    let mut remaining = values.iter();
                    current = remaining.next().expect("non-empty array");
                    frames.push(JsonCloneFrame::Array {
                        remaining,
                        output: Vec::with_capacity(values.len()),
                    });
                    continue;
                },
                Value::Object(values) if !values.is_empty() => {
                    let mut remaining = values.iter();
                    let (key, child) = remaining.next().expect("non-empty object");
                    current = child;
                    frames.push(JsonCloneFrame::Object {
                        remaining,
                        output: serde_json::Map::new(),
                        active_key: Some(key.clone()),
                    });
                    continue;
                },
                Value::Array(_) => produced = Some(Value::Array(Vec::new())),
                Value::Object(_) => produced = Some(Value::Object(serde_json::Map::new())),
                scalar => produced = Some(scalar.clone()),
            }
        }

        let value = produced.take().expect("a scalar or completed container");
        let Some(frame) = frames.last_mut() else {
            return value;
        };
        match frame {
            JsonCloneFrame::Array { remaining, output } => {
                output.push(value);
                if let Some(child) = remaining.next() {
                    current = child;
                } else {
                    let output = std::mem::take(output);
                    frames.pop();
                    produced = Some(Value::Array(output));
                }
            },
            JsonCloneFrame::Object {
                remaining,
                output,
                active_key,
            } => {
                output.insert(
                    active_key.take().expect("object child has an active key"),
                    value,
                );
                if let Some((key, child)) = remaining.next() {
                    *active_key = Some(key.clone());
                    current = child;
                } else {
                    let output = std::mem::take(output);
                    frames.pop();
                    produced = Some(Value::Object(output));
                }
            },
        }
    }
}

fn clone_content_block_heap_framed(block: &ContentBlock) -> ContentBlock {
    match block {
        ContentBlock::Text { text } => ContentBlock::Text { text: text.clone() },
        ContentBlock::Image {
            data,
            media_type,
            caption,
        } => ContentBlock::Image {
            data: data.clone(),
            media_type: media_type.clone(),
            caption: caption.clone(),
        },
        ContentBlock::ImageUrl { url, prompt } => ContentBlock::ImageUrl {
            url: url.clone(),
            prompt: prompt.clone(),
        },
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => ContentBlock::ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: clone_json_value_iteratively(arguments),
        },
        ContentBlock::ToolResult {
            tool_call_id,
            content,
        } => ContentBlock::ToolResult {
            tool_call_id: tool_call_id.clone(),
            content: clone_json_value_iteratively(content),
        },
        ContentBlock::Json { value } => ContentBlock::Json {
            value: clone_json_value_iteratively(value),
        },
    }
}

fn clone_messages_heap_framed(messages: &[LLMMessage]) -> Vec<LLMMessage> {
    messages
        .iter()
        .map(|message| LLMMessage {
            role: message.role,
            content: message
                .content
                .iter()
                .map(clone_content_block_heap_framed)
                .collect(),
        })
        .collect()
}

fn clone_tools_heap_framed(tools: &[LLMToolSpec]) -> Vec<LLMToolSpec> {
    tools
        .iter()
        .map(|tool| LLMToolSpec {
            name: tool.name.clone(),
            description: tool.description.clone(),
            parameters: clone_json_value_iteratively(&tool.parameters),
        })
        .collect()
}

fn clone_tool_calls_heap_framed(tool_calls: &[LLMToolCall]) -> Vec<LLMToolCall> {
    tool_calls
        .iter()
        .map(|tool_call| LLMToolCall {
            id: tool_call.id.clone(),
            name: tool_call.name.clone(),
            arguments: clone_json_value_iteratively(&tool_call.arguments),
        })
        .collect()
}

fn clone_tool_results_heap_framed(tool_results: &[LLMToolResult]) -> Vec<LLMToolResult> {
    tool_results
        .iter()
        .map(|tool_result| LLMToolResult {
            tool_call_id: tool_result.tool_call_id.clone(),
            output: clone_json_value_iteratively(&tool_result.output),
        })
        .collect()
}

/// Obtain a unique message lane without allowing derived `Clone` to recurse
/// through programmatically constructed JSON. `try_unwrap` also preserves the
/// allocation when only admission-cache `Weak` identities remain.
fn messages_cow_mut(messages: &mut Arc<Vec<LLMMessage>>) -> &mut Vec<LLMMessage> {
    if Arc::get_mut(messages).is_none() {
        let previous = std::mem::replace(messages, Arc::new(Vec::new()));
        *messages = Arc::new(match Arc::try_unwrap(previous) {
            Ok(unique) => unique,
            Err(shared) => clone_messages_heap_framed(shared.as_ref()),
        });
    }
    Arc::get_mut(messages).expect("message COW lane is uniquely owned")
}

/// Obtain a unique tool lane while cloning every schema with heap frames.
fn tools_cow_mut(tools: &mut Arc<Vec<LLMToolSpec>>) -> &mut Vec<LLMToolSpec> {
    if Arc::get_mut(tools).is_none() {
        let previous = std::mem::replace(tools, Arc::new(Vec::new()));
        *tools = Arc::new(match Arc::try_unwrap(previous) {
            Ok(unique) => unique,
            Err(shared) => clone_tools_heap_framed(shared.as_ref()),
        });
    }
    Arc::get_mut(tools).expect("tool COW lane is uniquely owned")
}

fn tool_calls_cow_mut(tool_calls: &mut Arc<Vec<LLMToolCall>>) -> &mut Vec<LLMToolCall> {
    if Arc::get_mut(tool_calls).is_none() {
        let previous = std::mem::replace(tool_calls, Arc::new(Vec::new()));
        *tool_calls = Arc::new(match Arc::try_unwrap(previous) {
            Ok(unique) => unique,
            Err(shared) => clone_tool_calls_heap_framed(shared.as_ref()),
        });
    }
    Arc::get_mut(tool_calls).expect("tool-call COW lane is uniquely owned")
}

fn tool_results_cow_mut(tool_results: &mut Arc<Vec<LLMToolResult>>) -> &mut Vec<LLMToolResult> {
    if Arc::get_mut(tool_results).is_none() {
        let previous = std::mem::replace(tool_results, Arc::new(Vec::new()));
        *tool_results = Arc::new(match Arc::try_unwrap(previous) {
            Ok(unique) => unique,
            Err(shared) => clone_tool_results_heap_framed(shared.as_ref()),
        });
    }
    Arc::get_mut(tool_results).expect("tool-result COW lane is uniquely owned")
}

fn json_arc_cow_mut(value: &mut Arc<Value>) -> &mut Value {
    if Arc::get_mut(value).is_none() {
        let previous = std::mem::replace(value, Arc::new(Value::Null));
        *value = Arc::new(match Arc::try_unwrap(previous) {
            Ok(unique) => unique,
            Err(shared) => clone_json_value_iteratively(shared.as_ref()),
        });
    }
    Arc::get_mut(value).expect("JSON COW lane is uniquely owned")
}

fn admit_json_lane<'a>(values: impl Iterator<Item = &'a Value>) -> JsonLaneAdmission {
    let mut remaining = MAX_LLM_REQUEST_JSON_NODES;
    for value in values {
        if !json_value_is_bounded(value, MAX_LLM_REQUEST_JSON_DEPTH, &mut remaining) {
            return JsonLaneAdmission::Rejected;
        }
    }
    JsonLaneAdmission::Admitted(MAX_LLM_REQUEST_JSON_NODES - remaining)
}

impl JsonAdmissionAuthority {
    fn validates(&self, request: &LLMRequest) -> bool {
        let mut cache = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut scanned = false;

        // The payload lanes intentionally remain public for API compatibility.
        // Therefore setter invalidation is an optimization, not an authority
        // boundary: verify the cached verdict still belongs to the exact Arc
        // allocation before trusting it. Retaining Weak identities prevents
        // allocator-address reuse (ABA) while an old verdict is live.
        if !optional_arc_identity_matches(cache.extra_owner.as_ref(), request.extra.as_ref()) {
            cache.extra = JsonLaneAdmission::Unknown;
            cache.extra_owner = None;
        }
        if !optional_arc_identity_matches(
            cache.response_format_owner.as_ref(),
            request.response_format.as_ref(),
        ) {
            cache.response_format = JsonLaneAdmission::Unknown;
            cache.response_format_owner = None;
        }
        if !required_arc_identity_matches(cache.tools_owner.as_ref(), &request.tools) {
            cache.tools = JsonLaneAdmission::Unknown;
            cache.tools_owner = None;
        }
        if !required_arc_identity_matches(cache.messages_owner.as_ref(), &request.messages) {
            cache.messages = JsonLaneAdmission::Unknown;
            cache.messages_owner = None;
        }

        if matches!(cache.extra, JsonLaneAdmission::Unknown) {
            cache.extra = admit_json_lane(request.extra_value().into_iter());
            cache.extra_owner = request.extra.as_ref().map(Arc::downgrade);
            scanned = true;
        }
        if matches!(cache.response_format, JsonLaneAdmission::Unknown) {
            cache.response_format = match request.response_format_value() {
                Some(LLMResponseFormat::JsonSchema { schema }) => {
                    admit_json_lane(std::iter::once(schema))
                },
                _ => JsonLaneAdmission::Admitted(0),
            };
            cache.response_format_owner = request.response_format.as_ref().map(Arc::downgrade);
            scanned = true;
        }
        if matches!(cache.tools, JsonLaneAdmission::Unknown) {
            cache.tools = admit_json_lane(request.tools.iter().map(|tool| &tool.parameters));
            cache.tools_owner = Some(Arc::downgrade(&request.tools));
            scanned = true;
        }
        if matches!(cache.messages, JsonLaneAdmission::Unknown) {
            cache.messages = admit_json_lane(request.messages.iter().flat_map(|message| {
                message.content.iter().filter_map(|block| match block {
                    ContentBlock::ToolCall { arguments, .. } => Some(arguments),
                    ContentBlock::ToolResult { content, .. } => Some(content),
                    ContentBlock::Json { value } => Some(value),
                    _ => None,
                })
            }));
            cache.messages_owner = Some(Arc::downgrade(&request.messages));
            scanned = true;
        }
        if scanned {
            cache.scan_passes = cache.scan_passes.saturating_add(1);
        }

        let lanes = [
            cache.messages,
            cache.tools,
            cache.response_format,
            cache.extra,
        ];
        let mut total = 0usize;
        for lane in lanes {
            match lane {
                JsonLaneAdmission::Admitted(nodes) => {
                    total = total.saturating_add(nodes);
                    if total > MAX_LLM_REQUEST_JSON_NODES {
                        return false;
                    }
                },
                JsonLaneAdmission::Unknown | JsonLaneAdmission::Rejected => return false,
            }
        }
        true
    }

    fn rejected_lanes(&self) -> JsonRejectedLanes {
        let cache = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        JsonRejectedLanes {
            messages: matches!(cache.messages, JsonLaneAdmission::Rejected),
            tools: matches!(cache.tools, JsonLaneAdmission::Rejected),
            response_format: matches!(cache.response_format, JsonLaneAdmission::Rejected),
            extra: matches!(cache.extra, JsonLaneAdmission::Rejected),
        }
    }
}

fn required_arc_identity_matches<T>(cached: Option<&Weak<T>>, current: &Arc<T>) -> bool {
    cached.is_some_and(|cached| Weak::ptr_eq(cached, &Arc::downgrade(current)))
}

fn optional_arc_identity_matches<T>(cached: Option<&Weak<T>>, current: Option<&Arc<T>>) -> bool {
    match (cached, current) {
        (None, None) => true,
        (Some(cached), Some(current)) => Weak::ptr_eq(cached, &Arc::downgrade(current)),
        _ => false,
    }
}

impl JsonDropGuard {
    fn capture_rejected(&self, request: &LLMRequest, rejected: JsonRejectedLanes) {
        let mut owners = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // An old lane which is now retained only by the guard can be drained
        // immediately. Lanes still referenced by another shallow request
        // clone remain guarded until that clone releases them.
        owners.retain(SafeJsonLaneOwner::has_external_owner);

        if rejected.messages && !owners.iter().any(|owner| {
            matches!(owner, SafeJsonLaneOwner::Messages(Some(existing)) if Arc::ptr_eq(existing, &request.messages))
        }) {
            owners.push(SafeJsonLaneOwner::Messages(Some(request.messages.clone())));
        }
        if rejected.tools && !owners.iter().any(|owner| {
            matches!(owner, SafeJsonLaneOwner::Tools(Some(existing)) if Arc::ptr_eq(existing, &request.tools))
        }) {
            owners.push(SafeJsonLaneOwner::Tools(Some(request.tools.clone())));
        }
        if rejected.response_format {
            if let Some(response_format) = request.response_format.as_ref() {
                if !owners.iter().any(|owner| {
                    matches!(owner, SafeJsonLaneOwner::ResponseFormat(Some(existing)) if Arc::ptr_eq(existing, response_format))
                }) {
                    owners.push(SafeJsonLaneOwner::ResponseFormat(Some(
                        response_format.clone(),
                    )));
                }
            }
        }
        if rejected.extra {
            if let Some(extra) = request.extra.as_ref() {
                if !owners.iter().any(|owner| {
                    matches!(owner, SafeJsonLaneOwner::Extra(Some(existing)) if Arc::ptr_eq(existing, extra))
                }) {
                    owners.push(SafeJsonLaneOwner::Extra(Some(extra.clone())));
                }
            }
        }
    }
}

fn discard_shared_messages(owner: Arc<Vec<LLMMessage>>) {
    if let Ok(messages) = Arc::try_unwrap(owner) {
        for message in messages {
            for block in message.content {
                match block {
                    ContentBlock::ToolCall { arguments, .. } => {
                        discard_json_value_iteratively(arguments)
                    },
                    ContentBlock::ToolResult { content, .. } => {
                        discard_json_value_iteratively(content)
                    },
                    ContentBlock::Json { value } => discard_json_value_iteratively(value),
                    _ => {},
                }
            }
        }
    }
}

fn discard_shared_tools(owner: Arc<Vec<LLMToolSpec>>) {
    if let Ok(tools) = Arc::try_unwrap(owner) {
        for tool in tools {
            discard_json_value_iteratively(tool.parameters);
        }
    }
}

fn discard_shared_tool_calls(owner: Arc<Vec<LLMToolCall>>) {
    if let Ok(tool_calls) = Arc::try_unwrap(owner) {
        for tool_call in tool_calls {
            discard_json_value_iteratively(tool_call.arguments);
        }
    }
}

fn discard_shared_tool_results(owner: Arc<Vec<LLMToolResult>>) {
    if let Ok(tool_results) = Arc::try_unwrap(owner) {
        for tool_result in tool_results {
            discard_json_value_iteratively(tool_result.output);
        }
    }
}

fn discard_shared_response_format(owner: Arc<LLMResponseFormat>) {
    if let Ok(LLMResponseFormat::JsonSchema { schema }) = Arc::try_unwrap(owner) {
        discard_json_value_iteratively(schema);
    }
}

fn discard_shared_extra(owner: Arc<Value>) {
    if let Ok(extra) = Arc::try_unwrap(owner) {
        discard_json_value_iteratively(extra);
    }
}

fn discard_shared_request_json_lanes(
    messages: Arc<Vec<LLMMessage>>,
    tools: Arc<Vec<LLMToolSpec>>,
    response_format: Option<Arc<LLMResponseFormat>>,
    extra: Option<Arc<Value>>,
) {
    discard_shared_messages(messages);
    discard_shared_tools(tools);
    if let Some(response_format) = response_format {
        discard_shared_response_format(response_format);
    }
    if let Some(extra) = extra {
        discard_shared_extra(extra);
    }
}

impl LLMRequest {
    /// Validate every arbitrary JSON lane before queue/router ownership. This
    /// includes provider state, response schemas, tool schemas, and structured
    /// message blocks; typed request metadata/context-reuse fields contain no
    /// arbitrary JSON and are intentionally outside this traversal.
    pub(crate) fn json_payloads_are_bounded(&self) -> bool {
        let admitted = self.metadata.json_admission.validates(self);
        if !admitted {
            // Only rejected lanes need a final-owner guard. Arming valid lanes
            // would add a hidden strong Arc owner and force local-prep/provider
            // copy-on-write paths to clone otherwise uniquely owned payloads.
            // An aggregate node-budget rejection consists solely of
            // depth-admitted lanes, so ordinary destruction remains bounded.
            self.json_drop_guard
                .capture_rejected(self, self.metadata.json_admission.rejected_lanes());
        }
        admitted
    }

    #[cfg(test)]
    pub(crate) fn json_admission_scan_passes(&self) -> u32 {
        self.metadata.json_admission.scan_passes()
    }

    /// Consume a rejected request without recursively dropping an untrusted
    /// programmatic `serde_json::Value`. The shared last-owner guard keeps
    /// shallow caller clones safe; the final owner drains with heap frames.
    pub(crate) fn discard_json_payloads_iteratively(mut self) {
        let messages = std::mem::take(&mut self.messages);
        let tools = std::mem::take(&mut self.tools);
        let response_format = self.response_format.take();
        let extra = self.extra.take();
        discard_shared_request_json_lanes(messages, tools, response_format, extra);
    }

    /// Copy-on-write access used only by request construction/local-prep.
    /// Once a request enters dispatch, ordinary clones retain pointer identity
    /// for the large payload lanes.
    pub fn messages_mut(&mut self) -> &mut Vec<LLMMessage> {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::Messages);
        messages_cow_mut(&mut self.messages)
    }

    pub fn set_messages(&mut self, messages: Vec<LLMMessage>) {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::Messages);
        self.messages = Arc::new(messages);
    }

    pub fn tools_mut(&mut self) -> &mut Vec<LLMToolSpec> {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::Tools);
        tools_cow_mut(&mut self.tools)
    }

    pub fn set_tools(&mut self, tools: Vec<LLMToolSpec>) {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::Tools);
        self.tools = Arc::new(tools);
    }

    pub fn summarisable_blocks_mut(&mut self) -> &mut Vec<SummarisableBlock> {
        Arc::make_mut(&mut self.summarisable_blocks)
    }

    pub fn set_extra(&mut self, value: Value) {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::Extra);
        self.extra = Some(Arc::new(value));
    }

    pub fn extra_value(&self) -> Option<&Value> {
        self.extra.as_deref()
    }

    /// Take an owned provider-state tree for an actual mutation/materialization
    /// boundary. Unique values move without copying; shared values use a
    /// heap-framed clone so adversarial nesting cannot grow the native stack.
    pub fn take_extra_value(&mut self) -> Option<Value> {
        if self.extra.is_some() {
            self.metadata
                .json_admission
                .invalidate_lane(JsonAdmissionLane::Extra);
        }
        self.extra.take().map(|extra| {
            Arc::try_unwrap(extra)
                .unwrap_or_else(|shared| clone_json_value_iteratively(shared.as_ref()))
        })
    }

    pub fn set_response_format(&mut self, format: LLMResponseFormat) {
        self.metadata
            .json_admission
            .invalidate_lane(JsonAdmissionLane::ResponseFormat);
        self.response_format = Some(Arc::new(format));
    }

    pub fn response_format_value(&self) -> Option<&LLMResponseFormat> {
        self.response_format.as_deref()
    }

    pub fn set_context_reuse(&mut self, reuse: crate::context_reuse::ContextReuseConfig) {
        self.context_reuse = Some(Arc::new(reuse));
    }

    pub fn context_reuse_mut(&mut self) -> Option<&mut crate::context_reuse::ContextReuseConfig> {
        self.context_reuse.as_mut().map(Arc::make_mut)
    }

    /// Structural probe for regression coverage and dispatch diagnostics.
    pub fn shares_large_payload_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.messages, &other.messages)
            && Arc::ptr_eq(&self.tools, &other.tools)
            && match (&self.media, &other.media) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && match (&self.input_media, &other.input_media) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && match (&self.response_format, &other.response_format) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && match (&self.context_reuse, &other.context_reuse) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && match (&self.extra, &other.extra) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            }
            && Arc::ptr_eq(&self.summarisable_blocks, &other.summarisable_blocks)
    }

    /// Conservative retained-byte estimate for one queued logical request.
    /// Arc-backed lanes are counted once for this job identity; shallow
    /// router/retry clones are not independently admitted into queue lanes.
    pub(crate) fn estimated_retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>()
            .saturating_add(self.model.capacity())
            .saturating_add(self.metadata.operation.capacity())
            .saturating_add(self.metadata.trace_id.as_ref().map_or(0, String::capacity));
        bytes = bytes.saturating_add(self.metadata.tags.as_ref().map_or(0, |tags| {
            tags.capacity()
                .saturating_mul(std::mem::size_of::<String>())
                .saturating_add(
                    tags.iter()
                        .map(String::capacity)
                        .fold(0usize, usize::saturating_add),
                )
        }));
        if let Some(trace) = self.metadata.trace_context.as_ref() {
            let optional_capacity =
                |value: &Option<String>| value.as_ref().map_or(0, String::capacity);
            bytes = bytes
                .saturating_add(std::mem::size_of::<LlmTraceContext>())
                .saturating_add(trace.trace_id.capacity())
                .saturating_add(trace.llm_call_id.capacity())
                .saturating_add(optional_capacity(&trace.parent_call_id))
                .saturating_add(optional_capacity(&trace.retry_group_id))
                .saturating_add(optional_capacity(&trace.route_decision_id))
                .saturating_add(trace.scope.principal.capacity())
                .saturating_add(trace.scope.workspace.capacity())
                .saturating_add(optional_capacity(&trace.task_id))
                .saturating_add(optional_capacity(&trace.root_execution_id))
                .saturating_add(optional_capacity(&trace.execution_id))
                .saturating_add(optional_capacity(&trace.plan_id))
                .saturating_add(optional_capacity(&trace.step_id))
                .saturating_add(optional_capacity(&trace.iteration_id))
                .saturating_add(optional_capacity(&trace.chat_session_id))
                .saturating_add(optional_capacity(&trace.chat_turn_id))
                .saturating_add(optional_capacity(&trace.user_message_id));
        }
        if let Some(guard) = self.metadata.disclosure_guard.as_ref() {
            bytes = bytes.saturating_add(guard.retained_bytes());
        }
        bytes = bytes.saturating_add(
            self.messages
                .capacity()
                .saturating_mul(std::mem::size_of::<LLMMessage>()),
        );
        bytes = bytes.saturating_add(
            self.messages
                .iter()
                .map(estimated_message_bytes)
                .fold(0usize, usize::saturating_add),
        );
        bytes = bytes.saturating_add(self.media.as_ref().map_or(0, |media| media.capacity()));
        bytes = bytes.saturating_add(self.input_media.as_ref().map_or(0, |items| {
            items
                .capacity()
                .saturating_mul(std::mem::size_of::<MediaContent>())
                .saturating_add(items.iter().fold(0usize, |total, item| {
                    total
                        .saturating_add(item.media_type.capacity())
                        .saturating_add(item.data.as_ref().map_or(0, Vec::capacity))
                        .saturating_add(item.url.as_ref().map_or(0, String::capacity))
                        .saturating_add(item.description.as_ref().map_or(0, String::capacity))
                }))
        }));
        bytes = bytes
            .saturating_add(
                self.tools
                    .capacity()
                    .saturating_mul(std::mem::size_of::<LLMToolSpec>()),
            )
            .saturating_add(self.tools.iter().fold(0usize, |total, tool| {
                total
                    .saturating_add(tool.name.capacity())
                    .saturating_add(tool.description.capacity())
                    .saturating_add(estimated_json_bytes(&tool.parameters))
            }));
        if let Some(LLMResponseFormat::JsonSchema { schema }) = self.response_format_value() {
            bytes = bytes.saturating_add(estimated_json_bytes(schema));
        }
        bytes = bytes.saturating_add(self.extra_value().map_or(0, estimated_json_bytes));
        if let Some(reasoning) = self.reasoning.as_ref() {
            bytes = bytes
                .saturating_add(std::mem::size_of::<ReasoningConfig>())
                .saturating_add(reasoning.effort.as_ref().map_or(0, String::capacity))
                .saturating_add(reasoning.strategy.as_ref().map_or(0, String::capacity))
                .saturating_add(reasoning.summary.as_ref().map_or(0, String::capacity));
        }
        if let Some(prompt_cache) = self.prompt_cache.as_ref() {
            bytes = bytes.saturating_add(std::mem::size_of::<PromptCacheConfig>());
            if let PromptCacheConfig::Enabled {
                ttl,
                cached_content,
            } = prompt_cache
            {
                bytes = bytes
                    .saturating_add(ttl.as_ref().map_or(0, String::capacity))
                    .saturating_add(cached_content.as_ref().map_or(0, String::capacity));
            }
        }
        if let Some(reuse) = self.context_reuse.as_ref() {
            bytes = bytes
                .saturating_add(std::mem::size_of::<crate::context_reuse::ContextReuseConfig>())
                .saturating_add(reuse.continuation_id.as_ref().map_or(0, String::capacity))
                .saturating_add(
                    reuse
                        .transport_cohort_fingerprint
                        .as_ref()
                        .map_or(0, String::capacity),
                )
                .saturating_add(
                    reuse
                        .stable_prefix_fingerprint
                        .as_ref()
                        .map_or(0, String::capacity),
                )
                .saturating_add(
                    reuse
                        .disclosure_partition_fingerprint
                        .as_ref()
                        .map_or(0, String::capacity),
                )
                .saturating_add(reuse.session_key.as_ref().map_or(0, String::capacity));
        }
        bytes
            .saturating_add(
                self.summarisable_blocks
                    .capacity()
                    .saturating_mul(std::mem::size_of::<SummarisableBlock>()),
            )
            .saturating_add(
                self.summarisable_blocks
                    .iter()
                    .fold(0usize, |total, block| {
                        total.saturating_add(block.raw.capacity())
                    }),
            )
    }
}

impl Default for LLMRequest {
    fn default() -> Self {
        Self {
            model: String::new(),
            messages: Arc::new(Vec::new()),
            modality: default_modality(),
            media: None,
            input_media: None,
            tools: Arc::new(Vec::new()),
            response_format: None,
            reasoning: None,
            metadata: RequestMetadata::default(),
            temperature: None,
            top_p: None,
            max_output_tokens: None,
            stream: false,
            prompt_cache: None,
            context_reuse: None,
            extra: None,
            stream_event_sink: StreamEventSink::default(),
            summarisable_blocks: Arc::new(Vec::new()),
            json_drop_guard: JsonDropGuard::default(),
        }
    }
}

/// Normalised response envelope returned by providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LlmRouteIdentity {
    /// Concrete profile selected for the successful physical call.
    pub profile: String,
    /// Concrete provider selected for the successful physical call.
    pub provider: LLMProviderKind,
    /// Concrete model sent to that provider after profile defaults.
    pub model: String,
}

/// Normalised response envelope returned by providers.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LLMResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<Arc<str>>,
    /// Local-only logical-call and dispatch receipt. It is attached by the
    /// configured router/dispatch queue after provider parsing and is never
    /// part of provider response serialization.
    #[serde(skip, default)]
    pub trace_receipt: Option<crate::trace::LlmTraceReceipt>,
    /// Local-only effective route selected by the router. Provider adapters
    /// leave this unset; the router stamps it only after a successful
    /// physical response so fallback attribution and pricing use the model
    /// that actually ran instead of the initially requested profile.
    #[serde(skip, default)]
    pub route_identity: Option<LlmRouteIdentity>,
    /// Provider-emitted reasoning / chain-of-thought text. Distinct from
    /// `text` because reasoning content has different downstream handling
    /// (Anthropic requires unmodified replay for chained extended-thinking;
    /// most observers want a separate trace channel rather than inline).
    /// Populated from:
    /// - Anthropic / DeepSeek (Anthropic-compat): concatenated `thinking`
    ///   content block text.
    /// - OpenAI Responses: `output[]` items with `type: reasoning`,
    ///   joined `summary[].text`.
    /// - OpenAI Chat / DeepSeek-R1: `message.reasoning_content` field.
    /// `None` when the provider didn't emit reasoning or the model wasn't
    /// asked to reason. Trace `assistant_turn` events surface this
    /// alongside `text` so debugging "what did the model think" works
    /// across providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_text: Option<Arc<str>>,
    /// Server-side response identifier the next turn can pass back to
    /// continue the conversation in the provider's stateful mode.
    ///
    /// Populated only by providers that surface a stable response id:
    /// - **OpenAI Responses API**: top-level `id` field (e.g. `resp_*`).
    /// - **Gemini Interactions API**: top-level interaction `id` field.
    /// The runner carries either through typed `LLMRequest.context_reuse` on
    /// the next call, allowing the adapter to send only new input. Anthropic
    /// Messages and chat-completions transports remain stateless.
    ///
    /// **Profile-switch invariant**: this id is scoped to the provider, model,
    /// endpoint, and transport that produced it. The runner MUST clear it when
    /// that cohort changes. Reusing it across cohorts can corrupt the chain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "arc_vec_is_empty")]
    pub messages: Arc<Vec<LLMMessage>>,
    #[serde(default, skip_serializing_if = "arc_vec_is_empty")]
    pub tool_calls: Arc<Vec<LLMToolCall>>,
    #[serde(default, skip_serializing_if = "arc_vec_is_empty")]
    pub tool_results: Arc<Vec<LLMToolResult>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<TokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finish_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_response: Option<Arc<Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_latency_ms: Option<u64>,
}

impl LLMResponse {
    /// Consume a provider response only after its aggregate arbitrary JSON
    /// lanes pass the provider wire byte/depth/node boundary and the complete
    /// normalized response passes the independent retained-memory ceiling.
    /// Built-in adapters already admit parsed wire values, but custom provider
    /// implementations can construct unbounded text and `Value` trees
    /// programmatically and therefore cross this normalized boundary too.
    pub(crate) fn into_json_bounded(self) -> LLMResult<Self> {
        self.into_json_bounded_with_limits(
            MAX_PROVIDER_RESPONSE_JSON_DEPTH,
            MAX_PROVIDER_RESPONSE_JSON_NODES,
            MAX_PROVIDER_RESPONSE_JSON_BYTES,
            MAX_NORMALIZED_RESPONSE_JSON_BYTES,
            MAX_NORMALIZED_RESPONSE_RETAINED_BYTES,
        )
    }

    /// Re-check only the complete normalized response's retained heap after
    /// dispatch-owned metadata (notably the trace receipt) is attached. JSON
    /// already crossed the router's exact byte/depth/node boundary, so this
    /// avoids a second full encoding traversal on the active task path.
    pub(crate) fn into_retained_bounded(self) -> LLMResult<Self> {
        self.into_retained_bounded_with_limit(MAX_NORMALIZED_RESPONSE_RETAINED_BYTES)
    }

    fn into_retained_bounded_with_limit(mut self, max_retained_bytes: usize) -> LLMResult<Self> {
        if self.estimated_retained_bytes() <= max_retained_bytes {
            return Ok(self);
        }
        self.discard_json_lanes_iteratively();
        Err(LLMError::Validation(format!(
            "LLM provider response exceeds the admitted {}-byte retained ceiling",
            max_retained_bytes,
        )))
    }

    fn into_json_bounded_with_limits(
        mut self,
        max_depth: usize,
        max_nodes: usize,
        max_json_lane_bytes: usize,
        max_json_aggregate_bytes: usize,
        max_retained_bytes: usize,
    ) -> LLMResult<Self> {
        let mut remaining_nodes = max_nodes;
        let mut aggregate_json_bytes = 0usize;
        let mut admit = |value: &Value| {
            if !json_value_is_structurally_bounded(value, max_depth, &mut remaining_nodes) {
                return false;
            }
            let mut encoded = BoundedJsonLengthWriter::new(max_json_lane_bytes);
            if crate::context_reuse::write_json(&mut encoded, value).is_err() {
                return false;
            }
            let Some(next_aggregate) = aggregate_json_bytes.checked_add(encoded.bytes) else {
                return false;
            };
            if next_aggregate > max_json_aggregate_bytes {
                return false;
            }
            aggregate_json_bytes = next_aggregate;
            true
        };

        let messages_admitted = self.messages.iter().all(|message| {
            message.content.iter().all(|block| match block {
                ContentBlock::ToolCall { arguments, .. } => admit(arguments),
                ContentBlock::ToolResult { content, .. } => admit(content),
                ContentBlock::Json { value } => admit(value),
                ContentBlock::Text { .. }
                | ContentBlock::Image { .. }
                | ContentBlock::ImageUrl { .. } => true,
            })
        });
        let admitted = messages_admitted
            && self
                .tool_calls
                .iter()
                .all(|tool_call| admit(&tool_call.arguments))
            && self
                .tool_results
                .iter()
                .all(|tool_result| admit(&tool_result.output))
            && self.raw_response.as_deref().is_none_or(&mut admit);
        let retained_bytes = admitted.then(|| self.estimated_retained_bytes());
        if retained_bytes.is_some_and(|bytes| bytes <= max_retained_bytes) {
            return Ok(self);
        }

        // Rejection owns the provider's response envelope. Remove every JSON
        // lane before `self` unwinds, and recursively destruct only Arc owners
        // which are known unique. A shared Arc is merely decremented here; its
        // remaining external owner is not destroyed on this boundary.
        self.discard_json_lanes_iteratively();
        Err(LLMError::Validation(format!(
            "LLM provider response exceeds the admitted {}-byte JSON lane/{}-byte aggregate JSON/{}-level/{}-node/{}-byte retained ceiling",
            max_json_lane_bytes,
            max_json_aggregate_bytes,
            max_depth,
            max_nodes,
            max_retained_bytes,
        )))
    }

    fn discard_json_lanes_iteratively(&mut self) {
        discard_shared_messages(std::mem::take(&mut self.messages));
        discard_shared_tool_calls(std::mem::take(&mut self.tool_calls));
        discard_shared_tool_results(std::mem::take(&mut self.tool_results));
        if let Some(raw_response) = self.raw_response.take() {
            discard_shared_extra(raw_response);
        }
    }

    /// Copy-on-write access for the provider message lane. Cloning a response
    /// for idempotent consumers remains allocation-light until a consumer
    /// explicitly mutates the provider payload.
    pub fn messages_mut(&mut self) -> &mut Vec<LLMMessage> {
        messages_cow_mut(&mut self.messages)
    }

    /// Copy-on-write access for structured tool calls.
    pub fn tool_calls_mut(&mut self) -> &mut Vec<LLMToolCall> {
        tool_calls_cow_mut(&mut self.tool_calls)
    }

    /// Copy-on-write access for structured tool results.
    pub fn tool_results_mut(&mut self) -> &mut Vec<LLMToolResult> {
        tool_results_cow_mut(&mut self.tool_results)
    }

    /// Copy-on-write access for provider-native response state.
    pub fn raw_response_mut(&mut self) -> Option<&mut Value> {
        self.raw_response.as_mut().map(json_arc_cow_mut)
    }

    /// Whether every potentially large response lane is shared with `other`.
    /// Used by dispatch regressions to prove receipt stamping did not clone
    /// provider payloads at the public ownership boundary.
    pub fn shares_large_payload_with(&self, other: &Self) -> bool {
        (match (&self.text, &other.text) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }) && (match (&self.reasoning_text, &other.reasoning_text) {
            (Some(left), Some(right)) => Arc::ptr_eq(left, right),
            (None, None) => true,
            _ => false,
        }) && Arc::ptr_eq(&self.messages, &other.messages)
            && Arc::ptr_eq(&self.tool_calls, &other.tool_calls)
            && Arc::ptr_eq(&self.tool_results, &other.tool_results)
            && (match (&self.raw_response, &other.raw_response) {
                (Some(left), Some(right)) => Arc::ptr_eq(left, right),
                (None, None) => true,
                _ => false,
            })
    }

    /// Conservative, allocation-free estimate used to bound retained
    /// idempotency responses. JSON is walked with iterator frames whose
    /// auxiliary memory is proportional to depth, never total width, so
    /// adversarial shape cannot consume the native call stack or duplicate a
    /// wide object's child list.
    pub(crate) fn estimated_retained_bytes(&self) -> usize {
        let mut bytes = std::mem::size_of::<Self>();
        bytes = bytes.saturating_add(
            self.trace_receipt
                .as_ref()
                .map_or(0, estimated_trace_receipt_bytes),
        );
        bytes = bytes.saturating_add(self.route_identity.as_ref().map_or(0, |identity| {
            identity
                .profile
                .capacity()
                .saturating_add(identity.model.capacity())
                .saturating_add(match &identity.provider {
                    LLMProviderKind::Custom(provider) => provider.capacity(),
                    _ => 0,
                })
        }));
        bytes = bytes.saturating_add(self.text.as_ref().map_or(0, |value| value.len()));
        bytes = bytes.saturating_add(self.reasoning_text.as_ref().map_or(0, |value| value.len()));
        bytes = bytes.saturating_add(self.response_id.as_ref().map_or(0, String::capacity));
        bytes = bytes.saturating_add(self.finish_reason.as_ref().map_or(0, String::capacity));
        bytes = bytes.saturating_add(
            self.messages
                .capacity()
                .saturating_mul(std::mem::size_of::<LLMMessage>()),
        );
        bytes = bytes.saturating_add(
            self.messages
                .iter()
                .map(estimated_message_bytes)
                .fold(0usize, usize::saturating_add),
        );
        bytes = bytes.saturating_add(
            self.tool_calls
                .capacity()
                .saturating_mul(std::mem::size_of::<LLMToolCall>()),
        );
        bytes = bytes.saturating_add(
            self.tool_calls
                .iter()
                .map(|call| {
                    call.id
                        .capacity()
                        .saturating_add(call.name.capacity())
                        .saturating_add(estimated_json_bytes(&call.arguments))
                })
                .fold(0usize, usize::saturating_add),
        );
        bytes = bytes.saturating_add(
            self.tool_results
                .capacity()
                .saturating_mul(std::mem::size_of::<LLMToolResult>()),
        );
        bytes = bytes.saturating_add(
            self.tool_results
                .iter()
                .map(|result| {
                    result
                        .tool_call_id
                        .capacity()
                        .saturating_add(estimated_json_bytes(&result.output))
                })
                .fold(0usize, usize::saturating_add),
        );
        bytes.saturating_add(self.raw_response.as_deref().map_or(0, estimated_json_bytes))
    }
}

fn estimated_trace_receipt_bytes(receipt: &crate::trace::LlmTraceReceipt) -> usize {
    let context = &receipt.context;
    let mut bytes = context
        .trace_id
        .capacity()
        .saturating_add(context.llm_call_id.capacity())
        .saturating_add(context.parent_call_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.retry_group_id.as_ref().map_or(0, String::capacity))
        .saturating_add(
            context
                .route_decision_id
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(context.scope.principal.capacity())
        .saturating_add(context.scope.workspace.capacity())
        .saturating_add(context.task_id.as_ref().map_or(0, String::capacity))
        .saturating_add(
            context
                .root_execution_id
                .as_ref()
                .map_or(0, String::capacity),
        )
        .saturating_add(context.execution_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.plan_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.step_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.iteration_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.chat_session_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.chat_turn_id.as_ref().map_or(0, String::capacity))
        .saturating_add(context.user_message_id.as_ref().map_or(0, String::capacity));
    bytes = bytes.saturating_add(receipt.dispatch_job_id.as_ref().map_or(0, String::capacity));
    bytes.saturating_add(
        receipt
            .provider_attempt_id
            .as_ref()
            .map_or(0, String::capacity),
    )
}

fn arc_vec_is_empty<T>(values: &Arc<Vec<T>>) -> bool {
    values.is_empty()
}

fn estimated_message_bytes(message: &LLMMessage) -> usize {
    message
        .content
        .iter()
        .map(|block| match block {
            ContentBlock::Text { text } => text.capacity(),
            ContentBlock::Image {
                data,
                media_type,
                caption,
            } => data
                .capacity()
                .saturating_add(media_type.capacity())
                .saturating_add(caption.as_ref().map_or(0, String::capacity)),
            ContentBlock::ImageUrl { url, prompt } => url
                .capacity()
                .saturating_add(prompt.as_ref().map_or(0, String::capacity)),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => id
                .capacity()
                .saturating_add(name.capacity())
                .saturating_add(estimated_json_bytes(arguments)),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
            } => tool_call_id
                .capacity()
                .saturating_add(estimated_json_bytes(content)),
            ContentBlock::Json { value } => estimated_json_bytes(value),
        })
        .fold(
            message
                .content
                .capacity()
                .saturating_mul(std::mem::size_of::<ContentBlock>()),
            usize::saturating_add,
        )
}

fn estimated_json_bytes(root: &Value) -> usize {
    enum Frame<'a> {
        Array(std::slice::Iter<'a, Value>),
        Object(serde_json::map::Iter<'a>),
    }

    let mut bytes = 0usize;
    let mut current = Some(root);
    let mut frames = Vec::<Frame<'_>>::new();

    loop {
        let Some(value) = current.take() else {
            let mut next = None;
            while let Some(frame) = frames.last_mut() {
                match frame {
                    Frame::Array(values) => {
                        if let Some(value) = values.next() {
                            next = Some(value);
                            break;
                        }
                    },
                    Frame::Object(values) => {
                        if let Some((key, value)) = values.next() {
                            // Include both the key allocation and a conservative
                            // per-entry map-node allowance. Counting only key
                            // length badly underestimates very wide objects.
                            bytes = bytes
                                .saturating_add(std::mem::size_of::<String>())
                                .saturating_add(key.capacity())
                                .saturating_add(4 * std::mem::size_of::<usize>());
                            next = Some(value);
                            break;
                        }
                    },
                }
                frames.pop();
            }
            let Some(next) = next else {
                break;
            };
            current = Some(next);
            continue;
        };

        bytes = bytes.saturating_add(std::mem::size_of::<Value>());
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => {},
            Value::String(value) => bytes = bytes.saturating_add(value.capacity()),
            Value::Array(values) => {
                bytes = bytes.saturating_add(
                    values
                        .capacity()
                        .saturating_sub(values.len())
                        .saturating_mul(std::mem::size_of::<Value>()),
                );
                frames.push(Frame::Array(values.iter()));
            },
            Value::Object(values) => frames.push(Frame::Object(values.iter())),
        }
    }
    bytes
}

#[cfg(test)]
mod retained_json_estimate_tests {
    use super::*;

    fn discard_json_iteratively(root: Value) {
        let mut pending = vec![root];
        while let Some(value) = pending.pop() {
            match value {
                Value::Array(values) => pending.extend(values),
                Value::Object(values) => pending.extend(values.into_values()),
                Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {},
            }
        }
    }

    #[test]
    fn retained_json_estimate_uses_depth_bounded_traversal() {
        let mut value = Value::Null;
        for _ in 0..10_000 {
            value = Value::Array(vec![value]);
        }

        assert!(estimated_json_bytes(&value) >= 10_001 * std::mem::size_of::<Value>());
        discard_json_iteratively(value);
    }

    #[test]
    fn retained_json_estimate_does_not_allocate_one_slot_per_wide_child() {
        let value = Value::Array(vec![Value::Null; 100_000]);
        assert!(estimated_json_bytes(&value) >= 100_001 * std::mem::size_of::<Value>());
        // A one-level wide array is safe to drop normally; avoid making the
        // regression's cleanup itself allocate a width-sized traversal stack.
        drop(value);
    }

    #[test]
    fn retained_json_estimate_accounts_for_reserved_container_capacity() {
        let mut text = String::with_capacity(512 * 1024);
        text.push('x');
        let reserved = text.capacity();
        let value = Value::String(text);
        assert!(estimated_json_bytes(&value) >= reserved);

        let mut array = Vec::with_capacity(16_384);
        array.push(Value::Null);
        let reserved = array.capacity() * std::mem::size_of::<Value>();
        let value = Value::Array(array);
        assert!(estimated_json_bytes(&value) >= reserved);
    }

    #[test]
    fn retained_request_estimate_counts_wide_empty_lane_capacity() {
        let baseline = LLMRequest::default().estimated_retained_bytes();
        let mut wide = LLMRequest::default();
        wide.messages = Arc::new(Vec::with_capacity(4_096));
        wide.tools = Arc::new(Vec::with_capacity(4_096));
        wide.input_media = Some(Arc::new(Vec::with_capacity(4_096)));
        wide.summarisable_blocks = Arc::new(Vec::with_capacity(4_096));

        let structural_floor = 4_096usize.saturating_mul(
            std::mem::size_of::<LLMMessage>()
                .saturating_add(std::mem::size_of::<LLMToolSpec>())
                .saturating_add(std::mem::size_of::<MediaContent>())
                .saturating_add(std::mem::size_of::<SummarisableBlock>()),
        );
        assert!(
            wide.estimated_retained_bytes() >= baseline.saturating_add(structural_floor),
            "empty but preallocated structural lanes must not bypass byte admission",
        );
    }

    #[test]
    fn arc_backed_request_lanes_preserve_the_public_json_shape() {
        let request = LLMRequest {
            model: "wire-model".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            media: Some(Arc::new(vec![1, 2, 3])),
            input_media: Some(Arc::new(vec![MediaContent {
                media_type: "image/png".to_string(),
                data: Some(vec![4, 5]),
                url: None,
                description: Some("fixture".to_string()),
            }])),
            tools: vec![LLMToolSpec {
                name: "read".to_string(),
                description: "Read".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }]
            .into(),
            summarisable_blocks: vec![SummarisableBlock {
                message_index: 0,
                content_index: 0,
                raw: "large source".to_string(),
                purpose: SummarisationPurpose::LargeStepOutput,
                max_chars: Some(128),
            }]
            .into(),
            ..LLMRequest::default()
        };

        let encoded = serde_json::to_value(&request).expect("request serialization");
        assert!(encoded.get("messages").is_some_and(Value::is_array));
        assert!(encoded.get("media").is_some_and(Value::is_array));
        assert!(encoded.get("input_media").is_some_and(Value::is_array));
        assert!(encoded.get("tools").is_some_and(Value::is_array));
        assert!(encoded
            .get("summarisable_blocks")
            .is_some_and(Value::is_array));

        let decoded: LLMRequest =
            serde_json::from_value(encoded).expect("request wire shape remains deserializable");
        assert_eq!(decoded.messages.len(), 1);
        assert_eq!(
            decoded.media.as_deref().map(|media| media.as_slice()),
            Some(&[1, 2, 3][..])
        );
        assert_eq!(decoded.input_media.as_deref().map(Vec::len), Some(1));
        assert_eq!(decoded.tools.len(), 1);
        assert_eq!(decoded.summarisable_blocks.len(), 1);
    }

    #[test]
    fn request_clone_shares_provider_state_schema_and_context_until_cow() {
        use crate::context_reuse::{ContextReuseConfig, ContextReuseStrategy};

        let mut request = LLMRequest::default();
        request.set_extra(serde_json::json!({"provider": {"state": [1, 2, 3]}}));
        request.set_response_format(LLMResponseFormat::JsonSchema {
            schema: serde_json::json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
        });
        request.set_context_reuse(ContextReuseConfig::new(
            ContextReuseStrategy::ServerContinuation,
        ));

        assert!(request.json_payloads_are_bounded());
        assert_eq!(request.json_admission_scan_passes(), 1);
        let mut cloned = request.clone();
        assert!(request.shares_large_payload_with(&cloned));
        assert!(cloned.json_payloads_are_bounded());
        assert_eq!(cloned.json_admission_scan_passes(), 1);
        let mut extra = cloned.take_extra_value().expect("shared provider state");
        extra["provider"]["state"] = serde_json::json!([4]);
        cloned.set_extra(extra);
        assert!(cloned.json_payloads_are_bounded());
        assert_eq!(cloned.json_admission_scan_passes(), 2);
        assert_eq!(request.json_admission_scan_passes(), 1);

        assert_eq!(
            request
                .extra_value()
                .and_then(|v| v.pointer("/provider/state/0"))
                .and_then(Value::as_i64),
            Some(1)
        );
        assert_eq!(
            cloned
                .extra_value()
                .and_then(|v| v.pointer("/provider/state/0"))
                .and_then(Value::as_i64),
            Some(4)
        );
        assert!(!Arc::ptr_eq(
            request.extra.as_ref().expect("original extra"),
            cloned.extra.as_ref().expect("cloned extra"),
        ));
        assert!(Arc::ptr_eq(
            request.response_format.as_ref().expect("original schema"),
            cloned.response_format.as_ref().expect("cloned schema"),
        ));
        assert!(Arc::ptr_eq(
            request.context_reuse.as_ref().expect("original reuse"),
            cloned.context_reuse.as_ref().expect("cloned reuse"),
        ));
    }

    #[test]
    fn valid_admission_does_not_add_a_hidden_owner_or_force_local_prep_cow() {
        let mut request = LLMRequest::default();
        request.set_extra(serde_json::json!({"provider": {"state": [1, 2, 3]}}));
        assert_eq!(
            Arc::strong_count(request.extra.as_ref().expect("provider state")),
            1,
            "construction must leave a uniquely owned provider-state lane",
        );

        assert!(request.json_payloads_are_bounded());
        assert_eq!(
            Arc::strong_count(request.extra.as_ref().expect("provider state")),
            1,
            "successful admission must not arm the rejected-value drop guard",
        );

        let mut extra = request
            .take_extra_value()
            .expect("unique local-prep provider state");
        extra["provider"]["state"] = serde_json::json!([4]);
        request.set_extra(extra);
        assert_eq!(
            Arc::strong_count(request.extra.as_ref().expect("updated provider state")),
            1,
            "local prep must preserve the unique fast path",
        );
    }

    #[test]
    fn admission_revalidates_direct_public_arc_lane_replacements() {
        fn over_depth() -> Value {
            let mut value = Value::Null;
            for _ in 0..=MAX_LLM_REQUEST_JSON_DEPTH {
                value = Value::Array(vec![value]);
            }
            value
        }

        let admitted = LLMRequest::default();
        assert!(admitted.json_payloads_are_bounded());
        assert_eq!(admitted.json_admission_scan_passes(), 1);
        let unchanged = admitted.clone();
        assert!(unchanged.json_payloads_are_bounded());
        assert_eq!(unchanged.json_admission_scan_passes(), 1);

        let mut extra = admitted.clone();
        extra.extra = Some(Arc::new(over_depth()));
        let passes_before_extra = extra.json_admission_scan_passes();
        assert!(!extra.json_payloads_are_bounded());
        assert_eq!(extra.json_admission_scan_passes(), passes_before_extra + 1,);

        let mut response = admitted.clone();
        response.response_format = Some(Arc::new(LLMResponseFormat::JsonSchema {
            schema: over_depth(),
        }));
        let passes_before_response = response.json_admission_scan_passes();
        assert!(!response.json_payloads_are_bounded());
        assert_eq!(
            response.json_admission_scan_passes(),
            passes_before_response + 1,
        );

        let mut tools = admitted.clone();
        tools.tools = Arc::new(vec![LLMToolSpec {
            name: "deep".to_string(),
            description: String::new(),
            parameters: over_depth(),
        }]);
        let passes_before_tools = tools.json_admission_scan_passes();
        assert!(!tools.json_payloads_are_bounded());
        assert_eq!(tools.json_admission_scan_passes(), passes_before_tools + 1,);

        let mut messages = admitted.clone();
        messages.messages = Arc::new(vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Json {
                value: over_depth(),
            }],
        }]);
        let passes_before_messages = messages.json_admission_scan_passes();
        assert!(!messages.json_payloads_are_bounded());
        assert_eq!(
            messages.json_admission_scan_passes(),
            passes_before_messages + 1,
        );
    }

    #[test]
    fn tool_argument_admission_preserves_small_malformed_fallback_and_rejects_valid_overages() {
        assert_eq!(
            parse_tool_argument_json_or_string("{not-json").expect("small malformed fallback"),
            Value::String("{not-json".to_string())
        );

        let exact_depth = format!(
            "{}null{}",
            "[".repeat(MAX_TOOL_ARGUMENT_JSON_DEPTH),
            "]".repeat(MAX_TOOL_ARGUMENT_JSON_DEPTH)
        );
        assert!(parse_tool_argument_json_or_string(&exact_depth).is_ok());
        let over_depth = format!(
            "{}null{}",
            "[".repeat(MAX_TOOL_ARGUMENT_JSON_DEPTH + 1),
            "]".repeat(MAX_TOOL_ARGUMENT_JSON_DEPTH + 1)
        );
        assert!(matches!(
            parse_tool_argument_json_or_string(&over_depth),
            Err(LLMError::Validation(_))
        ));

        // Syntax-invalid containers are still subject to the structural-stack
        // ceiling before Serde sees them. Repeated object openings keep
        // `expects_value` false after the first byte, which previously bypassed
        // the depth guard and let the scanner's Vec grow with the entire input.
        let malformed_over_depth = "{".repeat(MAX_TOOL_ARGUMENT_JSON_DEPTH + 1);
        assert!(!encoded_json_is_bounded(
            malformed_over_depth.as_bytes(),
            MAX_TOOL_ARGUMENT_JSON_DEPTH,
            MAX_TOOL_ARGUMENT_JSON_NODES,
        ));
        assert!(matches!(
            parse_tool_argument_json_or_string(&malformed_over_depth),
            Err(LLMError::Validation(_))
        ));

        let exact_bytes = format!(
            "\"{}\"",
            "a".repeat(MAX_TOOL_ARGUMENT_JSON_BYTES.saturating_sub(2))
        );
        assert!(parse_tool_argument_json_or_string(&exact_bytes).is_ok());
        let over_bytes = format!(
            "\"{}\"",
            "a".repeat(MAX_TOOL_ARGUMENT_JSON_BYTES.saturating_sub(1))
        );
        assert!(matches!(
            parse_tool_argument_json_or_string(&over_bytes),
            Err(LLMError::Validation(_))
        ));

        let exact_nodes = format!(
            "[{}]",
            "null,".repeat(MAX_TOOL_ARGUMENT_JSON_NODES - 2) + "null"
        );
        assert!(parse_tool_argument_json_or_string(&exact_nodes).is_ok());
        let over_nodes = format!(
            "[{}]",
            "null,".repeat(MAX_TOOL_ARGUMENT_JSON_NODES - 1) + "null"
        );
        assert!(matches!(
            parse_tool_argument_json_or_string(&over_nodes),
            Err(LLMError::Validation(_))
        ));

        let mut streamed = "a".repeat(MAX_TOOL_ARGUMENT_JSON_BYTES - 1);
        append_tool_argument_fragment(&mut streamed, "b").expect("exact streamed byte cap");
        let before = streamed.len();
        assert!(matches!(
            append_tool_argument_fragment(&mut streamed, "c"),
            Err(LLMError::Validation(_))
        ));
        assert_eq!(
            streamed.len(),
            before,
            "rejection must not retain a fragment"
        );
    }

    #[test]
    fn json_or_string_admission_scans_only_container_looking_input() {
        let brace_heavy_prose = format!(
            "provider said this was plain text: {}",
            "{".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH + 1),
        );
        assert_eq!(
            parse_provider_json_or_string(&brace_heavy_prose)
                .expect("bounded non-JSON provider text retains legacy fallback"),
            Value::String(brace_heavy_prose.clone()),
        );
        assert_eq!(
            parse_tool_argument_json_or_string(&brace_heavy_prose)
                .expect("bounded non-JSON tool text retains legacy fallback"),
            Value::String(brace_heavy_prose),
        );

        assert_eq!(
            parse_provider_json_or_string("  42").expect("scalar provider JSON"),
            Value::from(42),
        );
        assert_eq!(
            parse_tool_argument_json_or_string("\ntrue").expect("scalar tool JSON"),
            Value::Bool(true),
        );

        let malformed_container = "{".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH + 1);
        assert!(matches!(
            parse_provider_json_or_string(&malformed_container),
            Err(LLMError::Validation(_)),
        ));
        assert!(matches!(
            parse_tool_argument_json_or_string(&malformed_container),
            Err(LLMError::Validation(_)),
        ));
    }

    #[test]
    fn rejected_deep_request_drains_the_last_json_owner_on_a_small_stack() {
        std::thread::Builder::new()
            .name("llm-request-json-drain".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut deep = Value::Null;
                for _ in 0..10_000 {
                    deep = Value::Array(vec![deep]);
                }
                let mut request = LLMRequest::default();
                request.set_extra(deep);
                assert!(!request.json_payloads_are_bounded());
                request.discard_json_payloads_iteratively();
            })
            .expect("small-stack request worker")
            .join()
            .expect("deep last-owner rejection must not overflow");
    }

    #[test]
    fn cloned_rejected_request_drains_the_non_submit_last_owner_on_a_small_stack() {
        std::thread::Builder::new()
            .name("llm-shared-request-json-drain".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut deep = Value::Null;
                for _ in 0..10_000 {
                    deep = Value::Array(vec![deep]);
                }
                let mut submitted = LLMRequest::default();
                // Exercise the public-field compatibility path rather than a
                // setter: admission must arm the guard for every shallow clone.
                submitted.extra = Some(Arc::new(deep));
                let surviving_caller_clone = submitted.clone();
                assert!(!submitted.json_payloads_are_bounded());
                submitted.discard_json_payloads_iteratively();
                drop(surviving_caller_clone);
            })
            .expect("small-stack shared request worker")
            .join()
            .expect("shared deep last owner must drain iteratively");
    }

    #[test]
    fn pre_admission_request_cow_clones_deep_json_with_heap_frames() {
        fn deep_array(depth: usize) -> Value {
            let mut value = Value::Null;
            for _ in 0..depth {
                value = Value::Array(vec![value]);
            }
            value
        }

        fn assert_array_depth(mut value: &Value, depth: usize) {
            for _ in 0..depth {
                let Value::Array(children) = value else {
                    panic!("deep fixture lost an array level");
                };
                assert_eq!(children.len(), 1);
                value = &children[0];
            }
            assert!(value.is_null());
        }

        std::thread::Builder::new()
            .name("llm-request-cow-json-clone".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                const DEPTH: usize = 10_000;
                let mut original = LLMRequest::default();
                original.messages = Arc::new(vec![LLMMessage {
                    role: MessageRole::User,
                    content: vec![
                        ContentBlock::Text {
                            text: "preserved text".to_string(),
                        },
                        ContentBlock::Image {
                            data: vec![1, 2, 3, 4],
                            media_type: "image/test".to_string(),
                            caption: Some("preserved caption".to_string()),
                        },
                        ContentBlock::ImageUrl {
                            url: "https://example.invalid/image".to_string(),
                            prompt: Some("preserved prompt".to_string()),
                        },
                        ContentBlock::ToolCall {
                            id: "call-deep".to_string(),
                            name: "deep_call".to_string(),
                            arguments: deep_array(DEPTH),
                        },
                        ContentBlock::ToolResult {
                            tool_call_id: "call-deep".to_string(),
                            content: deep_array(DEPTH),
                        },
                        ContentBlock::Json {
                            value: deep_array(DEPTH),
                        },
                    ],
                }]);
                original.tools = Arc::new(vec![LLMToolSpec {
                    name: "deep_tool".to_string(),
                    description: "preserved description".to_string(),
                    parameters: deep_array(DEPTH),
                }]);

                let mut cow = original.clone();
                cow.messages_mut()[0].role = MessageRole::Assistant;
                cow.tools_mut()[0].name.push_str("_mutated");

                assert!(!Arc::ptr_eq(&original.messages, &cow.messages));
                assert!(!Arc::ptr_eq(&original.tools, &cow.tools));
                assert!(matches!(original.messages[0].role, MessageRole::User));
                assert!(matches!(cow.messages[0].role, MessageRole::Assistant));
                assert_eq!(original.tools[0].name, "deep_tool");
                assert_eq!(cow.tools[0].name, "deep_tool_mutated");
                assert_eq!(cow.tools[0].description, "preserved description");

                let blocks = &cow.messages[0].content;
                assert!(matches!(
                    &blocks[0],
                    ContentBlock::Text { text } if text == "preserved text"
                ));
                assert!(matches!(
                    &blocks[1],
                    ContentBlock::Image { data, media_type, caption }
                        if data.as_slice() == &[1, 2, 3, 4]
                            && media_type == "image/test"
                            && caption.as_deref() == Some("preserved caption")
                ));
                assert!(matches!(
                    &blocks[2],
                    ContentBlock::ImageUrl { url, prompt }
                        if url == "https://example.invalid/image"
                            && prompt.as_deref() == Some("preserved prompt")
                ));
                match &blocks[3] {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        assert_eq!(id, "call-deep");
                        assert_eq!(name, "deep_call");
                        assert_array_depth(arguments, DEPTH);
                    },
                    _ => panic!("tool-call block changed variant"),
                }
                match &blocks[4] {
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                    } => {
                        assert_eq!(tool_call_id, "call-deep");
                        assert_array_depth(content, DEPTH);
                    },
                    _ => panic!("tool-result block changed variant"),
                }
                match &blocks[5] {
                    ContentBlock::Json { value } => assert_array_depth(value, DEPTH),
                    _ => panic!("JSON block changed variant"),
                }
                assert_array_depth(&cow.tools[0].parameters, DEPTH);

                assert!(!original.json_payloads_are_bounded());
                assert!(!cow.json_payloads_are_bounded());
                drop(original);
                drop(cow);
            })
            .expect("small-stack request COW worker")
            .join()
            .expect("deep request COW must not overflow");
    }

    #[test]
    fn response_cow_clones_programmatic_deep_json_with_heap_frames() {
        fn deep_array(depth: usize) -> Value {
            let mut value = Value::Null;
            for _ in 0..depth {
                value = Value::Array(vec![value]);
            }
            value
        }

        fn assert_array_depth(mut value: &Value, depth: usize) {
            for _ in 0..depth {
                let Value::Array(children) = value else {
                    panic!("deep response fixture lost an array level");
                };
                assert_eq!(children.len(), 1);
                value = &children[0];
            }
            assert!(value.is_null());
        }

        fn discard_response_json_iteratively(mut response: LLMResponse) {
            discard_shared_messages(std::mem::take(&mut response.messages));
            if let Ok(tool_calls) = Arc::try_unwrap(std::mem::take(&mut response.tool_calls)) {
                for tool_call in tool_calls {
                    discard_json_value_iteratively(tool_call.arguments);
                }
            }
            if let Ok(tool_results) = Arc::try_unwrap(std::mem::take(&mut response.tool_results)) {
                for tool_result in tool_results {
                    discard_json_value_iteratively(tool_result.output);
                }
            }
            if let Some(raw_response) = response.raw_response.take() {
                discard_shared_extra(raw_response);
            }
        }

        std::thread::Builder::new()
            .name("llm-response-cow-json-clone".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                const DEPTH: usize = 10_000;
                let original = LLMResponse {
                    messages: Arc::new(vec![LLMMessage {
                        role: MessageRole::Assistant,
                        content: vec![
                            ContentBlock::Text {
                                text: "provider narration".to_string(),
                            },
                            ContentBlock::Json {
                                value: deep_array(DEPTH),
                            },
                        ],
                    }]),
                    tool_calls: Arc::new(vec![LLMToolCall {
                        id: "response-call".to_string(),
                        name: "response_tool".to_string(),
                        arguments: deep_array(DEPTH),
                    }]),
                    tool_results: Arc::new(vec![LLMToolResult {
                        tool_call_id: "response-call".to_string(),
                        output: deep_array(DEPTH),
                    }]),
                    raw_response: Some(Arc::new(deep_array(DEPTH))),
                    ..LLMResponse::default()
                };
                let mut cow = original.clone();

                cow.messages_mut()[0].role = MessageRole::Tool;
                cow.tool_calls_mut()[0].name.push_str("_mutated");
                cow.tool_results_mut()[0].tool_call_id.push_str("_mutated");
                assert_array_depth(cow.raw_response_mut().expect("raw response lane"), DEPTH);

                assert!(!Arc::ptr_eq(&original.messages, &cow.messages));
                assert!(!Arc::ptr_eq(&original.tool_calls, &cow.tool_calls));
                assert!(!Arc::ptr_eq(&original.tool_results, &cow.tool_results));
                assert!(!Arc::ptr_eq(
                    original
                        .raw_response
                        .as_ref()
                        .expect("original raw response"),
                    cow.raw_response.as_ref().expect("COW raw response"),
                ));
                assert!(matches!(original.messages[0].role, MessageRole::Assistant));
                assert!(matches!(cow.messages[0].role, MessageRole::Tool));
                assert!(matches!(
                    &cow.messages[0].content[0],
                    ContentBlock::Text { text } if text == "provider narration"
                ));
                match &cow.messages[0].content[1] {
                    ContentBlock::Json { value } => assert_array_depth(value, DEPTH),
                    _ => panic!("response JSON block changed variant"),
                }
                assert_eq!(original.tool_calls[0].name, "response_tool");
                assert_eq!(cow.tool_calls[0].name, "response_tool_mutated");
                assert_eq!(original.tool_results[0].tool_call_id, "response-call");
                assert_eq!(cow.tool_results[0].tool_call_id, "response-call_mutated");
                assert_array_depth(&cow.tool_calls[0].arguments, DEPTH);
                assert_array_depth(&cow.tool_results[0].output, DEPTH);

                discard_response_json_iteratively(original);
                discard_response_json_iteratively(cow);
            })
            .expect("small-stack response COW worker")
            .join()
            .expect("deep response COW must not overflow");
    }

    #[test]
    fn provider_json_depth_contract_parses_and_drops_exact_boundary_on_small_stack() {
        std::thread::Builder::new()
            .name("provider-json-boundary".to_string())
            .stack_size(512 * 1024)
            .spawn(|| {
                let exact = format!(
                    "{}null{}",
                    "[".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH),
                    "]".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH)
                );
                let value = parse_provider_json_value(&exact).expect("exact depth boundary");
                let response = LLMResponse {
                    raw_response: Some(Arc::new(value)),
                    ..LLMResponse::default()
                };
                drop(response);

                let over = format!(
                    "{}null{}",
                    "[".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH + 1),
                    "]".repeat(MAX_PROVIDER_RESPONSE_JSON_DEPTH + 1)
                );
                assert!(matches!(
                    parse_provider_json_value(&over),
                    Err(LLMError::Validation(_))
                ));
            })
            .expect("provider JSON small-stack worker")
            .join()
            .expect("bounded provider JSON parse/drop must not overflow");
    }

    #[test]
    fn programmatic_response_admission_preserves_exact_container_depth_boundary() {
        let mut exact = Value::Null;
        for _ in 0..MAX_PROVIDER_RESPONSE_JSON_DEPTH {
            exact = Value::Array(vec![exact]);
        }
        let response = LLMResponse {
            raw_response: Some(Arc::new(exact)),
            ..LLMResponse::default()
        };
        let mut admitted = response
            .into_json_bounded()
            .expect("exact structural response depth remains accepted");
        if let Some(raw_response) = admitted.raw_response.take() {
            discard_shared_extra(raw_response);
        }

        // An empty container still consumes depth even without a scalar child.
        let mut over = Value::Array(Vec::new());
        for _ in 0..MAX_PROVIDER_RESPONSE_JSON_DEPTH {
            over = Value::Array(vec![over]);
        }
        let over_response = LLMResponse {
            raw_response: Some(Arc::new(over)),
            ..LLMResponse::default()
        };
        assert!(matches!(
            over_response.into_json_bounded(),
            Err(LLMError::Validation(_))
        ));
    }

    #[test]
    fn normalized_retained_boundary_counts_route_and_trace_metadata() {
        fn fixture() -> LLMResponse {
            let mut context = crate::trace::LlmTraceContext::new(
                crate::trace::LlmScope::new("p".repeat(1_024), "w".repeat(1_024)),
                crate::trace::LlmWorkloadClass::Evaluation,
            );
            context.trace_id = "t".repeat(1_024);
            context.llm_call_id = "c".repeat(1_024);
            LLMResponse {
                trace_receipt: Some(crate::trace::LlmTraceReceipt::queued(
                    context,
                    "j".repeat(1_024),
                    1,
                )),
                route_identity: Some(LlmRouteIdentity {
                    profile: "r".repeat(1_024),
                    provider: LLMProviderKind::Custom("v".repeat(1_024)),
                    model: "m".repeat(1_024),
                }),
                ..LLMResponse::default()
            }
        }

        let baseline = LLMResponse::default().estimated_retained_bytes();
        let response = fixture();
        let retained = response.estimated_retained_bytes();
        assert!(
            retained >= baseline.saturating_add(8 * 1_024),
            "trace and route allocations must contribute to normalized retention",
        );
        assert!(response.into_retained_bounded_with_limit(retained).is_ok());
        assert!(matches!(
            fixture().into_retained_bounded_with_limit(retained.saturating_sub(1)),
            Err(LLMError::Validation(_)),
        ));
    }

    #[test]
    fn programmatic_response_byte_and_retained_boundaries_are_aggregate_and_stack_safe() {
        std::thread::Builder::new()
            .name("programmatic-response-byte-boundary".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let exact_json = LLMResponse {
                    raw_response: Some(Arc::new(Value::String("x".repeat(62)))),
                    ..LLMResponse::default()
                };
                assert!(exact_json
                    .into_json_bounded_with_limits(
                        MAX_PROVIDER_RESPONSE_JSON_DEPTH,
                        MAX_PROVIDER_RESPONSE_JSON_NODES,
                        64,
                        64,
                        usize::MAX,
                    )
                    .is_ok());

                fn aggregate_fixture() -> LLMResponse {
                    LLMResponse {
                        messages: Arc::new(vec![LLMMessage {
                            role: MessageRole::Assistant,
                            content: vec![ContentBlock::Json {
                                value: Value::String("aa".to_string()),
                            }],
                        }]),
                        tool_calls: Arc::new(vec![LLMToolCall {
                            id: "call".to_string(),
                            name: "tool".to_string(),
                            arguments: Value::String("bb".to_string()),
                        }]),
                        tool_results: Arc::new(vec![LLMToolResult {
                            tool_call_id: "call".to_string(),
                            output: Value::String("cc".to_string()),
                        }]),
                        raw_response: Some(Arc::new(Value::String("dd".to_string()))),
                        ..LLMResponse::default()
                    }
                }
                // Every arbitrary response lane contributes one four-byte
                // string. The 16-byte aggregate fits exactly but cannot pass
                // a 15-byte ceiling even though each lane is individually
                // below its four-byte limit.
                assert!(aggregate_fixture()
                    .into_json_bounded_with_limits(
                        MAX_PROVIDER_RESPONSE_JSON_DEPTH,
                        MAX_PROVIDER_RESPONSE_JSON_NODES,
                        4,
                        16,
                        usize::MAX,
                    )
                    .is_ok());
                assert!(matches!(
                    aggregate_fixture().into_json_bounded_with_limits(
                        MAX_PROVIDER_RESPONSE_JSON_DEPTH,
                        MAX_PROVIDER_RESPONSE_JSON_NODES,
                        4,
                        15,
                        usize::MAX,
                    ),
                    Err(LLMError::Validation(_)),
                ));

                fn retained_fixture() -> LLMResponse {
                    LLMResponse {
                        text: Some(Arc::<str>::from("visible answer")),
                        reasoning_text: Some(Arc::<str>::from("private reasoning")),
                        messages: Arc::new(vec![LLMMessage {
                            role: MessageRole::Assistant,
                            content: vec![ContentBlock::Text {
                                text: "typed message".to_string(),
                            }],
                        }]),
                        tool_calls: Arc::new(vec![LLMToolCall {
                            id: "call".to_string(),
                            name: "tool".to_string(),
                            arguments: Value::Null,
                        }]),
                        ..LLMResponse::default()
                    }
                }
                let exact_retained = retained_fixture();
                let retained_limit = exact_retained.estimated_retained_bytes();
                assert!(exact_retained
                    .into_json_bounded_with_limits(
                        MAX_PROVIDER_RESPONSE_JSON_DEPTH,
                        MAX_PROVIDER_RESPONSE_JSON_NODES,
                        MAX_PROVIDER_RESPONSE_JSON_BYTES,
                        MAX_NORMALIZED_RESPONSE_JSON_BYTES,
                        retained_limit,
                    )
                    .is_ok());
                let rejected_retained = retained_fixture();
                let rejected_limit = rejected_retained.estimated_retained_bytes();
                assert!(matches!(
                    rejected_retained.into_json_bounded_with_limits(
                        MAX_PROVIDER_RESPONSE_JSON_DEPTH,
                        MAX_PROVIDER_RESPONSE_JSON_NODES,
                        MAX_PROVIDER_RESPONSE_JSON_BYTES,
                        MAX_NORMALIZED_RESPONSE_JSON_BYTES,
                        rejected_limit.saturating_sub(1),
                    ),
                    Err(LLMError::Validation(_)),
                ));

                // Shallow programmatic JSON preserves the production boundary
                // exactly without allocating a serialized copy.
                let exact_shallow = LLMResponse {
                    raw_response: Some(Arc::new(Value::String(
                        "z".repeat(MAX_PROVIDER_RESPONSE_JSON_BYTES.saturating_sub(2)),
                    ))),
                    ..LLMResponse::default()
                };
                assert!(exact_shallow.into_json_bounded().is_ok());
                let oversized_shallow = LLMResponse {
                    raw_response: Some(Arc::new(Value::String(
                        "z".repeat(MAX_PROVIDER_RESPONSE_JSON_BYTES.saturating_sub(1)),
                    ))),
                    ..LLMResponse::default()
                };
                assert!(matches!(
                    oversized_shallow.into_json_bounded(),
                    Err(LLMError::Validation(_)),
                ));
            })
            .expect("small-stack response byte worker")
            .join()
            .expect("response byte rejection must not overflow");
    }
}

/// A single streaming delta from an LLM provider.
///
/// The first three variants (`Token`, `ToolCallDelta`, `Done`/`Error`) are the
/// classic shape the Anthropic / OpenAI providers emitted before
/// magicllm v0.1.33. The remaining variants are first-class lifecycle events
/// for the AG-UI-style streaming taxonomy: paired `*Start` / `*Delta` /
/// `*End` triads for reasoning blocks and tool-use blocks. Providers that
/// surface block-level boundaries (Anthropic) emit the new variants in
/// addition to the classic ones; providers that don't fire only the
/// classic shape, and consumers fall back to the post-`Done` reassembled
/// `LLMResponse` for those.
#[derive(Debug, Clone)]
pub enum StreamDelta {
    /// A text token fragment.
    Token(String),
    /// A tool call fragment (id, name, argument chunk).
    ToolCallDelta {
        id: String,
        name: Option<String>,
        arguments_chunk: String,
    },
    /// An extended-thinking / reasoning block opened. `index` matches the
    /// provider's content-block ordering so consumers can correlate
    /// `Delta` and `End` for the same block.
    ReasoningStart {
        index: usize,
        signature: Option<String>,
    },
    /// A streaming chunk of reasoning text. Concatenate by `index`.
    ReasoningDelta { index: usize, delta: String },
    /// The reasoning block closed. `total_chars` is the assembled length
    /// observed at close time (best-effort; providers may report 0).
    ReasoningEnd { index: usize, total_chars: usize },
    /// A tool-use block opened. `call_id` matches the provider's id; the
    /// later assembled `LLMToolCall` in the final response uses the same id.
    ToolCallStart { call_id: String, tool_name: String },
    /// A streaming chunk of tool-call argument JSON. Concatenate by
    /// `call_id` to reassemble the full argument string.
    ToolCallArgsDelta { call_id: String, delta: String },
    /// A tool-use block closed.
    ToolCallEnd { call_id: String },
    /// Stream complete — carries the final aggregated response.
    Done(LLMResponse),
    /// Stream error.
    Error(String),
}

/// A side-channel sink the inner-loop runner can attach to an
/// [`LLMRequest`] so it observes streaming-delta events in addition to
/// (or instead of) the classic `mpsc::Sender<StreamDelta>` path. Every
/// delta the provider emits is also forwarded here when a sink is
/// attached. Clones share the underlying `Arc`; default is no-op.
///
/// Used for: realtime UI fan-out (forwarding deltas to
/// `RuntimeTransportBroadcaster::emit_tool_call_args` /
/// `emit_reasoning_content` / etc.) without refactoring every call site
/// to switch from `invoke_complete` to `invoke_stream`. Providers that
/// stream internally for `invoke_complete` can fire the sink even when
/// the public API contract is "non-streaming".
#[derive(Clone, Default)]
pub struct StreamEventSink(pub Option<std::sync::Arc<dyn Fn(StreamDelta) + Send + Sync + 'static>>);

impl StreamEventSink {
    /// Build a sink from any closure. Returns a default no-op sink when
    /// the input is `None`.
    pub fn new<F>(f: Option<F>) -> Self
    where
        F: Fn(StreamDelta) + Send + Sync + 'static,
    {
        Self(
            f.map(|cb| {
                std::sync::Arc::new(cb) as std::sync::Arc<dyn Fn(StreamDelta) + Send + Sync>
            }),
        )
    }

    /// Fire the sink (no-op when not attached).
    pub fn fire(&self, delta: StreamDelta) {
        if let Some(cb) = &self.0 {
            cb(delta);
        }
    }

    pub fn is_attached(&self) -> bool {
        self.0.is_some()
    }
}

impl std::fmt::Debug for StreamEventSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamEventSink")
            .field("attached", &self.is_attached())
            .finish()
    }
}

tokio::task_local! {
    /// Task-local fan-out sink. Inner-loop callers set this via
    /// [`scoped_stream_event_sink`] before invoking an LLM call, so the
    /// provider receives the sink even though the call is made through
    /// the router/chain layers that don't (yet) know about
    /// `LLMRequest.stream_event_sink`. Providers prefer the request-scoped
    /// sink when attached, falling back to this task-local.
    pub static STREAM_EVENT_SINK: StreamEventSink;
}

/// Resolve the active fan-out sink for the current async task.
/// Returns a no-op sink when neither
/// [`LLMRequest.stream_event_sink`] nor the task-local
/// [`STREAM_EVENT_SINK`] is set.
pub fn current_stream_event_sink() -> StreamEventSink {
    STREAM_EVENT_SINK
        .try_with(|s| s.clone())
        .unwrap_or_default()
}

/// Convenience: run `future` with `sink` installed as the task-local
/// [`STREAM_EVENT_SINK`]. Tokio task-locals propagate across `tokio::spawn`,
/// so child tasks inherit the same sink unless they explicitly re-scope.
pub async fn scoped_stream_event_sink<F, R>(sink: StreamEventSink, future: F) -> R
where
    F: std::future::Future<Output = R>,
{
    STREAM_EVENT_SINK.scope(sink, future).await
}

fn default_modality() -> LLMModality {
    LLMModality::Text
}

#[cfg(test)]
mod disclosure_guard_tests {
    use super::*;

    #[derive(Debug)]
    struct SecretBearingAuthorizer {
        diagnostic_secret: &'static str,
    }

    #[async_trait::async_trait]
    impl LlmDisclosureAuthorizer for SecretBearingAuthorizer {
        async fn revalidate(
            &self,
            _profile: &str,
            _provider: &LLMProviderKind,
            _model: &str,
            _api_base_url: Option<&str>,
        ) -> Result<(), String> {
            let _ = self.diagnostic_secret;
            Ok(())
        }
    }

    #[test]
    fn disclosure_guard_debug_never_formats_authorizer_state() {
        let guard = LlmDisclosureGuard::new(
            "app-local",
            "0".repeat(64),
            "continuation-partition",
            "policy-digest",
            LlmDisclosureCapturePolicy::MetadataOnly,
            Arc::new(SecretBearingAuthorizer {
                diagnostic_secret: "must-never-enter-diagnostics",
            }),
        )
        .expect("valid disclosure guard");

        let debug = format!("{guard:?}");
        assert!(debug.contains("authorizer: \"<opaque>\""));
        assert!(!debug.contains("must-never-enter-diagnostics"));
        assert!(!debug.contains("SecretBearingAuthorizer"));
    }
}

#[cfg(test)]
mod cache_sentinel_tests {
    use super::*;

    #[test]
    fn split_returns_none_when_sentinel_absent() {
        let (prefix, suffix) = split_on_cache_sentinel("hello world");
        assert_eq!(prefix, "hello world");
        assert!(suffix.is_none());
    }

    #[test]
    fn split_returns_halves_and_strips_sentinel_line() {
        let text = "before\n<!--MAGICIAN_CACHE_BREAKPOINT-->\nafter";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "before");
        assert_eq!(suffix.as_deref(), Some("after"));
    }

    #[test]
    fn split_strips_full_sentinel_line_with_surrounding_newlines() {
        let text = "stable content\n\n<!--MAGICIAN_CACHE_BREAKPOINT-->\n\nvolatile content";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "stable content\n");
        assert_eq!(suffix.as_deref(), Some("\nvolatile content"));
    }

    #[test]
    fn split_handles_sentinel_at_very_start() {
        let text = "<!--MAGICIAN_CACHE_BREAKPOINT-->\nonly-volatile";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "");
        assert_eq!(suffix.as_deref(), Some("only-volatile"));
    }

    #[test]
    fn split_handles_sentinel_at_very_end() {
        let text = "only-stable\n<!--MAGICIAN_CACHE_BREAKPOINT-->";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "only-stable");
        assert_eq!(suffix.as_deref(), Some(""));
    }

    #[test]
    fn split_uses_first_sentinel_and_leaves_later_ones_in_suffix() {
        let text = "a\n<!--MAGICIAN_CACHE_BREAKPOINT-->\nb\n<!--MAGICIAN_CACHE_BREAKPOINT-->\nc";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "a");
        assert!(suffix
            .as_deref()
            .unwrap()
            .contains("<!--MAGICIAN_CACHE_BREAKPOINT-->"));
    }

    #[test]
    fn split_handles_multi_byte_char_immediately_before_sentinel() {
        // Regression for a UTF-8 panic: `&text[sentinel_start - 1..sentinel_start]`
        // crashed when the byte before the sentinel was a continuation byte of
        // a multi-byte UTF-8 char. A raw-byte compare on `b'\n'` is
        // char-boundary-safe because `\n` (0x0A) is never a continuation byte
        // in valid UTF-8.
        let text = "é<!--MAGICIAN_CACHE_BREAKPOINT-->\nafter";
        let (prefix, suffix) = split_on_cache_sentinel(text);
        assert_eq!(prefix, "é");
        assert_eq!(suffix.as_deref(), Some("after"));
    }
}

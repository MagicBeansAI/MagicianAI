use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::{
    append_sse_utf8_chunk, default_http_client, finish_sse_utf8, next_sse_data,
    openai_model_supports_reasoning_none, openai_prompt_cache::plan as openai_prompt_cache_plan,
    read_bounded_response_text, SseBodyAdmission,
};
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        append_tool_argument_fragment, clone_admitted_tool_argument, clone_json_value_iteratively,
        parse_provider_json_or_string, parse_provider_json_value,
        parse_tool_argument_json_or_string, ContentBlock, LLMMessage, LLMRequest, LLMResponse,
        LLMResponseFormat, LLMToolCall, LLMToolResult, LLMToolSpec, MessageRole, ReasoningConfig,
        RequestMetadata, StreamDelta, TokenUsage,
    },
};

pub const DEFAULT_BASE_URL_RESPONSES: &str = "https://api.openai.com/v1/responses";
const MAX_TOKENS_RETRY_CAP: u32 = 32_000;
const MAX_TOKENS_RETRY_ATTEMPT_KEY: &str = "max_tokens_retry_attempt";

/// Provider implementation backed by the OpenAI Responses API.
///
/// # Parameter Handling
///
/// ## Verbosity Parameter
/// The OpenAI Responses API requires the `verbosity` parameter to be nested inside a `text` object:
/// ```json
/// {
///   "text": {
///     "verbosity": "low"  // "low", "medium", or "high"
///   }
/// }
/// ```
///
/// This provider automatically extracts `verbosity` from the `extra` parameters and transforms it
/// to the correct nested structure. Configuration can use flat format:
/// ```yaml
/// llm_configs:
///   llm-reasoning-small:
///     provider: openai
///     model: gpt-5.6-terra
///     verbosity: low  # Automatically transformed to text.verbosity
/// ```
///
/// ## Modalities Parameter
/// Unlike the Realtime API, the Responses API does NOT support the `modalities` parameter.
/// Vision and media capabilities are handled through content block types.
/// The provider automatically processes media via content blocks, not through a modalities array.
///
/// ## Content Block Types
/// The Responses API uses specific content block type names:
/// - **Input**: `input_text`, `input_image` (with data URL in `image_url` property)
/// - **Output**: `output_text`, `refusal`, `summary_text`
///
/// Images are sent as `input_image` type with data URLs: `{"type": "input_image", "image_url": "data:{media_type};base64,{data}"}`
/// This provider automatically maps internal content blocks to the correct API types.
pub struct OpenAIResponsesProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
    dialect: ResponsesDialect,
}

/// Which vendor's Responses API this transport speaks. The wire protocol is
/// shared (input items, reasoning items, `function_call`, `previous_response_id`,
/// `usage.input_tokens_details.cached_tokens`); the dialect carries only the
/// measured differences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ResponsesDialect {
    #[default]
    OpenAI,
    /// `api.x.ai/v1/responses`. Measured 2026-09-23: refuses `metadata`
    /// ("Argument not supported") and `reasoning.effort: none` (Grok reasoning
    /// cannot be disabled); accepts `reasoning.summary`, `text.verbosity`,
    /// `tool_choice: required`, `previous_response_id`, `prompt_cache_key`.
    /// Caches per server, so a conversation's requests carry one
    /// `prompt_cache_key`; the GPT-5.6+ explicit-breakpoint controls are
    /// OpenAI-only.
    Xai,
}

/// FALLBACK for models absent from the model catalog; catalogued models
/// declare `wire.sampling` instead. OpenAI reasoning models (gpt-5+, o1*,
/// o3*) reject temperature/top_p.
pub(crate) fn uncatalogued_rejects_sampling(model: &str) -> bool {
    let basename = super::openai_model_basename(model);
    super::openai_gpt_family_from_5(model)
        || basename.starts_with("o1")
        || basename.starts_with("o3")
}

/// How many responses' function-call ids [`remember_response_function_calls`]
/// keeps. A chain only ever continues from a recent response.
const PENDING_CALL_REGISTRY_CAPACITY: usize = 4096;

/// The `function_call` ids each parsed response made, by response id.
///
/// A continuation (`previous_response_id`) must answer EVERY call that
/// response made, or the request is rejected ("No tool output found for
/// function call …"). The caller's transcript does not mark which assistant
/// message that response was: a turn its loop issued without a model call (a
/// Decision Engine step) reads the same. Remembering the calls lets the delta
/// start at the right message and lets the final pairing check answer any
/// call the transcript left without a result. Process-local and bounded: an
/// id it no longer knows (a restart, eviction) keeps the last-assistant
/// anchor.
#[derive(Default)]
struct PendingCallRegistry {
    order: std::collections::VecDeque<String>,
    calls: HashMap<String, Vec<String>>,
}

fn pending_call_registry() -> &'static std::sync::Mutex<PendingCallRegistry> {
    static REGISTRY: std::sync::OnceLock<std::sync::Mutex<PendingCallRegistry>> =
        std::sync::OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Record the `function_call` ids of a parsed Responses payload under its id.
fn remember_response_function_calls(payload: &Value) {
    let Some(response_id) = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return;
    };
    let call_ids: Vec<String> = payload
        .get("output")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter(|item| item.get("type").and_then(Value::as_str) == Some("function_call"))
                .filter_map(|item| {
                    item.get("call_id")
                        .or_else(|| item.get("id"))
                        .and_then(Value::as_str)
                })
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let Ok(mut registry) = pending_call_registry().lock() else {
        return;
    };
    if registry
        .calls
        .insert(response_id.to_string(), call_ids)
        .is_none()
    {
        registry.order.push_back(response_id.to_string());
        while registry.order.len() > PENDING_CALL_REGISTRY_CAPACITY {
            if let Some(evicted) = registry.order.pop_front() {
                registry.calls.remove(&evicted);
            }
        }
    }
}

/// The `function_call` ids a remembered response made, `None` when unknown.
fn remembered_response_function_calls(response_id: &str) -> Option<Vec<String>> {
    let registry = pending_call_registry().lock().ok()?;
    registry.calls.get(response_id.trim()).cloned()
}

/// Whether a Responses stream event is output: a `*.delta` (text, reasoning
/// summary, refusal, tool-call arguments) or a finished item or part. An item
/// merely opened (`output_item.added`, e.g. a reasoning item), a web search
/// still `in_progress` / `searching`, and the lifecycle events are not, so a
/// model reasoning silently stays under the first-output bound instead of
/// starting the idle clock.
fn responses_event_is_output(event_type: &str) -> bool {
    event_type.ends_with(".delta")
        || event_type.ends_with(".done")
        || (event_type.ends_with(".completed") && event_type != "response.completed")
}

impl OpenAIResponsesProvider {
    fn max_tokens_retry_attempt(request: &LLMRequest) -> u64 {
        request
            .extra
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|map| map.get(MAX_TOKENS_RETRY_ATTEMPT_KEY))
            .and_then(Value::as_u64)
            .unwrap_or(0)
    }

    fn set_max_tokens_retry_attempt(request: &mut LLMRequest, attempt: u64) {
        let mut extras = match request.take_extra_value() {
            Some(Value::Object(extras)) => extras,
            Some(_) | None => Map::new(),
        };
        extras.insert(
            MAX_TOKENS_RETRY_ATTEMPT_KEY.to_string(),
            Value::from(attempt),
        );
        request.set_extra(Value::Object(extras));
    }

    fn should_retry_max_tokens(response: &LLMResponse) -> bool {
        response.finish_reason.as_deref() == Some("incomplete")
            && response.stopped_at_output_limit()
    }

    fn is_excessive_max_tokens_error(error: &LLMError) -> bool {
        let lower = error.to_string().to_ascii_lowercase();
        (lower.contains("max_output_tokens")
            || lower.contains("max_completion_tokens")
            || lower.contains("max_tokens"))
            && (lower.contains("too large")
                || lower.contains("too high")
                || lower.contains("exceed")
                || lower.contains("maximum")
                || lower.contains("must be less")
                || lower.contains("invalid"))
    }

    fn retry_max_output_tokens(base_max_output_tokens: u32) -> u32 {
        base_max_output_tokens
            .saturating_mul(2)
            .min(MAX_TOKENS_RETRY_CAP)
    }

    fn retry_base_max_output_tokens(request: &LLMRequest, response: &LLMResponse) -> Option<u32> {
        request.max_output_tokens.or_else(|| {
            response
                .usage
                .as_ref()
                .and_then(|usage| usage.completion_tokens)
                .filter(|tokens| *tokens > 0)
        })
    }

    fn backed_off_max_output_tokens(
        base_max_output_tokens: u32,
        attempted_retry_max_output_tokens: u32,
    ) -> Option<u32> {
        let backed_off = base_max_output_tokens
            .saturating_add(base_max_output_tokens / 2)
            .min(attempted_retry_max_output_tokens.saturating_sub(1))
            .min(MAX_TOKENS_RETRY_CAP);
        if backed_off > base_max_output_tokens && backed_off < attempted_retry_max_output_tokens {
            Some(backed_off)
        } else {
            None
        }
    }

    /// Creates a provider using the default OpenAI Responses endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL_RESPONSES)
    }

    /// Creates a provider with a custom base URL (useful for proxies or testing).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

    /// Constructs a provider with a preconfigured HTTP client.
    pub fn with_client(
        client: Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_timeout: Duration::from_secs(180),
            dialect: ResponsesDialect::OpenAI,
        }
    }

    /// A Responses transport for another vendor's compatible endpoint.
    pub fn with_base_url_for_dialect(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        dialect: ResponsesDialect,
    ) -> Self {
        let mut provider = Self::with_client(default_http_client(), api_key, base_url);
        provider.dialect = dialect;
        provider
    }

    /// xAI's per-server cache is reached by routing one conversation to one
    /// server: a caller-supplied `prompt_cache_key`, else the execution-scoped
    /// `session_key`, else the stable-prefix fingerprint. None when caching is
    /// disabled or the request may not leave provider state behind.
    fn xai_prompt_cache_key(request: &LLMRequest) -> Option<String> {
        if request.metadata.single_physical_attempt
            || matches!(
                request.prompt_cache,
                Some(crate::types::PromptCacheConfig::Disabled)
            )
        {
            return None;
        }
        let explicit = request
            .extra_value()
            .and_then(|extra| extra.get("prompt_cache_key"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty())
            .map(str::to_string);
        explicit.or_else(|| {
            request
                .context_reuse
                .as_ref()
                .and_then(|reuse| {
                    reuse
                        .session_key
                        .clone()
                        .or_else(|| reuse.stable_prefix_fingerprint.clone())
                })
                .map(|key| key.trim().to_string())
                .filter(|key| !key.is_empty())
        })
    }

    /// Grok accepts `effort` low / medium / high / xhigh and nothing that
    /// turns reasoning off. `minimal` (an OpenAI tier) maps to `low`;
    /// `strategy` is OpenAI-shaped and dropped.
    fn xai_reasoning_payload(reasoning: Option<&ReasoningConfig>) -> Option<Value> {
        let reasoning = reasoning?;
        if reasoning.is_disabled() {
            return None;
        }
        let mut payload = Self::map_reasoning(reasoning);
        let object = payload.as_object_mut()?;
        object.remove("strategy");
        if object.get("effort").and_then(Value::as_str) == Some("minimal") {
            object.insert("effort".to_string(), Value::String("low".to_string()));
        }
        Some(payload)
    }

    pub(crate) fn capabilities_for_model(model: &str) -> LLMCapability {
        let mut capability = LLMCapability::default();
        capability.tool_calling = true;
        capability.json_mode = true;
        capability.streaming = true;
        // The Responses transport translates the `server_web_search` request
        // flag into OpenAI's `web_search` server tool for every model.
        capability.web_search = true;

        if model.contains("vision")
            || model.contains("gpt-4o")
            || super::openai_gpt_family_from_5(model)
        {
            if !capability.modalities.contains(&LLMModality::Vision) {
                capability.modalities.push(LLMModality::Vision);
            }
        }

        if model.contains("audio") {
            if !capability.modalities.contains(&LLMModality::Audio) {
                capability.modalities.push(LLMModality::Audio);
            }
        }

        let basename = super::openai_model_basename(model);
        capability.reasoning = if super::openai_gpt_family_from_5(model)
            || basename.starts_with("o1")
            || basename.starts_with("o3")
        {
            LLMReasoning::Advanced
        } else {
            LLMReasoning::None
        };

        capability
    }

    fn previous_response_id(request: &LLMRequest) -> Option<String> {
        if let Some(reuse) = request.context_reuse.as_ref() {
            return reuse
                .strategy
                .is_stateful()
                .then(|| reuse.continuation_id.clone())
                .flatten()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty());
        }
        request
            .extra
            .as_deref()
            .and_then(Value::as_object)
            .and_then(|extra| extra.get("openai_previous_response_id"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
    }

    fn map_message_content_items(
        message: &LLMMessage,
        explicit_cache_breakpoint: bool,
    ) -> LLMResult<Vec<Value>> {
        let mut content_items = Vec::with_capacity(message.content.len());
        for block in &message.content {
            content_items.extend(Self::map_content_block_items_for_role(
                message.role,
                block,
                explicit_cache_breakpoint,
            )?);
        }
        Ok(content_items)
    }

    fn map_tool_result_item(tool_call_id: &str, content: &Value) -> Value {
        json!({
            "type": "function_call_output",
            "call_id": tool_call_id,
            "output": match content {
                Value::String(text) => Value::String(text.clone()),
                other => Value::String(other.to_string()),
            },
        })
    }

    fn map_messages(
        messages: &[LLMMessage],
        previous_response_id: Option<&str>,
        explicit_cache_breakpoint: bool,
    ) -> LLMResult<Vec<Value>> {
        let chain_calls = previous_response_id.and_then(remembered_response_function_calls);
        Self::map_messages_with_chain_calls(
            messages,
            previous_response_id,
            chain_calls.as_deref(),
            explicit_cache_breakpoint,
        )
    }

    /// The assistant message a continuation delta starts after.
    ///
    /// The chain's response is the assistant message carrying one of the
    /// calls it made (`chain_calls`, when remembered). Turns after it that
    /// the caller's loop issued without a model call are not in the chain,
    /// so they go in the delta as `function_call` + output pairs. Without
    /// remembered calls — or when that message is gone from the transcript —
    /// the last assistant message is the anchor.
    fn continuation_anchor(
        messages: &[LLMMessage],
        chain_calls: Option<&[String]>,
    ) -> Option<usize> {
        let last_assistant = messages
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant);
        let Some(chain_calls) = chain_calls.filter(|calls| !calls.is_empty()) else {
            return last_assistant;
        };
        messages
            .iter()
            .rposition(|message| {
                message.role == MessageRole::Assistant
                    && message.content.iter().any(|block| {
                        matches!(block, ContentBlock::ToolCall { id, .. }
                            if chain_calls.iter().any(|call| call == id))
                    })
            })
            .or(last_assistant)
    }

    /// Every `function_call` in the request gets exactly one
    /// `function_call_output` with its `call_id`: a later duplicate output
    /// is dropped, and a call with none is answered with
    /// [`super::MISSING_TOOL_RESULT_TEXT`] at the end of its run of tool
    /// items. `chain_calls` are the calls the continued response made; they
    /// are answered at the start of the input when the delta lacks them.
    /// An output whose call is in neither is left alone — a pre-sliced
    /// delta answers a chain call this adapter does not remember.
    fn pair_function_calls(payload: Vec<Value>, chain_calls: &[String]) -> Vec<Value> {
        use std::collections::HashSet;

        fn item_type(item: &Value) -> Option<&str> {
            item.get("type").and_then(Value::as_str)
        }
        fn call_id(item: &Value) -> Option<&str> {
            item.get("call_id").and_then(Value::as_str)
        }
        fn missing_output(call_id: &str) -> Value {
            json!({
                "type": "function_call_output",
                "call_id": call_id,
                "output": super::MISSING_TOOL_RESULT_TEXT,
            })
        }
        fn flush_open(
            out: &mut Vec<Value>,
            open: &mut Vec<String>,
            answered: &mut HashSet<String>,
            synthesized: &mut usize,
        ) {
            for id in open.drain(..) {
                if answered.insert(id.clone()) {
                    out.push(missing_output(&id));
                    *synthesized += 1;
                }
            }
        }

        // One output per call id: the first one stands.
        let mut answered: HashSet<String> = HashSet::new();
        let mut dropped_duplicates = 0usize;
        let mut kept: Vec<Value> = Vec::with_capacity(payload.len());
        for item in payload {
            if item_type(&item) == Some("function_call_output") {
                if let Some(id) = call_id(&item) {
                    if !answered.insert(id.to_string()) {
                        dropped_duplicates += 1;
                        continue;
                    }
                }
            }
            kept.push(item);
        }

        // A chain call the input itself replays is paired where it stands.
        let replayed_calls: HashSet<&str> = kept
            .iter()
            .filter(|item| item_type(item) == Some("function_call"))
            .filter_map(call_id)
            .collect();
        let chain_pending: Vec<String> = chain_calls
            .iter()
            .filter(|id| !replayed_calls.contains(id.as_str()))
            .cloned()
            .collect();
        drop(replayed_calls);

        let mut synthesized = 0usize;
        let mut out: Vec<Value> = Vec::with_capacity(kept.len() + chain_pending.len());
        for id in &chain_pending {
            if answered.insert(id.clone()) {
                out.push(missing_output(id));
                synthesized += 1;
            }
        }
        let mut open: Vec<String> = Vec::new();
        for item in kept {
            let kind = item_type(&item);
            let in_tool_run = matches!(kind, Some("function_call" | "function_call_output"));
            if !in_tool_run {
                flush_open(&mut out, &mut open, &mut answered, &mut synthesized);
            }
            if kind == Some("function_call") {
                if let Some(id) = call_id(&item) {
                    if !answered.contains(id) && !open.iter().any(|pending| pending == id) {
                        open.push(id.to_string());
                    }
                }
            }
            out.push(item);
        }
        flush_open(&mut out, &mut open, &mut answered, &mut synthesized);

        if synthesized > 0 || dropped_duplicates > 0 {
            warn!(
                provider = "openai_responses",
                synthesized_outputs = synthesized,
                dropped_duplicate_outputs = dropped_duplicates,
                "paired every function_call with exactly one function_call_output before sending"
            );
        }
        out
    }

    fn map_messages_with_chain_calls(
        messages: &[LLMMessage],
        previous_response_id: Option<&str>,
        chain_calls: Option<&[String]>,
        explicit_cache_breakpoint: bool,
    ) -> LLMResult<Vec<Value>> {
        // Continuation mode: when `previous_response_id` is set, the OpenAI
        // Responses API already has the prior conversation server-side. Per
        // the docs (Migrate to Responses guide; Conversation State guide),
        // only NEW input items should be forwarded — re-sending the system
        // prompt + prior user/assistant turns just duplicates input tokens
        // and risks billing/latency regressions.
        //
        // Strategy: keep only the suffix that follows the LAST Assistant
        // message in the local message list. That suffix is exactly the
        // "new input items" the API expects: the tool_result(s) from the
        // dispatch we just ran, plus any fresh user-text injected this turn
        // (e.g. updated runtime ledger). Everything before — system, prior
        // user turns, prior assistant turns — is replayed by the chain.
        //
        // If we cannot find an Assistant message (compaction evicted it,
        // or local state diverged), fall back to standard mode for safety.
        // Scope: this branch only fires when previous_response_id is set,
        // so non-chained Responses calls and other transports are untouched.
        //
        // The anchor is the chain response's own message when its calls are
        // remembered (`continuation_anchor`): assistant turns the caller's
        // loop issued after it without a model call are not in the chain, so
        // the suffix after the LAST assistant would drop the output the chain
        // is waiting for (live 2026-10-20: two Decision Engine steps after a
        // model turn → 400 "No tool output found for function call").
        let effective_messages: Vec<&LLMMessage> = match previous_response_id {
            Some(_) => match Self::continuation_anchor(messages, chain_calls) {
                // Suffix has at least one new input item — preferred path.
                Some(idx) if idx + 1 < messages.len() => messages[(idx + 1)..].iter().collect(),
                // No Assistant at all: the caller already sliced the payload
                // down to the delta (the request builder only keeps the chain
                // id for that shape). Forward the delta without the system
                // prompt — the chain carries it from the bootstrap turn, so
                // re-sending appends another copy of it to the server-side
                // conversation on every single turn.
                None => messages
                    .iter()
                    .filter(|message| message.role != MessageRole::System)
                    .collect(),
                // The Assistant message is itself last, so there is no new
                // input. Keep full mapping as a defensive helper-level
                // fallback; the request builder detects this shape and drops
                // `previous_response_id`, so the list is sent as a clean
                // bootstrap rather than appended to a server chain.
                Some(_) => messages.iter().collect(),
            },
            None => messages.iter().collect(),
        };
        let messages = effective_messages;

        let mut payload = Vec::with_capacity(messages.len());

        for message in messages {
            // Continuation mode (`previous_response_id` set): the upstream
            // model already remembers prior assistant tool_use blocks, so we
            // only forward NEW tool_results plus optional user text.
            if previous_response_id.is_some() && message.role == MessageRole::Tool {
                for block in &message.content {
                    match block {
                        ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                        } => payload.push(Self::map_tool_result_item(tool_call_id, content)),
                        ContentBlock::Text { text } if !text.trim().is_empty() => {
                            let content = Self::map_content_block_items_for_role(
                                MessageRole::User,
                                block,
                                explicit_cache_breakpoint,
                            )?;
                            payload.push(json!({
                                "role": "user",
                                "content": content,
                            }));
                        },
                        ContentBlock::Text { .. } => {},
                        _ => {
                            return Err(LLMError::UnsupportedCapability(
                                "OpenAI Responses continuation only supports ToolResult blocks in tool messages"
                                    .to_string(),
                            ))
                        },
                    }
                }
                continue;
            }

            // Standard mode: rebuild the full conversation as a flat list of
            // input items. Anthropic-style nested tool_use / tool_result
            // content blocks must be HOISTED to top-level Responses items
            // (`function_call`, `function_call_output`); the surrounding
            // assistant/user message becomes a sibling text item if it
            // carries any text content.
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            let has_tool_blocks = message.content.iter().any(|block| {
                matches!(
                    block,
                    ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. }
                )
            });

            if !has_tool_blocks {
                let content_items =
                    Self::map_message_content_items(message, explicit_cache_breakpoint)?;
                payload.push(json!({ "role": role, "content": content_items }));
                continue;
            }

            // Mixed message: stream items in order so the model sees the
            // assistant's narrative before its tool_use, and tool_results
            // arrive paired with the prior call_ids.
            let mut pending_text_items: Vec<Value> = Vec::new();
            let flush_text = |payload: &mut Vec<Value>, pending: &mut Vec<Value>, role: &str| {
                if pending.is_empty() {
                    return;
                }
                payload.push(json!({
                    "role": role,
                    "content": std::mem::take(pending),
                }));
            };

            for block in &message.content {
                match block {
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        flush_text(&mut payload, &mut pending_text_items, role);
                        let arguments_str = match arguments {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        payload.push(json!({
                            "type": "function_call",
                            "call_id": id,
                            "name": name,
                            "arguments": arguments_str,
                        }));
                    },
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                    } => {
                        flush_text(&mut payload, &mut pending_text_items, role);
                        payload.push(Self::map_tool_result_item(tool_call_id, content));
                    },
                    other => pending_text_items.extend(Self::map_content_block_items_for_role(
                        message.role,
                        other,
                        explicit_cache_breakpoint,
                    )?),
                }
            }
            flush_text(&mut payload, &mut pending_text_items, role);
        }

        // Final invariant, whatever shape the caller sent: no call leaves
        // unanswered. A chain's own calls only count in continuation mode.
        let chain_calls = match previous_response_id {
            Some(_) => chain_calls.unwrap_or_default(),
            None => &[],
        };
        Ok(Self::pair_function_calls(payload, chain_calls))
    }

    /// Whether the local transcript has an unambiguous delta after the
    /// response represented by `previous_response_id`.
    ///
    /// A chain id and a full transcript must never be sent together as a
    /// defensive fallback: that asks the provider to replay the server-side
    /// prefix and then appends the same local prefix again. When compaction or
    /// resume hydration leaves no assistant anchor (or no suffix after it), the
    /// caller drops the chain id and performs one clean full bootstrap.
    ///
    /// There is a third shape, and treating it as unsafe is worse than the
    /// duplication this guard exists to prevent: a caller that has ALREADY
    /// sliced to the delta sends tool results with no assistant anchor at all.
    /// Dropping the chain there produces a `function_call_output` whose
    /// `function_call` exists in neither the payload nor a server-side chain,
    /// and the request is rejected outright — "No tool call found for function
    /// call output with call_id …". There is also nothing to double-replay,
    /// because a pre-sliced tool-result payload carries no replayable prefix. A
    /// user message after that result is still part of the delta: callers use
    /// it for recovery instructions and changed runtime state. Its presence
    /// must not orphan the preceding native result. So keep the chain whenever
    /// the anchorless payload contains an actual native tool result.
    fn has_safe_continuation_delta(messages: &[LLMMessage]) -> bool {
        if let Some(index) = messages
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant)
        {
            return index + 1 < messages.len();
        }
        // No anchor. The hazard this guard exists to prevent is duplication:
        // a chain id sent alongside a payload that repeats conversation the
        // chain already holds. That requires the payload to carry a
        // replayable conversational prefix, so ask whether one is present.
        //
        // An actual ToolResult proves a pre-sliced delta: a self-contained
        // bootstrap would necessarily include the matching ToolCall too.
        // Recovery/user state may legitimately follow the tool result, as it
        // does for Tutor completion guards.
        let carries_native_tool_result = messages.iter().any(|message| {
            message.role == MessageRole::Tool
                && message
                    .content
                    .iter()
                    .any(|block| matches!(block, ContentBlock::ToolResult { .. }))
        });
        if carries_native_tool_result {
            return true;
        }
        // Otherwise a single non-system message IS the delta: the payload
        // holds no prior turn that the chain could replay a second time. The
        // system prompt does not count, because continuation mode never
        // forwards it -- the chain carries it from the bootstrap turn. Two or
        // more non-system messages with no anchor is a real local transcript,
        // which must still take the clean bootstrap path.
        messages
            .iter()
            .filter(|message| message.role != MessageRole::System)
            .count()
            == 1
    }

    /// Strip the Magician cache-breakpoint sentinel from a text payload,
    /// reconnecting the halves with a single `\n` when both are non-empty.
    ///
    /// OpenAI Responses uses automatic upstream prefix-caching, so there is no
    /// breakpoint to emit — but if the rendered user prompt carries our
    /// internal marker we must strip it before sending. Matches the
    /// reconnection rule used by the Anthropic and OpenRouter providers.
    fn strip_cache_sentinel(text: &str) -> String {
        if !text.contains(crate::types::CACHE_BREAKPOINT_SENTINEL) {
            return crate::types::strip_cache_segments(text);
        }
        let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
        match suffix {
            Some(s) if prefix.is_empty() => s,
            Some(s) if s.is_empty() => prefix,
            Some(s) => format!("{prefix}\n{s}"),
            None => prefix,
        }
    }

    /// Map one normalized block to one or more Responses content items. On
    /// GPT-5.6+ requests the stable half of Magician's sentinel-bearing input
    /// text receives OpenAI's native explicit cache breakpoint; older models
    /// and disabled/markerless requests continue through the single-item path.
    fn map_content_block_items_for_role(
        role: MessageRole,
        block: &ContentBlock,
        explicit_cache_breakpoint: bool,
    ) -> LLMResult<Vec<Value>> {
        if explicit_cache_breakpoint && matches!(role, MessageRole::System | MessageRole::User) {
            if let ContentBlock::Text { text } = block {
                let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
                if let Some(suffix) = suffix {
                    if !prefix.is_empty() && !suffix.is_empty() {
                        // Segments after the stable prefix (finished chunks
                        // of a re-rendered conversation) get their own items;
                        // the newest finished ones also get breakpoints, since
                        // the cache reads only at breakpoint positions.
                        let segments = crate::types::split_cache_segments(text)
                            .map(|(_, segments)| segments)
                            .unwrap_or_default();
                        let mut items = vec![json!({
                            "type": "input_text",
                            "text": prefix,
                            "prompt_cache_breakpoint": { "mode": "explicit" },
                        })];
                        for (segment, breakpoint) in segments {
                            let mut item = json!({
                                "type": "input_text",
                                "text": Self::strip_cache_sentinel(&segment),
                            });
                            if breakpoint {
                                item["prompt_cache_breakpoint"] = json!({ "mode": "explicit" });
                            }
                            items.push(item);
                        }
                        return Ok(items);
                    }
                    // A marker that ends the block (the decision loop's stable
                    // prompt, sent as its own message before the conversation)
                    // marks the whole block: later messages still follow it.
                    if !prefix.is_empty() {
                        return Ok(vec![json!({
                            "type": "input_text",
                            "text": prefix,
                            "prompt_cache_breakpoint": { "mode": "explicit" },
                        })]);
                    }
                }
            }
        }

        Ok(vec![Self::map_content_block_for_role(role, block)?])
    }

    fn map_content_block_for_role(role: MessageRole, block: &ContentBlock) -> LLMResult<Value> {
        match role {
            MessageRole::Assistant => match block {
                ContentBlock::Text { text } => Ok(json!({
                    "type": "output_text",
                    "text": Self::strip_cache_sentinel(text),
                })),
                ContentBlock::Json { value } => Ok(json!({
                    "type": "output_text",
                    "text": value.to_string(),
                })),
                ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. } => Err(
                    LLMError::UnsupportedCapability(
                        "OpenAI Responses provider does not support replaying assistant image content from normalized transcript history"
                            .to_string(),
                    ),
                ),
                // ToolCall / ToolResult blocks must be hoisted to top-level
                // `function_call` / `function_call_output` items by
                // `map_messages` before this helper is called. Reaching here
                // means the caller bypassed the hoisting path.
                // ToolCall / ToolResult blocks must be hoisted to top-level
                // `function_call` / `function_call_output` items by
                // `map_messages` before this helper is called. Reaching here
                // means the caller bypassed the hoisting path.
                ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. } => Err(
                    LLMError::Other(
                        "OpenAI Responses: tool transcript blocks must be hoisted by map_messages"
                            .to_string(),
                    ),
                ),
            },
            MessageRole::System | MessageRole::User | MessageRole::Tool => match block {
                ContentBlock::Text { text } => Ok(json!({
                    "type": "input_text",
                    "text": Self::strip_cache_sentinel(text),
                })),
                ContentBlock::Json { value } => Ok(json!({
                    "type": "input_text",
                    "text": value.to_string(),
                })),
                ContentBlock::Image {
                    data,
                    media_type,
                    ..
                } => {
                    // OpenAI Responses API uses 'input_image' type with 'image_url' property containing data URL
                    let encoded = BASE64.encode(data);
                    let data_url = format!("data:{};base64,{}", media_type, encoded);
                    Ok(json!({
                        "type": "input_image",
                        "image_url": data_url,
                    }))
                }
                ContentBlock::ImageUrl { url, .. } => Ok(json!({
                    "type": "input_image",
                    "image_url": url,
                })),
                // ToolCall / ToolResult blocks must be hoisted to top-level
                // `function_call` / `function_call_output` items by
                // `map_messages` before this helper is called. Reaching here
                // means the caller bypassed the hoisting path.
                // ToolCall / ToolResult blocks must be hoisted to top-level
                // `function_call` / `function_call_output` items by
                // `map_messages` before this helper is called. Reaching here
                // means the caller bypassed the hoisting path.
                ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. } => Err(
                    LLMError::Other(
                        "OpenAI Responses: tool transcript blocks must be hoisted by map_messages"
                            .to_string(),
                    ),
                ),
            },
        }
    }

    fn map_tools(tools: &[LLMToolSpec]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                let (description, parameters) =
                    crate::tool_schema::project_whole_call_constraints(tool);
                // The Responses API (/v1/responses) uses a flat tool object with
                // top-level name/description/parameters — NOT the nested
                // {"type":"function","function":{...}} format used by Chat Completions.
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": description,
                    "parameters": parameters,
                })
            })
            .collect()
    }

    // Note: Currently unused - modalities parameter not supported in Responses API.
    // Kept for potential future compatibility. Vision/audio handled via content blocks.
    #[allow(dead_code)]
    fn map_modalities(modality: LLMModality) -> Vec<Value> {
        match modality {
            LLMModality::Text => vec![Value::String("text".to_string())],
            LLMModality::Vision => vec![
                Value::String("text".to_string()),
                Value::String("vision".to_string()),
            ],
            LLMModality::Audio => vec![
                Value::String("text".to_string()),
                Value::String("audio".to_string()),
            ],
        }
    }

    fn map_reasoning(reasoning: &ReasoningConfig) -> Value {
        let mut object = Map::new();
        let mut effort_is_none = false;
        if let Some(effort) = reasoning.effort.as_ref() {
            effort_is_none = ReasoningConfig::effort_disables_reasoning(effort);
            let effort = if effort_is_none {
                "none".to_string()
            } else {
                effort.clone()
            };
            object.insert("effort".to_string(), Value::String(effort));
        }
        if effort_is_none {
            return Value::Object(object);
        }
        // `reasoning.max_tokens` is NOT a valid field on OpenAI's
        // Responses API — the canonical OpenAPI schema only accepts
        // `effort`, `summary`, and the deprecated `generate_summary`.
        // Sending it produces a 400 `Unknown parameter:
        // 'reasoning.max_tokens'` rejection at request validation.
        //
        // The top-level `max_output_tokens` is the canonical way to
        // cap total tokens (visible output + reasoning combined), and
        // is already plumbed via `LLMRequest::max_tokens`. Reasoning
        // intensity is controlled qualitatively via `effort` (low /
        // medium / high / xhigh). `max_reasoning_tokens` on the
        // `ReasoningConfig` struct is intentionally kept for YAML
        // compatibility but is no longer forwarded to the API.
        // `strategy` selects an internal adapter policy (for example the
        // catalog's `native_effort`); it is not a Responses API parameter.
        // Keep it out of sync, streaming and fallback requests alike.
        // Ask for a reasoning summary when reasoning is enabled. Without this, the
        // Responses API bills `reasoning_tokens` but returns no
        // `output[].type == "reasoning"` summary text — so
        // `parse_output` had nothing to extract, `response.reasoning_text`
        // was empty, and the chat post-call fallback emitted no
        // `reasoning.start/content/end` events. The UI consequently
        // never showed a "Thought" row for reasoning-enabled models.
        // Honor an explicit `reasoning.summary` from config (auto /
        // concise / detailed); default to "auto" when reasoning is
        // enabled and summary is unset — cheaper than hard-coding
        // "detailed", richer than omitting.
        if !effort_is_none {
            let summary = reasoning
                .summary
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| "auto".to_string());
            object.insert("summary".to_string(), Value::String(summary));
        }
        Value::Object(object)
    }

    fn reasoning_payload_for_request(
        model: &str,
        reasoning: Option<&ReasoningConfig>,
    ) -> Option<Value> {
        if let Some(reasoning) = reasoning {
            if reasoning.is_disabled() && !openai_model_supports_reasoning_none(model) {
                return None;
            }
            return Some(Self::map_reasoning(reasoning));
        }
        if openai_model_supports_reasoning_none(model) {
            Some(json!({ "effort": "none" }))
        } else {
            None
        }
    }

    fn map_metadata(metadata: &RequestMetadata) -> Option<Value> {
        let mut object = Map::new();
        if !metadata.operation.is_empty() {
            object.insert(
                "operation".to_string(),
                Value::String(metadata.operation.clone()),
            );
        }
        if let Some(trace_id) = metadata.trace_id.as_ref() {
            object.insert("trace_id".to_string(), Value::String(trace_id.clone()));
        }
        if let Some(tags) = metadata.tags.as_ref() {
            object.insert("tags".to_string(), Value::String(tags.join(",")));
        }

        if object.is_empty() {
            None
        } else {
            Some(Value::Object(object))
        }
    }

    fn usage_u32(data: &Value, key: &'static str) -> LLMResult<Option<u32>> {
        super::bounded_usage_counter(
            "openai_responses",
            key,
            data.get(key).and_then(Value::as_u64),
        )
    }

    fn usage_nested_u32(
        data: &Value,
        details_key: &str,
        key: &'static str,
    ) -> LLMResult<Option<u32>> {
        super::bounded_usage_counter(
            "openai_responses",
            key,
            data.get(details_key)
                .and_then(|details| details.get(key))
                .and_then(Value::as_u64),
        )
    }

    fn map_usage(data: &Value) -> LLMResult<TokenUsage> {
        let prompt_tokens = match Self::usage_u32(data, "prompt_tokens")? {
            Some(tokens) => Some(tokens),
            None => Self::usage_u32(data, "input_tokens")?,
        };
        let completion_tokens = match Self::usage_u32(data, "completion_tokens")? {
            Some(tokens) => Some(tokens),
            None => Self::usage_u32(data, "output_tokens")?,
        };
        let mut cached_tokens =
            Self::usage_nested_u32(data, "prompt_tokens_details", "cached_tokens")?;
        if cached_tokens.is_none() {
            cached_tokens = Self::usage_nested_u32(data, "input_tokens_details", "cached_tokens")?;
        }
        if cached_tokens.is_none() {
            cached_tokens = Self::usage_u32(data, "cached_tokens")?;
        }
        let mut reasoning_tokens = Self::usage_u32(data, "reasoning_tokens")?;
        if reasoning_tokens.is_none() {
            reasoning_tokens =
                Self::usage_nested_u32(data, "completion_tokens_details", "reasoning_tokens")?;
        }
        if reasoning_tokens.is_none() {
            reasoning_tokens =
                Self::usage_nested_u32(data, "output_tokens_details", "reasoning_tokens")?;
        }
        let mut cache_creation_tokens =
            Self::usage_nested_u32(data, "prompt_tokens_details", "cache_write_tokens")?;
        if cache_creation_tokens.is_none() {
            cache_creation_tokens =
                Self::usage_nested_u32(data, "input_tokens_details", "cache_write_tokens")?;
        }
        if cache_creation_tokens.is_none() {
            cache_creation_tokens = Self::usage_u32(data, "cache_write_tokens")?;
        }
        // Anthropic-style proxies and older fixtures used the normalized
        // `cache_creation_tokens` spelling. Preserve that compatibility after
        // preferring OpenAI's canonical 5.6 field.
        if cache_creation_tokens.is_none() {
            cache_creation_tokens =
                Self::usage_nested_u32(data, "prompt_tokens_details", "cache_creation_tokens")?;
        }
        if cache_creation_tokens.is_none() {
            cache_creation_tokens =
                Self::usage_nested_u32(data, "input_tokens_details", "cache_creation_tokens")?;
        }
        if cache_creation_tokens.is_none() {
            cache_creation_tokens = Self::usage_u32(data, "cache_creation_tokens")?;
        }
        let total_tokens = match Self::usage_u32(data, "total_tokens")? {
            Some(total) => Some(total),
            None => prompt_tokens
                .zip(completion_tokens)
                .map(|(input, output)| {
                    super::checked_usage_sum("openai_responses", "total_tokens", &[input, output])
                })
                .transpose()?,
        };

        Ok(TokenUsage {
            prompt_tokens,
            completion_tokens,
            total_tokens,
            reasoning_tokens,
            cached_tokens,
            cache_creation_tokens,
        })
    }

    fn parse_output(
        payload: &Value,
    ) -> LLMResult<(
        Vec<LLMMessage>,
        Vec<LLMToolCall>,
        Vec<LLMToolResult>,
        Option<String>,
        Option<String>,
    )> {
        let mut messages = Vec::new();
        let mut tool_calls = Vec::new();
        let tool_results = Vec::new(); // Responses API returns tool calls; tool results originate from caller.
        let mut aggregated_text = Vec::new();
        // OpenAI Responses surfaces chain-of-thought as `output[]` items
        // with `type: reasoning` and a `summary[]` array of `{type:
        // summary_text, text}` objects. Pre-fix our parser silently
        // skipped these — `reasoning_tokens` was billed in usage but the
        // actual reasoning text never reached the trace. Capture the
        // concatenated `summary[].text` so the trace `assistant_turn`
        // event can render it next to `text`.
        let mut aggregated_reasoning = Vec::new();

        if let Some(output) = payload.get("output").and_then(Value::as_array) {
            for item in output {
                let item_type = item.get("type").and_then(Value::as_str).unwrap_or("");

                // The Responses API surfaces function calls as top-level output items
                // with type "function_call" and flat name/arguments fields — not nested
                // inside a "message" content block like the Chat Completions API.
                if item_type == "function_call" {
                    if let Some(call) = Self::parse_responses_function_call(item)? {
                        tool_calls.push(call);
                    }
                    continue;
                }

                if item_type == "reasoning" {
                    if let Some(summary) = item.get("summary").and_then(Value::as_array) {
                        for entry in summary {
                            if let Some(text) = entry.get("text").and_then(Value::as_str) {
                                aggregated_reasoning.push(text.to_string());
                            }
                        }
                    }
                    continue;
                }

                if item_type != "message" {
                    continue;
                }

                let role = match item.get("role").and_then(Value::as_str) {
                    Some("assistant") => MessageRole::Assistant,
                    Some("tool") => MessageRole::Tool,
                    Some("user") => MessageRole::User,
                    _ => MessageRole::Assistant,
                };

                let mut content_blocks = Vec::new();
                if let Some(content_items) = item.get("content").and_then(Value::as_array) {
                    for content in content_items {
                        match content.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                if let Some(text) = content.get("text").and_then(Value::as_str) {
                                    aggregated_text.push(text.to_string());
                                    content_blocks.push(ContentBlock::Text {
                                        text: text.to_string(),
                                    });
                                }
                            },
                            Some("tool_call") => {
                                if let Some(tool_call) = content.get("tool_call") {
                                    if let Some(call) = Self::parse_tool_call(tool_call)? {
                                        content_blocks.push(ContentBlock::ToolCall {
                                            id: call.id.clone(),
                                            name: call.name.clone(),
                                            arguments: clone_json_value_iteratively(
                                                &call.arguments,
                                            ),
                                        });
                                        tool_calls.push(call);
                                    }
                                }
                            },
                            Some("output_image") => {
                                if let Some(image) =
                                    content.get("image_base64").and_then(Value::as_str)
                                {
                                    if let Ok(bytes) = BASE64.decode(image) {
                                        content_blocks.push(ContentBlock::Image {
                                            data: bytes,
                                            media_type: content
                                                .get("media_type")
                                                .and_then(Value::as_str)
                                                .unwrap_or("image/png")
                                                .to_string(),
                                            caption: None,
                                        });
                                    }
                                } else if let Some(url) =
                                    content.get("image_url").and_then(Value::as_str)
                                {
                                    content_blocks.push(ContentBlock::ImageUrl {
                                        url: url.to_string(),
                                        prompt: None,
                                    });
                                }
                            },
                            _ => {},
                        }
                    }
                }

                messages.push(LLMMessage {
                    role,
                    content: content_blocks,
                });
            }
        }

        let reasoning_text = if !aggregated_reasoning.is_empty() {
            Some(aggregated_reasoning.join("\n"))
        } else {
            None
        };
        let text = if !aggregated_text.is_empty() {
            Some(aggregated_text.join("\n"))
        } else {
            payload
                .get("output_text")
                .and_then(Value::as_str)
                .map(|s| s.to_string())
        };

        Ok((messages, tool_calls, tool_results, text, reasoning_text))
    }

    fn parse_tool_call(value: &Value) -> LLMResult<Option<LLMToolCall>> {
        let Some(id) = value.get("id").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(function) = value.get("function") else {
            return Ok(None);
        };
        let Some(name) = function.get("name").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(arguments_raw) = function.get("arguments") else {
            return Ok(None);
        };

        let arguments = if let Some(args_str) = arguments_raw.as_str() {
            parse_tool_argument_json_or_string(args_str)?
        } else {
            clone_admitted_tool_argument(arguments_raw)?
        };

        Ok(Some(LLMToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        }))
    }

    /// Parse a top-level `function_call` output item from the Responses API.
    ///
    /// The Responses API returns function calls with `name` and `arguments` at the
    /// top level of the item (flat format), unlike Chat Completions where they are
    /// nested inside a `function` object.
    fn parse_responses_function_call(value: &Value) -> LLMResult<Option<LLMToolCall>> {
        // Prefer `call_id` over `id` when both are present (Responses API convention).
        let Some(id) = value
            .get("call_id")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
        else {
            return Ok(None);
        };
        let Some(name) = value.get("name").and_then(Value::as_str) else {
            return Ok(None);
        };
        let Some(arguments_raw) = value.get("arguments") else {
            return Ok(None);
        };

        let arguments = if let Some(args_str) = arguments_raw.as_str() {
            parse_tool_argument_json_or_string(args_str)?
        } else {
            clone_admitted_tool_argument(arguments_raw)?
        };

        Ok(Some(LLMToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        }))
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn finished_conversation_segments_become_breakpointed_items() {
        let seg = crate::types::CACHE_SEGMENT_SENTINEL;
        let text = format!(
            "stable\n{}\nturns 0-5\n{seg}\nturns 6-11\n{seg}\nturn 12 and state",
            crate::types::CACHE_BREAKPOINT_SENTINEL
        );
        let items = OpenAIResponsesProvider::map_content_block_items_for_role(
            MessageRole::User,
            &ContentBlock::Text { text },
            true,
        )
        .expect("maps");
        let shape: Vec<(String, bool)> = items
            .iter()
            .map(|item| {
                (
                    item["text"].as_str().unwrap().to_string(),
                    item.get("prompt_cache_breakpoint").is_some(),
                )
            })
            .collect();
        assert_eq!(
            shape,
            vec![
                ("stable".to_string(), true),
                ("turns 0-5".to_string(), true),
                ("\nturns 6-11".to_string(), true),
                ("\nturn 12 and state".to_string(), false),
            ]
        );
        assert!(items
            .iter()
            .all(|item| !item["text"].as_str().unwrap().contains("MAGICIAN_CACHE")));
    }

    #[test]
    fn a_marker_ending_a_block_marks_the_whole_block() {
        let text = format!("stable prompt\n{}", crate::types::CACHE_BREAKPOINT_SENTINEL);
        let items = OpenAIResponsesProvider::map_content_block_items_for_role(
            MessageRole::User,
            &ContentBlock::Text { text },
            true,
        )
        .expect("maps");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0]["text"], "stable prompt");
        assert_eq!(items[0]["prompt_cache_breakpoint"]["mode"], "explicit");
    }
    use serde_json::json;

    use super::*;

    #[test]
    fn gpt_6_1_terra_keeps_off_and_max_efforts_with_function_tools() {
        let provider = OpenAIResponsesProvider::new("test-key");
        for effort in ["none", "max"] {
            let request = LLMRequest {
                model: "gpt-6.1-terra".into(),
                messages: vec![LLMMessage::user("inspect the document")].into(),
                reasoning: Some(ReasoningConfig {
                    effort: Some(effort.into()),
                    ..Default::default()
                }),
                tools: vec![LLMToolSpec {
                    name: "read_file".into(),
                    description: "read".into(),
                    parameters: json!({"type": "object"}),
                }]
                .into(),
                ..Default::default()
            };
            let (body, _) = provider
                .build_responses_request_body(&request, &request.model)
                .unwrap();
            assert_eq!(body["reasoning"]["effort"], effort);
            assert_eq!(body["tools"][0]["type"], "function");
            assert_eq!(body["tools"][0]["name"], "read_file");
            assert!(body.get("temperature").is_none());
            assert!(body.get("top_p").is_none());
        }
    }

    #[test]
    fn gpt_6_1_sol_responses_preserves_low_reasoning_and_tool_support() {
        let caps = OpenAIResponsesProvider::capabilities_for_model("gpt-6.1-sol");
        assert!(caps.modalities.contains(&LLMModality::Vision));
        assert_eq!(caps.reasoning, LLMReasoning::Advanced);
        assert!(
            OpenAIResponsesProvider::reasoning_payload_for_request("gpt-6.1-sol", None).is_none()
        );
        assert!(OpenAIResponsesProvider::reasoning_payload_for_request(
            "gpt-6.1-sol",
            Some(&ReasoningConfig {
                effort: Some("none".into()),
                ..Default::default()
            }),
        )
        .is_none());
        let payload = OpenAIResponsesProvider::reasoning_payload_for_request(
            "gpt-6.1-sol",
            Some(&ReasoningConfig {
                effort: Some("low".into()),
                ..Default::default()
            }),
        )
        .expect("explicit low effort");
        assert_eq!(payload["effort"], "low");
    }

    #[test]
    fn gpt_6_astra_advertises_vision_and_advanced_reasoning() {
        let caps = OpenAIResponsesProvider::capabilities_for_model("gpt-6-astra");
        assert!(caps.modalities.contains(&LLMModality::Vision));
        assert_eq!(caps.reasoning, LLMReasoning::Advanced);
        let routed = OpenAIResponsesProvider::capabilities_for_model("openai/gpt-6-astra");
        assert!(routed.modalities.contains(&LLMModality::Vision));
        assert_eq!(routed.reasoning, LLMReasoning::Advanced);
    }

    #[test]
    fn server_web_search_tool_replaces_function_tools_in_body() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![LLMMessage::user("what changed today?")].into(),
            extra: Some(json!({"server_web_search": true}).into()),
            ..Default::default()
        };
        let (body, _timeout) = provider
            .build_responses_request_body(&request, "gpt-5.6-luna")
            .expect("body builds");
        assert_eq!(body["tools"][0]["type"], "web_search");
        // tool_choice "required" would force a dispatch that cannot happen
        // on a server-tool-only request.
        assert!(body.get("tool_choice").is_none());
    }

    /// The canonical Anthropic-shaped `{"type": "none"}` keeps the function
    /// tools advertised but forbids their use. It used to fall through to
    /// `required` — the opposite.
    #[test]
    fn tool_choice_none_object_maps_to_none() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![LLMMessage::user("wrap up")].into(),
            tools: vec![LLMToolSpec {
                name: "read_file".to_string(),
                description: "read".to_string(),
                parameters: json!({}),
            }]
            .into(),
            extra: Some(json!({"tool_choice": {"type": "none"}}).into()),
            ..Default::default()
        };
        let (body, _timeout) = provider
            .build_responses_request_body(&request, "gpt-5.6-luna")
            .expect("body builds");
        assert_eq!(body["tool_choice"], json!("none"));
        assert_eq!(body["tools"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn server_web_search_never_mixes_with_function_tools() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![LLMMessage::user("hi")].into(),
            tools: vec![LLMToolSpec {
                name: "read_file".to_string(),
                description: "read".to_string(),
                parameters: json!({}),
            }]
            .into(),
            extra: Some(json!({"server_web_search": {"max_uses": 2}}).into()),
            ..Default::default()
        };
        let error = provider
            .build_responses_request_body(&request, "gpt-5.6-luna")
            .expect_err("mixed request must fail closed");
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn mistyped_server_web_search_flag_fails_closed_at_body_build() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![LLMMessage::user("hi")].into(),
            extra: Some(json!({"server_web_search": "true"}).into()),
            ..Default::default()
        };
        let error = provider
            .build_responses_request_body(&request, "gpt-5.6-luna")
            .expect_err("a string flag must not degrade to off");
        assert!(error.to_string().contains("unsupported JSON shape"));
    }

    #[test]
    fn unflagged_requests_keep_function_tools_and_choice() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![LLMMessage::user("hi")].into(),
            tools: vec![LLMToolSpec {
                name: "read_file".to_string(),
                description: "read".to_string(),
                parameters: json!({}),
            }]
            .into(),
            ..Default::default()
        };
        let (body, _timeout) = provider
            .build_responses_request_body(&request, "gpt-5.6-luna")
            .expect("body builds");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tool_choice"], "required");
    }

    fn tool_call_block(id: &str, name: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: json!({}),
        }
    }

    fn tool_result_block(id: &str, output: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_call_id: id.to_string(),
            content: Value::String(output.to_string()),
        }
    }

    fn text_block(text: &str) -> ContentBlock {
        ContentBlock::Text {
            text: text.to_string(),
        }
    }

    fn message(role: MessageRole, content: Vec<ContentBlock>) -> LLMMessage {
        LLMMessage { role, content }
    }

    /// `(type, call_id)` per item, `("message:<role>", "")` for messages.
    fn item_shape(items: &[Value]) -> Vec<(String, String)> {
        items
            .iter()
            .map(|item| match item.get("type").and_then(Value::as_str) {
                Some(kind) => (
                    kind.to_string(),
                    item["call_id"].as_str().unwrap_or_default().to_string(),
                ),
                None => (
                    format!("message:{}", item["role"].as_str().unwrap_or_default()),
                    String::new(),
                ),
            })
            .collect()
    }

    fn outputs_for<'a>(items: &'a [Value], call_id: &str) -> Vec<&'a Value> {
        items
            .iter()
            .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
            .collect()
    }

    #[test]
    fn a_parallel_call_without_a_result_is_answered_in_the_same_request() {
        let messages = vec![
            message(MessageRole::User, vec![text_block("do two things")]),
            message(
                MessageRole::Assistant,
                vec![
                    tool_call_block("call_a", "apply_code_proposal"),
                    tool_call_block("call_b", "run_project_checks"),
                ],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block("call_a", "applied"), text_block("state")],
            ),
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, None, false).expect("maps");

        assert_eq!(
            item_shape(&mapped),
            vec![
                ("message:user".to_string(), String::new()),
                ("function_call".to_string(), "call_a".to_string()),
                ("function_call".to_string(), "call_b".to_string()),
                ("function_call_output".to_string(), "call_a".to_string()),
                ("function_call_output".to_string(), "call_b".to_string()),
                ("message:user".to_string(), String::new()),
            ]
        );
        assert_eq!(outputs_for(&mapped, "call_a")[0]["output"], "applied");
        let missing = outputs_for(&mapped, "call_b");
        assert_eq!(missing.len(), 1);
        assert_eq!(
            missing[0]["output"],
            crate::providers::MISSING_TOOL_RESULT_TEXT
        );
    }

    #[test]
    fn a_refused_call_with_no_result_is_answered_and_a_duplicate_output_dropped() {
        let messages = vec![
            message(MessageRole::User, vec![text_block("go")]),
            message(
                MessageRole::Assistant,
                vec![tool_call_block("call_refused", "delete_everything")],
            ),
            // The loop refused the call and only said so in text.
            message(MessageRole::User, vec![text_block("that call was refused")]),
            message(
                MessageRole::Assistant,
                vec![tool_call_block("call_ok", "read")],
            ),
            message(
                MessageRole::User,
                vec![
                    tool_result_block("call_ok", "first"),
                    tool_result_block("call_ok", "second"),
                ],
            ),
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, None, false).expect("maps");

        assert_eq!(
            item_shape(&mapped),
            vec![
                ("message:user".to_string(), String::new()),
                ("function_call".to_string(), "call_refused".to_string()),
                (
                    "function_call_output".to_string(),
                    "call_refused".to_string()
                ),
                ("message:user".to_string(), String::new()),
                ("function_call".to_string(), "call_ok".to_string()),
                ("function_call_output".to_string(), "call_ok".to_string()),
            ]
        );
        assert_eq!(
            outputs_for(&mapped, "call_refused")[0]["output"],
            crate::providers::MISSING_TOOL_RESULT_TEXT
        );
        assert_eq!(outputs_for(&mapped, "call_ok")[0]["output"], "first");
    }

    #[test]
    fn a_fully_paired_history_maps_exactly_as_before() {
        let messages = vec![
            LLMMessage::system("sys"),
            message(MessageRole::User, vec![text_block("go")]),
            message(
                MessageRole::Assistant,
                vec![text_block("calling"), tool_call_block("call_1", "read")],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block("call_1", "ok"), text_block("next")],
            ),
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, None, false).expect("maps");

        assert_eq!(
            Value::Array(mapped),
            json!([
                {"role": "system", "content": [{"type": "input_text", "text": "sys"}]},
                {"role": "user", "content": [{"type": "input_text", "text": "go"}]},
                {"role": "assistant", "content": [{"type": "output_text", "text": "calling"}]},
                {"type": "function_call", "call_id": "call_1", "name": "read", "arguments": "{}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "ok"},
                {"role": "user", "content": [{"type": "input_text", "text": "next"}]},
            ])
        );
    }

    /// Live 2026-10-20: a model turn, then two Decision Engine steps the loop
    /// issued without a model call, then a continuation from the model
    /// turn's response. The suffix after the LAST assistant message dropped
    /// `call_model`'s output → 400 "No tool output found for function call".
    #[test]
    fn a_continuation_after_loop_issued_turns_starts_at_the_chain_response() {
        let messages = vec![
            LLMMessage::system("sys"),
            message(
                MessageRole::Assistant,
                vec![tool_call_block("call_model", "run_coding_task")],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block("call_model", "proposal")],
            ),
            message(
                MessageRole::Assistant,
                vec![
                    text_block("Decision Engine: apply"),
                    tool_call_block("decision:2:apply_code_proposal", "apply_code_proposal"),
                ],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block(
                    "decision:2:apply_code_proposal",
                    "applied",
                )],
            ),
            message(
                MessageRole::Assistant,
                vec![tool_call_block(
                    "decision:3:run_project_checks",
                    "run_project_checks",
                )],
            ),
            message(
                MessageRole::User,
                vec![
                    tool_result_block("decision:3:run_project_checks", "passed"),
                    text_block("## CONTINUATION DELTA"),
                ],
            ),
        ];
        let chain_calls = vec!["call_model".to_string()];

        let mapped = OpenAIResponsesProvider::map_messages_with_chain_calls(
            &messages,
            Some("resp_model_turn"),
            Some(chain_calls.as_slice()),
            false,
        )
        .expect("maps");

        assert_eq!(
            item_shape(&mapped),
            vec![
                ("function_call_output".to_string(), "call_model".to_string()),
                ("message:assistant".to_string(), String::new()),
                (
                    "function_call".to_string(),
                    "decision:2:apply_code_proposal".to_string()
                ),
                (
                    "function_call_output".to_string(),
                    "decision:2:apply_code_proposal".to_string()
                ),
                (
                    "function_call".to_string(),
                    "decision:3:run_project_checks".to_string()
                ),
                (
                    "function_call_output".to_string(),
                    "decision:3:run_project_checks".to_string()
                ),
                ("message:user".to_string(), String::new()),
            ]
        );
        assert_eq!(outputs_for(&mapped, "call_model")[0]["output"], "proposal");

        // Without the remembered calls the old anchor is kept.
        let unknown = OpenAIResponsesProvider::map_messages_with_chain_calls(
            &messages,
            Some("resp_model_turn"),
            None,
            false,
        )
        .expect("maps");
        assert_eq!(
            item_shape(&unknown)[0],
            (
                "function_call_output".to_string(),
                "decision:3:run_project_checks".to_string()
            )
        );
    }

    #[test]
    fn a_continuation_answers_a_chain_call_the_delta_lacks() {
        let messages = vec![
            message(
                MessageRole::Assistant,
                vec![
                    tool_call_block("call_x", "apply_code_proposal"),
                    tool_call_block("call_y", "run_project_checks"),
                ],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block("call_x", "applied"), text_block("delta")],
            ),
        ];
        let chain_calls = vec!["call_x".to_string(), "call_y".to_string()];

        let mapped = OpenAIResponsesProvider::map_messages_with_chain_calls(
            &messages,
            Some("resp_parallel"),
            Some(chain_calls.as_slice()),
            false,
        )
        .expect("maps");

        assert_eq!(
            item_shape(&mapped),
            vec![
                ("function_call_output".to_string(), "call_y".to_string()),
                ("function_call_output".to_string(), "call_x".to_string()),
                ("message:user".to_string(), String::new()),
            ]
        );
        assert_eq!(
            outputs_for(&mapped, "call_y")[0]["output"],
            crate::providers::MISSING_TOOL_RESULT_TEXT
        );
        assert_eq!(outputs_for(&mapped, "call_x")[0]["output"], "applied");
    }

    #[test]
    fn a_parsed_response_remembers_its_function_calls_for_the_next_continuation() {
        let response_id = "resp_pairing_registry_roundtrip_7f3c";
        remember_response_function_calls(&json!({
            "id": response_id,
            "output": [
                {"type": "reasoning", "id": "rs_1"},
                {"type": "function_call", "id": "fc_1", "call_id": "call_reg", "name": "read", "arguments": "{}"},
                {"type": "message", "role": "assistant", "content": []},
            ],
        }));
        assert_eq!(
            remembered_response_function_calls(response_id),
            Some(vec!["call_reg".to_string()])
        );
        assert_eq!(
            remembered_response_function_calls("resp_never_seen_pairing"),
            None
        );

        let messages = vec![
            message(
                MessageRole::Assistant,
                vec![tool_call_block("call_reg", "read")],
            ),
            message(MessageRole::User, vec![tool_result_block("call_reg", "ok")]),
            message(
                MessageRole::Assistant,
                vec![tool_call_block("decision:2:read", "read")],
            ),
            message(
                MessageRole::User,
                vec![tool_result_block("decision:2:read", "ok2")],
            ),
        ];
        let mapped = OpenAIResponsesProvider::map_messages(&messages, Some(response_id), false)
            .expect("maps");
        assert_eq!(
            item_shape(&mapped)[0],
            ("function_call_output".to_string(), "call_reg".to_string())
        );
        assert_eq!(outputs_for(&mapped, "decision:2:read").len(), 1);
    }

    #[test]
    fn continuation_maps_tool_results_to_function_call_output_items() {
        let messages = vec![
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "call_123".to_string(),
                    content: Value::String("done".to_string()),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Use the generated image to make a second version.".to_string(),
                }],
            },
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, Some("resp_123"), false)
            .expect("continuation input");

        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0]["type"], "function_call_output");
        assert_eq!(mapped[0]["call_id"], "call_123");
        assert_eq!(mapped[0]["output"], "done");
        assert_eq!(mapped[1]["role"], "user");
        assert_eq!(mapped[1]["content"][0]["type"], "input_text");
    }

    #[test]
    fn projected_tool_results_keep_order_ids_and_single_json_encoding() {
        let first = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "records": [{ "id": 1 }] }
        });
        let second = json!({
            "schema_version": 1,
            "outcome": { "status": "partial" },
            "data": { "records": [{ "id": 2 }] }
        });
        let messages = vec![LLMMessage {
            role: MessageRole::Tool,
            content: vec![
                ContentBlock::ToolResult {
                    tool_call_id: "call-a".into(),
                    content: first.clone(),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "call-b".into(),
                    content: second.clone(),
                },
            ],
        }];

        for previous_response_id in [None, Some("resp-previous")] {
            let mapped =
                OpenAIResponsesProvider::map_messages(&messages, previous_response_id, false)
                    .expect("map projected results");

            assert_eq!(mapped.len(), 2);
            assert_eq!(mapped[0]["type"], "function_call_output");
            assert_eq!(mapped[0]["call_id"], "call-a");
            assert_eq!(mapped[1]["call_id"], "call-b");
            let decoded_first: Value =
                serde_json::from_str(mapped[0]["output"].as_str().expect("first output string"))
                    .expect("first projection serialized exactly once");
            let decoded_second: Value =
                serde_json::from_str(mapped[1]["output"].as_str().expect("second output string"))
                    .expect("second projection serialized exactly once");
            assert_eq!(decoded_first, first);
            assert_eq!(decoded_second, second);
        }
    }

    #[test]
    fn assistant_history_maps_text_to_output_text_items() {
        let messages = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "Here is the earlier answer.".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "Follow up on that.".to_string(),
                }],
            },
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, None, false)
            .expect("assistant replay input");

        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0]["role"], "assistant");
        assert_eq!(mapped[0]["content"][0]["type"], "output_text");
        assert_eq!(
            mapped[0]["content"][0]["text"],
            "Here is the earlier answer."
        );
        assert_eq!(mapped[1]["role"], "user");
        assert_eq!(mapped[1]["content"][0]["type"], "input_text");
    }

    #[test]
    fn previous_response_id_is_extracted_from_request_extra() {
        let request = LLMRequest {
            extra: Some(
                json!({
                    "openai_previous_response_id": "resp_abc",
                })
                .into(),
            ),
            ..Default::default()
        };

        assert_eq!(
            OpenAIResponsesProvider::previous_response_id(&request).as_deref(),
            Some("resp_abc")
        );
    }

    #[test]
    fn typed_context_plan_controls_continuation_and_overrides_legacy_extra() {
        let mut request = LLMRequest {
            extra: Some(
                json!({
                    "openai_previous_response_id": "legacy_id",
                })
                .into(),
            ),
            ..Default::default()
        };
        request.set_context_reuse(crate::context_reuse::ContextReuseConfig {
            strategy: crate::context_reuse::ContextReuseStrategy::ServerContinuation,
            continuation_id: Some("typed_id".to_string()),
            transport_cohort_fingerprint: None,
            stable_prefix_fingerprint: None,
            disclosure_partition_fingerprint: None,
            session_key: None,
            rolling_prefix: false,
        });
        assert_eq!(
            OpenAIResponsesProvider::previous_response_id(&request).as_deref(),
            Some("typed_id")
        );

        request.set_context_reuse(crate::context_reuse::ContextReuseConfig {
            strategy: crate::context_reuse::ContextReuseStrategy::PrefixCache,
            continuation_id: Some("must_be_ignored".to_string()),
            transport_cohort_fingerprint: None,
            stable_prefix_fingerprint: None,
            disclosure_partition_fingerprint: None,
            session_key: None,
            rolling_prefix: true,
        });
        assert_eq!(
            OpenAIResponsesProvider::previous_response_id(&request),
            None,
            "a stateless strategy must not be upgraded by a stale legacy id"
        );
    }

    #[test]
    fn continuation_mode_skips_history_before_last_assistant() {
        // Conversation: system, user, assistant, tool_result, user_ledger.
        // With chain id set, the API already has system+user+assistant; we
        // should send only the tool_result and the new user_ledger (the
        // suffix after the last Assistant message).
        let messages = vec![
            LLMMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "system prompt".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "first user turn".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "call_1".to_string(),
                    name: "snapshot".to_string(),
                    arguments: json!({}),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "call_1".to_string(),
                    content: json!("snapshot output"),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "fresh ledger turn 2".to_string(),
                }],
            },
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, Some("resp_prev"), false)
            .expect("continuation map");

        // Only the suffix after Assistant lands in the payload:
        //   - tool_result becomes function_call_output
        //   - new user text becomes input_text user message
        assert_eq!(mapped.len(), 2, "should drop history before last Assistant");
        assert_eq!(mapped[0]["type"], "function_call_output");
        assert_eq!(mapped[0]["call_id"], "call_1");
        assert_eq!(mapped[1]["role"], "user");
        assert_eq!(mapped[1]["content"][0]["type"], "input_text");
        assert_eq!(mapped[1]["content"][0]["text"], "fresh ledger turn 2");
    }

    #[test]
    fn continuation_mode_falls_back_when_no_assistant_present() {
        // If `previous_response_id` is set but local list has no Assistant
        // message (compaction or state divergence), we fall back to the
        // standard mapping over the full list rather than silently dropping
        // content.
        let messages = vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::Text {
                text: "stranded turn".to_string(),
            }],
        }];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, Some("resp_prev"), false)
            .expect("fallback map");

        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0]["role"], "user");
        assert_eq!(mapped[0]["content"][0]["text"], "stranded turn");
    }

    #[test]
    fn continuation_mode_falls_back_when_assistant_is_last_message() {
        // Defensive guard: if the last message is itself the Assistant
        // (i.e. the slice would be empty), fall back to the full list so
        // the API doesn't receive an empty `input` array.
        let messages = vec![
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "user turn".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "assistant text".to_string(),
                }],
            },
        ];

        let mapped = OpenAIResponsesProvider::map_messages(&messages, Some("resp_prev"), false)
            .expect("fallback map");

        // Both messages forwarded — fallback path on empty suffix.
        assert_eq!(mapped.len(), 2);
    }

    /// A caller that already sliced to the delta sends tool results with no
    /// assistant anchor. Dropping the chain there orphans the
    /// `function_call_output` — the provider rejects the whole request with
    /// "No tool call found for function call output with call_id ...", which is
    /// how an iOS Tutor turn failed on 2026-08-03.
    #[test]
    fn a_pre_sliced_tool_result_payload_keeps_the_chain() {
        let pre_sliced = vec![
            LLMMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "system".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "call_ibBsChw3TGLrwM2EU2Fwby2v".to_string(),
                    content: serde_json::Value::String("projected".to_string()),
                }],
            },
        ];
        assert!(OpenAIResponsesProvider::has_safe_continuation_delta(
            &pre_sliced
        ));
    }

    /// Tutor can reject an under-planned completion by appending a recovery
    /// instruction immediately after the projected tool error. The instruction
    /// is part of the continuation delta, not evidence that the server chain
    /// should be discarded.
    #[test]
    fn a_pre_sliced_tool_result_followed_by_user_recovery_keeps_the_chain() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-luna".to_string(),
            messages: vec![
                LLMMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: "system".to_string(),
                    }],
                },
                LLMMessage {
                    role: MessageRole::Tool,
                    content: vec![ContentBlock::ToolResult {
                        tool_call_id: "call_tutor_completion".to_string(),
                        content: json!({"status": "error", "reason": "plan incomplete"}),
                    }],
                },
                LLMMessage {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: "Continue the lesson and repair the plan.".to_string(),
                    }],
                },
            ]
            .into(),
            extra: Some(
                json!({
                    "openai_previous_response_id": "resp_with_tutor_tool_call",
                })
                .into(),
            ),
            ..Default::default()
        };

        let (body, _) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("continuation request body");

        assert_eq!(body["previous_response_id"], "resp_with_tutor_tool_call");
        // The system prompt is not repeated: the chain carries it, so the new
        // input is exactly the tool result plus the recovery instruction.
        assert_eq!(body["input"].as_array().expect("input items").len(), 2);
        assert_eq!(body["input"][0]["type"], "function_call_output");
        assert_eq!(body["input"][0]["call_id"], "call_tutor_completion");
        assert_eq!(body["input"][1]["role"], "user");
        assert_eq!(
            body["input"][1]["content"][0]["text"],
            "Continue the lesson and repair the plan."
        );
    }

    /// End-to-end shape of an ordinary second chat turn: the caller resolved
    /// the anchor, sliced history to the new user message, and handed over the
    /// chain id. The body must carry `previous_response_id` — without it the
    /// request is a brand-new conversation holding nothing but the system
    /// prompt and the latest message, and every earlier turn is lost with no
    /// error anywhere. The system prompt rides the chain, so it is not
    /// re-sent as a new input item.
    #[test]
    fn a_presliced_chat_turn_chains_and_omits_the_system_prompt() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![
                LLMMessage {
                    role: MessageRole::System,
                    content: vec![ContentBlock::Text {
                        text: "you are an assistant".to_string(),
                    }],
                },
                LLMMessage {
                    role: MessageRole::User,
                    content: vec![ContentBlock::Text {
                        text: "What word did I ask you to remember?".to_string(),
                    }],
                },
            ]
            .into(),
            extra: Some(
                json!({
                    "openai_previous_response_id": "resp_prior_turn",
                })
                .into(),
            ),
            ..Default::default()
        };

        let (body, _) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("continuation request body");

        assert_eq!(body["previous_response_id"], "resp_prior_turn");
        let input = body["input"].as_array().expect("input items");
        assert_eq!(input.len(), 1, "only the new user message is new input");
        assert_eq!(input[0]["role"], "user");
        assert_eq!(
            input[0]["content"][0]["text"],
            "What word did I ask you to remember?"
        );
    }

    /// One user turn behind the system prompt is the ordinary chat shape: the
    /// caller resolved the chain anchor itself and sliced history down to the
    /// new message. There is no prior turn in the payload for the chain to
    /// replay twice, so the chain must survive — dropping it here silently
    /// restarts the conversation and the model loses every earlier turn.
    #[test]
    fn a_presliced_user_turn_keeps_the_chain() {
        let delta = vec![
            LLMMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "system".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "hello".to_string(),
                }],
            },
        ];
        assert!(OpenAIResponsesProvider::has_safe_continuation_delta(&delta));
    }

    /// Two user turns with no anchor is a real local transcript (the orphan
    /// pile a failed turn leaves behind). Chaining it would append content the
    /// server already holds, so it still takes the clean bootstrap path.
    #[test]
    fn an_anchorless_transcript_still_bootstraps() {
        let orphaned = vec![
            LLMMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "system".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "first".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "second".to_string(),
                }],
            },
        ];
        assert!(!OpenAIResponsesProvider::has_safe_continuation_delta(
            &orphaned
        ));
    }

    #[test]
    fn continuation_checkpoint_requires_a_nonempty_suffix() {
        let system_only = vec![LLMMessage {
            role: MessageRole::System,
            content: vec![ContentBlock::Text {
                text: "system".to_string(),
            }],
        }];
        assert!(!OpenAIResponsesProvider::has_safe_continuation_delta(
            &system_only
        ));

        let empty_suffix = vec![
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "request".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "response".to_string(),
                }],
            },
        ];
        assert!(!OpenAIResponsesProvider::has_safe_continuation_delta(
            &empty_suffix
        ));

        let safe_delta = vec![
            empty_suffix[0].clone(),
            empty_suffix[1].clone(),
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "changed state".to_string(),
                }],
            },
        ];
        assert!(OpenAIResponsesProvider::has_safe_continuation_delta(
            &safe_delta
        ));
    }

    #[test]
    fn standard_mode_unchanged_when_no_chain_id() {
        // Verify the suffix-only behaviour is gated on `previous_response_id`.
        // Without a chain id, all messages — including system and prior user
        // turns — must still be sent for non-chained Responses calls.
        let messages = vec![
            LLMMessage {
                role: MessageRole::System,
                content: vec![ContentBlock::Text {
                    text: "system".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "user".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: "assistant".to_string(),
                }],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "follow up".to_string(),
                }],
            },
        ];

        let mapped =
            OpenAIResponsesProvider::map_messages(&messages, None, false).expect("standard map");
        assert_eq!(mapped.len(), 4);
    }

    #[test]
    fn gpt_5_6_request_emits_key_options_and_explicit_input_breakpoint() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user(format!(
                "stable prefix\n{}\nvolatile turn",
                crate::types::CACHE_BREAKPOINT_SENTINEL
            ))]
            .into(),
            metadata: RequestMetadata {
                operation: "agentic_decision".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let (body, _) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("request body");

        assert!(body["prompt_cache_key"]
            .as_str()
            .expect("cache key")
            .starts_with("magician:v1:"));
        assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
        assert_eq!(
            body["input"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert_eq!(body["input"][0]["content"][0]["text"], "stable prefix");
        assert_eq!(body["input"][0]["content"][1]["text"], "volatile turn");
    }

    #[test]
    fn older_or_disabled_responses_requests_strip_marker_without_cache_fields() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let message = LLMMessage::user(format!(
            "stable prefix\n{}\nvolatile turn",
            crate::types::CACHE_BREAKPOINT_SENTINEL
        ));
        let older = LLMRequest {
            model: "gpt-5.5".to_string(),
            messages: vec![message.clone()].into(),
            ..Default::default()
        };
        let disabled = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![message].into(),
            prompt_cache: Some(crate::types::PromptCacheConfig::Disabled),
            ..Default::default()
        };

        for request in [older, disabled] {
            let (body, _) = provider
                .build_responses_request_body(&request, &request.model)
                .expect("request body");
            assert!(body.get("prompt_cache_key").is_none());
            assert!(body.get("prompt_cache_options").is_none());
            assert!(body["input"][0]["content"][0]
                .get("prompt_cache_breakpoint")
                .is_none());
            assert_eq!(
                body["input"][0]["content"][0]["text"],
                "stable prefix\nvolatile turn"
            );
        }
    }

    #[test]
    fn map_usage_reads_responses_api_token_fields() {
        let usage = OpenAIResponsesProvider::map_usage(&json!({
            "input_tokens": 1200,
            "input_tokens_details": {
                "cached_tokens": 800,
                "cache_write_tokens": 50
            },
            "output_tokens": 300,
            "output_tokens_details": {
                "reasoning_tokens": 75
            },
            "total_tokens": 1500
        }))
        .expect("valid usage");

        assert_eq!(usage.prompt_tokens, Some(1200));
        assert_eq!(usage.completion_tokens, Some(300));
        assert_eq!(usage.total_tokens, Some(1500));
        assert_eq!(usage.reasoning_tokens, Some(75));
        assert_eq!(usage.cached_tokens, Some(800));
        assert_eq!(usage.cache_creation_tokens, Some(50));
    }

    #[test]
    fn map_usage_preserves_chat_completion_token_fields() {
        let usage = OpenAIResponsesProvider::map_usage(&json!({
            "prompt_tokens": 40,
            "prompt_tokens_details": {
                "cached_tokens": 10
            },
            "completion_tokens": 20,
            "completion_tokens_details": {
                "reasoning_tokens": 5
            }
        }))
        .expect("valid usage");

        assert_eq!(usage.prompt_tokens, Some(40));
        assert_eq!(usage.completion_tokens, Some(20));
        assert_eq!(usage.total_tokens, Some(60));
        assert_eq!(usage.reasoning_tokens, Some(5));
        assert_eq!(usage.cached_tokens, Some(10));
    }

    #[test]
    fn map_usage_keeps_canonical_field_precedence_lazy() {
        let too_large = u64::from(u32::MAX) + 1;
        let usage = OpenAIResponsesProvider::map_usage(&json!({
            "prompt_tokens": 40,
            "input_tokens": too_large,
            "completion_tokens": 20,
            "output_tokens": too_large,
            "prompt_tokens_details": {
                "cached_tokens": 10,
                "cache_write_tokens": 4
            },
            "input_tokens_details": {
                "cached_tokens": too_large,
                "reasoning_tokens": too_large,
                "cache_write_tokens": too_large,
                "cache_creation_tokens": too_large
            },
            "cached_tokens": too_large,
            "reasoning_tokens": 5,
            "completion_tokens_details": {
                "reasoning_tokens": too_large
            },
            "cache_write_tokens": too_large,
            "cache_creation_tokens": too_large,
            "total_tokens": 60
        }))
        .expect("unused compatibility aliases must not override canonical counters");

        assert_eq!(usage.prompt_tokens, Some(40));
        assert_eq!(usage.completion_tokens, Some(20));
        assert_eq!(usage.total_tokens, Some(60));
        assert_eq!(usage.reasoning_tokens, Some(5));
        assert_eq!(usage.cached_tokens, Some(10));
        assert_eq!(usage.cache_creation_tokens, Some(4));
    }

    #[test]
    fn map_usage_rejects_wide_and_overflowing_provider_counters() {
        let wide = OpenAIResponsesProvider::map_usage(&json!({
            "input_tokens": u64::from(u32::MAX) + 1
        }))
        .expect_err("wide usage must fail closed");
        assert!(wide.to_string().contains("input_tokens"));

        let sum = OpenAIResponsesProvider::map_usage(&json!({
            "input_tokens": u32::MAX,
            "output_tokens": 1
        }))
        .expect_err("overflowing derived total must fail closed");
        assert!(sum.to_string().contains("total_tokens"));
    }

    #[test]
    fn responses_json_mode_uses_text_format() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("Return JSON")].into(),
            response_format: Some(LLMResponseFormat::JsonObject.into()),
            ..Default::default()
        };

        let (body, _timeout) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("request body");
        assert!(body.get("response_format").is_none());
        assert_eq!(body["text"]["format"], json!({"type": "json_object"}));
    }

    #[test]
    fn disclosure_bound_responses_force_provider_storage_off_after_extras() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let mut request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("protected"), LLMMessage::user("delta")].into(),
            extra: Some(
                json!({
                    "store": true,
                    "background": true,
                    "conversation": "stale",
                    "openai_previous_response_id": "resp-stale"
                })
                .into(),
            ),
            ..Default::default()
        };
        request.metadata.single_physical_attempt = true;

        let (body, _) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("protected request body");
        assert_eq!(body.get("store"), Some(&Value::Bool(false)));
        assert!(body.get("background").is_none());
        assert!(body.get("conversation").is_none());
        assert!(body.get("previous_response_id").is_none());
    }

    #[test]
    fn responses_json_schema_uses_flat_text_format_shape() {
        let provider = OpenAIResponsesProvider::new("test-key");
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false
        });
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("Return JSON")].into(),
            response_format: Some(
                LLMResponseFormat::JsonSchema {
                    schema: schema.clone(),
                }
                .into(),
            ),
            ..Default::default()
        };

        let (body, _timeout) = provider
            .build_responses_request_body(&request, &request.model)
            .expect("request body");
        assert!(body.get("response_format").is_none());
        assert_eq!(
            body["text"]["format"],
            json!({"type": "json_schema", "name": "response", "schema": schema})
        );
    }

    #[test]
    fn responses_metadata_serializes_tags_as_a_string() {
        let metadata = RequestMetadata {
            operation: "memory_entity_extraction".to_string(),
            tags: Some(vec!["lane:cloud".to_string(), "repeat:1".to_string()]),
            ..Default::default()
        };

        assert_eq!(
            OpenAIResponsesProvider::map_metadata(&metadata),
            Some(json!({
                "operation": "memory_entity_extraction",
                "tags": "lane:cloud,repeat:1"
            }))
        );
    }

    #[test]
    fn reasoning_payload_defaults_to_none_for_gpt_5_1_plus() {
        let payload = OpenAIResponsesProvider::reasoning_payload_for_request("gpt-5.6-terra", None)
            .expect("reasoning payload");

        assert_eq!(payload["effort"], "none");
        assert!(payload.get("summary").is_none());
    }

    #[test]
    fn reasoning_payload_omits_none_for_gpt_6_astra() {
        assert!(
            OpenAIResponsesProvider::reasoning_payload_for_request("gpt-6-astra", None).is_none()
        );
    }

    #[test]
    fn reasoning_payload_preserves_explicit_reasoning_summary() {
        let payload = OpenAIResponsesProvider::reasoning_payload_for_request(
            "gpt-5.6-terra",
            Some(&ReasoningConfig {
                effort: Some("high".to_string()),
                ..Default::default()
            }),
        )
        .expect("reasoning payload");

        assert_eq!(payload["effort"], "high");
        assert_eq!(payload["summary"], "auto");
    }

    #[test]
    fn reasoning_payload_keeps_catalog_strategy_and_token_budget_internal() {
        for effort in ["low", "medium", "high", "xhigh", "max"] {
            let payload = OpenAIResponsesProvider::reasoning_payload_for_request(
                "gpt-6.1-sol",
                Some(&ReasoningConfig {
                    effort: Some(effort.to_string()),
                    max_reasoning_tokens: Some(4096),
                    strategy: Some("native_effort".to_string()),
                    summary: Some("concise".to_string()),
                }),
            )
            .expect("reasoning payload");

            assert_eq!(payload, json!({"effort": effort, "summary": "concise"}));
        }
    }

    #[test]
    fn reasoning_payload_omits_summary_for_explicit_none() {
        let payload = OpenAIResponsesProvider::reasoning_payload_for_request(
            "gpt-5.6-terra",
            Some(&ReasoningConfig {
                effort: Some("none".to_string()),
                max_reasoning_tokens: Some(4096),
                strategy: Some("ignored_when_disabled".to_string()),
                ..Default::default()
            }),
        )
        .expect("reasoning payload");

        assert_eq!(payload["effort"], "none");
        assert!(payload.get("summary").is_none());
        assert!(payload.get("max_tokens").is_none());
        assert!(payload.get("strategy").is_none());
    }

    #[test]
    fn reasoning_payload_does_not_default_none_for_legacy_or_pro_models() {
        assert!(OpenAIResponsesProvider::reasoning_payload_for_request("gpt-5", None).is_none());
        assert!(
            OpenAIResponsesProvider::reasoning_payload_for_request("gpt-5-pro", None).is_none()
        );
        assert!(OpenAIResponsesProvider::reasoning_payload_for_request(
            "gpt-5",
            Some(&ReasoningConfig {
                effort: Some("none".to_string()),
                ..Default::default()
            })
        )
        .is_none());
    }
}

#[derive(Default)]
struct ToolCallDelta {
    name: Option<String>,
    arguments: String,
}

#[async_trait]
impl LLMProvider for OpenAIResponsesProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        match self.dialect {
            ResponsesDialect::OpenAI => LLMProviderKind::OpenAI,
            ResponsesDialect::Xai => LLMProviderKind::Xai,
        }
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let protected_request = request.metadata.single_physical_attempt;
        let retry_attempt = Self::max_tokens_retry_attempt(&request);
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for OpenAI Responses provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let has_reasoning = request.reasoning.is_some();
        let has_extra = request.extra.is_some();
        let has_metadata = request.metadata.trace_id.is_some()
            || request
                .metadata
                .tags
                .as_ref()
                .map(|tags| !tags.is_empty())
                .unwrap_or(false);

        info!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "issuing OpenAI Responses request"
        );

        let (mut body, timeout) = self.build_responses_request_body(&request, &model)?;

        debug!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            has_reasoning = has_reasoning,
            has_response_format = request.response_format.is_some(),
            has_metadata = has_metadata,
            has_extra = has_extra,
            "constructed OpenAI Responses request payload"
        );

        // Plain calls stream too, so a long answer that is still producing is
        // bounded by progress, not cut at the request timeout; the
        // `response.completed` event carries the same response object a plain
        // call returns. A `background` response is the exception: a plain
        // call returns it queued at once, while streaming it would wait for
        // the result.
        let background = body.get("background").and_then(Value::as_bool) == Some(true);
        if request.stream || !background {
            body.insert("stream".to_string(), Value::Bool(true));
            info!(
                provider = "openai_responses",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "entering streaming mode for OpenAI Responses"
            );
            // No token sink on the `invoke()` path — callers that
            // want per-token delivery come in through the trait-level
            // `invoke_stream` override below, which builds the same
            // body and forwards the channel handle into
            // `invoke_streaming`.
            return self
                .invoke_streaming_with_max_tokens_retry(request, body, timeout, None)
                .await;
        }

        let response = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .timeout(timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;

        let status = response.status();
        let raw_body = read_bounded_response_text(response).await?;

        let payload = parse_provider_json_value(&raw_body)?;
        drop(raw_body);
        if status.is_client_error() || status.is_server_error() {
            if let Some(error) = payload.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                if protected_request {
                    error!(
                        provider = "openai_responses",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "OpenAI Responses API rejected protected request"
                    );
                    return Err(LLMError::Provider {
                        provider: self.provider_kind().to_string(),
                        message: "protected provider request failed".to_owned(),
                    });
                }
                error!(
                    provider = "openai_responses",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "OpenAI Responses API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                error!(
                    provider = "openai_responses",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "OpenAI Responses API returned unexpected error payload"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: format!("HTTP status {}", status),
                });
            }
        }

        remember_response_function_calls(&payload);
        let (messages, tool_calls, tool_results, text, reasoning_text) =
            Self::parse_output(&payload)?;

        // Server-side searches bill per call; surface the count at the
        // provider boundary so cost attribution does not depend on callers
        // re-scanning `raw_response`.
        let web_search_calls = crate::server_web_search::web_search_call_count(
            &crate::capability::LLMProviderKind::OpenAI,
            &payload,
        );
        if web_search_calls > 0 {
            info!(
                provider = "openai_responses",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                web_search_calls,
                "server-side web searches executed by OpenAI"
            );
        }

        let usage = payload.get("usage").map(Self::map_usage).transpose()?;

        let finish_reason = payload
            .get("status")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        // Surface the server-side response id so the inner-loop runner
        // can pass it back as `previous_response_id` next turn for
        // reasoning-item persistence. Scoped per-model — runner clears
        // on profile switch.
        let response_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let response = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            response_id,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(tool_results),
            usage,
            finish_reason,
            // Parsing above borrows the provider tree; retain the original
            // allocation instead of cloning the complete response payload.
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "openai_responses",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by OpenAI Responses"
            );
        }

        info!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "OpenAI Responses call succeeded"
        );

        if !request.metadata.single_physical_attempt
            && retry_attempt == 0
            && Self::should_retry_max_tokens(&response)
        {
            let Some(base_max_output_tokens) =
                Self::retry_base_max_output_tokens(&request, &response)
            else {
                return Ok(response);
            };
            let retry_max_output_tokens = Self::retry_max_output_tokens(base_max_output_tokens);
            if retry_max_output_tokens > base_max_output_tokens {
                warn!(
                    provider = "openai_responses",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    base_max_output_tokens,
                    retry_max_output_tokens,
                    tool_call_count = response.tool_calls.len(),
                    "OpenAI Responses call was incomplete due to max_output_tokens; retrying with higher max_output_tokens"
                );
                let mut retry_request = request.clone();
                retry_request.max_output_tokens = Some(retry_max_output_tokens);
                Self::set_max_tokens_retry_attempt(&mut retry_request, 1);
                match self.invoke(retry_request).await {
                    Ok(retry_response) => return Ok(retry_response),
                    Err(error) if Self::is_excessive_max_tokens_error(&error) => {
                        if let Some(backed_off_max_output_tokens) =
                            Self::backed_off_max_output_tokens(
                                base_max_output_tokens,
                                retry_max_output_tokens,
                            )
                        {
                            warn!(
                                provider = "openai_responses",
                                model = %model,
                                operation = %operation,
                                trace_id = trace_id.as_deref().unwrap_or(""),
                                retry_max_output_tokens,
                                backed_off_max_output_tokens,
                                error = %error,
                                "OpenAI Responses retry rejected larger max_output_tokens; retrying with backed-off token budget"
                            );
                            let mut backed_off_request = request.clone();
                            backed_off_request.max_output_tokens =
                                Some(backed_off_max_output_tokens);
                            Self::set_max_tokens_retry_attempt(&mut backed_off_request, 2);
                            return self.invoke(backed_off_request).await;
                        }
                        return Err(error);
                    },
                    Err(error) => return Err(error),
                }
            }
        }

        Ok(response)
    }

    /// Streaming entry point. Builds the same payload as `invoke()`
    /// but with `stream: true`, then drives the SSE feed through
    /// `invoke_streaming` with the channel handle wired in so each
    /// `response.output_text.delta` is forwarded as
    /// `StreamDelta::Token(text)`. The chat surface renders text
    /// incrementally as it arrives instead of waiting for the full
    /// response — fixes the "no streaming over the Responses API"
    /// regression where the default trait impl silently fell back to
    /// non-streaming invoke + single Done.
    async fn invoke_stream(
        &self,
        mut request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for OpenAI Responses provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();

        info!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing OpenAI Responses streaming request"
        );

        request.stream = true;
        let (mut body, timeout) = self.build_responses_request_body(&request, &model)?;
        body.insert("stream".to_string(), Value::Bool(true));

        let response = self
            .invoke_streaming_with_max_tokens_retry(request, body, timeout, Some(&tx))
            .await?;
        // Send the canonical Done frame so downstream consumers can
        // settle the turn (assemble the final response, fire
        // post-stream callbacks). Errors from a closed receiver are
        // best-effort — the LLM call itself already completed.
        let _ = tx.send(StreamDelta::Done(response)).await;
        Ok(())
    }
}

impl OpenAIResponsesProvider {
    /// Build the OpenAI Responses request body and resolve the
    /// per-request timeout. Pulled out of `invoke()` so the streaming
    /// entrypoint can produce the same payload without re-routing
    /// through `invoke()` (which can't surface the `tx` channel handle
    /// the streaming variant needs).
    ///
    /// Returns `(body, timeout)` ready to be `POST`ed. The body never
    /// has `stream: true` set; callers that want streaming insert it
    /// after this returns so both invocation paths can share the same
    /// builder.
    fn build_responses_request_body(
        &self,
        request: &LLMRequest,
        model: &str,
    ) -> LLMResult<(Map<String, Value>, Duration)> {
        if request.media.is_some() {
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Responses provider requires media to be included as ContentBlock::Image"
                    .to_string(),
            ));
        }
        if request.input_media.is_some() {
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Responses provider requires media inputs to be provided via message content blocks".to_string(),
            ));
        }

        let requested_previous_response_id = (!request.metadata.single_physical_attempt)
            .then(|| Self::previous_response_id(request))
            .flatten();
        let previous_response_id = requested_previous_response_id
            .filter(|_| Self::has_safe_continuation_delta(&request.messages));
        let xai = self.dialect == ResponsesDialect::Xai;
        if xai {
            crate::server_web_search::reject_unsupported("xai", request)?;
        }
        let cache_plan = if xai {
            super::openai_prompt_cache::OpenAIPromptCachePlan::default()
        } else {
            openai_prompt_cache_plan(request, model)
        };
        let messages = Self::map_messages(
            &request.messages,
            previous_response_id.as_deref(),
            cache_plan.explicit_breakpoint,
        )?;

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.to_string()));
        body.insert("input".to_string(), Value::Array(messages));
        if let Some(previous_response_id) = previous_response_id {
            body.insert(
                "previous_response_id".to_string(),
                Value::String(previous_response_id),
            );
        }

        // Note: modalities parameter is NOT supported in Responses API
        // (only in Realtime API). Media/vision is handled via content blocks.

        // Server-side web search and function tools are mutually exclusive on
        // one request: search-augmented one-shot calls never dispatch client
        // tools, so `tool_choice: "required"` must not be emitted either.
        crate::server_web_search::reject_invalid_flag("openai_responses", request)?;
        let server_web_search = crate::server_web_search::requested(request);
        if server_web_search {
            crate::server_web_search::reject_mixed_with_function_tools(
                "openai_responses",
                request,
            )?;
            body.insert(
                "tools".to_string(),
                Value::Array(vec![
                    crate::server_web_search::openai_responses_web_search_tool(request),
                ]),
            );
        } else if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools)),
            );

            // Translate caller-provided tool_choice (Anthropic format) to OpenAI Responses format.
            let caller_override = request.extra_value().and_then(|e| e.get("tool_choice"));
            let tool_choice = match caller_override {
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                    Value::String("required".to_string())
                },
                // Tools advertised, use forbidden (a final no-tool-use answer
                // over a transcript that holds tool calls).
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("none") => {
                    Value::String("none".to_string())
                },
                Some(v @ Value::String(_)) => clone_json_value_iteratively(v),
                _ => Value::String("required".to_string()),
            };
            body.insert("tool_choice".to_string(), tool_choice);
        }

        let response_text_format =
            request
                .response_format_value()
                .and_then(|format| match format {
                    LLMResponseFormat::Text => None,
                    LLMResponseFormat::JsonObject => Some(json!({ "type": "json_object" })),
                    LLMResponseFormat::JsonSchema { schema } => Some(json!({
                        "type": "json_schema",
                        "name": "response",
                        "schema": clone_json_value_iteratively(schema),
                    })),
                });

        let reasoning_payload = if xai {
            Self::xai_reasoning_payload(request.reasoning.as_ref())
        } else {
            Self::reasoning_payload_for_request(&model, request.reasoning.as_ref())
        };
        if let Some(reasoning) = reasoning_payload {
            body.insert("reasoning".to_string(), reasoning);
        }

        if !xai {
            if let Some(metadata) = Self::map_metadata(&request.metadata) {
                body.insert("metadata".to_string(), metadata);
            }
        }

        // Skip temperature/top_p when a reasoning config is set or the model
        // rejects them (catalog `wire.sampling`, else the name fallback).
        let rejects_sampling = match request.model_wire.as_ref() {
            Some(wire) => !wire.sampling,
            None => uncatalogued_rejects_sampling(model),
        };

        if request.reasoning.is_none() && !rejects_sampling {
            if let Some(temp) = request.temperature {
                body.insert("temperature".to_string(), Value::from(temp));
            }

            if let Some(top_p) = request.top_p {
                body.insert("top_p".to_string(), Value::from(top_p));
            }
        }

        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_output_tokens".to_string(), Value::from(max_tokens));
        }

        // Handle text parameter with nested fields (e.g., text.verbosity).
        // Responses structured output also belongs at text.format; the legacy
        // top-level response_format field is valid only for Chat Completions.
        let mut text_object = Map::new();
        if let Some(format) = response_text_format {
            text_object.insert("format".to_string(), format);
        }
        if let Some(extra) = request.extra_value() {
            if let Value::Object(extra_map) = extra {
                if let Some(verbosity) = extra_map.get("verbosity") {
                    if let Some(verbosity_str) = verbosity.as_str() {
                        text_object.insert(
                            "verbosity".to_string(),
                            Value::String(verbosity_str.to_string()),
                        );
                    }
                }
            }
        }

        if !text_object.is_empty() {
            body.insert("text".to_string(), Value::Object(text_object));
        }

        // Process remaining extra parameters (excluding fields handled above).
        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                // Skip config-only metadata keys and fields handled above.
                // verbosity → already moved into text.verbosity
                // tool_choice → already emitted in the tools block above
                // use_chat / use_responses / api_version / cost_per_observation /
                // fallback_profile → routing/config keys, must never reach the API
                // viewport / disable_yutori_builtins / disable_tools →
                //   magician/yutori-only routing hints; OpenAI rejects unknown
                //   parameters so these MUST be stripped before forwarding.
                // router_*  → magicllm-router routing pins (provider /
                //   profile / preserve_model overrides) consumed in
                //   `router.rs` before the request reaches a provider.
                if matches!(
                    key.as_str(),
                    "verbosity"
                            | "tool_choice"
                            | "server_web_search"
                            | "use_chat"
                            | "use_responses"
                            | "openai_api_mode"
                            | "gemini_api_mode"
                            | "openai_previous_response_id"
                            | "openai_responses_disable_chaining"
                            | "api_version"
                            | "cost_per_observation"
                            | "fallback_profile"
                            | "streaming"
                            | "max_tokens_retry_attempt"
                            | "viewport"
                            | "disable_tools"
                            | "disable_yutori_builtins"
                            | "router_provider_override"
                            | "router_profile_override"
                            | "router_preserve_model"
                            // GPT-5.6+ prompt-cache controls are validated and
                            // model-gated below. Never forward raw copies to
                            // older models, which reject these fields.
                            | "prompt_cache_key"
                            | "prompt_cache_options"
                            // All `reasoning_*` keys live on the typed
                            // `ReasoningConfig` struct (consumed via
                            // `map_reasoning`) and inside the nested
                            // `reasoning` object on the API. None of them
                            // are valid top-level Responses body params —
                            // OpenAI 400s on unknown keys, so strip them
                            // defensively if a profile YAML accidentally
                            // lands them in the `metadata:` block.
                            | "reasoning_summary"
                            | "reasoning_strategy"
                            | "reasoning_max_tokens"
                ) {
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        // Disclosure-bound app calls are admitted only under the explicit
        // no_provider_storage posture. Apply this after profile/caller extras
        // so an untrusted `store: true` lane cannot override the control-plane
        // decision at the physical body boundary.
        if request.metadata.single_physical_attempt {
            body.remove("previous_response_id");
            body.remove("background");
            body.remove("conversation");
            body.insert("store".to_string(), Value::Bool(false));
        }

        let prompt_cache_key = if xai {
            Self::xai_prompt_cache_key(request)
        } else {
            cache_plan.prompt_cache_key
        };
        if let Some(prompt_cache_key) = prompt_cache_key {
            body.insert(
                "prompt_cache_key".to_string(),
                Value::String(prompt_cache_key),
            );
        }
        if let Some(prompt_cache_options) = cache_plan.prompt_cache_options {
            body.insert("prompt_cache_options".to_string(), prompt_cache_options);
        }

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        Ok((body, timeout))
    }

    /// Stream a Responses API completion. The `token_tx` channel — when
    /// supplied — receives a `StreamDelta::Token(text)` for every
    /// `response.output_text.delta` event as the upstream SSE feed
    /// produces it. Callers that just want the aggregated response
    /// (the legacy `invoke()` path) pass `None`. The trait-level
    /// `invoke_stream` override sets it to `Some(tx)` so chat surfaces
    /// can render tokens as they arrive instead of waiting for the
    /// full response to land.
    /// `invoke_streaming` with the same max_output_tokens retry `invoke()`
    /// applies: a stream that ended `incomplete` at the output cap is asked
    /// again once with twice the budget (backed off if the provider rejects
    /// that). Text the cut-off attempt already streamed stays on screen, so
    /// the retry streams tokens only when that attempt showed none; its
    /// final response still carries the whole answer.
    async fn invoke_streaming_with_max_tokens_retry(
        &self,
        request: LLMRequest,
        body: Map<String, Value>,
        timeout: Duration,
        token_tx: Option<&mpsc::Sender<StreamDelta>>,
    ) -> LLMResult<LLMResponse> {
        let response = self
            .invoke_streaming(&request, body, timeout, token_tx)
            .await?;
        if request.metadata.single_physical_attempt
            || Self::max_tokens_retry_attempt(&request) != 0
            || !Self::should_retry_max_tokens(&response)
        {
            return Ok(response);
        }
        let Some(base_max_output_tokens) = Self::retry_base_max_output_tokens(&request, &response)
        else {
            return Ok(response);
        };
        let retry_max_output_tokens = Self::retry_max_output_tokens(base_max_output_tokens);
        if retry_max_output_tokens <= base_max_output_tokens {
            return Ok(response);
        }
        warn!(
            provider = "openai_responses",
            model = %request.model,
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            base_max_output_tokens,
            retry_max_output_tokens,
            "OpenAI Responses stream was incomplete due to max_output_tokens; retrying with higher max_output_tokens"
        );
        let retry_tx =
            token_tx.filter(|_| response.text.as_deref().is_none_or(|text| text.is_empty()));
        let mut retry_request = request;
        retry_request.max_output_tokens = Some(retry_max_output_tokens);
        Self::set_max_tokens_retry_attempt(&mut retry_request, 1);
        match self.stream_once(&retry_request, retry_tx).await {
            Err(error) if Self::is_excessive_max_tokens_error(&error) => {
                let Some(backed_off_max_output_tokens) = Self::backed_off_max_output_tokens(
                    base_max_output_tokens,
                    retry_max_output_tokens,
                ) else {
                    return Err(error);
                };
                warn!(
                    provider = "openai_responses",
                    model = %retry_request.model,
                    retry_max_output_tokens,
                    backed_off_max_output_tokens,
                    error = %error,
                    "OpenAI Responses stream retry rejected larger max_output_tokens; retrying with backed-off token budget"
                );
                retry_request.max_output_tokens = Some(backed_off_max_output_tokens);
                Self::set_max_tokens_retry_attempt(&mut retry_request, 2);
                self.stream_once(&retry_request, retry_tx).await
            },
            outcome => outcome,
        }
    }

    async fn stream_once(
        &self,
        request: &LLMRequest,
        token_tx: Option<&mpsc::Sender<StreamDelta>>,
    ) -> LLMResult<LLMResponse> {
        let (mut body, timeout) = self.build_responses_request_body(request, &request.model)?;
        body.insert("stream".to_string(), Value::Bool(true));
        self.invoke_streaming(request, body, timeout, token_tx)
            .await
    }

    async fn invoke_streaming(
        &self,
        request: &LLMRequest,
        body: Map<String, Value>,
        timeout: Duration,
        token_tx: Option<&mpsc::Sender<StreamDelta>>,
    ) -> LLMResult<LLMResponse> {
        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let model = request.model.clone();

        info!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "opened streaming response with OpenAI Responses"
        );

        let response = super::send_streaming_request(
            self.client
                .post(&self.base_url)
                .header("Authorization", format!("Bearer {}", self.api_key))
                .header("Content-Type", "application/json")
                .json(&body),
            timeout,
        )
        .await?;

        let status = response.status();
        if status.is_client_error() || status.is_server_error() {
            let raw_body = read_bounded_response_text(response).await?;
            let payload = parse_provider_json_or_string(&raw_body)?;
            if let Some(error) = payload.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                if request.metadata.single_physical_attempt {
                    error!(
                        provider = "openai_responses",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "OpenAI Responses API rejected protected request"
                    );
                    return Err(LLMError::Provider {
                        provider: self.provider_kind().to_string(),
                        message: super::PROTECTED_PROVIDER_ERROR.to_owned(),
                    });
                }
                error!(
                    provider = "openai_responses",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "OpenAI Responses streaming API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: format!("HTTP status {}", status),
                });
            }
        }

        let payload = if super::is_json_response(&response) {
            // A server that ignored `stream: true` sent the whole response.
            super::read_json_body(response, timeout).await?
        } else {
            // Only a plain call needs the terminal event: a streaming caller
            // already rendered whatever text arrived.
            self.read_response_events(
                response,
                timeout,
                token_tx,
                !request.stream,
                request.metadata.single_physical_attempt,
            )
            .await?
        };
        let finish_reason = payload
            .get("status")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        remember_response_function_calls(&payload);
        let (messages, tool_calls, tool_results, text, reasoning_text) =
            Self::parse_output(&payload)?;

        // Server-side searches bill per call; surface the count at the
        // provider boundary so cost attribution does not depend on callers
        // re-scanning `raw_response`.
        let web_search_calls = crate::server_web_search::web_search_call_count(
            &crate::capability::LLMProviderKind::OpenAI,
            &payload,
        );
        if web_search_calls > 0 {
            info!(
                provider = "openai_responses",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                web_search_calls,
                "server-side web searches executed by OpenAI"
            );
        }
        let usage = payload.get("usage").map(Self::map_usage).transpose()?;
        let response_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let response = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            response_id,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(tool_results),
            usage,
            finish_reason,
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "openai_responses",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by OpenAI Responses streaming"
            );
        }

        info!(
            provider = "openai_responses",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "OpenAI Responses streaming call succeeded"
        );

        Ok(response)
    }

    /// Read a Responses event stream to its end: the `response.completed` /
    /// `response.incomplete` response object, or one assembled from the
    /// deltas when the stream carried none. `require_terminal` fails the
    /// latter instead, since a plain call must not mistake a cut-off stream
    /// for a whole answer.
    async fn read_response_events(
        &self,
        response: reqwest::Response,
        timeout: Duration,
        token_tx: Option<&mpsc::Sender<StreamDelta>>,
        require_terminal: bool,
        protected: bool,
    ) -> LLMResult<Value> {
        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut utf8_carry = Vec::with_capacity(4);
        let mut body_admission = SseBodyAdmission::new();
        let mut aggregated_text = String::new();
        let mut tool_calls: HashMap<String, ToolCallDelta> = HashMap::new();
        let mut final_response: Option<Value> = None;

        #[derive(Clone, Copy)]
        enum StreamEventKind {
            TextDelta,
            ToolCallDelta,
            Completed,
            Error,
            Failed,
            Other,
        }

        let mut done = false;
        let mut watchdog = super::StreamWatchdog::new(timeout);

        while !done {
            let Some(chunk) = watchdog.next(&mut stream).await? else {
                break;
            };
            let chunk = chunk.map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;
            body_admission.admit_chunk(&chunk)?;
            append_sse_utf8_chunk(&mut buffer, &mut utf8_carry, &chunk)?;

            let mut consumed = 0;
            while let Some(data) = next_sse_data(&buffer, &mut consumed) {
                if data == "[DONE]" {
                    done = true;
                    break;
                }

                let mut value = parse_provider_json_value(data.as_ref())?;
                drop(data);

                if value
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(responses_event_is_output)
                {
                    watchdog.mark_progress();
                }
                let event_kind = match value.get("type").and_then(Value::as_str) {
                    Some("response.output_text.delta") => StreamEventKind::TextDelta,
                    Some("response.output_tool_call.delta") => StreamEventKind::ToolCallDelta,
                    // `incomplete` ends the stream as terminally as
                    // `completed` (e.g. at max_output_tokens) and carries the
                    // status, `incomplete_details` and usage the caller needs.
                    Some("response.completed" | "response.incomplete") => {
                        StreamEventKind::Completed
                    },
                    Some("response.error" | "error") => StreamEventKind::Error,
                    Some("response.failed") => StreamEventKind::Failed,
                    _ => StreamEventKind::Other,
                };
                match event_kind {
                    StreamEventKind::TextDelta => {
                        if let Some(delta) = value.get("delta") {
                            // Accept both the documented
                            // shape (`delta` is a string)
                            // and the legacy shape
                            // (`delta.text` is a string).
                            // OpenAI flipped formats between
                            // beta and GA — handling both
                            // keeps the parser stable across
                            // future tweaks too.
                            let token_text = delta.as_str().map(str::to_string).or_else(|| {
                                delta
                                    .get("text")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            });
                            if let Some(text) = token_text {
                                aggregated_text.push_str(&text);
                                // Forward the token chunk
                                // through the streaming
                                // channel so chat UIs render
                                // text incrementally. The
                                // upstream chain uses
                                // bounded channels with
                                // `try_send`-style drop
                                // semantics; using the
                                // backpressure-aware
                                // `send().await` here is
                                // fine because the channel
                                // sized 256+ deep
                                // (`chat_api.rs`) easily
                                // absorbs tight bursts.
                                if let Some(tx) = token_tx {
                                    let _ = tx.send(StreamDelta::Token(text)).await;
                                }
                            }
                        }
                    },
                    StreamEventKind::ToolCallDelta => {
                        if let Some(tool_call) =
                            value.get("tool_call").or_else(|| value.get("delta"))
                        {
                            if let Some(id) = tool_call
                                .get("id")
                                .or_else(|| tool_call.get("tool_call_id"))
                                .and_then(Value::as_str)
                            {
                                let entry = tool_calls.entry(id.to_string()).or_default();
                                if let Some(name) = tool_call.get("name").and_then(Value::as_str) {
                                    entry.name = Some(name.to_string());
                                }
                                if let Some(delta) = tool_call.get("delta") {
                                    if let Some(arguments) =
                                        delta.get("arguments").and_then(Value::as_str)
                                    {
                                        append_tool_argument_fragment(
                                            &mut entry.arguments,
                                            arguments,
                                        )?;
                                    }
                                } else if let Some(arguments) =
                                    tool_call.get("arguments").and_then(Value::as_str)
                                {
                                    append_tool_argument_fragment(&mut entry.arguments, arguments)?;
                                }
                            }
                        }
                    },
                    StreamEventKind::Completed => {
                        if let Some(response) = value
                            .as_object_mut()
                            .and_then(|object| object.remove("response"))
                        {
                            final_response = Some(response);
                        }
                    },
                    StreamEventKind::Error => {
                        let message = value
                            .get("error")
                            .and_then(|err| err.get("message"))
                            .or_else(|| value.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("unknown error")
                            .to_string();
                        // A protected request's error text may echo its
                        // content; it never leaves the adapter.
                        let message = if protected {
                            super::PROTECTED_PROVIDER_ERROR.to_owned()
                        } else {
                            message
                        };
                        return Err(LLMError::Provider {
                            provider: self.provider_kind().to_string(),
                            message,
                        });
                    },
                    StreamEventKind::Failed => {
                        let message = value
                            .get("response")
                            .and_then(|response| response.get("error"))
                            .and_then(|err| err.get("message"))
                            .and_then(Value::as_str)
                            .unwrap_or("response failed")
                            .to_string();
                        // A protected request's error text may echo its
                        // content; it never leaves the adapter.
                        let message = if protected {
                            super::PROTECTED_PROVIDER_ERROR.to_owned()
                        } else {
                            message
                        };
                        return Err(LLMError::Provider {
                            provider: self.provider_kind().to_string(),
                            message,
                        });
                    },
                    StreamEventKind::Other => {},
                }
                if done {
                    break;
                }
            }
            if consumed > 0 {
                buffer.drain(..consumed);
            }
        }

        finish_sse_utf8(&utf8_carry)?;

        if let Some(response) = final_response {
            return Ok(response);
        }
        if require_terminal {
            return Err(LLMError::Transport(
                "OpenAI Responses stream ended before the response completed".to_string(),
            ));
        }
        let mut content = Vec::new();
        if !aggregated_text.is_empty() {
            content.push(json!({ "type": "output_text", "text": aggregated_text }));
        }

        for (id, call) in tool_calls.iter() {
            if let Some(name) = &call.name {
                content.push(json!({
                    "type": "tool_call",
                    "tool_call": {
                        "id": id,
                        "function": {
                            "name": name,
                            "arguments": call.arguments.clone(),
                        }
                    }
                }));
            }
        }

        Ok(json!({
            "output": [
                {
                    "type": "message",
                    "role": "assistant",
                    "content": content,
                }
            ]
        }))
    }
}

#[cfg(test)]
mod stream_progress_tests {
    use super::responses_event_is_output;

    /// Deltas and finished items are output; an opened item, a search still
    /// running and the lifecycle events are not.
    #[test]
    fn only_output_events_count_as_progress() {
        for output in [
            "response.output_text.delta",
            "response.reasoning_summary_text.delta",
            "response.function_call_arguments.delta",
            "response.output_item.done",
            "response.web_search_call.completed",
        ] {
            assert!(responses_event_is_output(output), "{output}");
        }
        for not_output in [
            "response.created",
            "response.in_progress",
            "response.output_item.added",
            "response.content_part.added",
            "response.web_search_call.in_progress",
            "response.web_search_call.searching",
            "response.completed",
        ] {
            assert!(!responses_event_is_output(not_output), "{not_output}");
        }
    }
}

#[cfg(test)]
mod xai_dialect_tests {
    use std::sync::Arc;

    use serde_json::json;

    use super::*;

    #[test]
    fn whole_call_rules_reach_neither_dialect_as_schema() {
        // xAI refuses a top-level anyOf ("tool parameter root must be an object
        // type"); OpenAI accepts but does not enforce it outside strict mode.
        let tools = vec![LLMToolSpec {
            name: "shell".to_string(),
            description: "Run a command.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}, "job_id": {"type": "string"}},
                "anyOf": [{"required": ["command"]}, {"required": ["job_id"]}],
            }),
        }];

        let mapped = OpenAIResponsesProvider::map_tools(&tools);
        assert!(mapped[0]["parameters"].get("anyOf").is_none());
        assert_eq!(
            mapped[0]["parameters"]["properties"],
            tools[0].parameters["properties"]
        );
        assert!(mapped[0]["description"]
            .as_str()
            .unwrap()
            .contains(r#""anyOf":[{"required":["command"]},{"required":["job_id"]}]"#));
    }

    fn xai_request(reasoning: Option<ReasoningConfig>) -> LLMRequest {
        LLMRequest {
            model: "grok-4.7".to_string(),
            messages: vec![
                LLMMessage::system("stable instructions"),
                LLMMessage::user("volatile turn"),
            ]
            .into(),
            reasoning,
            metadata: RequestMetadata {
                operation: "agentic_decision".to_string(),
                trace_id: Some("exec_probe".to_string()),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn with_session_key(mut request: LLMRequest, key: &str) -> LLMRequest {
        let mut reuse = crate::context_reuse::ContextReuseConfig::new(
            crate::context_reuse::ContextReuseStrategy::ServerContinuation,
        );
        reuse.session_key = Some(key.to_string());
        request.context_reuse = Some(Arc::new(reuse));
        request
    }

    /// Measured against api.x.ai on 2026-09-23: `metadata` is refused with
    /// "Argument not supported: metadata" — every request the OpenAI dialect
    /// builds carries it, so a config-only `api_base_url` profile failed its
    /// first call.
    #[test]
    fn an_xai_request_never_carries_openai_metadata() {
        let provider = OpenAIResponsesProvider::with_base_url_for_dialect(
            "k",
            "https://api.x.ai/v1/responses",
            ResponsesDialect::Xai,
        );
        let (body, _) = provider
            .build_responses_request_body(&xai_request(None), "grok-4.7")
            .expect("xai body");
        assert!(body.get("metadata").is_none(), "{body:?}");
        let openai = OpenAIResponsesProvider::new("k");
        let (body, _) = openai
            .build_responses_request_body(&xai_request(None), "grok-4.7")
            .expect("openai body");
        assert!(
            body.get("metadata").is_some(),
            "the OpenAI dialect is unchanged"
        );
    }

    /// xAI caches per server; `prompt_cache_key` routes one conversation to
    /// one server. The execution-scoped `session_key` is exactly that key.
    /// A disabled cache or a single-attempt (no-storage) request sends none.
    #[test]
    fn an_xai_request_keys_its_cache_on_the_execution_session() {
        let provider = OpenAIResponsesProvider::with_base_url_for_dialect(
            "k",
            "https://api.x.ai/v1/responses",
            ResponsesDialect::Xai,
        );
        let request = with_session_key(xai_request(None), "sess-abc");
        let (body, _) = provider
            .build_responses_request_body(&request, "grok-4.7")
            .expect("body");
        assert_eq!(body["prompt_cache_key"], "sess-abc");
        assert!(body.get("prompt_cache_options").is_none());

        let mut disabled = with_session_key(xai_request(None), "sess-abc");
        disabled.prompt_cache = Some(crate::types::PromptCacheConfig::Disabled);
        let (body, _) = provider
            .build_responses_request_body(&disabled, "grok-4.7")
            .expect("body");
        assert!(body.get("prompt_cache_key").is_none());

        let mut protected = with_session_key(xai_request(None), "sess-abc");
        protected.metadata.single_physical_attempt = true;
        let (body, _) = provider
            .build_responses_request_body(&protected, "grok-4.7")
            .expect("body");
        assert!(body.get("prompt_cache_key").is_none());
        assert_eq!(body["store"], Value::Bool(false));
    }

    /// Grok reasoning cannot be disabled ("does not support `reasoning_effort`
    /// value `none`", measured) and accepts low / medium / high / xhigh.
    #[test]
    fn an_xai_reasoning_payload_carries_only_supported_efforts() {
        let provider = OpenAIResponsesProvider::with_base_url_for_dialect(
            "k",
            "https://api.x.ai/v1/responses",
            ResponsesDialect::Xai,
        );
        let effort = |value: &str| ReasoningConfig {
            effort: Some(value.to_string()),
            strategy: Some("deliberate".to_string()),
            ..Default::default()
        };
        let body_for = |reasoning| {
            provider
                .build_responses_request_body(&xai_request(Some(reasoning)), "grok-4.7")
                .expect("body")
                .0
        };
        let high = body_for(effort("high"));
        assert_eq!(high["reasoning"]["effort"], "high");
        assert!(high["reasoning"].get("strategy").is_none(), "{high:?}");
        assert_eq!(body_for(effort("minimal"))["reasoning"]["effort"], "low");
        assert!(body_for(effort("none")).get("reasoning").is_none());
        let unset = provider
            .build_responses_request_body(&xai_request(None), "grok-4.7")
            .expect("body")
            .0;
        assert!(unset.get("reasoning").is_none());
    }

    #[test]
    fn an_xai_request_refuses_server_web_search_and_names_its_provider() {
        let provider = OpenAIResponsesProvider::with_base_url_for_dialect(
            "k",
            "https://api.x.ai/v1/responses",
            ResponsesDialect::Xai,
        );
        let mut request = xai_request(None);
        request.set_extra(json!({ "server_web_search": true }));
        let error = provider
            .build_responses_request_body(&request, "grok-4.7")
            .expect_err("xai has no reviewed server web search");
        assert!(error.to_string().contains("xai"), "{error}");
        assert_eq!(provider.provider_kind(), LLMProviderKind::Xai);
        assert_eq!(
            OpenAIResponsesProvider::new("k").provider_kind(),
            LLMProviderKind::OpenAI
        );
    }
}

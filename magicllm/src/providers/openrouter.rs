use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tracing::{debug, error, info, warn};

use super::{
    append_sse_utf8_chunk, default_http_client, finish_sse_utf8, next_sse_data,
    read_bounded_response_text, SseBodyAdmission,
};
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        append_tool_argument_fragment, clone_json_value_iteratively, parse_provider_json_or_string,
        parse_provider_json_value, parse_tool_argument_json_or_string, ContentBlock, LLMMessage,
        LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall, LLMToolSpec, MessageRole,
        PromptCacheConfig, ReasoningConfig, TokenUsage,
    },
};

const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1/chat/completions";
const MAX_TOKENS_RETRY_CAP: u32 = 32_000;
const MAX_TOKENS_RETRY_ATTEMPT_KEY: &str = "max_tokens_retry_attempt";

/// Provider backed by OpenRouter's Chat/Responses-compatible API surface.
pub struct OpenRouterProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl OpenRouterProvider {
    fn enforce_protected_retention(body: &mut Map<String, Value>, request: &LLMRequest) {
        if request.metadata.single_physical_attempt {
            // This runs after profile/caller extras so the trusted protected
            // route, rather than mutable metadata, owns retention posture.
            body.remove("cache_control");
            body.remove("session_id");
            body.insert("store".to_string(), Value::Bool(false));
            body.insert("provider".to_string(), json!({ "allow_fallbacks": false }));
        }
    }

    /// Creates a provider using the default OpenRouter endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL)
    }

    /// Creates a provider with a custom base URL (for self-hosted gateways).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

    /// Creates a provider with a preconfigured HTTP client.
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
        }
    }

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
        response.finish_reason.as_deref() == Some("length")
    }

    fn is_excessive_max_tokens_error(error: &LLMError) -> bool {
        let lower = error.to_string().to_ascii_lowercase();
        lower.contains("max_tokens")
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

    /// Best-effort capability inference from an OpenRouter `vendor/model` id.
    ///
    /// A PROFILE'S EXPLICIT FLAGS OVERRIDE THIS — see
    /// `effective_capabilities_for_request` in `router.rs`. This only decides
    /// what happens when a profile leaves `supports_vision` /
    /// `supports_reasoning` / `supports_tool_calling` unset, so it should stay
    /// conservative rather than clever.
    ///
    /// It is still worth keeping current: the previous version matched vision
    /// on only `vision` / `gpt-4o` / `sonnet`, which meant `claude-opus-4-7`,
    /// `claude-haiku-4-5`, and the whole `gpt-5.x` family were reported as
    /// text-only; and it gated reasoning on the literal `sonnet-4-5`, so
    /// `claude-sonnet-4-6` regressed to `None` the moment 4.6 shipped. Both
    /// were silently wrong for every model this workspace actually prices.
    ///
    /// Note the reasoning tier matters even when a profile sets
    /// `supports_reasoning: true`: the router promotes an inferred `None` only
    /// as far as `Standard`, never `Advanced`.
    fn detect_capability(model: &str) -> LLMCapability {
        let id = model.to_ascii_lowercase();
        let mut capability = LLMCapability::default();
        capability.tool_calling = true;
        capability.json_mode = true;
        capability.streaming = true;

        // Vision is the norm for current frontier families; list the text-only
        // exceptions rather than trying to enumerate every vision model.
        let text_only = id.contains("embed")
            || id.contains("rerank")
            || id.contains("whisper")
            || id.contains("tts")
            || id.contains("moderation")
            || id.contains("guard");
        let vision_family = id.contains("claude")
            || id.contains("gpt-4o")
            || id.contains("gpt-4.1")
            || id.contains("gpt-5")
            || id.contains("gemini")
            || id.contains("pixtral")
            || id.contains("vision")
            || id.contains("-vl")
            || id.contains("llava");
        if vision_family && !text_only && !capability.modalities.contains(&LLMModality::Vision) {
            capability.modalities.push(LLMModality::Vision);
        }

        // Families that expose a real reasoning/thinking budget. Matched on
        // family rather than an exact version so a point release does not
        // silently downgrade the tier, which is how `sonnet-4-5` broke.
        capability.reasoning = if id.contains("gpt-5")
            || id.contains("/o1")
            || id.contains("/o3")
            || id.contains("/o4")
            || id.contains("claude-sonnet-4")
            || id.contains("claude-opus-4")
            || id.contains("claude-sonnet-5")
            || id.contains("claude-opus-5")
            || id.contains("deepseek-r")
            || id.contains("deepseek-flash")
            || id.contains("deepseek-v4")
            || id.contains("reasoner")
            || id.contains("thinking")
            || id.contains("qwq")
        {
            LLMReasoning::Advanced
        } else {
            LLMReasoning::None
        };

        capability
    }

    /// Converts the generic `LLMMessage[]` into OpenRouter's OpenAI-compatible
    /// message payload.
    ///
    /// Does NOT strip the cache-breakpoint sentinel — that is handled in a
    /// second pass by [`Self::rewrite_user_text_content`] (called from
    /// [`Self::invoke`]) after `map_messages` returns, because the
    /// strip-vs-split decision depends on request-level context
    /// (`is_anthropic_model`, `prompt_cache_control`) that is not available
    /// here. For non-sentinel prompts this is a no-op; for sentinel-bearing
    /// prompts, the two-pass flow ensures every text block is either stripped
    /// (non-Anthropic routes, or non-user roles) or split-and-cache-anchored
    /// (Anthropic-family + user message + caching enabled).
    fn map_messages(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        let mut payload = Vec::with_capacity(messages.len());

        for message in messages {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            let mut text_parts = Vec::new();
            let mut image_parts: Vec<Value> = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_results: Vec<(String, String)> = Vec::new();

            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::Json { value } => text_parts.push(value.to_string()),
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => {
                        let args_str = match arguments {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": {
                                "name": name,
                                "arguments": args_str,
                            }
                        }));
                    },
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                    } => {
                        let text = match content {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        tool_results.push((tool_call_id.clone(), text));
                    },
                    // OpenRouter takes OpenAI's `image_url` content part, and
                    // its `url` accepts either a real URL or a base64 data URL.
                    // <https://openrouter.ai/docs/api_reference/overview>
                    ContentBlock::Image {
                        data,
                        media_type,
                        caption,
                    } => {
                        if let Some(caption) = caption {
                            text_parts.push(caption.clone());
                        }
                        let encoded = BASE64_STANDARD.encode(data);
                        image_parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": format!("data:{media_type};base64,{encoded}") },
                        }));
                    },
                    ContentBlock::ImageUrl { url, prompt } => {
                        if let Some(prompt) = prompt {
                            text_parts.push(prompt.clone());
                        }
                        image_parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url },
                        }));
                    },
                }
            }

            let has_tool_calls = !tool_calls.is_empty();
            if has_tool_calls {
                if !image_parts.is_empty() {
                    return Err(LLMError::UnsupportedCapability(
                        "OpenRouter provider does not support images on an assistant \
                         tool-call message"
                            .to_string(),
                    ));
                }
                let content = if text_parts.is_empty() {
                    Value::Null
                } else {
                    Value::String(text_parts.join("\n"))
                };
                payload.push(json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls,
                }));
            } else if tool_results.is_empty() && image_parts.is_empty() {
                // Text-only stays a plain string. Providers behind OpenRouter
                // that predate content arrays keep working unchanged.
                payload.push(json!({
                    "role": role,
                    "content": text_parts.join("\n"),
                }));
            } else if tool_results.is_empty() {
                let mut parts = Vec::with_capacity(1 + image_parts.len());
                let text = text_parts.join("\n");
                if !text.is_empty() {
                    parts.push(json!({ "type": "text", "text": text }));
                }
                parts.extend(image_parts.iter().cloned());
                payload.push(json!({
                    "role": role,
                    "content": parts,
                }));
            }

            if !tool_results.is_empty() {
                // OpenAI-compatible tool result messages are text-only and
                // carry exactly one call id. Preserve every result in source
                // order instead of assigning joined content to the last id.
                if !image_parts.is_empty() {
                    return Err(LLMError::UnsupportedCapability(
                        "OpenRouter provider does not support images in tool results".to_string(),
                    ));
                }
                if !has_tool_calls && !text_parts.is_empty() {
                    return Err(LLMError::UnsupportedCapability(
                        "OpenRouter tool-result messages cannot mix unpaired text with tool results"
                            .to_string(),
                    ));
                }
                for (tool_call_id, content) in tool_results {
                    payload.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": content,
                    }));
                }
            }
        }

        Ok(payload)
    }

    /// Insert function tools, or the OpenRouter web search plugin when the
    /// request enables server-side web search. Mutually exclusive (see
    /// `server_web_search` module docs).
    fn attach_tools_or_web_plugin(
        body: &mut Map<String, Value>,
        request: &LLMRequest,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_invalid_flag("openrouter", request)?;
        if crate::server_web_search::requested(request) {
            crate::server_web_search::reject_mixed_with_function_tools("openrouter", request)?;
            body.insert(
                "plugins".to_string(),
                Value::Array(vec![crate::server_web_search::openrouter_web_plugin(
                    request,
                )]),
            );
            return Ok(());
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools)),
            );
        }
        Ok(())
    }

    fn map_tools(tools: &[LLMToolSpec]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": tool.parameters,
                    }
                })
            })
            .collect()
    }

    /// Map our `ReasoningConfig` onto OpenRouter's `reasoning` object.
    ///
    /// OpenRouter documents `effort` and `max_tokens` as MUTUALLY EXCLUSIVE
    /// ("use either `effort` or `max_tokens`, not both"), so sending both — as
    /// this did — risks the request being rejected or one silently winning
    /// depending on which upstream provider the request is routed to. `effort`
    /// takes precedence because it is the portable control: OpenRouter
    /// translates it per-provider, whereas a raw token budget means different
    /// things across model families.
    /// <https://openrouter.ai/docs/use-cases/reasoning-tokens>
    fn map_reasoning(reasoning: &ReasoningConfig) -> Value {
        let mut map = Map::new();
        if let Some(effort) = reasoning.effort.as_ref() {
            map.insert("effort".to_string(), Value::String(effort.clone()));
        } else if let Some(max_tokens) = reasoning.max_reasoning_tokens {
            map.insert("max_tokens".to_string(), Value::from(max_tokens));
        }
        Value::Object(map)
    }

    fn is_anthropic_model(model: &str) -> bool {
        model.contains("/anthropic/") || model.starts_with("anthropic/")
    }

    /// When OpenRouter is routing to an Anthropic-family model, anchor the
    /// prompt-cache breakpoint on the system message's content block rather
    /// than at the request top level. Anthropic auto-applies a top-level
    /// `cache_control` to the LAST cacheable block, which in iterative
    /// decision workloads is the volatile user message, so the top-level
    /// placement almost never hits. Attaching it to the stable system block
    /// caches the `tools + system` prefix across iterations.
    ///
    /// Returns `true` when the cache_control was anchored on a system
    /// message, so the caller can skip emitting a redundant top-level
    /// `cache_control` field.
    fn anchor_cache_control_on_system_message(
        messages: &mut [Value],
        cache_control: &Value,
    ) -> bool {
        for message in messages.iter_mut() {
            if message.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            let Some(obj) = message.as_object_mut() else {
                continue;
            };
            let existing_content = obj
                .get("content")
                .map(clone_json_value_iteratively)
                .unwrap_or(Value::Null);
            let text = match existing_content {
                Value::String(s) => s,
                // If the caller already emitted a structured content block
                // (unlikely today, but future-proof), skip — anchoring over
                // that is ambiguous and we'd rather fall back to top-level.
                Value::Array(_) => return false,
                _ => continue,
            };
            if text.is_empty() {
                return false;
            }
            let mut block = Map::new();
            block.insert("type".to_string(), Value::String("text".to_string()));
            block.insert("text".to_string(), Value::String(text));
            block.insert("cache_control".to_string(), cache_control.clone());
            obj.insert(
                "content".to_string(),
                Value::Array(vec![Value::Object(block)]),
            );
            return true;
        }
        false
    }

    /// Strip the cache-breakpoint sentinel from a text payload and reconnect
    /// the halves with a single `\n` when both are non-empty. No-op when the
    /// sentinel is absent. Mirrors the reconnection rule used by every other
    /// magicllm provider.
    fn strip_sentinel_text(text: &str) -> String {
        if !text.contains(crate::types::CACHE_BREAKPOINT_SENTINEL) {
            return text.to_string();
        }
        let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
        match suffix {
            Some(s) if prefix.is_empty() => s,
            Some(s) if s.is_empty() => prefix,
            Some(s) => format!("{prefix}\n{s}"),
            None => prefix,
        }
    }

    /// Rewrite messages in an already-mapped OpenRouter payload to strip or
    /// split on `CACHE_BREAKPOINT_SENTINEL`.
    ///
    /// User messages: for Anthropic-family models with caching enabled AND a
    /// non-empty prefix, emit the user message as two content blocks with
    /// `cache_control: {"type":"ephemeral"}` on the first. For everyone else
    /// (non-Anthropic route, caching disabled, or empty prefix), concatenate
    /// the halves with the sentinel stripped and keep the content as a plain
    /// string.
    ///
    /// Defense-in-depth: non-user roles (system, assistant, tool) also get
    /// their text stripped of the sentinel. Today only user-facing templates
    /// carry the sentinel, but a future system/assistant/tool message with
    /// it would otherwise leak the internal marker to the upstream model.
    /// The SPLIT behaviour stays user-only — non-user messages only ever get
    /// a plain-string strip-and-reconnect.
    fn rewrite_user_text_content(
        messages: &mut [Value],
        is_anthropic_family: bool,
        cache_control_for_user: Option<&Value>,
    ) {
        for message in messages.iter_mut() {
            let is_user = message.get("role").and_then(Value::as_str) == Some("user");
            let Some(obj) = message.as_object_mut() else {
                continue;
            };
            match obj.get("content") {
                Some(Value::String(s)) => {
                    let text = s.clone();
                    let (prefix, suffix) = crate::types::split_on_cache_sentinel(&text);
                    let Some(suffix) = suffix else {
                        // No sentinel: passthrough.
                        continue;
                    };
                    if is_user
                        && is_anthropic_family
                        && cache_control_for_user.is_some()
                        && !prefix.is_empty()
                        && suffix.is_empty()
                    {
                        // A marker that ends the text marks the whole block;
                        // an empty second text block is rejected upstream.
                        let mut only = Map::new();
                        only.insert("type".to_string(), Value::String("text".to_string()));
                        only.insert("text".to_string(), Value::String(prefix));
                        only.insert(
                            "cache_control".to_string(),
                            cache_control_for_user.unwrap().clone(),
                        );
                        obj.insert("content".to_string(), Value::Array(vec![Value::Object(only)]));
                    } else if is_user
                        && is_anthropic_family
                        && cache_control_for_user.is_some()
                        && !prefix.is_empty()
                    {
                        let cc = cache_control_for_user.unwrap();
                        let mut first = Map::new();
                        first.insert("type".to_string(), Value::String("text".to_string()));
                        first.insert("text".to_string(), Value::String(prefix));
                        first.insert("cache_control".to_string(), cc.clone());
                        let mut second = Map::new();
                        second.insert("type".to_string(), Value::String("text".to_string()));
                        second.insert("text".to_string(), Value::String(suffix));
                        obj.insert(
                            "content".to_string(),
                            Value::Array(vec![Value::Object(first), Value::Object(second)]),
                        );
                    } else {
                        // Non-Anthropic, caching disabled, non-user role, or
                        // empty prefix: strip the sentinel and concatenate
                        // the halves back into one string.
                        // `split_on_cache_sentinel` already trims the
                        // surrounding newlines, so reinsert one between the
                        // halves when both sides are non-empty to preserve
                        // line boundaries.
                        let merged = if prefix.is_empty() {
                            suffix
                        } else if suffix.is_empty() {
                            prefix
                        } else {
                            format!("{prefix}\n{suffix}")
                        };
                        obj.insert("content".to_string(), Value::String(merged));
                    }
                },
                Some(Value::Array(_)) => {
                    // Already-structured content (e.g. assistant with tool
                    // calls — OpenRouter emits an array when tool_calls are
                    // present). Walk each text block and strip the sentinel
                    // in place. The SPLIT shape is only meaningful for
                    // single-text user messages, so structured content never
                    // gets split here.
                    if let Some(arr) = obj.get_mut("content").and_then(Value::as_array_mut) {
                        for block in arr.iter_mut() {
                            if block.get("type").and_then(Value::as_str) != Some("text") {
                                continue;
                            }
                            let Some(text) = block.get("text").and_then(Value::as_str) else {
                                continue;
                            };
                            if !text.contains(crate::types::CACHE_BREAKPOINT_SENTINEL) {
                                continue;
                            }
                            let stripped = Self::strip_sentinel_text(text);
                            if let Some(block_obj) = block.as_object_mut() {
                                block_obj.insert("text".to_string(), Value::String(stripped));
                            }
                        }
                    }
                },
                _ => {
                    // Null / other: nothing to strip.
                    continue;
                },
            }
        }
    }

    fn build_ephemeral_cache_control(ttl: Option<&str>) -> Value {
        let mut cache_control = Map::new();
        cache_control.insert("type".to_string(), Value::String("ephemeral".to_string()));
        if let Some(ttl) = ttl {
            cache_control.insert("ttl".to_string(), Value::String(ttl.to_string()));
        }
        Value::Object(cache_control)
    }

    fn prompt_cache_control(request: &LLMRequest) -> Option<Value> {
        if !Self::is_anthropic_model(&request.model) {
            return None;
        }

        if request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("cache_control"))
            .is_some()
        {
            return None;
        }

        match request.prompt_cache.as_ref() {
            Some(PromptCacheConfig::Disabled) => None,
            Some(config) if config.is_enabled() => {
                Some(Self::build_ephemeral_cache_control(config.ttl()))
            },
            _ => None,
        }
    }

    fn map_usage(data: &Value) -> LLMResult<TokenUsage> {
        let cached_tokens = super::bounded_usage_counter(
            "openrouter",
            "cached_tokens",
            data.get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| data.get("cached_tokens").and_then(Value::as_u64)),
        )?;

        Ok(TokenUsage {
            prompt_tokens: super::bounded_usage_counter(
                "openrouter",
                "prompt_tokens",
                data.get("prompt_tokens").and_then(Value::as_u64),
            )?,
            completion_tokens: super::bounded_usage_counter(
                "openrouter",
                "completion_tokens",
                data.get("completion_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: super::bounded_usage_counter(
                "openrouter",
                "total_tokens",
                data.get("total_tokens").and_then(Value::as_u64),
            )?,
            reasoning_tokens: super::bounded_usage_counter(
                "openrouter",
                "reasoning_tokens",
                data.get("reasoning_tokens").and_then(Value::as_u64),
            )?,
            cached_tokens,
            cache_creation_tokens: None,
        })
    }

    fn parse_output(
        payload: &Value,
    ) -> LLMResult<(Vec<LLMMessage>, Vec<LLMToolCall>, Option<String>)> {
        let mut messages = Vec::new();
        let mut tool_calls = Vec::new();

        let mut aggregated_text = Vec::new();

        if let Some(choices) = payload.get("choices").and_then(Value::as_array) {
            if let Some(choice) = choices.first() {
                if let Some(message) = choice.get("message") {
                    let mut content_blocks = Vec::new();
                    if let Some(content) = message.get("content").and_then(Value::as_str) {
                        aggregated_text.push(content.to_string());
                        content_blocks.push(ContentBlock::Text {
                            text: content.to_string(),
                        });
                    }

                    if let Some(tool_calls_array) =
                        message.get("tool_calls").and_then(Value::as_array)
                    {
                        for tool_call in tool_calls_array {
                            if let Some(function) = tool_call.get("function") {
                                let id = tool_call
                                    .get("id")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let name = function
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("")
                                    .to_string();
                                let arguments_value = function
                                    .get("arguments")
                                    .and_then(Value::as_str)
                                    .map(parse_tool_argument_json_or_string)
                                    .transpose()?
                                    .unwrap_or(Value::Null);

                                tool_calls.push(LLMToolCall {
                                    id: id.clone(),
                                    name: name.clone(),
                                    arguments: clone_json_value_iteratively(&arguments_value),
                                });
                                content_blocks.push(ContentBlock::ToolCall {
                                    id,
                                    name,
                                    arguments: arguments_value,
                                });
                            }
                        }
                    }

                    messages.push(LLMMessage {
                        role: MessageRole::Assistant,
                        content: content_blocks,
                    });
                }
            }
        }

        let text = if !aggregated_text.is_empty() {
            Some(aggregated_text.join("\n"))
        } else {
            None
        };

        Ok((messages, tool_calls, text))
    }

    fn map_response_format(format: Option<&LLMResponseFormat>) -> Option<Value> {
        match format {
            Some(LLMResponseFormat::Text) | None => None,
            Some(LLMResponseFormat::JsonObject) => Some(json!({ "type": "json_object" })),
            Some(LLMResponseFormat::JsonSchema { schema }) => Some(json!({
                "type": "json_schema",
                "json_schema": {
                    "name": "response",
                    "schema": clone_json_value_iteratively(schema),
                }
            })),
        }
    }
}

#[async_trait]
impl LLMProvider for OpenRouterProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::OpenRouter
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        let mut capability = Self::detect_capability(model);
        // The chat-completions transport translates the flag into the
        // OpenRouter web search plugin for every routed model.
        capability.web_search = true;
        capability
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let protected_request = request.metadata.single_physical_attempt;
        let retry_attempt = Self::max_tokens_retry_attempt(&request);
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for OpenRouter provider".to_string(),
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
            provider = "openrouter",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "issuing OpenRouter request"
        );

        if request.media.is_some() || request.input_media.is_some() {
            debug!(
                provider = "openrouter",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "rejecting request because inline media is not supported"
            );
            return Err(LLMError::UnsupportedCapability(
                "OpenRouter provider currently expects media via content blocks only".to_string(),
            ));
        }

        let mut messages = Self::map_messages(&request.messages)?;

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.clone()));

        // Server-side web search rides OpenRouter's `plugins` lane and is
        // mutually exclusive with function tools (see `server_web_search`
        // module docs).
        Self::attach_tools_or_web_plugin(&mut body, &request)?;

        if let Some(format) = Self::map_response_format(request.response_format_value()) {
            if !format.is_null() {
                body.insert("response_format".to_string(), format);
            }
        }

        if let Some(reasoning) = request.reasoning.clone() {
            body.insert("reasoning".to_string(), Self::map_reasoning(&reasoning));
        }

        if let Some(temp) = request.temperature {
            body.insert("temperature".to_string(), Value::from(temp));
        }

        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }

        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_tokens".to_string(), Value::from(max_tokens));
        }

        let cache_control = Self::prompt_cache_control(&request);
        let is_anthropic_family = Self::is_anthropic_model(&request.model);
        let rolling_prefix = request
            .context_reuse
            .as_ref()
            .map(|reuse| reuse.rolling_prefix)
            .unwrap_or(false);
        let explicit_cache_control = if rolling_prefix {
            None
        } else {
            cache_control.as_ref()
        };
        Self::rewrite_user_text_content(&mut messages, is_anthropic_family, explicit_cache_control);
        let cache_anchored_on_system = match explicit_cache_control {
            Some(cc) => Self::anchor_cache_control_on_system_message(&mut messages, cc),
            None => false,
        };
        body.insert("messages".to_string(), Value::Array(messages));
        if rolling_prefix || !cache_anchored_on_system {
            if let Some(cache_control) = cache_control {
                body.insert("cache_control".to_string(), cache_control);
            }
        }

        // OpenRouter uses this stable key for sticky provider routing, which
        // preserves otherwise provider-local prefix caches across an agentic
        // execution. It is opaque and contains no user or prompt content.
        if let Some(session_key) = request
            .context_reuse
            .as_ref()
            .and_then(|reuse| reuse.session_key.as_ref())
        {
            body.insert("session_id".to_string(), Value::String(session_key.clone()));
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                // With server-side web search enabled the plugins lane is
                // authoritative: a raw extra `tools`/`tool_choice` must not
                // smuggle function tools onto the same request through the
                // generic extras passthrough.
                if crate::server_web_search::flag_state(&request)
                    == crate::server_web_search::FlagState::On
                    && matches!(key.as_str(), "tools" | "tool_choice")
                {
                    continue;
                }
                // Magician/magicllm internal routing keys must not reach
                // OpenRouter — strict APIs reject unknown parameters.
                if matches!(
                    key.as_str(),
                    "streaming"
                            | "cost_per_observation"
                            | "fallback_profile"
                            | "max_tokens_retry_attempt"
                            | "openai_api_mode"
                            | "openai_responses_disable_chaining"
                            | "gemini_api_mode"
                            | "openai_previous_response_id"
                            | "use_chat"
                            | "use_responses"
                            | "viewport"
                            | "disable_tools"
                            | "disable_yutori_builtins"
                            | "router_provider_override"
                            | "router_profile_override"
                            | "router_preserve_model"
                            | "server_web_search" // consumed by the plugins block above
                            // All `reasoning_*` keys live on the typed
                            // `ReasoningConfig` struct, not the metadata
                            // block. OpenRouter's normalized chat schema
                            // does not accept them as top-level params —
                            // strip defensively.
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
        Self::enforce_protected_retention(&mut body, &request);

        debug!(
            provider = "openrouter",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            has_reasoning = has_reasoning,
            has_response_format = request.response_format.is_some(),
            has_metadata = has_metadata,
            has_extra = has_extra,
            "constructed OpenRouter request payload"
        );

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let mut headers = Vec::new();
        if let Some(metadata) = request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("headers"))
        {
            if let Some(map) = metadata.as_object() {
                for (key, value) in map {
                    if let Some(val) = value.as_str() {
                        headers.push((key.clone(), val.to_string()));
                    }
                }
            }
        }

        if let Some(metadata) = request.metadata.tags.as_ref() {
            // allow tags to specify title/referer e.g., ["referer:https://app", "title:Magician"]
            for tag in metadata {
                if let Some(rest) = tag.strip_prefix("referer:") {
                    headers.push(("HTTP-Referer".to_string(), rest.to_string()));
                } else if let Some(rest) = tag.strip_prefix("title:") {
                    headers.push(("X-Title".to_string(), rest.to_string()));
                }
            }
        }

        debug!(
            provider = "openrouter",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            header_count = headers.len(),
            "prepared OpenRouter headers"
        );

        let mut request_builder = self
            .client
            .post(&self.base_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .timeout(timeout)
            .json(&body);

        for (key, value) in headers {
            request_builder = request_builder.header(key, value);
        }

        if request.stream {
            let response = request_builder.send().await.map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;

            let status = response.status();
            if status.is_client_error() || status.is_server_error() {
                let raw_body = read_bounded_response_text(response).await?;
                let payload = parse_provider_json_or_string(&raw_body)?;
                let message = payload
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                error!(
                    provider = "openrouter",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "OpenRouter streaming API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            }

            info!(
                provider = "openrouter",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "processing streaming response from OpenRouter"
            );

            return handle_streaming(response, operation.clone(), trace_id.clone(), model.clone())
                .await;
        }

        let response = request_builder.send().await.map_err(|error| {
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
                        provider = "openrouter",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "OpenRouter API rejected protected request"
                    );
                    return Err(LLMError::Provider {
                        provider: self.provider_kind().to_string(),
                        message: "protected provider request failed".to_owned(),
                    });
                }
                error!(
                    provider = "openrouter",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "OpenRouter API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                error!(
                    provider = "openrouter",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "OpenRouter API returned unexpected error payload"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: format!("HTTP status {}", status),
                });
            }
        }

        let (messages, tool_calls, text) = Self::parse_output(&payload)?;
        let usage = payload.get("usage").map(Self::map_usage).transpose()?;
        let finish_reason = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .and_then(|choice| choice.get("finish_reason"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let response = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: None,
            response_id: None,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason,
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "openrouter",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by OpenRouter"
            );
        }

        info!(
            provider = "openrouter",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "OpenRouter call succeeded"
        );

        if !request.metadata.single_physical_attempt
            && !request.stream
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
                    provider = "openrouter",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    base_max_output_tokens,
                    retry_max_output_tokens,
                    tool_call_count = response.tool_calls.len(),
                    "OpenRouter response hit length; retrying with higher max_tokens"
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
                                provider = "openrouter",
                                model = %model,
                                operation = %operation,
                                trace_id = trace_id.as_deref().unwrap_or(""),
                                retry_max_output_tokens,
                                backed_off_max_output_tokens,
                                error = %error,
                                "OpenRouter retry rejected larger max_tokens; retrying with backed-off token budget"
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
}

#[derive(Default)]
struct ToolCallDelta {
    name: Option<String>,
    arguments: String,
}

async fn handle_streaming(
    response: reqwest::Response,
    operation: String,
    trace_id: Option<String>,
    model: String,
) -> LLMResult<LLMResponse> {
    info!(
        provider = "openrouter",
        model = %model,
        operation = %operation,
        trace_id = trace_id.as_deref().unwrap_or(""),
        "opened streaming response with OpenRouter"
    );

    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut utf8_carry = Vec::with_capacity(4);
    let mut body_admission = SseBodyAdmission::new();
    let mut aggregated_text = String::new();
    let mut tool_calls: HashMap<String, ToolCallDelta> = HashMap::new();
    let mut aggregated_annotations: Vec<Value> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut usage_value: Option<Value> = None;
    let mut done = false;

    while !done {
        let Some(chunk) = stream.next().await else {
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

            let value = parse_provider_json_value(data.as_ref())?;
            drop(data);

            if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                if let Some(choice) = choices.first() {
                    if let Some(delta) = choice.get("delta") {
                        if let Some(content) = delta.get("content") {
                            if let Some(text) = content.as_str() {
                                aggregated_text.push_str(text);
                            } else if let Some(array) = content.as_array() {
                                for item in array {
                                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                                        aggregated_text.push_str(text);
                                    }
                                }
                            }
                        }

                        if let Some(tool_calls_delta) =
                            delta.get("tool_calls").and_then(Value::as_array)
                        {
                            for tool_call in tool_calls_delta {
                                if let Some(id) = tool_call.get("id").and_then(Value::as_str) {
                                    let entry = tool_calls.entry(id.to_string()).or_default();

                                    if let Some(name) = tool_call
                                        .get("function")
                                        .and_then(|f| f.get("name"))
                                        .and_then(Value::as_str)
                                    {
                                        entry.name = Some(name.to_string());
                                    }

                                    if let Some(arguments) = tool_call
                                        .get("function")
                                        .and_then(|f| f.get("arguments"))
                                        .and_then(Value::as_str)
                                    {
                                        append_tool_argument_fragment(
                                            &mut entry.arguments,
                                            arguments,
                                        )?;
                                    }
                                }
                            }
                        }

                        // The web search plugin streams url_citation
                        // annotations on deltas; without capturing them the
                        // synthesized raw_response loses every citation on
                        // the streaming path.
                        if let Some(annotations) =
                            delta.get("annotations").and_then(Value::as_array)
                        {
                            for annotation in annotations {
                                if annotation.get("type").and_then(Value::as_str)
                                    == Some("url_citation")
                                {
                                    aggregated_annotations
                                        .push(clone_json_value_iteratively(annotation));
                                }
                            }
                        }
                    }

                    if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                        finish_reason = Some(reason.to_string());
                    }
                }
            }

            if let Some(usage) = value.get("usage") {
                usage_value = Some(usage.clone());
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

    let mut tool_calls_array = Vec::new();
    for (id, call) in tool_calls.iter() {
        if let Some(name) = &call.name {
            tool_calls_array.push(json!({
                "id": id,
                "function": {
                    "name": name,
                    "arguments": call.arguments.clone(),
                }
            }));
        }
    }

    let mut message_obj = serde_json::Map::new();
    message_obj.insert("role".to_string(), Value::String("assistant".to_string()));
    message_obj.insert(
        "content".to_string(),
        Value::String(aggregated_text.clone()),
    );
    if !tool_calls_array.is_empty() {
        message_obj.insert("tool_calls".to_string(), Value::Array(tool_calls_array));
    }
    if !aggregated_annotations.is_empty() {
        message_obj.insert(
            "annotations".to_string(),
            Value::Array(aggregated_annotations),
        );
    }

    let mut choice_obj = serde_json::Map::new();
    choice_obj.insert("message".to_string(), Value::Object(message_obj));
    if let Some(reason) = finish_reason.clone() {
        choice_obj.insert("finish_reason".to_string(), Value::String(reason));
    }

    let mut payload = Value::Object(serde_json::Map::from_iter(vec![(
        "choices".to_string(),
        Value::Array(vec![Value::Object(choice_obj)]),
    )]));

    if let Some(usage) = usage_value {
        payload["usage"] = usage;
    }

    let (messages, tool_calls, text) = OpenRouterProvider::parse_output(&payload)?;
    let usage = payload
        .get("usage")
        .map(OpenRouterProvider::map_usage)
        .transpose()?;

    let response = LLMResponse {
        text: text.map(Arc::<str>::from),
        reasoning_text: None,
        response_id: None,
        messages: Arc::new(messages),
        tool_calls: Arc::new(tool_calls),
        tool_results: Arc::new(Vec::new()),
        usage,
        finish_reason,
        raw_response: Some(Arc::new(payload)),
        provider_latency_ms: None,
        trace_receipt: None,
        route_identity: None,
    };

    if let Some(usage) = response.usage.as_ref() {
        debug!(
            provider = "openrouter",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            usage = ?usage,
            "token usage reported by OpenRouter streaming"
        );
    }

    info!(
        provider = "openrouter",
        model = %model,
        operation = %operation,
        trace_id = trace_id.as_deref().unwrap_or(""),
        finish_reason = response.finish_reason.as_deref().unwrap_or(""),
        tool_call_count = response.tool_calls.len(),
        "OpenRouter streaming call succeeded"
    );

    Ok(response)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_plugin_replaces_function_tools_and_fails_mixed_closed() {
        let mut request = LLMRequest::default();
        request.model = "anthropic/claude-sonnet-4.5".to_string();
        request.set_extra(serde_json::json!({"server_web_search": {"max_uses": 4}}));

        let mut body = Map::new();
        OpenRouterProvider::attach_tools_or_web_plugin(&mut body, &request)
            .expect("attach succeeds");
        assert_eq!(body["plugins"][0]["id"], "web");
        assert_eq!(body["plugins"][0]["max_results"], 4);
        assert!(body.get("tools").is_none());

        let mut mixed = request.clone();
        mixed.tools = vec![LLMToolSpec {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: serde_json::json!({}),
        }]
        .into();
        let mut mixed_body = Map::new();
        let error = OpenRouterProvider::attach_tools_or_web_plugin(&mut mixed_body, &mixed)
            .expect_err("mixed request must fail closed");
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn protected_request_forces_provider_storage_off_after_extras() {
        let mut request = LLMRequest::default();
        request.metadata.single_physical_attempt = true;
        request.set_extra(json!({
            "store": true,
            "cache_control": { "type": "ephemeral" },
            "session_id": "stale",
            "provider": { "allow_fallbacks": true }
        }));
        let mut body = request
            .extra_value()
            .and_then(Value::as_object)
            .cloned()
            .expect("object extras");

        OpenRouterProvider::enforce_protected_retention(&mut body, &request);

        assert_eq!(body.get("store"), Some(&Value::Bool(false)));
        assert!(body.get("cache_control").is_none());
        assert!(body.get("session_id").is_none());
        assert_eq!(body["provider"]["allow_fallbacks"], false);
    }

    fn user(blocks: Vec<ContentBlock>) -> LLMMessage {
        LLMMessage {
            role: MessageRole::User,
            content: blocks,
        }
    }

    #[test]
    fn map_usage_rejects_wide_provider_counters() {
        let error = OpenRouterProvider::map_usage(&json!({
            "completion_tokens": u64::from(u32::MAX) + 1
        }))
        .expect_err("wide usage must fail closed");

        assert!(error.to_string().contains("completion_tokens"));
    }

    #[test]
    fn text_only_messages_stay_a_plain_string() {
        // Back-compat: providers behind OpenRouter that predate content arrays
        // must keep receiving a bare string when there is no image.
        let mapped = OpenRouterProvider::map_messages(&[user(vec![ContentBlock::Text {
            text: "hello".into(),
        }])])
        .expect("map");
        assert_eq!(mapped[0]["content"], Value::String("hello".into()));
    }

    #[test]
    fn inline_image_becomes_a_base64_data_url_part() {
        // Regression guard: this previously returned UnsupportedCapability, so
        // every vision flow failed before reaching the network.
        let mapped = OpenRouterProvider::map_messages(&[user(vec![
            ContentBlock::Text {
                text: "what is this?".into(),
            },
            ContentBlock::Image {
                data: vec![0xDE, 0xAD, 0xBE, 0xEF],
                media_type: "image/png".into(),
                caption: None,
            },
        ])])
        .expect("images must map, not error");

        let parts = mapped[0]["content"].as_array().expect("content array");
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[0]["text"], "what is this?");
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(
            parts[1]["image_url"]["url"],
            format!(
                "data:image/png;base64,{}",
                BASE64_STANDARD.encode([0xDE, 0xAD, 0xBE, 0xEF])
            )
        );
    }

    #[test]
    fn image_url_block_passes_the_url_through() {
        let mapped = OpenRouterProvider::map_messages(&[user(vec![ContentBlock::ImageUrl {
            url: "https://example.com/a.png".into(),
            prompt: Some("describe".into()),
        }])])
        .expect("map");
        let parts = mapped[0]["content"].as_array().expect("content array");
        assert_eq!(parts[0]["text"], "describe");
        assert_eq!(parts[1]["image_url"]["url"], "https://example.com/a.png");
    }

    #[test]
    fn images_in_tool_results_fail_loudly() {
        // `tool` messages are text-only on the OpenAI wire shape. Dropping the
        // image silently would return a confident answer about nothing.
        let err = OpenRouterProvider::map_messages(&[LLMMessage {
            role: MessageRole::Tool,
            content: vec![
                ContentBlock::ToolResult {
                    tool_call_id: "call_1".into(),
                    content: Value::String("ok".into()),
                },
                ContentBlock::ImageUrl {
                    url: "https://example.com/a.png".into(),
                    prompt: None,
                },
            ],
        }])
        .expect_err("must not silently drop the image");
        assert!(matches!(err, LLMError::UnsupportedCapability(_)));
    }

    #[test]
    fn multiple_projected_tool_results_keep_order_ids_and_single_json_encoding() {
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
        let mapped = OpenRouterProvider::map_messages(&[
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::ToolCall {
                        id: "call-a".into(),
                        name: "lookup".into(),
                        arguments: json!({ "id": 1 }),
                    },
                    ContentBlock::ToolCall {
                        id: "call-b".into(),
                        name: "lookup".into(),
                        arguments: json!({ "id": 2 }),
                    },
                ],
            },
            LLMMessage {
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
            },
        ])
        .expect("map messages");

        assert_eq!(mapped.len(), 3);
        assert_eq!(mapped[0]["tool_calls"][0]["id"], "call-a");
        assert_eq!(mapped[0]["tool_calls"][1]["id"], "call-b");
        assert_eq!(mapped[1]["tool_call_id"], "call-a");
        assert_eq!(mapped[2]["tool_call_id"], "call-b");
        let decoded_first: Value =
            serde_json::from_str(mapped[1]["content"].as_str().expect("first JSON string"))
                .expect("first projection serialized exactly once");
        let decoded_second: Value =
            serde_json::from_str(mapped[2]["content"].as_str().expect("second JSON string"))
                .expect("second projection serialized exactly once");
        assert_eq!(decoded_first, first);
        assert_eq!(decoded_second, second);
    }

    #[test]
    fn projected_tool_result_rejects_unpaired_text() {
        let err = OpenRouterProvider::map_messages(&[LLMMessage {
            role: MessageRole::Tool,
            content: vec![
                ContentBlock::Text {
                    text: "unpaired".into(),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "call-a".into(),
                    content: json!({ "status": "ok" }),
                },
            ],
        }])
        .expect_err("mixed provider message must fail loudly");

        assert!(matches!(err, LLMError::UnsupportedCapability(_)));
    }

    #[test]
    fn reasoning_never_sends_effort_and_max_tokens_together() {
        // OpenRouter documents the two as mutually exclusive.
        let both = ReasoningConfig {
            effort: Some("high".into()),
            max_reasoning_tokens: Some(2000),
            strategy: None,
            summary: None,
        };
        let mapped = OpenRouterProvider::map_reasoning(&both);
        assert_eq!(mapped["effort"], "high");
        assert!(
            mapped.get("max_tokens").is_none(),
            "effort and max_tokens must not both be sent"
        );

        let budget_only = ReasoningConfig {
            effort: None,
            max_reasoning_tokens: Some(2000),
            strategy: None,
            summary: None,
        };
        assert_eq!(
            OpenRouterProvider::map_reasoning(&budget_only)["max_tokens"],
            2000
        );
    }

    #[test]
    fn capability_detection_covers_the_models_we_price() {
        // Every OpenRouter model with a pricing row must infer sensibly when a
        // profile leaves the flags unset. The old heuristic reported
        // claude-opus-4-7 and the gpt-5.x family as text-only, and dropped
        // claude-sonnet-4-6 to no-reasoning because it matched only
        // `sonnet-4-5`.
        for model in [
            "anthropic/claude-sonnet-4-6",
            "anthropic/claude-opus-4-7",
            "anthropic/claude-haiku-4-5",
            "openai/gpt-5.6-terra",
            "openai/gpt-5.6-luna",
            "openai/gpt-4o",
        ] {
            let cap = OpenRouterProvider::detect_capability(model);
            assert!(
                cap.modalities.contains(&LLMModality::Vision),
                "{model} should infer vision"
            );
            assert!(cap.tool_calling, "{model} should infer tool calling");
        }

        for model in [
            "anthropic/claude-sonnet-4-6",
            "anthropic/claude-opus-4-7",
            "openai/gpt-5.6-terra",
        ] {
            assert!(
                matches!(
                    OpenRouterProvider::detect_capability(model).reasoning,
                    LLMReasoning::Advanced
                ),
                "{model} should infer advanced reasoning"
            );
        }

        // Non-generative endpoints must not claim vision.
        let embed = OpenRouterProvider::detect_capability("openai/text-embedding-3-large");
        assert!(!embed.modalities.contains(&LLMModality::Vision));
    }
}

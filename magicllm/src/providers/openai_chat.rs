use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
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
        append_tool_argument_fragment, clone_json_value_iteratively, parse_provider_json_or_string,
        parse_provider_json_value, parse_tool_argument_json_or_string, ContentBlock, LLMMessage,
        LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall, LLMToolSpec, MessageRole,
        ReasoningConfig, StreamDelta, TokenUsage,
    },
};

pub const DEFAULT_BASE_URL_CHAT: &str = "https://api.openai.com/v1/chat/completions";
const MAX_TOKENS_RETRY_CAP: u32 = 32_000;
const MAX_TOKENS_RETRY_ATTEMPT_KEY: &str = "max_tokens_retry_attempt";

/// Minimal Chat Completions provider implementation used during the Phase 1 migration.
pub struct OpenAIChatProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl OpenAIChatProvider {
    /// Creates a provider using the default OpenAI Chat Completions endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL_CHAT)
    }

    /// Creates a provider with a custom base URL (useful for testing or proxies).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

    /// Creates a provider with a pre-configured reqwest client.
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

    pub(crate) fn capabilities_for_model(model: &str) -> LLMCapability {
        let mut capability = LLMCapability::default();
        capability.tool_calling = true;
        capability.json_mode = true;
        capability.streaming = true;

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

    /// Strip the Magician cache-breakpoint sentinel from a text payload,
    /// reconnecting the halves with a single `\n` when both are non-empty.
    ///
    /// OpenAI Chat Completions has no breakpoint concept (it uses automatic
    /// upstream prefix-caching), so if the rendered user prompt carries our
    /// internal marker we must strip it before sending. Matches the
    /// reconnection rule used by the Anthropic and OpenRouter providers.
    fn strip_cache_sentinel(text: &str) -> String {
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

    fn map_chat_text_content(
        role: MessageRole,
        text: &str,
        explicit_cache_breakpoint: bool,
    ) -> Value {
        if explicit_cache_breakpoint && matches!(role, MessageRole::System | MessageRole::User) {
            let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
            if let Some(suffix) = suffix {
                if !prefix.is_empty() && !suffix.is_empty() {
                    return json!([
                        {
                            "type": "text",
                            "text": prefix,
                            "prompt_cache_breakpoint": { "mode": "explicit" },
                        },
                        {
                            "type": "text",
                            "text": Self::strip_cache_sentinel(&suffix),
                        }
                    ]);
                }
                // A marker that ends the block marks the whole block.
                if !prefix.is_empty() {
                    return json!([{
                        "type": "text",
                        "text": prefix,
                        "prompt_cache_breakpoint": { "mode": "explicit" },
                    }]);
                }
            }
        }

        Value::String(Self::strip_cache_sentinel(text))
    }

    fn map_messages(
        messages: &[LLMMessage],
        explicit_cache_breakpoint: bool,
    ) -> LLMResult<Vec<Value>> {
        let mut payload = Vec::with_capacity(messages.len());

        for message in messages {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            // Collect tool calls and tool results separately from text content.
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_results: Vec<(String, String)> = Vec::new();

            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::Json { value } => text_parts.push(value.to_string()),
                    ContentBlock::ToolCall { id, name, arguments } => {
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
                    }
                    ContentBlock::ToolResult { tool_call_id, content } => {
                        let text = match content {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        tool_results.push((tool_call_id.clone(), text));
                    }
                    ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. } => {
                        return Err(LLMError::UnsupportedCapability(
                            "OpenAI Chat provider does not support inline images; use Responses API".to_string(),
                        ))
                    }
                }
            }

            if !tool_calls.is_empty() {
                // Assistant message with tool calls — OpenAI expects tool_calls on the message
                let content = if text_parts.is_empty() {
                    Value::Null
                } else {
                    Value::String(Self::strip_cache_sentinel(&text_parts.join("\n")))
                };
                payload.push(json!({
                    "role": "assistant",
                    "content": content,
                    "tool_calls": tool_calls,
                }));
            } else if tool_results.is_empty() {
                let content = Self::map_chat_text_content(
                    message.role,
                    &text_parts.join("\n"),
                    explicit_cache_breakpoint,
                );
                payload.push(json!({
                    "role": role,
                    "content": content,
                }));
            }

            if !tool_results.is_empty() {
                if !text_parts.is_empty() {
                    return Err(LLMError::UnsupportedCapability(
                        "OpenAI Chat tool-result messages cannot mix unpaired text with tool results"
                            .to_string(),
                    ));
                }
                // One provider tool message per result is required. Keeping
                // these as an ordered vector prevents a multi-result message
                // from being collapsed onto the last call id.
                for (tool_call_id, content) in tool_results {
                    payload.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": Self::strip_cache_sentinel(&content),
                    }));
                }
            }
        }

        Ok(payload)
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

    fn map_usage(data: &Value) -> LLMResult<Option<TokenUsage>> {
        let cached_tokens = super::bounded_usage_counter(
            "openai_chat",
            "cached_tokens",
            data.get("prompt_tokens_details")
                .and_then(|details| details.get("cached_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| data.get("cached_tokens").and_then(Value::as_u64)),
        )?;
        let cache_creation_tokens = super::bounded_usage_counter(
            "openai_chat",
            "cache_creation_tokens",
            data.get("prompt_tokens_details")
                .and_then(|details| details.get("cache_write_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| data.get("cache_write_tokens").and_then(Value::as_u64))
                .or_else(|| {
                    data.get("prompt_tokens_details")
                        .and_then(|details| details.get("cache_creation_tokens"))
                        .and_then(Value::as_u64)
                })
                .or_else(|| data.get("cache_creation_tokens").and_then(Value::as_u64)),
        )?;

        Ok(Some(TokenUsage {
            prompt_tokens: super::bounded_usage_counter(
                "openai_chat",
                "prompt_tokens",
                data.get("prompt_tokens").and_then(Value::as_u64),
            )?,
            completion_tokens: super::bounded_usage_counter(
                "openai_chat",
                "completion_tokens",
                data.get("completion_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: super::bounded_usage_counter(
                "openai_chat",
                "total_tokens",
                data.get("total_tokens").and_then(Value::as_u64),
            )?,
            reasoning_tokens: super::bounded_usage_counter(
                "openai_chat",
                "reasoning_tokens",
                data.get("reasoning_tokens").and_then(Value::as_u64),
            )?,
            cached_tokens,
            cache_creation_tokens,
        }))
    }

    fn map_tool_calls(choice: &Value) -> LLMResult<Vec<LLMToolCall>> {
        let mut calls = Vec::new();
        if let Some(tool_calls) = choice
            .get("message")
            .and_then(|m| m.get("tool_calls"))
            .and_then(Value::as_array)
        {
            for entry in tool_calls {
                if let Some(function) = entry.get("function") {
                    let id = entry
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

                    calls.push(LLMToolCall {
                        id,
                        name,
                        arguments: arguments_value,
                    });
                }
            }
        }
        Ok(calls)
    }
}

impl OpenAIChatProvider {
    fn reasoning_effort_for_request(
        model: &str,
        reasoning: Option<&ReasoningConfig>,
    ) -> Option<String> {
        if let Some(reasoning) = reasoning {
            if reasoning.is_disabled() {
                return openai_model_supports_reasoning_none(model).then(|| "none".to_string());
            }
            return reasoning.effort.clone();
        }
        if openai_model_supports_reasoning_none(model) {
            Some("none".to_string())
        } else {
            None
        }
    }

    /// Build the common request body used by both `invoke` and `invoke_stream`.
    fn build_request_body(request: &LLMRequest) -> LLMResult<Map<String, Value>> {
        let cache_plan = openai_prompt_cache_plan(request, &request.model);
        let messages = Self::map_messages(&request.messages, cache_plan.explicit_breakpoint)?;
        let reasoning_effort =
            Self::reasoning_effort_for_request(&request.model, request.reasoning.as_ref());
        let has_reasoning_config = request.reasoning.is_some() || reasoning_effort.is_some();

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));

        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools)),
            );
            let caller_override = request.extra_value().and_then(|e| e.get("tool_choice"));
            let tool_choice = match caller_override {
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                    Value::String("required".to_string())
                },
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("auto") => {
                    Value::String("auto".to_string())
                },
                Some(v @ Value::String(_)) => clone_json_value_iteratively(v),
                _ => Value::String("required".to_string()),
            };
            body.insert("tool_choice".to_string(), tool_choice);
        }

        if let Some(format) = request.response_format_value() {
            let response_format = match format {
                LLMResponseFormat::Text => None,
                LLMResponseFormat::JsonObject => Some(json!({ "type": "json_object" })),
                LLMResponseFormat::JsonSchema { schema } => Some(json!({
                    "type": "json_schema",
                    "json_schema": {
                        "name": "response",
                        "schema": clone_json_value_iteratively(schema),
                    }
                })),
            };

            if let Some(format_value) = response_format {
                body.insert("response_format".to_string(), format_value);
            }
        }

        let basename = super::openai_model_basename(&request.model);
        // GPT-5.1+, GPT-6 and the o-series always reason and reject sampling
        // controls. The original GPT-5 chat ids predate that split and still
        // accept them, so they are carved back out — a family-wide "major >= 5"
        // test drops `temperature` for a model that takes it.
        let skip_sampling = has_reasoning_config
            || (super::openai_gpt_family_from_5(&request.model)
                && !super::openai_legacy_gpt5_chat_id(&request.model))
            || basename.starts_with("o1")
            || basename.starts_with("o3");
        if !skip_sampling {
            if let Some(temp) = request.temperature {
                body.insert("temperature".to_string(), Value::from(temp));
            }

            if let Some(top_p) = request.top_p {
                body.insert("top_p".to_string(), Value::from(top_p));
            }
        }

        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_completion_tokens".to_string(), Value::from(max_tokens));
        }

        if let Some(effort) = reasoning_effort {
            body.insert("reasoning_effort".to_string(), Value::String(effort));
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                // Magician/magicllm internal routing keys must not reach
                // the OpenAI Chat Completions API — strict APIs reject
                // unknown parameters.
                if matches!(
                    key.as_str(),
                    "tool_choice"
                            | "use_chat"
                            | "use_responses"
                            | "openai_api_mode"
                            | "gemini_api_mode"
                            | "openai_previous_response_id"
                            | "openai_responses_disable_chaining"
                            | "api_version"
                            | "verbosity"
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
                            // Validated and model-gated below; older models
                            // reject these GPT-5.6+ request fields.
                            | "prompt_cache_key"
                            | "prompt_cache_options"
                            // All `reasoning_*` keys belong on the typed
                            // `ReasoningConfig` struct — Chat Completions
                            // accepts only a flat top-level `reasoning_effort`
                            // and 400s on unknown params, so strip them
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

        if let Some(prompt_cache_key) = cache_plan.prompt_cache_key {
            body.insert(
                "prompt_cache_key".to_string(),
                Value::String(prompt_cache_key),
            );
        }
        if let Some(prompt_cache_options) = cache_plan.prompt_cache_options {
            body.insert("prompt_cache_options".to_string(), prompt_cache_options);
        }

        // Protected app calls are admitted only under an explicit
        // no-provider-storage posture. Apply this after caller/profile extras
        // so metadata cannot silently re-enable provider-side response
        // retention at the physical adapter boundary.
        if request.metadata.single_physical_attempt {
            body.insert("store".to_string(), Value::Bool(false));
        }

        Ok(body)
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

    fn should_attempt_max_tokens_retry(
        request: &LLMRequest,
        response: &LLMResponse,
        retry_attempt: u64,
    ) -> bool {
        !request.metadata.single_physical_attempt
            && retry_attempt == 0
            && Self::should_retry_max_tokens(response)
    }

    fn is_excessive_max_tokens_error(error: &LLMError) -> bool {
        let lower = error.to_string().to_ascii_lowercase();
        (lower.contains("max_completion_tokens")
            || lower.contains("max_output_tokens")
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

    /// Streaming implementation for OpenAI Chat Completions.
    ///
    /// Sends the request with `stream: true`, parses SSE chunks, sends deltas
    /// via the provided channel, and returns the aggregated response.
    async fn invoke_stream_impl(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for OpenAI chat provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();

        info!(
            provider = "openai_chat",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing OpenAI Chat streaming completion"
        );

        if request.modality != LLMModality::Text {
            return Err(LLMError::UnsupportedCapability(format!(
                "OpenAI Chat provider supports text modality only (requested {:?})",
                request.modality
            )));
        }

        if request.media.is_some() || request.input_media.is_some() {
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Chat provider does not support inline media; use Responses API provider"
                    .to_string(),
            ));
        }

        let mut body = Self::build_request_body(&request)?;
        body.insert("stream".to_string(), Value::Bool(true));

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

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
        if status.is_client_error() || status.is_server_error() {
            let raw_body = read_bounded_response_text(response).await?;
            let payload = parse_provider_json_or_string(&raw_body)?;
            if let Some(error) = payload.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            }
            return Err(LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message: format!("HTTP status {}", status),
            });
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut utf8_carry = Vec::with_capacity(4);
        let mut body_admission = SseBodyAdmission::new();
        let mut aggregated_text = String::new();
        let mut tool_call_deltas: HashMap<u32, ChatToolCallDelta> = HashMap::new();
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

                // Extract usage from the final chunk if present
                if let Some(usage) = value.get("usage") {
                    usage_value = Some(usage.clone());
                }

                if let Some(choices) = value.get("choices").and_then(Value::as_array) {
                    if let Some(choice) = choices.first() {
                        // Check finish_reason
                        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                            finish_reason = Some(reason.to_string());
                        }

                        if let Some(delta) = choice.get("delta") {
                            // Handle text content delta
                            if let Some(content) = delta.get("content").and_then(Value::as_str) {
                                aggregated_text.push_str(content);
                                let _ = tx.send(StreamDelta::Token(content.to_string())).await;
                            }

                            // Handle tool_calls deltas
                            if let Some(tool_calls) =
                                delta.get("tool_calls").and_then(Value::as_array)
                            {
                                for tc in tool_calls {
                                    let index =
                                        tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                                    let entry = tool_call_deltas
                                        .entry(index)
                                        .or_insert_with(ChatToolCallDelta::default);

                                    if let Some(id) = tc.get("id").and_then(Value::as_str) {
                                        entry.id = Some(id.to_string());
                                    }

                                    if let Some(function) = tc.get("function") {
                                        if let Some(name) =
                                            function.get("name").and_then(Value::as_str)
                                        {
                                            entry.name = Some(name.to_string());
                                        }
                                        if let Some(args) =
                                            function.get("arguments").and_then(Value::as_str)
                                        {
                                            append_tool_argument_fragment(
                                                &mut entry.arguments,
                                                args,
                                            )?;

                                            // Send tool call delta
                                            let _ = tx
                                                .send(StreamDelta::ToolCallDelta {
                                                    id: entry.id.clone().unwrap_or_default(),
                                                    name: entry.name.clone(),
                                                    arguments_chunk: args.to_string(),
                                                })
                                                .await;
                                        }
                                    }
                                }
                            }
                        }
                    }
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

        // Build the aggregated response
        let text = if aggregated_text.is_empty() {
            None
        } else {
            Some(aggregated_text.clone())
        };

        let messages = text.as_ref().map(|content| {
            vec![LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: content.clone(),
                }],
            }]
        });

        let mut tool_calls = Vec::new();
        let mut sorted_indices: Vec<u32> = tool_call_deltas.keys().copied().collect();
        sorted_indices.sort();
        for index in sorted_indices {
            if let Some(delta) = tool_call_deltas.remove(&index) {
                let id = delta.id.unwrap_or_default();
                let name = delta.name.unwrap_or_default();
                let arguments = parse_tool_argument_json_or_string(&delta.arguments)?;
                tool_calls.push(LLMToolCall {
                    id,
                    name,
                    arguments,
                });
            }
        }

        let usage = usage_value
            .as_ref()
            .map(Self::map_usage)
            .transpose()?
            .flatten();

        let aggregated_response = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: None,
            response_id: None,
            messages: Arc::new(messages.unwrap_or_default()),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason,
            raw_response: None,
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        info!(
            provider = "openai_chat",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = aggregated_response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = aggregated_response.tool_calls.len(),
            "OpenAI Chat streaming completion succeeded"
        );

        let _ = tx.send(StreamDelta::Done(aggregated_response)).await;
        Ok(())
    }
}

/// Accumulator for a single tool call across streaming deltas.
#[derive(Default)]
struct ChatToolCallDelta {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[async_trait]
impl LLMProvider for OpenAIChatProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::OpenAI
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_unsupported("openai_chat", &request)?;
        self.invoke_stream_impl(request, tx).await
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        crate::server_web_search::reject_unsupported("openai_chat", &request)?;
        let protected_request = request.metadata.single_physical_attempt;
        let retry_attempt = Self::max_tokens_retry_attempt(&request);
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for OpenAI chat provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();

        info!(
            provider = "openai_chat",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "issuing OpenAI Chat completion"
        );

        if request.modality != LLMModality::Text {
            debug!(
                provider = "openai_chat",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                modality = ?request.modality,
                "rejecting request due to unsupported modality"
            );
            return Err(LLMError::UnsupportedCapability(format!(
                "OpenAI Chat provider Phase 1 supports text modality only (requested {:?})",
                request.modality
            )));
        }

        if request.media.is_some() || request.input_media.is_some() {
            debug!(
                provider = "openai_chat",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "rejecting request because inline media is not supported"
            );
            return Err(LLMError::UnsupportedCapability(
                "OpenAI Chat provider Phase 1 does not support inline media; use Responses API provider"
                    .to_string(),
            ));
        }

        let body = Self::build_request_body(&request)?;

        debug!(
            provider = "openai_chat",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            has_reasoning = request.reasoning.is_some(),
            has_response_format = request.response_format.is_some(),
            has_extra = request.extra.is_some(),
            "constructed OpenAI Chat request body"
        );

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

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
                        provider = "openai_chat",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "OpenAI Chat API rejected protected request"
                    );
                    return Err(LLMError::Provider {
                        provider: self.provider_kind().to_string(),
                        message: "protected provider request failed".to_owned(),
                    });
                }
                error!(
                    provider = "openai_chat",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "OpenAI Chat API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                error!(
                    provider = "openai_chat",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "OpenAI Chat API returned unexpected error payload"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: format!("HTTP status {}", status),
                });
            }
        }

        let choices = payload
            .get("choices")
            .and_then(Value::as_array)
            .ok_or_else(|| LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message: "missing choices array in OpenAI response".to_string(),
            })?;

        let first_choice = choices.first().ok_or_else(|| LLMError::Provider {
            provider: self.provider_kind().to_string(),
            message: "empty choices array in OpenAI response".to_string(),
        })?;

        let text = first_choice
            .get("message")
            .and_then(|msg| msg.get("content"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let messages = text.as_ref().map(|content| {
            vec![LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text {
                    text: content.clone(),
                }],
            }]
        });

        let response = LLMResponse {
            text: text.clone().map(Arc::<str>::from),
            reasoning_text: first_choice
                .get("message")
                .and_then(|msg| msg.get("reasoning_content"))
                .and_then(Value::as_str)
                .map(Arc::<str>::from),
            // Chat Completions doesn't expose a chainable id; chaining
            // (`previous_response_id`) is a Responses-API-only feature.
            response_id: None,
            messages: Arc::new(messages.unwrap_or_default()),
            tool_calls: Arc::new(Self::map_tool_calls(first_choice)?),
            tool_results: Arc::new(Vec::new()),
            usage: payload
                .get("usage")
                .map(Self::map_usage)
                .transpose()?
                .flatten(),
            finish_reason: first_choice
                .get("finish_reason")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "openai_chat",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by OpenAI Chat"
            );
        }

        info!(
            provider = "openai_chat",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "OpenAI Chat completion succeeded"
        );

        if Self::should_attempt_max_tokens_retry(&request, &response, retry_attempt) {
            let Some(base_max_output_tokens) =
                Self::retry_base_max_output_tokens(&request, &response)
            else {
                return Ok(response);
            };
            let retry_max_output_tokens = Self::retry_max_output_tokens(base_max_output_tokens);
            if retry_max_output_tokens > base_max_output_tokens {
                warn!(
                    provider = "openai_chat",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    base_max_output_tokens,
                    retry_max_output_tokens,
                    tool_call_count = response.tool_calls.len(),
                    "OpenAI Chat response hit length; retrying with higher max_completion_tokens"
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
                                provider = "openai_chat",
                                model = %model,
                                operation = %operation,
                                trace_id = trace_id.as_deref().unwrap_or(""),
                                retry_max_output_tokens,
                                backed_off_max_output_tokens,
                                error = %error,
                                "OpenAI Chat retry rejected larger max_completion_tokens; retrying with backed-off token budget"
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn server_web_search_fails_closed_on_chat_transport() {
        let provider = OpenAIChatProvider::new("test-key");
        let mut request = LLMRequest::default();
        request.model = "gpt-5.6-luna".to_string();
        request.set_extra(json!({"server_web_search": true}));
        let error = provider
            .invoke(request)
            .await
            .expect_err("chat transport must reject the flag before any HTTP");
        assert!(matches!(error, LLMError::UnsupportedCapability(_)));
    }

    #[test]
    fn protected_single_physical_attempt_never_enters_chat_max_token_retry() {
        let response = LLMResponse {
            finish_reason: Some("length".to_owned()),
            ..Default::default()
        };
        let ordinary = LLMRequest::default();
        assert!(OpenAIChatProvider::should_attempt_max_tokens_retry(
            &ordinary, &response, 0,
        ));

        let mut protected = ordinary;
        protected.metadata.single_physical_attempt = true;
        assert!(!OpenAIChatProvider::should_attempt_max_tokens_retry(
            &protected, &response, 0,
        ));
    }

    #[test]
    fn map_messages_preserves_multiple_projected_tool_results_and_call_ids() {
        let first = json!({
            "schema_version": 1,
            "outcome": { "status": "succeeded" },
            "data": { "records": [{ "name": "Ada" }] }
        });
        let second = json!({
            "schema_version": 1,
            "outcome": { "status": "partial" },
            "data": { "records": [{ "name": "Grace" }] }
        });
        let messages = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![
                    ContentBlock::ToolCall {
                        id: "call-a".into(),
                        name: "memory_search".into(),
                        arguments: json!({ "query": "Ada" }),
                    },
                    ContentBlock::ToolCall {
                        id: "call-b".into(),
                        name: "memory_search".into(),
                        arguments: json!({ "query": "Grace" }),
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
        ];

        let mapped = OpenAIChatProvider::map_messages(&messages, false).expect("map messages");

        assert_eq!(mapped.len(), 3);
        assert_eq!(mapped[0]["tool_calls"][0]["id"], "call-a");
        assert_eq!(mapped[0]["tool_calls"][1]["id"], "call-b");
        assert_eq!(mapped[1]["role"], "tool");
        assert_eq!(mapped[1]["tool_call_id"], "call-a");
        assert_eq!(mapped[2]["role"], "tool");
        assert_eq!(mapped[2]["tool_call_id"], "call-b");
        let decoded_first: Value =
            serde_json::from_str(mapped[1]["content"].as_str().expect("first JSON string"))
                .expect("first projected result is serialized exactly once");
        let decoded_second: Value =
            serde_json::from_str(mapped[2]["content"].as_str().expect("second JSON string"))
                .expect("second projected result is serialized exactly once");
        assert_eq!(decoded_first, first);
        assert_eq!(decoded_second, second);
    }

    #[test]
    fn map_messages_rejects_unpaired_text_mixed_with_tool_results() {
        let err = OpenAIChatProvider::map_messages(
            &[LLMMessage {
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
            }],
            false,
        )
        .expect_err("mixed provider message must fail loudly");

        assert!(matches!(err, LLMError::UnsupportedCapability(_)));
    }

    #[test]
    fn legacy_json_text_tool_result_is_not_json_stringified_again() {
        let legacy = r#"{"status":"ok","value":"exact"}"#;
        let mapped = OpenAIChatProvider::map_messages(
            &[LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "legacy-call".into(),
                    content: Value::String(legacy.to_string()),
                }],
            }],
            false,
        )
        .expect("map legacy tool result");

        assert_eq!(mapped[0]["tool_call_id"], "legacy-call");
        assert_eq!(mapped[0]["content"], legacy);
        let decoded: Value =
            serde_json::from_str(mapped[0]["content"].as_str().expect("legacy JSON text"))
                .expect("legacy payload remains single-encoded JSON");
        assert_eq!(decoded, json!({ "status": "ok", "value": "exact" }));
    }

    #[test]
    fn map_tools_produces_openai_function_format() {
        let tools = vec![LLMToolSpec {
            name: "my_fn".to_string(),
            description: "Does something".to_string(),
            parameters: json!({ "type": "object", "properties": {} }),
        }];
        let mapped = OpenAIChatProvider::map_tools(&tools);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0]["type"], "function");
        assert_eq!(mapped[0]["function"]["name"], "my_fn");
        assert_eq!(mapped[0]["function"]["description"], "Does something");
    }

    #[test]
    fn tool_choice_defaults_to_required() {
        // Verifies that the tool_choice logic emits "required" when no override
        // is present (agentic execution default).
        let caller_override: Option<&Value> = None;
        let tool_choice = match caller_override {
            Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                Value::String("required".to_string())
            },
            Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("auto") => {
                Value::String("auto".to_string())
            },
            Some(v @ Value::String(_)) => v.clone(),
            _ => Value::String("required".to_string()),
        };
        assert_eq!(tool_choice, Value::String("required".to_string()));
    }

    #[test]
    fn tool_choice_translates_anthropic_any_to_required() {
        let extra = json!({ "tool_choice": { "type": "any" } });
        let caller_override = extra.get("tool_choice");
        let tool_choice = match caller_override {
            Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                Value::String("required".to_string())
            },
            Some(v @ Value::String(_)) => v.clone(),
            _ => Value::String("required".to_string()),
        };
        assert_eq!(tool_choice, Value::String("required".to_string()));
    }

    #[test]
    fn tool_choice_respects_openai_string_override() {
        let extra = json!({ "tool_choice": "auto" });
        let caller_override = extra.get("tool_choice");
        let tool_choice = match caller_override {
            Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                Value::String("required".to_string())
            },
            Some(v @ Value::String(_)) => v.clone(),
            _ => Value::String("required".to_string()),
        };
        assert_eq!(tool_choice, Value::String("auto".to_string()));
    }

    #[test]
    fn gpt_5_6_chat_request_emits_key_options_and_explicit_text_breakpoint() {
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user(format!(
                "stable prefix\n{}\nvolatile turn",
                crate::types::CACHE_BREAKPOINT_SENTINEL
            ))]
            .into(),
            metadata: crate::types::RequestMetadata {
                operation: "agentic_decision".to_string(),
                ..Default::default()
            },
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert!(body["prompt_cache_key"]
            .as_str()
            .expect("cache key")
            .starts_with("magician:v1:"));
        assert_eq!(body["prompt_cache_options"]["mode"], "implicit");
        assert_eq!(
            body["messages"][0]["content"][0]["prompt_cache_breakpoint"]["mode"],
            "explicit"
        );
        assert_eq!(body["messages"][0]["content"][0]["text"], "stable prefix");
        assert_eq!(body["messages"][0]["content"][1]["text"], "volatile turn");
    }

    #[test]
    fn older_or_disabled_chat_requests_strip_marker_without_cache_fields() {
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
            let body = OpenAIChatProvider::build_request_body(&request).expect("request body");
            assert!(body.get("prompt_cache_key").is_none());
            assert!(body.get("prompt_cache_options").is_none());
            assert_eq!(
                body["messages"][0]["content"],
                "stable prefix\nvolatile turn"
            );
        }
    }

    #[test]
    fn protected_chat_request_forces_provider_storage_off() {
        let mut request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("protected")].into(),
            ..Default::default()
        };
        request.metadata.single_physical_attempt = true;
        request.set_extra(json!({ "store": true }));

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert_eq!(body.get("store"), Some(&Value::Bool(false)));
        assert!(body.get("prompt_cache_key").is_none());
        assert!(body.get("prompt_cache_options").is_none());
    }

    #[test]
    fn map_usage_reads_gpt_5_6_cache_write_tokens() {
        let usage = OpenAIChatProvider::map_usage(&json!({
            "prompt_tokens": 1200,
            "prompt_tokens_details": {
                "cached_tokens": 800,
                "cache_write_tokens": 50
            },
            "completion_tokens": 300,
            "total_tokens": 1500
        }))
        .expect("valid usage")
        .expect("usage");

        assert_eq!(usage.cached_tokens, Some(800));
        assert_eq!(usage.cache_creation_tokens, Some(50));
    }

    #[test]
    fn map_usage_rejects_wide_provider_counters() {
        let error = OpenAIChatProvider::map_usage(&json!({
            "prompt_tokens": u64::from(u32::MAX) + 1
        }))
        .expect_err("wide usage must fail closed");

        assert!(error.to_string().contains("prompt_tokens"));
    }

    #[test]
    fn build_request_body_omits_none_and_sampling_for_gpt_6_astra() {
        let request = LLMRequest {
            model: "gpt-6-astra".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            temperature: Some(0.7),
            top_p: Some(0.9),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert!(body.get("reasoning_effort").is_none());
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn build_request_body_defaults_reasoning_effort_none_for_gpt_5_1_plus() {
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            temperature: Some(0.7),
            top_p: Some(0.9),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert_eq!(body["reasoning_effort"], "none");
        assert!(body.get("temperature").is_none());
        assert!(body.get("top_p").is_none());
    }

    #[test]
    fn build_request_body_preserves_explicit_reasoning_effort() {
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            reasoning: Some(ReasoningConfig {
                effort: Some("high".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert_eq!(body["reasoning_effort"], "high");
    }

    #[test]
    fn build_request_body_normalizes_disabled_reasoning_to_none() {
        let request = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            reasoning: Some(ReasoningConfig {
                effort: Some("disabled".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert_eq!(body["reasoning_effort"], "none");
    }

    #[test]
    fn build_request_body_does_not_send_none_to_legacy_gpt5_ids() {
        let request = LLMRequest {
            model: "gpt-5".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            temperature: Some(0.7),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert!(body.get("reasoning_effort").is_none());
        let temperature = body["temperature"].as_f64().unwrap();
        assert!(
            (temperature - 0.7).abs() < 1e-6,
            "temperature should still be forwarded for legacy GPT-5 chat requests"
        );
    }

    #[test]
    fn build_request_body_omits_disabled_reasoning_for_legacy_gpt5_ids() {
        let request = LLMRequest {
            model: "gpt-5".to_string(),
            messages: vec![LLMMessage::user("hello")].into(),
            reasoning: Some(ReasoningConfig {
                effort: Some("none".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let body = OpenAIChatProvider::build_request_body(&request).expect("request body");

        assert!(body.get("reasoning_effort").is_none());
    }
}

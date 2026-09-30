use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tracing::{debug, error, info, warn};

use super::{bounded_usage_counter, default_http_client, read_bounded_response_text};
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    context_reuse::ContextReuseStrategy,
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        clone_admitted_tool_argument, clone_json_value_iteratively, parse_provider_json_value,
        ContentBlock, LLMMessage, LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall,
        LLMToolSpec, MessageRole, PromptCacheConfig, ReasoningConfig, TokenUsage,
    },
};

const DEFAULT_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const MAX_TOKENS_RETRY_CAP: u32 = 32_000;
const MAX_TOKENS_RETRY_ATTEMPT_KEY: &str = "max_tokens_retry_attempt";
const GEMINI_TOOL_CALL_ID_SEPARATOR: &str = "::";

/// Provider implementation for the Google Gemini API.
pub struct GeminiProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl GeminiProvider {
    fn interactions_provider_storage_enabled(request: &LLMRequest) -> bool {
        !request.metadata.single_physical_attempt
    }
    /// Creates a provider using the default Gemini endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL)
    }

    /// Creates a provider with a custom base URL.
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

    fn detect_capability(model: &str) -> LLMCapability {
        let mut modalities = vec![LLMModality::Text];

        // Gemini 2.0+, 2.5, and 3.x models support vision
        if model.contains("gemini-2.0")
            || model.contains("gemini-2.5")
            || model.contains("gemini-3")
            || model.contains("gemini-pro-vision")
        {
            modalities.push(LLMModality::Vision);
        }

        // Gemini 2.5+ thinking/pro models and all 3.x models support reasoning
        let reasoning = if model.contains("2.5-flash-thinking")
            || model.contains("2.5-pro")
            || model.contains("gemini-3")
        {
            LLMReasoning::Standard
        } else {
            LLMReasoning::None
        };

        LLMCapability {
            modalities,
            reasoning,
            tool_calling: true,
            json_mode: true,
            streaming: false, // Phase 1: non-streaming only
            computer_use: false,
            // Overridden to true by `capabilities`; false at the detect layer
            // keeps the base declaration honest.
            web_search: false,
        }
    }

    fn accepts_sampling_parameters(model: &str) -> bool {
        let model = model.trim().to_ascii_lowercase();
        !model.starts_with("gemini-3.5-flash-lite") && !model.starts_with("gemini-3.6-flash")
    }

    fn prompt_cached_content(request: &LLMRequest) -> Option<String> {
        if request.metadata.single_physical_attempt {
            return None;
        }
        if request
            .extra
            .as_ref()
            .and_then(|extra| extra.get("cachedContent"))
            .is_some()
        {
            return None;
        }

        match request.prompt_cache.as_ref() {
            Some(PromptCacheConfig::Enabled {
                cached_content: Some(name),
                ..
            }) => Some(name.clone()),
            _ => None,
        }
    }

    fn should_forward_generate_content_extra(key: &str, protected_request: bool) -> bool {
        !Self::is_config_only_field(key) && !(protected_request && key == "cachedContent")
    }

    /// Strip the Magician cache-breakpoint sentinel from a text payload,
    /// reconnecting the halves with a single `\n` when both are non-empty.
    ///
    /// Gemini supports implicit `cachedContent` references but has no inline
    /// breakpoint concept on `contents[]`, so if the rendered user prompt
    /// carries our internal marker we must strip it before sending. Matches
    /// the reconnection rule used by the Anthropic and OpenRouter providers.
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

    /// Convert `LLMMessage[]` into Gemini `contents[]` and an optional `systemInstruction`.
    fn map_messages(messages: &[LLMMessage]) -> LLMResult<(Option<Value>, Vec<Value>)> {
        let mut system_parts: Vec<String> = Vec::new();
        let mut contents: Vec<Value> = Vec::new();

        for message in messages {
            match message.role {
                MessageRole::System => {
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                system_parts.push(Self::strip_cache_sentinel(text));
                            },
                            ContentBlock::Json { value } => {
                                system_parts.push(value.to_string());
                            },
                            _ => {
                                return Err(LLMError::UnsupportedCapability(
                                    "Gemini system instruction only supports text content"
                                        .to_string(),
                                ));
                            },
                        }
                    }
                },
                MessageRole::User => {
                    let parts = Self::map_content_parts(&message.content)?;
                    contents.push(json!({ "role": "user", "parts": parts }));
                },
                MessageRole::Assistant => {
                    let parts = Self::map_content_parts(&message.content)?;
                    contents.push(json!({ "role": "model", "parts": parts }));
                },
                MessageRole::Tool => {
                    let parts = Self::map_tool_response_parts(&message.content)?;
                    contents.push(json!({ "role": "user", "parts": parts }));
                },
            }
        }

        let system_instruction = if system_parts.is_empty() {
            None
        } else {
            let combined = system_parts.join("\n");
            Some(json!({
                "parts": [{ "text": combined }]
            }))
        };

        Ok((system_instruction, contents))
    }

    /// Map content blocks to Gemini `parts[]` for user/model roles.
    fn map_content_parts(blocks: &[ContentBlock]) -> LLMResult<Vec<Value>> {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => {
                    parts.push(json!({ "text": Self::strip_cache_sentinel(text) }));
                },
                ContentBlock::Json { value } => {
                    if let Some(raw_parts) = value
                        .get("_magicllm_gemini_raw_parts")
                        .and_then(Value::as_array)
                    {
                        parts.extend(raw_parts.iter().cloned());
                    } else {
                        parts.push(json!({ "text": value.to_string() }));
                    }
                },
                ContentBlock::ToolCall {
                    id,
                    name,
                    arguments,
                } => {
                    parts.push(json!({
                        "functionCall": Self::gemini_function_call_payload(id, name, arguments),
                    }));
                },
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    let name = extract_function_name_from_id(tool_call_id);
                    parts.push(json!({
                        "functionResponse": Self::gemini_function_response_payload(
                            tool_call_id,
                            name,
                            json!({ "result": content }),
                            Vec::new(),
                        ),
                    }));
                },
                ContentBlock::Image {
                    data, media_type, ..
                } => {
                    parts.push(json!({
                        "inlineData": {
                            "mimeType": media_type,
                            "data": BASE64.encode(data),
                        }
                    }));
                },
                ContentBlock::ImageUrl { url, .. } => parts.push(json!({
                    "fileData": {
                        "fileUri": url,
                    }
                })),
            }
        }
        Ok(parts)
    }

    /// Map tool-role message content blocks to Gemini `functionResponse` parts.
    fn map_tool_response_parts(blocks: &[ContentBlock]) -> LLMResult<Vec<Value>> {
        let mut parts = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::ToolResult {
                    tool_call_id,
                    content,
                } => {
                    let name = extract_function_name_from_id(tool_call_id);
                    if let Some(blocks) = content
                        .get("_magicllm_rich_tool_result")
                        .and_then(Value::as_bool)
                        .filter(|flag| *flag)
                        .and_then(|_| content.get("blocks"))
                        .and_then(Value::as_array)
                    {
                        let mut text_sections = Vec::new();
                        let mut image_parts = Vec::new();
                        for block in blocks {
                            match block.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    if let Some(text) = block.get("text").and_then(Value::as_str) {
                                        if !text.trim().is_empty() {
                                            text_sections.push(text.to_string());
                                        }
                                    }
                                },
                                Some("image") => {
                                    let Some(media_type) =
                                        block.get("media_type").and_then(Value::as_str)
                                    else {
                                        continue;
                                    };
                                    let Some(data_base64) =
                                        block.get("data_base64").and_then(Value::as_str)
                                    else {
                                        continue;
                                    };
                                    image_parts.push(json!({
                                        "inlineData": {
                                            "mimeType": media_type,
                                            "data": data_base64,
                                        }
                                    }));
                                },
                                _ => {},
                            }
                        }

                        let result_value = if text_sections.is_empty() {
                            Value::Object(Map::new())
                        } else {
                            json!({ "text": text_sections.join("\n\n") })
                        };
                        parts.push(json!({
                            "functionResponse": Self::gemini_function_response_payload(
                                tool_call_id,
                                name,
                                json!({ "result": result_value }),
                                image_parts,
                            ),
                        }));
                    } else {
                        parts.push(json!({
                            "functionResponse": Self::gemini_function_response_payload(
                                tool_call_id,
                                name,
                                json!({ "result": content }),
                                Vec::new(),
                            ),
                        }));
                    }
                },
                ContentBlock::Text { text } => {
                    parts.push(json!({
                        "text": Self::strip_cache_sentinel(text),
                    }));
                },
                _ => {
                    return Err(LLMError::UnsupportedCapability(
                        "Unexpected content block in tool message for Gemini".to_string(),
                    ));
                },
            }
        }
        Ok(parts)
    }

    /// Convert `LLMToolSpec[]` to Gemini `tools` format with `functionDeclarations`.
    ///
    /// Gemini's `function_declarations[].parameters` accepts a stripped subset
    /// of JSON-Schema. Fields the canonical OpenAI / Anthropic tool schema
    /// carries (`additionalProperties`, `$schema`, `$defs`, `oneOf`, `anyOf`,
    /// `allOf`, `not`, `if`/`then`/`else`, `definitions`) make Gemini's
    /// validator reject the entire `tools` array with HTTP 400. We sanitize
    /// each parameters object before sending so cross-provider tool schemas
    /// keep working without per-tool YAML rewrites. See the in-repo log
    /// reference for the canonical failure mode this fix closes.
    fn map_tools(tools: &[LLMToolSpec]) -> Vec<Value> {
        let declarations: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let mut parameters = clone_json_value_iteratively(&tool.parameters);
                sanitize_gemini_schema(&mut parameters);
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                })
            })
            .collect();

        vec![json!({ "functionDeclarations": declarations })]
    }

    fn gemini_function_call_payload(id: &str, name: &str, arguments: &Value) -> Value {
        let mut payload = Map::new();
        payload.insert("name".to_string(), Value::String(name.to_string()));
        payload.insert("args".to_string(), arguments.clone());
        if let Some(provider_id) = gemini_provider_function_id(id) {
            payload.insert("id".to_string(), Value::String(provider_id.to_string()));
        }
        Value::Object(payload)
    }

    fn gemini_function_response_payload(
        tool_call_id: &str,
        name: String,
        response: Value,
        nested_parts: Vec<Value>,
    ) -> Value {
        let mut payload = Map::new();
        payload.insert("name".to_string(), Value::String(name));
        if let Some(provider_id) = gemini_provider_function_id(tool_call_id) {
            payload.insert("id".to_string(), Value::String(provider_id.to_string()));
        }
        payload.insert("response".to_string(), response);
        if !nested_parts.is_empty() {
            payload.insert("parts".to_string(), Value::Array(nested_parts));
        }
        Value::Object(payload)
    }

    /// Insert the `tools` array, or the googleSearch grounding tool when the
    /// request enables server-side web search. The two are mutually
    /// exclusive (see `server_web_search` module docs); `map` serializes
    /// caller tools in the path's native shape (generateContent vs
    /// Interactions).
    fn attach_tools_or_google_search(
        body: &mut Map<String, Value>,
        request: &LLMRequest,
        map: fn(&[LLMToolSpec]) -> Vec<Value>,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_invalid_flag("gemini", request)?;
        if crate::server_web_search::requested(request) {
            crate::server_web_search::reject_mixed_with_function_tools("gemini", request)?;
            body.insert(
                "tools".to_string(),
                Value::Array(vec![crate::server_web_search::gemini_google_search_tool()]),
            );
            return Ok(());
        }
        if !request.tools.is_empty() {
            body.insert("tools".to_string(), Value::Array(map(&request.tools)));
        }
        Ok(())
    }

    /// Check if a field is config-only metadata that should NOT be sent to the Gemini API.
    fn is_config_only_field(key: &str) -> bool {
        matches!(
            key,
            "cost_per_observation"
                | "fallback_profile"
                | "reasoning_strategy"
                | "reasoning_max_tokens"
                | "reasoning_summary" // OpenAI Responses-only; Gemini's thinkingConfig has no summary mode.
                | "tool_choice"
                | "verbosity"
                | "openai_api_mode"
                | "openai_responses_disable_chaining"
                | "gemini_api_mode"
                | "openai_previous_response_id"
                | "use_chat"
                | "use_responses"
                | "streaming"
                | "max_tokens_retry_attempt"
                | "viewport"
                | "disable_tools"
                | "disable_yutori_builtins"
                | "router_provider_override"
                | "router_profile_override"
                | "router_preserve_model"
                | "server_web_search" // consumed by attach_tools_or_google_search
        )
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
        response.finish_reason.as_deref() == Some("MAX_TOKENS")
    }

    fn is_excessive_max_tokens_error(error: &LLMError) -> bool {
        let lower = error.to_string().to_ascii_lowercase();
        (lower.contains("maxoutputtokens")
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

    /// Translate tool choice from the normalised format to Gemini's `toolConfig`.
    fn map_tool_config(extra: Option<&Value>) -> Value {
        // Accept both string-shaped tool_choice ("auto" / "required" / "none",
        // OpenAI style) and object-shaped ({"type": "auto"}, Anthropic /
        // Magician config style). Without the string-shape branch a profile
        // configured with `tool_choice: "auto"` silently coerces to ANY here
        // (the `tc.get("type")` returns None on a string), which breaks chat
        // semantics by forcing tool-call output every turn.
        let normalize = |raw: &str| -> &'static str {
            match raw {
                "any" | "required" => "ANY",
                "auto" => "AUTO",
                "none" => "NONE",
                _ => "ANY",
            }
        };
        let mode = extra
            .and_then(|e| e.get("tool_choice"))
            .and_then(|tc| match tc {
                Value::String(value) => Some(normalize(value.as_str())),
                _ => tc.get("type").and_then(Value::as_str).map(normalize),
            })
            .unwrap_or("ANY");

        json!({
            "functionCallingConfig": {
                "mode": mode,
            }
        })
    }

    fn map_thinking_config(model: &str, reasoning: Option<&ReasoningConfig>) -> Option<Value> {
        let mut config = Map::new();
        let model_lower = model.to_ascii_lowercase();

        if reasoning
            .map(|reasoning| reasoning.is_disabled())
            .unwrap_or(true)
        {
            return gemini_lowest_thinking_config(&model_lower);
        }

        let reasoning = reasoning?;
        if model_lower.contains("gemini-3") {
            if let Some(level) = reasoning
                .effort
                .as_deref()
                .and_then(|effort| gemini3_thinking_level(&model_lower, effort))
            {
                config.insert(
                    "thinkingLevel".to_string(),
                    Value::String(level.to_string()),
                );
            }
        } else if model_lower.contains("gemini-2.5") {
            if let Some(budget) = reasoning.max_reasoning_tokens {
                config.insert("thinkingBudget".to_string(), Value::from(budget));
            }
        }

        (!config.is_empty()).then_some(Value::Object(config))
    }

    /// Extract tool calls from a Gemini response candidate.
    fn map_tool_calls(candidate: &Value) -> LLMResult<Vec<LLMToolCall>> {
        let mut calls = Vec::new();
        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for (idx, part) in parts.iter().enumerate() {
                if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let args = match fc.get("args") {
                        Some(args) => clone_admitted_tool_argument(args)?,
                        None => Value::Object(Map::new()),
                    };
                    let id = gemini_tool_call_id(&name, idx, fc.get("id").and_then(Value::as_str));
                    calls.push(LLMToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }
        }
        Ok(calls)
    }

    /// Map Gemini `usageMetadata` to normalised `TokenUsage`.
    fn map_usage(data: &Value) -> LLMResult<TokenUsage> {
        Ok(TokenUsage {
            prompt_tokens: bounded_usage_counter(
                "gemini",
                "promptTokenCount",
                data.get("promptTokenCount").and_then(Value::as_u64),
            )?,
            completion_tokens: bounded_usage_counter(
                "gemini",
                "candidatesTokenCount",
                data.get("candidatesTokenCount").and_then(Value::as_u64),
            )?,
            total_tokens: bounded_usage_counter(
                "gemini",
                "totalTokenCount",
                data.get("totalTokenCount").and_then(Value::as_u64),
            )?,
            reasoning_tokens: None,
            cached_tokens: bounded_usage_counter(
                "gemini",
                "cachedContentTokenCount",
                data.get("cachedContentTokenCount").and_then(Value::as_u64),
            )?,
            cache_creation_tokens: None,
        })
    }

    /// Parse the first candidate from a Gemini response into normalised output.
    fn parse_output(
        payload: &Value,
    ) -> LLMResult<(Vec<LLMMessage>, Vec<LLMToolCall>, Option<String>)> {
        let candidate = match payload
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
        {
            Some(c) => c,
            None => return Ok((Vec::new(), Vec::new(), None)),
        };

        let tool_calls = Self::map_tool_calls(candidate)?;

        let mut text_parts = Vec::new();
        let mut content_blocks = Vec::new();

        if let Some(parts) = candidate
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
        {
            for (idx, part) in parts.iter().enumerate() {
                if let Some(text) = part.get("text").and_then(Value::as_str) {
                    text_parts.push(text.to_string());
                    content_blocks.push(ContentBlock::Text {
                        text: text.to_string(),
                    });
                } else if let Some(fc) = part.get("functionCall") {
                    let name = fc
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    let args = match fc.get("args") {
                        Some(args) => clone_admitted_tool_argument(args)?,
                        None => Value::Object(Map::new()),
                    };
                    let id = gemini_tool_call_id(&name, idx, fc.get("id").and_then(Value::as_str));
                    content_blocks.push(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments: args,
                    });
                }
            }
        }

        let messages = if content_blocks.is_empty() {
            Vec::new()
        } else {
            vec![LLMMessage {
                role: MessageRole::Assistant,
                content: content_blocks,
            }]
        };

        let text = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join("\n"))
        };

        Ok((messages, tool_calls, text))
    }

    fn uses_interactions(request: &LLMRequest) -> bool {
        request
            .context_reuse
            .as_ref()
            .is_some_and(|reuse| reuse.strategy == ContextReuseStrategy::ServerContinuation)
            || request
                .extra
                .as_ref()
                .and_then(|extra| extra.get("gemini_api_mode"))
                .and_then(Value::as_str)
                .is_some_and(|mode| mode.eq_ignore_ascii_case("interactions"))
    }

    fn validate_protected_transport(request: &LLMRequest) -> LLMResult<()> {
        if request.metadata.single_physical_attempt && Self::uses_interactions(request) {
            return Err(LLMError::Validation(
                "protected Gemini calls require the stateless generateContent transport".to_owned(),
            ));
        }
        Ok(())
    }

    fn interaction_system_instruction(messages: &[LLMMessage]) -> LLMResult<Option<String>> {
        let mut parts = Vec::new();
        for message in messages
            .iter()
            .filter(|message| message.role == MessageRole::System)
        {
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => parts.push(Self::strip_cache_sentinel(text)),
                    ContentBlock::Json { value } => parts.push(value.to_string()),
                    _ => {
                        return Err(LLMError::UnsupportedCapability(
                            "Gemini Interactions system instruction only supports text content"
                                .to_string(),
                        ));
                    },
                }
            }
        }
        Ok((!parts.is_empty()).then(|| parts.join("\n")))
    }

    fn interaction_content(blocks: &[ContentBlock]) -> LLMResult<Vec<Value>> {
        let mut content = Vec::new();
        for block in blocks {
            match block {
                ContentBlock::Text { text } => content.push(json!({
                    "type": "text",
                    "text": Self::strip_cache_sentinel(text),
                })),
                ContentBlock::Json { value } => content.push(json!({
                    "type": "text",
                    "text": value.to_string(),
                })),
                ContentBlock::Image {
                    data, media_type, ..
                } => content.push(json!({
                    "type": "image",
                    "mime_type": media_type,
                    "data": BASE64.encode(data),
                })),
                ContentBlock::ImageUrl { url, prompt } => {
                    if let Some(prompt) = prompt {
                        content.push(json!({ "type": "text", "text": prompt }));
                    }
                    content.push(json!({ "type": "image", "uri": url }));
                },
                ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. } => {
                    return Err(LLMError::UnsupportedCapability(
                        "Gemini Interactions user content cannot contain tool protocol blocks"
                            .to_string(),
                    ));
                },
            }
        }
        Ok(content)
    }

    fn interaction_function_result(block: &ContentBlock) -> Option<Value> {
        let ContentBlock::ToolResult {
            tool_call_id,
            content,
        } = block
        else {
            return None;
        };
        let call_id = gemini_provider_function_id(tool_call_id).unwrap_or(tool_call_id);
        let name = extract_function_name_from_id(tool_call_id);
        let text = content
            .as_str()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| content.to_string());
        Some(json!({
            "type": "function_result",
            "call_id": call_id,
            "name": name,
            "result": [{ "type": "text", "text": text }],
        }))
    }

    /// Extract only the locally-new suffix after the response represented by
    /// `previous_interaction_id`. Returning `None` deliberately forces a full
    /// bootstrap when the local history was compacted into a shape we cannot
    /// prove corresponds to that server-side interaction.
    fn interaction_delta_input(messages: &[LLMMessage]) -> LLMResult<Option<Vec<Value>>> {
        let Some(last_assistant) = messages
            .iter()
            .rposition(|message| message.role == MessageRole::Assistant)
        else {
            return Ok(None);
        };
        let suffix = &messages[last_assistant + 1..];
        if suffix.is_empty()
            || suffix
                .iter()
                .any(|message| message.role == MessageRole::Assistant)
        {
            return Ok(None);
        }

        let mut input = Vec::new();
        for message in suffix {
            if message.role == MessageRole::System {
                continue;
            }
            let mut ordinary_blocks = Vec::new();
            for block in &message.content {
                if let Some(result) = Self::interaction_function_result(block) {
                    input.push(result);
                } else {
                    ordinary_blocks.push(block.clone());
                }
            }
            if !ordinary_blocks.is_empty() {
                let content = Self::interaction_content(&ordinary_blocks)?;
                if !content.is_empty() {
                    input.push(json!({ "type": "user_input", "content": content }));
                }
            }
        }
        Ok((!input.is_empty()).then_some(input))
    }

    fn interaction_rebootstrap_content(messages: &[LLMMessage]) -> Vec<Value> {
        let mut content = Vec::new();
        for message in messages
            .iter()
            .filter(|message| message.role != MessageRole::System)
        {
            let role = match message.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
                MessageRole::System => continue,
            };
            for block in &message.content {
                let rendered = match block {
                    ContentBlock::Text { text } => Some(Self::strip_cache_sentinel(text)),
                    ContentBlock::Json { value } => Some(value.to_string()),
                    ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    } => Some(format!(
                        "tool_call id={id} name={name} arguments={arguments}"
                    )),
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                    } => Some(format!(
                        "tool_result call_id={tool_call_id} result={content}"
                    )),
                    ContentBlock::Image {
                        data, media_type, ..
                    } => {
                        content.push(json!({
                            "type": "image",
                            "mime_type": media_type,
                            "data": BASE64.encode(data),
                        }));
                        None
                    },
                    ContentBlock::ImageUrl { url, prompt } => {
                        if let Some(prompt) = prompt {
                            content.push(json!({
                                "type": "text",
                                "text": format!("[{role}] {prompt}"),
                            }));
                        }
                        content.push(json!({ "type": "image", "uri": url }));
                        None
                    },
                };
                if let Some(rendered) = rendered.filter(|rendered| !rendered.is_empty()) {
                    content.push(json!({
                        "type": "text",
                        "text": format!("[{role}] {rendered}"),
                    }));
                }
            }
        }
        content
    }

    fn interaction_bootstrap_input(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        let non_system: Vec<&LLMMessage> = messages
            .iter()
            .filter(|message| message.role != MessageRole::System)
            .collect();
        if non_system.len() == 1
            && non_system[0].role == MessageRole::User
            && non_system[0].content.iter().all(|block| {
                !matches!(
                    block,
                    ContentBlock::ToolCall { .. } | ContentBlock::ToolResult { .. }
                )
            })
        {
            return Self::interaction_content(&non_system[0].content);
        }

        // A periodic rebootstrap may contain historical Gemini function calls
        // whose required thought signatures are intentionally not stored in
        // the provider-neutral message type. Replaying them as native protocol
        // steps would be rejected. Flattening them into role-labelled content
        // preserves semantic evidence while starting a fresh valid chain. Keep
        // image blocks as media rather than replacing the current screenshot
        // with a textual placeholder during the periodic rebase.
        Ok(Self::interaction_rebootstrap_content(messages))
    }

    fn interaction_tools(tools: &[LLMToolSpec]) -> Vec<Value> {
        tools
            .iter()
            .map(|tool| {
                let mut parameters = clone_json_value_iteratively(&tool.parameters);
                sanitize_gemini_schema(&mut parameters);
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": parameters,
                })
            })
            .collect()
    }

    fn interaction_generation_config(request: &LLMRequest) -> Option<Value> {
        let mut config = Map::new();
        if Self::accepts_sampling_parameters(&request.model) {
            if let Some(temperature) = request.temperature {
                config.insert("temperature".to_string(), Value::from(temperature));
            }
            if let Some(top_p) = request.top_p {
                config.insert("top_p".to_string(), Value::from(top_p));
            }
        }
        if let Some(max_tokens) = request.max_output_tokens {
            config.insert("max_output_tokens".to_string(), Value::from(max_tokens));
        }
        if request.model.to_ascii_lowercase().contains("gemini-3") {
            let level = request
                .reasoning
                .as_ref()
                .and_then(|reasoning| reasoning.effort.as_deref())
                .and_then(|effort| {
                    gemini3_thinking_level(&request.model.to_ascii_lowercase(), effort)
                })
                .unwrap_or("low");
            config.insert(
                "thinking_level".to_string(),
                Value::String(level.to_string()),
            );
        }
        if !request.tools.is_empty() {
            let tool_choice = request
                .extra
                .as_ref()
                .and_then(|extra| extra.get("tool_choice"))
                .and_then(|choice| match choice {
                    Value::String(value) => Some(value.as_str()),
                    Value::Object(map) => map.get("type").and_then(Value::as_str),
                    _ => None,
                })
                .map(|choice| match choice {
                    "required" => "any",
                    "none" => "none",
                    "auto" => "auto",
                    "any" => "any",
                    _ => "auto",
                })
                .unwrap_or("auto");
            config.insert(
                "tool_choice".to_string(),
                Value::String(tool_choice.to_string()),
            );
        }
        (!config.is_empty()).then_some(Value::Object(config))
    }

    fn interaction_response_format(format: Option<&LLMResponseFormat>) -> Option<Value> {
        match format {
            Some(LLMResponseFormat::JsonObject) => {
                Some(json!({ "type": "text", "mime_type": "application/json" }))
            },
            Some(LLMResponseFormat::JsonSchema { schema }) => Some(json!({
                "type": "text",
                "mime_type": "application/json",
                "schema": clone_json_value_iteratively(schema),
            })),
            Some(LLMResponseFormat::Text) | None => None,
        }
    }

    fn map_interaction_usage(data: &Value) -> LLMResult<TokenUsage> {
        Ok(TokenUsage {
            prompt_tokens: bounded_usage_counter(
                "gemini",
                "total_input_tokens",
                data.get("total_input_tokens").and_then(Value::as_u64),
            )?,
            completion_tokens: bounded_usage_counter(
                "gemini",
                "total_output_tokens",
                data.get("total_output_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: bounded_usage_counter(
                "gemini",
                "total_tokens",
                data.get("total_tokens").and_then(Value::as_u64),
            )?,
            reasoning_tokens: bounded_usage_counter(
                "gemini",
                "total_thought_tokens",
                data.get("total_thought_tokens").and_then(Value::as_u64),
            )?,
            cached_tokens: bounded_usage_counter(
                "gemini",
                "total_cached_tokens",
                data.get("total_cached_tokens").and_then(Value::as_u64),
            )?,
            cache_creation_tokens: None,
        })
    }

    fn parse_interaction_output(
        payload: &Value,
    ) -> LLMResult<(
        Vec<LLMMessage>,
        Vec<LLMToolCall>,
        Option<String>,
        Option<String>,
    )> {
        let mut content_blocks = Vec::new();
        let mut tool_calls = Vec::new();
        let mut text_parts = Vec::new();
        let mut reasoning_parts = Vec::new();
        for (index, step) in payload
            .get("steps")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .enumerate()
        {
            match step.get("type").and_then(Value::as_str) {
                Some("model_output") => {
                    for block in step
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if block.get("type").and_then(Value::as_str) == Some("text") {
                            if let Some(text) = block.get("text").and_then(Value::as_str) {
                                text_parts.push(text.to_string());
                                content_blocks.push(ContentBlock::Text {
                                    text: text.to_string(),
                                });
                            }
                        }
                    }
                },
                Some("function_call") => {
                    let name = step
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string();
                    let provider_id = step.get("id").and_then(Value::as_str);
                    let id = gemini_tool_call_id(&name, index, provider_id);
                    let arguments = match step.get("arguments") {
                        Some(arguments) => clone_admitted_tool_argument(arguments)?,
                        None => Value::Object(Map::new()),
                    };
                    tool_calls.push(LLMToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: clone_json_value_iteratively(&arguments),
                    });
                    content_blocks.push(ContentBlock::ToolCall {
                        id,
                        name,
                        arguments,
                    });
                },
                Some("thought") => {
                    for block in step
                        .get("summary")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            reasoning_parts.push(text.to_string());
                        }
                    }
                },
                _ => {},
            }
        }
        let messages = if content_blocks.is_empty() {
            Vec::new()
        } else {
            vec![LLMMessage {
                role: MessageRole::Assistant,
                content: content_blocks,
            }]
        };
        Ok((
            messages,
            tool_calls,
            (!text_parts.is_empty()).then(|| text_parts.join("\n")),
            (!reasoning_parts.is_empty()).then(|| reasoning_parts.join("\n")),
        ))
    }

    async fn invoke_interactions(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let model = request.model.clone();
        let provider_storage_enabled = Self::interactions_provider_storage_enabled(&request);
        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let requested_continuation = request
            .context_reuse
            .as_ref()
            .and_then(|reuse| reuse.continuation_id.clone());
        let (previous_interaction_id, input) = if let Some(previous_id) = requested_continuation {
            match Self::interaction_delta_input(&request.messages)? {
                Some(input) => (Some(previous_id), input),
                None => (None, Self::interaction_bootstrap_input(&request.messages)?),
            }
        } else {
            (None, Self::interaction_bootstrap_input(&request.messages)?)
        };

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("input".to_string(), Value::Array(input));
        body.insert("store".to_string(), Value::Bool(provider_storage_enabled));
        if let Some(previous_id) = previous_interaction_id {
            body.insert(
                "previous_interaction_id".to_string(),
                Value::String(previous_id),
            );
        }
        if let Some(system) = Self::interaction_system_instruction(&request.messages)? {
            body.insert("system_instruction".to_string(), Value::String(system));
        }
        Self::attach_tools_or_google_search(&mut body, &request, Self::interaction_tools)?;
        if let Some(config) = Self::interaction_generation_config(&request) {
            body.insert("generation_config".to_string(), config);
        }
        if let Some(format) = Self::interaction_response_format(request.response_format_value()) {
            body.insert("response_format".to_string(), format);
        }
        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                if Self::is_config_only_field(&key)
                    || matches!(
                        key.as_str(),
                        "cachedContent" | "systemInstruction" | "toolConfig" | "generationConfig"
                    )
                {
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        let url = format!("{}/interactions", self.base_url.trim_end_matches('/'));
        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);
        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
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
            let message = payload
                .get("error")
                .and_then(|error| error.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            return Err(LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message,
            });
        }
        let interaction_status = payload
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("completed");
        if !matches!(
            interaction_status,
            "completed" | "requires_action" | "incomplete"
        ) {
            return Err(LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message: format!("Gemini interaction ended with status {interaction_status}"),
            });
        }
        let (messages, tool_calls, text, reasoning_text) =
            Self::parse_interaction_output(&payload)?;
        let finish_reason = match interaction_status {
            "incomplete" => Some("MAX_TOKENS".to_string()),
            other => Some(other.to_string()),
        };
        let usage = payload
            .get("usage")
            .map(Self::map_interaction_usage)
            .transpose()?;
        let response_id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        debug!(
            provider = "gemini",
            api_mode = "interactions",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            chained = body.contains_key("previous_interaction_id"),
            "Gemini Interactions call succeeded"
        );
        Ok(LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            response_id,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason,
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        })
    }
}

fn gemini_tool_call_id(name: &str, index: usize, provider_id: Option<&str>) -> String {
    let synthetic = format!("gemini-call-{name}-{index}");
    match provider_id.map(str::trim).filter(|id| !id.is_empty()) {
        Some(provider_id) => format!("{synthetic}{GEMINI_TOOL_CALL_ID_SEPARATOR}{provider_id}"),
        None => synthetic,
    }
}

/// Strip JSON-Schema fields Gemini's `function_declarations[].parameters`
/// validator rejects, and normalise type / format constructs to the
/// shape Gemini accepts. Walks the schema recursively because the
/// unsupported keys can appear at any nesting depth (object properties,
/// array items' schemas, nested object schemas). Modifies in place;
/// safe to call on any `Value` shape — non-object / non-array values
/// pass through untouched.
///
/// Stripped keys (each known to trigger HTTP 400 from Gemini):
/// - `additionalProperties` (the canonical observed failure)
/// - `$schema`, `$id`, `$defs`, `$ref`, `definitions` — JSON-Schema meta
/// - `oneOf`, `anyOf`, `allOf`, `not`, `if`, `then`, `else` — composition
/// - `examples`, `default` — Gemini accepts neither
/// - `patternProperties`, `unevaluatedProperties` — modern JSON-Schema only
/// - `format` — only allow-listed values; arbitrary `format: "email"`
///   / `"uri"` etc. are rejected by Gemini's strict validator. Stripping
///   the field keeps the type constraint while letting the model rely
///   on the description text for format hints. Allowed values:
///   - string formats: `enum`, `date-time`, `date`, `time`, `duration`
///   - number formats: `float`, `double`
///   - integer formats: `int32`, `int64`
///   Non-string values (e.g., `format: 42` from malformed input) are
///   also stripped — Gemini rejects those equally.
///
/// Type normalisation:
/// - `type: ["X", "null"]` (JSON-Schema nullable shorthand) →
///   `type: "X"` + `nullable: true`. Gemini accepts the latter; the
///   former is rejected.
/// - `type: ["null"]` (just null) → drop `type`, set `nullable: true`.
/// - `type: ["X", "Y"]` (real type union, no null) → drop `type`
///   entirely. Gemini has no union type; the model still sees the
///   property name + description and the rest of the schema constrains
///   structure (`properties` / `items` etc.). Better than a 400.
/// - `type: []` (malformed empty array) → drop `type`, do NOT
///   synthesise `nullable: true` (the input didn't claim null-ness).
///
/// Array completion:
/// - `type: "array"` without `items` gains `items: {}`. Gemini requires
///   `items` on every array — the Live API closes the whole session 1007
///   (`…properties[<name>].items: missing`) and `generateContent` returns
///   400 — and an empty item schema is the one it accepts that asserts
///   nothing the source did not (measured on `gemini-3.8-live`,
///   2026-09-18: `items: {}` → `setupComplete`; no `items` → 1007). Pack
///   parameters declared `param_type: array` with no explicit `schema`
///   arrived exactly like that, and one of them (`web_search.blocked_domains`)
///   took every Gemini 3.8 voice call down at setup.
pub(crate) fn sanitize_gemini_schema(value: &mut Value) {
    const UNSUPPORTED_KEYS: &[&str] = &[
        "additionalProperties",
        "$schema",
        "$id",
        "$defs",
        "$ref",
        "definitions",
        "oneOf",
        "anyOf",
        "allOf",
        "not",
        "if",
        "then",
        "else",
        "examples",
        "default",
        "patternProperties",
        "unevaluatedProperties",
    ];
    // Allow-list of `format` values Gemini's OpenAPI-flavoured schema
    // dialect accepts on string / number / integer types. Anything else
    // (`email`, `uri`, `uuid`, `ipv4`, `regex`, …) → strip.
    const ALLOWED_FORMATS: &[&str] = &[
        "enum",
        "date-time",
        "date",
        "time",
        "duration", // string formats
        "float",
        "double", // number formats
        "int32",
        "int64", // integer formats
    ];
    match value {
        Value::Object(map) => {
            for key in UNSUPPORTED_KEYS {
                map.remove(*key);
            }
            // Format whitelist: drop unsupported values rather than the
            // whole schema. Keep the field only when it matches an
            // explicitly-allowed value (case-insensitive — JSON-Schema
            // doesn't mandate case, and some emitters produce
            // `Date-Time` etc.). Non-string `format` values (e.g.
            // `format: 42` from a malformed schema) are also stripped:
            // Gemini's validator rejects non-string formats too, so
            // dropping the field is safer than leaving the 400 in
            // place. `map.contains_key` lets us distinguish "field
            // absent" (leave alone) from "field present but bad".
            if map.contains_key("format") {
                let keep = map.get("format").and_then(Value::as_str).is_some_and(|s| {
                    ALLOWED_FORMATS
                        .iter()
                        .any(|allowed| allowed.eq_ignore_ascii_case(s))
                });
                if !keep {
                    map.remove("format");
                }
            }
            // Type-array normalisation: handle JSON-Schema nullable
            // shorthand (`type: [X, "null"]`) and real type unions.
            // Done in two phases so the borrow of `map` is unambiguous.
            let normalised_type: Option<TypeNormalisation> = match map.get("type") {
                Some(Value::Array(types)) => Some(normalise_type_array(types)),
                _ => None,
            };
            if let Some(norm) = normalised_type {
                match norm {
                    TypeNormalisation::Single { ty, nullable } => {
                        map.insert("type".to_string(), Value::String(ty));
                        if nullable {
                            map.insert("nullable".to_string(), Value::Bool(true));
                        }
                    },
                    TypeNormalisation::NullOnly => {
                        map.remove("type");
                        map.insert("nullable".to_string(), Value::Bool(true));
                    },
                    TypeNormalisation::Union => {
                        map.remove("type");
                    },
                    TypeNormalisation::Empty => {
                        // Malformed `type: []` — drop the field, don't
                        // synthesise a `nullable: true` (the array
                        // didn't claim null-ness; it claimed nothing).
                        map.remove("type");
                    },
                }
            }
            // After type normalisation so `type: ["array", "null"]` is
            // covered too. Only a schema node is completed: a `properties`
            // map whose keys happen to be named `type`/`items` is data, not
            // a schema, and is left alone by the `as_str` check on `type`.
            if map.get("type").and_then(Value::as_str) == Some("array")
                && !map.contains_key("items")
            {
                map.insert("items".to_string(), Value::Object(serde_json::Map::new()));
            }
            for child in map.values_mut() {
                sanitize_gemini_schema(child);
            }
        },
        Value::Array(items) => {
            for item in items.iter_mut() {
                sanitize_gemini_schema(item);
            }
        },
        _ => {},
    }
}

/// Outcome of normalising a JSON-Schema `type:` array into Gemini's
/// scalar `type` + `nullable` model.
enum TypeNormalisation {
    /// Exactly one non-null type, with optional `null` companion.
    Single { ty: String, nullable: bool },
    /// `["null"]` — purely the null type.
    NullOnly,
    /// Real union of multiple non-null types — Gemini has no
    /// representation, so the caller drops `type` entirely.
    Union,
    /// Malformed empty `type: []` array (or one containing only
    /// non-string entries that the parser couldn't interpret). The
    /// caller drops `type` without synthesising any `nullable` flag
    /// since the input didn't actually claim nullability.
    Empty,
}

fn normalise_type_array(types: &[Value]) -> TypeNormalisation {
    let mut non_null: Vec<String> = Vec::new();
    let mut has_null = false;
    for t in types {
        match t.as_str() {
            Some("null") => has_null = true,
            Some(other) => non_null.push(other.to_string()),
            None => {}, // ignore non-string entries (malformed schema)
        }
    }
    match (non_null.len(), has_null) {
        (0, false) => TypeNormalisation::Empty,
        (0, true) => TypeNormalisation::NullOnly,
        (1, _) => TypeNormalisation::Single {
            ty: non_null.remove(0),
            nullable: has_null,
        },
        _ => TypeNormalisation::Union,
    }
}

fn gemini_provider_function_id(tool_call_id: &str) -> Option<&str> {
    if let Some((_, provider_id)) = tool_call_id.split_once(GEMINI_TOOL_CALL_ID_SEPARATOR) {
        return (!provider_id.trim().is_empty()).then_some(provider_id);
    }

    if !tool_call_id.is_empty() && !tool_call_id.starts_with("gemini-call-") {
        return Some(tool_call_id);
    }

    None
}

fn gemini_lowest_thinking_config(model: &str) -> Option<Value> {
    let mut config = Map::new();
    if model.contains("gemini-3") {
        let level = if model.contains("flash") || model.contains("flash-lite") {
            "minimal"
        } else {
            "low"
        };
        config.insert(
            "thinkingLevel".to_string(),
            Value::String(level.to_string()),
        );
    } else if model.contains("gemini-2.5-flash")
        || model.contains("gemini-2.5-flash-lite")
        || model.contains("robotics-er")
    {
        config.insert("thinkingBudget".to_string(), Value::from(0));
    } else if model.contains("gemini-2.5-pro") {
        config.insert("thinkingBudget".to_string(), Value::from(128));
    }

    (!config.is_empty()).then_some(Value::Object(config))
}

fn gemini3_thinking_level(model: &str, effort: &str) -> Option<&'static str> {
    let effort = effort.trim().to_ascii_lowercase();
    match effort.as_str() {
        "minimal" | "min" => {
            if model.contains("flash") || model.contains("flash-lite") {
                Some("minimal")
            } else {
                Some("low")
            }
        },
        "low" => Some("low"),
        "medium" | "med" => Some("medium"),
        "high" | "max" => Some("high"),
        _ => None,
    }
}

/// Extract the function name from a synthetic Gemini tool call ID.
/// IDs follow the format `gemini-call-{name}-{index}`.
fn extract_function_name_from_id(id: &str) -> String {
    let id = id
        .split_once(GEMINI_TOOL_CALL_ID_SEPARATOR)
        .map(|(synthetic, _)| synthetic)
        .unwrap_or(id);

    if let Some(rest) = id.strip_prefix("gemini-call-") {
        // Strip trailing `-{digit(s)}`
        if let Some(pos) = rest.rfind('-') {
            let suffix = &rest[pos + 1..];
            if !suffix.is_empty() && suffix.chars().all(|c| c.is_ascii_digit()) {
                return rest[..pos].to_string();
            }
        }
        rest.to_string()
    } else {
        id.to_string()
    }
}

#[async_trait]
impl LLMProvider for GeminiProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Gemini
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        let mut capability = Self::detect_capability(model);
        // Both Gemini paths translate the flag into the googleSearch
        // grounding tool.
        capability.web_search = true;
        capability
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let protected_request = request.metadata.single_physical_attempt;
        let retry_attempt = Self::max_tokens_retry_attempt(&request);
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for Gemini provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        Self::validate_protected_transport(&request)?;
        if Self::uses_interactions(&request) {
            return self.invoke_interactions(request).await;
        }

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();

        debug!(
            provider = "gemini",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing Gemini generateContent request"
        );

        let (system_instruction, contents) = Self::map_messages(&request.messages)?;

        let mut body = Map::new();
        body.insert("contents".to_string(), Value::Array(contents));

        if let Some(system) = system_instruction {
            body.insert("systemInstruction".to_string(), system);
        }

        if let Some(cached_content) = Self::prompt_cached_content(&request) {
            body.insert("cachedContent".to_string(), Value::String(cached_content));
        }

        // When server-side search is enabled the invariant guarantees
        // `request.tools` is empty, so toolConfig is never emitted next to
        // the googleSearch tool.
        Self::attach_tools_or_google_search(&mut body, &request, Self::map_tools)?;
        if !request.tools.is_empty() && !crate::server_web_search::requested(&request) {
            body.insert(
                "toolConfig".to_string(),
                Self::map_tool_config(request.extra_value()),
            );
        }

        // Generation config
        let mut gen_config = Map::new();
        if Self::accepts_sampling_parameters(&model) {
            if let Some(temp) = request.temperature {
                gen_config.insert("temperature".to_string(), Value::from(temp));
            }
            if let Some(top_p) = request.top_p {
                gen_config.insert("topP".to_string(), Value::from(top_p));
            }
        } else if request.temperature.is_some() || request.top_p.is_some() {
            debug!(
                provider = "gemini",
                model = %model,
                "omitting sampling parameters unsupported by this Gemini model generation"
            );
        }
        if let Some(thinking_config) = Self::map_thinking_config(&model, request.reasoning.as_ref())
        {
            gen_config.insert("thinkingConfig".to_string(), thinking_config);
        }
        if let Some(max_tokens) = request.max_output_tokens {
            gen_config.insert("maxOutputTokens".to_string(), Value::from(max_tokens));
        }
        if !gen_config.is_empty() {
            body.insert("generationConfig".to_string(), Value::Object(gen_config));
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                if !Self::should_forward_generate_content_extra(&key, protected_request) {
                    debug!(
                        provider = "gemini",
                        key = %key,
                        "filtering out config-only or protected-state field from Gemini API request"
                    );
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        let url = format!("{}/models/{}:generateContent", self.base_url, model);

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let response = self
            .client
            .post(&url)
            .header("x-goog-api-key", &self.api_key)
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
            let message = payload
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_string();
            if protected_request {
                error!(
                    provider = "gemini",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "Gemini API rejected protected request"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: "protected provider request failed".to_owned(),
                });
            }
            error!(
                provider = "gemini",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                status = %status,
                error_message = %message,
                "Gemini API responded with error"
            );
            return Err(LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message,
            });
        }

        let (messages, tool_calls, text) = Self::parse_output(&payload)?;
        let usage = payload
            .get("usageMetadata")
            .map(Self::map_usage)
            .transpose()?;

        let finish_reason = payload
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|arr| arr.first())
            .and_then(|c| c.get("finishReason"))
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
            // Keep the one parsed provider tree; do not duplicate the full
            // candidate payload merely to retain the raw response.
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "gemini",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by Gemini"
            );
        }

        info!(
            provider = "gemini",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            response_text_len = response.text.as_ref().map(|t| t.len()).unwrap_or(0),
            "Gemini generateContent call succeeded"
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
                    provider = "gemini",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    base_max_output_tokens,
                    retry_max_output_tokens,
                    tool_call_count = response.tool_calls.len(),
                    "Gemini response hit MAX_TOKENS; retrying with higher maxOutputTokens"
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
                                provider = "gemini",
                                model = %model,
                                operation = %operation,
                                trace_id = trace_id.as_deref().unwrap_or(""),
                                retry_max_output_tokens,
                                backed_off_max_output_tokens,
                                error = %error,
                                "Gemini retry rejected larger maxOutputTokens; retrying with backed-off token budget"
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

    #[test]
    fn google_search_tool_replaces_function_tools_and_fails_mixed_closed() {
        let mut request = LLMRequest::default();
        request.model = "gemini-3-pro".to_string();
        request.set_extra(json!({"server_web_search": true}));

        let mut body = Map::new();
        GeminiProvider::attach_tools_or_google_search(
            &mut body,
            &request,
            GeminiProvider::map_tools,
        )
        .expect("attach succeeds");
        assert_eq!(body["tools"][0]["googleSearch"], json!({}));
        assert!(body["tools"][0].get("function_declarations").is_none());

        let mut mixed = request.clone();
        mixed.tools = vec![LLMToolSpec {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: json!({}),
        }]
        .into();
        let mut mixed_body = Map::new();
        let error = GeminiProvider::attach_tools_or_google_search(
            &mut mixed_body,
            &mixed,
            GeminiProvider::map_tools,
        )
        .expect_err("mixed request must fail closed");
        assert!(error.to_string().contains("cannot be combined"));
    }

    #[test]
    fn disclosure_bound_interactions_disable_provider_storage() {
        let mut protected = LLMRequest::default();
        protected.metadata.single_physical_attempt = true;
        assert!(!GeminiProvider::interactions_provider_storage_enabled(
            &protected
        ));
        assert!(GeminiProvider::interactions_provider_storage_enabled(
            &LLMRequest::default()
        ));
    }

    #[test]
    fn disclosure_bound_interactions_fail_before_physical_io() {
        let mut protected = LLMRequest {
            extra: Some(json!({ "gemini_api_mode": "interactions" }).into()),
            ..Default::default()
        };
        protected.metadata.single_physical_attempt = true;

        assert!(matches!(
            GeminiProvider::validate_protected_transport(&protected),
            Err(LLMError::Validation(_))
        ));
    }

    #[test]
    fn disclosure_bound_generate_content_ignores_cached_content_state() {
        let mut protected = LLMRequest {
            prompt_cache: Some(PromptCacheConfig::Enabled {
                cached_content: Some("cachedContents/private-history".to_owned()),
                ttl: None,
            }),
            extra: Some(
                json!({
                    "cachedContent": "cachedContents/other-history",
                    "temperature": 0.2
                })
                .into(),
            ),
            ..Default::default()
        };
        protected.metadata.single_physical_attempt = true;

        assert_eq!(GeminiProvider::prompt_cached_content(&protected), None);
        assert!(!GeminiProvider::should_forward_generate_content_extra(
            "cachedContent",
            true
        ));
        assert!(GeminiProvider::should_forward_generate_content_extra(
            "temperature",
            true
        ));
    }

    #[test]
    fn map_tools_produces_gemini_format() {
        let tools = vec![LLMToolSpec {
            name: "get_weather".to_string(),
            description: "Get weather for a city".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "city": { "type": "string" }
                },
                "required": ["city"]
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        assert_eq!(mapped.len(), 1);
        let declarations = mapped[0]["functionDeclarations"].as_array().unwrap();
        assert_eq!(declarations.len(), 1);
        assert_eq!(declarations[0]["name"], "get_weather");
        assert_eq!(declarations[0]["description"], "Get weather for a city");
        assert_eq!(declarations[0]["parameters"]["type"], "object");
    }

    /// Gemini requires `items` on every array. The Live API closes the
    /// session 1007 at setup when one is missing — measured with the shipped
    /// voice catalog, whose `web_search.blocked_domains` (a pack
    /// `param_type: array` with no explicit schema) took every Gemini 3.8
    /// call down — and `items: {}` is the shape it accepts without inventing
    /// an item type. Explicit `items` must survive untouched, at any depth.
    #[test]
    fn arrays_without_items_gain_an_unconstrained_item_schema() {
        let mut schema = json!({
            "type": "object",
            "properties": {
                "blocked_domains": {
                    "type": "array",
                    "description": "Optional block-list of domains."
                },
                "nullable_list": { "type": ["array", "null"] },
                "typed": { "type": "array", "items": { "type": "string" } },
                "nested": {
                    "type": "object",
                    "properties": {
                        "deep": { "type": "array" }
                    }
                },
                "rows": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": { "tags": { "type": "array" } }
                    }
                }
            }
        });
        sanitize_gemini_schema(&mut schema);
        let props = &schema["properties"];
        assert_eq!(props["blocked_domains"]["items"], json!({}));
        assert_eq!(
            props["blocked_domains"]["description"], "Optional block-list of domains.",
            "completion adds, it never rewrites"
        );
        assert_eq!(props["nullable_list"]["type"], "array");
        assert_eq!(props["nullable_list"]["nullable"], true);
        assert_eq!(props["nullable_list"]["items"], json!({}));
        assert_eq!(
            props["typed"]["items"],
            json!({ "type": "string" }),
            "an explicit item schema is kept"
        );
        assert_eq!(props["nested"]["properties"]["deep"]["items"], json!({}));
        assert_eq!(
            props["rows"]["items"]["properties"]["tags"]["items"],
            json!({}),
            "arrays inside array items are completed too"
        );
        // A property that merely shares the name `type` with a string value
        // that is not "array" is not mistaken for an array schema.
        let mut data_like =
            json!({ "type": "object", "properties": { "type": { "type": "string" } } });
        sanitize_gemini_schema(&mut data_like);
        assert!(data_like["properties"]["type"].get("items").is_none());
    }

    #[test]
    fn map_tools_strips_additional_properties_at_every_depth() {
        // Mirrors the in-the-wild failure mode: the YAML pack schemas
        // ship with `additionalProperties: false` at the top, inside
        // nested object properties, and inside array items' schemas.
        // Without sanitization, Gemini's validator rejects the entire
        // tools array with HTTP 400 (see log: `chat-turn-...-iter1.json`
        // → "Unknown name additionalProperties at function_declarations[N]
        // .parameters.properties[0].value.items.properties[4].value").
        let tools = vec![LLMToolSpec {
            name: "create_task".to_string(),
            description: "Create a task".to_string(),
            parameters: json!({
                "type": "object",
                "additionalProperties": false,
                "$schema": "http://json-schema.org/draft-07/schema#",
                "properties": {
                    "title": { "type": "string", "default": "Untitled" },
                    "depends_on": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "additionalProperties": false,
                            "properties": {
                                "task_id": { "type": "string" }
                            }
                        }
                    },
                    "schedule": {
                        "oneOf": [
                            { "type": "null" },
                            { "type": "object" }
                        ]
                    }
                },
                "required": ["title"]
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        let params = &mapped[0]["functionDeclarations"][0]["parameters"];

        // Top-level metadata + composition keys stripped.
        assert!(params.get("additionalProperties").is_none());
        assert!(params.get("$schema").is_none());
        // Composition node fully removed even with valid type alternates.
        assert!(params["properties"]["schedule"].get("oneOf").is_none());
        // `default` stripped from string property.
        assert!(params["properties"]["title"].get("default").is_none());
        // Nested array-items schema also sanitized.
        assert!(params["properties"]["depends_on"]["items"]
            .get("additionalProperties")
            .is_none());
        // Real schema content preserved.
        assert_eq!(params["type"], "object");
        assert_eq!(params["properties"]["title"]["type"], "string");
        assert_eq!(
            params["properties"]["depends_on"]["items"]["properties"]["task_id"]["type"],
            "string"
        );
        let required = params["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v == "title"));
    }

    #[test]
    fn map_tools_normalises_nullable_type_array_to_nullable_flag() {
        // JSON-Schema's `type: ["X", "null"]` nullable shorthand → Gemini's
        // `type: "X", nullable: true` representation.
        let tools = vec![LLMToolSpec {
            name: "set_alias".to_string(),
            description: "Optionally set an alias".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "alias": { "type": ["string", "null"] },
                    "count": { "type": ["integer", "null"] },
                    "raw": { "type": "string" } // unchanged scalar type
                }
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        let params = &mapped[0]["functionDeclarations"][0]["parameters"];

        // String + null → string + nullable
        assert_eq!(params["properties"]["alias"]["type"], "string");
        assert_eq!(params["properties"]["alias"]["nullable"], true);
        // Integer + null → integer + nullable
        assert_eq!(params["properties"]["count"]["type"], "integer");
        assert_eq!(params["properties"]["count"]["nullable"], true);
        // Scalar `type: "string"` left alone (no `nullable` injected).
        assert_eq!(params["properties"]["raw"]["type"], "string");
        assert!(params["properties"]["raw"].get("nullable").is_none());
    }

    #[test]
    fn map_tools_drops_real_union_type_arrays() {
        // `type: ["string", "integer"]` is a JSON-Schema type union — no
        // null involved. Gemini has no union representation, so the
        // sanitizer drops `type` entirely; the schema then constrains
        // structure via `properties` / `description` only.
        let tools = vec![LLMToolSpec {
            name: "set_value".to_string(),
            description: "Set a string-or-integer value".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "value": { "type": ["string", "integer"] }
                }
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        let value_schema =
            &mapped[0]["functionDeclarations"][0]["parameters"]["properties"]["value"];
        assert!(
            value_schema.get("type").is_none(),
            "union type should be dropped, got: {value_schema}"
        );
        assert!(value_schema.get("nullable").is_none());
    }

    #[test]
    fn map_tools_handles_malformed_type_and_format_edges() {
        // Two malformed-but-real-world edge cases:
        //   • `type: []` (empty array) — drop type entirely, do NOT
        //     synthesise `nullable: true` (the input didn't claim
        //     null-ness; it claimed nothing).
        //   • `format: 42` (non-string format value) — strip; Gemini's
        //     validator rejects non-string formats too.
        let tools = vec![LLMToolSpec {
            name: "edge_cases".to_string(),
            description: "Schemas with malformed edges".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "empty_type":   { "type": [] },
                    "bad_format":   { "type": "string", "format": 42 },
                    "good_format":  { "type": "string", "format": "date-time" }
                }
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        let props = &mapped[0]["functionDeclarations"][0]["parameters"]["properties"];

        // Empty type array → `type` gone, `nullable` NOT injected.
        assert!(props["empty_type"].get("type").is_none());
        assert!(props["empty_type"].get("nullable").is_none());
        // Non-string format → stripped; type preserved.
        assert!(props["bad_format"].get("format").is_none());
        assert_eq!(props["bad_format"]["type"], "string");
        // Supported string format → kept verbatim.
        assert_eq!(props["good_format"]["format"], "date-time");
    }

    #[test]
    fn map_tools_strips_unsupported_format_specifiers() {
        // Gemini accepts a small allowlist of `format` values (date-time,
        // date, time, duration, enum, float, double, int32, int64).
        // Anything else (`email`, `uri`, `uuid`, `ipv4`, …) makes the
        // validator reject the whole tools array. The sanitizer drops
        // unrecognised formats while preserving the rest of the schema;
        // the model still learns the format from the description text.
        let tools = vec![LLMToolSpec {
            name: "register_user".to_string(),
            description: "Register a user".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "email":     { "type": "string", "format": "email" },
                    "homepage":  { "type": "string", "format": "uri" },
                    "user_id":   { "type": "string", "format": "uuid" },
                    "joined_at": { "type": "string", "format": "date-time" },
                    "duration":  { "type": "string", "format": "duration" },
                    "amount":    { "type": "number", "format": "double" }
                }
            }),
        }];

        let mapped = GeminiProvider::map_tools(&tools);
        let props = &mapped[0]["functionDeclarations"][0]["parameters"]["properties"];

        // Unsupported formats stripped, type preserved.
        assert!(props["email"].get("format").is_none());
        assert!(props["homepage"].get("format").is_none());
        assert!(props["user_id"].get("format").is_none());
        assert_eq!(props["email"]["type"], "string");
        assert_eq!(props["homepage"]["type"], "string");
        assert_eq!(props["user_id"]["type"], "string");

        // Supported formats kept verbatim.
        assert_eq!(props["joined_at"]["format"], "date-time");
        assert_eq!(props["duration"]["format"], "duration");
        assert_eq!(props["amount"]["format"], "double");
    }

    #[test]
    fn tool_choice_defaults_to_any() {
        let config = GeminiProvider::map_tool_config(None);
        assert_eq!(config["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tool_choice_translates_anthropic_any() {
        let extra = json!({ "tool_choice": { "type": "any" } });
        let config = GeminiProvider::map_tool_config(Some(&extra));
        assert_eq!(config["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tool_choice_translates_auto() {
        let extra = json!({ "tool_choice": { "type": "auto" } });
        let config = GeminiProvider::map_tool_config(Some(&extra));
        assert_eq!(config["functionCallingConfig"]["mode"], "AUTO");
    }

    #[test]
    fn tool_choice_translates_string_auto() {
        // OpenAI-style string shape — the chat profile filter accepts both
        // string and object forms; the provider must too. Earlier the
        // string form was silently coerced to ANY, which broke chat
        // semantics by forcing tool-call output every turn.
        let extra = json!({ "tool_choice": "auto" });
        let config = GeminiProvider::map_tool_config(Some(&extra));
        assert_eq!(config["functionCallingConfig"]["mode"], "AUTO");
    }

    #[test]
    fn tool_choice_translates_string_required() {
        let extra = json!({ "tool_choice": "required" });
        let config = GeminiProvider::map_tool_config(Some(&extra));
        assert_eq!(config["functionCallingConfig"]["mode"], "ANY");
    }

    #[test]
    fn tool_choice_translates_string_none() {
        let extra = json!({ "tool_choice": "none" });
        let config = GeminiProvider::map_tool_config(Some(&extra));
        assert_eq!(config["functionCallingConfig"]["mode"], "NONE");
    }

    #[test]
    fn thinking_config_maps_gemini3_effort_to_thinking_level() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-3.1-pro-preview",
            Some(&ReasoningConfig {
                effort: Some("high".to_string()),
                max_reasoning_tokens: Some(4096),
                strategy: None,
                summary: None,
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingLevel"], "high");
        assert!(config.get("thinkingBudget").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini35_flash_lite_none_to_minimal() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-3.5-flash-lite",
            Some(&ReasoningConfig {
                effort: Some("none".to_string()),
                ..Default::default()
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingLevel"], "minimal");
        assert!(config.get("thinkingBudget").is_none());
    }

    #[test]
    fn thinking_config_defaults_gemini3_pro_to_low_when_reasoning_absent() {
        let config = GeminiProvider::map_thinking_config("gemini-3.1-pro-preview", None)
            .expect("thinking config");

        assert_eq!(config["thinkingLevel"], "low");
        assert!(config.get("thinkingBudget").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini3_pro_medium_to_medium() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-3.1-pro-preview",
            Some(&ReasoningConfig {
                effort: Some("medium".to_string()),
                ..Default::default()
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingLevel"], "medium");
        assert!(config.get("thinkingBudget").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini35_flash_lite_medium_to_medium() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-3.5-flash-lite",
            Some(&ReasoningConfig {
                effort: Some("medium".to_string()),
                ..Default::default()
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingLevel"], "medium");
        assert!(config.get("thinkingBudget").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini25_tokens_to_thinking_budget() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-2.5-pro",
            Some(&ReasoningConfig {
                effort: Some("high".to_string()),
                max_reasoning_tokens: Some(4096),
                strategy: None,
                summary: None,
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingBudget"], 4096);
        assert!(config.get("thinkingLevel").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini25_flash_none_to_zero_budget() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-2.5-flash",
            Some(&ReasoningConfig {
                effort: Some("none".to_string()),
                ..Default::default()
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingBudget"], 0);
        assert!(config.get("thinkingLevel").is_none());
    }

    #[test]
    fn thinking_config_maps_gemini25_pro_none_to_minimum_budget() {
        let config = GeminiProvider::map_thinking_config(
            "gemini-2.5-pro",
            Some(&ReasoningConfig {
                effort: Some("none".to_string()),
                ..Default::default()
            }),
        )
        .expect("thinking config");

        assert_eq!(config["thinkingBudget"], 128);
        assert!(config.get("thinkingLevel").is_none());
    }

    #[test]
    fn map_tool_calls_extracts_function_calls() {
        let candidate = json!({
            "content": {
                "parts": [
                    {
                        "functionCall": {
                            "id": "call_123",
                            "name": "get_weather",
                            "args": { "city": "London" }
                        }
                    }
                ],
                "role": "model"
            },
            "finishReason": "STOP"
        });

        let calls = GeminiProvider::map_tool_calls(&candidate).expect("bounded tool arguments");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "get_weather");
        assert_eq!(calls[0].arguments, json!({ "city": "London" }));
        assert_eq!(calls[0].id, "gemini-call-get_weather-0::call_123");
    }

    #[test]
    fn map_messages_separates_system_prompt() {
        let messages = vec![
            LLMMessage::system("You are a helpful assistant."),
            LLMMessage::user("Hello"),
        ];

        let (system, contents) = GeminiProvider::map_messages(&messages).unwrap();
        assert!(system.is_some());
        let sys = system.unwrap();
        assert_eq!(sys["parts"][0]["text"], "You are a helpful assistant.");

        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
    }

    #[test]
    fn map_messages_handles_assistant_as_model() {
        let messages = vec![LLMMessage::user("Hi"), LLMMessage::assistant("Hello!")];

        let (system, contents) = GeminiProvider::map_messages(&messages).unwrap();
        assert!(system.is_none());
        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(contents[1]["role"], "model");
    }

    #[test]
    fn map_messages_handles_tool_role() {
        let messages = vec![LLMMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "gemini-call-get_weather-0::call_weather_1".to_string(),
                content: json!({ "temperature": 20 }),
            }],
        }];

        let (_system, contents) = GeminiProvider::map_messages(&messages).unwrap();
        assert_eq!(contents.len(), 1);
        assert_eq!(contents[0]["role"], "user");
        let parts = contents[0]["parts"].as_array().unwrap();
        assert!(parts[0].get("functionResponse").is_some());
        assert_eq!(parts[0]["functionResponse"]["name"], "get_weather");
        assert_eq!(parts[0]["functionResponse"]["id"], "call_weather_1");
        // Verify response is wrapped in {"result": ...}
        assert!(parts[0]["functionResponse"]["response"]["result"].is_object());
    }

    #[test]
    fn projected_tool_results_keep_order_ids_and_structured_values() {
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
                    tool_call_id: "gemini-call-lookup-0::call-a".into(),
                    content: first.clone(),
                },
                ContentBlock::ToolResult {
                    tool_call_id: "gemini-call-lookup-1::call-b".into(),
                    content: second.clone(),
                },
            ],
        }];

        let (_system, mapped) =
            GeminiProvider::map_messages(&messages).expect("map projected results");

        let parts = mapped[0]["parts"].as_array().expect("function responses");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["functionResponse"]["id"], "call-a");
        assert_eq!(parts[1]["functionResponse"]["id"], "call-b");
        assert_eq!(parts[0]["functionResponse"]["response"]["result"], first);
        assert_eq!(parts[1]["functionResponse"]["response"]["result"], second);
        assert!(parts[0]["functionResponse"]["response"]["result"].is_object());
    }

    #[test]
    fn map_usage_extracts_gemini_fields() {
        let metadata = json!({
            "promptTokenCount": 100,
            "candidatesTokenCount": 50,
            "totalTokenCount": 150,
        });

        let usage = GeminiProvider::map_usage(&metadata).expect("bounded usage maps");
        assert_eq!(usage.prompt_tokens, Some(100));
        assert_eq!(usage.completion_tokens, Some(50));
        assert_eq!(usage.total_tokens, Some(150));
    }

    #[test]
    fn usage_mappers_reject_provider_counters_larger_than_u32() {
        let too_large = u64::from(u32::MAX) + 1;
        let standard = json!({ "promptTokenCount": too_large });
        let interactions = json!({ "total_thought_tokens": too_large });

        assert!(matches!(
            GeminiProvider::map_usage(&standard),
            Err(LLMError::Provider { .. })
        ));
        assert!(matches!(
            GeminiProvider::map_interaction_usage(&interactions),
            Err(LLMError::Provider { .. })
        ));
    }

    #[test]
    fn parse_output_extracts_text_and_tool_calls() {
        let payload = json!({
            "candidates": [{
                "content": {
                    "parts": [
                        { "text": "Here is the weather:" },
                        {
                            "functionCall": {
                                "id": "call_weather_1",
                                "name": "get_weather",
                                "args": { "city": "Paris" }
                            }
                        }
                    ],
                    "role": "model"
                },
                "finishReason": "STOP"
            }],
            "usageMetadata": {
                "promptTokenCount": 10,
                "candidatesTokenCount": 20,
                "totalTokenCount": 30
            }
        });

        let (messages, tool_calls, text) =
            GeminiProvider::parse_output(&payload).expect("bounded provider output");
        assert_eq!(text.as_deref(), Some("Here is the weather:"));
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0].name, "get_weather");
        assert_eq!(
            tool_calls[0].id,
            "gemini-call-get_weather-1::call_weather_1"
        );
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.len(), 2);
    }

    #[test]
    fn map_messages_replays_raw_model_parts_and_multimodal_tool_results() {
        let messages = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Json {
                    value: json!({
                        "_magicllm_gemini_raw_parts": [
                            {
                                "functionCall": {
                                    "id": "call_generate_image_1",
                                    "name": "generate_image",
                                    "args": { "prompt": "poster" }
                                },
                                "thoughtSignature": "sig-123"
                            }
                        ]
                    }),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "gemini-call-generate_image-0::call_generate_image_1".to_string(),
                    content: json!({
                        "_magicllm_rich_tool_result": true,
                        "blocks": [
                            { "type": "text", "text": "Generated poster.png" },
                            {
                                "type": "image",
                                "media_type": "image/png",
                                "data_base64": "iVBORw0KGgo="
                            }
                        ]
                    }),
                }],
            },
        ];

        let (_system, contents) = GeminiProvider::map_messages(&messages).expect("gemini mapping");

        assert_eq!(contents.len(), 2);
        assert_eq!(contents[0]["role"], "model");
        assert_eq!(
            contents[0]["parts"][0]["thoughtSignature"].as_str(),
            Some("sig-123")
        );
        assert_eq!(contents[1]["role"], "user");
        let parts = contents[1]["parts"].as_array().expect("function parts");
        assert_eq!(parts[0]["functionResponse"]["name"], "generate_image");
        assert_eq!(parts[0]["functionResponse"]["id"], "call_generate_image_1");
        assert_eq!(
            parts[0]["functionResponse"]["response"]["result"]["text"],
            "Generated poster.png"
        );
        assert_eq!(
            parts[0]["functionResponse"]["parts"][0]["inlineData"]["mimeType"],
            "image/png"
        );
    }

    #[test]
    fn map_messages_supports_inline_user_images() {
        let messages = vec![LLMMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "Edit this".to_string(),
                },
                ContentBlock::Image {
                    data: vec![1, 2, 3],
                    media_type: "image/png".to_string(),
                    caption: None,
                },
            ],
        }];

        let (_system, contents) = GeminiProvider::map_messages(&messages).expect("gemini mapping");
        assert_eq!(contents[0]["role"], "user");
        assert_eq!(
            contents[0]["parts"][1]["inlineData"]["mimeType"],
            "image/png"
        );
    }

    #[test]
    fn extract_function_name_from_synthetic_id() {
        assert_eq!(
            extract_function_name_from_id("gemini-call-get_weather-0"),
            "get_weather"
        );
        assert_eq!(
            extract_function_name_from_id("gemini-call-get_weather-0::call_123"),
            "get_weather"
        );
        assert_eq!(
            extract_function_name_from_id("gemini-call-search-42"),
            "search"
        );
        // Non-synthetic IDs pass through as-is
        assert_eq!(
            extract_function_name_from_id("some-other-id"),
            "some-other-id"
        );
    }

    #[test]
    fn detect_capability_vision_for_2_0_models() {
        let cap = GeminiProvider::detect_capability("gemini-2.0-flash");
        assert!(cap.supports_modality(LLMModality::Vision));
        assert!(cap.tool_calling);
        assert!(cap.json_mode);
    }

    #[test]
    fn detect_capability_reasoning_for_2_5_pro() {
        let cap = GeminiProvider::detect_capability("gemini-2.5-pro");
        assert!(cap.supports_reasoning());
    }

    #[test]
    fn detect_capability_no_reasoning_for_flash() {
        let cap = GeminiProvider::detect_capability("gemini-2.0-flash");
        assert!(!cap.supports_reasoning());
    }

    #[test]
    fn extract_function_name_handles_hyphens_in_name() {
        assert_eq!(
            extract_function_name_from_id("gemini-call-get-data-0"),
            "get-data"
        );
        assert_eq!(
            extract_function_name_from_id("gemini-call-my-tool-v2-12"),
            "my-tool-v2"
        );
    }

    #[test]
    fn detect_capability_vision_for_3_5_flash_lite() {
        let cap = GeminiProvider::detect_capability("gemini-3.5-flash-lite");
        assert!(cap.supports_modality(LLMModality::Vision));
        assert!(cap.supports_reasoning());
        assert!(cap.tool_calling);
    }

    #[test]
    fn newest_flash_models_reject_sampling_parameters() {
        assert!(!GeminiProvider::accepts_sampling_parameters(
            "gemini-3.5-flash-lite"
        ));
        assert!(!GeminiProvider::accepts_sampling_parameters(
            "gemini-3.6-flash"
        ));
        assert!(GeminiProvider::accepts_sampling_parameters(
            "gemini-3.5-flash"
        ));
        assert!(GeminiProvider::accepts_sampling_parameters(
            "gemini-3.1-pro-preview"
        ));
    }

    #[test]
    fn detect_capability_vision_and_reasoning_for_3_1_pro() {
        let cap = GeminiProvider::detect_capability("gemini-3.1-pro-preview");
        assert!(cap.supports_modality(LLMModality::Vision));
        assert!(cap.supports_reasoning());
        assert!(cap.tool_calling);
    }
}

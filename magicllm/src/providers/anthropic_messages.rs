use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::{
    append_sse_utf8_chunk, bounded_usage_counter, checked_usage_sum, default_http_client,
    finish_sse_utf8, next_sse_data, read_bounded_response_text, SseBodyAdmission,
};
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    catalog::{ModelWire, ThinkingWire},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        anthropic_raw_content_block, append_tool_argument_fragment, as_anthropic_raw_content_block,
        clone_admitted_tool_argument, clone_json_value_iteratively, parse_provider_json_or_string,
        parse_provider_json_value, parse_tool_argument_json_or_string, ContentBlock, LLMMessage,
        LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall, LLMToolResult, LLMToolSpec,
        MessageRole, PromptCacheConfig, ReasoningConfig, RequestMetadata, StreamDelta, TokenUsage,
    },
};

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;
const ANTHROPIC_MAX_TOKENS_RETRY_CAP: u32 = 32_000;

/// Whether an Anthropic model takes adaptive thinking (`thinking.type =
/// adaptive` + `output_config.effort`) rather than a `budget_tokens` budget,
/// by model name. Uncatalogued fallback only (`uncatalogued_wire`).
/// Public so a harness that calls Anthropic itself (Pi) without a catalog
/// entry is configured by the same rule as Magician's own calls.
pub fn anthropic_model_uses_adaptive_thinking(model: &str) -> bool {
    uncatalogued_wire(&LLMProviderKind::Anthropic, model).thinking == Some(ThinkingWire::Adaptive)
}

/// FALLBACK for models absent from the model catalog (ad-hoc ids, legacy
/// profiles, direct provider calls). Catalogued models carry their request
/// shape on [`LLMRequest::model_wire`] (catalog `thinking` + `wire`) and never
/// reach this function. Every model-name rule of this transport lives here.
///
/// - Adaptive thinking: Fable / Mythos / Opus 5.x / Sonnet 5 / Haiku 5.x /
///   Opus 4.6–4.7 / Sonnet 4.6 reject `thinking.type = enabled`; Haiku 4.5
///   still takes it and rejects adaptive.
/// - Sampling: Opus 4.7+, Opus 5.x, Sonnet 5.x, Haiku 5.x, Fable and Mythos
///   return 400 on `temperature` / `top_p`; Opus 4.6, Sonnet 4.6 and Haiku
///   4.5 accept them.
/// - Thinking Off: Claude 4.x, Opus 5, Sonnet 5 and Haiku 5.5; Opus 5 and
///   Haiku 5.5 reject Off with an inherited xhigh/max effort, so Off sends
///   `low`.
pub(crate) fn uncatalogued_wire(provider_kind: &LLMProviderKind, model: &str) -> ModelWire {
    const ADAPTIVE: [&str; 8] = [
        "claude-fable",
        "claude-mythos",
        "claude-opus-5",
        "claude-opus-4-7",
        "claude-opus-4-6",
        "claude-sonnet-5",
        "claude-haiku-5",
        "claude-sonnet-4-6",
    ];
    const REJECTS_SAMPLING: [&str; 7] = [
        "claude-fable",
        "claude-mythos",
        "claude-opus-5",
        "claude-opus-4-7",
        "claude-opus-4-8",
        "claude-sonnet-5",
        "claude-haiku-5",
    ];
    const OFF_NEEDS_EFFORT: [&str; 2] = ["claude-opus-5", "claude-haiku-5-5"];
    let model = model.to_ascii_lowercase();
    let any = |families: &[&str]| families.iter().any(|family| model.contains(family));
    let anthropic = *provider_kind == LLMProviderKind::Anthropic;
    let name = model.rsplit('/').next().unwrap_or(&model);
    let thinking_off = anthropic
        && (["claude-opus-4", "claude-sonnet-4", "claude-haiku-4"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
            || name == "claude-opus-5"
            || name == "claude-sonnet-5"
            || name.starts_with("claude-haiku-5-5"));
    ModelWire {
        thinking: Some(if anthropic && any(&ADAPTIVE) {
            ThinkingWire::Adaptive
        } else {
            ThinkingWire::Budget
        }),
        thinking_off,
        thinking_off_effort: (anthropic && any(&OFF_NEEDS_EFFORT)).then(|| "low".to_string()),
        sampling: !any(&REJECTS_SAMPLING),
    }
}

/// The catalogued model's request shape, else the name-based fallback.
fn wire_for(
    provider_kind: &LLMProviderKind,
    model: &str,
    catalogued: Option<&ModelWire>,
) -> ModelWire {
    catalogued
        .cloned()
        .unwrap_or_else(|| uncatalogued_wire(provider_kind, model))
}

/// Provider implementation for the Anthropic Messages API.
pub struct AnthropicMessagesProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_kind: LLMProviderKind,
    default_timeout: Duration,
}

impl AnthropicMessagesProvider {
    /// Creates a provider using the default Anthropic Messages endpoint.
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL)
    }

    /// Creates a provider with a custom base URL (useful for proxies or enterprises).
    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

    /// Creates a provider with a preconfigured HTTP client.
    pub fn with_client(
        client: Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self::with_client_for_provider(client, api_key, base_url, LLMProviderKind::Anthropic)
    }

    /// Creates an Anthropic-wire provider with a non-Anthropic provider identity.
    pub fn with_base_url_for_provider(
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        provider_kind: LLMProviderKind,
    ) -> Self {
        Self::with_client_for_provider(default_http_client(), api_key, base_url, provider_kind)
    }

    /// Creates an Anthropic-wire provider with a preconfigured HTTP client and provider identity.
    pub fn with_client_for_provider(
        client: Client,
        api_key: impl Into<String>,
        base_url: impl Into<String>,
        provider_kind: LLMProviderKind,
    ) -> Self {
        Self {
            client,
            api_key: api_key.into(),
            base_url: base_url.into(),
            provider_kind,
            default_timeout: Duration::from_secs(180),
        }
    }

    fn detect_capability(model: &str) -> LLMCapability {
        let mut capability = LLMCapability::default();
        capability.modalities = vec![LLMModality::Text];
        if model.contains("haiku") || model.contains("sonnet") {
            capability.modalities.push(LLMModality::Vision);
        }
        capability.tool_calling = true;
        capability.json_mode = false;
        capability.streaming = true;
        capability.computer_use = false;

        capability.reasoning = LLMReasoning::Standard;
        capability
    }

    fn map_messages(messages: &[LLMMessage]) -> LLMResult<(Option<String>, Vec<Value>)> {
        let mut anthropic_messages = Vec::new();
        let mut system_prompt: Option<String> = None;

        for message in messages {
            match message.role {
                MessageRole::System => {
                    let mut combined = String::new();
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                if !combined.is_empty() {
                                    combined.push('\n');
                                }
                                combined.push_str(text);
                            },
                            ContentBlock::Json { value } => {
                                if !combined.is_empty() {
                                    combined.push('\n');
                                }
                                combined.push_str(&value.to_string());
                            },
                            _ => {
                                return Err(LLMError::UnsupportedCapability(
                                    "Anthropic system prompt only supports text/json content"
                                        .to_string(),
                                ));
                            },
                        }
                    }
                    system_prompt = Some(match system_prompt {
                        Some(existing) => {
                            if combined.is_empty() {
                                existing
                            } else if existing.is_empty() {
                                combined
                            } else {
                                format!("{existing}\n{combined}")
                            }
                        },
                        None => combined,
                    });
                },
                MessageRole::User | MessageRole::Assistant | MessageRole::Tool => {
                    let role = match message.role {
                        MessageRole::User => "user",
                        MessageRole::Assistant => "assistant",
                        MessageRole::Tool => "user",
                        MessageRole::System => unreachable!(),
                    };

                    let mut content_items = Vec::new();
                    for block in &message.content {
                        content_items.push(Self::map_content_block(block)?);
                    }

                    anthropic_messages.push(json!({
                        "role": role,
                        "content": content_items,
                    }));
                },
            }
        }

        Ok((system_prompt, Self::pair_tool_uses(anthropic_messages)))
    }

    /// Every `tool_use` gets exactly one `tool_result` in the user message
    /// right after its assistant message, as Anthropic requires ("tool_use ids
    /// were found without tool_result blocks immediately after"). A later
    /// duplicate result for the same id there is dropped; a call with none
    /// (a parallel call that never ran, a refused or skipped call) is
    /// answered with [`super::MISSING_TOOL_RESULT_TEXT`] after the results
    /// that are present, in a new user message when no user message follows.
    /// A history whose calls are all answered once is returned unchanged.
    fn pair_tool_uses(messages: Vec<Value>) -> Vec<Value> {
        use std::collections::HashSet;

        fn role(message: &Value) -> Option<&str> {
            message.get("role").and_then(Value::as_str)
        }
        fn block_type(block: &Value) -> Option<&str> {
            block.get("type").and_then(Value::as_str)
        }

        let mut out: Vec<Value> = Vec::with_capacity(messages.len());
        let mut synthesized = 0usize;
        let mut dropped_duplicates = 0usize;
        let mut messages = messages.into_iter().peekable();
        while let Some(message) = messages.next() {
            let mut tool_use_ids: Vec<String> = Vec::new();
            if role(&message) == Some("assistant") {
                for block in message
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if block_type(block) == Some("tool_use") {
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            if !tool_use_ids.iter().any(|seen| seen == id) {
                                tool_use_ids.push(id.to_string());
                            }
                        }
                    }
                }
            }
            out.push(message);
            if tool_use_ids.is_empty() {
                continue;
            }

            let mut answer = match messages.next_if(|next| role(next) == Some("user")) {
                Some(next) => next,
                None => json!({ "role": "user", "content": [] }),
            };
            let text_content = answer
                .get("content")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            if let Some(text) = text_content {
                answer["content"] = json!([{ "type": "text", "text": text }]);
            }
            if let Some(content) = answer.get_mut("content").and_then(Value::as_array_mut) {
                let mut answered: HashSet<String> = HashSet::new();
                content.retain(|block| {
                    if block_type(block) != Some("tool_result") {
                        return true;
                    }
                    match block.get("tool_use_id").and_then(Value::as_str) {
                        Some(id) if !answered.insert(id.to_string()) => {
                            dropped_duplicates += 1;
                            false
                        },
                        _ => true,
                    }
                });
                let insert_at = content
                    .iter()
                    .rposition(|block| block_type(block) == Some("tool_result"))
                    .map_or(0, |index| index + 1);
                let missing: Vec<Value> = tool_use_ids
                    .iter()
                    .filter(|id| !answered.contains(id.as_str()))
                    .map(|id| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": id,
                            "content": super::MISSING_TOOL_RESULT_TEXT,
                            "is_error": true,
                        })
                    })
                    .collect();
                synthesized += missing.len();
                for (offset, block) in missing.into_iter().enumerate() {
                    content.insert(insert_at + offset, block);
                }
            }
            out.push(answer);
        }

        if synthesized > 0 || dropped_duplicates > 0 {
            warn!(
                provider = "anthropic_messages",
                synthesized_results = synthesized,
                dropped_duplicate_results = dropped_duplicates,
                "paired every tool_use with exactly one tool_result before sending"
            );
        }
        out
    }

    fn map_content_block(block: &ContentBlock) -> LLMResult<Value> {
        match block {
            ContentBlock::Text { text } => Ok(json!({ "type": "text", "text": text })),
            ContentBlock::Json { value } => match as_anthropic_raw_content_block(value) {
                Some(block) => Ok(block.clone()),
                None => Ok(json!({ "type": "text", "text": value.to_string() })),
            },
            ContentBlock::Image {
                data, media_type, ..
            } => Ok(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": media_type,
                    "data": BASE64.encode(data),
                }
            })),
            ContentBlock::ImageUrl { url, .. } => Ok(json!({
                "type": "image",
                "source": {
                    "type": "url",
                    "url": url,
                }
            })),
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => Ok(json!({
                "type": "tool_use",
                "id": id,
                "name": name,
                "input": Self::tool_use_input(arguments),
            })),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
            } => Ok(json!({
                "type": "tool_result",
                "tool_use_id": tool_call_id,
                "content": Self::map_tool_result_content(content),
            })),
        }
    }

    /// Anthropic rejects a `tool_use` whose `input` is not a JSON object. A
    /// call the model emitted with unparseable arguments is kept in history as
    /// its raw string, so replaying it verbatim fails every later turn of that
    /// conversation. Send an object that names the problem instead.
    fn tool_use_input(arguments: &Value) -> Value {
        const PREVIEW_CHARS: usize = 200;
        let preview = |raw: &str| -> Value {
            let head: String = raw.chars().take(PREVIEW_CHARS).collect();
            json!({
                "_unparsed_arguments": format!(
                    "{head}... ({} bytes, not a valid JSON object)",
                    raw.len()
                )
            })
        };
        match arguments {
            Value::Object(_) => arguments.clone(),
            Value::Null => json!({}),
            Value::String(raw) if raw.trim().is_empty() => json!({}),
            Value::String(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(parsed @ Value::Object(_)) => parsed,
                _ => preview(raw),
            },
            other => preview(&other.to_string()),
        }
    }

    fn map_tool_result_content(content: &Value) -> Value {
        // Rich `_magicllm_rich_tool_result` envelope → an Anthropic content-block
        // array (text / image blocks).
        if let Some(blocks) = content
            .get("_magicllm_rich_tool_result")
            .and_then(Value::as_bool)
            .filter(|enabled| *enabled)
            .and_then(|_| content.get("blocks"))
            .and_then(Value::as_array)
        {
            let content_items = blocks
                .iter()
                .filter_map(|block| match block.get("type").and_then(Value::as_str) {
                    Some("text") => block.get("text").and_then(Value::as_str).map(|text| {
                        json!({
                            "type": "text",
                            "text": text,
                        })
                    }),
                    Some("image") => {
                        let media_type = block.get("media_type").and_then(Value::as_str)?;
                        let data = block.get("data_base64").and_then(Value::as_str)?;
                        Some(json!({
                            "type": "image",
                            "source": {
                                "type": "base64",
                                "media_type": media_type,
                                "data": data,
                            }
                        }))
                    },
                    _ => None,
                })
                .collect::<Vec<_>>();
            if !content_items.is_empty() {
                return Value::Array(content_items);
            }
        }

        // Anthropic's `tool_result.content` accepts ONLY a string or an array of
        // content blocks — never a bare object / null / number. Claude tolerates a
        // bare object (it stringifies internally), but DeepSeek's stricter
        // Anthropic-compatible deserializer rejects it with "expected a string or a
        // list" (400). Normalize to spec: pass strings + arrays through unchanged;
        // serialize everything else (e.g. the `{"status":"deferred"}` placeholder
        // or a structured browser-state object from build_tool_result_payload) to a
        // JSON string.
        match content {
            Value::String(_) | Value::Array(_) => content.clone(),
            Value::Null => Value::String(String::new()),
            other => Value::String(other.to_string()),
        }
    }

    fn map_tools(tools: &[LLMToolSpec], cache_control: Option<&Value>) -> Vec<Value> {
        let mut mapped: Vec<Value> = tools
            .iter()
            .map(|tool| {
                let (description, parameters) =
                    crate::tool_schema::project_whole_call_constraints(tool);
                json!({
                    "name": tool.name,
                    "description": description,
                    "input_schema": parameters,
                })
            })
            .collect();

        // Anthropic prompt caching for tool definitions: attach
        // `cache_control: ephemeral` to the LAST tool object. The server
        // caches everything up to and including that tool as a stable
        // prefix. With ~50 outer-loop tools and full JSON schemas this is
        // ~10K tokens of repeat input per iteration that drops to the
        // cache-read rate (~10% of full input cost on Claude). Without
        // this marker tools are billed at full input rates every turn.
        // Docs: https://docs.anthropic.com/en/docs/build-with-claude/prompt-caching#caching-tool-definitions
        if let Some(cc) = cache_control {
            if let Some(last) = mapped.last_mut() {
                if let Some(obj) = last.as_object_mut() {
                    obj.insert("cache_control".to_string(), cc.clone());
                }
            }
        }

        mapped
    }

    /// Insert the `tools` / `tool_choice` pair into an Anthropic request
    /// body, or the server-side web search tool when the request enables it.
    ///
    /// The two are mutually exclusive: the `tool_choice` default is
    /// meaningless next to a server tool, and mixing client tools with
    /// server-side search makes spend attribution ambiguous.
    fn attach_tools_or_server_web_search(
        &self,
        body: &mut Map<String, Value>,
        request: &LLMRequest,
        explicit_cache_control: Option<&Value>,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_invalid_flag("anthropic_messages", request)?;
        if crate::server_web_search::requested(request) {
            if self.provider_kind != LLMProviderKind::Anthropic {
                return Err(LLMError::UnsupportedCapability(format!(
                    "{}: this Anthropic-compatible transport does not support the \
                     server-side web search tool; only the native Anthropic transport \
                     honors `server_web_search`",
                    self.provider_kind
                )));
            }
            crate::server_web_search::reject_mixed_with_function_tools(
                "anthropic_messages",
                request,
            )?;
            body.insert(
                "tools".to_string(),
                Value::Array(vec![crate::server_web_search::anthropic_web_search_tool(
                    request,
                )]),
            );
            return Ok(());
        }
        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools, explicit_cache_control)),
            );
            body.insert(
                "tool_choice".to_string(),
                Self::tool_choice_for_request(request),
            );
        }
        Ok(())
    }

    /// Anthropic takes `tool_choice` only as an object. Catalog routes carry
    /// the OpenAI spelling (`"auto"`, `"required"`, `{"type":"function"}`),
    /// so it is translated here instead of reaching the API as a 400.
    fn tool_choice_for_request(request: &LLMRequest) -> Value {
        let Some(choice) = request.extra.as_ref().and_then(|e| e.get("tool_choice")) else {
            return json!({"type": "auto"});
        };
        match choice {
            Value::String(mode) => match mode.as_str() {
                "none" => json!({"type": "none"}),
                "required" | "any" => json!({"type": "any"}),
                _ => json!({"type": "auto"}),
            },
            Value::Object(object) if object.get("type") == Some(&json!("function")) => match choice
                .pointer("/function/name")
                .or_else(|| object.get("name"))
            {
                Some(name) => json!({"type": "tool", "name": name}),
                None => json!({"type": "any"}),
            },
            other => other.clone(),
        }
    }

    fn map_reasoning(reasoning: &ReasoningConfig) -> Value {
        let mut thinking = Map::new();
        // Anthropic extended thinking API requires:
        // - type: "enabled" (required)
        // - budget_tokens: number (required when type is enabled)
        // See: https://docs.anthropic.com/en/docs/build-with-claude/extended-thinking
        thinking.insert("type".to_string(), Value::String("enabled".to_string()));

        // Use max_reasoning_tokens if specified, otherwise default to 10000
        let budget = reasoning.max_reasoning_tokens.unwrap_or(10000);
        thinking.insert("budget_tokens".to_string(), Value::Number(budget.into()));

        Value::Object(thinking)
    }

    /// Callers set sampling per operation, not per model, so a model that
    /// rejects it gets the request without it rather than a 400.
    fn insert_sampling(
        body: &mut Map<String, Value>,
        model: &str,
        wire: &ModelWire,
        request: &LLMRequest,
    ) {
        if !wire.sampling {
            if request.temperature.is_some() || request.top_p.is_some() {
                debug!(model, "omitting sampling controls the model rejects");
            }
            return;
        }
        if let Some(temp) = request.temperature {
            body.insert("temperature".to_string(), Value::from(temp));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
    }

    fn provider_supports_disabled_thinking(
        provider_kind: &LLMProviderKind,
        wire: &ModelWire,
    ) -> bool {
        *provider_kind == LLMProviderKind::DeepSeek || wire.thinking_off
    }

    fn disabled_thinking_config() -> Value {
        json!({ "type": "disabled" })
    }

    fn enabled_thinking_config() -> Value {
        json!({ "type": "enabled" })
    }

    fn adaptive_thinking_config() -> Value {
        json!({ "type": "adaptive", "display": "summarized" })
    }

    fn claude_output_config(reasoning: &ReasoningConfig) -> Option<Value> {
        let effort = reasoning.effort.as_deref()?;
        if ReasoningConfig::effort_disables_reasoning(effort) {
            return None;
        }

        let effort = match effort.trim().to_ascii_lowercase().as_str() {
            "max" => "max",
            "xhigh" | "extra_high" | "extra-high" => "xhigh",
            "high" => "high",
            "medium" | "med" => "medium",
            "low" | "minimal" | "min" => "low",
            _ => return None,
        };

        Some(json!({ "effort": effort }))
    }

    /// Native Messages structured output shares output_config with effort.
    /// Preserve the caller's exact schema; unsupported constraints are an API
    /// error, never silently weakened by this transport.
    fn attach_response_format(
        &self,
        body: &mut Map<String, Value>,
        request: &LLMRequest,
    ) -> LLMResult<()> {
        let schema = match request.response_format_value() {
            None | Some(LLMResponseFormat::Text) => return Ok(()),
            Some(LLMResponseFormat::JsonObject) => {
                return Err(LLMError::UnsupportedCapability(
                    "Anthropic Messages JSON output requires an explicit JsonSchema".into(),
                ));
            },
            Some(LLMResponseFormat::JsonSchema { schema }) => schema,
        };
        if self.provider_kind != LLMProviderKind::Anthropic {
            return Err(LLMError::UnsupportedCapability(
                "this Anthropic-compatible transport has no qualified structured-output mapping"
                    .into(),
            ));
        }
        let output_config = body
            .entry("output_config".to_string())
            .or_insert_with(|| json!({}));
        let output_config = output_config.as_object_mut().ok_or_else(|| {
            LLMError::Validation("Anthropic output_config must be an object".into())
        })?;
        output_config.insert(
            "format".to_string(),
            json!({
                "type": "json_schema",
                "schema": clone_json_value_iteratively(schema),
            }),
        );
        Ok(())
    }

    fn deepseek_output_config(reasoning: &ReasoningConfig) -> Value {
        let effort = reasoning
            .effort
            .as_deref()
            .map(|effort| match effort.trim().to_ascii_lowercase().as_str() {
                "low" | "minimal" => "low",
                "max" | "xhigh" | "extra_high" | "extra-high" => "max",
                _ => "high",
            })
            .unwrap_or("high");

        json!({ "effort": effort })
    }

    fn insert_provider_reasoning_config(
        provider_kind: &LLMProviderKind,
        model: &str,
        wire: &ModelWire,
        body: &mut Map<String, Value>,
        reasoning: &ReasoningConfig,
    ) -> LLMResult<()> {
        if reasoning.is_disabled() {
            if !Self::provider_supports_disabled_thinking(provider_kind, wire) {
                return Err(LLMError::UnsupportedCapability(format!(
                    "{model} does not support disabling thinking"
                )));
            }
            body.insert("thinking".to_string(), Self::disabled_thinking_config());
            if *provider_kind != LLMProviderKind::DeepSeek {
                if let Some(effort) = wire.thinking_off_effort.as_deref() {
                    // The model rejects disabled + xhigh/max, including an
                    // inherited effort. Off gets a valid, explicit effort.
                    body.insert("output_config".to_string(), json!({ "effort": effort }));
                }
            }
            return Ok(());
        }
        match provider_kind {
            LLMProviderKind::DeepSeek => {
                body.insert("thinking".to_string(), Self::enabled_thinking_config());
                body.entry("output_config".to_string())
                    .or_insert_with(|| Self::deepseek_output_config(reasoning));
            },
            _ if wire.thinking == Some(ThinkingWire::Adaptive) => {
                body.insert("thinking".to_string(), Self::adaptive_thinking_config());
                if let Some(output_config) = Self::claude_output_config(reasoning) {
                    body.entry("output_config".to_string())
                        .or_insert(output_config);
                }
            },
            _ => {
                body.insert("thinking".to_string(), Self::map_reasoning(reasoning));
            },
        }
        Ok(())
    }

    fn ensure_default_no_thinking(
        provider_kind: &LLMProviderKind,
        body: &mut Map<String, Value>,
        reasoning: Option<&ReasoningConfig>,
    ) {
        if body.contains_key("thinking") || *provider_kind != LLMProviderKind::DeepSeek {
            return;
        }

        let should_disable = reasoning
            .map(|reasoning| reasoning.is_disabled())
            .unwrap_or(true);
        if should_disable {
            body.insert("thinking".to_string(), Self::disabled_thinking_config());
        }
    }

    fn map_metadata(_metadata: &RequestMetadata) -> Option<Value> {
        // Anthropic API only accepts `user_id` in metadata.
        // Custom fields (operation, trace_id, tags) are NOT sent to Anthropic API.
        // They are logged locally via tracing but not included in the API request.
        // See: https://docs.anthropic.com/en/api/messages
        //
        // Note: If user_id tracking is needed in the future, add a user_id field
        // to RequestMetadata and include it here:
        // if let Some(user_id) = metadata.user_id.as_ref() {
        //     let mut object = Map::new();
        //     object.insert("user_id".to_string(), Value::String(user_id.clone()));
        //     return Some(Value::Object(object));
        // }
        None
    }

    /// Check if a field is config-only metadata that should NOT be sent to the Anthropic API.
    /// These fields are used internally for routing, budget tracking, and fallback logic.
    fn is_config_only_field(key: &str) -> bool {
        matches!(
            key,
            "cost_per_observation"
                // Magician pricing hints — centralized pricing lives in
                // magicllm::pricing; these YAML keys are documentation/legacy
                // only and must NOT leak to Anthropic's strict body validator.
                | "cost_per_million_input_tokens"
                | "cost_per_million_output_tokens"
                | "fallback_profile"
                | "reasoning_strategy"
                | "reasoning_max_tokens"
                | "reasoning_summary" // OpenAI Responses-only knob (auto/concise/detailed); Anthropic has no equivalent.
                | "use_chat"
                | "use_responses"
                | "openai_api_mode"
                | "openai_responses_disable_chaining"
                | "gemini_api_mode"
                | "openai_previous_response_id" // OpenAI Responses chain id, not Anthropic
                | "verbosity" // Handled separately or not supported by Anthropic
                | "streaming" // Router-internal gate, not an API parameter
                | "viewport" // Magician viewport hint, not an API parameter
                | "disable_tools" // Yutori-only filter, not an Anthropic parameter
                | "disable_yutori_builtins"
                | "router_provider_override" // magicllm router pin, consumed before provider
                | "router_profile_override"
                | "router_preserve_model"
                | "max_tokens_retry_attempt"
                | "server_web_search" // consumed by attach_tools_or_server_web_search
                | "tool_choice" // handled separately by Anthropic mapper
        )
    }

    fn should_forward_extra(key: &str, protected_request: bool) -> bool {
        !Self::is_config_only_field(key) && !(protected_request && key == "cache_control")
    }

    fn build_ephemeral_cache_control(ttl: Option<&str>) -> Value {
        let mut cache_control = Map::new();
        cache_control.insert("type".to_string(), Value::String("ephemeral".to_string()));
        if let Some(ttl) = ttl {
            cache_control.insert("ttl".to_string(), Value::String(ttl.to_string()));
        }
        Value::Object(cache_control)
    }

    fn prompt_cache_control(&self, request: &LLMRequest) -> Option<Value> {
        // DeepSeek's Anthropic-compatible endpoint accepts these fields but
        // explicitly ignores them. Rely on its automatic prefix cache instead
        // so request telemetry does not claim that an explicit write was
        // requested.
        if self.provider_kind == LLMProviderKind::DeepSeek {
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
            None => Some(Self::build_ephemeral_cache_control(None)),
            Some(_) => None,
        }
    }

    /// A rolling request anchors the last completed turn as well as the
    /// stable prefix. An agentic loop asks for it through its context-reuse
    /// plan; a chat, which carries no reuse plan, asks through a conversation
    /// prompt cache. Without it a chat cached only tools + system and paid
    /// full price for its whole history on every call.
    fn uses_rolling_prefix(&self, request: &LLMRequest) -> bool {
        self.provider_kind == LLMProviderKind::Anthropic
            && (request
                .context_reuse
                .as_ref()
                .is_some_and(|reuse| reuse.rolling_prefix)
                || request
                    .prompt_cache
                    .as_ref()
                    .is_some_and(PromptCacheConfig::is_conversation))
    }

    /// Convert a text-only user message to an Anthropic content payload,
    /// splitting on the magicllm cache-breakpoint sentinel when present and
    /// `cache_control_for_user` is `Some(...)`. Returns either a plain-string
    /// content (single block) or a `Value::Array` of two content-block objects
    /// with cache_control on the first.
    fn user_text_content(text: String, cache_control_for_user: Option<&Value>) -> Value {
        let (prefix, suffix) = crate::types::split_on_cache_sentinel(&text);
        match (suffix, cache_control_for_user) {
            // A marker that ends the text marks the whole block; an empty
            // second text block is rejected by the API.
            (Some(suffix), Some(cc)) if !prefix.is_empty() && suffix.is_empty() => {
                let mut only = Map::new();
                only.insert("type".to_string(), Value::String("text".to_string()));
                only.insert("text".to_string(), Value::String(prefix));
                only.insert("cache_control".to_string(), cc.clone());
                Value::Array(vec![Value::Object(only)])
            },
            (Some(suffix), Some(cc)) if !prefix.is_empty() => {
                let mut first = Map::new();
                first.insert("type".to_string(), Value::String("text".to_string()));
                first.insert("text".to_string(), Value::String(prefix));
                first.insert("cache_control".to_string(), cc.clone());
                let mut second = Map::new();
                second.insert("type".to_string(), Value::String("text".to_string()));
                second.insert("text".to_string(), Value::String(suffix));
                Value::Array(vec![Value::Object(first), Value::Object(second)])
            },
            (Some(suffix), _) => {
                // Sentinel present but caching disabled or prefix empty:
                // reconnect stripped halves and emit a plain string. Preserve
                // the line boundary that originally separated them — a naive
                // concat would glue unrelated sections together as a single
                // word.
                Value::String(if prefix.is_empty() {
                    suffix
                } else if suffix.is_empty() {
                    prefix
                } else {
                    format!("{prefix}\n{suffix}")
                })
            },
            (None, _) => Value::String(prefix),
        }
    }

    /// Strip the cache-breakpoint sentinel from a text payload and reconnect
    /// the halves with a single `\n` when both are non-empty. No-op when the
    /// sentinel is absent. Mirrors the reconnection rule in
    /// [`Self::user_text_content`]'s middle branch.
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

    /// Walk `messages` (already produced by `map_messages`) and rewrite text
    /// content to strip the Magician cache-breakpoint sentinel, applying the
    /// two-content-block split only for user messages when caching is enabled
    /// (see [`Self::user_text_content`] for the split rule).
    ///
    /// Defense-in-depth: every message role has its text blocks stripped of
    /// the sentinel, not just user. Today only user-facing templates carry the
    /// sentinel, but a future system/assistant/tool message would otherwise
    /// leak the internal marker to the model. The SPLIT behaviour stays
    /// user-only — non-user messages just get the sentinel stripped and their
    /// halves reconnected with `\n`.
    ///
    /// Mixed / image-bearing content is walked in place; individual text
    /// blocks within the content array are rewritten, non-text blocks are
    /// left alone.
    fn rewrite_user_text_content(messages: &mut [Value], cache_control_for_user: Option<&Value>) {
        for message in messages.iter_mut() {
            let is_user = message.get("role").and_then(Value::as_str) == Some("user");
            // Fast path: single-text-block user message + caching enabled ⇒
            // apply the split-or-reconnect rule via `user_text_content`.
            if is_user {
                if let Some(content_array) = message.get("content").and_then(Value::as_array) {
                    if content_array.len() == 1
                        && content_array[0].get("type").and_then(Value::as_str) == Some("text")
                    {
                        if let Some(text) = content_array[0].get("text").and_then(Value::as_str) {
                            let rewritten =
                                Self::user_text_content(text.to_string(), cache_control_for_user);
                            if let Some(obj) = message.as_object_mut() {
                                obj.insert("content".to_string(), rewritten);
                            }
                            continue;
                        }
                    }
                }
            }

            // Multi-block user content (e.g. SoM / vision path: [text, image]):
            // when caching is enabled and the FIRST sentinel-bearing text block
            // has a non-empty prefix, split it in-place into two text blocks
            // with cache_control on the prefix. All other blocks (images,
            // tool_use, tool_result, subsequent text blocks) stay in their
            // original positions — only the sentinel-bearing text block at its
            // original index is replaced by (prefix_text, suffix_text).
            //
            // cache_control is only attached when the sentinel-bearing text
            // block is at index 0. Anthropic caches everything up to and
            // including the cache_control block, so if any prior block is
            // volatile (e.g. an image whose bytes change per iteration), the
            // anchor would never hit — do the split for sentinel-stripping
            // correctness but skip the useless cache_control attachment.
            //
            // Only the FIRST sentinel-bearing text block gets split; any
            // subsequent sentinel-bearing text blocks fall through to the
            // defensive strip pass below.
            if is_user && cache_control_for_user.is_some() {
                let split_index =
                    message
                        .get("content")
                        .and_then(Value::as_array)
                        .and_then(|arr| {
                            arr.iter().enumerate().find_map(|(idx, block)| {
                                if block.get("type").and_then(Value::as_str) != Some("text") {
                                    return None;
                                }
                                let text = block.get("text").and_then(Value::as_str)?;
                                if text.contains(crate::types::CACHE_BREAKPOINT_SENTINEL) {
                                    Some(idx)
                                } else {
                                    None
                                }
                            })
                        });

                if let Some(idx) = split_index {
                    let cc = cache_control_for_user.expect("checked is_some above");
                    if let Some(content_array) =
                        message.get_mut("content").and_then(Value::as_array_mut)
                    {
                        let original_text = content_array[idx]
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        let (prefix, suffix_opt) =
                            crate::types::split_on_cache_sentinel(&original_text);
                        // `split_index` only matches when sentinel is present,
                        // so `suffix_opt` is always `Some` here.
                        let suffix = suffix_opt.unwrap_or_default();

                        let replacement: Vec<Value> = match (prefix.is_empty(), suffix.is_empty()) {
                            (true, true) => {
                                // Sentinel-only text block: collapse to an
                                // empty text block. Shouldn't happen in
                                // practice but keep the shape legal.
                                let mut block = Map::new();
                                block.insert("type".to_string(), Value::String("text".to_string()));
                                block.insert("text".to_string(), Value::String(String::new()));
                                vec![Value::Object(block)]
                            },
                            (true, false) => {
                                // Prefix empty: single suffix block, no
                                // cache_control (nothing to anchor on).
                                let mut block = Map::new();
                                block.insert("type".to_string(), Value::String("text".to_string()));
                                block.insert("text".to_string(), Value::String(suffix));
                                vec![Value::Object(block)]
                            },
                            (false, true) => {
                                // Suffix empty: single prefix block, no
                                // cache_control (no tokens after to
                                // benefit from the anchor).
                                let mut block = Map::new();
                                block.insert("type".to_string(), Value::String("text".to_string()));
                                block.insert("text".to_string(), Value::String(prefix));
                                vec![Value::Object(block)]
                            },
                            (false, false) => {
                                // Normal split: prefix (maybe + cc) +
                                // suffix. Attach cache_control only when
                                // the sentinel block is at index 0; any
                                // prior block could be volatile (image
                                // bytes change per iter) and would defeat
                                // the cache anchor.
                                let mut first = Map::new();
                                first.insert("type".to_string(), Value::String("text".to_string()));
                                first.insert("text".to_string(), Value::String(prefix));
                                if idx == 0 {
                                    first.insert("cache_control".to_string(), cc.clone());
                                }
                                let mut second = Map::new();
                                second
                                    .insert("type".to_string(), Value::String("text".to_string()));
                                second.insert("text".to_string(), Value::String(suffix));
                                vec![Value::Object(first), Value::Object(second)]
                            },
                        };
                        content_array.splice(idx..=idx, replacement);
                    }
                }
            }

            // Defensive strip for every other shape: walk the content array
            // and rewrite any text block that still contains the sentinel
            // (the split pass above consumed the FIRST sentinel-bearing block
            // for user messages with caching enabled; everything else —
            // subsequent sentinel-bearing text blocks, non-user roles, or
            // caching disabled — is handled here).
            let Some(content_array) = message.get_mut("content").and_then(Value::as_array_mut)
            else {
                continue;
            };
            for block in content_array.iter_mut() {
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
    }

    /// Anchor the moving prompt-cache breakpoint on the last completed turn:
    /// the last block that can carry `cache_control` in the last message
    /// BEFORE the final one.
    ///
    /// A single top-level `cache_control` — "cache the whole prompt, look
    /// back for a hit" — only pays off when each request is the previous one
    /// plus appended turns. An agentic decision loop is not that: every
    /// iteration ends with a re-rendered observation prompt that replaces the
    /// previous one, so the entry written at the end of request N is never a
    /// prefix of request N+1. A Fable 5.1 run wrote 30–50k tokens on each of
    /// nine decisions and read none back. The conversation up to the last
    /// completed turn IS a prefix of the next request, so that is where the
    /// breakpoint goes; the next iteration reads it and extends it.
    ///
    /// `thinking` / `redacted_thinking` blocks cannot carry a breakpoint, so
    /// the walk skips them (and whole messages made only of them). Returns
    /// `true` when a block took the breakpoint.
    fn anchor_last_completed_turn(messages: &mut [Value], cache_control: Option<&Value>) -> bool {
        let Some(cc) = cache_control else {
            return false;
        };
        let len = messages.len();
        if len < 2 {
            return false;
        }
        for message in messages[..len - 1].iter_mut().rev() {
            let Some(obj) = message.as_object_mut() else {
                continue;
            };
            let Some(content) = obj.get_mut("content") else {
                continue;
            };
            match content {
                Value::String(text) => {
                    let mut block = Map::new();
                    block.insert("type".to_string(), Value::String("text".to_string()));
                    block.insert("text".to_string(), Value::String(std::mem::take(text)));
                    block.insert("cache_control".to_string(), cc.clone());
                    *content = Value::Array(vec![Value::Object(block)]);
                    return true;
                },
                Value::Array(blocks) => {
                    let anchor = blocks.iter_mut().rev().find(|block| {
                        !matches!(
                            block.get("type").and_then(Value::as_str),
                            Some("thinking") | Some("redacted_thinking")
                        )
                    });
                    if let Some(Value::Object(block)) = anchor {
                        block.insert("cache_control".to_string(), cc.clone());
                        return true;
                    }
                },
                _ => {},
            }
        }
        false
    }

    /// Rewrite user sentinels and place the prompt-cache breakpoints for one
    /// request. Ordinary requests split user sentinels into a cached prefix
    /// block. A rolling (agentic) request treats sentinels as strip-only and
    /// anchors the last completed turn, a second block ~16 back, and — when
    /// the final message's text ends with the sentinel — that final block.
    /// The last one is the caller saying "this turn's text is kept verbatim in
    /// the conversation": written now at 1.25x and read next call at 0.1x,
    /// instead of billed in full now and written next call. Returns whether
    /// the turn anchor was placed.
    fn apply_cache_anchors(
        messages: &mut [Value],
        rolling_prefix: bool,
        cache_control: Option<&Value>,
    ) -> bool {
        let final_marked = if rolling_prefix {
            Self::final_marked_block(messages)
        } else {
            None
        };
        Self::rewrite_user_text_content(
            messages,
            if rolling_prefix { None } else { cache_control },
        );
        let anchored_on_turn =
            rolling_prefix && Self::anchor_last_completed_turn(messages, cache_control);
        if anchored_on_turn {
            Self::anchor_lookback_block(messages, cache_control);
        }
        if let (Some(block), Some(cc)) = (final_marked, cache_control) {
            Self::anchor_final_block(messages, block, cc);
        }
        anchored_on_turn
    }

    /// The index of the last text block of the final user message whose text
    /// ends with the cache sentinel, before the sentinel is stripped.
    fn final_marked_block(messages: &[Value]) -> Option<usize> {
        let last = messages.last()?;
        if last.get("role").and_then(Value::as_str) != Some("user") {
            return None;
        }
        let marked = |text: &str| {
            text.trim_end()
                .ends_with(crate::types::CACHE_BREAKPOINT_SENTINEL)
        };
        match last.get("content")? {
            Value::String(text) => marked(text).then_some(0),
            Value::Array(blocks) => blocks.iter().rposition(|block| {
                block.get("type").and_then(Value::as_str) == Some("text")
                    && block
                        .get("text")
                        .and_then(Value::as_str)
                        .is_some_and(marked)
            }),
            _ => None,
        }
    }

    fn anchor_final_block(messages: &mut [Value], index: usize, cache_control: &Value) {
        let Some(content) = messages
            .last_mut()
            .and_then(|last| last.as_object_mut())
            .and_then(|obj| obj.get_mut("content"))
        else {
            return;
        };
        if let Value::String(text) = content {
            let mut block = Map::new();
            block.insert("type".to_string(), Value::String("text".to_string()));
            block.insert("text".to_string(), Value::String(std::mem::take(text)));
            *content = Value::Array(vec![Value::Object(block)]);
        }
        if let Some(Value::Object(block)) = content.as_array_mut().and_then(|b| b.get_mut(index)) {
            block.insert("cache_control".to_string(), cache_control.clone());
        }
    }

    /// Anthropic rejects a request with more than four `cache_control` blocks.
    const MAX_CACHE_BREAKPOINTS: usize = 4;

    /// The tools' own breakpoint. The system breakpoint caches the tools too
    /// (they come first), but only while the system prompt is unchanged; the
    /// tools' own entry still reads back when it changes. A rolling request
    /// with a system prompt gives it up only when system, the last completed
    /// turn, the lookback block and the final block have taken all four
    /// slots. `body` already holds the system prompt and messages.
    fn tools_cache_control<'a>(
        rolling_prefix: bool,
        anchored_on_system: bool,
        body: &Map<String, Value>,
        cache_control: Option<&'a Value>,
    ) -> Option<&'a Value> {
        if rolling_prefix
            && anchored_on_system
            && Self::cache_breakpoints_in(body) >= Self::MAX_CACHE_BREAKPOINTS
        {
            None
        } else {
            cache_control
        }
    }

    /// `cache_control` blocks already placed in the system prompt and messages.
    fn cache_breakpoints_in(body: &Map<String, Value>) -> usize {
        let marked = |blocks: &Value| {
            blocks.as_array().map_or(0, |blocks| {
                blocks
                    .iter()
                    .filter(|block| block.get("cache_control").is_some())
                    .count()
            })
        };
        let in_system = body.get("system").map_or(0, marked);
        let in_messages = body
            .get("messages")
            .and_then(Value::as_array)
            .map_or(0, |messages| {
                messages
                    .iter()
                    .filter_map(|message| message.get("content"))
                    .map(marked)
                    .sum()
            });
        in_system + in_messages
    }

    /// Content blocks between the last-completed-turn breakpoint and a second
    /// one further back. Anthropic looks back only about 20 blocks from a
    /// breakpoint for an earlier cache entry; a decision loop can append more
    /// than that between two model calls (a structured-decision gate's steps
    /// and their act→observe snapshots, each an assistant/user pair), and the
    /// entry the previous call wrote then fell out of reach — a Calculator run
    /// read back only the system + stable prompt (28K of 63K) after six gated
    /// steps. A second breakpoint this many blocks back extends the reach to
    /// ~36 blocks.
    const LOOKBACK_ANCHOR_BLOCKS: usize = 16;

    /// Put the second rolling breakpoint `LOOKBACK_ANCHOR_BLOCKS` cacheable
    /// blocks before the one `anchor_last_completed_turn` placed. No-op when
    /// the conversation is not that long. Walks the same blocks the turn
    /// anchor may take (not `thinking` / `redacted_thinking`).
    fn anchor_lookback_block(messages: &mut [Value], cache_control: Option<&Value>) -> bool {
        let Some(cc) = cache_control else {
            return false;
        };
        let len = messages.len();
        if len < 2 {
            return false;
        }
        let mut passed_turn_anchor = false;
        let mut counted = 0usize;
        for message in messages[..len - 1].iter_mut().rev() {
            let Some(obj) = message.as_object_mut() else {
                continue;
            };
            let Some(content) = obj.get_mut("content") else {
                continue;
            };
            if let Value::String(text) = content {
                let mut block = Map::new();
                block.insert("type".to_string(), Value::String("text".to_string()));
                block.insert("text".to_string(), Value::String(std::mem::take(text)));
                *content = Value::Array(vec![Value::Object(block)]);
            }
            let Value::Array(blocks) = content else {
                continue;
            };
            for block in blocks.iter_mut().rev() {
                if matches!(
                    block.get("type").and_then(Value::as_str),
                    Some("thinking") | Some("redacted_thinking")
                ) {
                    continue;
                }
                if !passed_turn_anchor {
                    passed_turn_anchor = block.get("cache_control").is_some();
                    continue;
                }
                counted += 1;
                if counted == Self::LOOKBACK_ANCHOR_BLOCKS {
                    if let Value::Object(block) = block {
                        block.insert("cache_control".to_string(), cc.clone());
                        return true;
                    }
                    return false;
                }
            }
        }
        false
    }

    /// Attach the system prompt to the request body, anchoring the prompt-cache
    /// breakpoint on the system content block when caching is enabled.
    ///
    /// Anthropic's API keys cache entries by the prefix up to and including the
    /// block marked with `cache_control`. Attaching the breakpoint to the
    /// system block caches the stable `tools + system` prefix so that per-iter
    /// volatile user messages don't invalidate it — which is the common case
    /// for agentic decision loops.
    ///
    /// Returns `true` when the system block consumed the `cache_control` value,
    /// so the caller can skip emitting a redundant top-level `cache_control`.
    fn attach_system_with_cache_control(
        body: &mut Map<String, Value>,
        system_prompt: Option<String>,
        cache_control: Option<&Value>,
    ) -> bool {
        let Some(system) = system_prompt else {
            return false;
        };
        if system.is_empty() {
            return false;
        }
        // Defense-in-depth: strip any cache-breakpoint sentinel from the
        // system prompt before sending. System prompts bypass
        // `rewrite_user_text_content`, so a sentinel embedded in a system
        // template would otherwise leak verbatim to Anthropic.
        let system = Self::strip_sentinel_text(&system);
        match cache_control {
            Some(cc) => {
                let mut block = Map::new();
                block.insert("type".to_string(), Value::String("text".to_string()));
                block.insert("text".to_string(), Value::String(system));
                block.insert("cache_control".to_string(), cc.clone());
                body.insert(
                    "system".to_string(),
                    Value::Array(vec![Value::Object(block)]),
                );
                true
            },
            None => {
                body.insert("system".to_string(), Value::String(system));
                false
            },
        }
    }

    fn should_retry_max_tokens(response: &LLMResponse) -> bool {
        response.finish_reason.as_deref() == Some("max_tokens")
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
            .min(ANTHROPIC_MAX_TOKENS_RETRY_CAP)
    }

    fn backed_off_max_output_tokens(
        base_max_output_tokens: u32,
        attempted_retry_max_output_tokens: u32,
    ) -> Option<u32> {
        let backed_off = base_max_output_tokens
            .saturating_add(base_max_output_tokens / 2)
            .min(attempted_retry_max_output_tokens.saturating_sub(1))
            .min(ANTHROPIC_MAX_TOKENS_RETRY_CAP);
        if backed_off > base_max_output_tokens && backed_off < attempted_retry_max_output_tokens {
            Some(backed_off)
        } else {
            None
        }
    }

    fn map_usage(provider_kind: &LLMProviderKind, data: &Value) -> LLMResult<TokenUsage> {
        if provider_kind == &LLMProviderKind::DeepSeek {
            let cache_read = bounded_usage_counter(
                "deepseek",
                "prompt_cache_hit_tokens",
                data.get("prompt_cache_hit_tokens").and_then(Value::as_u64),
            )?
            .unwrap_or(0);
            let cache_miss = bounded_usage_counter(
                "deepseek",
                "prompt_cache_miss_tokens",
                data.get("prompt_cache_miss_tokens").and_then(Value::as_u64),
            )?
            .unwrap_or(0);
            let reported_input = bounded_usage_counter(
                "deepseek",
                "input_tokens",
                data.get("input_tokens").and_then(Value::as_u64),
            )?
            .unwrap_or(0);
            let prompt_total = if cache_read > 0 || cache_miss > 0 {
                checked_usage_sum("deepseek", "prompt_tokens", &[cache_read, cache_miss])?
            } else {
                reported_input
            };
            let completion = bounded_usage_counter(
                "deepseek",
                "output_tokens",
                data.get("output_tokens").and_then(Value::as_u64),
            )?
            .unwrap_or(0);
            let total = checked_usage_sum("deepseek", "total_tokens", &[prompt_total, completion])?;

            return Ok(TokenUsage {
                prompt_tokens: Some(prompt_total),
                completion_tokens: Some(completion),
                total_tokens: Some(total),
                reasoning_tokens: None,
                cached_tokens: Some(cache_read),
                cache_creation_tokens: Some(0),
            });
        }

        let cache_creation = bounded_usage_counter(
            "anthropic_messages",
            "cache_creation_input_tokens",
            data.get("cache_creation_input_tokens")
                .and_then(Value::as_u64),
        )?
        .unwrap_or(0);
        let cache_read = bounded_usage_counter(
            "anthropic_messages",
            "cache_read_input_tokens",
            data.get("cache_read_input_tokens").and_then(Value::as_u64),
        )?
        .unwrap_or(0);
        let uncached_input = bounded_usage_counter(
            "anthropic_messages",
            "input_tokens",
            data.get("input_tokens").and_then(Value::as_u64),
        )?
        .unwrap_or(0);
        let prompt_total = checked_usage_sum(
            "anthropic_messages",
            "prompt_tokens",
            &[uncached_input, cache_creation, cache_read],
        )?;
        let completion = bounded_usage_counter(
            "anthropic_messages",
            "output_tokens",
            data.get("output_tokens").and_then(Value::as_u64),
        )?;
        let total = checked_usage_sum(
            "anthropic_messages",
            "total_tokens",
            &[prompt_total, completion.unwrap_or(0)],
        )?;

        Ok(TokenUsage {
            prompt_tokens: Some(prompt_total),
            completion_tokens: completion,
            total_tokens: Some(total),
            reasoning_tokens: None,
            cached_tokens: Some(cache_read),
            cache_creation_tokens: Some(cache_creation),
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
        let mut tool_results = Vec::new();
        let mut aggregated_text = Vec::new();
        // Anthropic Extended Thinking (and DeepSeek V4 via the Anthropic-
        // compat endpoint) emits chain-of-thought as `thinking` content
        // blocks. The raw block stays in `messages` so it can be
        // round-tripped on subsequent turns (Anthropic requires this for
        // chained extended thinking — the signature must survive). We
        // separately concatenate the text into `reasoning_text` so the
        // trace `assistant_turn` event has a readable reasoning channel
        // alongside `text`. Without this the model's chain-of-thought is
        // billed but invisible to debugging.
        let mut aggregated_reasoning = Vec::new();

        if let Some(content) = payload.get("content").and_then(Value::as_array) {
            let mut message_blocks = Vec::new();
            for block in content {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = block.get("text").and_then(Value::as_str) {
                            aggregated_text.push(text.to_string());
                            message_blocks.push(ContentBlock::Text {
                                text: text.to_string(),
                            });
                        }
                    },
                    Some("tool_use") => {
                        if let Some(id) = block.get("id").and_then(Value::as_str) {
                            let name = block
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            let input = match block.get("input") {
                                Some(input) => clone_admitted_tool_argument(input)?,
                                None => Value::Null,
                            };
                            let call = LLMToolCall {
                                id: id.to_string(),
                                name: name.clone(),
                                arguments: clone_json_value_iteratively(&input),
                            };
                            tool_calls.push(call.clone());
                            message_blocks.push(ContentBlock::ToolCall {
                                id: call.id,
                                name: call.name,
                                arguments: call.arguments,
                            });
                        }
                    },
                    Some("tool_result") => {
                        if let Some(tool_use_id) = block.get("tool_use_id").and_then(Value::as_str)
                        {
                            let content_value = block
                                .get("content")
                                .map(clone_json_value_iteratively)
                                .unwrap_or(Value::Null);
                            tool_results.push(LLMToolResult {
                                tool_call_id: tool_use_id.to_string(),
                                output: content_value,
                            });
                        }
                    },
                    Some("thinking") => {
                        if let Some(thinking) = block.get("thinking").and_then(Value::as_str) {
                            aggregated_reasoning.push(thinking.to_string());
                            message_blocks.push(anthropic_raw_content_block(
                                clone_json_value_iteratively(block),
                            ));
                        }
                    },
                    Some("redacted_thinking") => {
                        message_blocks.push(anthropic_raw_content_block(
                            clone_json_value_iteratively(block),
                        ));
                    },
                    _ => {},
                }
            }

            if !message_blocks.is_empty() {
                messages.push(LLMMessage {
                    role: MessageRole::Assistant,
                    content: message_blocks,
                });
            }
        }

        let text = if !aggregated_text.is_empty() {
            Some(aggregated_text.join("\n"))
        } else {
            None
        };

        let reasoning_text = if !aggregated_reasoning.is_empty() {
            Some(aggregated_reasoning.join("\n"))
        } else {
            None
        };

        Ok((messages, tool_calls, tool_results, text, reasoning_text))
    }

    async fn invoke_streaming(
        &self,
        mut body: Map<String, Value>,
        timeout: Duration,
        operation: String,
        trace_id: Option<String>,
        model: String,
        stream_event_sink: crate::types::StreamEventSink,
    ) -> LLMResult<LLMResponse> {
        body.insert("stream".to_string(), Value::Bool(true));

        info!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "opened streaming response with Anthropic"
        );

        let response = super::send_streaming_request(
            self.client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("Content-Type", "application/json")
                .json(&body),
            timeout,
        )
        .await?;

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
                provider = "anthropic",
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                status = %status,
                error_message = %message,
                "Anthropic streaming API responded with error"
            );
            return Err(LLMError::Provider {
                provider: self.provider_kind().to_string(),
                message,
            });
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut utf8_carry = Vec::with_capacity(4);
        let mut body_admission = SseBodyAdmission::new();
        let mut state = AnthropicStreamState::new(self.provider_kind())
            .with_stream_event_sink(stream_event_sink);

        let mut watchdog = super::StreamWatchdog::new(timeout);
        let mut marker_tail = Vec::new();
        while let Some(chunk_result) = watchdog.next(&mut stream).await? {
            let chunk = chunk_result.map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;

            // Text, tool-use and thinking all stream as content blocks;
            // `ping` keepalives are not output.
            if super::chunk_mentions(&mut marker_tail, &chunk, b"content_block") {
                watchdog.mark_progress();
            }
            body_admission.admit_chunk(&chunk)?;
            append_sse_utf8_chunk(&mut buffer, &mut utf8_carry, &chunk)?;
            Self::drain_sse_buffer(&mut buffer, &mut state)?;

            if state.done() {
                break;
            }
        }

        finish_sse_utf8(&utf8_carry)?;
        Self::drain_sse_buffer(&mut buffer, &mut state)?;

        let response = state.into_response();

        info!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "streaming response completed for Anthropic"
        );

        response
    }

    fn drain_sse_buffer(buffer: &mut String, state: &mut AnthropicStreamState) -> LLMResult<()> {
        if state.done() {
            return Ok(());
        }
        let mut consumed = 0;
        while let Some(data) = next_sse_data(buffer, &mut consumed) {
            if data == "[DONE]" {
                state.mark_done();
                break;
            }

            let value = parse_provider_json_value(data.as_ref())?;

            if state.apply_event(&value)? {
                break;
            }
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }

        Ok(())
    }

    async fn invoke_non_stream_once(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        let protected_request = request.metadata.single_physical_attempt;
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for Anthropic provider".to_string(),
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

        debug!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "issuing Anthropic Messages request"
        );

        let (system_prompt, mut messages) = Self::map_messages(&request.messages)?;
        let has_system_prompt = system_prompt
            .as_ref()
            .map(|value| !value.is_empty())
            .unwrap_or(false);

        let cache_control = self.prompt_cache_control(&request);
        let rolling_prefix = self.uses_rolling_prefix(&request);
        // The stable prefix is anchored where it is stable — the tools and the
        // system prompt — in both modes. A rolling request additionally anchors
        // the last completed turn (`anchor_last_completed_turn`) and treats every
        // user sentinel as strip-only: a sentinel inside the final, re-rendered
        // prompt would sit after the growing conversation and never hit.
        let explicit_cache_control = cache_control.as_ref();
        let cache_anchored_on_turn =
            Self::apply_cache_anchors(&mut messages, rolling_prefix, explicit_cache_control);

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));

        let max_output_tokens = request
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        body.insert("max_tokens".to_string(), Value::from(max_output_tokens));

        debug!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            max_output_tokens = max_output_tokens,
            request_max_output_tokens = ?request.max_output_tokens,
            "Building Anthropic request with max_tokens"
        );

        let cache_anchored_on_system = Self::attach_system_with_cache_control(
            &mut body,
            system_prompt,
            explicit_cache_control,
        );

        let tools_cache_control = Self::tools_cache_control(
            rolling_prefix,
            cache_anchored_on_system,
            &body,
            explicit_cache_control,
        );
        self.attach_tools_or_server_web_search(&mut body, &request, tools_cache_control)?;

        let wire = wire_for(&self.provider_kind, &model, request.model_wire.as_ref());
        Self::insert_sampling(&mut body, &model, &wire, &request);

        if let Some(reasoning) = request.reasoning.as_ref() {
            Self::insert_provider_reasoning_config(
                &self.provider_kind,
                &model,
                &wire,
                &mut body,
                reasoning,
            )?;
        }

        self.attach_response_format(&mut body, &request)?;

        if let Some(metadata) = Self::map_metadata(&request.metadata) {
            body.insert("metadata".to_string(), metadata);
        }

        if !cache_anchored_on_system && !cache_anchored_on_turn {
            if let Some(cache_control) = cache_control {
                body.insert("cache_control".to_string(), cache_control);
            }
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                if !Self::should_forward_extra(&key, protected_request) {
                    debug!(
                        provider = "anthropic_messages",
                        key = %key,
                        "filtering out config-only field from API request"
                    );
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        Self::ensure_default_no_thinking(
            &self.provider_kind,
            &mut body,
            request.reasoning.as_ref(),
        );

        debug!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            has_reasoning = has_reasoning,
            has_system_prompt = has_system_prompt,
            has_metadata = has_metadata,
            has_extra = has_extra,
            "constructed Anthropic request payload"
        );

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        // Plain calls stream too, so a long answer that is still producing is
        // bounded by progress, not cut at the request timeout. The events fold
        // back into the message body parsed below.
        body.insert("stream".to_string(), Value::Bool(true));
        let response = super::send_streaming_request(
            self.client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("Content-Type", "application/json")
                .json(&body),
            timeout,
        )
        .await?;

        let status = response.status();
        let payload = if !status.is_success() || super::is_json_response(&response) {
            super::read_json_body(response, timeout).await?
        } else {
            let mut message =
                AnthropicMessageAssembler::new(self.provider_kind(), protected_request);
            let mut reader = super::SseReader::new(response.bytes_stream(), timeout);
            while let Some(data) = reader.next_data().await? {
                if message.fold_data(&data)? {
                    reader.mark_progress();
                }
                if message.done() {
                    break;
                }
            }
            message.finish()?
        };
        if status.is_client_error() || status.is_server_error() {
            if let Some(error) = payload.get("error") {
                let message = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                if protected_request {
                    error!(
                        provider = "anthropic_messages",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "Anthropic API rejected protected request"
                    );
                    return Err(LLMError::Provider {
                        provider: self.provider_kind().to_string(),
                        message: "protected provider request failed".to_owned(),
                    });
                }
                error!(
                    provider = "anthropic_messages",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    error_message = %message,
                    "Anthropic API responded with error"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                error!(
                    provider = "anthropic_messages",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "Anthropic API returned unexpected error payload"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: format!("HTTP status {}", status),
                });
            }
        }

        let (messages, tool_calls, tool_results, text, reasoning_text) =
            Self::parse_output(&payload)?;

        // Anthropic-family `usage` folds extended-thinking tokens into
        // `output_tokens` and exposes no separate field, so `map_usage` leaves
        // `reasoning_tokens: None` — telemetry then reads 0 even when the model
        // reasoned heavily (DeepSeek V4 / Claude with thinking). Estimate it from
        // the captured thinking text (~4 chars/token) so logs reflect that
        // reasoning happened. It's an estimate, not a billed figure — the real
        // tokens are already inside `completion_tokens` (`output_tokens`).
        let estimated_reasoning_tokens = reasoning_text
            .as_deref()
            .map(|t| t.chars().count())
            .filter(|chars| *chars > 0)
            .map(|chars| ((chars / 4).max(1)) as u32);
        let usage = payload
            .get("usage")
            .map(|value| Self::map_usage(&self.provider_kind, value))
            .transpose()?
            .map(|mut usage| {
                if usage.reasoning_tokens.is_none() {
                    usage.reasoning_tokens = estimated_reasoning_tokens;
                }
                usage
            });

        let finish_reason = payload
            .get("stop_reason")
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let response = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            // Anthropic's Messages API doesn't expose a chainable id;
            // chaining (`previous_response_id`) is OpenAI-Responses-only.
            response_id: None,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(tool_results),
            usage,
            finish_reason,
            // Normalization borrows `payload`; the provider-native response
            // itself moves into the shared retention lane exactly once.
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "anthropic_messages",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by Anthropic"
            );
        }

        let response_text_len = response.text.as_ref().map(|t| t.len()).unwrap_or(0);
        let completion_tokens = response
            .usage
            .as_ref()
            .and_then(|u| u.completion_tokens)
            .unwrap_or(0);
        info!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            response_text_len = response_text_len,
            completion_tokens = completion_tokens,
            "Anthropic Messages call succeeded"
        );

        Ok(response)
    }

    async fn invoke_non_stream_with_max_tokens_retry(
        &self,
        request: LLMRequest,
    ) -> LLMResult<LLMResponse> {
        let base_max_output_tokens = request
            .max_output_tokens
            .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS);
        let initial_response = self.invoke_non_stream_once(request.clone()).await?;
        if request.metadata.single_physical_attempt
            || !Self::should_retry_max_tokens(&initial_response)
        {
            return Ok(initial_response);
        }

        let retry_max_output_tokens = Self::retry_max_output_tokens(base_max_output_tokens);
        if retry_max_output_tokens <= base_max_output_tokens {
            return Ok(initial_response);
        }

        warn!(
            provider = "anthropic_messages",
            model = %request.model,
            operation = %request.metadata.operation,
            trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
            base_max_output_tokens,
            retry_max_output_tokens,
            tool_call_count = initial_response.tool_calls.len(),
            "Anthropic response hit max_tokens; retrying with higher max_tokens"
        );

        let mut retry_request = request.clone();
        retry_request.max_output_tokens = Some(retry_max_output_tokens);

        match self.invoke_non_stream_once(retry_request).await {
            Ok(retry_response) => Ok(retry_response),
            Err(error) if Self::is_excessive_max_tokens_error(&error) => {
                let Some(backed_off_max_output_tokens) = Self::backed_off_max_output_tokens(
                    base_max_output_tokens,
                    retry_max_output_tokens,
                ) else {
                    return Err(error);
                };

                warn!(
                    provider = "anthropic_messages",
                    model = %request.model,
                    operation = %request.metadata.operation,
                    trace_id = request.metadata.trace_id.as_deref().unwrap_or(""),
                    retry_max_output_tokens,
                    backed_off_max_output_tokens,
                    error = %error,
                    "Anthropic retry rejected larger max_tokens; retrying with backed-off token budget"
                );

                let mut backed_off_request = request;
                backed_off_request.max_output_tokens = Some(backed_off_max_output_tokens);
                self.invoke_non_stream_once(backed_off_request).await
            },
            Err(error) => Err(error),
        }
    }

    /// Streaming implementation that sends deltas to the provided channel.
    async fn invoke_stream_impl(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for Anthropic provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        // Capture optional fan-out sink for fine-grained AG-UI deltas.
        // Independent of the mpsc `tx` channel — both can fire side by side.
        // Prefer request-scoped, fall back to task-local (set by the
        // inner-loop runner before invoking the call).
        let stream_event_sink = if request.stream_event_sink.is_attached() {
            request.stream_event_sink.clone()
        } else {
            crate::types::current_stream_event_sink()
        };

        info!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "issuing Anthropic Messages streaming request"
        );

        let (system_prompt, mut messages) = Self::map_messages(&request.messages)?;

        let cache_control = self.prompt_cache_control(&request);
        let rolling_prefix = self.uses_rolling_prefix(&request);
        // The stable prefix is anchored where it is stable — the tools and the
        // system prompt — in both modes. A rolling request additionally anchors
        // the last completed turn (`anchor_last_completed_turn`) and treats every
        // user sentinel as strip-only: a sentinel inside the final, re-rendered
        // prompt would sit after the growing conversation and never hit.
        let explicit_cache_control = cache_control.as_ref();
        let cache_anchored_on_turn =
            Self::apply_cache_anchors(&mut messages, rolling_prefix, explicit_cache_control);

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));

        let max_output_tokens = request.max_output_tokens.unwrap_or(4096);
        body.insert("max_tokens".to_string(), Value::from(max_output_tokens));

        let cache_anchored_on_system = Self::attach_system_with_cache_control(
            &mut body,
            system_prompt,
            explicit_cache_control,
        );

        let tools_cache_control = Self::tools_cache_control(
            rolling_prefix,
            cache_anchored_on_system,
            &body,
            explicit_cache_control,
        );
        self.attach_tools_or_server_web_search(&mut body, &request, tools_cache_control)?;

        let wire = wire_for(&self.provider_kind, &model, request.model_wire.as_ref());
        Self::insert_sampling(&mut body, &model, &wire, &request);

        if let Some(reasoning) = request.reasoning.as_ref() {
            Self::insert_provider_reasoning_config(
                &self.provider_kind,
                &model,
                &wire,
                &mut body,
                reasoning,
            )?;
        }

        self.attach_response_format(&mut body, &request)?;

        if let Some(metadata) = Self::map_metadata(&request.metadata) {
            body.insert("metadata".to_string(), metadata);
        }

        if !cache_anchored_on_system && !cache_anchored_on_turn {
            if let Some(cache_control) = cache_control {
                body.insert("cache_control".to_string(), cache_control);
            }
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                if !Self::should_forward_extra(&key, request.metadata.single_physical_attempt) {
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        Self::ensure_default_no_thinking(
            &self.provider_kind,
            &mut body,
            request.reasoning.as_ref(),
        );

        body.insert("stream".to_string(), Value::Bool(true));

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let response = super::send_streaming_request(
            self.client
                .post(&self.base_url)
                .header("x-api-key", &self.api_key)
                .header("anthropic-version", ANTHROPIC_VERSION)
                .header("Content-Type", "application/json")
                .json(&body),
            timeout,
        )
        .await?;

        // Check HTTP status before consuming the stream — mirrors OpenAI Chat provider
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
        let mut state = AnthropicStreamState::new(self.provider_kind())
            .with_stream_event_sink(stream_event_sink);

        let mut watchdog = super::StreamWatchdog::new(timeout);
        let mut marker_tail = Vec::new();
        while let Some(chunk_result) = watchdog.next(&mut stream).await? {
            let chunk = chunk_result.map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;

            // Text, tool-use and thinking all stream as content blocks;
            // `ping` keepalives are not output.
            if super::chunk_mentions(&mut marker_tail, &chunk, b"content_block") {
                watchdog.mark_progress();
            }
            body_admission.admit_chunk(&chunk)?;
            append_sse_utf8_chunk(&mut buffer, &mut utf8_carry, &chunk)?;
            Self::drain_sse_buffer_with_deltas(&mut buffer, &mut state, &tx).await?;

            if state.done() {
                break;
            }
        }

        finish_sse_utf8(&utf8_carry)?;
        Self::drain_sse_buffer_with_deltas(&mut buffer, &mut state, &tx).await?;

        let llm_response = state.into_response()?;

        info!(
            provider = "anthropic_messages",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = llm_response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = llm_response.tool_calls.len(),
            "Anthropic Messages streaming with deltas succeeded"
        );

        let _ = tx.send(StreamDelta::Done(llm_response)).await;
        Ok(())
    }

    /// Like `drain_sse_buffer` but also sends `StreamDelta` tokens via `tx`.
    async fn drain_sse_buffer_with_deltas(
        buffer: &mut String,
        state: &mut AnthropicStreamState,
        tx: &mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        if state.done() {
            return Ok(());
        }
        let mut consumed = 0;
        while let Some(data) = next_sse_data(buffer, &mut consumed) {
            if data == "[DONE]" {
                state.mark_done();
                break;
            }

            let value = parse_provider_json_value(data.as_ref())?;
            drop(data);

            // Send text deltas before applying to state
            if let Some(event_type) = value.get("type").and_then(Value::as_str) {
                if event_type == "content_block_delta" {
                    if let Some(delta) = value.get("delta") {
                        let delta_type = delta.get("type").and_then(Value::as_str);
                        match delta_type {
                            Some("text_delta") => {
                                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                                    let _ = tx.send(StreamDelta::Token(text.to_string())).await;
                                }
                            },
                            Some("input_json_delta") => {
                                // Tool call argument chunk — extract the index to get the tool id
                                let index =
                                    value.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                                if let Some(partial_json) =
                                    delta.get("partial_json").and_then(Value::as_str)
                                {
                                    // Look up the tool id from the block state
                                    let tool_id = match state.block_states.get(&index) {
                                        Some(BlockState::ToolUse { id }) => id.clone(),
                                        _ => String::new(),
                                    };
                                    let tool_name = state
                                        .tool_states
                                        .get(&tool_id)
                                        .and_then(|s| s.name.clone());
                                    let _ = tx
                                        .send(StreamDelta::ToolCallDelta {
                                            id: tool_id,
                                            name: tool_name,
                                            arguments_chunk: partial_json.to_string(),
                                        })
                                        .await;
                                }
                            },
                            _ => {},
                        }
                    }
                }
            }

            if state.apply_event(&value)? {
                break;
            }
        }
        if consumed > 0 {
            buffer.drain(..consumed);
        }

        Ok(())
    }
}

#[async_trait]
impl LLMProvider for AnthropicMessagesProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        self.provider_kind.clone()
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        let mut capability = Self::detect_capability(model);
        // Only the native Anthropic transport translates the flag; the
        // Anthropic-compatible transports (DeepSeek) fail it closed.
        capability.web_search = self.provider_kind == LLMProviderKind::Anthropic;
        // Native Messages accepts output_config.format with a JSON schema;
        // JsonObject without a schema is rejected at body admission.
        capability.json_mode = self.provider_kind == LLMProviderKind::Anthropic;
        capability
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        self.invoke_stream_impl(request, tx).await
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        if request.stream {
            let model = if request.model.is_empty() {
                return Err(LLMError::Validation(
                    "LLMRequest.model must be set for Anthropic provider".to_string(),
                ));
            } else {
                request.model.clone()
            };

            let operation = request.metadata.operation.clone();
            let trace_id = request.metadata.trace_id.clone();
            let (system_prompt, mut messages) = Self::map_messages(&request.messages)?;

            let cache_control = self.prompt_cache_control(&request);
            let rolling_prefix = self.uses_rolling_prefix(&request);
            // The stable prefix is anchored where it is stable — the tools and the
            // system prompt — in both modes. A rolling request additionally anchors
            // the last completed turn (`anchor_last_completed_turn`) and treats every
            // user sentinel as strip-only: a sentinel inside the final, re-rendered
            // prompt would sit after the growing conversation and never hit.
            let explicit_cache_control = cache_control.as_ref();
            let cache_anchored_on_turn =
                Self::apply_cache_anchors(&mut messages, rolling_prefix, explicit_cache_control);

            let mut body = Map::new();
            body.insert("model".to_string(), Value::String(model.clone()));
            body.insert("messages".to_string(), Value::Array(messages));
            body.insert(
                "max_tokens".to_string(),
                Value::from(
                    request
                        .max_output_tokens
                        .unwrap_or(DEFAULT_MAX_OUTPUT_TOKENS),
                ),
            );

            let cache_anchored_on_system = Self::attach_system_with_cache_control(
                &mut body,
                system_prompt,
                explicit_cache_control,
            );

            let tools_cache_control = Self::tools_cache_control(
                rolling_prefix,
                cache_anchored_on_system,
                &body,
                explicit_cache_control,
            );
            self.attach_tools_or_server_web_search(&mut body, &request, tools_cache_control)?;

            let wire = wire_for(&self.provider_kind, &model, request.model_wire.as_ref());
            Self::insert_sampling(&mut body, &model, &wire, &request);

            if let Some(reasoning) = request.reasoning.as_ref() {
                Self::insert_provider_reasoning_config(
                    &self.provider_kind,
                    &model,
                    &wire,
                    &mut body,
                    reasoning,
                )?;
            }

            self.attach_response_format(&mut body, &request)?;

            if let Some(metadata) = Self::map_metadata(&request.metadata) {
                body.insert("metadata".to_string(), metadata);
            }

            if !cache_anchored_on_system && !cache_anchored_on_turn {
                if let Some(cache_control) = cache_control {
                    body.insert("cache_control".to_string(), cache_control);
                }
            }

            // Capture optional fan-out sink before consuming `request.extra`.
            // The sink lets observers subscribe to fine-grained AG-UI-style
            // stream deltas (ReasoningStart/Delta/End, ToolCallStart/
            // ArgsDelta/End) without using the public mpsc channel.
            // Prefer the request-scoped sink; fall back to the task-local
            // sink (set by the inner-loop runner via
            // `magicllm::types::scoped_stream_event_sink`) when the call
            // comes through the router/chain layers that don't yet plumb
            // the field into LLMRequest.
            let stream_event_sink = if request.stream_event_sink.is_attached() {
                request.stream_event_sink.clone()
            } else {
                crate::types::current_stream_event_sink()
            };

            if let Some(Value::Object(extra_map)) = request.extra_value() {
                for (key, value) in extra_map {
                    if !Self::should_forward_extra(&key, request.metadata.single_physical_attempt) {
                        continue;
                    }
                    body.entry(key.clone())
                        .or_insert_with(|| clone_json_value_iteratively(value));
                }
            }

            Self::ensure_default_no_thinking(
                &self.provider_kind,
                &mut body,
                request.reasoning.as_ref(),
            );

            let timeout = request
                .metadata
                .timeout_secs
                .map(Duration::from_secs)
                .unwrap_or(self.default_timeout);

            info!(
                provider = "anthropic_messages",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "entering streaming mode for Anthropic response"
            );
            return self
                .invoke_streaming(body, timeout, operation, trace_id, model, stream_event_sink)
                .await;
        }

        self.invoke_non_stream_with_max_tokens_retry(request).await
    }
}

/// Folds a Messages event stream back into the non-streaming message body,
/// so a plain call that streamed is parsed by `parse_output` like one that
/// did not. Unlike the delta-oriented [`AnthropicStreamState`] it keeps every
/// block as sent (server tool use, web search results, citations), merges
/// `message_start` usage with the final `message_delta` counts, and keeps
/// the message envelope (`id`, `model`, `stop_reason`, `stop_sequence`).
struct AnthropicMessageAssembler {
    provider: LLMProviderKind,
    protected: bool,
    message: Map<String, Value>,
    usage: Map<String, Value>,
    blocks: std::collections::BTreeMap<u64, Value>,
    tool_input: HashMap<u64, String>,
    done: bool,
}

impl AnthropicMessageAssembler {
    fn new(provider: LLMProviderKind, protected: bool) -> Self {
        Self {
            provider,
            protected,
            message: Map::new(),
            usage: Map::new(),
            blocks: std::collections::BTreeMap::new(),
            tool_input: HashMap::new(),
            done: false,
        }
    }

    fn done(&self) -> bool {
        self.done
    }

    /// Fold one SSE `data` payload; `Ok(true)` when it was a content-block
    /// event, the same output rule the streaming paths use for progress.
    fn fold_data(&mut self, data: &str) -> LLMResult<bool> {
        if data == "[DONE]" {
            self.done = true;
            return Ok(false);
        }
        let Value::Object(mut event) = parse_provider_json_value(data)? else {
            return Ok(false);
        };
        let kind = event
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0);
        if kind == "message_start" {
            if let Some(Value::Object(message)) = event.remove("message") {
                for (key, value) in message {
                    if key == "usage" {
                        Self::merge_usage(&mut self.usage, value);
                    } else if key != "content" {
                        self.message.insert(key, value);
                    }
                }
            }
        } else if kind == "content_block_start" {
            if let Some(block) = event.remove("content_block") {
                self.blocks.insert(index, block);
            }
            return Ok(true);
        } else if kind == "content_block_delta" {
            if let Some(Value::Object(delta)) = event.remove("delta") {
                self.fold_delta(index, delta)?;
            }
            return Ok(true);
        } else if kind == "content_block_stop" {
            self.close_block(index)?;
            return Ok(true);
        } else if kind == "message_delta" {
            if let Some(Value::Object(delta)) = event.remove("delta") {
                for (key, value) in delta {
                    if key == "usage" {
                        Self::merge_usage(&mut self.usage, value);
                    } else if !value.is_null() {
                        self.message.insert(key, value);
                    }
                }
            }
            if let Some(usage) = event.remove("usage") {
                Self::merge_usage(&mut self.usage, usage);
            }
        } else if kind == "message_stop" {
            self.done = true;
        } else if kind == "error" {
            let message = if self.protected {
                super::PROTECTED_PROVIDER_ERROR.to_owned()
            } else {
                event
                    .get("error")
                    .and_then(|error| error.get("message"))
                    .and_then(Value::as_str)
                    .unwrap_or("Anthropic streaming error")
                    .to_owned()
            };
            return Err(LLMError::Provider {
                provider: self.provider.to_string(),
                message,
            });
        }
        Ok(false)
    }

    fn fold_delta(&mut self, index: u64, mut delta: Map<String, Value>) -> LLMResult<()> {
        let kind = delta
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if kind == "input_json_delta" {
            if let Some(fragment) = delta.get("partial_json").and_then(Value::as_str) {
                append_tool_argument_fragment(self.tool_input.entry(index).or_default(), fragment)?;
            }
            return Ok(());
        }
        let Some(Value::Object(block)) = self.blocks.get_mut(&index) else {
            return Ok(());
        };
        if kind == "citations_delta" {
            if let Some(citation) = delta.remove("citation") {
                match block.get_mut("citations") {
                    Some(Value::Array(citations)) => citations.push(citation),
                    _ => {
                        block.insert("citations".to_string(), Value::Array(vec![citation]));
                    },
                }
            }
            return Ok(());
        }
        // text_delta -> text, thinking_delta -> thinking, signature_delta ->
        // signature: each appends to the block field of the same name.
        for (key, value) in delta {
            if key == "type" {
                continue;
            }
            if let Value::String(fragment) = value {
                match block.get_mut(&key) {
                    Some(Value::String(existing)) => existing.push_str(&fragment),
                    _ => {
                        block.insert(key, Value::String(fragment));
                    },
                }
            }
        }
        Ok(())
    }

    /// A tool-use block's streamed JSON replaces the empty `input` it opened
    /// with.
    fn close_block(&mut self, index: u64) -> LLMResult<()> {
        let Some(raw) = self.tool_input.remove(&index) else {
            return Ok(());
        };
        if raw.is_empty() {
            return Ok(());
        }
        let input = parse_tool_argument_json_or_string(&raw)?;
        if let Some(Value::Object(block)) = self.blocks.get_mut(&index) {
            block.insert("input".to_string(), input);
        }
        Ok(())
    }

    fn merge_usage(usage: &mut Map<String, Value>, value: Value) {
        if let Value::Object(fields) = value {
            for (key, value) in fields {
                if !value.is_null() {
                    usage.insert(key, value);
                }
            }
        }
    }

    /// The assembled message. A stream that ended before `message_stop` was
    /// cut off: a plain call got no complete answer, so it fails as
    /// transport (as a truncated JSON body did).
    fn finish(mut self) -> LLMResult<Value> {
        if !self.done {
            return Err(LLMError::Transport(
                "Anthropic stream ended before message_stop".to_string(),
            ));
        }
        let open: Vec<u64> = self.tool_input.keys().copied().collect();
        for index in open {
            self.close_block(index)?;
        }
        let mut message = std::mem::take(&mut self.message);
        let blocks = std::mem::take(&mut self.blocks);
        message.insert(
            "content".to_string(),
            Value::Array(blocks.into_values().collect()),
        );
        if !self.usage.is_empty() {
            message.insert(
                "usage".to_string(),
                Value::Object(std::mem::take(&mut self.usage)),
            );
        }
        Ok(Value::Object(message))
    }
}

#[derive(Debug)]
enum BlockState {
    Text {
        buffer: String,
    },
    ToolUse {
        id: String,
    },
    Thinking {
        buffer: String,
        signature: Option<String>,
    },
    Raw {
        block: Value,
    },
    Unknown,
}

#[derive(Debug, Default)]
struct ToolCallState {
    name: Option<String>,
    buffer: String,
    value: Option<Value>,
}

impl ToolCallState {
    fn new(name: Option<String>, value: Option<Value>) -> Self {
        Self {
            name,
            buffer: String::new(),
            value,
        }
    }

    fn set_name(&mut self, name: &str) {
        self.name = Some(name.to_string());
    }

    fn set_value(&mut self, value: Value) {
        self.value = Some(value);
    }

    fn append_json(&mut self, fragment: &str) -> LLMResult<()> {
        if self.buffer.len().saturating_add(fragment.len())
            > crate::types::MAX_TOOL_ARGUMENT_JSON_BYTES
        {
            return Err(LLMError::Validation(format!(
                "streamed tool arguments exceed the admitted {}-byte ceiling",
                crate::types::MAX_TOOL_ARGUMENT_JSON_BYTES
            )));
        }
        self.buffer.push_str(fragment);
        Ok(())
    }

    fn append_text(&mut self, fragment: &str) -> LLMResult<()> {
        if self.buffer.len().saturating_add(fragment.len())
            > crate::types::MAX_TOOL_ARGUMENT_JSON_BYTES
        {
            return Err(LLMError::Validation(format!(
                "streamed tool input exceeds the admitted {}-byte ceiling",
                crate::types::MAX_TOOL_ARGUMENT_JSON_BYTES
            )));
        }
        self.buffer.push_str(fragment);
        Ok(())
    }

    fn finalize(&mut self) -> LLMResult<Value> {
        if !self.buffer.is_empty() {
            parse_tool_argument_json_or_string(&self.buffer)
        } else if let Some(value) = self.value.take() {
            Ok(value)
        } else {
            Ok(Value::Null)
        }
    }

    fn name(&self) -> String {
        self.name.clone().unwrap_or_default()
    }
}

struct AnthropicStreamState {
    provider: LLMProviderKind,
    aggregated_text: String,
    /// Per-thinking-block text accumulator. Each finalised thinking block
    /// is appended (newline-separated) so the streamed reasoning surface
    /// matches the non-streaming `parse_output` reasoning_text shape.
    aggregated_reasoning: Vec<String>,
    message_blocks: Vec<ContentBlock>,
    tool_calls: Vec<LLMToolCall>,
    tool_states: HashMap<String, ToolCallState>,
    block_states: HashMap<u32, BlockState>,
    /// Per-block accumulated character count (for `ReasoningEnd.total_chars`).
    block_char_counts: HashMap<u32, usize>,
    finish_reason: Option<String>,
    usage_value: Option<Value>,
    done: bool,
    /// Optional fan-out sink for fine-grained AG-UI-style stream deltas.
    /// Cloned from `LLMRequest.stream_event_sink` at the start of
    /// `invoke_streaming` so observers (e.g. the magician inner-loop
    /// runner forwarding to `RuntimeTransportBroadcaster`) see deltas
    /// inline. No-op when not attached.
    stream_event_sink: crate::types::StreamEventSink,
}

impl AnthropicStreamState {
    fn new(provider: LLMProviderKind) -> Self {
        Self {
            provider,
            aggregated_text: String::new(),
            aggregated_reasoning: Vec::new(),
            message_blocks: Vec::new(),
            tool_calls: Vec::new(),
            tool_states: HashMap::new(),
            block_states: HashMap::new(),
            block_char_counts: HashMap::new(),
            finish_reason: None,
            usage_value: None,
            done: false,
            stream_event_sink: crate::types::StreamEventSink::default(),
        }
    }

    fn with_stream_event_sink(mut self, sink: crate::types::StreamEventSink) -> Self {
        self.stream_event_sink = sink;
        self
    }

    fn mark_done(&mut self) {
        self.done = true;
    }

    fn done(&self) -> bool {
        self.done
    }

    fn apply_event(&mut self, event: &Value) -> LLMResult<bool> {
        let event_type = event.get("type").and_then(Value::as_str);
        match event_type {
            Some("content_block_start") => self.handle_content_block_start(event)?,
            Some("content_block_delta") => self.handle_content_block_delta(event)?,
            Some("content_block_stop") => self.handle_content_block_stop(event)?,
            // Anthropic reports input and cache counts on `message_start`
            // (`message.usage`) and the cumulative counts on `message_delta`
            // (top-level `usage`, beside `delta`). Later fields win.
            Some("message_start") => {
                if let Some(usage) = event.pointer("/message/usage") {
                    self.merge_usage(usage);
                }
            },
            Some("message_delta") => {
                if let Some(delta) = event.get("delta") {
                    if let Some(stop_reason) = delta.get("stop_reason").and_then(Value::as_str) {
                        self.finish_reason = Some(stop_reason.to_string());
                    }
                    if let Some(usage) = delta.get("usage") {
                        self.merge_usage(usage);
                    }
                }
                if let Some(usage) = event.get("usage") {
                    self.merge_usage(usage);
                }
            },
            Some("message_stop") => {
                self.done = true;
                self.finalize_remaining_blocks()?;
                return Ok(true);
            },
            Some("error") => {
                let message = event
                    .get("error")
                    .and_then(|value| value.get("message").and_then(Value::as_str))
                    .or_else(|| event.get("message").and_then(Value::as_str))
                    .unwrap_or("Anthropic streaming error");
                return Err(LLMError::Provider {
                    provider: self.provider.to_string(),
                    message: message.to_string(),
                });
            },
            _ => {},
        }

        Ok(false)
    }

    fn merge_usage(&mut self, usage: &Value) {
        let Some(fields) = usage.as_object() else {
            return;
        };
        let merged = self.usage_value.get_or_insert_with(|| json!({}));
        if let Some(object) = merged.as_object_mut() {
            for (key, value) in fields {
                if !value.is_null() {
                    object.insert(key.clone(), value.clone());
                }
            }
        }
    }

    fn handle_content_block_start(&mut self, event: &Value) -> LLMResult<()> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;

        let Some(content_block) = event.get("content_block") else {
            self.block_states.insert(index, BlockState::Unknown);
            return Ok(());
        };

        match content_block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let mut buffer = String::new();
                if let Some(text) = content_block.get("text").and_then(Value::as_str) {
                    buffer.push_str(text);
                }
                self.block_states.insert(index, BlockState::Text { buffer });
            },
            Some("tool_use") => {
                let id = content_block
                    .get("id")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let name = content_block
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string());
                let input_value = content_block
                    .get("input")
                    .map(clone_admitted_tool_argument)
                    .transpose()?;
                let tool_name_for_event = name.clone().unwrap_or_default();
                self.tool_states
                    .insert(id.clone(), ToolCallState::new(name, input_value));
                self.block_states
                    .insert(index, BlockState::ToolUse { id: id.clone() });
                // Fan out AG-UI-style ToolCallStart on block open.
                self.stream_event_sink
                    .fire(crate::types::StreamDelta::ToolCallStart {
                        call_id: id,
                        tool_name: tool_name_for_event,
                    });
            },
            Some("thinking") => {
                let mut buffer = String::new();
                if let Some(text) = content_block
                    .get("thinking")
                    .and_then(Value::as_str)
                    .or_else(|| content_block.get("text").and_then(Value::as_str))
                {
                    buffer.push_str(text);
                }
                let signature = content_block
                    .get("signature")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let initial_chars = buffer.chars().count();
                let initial_text = buffer.clone();
                self.block_states.insert(
                    index,
                    BlockState::Thinking {
                        buffer,
                        signature: signature.clone(),
                    },
                );
                self.block_char_counts.insert(index, initial_chars);
                // Fan out AG-UI-style ReasoningStart + opening delta if the
                // start frame already carried thinking text.
                self.stream_event_sink
                    .fire(crate::types::StreamDelta::ReasoningStart {
                        index: index as usize,
                        signature,
                    });
                if !initial_text.is_empty() {
                    self.stream_event_sink
                        .fire(crate::types::StreamDelta::ReasoningDelta {
                            index: index as usize,
                            delta: initial_text,
                        });
                }
            },
            Some("redacted_thinking") => {
                self.block_states.insert(
                    index,
                    BlockState::Raw {
                        block: content_block.clone(),
                    },
                );
            },
            _ => {
                self.block_states.insert(index, BlockState::Unknown);
            },
        }

        Ok(())
    }

    fn handle_content_block_delta(&mut self, event: &Value) -> LLMResult<()> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let Some(delta) = event.get("delta") else {
            return Ok(());
        };

        // Capture textual deltas before mutating state so we can fire the
        // AG-UI fan-out sink without re-reading the buffer.
        let mut reasoning_delta: Option<String> = None;
        let mut tool_args_delta: Option<(String, String)> = None;

        match self.block_states.get_mut(&index) {
            Some(BlockState::Text { buffer }) => {
                if let Some(text) = delta.get("text").and_then(Value::as_str) {
                    buffer.push_str(text);
                }
            },
            Some(BlockState::Thinking { buffer, signature }) => {
                if let Some(text) = delta
                    .get("thinking")
                    .and_then(Value::as_str)
                    .or_else(|| delta.get("text").and_then(Value::as_str))
                {
                    buffer.push_str(text);
                    reasoning_delta = Some(text.to_string());
                }
                if let Some(delta_signature) = delta
                    .get("signature_delta")
                    .and_then(Value::as_str)
                    .or_else(|| delta.get("signature").and_then(Value::as_str))
                {
                    signature
                        .get_or_insert_with(String::new)
                        .push_str(delta_signature);
                }
            },
            Some(BlockState::Raw { .. }) => {},
            Some(BlockState::ToolUse { id }) => {
                let id_clone = id.clone();
                if let Some(state) = self.tool_states.get_mut(&id_clone) {
                    if let Some(name) = delta.get("name").and_then(Value::as_str) {
                        state.set_name(name);
                    }
                    if let Some(input) = delta.get("input") {
                        state.set_value(input.clone());
                    }
                    if let Some(partial_json) = delta
                        .get("partial_json")
                        .and_then(Value::as_str)
                        .or_else(|| delta.get("input_json").and_then(Value::as_str))
                    {
                        state.append_json(partial_json)?;
                        tool_args_delta = Some((id_clone.clone(), partial_json.to_string()));
                    }
                    if let Some(partial_text) = delta
                        .get("text")
                        .and_then(Value::as_str)
                        .or_else(|| delta.get("input_text").and_then(Value::as_str))
                    {
                        state.append_text(partial_text)?;
                    }
                }
            },
            _ => {},
        }

        // Fan out fine-grained deltas to the optional sink. Done after the
        // mutation pass so the closure can't re-borrow `self.block_states`.
        if let Some(text) = reasoning_delta {
            *self.block_char_counts.entry(index).or_insert(0) += text.chars().count();
            self.stream_event_sink
                .fire(crate::types::StreamDelta::ReasoningDelta {
                    index: index as usize,
                    delta: text,
                });
        }
        if let Some((call_id, partial_json)) = tool_args_delta {
            self.stream_event_sink
                .fire(crate::types::StreamDelta::ToolCallArgsDelta {
                    call_id,
                    delta: partial_json,
                });
        }

        Ok(())
    }

    fn handle_content_block_stop(&mut self, event: &Value) -> LLMResult<()> {
        let index = event.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        self.finalize_block(index)
    }

    fn finalize_block(&mut self, index: u32) -> LLMResult<()> {
        let state = self.block_states.remove(&index);
        match state {
            Some(BlockState::Text { buffer }) => {
                if !buffer.is_empty() {
                    if !self.aggregated_text.is_empty() {
                        self.aggregated_text.push('\n');
                    }
                    self.aggregated_text.push_str(&buffer);
                }
                self.message_blocks
                    .push(ContentBlock::Text { text: buffer });
            },
            Some(BlockState::Thinking { buffer, signature }) => {
                let total_chars = self
                    .block_char_counts
                    .remove(&index)
                    .unwrap_or_else(|| buffer.chars().count());
                if !buffer.is_empty() {
                    // Mirror parse_output: emit raw block for replay AND
                    // accumulate plain reasoning text for trace surfacing.
                    self.aggregated_reasoning.push(buffer.clone());
                    let mut block = Map::new();
                    block.insert("type".to_string(), Value::String("thinking".to_string()));
                    block.insert("thinking".to_string(), Value::String(buffer));
                    if let Some(signature) = signature {
                        if !signature.is_empty() {
                            block.insert("signature".to_string(), Value::String(signature));
                        }
                    }
                    self.message_blocks
                        .push(anthropic_raw_content_block(Value::Object(block)));
                }
                // Fan out AG-UI-style ReasoningEnd. Carries assembled char
                // count so consumers can finalise their thinking panel.
                self.stream_event_sink
                    .fire(crate::types::StreamDelta::ReasoningEnd {
                        index: index as usize,
                        total_chars,
                    });
            },
            Some(BlockState::Raw { block }) => {
                self.message_blocks.push(anthropic_raw_content_block(block));
            },
            Some(BlockState::ToolUse { id }) => {
                if let Some(mut state) = self.tool_states.remove(&id) {
                    let arguments = state.finalize()?;
                    let name = state.name();
                    self.tool_calls.push(LLMToolCall {
                        id: id.clone(),
                        name: name.clone(),
                        arguments: clone_json_value_iteratively(&arguments),
                    });
                    self.message_blocks.push(ContentBlock::ToolCall {
                        id: id.clone(),
                        name,
                        arguments,
                    });
                    // Fan out AG-UI-style ToolCallEnd.
                    self.stream_event_sink
                        .fire(crate::types::StreamDelta::ToolCallEnd { call_id: id });
                }
            },
            Some(BlockState::Unknown) | None => {},
        }

        Ok(())
    }

    fn finalize_remaining_blocks(&mut self) -> LLMResult<()> {
        let indices: Vec<u32> = self.block_states.keys().copied().collect();
        for index in indices {
            self.finalize_block(index)?;
        }
        Ok(())
    }

    fn into_response(mut self) -> LLMResult<LLMResponse> {
        self.finalize_remaining_blocks()?;

        let usage_value = self.usage_value.take();
        let usage = usage_value
            .as_ref()
            .map(|value| AnthropicMessagesProvider::map_usage(&self.provider, value))
            .transpose()?;

        let text = if self.aggregated_text.is_empty() {
            None
        } else {
            Some(self.aggregated_text.clone())
        };

        let mut messages = Vec::new();
        let message_blocks = std::mem::take(&mut self.message_blocks);
        if !message_blocks.is_empty() {
            messages.push(LLMMessage {
                role: MessageRole::Assistant,
                content: message_blocks,
            });
        }

        let reasoning_text = if self.aggregated_reasoning.is_empty() {
            None
        } else {
            Some(self.aggregated_reasoning.join("\n"))
        };

        Ok(LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            response_id: None,
            messages: Arc::new(messages),
            tool_calls: Arc::new(std::mem::take(&mut self.tool_calls)),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason: self.finish_reason.take(),
            raw_response: None,
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_marked_final_turn_text_is_cached_and_the_tools_defer_to_the_system_anchor() {
        let cc = serde_json::json!({"type": "ephemeral"});
        let marker = crate::types::CACHE_BREAKPOINT_SENTINEL;
        let mut messages = vec![
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "stable"}]}),
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "call"}]}),
            serde_json::json!({"role": "user", "content": [
                {"type": "text", "text": format!("changed sections\n{marker}")},
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}}
            ]}),
        ];
        assert!(AnthropicMessagesProvider::apply_cache_anchors(
            &mut messages,
            true,
            Some(&cc)
        ));
        let block = &messages[2]["content"][0];
        assert_eq!(block["text"], "changed sections");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
        assert!(messages[2]["content"][1].get("cache_control").is_none());
        // An unmarked final message is not cached.
        let mut plain = vec![
            serde_json::json!({"role": "assistant", "content": [{"type": "text", "text": "call"}]}),
            serde_json::json!({"role": "user", "content": [{"type": "text", "text": "changed"}]}),
        ];
        AnthropicMessagesProvider::apply_cache_anchors(&mut plain, true, Some(&cc));
        assert!(plain[1]["content"]
            .get(0)
            .and_then(|b| b.get("cache_control"))
            .is_none());
        // The tools keep their own breakpoint while a slot is free and defer to
        // the system anchor only when all four are taken.
        let marked = serde_json::json!([{"type": "text", "text": "x", "cache_control": cc}]);
        let body = |marked_messages: usize| {
            let mut body = serde_json::Map::new();
            body.insert("system".to_string(), marked.clone());
            body.insert(
                "messages".to_string(),
                Value::Array(
                    (0..3)
                        .map(|i| {
                            let content = if i < marked_messages {
                                marked.clone()
                            } else {
                                serde_json::json!("plain")
                            };
                            serde_json::json!({"role": "user", "content": content})
                        })
                        .collect(),
                ),
            );
            body
        };
        assert!(
            AnthropicMessagesProvider::tools_cache_control(true, true, &body(3), Some(&cc))
                .is_none()
        );
        assert!(
            AnthropicMessagesProvider::tools_cache_control(true, true, &body(2), Some(&cc))
                .is_some()
        );
        assert!(
            AnthropicMessagesProvider::tools_cache_control(false, true, &body(3), Some(&cc))
                .is_some()
        );
    }

    #[test]
    fn a_long_conversation_gets_a_second_breakpoint_within_anthropics_lookback() {
        let cc = serde_json::json!({"type": "ephemeral"});
        let turn = |role: &str, n: usize| serde_json::json!({"role": role, "content": [{"type": "text", "text": format!("{role} {n}")}]});
        let count = |messages: &[Value]| {
            messages
                .iter()
                .flat_map(|m| m["content"].as_array().cloned().unwrap_or_default())
                .filter(|b| b.get("cache_control").is_some())
                .count()
        };
        // 40 messages of one block each, then the final prompt.
        let mut long: Vec<Value> = (0..40)
            .map(|n| turn(if n % 2 == 0 { "assistant" } else { "user" }, n))
            .collect();
        long.push(turn("user", 99));
        assert!(AnthropicMessagesProvider::anchor_last_completed_turn(
            &mut long,
            Some(&cc)
        ));
        assert!(AnthropicMessagesProvider::anchor_lookback_block(
            &mut long,
            Some(&cc)
        ));
        assert_eq!(count(&long), 2);
        // The turn anchor sits on message 39; the second one 16 blocks back.
        assert!(long[39]["content"][0].get("cache_control").is_some());
        assert!(long[39 - 16]["content"][0].get("cache_control").is_some());

        // A short conversation keeps the single turn anchor.
        let mut short: Vec<Value> = (0..6)
            .map(|n| turn(if n % 2 == 0 { "assistant" } else { "user" }, n))
            .collect();
        assert!(AnthropicMessagesProvider::anchor_last_completed_turn(
            &mut short,
            Some(&cc)
        ));
        assert!(!AnthropicMessagesProvider::anchor_lookback_block(
            &mut short,
            Some(&cc)
        ));
        assert_eq!(count(&short), 1);
    }

    #[test]
    fn a_marker_ending_the_text_marks_one_block_and_adds_no_empty_one() {
        let cc = serde_json::json!({"type": "ephemeral"});
        let text = format!("stable prompt\n{}", crate::types::CACHE_BREAKPOINT_SENTINEL);
        let content = AnthropicMessagesProvider::user_text_content(text, Some(&cc));
        let blocks = content.as_array().expect("block array");
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0]["text"], "stable prompt");
        assert_eq!(blocks[0]["cache_control"]["type"], "ephemeral");
    }
    use super::*;
    use serde_json::json;

    /// The uncatalogued path: request shape from the model-name fallback.
    fn fallback_reasoning(
        kind: &LLMProviderKind,
        model: &str,
        body: &mut Map<String, Value>,
        reasoning: &ReasoningConfig,
    ) -> LLMResult<()> {
        let wire = wire_for(kind, model, None);
        AnthropicMessagesProvider::insert_provider_reasoning_config(
            kind, model, &wire, body, reasoning,
        )
    }

    /// Catalog facts for a model resolved from YAML, as the router hands
    /// them to the adapter.
    fn catalogued(thinking: &str, wire: &str) -> ModelWire {
        let thinking: crate::catalog::ThinkingControl = serde_yaml::from_str(thinking).unwrap();
        let wire: crate::catalog::ModelWireTraits = serde_yaml::from_str(wire).unwrap();
        wire.resolve(&thinking)
    }

    const EFFORT_WITH_OFF: &str =
        "{mode: effort, levels: [none, low, medium, high, xhigh, max], default: medium}";

    #[test]
    fn server_web_search_tool_replaces_function_tools() {
        let provider = AnthropicMessagesProvider::new("test-key");
        let mut request = LLMRequest::default();
        request.model = "claude-sonnet-4-5".to_string();
        request.set_extra(json!({"server_web_search": {"max_uses": 2}}));

        let mut body = Map::new();
        provider
            .attach_tools_or_server_web_search(&mut body, &request, None)
            .expect("attach succeeds");
        assert_eq!(body["tools"][0]["type"], "web_search_20250305");
        assert_eq!(body["tools"][0]["max_uses"], 2);
        assert!(body.get("tool_choice").is_none());
    }

    #[test]
    fn server_web_search_rejects_mixed_and_stays_off_unflagged() {
        let provider = AnthropicMessagesProvider::new("test-key");

        let mut mixed = LLMRequest::default();
        mixed.model = "claude-sonnet-4-5".to_string();
        mixed.set_extra(json!({"server_web_search": true}));
        mixed.tools = vec![LLMToolSpec {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: json!({}),
        }]
        .into();
        let mut body = Map::new();
        let error = provider
            .attach_tools_or_server_web_search(&mut body, &mixed, None)
            .expect_err("mixed request must fail closed");
        assert!(error.to_string().contains("cannot be combined"));

        let mut plain = LLMRequest::default();
        plain.model = "claude-sonnet-4-5".to_string();
        let mut plain_body = Map::new();
        provider
            .attach_tools_or_server_web_search(&mut plain_body, &plain, None)
            .expect("plain request attaches nothing");
        assert!(plain_body.get("tools").is_none());
    }

    #[test]
    fn disclosure_bound_request_drops_provider_cache_control_extra() {
        assert!(!AnthropicMessagesProvider::should_forward_extra(
            "cache_control",
            true
        ));
        assert!(AnthropicMessagesProvider::should_forward_extra(
            "cache_control",
            false
        ));
        assert!(AnthropicMessagesProvider::should_forward_extra(
            "custom_provider_option",
            true
        ));
    }

    #[test]
    fn usage_mapping_rejects_provider_counter_larger_than_u32() {
        let usage = json!({
            "input_tokens": u64::from(u32::MAX) + 1,
            "output_tokens": 0,
        });

        assert!(matches!(
            AnthropicMessagesProvider::map_usage(&LLMProviderKind::Anthropic, &usage),
            Err(LLMError::Provider { .. })
        ));
    }

    #[test]
    fn usage_mapping_rejects_aggregate_sum_overflow() {
        let anthropic = json!({
            "input_tokens": u32::MAX,
            "output_tokens": 1,
        });
        let deepseek = json!({
            "prompt_cache_hit_tokens": u32::MAX,
            "prompt_cache_miss_tokens": 1,
            "output_tokens": 0,
        });

        assert!(matches!(
            AnthropicMessagesProvider::map_usage(&LLMProviderKind::Anthropic, &anthropic),
            Err(LLMError::Provider { .. })
        ));
        assert!(matches!(
            AnthropicMessagesProvider::map_usage(&LLMProviderKind::DeepSeek, &deepseek),
            Err(LLMError::Provider { .. })
        ));
    }

    #[test]
    fn terminal_sse_event_is_consumed_before_the_parser_stops() {
        let mut state = AnthropicStreamState::new(LLMProviderKind::Anthropic);
        let trailing = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"must-not-apply\"}}\n\n";
        let mut buffer = format!("data: {{\"type\":\"message_stop\"}}\n\n{trailing}");

        AnthropicMessagesProvider::drain_sse_buffer(&mut buffer, &mut state)
            .expect("terminal event");

        assert!(state.done());
        assert_eq!(buffer, trailing);
        AnthropicMessagesProvider::drain_sse_buffer(&mut buffer, &mut state)
            .expect("post-terminal bytes remain untouched");
        assert_eq!(buffer, trailing);
    }

    #[tokio::test]
    async fn terminal_delta_sse_event_is_consumed_before_the_parser_stops() {
        let mut state = AnthropicStreamState::new(LLMProviderKind::Anthropic);
        let trailing = "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"must-not-emit\"}}\n\n";
        let mut buffer = format!("data: {{\"type\":\"message_stop\"}}\n\n{trailing}");
        let (tx, mut rx) = mpsc::channel(1);

        AnthropicMessagesProvider::drain_sse_buffer_with_deltas(&mut buffer, &mut state, &tx)
            .await
            .expect("terminal event");

        assert!(state.done());
        assert_eq!(buffer, trailing);
        AnthropicMessagesProvider::drain_sse_buffer_with_deltas(&mut buffer, &mut state, &tx)
            .await
            .expect("post-terminal bytes remain untouched");
        assert!(matches!(
            rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn test_stream_state_accumulates_text() {
        let mut state = AnthropicStreamState::new(LLMProviderKind::Anthropic);

        let events = vec![
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[]}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"Hello"}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":" world"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","usage":{"input_tokens":10,"output_tokens":20}}}),
            json!({"type":"message_stop"}),
        ];

        let mut done = false;
        for event in events {
            done = state.apply_event(&event).unwrap() || done;
        }
        assert!(done);

        let response = state.into_response().unwrap();
        assert_eq!(response.text.as_deref(), Some("Hello world"));
        assert_eq!(response.finish_reason.as_deref(), Some("end_turn"));
        let usage = response.usage.expect("usage should be present");
        assert_eq!(usage.prompt_tokens, Some(10));
        assert_eq!(usage.completion_tokens, Some(20));
        assert_eq!(usage.total_tokens, Some(30));

        assert_eq!(response.messages.len(), 1);
        assert_eq!(response.messages[0].content.len(), 1);
        match &response.messages[0].content[0] {
            ContentBlock::Text { text } => assert_eq!(text, "Hello world"),
            block => panic!("unexpected block: {:?}", block),
        }
    }

    #[test]
    fn stream_usage_merges_message_start_and_top_level_message_delta() {
        let mut state = AnthropicStreamState::new(LLMProviderKind::Anthropic);
        for event in [
            json!({"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","content":[],
                "usage":{"input_tokens":1200,"cache_creation_input_tokens":300,"cache_read_input_tokens":500,"output_tokens":1}}}),
            json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"OK"}}),
            json!({"type":"content_block_stop","index":0}),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn","stop_sequence":null},"usage":{"output_tokens":42}}),
            json!({"type":"message_stop"}),
        ] {
            state.apply_event(&event).unwrap();
        }
        let usage = state.into_response().unwrap().usage.expect("usage");
        assert_eq!(usage.completion_tokens, Some(42));
        assert!(usage.prompt_tokens.unwrap_or(0) >= 1200, "{usage:?}");
        assert_eq!(usage.cached_tokens, Some(500));
        assert_eq!(usage.cache_creation_tokens, Some(300));
    }

    #[test]
    fn test_stream_state_collects_tool_calls() {
        let mut state = AnthropicStreamState::new(LLMProviderKind::Anthropic);
        let partial_1 = "{\"city\":\"San";
        let partial_2 = " Francisco\"}";

        let events = vec![
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"extract_data","input":{}}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":partial_1}}),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":partial_2}}),
            json!({"type":"content_block_stop","index":1}),
            json!({"type":"message_stop"}),
        ];

        for event in events {
            state.apply_event(&event).unwrap();
        }

        let response = state.into_response().unwrap();
        assert!(response.text.is_none());
        assert_eq!(response.tool_calls.len(), 1);

        let tool_call = &response.tool_calls[0];
        assert_eq!(tool_call.id, "toolu_1");
        assert_eq!(tool_call.name, "extract_data");
        assert_eq!(tool_call.arguments, json!({"city":"San Francisco"}));

        assert_eq!(response.messages.len(), 1);
        assert_eq!(response.messages[0].content.len(), 1);
        match &response.messages[0].content[0] {
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => {
                assert_eq!(id, "toolu_1");
                assert_eq!(name, "extract_data");
                assert_eq!(arguments, &json!({"city":"San Francisco"}));
            },
            block => panic!("unexpected block: {:?}", block),
        }
    }
    #[test]
    fn map_tools_moves_whole_call_rules_out_of_the_input_schema() {
        use serde_json::json;

        // Anthropic answers a top-level anyOf/oneOf/allOf with a 400 for the
        // whole request (2026-10-30, a pack's whole-call constraint).
        let tools = vec![LLMToolSpec {
            name: "shell".to_string(),
            description: "Run a command.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {"command": {"type": "string"}, "job_id": {"type": "string"}},
                "required": [],
                "allOf": [{"anyOf": [{"required": ["command"]}, {"required": ["job_id"]}]}],
            }),
        }];

        let mapped = AnthropicMessagesProvider::map_tools(&tools, None);
        let schema = &mapped[0]["input_schema"];
        for key in ["allOf", "anyOf", "oneOf", "not"] {
            assert!(schema.get(key).is_none(), "{key} reached Anthropic");
        }
        assert_eq!(schema["properties"], tools[0].parameters["properties"]);
        let description = mapped[0]["description"].as_str().unwrap();
        assert!(description.starts_with("Run a command.\n\n"));
        assert!(description.contains(r#"{"required":["command"]}"#));
    }

    #[test]
    fn map_tools_produces_anthropic_format() {
        use serde_json::json;

        let tools = vec![LLMToolSpec {
            name: "test_tool".to_string(),
            description: "A test tool".to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"}
                },
                "required": ["query"]
            }),
        }];

        let mapped = AnthropicMessagesProvider::map_tools(&tools, None);
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0]["name"], "test_tool");
        assert_eq!(mapped[0]["description"], "A test tool");
        assert!(mapped[0]["input_schema"].is_object());
        assert_eq!(mapped[0]["input_schema"]["type"], "object");
        assert!(
            mapped[0].get("cache_control").is_none(),
            "no cache_control when caching disabled"
        );
    }

    #[test]
    fn anthropic_reasoning_none_sends_disabled_thinking_for_claude() {
        let mut body = Map::new();
        fallback_reasoning(
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &mut body,
            &ReasoningConfig {
                effort: Some("none".to_string()),
                strategy: Some("extended_thinking".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(body["thinking"], json!({"type": "disabled"}));
    }

    #[test]
    fn claude_off_is_explicit_and_mandatory_reasoners_reject_it() {
        let off = ReasoningConfig {
            effort: Some("none".into()),
            ..Default::default()
        };
        for model in [
            "claude-haiku-4-5",
            "claude-opus-4-7",
            "claude-opus-5",
            "claude-haiku-5-5",
        ] {
            let mut body = Map::new();
            fallback_reasoning(&LLMProviderKind::Anthropic, model, &mut body, &off).unwrap();
            assert_eq!(body["thinking"], json!({"type": "disabled"}), "{model}");
            if model == "claude-opus-5" || model == "claude-haiku-5-5" {
                assert_eq!(body["output_config"]["effort"], "low");
            }
        }
        for model in ["claude-opus-5-5", "claude-fable-5-1", "claude-sonnet-5-5"] {
            let mut body = Map::new();
            assert!(
                fallback_reasoning(&LLMProviderKind::Anthropic, model, &mut body, &off).is_err(),
                "{model} must not silently enable reasoning for Off"
            );
            assert!(body.is_empty());
        }
    }

    #[test]
    fn anthropic_reasoning_high_enables_claude_thinking() {
        let mut body = Map::new();
        fallback_reasoning(
            &LLMProviderKind::Anthropic,
            "claude-sonnet-4-6",
            &mut body,
            &ReasoningConfig {
                effort: Some("high".to_string()),
                max_reasoning_tokens: Some(8192),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["thinking"]["display"], "summarized");
        assert_eq!(body["output_config"], json!({ "effort": "high" }));
    }

    #[test]
    fn fable_and_opus5_use_adaptive_thinking_not_budget_tokens() {
        for model in [
            "claude-fable-5-1",
            "claude-opus-5",
            "claude-opus-5-5",
            "claude-sonnet-5-5",
            "claude-haiku-5-5",
            "anthropic/claude-fable-5-1",
        ] {
            let mut body = Map::new();
            fallback_reasoning(
                &LLMProviderKind::Anthropic,
                model,
                &mut body,
                &ReasoningConfig {
                    effort: Some("low".to_string()),
                    max_reasoning_tokens: Some(8192),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(
                body["thinking"]["type"], "adaptive",
                "{model} must not send enabled+budget_tokens"
            );
            assert!(body["thinking"].get("budget_tokens").is_none());
            assert_eq!(body["output_config"], json!({ "effort": "low" }));
        }
    }

    #[test]
    fn deepseek_reasoning_none_sends_disabled_thinking() {
        let mut body = Map::new();
        fallback_reasoning(
            &LLMProviderKind::DeepSeek,
            "deepseek-v4-pro",
            &mut body,
            &ReasoningConfig {
                effort: Some("none".to_string()),
                strategy: Some("deepseek_thinking".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert!(body.get("output_config").is_none());
    }

    #[test]
    fn deepseek_low_effort_stays_low_on_the_wire() {
        let mut body = Map::new();
        fallback_reasoning(
            &LLMProviderKind::DeepSeek,
            "deepseek-flash",
            &mut body,
            &ReasoningConfig {
                effort: Some("low".into()),
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(body["thinking"], json!({"type": "enabled"}));
        assert_eq!(body["output_config"], json!({"effort": "low"}));
    }

    #[test]
    fn deepseek_reasoning_max_uses_deepseek_thinking_shape() {
        let mut body = Map::new();
        fallback_reasoning(
            &LLMProviderKind::DeepSeek,
            "deepseek-v4-pro",
            &mut body,
            &ReasoningConfig {
                effort: Some("max".to_string()),
                strategy: Some("deepseek_thinking".to_string()),
                ..Default::default()
            },
        )
        .unwrap();

        assert_eq!(body["thinking"], json!({ "type": "enabled" }));
        assert_eq!(body["output_config"], json!({ "effort": "max" }));
    }

    #[test]
    fn map_tools_attaches_cache_control_to_last_tool_only() {
        use serde_json::json;

        let tools = vec![
            LLMToolSpec {
                name: "first".to_string(),
                description: "first tool".to_string(),
                parameters: json!({"type": "object", "properties": {}, "required": []}),
            },
            LLMToolSpec {
                name: "second".to_string(),
                description: "second tool".to_string(),
                parameters: json!({"type": "object", "properties": {}, "required": []}),
            },
            LLMToolSpec {
                name: "third".to_string(),
                description: "third tool".to_string(),
                parameters: json!({"type": "object", "properties": {}, "required": []}),
            },
        ];

        let cc = AnthropicMessagesProvider::build_ephemeral_cache_control(None);
        let mapped = AnthropicMessagesProvider::map_tools(&tools, Some(&cc));

        // Anthropic prompt-caching contract: cache_control on the last
        // tool caches all preceding tools as a stable prefix. Earlier
        // tools must NOT carry the marker — that would create extra
        // (redundant) cache breakpoints.
        assert_eq!(mapped.len(), 3);
        assert!(mapped[0].get("cache_control").is_none());
        assert!(mapped[1].get("cache_control").is_none());
        let last_cc = mapped[2].get("cache_control").expect("last tool cached");
        assert_eq!(last_cc["type"], "ephemeral");
    }

    #[test]
    fn parse_output_preserves_thinking_blocks_for_replay() {
        let payload = json!({
            "content": [
                {
                    "type": "thinking",
                    "thinking": "Need to inspect the account state before choosing a tool.",
                    "signature": "sig-123"
                },
                {
                    "type": "redacted_thinking",
                    "data": "encrypted-redacted-block"
                },
                {
                    "type": "tool_use",
                    "id": "toolu_1",
                    "name": "lookup_account",
                    "input": { "account_id": "acct_1" }
                }
            ]
        });

        let (messages, tool_calls, _tool_results, text, reasoning_text) =
            AnthropicMessagesProvider::parse_output(&payload).expect("bounded response");

        assert!(text.is_none());
        assert_eq!(
            reasoning_text.as_deref(),
            Some("Need to inspect the account state before choosing a tool.")
        );
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.len(), 3);

        let ContentBlock::Json { value } = &messages[0].content[0] else {
            panic!("thinking block should be preserved as provider-native JSON");
        };
        let raw = as_anthropic_raw_content_block(&value).expect("raw thinking block");
        assert_eq!(raw["type"], "thinking");
        assert_eq!(
            raw["thinking"],
            "Need to inspect the account state before choosing a tool."
        );
        assert_eq!(raw["signature"], "sig-123");
        let ContentBlock::Json { value } = &messages[0].content[1] else {
            panic!("redacted thinking block should be preserved as provider-native JSON");
        };
        let redacted = as_anthropic_raw_content_block(&value).expect("raw redacted block");
        assert_eq!(redacted["type"], "redacted_thinking");
        assert_eq!(redacted["data"], "encrypted-redacted-block");

        let (_system, remapped) =
            AnthropicMessagesProvider::map_messages(&messages).expect("remap");
        assert_eq!(remapped[0]["content"][0]["type"], "thinking");
        assert_eq!(remapped[0]["content"][0]["signature"], "sig-123");
        assert_eq!(remapped[0]["content"][1]["type"], "redacted_thinking");
        assert_eq!(remapped[0]["content"][2]["type"], "tool_use");
    }

    #[test]
    fn sampling_is_omitted_only_for_models_that_reject_it() {
        let request = LLMRequest {
            temperature: Some(0.2),
            top_p: Some(0.9),
            ..Default::default()
        };
        for model in [
            "claude-sonnet-5-5",
            "claude-opus-5-5",
            "claude-opus-4-7",
            "claude-fable-5-1",
            "claude-haiku-5-5",
            "anthropic/claude-sonnet-5-5",
        ] {
            let mut body = Map::new();
            let wire = wire_for(&LLMProviderKind::Anthropic, model, None);
            AnthropicMessagesProvider::insert_sampling(&mut body, model, &wire, &request);
            assert!(body.is_empty(), "{model} must not send sampling: {body:?}");
        }
        for model in ["claude-sonnet-4-6", "claude-haiku-4-5", "deepseek-flash"] {
            let mut body = Map::new();
            let wire = wire_for(&LLMProviderKind::Anthropic, model, None);
            AnthropicMessagesProvider::insert_sampling(&mut body, model, &wire, &request);
            assert_eq!(body["temperature"], Value::from(0.2_f32), "{model}");
            assert_eq!(body["top_p"], Value::from(0.9_f32), "{model}");
        }
    }

    /// A catalogued model is shaped by its catalog facts alone: a made-up id
    /// declaring Haiku 5.5's facts gets Haiku 5.5's request, and a real
    /// Claude name without them gets the plain shape.
    #[test]
    fn catalogued_models_are_shaped_by_catalog_facts_not_names() {
        let haiku_like = catalogued(
            EFFORT_WITH_OFF,
            "{sampling: false, thinking_off_effort: low}",
        );
        let plain = catalogued(
            "{mode: budget, levels: [none, low], default: low, budget: {min_tokens: 1024, max_tokens: 32000, step_tokens: 1024}}",
            "{}",
        );
        let low = ReasoningConfig {
            effort: Some("low".into()),
            max_reasoning_tokens: Some(2048),
            ..Default::default()
        };
        let off = ReasoningConfig {
            effort: Some("none".into()),
            ..Default::default()
        };
        let request = LLMRequest {
            temperature: Some(0.2),
            top_p: Some(0.9),
            ..Default::default()
        };
        let shape = |model: &str, wire: &ModelWire, reasoning: &ReasoningConfig| {
            let mut body = Map::new();
            let wire = wire_for(&LLMProviderKind::Anthropic, model, Some(wire));
            AnthropicMessagesProvider::insert_sampling(&mut body, model, &wire, &request);
            AnthropicMessagesProvider::insert_provider_reasoning_config(
                &LLMProviderKind::Anthropic,
                model,
                &wire,
                &mut body,
                reasoning,
            )
            .map(|()| body)
        };

        let body = shape("acme-small-7", &haiku_like, &low).unwrap();
        assert_eq!(body["thinking"]["type"], "adaptive");
        assert_eq!(body["output_config"], json!({ "effort": "low" }));
        assert!(body.get("temperature").is_none() && body.get("top_p").is_none());
        let body = shape("acme-small-7", &haiku_like, &off).unwrap();
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert_eq!(body["output_config"], json!({ "effort": "low" }));

        // `claude-haiku-5-5` would match every fallback rule; its catalog
        // entry here declares none of them.
        let body = shape("claude-haiku-5-5", &plain, &low).unwrap();
        assert_eq!(body["thinking"]["type"], "enabled");
        assert_eq!(body["thinking"]["budget_tokens"], 2048);
        assert_eq!(body["temperature"], Value::from(0.2_f32));
        let body = shape("claude-haiku-5-5", &plain, &off).unwrap();
        assert_eq!(body["thinking"], json!({ "type": "disabled" }));
        assert!(body.get("output_config").is_none());

        let mandatory = catalogued(
            "{mode: effort, levels: [low, high], default: low}",
            "{sampling: false}",
        );
        assert!(shape("claude-haiku-4-5", &mandatory, &off).is_err());
    }

    #[test]
    fn uncatalogued_names_keep_the_fallback_rules() {
        let wire = uncatalogued_wire(&LLMProviderKind::Anthropic, "claude-haiku-5-5");
        assert_eq!(
            wire,
            ModelWire {
                thinking: Some(ThinkingWire::Adaptive),
                thinking_off: true,
                thinking_off_effort: Some("low".into()),
                sampling: false,
            }
        );
        let wire = uncatalogued_wire(&LLMProviderKind::Anthropic, "claude-haiku-4-5");
        assert_eq!(wire.thinking, Some(ThinkingWire::Budget));
        assert!(wire.thinking_off && wire.sampling && wire.thinking_off_effort.is_none());
        let wire = uncatalogued_wire(&LLMProviderKind::DeepSeek, "deepseek-flash");
        assert_eq!(wire.thinking, Some(ThinkingWire::Budget));
        assert!(wire.sampling && !wire.thinking_off);
        assert!(anthropic_model_uses_adaptive_thinking("claude-opus-5-5"));
        assert!(!anthropic_model_uses_adaptive_thinking("claude-haiku-4-5"));
    }

    #[test]
    fn tool_choice_defaults_to_auto_when_no_extra() {
        use serde_json::json;

        let request = LLMRequest::default();
        assert_eq!(
            AnthropicMessagesProvider::tool_choice_for_request(&request),
            json!({"type": "auto"})
        );
    }

    #[test]
    fn tool_choice_respects_extra_override() {
        use serde_json::json;

        let mut request = LLMRequest::default();
        request.set_extra(
            json!({
                "tool_choice": {"type": "auto"}
            })
            .into(),
        );
        assert_eq!(
            AnthropicMessagesProvider::tool_choice_for_request(&request),
            json!({"type": "auto"})
        );
    }

    /// The canonical "tools advertised, use forbidden" choice is Anthropic's
    /// own `{"type": "none"}`; it passes through unchanged.
    #[test]
    fn openai_style_tool_choice_becomes_an_anthropic_object() {
        for (choice, expected) in [
            (json!("auto"), json!({"type": "auto"})),
            (json!("none"), json!({"type": "none"})),
            (json!("required"), json!({"type": "any"})),
            (
                json!({"type": "function", "function": {"name": "search"}}),
                json!({"type": "tool", "name": "search"}),
            ),
        ] {
            let mut request = LLMRequest::default();
            request.set_extra(json!({ "tool_choice": choice }).into());
            assert_eq!(
                AnthropicMessagesProvider::tool_choice_for_request(&request),
                expected
            );
        }
    }

    #[test]
    fn tool_choice_none_passes_through() {
        use serde_json::json;

        let mut request = LLMRequest::default();
        request.set_extra(json!({ "tool_choice": {"type": "none"} }).into());
        assert_eq!(
            AnthropicMessagesProvider::tool_choice_for_request(&request),
            json!({"type": "none"})
        );
    }

    #[test]
    fn replayed_tool_calls_always_carry_an_object_input() {
        let truncated = format!("{{\"context\":\"{}", "x".repeat(5_000));
        let cases = [
            (json!(truncated), true),
            (json!("[1, 2]"), true),
            (json!(42), true),
            (json!(null), false),
            (json!("  "), false),
            (json!("{\"prompt\":\"poster\"}"), false),
            (json!({ "prompt": "poster" }), false),
        ];
        for (arguments, flagged) in cases {
            let input = AnthropicMessagesProvider::tool_use_input(&arguments);
            assert!(input.is_object(), "{arguments} mapped to {input}");
            assert_eq!(input.get("_unparsed_arguments").is_some(), flagged);
        }
        let preview = AnthropicMessagesProvider::tool_use_input(&json!(truncated));
        assert!(preview["_unparsed_arguments"].as_str().unwrap().len() < 400);
    }

    #[test]
    fn map_messages_supports_tool_use_and_rich_tool_result_blocks() {
        let messages = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "toolu_123".to_string(),
                    name: "generate_image".to_string(),
                    arguments: json!({ "prompt": "poster" }),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "toolu_123".to_string(),
                    content: json!({
                        "_magicllm_rich_tool_result": true,
                        "blocks": [
                            { "type": "text", "text": "Generated a draft." },
                            {
                                "type": "image",
                                "media_type": "image/png",
                                "data_base64": "aGVsbG8="
                            }
                        ]
                    }),
                }],
            },
        ];

        let (_system_prompt, mapped) =
            AnthropicMessagesProvider::map_messages(&messages).expect("anthropic mapping");

        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0]["role"], "assistant");
        assert_eq!(mapped[0]["content"][0]["type"], "tool_use");
        assert_eq!(mapped[0]["content"][0]["id"], "toolu_123");
        assert_eq!(mapped[0]["content"][0]["name"], "generate_image");
        assert_eq!(mapped[0]["content"][0]["input"]["prompt"], "poster");

        assert_eq!(mapped[1]["role"], "user");
        assert_eq!(mapped[1]["content"][0]["type"], "tool_result");
        assert_eq!(mapped[1]["content"][0]["tool_use_id"], "toolu_123");
        assert!(mapped[1]["content"][0]["content"].is_array());
        assert_eq!(mapped[1]["content"][0]["content"][0]["type"], "text");
        assert_eq!(
            mapped[1]["content"][0]["content"][0]["text"],
            "Generated a draft."
        );
        assert_eq!(mapped[1]["content"][0]["content"][1]["type"], "image");
        assert_eq!(
            mapped[1]["content"][0]["content"][1]["source"]["media_type"],
            "image/png"
        );
        assert_eq!(
            mapped[1]["content"][0]["content"][1]["source"]["data"],
            "aGVsbG8="
        );
    }

    fn pairing_tool_call(id: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.to_string(),
            name: "run_project_checks".to_string(),
            arguments: json!({}),
        }
    }

    fn pairing_tool_result(id: &str, output: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            tool_call_id: id.to_string(),
            content: Value::String(output.to_string()),
        }
    }

    /// `(type, tool_use_id)` of each block of a mapped message.
    fn result_shape(message: &Value) -> Vec<(String, String)> {
        message["content"]
            .as_array()
            .expect("content blocks")
            .iter()
            .map(|block| {
                (
                    block["type"].as_str().unwrap_or_default().to_string(),
                    block["tool_use_id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string(),
                )
            })
            .collect()
    }

    #[test]
    fn a_parallel_tool_use_without_a_result_is_answered_right_after_it() {
        let messages = vec![
            LLMMessage::user("do two things"),
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![pairing_tool_call("toolu_a"), pairing_tool_call("toolu_b")],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![
                    pairing_tool_result("toolu_a", "applied"),
                    ContentBlock::Text {
                        text: "state".to_string(),
                    },
                ],
            },
        ];

        let (_system, mapped) = AnthropicMessagesProvider::map_messages(&messages).expect("maps");

        assert_eq!(mapped.len(), 3);
        assert_eq!(
            result_shape(&mapped[2]),
            vec![
                ("tool_result".to_string(), "toolu_a".to_string()),
                ("tool_result".to_string(), "toolu_b".to_string()),
                ("text".to_string(), String::new()),
            ]
        );
        assert_eq!(mapped[2]["content"][0]["content"], "applied");
        assert_eq!(
            mapped[2]["content"][1]["content"],
            crate::providers::MISSING_TOOL_RESULT_TEXT
        );
        assert_eq!(mapped[2]["content"][1]["is_error"], true);
    }

    #[test]
    fn a_refused_tool_use_is_answered_and_a_trailing_one_gets_a_user_turn() {
        let messages = vec![
            LLMMessage::user("go"),
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![pairing_tool_call("toolu_refused")],
            },
            // The loop refused the call and only said so in text.
            LLMMessage::user("that call was refused"),
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![pairing_tool_call("toolu_last")],
            },
        ];

        let (_system, mapped) = AnthropicMessagesProvider::map_messages(&messages).expect("maps");

        assert_eq!(mapped.len(), 5);
        assert_eq!(
            result_shape(&mapped[2]),
            vec![
                ("tool_result".to_string(), "toolu_refused".to_string()),
                ("text".to_string(), String::new()),
            ]
        );
        assert_eq!(mapped[4]["role"], "user");
        assert_eq!(
            result_shape(&mapped[4]),
            vec![("tool_result".to_string(), "toolu_last".to_string())]
        );
    }

    #[test]
    fn a_paired_history_is_unchanged_and_a_duplicate_result_dropped() {
        let paired = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![pairing_tool_call("toolu_1")],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![pairing_tool_result("toolu_1", "ok")],
            },
        ];
        let (_system, mapped) = AnthropicMessagesProvider::map_messages(&paired).expect("maps");
        assert_eq!(
            Value::Array(mapped),
            json!([
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "toolu_1", "name": "run_project_checks", "input": {}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "toolu_1", "content": "ok"}
                ]},
            ])
        );

        let mut duplicated = paired.clone();
        duplicated[1]
            .content
            .push(pairing_tool_result("toolu_1", "again"));
        let (_system, mapped) = AnthropicMessagesProvider::map_messages(&duplicated).expect("maps");
        assert_eq!(
            result_shape(&mapped[1]),
            vec![("tool_result".to_string(), "toolu_1".to_string())]
        );
        assert_eq!(mapped[1]["content"][0]["content"], "ok");
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

        let (_system, mapped) =
            AnthropicMessagesProvider::map_messages(&messages).expect("map messages");

        let blocks = mapped[0]["content"].as_array().expect("content blocks");
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "call-a");
        assert_eq!(blocks[1]["tool_use_id"], "call-b");
        let decoded_first: Value =
            serde_json::from_str(blocks[0]["content"].as_str().expect("first JSON string"))
                .expect("first projection serialized exactly once");
        let decoded_second: Value =
            serde_json::from_str(blocks[1]["content"].as_str().expect("second JSON string"))
                .expect("second projection serialized exactly once");
        assert_eq!(decoded_first, first);
        assert_eq!(decoded_second, second);
    }

    #[test]
    fn anthropic_retry_max_output_tokens_doubles_and_caps() {
        assert_eq!(
            AnthropicMessagesProvider::retry_max_output_tokens(4096),
            8192
        );
        assert_eq!(
            AnthropicMessagesProvider::retry_max_output_tokens(20_000),
            ANTHROPIC_MAX_TOKENS_RETRY_CAP
        );
    }

    #[test]
    fn anthropic_backed_off_max_output_tokens_sits_between_base_and_retry() {
        assert_eq!(
            AnthropicMessagesProvider::backed_off_max_output_tokens(4096, 8192),
            Some(6144)
        );
        assert_eq!(
            AnthropicMessagesProvider::backed_off_max_output_tokens(20_000, 32_000),
            Some(30_000)
        );
    }

    #[test]
    fn anthropic_excessive_max_tokens_error_detection_matches_provider_messages() {
        let error = LLMError::Provider {
            provider: "anthropic".to_string(),
            message: "max_tokens must be less than or equal to 8192".to_string(),
        };
        assert!(AnthropicMessagesProvider::is_excessive_max_tokens_error(
            &error
        ));

        let unrelated = LLMError::Provider {
            provider: "anthropic".to_string(),
            message: "rate limit exceeded".to_string(),
        };
        assert!(!AnthropicMessagesProvider::is_excessive_max_tokens_error(
            &unrelated
        ));
    }
}

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::{debug, error, info};

use super::{
    bounded_usage_counter, checked_usage_sum, default_http_client, read_bounded_response_text,
};
use crate::ollama_keep_alive;
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind, LLMReasoning},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        clone_json_value_iteratively, parse_provider_json_value, ContentBlock, EmbeddingRequest,
        EmbeddingResponse, LLMMessage, LLMRequest, LLMResponse, LLMResponseFormat, LLMToolCall,
        MessageRole, ReasoningConfig, RequestMetadata, TokenUsage,
    },
};

const DEFAULT_BASE_URL: &str = "http://localhost:11434/api/generate";

/// Provider implementation for local Ollama models.
pub struct OllamaProvider {
    client: Client,
    base_url: String,
    default_timeout: Duration,
    keep_alive: Option<String>,
}

/// Whether a configured Ollama base URL selects the native chat contract,
/// which carries tools, rather than the completion contract, which refuses
/// them at call time. This provider is the single place that knows the
/// endpoint shape; configuration validation asks it instead of matching the
/// path itself.
pub fn base_url_uses_chat_api(base_url: &str) -> bool {
    base_url.trim_end_matches('/').ends_with("/api/chat")
}

impl OllamaProvider {
    /// Creates a provider using the default Ollama endpoint.
    pub fn new() -> Self {
        Self::with_client(default_http_client(), DEFAULT_BASE_URL)
    }

    /// Creates a provider with a custom base URL (useful for remote Ollama instances).
    pub fn with_base_url(base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), base_url)
    }

    /// Creates a provider with a preconfigured HTTP client.
    pub fn with_client(client: Client, base_url: impl Into<String>) -> Self {
        Self {
            client,
            base_url: base_url.into(),
            default_timeout: Duration::from_secs(180),
            keep_alive: ollama_keep_alive::default_keep_alive(),
        }
    }

    /// Override the Ollama model residency window sent as request `keep_alive`.
    /// `None` leaves residency to the daemon/global Ollama setting.
    pub fn with_keep_alive(mut self, keep_alive: Option<String>) -> Self {
        self.keep_alive =
            keep_alive.and_then(|value| ollama_keep_alive::normalize_keep_alive(&value));
        self
    }

    fn detect_capability(tool_calling: bool) -> LLMCapability {
        LLMCapability {
            modalities: vec![LLMModality::Text],
            reasoning: LLMReasoning::None,
            tool_calling,
            json_mode: true,
            streaming: false,
            computer_use: false,
            web_search: false,
        }
    }

    fn uses_chat_api(&self) -> bool {
        base_url_uses_chat_api(&self.base_url)
    }

    fn chat_role(role: MessageRole) -> &'static str {
        match role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        }
    }

    fn map_chat_messages(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        let mut mapped = Vec::new();
        for message in messages {
            let mut content = Vec::new();
            let mut tool_calls = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        content.push(Self::strip_cache_sentinel(text));
                    },
                    ContentBlock::Json { value } => {
                        content.push(serde_json::to_string(value)?);
                    },
                    ContentBlock::ToolCall {
                        name, arguments, ..
                    } => {
                        tool_calls.push(json!({
                            "function": {
                                "name": name,
                                "arguments": clone_json_value_iteratively(arguments),
                            }
                        }));
                    },
                    ContentBlock::ToolResult { content, .. } => {
                        mapped.push(json!({
                            "role": "tool",
                            "content": serde_json::to_string(content)?,
                        }));
                    },
                    ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. } => {
                        return Err(LLMError::UnsupportedCapability(
                            "Ollama app chat does not admit image content".to_owned(),
                        ));
                    },
                }
            }
            if !content.is_empty() || !tool_calls.is_empty() {
                let mut object = serde_json::Map::new();
                object.insert(
                    "role".to_owned(),
                    Value::String(Self::chat_role(message.role).to_owned()),
                );
                object.insert("content".to_owned(), Value::String(content.join("\n")));
                if !tool_calls.is_empty() {
                    object.insert("tool_calls".to_owned(), Value::Array(tool_calls));
                }
                mapped.push(Value::Object(object));
            }
        }
        Ok(mapped)
    }

    fn map_chat_tools(request: &LLMRequest) -> Vec<Value> {
        request
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "function": {
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": clone_json_value_iteratively(&tool.parameters),
                    }
                })
            })
            .collect()
    }

    fn map_chat_tool_calls(payload: &Value) -> LLMResult<Vec<LLMToolCall>> {
        let Some(calls) = payload
            .get("message")
            .and_then(|message| message.get("tool_calls"))
            .and_then(Value::as_array)
        else {
            return Ok(Vec::new());
        };
        let mut mapped = Vec::with_capacity(calls.len());
        for (index, call) in calls.iter().enumerate() {
            let function = call.get("function").ok_or_else(|| LLMError::Provider {
                provider: "ollama".to_owned(),
                message: "invalid tool-call envelope".to_owned(),
            })?;
            let name = function
                .get("name")
                .and_then(Value::as_str)
                .filter(|name| !name.trim().is_empty())
                .ok_or_else(|| LLMError::Provider {
                    provider: "ollama".to_owned(),
                    message: "invalid tool-call name".to_owned(),
                })?;
            let arguments = match function.get("arguments") {
                Some(Value::String(encoded)) => parse_provider_json_value(encoded)?,
                Some(value) => clone_json_value_iteratively(value),
                None => Value::Object(serde_json::Map::new()),
            };
            mapped.push(LLMToolCall {
                id: format!("ollama-tool-call-{index}"),
                name: name.to_owned(),
                arguments,
            });
        }
        Ok(mapped)
    }

    /// Strip the Magician cache-breakpoint sentinel from a text payload,
    /// reconnecting the halves with a single `\n` when both are non-empty.
    ///
    /// Ollama doesn't cache prompts at all, but if the rendered user prompt
    /// carries our internal marker we must strip it before sending so the
    /// model never sees the sentinel string. Matches the reconnection rule
    /// used by the Anthropic and OpenRouter providers.
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

    fn extract_prompt(messages: &[LLMMessage]) -> String {
        // Ollama's /api/generate accepts a single prompt string. Join messages in a simple format.
        let mut segments = Vec::new();
        for message in messages {
            let role = match message.role {
                MessageRole::System => "[system]",
                MessageRole::User => "[user]",
                MessageRole::Assistant => "[assistant]",
                MessageRole::Tool => "[tool]",
            };
            for block in &message.content {
                if let ContentBlock::Text { text } = block {
                    let cleaned = Self::strip_cache_sentinel(text);
                    segments.push(format!("{} {}", role, cleaned));
                }
            }
        }
        segments.join("\n")
    }

    fn map_usage(data: &Value) -> LLMResult<Option<TokenUsage>> {
        let prompt = bounded_usage_counter(
            "ollama",
            "prompt_eval_count",
            data.get("prompt_eval_count").and_then(Value::as_u64),
        )?;
        let eval = bounded_usage_counter(
            "ollama",
            "eval_count",
            data.get("eval_count").and_then(Value::as_u64),
        )?;
        match (prompt, eval) {
            (Some(prompt), Some(eval)) => {
                let total = checked_usage_sum("ollama", "total_tokens", &[prompt, eval])?;
                Ok(Some(TokenUsage {
                    prompt_tokens: Some(prompt),
                    completion_tokens: Some(eval),
                    total_tokens: Some(total),
                    reasoning_tokens: None,
                    cached_tokens: None,
                    cache_creation_tokens: None,
                }))
            },
            _ => Ok(None),
        }
    }

    fn validate_non_streaming_terminal_payload(data: &Value) -> LLMResult<()> {
        if data.get("done") == Some(&Value::Bool(false)) {
            // This is an incomplete HTTP exchange, not a malformed caller
            // request or a model-level refusal. Surface it as transport so the
            // dispatch queue may retry the idempotent call.
            return Err(LLMError::Transport(
                "Ollama returned a non-terminal payload for a non-streaming request".to_string(),
            ));
        }
        Ok(())
    }

    fn map_metadata(metadata: &RequestMetadata) -> Option<Value> {
        if metadata.timeout_secs.is_some() || metadata.trace_id.is_some() {
            let mut object = serde_json::Map::new();
            if let Some(timeout) = metadata.timeout_secs {
                object.insert("timeout_secs".to_string(), Value::from(timeout));
            }
            if let Some(trace_id) = metadata.trace_id.as_ref() {
                object.insert("trace_id".to_string(), Value::String(trace_id.clone()));
            }
            Some(Value::Object(object))
        } else {
            None
        }
    }

    /// Daemon root shared by per-call endpoints (embed, tags health). Profile
    /// `api_base_url`s for generation arrive already ending in `/api/generate`
    /// or `/api/chat`; embedding profiles may carry a bare daemon root. Stripping
    /// any known endpoint suffix lets every form address the same daemon.
    fn daemon_root_url(&self) -> &str {
        let trimmed = self.base_url.trim_end_matches('/');
        trimmed
            .strip_suffix("/api/generate")
            .or_else(|| trimmed.strip_suffix("/api/chat"))
            .or_else(|| trimmed.strip_suffix("/api/embed"))
            .or_else(|| trimmed.strip_suffix("/api/tags"))
            .unwrap_or(trimmed)
    }

    fn embed_endpoint(&self) -> String {
        format!("{}/api/embed", self.daemon_root_url())
    }
}

#[async_trait]
impl LLMProvider for OllamaProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Ollama
    }

    fn capabilities(&self, _model: &str) -> LLMCapability {
        Self::detect_capability(self.uses_chat_api())
    }

    /// Ollama embeddings (`POST {daemon}/api/embed`). The wire body mirrors the
    /// previous direct-HTTP callers in magician-vector-index field-for-field —
    /// including `truncate` and numeric-sentinel `keep_alive` — so migrating
    /// them onto this seam is behavior-preserving. Unlike `invoke`, provider
    /// keep-alive defaults are NOT injected: the caller owns residency exactly
    /// as it did pre-migration (vector-index pins `-1`).
    async fn embed(&self, request: EmbeddingRequest) -> LLMResult<EmbeddingResponse> {
        if request.model.trim().is_empty() {
            return Err(LLMError::Validation(
                "EmbeddingRequest.model must be set for Ollama provider".to_string(),
            ));
        }
        if request.inputs.is_empty() {
            return Err(LLMError::Validation(
                "Ollama embed requires at least one input".to_string(),
            ));
        }

        let endpoint = self.embed_endpoint();
        let mut body = json!({
            "model": request.model,
            "input": request.inputs,
            "truncate": request.truncate,
        });
        if let Some(keep_alive) = request.keep_alive {
            body["keep_alive"] = keep_alive;
        }
        if let Some(options) = request.options {
            let mut mapped = serde_json::Map::new();
            if let Some(context_tokens) = options.context_tokens {
                mapped.insert("num_ctx".to_string(), json!(context_tokens));
            }
            if let Some(batch_tokens) = options.batch_tokens {
                mapped.insert("num_batch".to_string(), json!(batch_tokens));
            }
            body["options"] = Value::Object(mapped);
        }

        let operation = request.metadata.operation.clone();
        debug!(
            provider = "ollama",
            model = %request.model,
            operation = %operation,
            input_count = request.inputs.len(),
            endpoint = %endpoint,
            "issuing Ollama embed request"
        );

        let timeout = request
            .timeout_ms
            .map(Duration::from_millis)
            .or_else(|| request.metadata.timeout_secs.map(Duration::from_secs))
            .unwrap_or(self.default_timeout);
        let response = self
            .client
            .post(&endpoint)
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
        if !status.is_success() {
            // Status must survive as a typed field: callers classify retry
            // behavior off the number (429/5xx retriable, other 4xx terminal).
            return Err(LLMError::ProviderStatus {
                provider: "ollama".to_owned(),
                status: status.as_u16(),
                body: raw_body,
            });
        }

        let payload = parse_provider_json_value(&raw_body)?;
        let embeddings = payload
            .get("embeddings")
            .cloned()
            .ok_or_else(|| LLMError::Provider {
                provider: "ollama".to_owned(),
                message: "Ollama embed response missing `embeddings` array".to_owned(),
            })?;
        let embeddings: Vec<Vec<f32>> =
            serde_json::from_value(embeddings).map_err(LLMError::from)?;
        Ok(EmbeddingResponse { embeddings })
    }

    /// Liveness probe against the daemon (`GET {daemon}/api/tags`). Replaces
    /// the always-true default only for providers constructed through this
    /// crate's registry; today that is exercised by the embedding seam.
    async fn health_check(&self) -> LLMResult<bool> {
        let endpoint = format!("{}/api/tags", self.daemon_root_url());
        let response = self
            .client
            .get(&endpoint)
            .timeout(self.default_timeout)
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    LLMError::Timeout
                } else {
                    LLMError::Transport(error.to_string())
                }
            })?;
        Ok(response.status().is_success())
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        crate::server_web_search::reject_unsupported("ollama", &request)?;
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for Ollama provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();

        info!(
            provider = "ollama",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            stream = request.stream,
            "issuing Ollama request"
        );

        if request.stream {
            debug!(
                provider = "ollama",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                "rejecting request because streaming is not supported"
            );
            return Err(LLMError::UnsupportedCapability(
                "Ollama streaming is not implemented in magicllm".to_string(),
            ));
        }

        let chat_api = self.uses_chat_api();
        if !chat_api && !request.tools.is_empty() {
            debug!(
                provider = "ollama",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                tool_count = request.tools.len(),
                "rejecting request because tool calling is not supported"
            );
            return Err(LLMError::UnsupportedCapability(
                "Ollama /api/generate does not support tool calling".to_string(),
            ));
        }

        let mut body = if chat_api {
            let messages = Self::map_chat_messages(&request.messages)?;
            if messages.is_empty() {
                return Err(LLMError::Validation(
                    "Ollama chat provider requires at least one admitted message".to_owned(),
                ));
            }
            let mut body = json!({
                "model": model,
                "messages": messages,
                "stream": false,
            });
            if !request.tools.is_empty() {
                body["tools"] = Value::Array(Self::map_chat_tools(&request));
            }
            body
        } else {
            let prompt = if request.messages.is_empty() && request.extra.is_some() {
                request
                    .extra
                    .as_ref()
                    .and_then(|extra| extra.get("prompt"))
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .unwrap_or_default()
            } else {
                Self::extract_prompt(&request.messages)
            };
            if prompt.trim().is_empty() {
                return Err(LLMError::Validation(
                    "Ollama provider requires at least one text message".to_owned(),
                ));
            }
            json!({
                "model": model,
                "prompt": prompt,
                "stream": false,
            })
        };

        if let Some(metadata) = Self::map_metadata(&request.metadata) {
            body["metadata"] = metadata;
        }

        if let Some(Value::Object(map)) = request.extra_value() {
            for (key, value) in map {
                if matches!(
                    key.as_str(),
                    "openai_api_mode"
                        | "gemini_api_mode"
                        | "openai_previous_response_id"
                        | "openai_responses_disable_chaining"
                        | "router_provider_override"
                        | "router_profile_override"
                        | "router_preserve_model"
                ) {
                    continue;
                }
                body.as_object_mut()
                    .unwrap()
                    .entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        Self::apply_response_format(&mut body, request.response_format_value());
        Self::apply_generation_options(&mut body, request.max_output_tokens, request.temperature);
        Self::apply_thinking(
            &mut body,
            Self::reasoning_requested(request.reasoning.as_ref()),
        );

        if let Some(keep_alive) = self.keep_alive.as_ref() {
            body.as_object_mut()
                .unwrap()
                .entry("keep_alive")
                .or_insert_with(|| Value::String(keep_alive.clone()));
        }

        debug!(
            provider = "ollama",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            has_metadata = request.metadata.trace_id.is_some(),
            has_extra = request.extra.is_some(),
            "constructed Ollama request payload"
        );

        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);

        let response = self
            .client
            .post(&self.base_url)
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

        let raw_body = read_bounded_response_text(response).await?;

        let payload = parse_provider_json_value(&raw_body)?;
        drop(raw_body);
        Self::validate_non_streaming_terminal_payload(&payload)?;

        let content = match (if chat_api {
            payload
                .get("message")
                .and_then(|message| message.get("content"))
        } else {
            payload.get("response")
        })
        .and_then(Value::as_str)
        .map(|s| s.to_string())
        {
            Some(value) => value,
            None => {
                error!(
                    provider = "ollama",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    "missing assistant content in Ollama payload"
                );
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message: "missing assistant content in Ollama payload".to_string(),
                });
            },
        };

        let usage = Self::map_usage(&payload)?;
        let tool_calls = if chat_api {
            Self::map_chat_tool_calls(&payload)?
        } else {
            Vec::new()
        };
        let mut response_blocks = Vec::new();
        if !content.is_empty() {
            response_blocks.push(ContentBlock::Text {
                text: content.clone(),
            });
        }
        response_blocks.extend(tool_calls.iter().map(|call| ContentBlock::ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments: clone_json_value_iteratively(&call.arguments),
        }));

        let response = LLMResponse {
            text: Some(Arc::<str>::from(content.clone())),
            reasoning_text: None,
            response_id: None,
            messages: Arc::new(vec![LLMMessage {
                role: MessageRole::Assistant,
                content: response_blocks,
            }]),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason: payload
                .get("done_reason")
                .and_then(Value::as_str)
                .map(str::to_owned),
            raw_response: Some(Arc::new(payload)),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        if let Some(usage) = response.usage.as_ref() {
            debug!(
                provider = "ollama",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by Ollama"
            );
        }

        // Per-call success chatter: carries nothing the router's
        // `llm_call_completed` INFO line does not already report (and failures
        // still log at ERROR), so it rides at DEBUG to keep INFO readable.
        debug!(
            provider = "ollama",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "Ollama call succeeded"
        );

        Ok(response)
    }
}

impl OllamaProvider {
    fn apply_response_format(body: &mut Value, response_format: Option<&LLMResponseFormat>) {
        let Some(response_format) = response_format else {
            return;
        };
        let Some(body_object) = body.as_object_mut() else {
            return;
        };
        match response_format {
            LLMResponseFormat::Text => {},
            LLMResponseFormat::JsonObject => {
                body_object
                    .entry("format")
                    .or_insert_with(|| Value::String("json".to_string()));
            },
            LLMResponseFormat::JsonSchema { schema } => {
                // A typed per-request schema is more specific than a profile's
                // generic `format: json` metadata. Always install it so profile
                // defaults cannot silently discard the adapter contract.
                body_object.insert("format".to_string(), clone_json_value_iteratively(schema));
            },
        }
    }

    /// Send an explicit `think` flag so Ollama reasoning models (Qwen3.x, the
    /// gemma-4 `*-it` / `*-a4b` families, etc.) don't run in their default
    /// thinking-ON mode for structured extraction ops.
    ///
    /// With thinking on + a JSON-format request, Ollama either routes the answer
    /// into a separate `thinking` field (leaving `response` empty) or the model
    /// degenerates into repetition until it hits `num_predict` — both surface as
    /// invalid/empty JSON. This is the root cause of the channel distill/classify
    /// failures observed with reasoning models. Mirroring the eval harness, we
    /// send `think: false` for non-reasoning requests and `think: true` only when
    /// the request's typed `ReasoningConfig` explicitly enables reasoning.
    /// Non-reasoning models ignore the flag, so it is safe to always send.
    ///
    /// Typed `ReasoningConfig` is the sole authority. Provider-specific raw
    /// extras cannot turn thinking back on after router capability admission.
    fn reasoning_requested(reasoning: Option<&ReasoningConfig>) -> bool {
        reasoning
            .map(|reasoning| !reasoning.is_disabled())
            .unwrap_or(false)
    }

    fn apply_thinking(body: &mut Value, think: bool) {
        let Some(body_object) = body.as_object_mut() else {
            return;
        };
        body_object.insert("think".to_string(), Value::Bool(think));
    }

    fn apply_generation_options(
        body: &mut Value,
        max_output_tokens: Option<u32>,
        temperature: Option<f32>,
    ) {
        if max_output_tokens.is_none() && temperature.is_none() {
            return;
        }

        let Some(body_object) = body.as_object_mut() else {
            return;
        };
        let options = body_object
            .entry("options")
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        let Value::Object(options) = options else {
            debug!("Ollama request options were not an object; generation controls not applied");
            return;
        };

        if let Some(max_output_tokens) = max_output_tokens {
            options
                .entry("num_predict")
                .or_insert_with(|| Value::from(max_output_tokens));
        }
        if let Some(temperature) = temperature {
            options
                .entry("temperature")
                .or_insert_with(|| Value::from(temperature));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LLMToolSpec;
    use serde_json::json;

    #[test]
    fn think_false_for_non_reasoning_request() {
        let mut body = json!({ "model": "m", "prompt": "p", "stream": false });
        OllamaProvider::apply_thinking(&mut body, false);
        assert_eq!(body["think"], json!(false));
    }

    #[test]
    fn think_true_when_reasoning_requested() {
        let mut body = json!({ "model": "m", "prompt": "p", "stream": false });
        OllamaProvider::apply_thinking(&mut body, true);
        assert_eq!(body["think"], json!(true));
    }

    #[test]
    fn typed_reasoning_is_the_only_thinking_authority() {
        // Provider-specific raw extras are merged before apply_thinking, but
        // cannot bypass the router's typed capability decision.
        let mut body = json!({ "model": "m", "think": true });
        OllamaProvider::apply_thinking(&mut body, false);
        assert_eq!(body["think"], json!(false));

        let mut body = json!({ "model": "m", "think": false });
        OllamaProvider::apply_thinking(&mut body, true);
        assert_eq!(body["think"], json!(true));
    }

    #[test]
    fn disabled_reasoning_config_never_enables_ollama_thinking() {
        for effort in ["none", "off", "no", "disabled", "disable", "false"] {
            let reasoning = ReasoningConfig {
                effort: Some(effort.to_string()),
                ..ReasoningConfig::default()
            };
            assert!(
                !OllamaProvider::reasoning_requested(Some(&reasoning)),
                "disabled effort `{effort}` must stay non-reasoning"
            );
        }

        let enabled = ReasoningConfig {
            effort: Some("medium".to_string()),
            ..ReasoningConfig::default()
        };
        assert!(OllamaProvider::reasoning_requested(Some(&enabled)));
        assert!(!OllamaProvider::reasoning_requested(None));
    }

    #[test]
    fn json_object_maps_to_ollama_format_json() {
        let mut body = json!({ "model": "m" });
        OllamaProvider::apply_response_format(&mut body, Some(&LLMResponseFormat::JsonObject));
        assert_eq!(body["format"], json!("json"));
    }

    #[test]
    fn json_schema_overrides_generic_profile_json_format() {
        let schema = json!({
            "type": "object",
            "properties": {"ok": {"type": "boolean"}},
            "required": ["ok"]
        });
        let mut body = json!({ "model": "m", "format": "json" });
        OllamaProvider::apply_response_format(
            &mut body,
            Some(&LLMResponseFormat::JsonSchema {
                schema: schema.clone(),
            }),
        );
        assert_eq!(body["format"], schema);
    }

    #[test]
    fn structured_op_request_shape_is_json_and_thinkless() {
        // Parity with the offline channel-model benchmark: a non-reasoning JSON
        // extraction op (classify/distill) must send format:json + think:false
        // + options{num_predict, temperature}, so a reasoning model cannot divert
        // its answer into a `thinking` field or degenerate into repetition.
        let mut body =
            json!({ "model": "gemma4:26b-a4b-it-q4_K_M", "prompt": "x", "stream": false });
        OllamaProvider::apply_response_format(&mut body, Some(&LLMResponseFormat::JsonObject));
        OllamaProvider::apply_generation_options(&mut body, Some(2048), Some(0.1));
        OllamaProvider::apply_thinking(&mut body, false);

        assert_eq!(body["format"], json!("json"));
        assert_eq!(body["think"], json!(false));
        assert_eq!(body["stream"], json!(false));
        assert_eq!(body["options"]["num_predict"], json!(2048));
        assert!(body["options"]["temperature"].is_number());
    }

    #[test]
    fn non_streaming_provider_rejects_explicitly_partial_payloads() {
        let partial = json!({
            "response": "{\"label\":\"needs_reply\",\"follow_up_kind\":",
            "done": false
        });
        let error = OllamaProvider::validate_non_streaming_terminal_payload(&partial)
            .expect_err("partial payload must not reach JSON consumers as success");
        assert!(error.to_string().contains("non-terminal payload"));

        OllamaProvider::validate_non_streaming_terminal_payload(&json!({
            "response": "{\"label\":\"fyi\"}",
            "done": true
        }))
        .expect("terminal payload is accepted");
        OllamaProvider::validate_non_streaming_terminal_payload(&json!({
            "response": "legacy payload without done"
        }))
        .expect("older compatible payloads remain accepted");
    }

    #[test]
    fn native_chat_contract_advertises_and_maps_tools() {
        let provider = OllamaProvider::with_base_url("http://127.0.0.1:11434/api/chat");
        assert!(provider.capabilities("gemma4:12b").tool_calling);
        let request = LLMRequest {
            model: "gemma4:12b".to_owned(),
            messages: Arc::new(vec![LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::Text {
                    text: "invoke the terminal tool".to_owned(),
                }],
            }]),
            tools: Arc::new(vec![LLMToolSpec {
                name: "app_commit_mutations".to_owned(),
                description: "commit".to_owned(),
                parameters: json!({"type": "object"}),
            }]),
            ..LLMRequest::default()
        };
        let messages =
            OllamaProvider::map_chat_messages(&request.messages).expect("bounded messages map");
        let tools = OllamaProvider::map_chat_tools(&request);
        assert_eq!(messages[0]["role"], json!("user"));
        assert_eq!(tools[0]["type"], json!("function"));
        assert_eq!(tools[0]["function"]["name"], json!("app_commit_mutations"));
    }

    #[test]
    fn native_chat_response_maps_exact_tool_call_and_usage() {
        let payload = json!({
            "message": {
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "function": {
                        "name": "app_commit_mutations",
                        "arguments": {"output": {"ok": true}}
                    }
                }]
            },
            "done": true,
            "prompt_eval_count": 41,
            "eval_count": 7
        });
        let calls = OllamaProvider::map_chat_tool_calls(&payload).expect("tool call maps");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "app_commit_mutations");
        assert_eq!(calls[0].arguments["output"]["ok"], json!(true));
        let usage = OllamaProvider::map_usage(&payload)
            .expect("bounded usage maps")
            .expect("authoritative usage maps");
        assert_eq!(usage.prompt_tokens, Some(41));
        assert_eq!(usage.completion_tokens, Some(7));
    }

    #[test]
    fn usage_mapping_rejects_provider_counter_larger_than_u32() {
        let payload = json!({
            "prompt_eval_count": u64::from(u32::MAX) + 1,
            "eval_count": 0,
        });

        assert!(matches!(
            OllamaProvider::map_usage(&payload),
            Err(LLMError::Provider { .. })
        ));
    }

    #[test]
    fn usage_mapping_rejects_total_sum_overflow() {
        let payload = json!({
            "prompt_eval_count": u32::MAX,
            "eval_count": 1,
        });

        assert!(matches!(
            OllamaProvider::map_usage(&payload),
            Err(LLMError::Provider { .. })
        ));
    }
}

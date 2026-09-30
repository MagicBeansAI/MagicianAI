//! Sarvam AI over its Chat Completions API (`api.sarvam.ai`).
//!
//! Sarvam's models are Indic-first: they read and answer in the scheduled
//! Indian languages (native script and romanized) as well as English, which
//! is why demos for Indian customers route here.
//!
//! The endpoint borrows OpenAI's Chat Completions shape, but this is its own
//! transport rather than a mode of `OpenAIChatProvider`, so Sarvam's quirks
//! never sit on the OpenAI path. Measured 2026-09-30 against `sarvam-105b`:
//!
//! - Auth is the `api-subscription-key` header.
//! - Message `content` must be a string. Content-part arrays — and so images
//!   — are refused with a 400; the models are text-only.
//! - Every request reasons (`reasoning_content`). `reasoning_effort` takes
//!   only `low|medium|high`; there is no off switch. With no effort set, a
//!   one-word prompt spent a 3000-token budget thinking and answered nothing,
//!   so a request always carries an effort (default `low`) and a budget.
//! - `max_tokens` counts reasoning, and Sarvam's own default is 2048 — too
//!   small even at `low` — so an unset budget becomes
//!   [`SARVAM_DEFAULT_MAX_TOKENS`].
//! - Tools, `tool_choice`, and `response_format: json_schema` behave as on
//!   OpenAI. A tool-result turn does not need prior `reasoning_content`
//!   echoed back.
//! - Streaming sends `delta.reasoning_content` before `delta.content`, then a
//!   usage-only chunk with a top-level `reasoning_tokens`. The answer opens
//!   with blank lines left over from the thinking block; they are dropped.
//! - No prompt-cache controls and no `store`.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tracing::{info, warn};

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
        ReasoningConfig, StreamDelta, TokenUsage,
    },
};

pub const DEFAULT_BASE_URL_SARVAM_CHAT: &str = "https://api.sarvam.ai/v1/chat/completions";

/// Output budget (reasoning included) when a request sets none.
pub const SARVAM_DEFAULT_MAX_TOKENS: u32 = 8192;

const PROVIDER: &str = "sarvam";

/// Documented Sarvam request fields a profile's `metadata` may pass through.
/// Everything else in `extra` is Magician routing state; Sarvam 400s on
/// unknown fields, so extras are allow-listed rather than deny-listed.
const PASSTHROUGH_EXTRAS: &[&str] = &[
    "stop",
    "seed",
    "frequency_penalty",
    "presence_penalty",
    "wiki_grounding",
];

/// Provider for Sarvam AI's chat models.
pub struct SarvamProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl SarvamProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_base_url(api_key, DEFAULT_BASE_URL_SARVAM_CHAT)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            api_key: api_key.into(),
            base_url: base_url.into(),
            default_timeout: Duration::from_secs(180),
        }
    }

    /// No Sarvam chat model takes image input. Public so config validation
    /// gates `supports_vision` on the same predicate the runtime uses.
    pub fn model_supports_vision(_model: &str) -> bool {
        false
    }

    pub fn capabilities_for_model(_model: &str) -> LLMCapability {
        LLMCapability {
            modalities: vec![LLMModality::Text],
            reasoning: LLMReasoning::Standard,
            tool_calling: true,
            json_mode: true,
            streaming: true,
            computer_use: false,
            web_search: false,
        }
    }

    /// Refuse what Sarvam cannot serve before any I/O, with a reason that
    /// names the provider.
    fn admit(request: &LLMRequest) -> LLMResult<()> {
        crate::server_web_search::reject_unsupported(PROVIDER, request)?;
        if request.model.trim().is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for the sarvam provider".to_string(),
            ));
        }
        let has_image = request.messages.iter().any(|message| {
            message.content.iter().any(|block| {
                matches!(block, ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. })
            })
        });
        if request.modality != LLMModality::Text
            || has_image
            || request.media.is_some()
            || request.input_media.is_some()
        {
            return Err(LLMError::UnsupportedCapability(format!(
                "sarvam: `{}` is text-only; route image or media turns to a vision profile",
                request.model
            )));
        }
        Ok(())
    }

    /// `low|medium|high` is the whole vocabulary and reasoning cannot be
    /// switched off, so "disabled" and the levels other providers add fold
    /// onto the nearest one served.
    fn reasoning_effort(reasoning: Option<&ReasoningConfig>) -> &'static str {
        let effort = reasoning
            .filter(|reasoning| !reasoning.is_disabled())
            .and_then(|reasoning| reasoning.effort.as_deref())
            .map(|effort| effort.trim().to_ascii_lowercase());
        match effort.as_deref() {
            Some("medium") => "medium",
            Some("high" | "xhigh" | "max") => "high",
            _ => "low",
        }
    }

    fn text_without_cache_sentinel(text: &str) -> String {
        let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
        match suffix {
            Some(suffix) if prefix.is_empty() => suffix,
            Some(suffix) if suffix.is_empty() => prefix,
            Some(suffix) => format!("{prefix}\n{suffix}"),
            None => prefix,
        }
    }

    fn map_messages(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        let mut payload = Vec::with_capacity(messages.len());
        for message in messages {
            let mut text_parts = Vec::new();
            let mut tool_calls = Vec::new();
            let mut tool_results = Vec::new();
            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => text_parts.push(text.clone()),
                    ContentBlock::Json { value } => text_parts.push(value.to_string()),
                    ContentBlock::ToolCall { id, name, arguments } => {
                        let arguments = match arguments {
                            Value::String(raw) => raw.clone(),
                            other => other.to_string(),
                        };
                        tool_calls.push(json!({
                            "id": id,
                            "type": "function",
                            "function": { "name": name, "arguments": arguments },
                        }));
                    },
                    ContentBlock::ToolResult { tool_call_id, content } => {
                        let content = match content {
                            Value::String(raw) => raw.clone(),
                            other => other.to_string(),
                        };
                        tool_results.push((tool_call_id.clone(), content));
                    },
                    ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. } => {
                        return Err(LLMError::UnsupportedCapability(
                            "sarvam: message content must be text".to_string(),
                        ));
                    },
                }
            }

            let text = Self::text_without_cache_sentinel(&text_parts.join("\n"));
            if !tool_calls.is_empty() {
                payload.push(json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    "tool_calls": tool_calls,
                }));
            } else if !tool_results.is_empty() {
                if !text.is_empty() {
                    return Err(LLMError::UnsupportedCapability(
                        "sarvam: tool-result messages cannot mix unpaired text with tool results"
                            .to_string(),
                    ));
                }
                // One `tool` message per result, in order, so a multi-result
                // turn never collapses onto a single call id.
                for (tool_call_id, content) in tool_results {
                    payload.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "content": Self::text_without_cache_sentinel(&content),
                    }));
                }
            } else {
                let role = match message.role {
                    MessageRole::System => "system",
                    MessageRole::User => "user",
                    MessageRole::Assistant => "assistant",
                    MessageRole::Tool => "tool",
                };
                payload.push(json!({ "role": role, "content": text }));
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

    /// `auto` unless the caller asks otherwise, as on the Anthropic path: a
    /// chat turn must be free to answer without calling a tool.
    fn tool_choice(request: &LLMRequest) -> Value {
        let requested = request.extra_value().and_then(|extra| extra.get("tool_choice"));
        match requested {
            Some(Value::String(choice)) => Value::String(choice.clone()),
            Some(choice) => match choice.get("type").and_then(Value::as_str) {
                Some("any" | "required") => Value::String("required".to_string()),
                Some("none") => Value::String("none".to_string()),
                Some("tool") => match choice.get("name").and_then(Value::as_str) {
                    Some(name) => json!({ "type": "function", "function": { "name": name } }),
                    None => Value::String("required".to_string()),
                },
                Some("function") => clone_json_value_iteratively(choice),
                _ => Value::String("auto".to_string()),
            },
            None => Value::String("auto".to_string()),
        }
    }

    fn build_request_body(request: &LLMRequest) -> LLMResult<Map<String, Value>> {
        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert(
            "messages".to_string(),
            Value::Array(Self::map_messages(&request.messages)?),
        );

        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools)),
            );
            body.insert("tool_choice".to_string(), Self::tool_choice(request));
        }

        match request.response_format_value() {
            Some(LLMResponseFormat::JsonObject) => {
                body.insert(
                    "response_format".to_string(),
                    json!({ "type": "json_object" }),
                );
            },
            Some(LLMResponseFormat::JsonSchema { schema }) => {
                body.insert(
                    "response_format".to_string(),
                    json!({
                        "type": "json_schema",
                        "json_schema": {
                            "name": "response",
                            "schema": clone_json_value_iteratively(schema),
                        }
                    }),
                );
            },
            Some(LLMResponseFormat::Text) | None => {},
        }

        if let Some(temperature) = request.temperature {
            body.insert("temperature".to_string(), Value::from(temperature));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
        body.insert(
            "max_tokens".to_string(),
            Value::from(
                request
                    .max_output_tokens
                    .unwrap_or(SARVAM_DEFAULT_MAX_TOKENS),
            ),
        );
        body.insert(
            "reasoning_effort".to_string(),
            Value::String(Self::reasoning_effort(request.reasoning.as_ref()).to_string()),
        );

        if let Some(Value::Object(extra)) = request.extra_value() {
            for key in PASSTHROUGH_EXTRAS {
                if let Some(value) = extra.get(*key) {
                    body.entry(key.to_string())
                        .or_insert_with(|| clone_json_value_iteratively(value));
                }
            }
        }

        Ok(body)
    }

    fn map_usage(usage: &Value) -> LLMResult<TokenUsage> {
        let counter = |field: &'static str, value: Option<u64>| {
            super::bounded_usage_counter(PROVIDER, field, value)
        };
        Ok(TokenUsage {
            prompt_tokens: counter("prompt_tokens", usage.get("prompt_tokens").and_then(Value::as_u64))?,
            completion_tokens: counter(
                "completion_tokens",
                usage.get("completion_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: counter("total_tokens", usage.get("total_tokens").and_then(Value::as_u64))?,
            // Streams report it top-level; the documented non-stream shape
            // nests it under `completion_tokens_details`.
            reasoning_tokens: counter(
                "reasoning_tokens",
                usage
                    .get("reasoning_tokens")
                    .or_else(|| usage.pointer("/completion_tokens_details/reasoning_tokens"))
                    .and_then(Value::as_u64),
            )?,
            cached_tokens: counter(
                "cached_tokens",
                usage
                    .pointer("/prompt_tokens_details/cached_tokens")
                    .and_then(Value::as_u64),
            )?,
            cache_creation_tokens: None,
        })
    }

    fn map_tool_calls(message: &Value) -> LLMResult<Vec<LLMToolCall>> {
        let Some(tool_calls) = message.get("tool_calls").and_then(Value::as_array) else {
            return Ok(Vec::new());
        };
        let mut calls = Vec::with_capacity(tool_calls.len());
        for entry in tool_calls {
            let Some(function) = entry.get("function") else {
                continue;
            };
            calls.push(LLMToolCall {
                id: entry.get("id").and_then(Value::as_str).unwrap_or_default().to_string(),
                name: function.get("name").and_then(Value::as_str).unwrap_or_default().to_string(),
                arguments: function
                    .get("arguments")
                    .and_then(Value::as_str)
                    .map(parse_tool_argument_json_or_string)
                    .transpose()?
                    .unwrap_or(Value::Null),
            });
        }
        Ok(calls)
    }

    fn response(
        text: Option<String>,
        reasoning_text: Option<String>,
        tool_calls: Vec<LLMToolCall>,
        usage: Option<TokenUsage>,
        finish_reason: Option<String>,
        raw_response: Option<Value>,
    ) -> LLMResponse {
        let messages = text
            .iter()
            .map(|text| LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::Text { text: text.clone() }],
            })
            .collect();
        LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: reasoning_text.map(Arc::<str>::from),
            response_id: None,
            messages: Arc::new(messages),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage,
            finish_reason,
            raw_response: raw_response.map(Arc::new),
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        }
    }

    fn map_response(payload: Value) -> LLMResult<LLMResponse> {
        let choice = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|choices| choices.first())
            .ok_or_else(|| LLMError::Provider {
                provider: PROVIDER.to_string(),
                message: "response carried no choices".to_string(),
            })?;
        let message = choice.get("message").cloned().unwrap_or(Value::Null);
        let text_field = |field: &str| {
            message
                .get(field)
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
                .map(str::to_string)
        };
        let text = text_field("content")
            .map(|text| text.trim_start().to_string())
            .filter(|text| !text.is_empty());
        let reasoning_text = text_field("reasoning_content");
        let tool_calls = Self::map_tool_calls(&message)?;
        let finish_reason = choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string);
        let usage = payload.get("usage").map(Self::map_usage).transpose()?;
        Ok(Self::response(
            text,
            reasoning_text,
            tool_calls,
            usage,
            finish_reason,
            Some(payload),
        ))
    }

    /// A reply cut off while still thinking is the one failure a Sarvam
    /// profile invites by construction; say so where operators will look.
    fn warn_if_budget_spent_thinking(request: &LLMRequest, response: &LLMResponse) {
        if response.finish_reason.as_deref() == Some("length")
            && response.text.is_none()
            && response.tool_calls.is_empty()
        {
            warn!(
                provider = PROVIDER,
                model = %request.model,
                operation = %request.metadata.operation,
                max_tokens = request.max_output_tokens.unwrap_or(SARVAM_DEFAULT_MAX_TOKENS),
                "Sarvam spent the whole output budget reasoning; raise max_output_tokens or \
                 lower reasoning effort"
            );
        }
    }

    async fn send(&self, request: &LLMRequest, body: &Map<String, Value>) -> LLMResult<reqwest::Response> {
        let timeout = request
            .metadata
            .timeout_secs
            .map(Duration::from_secs)
            .unwrap_or(self.default_timeout);
        let response = self
            .client
            .post(&self.base_url)
            .header("api-subscription-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
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
        if !(status.is_client_error() || status.is_server_error()) {
            return Ok(response);
        }
        // A protected request's refusal text may echo its content; it never
        // leaves the adapter.
        if request.metadata.single_physical_attempt {
            return Err(LLMError::Provider {
                provider: PROVIDER.to_string(),
                message: "protected provider request failed".to_string(),
            });
        }
        let raw = read_bounded_response_text(response).await?;
        let payload = parse_provider_json_or_string(&raw)?;
        let message = payload
            .pointer("/error/message")
            .and_then(Value::as_str)
            .map(|message| format!("HTTP {status}: {message}"))
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(LLMError::Provider {
            provider: PROVIDER.to_string(),
            message,
        })
    }
}

/// One streamed tool call, reassembled across deltas.
#[derive(Default)]
struct ToolCallDelta {
    id: Option<String>,
    name: Option<String>,
    arguments: String,
}

#[async_trait]
impl LLMProvider for SarvamProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Sarvam
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        Self::admit(&request)?;
        let body = Self::build_request_body(&request)?;
        info!(
            provider = PROVIDER,
            model = %request.model,
            operation = %request.metadata.operation,
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing Sarvam chat completion"
        );
        let response = self.send(&request, &body).await?;
        let raw = read_bounded_response_text(response).await?;
        let response = Self::map_response(parse_provider_json_value(&raw)?)?;
        Self::warn_if_budget_spent_thinking(&request, &response);
        Ok(response)
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        Self::admit(&request)?;
        let mut body = Self::build_request_body(&request)?;
        body.insert("stream".to_string(), Value::Bool(true));
        info!(
            provider = PROVIDER,
            model = %request.model,
            operation = %request.metadata.operation,
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing Sarvam streaming chat completion"
        );
        let response = self.send(&request, &body).await?;

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        let mut utf8_carry = Vec::with_capacity(4);
        let mut admission = SseBodyAdmission::new();
        let mut text = String::new();
        let mut reasoning = String::new();
        let mut reasoning_open = false;
        let mut tool_calls: BTreeMap<u64, ToolCallDelta> = BTreeMap::new();
        let mut finish_reason = None;
        let mut usage = None;
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
            admission.admit_chunk(&chunk)?;
            append_sse_utf8_chunk(&mut buffer, &mut utf8_carry, &chunk)?;

            let mut consumed = 0;
            while let Some(data) = next_sse_data(&buffer, &mut consumed) {
                if data == "[DONE]" {
                    done = true;
                    break;
                }
                let event = parse_provider_json_value(data.as_ref())?;
                drop(data);

                if let Some(event_usage) = event.get("usage").filter(|usage| !usage.is_null()) {
                    usage = Some(Self::map_usage(event_usage)?);
                }
                let Some(choice) = event
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|choices| choices.first())
                else {
                    continue;
                };
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = Some(reason.to_string());
                }
                let Some(delta) = choice.get("delta") else {
                    continue;
                };

                if let Some(fragment) = delta
                    .get("reasoning_content")
                    .and_then(Value::as_str)
                    .filter(|fragment| !fragment.is_empty())
                {
                    if !reasoning_open {
                        reasoning_open = true;
                        let _ = tx
                            .send(StreamDelta::ReasoningStart {
                                index: 0,
                                signature: None,
                            })
                            .await;
                    }
                    reasoning.push_str(fragment);
                    let _ = tx
                        .send(StreamDelta::ReasoningDelta {
                            index: 0,
                            delta: fragment.to_string(),
                        })
                        .await;
                }

                let content = delta
                    .get("content")
                    .and_then(Value::as_str)
                    .map(|content| {
                        if text.is_empty() {
                            content.trim_start()
                        } else {
                            content
                        }
                    })
                    .filter(|content| !content.is_empty());
                let tool_deltas = delta.get("tool_calls").and_then(Value::as_array);
                // The thinking block closes when the answer begins.
                if reasoning_open && (content.is_some() || tool_deltas.is_some()) {
                    reasoning_open = false;
                    let _ = tx
                        .send(StreamDelta::ReasoningEnd {
                            index: 0,
                            total_chars: reasoning.chars().count(),
                        })
                        .await;
                }

                if let Some(content) = content {
                    text.push_str(content);
                    let _ = tx.send(StreamDelta::Token(content.to_string())).await;
                }

                for call in tool_deltas.into_iter().flatten() {
                    let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
                    let entry = tool_calls.entry(index).or_default();
                    if let Some(id) = call.get("id").and_then(Value::as_str) {
                        entry.id = Some(id.to_string());
                    }
                    let Some(function) = call.get("function") else {
                        continue;
                    };
                    if let Some(name) = function.get("name").and_then(Value::as_str) {
                        entry.name = Some(name.to_string());
                    }
                    if let Some(arguments) = function.get("arguments").and_then(Value::as_str) {
                        append_tool_argument_fragment(&mut entry.arguments, arguments)?;
                        let _ = tx
                            .send(StreamDelta::ToolCallDelta {
                                id: entry.id.clone().unwrap_or_default(),
                                name: entry.name.clone(),
                                arguments_chunk: arguments.to_string(),
                            })
                            .await;
                    }
                }
            }
            if consumed > 0 {
                buffer.drain(..consumed);
            }
        }
        finish_sse_utf8(&utf8_carry)?;

        // A budget-truncated stream ends mid-thought with the block open.
        if reasoning_open {
            let _ = tx
                .send(StreamDelta::ReasoningEnd {
                    index: 0,
                    total_chars: reasoning.chars().count(),
                })
                .await;
        }

        let mut calls = Vec::with_capacity(tool_calls.len());
        for (_, call) in tool_calls {
            calls.push(LLMToolCall {
                id: call.id.unwrap_or_default(),
                name: call.name.unwrap_or_default(),
                arguments: parse_tool_argument_json_or_string(&call.arguments)?,
            });
        }
        let response = Self::response(
            (!text.is_empty()).then_some(text),
            (!reasoning.is_empty()).then_some(reasoning),
            calls,
            usage,
            finish_reason,
            None,
        );
        Self::warn_if_budget_spent_thinking(&request, &response);
        let _ = tx.send(StreamDelta::Done(response)).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(model: &str) -> LLMRequest {
        LLMRequest {
            model: model.to_string(),
            messages: vec![LLMMessage::user("नमस्ते")].into(),
            ..Default::default()
        }
    }

    #[test]
    fn sarvam_models_are_text_only_reasoning_tool_callers() {
        for model in ["sarvam-105b", "sarvam-105b-conversations", "sarvam-30b"] {
            let caps = SarvamProvider::capabilities_for_model(model);
            assert_eq!(caps.modalities, vec![LLMModality::Text], "{model}");
            assert_eq!(caps.reasoning, LLMReasoning::Standard);
            assert!(caps.tool_calling && caps.json_mode && caps.streaming);
            assert!(!caps.web_search);
            assert!(!SarvamProvider::model_supports_vision(model));
        }
    }

    #[test]
    fn body_always_carries_an_effort_and_a_budget_and_no_openai_only_fields() {
        let body = SarvamProvider::build_request_body(&request("sarvam-105b")).unwrap();
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["max_tokens"], SARVAM_DEFAULT_MAX_TOKENS);
        for absent in ["max_completion_tokens", "store", "prompt_cache_key", "tools"] {
            assert!(body.get(absent).is_none(), "{absent}");
        }
        assert_eq!(body["messages"][0], json!({"role": "user", "content": "नमस्ते"}));
    }

    #[test]
    fn effort_folds_onto_low_medium_high() {
        let effort = |value: &str| {
            SarvamProvider::reasoning_effort(Some(&ReasoningConfig {
                effort: Some(value.to_string()),
                ..Default::default()
            }))
        };
        assert_eq!(effort("none"), "low");
        assert_eq!(effort("disabled"), "low");
        assert_eq!(effort("minimal"), "low");
        assert_eq!(effort("Medium"), "medium");
        assert_eq!(effort("high"), "high");
        assert_eq!(effort("xhigh"), "high");
        assert_eq!(SarvamProvider::reasoning_effort(None), "low");
    }

    #[test]
    fn explicit_budget_sampling_and_allow_listed_extras_pass_through() {
        let mut request = request("sarvam-105b");
        request.max_output_tokens = Some(1200);
        request.temperature = Some(0.4);
        request.set_extra(json!({
            "seed": 7,
            "wiki_grounding": true,
            "openai_api_mode": "chat",
            "router_profile_override": "x",
        }));
        let body = SarvamProvider::build_request_body(&request).unwrap();
        assert_eq!(body["max_tokens"], 1200);
        assert!((body["temperature"].as_f64().unwrap() - 0.4).abs() < 1e-6);
        assert_eq!(body["seed"], 7);
        assert_eq!(body["wiki_grounding"], true);
        assert!(body.get("openai_api_mode").is_none());
        assert!(body.get("router_profile_override").is_none());
    }

    #[test]
    fn tools_default_to_auto_and_honour_caller_choice() {
        let mut request = request("sarvam-105b");
        request.tools = vec![LLMToolSpec {
            name: "get_weather".to_string(),
            description: "weather".to_string(),
            parameters: json!({"type": "object"}),
        }]
        .into();
        let body = SarvamProvider::build_request_body(&request).unwrap();
        assert_eq!(body["tool_choice"], "auto");
        assert_eq!(body["tools"][0]["function"]["name"], "get_weather");

        request.set_extra(json!({"tool_choice": {"type": "any"}}));
        let body = SarvamProvider::build_request_body(&request).unwrap();
        assert_eq!(body["tool_choice"], "required");
    }

    #[test]
    fn tool_round_trip_maps_to_assistant_tool_calls_and_tool_messages() {
        let messages = vec![
            LLMMessage {
                role: MessageRole::Assistant,
                content: vec![ContentBlock::ToolCall {
                    id: "call_1".to_string(),
                    name: "get_weather".to_string(),
                    arguments: json!({"city": "Bengaluru"}),
                }],
            },
            LLMMessage {
                role: MessageRole::Tool,
                content: vec![ContentBlock::ToolResult {
                    tool_call_id: "call_1".to_string(),
                    content: json!({"temp_c": 24}),
                }],
            },
        ];
        let mapped = SarvamProvider::map_messages(&messages).unwrap();
        assert_eq!(mapped[0]["content"], Value::Null);
        assert_eq!(
            mapped[0]["tool_calls"][0]["function"]["arguments"],
            "{\"city\":\"Bengaluru\"}"
        );
        assert_eq!(
            mapped[1],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "{\"temp_c\":24}"})
        );
    }

    #[test]
    fn cache_sentinel_never_reaches_the_wire() {
        let text = format!("prefix{}suffix", crate::types::CACHE_BREAKPOINT_SENTINEL);
        let mapped = SarvamProvider::map_messages(&[LLMMessage::user(&text)]).unwrap();
        assert_eq!(mapped[0]["content"], "prefix\nsuffix");
    }

    #[test]
    fn response_maps_answer_reasoning_tool_calls_and_usage() {
        // Shape captured from api.sarvam.ai on 2026-09-30.
        let payload = json!({
            "id": "20260929_x",
            "choices": [{
                "finish_reason": "tool_calls",
                "index": 0,
                "message": {
                    "content": null,
                    "reasoning_content": "The user wants Bengaluru weather.",
                    "tool_calls": [{
                        "id": "call_2bb",
                        "type": "function",
                        "function": {"name": "get_weather", "arguments": "{\"city\": \"Bengaluru\"}"}
                    }]
                }
            }],
            "usage": {
                "prompt_tokens": 18, "completion_tokens": 24, "total_tokens": 42,
                "completion_tokens_details": {"reasoning_tokens": 10},
                "prompt_tokens_details": {"cached_tokens": 4}
            }
        });
        let response = SarvamProvider::map_response(payload).unwrap();
        assert!(response.text.is_none());
        let answer = SarvamProvider::map_response(json!({
            "choices": [{"message": {"content": "\n\n\nவருக!"}, "finish_reason": "stop"}]
        }))
        .unwrap();
        assert_eq!(answer.text.as_deref(), Some("வருக!"));
        assert_eq!(
            response.reasoning_text.as_deref(),
            Some("The user wants Bengaluru weather.")
        );
        assert_eq!(response.tool_calls[0].name, "get_weather");
        assert_eq!(response.tool_calls[0].arguments, json!({"city": "Bengaluru"}));
        let usage = response.usage.unwrap();
        assert_eq!(usage.reasoning_tokens, Some(10));
        assert_eq!(usage.cached_tokens, Some(4));
        assert_eq!(response.finish_reason.as_deref(), Some("tool_calls"));
    }

    #[tokio::test]
    async fn an_image_turn_is_refused_before_any_io() {
        let provider = SarvamProvider::with_base_url("k", "http://127.0.0.1:9/unreachable");
        let mut request = request("sarvam-105b");
        request.messages = vec![LLMMessage {
            role: MessageRole::User,
            content: vec![ContentBlock::ImageUrl {
                url: "https://example.com/a.png".to_string(),
                prompt: None,
            }],
        }]
        .into();
        let error = provider.invoke(request).await.expect_err("image refused");
        assert!(matches!(error, LLMError::UnsupportedCapability(_)), "{error}");
        assert!(error.to_string().contains("sarvam"), "{error}");
        assert_eq!(provider.provider_kind(), LLMProviderKind::Sarvam);
    }
}

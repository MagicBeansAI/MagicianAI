use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use futures_util::StreamExt;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tokio::sync::mpsc;
use tracing::{debug, error, info};

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
        StreamDelta, TokenUsage,
    },
};

/// MiniMax's own hosted API, OpenAI-compatible Chat Completions surface.
///
/// We deliberately target this endpoint (NOT the Anthropic-compatible
/// `/anthropic/v1/messages`, and NOT the native `/v1/text/chatcompletion_v2`):
/// - The Anthropic-compat endpoint rejects our multimodal/array `tool_result`
///   content blocks (`400 invalid tool_result content (2013)`), breaking any
///   tool-using loop.
/// - The native `chatcompletion_v2` endpoint hard-blocks every image input with
///   a `new_sensitive` content filter (`1026`), so it cannot do M3 vision.
/// - This OpenAI-compatible surface was verified live to handle image input,
///   tool calls, the tool-result round-trip (`role:tool` + `tool_call_id` +
///   string content), and reasoning — all on `MiniMax-M3`.
const DEFAULT_BASE_URL: &str = "https://api.minimax.io/v1/chat/completions";

fn parse_minimax_stream_event(data: &str) -> LLMResult<Option<Value>> {
    match parse_provider_json_value(data) {
        Ok(value) => Ok(Some(value)),
        // Preserve MiniMax's compatibility behavior for small malformed SSE
        // frames, but never turn a structural admission failure into a skip.
        Err(LLMError::Serialization(_)) => Ok(None),
        Err(error) => Err(error),
    }
}

/// Provider for MiniMax's OpenAI-compatible Chat Completions API.
pub struct MinimaxProvider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl MinimaxProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL)
    }

    pub fn with_base_url(api_key: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, base_url)
    }

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

    pub fn capabilities_for_model(model: &str) -> LLMCapability {
        // MiniMax M3 is natively multimodal (text + image + video input) and
        // works with image + reasoning + tool-calling in one request (verified
        // on this OpenAI-compatible endpoint). The M2 family (M2 / M2.1 / M2.5 /
        // M2.7 and -highspeed) is text-only.
        let mut modalities = vec![LLMModality::Text];
        if model.contains("M3") {
            modalities.push(LLMModality::Vision);
        }
        LLMCapability {
            modalities,
            reasoning: LLMReasoning::Standard,
            tool_calling: true,
            json_mode: false,
            streaming: true,
            computer_use: false,
            web_search: false,
        }
    }

    /// Strip the Magician cache-breakpoint sentinel from a text payload. MiniMax
    /// (like OpenAI Chat) has no breakpoint concept and uses automatic upstream
    /// prefix caching, so the internal marker must be removed before sending.
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

    /// Split an inline `<think>...</think>` reasoning block out of assistant
    /// content. MiniMax delivers reasoning inline (especially the M2 family and
    /// the vision path) rather than always in a separate field, so we lift it
    /// into `reasoning_text` and keep the visible answer clean.
    fn split_think(content: &str) -> (String, Option<String>) {
        if let (Some(start), Some(end)) = (content.find("<think>"), content.find("</think>")) {
            if start < end {
                let think = content[start + "<think>".len()..end].trim().to_string();
                let mut text = String::new();
                text.push_str(&content[..start]);
                text.push_str(&content[end + "</think>".len()..]);
                let text = text.trim().to_string();
                return (text, if think.is_empty() { None } else { Some(think) });
            }
        }
        (content.to_string(), None)
    }

    fn map_messages(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        let mut payload = Vec::with_capacity(messages.len());

        for message in messages {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            // Collect blocks into ordered buckets. CRITICAL: `tool_results` is a
            // Vec of (tool_call_id, content) preserving block order. A single
            // user LLMMessage from a parallel / multi-tool turn carries N
            // ToolResult blocks, and EACH must become its own `role:tool` wire
            // message keyed by its own id. The previous single `Option<String>`
            // kept only the LAST id, leaving the other tool_calls unanswered —
            // MiniMax rejected that with "tool call result does not follow tool
            // call (2013)".
            let mut text_parts: Vec<String> = Vec::new();
            let mut image_parts: Vec<Value> = Vec::new();
            let mut tool_calls: Vec<Value> = Vec::new();
            let mut tool_results: Vec<(String, String)> = Vec::new();

            for block in &message.content {
                match block {
                    ContentBlock::Text { text } => {
                        text_parts.push(Self::strip_cache_sentinel(text))
                    },
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
                            "function": { "name": name, "arguments": args_str }
                        }));
                    },
                    ContentBlock::ToolResult {
                        tool_call_id,
                        content,
                    } => {
                        // MiniMax tool-role content is a STRING. Flatten rich /
                        // multimodal tool results (e.g. the `_magicllm_rich_tool_result`
                        // envelope used for image tool outputs) to their text; the
                        // native v2 endpoint blocks images anyway and the OpenAI
                        // surface expects a string here. Pair each result with its
                        // own id so a multi-result turn fans out to N tool messages.
                        let text = match content {
                            Value::String(s) => s.clone(),
                            other => serde_json::to_string(other).unwrap_or_default(),
                        };
                        tool_results.push((tool_call_id.clone(), text));
                    },
                    ContentBlock::Image {
                        data, media_type, ..
                    } => {
                        let url = format!("data:{};base64,{}", media_type, BASE64.encode(data));
                        image_parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    },
                    ContentBlock::ImageUrl { url, .. } => {
                        image_parts.push(json!({
                            "type": "image_url",
                            "image_url": { "url": url }
                        }));
                    },
                }
            }

            // Assistant turn that issued tool calls. magician never mixes
            // tool_calls and tool_results in one LLMMessage (the decision layer
            // puts results in the FOLLOWING user message), so this is terminal:
            // emit one assistant message carrying the pre-tool narration (or
            // null) plus the tool_calls array, then move on. The `continue` is
            // what stops the narration text from being re-emitted below as a
            // stray user message — which would itself break the assistant→tool
            // adjacency and re-trigger 2013.
            if !tool_calls.is_empty() {
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
                continue;
            }

            // Tool-result run: ONE `role:tool` message per result, in original
            // order, each with its own tool_call_id and STRING content. This
            // contiguous block immediately follows the assistant tool_calls
            // message; the observation text/images the decision layer appended
            // to this same user message are emitted as a SEPARATE message below,
            // AFTER the run closes — never interleaved between a tool_call and
            // its response.
            for (tc_id, content) in &tool_results {
                payload.push(json!({
                    "role": "tool",
                    "tool_call_id": tc_id,
                    "content": content,
                }));
            }

            // Trailing observation / plain content, emitted as its own message.
            // When it follows a tool-result run it is the appended current
            // observation and must ride a `role:user` turn (a screenshot can
            // never live in a tool message); otherwise the message's own role is
            // preserved.
            let after_tool_run = !tool_results.is_empty();
            let out_role = if after_tool_run { "user" } else { role };
            if !image_parts.is_empty() {
                // Multimodal turn: content must be an array of typed parts.
                let mut parts: Vec<Value> = Vec::new();
                let joined = text_parts.join("\n");
                if !joined.is_empty() {
                    parts.push(json!({ "type": "text", "text": joined }));
                }
                parts.extend(image_parts);
                payload.push(json!({ "role": out_role, "content": parts }));
            } else if !text_parts.is_empty() {
                payload.push(json!({ "role": out_role, "content": text_parts.join("\n") }));
            } else if !after_tool_run {
                // Genuinely empty, non-tool message: preserve the prior behavior
                // of emitting an empty-content message for this role rather than
                // dropping it.
                payload.push(json!({ "role": role, "content": "" }));
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
            "minimax",
            "cached_tokens",
            data.get("prompt_tokens_details")
                .and_then(|d| d.get("cached_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| data.get("cached_tokens").and_then(Value::as_u64)),
        )?;
        let reasoning_tokens = super::bounded_usage_counter(
            "minimax",
            "reasoning_tokens",
            data.get("completion_tokens_details")
                .and_then(|d| d.get("reasoning_tokens"))
                .and_then(Value::as_u64)
                .or_else(|| data.get("reasoning_tokens").and_then(Value::as_u64)),
        )?;

        Ok(Some(TokenUsage {
            prompt_tokens: super::bounded_usage_counter(
                "minimax",
                "prompt_tokens",
                data.get("prompt_tokens").and_then(Value::as_u64),
            )?,
            completion_tokens: super::bounded_usage_counter(
                "minimax",
                "completion_tokens",
                data.get("completion_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: super::bounded_usage_counter(
                "minimax",
                "total_tokens",
                data.get("total_tokens").and_then(Value::as_u64),
            )?,
            reasoning_tokens,
            cached_tokens,
            cache_creation_tokens: None,
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
                    let arguments = function
                        .get("arguments")
                        .and_then(Value::as_str)
                        .map(parse_tool_argument_json_or_string)
                        .transpose()?
                        .unwrap_or(Value::Null);
                    calls.push(LLMToolCall {
                        id,
                        name,
                        arguments,
                    });
                }
            }
        }
        Ok(calls)
    }

    /// Reasoning text from a message: prefer the dedicated `reasoning_content`
    /// field, else join `reasoning_details[].text`, else the inline `<think>`
    /// body extracted by the caller.
    fn message_reasoning(message: &Value) -> Option<String> {
        if let Some(rc) = message.get("reasoning_content").and_then(Value::as_str) {
            if !rc.trim().is_empty() {
                return Some(rc.to_string());
            }
        }
        if let Some(r) = message.get("reasoning").and_then(Value::as_str) {
            if !r.trim().is_empty() {
                return Some(r.to_string());
            }
        }
        if let Some(details) = message.get("reasoning_details").and_then(Value::as_array) {
            let joined: String = details
                .iter()
                .filter_map(|d| d.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            if !joined.trim().is_empty() {
                return Some(joined);
            }
        }
        None
    }

    /// MiniMax returns HTTP 200 with a top-level `base_resp { status_code,
    /// status_msg }`. A non-zero `status_code` is an error even though the HTTP
    /// status is 200 — check it BEFORE reading `choices`.
    fn check_base_resp(payload: &Value, protected_request: bool) -> LLMResult<()> {
        if let Some(base) = payload.get("base_resp") {
            let code = base.get("status_code").and_then(Value::as_i64).unwrap_or(0);
            if code != 0 {
                let msg = base
                    .get("status_msg")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error")
                    .to_string();
                return match code {
                    // 1002 rate limit, 1041 concurrency limit.
                    1002 | 1041 => Err(LLMError::RateLimited { retry_after: None }),
                    _ if protected_request => Err(LLMError::Provider {
                        provider: "minimax".to_string(),
                        message: "protected provider request failed".to_owned(),
                    }),
                    _ => Err(LLMError::Provider {
                        provider: "minimax".to_string(),
                        message: format!("{msg} ({code})"),
                    }),
                };
            }
        }
        Ok(())
    }

    fn build_request_body(request: &LLMRequest) -> LLMResult<Map<String, Value>> {
        let messages = Self::map_messages(&request.messages)?;

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(request.model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));

        if !request.tools.is_empty() {
            body.insert(
                "tools".to_string(),
                Value::Array(Self::map_tools(&request.tools)),
            );
            // MiniMax documents only `none` | `auto` for tool_choice (NOT
            // `required`). Default to `auto`; honor an explicit caller override,
            // mapping Anthropic-style `{type:any}` to `auto`.
            let caller_override = request.extra_value().and_then(|e| e.get("tool_choice"));
            let tool_choice = match caller_override {
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("auto") => {
                    Value::String("auto".to_string())
                },
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("any") => {
                    Value::String("auto".to_string())
                },
                Some(v) if v.get("type").and_then(|t| t.as_str()) == Some("none") => {
                    Value::String("none".to_string())
                },
                Some(v @ Value::String(s)) if s == "auto" || s == "none" => {
                    clone_json_value_iteratively(v)
                },
                _ => Value::String("auto".to_string()),
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

        // MiniMax temperature range is (0, 1] — clamp defensively so profiles
        // tuned for other providers (which allow up to 2.0) don't 400.
        if let Some(temp) = request.temperature {
            let clamped = (temp as f64).clamp(0.01, 1.0);
            body.insert("temperature".to_string(), Value::from(clamped));
        }
        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }
        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_completion_tokens".to_string(), Value::from(max_tokens));
        }

        // Engage M3's reasoning via MiniMax's NATIVE `thinking` switch. M3
        // reasons inline (`<think>` / `reasoning_content`) adaptively, but left
        // on its default it frequently emitted ZERO reasoning before deciding
        // (the lazy "inspect, don't act" turns that sank the SoTA run). The
        // profile's `reasoning.effort` was previously dropped on the wire, so
        // the model never got the "think hard" signal. On this surface the
        // effective control is `thinking: {type: enabled|disabled}` — the same
        // toggle the old Anthropic-compat path used. A live probe showed it
        // roughly doubles reasoning vs. default, whereas the OpenAI-style
        // `reasoning_effort` is essentially a no-op here AND combining the two
        // REDUCES reasoning — so we send ONLY `thinking`. The toggle is BINARY:
        // M3 has no high/medium/low intensity dial, so the profile's effort
        // TIER collapses to on/off — `high`/`medium`/`low`/`minimal` all map to
        // the SAME `enabled` (the tier is not a depth M3 acts on; when enabled
        // it reasons adaptively per turn). An explicit `none`/`off` maps to
        // `disabled`; an absent reasoning config leaves M3 on its adaptive
        // default.
        if let Some(reasoning) = request.reasoning.as_ref() {
            let state = if reasoning.is_disabled() {
                "disabled"
            } else {
                "enabled"
            };
            body.insert("thinking".to_string(), json!({ "type": state }));
        }

        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                // Strip Magician/magicllm internal routing keys + reasoning
                // controls. `thinking` is set above from the typed reasoning
                // config (authoritative); `reasoning_effort` / `reasoning_split`
                // are a no-op / non-standard on this surface. Dropping them from
                // `extra` keeps a noisy `metadata:` block from injecting a
                // conflicting copy.
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
                        | "reasoning_summary"
                        | "reasoning_strategy"
                        | "reasoning_max_tokens"
                        | "reasoning_effort"
                        | "reasoning_split"
                        | "thinking"
                ) {
                    continue;
                }
                body.entry(key.clone())
                    .or_insert_with(|| clone_json_value_iteratively(value));
            }
        }

        Ok(body)
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
impl LLMProvider for MinimaxProvider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Minimax
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        crate::server_web_search::reject_unsupported("minimax", &request)?;
        if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for MiniMax provider".to_string(),
            ));
        }
        let model = request.model.clone();
        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let protected_request = request.metadata.single_physical_attempt;

        info!(
            provider = "minimax",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing MiniMax completion"
        );

        let body = Self::build_request_body(&request)?;
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

        // MiniMax surfaces logical errors via base_resp on HTTP 200 — check first.
        if let Err(err) = Self::check_base_resp(&payload, protected_request) {
            if protected_request {
                error!(
                    provider = "minimax",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    "MiniMax rejected protected request"
                );
            } else {
                error!(
                    provider = "minimax",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    error = %err,
                    "MiniMax returned a base_resp error"
                );
            }
            return Err(err);
        }

        if status.is_client_error() || status.is_server_error() {
            let message = payload
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("HTTP status {status}"));
            if protected_request {
                error!(
                    provider = "minimax",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "MiniMax API rejected protected request"
                );
                return Err(LLMError::Provider {
                    provider: "minimax".to_string(),
                    message: "protected provider request failed".to_owned(),
                });
            }
            error!(
                provider = "minimax",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                status = %status,
                error_message = %message,
                "MiniMax API responded with error"
            );
            return Err(LLMError::Provider {
                provider: "minimax".to_string(),
                message,
            });
        }

        let first_choice = payload
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
            .ok_or_else(|| LLMError::Provider {
                provider: "minimax".to_string(),
                message: "missing/empty choices array in MiniMax response".to_string(),
            })?;

        let message = first_choice.get("message");
        let raw_content = message
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let (text_body, inline_think) = Self::split_think(raw_content);
        let text = if text_body.is_empty() {
            None
        } else {
            Some(text_body)
        };

        let reasoning_text = message.and_then(Self::message_reasoning).or(inline_think);

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
            reasoning_text: reasoning_text.map(Arc::<str>::from),
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

        info!(
            provider = "minimax",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "MiniMax completion succeeded"
        );

        Ok(response)
    }

    async fn invoke_stream(
        &self,
        request: LLMRequest,
        tx: mpsc::Sender<StreamDelta>,
    ) -> LLMResult<()> {
        crate::server_web_search::reject_unsupported("minimax", &request)?;
        if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for MiniMax provider".to_string(),
            ));
        }
        let model = request.model.clone();
        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let protected_request = request.metadata.single_physical_attempt;

        info!(
            provider = "minimax",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "issuing MiniMax streaming completion"
        );

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
            let message = payload
                .get("error")
                .and_then(|e| e.get("message"))
                .and_then(Value::as_str)
                .map(|s| s.to_string())
                .unwrap_or_else(|| format!("HTTP status {status}"));
            if protected_request {
                return Err(LLMError::Provider {
                    provider: "minimax".to_string(),
                    message: "protected provider request failed".to_owned(),
                });
            }
            return Err(LLMError::Provider {
                provider: "minimax".to_string(),
                message,
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
        let mut base_resp_err: Option<LLMError> = None;
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
                let Some(value) = parse_minimax_stream_event(data.as_ref())? else {
                    continue;
                };
                drop(data);

                // base_resp can ride a streamed chunk; capture an error.
                if let Err(error) = Self::check_base_resp(&value, protected_request) {
                    base_resp_err = Some(error);
                }
                if let Some(usage) = value.get("usage") {
                    usage_value = Some(clone_json_value_iteratively(usage));
                }
                let Some(choice) = value
                    .get("choices")
                    .and_then(Value::as_array)
                    .and_then(|c| c.first())
                else {
                    continue;
                };
                if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
                    finish_reason = Some(reason.to_string());
                }
                let Some(delta) = choice.get("delta") else {
                    continue;
                };

                if let Some(content) = delta.get("content").and_then(Value::as_str) {
                    aggregated_text.push_str(content);
                    let _ = tx.send(StreamDelta::Token(content.to_string())).await;
                }
                if let Some(rc) = delta.get("reasoning_content").and_then(Value::as_str) {
                    let _ = tx
                        .send(StreamDelta::ReasoningDelta {
                            index: 0,
                            delta: rc.to_string(),
                        })
                        .await;
                }
                if let Some(tool_calls) = delta.get("tool_calls").and_then(Value::as_array) {
                    for tc in tool_calls {
                        let index = tc.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
                        let entry = tool_call_deltas
                            .entry(index)
                            .or_insert_with(ChatToolCallDelta::default);
                        if let Some(id) = tc.get("id").and_then(Value::as_str) {
                            entry.id = Some(id.to_string());
                        }
                        if let Some(function) = tc.get("function") {
                            if let Some(name) = function.get("name").and_then(Value::as_str) {
                                entry.name = Some(name.to_string());
                            }
                            if let Some(args) = function.get("arguments").and_then(Value::as_str) {
                                append_tool_argument_fragment(&mut entry.arguments, args)?;
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
                if done {
                    break;
                }
            }
            if consumed > 0 {
                buffer.drain(..consumed);
            }
        }
        finish_sse_utf8(&utf8_carry)?;

        if let Some(err) = base_resp_err {
            return Err(err);
        }

        let (text_body, inline_think) = Self::split_think(&aggregated_text);
        let text = if text_body.is_empty() {
            None
        } else {
            Some(text_body)
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
        let mut indices: Vec<u32> = tool_call_deltas.keys().copied().collect();
        indices.sort();
        for index in indices {
            if let Some(delta) = tool_call_deltas.remove(&index) {
                let arguments = parse_tool_argument_json_or_string(&delta.arguments)?;
                tool_calls.push(LLMToolCall {
                    id: delta.id.unwrap_or_default(),
                    name: delta.name.unwrap_or_default(),
                    arguments,
                });
            }
        }

        let aggregated = LLMResponse {
            text: text.map(Arc::<str>::from),
            reasoning_text: inline_think.map(Arc::<str>::from),
            response_id: None,
            messages: Arc::new(messages.unwrap_or_default()),
            tool_calls: Arc::new(tool_calls),
            tool_results: Arc::new(Vec::new()),
            usage: usage_value
                .as_ref()
                .map(Self::map_usage)
                .transpose()?
                .flatten(),
            finish_reason,
            raw_response: None,
            provider_latency_ms: None,
            trace_receipt: None,
            route_identity: None,
        };

        debug!(
            provider = "minimax",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            tool_call_count = aggregated.tool_calls.len(),
            "MiniMax streaming completion succeeded"
        );

        let _ = tx.send(StreamDelta::Done(aggregated)).await;
        Ok(())
    }

    async fn health_check(&self) -> LLMResult<bool> {
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_usage_rejects_wide_provider_counters() {
        let error = MinimaxProvider::map_usage(&json!({
            "total_tokens": u64::from(u32::MAX) + 1
        }))
        .expect_err("wide usage must fail closed");

        assert!(error.to_string().contains("total_tokens"));
    }

    #[test]
    fn malformed_small_stream_event_is_skipped_but_structural_overage_fails() {
        assert!(parse_minimax_stream_event("{not-json")
            .expect("small malformed compatibility frame")
            .is_none());

        let depth = crate::types::MAX_PROVIDER_RESPONSE_JSON_DEPTH + 1;
        let over_depth = format!("{}null{}", "[".repeat(depth), "]".repeat(depth));
        assert!(matches!(
            parse_minimax_stream_event(&over_depth),
            Err(LLMError::Validation(_))
        ));
    }

    #[test]
    fn protected_base_response_never_surfaces_upstream_message() {
        let marker = "secret-app-input-canary";
        let payload = json!({
            "base_resp": {
                "status_code": 2001,
                "status_msg": marker
            }
        });

        let protected = MinimaxProvider::check_base_resp(&payload, true)
            .expect_err("protected upstream failure must fail");
        assert!(!protected.to_string().contains(marker));

        let ordinary = MinimaxProvider::check_base_resp(&payload, false)
            .expect_err("ordinary upstream failure must fail");
        assert!(ordinary.to_string().contains(marker));
    }
}

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tracing::{debug, error, info, warn};

use super::{default_http_client, read_bounded_response_text};
use crate::{
    capability::{LLMCapability, LLMModality, LLMProviderKind},
    error::{LLMError, LLMResult},
    provider::LLMProvider,
    types::{
        clone_json_value_iteratively, parse_provider_json_value,
        parse_tool_argument_json_or_string, ContentBlock, LLMMessage, LLMRequest, LLMResponse,
        LLMResponseFormat, LLMToolCall, MessageRole, TokenUsage,
    },
};

pub const DEFAULT_BASE_URL_YUTORI: &str = "https://api.yutori.com/v1/chat/completions";

/// Extract a plain string from a `Value`, avoiding the double-quoting that
/// `Value::to_string()` produces for `Value::String`.
fn value_to_content_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Yutori N1 provider — pixels-to-actions browser use model.
///
/// Uses the OpenAI Chat Completions wire format with vision support.
/// The N1 model accepts screenshots as `image_url` content blocks and returns
/// browser actions (click, type, scroll, etc.) as tool calls with coordinates
/// in a normalised 1000×1000 space.
///
/// # Integration status (updated v0.6.701)
///
/// This provider handles transport (send/receive messages + tool calls to the
/// Yutori Chat Completions API) **and** coordinate denormalization. The model
/// emits `[x, y]` in a normalized 1000×1000 grid; `translate_tool_call_coordinates`
/// rewrites every coordinate to CSS viewport pixels — per-axis and clamped —
/// using `extra.viewport`, which the magician browser runtime injects from the
/// live session (`window.innerWidth/innerHeight`). Per-axis scaling is correct
/// for this stretched-grid model and is DPR-independent (the model returns a
/// *fraction* of a viewport-only screenshot, whose aspect matches the CSS
/// viewport at any device-pixel ratio). The magician-side action shim
/// (`primitive_dispatch::browser::yutori_translator`) maps N1.5's native
/// `browser_tools_core` actions to agent-browser argv, and the flat loop
/// preserves full multi-turn history.
///
/// Remaining/known gaps are tracked in
/// `docs/plans/2026-06-01-yutori-navigator-fix.md`: the model must still be
/// SELECTED at runtime (a `when_has_images` operation mapping to a Yutori
/// profile); screenshots sent to N1.5 must be viewport-only (a `--full`
/// screenshot breaks the fraction→pixel mapping); and the expanded `ref`-based
/// toolset is not yet bridged to agent-browser (the shim constrains the model
/// to the coordinate-based core set).
pub struct YutoriN1Provider {
    client: Client,
    api_key: String,
    base_url: String,
    default_timeout: Duration,
}

impl YutoriN1Provider {
    fn provider_error_message(error: &Value, protected_request: bool) -> String {
        if protected_request {
            return "protected provider request failed".to_owned();
        }
        error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown error")
            .to_owned()
    }

    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(default_http_client(), api_key, DEFAULT_BASE_URL_YUTORI)
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

    /// N1 models support vision + tool calling (browser actions) + computer use.
    /// No reasoning tiers, no streaming documented.
    pub(crate) fn capabilities_for_model(_model: &str) -> LLMCapability {
        LLMCapability {
            modalities: vec![LLMModality::Text, LLMModality::Vision],
            reasoning: crate::capability::LLMReasoning::None,
            tool_calling: true,
            json_mode: true,
            streaming: false,
            computer_use: true,
            web_search: false,
        }
    }

    /// Strip the Magician cache-breakpoint sentinel from a text payload,
    /// reconnecting the halves with a single `\n` when both are non-empty.
    ///
    /// Yutori N1 doesn't cache prompts, but if the rendered user prompt
    /// carries our internal marker we must strip it before sending so the
    /// browser-action model never sees the sentinel string. Matches the
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

    /// Map normalised messages to OpenAI Chat Completions format, including
    /// vision content blocks (image_url) for screenshots.
    ///
    /// N1 docs discourage system prompts. System message text is collected and
    /// prepended to the first user message to avoid consecutive same-role
    /// messages (which the API rejects with 400).
    fn map_messages(messages: &[LLMMessage]) -> LLMResult<Vec<Value>> {
        // Collect system text to merge into the first user message.
        let system_text: String = messages
            .iter()
            .filter(|m| m.role == MessageRole::System)
            .flat_map(|m| m.content.iter())
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(Self::strip_cache_sentinel(text)),
                ContentBlock::Json { value } => Some(value.as_str().unwrap_or("").to_string()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let mut payload = Vec::with_capacity(messages.len());
        let mut system_prepended = system_text.is_empty();

        for message in messages {
            // Skip system messages — their content was collected above.
            if message.role == MessageRole::System {
                continue;
            }

            let role = match message.role {
                MessageRole::System => unreachable!(),
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };

            // Check if message has mixed content (text + images).
            let has_image = message.content.iter().any(|b| {
                matches!(
                    b,
                    ContentBlock::Image { .. } | ContentBlock::ImageUrl { .. }
                )
            });

            if has_image {
                // Multi-part content array for vision messages.
                let mut parts: Vec<Value> = Vec::new();
                // Prepend system text to the first user message.
                if !system_prepended && message.role == MessageRole::User {
                    parts.push(json!({"type": "text", "text": &system_text}));
                    system_prepended = true;
                }
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            parts.push(json!({
                                "type": "text",
                                "text": Self::strip_cache_sentinel(text),
                            }));
                        },
                        ContentBlock::ImageUrl { url, .. } => {
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": url, "detail": "high"}
                            }));
                        },
                        ContentBlock::Image {
                            data, media_type, ..
                        } => {
                            let b64 = base64::engine::general_purpose::STANDARD.encode(data);
                            let data_url = format!("data:{};base64,{}", media_type, b64);
                            parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": data_url, "detail": "high"}
                            }));
                        },
                        ContentBlock::ToolCall {
                            id,
                            name,
                            arguments,
                        } => {
                            parts.push(json!({
                                "type": "text",
                                "text": format!("tool_call: {} {} {}", id, name, arguments)
                            }));
                        },
                        ContentBlock::ToolResult {
                            tool_call_id,
                            content,
                        } => {
                            let content_str = value_to_content_string(content);
                            parts.push(json!({
                                "type": "text",
                                "text": format!("tool_result {}: {}", tool_call_id, content_str)
                            }));
                        },
                        ContentBlock::Json { value } => {
                            parts.push(json!({"type": "text", "text": value.to_string()}));
                        },
                    }
                }
                let mut msg = json!({"role": role, "content": parts});
                // Tool messages need tool_call_id at the message level.
                if message.role == MessageRole::Tool {
                    if let Some(ContentBlock::ToolResult { tool_call_id, .. }) =
                        message.content.first()
                    {
                        msg["tool_call_id"] = Value::String(tool_call_id.clone());
                    }
                }
                payload.push(msg);
            } else {
                // Text-only or tool result message.
                match message.role {
                    MessageRole::Tool => {
                        // Tool results: extract tool_call_id and content.
                        let (tool_call_id, content_str) = message
                            .content
                            .iter()
                            .find_map(|b| {
                                if let ContentBlock::ToolResult {
                                    tool_call_id,
                                    content,
                                } = b
                                {
                                    Some((tool_call_id.clone(), value_to_content_string(content)))
                                } else {
                                    None
                                }
                            })
                            .unwrap_or_else(|| {
                                let text: String = message
                                    .content
                                    .iter()
                                    .filter_map(|b| {
                                        if let ContentBlock::Text { text } = b {
                                            Some(Self::strip_cache_sentinel(text))
                                        } else {
                                            None
                                        }
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n");
                                (String::new(), text)
                            });
                        payload.push(json!({
                            "role": "tool",
                            "tool_call_id": tool_call_id,
                            "content": content_str,
                        }));
                    },
                    MessageRole::Assistant => {
                        // Check for tool calls in content blocks.
                        let tool_calls: Vec<Value> = message
                            .content
                            .iter()
                            .filter_map(|b| {
                                if let ContentBlock::ToolCall {
                                    id,
                                    name,
                                    arguments,
                                } = b
                                {
                                    Some(json!({
                                        "id": id,
                                        "type": "function",
                                        "function": {
                                            "name": name,
                                            "arguments": arguments.to_string(),
                                        }
                                    }))
                                } else {
                                    None
                                }
                            })
                            .collect();

                        let text: String = message
                            .content
                            .iter()
                            .filter_map(|b| match b {
                                ContentBlock::Text { text } => {
                                    Some(Self::strip_cache_sentinel(text))
                                },
                                ContentBlock::Json { value } => Some(value.to_string()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n");

                        let mut msg = json!({"role": "assistant"});
                        if !text.is_empty() {
                            msg["content"] = Value::String(text);
                        } else {
                            msg["content"] = Value::Null;
                        }
                        if !tool_calls.is_empty() {
                            msg["tool_calls"] = Value::Array(tool_calls);
                        }
                        payload.push(msg);
                    },
                    _ => {
                        let mut parts: Vec<String> = Vec::new();
                        // Prepend system text to the first user message.
                        if !system_prepended && message.role == MessageRole::User {
                            parts.push(system_text.clone());
                            system_prepended = true;
                        }
                        for b in &message.content {
                            match b {
                                ContentBlock::Text { text } => {
                                    parts.push(Self::strip_cache_sentinel(text))
                                },
                                ContentBlock::Json { value } => parts.push(value.to_string()),
                                _ => {},
                            }
                        }
                        let text = parts.join("\n");
                        payload.push(json!({"role": role, "content": text}));
                    },
                }
            }
        }

        Ok(payload)
    }

    fn map_usage(data: &Value) -> LLMResult<Option<TokenUsage>> {
        Ok(Some(TokenUsage {
            prompt_tokens: super::bounded_usage_counter(
                "yutori_n1",
                "prompt_tokens",
                data.get("prompt_tokens").and_then(Value::as_u64),
            )?,
            completion_tokens: super::bounded_usage_counter(
                "yutori_n1",
                "completion_tokens",
                data.get("completion_tokens").and_then(Value::as_u64),
            )?,
            total_tokens: super::bounded_usage_counter(
                "yutori_n1",
                "total_tokens",
                data.get("total_tokens").and_then(Value::as_u64),
            )?,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_creation_tokens: None,
        }))
    }

    /// Argument-object field names whose value is a `[x, y]` coordinate
    /// pair the Yutori Navigator emits in a normalized 1000×1000 space.
    ///
    /// Sourced from the canonical Python SDK
    /// (`yutori/navigator/replay.py::_PREFERRED_ACTION_KEYS`,
    /// `_get_action_marker_style`). Single-point actions (`left_click`,
    /// `right_click`, `middle_click`, `double_click`, `triple_click`,
    /// `click`, `scroll`, `mouse_move`, `hover`) populate `coordinates`;
    /// drag-style actions (`drag`, `left_click_drag`) populate any
    /// combination of `start_coordinates`, `end_coordinates`,
    /// `center_coordinates`, and `coordinates`. We translate by FIELD
    /// NAME so all of these get denormalized regardless of the tool the
    /// model picked, and we don't need to maintain a tool-name list as
    /// Yutori adds new actions in future tool sets.
    ///
    /// Tools that don't take coordinates (`type`, `key`, `hold_key`,
    /// `wait`, `goto_url`, `go_back`, `screenshot`, `cursor_position`,
    /// `extract_elements`, `find`, `set_element_value`, `execute_js`)
    /// don't carry these field names so they pass through untouched.
    /// Custom (caller-supplied) tools that happen to use one of these
    /// names with a 2-number array WILL be denormalized — which is the
    /// right call since Yutori is fine-tuned to emit normalized coords
    /// across all tool calls anyway.
    const YUTORI_COORDINATE_FIELDS: &'static [&'static str] = &[
        "coordinates",
        "start_coordinates",
        "end_coordinates",
        "center_coordinates",
    ];

    /// Read viewport dimensions from `request.extra.viewport`. The magician
    /// runtime injects these from the active browser session before each
    /// LLM call. Returns `None` when absent — the translator falls back to
    /// identity (no-op) so non-magician callers and non-browser flows are
    /// unaffected.
    fn extract_viewport(request: &LLMRequest) -> Option<(f64, f64)> {
        let viewport = request.extra.as_ref()?.get("viewport")?;
        let width = viewport.get("width").and_then(Value::as_f64)?;
        let height = viewport.get("height").and_then(Value::as_f64)?;
        if width > 0.0 && height > 0.0 {
            Some((width, height))
        } else {
            None
        }
    }

    /// Whether a returned tool call carries any coordinate payload that would
    /// need denormalization — a builtin `coordinates`-style field, or an
    /// agent-browser `mouse`/`batch` argv that may contain raw coordinates.
    /// Used only to decide whether a missing viewport is worth warning about.
    fn tool_call_carries_coordinates(tc: &LLMToolCall) -> bool {
        if let Some(obj) = tc.arguments.as_object() {
            if Self::YUTORI_COORDINATE_FIELDS
                .iter()
                .any(|field| obj.contains_key(*field))
            {
                return true;
            }
        }
        matches!(tc.name.as_str(), "mouse" | "batch")
    }

    /// Denormalize one axis from the Yutori 0-1000 grid to a viewport pixel,
    /// clamped to `[0, dim-1]`. Per-axis scaling against the (CSS-pixel)
    /// viewport is the correct inverse for this stretched-grid model — the
    /// model returns a *fraction* of the screenshot it saw, and a viewport-only
    /// screenshot shares the viewport's aspect at any device-pixel ratio, so
    /// the fraction maps straight to CSS pixels (DPR cancels). The clamp guards
    /// the boundary case where the model emits exactly 1000 (or slightly past),
    /// which would otherwise land one pixel outside the viewport where a CDP
    /// mouse event silently fails to dispatch.
    fn denorm_axis(norm: f64, dim: f64) -> i64 {
        let px = (norm * dim / 1000.0).round() as i64;
        px.clamp(0, (dim as i64 - 1).max(0))
    }

    /// Translate `[x_norm, y_norm]` from the Yutori 1000×1000 normalized
    /// space to actual viewport pixels, clamped to the viewport.
    fn denormalize_coordinates(coords: &Value, viewport: (f64, f64)) -> Option<Value> {
        let pair = coords.as_array()?;
        if pair.len() != 2 {
            return None;
        }
        let x_norm = pair[0].as_f64()?;
        let y_norm = pair[1].as_f64()?;
        let (vw, vh) = viewport;
        let x_px = Self::denorm_axis(x_norm, vw);
        let y_px = Self::denorm_axis(y_norm, vh);
        Some(Value::Array(vec![
            Value::Number(x_px.into()),
            Value::Number(y_px.into()),
        ]))
    }

    /// Walk a tool-call's argument object and rewrite every well-known
    /// coordinate field in place. Drag-style actions can carry up to
    /// four (`start_coordinates`, `end_coordinates`, `center_coordinates`,
    /// `coordinates`) in one call — translate every one we find. Pass
    /// through fields not in the coordinate field set, and pass through
    /// values whose shape doesn't match `[x, y]`.
    fn translate_tool_call_coordinates(
        tool_name: &str,
        arguments: &mut Value,
        viewport: (f64, f64),
    ) {
        let Some(obj) = arguments.as_object_mut() else {
            return;
        };
        for field in Self::YUTORI_COORDINATE_FIELDS {
            let Some(raw_coords) = obj.get(*field) else {
                continue;
            };
            if let Some(translated) = Self::denormalize_coordinates(raw_coords, viewport) {
                obj.insert(field.to_string(), translated);
            }
        }

        // Custom-schema (agent-browser) tool calls: Yutori is fine-tuned
        // to emit normalized 1000×1000 coordinates regardless of which
        // tool schema it sees, but our agent-browser tools don't carry a
        // typed `coordinates` field — coords land as numeric strings
        // inside an `args` array. The provider has to know the
        // tool-specific arg position of any coordinate value to translate
        // it. Below: hardcoded knowledge of the agent-browser `mouse`
        // primitive (the only catalog tool today that takes raw
        // coordinates) plus the `batch` tool whose `commands` are arrays
        // of agent-browser argv that may contain nested `mouse move x y`
        // calls. Add new entries when other custom tools start carrying
        // coords.
        Self::translate_agent_browser_args(tool_name, obj, viewport);
    }

    /// Translate normalized → viewport-pixel coordinates inside our
    /// custom agent-browser tool schemas. The Yutori model emits
    /// coordinates in 1000×1000 normalized space even when calling
    /// caller-supplied tools, so values like `["move","391","377"]`
    /// inside `mouse({"args": …})` need to be rewritten to actual
    /// viewport pixels before reaching agent-browser, just like the
    /// builtin `coordinates` field on `left_click`/`mouse_move` does.
    fn translate_agent_browser_args(
        tool_name: &str,
        arguments: &mut serde_json::Map<String, Value>,
        viewport: (f64, f64),
    ) {
        match tool_name {
            // mouse({"args": ["move","X","Y"]}) → translate args[1], args[2].
            // Other subcommands (down/up/wheel) carry no coordinates.
            "mouse" => {
                if let Some(Value::Array(arr)) = arguments.get_mut("args") {
                    Self::translate_mouse_argv(arr, viewport);
                }
            },
            // batch({"commands": [["mouse","move","X","Y"], …]}) — nested
            // agent-browser argv. Recurse into each inner command and
            // translate when the head is a known coordinate-bearing
            // subcommand. Index 0 inside a batch command is the
            // agent-browser CLI tool name, e.g. "mouse"; index 1 is the
            // subcommand ("move"); indices 2 and 3 are X, Y.
            "batch" => {
                if let Some(Value::Array(commands)) = arguments.get_mut("commands") {
                    for cmd in commands {
                        if let Value::Array(inner) = cmd {
                            // Drop the leading tool name (e.g. "mouse")
                            // for the translator, mirroring the bare
                            // `mouse` argv layout.
                            let head = inner.first().and_then(|v| v.as_str()).map(str::to_string);
                            if head.as_deref() == Some("mouse") {
                                let mut tail = inner.iter().skip(1).cloned().collect::<Vec<_>>();
                                Self::translate_mouse_argv(&mut tail, viewport);
                                if !tail.is_empty() {
                                    inner.splice(1.., tail);
                                }
                            }
                        }
                    }
                }
            },
            _ => {},
        }
    }

    /// Given the args layout for the agent-browser `mouse` CLI primitive
    /// (`[subcommand, X, Y, …]`), denormalize the X/Y values when the
    /// subcommand is `move`. Mutates in place.
    fn translate_mouse_argv(argv: &mut [Value], viewport: (f64, f64)) {
        let Some(subcommand) = argv.first().and_then(|v| v.as_str()) else {
            return;
        };
        if subcommand != "move" || argv.len() < 3 {
            return;
        }
        let Some(x_norm) = argv[1]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| argv[1].as_f64())
        else {
            return;
        };
        let Some(y_norm) = argv[2]
            .as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or_else(|| argv[2].as_f64())
        else {
            return;
        };
        let (vw, vh) = viewport;
        let x_px = Self::denorm_axis(x_norm, vw);
        let y_px = Self::denorm_axis(y_norm, vh);
        argv[1] = Value::String(x_px.to_string());
        argv[2] = Value::String(y_px.to_string());
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

#[async_trait]
impl LLMProvider for YutoriN1Provider {
    fn provider_kind(&self) -> LLMProviderKind {
        LLMProviderKind::Yutori
    }

    fn capabilities(&self, model: &str) -> LLMCapability {
        Self::capabilities_for_model(model)
    }

    async fn invoke(&self, request: LLMRequest) -> LLMResult<LLMResponse> {
        crate::server_web_search::reject_unsupported("yutori_n1", &request)?;
        let model = if request.model.is_empty() {
            return Err(LLMError::Validation(
                "LLMRequest.model must be set for Yutori N1 provider".to_string(),
            ));
        } else {
            request.model.clone()
        };

        let operation = request.metadata.operation.clone();
        let trace_id = request.metadata.trace_id.clone();
        let protected_request = request.metadata.single_physical_attempt;

        info!(
            provider = "yutori_n1",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            message_count = request.messages.len(),
            tool_count = request.tools.len(),
            "issuing Yutori N1 completion"
        );

        if request.stream {
            return Err(LLMError::UnsupportedCapability(
                "Yutori N1 provider does not support streaming".to_string(),
            ));
        }

        let messages = Self::map_messages(&request.messages)?;

        let mut body = Map::new();
        body.insert("model".to_string(), Value::String(model.clone()));
        body.insert("messages".to_string(), Value::Array(messages));

        // Yutori N1 is trained on its own native browser-action catalog
        // (left_click, type, scroll, goto_url, …) and emits all
        // interactions through that catalog regardless of what `tools`
        // we pass. Earlier we tried disabling N1's builtins by name and
        // forwarding our own agent-browser catalog so the model would
        // call `click` / `mouse move` / etc. directly — that produced
        // wrong tool calls because N1 ignores prompt-stuffed schemas.
        // Inverted approach: don't forward custom tools, don't send a
        // disable list. Let N1 emit its native vocabulary and translate
        // the resulting tool_calls to agent-browser argv on the magician
        // side (see `magician::execution::inner_loop::browser`'s Yutori
        // translator). Coordinates arrive normalized to 1000×1000 and
        // are denormalized via `extra.viewport` further down this same
        // request build.

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

        // N1's reference SDK defaults temperature to 0.3; respect an explicit
        // override, else send 0.3 so behavior matches the reference client.
        let temperature = request
            .temperature
            .map(Value::from)
            .unwrap_or_else(|| Value::from(0.3));
        body.insert("temperature".to_string(), temperature);

        if let Some(top_p) = request.top_p {
            body.insert("top_p".to_string(), Value::from(top_p));
        }

        if let Some(max_tokens) = request.max_output_tokens {
            body.insert("max_completion_tokens".to_string(), Value::from(max_tokens));
        }

        // Pass through extra params, filtering config-only keys.
        // `viewport` is consumed earlier in this provider
        // (denormalize_coordinates); it's a router-internal hint, not a
        // Yutori API parameter, so it must NOT be forwarded as part of
        // the request body. Same for the magicllm router pins and
        // OpenAI-only `openai_*` keys that may appear when a profile
        // gets swapped at the dispatch site.
        if let Some(Value::Object(extra_map)) = request.extra_value() {
            for (key, value) in extra_map {
                if matches!(
                    key.as_str(),
                    "tool_choice"
                            | "cost_per_observation"
                            | "fallback_profile"
                            | "streaming"
                            | "viewport"
                            | "openai_api_mode"
                            | "openai_responses_disable_chaining"
                            | "gemini_api_mode"
                            | "openai_previous_response_id"
                            | "use_chat"
                            | "use_responses"
                            | "router_provider_override"
                            | "router_profile_override"
                            | "router_preserve_model"
                            | "max_tokens_retry_attempt"
                            | "verbosity"
                            // All `reasoning_*` keys live on the typed
                            // `ReasoningConfig` struct, not the metadata
                            // block. Yutori N1's schema does not accept
                            // them as top-level params — strip defensively.
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

        debug!(
            provider = "yutori_n1",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            "constructed Yutori N1 request body"
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
            if let Some(error_val) = payload.get("error") {
                let message = Self::provider_error_message(error_val, protected_request);
                if protected_request {
                    error!(
                        provider = "yutori_n1",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        "Yutori N1 API rejected protected request"
                    );
                } else {
                    error!(
                        provider = "yutori_n1",
                        model = %model,
                        operation = %operation,
                        trace_id = trace_id.as_deref().unwrap_or(""),
                        status = %status,
                        error_message = %message,
                        "Yutori N1 API responded with error"
                    );
                }
                return Err(LLMError::Provider {
                    provider: self.provider_kind().to_string(),
                    message,
                });
            } else {
                error!(
                    provider = "yutori_n1",
                    model = %model,
                    operation = %operation,
                    trace_id = trace_id.as_deref().unwrap_or(""),
                    status = %status,
                    "Yutori N1 API returned unexpected error payload"
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
                message: "missing choices array in Yutori N1 response".to_string(),
            })?;

        let first_choice = choices.first().ok_or_else(|| LLMError::Provider {
            provider: self.provider_kind().to_string(),
            message: "empty choices array in Yutori N1 response".to_string(),
        })?;

        let text = first_choice
            .get("message")
            .and_then(|msg| msg.get("content"))
            .and_then(Value::as_str)
            .map(|s| s.to_string());

        let mut tool_calls = Self::map_tool_calls(first_choice)?;

        // Yutori Navigator emits all coordinates in a normalized 1000×1000
        // space. Denormalize each builtin tool call's `coordinates` field
        // to actual viewport pixels before returning so the dispatcher
        // (e.g. agent-browser mouse/click commands) gets directly-usable
        // pixel coordinates. Pass-through when the magician runtime
        // doesn't supply `extra.viewport`, or for any tool name that
        // isn't a known coordinate-taking builtin.
        if let Some(viewport) = Self::extract_viewport(&request) {
            for tc in tool_calls.iter_mut() {
                Self::translate_tool_call_coordinates(&tc.name, &mut tc.arguments, viewport);
            }
        } else if tool_calls.iter().any(Self::tool_call_carries_coordinates) {
            // No `extra.viewport` was supplied but the model returned a
            // coordinate-bearing tool call. Without denormalization the raw
            // 0-1000 values pass through as if they were CSS pixels — a click
            // at normalized (500,500) lands in the top-left quadrant. This is
            // the dominant Yutori failure mode; surface it loudly instead of
            // silently mis-clicking.
            warn!(
                target: "magicllm::yutori",
                "Yutori returned coordinate-bearing tool calls but no extra.viewport was \
                 supplied — coordinates are NOT denormalized and will be treated as raw \
                 pixels. The magician browser runtime must inject extra.viewport (CSS \
                 window.innerWidth/innerHeight) for every Yutori browser call."
            );
        }

        // Build the assistant message for conversation history.
        // Include both text (reasoning) and tool call blocks so callers can
        // reconstruct the full turn when appending tool results.
        let mut assistant_content: Vec<ContentBlock> = Vec::new();
        if let Some(ref t) = text {
            assistant_content.push(ContentBlock::Text { text: t.clone() });
        }
        for tc in &tool_calls {
            assistant_content.push(ContentBlock::ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: clone_json_value_iteratively(&tc.arguments),
            });
        }

        let messages_out = if assistant_content.is_empty() {
            Vec::new()
        } else {
            vec![LLMMessage {
                role: MessageRole::Assistant,
                content: assistant_content,
            }]
        };

        let response = LLMResponse {
            text: text.clone().map(Arc::<str>::from),
            reasoning_text: None,
            response_id: None,
            messages: Arc::new(messages_out),
            tool_calls: Arc::new(tool_calls),
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
                provider = "yutori_n1",
                model = %model,
                operation = %operation,
                trace_id = trace_id.as_deref().unwrap_or(""),
                usage = ?usage,
                "token usage reported by Yutori N1"
            );
        }

        info!(
            provider = "yutori_n1",
            model = %model,
            operation = %operation,
            trace_id = trace_id.as_deref().unwrap_or(""),
            finish_reason = response.finish_reason.as_deref().unwrap_or(""),
            tool_call_count = response.tool_calls.len(),
            "Yutori N1 completion succeeded"
        );

        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_usage_rejects_wide_provider_counters() {
        let error = YutoriN1Provider::map_usage(&json!({
            "prompt_tokens": u64::from(u32::MAX) + 1
        }))
        .expect_err("wide usage must fail closed");

        assert!(error.to_string().contains("prompt_tokens"));
    }

    #[test]
    fn capabilities_include_vision_and_computer_use() {
        let cap = YutoriN1Provider::capabilities_for_model("n1-latest");
        assert!(cap.supports_modality(LLMModality::Vision));
        assert!(cap.supports_modality(LLMModality::Text));
        assert!(cap.tool_calling);
        assert!(cap.computer_use);
        assert!(!cap.streaming);
    }

    #[test]
    fn protected_provider_error_never_surfaces_upstream_message() {
        let marker = "secret-app-input-canary";
        let error = serde_json::json!({"message": marker});

        let protected = YutoriN1Provider::provider_error_message(&error, true);
        assert!(!protected.contains(marker));

        let ordinary = YutoriN1Provider::provider_error_message(&error, false);
        assert_eq!(ordinary, marker);
    }

    #[test]
    fn denormalize_translates_normalized_coords_to_viewport_pixels() {
        // Yutori emits [139, 652] in 1000×1000 space; viewport is 1280×800;
        // expected viewport pixels = (139 * 1.28, 652 * 0.8) = (178, 522).
        let coords = serde_json::json!([139, 652]);
        let translated =
            YutoriN1Provider::denormalize_coordinates(&coords, (1280.0, 800.0)).unwrap();
        assert_eq!(translated, serde_json::json!([178, 522]));
    }

    #[test]
    fn denormalize_clamps_to_viewport_bounds() {
        // Exactly 1000 maps to vw/vh (one pixel outside the viewport), where a
        // CDP mouse event silently fails to dispatch; clamp pulls it in. Values
        // the model occasionally emits past 1000 are clamped too.
        let edge = serde_json::json!([1000, 1000]);
        assert_eq!(
            YutoriN1Provider::denormalize_coordinates(&edge, (1280.0, 800.0)).unwrap(),
            serde_json::json!([1279, 799])
        );
        // Defensive: negatives clamp to 0.
        let neg = serde_json::json!([-5, -5]);
        assert_eq!(
            YutoriN1Provider::denormalize_coordinates(&neg, (1280.0, 800.0)).unwrap(),
            serde_json::json!([0, 0])
        );
    }

    #[test]
    fn translate_rewrites_left_click_coordinates_field() {
        let mut args = serde_json::json!({"coordinates": [500, 500]});
        YutoriN1Provider::translate_tool_call_coordinates("left_click", &mut args, (1280.0, 800.0));
        assert_eq!(args["coordinates"], serde_json::json!([640, 400]));
    }

    #[test]
    fn translate_preserves_non_coordinate_fields_in_scroll() {
        // scroll(amount=3, coordinates=[333,370], direction="down") — only
        // `coordinates` should be denormalized; `amount` and `direction`
        // pass through untouched.
        let mut args = serde_json::json!({
            "amount": 3,
            "coordinates": [333, 370],
            "direction": "down"
        });
        YutoriN1Provider::translate_tool_call_coordinates("scroll", &mut args, (1280.0, 800.0));
        assert_eq!(args["amount"], serde_json::json!(3));
        assert_eq!(args["direction"], serde_json::json!("down"));
        assert_eq!(args["coordinates"], serde_json::json!([426, 296]));
    }

    #[test]
    fn translate_leaves_unrelated_custom_tools_alone() {
        // Custom tools we don't have schema knowledge of (anything other
        // than `mouse` and `batch`) pass through untouched even when they
        // happen to carry numeric strings in `args`.
        let original = serde_json::json!({"args": ["custom-arg-1", "139", "652"]});
        let mut args = original.clone();
        YutoriN1Provider::translate_tool_call_coordinates(
            "some_unrelated_custom_tool",
            &mut args,
            (1280.0, 800.0),
        );
        assert_eq!(args, original);
    }

    #[test]
    fn translate_rewrites_all_drag_coordinate_fields() {
        // Drag-style actions can carry start_coordinates +
        // end_coordinates + center_coordinates + coordinates in one
        // call (per yutori-sdk-python::replay.py). All four must be
        // denormalized to viewport pixels, not just the canonical
        // `coordinates` field.
        let mut args = serde_json::json!({
            "start_coordinates": [100, 100],
            "end_coordinates": [900, 700],
            "center_coordinates": [500, 400],
            "coordinates": [500, 400],
            "duration": 250
        });
        YutoriN1Provider::translate_tool_call_coordinates(
            "left_click_drag",
            &mut args,
            (1280.0, 800.0),
        );
        assert_eq!(args["start_coordinates"], serde_json::json!([128, 80]));
        assert_eq!(args["end_coordinates"], serde_json::json!([1152, 560]));
        assert_eq!(args["center_coordinates"], serde_json::json!([640, 320]));
        assert_eq!(args["coordinates"], serde_json::json!([640, 320]));
        // Non-coord scalars stay untouched.
        assert_eq!(args["duration"], serde_json::json!(250));
    }

    #[test]
    fn translate_handles_unknown_tool_with_coordinates_field() {
        // Yutori is fine-tuned to emit normalized coords for any tool it
        // calls. If the model picks a tool we don't know about (new
        // version, expanded tool set) but populates a known coordinate
        // field name, we still translate. The marker is the field name,
        // not the tool name.
        let mut args = serde_json::json!({"coordinates": [250, 750]});
        YutoriN1Provider::translate_tool_call_coordinates(
            "future_unseen_tool",
            &mut args,
            (1280.0, 800.0),
        );
        assert_eq!(args["coordinates"], serde_json::json!([320, 600]));
    }

    #[test]
    fn translate_agent_browser_mouse_move_args_to_pixels() {
        // Yutori, when constrained to agent-browser's `mouse` schema,
        // emits coordinates as numeric strings inside `args` instead of a
        // typed `coordinates` field. Translator must recognize the
        // mouse-move argv shape and rewrite args[1] / args[2] to
        // viewport pixels.
        let mut args = serde_json::json!({"args": ["move", "391", "377"]});
        YutoriN1Provider::translate_tool_call_coordinates("mouse", &mut args, (1280.0, 800.0));
        assert_eq!(args["args"][0], serde_json::json!("move"));
        assert_eq!(args["args"][1], serde_json::json!("500"));
        assert_eq!(args["args"][2], serde_json::json!("302"));
    }

    #[test]
    fn translate_agent_browser_mouse_non_move_args_pass_through() {
        // mouse down/up/wheel carry no coordinates. The translator must
        // leave the args alone.
        for sub in ["down", "up"] {
            let original = serde_json::json!({"args": [sub]});
            let mut args = original.clone();
            YutoriN1Provider::translate_tool_call_coordinates("mouse", &mut args, (1280.0, 800.0));
            assert_eq!(args, original);
        }
        let original = serde_json::json!({"args": ["wheel", "500"]});
        let mut args = original.clone();
        YutoriN1Provider::translate_tool_call_coordinates("mouse", &mut args, (1280.0, 800.0));
        assert_eq!(args, original);
    }

    #[test]
    fn translate_agent_browser_batch_commands_translates_nested_mouse_move() {
        // batch({"commands": [["mouse","move","X","Y"], …]}) — translator
        // recurses into each inner command and rewrites coords for
        // mouse-move entries while leaving down/up entries alone.
        let mut args = serde_json::json!({
            "args": ["--bail", "--json"],
            "commands": [
                ["mouse", "move", "391", "377"],
                ["mouse", "down"],
                ["mouse", "move", "633", "472"],
                ["mouse", "up"],
                ["wait", "100"]
            ]
        });
        YutoriN1Provider::translate_tool_call_coordinates("batch", &mut args, (1280.0, 800.0));
        assert_eq!(
            args["commands"][0],
            serde_json::json!(["mouse", "move", "500", "302"])
        );
        assert_eq!(args["commands"][1], serde_json::json!(["mouse", "down"]));
        assert_eq!(
            args["commands"][2],
            serde_json::json!(["mouse", "move", "810", "378"])
        );
        assert_eq!(args["commands"][3], serde_json::json!(["mouse", "up"]));
        // Non-mouse batch entries (e.g. wait) are untouched.
        assert_eq!(args["commands"][4], serde_json::json!(["wait", "100"]));
    }

    #[test]
    fn translate_agent_browser_args_passthrough_for_other_tools() {
        // click({"args": ["@e2"]}) carries no coords — must pass through.
        let original = serde_json::json!({"args": ["@e2"]});
        let mut args = original.clone();
        YutoriN1Provider::translate_tool_call_coordinates("click", &mut args, (1280.0, 800.0));
        assert_eq!(args, original);
    }

    #[test]
    fn extract_viewport_returns_none_when_missing() {
        let request = LLMRequest {
            extra: Some(serde_json::json!({"other": "value"}).into()),
            ..Default::default()
        };
        assert!(YutoriN1Provider::extract_viewport(&request).is_none());
    }

    #[test]
    fn extract_viewport_reads_width_and_height_from_extra() {
        let request = LLMRequest {
            extra: Some(serde_json::json!({"viewport": {"width": 1280, "height": 800}}).into()),
            ..Default::default()
        };
        assert_eq!(
            YutoriN1Provider::extract_viewport(&request),
            Some((1280.0, 800.0))
        );
    }

    #[test]
    fn map_messages_handles_vision_content() {
        let messages = vec![LLMMessage {
            role: MessageRole::User,
            content: vec![
                ContentBlock::Text {
                    text: "Click the search button".to_string(),
                },
                ContentBlock::ImageUrl {
                    url: "data:image/webp;base64,abc123".to_string(),
                    prompt: None,
                },
            ],
        }];
        let mapped = YutoriN1Provider::map_messages(&messages).unwrap();
        assert_eq!(mapped.len(), 1);
        assert_eq!(mapped[0]["role"], "user");
        let content = mapped[0]["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image_url");
    }

    #[test]
    fn map_messages_handles_tool_result() {
        let messages = vec![LLMMessage {
            role: MessageRole::Tool,
            content: vec![ContentBlock::ToolResult {
                tool_call_id: "call-123".to_string(),
                content: serde_json::json!({"url": "https://google.com", "status": "ok"}),
            }],
        }];
        let mapped = YutoriN1Provider::map_messages(&messages).unwrap();
        assert_eq!(mapped[0]["role"], "tool");
        assert_eq!(mapped[0]["tool_call_id"], "call-123");
    }

    #[test]
    fn map_messages_handles_assistant_tool_calls() {
        let messages = vec![LLMMessage {
            role: MessageRole::Assistant,
            content: vec![
                ContentBlock::Text {
                    text: "I'll click the search button.".to_string(),
                },
                ContentBlock::ToolCall {
                    id: "call-456".to_string(),
                    name: "left_click".to_string(),
                    arguments: serde_json::json!({"coordinates": [500, 300]}),
                },
            ],
        }];
        let mapped = YutoriN1Provider::map_messages(&messages).unwrap();
        assert_eq!(mapped[0]["role"], "assistant");
        assert_eq!(mapped[0]["content"], "I'll click the search button.");
        let tool_calls = mapped[0]["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "left_click");
    }
}

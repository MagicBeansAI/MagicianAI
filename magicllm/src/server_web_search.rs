//! Server-side web search: one provider-neutral request flag, per-provider
//! wire translations, and response-side citation/search-count extraction.
//!
//! Supported transports:
//! - **OpenAI Responses** — `{"type": "web_search"}` server tool.
//! - **Anthropic Messages** — `web_search_20250305` server tool.
//! - **Gemini** — `googleSearch` grounding tool (both generateContent and
//!   Interactions paths).
//! - **OpenRouter** — the `web` search plugin on chat completions.
//!
//! Every other transport (OpenAI Chat Completions, DeepSeek, MiniMax, Ollama,
//! Yutori) rejects the flag with `UnsupportedCapability` instead of silently
//! dropping it: a caller that asked for server-side search must never get an
//! unsearched answer back without an error.
//!
//! # The no-mixing invariant
//!
//! A request carrying `server_web_search` must not also carry function
//! tools. Search-augmented one-shot calls and tool-dispatching agentic turns
//! are different disciplines; mixing them on one request lets the model
//! double-path (search server-side instead of calling the research tool) and
//! makes spend attribution ambiguous. Providers enforce this via
//! [`reject_mixed_with_function_tools`] at body-build time, so an
//! accidental combination fails closed before any bytes reach the wire.
//!
//! # Request shape
//!
//! The flag rides the generic `LLMRequest.extra` lane (same mechanism as
//! `openai_api_mode`), never a new `LLMRequest` field:
//!
//! ```json
//! { "server_web_search": true }
//! { "server_web_search": { "max_uses": 3, "allowed_domains": ["example.com"] } }
//! ```
//!
//! Neutral option keys, mapped per provider (unrecognized keys are ignored
//! by each builder rather than forwarded raw):
//! - `max_uses` — Anthropic `max_uses`; OpenRouter plugin `max_results`
//!   (clamped to the API's 1..=10). OpenAI Responses ignores it: the live
//!   API rejects `max_uses` on the `web_search` tool.
//! - `allowed_domains` / `blocked_domains` — OpenAI `filters`,
//!   Anthropic `allowed_domains`/`blocked_domains`.
//! - `user_location` — OpenAI and Anthropic approximate location object.
//!
//! # Response shape
//!
//! Citations and search counts are NOT added to `LLMResponse` (42 literal
//! constructors across the crate); providers already retain the complete
//! payload in `LLMResponse.raw_response`, so
//! [`extract_citations`]/[`web_search_call_count`] operate on that value
//! and any caller (telemetry, cost events, UI) can derive them lazily.

use crate::capability::LLMProviderKind;
use crate::error::{LLMError, LLMResult};
use crate::types::LLMRequest;
use serde::Serialize;
use serde_json::{json, Map, Value};

/// Extra-lane key enabling server-side web search for one request.
pub const EXTRA_SERVER_WEB_SEARCH: &str = "server_web_search";

/// Parsed state of the `server_web_search` extra-lane flag.
///
/// `true` and any object enable the flag; absence and `false` leave it off;
/// every other JSON shape (strings, numbers, arrays, null) is `Invalid` so
/// transports can fail the request closed — a mistyped flag must never
/// proceed as an unsearched answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagState {
    Off,
    On,
    Invalid,
}

/// Parse the flag into its tri-state form.
pub fn flag_state(request: &LLMRequest) -> FlagState {
    match request
        .extra_value()
        .and_then(|extra| extra.get(EXTRA_SERVER_WEB_SEARCH))
    {
        None => FlagState::Off,
        Some(Value::Bool(enabled)) => {
            if *enabled {
                FlagState::On
            } else {
                FlagState::Off
            }
        },
        Some(Value::Object(_)) => FlagState::On,
        Some(_) => FlagState::Invalid,
    }
}

/// True when the request enables server-side web search.
pub fn requested(request: &LLMRequest) -> bool {
    flag_state(request) == FlagState::On
}

/// Reject a mistyped flag at the same gates that enforce the other
/// invariants, so an invalid shape errors instead of degrading to "off".
pub fn reject_invalid_flag(provider: &str, request: &LLMRequest) -> LLMResult<()> {
    if flag_state(request) == FlagState::Invalid {
        return Err(LLMError::Validation(format!(
            "{provider}: `server_web_search` must be `true`, `false`, or an options \
             object; the provided value has an unsupported JSON shape"
        )));
    }
    Ok(())
}

/// Options object for the enabled flag, when the object form was used.
fn options(request: &LLMRequest) -> Option<&Map<String, Value>> {
    request
        .extra_value()
        .and_then(|extra| extra.get(EXTRA_SERVER_WEB_SEARCH))
        .and_then(Value::as_object)
}

fn option_str_list(options: &Map<String, Value>, key: &str) -> Option<Vec<String>> {
    options.get(key).and_then(Value::as_array).map(|entries| {
        entries
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect()
    })
}

fn option_u32(options: &Map<String, Value>, key: &str) -> Option<u32> {
    options
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok())
}

/// The no-mixing invariant: a request with server-side web search enabled
/// must not also advertise function tools.
pub fn reject_mixed_with_function_tools(provider: &str, request: &LLMRequest) -> LLMResult<()> {
    if requested(request) && !request.tools.is_empty() {
        return Err(LLMError::Validation(format!(
            "{provider}: `server_web_search` cannot be combined with function tools on one \
             request; server-side search turns and tool-dispatch turns must stay separate"
        )));
    }
    Ok(())
}

/// Fail-closed gate for transports without server-side web search support.
/// Must be called at invoke entry so the request errors before any HTTP.
/// A mistyped flag is rejected here too — it must not degrade to "off".
pub fn reject_unsupported(provider: &str, request: &LLMRequest) -> LLMResult<()> {
    match flag_state(request) {
        FlagState::Off => Ok(()),
        FlagState::On | FlagState::Invalid => Err(LLMError::UnsupportedCapability(format!(
            "{provider}: this transport does not support server-side web search; the \
             `server_web_search` flag is honored only by the OpenAI Responses, Anthropic, \
             Gemini and OpenRouter transports"
        ))),
    }
}

/// OpenAI Responses server tool: `{"type": "web_search", ...}`.
///
/// `max_uses` is deliberately NOT mapped here: the live Responses API
/// rejects `tools[n].max_uses` for `web_search` with `Unknown parameter`
/// (verified against production, 2026-08). Anthropic documents `max_uses`
/// for its server tool and maps it; OpenRouter maps it to `max_results`.
pub fn openai_responses_web_search_tool(request: &LLMRequest) -> Value {
    let mut tool = Map::new();
    tool.insert("type".to_string(), json!("web_search"));
    if let Some(options) = options(request) {
        let mut filters = Map::new();
        if let Some(domains) = option_str_list(options, "allowed_domains") {
            filters.insert("allowed_domains".to_string(), json!(domains));
        }
        if let Some(domains) = option_str_list(options, "blocked_domains") {
            filters.insert("blocked_domains".to_string(), json!(domains));
        }
        if !filters.is_empty() {
            tool.insert("filters".to_string(), Value::Object(filters));
        }
        if let Some(location) = options.get("user_location") {
            tool.insert("user_location".to_string(), location.clone());
        }
    }
    Value::Object(tool)
}

/// Anthropic Messages server tool: `web_search_20250305`.
pub fn anthropic_web_search_tool(request: &LLMRequest) -> Value {
    let mut tool = Map::new();
    tool.insert("type".to_string(), json!("web_search_20250305"));
    tool.insert("name".to_string(), json!("web_search"));
    if let Some(options) = options(request) {
        if let Some(max_uses) = option_u32(options, "max_uses") {
            tool.insert("max_uses".to_string(), json!(max_uses));
        }
        if let Some(domains) = option_str_list(options, "allowed_domains") {
            tool.insert("allowed_domains".to_string(), json!(domains));
        }
        if let Some(domains) = option_str_list(options, "blocked_domains") {
            tool.insert("blocked_domains".to_string(), json!(domains));
        }
        if let Some(location) = options.get("user_location") {
            tool.insert("user_location".to_string(), location.clone());
        }
    }
    Value::Object(tool)
}

/// Gemini grounding tool: `{"googleSearch": {}}`. Gemini's grounding tool
/// takes no per-request options on the stable surface. The REST API's
/// documented JSON shape is lowerCamelCase, matching `systemInstruction` /
/// `toolConfig` elsewhere in the Gemini body.
pub fn gemini_google_search_tool() -> Value {
    json!({ "googleSearch": {} })
}

/// OpenRouter chat-completions web plugin. Engine selection stays on the
/// account-level plugin settings; `max_results` is the only per-request
/// knob mapped from the neutral `max_uses` option, clamped to the API's
/// documented 1..=10 range.
pub fn openrouter_web_plugin(request: &LLMRequest) -> Value {
    let mut plugin = Map::new();
    plugin.insert("id".to_string(), json!("web"));
    if let Some(options) = options(request) {
        if let Some(max_uses) = option_u32(options, "max_uses") {
            plugin.insert("max_results".to_string(), json!(max_uses.clamp(1, 10)));
        }
    }
    Value::Object(plugin)
}

/// One cited source extracted from a provider response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LLMCitation {
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Extract deduplicated citations from a retained `LLMResponse.raw_response`
/// payload according to the provider that produced it.
///
/// Deduplication is by URL across the whole response (not adjacent runs),
/// preserving first-seen order. Returns an empty vec when the response
/// carries no citations (including when server-side search was never
/// enabled) — absence of citations is not an error.
pub fn extract_citations(provider: &LLMProviderKind, raw: &Value) -> Vec<LLMCitation> {
    let citations = match provider {
        LLMProviderKind::OpenAI => extract_openai_responses_citations(raw),
        LLMProviderKind::Anthropic => extract_anthropic_citations(raw),
        LLMProviderKind::Gemini => extract_gemini_citations(raw),
        LLMProviderKind::OpenRouter => extract_openrouter_citations(raw),
        _ => Vec::new(),
    };
    let mut seen = std::collections::HashSet::new();
    citations
        .into_iter()
        .filter(|citation| seen.insert(citation.url.clone()))
        .collect()
}

/// Number of server-side searches the provider executed for one response.
/// Used for per-call cost accounting; 0 when none ran.
///
/// OpenRouter reports the billed request count in
/// `usage.cost_details.web_search_requests_count`; annotations are only a
/// fallback because they mark excerpts (several per page), not searches.
pub fn web_search_call_count(provider: &LLMProviderKind, raw: &Value) -> usize {
    match provider {
        LLMProviderKind::OpenAI => raw
            .get("output")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter(|item| {
                        item.get("type").and_then(Value::as_str) == Some("web_search_call")
                    })
                    .count()
            })
            .unwrap_or(0),
        LLMProviderKind::Anthropic => raw
            .get("content")
            .and_then(Value::as_array)
            .map(|blocks| {
                blocks
                    .iter()
                    .filter(|block| {
                        block.get("type").and_then(Value::as_str) == Some("web_search_tool_result")
                    })
                    .count()
            })
            .unwrap_or(0),
        LLMProviderKind::Gemini => gemini_grounding_metadata_sources(raw)
            .iter()
            .filter_map(|metadata| metadata.get("webSearchQueries"))
            .filter_map(Value::as_array)
            .map(Vec::len)
            .sum(),
        LLMProviderKind::OpenRouter => {
            if let Some(count) = raw
                .pointer("/usage/cost_details/web_search_requests_count")
                .and_then(Value::as_u64)
            {
                return usize::try_from(count).unwrap_or(usize::MAX);
            }
            raw.pointer("/choices/0/message/annotations")
                .and_then(Value::as_array)
                .map(|annotations| {
                    annotations
                        .iter()
                        .filter(|annotation| {
                            annotation.get("type").and_then(Value::as_str) == Some("url_citation")
                        })
                        .count()
                })
                .unwrap_or(0)
        },
        _ => 0,
    }
}

/// Locate every groundingMetadata object in a Gemini response, covering
/// both wire shapes: generateContent (`candidates[].groundingMetadata`)
/// and Interactions (`steps[].groundingMetadata` plus
/// `steps[].response.candidates[].groundingMetadata`). Absent metadata
/// simply yields no sources.
fn gemini_grounding_metadata_sources(raw: &Value) -> Vec<&Value> {
    let mut sources = Vec::new();
    if let Some(candidates) = raw.get("candidates").and_then(Value::as_array) {
        for candidate in candidates {
            if let Some(metadata) = candidate.get("groundingMetadata") {
                sources.push(metadata);
            }
        }
    }
    if let Some(steps) = raw.get("steps").and_then(Value::as_array) {
        for step in steps {
            if let Some(metadata) = step.get("groundingMetadata") {
                sources.push(metadata);
            }
            if let Some(candidates) = step
                .pointer("/response/candidates")
                .and_then(Value::as_array)
            {
                for candidate in candidates {
                    if let Some(metadata) = candidate.get("groundingMetadata") {
                        sources.push(metadata);
                    }
                }
            }
        }
    }
    sources
}

/// OpenAI Responses: `output[].content[].annotations[]` of type
/// `url_citation` on `output_text` items.
fn extract_openai_responses_citations(raw: &Value) -> Vec<LLMCitation> {
    let mut citations = Vec::new();
    let Some(output) = raw.get("output").and_then(Value::as_array) else {
        return citations;
    };
    for item in output {
        let Some(content_items) = item.get("content").and_then(Value::as_array) else {
            continue;
        };
        for content in content_items {
            let Some(annotations) = content.get("annotations").and_then(Value::as_array) else {
                continue;
            };
            for annotation in annotations {
                if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
                    continue;
                }
                let Some(url) = annotation.get("url").and_then(Value::as_str) else {
                    continue;
                };
                citations.push(LLMCitation {
                    url: url.to_string(),
                    title: annotation
                        .get("title")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
        }
    }
    citations
}

/// Anthropic: `content[].content[]` blocks of type `web_search_result`
/// nested inside `web_search_tool_result` blocks.
fn extract_anthropic_citations(raw: &Value) -> Vec<LLMCitation> {
    let mut citations = Vec::new();
    let Some(blocks) = raw.get("content").and_then(Value::as_array) else {
        return citations;
    };
    for block in blocks {
        if block.get("type").and_then(Value::as_str) != Some("web_search_tool_result") {
            continue;
        }
        let Some(results) = block.get("content").and_then(Value::as_array) else {
            continue;
        };
        for result in results {
            if result.get("type").and_then(Value::as_str) != Some("web_search_result") {
                continue;
            }
            let Some(url) = result.get("url").and_then(Value::as_str) else {
                continue;
            };
            citations.push(LLMCitation {
                url: url.to_string(),
                title: result
                    .get("title")
                    .and_then(Value::as_str)
                    .map(str::to_string),
            });
        }
    }
    citations
}

/// Gemini: grounding chunks inside every groundingMetadata object found by
/// [`gemini_grounding_metadata_sources`] (both wire shapes).
fn extract_gemini_citations(raw: &Value) -> Vec<LLMCitation> {
    let mut citations = Vec::new();
    for metadata in gemini_grounding_metadata_sources(raw) {
        let Some(chunks) = metadata.get("groundingChunks").and_then(Value::as_array) else {
            continue;
        };
        for chunk in chunks {
            let Some(web) = chunk.get("web") else {
                continue;
            };
            let Some(url) = web.get("uri").and_then(Value::as_str) else {
                continue;
            };
            citations.push(LLMCitation {
                url: url.to_string(),
                title: web.get("title").and_then(Value::as_str).map(str::to_string),
            });
        }
    }
    citations
}

/// OpenRouter: `choices[].message.annotations[]` of type `url_citation`
/// (OpenAI-compatible annotation shape added by the web plugin).
fn extract_openrouter_citations(raw: &Value) -> Vec<LLMCitation> {
    let mut citations = Vec::new();
    let Some(annotations) = raw
        .pointer("/choices/0/message/annotations")
        .and_then(Value::as_array)
    else {
        return citations;
    };
    for annotation in annotations {
        if annotation.get("type").and_then(Value::as_str) != Some("url_citation") {
            continue;
        }
        let Some(url) = annotation.get("url").and_then(Value::as_str) else {
            continue;
        };
        citations.push(LLMCitation {
            url: url.to_string(),
            title: annotation
                .get("title")
                .and_then(Value::as_str)
                .map(str::to_string),
        });
    }
    citations
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LLMToolSpec;
    use serde_json::json;
    use std::sync::Arc;

    fn request_with_extra(extra: Value) -> LLMRequest {
        let mut request = LLMRequest {
            model: "test-model".to_string(),
            ..Default::default()
        };
        request.set_extra(extra);
        request
    }

    #[test]
    fn flag_absent_or_false_leaves_search_disabled() {
        assert!(!requested(&request_with_extra(json!({}))));
        assert!(!requested(&request_with_extra(
            json!({"server_web_search": false})
        )));
        assert!(!requested(&request_with_extra(
            json!({"server_web_search": "yes"})
        )));
        assert_eq!(
            flag_state(&request_with_extra(json!({"server_web_search": "yes"}))),
            FlagState::Invalid
        );
    }

    #[test]
    fn flag_bool_or_object_enables_search() {
        assert!(requested(&request_with_extra(
            json!({"server_web_search": true})
        )));
        assert!(requested(&request_with_extra(
            json!({"server_web_search": {"max_uses": 2}})
        )));
        assert!(requested(&request_with_extra(
            json!({"server_web_search": {}})
        )));
    }

    #[test]
    fn mistyped_flag_is_rejected_not_silently_ignored() {
        for malformed in [json!("true"), json!(1), json!([true]), json!(null)] {
            let request = request_with_extra(json!({ "server_web_search": malformed }));
            assert_eq!(flag_state(&request), FlagState::Invalid);
            assert!(
                reject_invalid_flag("openai_responses", &request).is_err(),
                "malformed flag {malformed} must fail closed"
            );
            // Unsupported transports reject the malformed flag too — it must
            // not degrade to "off" and bypass their fail-closed gate.
            assert!(reject_unsupported("deepseek", &request).is_err());
        }
    }

    #[test]
    fn mixing_with_function_tools_is_rejected() {
        let mut request = request_with_extra(json!({"server_web_search": true}));
        request.tools = Arc::new(vec![LLMToolSpec {
            name: "read_file".to_string(),
            description: "read".to_string(),
            parameters: json!({}),
        }]);
        assert!(reject_mixed_with_function_tools("openai_responses", &request).is_err());
    }

    #[test]
    fn search_without_tools_passes_the_invariant() {
        let request = request_with_extra(json!({"server_web_search": true}));
        assert!(reject_mixed_with_function_tools("openai_responses", &request).is_ok());
    }

    #[test]
    fn unsupported_providers_fail_closed_only_when_flagged() {
        let plain = request_with_extra(json!({}));
        assert!(reject_unsupported("deepseek", &plain).is_ok());
        let flagged = request_with_extra(json!({"server_web_search": true}));
        let error = reject_unsupported("deepseek", &flagged).unwrap_err();
        assert!(matches!(error, LLMError::UnsupportedCapability(_)));
    }

    #[test]
    fn openai_tool_maps_neutral_options_to_filters() {
        let request = request_with_extra(json!({
            "server_web_search": {
                "max_uses": 3,
                "allowed_domains": ["example.com"],
                "blocked_domains": ["spam.example"],
                "user_location": {"type": "approximate", "country": "US"}
            }
        }));
        let tool = openai_responses_web_search_tool(&request);
        assert_eq!(tool["type"], "web_search");
        assert_eq!(tool["filters"]["allowed_domains"][0], "example.com");
        assert_eq!(tool["filters"]["blocked_domains"][0], "spam.example");
        assert_eq!(tool["user_location"]["country"], "US");
        // The live Responses API rejects tools[0].max_uses for web_search
        // with `Unknown parameter` — it must never be forwarded.
        assert!(tool.get("max_uses").is_none());
    }

    #[test]
    fn openai_tool_without_options_stays_minimal() {
        let request = request_with_extra(json!({"server_web_search": true}));
        let tool = openai_responses_web_search_tool(&request);
        assert_eq!(tool["type"], "web_search");
        assert!(tool.get("max_uses").is_none());
        assert!(tool.get("filters").is_none());
        assert!(tool.get("user_location").is_none());
    }

    #[test]
    fn anthropic_tool_maps_max_uses_and_domains() {
        let request = request_with_extra(json!({
            "server_web_search": {"max_uses": 3, "allowed_domains": ["docs.example"]}
        }));
        let tool = anthropic_web_search_tool(&request);
        assert_eq!(tool["type"], "web_search_20250305");
        assert_eq!(tool["name"], "web_search");
        assert_eq!(tool["max_uses"], 3);
        assert_eq!(tool["allowed_domains"][0], "docs.example");
    }

    #[test]
    fn openrouter_plugin_maps_max_uses_to_max_results() {
        let request = request_with_extra(json!({"server_web_search": {"max_uses": 5}}));
        let plugin = openrouter_web_plugin(&request);
        assert_eq!(plugin["id"], "web");
        assert_eq!(plugin["max_results"], 5);
    }

    #[test]
    fn openrouter_plugin_clamps_max_results_to_api_range() {
        let request = request_with_extra(json!({"server_web_search": {"max_uses": 50}}));
        let plugin = openrouter_web_plugin(&request);
        assert_eq!(plugin["max_results"], 10);
    }

    #[test]
    fn openai_citations_and_call_count_extract_from_raw_response() {
        let raw = json!({
            "output": [
                {"type": "web_search_call", "id": "ws_1"},
                {"type": "web_search_call", "id": "ws_2"},
                {"type": "message", "content": [
                    {"type": "output_text", "text": "answer", "annotations": [
                        {"type": "url_citation", "url": "https://a.example", "title": "A"},
                        {"type": "url_citation", "url": "https://b.example"}
                    ]}
                ]}
            ]
        });
        assert_eq!(web_search_call_count(&LLMProviderKind::OpenAI, &raw), 2);
        let citations = extract_citations(&LLMProviderKind::OpenAI, &raw);
        assert_eq!(citations.len(), 2);
        assert_eq!(citations[0].url, "https://a.example");
        assert_eq!(citations[0].title.as_deref(), Some("A"));
    }

    #[test]
    fn anthropic_citations_and_call_count_extract_from_raw_response() {
        // Non-adjacent duplicates must deduplicate too: the same URL can be
        // cited by two separate searches separated by other results.
        let raw = json!({
            "content": [
                {"type": "web_search_tool_result", "content": [
                    {"type": "web_search_result", "url": "https://a.example", "title": "A"}
                ]},
                {"type": "text", "text": "middle"},
                {"type": "web_search_tool_result", "content": [
                    {"type": "web_search_result", "url": "https://b.example"},
                    {"type": "web_search_result", "url": "https://a.example", "title": "A"}
                ]},
                {"type": "text", "text": "answer"}
            ]
        });
        assert_eq!(web_search_call_count(&LLMProviderKind::Anthropic, &raw), 2);
        let citations = extract_citations(&LLMProviderKind::Anthropic, &raw);
        assert_eq!(
            citations.len(),
            2,
            "same url across separated searches deduplicates, first-seen order kept"
        );
        assert_eq!(citations[0].url, "https://a.example");
        assert_eq!(citations[1].url, "https://b.example");
    }

    #[test]
    fn gemini_citations_and_call_count_extract_from_raw_response() {
        let raw = json!({
            "candidates": [{
                "groundingMetadata": {
                    "webSearchQueries": ["a", "b"],
                    "groundingChunks": [
                        {"web": {"uri": "https://a.example", "title": "A"}}
                    ]
                }
            }]
        });
        assert_eq!(web_search_call_count(&LLMProviderKind::Gemini, &raw), 2);
        let citations = extract_citations(&LLMProviderKind::Gemini, &raw);
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].title.as_deref(), Some("A"));
    }

    #[test]
    fn gemini_interactions_shape_extracts_grounding_too() {
        let raw = json!({
            "id": "interaction-1",
            "status": "COMPLETED",
            "steps": [
                {"type": "model_output", "content": [{"type": "text", "text": "answer"}]},
                {"response": {"candidates": [{
                    "groundingMetadata": {
                        "webSearchQueries": ["q"],
                        "groundingChunks": [
                            {"web": {"uri": "https://steps.example", "title": "Steps"}}
                        ]
                    }
                }]}}
            ]
        });
        assert_eq!(web_search_call_count(&LLMProviderKind::Gemini, &raw), 1);
        let citations = extract_citations(&LLMProviderKind::Gemini, &raw);
        assert_eq!(citations.len(), 1);
        assert_eq!(citations[0].url, "https://steps.example");
    }

    #[test]
    fn openrouter_count_prefers_billed_request_count_over_annotations() {
        let raw = json!({
            "choices": [{
                "message": {
                    "content": "answer",
                    "annotations": [
                        {"type": "url_citation", "url": "https://a.example"},
                        {"type": "url_citation", "url": "https://a.example#excerpt"},
                        {"type": "url_citation", "url": "https://b.example"}
                    ]
                }
            }],
            "usage": {
                "cost_details": {"web_search_requests_count": 1}
            }
        });
        assert_eq!(
            web_search_call_count(&LLMProviderKind::OpenRouter, &raw),
            1,
            "billed request count wins over excerpt-level annotations"
        );
        assert_eq!(
            extract_citations(&LLMProviderKind::OpenRouter, &raw).len(),
            3
        );
    }

    #[test]
    fn openrouter_citations_and_call_count_extract_from_raw_response() {
        let raw = json!({
            "choices": [{
                "message": {
                    "content": "answer",
                    "annotations": [
                        {"type": "url_citation", "url": "https://a.example", "title": "A"}
                    ]
                }
            }]
        });
        assert_eq!(web_search_call_count(&LLMProviderKind::OpenRouter, &raw), 1);
        let citations = extract_citations(&LLMProviderKind::OpenRouter, &raw);
        assert_eq!(citations.len(), 1);
    }

    #[test]
    fn unsupported_providers_report_zero_and_no_citations() {
        let raw = json!({"anything": true});
        assert_eq!(web_search_call_count(&LLMProviderKind::DeepSeek, &raw), 0);
        assert!(extract_citations(&LLMProviderKind::DeepSeek, &raw).is_empty());
    }
}

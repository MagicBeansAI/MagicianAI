use std::io;

use serde_json::{json, Value};

use crate::types::{
    ContentBlock, LLMRequest, MessageRole, PromptCacheConfig, CACHE_BREAKPOINT_SENTINEL,
};

/// Provider-internal plan for GPT-5.6+ prompt caching.
///
/// OpenAI rejects `prompt_cache_options` and per-block
/// `prompt_cache_breakpoint` on older models, so callers must only apply this
/// plan when `explicit_breakpoint` is true. The default request-wide mode is
/// deliberately `implicit`: OpenAI keeps its automatic latest-message
/// breakpoint while also honoring Magician's stable-prefix marker.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct OpenAIPromptCachePlan {
    pub(crate) explicit_breakpoint: bool,
    pub(crate) prompt_cache_key: Option<String>,
    pub(crate) prompt_cache_options: Option<Value>,
}

pub(crate) fn plan(request: &LLMRequest, model: &str) -> OpenAIPromptCachePlan {
    if request.metadata.single_physical_attempt
        || !model_supports_explicit_prompt_caching(model)
        || matches!(request.prompt_cache, Some(PromptCacheConfig::Disabled))
    {
        return OpenAIPromptCachePlan::default();
    }

    let extra = request.extra.as_deref().and_then(Value::as_object);
    let caller_key = extra
        .and_then(|map| map.get("prompt_cache_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(ToOwned::to_owned);
    let caller_options = extra
        .and_then(|map| map.get("prompt_cache_options"))
        .and_then(validated_options);

    match stable_prefix_digest(request, model) {
        Some(digest) => OpenAIPromptCachePlan {
            explicit_breakpoint: true,
            prompt_cache_key: Some(
                caller_key.unwrap_or_else(|| format!("magician:v1:{}", &digest[..32])),
            ),
            prompt_cache_options: Some(
                caller_options.unwrap_or_else(|| json!({ "mode": "implicit" })),
            ),
        },
        // No usable sentinel means there is nowhere safe to place an explicit
        // breakpoint. The caller's routing key still applies: a continuation
        // turn of a Responses chain sends only its delta — no full prompt, no
        // sentinel — and has to reach the cache the chain's bootstrap warmed.
        // Run 17 (2026-09-21): the bootstrap and every rebootstrap carried a
        // digest key that changed with the history in front of the sentinel,
        // the continuation turns carried none, and each rebootstrap of a
        // ~105K-token prompt was billed at 0% cached with an identical 70K
        // system+tools prefix warm on another key.
        None => match caller_key {
            Some(key) => OpenAIPromptCachePlan {
                explicit_breakpoint: false,
                prompt_cache_key: Some(key),
                prompt_cache_options: caller_options,
            },
            // Preserve OpenAI's pre-existing automatic caching by leaving
            // the request unchanged.
            None => OpenAIPromptCachePlan::default(),
        },
    }
}

/// GPT-5.6 and later GPT model families accept the explicit prompt-cache
/// fields. Keep the check structural so future GPT-5.x minors and GPT-6+
/// inherit support without sending the fields to GPT-5.5 or older models.
fn model_supports_explicit_prompt_caching(model: &str) -> bool {
    let normalized = model
        .trim()
        .to_ascii_lowercase()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    let Some(version) = normalized.strip_prefix("gpt-") else {
        return false;
    };

    let major_digits = version
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    let Ok(major) = major_digits.parse::<u32>() else {
        return false;
    };
    if major > 5 {
        return true;
    }
    if major < 5 {
        return false;
    }

    version
        .strip_prefix(&major_digits)
        .and_then(|rest| rest.strip_prefix('.'))
        .map(|rest| {
            rest.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
        })
        .and_then(|minor| minor.parse::<u32>().ok())
        .map(|minor| minor >= 6)
        .unwrap_or(false)
}

struct StableMessage<'a> {
    role: &'static str,
    blocks: &'a [ContentBlock],
    final_prefix: Option<&'a str>,
}

struct StablePrefix<'a> {
    messages: &'a [StableMessage<'a>],
    model: &'a str,
    operation: &'a str,
    response_format: Option<&'a crate::types::LLMResponseFormat>,
    tools: &'a [crate::types::LLMToolSpec],
}

fn write_scalar<T: serde::Serialize + ?Sized>(
    output: &mut dyn std::io::Write,
    value: &T,
) -> io::Result<()> {
    serde_json::to_writer(&mut *output, value).map_err(io::Error::other)
}

fn write_stable_block(output: &mut dyn std::io::Write, block: &ContentBlock) -> io::Result<()> {
    match block {
        ContentBlock::Text { text } => {
            output.write_all(b"{\"text\":")?;
            write_scalar(output, text)?;
            output.write_all(b",\"type\":\"text\"}")?;
        },
        ContentBlock::Image {
            data,
            media_type,
            caption,
        } => {
            output.write_all(b"{")?;
            if let Some(caption) = caption {
                output.write_all(b"\"caption\":")?;
                write_scalar(output, caption)?;
                output.write_all(b",")?;
            }
            output.write_all(b"\"data\":")?;
            write_scalar(output, data)?;
            output.write_all(b",\"media_type\":")?;
            write_scalar(output, media_type)?;
            output.write_all(b",\"type\":\"image\"}")?;
        },
        ContentBlock::ImageUrl { url, prompt } => {
            output.write_all(b"{")?;
            if let Some(prompt) = prompt {
                output.write_all(b"\"prompt\":")?;
                write_scalar(output, prompt)?;
                output.write_all(b",")?;
            }
            output.write_all(b"\"type\":\"image_url\",\"url\":")?;
            write_scalar(output, url)?;
            output.write_all(b"}")?;
        },
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => {
            output.write_all(b"{\"arguments\":")?;
            crate::context_reuse::write_json(output, arguments)?;
            output.write_all(b",\"id\":")?;
            write_scalar(output, id)?;
            output.write_all(b",\"name\":")?;
            write_scalar(output, name)?;
            output.write_all(b",\"type\":\"tool_call\"}")?;
        },
        ContentBlock::ToolResult {
            tool_call_id,
            content,
        } => {
            output.write_all(b"{\"content\":")?;
            crate::context_reuse::write_json(output, content)?;
            output.write_all(b",\"tool_call_id\":")?;
            write_scalar(output, tool_call_id)?;
            output.write_all(b",\"type\":\"tool_result\"}")?;
        },
        ContentBlock::Json { value } => {
            output.write_all(b"{\"type\":\"json\",\"value\":")?;
            crate::context_reuse::write_json(output, value)?;
            output.write_all(b"}")?;
        },
    }
    Ok(())
}

fn write_prefix_text(output: &mut dyn std::io::Write, text: &str) -> io::Result<()> {
    output.write_all(b"{\"text\":")?;
    write_scalar(output, text)?;
    output.write_all(b",\"type\":\"text\"}")?;
    Ok(())
}

fn write_stable_prefix(
    output: &mut dyn std::io::Write,
    prefix: &StablePrefix<'_>,
) -> io::Result<()> {
    // The legacy implementation built serde_json::Map values, whose default
    // compact encoding sorted every object key lexicographically. Preserve
    // those exact bytes while arbitrary JSON values use heap-owned frames.
    output.write_all(b"{\"messages\":[")?;
    for (message_index, message) in prefix.messages.iter().enumerate() {
        if message_index > 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"{\"content\":[")?;
        for (block_index, block) in message.blocks.iter().enumerate() {
            if block_index > 0 {
                output.write_all(b",")?;
            }
            write_stable_block(output, block)?;
        }
        if let Some(final_prefix) = message.final_prefix {
            if !message.blocks.is_empty() {
                output.write_all(b",")?;
            }
            write_prefix_text(output, final_prefix)?;
        }
        output.write_all(b"],\"role\":")?;
        write_scalar(output, message.role)?;
        output.write_all(b"}")?;
    }
    output.write_all(b"],\"model\":")?;
    write_scalar(output, prefix.model)?;
    output.write_all(b",\"operation\":")?;
    write_scalar(output, prefix.operation)?;
    output.write_all(b",\"response_format\":")?;
    match prefix.response_format {
        None => output.write_all(b"null")?,
        Some(crate::types::LLMResponseFormat::Text) => output.write_all(b"{\"type\":\"text\"}")?,
        Some(crate::types::LLMResponseFormat::JsonObject) => {
            output.write_all(b"{\"type\":\"json_object\"}")?
        },
        Some(crate::types::LLMResponseFormat::JsonSchema { schema }) => {
            output.write_all(b"{\"schema\":")?;
            crate::context_reuse::write_json(output, schema)?;
            output.write_all(b",\"type\":\"json_schema\"}")?;
        },
    }
    output.write_all(b",\"tools\":[")?;
    for (tool_index, tool) in prefix.tools.iter().enumerate() {
        if tool_index > 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"{\"description\":")?;
        write_scalar(output, &tool.description)?;
        output.write_all(b",\"name\":")?;
        write_scalar(output, &tool.name)?;
        output.write_all(b",\"parameters\":")?;
        crate::context_reuse::write_json(output, &tool.parameters)?;
        output.write_all(b"}")?;
    }
    output.write_all(b"]}")?;
    Ok(())
}

/// Stream the exact legacy stable-prefix JSON bytes directly into BLAKE3.
/// This avoids both the cloned `Value` forest and the final request-sized
/// `Vec<u8>` while preserving existing cache keys byte-for-byte.
fn stable_prefix_digest(request: &LLMRequest, model: &str) -> Option<String> {
    let mut stable_messages = Vec::<StableMessage<'_>>::new();

    for message in request.messages.iter() {
        let role = match message.role {
            MessageRole::System => "system",
            MessageRole::User => "user",
            MessageRole::Assistant => "assistant",
            MessageRole::Tool => "tool",
        };
        let mut stable_block_count = 0usize;
        for block in &message.content {
            if matches!(message.role, MessageRole::System | MessageRole::User) {
                if let ContentBlock::Text { text } = block {
                    if let Some(sentinel_start) = text.find(CACHE_BREAKPOINT_SENTINEL) {
                        let sentinel_end = sentinel_start + CACHE_BREAKPOINT_SENTINEL.len();
                        let prefix_end =
                            if sentinel_start > 0 && text.as_bytes()[sentinel_start - 1] == b'\n' {
                                sentinel_start - 1
                            } else {
                                sentinel_start
                            };
                        let suffix_start = if text.as_bytes().get(sentinel_end) == Some(&b'\n') {
                            sentinel_end + 1
                        } else {
                            sentinel_end
                        };
                        let prefix = &text[..prefix_end];
                        let suffix = &text[suffix_start..];
                        if !suffix.is_empty() && !prefix.is_empty() {
                            stable_messages.push(StableMessage {
                                role,
                                blocks: &message.content[..stable_block_count],
                                final_prefix: Some(prefix),
                            });
                            let material = StablePrefix {
                                messages: &stable_messages,
                                model,
                                operation: &request.metadata.operation,
                                response_format: request.response_format_value(),
                                tools: &request.tools,
                            };
                            struct HashWriter<'a>(&'a mut blake3::Hasher);
                            impl std::io::Write for HashWriter<'_> {
                                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                                    self.0.update(bytes);
                                    Ok(bytes.len())
                                }
                                fn flush(&mut self) -> std::io::Result<()> {
                                    Ok(())
                                }
                            }
                            let mut hasher = blake3::Hasher::new();
                            write_stable_prefix(&mut HashWriter(&mut hasher), &material).ok()?;
                            return Some(hasher.finalize().to_hex().to_string());
                        }
                    }
                }
            }
            stable_block_count += 1;
        }

        stable_messages.push(StableMessage {
            role,
            blocks: &message.content,
            final_prefix: None,
        });
    }

    None
}

fn validated_options(value: &Value) -> Option<Value> {
    let object = value.as_object()?;
    let mode = object.get("mode").and_then(Value::as_str)?;
    if !matches!(mode, "implicit" | "explicit") {
        return None;
    }
    if object
        .get("ttl")
        .and_then(Value::as_str)
        .map(|ttl| ttl != "30m")
        .unwrap_or(false)
    {
        return None;
    }
    Some(Value::Object(object.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LLMMessage, LLMToolSpec, RequestMetadata};

    fn request(prefix: &str, suffix: &str) -> LLMRequest {
        LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user(format!(
                "{prefix}\n{CACHE_BREAKPOINT_SENTINEL}\n{suffix}"
            ))]
            .into(),
            tools: vec![LLMToolSpec {
                name: "read".to_string(),
                description: "Read data".to_string(),
                parameters: json!({ "type": "object" }),
            }]
            .into(),
            metadata: RequestMetadata {
                operation: "agentic_decision".to_string(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn legacy_stable_prefix_material(request: &LLMRequest, model: &str) -> Option<Vec<u8>> {
        let mut stable_messages = Vec::new();
        for message in request.messages.iter() {
            let role = match message.role {
                MessageRole::System => "system",
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
                MessageRole::Tool => "tool",
            };
            let mut stable_content = Vec::new();
            for block in &message.content {
                if matches!(message.role, MessageRole::System | MessageRole::User) {
                    if let ContentBlock::Text { text } = block {
                        if text.contains(CACHE_BREAKPOINT_SENTINEL) {
                            let (prefix, suffix) = crate::types::split_on_cache_sentinel(text);
                            if suffix.as_deref().is_some_and(|suffix| !suffix.is_empty())
                                && !prefix.is_empty()
                            {
                                stable_content.push(json!({ "type": "text", "text": prefix }));
                                stable_messages.push(json!({
                                    "role": role,
                                    "content": stable_content,
                                }));
                                return serde_json::to_vec(&json!({
                                    "model": model,
                                    "operation": request.metadata.operation,
                                    "tools": request.tools,
                                    "response_format": request.response_format,
                                    "messages": stable_messages,
                                }))
                                .ok();
                            }
                        }
                    }
                }
                stable_content.push(serde_json::to_value(block).ok()?);
            }
            stable_messages.push(json!({"role": role, "content": stable_content}));
        }
        None
    }

    #[test]
    fn model_gate_accepts_5_6_and_later_only() {
        assert!(model_supports_explicit_prompt_caching("gpt-5.6-terra"));
        assert!(model_supports_explicit_prompt_caching("openai/gpt-5.7"));
        assert!(model_supports_explicit_prompt_caching("gpt-6"));
        assert!(model_supports_explicit_prompt_caching("gpt-6-astra"));
        assert!(!model_supports_explicit_prompt_caching("gpt-5.5"));
        assert!(!model_supports_explicit_prompt_caching("gpt-4.1"));
        assert!(!model_supports_explicit_prompt_caching("claude-sonnet-4-6"));
    }

    #[test]
    fn volatile_suffix_does_not_change_cache_key() {
        let first = plan(&request("stable", "turn one"), "gpt-5.6-terra");
        let second = plan(&request("stable", "turn two"), "gpt-5.6-terra");

        assert!(first.explicit_breakpoint);
        assert_eq!(first.prompt_cache_key, second.prompt_cache_key);
        assert_eq!(
            first.prompt_cache_options,
            Some(json!({ "mode": "implicit" }))
        );
    }

    #[test]
    fn streaming_cache_key_digest_matches_the_legacy_value_materialization() {
        let mut request = request("stable", "turn one");
        request.messages_mut().insert(
            0,
            crate::types::LLMMessage {
                role: MessageRole::System,
                content: vec![
                    ContentBlock::text("system instruction"),
                    ContentBlock::Image {
                        data: vec![0, 1, 255],
                        media_type: "image/png".to_string(),
                        caption: Some("caption".to_string()),
                    },
                    ContentBlock::Image {
                        data: Vec::new(),
                        media_type: "image/jpeg".to_string(),
                        caption: None,
                    },
                    ContentBlock::ImageUrl {
                        url: "https://example.invalid/image.png".to_string(),
                        prompt: None,
                    },
                    ContentBlock::ImageUrl {
                        url: "https://example.invalid/prompted.png".to_string(),
                        prompt: Some("inspect closely".to_string()),
                    },
                    ContentBlock::ToolCall {
                        id: "call-1".to_string(),
                        name: "read".to_string(),
                        arguments: json!({"paths": ["a", "b"]}),
                    },
                    ContentBlock::ToolResult {
                        tool_call_id: "call-1".to_string(),
                        content: json!({"ok": true}),
                    },
                    ContentBlock::Json {
                        value: json!({"nested": [1, false, null]}),
                    },
                ],
            },
        );
        request.tools_mut()[0].parameters =
            json!({"type": "object", "properties": {"path": {"type": "string"}}});
        request.set_response_format(crate::types::LLMResponseFormat::JsonSchema {
            schema: json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
        });
        let legacy =
            legacy_stable_prefix_material(&request, "gpt-5.6-terra").expect("legacy stable prefix");
        let expected = blake3::hash(&legacy).to_hex().to_string();
        let expected_key = format!("magician:v1:{}", &expected[..32]);
        assert_eq!(
            stable_prefix_digest(&request, "gpt-5.6-terra").as_deref(),
            Some(expected.as_str()),
        );
        assert_eq!(
            plan(&request, "gpt-5.6-terra").prompt_cache_key.as_deref(),
            Some(expected_key.as_str()),
        );
    }

    #[test]
    fn scalar_response_format_variants_preserve_legacy_cache_digest_bytes() {
        for response_format in [
            None,
            Some(crate::types::LLMResponseFormat::Text),
            Some(crate::types::LLMResponseFormat::JsonObject),
        ] {
            let mut request = request("stable", "volatile");
            if let Some(response_format) = response_format {
                request.set_response_format(response_format);
            }
            let legacy = legacy_stable_prefix_material(&request, "gpt-5.6-terra")
                .expect("legacy stable prefix");
            let expected = blake3::hash(&legacy).to_hex().to_string();
            assert_eq!(
                stable_prefix_digest(&request, "gpt-5.6-terra").as_deref(),
                Some(expected.as_str()),
            );
        }
    }

    #[test]
    fn prompt_cache_digest_handles_unadmitted_deep_schemas_on_a_small_stack() {
        std::thread::Builder::new()
            .name("prompt-cache-fingerprint-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut schema = Value::Null;
                for _ in 0..10_000 {
                    schema = Value::Array(vec![schema]);
                }
                let mut request = request("stable", "volatile");
                request.set_response_format(crate::types::LLMResponseFormat::JsonSchema { schema });
                let digest = stable_prefix_digest(&request, "gpt-5.6-terra")
                    .expect("sentinel-bearing request has a stable prefix");
                assert_eq!(digest.len(), 64);
                request.discard_json_payloads_iteratively();
            })
            .expect("small-stack prompt-cache fingerprint worker")
            .join()
            .expect("heap-framed prompt-cache digest must not overflow");
    }

    #[test]
    fn stable_prefix_or_tool_change_rotates_cache_key() {
        let base = request("stable", "turn one");
        let changed_prefix = request("different", "turn one");
        let mut changed_tool = base.clone();
        changed_tool.tools_mut()[0].name = "search".to_string();

        assert_ne!(
            plan(&base, "gpt-5.6-terra").prompt_cache_key,
            plan(&changed_prefix, "gpt-5.6-terra").prompt_cache_key
        );
        assert_ne!(
            plan(&base, "gpt-5.6-terra").prompt_cache_key,
            plan(&changed_tool, "gpt-5.6-terra").prompt_cache_key
        );
    }

    #[test]
    fn disabled_older_or_markerless_requests_remain_unchanged() {
        let mut disabled = request("stable", "turn");
        disabled.prompt_cache = Some(PromptCacheConfig::Disabled);

        assert_eq!(
            plan(&disabled, "gpt-5.6-terra"),
            OpenAIPromptCachePlan::default()
        );
        let mut protected = request("stable", "turn");
        protected.metadata.single_physical_attempt = true;
        assert_eq!(
            plan(&protected, "gpt-5.6-terra"),
            OpenAIPromptCachePlan::default()
        );
        assert_eq!(
            plan(&request("stable", "turn"), "gpt-5.5"),
            OpenAIPromptCachePlan::default()
        );
        assert_eq!(
            plan(
                &LLMRequest {
                    model: "gpt-5.6-terra".to_string(),
                    messages: vec![LLMMessage::user("no marker")].into(),
                    ..Default::default()
                },
                "gpt-5.6-terra"
            ),
            OpenAIPromptCachePlan::default()
        );
    }

    #[test]
    fn a_caller_key_routes_a_markerless_continuation_turn_without_a_breakpoint() {
        // A continuation delta: no sentinel anywhere, only the new tool result.
        let mut delta = LLMRequest {
            model: "gpt-5.6-terra".to_string(),
            messages: vec![LLMMessage::user("tool result for the last call")].into(),
            ..Default::default()
        };
        delta.set_extra(json!({ "prompt_cache_key": "magician:chain:abc123" }));
        let delta_plan = plan(&delta, "gpt-5.6-terra");
        assert_eq!(
            delta_plan.prompt_cache_key.as_deref(),
            Some("magician:chain:abc123")
        );
        assert!(
            !delta_plan.explicit_breakpoint,
            "no sentinel, no breakpoint"
        );
        assert_eq!(delta_plan.prompt_cache_options, None);
        // The same key on the full (rebootstrap) prompt keeps the breakpoint.
        let mut full = request("stable", "turn");
        full.set_extra(json!({ "prompt_cache_key": "magician:chain:abc123" }));
        let full_plan = plan(&full, "gpt-5.6-terra");
        assert_eq!(
            full_plan.prompt_cache_key.as_deref(),
            Some("magician:chain:abc123")
        );
        assert!(full_plan.explicit_breakpoint);
    }

    #[test]
    fn valid_caller_key_and_options_override_generated_defaults() {
        let mut request = request("stable", "turn");
        request.set_extra(json!({
            "prompt_cache_key": "tenant:agent:stable-v2",
            "prompt_cache_options": { "mode": "explicit", "ttl": "30m" }
        }));

        let plan = plan(&request, "gpt-5.6-terra");
        assert_eq!(
            plan.prompt_cache_key.as_deref(),
            Some("tenant:agent:stable-v2")
        );
        assert_eq!(
            plan.prompt_cache_options,
            Some(json!({ "mode": "explicit", "ttl": "30m" }))
        );
    }
}

//! Provider-neutral context-reuse planning.
//!
//! Providers do not share one continuation contract.  This module keeps the
//! semantic choice explicit so callers can request the strongest safe mode
//! without teaching the agent loop provider-specific wire details.

use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    capability::LLMProviderKind,
    types::{split_on_cache_sentinel, ContentBlock, LLMMessage, LLMToolSpec, MessageRole},
};

/// Strongest safe context-reuse mode for one physical provider request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextReuseStrategy {
    /// The provider owns prior turns and accepts an opaque continuation id.
    ServerContinuation,
    /// The caller replays history, while the provider reuses an identical
    /// prefix to reduce processing cost and latency.
    PrefixCache,
    /// The provider has no usable server state/cache contract.  The caller
    /// must send a bounded, self-contained replay on every turn.
    BoundedReplay,
}

impl ContextReuseStrategy {
    pub fn is_stateful(self) -> bool {
        matches!(self, Self::ServerContinuation)
    }
}

/// Local request plan consumed by provider adapters.
///
/// This is deliberately typed rather than hidden in `LLMRequest.extra`: it is
/// control-plane state and must never leak as an unknown provider parameter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextReuseConfig {
    pub strategy: ContextReuseStrategy,
    /// Opaque id produced by the immediately preceding response in the same
    /// provider/model/transport cohort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation_id: Option<String>,
    /// Hash of the provider/model/base-URL/API-mode cohort that issued the
    /// continuation id. Routers use it to invalidate state across profile
    /// fallbacks and config reloads; adapters never serialize it upstream.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_cohort_fingerprint: Option<String>,
    /// Exact hash of the reusable prefix (model + tools + stable messages).
    /// Used for observability and invalidation, not authorization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stable_prefix_fingerprint: Option<String>,
    /// Disclosure/authority partition for labeled content. A router clears an
    /// opaque continuation whenever this identity is absent or differs from
    /// the request's current disclosure guard.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disclosure_partition_fingerprint: Option<String>,
    /// Execution-scoped sticky-routing key (not a user or tenant identifier).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    /// Prefer a moving cache boundary over static-only breakpoints for
    /// multi-turn agentic requests.
    #[serde(default)]
    pub rolling_prefix: bool,
}

impl ContextReuseConfig {
    pub fn new(strategy: ContextReuseStrategy) -> Self {
        Self {
            strategy,
            continuation_id: None,
            transport_cohort_fingerprint: None,
            stable_prefix_fingerprint: None,
            disclosure_partition_fingerprint: None,
            session_key: None,
            rolling_prefix: false,
        }
    }
}

/// Hash the transport identity within which an opaque continuation id is
/// valid. This is deliberately separate from the prompt-prefix fingerprint:
/// equal prompts sent to different providers or endpoints do not share state.
pub fn transport_cohort_fingerprint(
    provider: &LLMProviderKind,
    model: &str,
    base_url: Option<&str>,
    metadata: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_len_prefixed(&mut hasher, provider.as_str().as_bytes());
    hash_len_prefixed(&mut hasher, model.as_bytes());
    hash_len_prefixed(
        &mut hasher,
        base_url
            .unwrap_or_default()
            .trim_end_matches('/')
            .as_bytes(),
    );
    let api_mode = match provider {
        LLMProviderKind::OpenAI => "openai_api_mode",
        LLMProviderKind::Gemini => "gemini_api_mode",
        _ => "",
    };
    let api_mode = metadata
        .and_then(|values| values.get(api_mode))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    hash_len_prefixed(&mut hasher, api_mode.as_bytes());
    hasher.finalize().to_hex().to_string()
}

/// Resolve the default strategy for a configured provider transport.
///
/// Stateful modes are opt-in at the transport level.  In particular, Gemini
/// `generateContent` remains prefix-cached; only an explicit `interactions`
/// profile may consume `previous_interaction_id`.
pub fn strategy_for_provider(
    provider: &LLMProviderKind,
    metadata: Option<&std::collections::HashMap<String, serde_json::Value>>,
) -> ContextReuseStrategy {
    let metadata_string = |key: &str| {
        metadata
            .and_then(|values| values.get(key))
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
    };

    match provider {
        LLMProviderKind::OpenAI
            if metadata_string("openai_api_mode")
                .is_some_and(|mode| mode.eq_ignore_ascii_case("responses")) =>
        {
            ContextReuseStrategy::ServerContinuation
        },
        LLMProviderKind::Gemini
            if metadata_string("gemini_api_mode")
                .is_some_and(|mode| mode.eq_ignore_ascii_case("interactions")) =>
        {
            ContextReuseStrategy::ServerContinuation
        },
        // xAI is served only over its Responses API, which keeps the
        // conversation (encrypted reasoning included) server-side for 30 days.
        LLMProviderKind::Xai => ContextReuseStrategy::ServerContinuation,
        LLMProviderKind::OpenAI
        | LLMProviderKind::Anthropic
        | LLMProviderKind::Minimax
        | LLMProviderKind::DeepSeek
        | LLMProviderKind::OpenRouter
        | LLMProviderKind::Gemini
        // Sarvam bills `prompt_tokens_details.cached_tokens` at a discount;
        // a byte-stable prefix is what earns it.
        | LLMProviderKind::Sarvam => ContextReuseStrategy::PrefixCache,
        LLMProviderKind::Ollama | LLMProviderKind::Yutori | LLMProviderKind::Custom(_) => {
            ContextReuseStrategy::BoundedReplay
        },
    }
}

/// Hash the exact stable request prefix without reordering tools or messages.
///
/// Tool order is part of the hash because providers key caches over serialized
/// prefixes.  A catalog reorder must rotate this fingerprint instead of
/// pretending the old cache entry is reusable.
pub fn stable_prefix_fingerprint(
    model: &str,
    messages: &[LLMMessage],
    tools: &[LLMToolSpec],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hash_len_prefixed(&mut hasher, model.as_bytes());
    hash_wire(&mut hasher, |output| write_tools(output, tools));

    for message in messages {
        match message.role {
            MessageRole::System => hash_wire(&mut hasher, |output| write_message(output, message)),
            MessageRole::User => {
                // The first sentinel-bearing user block defines the stable /
                // volatile boundary used by every adapter.  Hash only the
                // stable half, then stop: everything after it is per-turn.
                let mut candidate = hasher.clone();
                let mut found_boundary = false;
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let (prefix, suffix) = split_on_cache_sentinel(text);
                            hash_len_prefixed(&mut candidate, prefix.as_bytes());
                            if suffix.is_some() {
                                found_boundary = true;
                                break;
                            }
                        },
                        ContentBlock::Json { value } => {
                            hash_wire(&mut candidate, |output| write_json(output, value))
                        },
                        // Media and tool transcript blocks are volatile unless
                        // a future provider supplies an explicit stable-media
                        // identity contract.
                        _ => break,
                    }
                }
                if found_boundary {
                    hasher = candidate;
                }
                // A complete user message without a sentinel is a volatile
                // turn; do not include subsequent assistant/tool history.
                break;
            },
            MessageRole::Assistant | MessageRole::Tool => break,
        }
    }

    hasher.finalize().to_hex().to_string()
}

fn hash_wire(
    hasher: &mut blake3::Hasher,
    write: impl Fn(&mut dyn std::io::Write) -> io::Result<()>,
) {
    struct CountWriter(u64);
    impl std::io::Write for CountWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len() as u64);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

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

    // The prefix retains byte-for-byte compatibility with the prior
    // serde_json::to_vec encoding, including its length prefix, without ever
    // materializing a payload-sized duplicate. The supplied writers keep
    // arbitrary JSON traversal on heap-owned frames, so this public helper is
    // safe even when it is called outside the normal queue/router admission
    // path.
    let mut counter = CountWriter(0);
    if write(&mut counter).is_err() {
        hash_len_prefixed(hasher, &[]);
        return;
    }
    hasher.update(&counter.0.to_le_bytes());
    let mut writer = HashWriter(hasher);
    let _ = write(&mut writer);
}

enum JsonWriteFrame<'a> {
    Array {
        remaining: std::slice::Iter<'a, Value>,
        first: bool,
    },
    Object {
        remaining: serde_json::map::Iter<'a>,
        first: bool,
    },
}

/// Match serde_json's compact `Value` encoding while retaining traversal
/// state on the heap instead of the native stack.
pub(crate) fn write_json(output: &mut dyn std::io::Write, root: &Value) -> io::Result<()> {
    let mut frames = Vec::<JsonWriteFrame<'_>>::new();
    let mut current = Some(root);
    loop {
        if let Some(value) = current.take() {
            match value {
                Value::Null => output.write_all(b"null")?,
                Value::Bool(true) => output.write_all(b"true")?,
                Value::Bool(false) => output.write_all(b"false")?,
                Value::Number(number) => output.write_all(number.to_string().as_bytes())?,
                Value::String(text) => {
                    serde_json::to_writer(&mut *output, text).map_err(io::Error::other)?
                },
                Value::Array(values) => {
                    output.write_all(b"[")?;
                    frames.push(JsonWriteFrame::Array {
                        remaining: values.iter(),
                        first: true,
                    });
                },
                Value::Object(values) => {
                    output.write_all(b"{")?;
                    frames.push(JsonWriteFrame::Object {
                        remaining: values.iter(),
                        first: true,
                    });
                },
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return Ok(());
            };
            match frame {
                JsonWriteFrame::Array { remaining, first } => {
                    if let Some(value) = remaining.next() {
                        if !*first {
                            output.write_all(b",")?;
                        }
                        *first = false;
                        current = Some(value);
                        break;
                    }
                    output.write_all(b"]")?;
                    frames.pop();
                },
                JsonWriteFrame::Object { remaining, first } => {
                    if let Some((key, value)) = remaining.next() {
                        if !*first {
                            output.write_all(b",")?;
                        }
                        *first = false;
                        serde_json::to_writer(&mut *output, key).map_err(io::Error::other)?;
                        output.write_all(b":")?;
                        current = Some(value);
                        break;
                    }
                    output.write_all(b"}")?;
                    frames.pop();
                },
            }
        }
    }
}

fn write_tools(output: &mut dyn std::io::Write, tools: &[LLMToolSpec]) -> io::Result<()> {
    output.write_all(b"[")?;
    for (index, tool) in tools.iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        output.write_all(b"{\"name\":")?;
        serde_json::to_writer(&mut *output, &tool.name).map_err(io::Error::other)?;
        output.write_all(b",\"description\":")?;
        serde_json::to_writer(&mut *output, &tool.description).map_err(io::Error::other)?;
        output.write_all(b",\"parameters\":")?;
        write_json(output, &tool.parameters)?;
        output.write_all(b"}")?;
    }
    output.write_all(b"]")?;
    Ok(())
}

fn write_message(output: &mut dyn std::io::Write, message: &LLMMessage) -> io::Result<()> {
    output.write_all(b"{\"role\":")?;
    let role = match message.role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    };
    serde_json::to_writer(&mut *output, role).map_err(io::Error::other)?;
    if message.content.is_empty() {
        output.write_all(b"}")?;
        return Ok(());
    }
    output.write_all(b",\"content\":[")?;
    for (index, block) in message.content.iter().enumerate() {
        if index > 0 {
            output.write_all(b",")?;
        }
        write_content_block(output, block)?;
    }
    output.write_all(b"]}")?;
    Ok(())
}

fn write_content_block(output: &mut dyn std::io::Write, block: &ContentBlock) -> io::Result<()> {
    match block {
        ContentBlock::Text { text } => {
            output.write_all(b"{\"type\":\"text\",\"text\":")?;
            serde_json::to_writer(&mut *output, text).map_err(io::Error::other)?;
        },
        ContentBlock::Image {
            data,
            media_type,
            caption,
        } => {
            output.write_all(b"{\"type\":\"image\",\"data\":")?;
            serde_json::to_writer(&mut *output, data).map_err(io::Error::other)?;
            output.write_all(b",\"media_type\":")?;
            serde_json::to_writer(&mut *output, media_type).map_err(io::Error::other)?;
            if let Some(caption) = caption {
                output.write_all(b",\"caption\":")?;
                serde_json::to_writer(&mut *output, caption).map_err(io::Error::other)?;
            }
        },
        ContentBlock::ImageUrl { url, prompt } => {
            output.write_all(b"{\"type\":\"image_url\",\"url\":")?;
            serde_json::to_writer(&mut *output, url).map_err(io::Error::other)?;
            if let Some(prompt) = prompt {
                output.write_all(b",\"prompt\":")?;
                serde_json::to_writer(&mut *output, prompt).map_err(io::Error::other)?;
            }
        },
        ContentBlock::ToolCall {
            id,
            name,
            arguments,
        } => {
            output.write_all(b"{\"type\":\"tool_call\",\"id\":")?;
            serde_json::to_writer(&mut *output, id).map_err(io::Error::other)?;
            output.write_all(b",\"name\":")?;
            serde_json::to_writer(&mut *output, name).map_err(io::Error::other)?;
            output.write_all(b",\"arguments\":")?;
            write_json(output, arguments)?;
        },
        ContentBlock::ToolResult {
            tool_call_id,
            content,
        } => {
            output.write_all(b"{\"type\":\"tool_result\",\"tool_call_id\":")?;
            serde_json::to_writer(&mut *output, tool_call_id).map_err(io::Error::other)?;
            output.write_all(b",\"content\":")?;
            write_json(output, content)?;
        },
        ContentBlock::Json { value } => {
            output.write_all(b"{\"type\":\"json\",\"value\":")?;
            write_json(output, value)?;
        },
    }
    output.write_all(b"}")?;
    Ok(())
}

fn hash_len_prefixed(hasher: &mut blake3::Hasher, bytes: &[u8]) {
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn legacy_hash_serialized<T: Serialize + ?Sized>(hasher: &mut blake3::Hasher, value: &T) {
        let bytes = serde_json::to_vec(value).unwrap_or_default();
        hash_len_prefixed(hasher, &bytes);
    }

    fn legacy_stable_prefix_fingerprint(
        model: &str,
        messages: &[LLMMessage],
        tools: &[LLMToolSpec],
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hash_len_prefixed(&mut hasher, model.as_bytes());
        legacy_hash_serialized(&mut hasher, tools);
        for message in messages {
            match message.role {
                MessageRole::System => legacy_hash_serialized(&mut hasher, message),
                MessageRole::User => {
                    let mut candidate = hasher.clone();
                    let mut found_boundary = false;
                    for block in &message.content {
                        match block {
                            ContentBlock::Text { text } => {
                                let (prefix, suffix) = split_on_cache_sentinel(text);
                                hash_len_prefixed(&mut candidate, prefix.as_bytes());
                                if suffix.is_some() {
                                    found_boundary = true;
                                    break;
                                }
                            },
                            ContentBlock::Json { value } => {
                                legacy_hash_serialized(&mut candidate, value)
                            },
                            _ => break,
                        }
                    }
                    if found_boundary {
                        hasher = candidate;
                    }
                    break;
                },
                MessageRole::Assistant | MessageRole::Tool => break,
            }
        }
        hasher.finalize().to_hex().to_string()
    }

    fn tool(name: &str) -> LLMToolSpec {
        LLMToolSpec {
            name: name.to_string(),
            description: format!("{name} description"),
            parameters: json!({"type": "object"}),
        }
    }

    #[test]
    fn streaming_fingerprint_is_byte_compatible_with_legacy_serialization() {
        let tools = vec![tool("read"), tool("write")];
        let messages = vec![
            LLMMessage {
                role: MessageRole::System,
                content: Vec::new(),
            },
            LLMMessage {
                role: MessageRole::System,
                content: vec![
                    ContentBlock::text("system"),
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
                        prompt: Some("inspect".to_string()),
                    },
                    ContentBlock::ImageUrl {
                        url: "https://example.invalid/no-prompt.png".to_string(),
                        prompt: None,
                    },
                    ContentBlock::ToolCall {
                        id: "call-1".to_string(),
                        name: "read".to_string(),
                        arguments: json!({"path": ["a", "b"]}),
                    },
                    ContentBlock::ToolResult {
                        tool_call_id: "call-1".to_string(),
                        content: json!({"ok": true}),
                    },
                    ContentBlock::Json {
                        value: json!({"nested": [1, true, "value"]}),
                    },
                ],
            },
            LLMMessage {
                role: MessageRole::User,
                content: vec![ContentBlock::text(format!(
                    "stable{}volatile",
                    crate::types::CACHE_BREAKPOINT_SENTINEL
                ))],
            },
        ];
        assert_eq!(
            stable_prefix_fingerprint("model", &messages, &tools),
            legacy_stable_prefix_fingerprint("model", &messages, &tools),
        );
    }

    #[test]
    fn unadmitted_deep_schema_fingerprint_fits_a_small_stack_without_a_payload_copy() {
        std::thread::Builder::new()
            .name("prefix-fingerprint-small-stack".to_string())
            .stack_size(256 * 1024)
            .spawn(|| {
                let mut schema = Value::Null;
                // The public helper is safe independently of queue/router
                // admission; production requests still fail this shape at
                // the normal request boundary.
                for _ in 0..10_000 {
                    schema = Value::Array(vec![schema]);
                }
                let tools = vec![LLMToolSpec {
                    name: "deep".to_string(),
                    description: String::new(),
                    parameters: schema,
                }];
                let digest = stable_prefix_fingerprint("model", &[], &tools);
                assert_eq!(digest.len(), 64);
                crate::types::discard_json_value_iteratively(
                    tools.into_iter().next().expect("tool").parameters,
                );
            })
            .expect("small-stack fingerprint worker")
            .join()
            .expect("bounded schema fingerprint must not overflow");
    }

    #[test]
    fn strategies_are_transport_specific_and_fail_safe() {
        let responses = serde_json::from_value(json!({"openai_api_mode": "responses"})).unwrap();
        let interactions =
            serde_json::from_value(json!({"gemini_api_mode": "interactions"})).unwrap();
        let uppercase_responses =
            serde_json::from_value(json!({"openai_api_mode": "RESPONSES"})).unwrap();

        assert_eq!(
            strategy_for_provider(&LLMProviderKind::OpenAI, Some(&responses)),
            ContextReuseStrategy::ServerContinuation
        );
        assert_eq!(
            strategy_for_provider(&LLMProviderKind::Gemini, Some(&interactions)),
            ContextReuseStrategy::ServerContinuation
        );
        assert_eq!(
            strategy_for_provider(&LLMProviderKind::OpenAI, Some(&uppercase_responses)),
            ContextReuseStrategy::ServerContinuation
        );
        assert_eq!(
            strategy_for_provider(&LLMProviderKind::Anthropic, None),
            ContextReuseStrategy::PrefixCache
        );
        assert_eq!(
            strategy_for_provider(&LLMProviderKind::Ollama, None),
            ContextReuseStrategy::BoundedReplay
        );
        assert_eq!(
            strategy_for_provider(&LLMProviderKind::Custom("unknown".into()), None),
            ContextReuseStrategy::BoundedReplay
        );
    }

    #[test]
    fn volatile_suffix_does_not_rotate_stable_prefix_fingerprint() {
        let messages = |suffix: &str| {
            vec![
                LLMMessage::system("stable system"),
                LLMMessage::user(format!(
                    "stable task\n{}\n{suffix}",
                    crate::types::CACHE_BREAKPOINT_SENTINEL
                )),
            ]
        };
        let tools = vec![tool("read")];

        assert_eq!(
            stable_prefix_fingerprint("model", &messages("turn one"), &tools),
            stable_prefix_fingerprint("model", &messages("turn two"), &tools)
        );
    }

    #[test]
    fn markerless_user_turn_is_not_misclassified_as_stable_prefix() {
        let tools = vec![tool("read")];
        let messages =
            |turn: &str| vec![LLMMessage::system("stable system"), LLMMessage::user(turn)];

        assert_eq!(
            stable_prefix_fingerprint("model", &messages("turn one"), &tools),
            stable_prefix_fingerprint("model", &messages("turn two"), &tools)
        );
    }

    #[test]
    fn model_tool_order_and_stable_text_rotate_fingerprint() {
        let messages = vec![LLMMessage::system("stable system")];
        let read_write = vec![tool("read"), tool("write")];
        let write_read = vec![tool("write"), tool("read")];

        assert_ne!(
            stable_prefix_fingerprint("model-a", &messages, &read_write),
            stable_prefix_fingerprint("model-b", &messages, &read_write)
        );
        assert_ne!(
            stable_prefix_fingerprint("model-a", &messages, &read_write),
            stable_prefix_fingerprint("model-a", &messages, &write_read)
        );
        assert_ne!(
            stable_prefix_fingerprint(
                "model-a",
                &[LLMMessage::system("changed system")],
                &read_write
            ),
            stable_prefix_fingerprint("model-a", &messages, &read_write)
        );
    }

    #[test]
    fn transport_cohort_rotates_across_provider_model_endpoint_and_mode() {
        let responses = serde_json::from_value(json!({"openai_api_mode": "responses"})).unwrap();
        let chat = serde_json::from_value(json!({"openai_api_mode": "chat"})).unwrap();
        let base = transport_cohort_fingerprint(
            &LLMProviderKind::OpenAI,
            "model-a",
            Some("https://api.example/v1/"),
            Some(&responses),
        );

        assert_eq!(
            base,
            transport_cohort_fingerprint(
                &LLMProviderKind::OpenAI,
                "model-a",
                Some("https://api.example/v1"),
                Some(&responses),
            ),
            "a trailing slash is not a distinct physical endpoint"
        );
        assert_ne!(
            base,
            transport_cohort_fingerprint(
                &LLMProviderKind::Gemini,
                "model-a",
                Some("https://api.example/v1"),
                None,
            )
        );
        assert_ne!(
            base,
            transport_cohort_fingerprint(
                &LLMProviderKind::OpenAI,
                "model-b",
                Some("https://api.example/v1"),
                Some(&responses),
            )
        );
        assert_ne!(
            base,
            transport_cohort_fingerprint(
                &LLMProviderKind::OpenAI,
                "model-a",
                Some("https://proxy.example/v1"),
                Some(&responses),
            )
        );
        assert_ne!(
            base,
            transport_cohort_fingerprint(
                &LLMProviderKind::OpenAI,
                "model-a",
                Some("https://api.example/v1"),
                Some(&chat),
            )
        );
    }
}

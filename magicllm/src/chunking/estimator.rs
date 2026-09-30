use crate::types::{ContentBlock, LLMRequest, LLMResponseFormat, MessageRole};
use serde_json::Value;

#[derive(Default)]
struct JsonLengthWriter {
    bytes: u64,
}

impl std::io::Write for JsonLengthWriter {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        self.bytes = self.bytes.saturating_add(buffer.len() as u64);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

enum JsonLengthFrame<'a> {
    Array {
        remaining: std::slice::Iter<'a, Value>,
        first: bool,
    },
    Object {
        remaining: serde_json::map::Iter<'a>,
        first: bool,
    },
}

/// Exact compact JSON encoded length without allocating a payload-sized
/// String or asking Serde to recurse through the complete tree.
fn json_encoded_len(root: &Value) -> u64 {
    let mut output = JsonLengthWriter::default();
    let mut frames = Vec::<JsonLengthFrame<'_>>::new();
    let mut current = Some(root);
    loop {
        if let Some(value) = current.take() {
            match value {
                Value::Null => output.bytes = output.bytes.saturating_add(4),
                Value::Bool(value) => {
                    output.bytes = output.bytes.saturating_add(if *value { 4 } else { 5 });
                },
                Value::Number(number) => {
                    serde_json::to_writer(&mut output, number)
                        .expect("JSON length writer is infallible");
                },
                Value::String(text) => {
                    serde_json::to_writer(&mut output, text)
                        .expect("JSON length writer is infallible");
                },
                Value::Array(values) => {
                    output.bytes = output.bytes.saturating_add(1);
                    frames.push(JsonLengthFrame::Array {
                        remaining: values.iter(),
                        first: true,
                    });
                },
                Value::Object(values) => {
                    output.bytes = output.bytes.saturating_add(1);
                    frames.push(JsonLengthFrame::Object {
                        remaining: values.iter(),
                        first: true,
                    });
                },
            }
        }

        loop {
            let Some(frame) = frames.last_mut() else {
                return output.bytes;
            };
            match frame {
                JsonLengthFrame::Array { remaining, first } => {
                    if let Some(value) = remaining.next() {
                        if !*first {
                            output.bytes = output.bytes.saturating_add(1);
                        }
                        *first = false;
                        current = Some(value);
                        break;
                    }
                    output.bytes = output.bytes.saturating_add(1);
                    frames.pop();
                },
                JsonLengthFrame::Object { remaining, first } => {
                    if let Some((key, value)) = remaining.next() {
                        if !*first {
                            output.bytes = output.bytes.saturating_add(1);
                        }
                        *first = false;
                        serde_json::to_writer(&mut output, key)
                            .expect("JSON length writer is infallible");
                        output.bytes = output.bytes.saturating_add(1);
                        current = Some(value);
                        break;
                    }
                    output.bytes = output.bytes.saturating_add(1);
                    frames.pop();
                },
            }
        }
    }
}

/// A deterministic estimate of provider input size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TokenEstimate {
    pub estimated_tokens: u32,
    pub source_bytes: u64,
    pub estimator: &'static str,
}

/// Provider/model-aware token estimation boundary.
///
/// Domain adapters depend on this interface rather than a particular model
/// tokenizer so a GGUF-aware implementation can replace the conservative
/// estimator later without changing split/merge semantics.
pub trait TokenEstimator: Send + Sync {
    fn id(&self) -> &'static str;
    fn estimate_text(&self, model: &str, text: &str) -> u32;
    fn estimate_request(&self, model: &str, request: &LLMRequest) -> TokenEstimate;
}

/// Conservative estimator for Ollama generation requests.
///
/// Ollama's generate endpoint does not expose a universal tokenizer before
/// dispatch. One token per two UTF-8 bytes intentionally overestimates normal
/// English, code, compact JSON, and most Unicode text. The separate profile
/// safety margin protects the remaining tokenizer/model variance.
#[derive(Debug, Clone, Copy, Default)]
pub struct ConservativeOllamaEstimator;

impl ConservativeOllamaEstimator {
    const BYTES_PER_ESTIMATED_TOKEN: u64 = 2;

    fn tokens_for_bytes(bytes: u64) -> u32 {
        let tokens = bytes.saturating_add(Self::BYTES_PER_ESTIMATED_TOKEN - 1)
            / Self::BYTES_PER_ESTIMATED_TOKEN;
        u32::try_from(tokens).unwrap_or(u32::MAX)
    }

    /// Estimate an already-serialized payload without requiring callers to
    /// allocate or decode a UTF-8 `String` first.
    ///
    /// This is also the shared conservative fallback for bounded structured
    /// payloads outside an Ollama request. Keeping the byte calculation here
    /// prevents those call sites from silently drifting to a less
    /// conservative ratio than request preflight uses.
    pub fn estimate_serialized_bytes(&self, bytes: &[u8]) -> u32 {
        Self::tokens_for_bytes(bytes.len() as u64)
    }

    fn role_marker(role: MessageRole) -> &'static str {
        match role {
            MessageRole::System => "[system] ",
            MessageRole::User => "[user] ",
            MessageRole::Assistant => "[assistant] ",
            MessageRole::Tool => "[tool] ",
        }
    }

    fn content_bytes(block: &ContentBlock) -> u64 {
        match block {
            ContentBlock::Text { text } => text.len() as u64,
            ContentBlock::Image {
                data,
                media_type,
                caption,
            } => {
                data.len() as u64
                    + media_type.len() as u64
                    + caption.as_deref().map(str::len).unwrap_or(0) as u64
            },
            ContentBlock::ImageUrl { url, prompt } => {
                url.len() as u64 + prompt.as_deref().map(str::len).unwrap_or(0) as u64
            },
            ContentBlock::ToolCall {
                id,
                name,
                arguments,
            } => id.len() as u64 + name.len() as u64 + json_encoded_len(arguments),
            ContentBlock::ToolResult {
                tool_call_id,
                content,
            } => tool_call_id.len() as u64 + json_encoded_len(content),
            ContentBlock::Json { value } => json_encoded_len(value),
        }
    }
}

impl TokenEstimator for ConservativeOllamaEstimator {
    fn id(&self) -> &'static str {
        "ollama_utf8_bytes_div_2_v1"
    }

    fn estimate_text(&self, _model: &str, text: &str) -> u32 {
        self.estimate_serialized_bytes(text.as_bytes())
    }

    fn estimate_request(&self, _model: &str, request: &LLMRequest) -> TokenEstimate {
        let mut source_bytes = 0u64;

        if request.messages.is_empty() {
            source_bytes = request
                .extra
                .as_deref()
                .and_then(|extra| extra.get("prompt"))
                .and_then(serde_json::Value::as_str)
                .map(|prompt| prompt.len() as u64)
                .unwrap_or(0);
        } else {
            for (message_index, message) in request.messages.iter().enumerate() {
                for block in &message.content {
                    source_bytes = source_bytes
                        .saturating_add(Self::role_marker(message.role).len() as u64)
                        .saturating_add(Self::content_bytes(block));
                    if message_index + 1 < request.messages.len() {
                        source_bytes = source_bytes.saturating_add(1);
                    }
                }
            }
        }

        if let Some(format) = request.response_format_value() {
            source_bytes = source_bytes.saturating_add(match format {
                LLMResponseFormat::Text => 0,
                LLMResponseFormat::JsonObject => 4,
                LLMResponseFormat::JsonSchema { schema } => json_encoded_len(schema),
            });
        }

        for tool in request.tools.iter() {
            source_bytes = source_bytes
                .saturating_add(tool.name.len() as u64)
                .saturating_add(tool.description.len() as u64)
                .saturating_add(json_encoded_len(&tool.parameters));
        }

        TokenEstimate {
            estimated_tokens: Self::tokens_for_bytes(source_bytes),
            source_bytes,
            estimator: self.id(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::LLMMessage;
    use serde_json::json;

    fn deeply_nested_array(depth: usize) -> serde_json::Value {
        let mut value = serde_json::Value::Null;
        for _ in 0..depth {
            value = serde_json::Value::Array(vec![value]);
        }
        value
    }

    fn discard_json_iteratively(root: serde_json::Value) {
        enum Frame {
            Array(std::vec::IntoIter<serde_json::Value>),
            Object(serde_json::map::IntoIter),
        }

        let mut frames = Vec::<Frame>::new();
        let mut current = Some(root);
        loop {
            if let Some(value) = current.take() {
                match value {
                    serde_json::Value::Array(values) => {
                        frames.push(Frame::Array(values.into_iter()))
                    },
                    serde_json::Value::Object(values) => {
                        frames.push(Frame::Object(values.into_iter()))
                    },
                    _ => {},
                }
            }
            loop {
                let Some(frame) = frames.last_mut() else {
                    return;
                };
                let next = match frame {
                    Frame::Array(values) => values.next(),
                    Frame::Object(values) => values.next().map(|(_, value)| value),
                };
                if let Some(value) = next {
                    current = Some(value);
                    break;
                }
                frames.pop();
            }
        }
    }

    #[test]
    fn estimator_is_deterministic_across_representative_text_shapes() {
        let estimator = ConservativeOllamaEstimator;
        for input in [
            "ordinary prose with punctuation and several words",
            "fn main() { println!(\"hello\"); }",
            r#"{"compact":[1,2,3],"enabled":true}"#,
            "नमस्ते 世界 👋🏽",
            "fixture_01HZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZZ",
        ] {
            let first = estimator.estimate_text("gemma4:12b", input);
            assert_eq!(first, estimator.estimate_text("gemma4:12b", input));
            assert!(first > 0);
        }
    }

    #[test]
    fn request_estimate_includes_roles_and_json_schema() {
        let request = LLMRequest {
            messages: vec![
                LLMMessage::system("extract facts"),
                LLMMessage::user("{\"source_id\":\"episode-1\"}"),
            ]
            .into(),
            response_format: Some(
                LLMResponseFormat::JsonSchema {
                    schema: json!({"type": "object", "required": ["facts"]}),
                }
                .into(),
            ),
            ..Default::default()
        };
        let estimate = ConservativeOllamaEstimator.estimate_request("gemma4:12b", &request);
        assert!(estimate.source_bytes > "extract facts".len() as u64);
        assert_eq!(estimate.estimator, "ollama_utf8_bytes_div_2_v1");
    }

    #[test]
    fn serialized_byte_estimate_matches_text_estimate_at_boundary() {
        let serialized = "x".repeat(501);
        let estimator = ConservativeOllamaEstimator;
        assert_eq!(
            estimator.estimate_serialized_bytes(serialized.as_bytes()),
            251
        );
        assert_eq!(
            estimator.estimate_serialized_bytes(serialized.as_bytes()),
            estimator.estimate_text("provider-neutral", &serialized)
        );
    }

    #[test]
    fn iterative_json_length_matches_compact_serde_encoding_exactly() {
        let value = json!({
            "escaped": "quote=\" slash=\\ newline=\n control=\u{0001}",
            "unicode": "नमस्ते 世界 👋🏽",
            "numbers": [-42, 0, 1.25e10],
            "nested": [true, false, null, {"key": "value"}],
        });
        assert_eq!(
            json_encoded_len(&value),
            serde_json::to_vec(&value).expect("compact JSON").len() as u64
        );
    }

    #[test]
    fn iterative_json_length_handles_ten_thousand_levels_without_native_recursion() {
        let depth = 10_000;
        let value = deeply_nested_array(depth);
        assert_eq!(json_encoded_len(&value), (depth as u64) * 2 + 4);
        discard_json_iteratively(value);
    }

    #[test]
    fn iterative_json_length_handles_wide_payload_without_serialized_copy() {
        let entries = 100_000_u64;
        let value = serde_json::Value::Array(vec![
            serde_json::Value::String("x".to_string());
            entries as usize
        ]);
        // Each encoded string is three bytes, with one comma between entries
        // and one bracket on each side.
        assert_eq!(json_encoded_len(&value), entries * 3 + (entries - 1) + 2);
        discard_json_iteratively(value);
    }
}

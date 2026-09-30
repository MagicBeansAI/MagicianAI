pub mod anthropic_messages;
pub mod deepseek;
pub mod gemini;
pub mod harness_cli;
pub mod minimax;
pub mod ollama;
pub mod openai_chat;
pub mod openai_meta;
mod openai_prompt_cache;
pub mod openai_responses;
pub mod openrouter;
pub mod sarvam;
pub mod xai;
pub mod yutori_n1;

pub use anthropic_messages::AnthropicMessagesProvider;
pub use deepseek::DeepSeekProvider;
pub use gemini::GeminiProvider;
pub use harness_cli::{HarnessCliKind, HarnessCliProvider, HARNESS_CLI_KINDS};
pub use minimax::MinimaxProvider;
pub use ollama::OllamaProvider;
pub use openai_chat::{OpenAIChatProvider, DEFAULT_BASE_URL_CHAT};
pub use openai_meta::OpenAIMetaProvider;
pub use openai_responses::{OpenAIResponsesProvider, ResponsesDialect, DEFAULT_BASE_URL_RESPONSES};
pub use openrouter::OpenRouterProvider;
pub use sarvam::{SarvamProvider, DEFAULT_BASE_URL_SARVAM_CHAT};
pub use xai::{XaiProvider, DEFAULT_BASE_URL_XAI_RESPONSES};
pub use yutori_n1::YutoriN1Provider;

/// Convert a provider-owned usage counter without allowing a wide JSON
/// integer to wrap the canonical `u32` accounting representation.
pub(crate) fn bounded_usage_counter(
    provider: &'static str,
    field: &'static str,
    value: Option<u64>,
) -> crate::error::LLMResult<Option<u32>> {
    value
        .map(|value| {
            u32::try_from(value).map_err(|_| crate::error::LLMError::Provider {
                provider: provider.to_owned(),
                message: format!("usage counter `{field}` exceeds the supported range"),
            })
        })
        .transpose()
}

/// Sum already-bounded provider counters without saturating or wrapping.
pub(crate) fn checked_usage_sum(
    provider: &'static str,
    field: &'static str,
    values: &[u32],
) -> crate::error::LLMResult<u32> {
    values.iter().try_fold(0u32, |total, value| {
        total
            .checked_add(*value)
            .ok_or_else(|| crate::error::LLMError::Provider {
                provider: provider.to_owned(),
                message: format!("usage counter `{field}` exceeds the supported range"),
            })
    })
}

pub(crate) fn default_http_client() -> reqwest::Client {
    // Provider endpoint identity is resolved and attested before a protected
    // request reaches this transport. Following a redirect would move the
    // request to an origin that was never admitted (a 307/308 also preserves
    // the protected POST body), so every built-in provider uses a no-redirect
    // client. Provider APIs must expose their final physical URL in config;
    // an HTTP redirect is an endpoint change, not a retry.
    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());

    if disable_system_proxy() {
        builder = builder.no_proxy();
    }

    builder.build().expect("failed to build LLM HTTP client")
}

/// Consume a non-streaming provider body through one bounded allocation.
/// `Response::text()` buffers without a ceiling; checking after that call is
/// too late to prevent a hostile/misconfigured endpoint from exhausting the
/// process before JSON admission runs.
pub(crate) async fn read_bounded_response_text(
    response: reqwest::Response,
) -> crate::error::LLMResult<String> {
    use futures_util::StreamExt;

    let max_bytes = crate::types::MAX_PROVIDER_RESPONSE_JSON_BYTES;
    if response
        .content_length()
        .is_some_and(|length| length > max_bytes as u64)
    {
        return Err(crate::error::LLMError::Validation(format!(
            "provider response Content-Length exceeds the admitted {max_bytes}-byte ceiling"
        )));
    }

    let mut body = Vec::with_capacity(
        response
            .content_length()
            .unwrap_or_default()
            .min(max_bytes as u64) as usize,
    );
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| {
            if error.is_timeout() {
                crate::error::LLMError::Timeout
            } else {
                crate::error::LLMError::Transport(error.to_string())
            }
        })?;
        if body.len().saturating_add(chunk.len()) > max_bytes {
            return Err(crate::error::LLMError::Validation(format!(
                "provider response exceeds the admitted {max_bytes}-byte ceiling"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    String::from_utf8(body).map_err(|error| {
        crate::error::LLMError::Transport(format!("provider response was not UTF-8: {error}"))
    })
}

pub(crate) const MAX_PROVIDER_SSE_EVENT_BYTES: usize = 8 * 1024 * 1024;
pub(crate) const MAX_PROVIDER_SSE_STREAM_BYTES: usize =
    crate::types::MAX_PROVIDER_RESPONSE_JSON_BYTES;

/// Return the next complete SSE `data` payload without moving the unconsumed
/// buffer. The caller advances a cursor for all events in the received chunk
/// and drains that prefix once. Single-line data stays borrowed; only a true
/// multiline SSE payload allocates for the spec-required newline join.
pub(crate) fn next_sse_data<'a>(
    buffer: &'a str,
    cursor: &mut usize,
) -> Option<std::borrow::Cow<'a, str>> {
    while *cursor < buffer.len() {
        let remainder = &buffer[*cursor..];
        let (relative_end, delimiter_len) =
            match (remainder.find("\n\n"), remainder.find("\r\n\r\n")) {
                (Some(lf), Some(crlf)) if crlf < lf => (crlf, 4),
                (Some(lf), _) => (lf, 2),
                (None, Some(crlf)) => (crlf, 4),
                (None, None) => return None,
            };
        let event_start = *cursor;
        let event_end = event_start + relative_end;
        *cursor = event_end + delimiter_len;
        let event = &buffer[event_start..event_end];

        let mut first: Option<&str> = None;
        let mut joined = None::<String>;
        for line in event.lines() {
            let Some(rest) = line.strip_prefix("data:") else {
                continue;
            };
            let data = rest.trim();
            if data.is_empty() {
                continue;
            }
            if let Some(joined) = joined.as_mut() {
                joined.push('\n');
                joined.push_str(data);
            } else if let Some(existing) = first.take() {
                let mut value = String::with_capacity(existing.len() + 1 + data.len());
                value.push_str(existing);
                value.push('\n');
                value.push_str(data);
                joined = Some(value);
            } else {
                first = Some(data);
            }
        }
        if let Some(joined) = joined {
            return Some(std::borrow::Cow::Owned(joined));
        }
        if let Some(first) = first {
            return Some(std::borrow::Cow::Borrowed(first));
        }
    }
    None
}

/// Append a network chunk without corrupting a valid UTF-8 scalar split at a
/// chunk boundary. Only the at-most-three-byte incomplete suffix is retained;
/// malformed UTF-8 fails closed instead of entering the JSON/SSE parser as a
/// replacement character.
pub(crate) fn append_sse_utf8_chunk(
    buffer: &mut String,
    carry: &mut Vec<u8>,
    chunk: &[u8],
) -> crate::error::LLMResult<()> {
    if carry.is_empty() {
        append_utf8_bytes(buffer, carry, chunk)
    } else {
        let prefix_len = chunk.len().min(4usize.saturating_sub(carry.len()));
        carry.extend_from_slice(&chunk[..prefix_len]);
        match std::str::from_utf8(carry) {
            Ok(text) => buffer.push_str(text),
            Err(error) if error.error_len().is_none() && prefix_len == chunk.len() => return Ok(()),
            Err(_) => {
                return Err(crate::error::LLMError::Validation(
                    "provider SSE stream contains invalid UTF-8".to_string(),
                ))
            },
        }
        carry.clear();
        append_utf8_bytes(buffer, carry, &chunk[prefix_len..])
    }
}

fn append_utf8_bytes(
    buffer: &mut String,
    carry: &mut Vec<u8>,
    bytes: &[u8],
) -> crate::error::LLMResult<()> {
    match std::str::from_utf8(bytes) {
        Ok(text) => {
            buffer.push_str(text);
            Ok(())
        },
        Err(error) if error.error_len().is_none() => {
            let valid_up_to = error.valid_up_to();
            if valid_up_to > 0 {
                buffer.push_str(
                    std::str::from_utf8(&bytes[..valid_up_to])
                        .expect("Utf8Error::valid_up_to proves a valid prefix"),
                );
            }
            let suffix = &bytes[valid_up_to..];
            if suffix.len() > 3 {
                return Err(crate::error::LLMError::Validation(
                    "provider SSE chunk ended with an invalid UTF-8 suffix".to_string(),
                ));
            }
            carry.extend_from_slice(suffix);
            Ok(())
        },
        Err(_) => Err(crate::error::LLMError::Validation(
            "provider SSE stream contains invalid UTF-8".to_string(),
        )),
    }
}

pub(crate) fn finish_sse_utf8(carry: &[u8]) -> crate::error::LLMResult<()> {
    if carry.is_empty() {
        Ok(())
    } else {
        Err(crate::error::LLMError::Validation(
            "provider SSE stream ended inside a UTF-8 scalar".to_string(),
        ))
    }
}

/// Allocation-free admission state checked before provider chunks are copied
/// into each adapter's UTF-8/SSE accumulator.
pub(crate) struct SseBodyAdmission {
    total_bytes: usize,
    current_event_bytes: usize,
    previous_was_newline: bool,
    max_event_bytes: usize,
    max_stream_bytes: usize,
}

impl SseBodyAdmission {
    pub(crate) fn new() -> Self {
        Self {
            total_bytes: 0,
            current_event_bytes: 0,
            previous_was_newline: false,
            max_event_bytes: MAX_PROVIDER_SSE_EVENT_BYTES,
            max_stream_bytes: MAX_PROVIDER_SSE_STREAM_BYTES,
        }
    }

    #[cfg(test)]
    fn with_limits(max_event_bytes: usize, max_stream_bytes: usize) -> Self {
        Self {
            total_bytes: 0,
            current_event_bytes: 0,
            previous_was_newline: false,
            max_event_bytes,
            max_stream_bytes,
        }
    }

    pub(crate) fn admit_chunk(&mut self, chunk: &[u8]) -> crate::error::LLMResult<()> {
        self.total_bytes = self.total_bytes.saturating_add(chunk.len());
        if self.total_bytes > self.max_stream_bytes {
            return Err(crate::error::LLMError::Validation(format!(
                "provider SSE stream exceeds the admitted {}-byte aggregate ceiling",
                self.max_stream_bytes
            )));
        }
        for byte in chunk {
            self.current_event_bytes = self.current_event_bytes.saturating_add(1);
            if self.current_event_bytes > self.max_event_bytes {
                return Err(crate::error::LLMError::Validation(format!(
                    "provider SSE event exceeds the admitted {}-byte ceiling",
                    self.max_event_bytes
                )));
            }
            if *byte == b'\n' {
                if self.previous_was_newline {
                    self.current_event_bytes = 0;
                }
                self.previous_was_newline = true;
            } else if *byte != b'\r' {
                self.previous_was_newline = false;
            }
        }
        Ok(())
    }
}

pub(crate) fn openai_model_basename(model: &str) -> &str {
    model.trim().rsplit('/').next().unwrap_or(model)
}

pub(crate) fn openai_gpt_major(model: &str) -> Option<u32> {
    let normalized = openai_model_basename(model).to_ascii_lowercase();
    let version = normalized.strip_prefix("gpt-")?;
    let major_digits = version
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    major_digits.parse().ok()
}

pub(crate) fn openai_gpt_family_from_5(model: &str) -> bool {
    openai_gpt_major(model).is_some_and(|major| major >= 5)
}

/// The original GPT-5 chat ids (`gpt-5`, `gpt-5-mini`, `gpt-5-nano`), which
/// still accept `temperature` and `top_p`.
///
/// GPT-5.1 and later, GPT-6, and `gpt-5-pro` do not: they always reason, and
/// the Chat Completions API rejects sampling controls for them. Plain GPT-5
/// predates that split, so a family-wide "major >= 5" test is too wide to
/// decide sampling — it drops `temperature` for a model that accepts it.
pub(crate) fn openai_legacy_gpt5_chat_id(model: &str) -> bool {
    let basename = openai_model_basename(model).to_ascii_lowercase();
    // A minor version means 5.1+, which is the reasoning generation.
    if basename.starts_with("gpt-5.") {
        return false;
    }
    // Pro always reasons, so it belongs with 5.1+ rather than with plain 5.
    if basename.starts_with("gpt-5-pro") {
        return false;
    }
    basename == "gpt-5" || basename.starts_with("gpt-5-")
}

pub(crate) fn openai_model_supports_reasoning_none(model: &str) -> bool {
    let model = openai_model_basename(model).to_ascii_lowercase();
    if model.starts_with("gpt-5-pro") {
        return false;
    }

    // GPT-6 split the family contract: Astra and GPT-6.1 Sol always reason,
    // while GPT-6 Sol and Luna accept `none`. Keep unknown variants fail-closed instead of
    // assuming that a future flagship accepts a latency-oriented setting.
    if openai_gpt_major(&model).is_some_and(|major| major >= 6) {
        return model.starts_with("gpt-6-sol") || model.starts_with("gpt-6-luna");
    }

    let Some(rest) = model.strip_prefix("gpt-5.") else {
        return false;
    };
    let minor_version = rest
        .chars()
        .take_while(|ch| ch.is_ascii_digit())
        .collect::<String>();
    minor_version
        .parse::<u32>()
        .map(|minor| minor >= 1)
        .unwrap_or(false)
}

fn disable_system_proxy() -> bool {
    if cfg!(test) {
        return true;
    }

    std::env::var("MAGICIAN_DISABLE_SYSTEM_PROXY")
        .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_6_family_preserves_per_tier_reasoning_none_support() {
        assert_eq!(openai_gpt_major("gpt-6.1-sol"), Some(6));
        assert!(!openai_model_supports_reasoning_none("gpt-6.1-sol"));
        assert!(!openai_model_supports_reasoning_none(
            "openai/gpt-6.1-sol-2026-09-29"
        ));
        assert!(!openai_legacy_gpt5_chat_id("gpt-6.1-sol"));
        assert_eq!(openai_gpt_major("gpt-6-astra"), Some(6));
        assert_eq!(openai_gpt_major("openai/gpt-6-astra"), Some(6));
        assert!(openai_gpt_family_from_5("gpt-6-astra"));
        assert!(openai_gpt_family_from_5("gpt-5.6-sol"));
        assert!(!openai_gpt_family_from_5("gpt-4o"));
    }

    #[test]
    fn only_the_original_gpt5_chat_ids_still_take_sampling_controls() {
        // These accept `temperature`/`top_p`.
        assert!(openai_legacy_gpt5_chat_id("gpt-5"));
        assert!(openai_legacy_gpt5_chat_id("gpt-5-mini"));
        assert!(openai_legacy_gpt5_chat_id("gpt-5-nano"));
        assert!(openai_legacy_gpt5_chat_id("openai/gpt-5"));
        // These always reason and reject them.
        assert!(!openai_legacy_gpt5_chat_id("gpt-5-pro"));
        assert!(!openai_legacy_gpt5_chat_id("gpt-5.1"));
        assert!(!openai_legacy_gpt5_chat_id("gpt-5.6-terra"));
        assert!(!openai_legacy_gpt5_chat_id("gpt-6-astra"));
        // Not the GPT-5 family at all.
        assert!(!openai_legacy_gpt5_chat_id("gpt-4o"));
        assert!(!openai_model_supports_reasoning_none("gpt-6-astra"));
        assert!(openai_model_supports_reasoning_none("gpt-6-sol"));
        assert!(openai_model_supports_reasoning_none("openai/gpt-6-luna"));
        assert!(!openai_model_supports_reasoning_none("gpt-6-unknown"));
        assert!(openai_model_supports_reasoning_none("gpt-5.6-terra"));
    }

    #[tokio::test]
    async fn default_provider_client_never_forwards_a_redirected_request() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let target = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirect target listener");
        let target_address = target.local_addr().expect("redirect target address");
        let target_task = tokio::spawn(async move {
            let accepted =
                tokio::time::timeout(std::time::Duration::from_millis(250), target.accept()).await;
            let Ok(Ok((mut socket, _))) = accepted else {
                return false;
            };
            let mut request = [0_u8; 2_048];
            let _ = socket.read(&mut request).await;
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await
                .expect("redirect target response");
            true
        });
        let source = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("redirect source listener");
        let source_address = source.local_addr().expect("redirect source address");
        let source_task = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.expect("source request");
            let mut request = Vec::with_capacity(2_048);
            while request.len() < 4_096
                && !request
                    .windows(b"protected-redirect-canary".len())
                    .any(|window| window == b"protected-redirect-canary")
            {
                let mut chunk = [0_u8; 512];
                let read = socket.read(&mut chunk).await.expect("source request bytes");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            assert!(request
                .windows(b"protected-redirect-canary".len())
                .any(|window| window == b"protected-redirect-canary"));
            socket
                .write_all(
                    format!(
                        "HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_address}/collect\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .as_bytes(),
                )
                .await
                .expect("redirect response");
        });

        let response = default_http_client()
            .post(format!("http://{source_address}/provider"))
            .body("protected-redirect-canary")
            .send()
            .await
            .expect("redirect response remains observable");
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
        source_task.await.expect("source task");
        assert!(
            !target_task.await.expect("redirect target task"),
            "the unattested redirect target must receive no connection or body",
        );
    }

    #[test]
    fn sse_cursor_processes_many_events_then_moves_the_buffer_once() {
        let mut buffer = String::new();
        for index in 0..10_000 {
            buffer.push_str(&format!("data: {{\"index\":{index}}}\n\n"));
        }
        buffer.push_str("data: partial");
        let original_capacity = buffer.capacity();
        let mut cursor = 0;
        let mut seen = 0;
        while let Some(data) = next_sse_data(&buffer, &mut cursor) {
            assert!(matches!(data, std::borrow::Cow::Borrowed(_)));
            seen += 1;
        }
        assert_eq!(seen, 10_000);
        buffer.drain(..cursor);
        assert_eq!(buffer, "data: partial");
        assert_eq!(buffer.capacity(), original_capacity);
    }

    #[test]
    fn sse_cursor_joins_true_multiline_data_and_accepts_crlf() {
        let buffer = "event: message\r\ndata: {\"a\":\r\ndata: 1}\r\n\r\n";
        let mut cursor = 0;
        let data = next_sse_data(buffer, &mut cursor).expect("complete CRLF event");
        assert!(matches!(data, std::borrow::Cow::Owned(_)));
        assert_eq!(data, "{\"a\":\n1}");
        assert_eq!(cursor, buffer.len());
    }

    #[test]
    fn sse_utf8_decoder_preserves_a_scalar_split_across_chunks() {
        let encoded = "data: {\"text\":\"€\"}\r\n\r\n".as_bytes();
        let split = encoded
            .windows(3)
            .position(|window| window == "€".as_bytes())
            .expect("multibyte scalar");
        let mut buffer = String::new();
        let mut carry = Vec::with_capacity(4);
        append_sse_utf8_chunk(&mut buffer, &mut carry, &encoded[..split + 1])
            .expect("incomplete scalar is retained");
        append_sse_utf8_chunk(&mut buffer, &mut carry, &encoded[split + 1..])
            .expect("later chunk completes scalar");
        finish_sse_utf8(&carry).expect("no terminal suffix");
        let mut cursor = 0;
        assert_eq!(
            next_sse_data(&buffer, &mut cursor).as_deref(),
            Some("{\"text\":\"€\"}"),
        );
    }

    #[test]
    fn one_byte_chunks_preserve_multibyte_crlf_and_multiline_sse() {
        let encoded = "data: {\"text\":\"€\",\r\ndata: \"ok\":true}\r\n\r\n".as_bytes();
        let mut admission = SseBodyAdmission::with_limits(encoded.len(), encoded.len());
        let mut buffer = String::new();
        let mut carry = Vec::with_capacity(4);
        for byte in encoded {
            admission
                .admit_chunk(std::slice::from_ref(byte))
                .expect("byte cap");
            append_sse_utf8_chunk(&mut buffer, &mut carry, std::slice::from_ref(byte))
                .expect("one-byte UTF-8/CRLF chunk");
        }
        finish_sse_utf8(&carry).expect("complete terminal UTF-8");
        let mut cursor = 0;
        assert_eq!(
            next_sse_data(&buffer, &mut cursor).as_deref(),
            Some("{\"text\":\"€\",\n\"ok\":true}"),
        );
        assert_eq!(cursor, buffer.len());
    }

    #[test]
    fn sse_utf8_decoder_rejects_invalid_and_incomplete_terminal_bytes() {
        let mut buffer = String::new();
        let mut carry = Vec::with_capacity(4);
        assert!(append_sse_utf8_chunk(&mut buffer, &mut carry, &[0xE2]).is_ok());
        assert!(finish_sse_utf8(&carry).is_err());

        carry.clear();
        assert!(append_sse_utf8_chunk(&mut buffer, &mut carry, &[0xFF]).is_err());
    }

    #[test]
    fn sse_admission_accepts_exact_event_and_rejects_one_byte_over() {
        let mut exact = SseBodyAdmission::with_limits(16, 64);
        let exact_event = vec![b'a'; 14];
        exact
            .admit_chunk(&exact_event)
            .expect("exact event payload");
        exact.admit_chunk(b"\n\n").expect("exact event delimiter");

        let mut over = SseBodyAdmission::with_limits(16, 64);
        let over_event = vec![b'a'; 15];
        over.admit_chunk(&over_event).expect("one byte remains");
        assert!(matches!(
            over.admit_chunk(b"\n\n"),
            Err(crate::error::LLMError::Validation(_))
        ));
    }

    #[test]
    fn sse_admission_enforces_aggregate_stream_ceiling_across_events() {
        let event = b"data: {}\n\n";
        let max_stream_bytes = event.len() * 4 + 3;
        let event_count = max_stream_bytes / event.len();
        let remainder = max_stream_bytes % event.len();
        let mut admission = SseBodyAdmission::with_limits(event.len(), max_stream_bytes);
        for _ in 0..event_count {
            admission.admit_chunk(event).expect("within aggregate cap");
        }
        admission
            .admit_chunk(&vec![b' '; remainder])
            .expect("exact aggregate cap");
        assert!(matches!(
            admission.admit_chunk(b"x"),
            Err(crate::error::LLMError::Validation(_))
        ));
    }
}

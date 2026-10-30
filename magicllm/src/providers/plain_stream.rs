//! Plain (non-streaming) calls that stream under the hood.
//!
//! A request-wide timeout cut long answers that were still producing: 32k
//! output tokens take 5-11 minutes. A plain `invoke()` therefore asks the
//! provider to stream, bounds the body with a [`StreamWatchdog`] (progress,
//! not total length), and folds the stream back into the provider's own
//! non-streaming response body. The provider's existing parser reads that
//! body, so callers, and `raw_response` (web-search counts, citations, replay
//! state), see what a non-streaming call returned.

use std::collections::BTreeMap;
use std::time::Duration;

use serde_json::{Map, Value};

use super::{
    append_sse_utf8_chunk, finish_sse_utf8, next_sse_data, read_bounded_response_text,
    SseBodyAdmission, StreamWatchdog, MAX_PROVIDER_SSE_EVENT_BYTES, MAX_PROVIDER_SSE_STREAM_BYTES,
};
use crate::error::{LLMError, LLMResult};
use crate::types::{append_tool_argument_fragment, parse_provider_json_value};

/// What a protected request's provider error says instead of the upstream
/// text, which may echo the request.
pub(crate) const PROTECTED_PROVIDER_ERROR: &str = "protected provider request failed";

fn transport_error(error: reqwest::Error) -> LLMError {
    if error.is_timeout() {
        LLMError::Timeout
    } else {
        LLMError::Transport(error.to_string())
    }
}

/// Whether a response to a streaming request is one JSON document rather
/// than an event stream: error bodies, and servers that ignore the stream
/// flag. Callers parse those exactly as a non-streaming call did.
pub(crate) fn is_json_response(response: &reqwest::Response) -> bool {
    response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("application/json")
        })
}

/// Read a whole JSON body that answered a streaming request. The stream
/// watchdog does not cover it, so the request timeout does.
pub(crate) async fn read_json_body(
    response: reqwest::Response,
    timeout: Duration,
) -> LLMResult<Value> {
    let raw = tokio::time::timeout(timeout, read_bounded_response_text(response))
        .await
        .map_err(|_| LLMError::Timeout)??;
    parse_provider_json_value(&raw)
}

/// Reads the `data` payloads of a server-sent-event body under a
/// [`StreamWatchdog`]. Callers mark progress on content events only.
pub(crate) struct SseReader<S> {
    stream: S,
    watchdog: StreamWatchdog,
    admission: SseBodyAdmission,
    buffer: String,
    cursor: usize,
    utf8_carry: Vec<u8>,
    ended: bool,
}

impl<S, B> SseReader<S>
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin,
    B: AsRef<[u8]>,
{
    pub(crate) fn new(stream: S, first_output_timeout: Duration) -> Self {
        Self {
            stream,
            watchdog: StreamWatchdog::new(first_output_timeout),
            admission: SseBodyAdmission::new(),
            buffer: String::new(),
            cursor: 0,
            utf8_carry: Vec::with_capacity(4),
            ended: false,
        }
    }

    pub(crate) fn mark_progress(&mut self) {
        self.watchdog.mark_progress();
    }

    /// The next event's `data` payload, or `None` once the body ends.
    pub(crate) async fn next_data(&mut self) -> LLMResult<Option<String>> {
        loop {
            if let Some(data) = next_sse_data(&self.buffer, &mut self.cursor) {
                return Ok(Some(data.into_owned()));
            }
            if self.cursor > 0 {
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
            }
            if self.ended {
                return Ok(None);
            }
            match self.watchdog.next(&mut self.stream).await? {
                Some(chunk) => {
                    let chunk = chunk.map_err(transport_error)?;
                    self.admission.admit_chunk(chunk.as_ref())?;
                    append_sse_utf8_chunk(&mut self.buffer, &mut self.utf8_carry, chunk.as_ref())?;
                },
                None => {
                    self.ended = true;
                    finish_sse_utf8(&self.utf8_carry)?;
                },
            }
        }
    }
}

/// Reads the lines of a newline-delimited JSON body (Ollama) under a
/// [`StreamWatchdog`], with the same per-record and total ceilings as SSE.
pub(crate) struct NdjsonReader<S> {
    stream: S,
    watchdog: StreamWatchdog,
    buffer: String,
    cursor: usize,
    utf8_carry: Vec<u8>,
    total_bytes: usize,
    ended: bool,
}

impl<S, B> NdjsonReader<S>
where
    S: futures_util::Stream<Item = reqwest::Result<B>> + Unpin,
    B: AsRef<[u8]>,
{
    pub(crate) fn new(stream: S, first_output_timeout: Duration) -> Self {
        Self {
            stream,
            watchdog: StreamWatchdog::new(first_output_timeout),
            buffer: String::new(),
            cursor: 0,
            utf8_carry: Vec::with_capacity(4),
            total_bytes: 0,
            ended: false,
        }
    }

    pub(crate) fn mark_progress(&mut self) {
        self.watchdog.mark_progress();
    }

    /// The next non-empty line, or `None` once the body ends.
    pub(crate) async fn next_line(&mut self) -> LLMResult<Option<String>> {
        loop {
            if let Some(relative_end) = self.buffer[self.cursor..].find('\n') {
                let line = self.buffer[self.cursor..self.cursor + relative_end]
                    .trim()
                    .to_owned();
                self.cursor += relative_end + 1;
                if line.is_empty() {
                    continue;
                }
                return Ok(Some(line));
            }
            if self.buffer.len() - self.cursor > MAX_PROVIDER_SSE_EVENT_BYTES {
                return Err(LLMError::Validation(format!(
                    "provider stream record exceeds the admitted {MAX_PROVIDER_SSE_EVENT_BYTES}-byte ceiling"
                )));
            }
            if self.cursor > 0 {
                self.buffer.drain(..self.cursor);
                self.cursor = 0;
            }
            if self.ended {
                // A final record without its trailing newline.
                let tail = self.buffer.trim().to_owned();
                self.buffer.clear();
                return Ok((!tail.is_empty()).then_some(tail));
            }
            match self.watchdog.next(&mut self.stream).await? {
                Some(chunk) => {
                    let chunk = chunk.map_err(transport_error)?;
                    let bytes = chunk.as_ref();
                    self.total_bytes = self.total_bytes.saturating_add(bytes.len());
                    if self.total_bytes > MAX_PROVIDER_SSE_STREAM_BYTES {
                        return Err(LLMError::Validation(format!(
                            "provider stream exceeds the admitted {MAX_PROVIDER_SSE_STREAM_BYTES}-byte aggregate ceiling"
                        )));
                    }
                    append_sse_utf8_chunk(&mut self.buffer, &mut self.utf8_carry, bytes)?;
                },
                None => {
                    self.ended = true;
                    finish_sse_utf8(&self.utf8_carry)?;
                },
            }
        }
    }
}

/// Folds an OpenAI-style `chat.completion.chunk` stream back into the
/// `chat.completion` body a non-streaming call returns. Delta strings
/// (content, reasoning) concatenate, arrays (annotations, images) extend and
/// tool calls merge by index; any other field keeps its latest value, so
/// provider extensions survive.
pub(crate) struct ChatCompletionAssembler {
    provider: String,
    protected: bool,
    top: Map<String, Value>,
    choices: BTreeMap<u64, ChoiceAssembly>,
    usage: Option<Value>,
    saw_done: bool,
    finished: bool,
}

#[derive(Default)]
struct ChoiceAssembly {
    fields: Map<String, Value>,
    message: Map<String, Value>,
    tool_calls: BTreeMap<u64, ToolCallAssembly>,
}

#[derive(Default)]
struct ToolCallAssembly {
    fields: Map<String, Value>,
    function: Map<String, Value>,
    arguments: String,
}

impl ChatCompletionAssembler {
    pub(crate) fn new(provider: impl Into<String>, protected: bool) -> Self {
        Self {
            provider: provider.into(),
            protected,
            top: Map::new(),
            choices: BTreeMap::new(),
            usage: None,
            saw_done: false,
            finished: false,
        }
    }

    /// The stream sent its `[DONE]` sentinel.
    pub(crate) fn mark_done(&mut self) {
        self.saw_done = true;
    }

    pub(crate) fn done(&self) -> bool {
        self.saw_done
    }

    /// Fold one SSE `data` payload; `Ok(true)` when it carried output.
    pub(crate) fn fold_data(&mut self, data: &str) -> LLMResult<bool> {
        if data == "[DONE]" {
            self.mark_done();
            return Ok(false);
        }
        self.fold(parse_provider_json_value(data)?)
    }

    /// Fold one parsed chunk; `Ok(true)` when it carried output
    /// ([`chat_choice_has_output`], the rule the streaming paths use too).
    pub(crate) fn fold(&mut self, chunk: Value) -> LLMResult<bool> {
        let Value::Object(mut chunk) = chunk else {
            return Ok(false);
        };
        if let Some(error) = chunk.get("error").filter(|error| !error.is_null()) {
            return Err(self.stream_error(error));
        }
        let mut output = false;
        if let Some(Value::Array(choices)) = chunk.remove("choices") {
            output = choices.iter().any(chat_choice_has_output);
            for choice in choices {
                self.fold_choice(choice)?;
            }
        }
        if let Some(usage) = chunk.remove("usage").filter(|usage| !usage.is_null()) {
            self.usage = Some(usage);
        }
        for (key, value) in chunk {
            if !value.is_null() && key != "object" {
                self.top.insert(key, value);
            }
        }
        Ok(output)
    }

    fn stream_error(&self, error: &Value) -> LLMError {
        let message = if self.protected {
            PROTECTED_PROVIDER_ERROR.to_owned()
        } else {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
                .unwrap_or("provider stream reported an error")
                .to_owned()
        };
        LLMError::Provider {
            provider: self.provider.clone(),
            message,
        }
    }

    fn fold_choice(&mut self, choice: Value) -> LLMResult<()> {
        let Value::Object(mut choice) = choice else {
            return Ok(());
        };
        let index = choice.get("index").and_then(Value::as_u64).unwrap_or(0);
        let entry = self.choices.entry(index).or_default();
        if let Some(Value::Object(delta)) = choice.remove("delta") {
            entry.fold_delta(delta)?;
        }
        for (key, value) in choice {
            // A chunk-level `message` (some servers repeat the whole message
            // on the last chunk) would double the deltas already folded.
            if value.is_null() || key == "message" {
                continue;
            }
            if key == "finish_reason" {
                self.finished = true;
            }
            if key == "logprobs" {
                merge_streamed_field(&mut entry.fields, key, value, false);
            } else {
                entry.fields.insert(key, value);
            }
        }
        Ok(())
    }

    /// The assembled `chat.completion` body. A stream that ended without a
    /// finish reason or `[DONE]` was cut off: a plain call got no complete
    /// answer, so it fails as transport (as a truncated JSON body did).
    pub(crate) fn finish(self) -> LLMResult<Value> {
        if !self.finished && !self.saw_done {
            return Err(LLMError::Transport(format!(
                "{} stream ended before the response completed",
                self.provider
            )));
        }
        let mut body = self.top;
        body.insert(
            "object".to_owned(),
            Value::String("chat.completion".to_owned()),
        );
        let choices = self
            .choices
            .into_iter()
            .map(|(index, choice)| choice.into_value(index))
            .collect();
        body.insert("choices".to_owned(), Value::Array(choices));
        if let Some(usage) = self.usage {
            body.insert("usage".to_owned(), usage);
        }
        Ok(Value::Object(body))
    }
}

impl ChoiceAssembly {
    fn fold_delta(&mut self, delta: Map<String, Value>) -> LLMResult<()> {
        for (key, value) in delta {
            if value.is_null() {
                continue;
            }
            if key == "tool_calls" {
                if let Value::Array(calls) = value {
                    for call in calls {
                        self.fold_tool_call(call)?;
                    }
                }
            } else if key == "role" {
                self.message.insert(key, value);
            } else {
                merge_streamed_field(&mut self.message, key, value, true);
            }
        }
        Ok(())
    }

    fn fold_tool_call(&mut self, call: Value) -> LLMResult<()> {
        let Value::Object(call) = call else {
            return Ok(());
        };
        let index = call.get("index").and_then(Value::as_u64).unwrap_or(0);
        let entry = self.tool_calls.entry(index).or_default();
        for (key, value) in call {
            if value.is_null() || key == "index" {
                continue;
            }
            if key != "function" {
                entry.fields.insert(key, value);
                continue;
            }
            let Value::Object(function) = value else {
                continue;
            };
            for (field, value) in function {
                match value {
                    Value::Null => {},
                    Value::String(fragment) if field == "arguments" => {
                        append_tool_argument_fragment(&mut entry.arguments, &fragment)?;
                    },
                    value => {
                        entry.function.insert(field, value);
                    },
                }
            }
        }
        Ok(())
    }

    fn into_value(self, index: u64) -> Value {
        let mut message = self.message;
        message
            .entry("role".to_owned())
            .or_insert_with(|| Value::String("assistant".to_owned()));
        message.entry("content".to_owned()).or_insert(Value::Null);
        if !self.tool_calls.is_empty() {
            let calls = self
                .tool_calls
                .into_values()
                .map(ToolCallAssembly::into_value)
                .collect();
            message.insert("tool_calls".to_owned(), Value::Array(calls));
        }
        let mut choice = self.fields;
        choice.insert("index".to_owned(), Value::from(index));
        choice.insert("message".to_owned(), Value::Object(message));
        Value::Object(choice)
    }
}

impl ToolCallAssembly {
    fn into_value(self) -> Value {
        let mut call = self.fields;
        let mut function = self.function;
        if !self.arguments.is_empty() {
            function.insert("arguments".to_owned(), Value::String(self.arguments));
        }
        call.entry("type".to_owned())
            .or_insert_with(|| Value::String("function".to_owned()));
        call.insert("function".to_owned(), Value::Object(function));
        Value::Object(call)
    }
}

/// Merge one streamed field into its accumulated value. Arrays extend, with
/// items that carry the same `index` (streamed `reasoning_details`) merged
/// into one; objects merge field by field; strings concatenate when
/// `concat_strings` (message deltas) and are replaced otherwise.
fn merge_streamed_field(
    target: &mut Map<String, Value>,
    key: String,
    value: Value,
    concat_strings: bool,
) {
    if value.is_null() {
        return;
    }
    match target.get_mut(&key) {
        Some(Value::String(existing))
            if concat_strings && is_streamed_text_field(&key) && value.is_string() =>
        {
            if let Value::String(fragment) = value {
                existing.push_str(&fragment);
            }
        },
        Some(Value::Array(existing)) if value.is_array() => {
            if let Value::Array(items) = value {
                for item in items {
                    merge_streamed_item(existing, item, concat_strings);
                }
            }
        },
        Some(Value::Object(existing)) if value.is_object() => {
            if let Value::Object(fields) = value {
                for (field, value) in fields {
                    merge_streamed_field(existing, field, value, concat_strings);
                }
            }
        },
        _ => {
            target.insert(key, value);
        },
    }
}

/// Delta fields whose string fragments join into one value. Every other
/// string a server repeats on each delta (`id`, `name`, `format`, `type`) is
/// replaced, so a repeated `format` is not concatenated into garbage.
fn is_streamed_text_field(key: &str) -> bool {
    matches!(
        key,
        "content"
            | "reasoning"
            | "reasoning_content"
            | "refusal"
            | "text"
            | "data"
            | "summary"
            | "arguments"
    )
}

fn merge_streamed_item(items: &mut Vec<Value>, item: Value, concat_strings: bool) {
    let index = item.get("index").and_then(Value::as_u64);
    if let (Some(index), Value::Object(fields)) = (index, &item) {
        let kind = fields.get("type").cloned();
        let same = items.iter_mut().rev().find(|existing| {
            existing.get("index").and_then(Value::as_u64) == Some(index)
                && existing.get("type").cloned() == kind
        });
        if let Some(Value::Object(existing)) = same {
            if let Value::Object(fields) = item {
                for (field, value) in fields {
                    if field != "index" && field != "type" {
                        merge_streamed_field(existing, field, value, concat_strings);
                    }
                }
            }
            return;
        }
    }
    items.push(item);
}

/// Fold an OpenAI-style chat-completions response into its non-streaming
/// body: SSE when the server streamed, the JSON body as it came otherwise
/// (an error status, or a server that ignored `stream: true`).
pub(crate) async fn collect_chat_completion(
    response: reqwest::Response,
    timeout: Duration,
    provider: &str,
    protected: bool,
) -> LLMResult<Value> {
    if !response.status().is_success() || is_json_response(&response) {
        return read_json_body(response, timeout).await;
    }
    let mut assembler = ChatCompletionAssembler::new(provider, protected);
    let mut reader = SseReader::new(response.bytes_stream(), timeout);
    while let Some(data) = reader.next_data().await? {
        if assembler.fold_data(&data)? {
            reader.mark_progress();
        }
        if assembler.done() {
            break;
        }
    }
    assembler.finish()
}

/// Whether a chat-completions chunk choice carries output: non-empty text,
/// reasoning or refusal, or a tool-call fragment. The opening role-only chunk
/// (`{"role":"assistant","content":""}`) and a bare `finish_reason` do not,
/// so a reasoning model's silent first minutes stay under the first-output
/// bound instead of starting the idle clock.
pub(crate) fn chat_choice_has_output(choice: &Value) -> bool {
    let Some(delta) = choice.get("delta").or_else(|| choice.get("message")) else {
        return false;
    };
    let non_empty = |key: &str| {
        delta
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty())
    };
    non_empty("content")
        || non_empty("reasoning_content")
        || non_empty("reasoning")
        || non_empty("refusal")
        || delta
            .get("tool_calls")
            .and_then(Value::as_array)
            .is_some_and(|calls| !calls.is_empty())
        || delta
            .get("reasoning_details")
            .and_then(Value::as_array)
            .is_some_and(|details| !details.is_empty())
}

/// Whether a chat-completions chunk carries output in any choice.
pub(crate) fn chat_chunk_has_output(chunk: &Value) -> bool {
    chunk
        .get("choices")
        .and_then(Value::as_array)
        .is_some_and(|choices| choices.iter().any(chat_choice_has_output))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn fold_all(chunks: &[Value]) -> LLMResult<Value> {
        let mut assembler = ChatCompletionAssembler::new("test", false);
        for chunk in chunks {
            assembler.fold(chunk.clone())?;
        }
        assembler.finish()
    }

    #[test]
    fn chat_chunks_fold_into_the_non_streaming_body() {
        let body = fold_all(&[
            json!({"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}),
            json!({"id":"c1","choices":[{"index":0,"delta":{"content":"Hel","reasoning_content":"thi"}}]}),
            json!({"id":"c1","choices":[{"index":0,"delta":{"content":"lo","reasoning_content":"nk","tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"f","arguments":"{\"a\""}}]}}]}),
            json!({"id":"c1","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}}],"annotations":[{"type":"url_citation","url":"u"}]},"finish_reason":"tool_calls"}]}),
            json!({"id":"c1","choices":[],"usage":{"prompt_tokens":3,"completion_tokens":4,"total_tokens":7}}),
        ])
        .unwrap();
        let message = &body["choices"][0]["message"];
        assert_eq!(message["role"], "assistant");
        assert_eq!(message["content"], "Hello");
        assert_eq!(message["reasoning_content"], "think");
        assert_eq!(message["annotations"][0]["url"], "u");
        assert_eq!(
            message["tool_calls"][0],
            json!({"id":"call_1","type":"function","function":{"name":"f","arguments":"{\"a\":1}"}})
        );
        assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(body["usage"]["total_tokens"], 7);
        assert_eq!(body["id"], "c1");
        assert_eq!(body["model"], "m");
    }

    #[test]
    fn indexed_reasoning_details_merge_and_repeated_finish_reasons_do_not_concat() {
        let body = fold_all(&[
            json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"text":"a"}]}}]}),
            json!({"choices":[{"delta":{"reasoning_details":[{"type":"reasoning.text","index":0,"text":"b","signature":"s"}]},"finish_reason":"stop"}]}),
            json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
        ])
        .unwrap();
        let details = &body["choices"][0]["message"]["reasoning_details"];
        assert_eq!(
            details,
            &json!([{"type":"reasoning.text","index":0,"text":"ab","signature":"s"}])
        );
        assert_eq!(body["choices"][0]["finish_reason"], "stop");
        assert!(body["choices"][0]["message"]["content"].is_null());
    }

    #[test]
    fn a_stream_cut_before_its_finish_is_a_transport_error_and_errors_surface() {
        assert!(matches!(
            fold_all(&[json!({"choices":[{"delta":{"content":"partial"}}]})]),
            Err(LLMError::Transport(_))
        ));
        let mut assembler = ChatCompletionAssembler::new("test", true);
        assert!(matches!(
            assembler.fold(json!({"error":{"message":"echoes the prompt"}})),
            Err(LLMError::Provider { message, .. }) if message == PROTECTED_PROVIDER_ERROR
        ));
    }
}

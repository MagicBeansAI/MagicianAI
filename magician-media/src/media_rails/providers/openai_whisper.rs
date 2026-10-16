//! OpenAI Whisper speech-to-text adapter.
//!
//! Posts a multipart form to `/v1/audio/transcriptions` with the audio
//! blob, optional language hint, and model. The endpoint is also used
//! by `whisper-1`, `gpt-transcribe`, and other transcription models — the
//! model id is configurable per request.

use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use reqwest::{multipart, Client};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

use super::stt::{SttError, SttProvider, SttRequest, SttResponse, SttStreamEvent};

pub const OPENAI_WHISPER_DEFAULT_BASE_URL: &str = "https://api.openai.com/v1/audio/transcriptions";
pub const OPENAI_WHISPER_PROVIDER_ID: &str = "openai";

/// Default STT model. `gpt-transcribe` is OpenAI's current high-accuracy
/// speech-to-text model for completed audio files and streamed file
/// transcripts. It uses the existing `/v1/audio/transcriptions` endpoint and
/// supports the language and prompt hints this adapter already sends.
///
/// Override via `MAGICIAN_STT_MODEL`. Valid alternatives:
///   - `gpt-transcribe` (default, high accuracy)
///   - `gpt-4o-mini-transcribe` (cheaper variant)
///   - `gpt-4o-transcribe-diarize` (adds speaker labels)
///   - `whisper-1` (legacy)
///
/// `gpt-live-transcribe` is intentionally not advertised until the media rail
/// implements and qualifies its low-latency realtime contract.
pub const OPENAI_WHISPER_DEFAULT_MODEL: &str = "gpt-transcribe";

#[derive(Debug, Clone)]
pub struct OpenAiWhisperProvider {
    client: Client,
    api_key: String,
    base_url: String,
    provider_id: String,
    label: Option<String>,
    default_model: String,
}

impl OpenAiWhisperProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_client(
            default_http_client(),
            api_key,
            OPENAI_WHISPER_DEFAULT_BASE_URL,
        )
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
            provider_id: OPENAI_WHISPER_PROVIDER_ID.to_string(),
            label: None,
            default_model: OPENAI_WHISPER_DEFAULT_MODEL.to_string(),
        }
    }

    pub fn with_provider_id(mut self, provider_id: impl Into<String>) -> Self {
        self.provider_id = provider_id.into();
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn with_default_model(mut self, model: impl Into<String>) -> Self {
        self.default_model = model.into();
        self
    }
}

#[derive(Deserialize)]
struct OpenAiTranscriptionResponse {
    text: String,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    duration: Option<f64>,
}

#[derive(Deserialize)]
#[serde(tag = "type")]
enum OpenAiTranscriptionStreamEvent {
    /// Incremental decoded fragment — `delta` is the NEW text since
    /// the prior event. OpenAI sometimes ships a `logprobs` array too
    /// but we don't surface it; serde just ignores unknown fields.
    #[serde(rename = "transcript.text.delta")]
    Delta { delta: String },
    /// Terminal event with the full cumulative transcript. After this
    /// arrives the OpenAI stream closes; we always prefer this over
    /// the running `accumulated` buffer because it's the authoritative
    /// final form (the model may correct earlier deltas).
    #[serde(rename = "transcript.text.done")]
    Done { text: String },
    /// Segment event used by diarized streaming responses. The default
    /// composer path does not request diarized JSON, but accepting the
    /// event keeps the adapter tolerant if a caller overrides the model
    /// or response format later.
    #[serde(rename = "transcript.text.segment")]
    Segment { text: String },
}

#[async_trait]
impl SttProvider for OpenAiWhisperProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let filename = request
            .filename
            .clone()
            .unwrap_or_else(|| guess_filename(&request.content_type));
        let bytes_vec: Vec<u8> = request.audio.to_vec();
        let audio_part = multipart::Part::bytes(bytes_vec)
            .file_name(filename.clone())
            .mime_str(&request.content_type)
            .map_err(|e| SttError::BadRequest(format!("invalid content type: {e}")))?;

        let mut form = multipart::Form::new()
            .text("model", model.clone())
            .text("response_format", "json")
            .part("file", audio_part);
        if let Some(lang) = &request.language {
            form = form.text("language", lang.clone());
        }
        if let Some(prompt) = &request.prompt {
            form = form.text("prompt", prompt.clone());
        }

        info!(
            "[STT] transcribing url={} model={} bytes={} content_type={} lang={:?} filename={}",
            self.base_url,
            model,
            request.audio.len(),
            request.content_type,
            request.language,
            filename
        );
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(180))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                error!("[STT] transport failure: {e}");
                SttError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!("[STT] upstream rejected status={} body={}", status, body);
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: OpenAiTranscriptionResponse = response.json().await.map_err(|e| {
            error!("[STT] response decode failure: {e}");
            SttError::Transport(format!("decoding response: {e}"))
        })?;
        debug!(
            "[STT] transcribed text_len={} language={:?}",
            parsed.text.len(),
            parsed.language
        );
        let language = parsed.language.clone().or_else(|| request.language.clone());
        let extras = parsed
            .duration
            .map(|duration| serde_json::json!({ "duration_seconds": duration }));
        Ok(SttResponse {
            transcript: parsed.text,
            model,
            language,
            message_id: request.message_id,
            extras,
        })
    }

    async fn transcribe_stream(
        &self,
        request: SttRequest,
        events: mpsc::Sender<SttStreamEvent>,
    ) -> Result<SttResponse, SttError> {
        if request.audio.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        let model = request
            .model
            .clone()
            .unwrap_or_else(|| self.default_model.clone());
        let filename = request
            .filename
            .clone()
            .unwrap_or_else(|| guess_filename(&request.content_type));
        let bytes_vec: Vec<u8> = request.audio.to_vec();
        let audio_part = multipart::Part::bytes(bytes_vec)
            .file_name(filename.clone())
            .mime_str(&request.content_type)
            .map_err(|e| SttError::BadRequest(format!("invalid content type: {e}")))?;

        // `stream=true` flips the response shape from JSON to
        // `text/event-stream`. The model still processes the full
        // upload — we win latency by getting `transcript.text.delta`
        // events as soon as decoded tokens are available, instead of
        // blocking for the full transcript.
        let mut form = multipart::Form::new()
            .text("model", model.clone())
            .text("response_format", "json")
            .text("stream", "true")
            .part("file", audio_part);
        if let Some(lang) = &request.language {
            form = form.text("language", lang.clone());
        }
        if let Some(prompt) = &request.prompt {
            form = form.text("prompt", prompt.clone());
        }

        info!(
            "[STT-STREAM] transcribing url={} model={} bytes={} content_type={} lang={:?}",
            self.base_url,
            model,
            request.audio.len(),
            request.content_type,
            request.language
        );
        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(180))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                error!("[STT-STREAM] transport failure: {e}");
                SttError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(
                "[STT-STREAM] upstream rejected status={} body={}",
                status, body
            );
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }

        // Parse the OpenAI SSE stream byte-by-byte. Events arrive as
        // `data: {...json...}\n\n` blocks; we accumulate bytes in a
        // line buffer and split on the blank-line terminator. The
        // `data:` payload is the only field we care about. We
        // forward each delta to the caller and accumulate the final
        // transcript for the SttResponse return value.
        let mut byte_stream = response.bytes_stream();
        let mut buffer: Vec<u8> = Vec::with_capacity(4096);
        let mut accumulated = String::new();
        let mut final_text: Option<String> = None;
        let mut parsed_event_count: usize = 0;
        let mut decode_failure_count: usize = 0;

        while let Some(chunk_res) = byte_stream.next().await {
            let chunk = chunk_res.map_err(|e| SttError::Transport(e.to_string()))?;
            buffer.extend_from_slice(&chunk);
            while let Some((end, terminator_len)) = find_event_boundary(&buffer) {
                let event_bytes = buffer.drain(..end).collect::<Vec<u8>>();
                // Remove the trailing blank-line terminator from the
                // remaining buffer. OpenAI currently streams with SSE,
                // but intermediaries can normalize line endings to CRLF.
                buffer.drain(..usize::min(terminator_len, buffer.len()));
                let event_text = match std::str::from_utf8(&event_bytes) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                let data_payload = extract_sse_data(event_text);
                let Some(payload) = data_payload else {
                    continue;
                };
                if payload == "[DONE]" {
                    continue;
                }
                let parsed: OpenAiTranscriptionStreamEvent = match serde_json::from_str(payload) {
                    Ok(p) => p,
                    Err(err) => {
                        decode_failure_count += 1;
                        warn!(
                            "[STT-STREAM] failed to decode event payload (skipping): {err} \
                             payload={payload}"
                        );
                        continue;
                    },
                };
                parsed_event_count += 1;
                match parsed {
                    OpenAiTranscriptionStreamEvent::Delta { delta, .. } => {
                        accumulated.push_str(&delta);
                        let _ = events
                            .send(SttStreamEvent::Delta {
                                text: accumulated.clone(),
                                fragment: Some(delta),
                            })
                            .await;
                    },
                    OpenAiTranscriptionStreamEvent::Done { text, .. } => {
                        final_text = Some(text);
                    },
                    OpenAiTranscriptionStreamEvent::Segment { text, .. } => {
                        if !text.is_empty() {
                            if !accumulated.is_empty() {
                                accumulated.push('\n');
                            }
                            accumulated.push_str(&text);
                            let _ = events
                                .send(SttStreamEvent::Delta {
                                    text: accumulated.clone(),
                                    fragment: Some(text),
                                })
                                .await;
                        }
                    },
                }
            }
        }

        if !buffer.is_empty() {
            let tail = String::from_utf8_lossy(&buffer);
            let trimmed = tail.trim();
            if !trimmed.is_empty() {
                if let Some(payload) = extract_sse_data(trimmed) {
                    if payload != "[DONE]" {
                        match serde_json::from_str::<OpenAiTranscriptionStreamEvent>(payload) {
                            Ok(OpenAiTranscriptionStreamEvent::Delta { delta, .. }) => {
                                parsed_event_count += 1;
                                accumulated.push_str(&delta);
                                let _ = events
                                    .send(SttStreamEvent::Delta {
                                        text: accumulated.clone(),
                                        fragment: Some(delta),
                                    })
                                    .await;
                            },
                            Ok(OpenAiTranscriptionStreamEvent::Done { text, .. }) => {
                                parsed_event_count += 1;
                                final_text = Some(text);
                            },
                            Ok(OpenAiTranscriptionStreamEvent::Segment { text, .. }) => {
                                parsed_event_count += 1;
                                if !text.is_empty() {
                                    if !accumulated.is_empty() {
                                        accumulated.push('\n');
                                    }
                                    accumulated.push_str(&text);
                                    let _ = events
                                        .send(SttStreamEvent::Delta {
                                            text: accumulated.clone(),
                                            fragment: Some(text),
                                        })
                                        .await;
                                }
                            },
                            Err(err) => {
                                decode_failure_count += 1;
                                warn!(
                                    "[STT-STREAM] failed to decode trailing SSE payload: {err} \
                                     payload={payload}"
                                );
                            },
                        }
                    }
                } else if trimmed.starts_with('{') {
                    match serde_json::from_str::<OpenAiTranscriptionResponse>(trimmed) {
                        Ok(parsed) => {
                            // If OpenAI ignores streaming for a model or
                            // deployment, it can return the normal JSON
                            // transcription body. Treat that as the final
                            // transcript instead of reporting a false
                            // empty transcript to the UI.
                            final_text = Some(parsed.text);
                        },
                        Err(err) => {
                            decode_failure_count += 1;
                            warn!(
                                "[STT-STREAM] failed to decode trailing transcription JSON: {err}"
                            );
                        },
                    }
                } else {
                    warn!(
                        "[STT-STREAM] unparsed trailing response bytes={} preview={}",
                        trimmed.len(),
                        trimmed.chars().take(160).collect::<String>()
                    );
                }
            }
        }

        let transcript = final_text.unwrap_or(accumulated);
        if transcript.trim().is_empty() {
            warn!(
                "[STT-STREAM] completed with empty transcript model={} bytes={} content_type={} \
                 parsed_events={} decode_failures={}",
                model,
                request.audio.len(),
                request.content_type,
                parsed_event_count,
                decode_failure_count
            );
        } else {
            info!(
                "[STT-STREAM] transcribed text_len={} model={} parsed_events={} decode_failures={}",
                transcript.len(),
                model,
                parsed_event_count,
                decode_failure_count
            );
        }
        let language = request.language.clone();
        let _ = events
            .send(SttStreamEvent::Final {
                transcript: transcript.clone(),
                model: model.clone(),
                language: language.clone(),
            })
            .await;
        Ok(SttResponse {
            transcript,
            model,
            language,
            message_id: request.message_id,
            extras: None,
        })
    }
}

/// Find the byte offset and terminator length of the next SSE event
/// boundary. SSE allows CRLF line endings; OpenAI usually streams LF,
/// but proxies and HTTP stacks can normalize to `\r\n\r\n`.
fn find_event_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    let lf = buffer
        .windows(2)
        .position(|w| w == b"\n\n")
        .map(|pos| (pos, 2));
    let crlf = buffer
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| (pos, 4));
    match (lf, crlf) {
        (Some(left), Some(right)) => Some(if left.0 <= right.0 { left } else { right }),
        (Some(boundary), None) | (None, Some(boundary)) => Some(boundary),
        (None, None) => None,
    }
}

/// Extract the `data:` payload from a single SSE event block. Returns
/// `None` for keep-alive comments (lines starting with `:`) or events
/// without a `data:` line.
fn extract_sse_data(event: &str) -> Option<&str> {
    for line in event.split('\n') {
        let line = line.trim_start();
        if let Some(rest) = line.strip_prefix("data:") {
            return Some(rest.trim_start());
        }
    }
    None
}

fn guess_filename(content_type: &str) -> String {
    let ext = match content_type.to_ascii_lowercase().as_str() {
        s if s.contains("webm") => "webm",
        s if s.contains("ogg") => "ogg",
        s if s.contains("mp3") => "mp3",
        s if s.contains("mpeg") => "mp3",
        s if s.contains("mp4") | s.contains("m4a") => "m4a",
        s if s.contains("wav") => "wav",
        s if s.contains("flac") => "flac",
        _ => "webm",
    };
    format!("audio.{ext}")
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build Whisper HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media_rails::providers::stt::SttProvider;

    #[test]
    fn provider_id_model_and_label_can_be_configured_for_selector_choices() {
        let provider = OpenAiWhisperProvider::with_base_url(
            "test-key",
            "https://example.invalid/v1/audio/transcriptions",
        )
        .with_provider_id("future-stt")
        .with_label("Future STT")
        .with_default_model("future-transcribe-model");

        assert_eq!(provider.id(), "future-stt");
        assert_eq!(provider.label(), Some("Future STT"));
        assert_eq!(provider.default_model(), "future-transcribe-model");
    }
}

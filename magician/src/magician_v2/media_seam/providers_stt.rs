//! Provider-agnostic speech-to-text contract.
//!
//! Adapters take raw audio bytes (any codec the provider accepts) and
//! return a final transcript. Streaming partials are handled by
//! `magicllm::realtime::RealtimeProvider` instead — STT here is the
//! one-shot path the chat composer's mic flow uses.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

#[derive(Debug, Clone)]
pub struct SttRequest {
    pub audio: bytes::Bytes,
    pub content_type: String,
    /// Hint to the provider — e.g. `"en"`, `"en-US"`. Adapters that
    /// auto-detect ignore this.
    pub language: Option<String>,
    pub model: Option<String>,
    pub message_id: Option<String>,
    /// Optional filename hint. Whisper requires a filename in its
    /// multipart payload; adapters that don't can ignore.
    pub filename: Option<String>,
    /// Optional prompt to bias the transcription.
    pub prompt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SttResponse {
    pub transcript: String,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
    /// Optional per-provider extras (timings, confidence, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extras: Option<serde_json::Value>,
}

#[derive(Debug, thiserror::Error)]
pub enum SttError {
    #[error("stt provider not configured: {0}")]
    NotConfigured(String),
    #[error("stt request rejected: {0}")]
    BadRequest(String),
    #[error("stt upstream returned {status}: {body}")]
    Upstream { status: u16, body: String },
    #[error("stt transport: {0}")]
    Transport(String),
    /// The provider determined the audio contained no speech (silence). This is
    /// a definitive verdict, NOT a failure — the STT chain treats it as terminal
    /// and does NOT fall through to another provider (which would hallucinate a
    /// transcript from silence).
    #[error("stt detected no speech")]
    NoSpeech,
}

/// Incremental events published while a streaming transcription is in
/// flight. The browser typically renders `Delta` events as a live
/// composer update and triggers downstream behaviour (auto-send, voice
/// banner countdown, etc.) when the `Final` event lands.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SttStreamEvent {
    /// Incremental delta — `text` is the cumulative transcript so
    /// far, NOT just the new fragment. Callers can render this
    /// directly without bookkeeping. `fragment` carries the new
    /// piece if the caller wants to do typewriter-style effects.
    Delta {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fragment: Option<String>,
    },
    /// Final transcript + metadata. Always the last event a
    /// successful stream emits.
    Final {
        transcript: String,
        model: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
    },
    /// Mid-stream error. The stream MAY continue (provider-dependent)
    /// but callers typically treat this as terminal.
    Error { reason: String },
}

#[async_trait]
pub trait SttProvider: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> Option<&str> {
        None
    }
    fn default_model(&self) -> &str;
    async fn transcribe(&self, request: SttRequest) -> Result<SttResponse, SttError>;

    /// Stream incremental transcript deltas while the provider runs.
    /// Default impl falls back to one-shot `transcribe()` so adapters
    /// without true streaming continue to work — they just emit a
    /// single `Final` event when the full result lands.
    ///
    /// Adapters that support streaming (e.g. OpenAI's
    /// `/v1/audio/transcriptions` with `stream: true`) should override
    /// this method to push `Delta` events as they arrive.
    async fn transcribe_stream(
        &self,
        request: SttRequest,
        events: mpsc::Sender<SttStreamEvent>,
    ) -> Result<SttResponse, SttError> {
        let response = self.transcribe(request).await?;
        let _ = events
            .send(SttStreamEvent::Final {
                transcript: response.transcript.clone(),
                model: response.model.clone(),
                language: response.language.clone(),
            })
            .await;
        Ok(response)
    }
}

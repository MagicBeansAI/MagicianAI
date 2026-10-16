//! Continuous, streaming-INGEST speech-to-text contract for meeting-length
//! audio — the meeting bot's "ears". Distinct from [`crate::magician_v2::media_seam::providers_stt::SttProvider`]
//! (one-shot, whole-clip ≤25 MiB): this accepts an open-ended audio stream and
//! emits incremental, optionally speaker-attributed transcript events until the
//! session is closed.
//!
//! Two OpenAI adapters segment the stream and transcribe each window via
//! `/v1/audio/transcriptions`: [`super::openai_streaming_stt`] (`gpt-transcribe`,
//! English-first, Hindi best-effort) and [`super::openai_diarize_stt`]
//! (`gpt-4o-transcribe-diarize`, speaker-attributed). Still deferred (meet-bot design doc
//! §5.C): a Deepgram Nova-3 multilingual adapter (live EN+HI code-switch +
//! diarization, for when Hindi quality matters) and a local whisper.cpp adapter.
//! This module ships the contract + a no-op provider so the meeting orchestration
//! can compile and be unit-tested independent of any adapter.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

// Reuse the one-shot STT error taxonomy — the failure modes are the same.
pub use crate::magician_v2::media_seam::stt::SttError;

/// A chunk of captured audio pushed into a live session. `pcm` is raw frames in
/// the format the session was opened with; `seq` lets the provider order /
/// diagnose dropped chunks.
#[derive(Debug, Clone)]
pub struct AudioChunk {
    pub seq: u64,
    pub pcm: bytes::Bytes,
}

/// Audio format a session expects on its input stream. 16 kHz mono PCM16 is the
/// lingua franca most STT engines accept; adapters resample as needed.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum StreamSampleFormat {
    #[default]
    PcmS16Le,
    PcmF32Le,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct StreamAudioFormat {
    pub sample_rate_hz: u32,
    pub channels: u16,
    #[serde(default)]
    pub sample_format: StreamSampleFormat,
}

impl Default for StreamAudioFormat {
    fn default() -> Self {
        Self {
            sample_rate_hz: 16_000,
            channels: 1,
            sample_format: StreamSampleFormat::PcmS16Le,
        }
    }
}

/// Incremental transcript events from a live session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamingSttEvent {
    /// Interim (not-yet-final) hypothesis for the current utterance.
    Partial {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
    },
    /// A finalized utterance segment.
    Final {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        speaker: Option<String>,
        /// Detected language if reported (e.g. `"en"`, `"hi"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        /// Wall-clock ms offset from session start, if known.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_ms: Option<u64>,
    },
    /// Mid-stream error; callers decide whether to continue.
    Error { reason: String },
}

/// A live transcription session: push audio in, drain events out, finish.
#[async_trait]
pub trait StreamingSttSession: Send + Sync {
    /// Feed a chunk of captured audio.
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError>;
    /// Signal end-of-audio and flush any pending finals.
    async fn finish(&self) -> Result<(), SttError>;
}

/// Opens streaming sessions. One provider, many concurrent sessions.
#[async_trait]
pub trait StreamingSttProvider: Send + Sync {
    fn id(&self) -> &str;
    fn label(&self) -> Option<&str> {
        None
    }
    fn default_model(&self) -> &str {
        self.id()
    }
    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities::default()
    }
    /// Open a session; transcript events are delivered on `events`.
    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StreamingSttCapabilities {
    #[serde(default)]
    pub partial_results: bool,
    #[serde(default)]
    pub word_timestamps: bool,
    #[serde(default)]
    pub end_of_utterance: bool,
    #[serde(default)]
    pub speaker_attribution: bool,
}

/// Placeholder provider used until a real adapter (Deepgram / whisper.cpp) is
/// wired. Accepts audio and emits nothing — keeps the orchestration testable.
pub struct NoopStreamingSttProvider;

#[async_trait]
impl StreamingSttProvider for NoopStreamingSttProvider {
    fn id(&self) -> &str {
        "noop"
    }

    async fn open_session(
        &self,
        _format: StreamAudioFormat,
        _events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        Ok(Box::new(NoopSession))
    }
}

struct NoopSession;

#[async_trait]
impl StreamingSttSession for NoopSession {
    async fn push_audio(&self, _chunk: AudioChunk) -> Result<(), SttError> {
        Ok(())
    }
    async fn finish(&self) -> Result<(), SttError> {
        Ok(())
    }
}

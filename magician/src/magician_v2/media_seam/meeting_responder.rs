//! The "respond when addressed" seam.
//!
//! When a wake phrase fires, `MeetingSession` hands the triggering utterance and
//! the meeting context (rolling summary + recent transcript) to a
//! [`MeetingResponder`], which drafts a reply and synthesizes it to PCM16 for
//! the [`AudioSink`](crate::magician_v2::media_seam::meeting_audio::AudioSink) to inject.
//!
//! Spike 2 proved this with a local-LLM answer + macOS `say` → BlackHole 16ch
//! (`scripts/meet-bot/transcribe_loop.py`, respond-on-wake mode). Production
//! swaps in the OpenAI-Realtime rail (lower latency, natural voice) behind this
//! same trait — the session does not care which.

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::StreamAudioFormat;

#[derive(Debug, thiserror::Error)]
pub enum ResponderError {
    #[error("responder failed: {0}")]
    Failed(String),
}

/// A spoken reply: the text (for logging / captions / the transcript) plus the
/// PCM16 audio to inject into the meeting and the format that audio is in (so
/// the sink plays it back at the right rate).
#[derive(Debug, Clone)]
pub struct SpokenReply {
    pub text: String,
    pub pcm: bytes::Bytes,
    pub format: StreamAudioFormat,
}

impl SpokenReply {
    /// True when there is nothing to say (the responder declined / produced no
    /// audio); the session skips injection in that case.
    pub fn is_empty(&self) -> bool {
        self.pcm.is_empty()
    }
}

/// The raw captured audio of the addressed utterance (the question). A realtime
/// responder can feed this straight to the model so it HEARS the question
/// (better transcription + tone) instead of relying on the on-device STT text.
#[derive(Debug, Clone)]
pub struct UtteranceAudio {
    pub pcm: bytes::Bytes,
    pub format: StreamAudioFormat,
}

#[async_trait]
pub trait MeetingResponder: Send + Sync {
    /// Draft and synthesize a reply to `utterance`, grounded in `context`.
    async fn respond(&self, utterance: &str, context: &str) -> Result<SpokenReply, ResponderError>;

    /// Like [`respond`](Self::respond), but may also receive the raw question
    /// **audio**. Responders that can consume audio (the realtime rail) override
    /// this to let the model hear the actual utterance; the default ignores the
    /// audio and answers from the `utterance` text.
    async fn respond_with_audio(
        &self,
        utterance: &str,
        _audio: Option<&UtteranceAudio>,
        context: &str,
    ) -> Result<SpokenReply, ResponderError> {
        self.respond(utterance, context).await
    }

    /// Whether this responder ALSO persists the exchange (the addressed
    /// utterance + its reply) to the meeting's chat thread itself — true for
    /// the agent-backed responders that route through the chat/voice APIs.
    /// The session uses this to keep the live-transcript lane and the
    /// responder lane from double-posting the same exchange: when true the
    /// transcript sink skips the turns the responder will surface; when false
    /// (local-only responders) the sink carries them instead.
    fn persists_to_chat(&self) -> bool {
        false
    }

    /// Open the reply session before the first question, so that question does
    /// not pay connection setup. The default has nothing to open.
    async fn warm(&self) -> Result<(), ResponderError> {
        Ok(())
    }

    /// Hand the call to the realtime model. Capture PCM goes up as it arrives
    /// and the model's audio comes back on `downlink`. The model decides when
    /// a turn starts and ends. `None` means this responder still answers one
    /// utterance at a time (the Google Meet wake path).
    async fn begin_live(&self, _context: &str) -> Result<Option<LiveSession>, ResponderError> {
        Ok(None)
    }

    /// Close a session opened by [`begin_live`](Self::begin_live). The default
    /// has nothing to close.
    async fn end_live(&self) {}
}

/// One chunk of call audio for a live realtime session. `pcm` is mono PCM16
/// at `sample_rate_hz` (the meeting capture rate).
#[derive(Debug, Clone)]
pub struct LiveUplinkChunk {
    pub pcm: bytes::Bytes,
    pub sample_rate_hz: u32,
}

/// What the realtime model sends back during a live call.
#[derive(Debug, Clone)]
pub enum LiveDownlink {
    /// 24 kHz mono PCM16, a piece of the spoken reply.
    Audio(bytes::Bytes),
    /// The reply text, when the session reports it.
    Text(String),
    /// The voice socket closed.
    Closed,
}

/// The two lanes of a live call. `uplink` accepts capture audio. `downlink`
/// yields the model's audio and text. Dropping `uplink` ends the stream.
pub struct LiveSession {
    pub uplink: mpsc::Sender<LiveUplinkChunk>,
    pub downlink: mpsc::Receiver<LiveDownlink>,
}

/// Responder that declines to speak — keeps the session testable before a real
/// LLM+TTS (or realtime) responder is wired.
pub struct NoopResponder;

#[async_trait]
impl MeetingResponder for NoopResponder {
    async fn respond(
        &self,
        _utterance: &str,
        _context: &str,
    ) -> Result<SpokenReply, ResponderError> {
        Ok(SpokenReply {
            text: String::new(),
            pcm: bytes::Bytes::new(),
            format: StreamAudioFormat::default(),
        })
    }
}

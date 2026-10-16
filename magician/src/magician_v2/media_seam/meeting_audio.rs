//! Audio I/O seams for a live meeting.
//!
//! Two directions, both implemented by the macOS native Core-Audio bridge (the
//! production replacement for the Spike-1 ffmpeg/sox path —
//! `scripts/meet-bot/transcribe_loop.py`):
//!
//! * [`AudioSource`] — meeting audio → the bot's ears. Captures the meeting
//!   output and pushes PCM16 chunks toward the [`StreamingSttProvider`] and the
//!   wake-word detector.
//! * [`AudioSink`] — the bot's voice → the meeting mic. Injects the responder's
//!   synthesized PCM16 so participants hear it.
//!
//! `MeetingSession` is written against these traits, so it stays testable with
//! the no-op impls here until the native bridge lands.
//!
//! [`StreamingSttProvider`]: crate::magician_v2::media_seam::streaming_stt::StreamingSttProvider

use async_trait::async_trait;
use tokio::sync::mpsc;

use crate::magician_v2::media_seam::{AudioChunk, StreamAudioFormat};

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("audio device error: {0}")]
    Device(String),
    #[error("audio transport closed")]
    Closed,
}

/// Which audio ScreenCaptureKit captures. PID is set after the bot launches
/// its own browser (Phase 2 auto-join); bundle id is the manual-join default.
#[derive(Debug, Clone)]
pub enum CaptureTarget {
    BundleId(String),
    Pid(i32),
    /// Whole-display (system) audio — required for engines whose audio SCK
    /// never attributes to their application (live-proven for the cloak
    /// Chromium engine: app-filtered capture of a playing tone measured 0.0
    /// while display-wide capture heard it at full level). Hears EVERY app's
    /// audio, including the bot's own injected voice, so the session runs
    /// half-duplex with this target (capture is dropped while the bot speaks).
    DisplayAudio,
}

/// Captures meeting audio and streams PCM16 chunks until the meeting ends.
///
/// Implementations own the capture device and run until `out` is dropped or the
/// source errors; chunks are in the session's `StreamAudioFormat` (16 kHz mono
/// PCM16 by default). Held as `Arc<dyn AudioSource>` so the session can drive
/// `run` on a spawned task.
#[async_trait]
pub trait AudioSource: Send + Sync {
    /// Begin capturing, pushing chunks to `out` until end-of-audio or error.
    async fn run(&self, out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError>;

    /// Late-bind the capture target (e.g. the just-launched browser's PID). No-op
    /// for sources that don't capture by target.
    async fn set_capture_target(&self, _target: CaptureTarget) {}
}

/// Plays the bot's synthesized PCM16 into the meeting mic.
#[async_trait]
pub trait AudioSink: Send + Sync {
    /// Inject one PCM16 buffer in `format`. Returns once the buffer is queued or
    /// played (implementation-defined).
    async fn play_pcm(
        &self,
        pcm: bytes::Bytes,
        format: StreamAudioFormat,
    ) -> Result<(), AudioError>;
}

/// Source that never yields audio — keeps the session loop testable.
pub struct NoopAudioSource;

#[async_trait]
impl AudioSource for NoopAudioSource {
    async fn run(&self, _out: mpsc::Sender<AudioChunk>) -> Result<(), AudioError> {
        Ok(())
    }
}

/// Sink that discards audio — the bot "speaks" into the void.
pub struct NoopAudioSink;

#[async_trait]
impl AudioSink for NoopAudioSink {
    async fn play_pcm(
        &self,
        _pcm: bytes::Bytes,
        _format: StreamAudioFormat,
    ) -> Result<(), AudioError> {
        Ok(())
    }
}

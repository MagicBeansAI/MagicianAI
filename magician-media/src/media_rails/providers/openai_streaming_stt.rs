//! OpenAI streaming-INGEST STT adapter for the meeting bot's "ears".
//!
//! The meeting-length audio stream is split into fixed-duration segments and
//! each segment is transcribed via the one-shot `/v1/audio/transcriptions`
//! endpoint (reusing [`OpenAiWhisperProvider`]), emitting one
//! [`StreamingSttEvent::Final`] per segment. This is NOT token-level streaming —
//! it's the proven, English-first cut from Spike 1: the live spike validated
//! `gpt-transcribe` on English (Hindi is best-effort, which the operator
//! accepted). A true-streaming, diarized adapter (Deepgram Nova-3) can drop in
//! behind the same [`StreamingSttProvider`] trait when EN+HI quality matters.
//!
//! Incoming chunks are raw little-endian PCM16 in the session's
//! [`StreamAudioFormat`] (default 16 kHz mono); each segment is wrapped in a
//! minimal WAV container before upload.

use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use super::openai_whisper::OpenAiWhisperProvider;
use super::streaming_stt::{
    AudioChunk, StreamAudioFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession,
};
use super::stt::{SttError, SttProvider, SttRequest};
pub use magician::magician_v2::media_seam::openai_streaming_stt::{pcm16_rms, DEFAULT_SILENCE_RMS};

pub const OPENAI_STREAMING_STT_PROVIDER_ID: &str = "openai-streaming";

/// Default seconds of audio per transcription segment. Smaller = lower wake-to-
/// reply latency (the wake phrase is only seen once its segment finalizes) but
/// risks splitting a long addressed question across two segments. Tunable per
/// run via `MEET_BOT_STT_SEGMENT_SECS`.
pub const OPENAI_STREAMING_DEFAULT_SEGMENT_SECS: f32 = 4.0;

/// Low-confidence outputs `gpt-transcribe` / Whisper hallucinate on silent or
/// near-silent segments — dropped rather than recorded as transcript turns.
const SILENCE_NOISE: &[&str] = &[
    "",
    "you",
    "thank you.",
    "thanks for watching!",
    "okay.",
    ".",
];

/// Opens segmenting OpenAI transcription sessions. One provider, many sessions.
#[derive(Clone)]
pub struct OpenAiStreamingSttProvider {
    whisper: Arc<OpenAiWhisperProvider>,
    segment_secs: f32,
}

impl OpenAiStreamingSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self::with_whisper(OpenAiWhisperProvider::new(api_key))
    }

    pub fn with_whisper(whisper: OpenAiWhisperProvider) -> Self {
        Self {
            whisper: Arc::new(whisper),
            segment_secs: OPENAI_STREAMING_DEFAULT_SEGMENT_SECS,
        }
    }

    /// Override the per-segment duration (clamped to ≥1 s).
    pub fn with_segment_secs(mut self, secs: f32) -> Self {
        self.segment_secs = secs.max(1.0);
        self
    }
}

#[async_trait]
impl StreamingSttProvider for OpenAiStreamingSttProvider {
    fn id(&self) -> &str {
        OPENAI_STREAMING_STT_PROVIDER_ID
    }

    fn label(&self) -> Option<&str> {
        Some("OpenAI streaming transcription")
    }

    fn default_model(&self) -> &str {
        self.whisper.default_model()
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities {
            end_of_utterance: true,
            ..StreamingSttCapabilities::default()
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let (tx, rx) = mpsc::channel::<SegMsg>(64);
        let worker = SegmentWorker {
            whisper: self.whisper.clone(),
            format,
            segment_bytes: segment_bytes(format, self.segment_secs),
            events,
        };
        tokio::spawn(worker.run(rx));
        Ok(Box::new(OpenAiStreamingSession { tx }))
    }
}

/// Internal control messages from the session handle to its segment worker.
enum SegMsg {
    Chunk(bytes::Bytes),
    Flush(oneshot::Sender<Result<(), SttError>>),
}

struct OpenAiStreamingSession {
    tx: mpsc::Sender<SegMsg>,
}

#[async_trait]
impl StreamingSttSession for OpenAiStreamingSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        self.tx
            .send(SegMsg::Chunk(chunk.pcm))
            .await
            .map_err(|_| SttError::Transport("streaming stt session closed".into()))
    }

    async fn finish(&self) -> Result<(), SttError> {
        let (ack_tx, ack_rx) = oneshot::channel();
        self.tx
            .send(SegMsg::Flush(ack_tx))
            .await
            .map_err(|_| SttError::Transport("streaming stt session closed before flush".into()))?;
        ack_rx
            .await
            .map_err(|_| SttError::Transport("streaming stt flush acknowledgement lost".into()))?
    }
}

/// Accumulates PCM, cuts segments at the configured size, and transcribes each.
struct SegmentWorker {
    whisper: Arc<OpenAiWhisperProvider>,
    format: StreamAudioFormat,
    segment_bytes: usize,
    events: mpsc::Sender<StreamingSttEvent>,
}

impl SegmentWorker {
    async fn run(self, mut rx: mpsc::Receiver<SegMsg>) {
        let mut pcm: Vec<u8> = Vec::with_capacity(self.segment_bytes.saturating_mul(2).max(4096));
        let mut seq: u64 = 0;
        let mut segment_failed = false;
        while let Some(msg) = rx.recv().await {
            match msg {
                SegMsg::Chunk(buf) => {
                    pcm.extend_from_slice(&buf);
                    while pcm.len() >= self.segment_bytes {
                        let seg: Vec<u8> = pcm.drain(..self.segment_bytes).collect();
                        if self.transcribe_segment(seg, &mut seq).await.is_err() {
                            segment_failed = true;
                        }
                    }
                },
                SegMsg::Flush(ack) => {
                    let result = if !pcm.is_empty() {
                        let seg = std::mem::take(&mut pcm);
                        self.transcribe_segment(seg, &mut seq).await
                    } else if segment_failed {
                        Err(SttError::Transport(
                            "one or more streaming transcription segments failed".into(),
                        ))
                    } else {
                        Ok(())
                    };
                    let _ = ack.send(result);
                    return;
                },
            }
        }
        // Channel dropped (provider gone): flush whatever tail remains.
        if !pcm.is_empty() {
            let seg = std::mem::take(&mut pcm);
            let _ = self.transcribe_segment(seg, &mut seq).await;
        }
    }

    async fn transcribe_segment(&self, pcm: Vec<u8>, seq: &mut u64) -> Result<(), SttError> {
        let n = *seq;
        *seq += 1;
        // Energy gate: never transcribe near-silence. Whisper-family models
        // HALLUCINATE plausible sentences on silent audio — on the passive
        // listener's mic track this floods the transcript with bogus `You:`
        // turns whenever the meeting app's mute leaves the OS input device
        // hot (app mute ≠ device mute), and on the system track it would
        // fabricate "activity" that defeats the silence auto-stop. The
        // phrase blocklist below stays as backup for borderline segments.
        let rms = pcm16_rms(&pcm);
        if rms < silence_rms_threshold() {
            debug!(
                target: "meet_bot",
                seq = n,
                rms,
                "streaming stt: skipped sub-threshold (silent) segment"
            );
            return Ok(());
        }
        let wav = pcm_to_wav(&pcm, self.format);
        let request = SttRequest {
            audio: bytes::Bytes::from(wav),
            content_type: "audio/wav".to_string(),
            language: None,
            model: None, // let the whisper provider use its configured default
            message_id: None,
            filename: Some(format!("segment_{n:05}.wav")),
            prompt: None,
        };
        match self.whisper.transcribe(request).await {
            Ok(resp) => {
                let text = resp.transcript.trim().to_string();
                if is_silence_noise(&text) {
                    debug!(target: "meet_bot", seq = n, "streaming stt: dropped silent/noise segment");
                    return Ok(());
                }
                self.events
                    .send(StreamingSttEvent::Final {
                        text,
                        speaker: None,
                        language: resp.language,
                        start_ms: None,
                    })
                    .await
                    .map_err(|_| {
                        SttError::Transport("streaming transcript receiver closed".into())
                    })?;
                Ok(())
            },
            // Definitive "no speech" verdict — skip, never a transcript turn.
            Err(SttError::NoSpeech) => Ok(()),
            Err(err) => {
                let reason = err.to_string();
                let _ = self.events.send(StreamingSttEvent::Error { reason }).await;
                Err(err)
            },
        }
    }
}

/// Bytes of PCM16 for `secs` seconds in the given format.
pub(super) fn segment_bytes(format: StreamAudioFormat, secs: f32) -> usize {
    let bytes_per_sec = format.sample_rate_hz as usize * format.channels.max(1) as usize * 2;
    ((bytes_per_sec as f32) * secs).round() as usize
}

pub(super) fn is_silence_noise(text: &str) -> bool {
    let lower = text.to_lowercase();
    SILENCE_NOISE.contains(&lower.as_str())
}

pub fn silence_rms_threshold() -> f64 {
    std::env::var("MEET_STT_SILENCE_RMS")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or(DEFAULT_SILENCE_RMS)
}

/// Wrap raw little-endian PCM16 samples in a minimal 44-byte WAV header so the
/// transcription endpoint sees a self-describing container.
pub(super) fn pcm_to_wav(pcm: &[u8], format: StreamAudioFormat) -> Vec<u8> {
    let channels = format.channels.max(1);
    let sample_rate = format.sample_rate_hz;
    let bits_per_sample: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * (bits_per_sample / 8) as u32;
    let block_align = channels * (bits_per_sample / 8);
    let data_len = pcm.len() as u32;
    let riff_len = 36u32.saturating_add(data_len);

    let mut out = Vec::with_capacity(44 + pcm.len());
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // audio format = PCM
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&bits_per_sample.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    out.extend_from_slice(pcm);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_header_is_well_formed() {
        let pcm = vec![0u8; 320]; // 10 ms @ 16 kHz mono PCM16
        let wav = pcm_to_wav(&pcm, StreamAudioFormat::default());
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(wav.len(), 44 + pcm.len());
        let sr = u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]);
        assert_eq!(sr, 16_000);
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len as usize, pcm.len());
    }

    #[test]
    fn segment_bytes_matches_duration() {
        // 16 kHz mono PCM16 → 32000 bytes/sec; 6 s → 192000.
        assert_eq!(
            segment_bytes(StreamAudioFormat::default(), 6.0),
            16_000 * 2 * 6
        );
    }

    #[test]
    fn silence_noise_is_filtered() {
        assert!(is_silence_noise("you"));
        assert!(is_silence_noise("Thank you."));
        assert!(is_silence_noise(""));
        assert!(!is_silence_noise("hey magical what is the plan"));
    }

    #[test]
    fn pcm16_rms_distinguishes_silence_from_signal() {
        assert_eq!(pcm16_rms(&[0; 32]), 0.0);
        let signal = [1_000i16.to_le_bytes(), (-1_000i16).to_le_bytes()].concat();
        assert_eq!(pcm16_rms(&signal), 1_000.0);
    }

    #[tokio::test]
    async fn finish_waits_for_worker_flush_ack_without_calling_cloud_for_silence() {
        let provider = OpenAiStreamingSttProvider::new("unused-test-key");
        let (events, mut received) = mpsc::channel(4);
        let session = provider
            .open_session(StreamAudioFormat::default(), events)
            .await
            .expect("open session");
        session
            .push_audio(AudioChunk {
                seq: 1,
                pcm: bytes::Bytes::from(vec![0; 320]),
            })
            .await
            .expect("queue silence");
        session.finish().await.expect("flush acknowledged");
        assert!(received.recv().await.is_none());
    }
}

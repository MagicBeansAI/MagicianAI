//! OpenAI streaming-INGEST STT adapter **with speaker diarization** — the
//! meeting bot's "ears", speaker-attributed.
//!
//! Like [`super::openai_streaming_stt`] it segments the meeting-length stream and
//! transcribes each window via `/v1/audio/transcriptions`, but it requests
//! `gpt-4o-transcribe-diarize` with `response_format=diarized_json` and emits one
//! [`StreamingSttEvent::Final`] **per speaker segment**, with `speaker`
//! populated — so the rolling transcript reads `[Speaker 1] …` / `[Speaker 2] …`
//! and the addressed (wake) utterance can be attributed to a speaker.
//!
//! Caveat — **per-window diarization**: speaker labels are stable WITHIN a window
//! but NOT guaranteed consistent across windows (the model re-labels each call
//! with no cross-call speaker memory). A larger `segment_secs` gives more
//! coherent labels at the cost of wake-to-reply latency. Globally-stable
//! identities across a long meeting need a true streaming-diarization engine
//! (e.g. Deepgram Nova-3) behind this same trait, or speaker-embedding matching
//! across windows. This adapter is a drop-in for [`StreamingSttProvider`]; select
//! it via [`MeetingSession::with_stt`](super::super::meeting::MeetingSession).
//!
//! Incoming chunks are raw little-endian PCM16 in the session's
//! [`StreamAudioFormat`] (default 16 kHz mono); each window is wrapped in WAV.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::{multipart, Client};
use serde::Deserialize;
use tokio::sync::mpsc;
use tracing::{debug, error};

use super::openai_streaming_stt::{
    is_silence_noise, pcm16_rms, pcm_to_wav, segment_bytes, silence_rms_threshold,
};
use super::streaming_stt::{
    AudioChunk, StreamAudioFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession,
};
use super::stt::SttError;

pub const OPENAI_DIARIZE_STT_PROVIDER_ID: &str = "openai-diarize";
pub const OPENAI_DIARIZE_DEFAULT_MODEL: &str = "gpt-4o-transcribe-diarize";
pub const OPENAI_DIARIZE_TRANSCRIPTIONS_URL: &str =
    "https://api.openai.com/v1/audio/transcriptions";
/// Default seconds of audio per diarized window. Larger than the plain adapter's
/// 4 s so each window carries enough multi-speaker context to attribute turns.
pub const OPENAI_DIARIZE_DEFAULT_SEGMENT_SECS: f32 = 8.0;

/// Opens segmenting OpenAI diarized-transcription sessions. One provider, many
/// concurrent sessions.
#[derive(Clone)]
pub struct OpenAiDiarizeStreamingSttProvider {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    segment_secs: f32,
}

impl OpenAiDiarizeStreamingSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            client: default_http_client(),
            api_key: api_key.into(),
            base_url: OPENAI_DIARIZE_TRANSCRIPTIONS_URL.to_string(),
            model: OPENAI_DIARIZE_DEFAULT_MODEL.to_string(),
            segment_secs: OPENAI_DIARIZE_DEFAULT_SEGMENT_SECS,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into();
        self
    }

    /// Override the diarization model (defaults to `gpt-4o-transcribe-diarize`).
    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    /// Override the per-window duration (clamped to ≥1 s).
    pub fn with_segment_secs(mut self, secs: f32) -> Self {
        self.segment_secs = secs.max(1.0);
        self
    }
}

#[async_trait]
impl StreamingSttProvider for OpenAiDiarizeStreamingSttProvider {
    fn id(&self) -> &str {
        OPENAI_DIARIZE_STT_PROVIDER_ID
    }

    fn label(&self) -> Option<&str> {
        Some("OpenAI streaming transcription with diarization")
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities {
            end_of_utterance: true,
            speaker_attribution: true,
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
            client: self.client.clone(),
            api_key: self.api_key.clone(),
            base_url: self.base_url.clone(),
            model: self.model.clone(),
            format,
            segment_bytes: segment_bytes(format, self.segment_secs),
            events,
        };
        tokio::spawn(worker.run(rx));
        Ok(Box::new(DiarizeSession { tx }))
    }
}

/// Internal control messages from the session handle to its segment worker.
enum SegMsg {
    Chunk(bytes::Bytes),
    Flush,
}

struct DiarizeSession {
    tx: mpsc::Sender<SegMsg>,
}

#[async_trait]
impl StreamingSttSession for DiarizeSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        self.tx
            .send(SegMsg::Chunk(chunk.pcm))
            .await
            .map_err(|_| SttError::Transport("diarize stt session closed".into()))
    }

    async fn finish(&self) -> Result<(), SttError> {
        let _ = self.tx.send(SegMsg::Flush).await;
        Ok(())
    }
}

/// Accumulates PCM, cuts windows at the configured size, and diarizes each.
struct SegmentWorker {
    client: Client,
    api_key: String,
    base_url: String,
    model: String,
    format: StreamAudioFormat,
    segment_bytes: usize,
    events: mpsc::Sender<StreamingSttEvent>,
}

impl SegmentWorker {
    async fn run(self, mut rx: mpsc::Receiver<SegMsg>) {
        let mut pcm: Vec<u8> = Vec::with_capacity(self.segment_bytes.saturating_mul(2).max(4096));
        let mut elapsed_ms: u64 = 0;
        while let Some(msg) = rx.recv().await {
            match msg {
                SegMsg::Chunk(buf) => {
                    pcm.extend_from_slice(&buf);
                    while pcm.len() >= self.segment_bytes {
                        let seg: Vec<u8> = pcm.drain(..self.segment_bytes).collect();
                        elapsed_ms = self.transcribe_segment(seg, elapsed_ms).await;
                    }
                },
                SegMsg::Flush => {
                    if !pcm.is_empty() {
                        let seg = std::mem::take(&mut pcm);
                        elapsed_ms = self.transcribe_segment(seg, elapsed_ms).await;
                    }
                },
            }
        }
        // Channel dropped (provider gone): flush whatever tail remains.
        if !pcm.is_empty() {
            let seg = std::mem::take(&mut pcm);
            let _ = self.transcribe_segment(seg, elapsed_ms).await;
        }
    }

    /// Diarize one window. Returns the new meeting-elapsed ms (window start +
    /// window duration) so per-segment offsets stay meeting-relative.
    async fn transcribe_segment(&self, pcm: Vec<u8>, window_start_ms: u64) -> u64 {
        let window_ms = pcm_duration_ms(pcm.len(), self.format);
        let rms = pcm16_rms(&pcm);
        if rms < silence_rms_threshold() {
            debug!(
                target: "meet_bot",
                rms,
                "diarized streaming stt: skipped sub-threshold (silent) segment"
            );
            return window_start_ms.saturating_add(window_ms);
        }
        let wav = pcm_to_wav(&pcm, self.format);
        match self.diarize(wav).await {
            Ok(resp) => {
                if !resp.segments.is_empty() {
                    for seg in &resp.segments {
                        let text = seg.text.trim();
                        if text.is_empty() || is_silence_noise(text) {
                            continue;
                        }
                        let start_ms = seg
                            .start
                            .map(|s| window_start_ms + (s.max(0.0) * 1000.0).round() as u64);
                        let _ = self
                            .events
                            .send(StreamingSttEvent::Final {
                                text: text.to_string(),
                                speaker: normalize_speaker(seg.speaker.as_deref()),
                                language: resp.language.clone(),
                                start_ms,
                            })
                            .await;
                    }
                } else if let Some(text) = resp.text.as_deref() {
                    // No speaker segments (model fell back to flat text) — still
                    // emit the transcript so context isn't lost.
                    let text = text.trim();
                    if !text.is_empty() && !is_silence_noise(text) {
                        let _ = self
                            .events
                            .send(StreamingSttEvent::Final {
                                text: text.to_string(),
                                speaker: None,
                                language: resp.language.clone(),
                                start_ms: Some(window_start_ms),
                            })
                            .await;
                    }
                }
            },
            // Definitive "no speech" verdict — skip, never a transcript turn.
            Err(SttError::NoSpeech) => {},
            Err(err) => {
                let _ = self
                    .events
                    .send(StreamingSttEvent::Error {
                        reason: err.to_string(),
                    })
                    .await;
            },
        }
        window_start_ms.saturating_add(window_ms)
    }

    /// POST one WAV window to `/v1/audio/transcriptions` with the diarize model +
    /// `diarized_json` and parse the speaker-attributed response.
    async fn diarize(&self, wav: Vec<u8>) -> Result<DiarizedResponse, SttError> {
        if wav.is_empty() {
            return Err(SttError::BadRequest("empty audio".into()));
        }
        let audio_part = multipart::Part::bytes(wav)
            .file_name("segment.wav")
            .mime_str("audio/wav")
            .map_err(|e| SttError::BadRequest(format!("invalid content type: {e}")))?;
        let form = multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "diarized_json")
            .part("file", audio_part);

        let response = self
            .client
            .post(&self.base_url)
            .bearer_auth(&self.api_key)
            .timeout(Duration::from_secs(180))
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                error!(target: "meet_bot", "[STT-DIARIZE] transport failure: {e}");
                SttError::Transport(e.to_string())
            })?;
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            error!(target: "meet_bot", "[STT-DIARIZE] upstream rejected status={status} body={body}");
            return Err(SttError::Upstream {
                status: status.as_u16(),
                body,
            });
        }
        let parsed: DiarizedResponse = response.json().await.map_err(|e| {
            error!(target: "meet_bot", "[STT-DIARIZE] decode failure: {e}");
            SttError::Transport(format!("decoding diarized response: {e}"))
        })?;
        debug!(
            target: "meet_bot",
            "[STT-DIARIZE] segments={} text_len={}",
            parsed.segments.len(),
            parsed.text.as_deref().map(str::len).unwrap_or(0)
        );
        Ok(parsed)
    }
}

/// Tolerant view of the `diarized_json` response: a flat `text` plus
/// speaker-attributed `segments`. Speaker/offset field names are accepted with
/// aliases so a minor API-shape change doesn't break parsing; absent segments
/// fall back to the flat `text`.
#[derive(Debug, Default, Deserialize)]
struct DiarizedResponse {
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    segments: Vec<DiarizedSegment>,
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DiarizedSegment {
    #[serde(default)]
    text: String,
    #[serde(
        default,
        alias = "speaker_id",
        alias = "speaker_label",
        alias = "speaker_name"
    )]
    speaker: Option<String>,
    #[serde(default, alias = "start_time")]
    start: Option<f64>,
}

/// Normalize a raw speaker label to a friendly display form: bare indices and
/// `speaker_N` map to a 1-based `Speaker N`; already-named labels pass through.
/// Empty/absent → None.
fn normalize_speaker(raw: Option<&str>) -> Option<String> {
    let raw = raw.map(str::trim).filter(|s| !s.is_empty())?;
    let digits = raw.trim_start_matches(|c: char| !c.is_ascii_digit());
    if let Ok(idx) = digits.parse::<u32>() {
        Some(format!("Speaker {}", idx.saturating_add(1)))
    } else {
        Some(raw.to_string())
    }
}

/// Milliseconds of audio in `len` bytes of PCM16 in the given format.
fn pcm_duration_ms(len: usize, format: StreamAudioFormat) -> u64 {
    let bytes_per_ms =
        (format.sample_rate_hz as usize * format.channels.max(1) as usize * 2) / 1000;
    if bytes_per_ms == 0 {
        return 0;
    }
    (len / bytes_per_ms) as u64
}

fn default_http_client() -> Client {
    Client::builder()
        .build()
        .expect("failed to build diarize STT HTTP client")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_diarized_segments() {
        let json = r#"{
            "text": "hello there how are you",
            "language": "en",
            "segments": [
                {"speaker": "speaker_0", "text": "hello there", "start": 0.0},
                {"speaker": "1", "text": "how are you", "start": 1.2}
            ]
        }"#;
        let parsed: DiarizedResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.segments.len(), 2);
        assert_eq!(
            normalize_speaker(parsed.segments[0].speaker.as_deref()),
            Some("Speaker 1".to_string())
        );
        assert_eq!(
            normalize_speaker(parsed.segments[1].speaker.as_deref()),
            Some("Speaker 2".to_string())
        );
    }

    #[test]
    fn falls_back_to_flat_text() {
        let json = r#"{"text":"just one line","language":"en"}"#;
        let parsed: DiarizedResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.segments.is_empty());
        assert_eq!(parsed.text.as_deref(), Some("just one line"));
    }

    #[test]
    fn named_speaker_passes_through() {
        assert_eq!(normalize_speaker(Some("Alice")), Some("Alice".to_string()));
        assert_eq!(normalize_speaker(Some("  ")), None);
        assert_eq!(normalize_speaker(None), None);
    }

    #[test]
    fn pcm_duration_ms_is_right() {
        // 16 kHz mono PCM16 → 32 bytes/ms; 32000 bytes → 1000 ms.
        assert_eq!(pcm_duration_ms(32_000, StreamAudioFormat::default()), 1000);
    }

    #[test]
    fn silence_gate_rejects_zero_pcm_before_diarization() {
        assert!(pcm16_rms(&vec![0; 32_000]) < silence_rms_threshold());
    }
}

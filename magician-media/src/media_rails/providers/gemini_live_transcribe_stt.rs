//! Gemini 3.5 Live Transcribe streaming-STT adapter.
//!
//! Opens Google's Live API WebSocket (`gemini-3.5-transcribe-live`), streams
//! 16 kHz PCM16, and maps `interimInputTranscription` / `inputTranscription`
//! onto Magician [`StreamingSttEvent`] partials and finals. This is speech-to-
//! text only — not Gemini Live assistant and not Live Translate.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use tracing::{info, warn};

use super::streaming_stt::{
    AudioChunk, StreamAudioFormat, StreamSampleFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession,
};
use super::stt::SttError;

pub const GEMINI_LIVE_TRANSCRIBE_PROVIDER_ID: &str = "gemini-live-transcribe";
pub const GEMINI_LIVE_TRANSCRIBE_DEFAULT_MODEL: &str = "gemini-3.5-transcribe-live";
pub const GEMINI_LIVE_TRANSCRIBE_DEFAULT_WS_URL: &str =
    "wss://generativelanguage.googleapis.com/ws/\
                                                         google.ai.generativelanguage.v1beta.\
                                                         GenerativeService.BidiGenerateContent";
const GEMINI_LIVE_TRANSCRIBE_PCM_RATE: u32 = 16_000;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const SETUP_TIMEOUT: Duration = Duration::from_secs(8);
const FINISH_TIMEOUT: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub struct GeminiLiveTranscribeSttProvider {
    api_key: String,
    websocket_url: String,
    provider_id: String,
    label: String,
    model: String,
    language_codes: Vec<String>,
}

impl GeminiLiveTranscribeSttProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            websocket_url: GEMINI_LIVE_TRANSCRIBE_DEFAULT_WS_URL.to_string(),
            provider_id: GEMINI_LIVE_TRANSCRIBE_PROVIDER_ID.to_string(),
            label: "Gemini · 3.5 Live Transcribe".to_string(),
            model: GEMINI_LIVE_TRANSCRIBE_DEFAULT_MODEL.to_string(),
            language_codes: Vec::new(),
        }
    }

    pub fn with_websocket_url(mut self, url: impl Into<String>) -> Self {
        self.websocket_url = url.into();
        self
    }

    pub fn with_provider_id(mut self, id: impl Into<String>) -> Self {
        self.provider_id = id.into();
        self
    }

    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_model(mut self, model: impl Into<String>) -> Self {
        self.model = model.into();
        self
    }

    pub fn with_language_codes(mut self, codes: Vec<String>) -> Self {
        self.language_codes = codes
            .into_iter()
            .map(|code| code.trim().to_string())
            .filter(|code| !code.is_empty())
            .collect();
        self
    }
}

#[async_trait]
impl StreamingSttProvider for GeminiLiveTranscribeSttProvider {
    fn id(&self) -> &str {
        &self.provider_id
    }

    fn label(&self) -> Option<&str> {
        Some(self.label.as_str())
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> StreamingSttCapabilities {
        StreamingSttCapabilities {
            partial_results: true,
            end_of_utterance: true,
            ..StreamingSttCapabilities::default()
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        events: mpsc::Sender<StreamingSttEvent>,
    ) -> Result<Box<dyn StreamingSttSession>, SttError> {
        let url = gemini_live_ws_url(&self.websocket_url, &self.api_key);
        let (ws, _) = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(&url))
            .await
            .map_err(|_| {
                SttError::Transport("Gemini Live Transcribe websocket handshake timed out".into())
            })?
            .map_err(|error| {
                SttError::Transport(format!("Gemini Live Transcribe connect: {error}"))
            })?;
        let (mut write, mut read) = ws.split();
        let setup = setup_payload(&self.model, &self.language_codes);
        write
            .send(Message::Text(setup.to_string()))
            .await
            .map_err(|error| {
                SttError::Transport(format!("Gemini Live Transcribe setup: {error}"))
            })?;

        let (audio_tx, mut audio_rx) = mpsc::channel::<LiveCmd>(64);
        let (setup_tx, setup_rx) = tokio::sync::oneshot::channel();
        let setup_tx = Arc::new(Mutex::new(Some(setup_tx)));
        let setup_notify = Arc::clone(&setup_tx);
        let events_reader = events.clone();
        let closed = Arc::new(AtomicBool::new(false));
        let closed_reader = Arc::clone(&closed);

        let reader = tokio::spawn(async move {
            while let Some(message) = read.next().await {
                let Ok(message) = message else {
                    let _ = events_reader
                        .send(StreamingSttEvent::Error {
                            reason: "Gemini Live Transcribe socket closed".into(),
                        })
                        .await;
                    break;
                };
                let text = match message {
                    Message::Text(text) => text,
                    Message::Binary(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let Ok(event) = serde_json::from_str::<Value>(&text) else {
                    continue;
                };
                if event.get("setupComplete").is_some() {
                    if let Some(tx) = setup_notify.lock().await.take() {
                        let _ = tx.send(());
                    }
                }
                for mapped in live_transcribe_events(&event) {
                    if events_reader.send(mapped).await.is_err() {
                        return;
                    }
                }
            }
            closed_reader.store(true, Ordering::SeqCst);
        });

        match tokio::time::timeout(SETUP_TIMEOUT, setup_rx).await {
            Ok(Ok(())) => {},
            Ok(Err(_)) => {
                reader.abort();
                return Err(SttError::Transport(
                    "Gemini Live Transcribe setup completed channel dropped".into(),
                ));
            },
            Err(_) => {
                reader.abort();
                return Err(SttError::Transport(
                    "Gemini Live Transcribe setupComplete timed out".into(),
                ));
            },
        }

        info!(
            model = %self.model,
            sample_rate = format.sample_rate_hz,
            "[STT-GEMINI-LIVE] session ready"
        );

        let writer = tokio::spawn(async move {
            while let Some(cmd) = audio_rx.recv().await {
                match cmd {
                    LiveCmd::Pcm(pcm) => {
                        if send_pcm(&mut write, &pcm).await.is_err() {
                            break;
                        }
                    },
                    LiveCmd::Finish => {
                        let _ = write
                            .send(Message::Text(
                                json!({ "realtimeInput": { "audioStreamEnd": true } }).to_string(),
                            ))
                            .await;
                        let _ = write.close().await;
                        break;
                    },
                }
            }
        });

        Ok(Box::new(GeminiLiveTranscribeSession {
            audio_tx,
            format,
            reader: Mutex::new(Some(reader)),
            writer: Mutex::new(Some(writer)),
            closed,
        }))
    }
}

enum LiveCmd {
    Pcm(Vec<u8>),
    Finish,
}

struct GeminiLiveTranscribeSession {
    audio_tx: mpsc::Sender<LiveCmd>,
    format: StreamAudioFormat,
    reader: Mutex<Option<JoinHandle<()>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
    closed: Arc<AtomicBool>,
}

#[async_trait]
impl StreamingSttSession for GeminiLiveTranscribeSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(SttError::Transport(
                "Gemini Live Transcribe session is closed".into(),
            ));
        }
        let pcm = to_pcm16_16k(&chunk.pcm, self.format);
        if pcm.is_empty() {
            return Ok(());
        }
        self.audio_tx
            .send(LiveCmd::Pcm(pcm))
            .await
            .map_err(|_| SttError::Transport("Gemini Live Transcribe writer exited".into()))
    }

    async fn finish(&self) -> Result<(), SttError> {
        let _ = self.audio_tx.send(LiveCmd::Finish).await;
        if let Some(writer) = self.writer.lock().await.take() {
            let _ = tokio::time::timeout(FINISH_TIMEOUT, writer).await;
        }
        if let Some(reader) = self.reader.lock().await.take() {
            let _ = tokio::time::timeout(FINISH_TIMEOUT, reader).await;
        }
        Ok(())
    }
}

async fn send_pcm<S>(write: &mut S, pcm: &[u8]) -> Result<(), ()>
where
    S: SinkExt<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    let payload = json!({
        "realtimeInput": {
            "audio": {
                "data": BASE64_STANDARD.encode(pcm),
                "mimeType": format!("audio/pcm;rate={GEMINI_LIVE_TRANSCRIBE_PCM_RATE}")
            }
        }
    });
    write
        .send(Message::Text(payload.to_string()))
        .await
        .map_err(|error| {
            warn!(error = %error, "[STT-GEMINI-LIVE] audio send failed");
        })
}

fn gemini_live_ws_url(base: &str, api_key: &str) -> String {
    let base = base.trim_end_matches('/');
    let key = urlencoding::encode(api_key);
    if base.contains('?') {
        format!("{base}&key={key}")
    } else {
        format!("{base}?key={key}")
    }
}

fn setup_payload(model: &str, language_codes: &[String]) -> Value {
    let model = model.trim();
    let model_path = if model.starts_with("models/") {
        model.to_string()
    } else {
        format!("models/{model}")
    };
    json!({
        "setup": {
            "model": model_path,
            "generationConfig": {
                "responseModalities": ["TEXT"]
            },
            "inputAudioTranscription": {
                "languageCodes": language_codes,
                "mode": "VERBATIM"
            }
        }
    })
}

fn live_transcribe_events(event: &Value) -> Vec<StreamingSttEvent> {
    let mut events = Vec::new();
    if let Some(error) = event.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| error.as_str())
            .unwrap_or("Gemini Live Transcribe error")
            .to_string();
        events.push(StreamingSttEvent::Error { reason: message });
        return events;
    }
    let Some(content) = event
        .get("serverContent")
        .or_else(|| event.get("server_content"))
    else {
        return events;
    };
    if let Some(text) = transcription_text(
        content,
        "interimInputTranscription",
        "interim_input_transcription",
    ) {
        events.push(StreamingSttEvent::Partial {
            text,
            speaker: None,
        });
    }
    if let Some(text) = transcription_text(content, "inputTranscription", "input_transcription") {
        events.push(StreamingSttEvent::Final {
            text,
            speaker: None,
            language: None,
            start_ms: None,
        });
    }
    events
}

fn transcription_text(content: &Value, camel: &str, snake: &str) -> Option<String> {
    content
        .get(camel)
        .or_else(|| content.get(snake))
        .and_then(|value| value.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

fn to_pcm16_16k(pcm: &Bytes, format: StreamAudioFormat) -> Vec<u8> {
    if pcm.is_empty() {
        return Vec::new();
    }
    let samples = match format.sample_format {
        StreamSampleFormat::PcmS16Le => pcm
            .chunks_exact(2)
            .map(|bytes| i16::from_le_bytes([bytes[0], bytes[1]]))
            .collect::<Vec<_>>(),
        StreamSampleFormat::PcmF32Le => pcm
            .chunks_exact(4)
            .map(|bytes| {
                let value = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                (value.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16
            })
            .collect::<Vec<_>>(),
    };
    let channels = format.channels.max(1) as usize;
    let mono = if channels == 1 {
        samples
    } else {
        samples
            .chunks(channels)
            .map(|frame| {
                let sum = frame.iter().map(|sample| i64::from(*sample)).sum::<i64>();
                (sum / frame.len() as i64) as i16
            })
            .collect()
    };
    let resampled = resample_pcm16(
        &mono,
        format.sample_rate_hz.max(1),
        GEMINI_LIVE_TRANSCRIBE_PCM_RATE,
    );
    resampled.into_iter().flat_map(i16::to_le_bytes).collect()
}

fn resample_pcm16(input: &[i16], source_rate: u32, target_rate: u32) -> Vec<i16> {
    if input.is_empty() || source_rate == target_rate {
        return input.to_vec();
    }
    let output_len =
        ((input.len() as u64 * u64::from(target_rate)) / u64::from(source_rate)).max(1) as usize;
    (0..output_len)
        .map(|index| {
            let source = index as f64 * source_rate as f64 / target_rate as f64;
            let lower = source.floor() as usize;
            let upper = (lower + 1).min(input.len().saturating_sub(1));
            let fraction = source - lower as f64;
            (input[lower] as f64 * (1.0 - fraction) + input[upper] as f64 * fraction).round() as i16
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_payload_uses_text_modality_and_top_level_transcription() {
        let payload = setup_payload("gemini-3.5-transcribe-live", &[]);
        assert_eq!(
            payload["setup"]["model"],
            "models/gemini-3.5-transcribe-live"
        );
        assert_eq!(
            payload["setup"]["generationConfig"]["responseModalities"],
            json!(["TEXT"])
        );
        assert!(payload["setup"]["inputAudioTranscription"].is_object());
        assert!(payload["setup"]["generationConfig"]["inputAudioTranscription"].is_null());
    }

    #[test]
    fn live_events_map_interim_and_final_transcripts() {
        let event = json!({
            "serverContent": {
                "interimInputTranscription": { "text": "hel" },
                "inputTranscription": { "text": "hello" }
            }
        });
        let events = live_transcribe_events(&event);
        assert!(matches!(
            events.as_slice(),
            [
                StreamingSttEvent::Partial { text, .. },
                StreamingSttEvent::Final { text: final_text, .. }
            ] if text == "hel" && final_text == "hello"
        ));
    }

    #[test]
    fn resample_24k_to_16k_preserves_duration() {
        let input = vec![0i16; 24_000];
        let output = resample_pcm16(&input, 24_000, 16_000);
        assert_eq!(output.len(), 16_000);
    }
}

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::media_rails::{
    AudioChunk, StreamAudioFormat, VadCapabilities, VadError, VadEvent, VadProvider, VadSession,
    VadSessionConfig,
};

use super::engine_manager::{FluidAudioEngineLease, FluidAudioEngineManager};
use super::protocol::{StreamClientControl, StreamServerEvent, FLUID_AUDIO_PROTOCOL_VERSION};

enum SessionCommand {
    Audio(AudioChunk),
    Finish(oneshot::Sender<Result<(), String>>),
}

pub struct FluidAudioVadProvider {
    id: String,
    label: Option<String>,
    model: String,
    manager: Arc<FluidAudioEngineManager>,
}

impl FluidAudioVadProvider {
    pub fn new(
        id: impl Into<String>,
        label: Option<String>,
        model: impl Into<String>,
        manager: Arc<FluidAudioEngineManager>,
    ) -> Self {
        Self {
            id: id.into(),
            label,
            model: model.into(),
            manager,
        }
    }
}

#[async_trait]
impl VadProvider for FluidAudioVadProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> VadCapabilities {
        VadCapabilities {
            probability_events: true,
            configurable_threshold: true,
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        config: VadSessionConfig,
        events: mpsc::Sender<VadEvent>,
    ) -> Result<Box<dyn VadSession>, VadError> {
        validate_session_config(&format, &config)?;
        let (token, lease) = self
            .manager
            .prepare_streaming_session(&self.id)
            .await
            .map_err(VadError::Unavailable)?;
        let ws_url = websocket_url(self.manager.endpoint())?;
        let mut request = ws_url.into_client_request().map_err(|error| {
            VadError::Session(format!("building FluidAudio WebSocket request: {error}"))
        })?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                VadError::Session(format!("building FluidAudio auth header: {error}"))
            })?,
        );
        request.headers_mut().insert(
            "x-magician-audio-protocol",
            HeaderValue::from_str(&FLUID_AUDIO_PROTOCOL_VERSION.to_string()).map_err(|error| {
                VadError::Session(format!("building FluidAudio protocol header: {error}"))
            })?,
        );
        let connection = tokio_tungstenite::connect_async(request);
        let mut connect_lease = lease.clone();
        let (socket, _) = tokio::select! {
            connection = connection => connection.map_err(|error| {
                VadError::Unavailable(format!("connecting to FluidAudio sidecar: {error}"))
            })?,
            _ = connect_lease.cancelled() => {
                return Err(VadError::Unavailable("FluidAudio engine is disabled".to_string()));
            },
        };
        let (commands, command_rx) = mpsc::channel(32);
        let model_id = self.id.clone();
        tokio::spawn(run_session(
            socket, command_rx, events, model_id, format, config, lease,
        ));
        Ok(Box::new(FluidAudioVadSession { commands }))
    }
}

struct FluidAudioVadSession {
    commands: mpsc::Sender<SessionCommand>,
}

#[async_trait]
impl VadSession for FluidAudioVadSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), VadError> {
        self.commands
            .send(SessionCommand::Audio(chunk))
            .await
            .map_err(|_| VadError::Session("FluidAudio VAD session is closed".to_string()))
    }

    async fn finish(&self) -> Result<(), VadError> {
        let (done_tx, done_rx) = oneshot::channel();
        self.commands
            .send(SessionCommand::Finish(done_tx))
            .await
            .map_err(|_| VadError::Session("FluidAudio VAD session is closed".to_string()))?;
        tokio::time::timeout(std::time::Duration::from_secs(5), done_rx)
            .await
            .map_err(|_| VadError::Session("FluidAudio VAD finish timed out".to_string()))?
            .map_err(|_| {
                VadError::Session("FluidAudio VAD finish acknowledgement dropped".to_string())
            })?
            .map_err(VadError::Session)
    }
}

async fn run_session<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    mut commands: mpsc::Receiver<SessionCommand>,
    events: mpsc::Sender<VadEvent>,
    model_id: String,
    format: StreamAudioFormat,
    config: VadSessionConfig,
    mut lease: FluidAudioEngineLease,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();
    let start = StreamClientControl::Start {
        protocol_version: FLUID_AUDIO_PROTOCOL_VERSION,
        stage: "vad",
        model_id,
        format,
        config: serde_json::to_value(config).unwrap_or_else(|_| serde_json::json!({})),
    };
    let start_json = match serde_json::to_string(&start) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(%error, "failed to serialize FluidAudio VAD start control");
            return;
        },
    };
    if sink.send(Message::Text(start_json)).await.is_err() {
        return;
    }

    let mut finish: Option<oneshot::Sender<Result<(), String>>> = None;
    loop {
        tokio::select! {
            _ = lease.cancelled() => {
                let _ = sink.send(Message::Close(None)).await;
                if let Some(done) = finish.take() {
                    let _ = done.send(Err("FluidAudio engine was disabled".to_string()));
                }
                break;
            },
            command = commands.recv(), if finish.is_none() => {
                match command {
                    Some(SessionCommand::Audio(chunk)) => {
                        if sink.send(Message::Binary(chunk.pcm.to_vec())).await.is_err() {
                            break;
                        }
                    },
                    Some(SessionCommand::Finish(done)) => {
                        finish = Some(done);
                        let stop = serde_json::to_string(&StreamClientControl::Stop)
                            .unwrap_or_else(|_| "{\"type\":\"stop\"}".to_string());
                        if let Err(error) = sink.send(Message::Text(stop)).await {
                            if let Some(done) = finish.take() {
                                let _ = done.send(Err(format!("sending FluidAudio stop: {error}")));
                            }
                            break;
                        }
                    },
                    None => break,
                }
            },
            message = stream.next() => {
                match message {
                    Some(Ok(Message::Text(text))) => {
                        match serde_json::from_str::<StreamServerEvent>(&text) {
                            Ok(StreamServerEvent::Ready) => {},
                            Ok(StreamServerEvent::Probability { value, at_ms }) => {
                                let _ = events.send(VadEvent::Probability { value, at_ms }).await;
                            },
                            Ok(StreamServerEvent::SpeechStarted { at_ms }) => {
                                let _ = events.send(VadEvent::SpeechStarted { at_ms }).await;
                            },
                            Ok(StreamServerEvent::SpeechEnded { at_ms }) => {
                                let _ = events.send(VadEvent::SpeechEnded { at_ms }).await;
                            },
                            Ok(StreamServerEvent::TranscriptPartial { .. }
                                | StreamServerEvent::TranscriptFinal { .. }
                                | StreamServerEvent::SpeakerStarted { .. }
                                | StreamServerEvent::SpeakerEnded { .. }
                                | StreamServerEvent::SegmentRevised { .. }) => {
                                tracing::warn!("FluidAudio VAD stream returned an event for another stage");
                                break;
                            },
                            Ok(StreamServerEvent::Finished) => {
                                if let Some(done) = finish.take() {
                                    let _ = done.send(Ok(()));
                                }
                                break;
                            },
                            Ok(StreamServerEvent::Error { code, message }) => {
                                let reason = format!("{code}: {message}");
                                if let Some(done) = finish.take() {
                                    let _ = done.send(Err(reason.clone()));
                                }
                                tracing::warn!(reason, "FluidAudio VAD stream failed");
                                break;
                            },
                            Err(error) => {
                                tracing::warn!(%error, payload = %text, "invalid FluidAudio VAD event");
                                break;
                            },
                        }
                    },
                    Some(Ok(Message::Close(_))) | None => break,
                    Some(Ok(_)) => {},
                    Some(Err(error)) => {
                        tracing::warn!(%error, "FluidAudio VAD WebSocket failed");
                        break;
                    },
                }
            }
        }
    }
    if let Some(done) = finish.take() {
        let _ = done.send(Err("FluidAudio VAD stream closed before finish".to_string()));
    }
}

fn websocket_url(endpoint: &str) -> Result<String, VadError> {
    if let Some(rest) = endpoint.strip_prefix("http://") {
        Ok(format!(
            "ws://{}/v1/audio/stream",
            rest.trim_end_matches('/')
        ))
    } else if let Some(rest) = endpoint.strip_prefix("https://") {
        Ok(format!(
            "wss://{}/v1/audio/stream",
            rest.trim_end_matches('/')
        ))
    } else {
        Err(VadError::InvalidConfig(
            "FluidAudio endpoint must use http:// or https://".to_string(),
        ))
    }
}

fn validate_session_config(
    format: &StreamAudioFormat,
    config: &VadSessionConfig,
) -> Result<(), VadError> {
    if !(8_000..=192_000).contains(&format.sample_rate_hz) || !(1..=8).contains(&format.channels) {
        return Err(VadError::InvalidConfig(
            "sample rate must be 8-192 kHz and channel count must be 1-8".to_string(),
        ));
    }
    if !config.threshold.is_finite() || !(0.0..=1.0).contains(&config.threshold) {
        return Err(VadError::InvalidConfig(
            "threshold must be between 0 and 1".to_string(),
        ));
    }
    if config.max_utterance_ms == 0 || config.min_speech_ms > config.max_utterance_ms {
        return Err(VadError::InvalidConfig(
            "max utterance must be positive and at least min speech".to_string(),
        ));
    }
    const MAX_SESSION_BOUNDARY_MS: u64 = 24 * 60 * 60 * 1_000;
    if config.max_utterance_ms > MAX_SESSION_BOUNDARY_MS
        || config.min_silence_ms > MAX_SESSION_BOUNDARY_MS
        || config.pre_roll_ms > MAX_SESSION_BOUNDARY_MS
        || config.hangover_ms > MAX_SESSION_BOUNDARY_MS
        || config
            .min_silence_ms
            .checked_add(config.hangover_ms)
            .is_none()
    {
        return Err(VadError::InvalidConfig(
            "VAD timing values cannot exceed 24 hours".to_string(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_session_config, websocket_url};
    use crate::media_rails::{StreamAudioFormat, StreamSampleFormat, VadError, VadSessionConfig};

    #[test]
    fn websocket_url_preserves_only_the_validated_sidecar_origin() {
        assert_eq!(
            websocket_url("http://127.0.0.1:3029").expect("loopback URL"),
            "ws://127.0.0.1:3029/v1/audio/stream"
        );
        assert_eq!(
            websocket_url("https://audio.example.test/").expect("TLS URL"),
            "wss://audio.example.test/v1/audio/stream"
        );
        assert!(matches!(
            websocket_url("file:///tmp/audio.sock"),
            Err(VadError::InvalidConfig(_))
        ));
    }

    #[test]
    fn session_validation_accepts_supported_pcm_and_rejects_unsafe_bounds() {
        let format = StreamAudioFormat {
            sample_rate_hz: 48_000,
            channels: 2,
            sample_format: StreamSampleFormat::PcmF32Le,
        };
        validate_session_config(&format, &VadSessionConfig::default()).expect("valid session");

        let invalid_format = StreamAudioFormat {
            sample_rate_hz: 7_999,
            ..format
        };
        assert!(matches!(
            validate_session_config(&invalid_format, &VadSessionConfig::default()),
            Err(VadError::InvalidConfig(_))
        ));

        let invalid_threshold = VadSessionConfig {
            threshold: f32::NAN,
            ..VadSessionConfig::default()
        };
        assert!(matches!(
            validate_session_config(&format, &invalid_threshold),
            Err(VadError::InvalidConfig(_))
        ));

        let invalid_timing = VadSessionConfig {
            min_speech_ms: 2_000,
            max_utterance_ms: 1_000,
            ..VadSessionConfig::default()
        };
        assert!(matches!(
            validate_session_config(&format, &invalid_timing),
            Err(VadError::InvalidConfig(_))
        ));

        let unbounded_timing = VadSessionConfig {
            hangover_ms: 24 * 60 * 60 * 1_000 + 1,
            ..VadSessionConfig::default()
        };
        assert!(matches!(
            validate_session_config(&format, &unbounded_timing),
            Err(VadError::InvalidConfig(_))
        ));
    }
}

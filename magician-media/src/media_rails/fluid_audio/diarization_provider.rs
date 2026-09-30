use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::media_rails::{
    AudioChunk, DiarizationCapabilities, DiarizationError, DiarizationEvent, DiarizationProvider,
    DiarizationSession, DiarizationSessionConfig, SpeakerSegment, StreamAudioFormat,
};

use super::engine_manager::{FluidAudioEngineLease, FluidAudioEngineManager};
use super::protocol::{StreamClientControl, StreamServerEvent, FLUID_AUDIO_PROTOCOL_VERSION};

enum SessionCommand {
    Audio(AudioChunk),
    Finish(oneshot::Sender<Result<(), String>>),
}

pub struct FluidAudioDiarizationProvider {
    id: String,
    label: Option<String>,
    model: String,
    manager: Arc<FluidAudioEngineManager>,
}

impl FluidAudioDiarizationProvider {
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
impl DiarizationProvider for FluidAudioDiarizationProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    fn default_model(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> DiarizationCapabilities {
        DiarizationCapabilities {
            online_revisions: true,
            max_speakers: Some(4),
        }
    }

    async fn open_session(
        &self,
        format: StreamAudioFormat,
        config: DiarizationSessionConfig,
        events: mpsc::Sender<DiarizationEvent>,
    ) -> Result<Box<dyn DiarizationSession>, DiarizationError> {
        if !(8_000..=192_000).contains(&format.sample_rate_hz)
            || !(1..=8).contains(&format.channels)
        {
            return Err(DiarizationError::InvalidConfig(
                "FluidAudio input must be 8-192 kHz with 1-8 channels".to_string(),
            ));
        }
        if config
            .expected_speakers
            .is_some_and(|count| !(1..=4).contains(&count))
        {
            return Err(DiarizationError::InvalidConfig(
                "FluidAudio Sortformer supports 1-4 expected speakers".to_string(),
            ));
        }
        let (token, lease) = self
            .manager
            .prepare_streaming_session(&self.id)
            .await
            .map_err(DiarizationError::Unavailable)?;
        let mut request = websocket_url(self.manager.endpoint())?
            .into_client_request()
            .map_err(|error| DiarizationError::Session(format!("building request: {error}")))?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).map_err(|error| {
                DiarizationError::Session(format!("building auth header: {error}"))
            })?,
        );
        request.headers_mut().insert(
            "x-magician-audio-protocol",
            HeaderValue::from_str(&FLUID_AUDIO_PROTOCOL_VERSION.to_string()).map_err(|error| {
                DiarizationError::Session(format!("building protocol header: {error}"))
            })?,
        );
        let connection = tokio_tungstenite::connect_async(request);
        let mut connect_lease = lease.clone();
        let (socket, _) = tokio::select! {
            connection = connection => connection.map_err(|error| {
                DiarizationError::Unavailable(format!("connecting to FluidAudio: {error}"))
            })?,
            _ = connect_lease.cancelled() => {
                return Err(DiarizationError::Unavailable("FluidAudio engine is disabled".to_string()));
            },
        };
        let (commands, receiver) = mpsc::channel(32);
        tokio::spawn(run_session(
            socket,
            receiver,
            events,
            self.id.clone(),
            format,
            config,
            lease,
        ));
        Ok(Box::new(FluidAudioDiarizationSession { commands }))
    }
}

struct FluidAudioDiarizationSession {
    commands: mpsc::Sender<SessionCommand>,
}

#[async_trait]
impl DiarizationSession for FluidAudioDiarizationSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), DiarizationError> {
        self.commands
            .send(SessionCommand::Audio(chunk))
            .await
            .map_err(|_| {
                DiarizationError::Session("FluidAudio diarization session closed".to_string())
            })
    }

    async fn finish(&self) -> Result<(), DiarizationError> {
        let (done, acknowledgement) = oneshot::channel();
        self.commands
            .send(SessionCommand::Finish(done))
            .await
            .map_err(|_| {
                DiarizationError::Session("FluidAudio diarization session closed".to_string())
            })?;
        tokio::time::timeout(std::time::Duration::from_secs(15), acknowledgement)
            .await
            .map_err(|_| {
                DiarizationError::Session("FluidAudio diarization finish timed out".to_string())
            })?
            .map_err(|_| {
                DiarizationError::Session("FluidAudio finish acknowledgement dropped".to_string())
            })?
            .map_err(DiarizationError::Session)
    }
}

async fn run_session<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    mut commands: mpsc::Receiver<SessionCommand>,
    events: mpsc::Sender<DiarizationEvent>,
    model_id: String,
    format: StreamAudioFormat,
    config: DiarizationSessionConfig,
    mut lease: FluidAudioEngineLease,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();
    let start = StreamClientControl::Start {
        protocol_version: FLUID_AUDIO_PROTOCOL_VERSION,
        stage: "diarization",
        model_id,
        format,
        config: serde_json::json!({ "expected_speakers": config.expected_speakers }),
    };
    if sink
        .send(Message::Text(
            serde_json::to_string(&start).unwrap_or_else(|_| "{}".to_string()),
        ))
        .await
        .is_err()
    {
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
            command = commands.recv(), if finish.is_none() => match command {
                Some(SessionCommand::Audio(chunk)) => {
                    if sink.send(Message::Binary(chunk.pcm.to_vec())).await.is_err() { break; }
                },
                Some(SessionCommand::Finish(done)) => {
                    finish = Some(done);
                    if sink.send(Message::Text("{\"type\":\"stop\"}".to_string())).await.is_err() { break; }
                },
                None => break,
            },
            message = stream.next() => match message {
                Some(Ok(Message::Text(text))) => match serde_json::from_str::<StreamServerEvent>(&text) {
                    Ok(StreamServerEvent::Ready) => {},
                    Ok(StreamServerEvent::SpeakerStarted { speaker_id, at_ms }) => {
                        let _ = events.send(DiarizationEvent::SpeakerStarted { speaker_id, at_ms }).await;
                    },
                    Ok(StreamServerEvent::SpeakerEnded { speaker_id, at_ms }) => {
                        let _ = events.send(DiarizationEvent::SpeakerEnded { speaker_id, at_ms }).await;
                    },
                    Ok(StreamServerEvent::SegmentRevised {
                        speaker_id, start_ms, end_ms, confidence,
                    }) => {
                        let _ = events.send(DiarizationEvent::SegmentRevised {
                            segment: SpeakerSegment { speaker_id, start_ms, end_ms, confidence },
                        }).await;
                    },
                    Ok(StreamServerEvent::Finished) => {
                        if let Some(done) = finish.take() { let _ = done.send(Ok(())); }
                        break;
                    },
                    Ok(StreamServerEvent::Error { code, message }) => {
                        if let Some(done) = finish.take() { let _ = done.send(Err(format!("{code}: {message}"))); }
                        break;
                    },
                    Ok(_) | Err(_) => break,
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {},
                Some(Err(_)) => break,
            },
        }
    }
    if let Some(done) = finish.take() {
        let _ = done.send(Err("FluidAudio stream closed before finish".to_string()));
    }
}

fn websocket_url(endpoint: &str) -> Result<String, DiarizationError> {
    endpoint
        .strip_prefix("http://")
        .map(|rest| format!("ws://{}/v1/audio/stream", rest.trim_end_matches('/')))
        .or_else(|| {
            endpoint
                .strip_prefix("https://")
                .map(|rest| format!("wss://{}/v1/audio/stream", rest.trim_end_matches('/')))
        })
        .ok_or_else(|| {
            DiarizationError::InvalidConfig("FluidAudio endpoint must use HTTP(S)".to_string())
        })
}

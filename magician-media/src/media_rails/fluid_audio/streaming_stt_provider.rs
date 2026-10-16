use std::sync::Arc;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::media_rails::{
    AudioChunk, StreamAudioFormat, StreamingSttCapabilities, StreamingSttEvent,
    StreamingSttProvider, StreamingSttSession, SttError,
};

use super::engine_manager::{FluidAudioEngineLease, FluidAudioEngineManager};
use super::protocol::{StreamClientControl, StreamServerEvent, FLUID_AUDIO_PROTOCOL_VERSION};

enum SessionCommand {
    Audio(AudioChunk),
    Finish(oneshot::Sender<Result<(), String>>),
}

pub struct FluidAudioStreamingSttProvider {
    id: String,
    label: Option<String>,
    model: String,
    language: Option<String>,
    manager: Arc<FluidAudioEngineManager>,
}

impl FluidAudioStreamingSttProvider {
    pub fn new(
        id: impl Into<String>,
        label: Option<String>,
        model: impl Into<String>,
        language: Option<String>,
        manager: Arc<FluidAudioEngineManager>,
    ) -> Self {
        Self {
            id: id.into(),
            label,
            model: model.into(),
            language,
            manager,
        }
    }
}

#[async_trait]
impl StreamingSttProvider for FluidAudioStreamingSttProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn label(&self) -> Option<&str> {
        self.label.as_deref()
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
        validate_format(format)?;
        let (token, lease) = self
            .manager
            .prepare_streaming_session(&self.id)
            .await
            .map_err(SttError::NotConfigured)?;
        let mut request = websocket_url(self.manager.endpoint())?
            .into_client_request()
            .map_err(|error| {
                SttError::Transport(format!("building FluidAudio request: {error}"))
            })?;
        request.headers_mut().insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|error| SttError::Transport(format!("building auth header: {error}")))?,
        );
        request.headers_mut().insert(
            "x-magician-audio-protocol",
            HeaderValue::from_str(&FLUID_AUDIO_PROTOCOL_VERSION.to_string()).map_err(|error| {
                SttError::Transport(format!("building protocol header: {error}"))
            })?,
        );
        let connection = tokio_tungstenite::connect_async(request);
        let mut connect_lease = lease.clone();
        let (socket, _) = tokio::select! {
            connection = connection => connection
                .map_err(|error| SttError::Transport(format!("connecting to FluidAudio: {error}")))?,
            _ = connect_lease.cancelled() => {
                return Err(SttError::NotConfigured("FluidAudio engine is disabled".to_string()));
            },
        };
        let (commands, receiver) = mpsc::channel(32);
        let task = tokio::spawn(run_session(
            socket,
            receiver,
            events,
            self.id.clone(),
            format,
            self.language.clone(),
            lease,
        ));
        Ok(Box::new(FluidAudioStreamingSttSession {
            commands,
            task: Mutex::new(Some(task)),
        }))
    }
}

struct FluidAudioStreamingSttSession {
    commands: mpsc::Sender<SessionCommand>,
    /// The session's background task. `finish()` awaits it so the engine lease and
    /// the sidecar socket are fully released (freeing the model's streaming slot)
    /// before returning — otherwise a caller that immediately reopens a stream on
    /// the same model collides with the still-active old stream ("model has active
    /// session").
    task: Mutex<Option<JoinHandle<()>>>,
}

#[async_trait]
impl StreamingSttSession for FluidAudioStreamingSttSession {
    async fn push_audio(&self, chunk: AudioChunk) -> Result<(), SttError> {
        self.commands
            .send(SessionCommand::Audio(chunk))
            .await
            .map_err(|_| upstream("FluidAudio streaming STT session closed"))
    }

    async fn finish(&self) -> Result<(), SttError> {
        let (done, acknowledgement) = oneshot::channel();
        self.commands
            .send(SessionCommand::Finish(done))
            .await
            .map_err(|_| upstream("FluidAudio streaming STT session closed"))?;
        let result = tokio::time::timeout(std::time::Duration::from_secs(15), acknowledgement)
            .await
            .map_err(|_| upstream("FluidAudio streaming STT finish timed out"))?
            .map_err(|_| upstream("FluidAudio finish acknowledgement dropped"))?
            .map_err(|reason| upstream(reason));
        // Wait for the session task to actually return — dropping the engine lease
        // and closing the sidecar socket — so the model's streaming slot is freed
        // before we hand control back. Bounded so a stuck task can't hang finish().
        self.await_teardown().await;
        result
    }
}

impl FluidAudioStreamingSttSession {
    async fn await_teardown(&self) {
        let handle = self.task.lock().await.take();
        if let Some(handle) = handle {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), handle).await;
        }
    }
}

async fn run_session<S>(
    socket: tokio_tungstenite::WebSocketStream<S>,
    mut commands: mpsc::Receiver<SessionCommand>,
    events: mpsc::Sender<StreamingSttEvent>,
    model_id: String,
    format: StreamAudioFormat,
    language: Option<String>,
    mut lease: FluidAudioEngineLease,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let (mut sink, mut stream) = socket.split();
    let start = StreamClientControl::Start {
        protocol_version: FLUID_AUDIO_PROTOCOL_VERSION,
        stage: "streaming_stt",
        model_id,
        format,
        config: serde_json::json!({
            "language": language,
            "eou_debounce_ms": 1_280,
        }),
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
                let reason = "FluidAudio engine was disabled".to_string();
                let _ = events.send(StreamingSttEvent::Error { reason: reason.clone() }).await;
                let _ = sink.send(Message::Close(None)).await;
                if let Some(done) = finish.take() { let _ = done.send(Err(reason)); }
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
                    Ok(StreamServerEvent::TranscriptPartial { text, .. }) => {
                        let _ = events.send(StreamingSttEvent::Partial { text, speaker: None }).await;
                    },
                    Ok(StreamServerEvent::TranscriptFinal { text, language, start_ms, .. }) => {
                        let _ = events.send(StreamingSttEvent::Final {
                            text,
                            speaker: None,
                            language,
                            start_ms: Some(start_ms),
                        }).await;
                    },
                    Ok(StreamServerEvent::Finished) => {
                        // Close the socket gracefully so the sidecar frees the
                        // model's streaming slot promptly (not only on drop).
                        let _ = sink.send(Message::Close(None)).await;
                        if let Some(done) = finish.take() { let _ = done.send(Ok(())); }
                        break;
                    },
                    Ok(StreamServerEvent::Error { code, message }) => {
                        let reason = format!("{code}: {message}");
                        let _ = events.send(StreamingSttEvent::Error { reason: reason.clone() }).await;
                        if let Some(done) = finish.take() { let _ = done.send(Err(reason)); }
                        break;
                    },
                    Ok(_) => {
                        let reason = "FluidAudio streaming STT returned an event for another stage".to_string();
                        let _ = events.send(StreamingSttEvent::Error { reason }).await;
                        break;
                    },
                    Err(error) => {
                        let _ = events.send(StreamingSttEvent::Error {
                            reason: format!("invalid FluidAudio streaming STT event: {error}"),
                        }).await;
                        break;
                    },
                },
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {},
                Some(Err(error)) => {
                    let _ = events.send(StreamingSttEvent::Error {
                        reason: format!("FluidAudio streaming STT WebSocket failed: {error}"),
                    }).await;
                    break;
                },
            },
        }
    }
    if let Some(done) = finish.take() {
        let _ = done.send(Err("FluidAudio stream closed before finish".to_string()));
    }
}

fn websocket_url(endpoint: &str) -> Result<String, SttError> {
    endpoint
        .strip_prefix("http://")
        .map(|rest| format!("ws://{}/v1/audio/stream", rest.trim_end_matches('/')))
        .or_else(|| {
            endpoint
                .strip_prefix("https://")
                .map(|rest| format!("wss://{}/v1/audio/stream", rest.trim_end_matches('/')))
        })
        .ok_or_else(|| SttError::NotConfigured("FluidAudio endpoint must use HTTP(S)".to_string()))
}

fn validate_format(format: StreamAudioFormat) -> Result<(), SttError> {
    if !(8_000..=192_000).contains(&format.sample_rate_hz) || !(1..=8).contains(&format.channels) {
        return Err(SttError::BadRequest(
            "FluidAudio input must be 8-192 kHz with 1-8 channels".to_string(),
        ));
    }
    Ok(())
}

fn upstream(reason: impl Into<String>) -> SttError {
    SttError::Upstream {
        status: 502,
        body: reason.into(),
    }
}

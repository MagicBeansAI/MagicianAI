//! OpenAI GPT-Live-1 provider (`wss://api.openai.com/v1/live/sessions`).
//!
//! Full-duplex speech frontend. Magician is the Live "engine" (brains) via
//! `delegation.type = client`: Live asks for help with
//! `session.delegation.created`, Magician runs `delegate_to_chat`, and the
//! speakable result returns as `session.commentary.append`.
//!
//! That brain is Magician's chat turn, which follows `chat.harness_engine`
//! (Magician or Claude Code / Codex / Grok / Agy). Magician plane tools stay
//! the hands.
//!
//! This is not Realtime. There is no `response.create` turn loop, no
//! `input_audio_buffer.commit`, and no in-session function catalog.
//! Downstream PCM is paced into ~40 ms frames with an 80 ms preroll so
//! mobile players do not underrun on Live's tiny `output_audio.delta`s.

use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{engine::general_purpose::STANDARD as BASE64_STANDARD, Engine as _};
use dashmap::DashMap;
use futures_util::{Sink, SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
};
use tracing::{info, warn};
use ulid::Ulid;

use crate::realtime::provider::RealtimeProvider;
use crate::realtime::types::{
    AudioStreamChannel, RealtimeAudioControl, RealtimeAudioTopology, RealtimeProviderError,
    RealtimeProviderEvent, RealtimeProviderKind, RealtimeSessionDescriptor,
};

pub const OPENAI_LIVE_PROVIDER_ID: &str = "openai-live";
pub const OPENAI_LIVE_DEFAULT_MODEL: &str = "gpt-live-1";
pub const OPENAI_LIVE_DEFAULT_VOICE: &str = "marin";
pub const OPENAI_LIVE_DEFAULT_WEBSOCKET_URL: &str = "wss://api.openai.com/v1/live/sessions";
const OPENAI_LIVE_MAX_SESSION_SECS: u64 = 30 * 60;
const OPENAI_LIVE_CONNECT_TIMEOUT: Duration = Duration::from_secs(12);
const AUDIO_CHANNEL_CAPACITY: usize = 64;
const CONTROL_CHANNEL_CAPACITY: usize = 16;
const EVENT_CHANNEL_CAPACITY: usize = 64;
const PENDING_AFTER_START_MAX_BYTES: usize = 1024 * 1024;
/// 24 kHz mono PCM16 = 48 bytes/ms. Live deltas are often a few ms; iOS
/// hops each binary frame onto the main actor before `scheduleBuffer`, and
/// Android `AudioTrack.write`s them immediately, so sub-frame chunks underrun.
const LIVE_PCM_BYTES_PER_MS: usize = 48;
const OUTPUT_FRAME_MS: usize = 40;
const OUTPUT_FRAME_BYTES: usize = LIVE_PCM_BYTES_PER_MS * OUTPUT_FRAME_MS;
const OUTPUT_PREROLL_MS: usize = 80;
const OUTPUT_PREROLL_BYTES: usize = LIVE_PCM_BYTES_PER_MS * OUTPUT_PREROLL_MS;
const OUTPUT_FLUSH_AFTER: Duration = Duration::from_millis(25);
const OUTPUT_PACE_TICK: Duration = Duration::from_millis(20);
/// Fallback `session.instructions` when prompt-manager render is
/// unavailable. Keep in sync with `voice_live_mouth_system` v1.0.0.
pub const OPENAI_LIVE_DEFAULT_INSTRUCTIONS: &str = "\
You are Magican, a calm, friendly voice assistant for Magican.\n\
Speak warmly and naturally, at an unhurried pace. Be clear and direct, not overly cheerful.\n\
If the user is frustrated, acknowledge it briefly and focus on the next helpful step.\n\
\n\
Backchannel policy: Use moderate backchannels. Acknowledge naturally without competing with the main response.\n\
\n\
Interruption policy: Stop speaking when the user interrupts. Listen to what they say.\n\
\n\
Delegation policy:\n\
Backend tools:\n\
- Magician: memory, mail, calendar, files, search, tasks, and Magician tools.\n\
- Reasoning: careful or multi-step answers that need Magician's chat turn.\n\
\n\
Delegate to the backend when:\n\
- The request needs a Magician capability or careful reasoning.\n\
- A correction changes work already requested.\n\
\n\
Do not delegate to the backend when:\n\
- You can answer from the conversation or a still-current Magician result.\n\
- You need a brief clarification to understand the request.\n\
\n\
Delegate before giving an answer that depends on Magician.\n\
Do not guess the result while waiting.";

#[derive(Debug, Clone)]
pub struct OpenAiLiveProvider {
    api_key: String,
    websocket_url: String,
    default_model: String,
    default_voice: Option<String>,
    max_session_duration_secs: Option<u64>,
    sessions: Arc<DashMap<String, mpsc::Sender<RealtimeAudioControl>>>,
}

impl OpenAiLiveProvider {
    pub fn new(api_key: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            websocket_url: OPENAI_LIVE_DEFAULT_WEBSOCKET_URL.to_string(),
            default_model: OPENAI_LIVE_DEFAULT_MODEL.to_string(),
            default_voice: Some(OPENAI_LIVE_DEFAULT_VOICE.to_string()),
            max_session_duration_secs: Some(OPENAI_LIVE_MAX_SESSION_SECS),
            sessions: Arc::new(DashMap::new()),
        }
    }

    pub fn with_websocket_url(mut self, url: impl Into<String>) -> Self {
        self.websocket_url = url.into();
        self
    }

    pub fn with_defaults(mut self, model: impl Into<String>, voice: Option<String>) -> Self {
        self.default_model = model.into();
        self.default_voice = voice;
        self
    }

    pub fn with_max_session_duration_secs(mut self, secs: Option<u64>) -> Self {
        self.max_session_duration_secs = secs;
        self
    }
}

#[async_trait]
impl RealtimeProvider for OpenAiLiveProvider {
    fn id(&self) -> &str {
        OPENAI_LIVE_PROVIDER_ID
    }

    fn kind(&self) -> RealtimeProviderKind {
        RealtimeProviderKind::OpenAiLive
    }

    fn default_model(&self) -> &str {
        &self.default_model
    }

    fn audio_topology(&self) -> RealtimeAudioTopology {
        RealtimeAudioTopology::BackendProxied
    }

    fn max_session_duration_secs(&self) -> Option<u64> {
        self.max_session_duration_secs
    }

    async fn create_session(
        &self,
        _principal: &str,
        _workspace: &str,
        voice_session_id: &str,
        _thread_id: Option<&str>,
        preferred_voice: Option<&str>,
    ) -> Result<RealtimeSessionDescriptor, RealtimeProviderError> {
        Ok(RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAiLive,
            model: self.default_model.clone(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: crate::config::RealtimeVoiceMode::Assistant,
            voice: preferred_voice
                .and_then(|value| crate::realtime::canonical_realtime_voice("openai_live", value))
                .or_else(|| self.default_voice.clone()),
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: Some(format!("openai-live:{voice_session_id}")),
            max_session_duration_secs: self.max_session_duration_secs,
            native_resume_handle: None,
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: Some("server_vad".to_string()),
            context_window_tokens: Some(128_000),
            half_duplex: None,
        })
    }

    async fn open_proxied_audio(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<AudioStreamChannel, RealtimeProviderError> {
        let upstream_session_id = descriptor
            .upstream_provider_session_id
            .clone()
            .unwrap_or_else(|| format!("openai-live:unknown:{}", Ulid::new()));
        let (upstream_tx, upstream_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
        let (downstream_tx, downstream_rx) = mpsc::channel(AUDIO_CHANNEL_CAPACITY);
        let (control_tx, control_rx) = mpsc::channel(CONTROL_CHANNEL_CAPACITY);
        let (events_tx, events_rx) = mpsc::channel(EVENT_CHANNEL_CAPACITY);
        self.sessions
            .insert(upstream_session_id.clone(), control_tx.clone());
        let task = OpenAiLiveTask {
            api_key: self.api_key.clone(),
            websocket_url: self.websocket_url.clone(),
            model: descriptor.model.clone(),
            voice: descriptor
                .voice
                .clone()
                .or_else(|| self.default_voice.clone()),
            upstream_session_id: upstream_session_id.clone(),
            sessions: Arc::clone(&self.sessions),
        };
        let cleanup_sessions = Arc::clone(&self.sessions);
        let cleanup_id = upstream_session_id.clone();
        tokio::spawn(async move {
            if let Err(error) = task
                .run(upstream_rx, downstream_tx, control_rx, events_tx.clone())
                .await
            {
                let _ = events_tx
                    .send(RealtimeProviderEvent::TransportClosed {
                        message: error.to_string(),
                    })
                    .await;
            }
            cleanup_sessions.remove(&cleanup_id);
        });
        Ok(AudioStreamChannel {
            upstream_tx,
            downstream_rx,
            control_tx,
            events_rx,
        })
    }

    async fn close_session(
        &self,
        descriptor: &RealtimeSessionDescriptor,
    ) -> Result<(), RealtimeProviderError> {
        let Some(session_id) = descriptor.upstream_provider_session_id.as_ref() else {
            return Ok(());
        };
        if let Some((_, tx)) = self.sessions.remove(session_id) {
            let _ = tx.send(RealtimeAudioControl::End).await;
        }
        Ok(())
    }
}

struct OpenAiLiveTask {
    api_key: String,
    websocket_url: String,
    model: String,
    voice: Option<String>,
    upstream_session_id: String,
    sessions: Arc<DashMap<String, mpsc::Sender<RealtimeAudioControl>>>,
}

impl OpenAiLiveTask {
    async fn run(
        self,
        mut upstream_rx: mpsc::Receiver<Vec<u8>>,
        downstream_tx: mpsc::Sender<Vec<u8>>,
        mut control_rx: mpsc::Receiver<RealtimeAudioControl>,
        events_tx: mpsc::Sender<RealtimeProviderEvent>,
    ) -> Result<(), RealtimeProviderError> {
        let mut request = self
            .websocket_url
            .as_str()
            .into_client_request()
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        request.headers_mut().insert(
            AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {}", self.api_key))
                .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?,
        );
        info!(
            model = %self.model,
            "[REALTIME-VOICE] opening GPT-Live WebSocket"
        );
        let (ws, _) = tokio::time::timeout(OPENAI_LIVE_CONNECT_TIMEOUT, connect_async(request))
            .await
            .map_err(|_| {
                RealtimeProviderError::Upstream("GPT-Live websocket handshake timed out".into())
            })?
            .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))?;
        let _ = events_tx.send(RealtimeProviderEvent::TransportReady).await;
        let (mut write, mut read) = ws.split();

        let mut start_sent = false;
        let mut started = false;
        let mut pending: VecDeque<Vec<u8>> = VecDeque::new();
        let mut pending_bytes = 0usize;
        let mut pending_history: Vec<String> = Vec::new();
        let mut pending_update_id: Option<String> = None;
        let mut user_transcript = String::new();
        let mut last_user_intent = String::new();
        let mut recent_turns: VecDeque<(String, String)> = VecDeque::new();
        let mut assistant_transcript = String::new();
        let mut assistant_seq = 0u64;
        // Live's billable duration; the session is the billing unit. The
        // provider's own figure when it sends one, else this connection.
        let mut reported_seconds: Option<f64> = None;
        let connection_started_at = Instant::now();
        let mut assistant_turn = 0u64;
        let mut latest_delegation_id: Option<String> = None;
        let mut known_delegation_ids: HashSet<String> = HashSet::new();
        let mut output = OutputPacer::new();
        let mut pace_tick = tokio::time::interval(OUTPUT_PACE_TICK);
        pace_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        pace_tick.tick().await;

        loop {
            tokio::select! {
                biased;
                maybe_control = control_rx.recv() => {
                    let Some(control) = maybe_control else { break; };
                    match control {
                        RealtimeAudioControl::ConfigureSession { instructions, update_id, .. } => {
                            if !start_sent {
                                pending_update_id = update_id;
                                send_json(
                                    &mut write,
                                    session_start_payload(
                                        &self.model,
                                        self.voice.as_deref(),
                                        Some(instructions.as_str()),
                                        &pending_history,
                                    ),
                                )
                                .await?;
                                start_sent = true;
                                pending_history.clear();
                            } else if started && !instructions.trim().is_empty() {
                                send_json(
                                    &mut write,
                                    live_append(
                                        "session.instructions.append",
                                        Value::Null,
                                        &instructions,
                                    ),
                                )
                                .await?;
                                let _ = events_tx
                                    .send(RealtimeProviderEvent::SessionConfigured { update_id })
                                    .await;
                            }
                        },
                        RealtimeAudioControl::ToolResult { call_id, output } => {
                            let delegation = live_delegation_id(
                                &call_id,
                                &known_delegation_ids,
                                latest_delegation_id.as_deref(),
                            );
                            if is_live_delegate_ack(&output) {
                                send_json(
                                    &mut write,
                                    live_append(
                                        "session.thinking.append",
                                        delegation,
                                        "Magician is working on this request.",
                                    ),
                                )
                                .await?;
                            } else {
                                let spoken = speakable_live_commentary(&output);
                                if !spoken.is_empty() {
                                    send_json(
                                        &mut write,
                                        live_append(
                                            "session.commentary.append",
                                            delegation,
                                            &spoken,
                                        ),
                                    )
                                    .await?;
                                }
                            }
                        },
                        RealtimeAudioControl::InjectInitialHistory { text } => {
                            if !start_sent {
                                if !text.trim().is_empty() {
                                    pending_history.push(text);
                                }
                            } else if started {
                                send_json(
                                    &mut write,
                                    live_append(
                                        "session.thinking.append",
                                        Value::Null,
                                        &text,
                                    ),
                                )
                                .await?;
                            } else if !text.trim().is_empty() {
                                pending_history.push(text);
                            }
                        },
                        RealtimeAudioControl::InjectSystemMessage { text, request_response } => {
                            if !start_sent {
                                if !text.trim().is_empty() {
                                    pending_history.push(text);
                                }
                            } else if started {
                                let kind = if request_response {
                                    "session.commentary.append"
                                } else {
                                    "session.thinking.append"
                                };
                                let delegation = if request_response {
                                    latest_delegation_id
                                        .as_deref()
                                        .map(Value::from)
                                        .unwrap_or(Value::Null)
                                } else {
                                    Value::Null
                                };
                                send_json(
                                    &mut write,
                                    live_append(kind, delegation, &speakable_live_text(&text)),
                                )
                                .await?;
                            }
                        },
                        RealtimeAudioControl::End => {
                            flush_output(&downstream_tx, &mut output, true).await;
                            let _ = send_json(&mut write, json!({ "type": "session.close" })).await;
                            break;
                        },
                        RealtimeAudioControl::InterruptResponse
                        | RealtimeAudioControl::ClearInput
                        | RealtimeAudioControl::CommitInputAndRespond
                        | RealtimeAudioControl::InjectToolExchange { .. }
                        | RealtimeAudioControl::RespondWithTurnContext { .. }
                        | RealtimeAudioControl::SynthesizeResponse { .. } => {},
                    }
                },
                maybe_pcm = upstream_rx.recv() => {
                    let Some(pcm) = maybe_pcm else { break; };
                    if pcm.is_empty() {
                        continue;
                    }
                    if !started {
                        if pending_bytes + pcm.len() > PENDING_AFTER_START_MAX_BYTES {
                            continue;
                        }
                        pending_bytes += pcm.len();
                        pending.push_back(pcm);
                        continue;
                    }
                    send_input_audio(&mut write, &pcm).await?;
                },
                _ = pace_tick.tick() => {
                    if output.due_to_flush() {
                        flush_output(&downstream_tx, &mut output, true).await;
                    }
                },
                maybe_msg = read.next() => {
                    let Some(message) = maybe_msg else {
                        return Err(RealtimeProviderError::Upstream(
                            "GPT-Live connection ended".into(),
                        ));
                    };
                    let text = match message {
                        Ok(Message::Text(text)) => text,
                        Ok(Message::Binary(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                        Ok(Message::Close(_)) => break,
                        Ok(_) => continue,
                        Err(error) => {
                            return Err(RealtimeProviderError::Upstream(error.to_string()));
                        },
                    };
                    let Ok(event) = serde_json::from_str::<Value>(&text) else {
                        continue;
                    };
                    let event_type = event.get("type").and_then(Value::as_str).unwrap_or("");
                    match event_type {
                        "session.started" => {
                            started = true;
                            let _ = events_tx
                                .send(RealtimeProviderEvent::SessionConfigured {
                                    update_id: pending_update_id.take(),
                                })
                                .await;
                            while let Some(pcm) = pending.pop_front() {
                                send_input_audio(&mut write, &pcm).await?;
                            }
                            pending_bytes = 0;
                            for leftover in pending_history.drain(..) {
                                send_json(
                                    &mut write,
                                    live_append(
                                        "session.thinking.append",
                                        Value::Null,
                                        &leftover,
                                    ),
                                )
                                .await?;
                            }
                        },
                        "session.output_audio.delta" => {
                            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                                if let Ok(bytes) = BASE64_STANDARD.decode(delta) {
                                    if !bytes.is_empty() {
                                        if assistant_seq == 0 {
                                            assistant_turn += 1;
                                            assistant_seq = assistant_turn;
                                            let _ = events_tx
                                                .send(RealtimeProviderEvent::AssistantAudioStarted {
                                                    response_id: live_assistant_id(assistant_seq),
                                                })
                                                .await;
                                        }
                                        output.push(&bytes);
                                        flush_output(&downstream_tx, &mut output, false).await;
                                    }
                                }
                            }
                        },
                        "session.input_transcript.delta" => {
                            if assistant_seq > 0 {
                                flush_output(&downstream_tx, &mut output, true).await;
                                output.reset();
                                if let Some(text) = finalize_assistant_turn(
                                    &events_tx,
                                    &mut assistant_seq,
                                    &mut assistant_transcript,
                                )
                                .await
                                {
                                    push_live_turn(&mut recent_turns, "assistant", text);
                                }
                            }
                            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                                if user_transcript.is_empty() {
                                    let _ = events_tx
                                        .send(RealtimeProviderEvent::SpeechStarted)
                                        .await;
                                }
                                user_transcript.push_str(delta);
                                let snapshot = user_transcript.trim().to_string();
                                if !snapshot.is_empty() {
                                    let _ = events_tx
                                        .send(RealtimeProviderEvent::UserTranscriptPartial {
                                            text: snapshot,
                                            item_id: "gpt-live-user".into(),
                                        })
                                        .await;
                                }
                            }
                        },
                        "session.output_transcript.delta" => {
                            if let Some(delta) = event.get("delta").and_then(Value::as_str) {
                                if let Some(text) =
                                    finalize_user_transcript(&events_tx, &mut user_transcript).await
                                {
                                    last_user_intent = text.clone();
                                    push_live_turn(&mut recent_turns, "user", text);
                                }
                                if assistant_seq == 0 {
                                    assistant_turn += 1;
                                    assistant_seq = assistant_turn;
                                    let _ = events_tx
                                        .send(RealtimeProviderEvent::AssistantAudioStarted {
                                            response_id: live_assistant_id(assistant_seq),
                                        })
                                        .await;
                                }
                                assistant_transcript.push_str(delta);
                                let _ = events_tx
                                    .send(RealtimeProviderEvent::AssistantTranscriptDelta {
                                        response_id: live_assistant_id(assistant_seq),
                                        text: assistant_transcript.clone(),
                                    })
                                    .await;
                            }
                        },
                        "session.delegation.created" => {
                            let delegation_id = event
                                .pointer("/delegation/id")
                                .and_then(Value::as_str)
                                .unwrap_or("")
                                .to_string();
                            if !delegation_id.is_empty() {
                                known_delegation_ids.insert(delegation_id.clone());
                                latest_delegation_id = Some(delegation_id.clone());
                                let current_user = if user_transcript.trim().is_empty() {
                                    last_user_intent.as_str()
                                } else {
                                    user_transcript.as_str()
                                };
                                let intent =
                                    live_delegation_intent(current_user, &recent_turns);
                                if let Some(text) =
                                    finalize_user_transcript(&events_tx, &mut user_transcript).await
                                {
                                    last_user_intent = text.clone();
                                    push_live_turn(&mut recent_turns, "user", text);
                                }
                                let _ = events_tx
                                    .send(RealtimeProviderEvent::FunctionCall {
                                        response_id: Some(delegation_id.clone()),
                                        call_id: delegation_id,
                                        name: "delegate_to_chat".to_string(),
                                        arguments_json: json!({ "intent": intent }).to_string(),
                                    })
                                    .await;
                            }
                        },
                        "session.usage.updated" => {
                            if let Some(seconds) = live_usage_seconds(&event) {
                                reported_seconds = Some(seconds);
                            }
                        },
                        "session.closed" => {
                            flush_output(&downstream_tx, &mut output, true).await;
                            output.reset();
                            if let Some(seconds) = live_usage_seconds(&event) {
                                reported_seconds = Some(seconds);
                            }
                            break;
                        },
                        "error" => {
                            let message = event
                                .pointer("/error/message")
                                .or_else(|| event.get("message"))
                                .and_then(Value::as_str)
                                .unwrap_or("GPT-Live error")
                                .to_string();
                            warn!(error = %message, "[REALTIME-VOICE] GPT-Live error");
                            let _ = events_tx
                                .send(RealtimeProviderEvent::Error {
                                    message,
                                    recoverable: true,
                                })
                                .await;
                        },
                        _ => {},
                    }
                },
            }
        }

        flush_output(&downstream_tx, &mut output, true).await;
        if let Some(text) =
            finalize_assistant_turn(&events_tx, &mut assistant_seq, &mut assistant_transcript).await
        {
            push_live_turn(&mut recent_turns, "assistant", text);
        }
        // The session's duration is its bill, reported once however the
        // session ended. Not from the `session.closed` arm alone: ending the
        // call sends `session.close` and stops reading, so the server's
        // closing frame usually never arrives. What is left is whatever
        // `session.usage.updated` had reported — none of it, in a short
        // session — and the connection this loop held, clocked from the
        // handshake. See `live_session_usage_event`.
        let billable_seconds = live_billable_seconds(
            reported_seconds,
            connection_started_at.elapsed().as_secs_f64(),
        );
        info!(
            reported_by_provider = reported_seconds.is_some(),
            billable_seconds, "[REALTIME-VOICE] GPT-Live session bill"
        );
        let _ = events_tx
            .send(live_session_usage_event(billable_seconds))
            .await;
        self.sessions.remove(&self.upstream_session_id);
        Ok(())
    }
}

struct OutputPacer {
    pending: Vec<u8>,
    prerolled: bool,
    last_push: Option<Instant>,
}

impl OutputPacer {
    fn new() -> Self {
        Self {
            pending: Vec::new(),
            prerolled: false,
            last_push: None,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.pending.extend_from_slice(bytes);
        self.last_push = Some(Instant::now());
    }

    fn due_to_flush(&self) -> bool {
        !self.pending.is_empty()
            && self
                .last_push
                .is_some_and(|last| last.elapsed() >= OUTPUT_FLUSH_AFTER)
    }

    fn drain(&mut self, force_remainder: bool) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        let even = self.pending.len() - (self.pending.len() % 2);
        if even == 0 {
            if force_remainder {
                self.pending.clear();
            }
            return frames;
        }
        if even < self.pending.len() {
            self.pending.truncate(even);
        }

        if !self.prerolled {
            if self.pending.len() >= OUTPUT_PREROLL_BYTES {
                self.prerolled = true;
                let rest = self.pending.split_off(OUTPUT_PREROLL_BYTES);
                frames.push(std::mem::replace(&mut self.pending, rest));
            } else if force_remainder {
                self.prerolled = true;
                frames.push(std::mem::take(&mut self.pending));
                return frames;
            } else {
                return frames;
            }
        }

        while self.pending.len() >= OUTPUT_FRAME_BYTES {
            let rest = self.pending.split_off(OUTPUT_FRAME_BYTES);
            frames.push(std::mem::replace(&mut self.pending, rest));
        }
        if force_remainder && !self.pending.is_empty() {
            frames.push(std::mem::take(&mut self.pending));
        }
        frames
    }

    fn reset(&mut self) {
        self.pending.clear();
        self.prerolled = false;
        self.last_push = None;
    }
}

async fn flush_output(
    downstream_tx: &mpsc::Sender<Vec<u8>>,
    output: &mut OutputPacer,
    force_remainder: bool,
) {
    for frame in output.drain(force_remainder) {
        let _ = downstream_tx.send(frame).await;
    }
}

async fn send_json<S>(write: &mut S, payload: Value) -> Result<(), RealtimeProviderError>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    write
        .send(Message::Text(payload.to_string()))
        .await
        .map_err(|e| RealtimeProviderError::Upstream(e.to_string()))
}

async fn send_input_audio<S>(write: &mut S, pcm: &[u8]) -> Result<(), RealtimeProviderError>
where
    S: Sink<Message> + Unpin,
    S::Error: std::fmt::Display,
{
    if pcm.len() < 2 {
        return Ok(());
    }
    let even = pcm.len() - (pcm.len() % 2);
    send_json(
        write,
        json!({
            "type": "session.input_audio.append",
            "audio": BASE64_STANDARD.encode(&pcm[..even]),
        }),
    )
    .await
}

fn session_start_payload(
    model: &str,
    voice: Option<&str>,
    instructions: Option<&str>,
    history: &[String],
) -> Value {
    let mut session = json!({
        "model": model,
        "instructions": instructions
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or(OPENAI_LIVE_DEFAULT_INSTRUCTIONS),
        "audio": {
            "format": { "type": "audio/pcm", "rate": 24000 },
            "output": { "voice": voice.unwrap_or(OPENAI_LIVE_DEFAULT_VOICE) }
        },
        "delegation": { "type": "client" }
    });
    if !history.is_empty() {
        session["input"] = json!([{
            "type": "message",
            "role": "user",
            "content": [{
                "type": "input_text",
                "text": history.join("\n")
            }]
        }]);
    }
    json!({
        "type": "session.start",
        "event_id": live_event_id(),
        "session": session
    })
}

fn live_append(kind: &str, delegation_id: Value, content: &str) -> Value {
    json!({
        "type": kind,
        "event_id": live_event_id(),
        "delegation_id": delegation_id,
        "content": truncate_live_append(content),
    })
}

fn live_event_id() -> String {
    format!("evt_{}", Ulid::new())
}

fn live_assistant_id(seq: u64) -> String {
    format!("gpt-live-{seq}")
}

fn truncate_live_append(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 1800 {
        return trimmed.to_string();
    }
    trimmed.chars().take(1800).collect()
}

fn speakable_tool_output(output: &str) -> String {
    if let Ok(value) = serde_json::from_str::<Value>(output) {
        for key in ["voice_summary", "summary", "text", "result"] {
            if let Some(text) = value.get(key).and_then(Value::as_str) {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    return trimmed.to_string();
                }
            }
        }
    }
    output.trim().to_string()
}

fn speakable_live_text(text: &str) -> String {
    strip_speech_markup(text)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn speakable_live_commentary(output: &str) -> String {
    speakable_live_text(&speakable_tool_output(output))
}

fn strip_speech_markup(text: &str) -> String {
    let mut rest = text;
    let mut out = String::new();
    while let Some(start) = rest.find("<speech") {
        out.push_str(&rest[..start]);
        let after_open = &rest[start..];
        let Some(tag_end) = after_open.find('>') else {
            break;
        };
        rest = &after_open[tag_end + 1..];
        if let Some(close) = rest.find("</speech>") {
            out.push_str(&rest[..close]);
            rest = &rest[close + "</speech>".len()..];
        } else {
            out.push_str(rest);
            return out;
        }
    }
    out.push_str(rest);
    out
}

const LIVE_RECENT_TURN_CAP: usize = 8;

fn push_live_turn(recent: &mut VecDeque<(String, String)>, role: &str, text: String) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }
    if recent
        .back()
        .is_some_and(|(last_role, last_text)| last_role == role && last_text == trimmed)
    {
        return;
    }
    recent.push_back((role.to_string(), trimmed.to_string()));
    while recent.len() > LIVE_RECENT_TURN_CAP {
        recent.pop_front();
    }
}

fn live_delegation_id(call_id: &str, known: &HashSet<String>, latest: Option<&str>) -> Value {
    if known.contains(call_id) {
        return Value::from(call_id);
    }
    latest.map(Value::from).unwrap_or(Value::Null)
}

fn is_live_delegate_ack(output: &str) -> bool {
    let Ok(value) = serde_json::from_str::<Value>(output) else {
        return false;
    };
    if value.get("status").and_then(Value::as_str) == Some("streaming") {
        return true;
    }
    value
        .get("voice_summary")
        .and_then(Value::as_str)
        .is_some_and(|summary| summary.to_ascii_lowercase().contains("think out loud"))
}

fn live_delegation_intent(current_user: &str, _recent: &VecDeque<(String, String)>) -> String {
    let current = current_user.trim();
    if current.is_empty() {
        "Help with the latest request from this live voice conversation.".to_string()
    } else {
        current.to_string()
    }
}

async fn finalize_user_transcript(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    user_transcript: &mut String,
) -> Option<String> {
    if user_transcript.trim().is_empty() {
        return None;
    }
    let text = user_transcript.trim().to_string();
    user_transcript.clear();
    let _ = events_tx
        .send(RealtimeProviderEvent::UserTranscriptFinal {
            text: text.clone(),
            item_id: "gpt-live-user".into(),
        })
        .await;
    let _ = events_tx.send(RealtimeProviderEvent::SpeechStopped).await;
    Some(text)
}

async fn finalize_assistant_turn(
    events_tx: &mpsc::Sender<RealtimeProviderEvent>,
    assistant_seq: &mut u64,
    assistant_transcript: &mut String,
) -> Option<String> {
    if *assistant_seq == 0 && assistant_transcript.trim().is_empty() {
        return None;
    }
    let seq = *assistant_seq;
    let response_id = live_assistant_id(seq.max(1));
    let finalized = if !assistant_transcript.trim().is_empty() {
        let text = assistant_transcript.trim().to_string();
        assistant_transcript.clear();
        let _ = events_tx
            .send(RealtimeProviderEvent::AssistantTranscriptFinal {
                response_id: response_id.clone(),
                text: text.clone(),
            })
            .await;
        Some(text)
    } else {
        None
    };
    if seq > 0 {
        let _ = events_tx
            .send(RealtimeProviderEvent::AssistantAudioDone {
                response_id,
                interrupted: false,
            })
            .await;
    }
    *assistant_seq = 0;
    finalized
}

/// The billable duration a GPT-Live event reports, in seconds. Live bills by
/// the clock — `session.usage.updated` carries the running total and
/// `session.closed` the final one — and reports no tokens of its own.
fn live_usage_seconds(event: &Value) -> Option<f64> {
    event
        .pointer("/usage/seconds")
        .and_then(Value::as_f64)
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
}

/// What to bill a Live session for: the provider's own figure when it sent
/// one, else the connection we held open. Live reports
/// `session.usage.updated` on its own schedule and a short session gets none,
/// so without the fallback a real, charged session is recorded as unmetered.
fn live_billable_seconds(reported: Option<f64>, connection_seconds: f64) -> f64 {
    reported.unwrap_or(connection_seconds.max(0.0))
}

/// The session's duration as its one billable response. `response_id` is
/// deliberately `None`: this is the session's bill rather than a turn's, and
/// an unidentified terminal closes out whichever call is in flight — which is
/// what stops a Live session from ending with its open call recorded as a
/// `cancelled` failure and no metered row at all.
fn live_session_usage_event(billed_seconds: f64) -> RealtimeProviderEvent {
    RealtimeProviderEvent::ResponseDone {
        response_id: None,
        input_tokens: None,
        output_tokens: None,
        usage: (billed_seconds > 0.0).then(|| crate::types::RealtimeUsage {
            billed_seconds,
            ..Default::default()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// GPT-Live reports its billable duration as a running total
    /// (`session.usage.updated`) and a final one on `session.closed`; there is
    /// no per-turn usage and no turn-completed event. The session is the
    /// billing unit, so the adapter keeps the latest total and reports it once.
    #[test]
    fn billable_seconds_are_read_from_the_session_usage_events() {
        assert_eq!(
            live_usage_seconds(
                &json!({ "type": "session.usage.updated", "usage": { "seconds": 12 } })
            ),
            Some(12.0)
        );
        assert_eq!(
            live_usage_seconds(&json!({ "type": "session.closed", "usage": { "seconds": 47.5 } })),
            Some(47.5)
        );
        // A close with no usage leaves the running total standing.
        assert_eq!(
            live_usage_seconds(&json!({ "type": "session.closed" })),
            None
        );
        assert_eq!(
            live_usage_seconds(&json!({ "usage": { "seconds": "many" } })),
            None
        );
    }

    /// GPT-Live's own usage report does not arrive in a short session — a
    /// minute of speech produced none — so the duration is taken from the
    /// connection when the provider has not reported one. That is a
    /// measurement, not a quote, and the fact it lands in is labelled
    /// `Estimated` for exactly that reason.
    #[test]
    fn a_session_the_provider_never_reported_is_billed_by_its_connection() {
        assert_eq!(live_billable_seconds(Some(47.5), 12.0), 47.5);
        assert_eq!(live_billable_seconds(None, 12.0), 12.0);
        assert_eq!(live_billable_seconds(None, 0.0), 0.0);
    }

    /// The session's duration is one billable response, reported when the
    /// session closes. `response_id: None` deliberately: it is the session's
    /// bill, not a turn's, and the control handler attributes an unidentified
    /// terminal to the call in flight — which is what closes that call out.
    /// Without it, every Live session ended with its open call recorded as a
    /// `cancelled` failure and no metered row at all.
    #[test]
    fn the_session_close_reports_its_duration_as_the_billable_usage() {
        let done = live_session_usage_event(47.5);
        match done {
            RealtimeProviderEvent::ResponseDone {
                response_id,
                usage,
                input_tokens,
                output_tokens,
            } => {
                assert_eq!(response_id, None);
                assert_eq!(input_tokens, None);
                assert_eq!(output_tokens, None);
                let usage = usage.expect("the session's duration is its usage");
                assert!((usage.billed_seconds - 47.5).abs() < 1e-9);
                assert_eq!(usage.total_tokens(), 0, "Live bills no tokens of its own");
            },
            other => panic!("expected a ResponseDone carrying the session duration: {other:?}"),
        }
        match live_session_usage_event(0.0) {
            RealtimeProviderEvent::ResponseDone { usage, .. } => {
                assert!(usage.is_none(), "an unmeasured session bills nothing")
            },
            other => panic!("expected a ResponseDone: {other:?}"),
        }
    }

    #[test]
    fn session_start_uses_client_delegation_and_pcm_24k() {
        let payload = session_start_payload(
            "gpt-live-1",
            Some("marin"),
            Some("Be brief."),
            &["Earlier: hello".to_string()],
        );
        assert_eq!(payload["type"], "session.start");
        assert_eq!(payload["session"]["model"], "gpt-live-1");
        assert_eq!(payload["session"]["delegation"]["type"], "client");
        assert_eq!(payload["session"]["audio"]["format"]["rate"], 24000);
        assert_eq!(payload["session"]["instructions"], "Be brief.");
        assert_eq!(
            payload["session"]["input"][0]["content"][0]["text"],
            "Earlier: hello"
        );
    }

    #[test]
    fn commentary_append_keeps_delegation_id() {
        let payload = live_append(
            "session.commentary.append",
            Value::from("item_abc"),
            "Booked for Thursday.",
        );
        assert_eq!(payload["type"], "session.commentary.append");
        assert_eq!(payload["delegation_id"], "item_abc");
        assert_eq!(payload["content"], "Booked for Thursday.");
    }

    #[test]
    fn empty_user_transcript_still_yields_a_delegate_intent() {
        assert_eq!(
            live_delegation_intent("   ", &VecDeque::new()),
            "Help with the latest request from this live voice conversation."
        );
        assert_eq!(
            live_delegation_intent("What's on my calendar?", &VecDeque::new()),
            "What's on my calendar?"
        );
    }

    #[test]
    fn live_intent_is_only_the_current_utterance() {
        let mut recent = VecDeque::new();
        push_live_turn(&mut recent, "user", "Book dinner for Friday.".into());
        push_live_turn(&mut recent, "assistant", "Friday at 7 works.".into());
        assert_eq!(
            live_delegation_intent("Make it Thursday instead.", &recent),
            "Make it Thursday instead."
        );
    }

    #[test]
    fn commentary_strips_speech_tags() {
        assert_eq!(
            speakable_live_text("<speech emotion=\"calm\">You're booked Thursday.</speech>"),
            "You're booked Thursday."
        );
        assert_eq!(
            speakable_live_commentary(
                r#"{"voice_summary":"<speech>Maya is in June.</speech>"}"#
            ),
            "Maya is in June."
        );
    }

    #[test]
    fn streaming_ack_is_quiet_not_spoken() {
        let ack = r#"{"status":"streaming","voice_summary":"Let me think out loud — I'll speak each thought as it lands."}"#;
        assert!(is_live_delegate_ack(ack));
        assert!(!is_live_delegate_ack(
            r#"{"voice_summary":"You're booked Thursday."}"#
        ));
    }

    #[test]
    fn tool_result_uses_matching_delegation_id() {
        let mut known = HashSet::new();
        known.insert("item_one".into());
        known.insert("item_two".into());
        assert_eq!(
            live_delegation_id("item_two", &known, Some("item_two")),
            Value::from("item_two")
        );
        assert_eq!(
            live_delegation_id("unknown", &known, Some("item_two")),
            Value::from("item_two")
        );
    }

    #[test]
    fn output_pacer_holds_until_preroll_then_emits_40ms_frames() {
        let mut pacer = OutputPacer::new();
        pacer.push(&vec![0u8; OUTPUT_FRAME_BYTES]);
        assert!(
            pacer.drain(false).is_empty(),
            "40 ms is below the 80 ms preroll"
        );
        pacer.push(&vec![1u8; OUTPUT_FRAME_BYTES]);
        let frames = pacer.drain(false);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), OUTPUT_PREROLL_BYTES);
        pacer.push(&vec![2u8; OUTPUT_FRAME_BYTES]);
        let frames = pacer.drain(false);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].len(), OUTPUT_FRAME_BYTES);
        pacer.push(&vec![3u8; 100]);
        assert!(pacer.drain(false).is_empty());
        let tail = pacer.drain(true);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].len(), 100);
    }
}

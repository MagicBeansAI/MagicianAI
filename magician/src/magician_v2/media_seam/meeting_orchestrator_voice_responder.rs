//! Meeting responder that answers in the magician agent's **realtime voice**
//! via the server's `VoiceOrchestrator` — so the reply is the real agent
//! (Presto = `personal-assistant`, with its tools + context), spoken in a
//! natural low-latency voice, and session rotation / reconnect are handled
//! server-side.
//!
//! Unlike [`MagicianAgentResponder`](super::magician_agent_responder) (agent
//! over REST + a separate OpenAI-TTS voice) and
//! [`RealtimeResponder`](super::realtime_responder) (a bot-local OpenAI Realtime
//! session with no agent tools/context), this is a headless client of the
//! control WebSocket `/media/voice/{id}/control`:
//!
//!   1. `POST /media/sessions` mints a `voice_session_id`.
//!   2. Open the control WS (non-browser: no `Origin` header).
//!   3. `session.start` with the backend-proxied PTT profile
//!      (`turn_detection: none`) — the server configures the realtime session
//!      with the agent's instructions + tool catalog and dispatches tool calls.
//!   4. On a wake phrase, send the transcribed utterance as a `user.text`
//!      prompt with `request_response: true`; the agent's spoken reply streams
//!      back as 24 kHz mono PCM16 over the same socket, which we collect for the
//!      sink.
//!
//! The bot keeps owning wake detection locally (on-device STT), so we never
//! stream the whole meeting to the provider — only the addressed utterance is
//! sent, as text. (Streaming the raw question audio in via PTT is a future
//! step; see `docs/plans/2026-06-07-realtime-voice-agent-meet-bot.md`.)
//!
//! Config (env): `MEET_BOT_MAGICIAN_URL` (default the local server),
//! `MEET_BOT_PRINCIPAL` / `MEET_BOT_WORKSPACE` (default `anonymous` / `default`),
//! `MEET_BOT_BEARER_TOKEN` (scoped credential; falls back to
//! `MAGICIAN_BEARER_TOKEN`),
//! `MEET_BOT_THREAD` (default `meeting-bot`), `MEET_BOT_REALTIME_PROFILE`
//! (default `voice_realtime_openai_backend`).

use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    http::{header::AUTHORIZATION, HeaderValue},
    Message,
};

use crate::magician_v2::media_seam::responder::{
    MeetingResponder, ResponderError, SpokenReply, UtteranceAudio,
};
use crate::magician_v2::media_seam::StreamAudioFormat;

/// The realtime rail's PCM is 24 kHz mono, little-endian 16-bit.
const REALTIME_PCM_SAMPLE_RATE: u32 = 24_000;
/// Max wall-clock to wait for the first audio frame of a reply.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(20);
/// Idle gap (after audio starts) that marks the reply finished — the control WS
/// does not forward a per-response "done" frame to the client.
const INTER_FRAME_IDLE: Duration = Duration::from_millis(800);
/// Hard safety cap on collecting one reply.
const REPLY_DEADLINE: Duration = Duration::from_secs(45);
/// Max wait for `session.ready` after `session.start`.
const READY_TIMEOUT: Duration = Duration::from_secs(20);
/// PCM frame size for streaming question audio up (~200 ms at 24 kHz mono PCM16).
const REALTIME_FRAME_BYTES: usize = 9600;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:3002/api/magician/v2";
const DEFAULT_REALTIME_PROFILE: &str = "voice_realtime_openai_backend";

#[derive(Deserialize)]
struct RegisterResponse {
    session: RegisteredSession,
}

#[derive(Deserialize)]
struct RegisteredSession {
    session_id: String,
}

/// Events the driver task surfaces from the control WS to `respond()`.
enum InboundEvent {
    Ready,
    Audio(Vec<u8>),
    AssistantText(String),
    Error(String),
    Closed,
}

/// One live control-WS connection: a command lane out + an event lane in. The
/// socket itself is owned by a spawned driver task; these are its two ends.
struct Conn {
    outbound_tx: mpsc::Sender<Message>,
    inbound_rx: mpsc::Receiver<InboundEvent>,
    #[allow(dead_code)]
    session_id: String,
}

pub struct OrchestratorVoiceResponder {
    http: reqwest::Client,
    base_url: String,
    principal: String,
    workspace: String,
    bearer_token: Option<String>,
    thread: String,
    profile: String,
    /// Lazily established; rebuilt on a dead socket. The whole turn holds this
    /// lock, which also serializes responses (the session is single-turn).
    conn: Mutex<Option<Conn>>,
    /// Last meeting context seeded into the live session (audio path only), so we
    /// re-inject it only when it changes rather than every turn.
    last_context: Mutex<Option<String>>,
}

impl OrchestratorVoiceResponder {
    pub fn from_env() -> Self {
        let base_url =
            std::env::var("MEET_BOT_MAGICIAN_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        let principal =
            std::env::var("MEET_BOT_PRINCIPAL").unwrap_or_else(|_| "anonymous".to_string());
        let workspace =
            std::env::var("MEET_BOT_WORKSPACE").unwrap_or_else(|_| "default".to_string());
        let thread = std::env::var("MEET_BOT_THREAD").unwrap_or_else(|_| "meeting-bot".to_string());
        let profile = std::env::var("MEET_BOT_REALTIME_PROFILE")
            .unwrap_or_else(|_| DEFAULT_REALTIME_PROFILE.to_string());
        let bearer_token = std::env::var("MEET_BOT_BEARER_TOKEN")
            .or_else(|_| std::env::var("MAGICIAN_BEARER_TOKEN"))
            .ok()
            .map(|token| token.trim().to_owned())
            .filter(|token| !token.is_empty());
        Self {
            http: reqwest::Client::new(),
            base_url,
            principal,
            workspace,
            bearer_token,
            thread,
            profile,
            conn: Mutex::new(None),
            last_context: Mutex::new(None),
        }
    }

    /// Override the chat thread this responder reports to (per-meeting). `None` or
    /// a blank value keeps the env/default thread set in [`Self::from_env`].
    pub fn with_thread(mut self, thread: Option<String>) -> Self {
        if let Some(t) = thread
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
        {
            self.thread = t;
        }
        self
    }

    /// Bind the invoking agent's `(principal, workspace)` so the voice session +
    /// its chat ingest land in that agent's scope instead of the anonymous/default
    /// fallback. Explicit `MEET_BOT_PRINCIPAL`/`MEET_BOT_WORKSPACE` env pins still
    /// win; `None` keeps the env/default scope from [`Self::from_env`].
    pub fn with_scope(mut self, scope: Option<(String, String)>) -> Self {
        if let Some((principal, workspace)) = scope {
            let principal_pinned = std::env::var("MEET_BOT_PRINCIPAL")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !principal_pinned {
                self.principal = principal;
            }
            let workspace_pinned = std::env::var("MEET_BOT_WORKSPACE")
                .map(|v| !v.trim().is_empty())
                .unwrap_or(false);
            if !workspace_pinned {
                self.workspace = workspace;
            }
        }
        self
    }

    /// Register a media session and return its id (the `voice_session_id`).
    async fn register_session(&self) -> Result<String, ResponderError> {
        if self.bearer_token.is_none()
            && (self.principal != "anonymous" || self.workspace != "default")
        {
            return Err(ResponderError::Failed(
                "scoped meeting voice requires MEET_BOT_BEARER_TOKEN or MAGICIAN_BEARER_TOKEN"
                    .to_string(),
            ));
        }
        let url = format!("{}/media/sessions", self.base_url);
        let request = self.http.post(&url);
        let request = if let Some(token) = &self.bearer_token {
            request.bearer_auth(token)
        } else {
            request
        };
        let resp = request
            .timeout(Duration::from_secs(15))
            .json(&json!({
                "thread_id": self.thread,
                // Was `tray_macos`, which made a room indistinguishable from a
                // private desktop session — the exposure this surface exists to
                // close. Registering as the bot it is narrows the session.
                "surface_type": "meeting_bot",
                "transport": "websocket",
                "display_label": "Meet Bot",
            }))
            .send()
            .await
            .map_err(|e| ResponderError::Failed(format!("media/sessions transport: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(ResponderError::Failed(format!(
                "media/sessions {status}: {body}"
            )));
        }
        let parsed: RegisterResponse = resp
            .json()
            .await
            .map_err(|e| ResponderError::Failed(format!("media/sessions decode: {e}")))?;
        Ok(parsed.session.session_id)
    }

    /// Build the control-WS URL from the (http) base url.
    fn control_ws_url(&self, session_id: &str) -> String {
        let ws_base = self
            .base_url
            .replacen("https://", "wss://", 1)
            .replacen("http://", "ws://", 1);
        format!("{ws_base}/media/voice/{session_id}/control")
    }

    /// Establish a fresh control-WS connection: register → connect → start →
    /// wait for `session.ready`. Spawns the driver task that owns the socket.
    async fn open_conn(&self) -> Result<Conn, ResponderError> {
        let session_id = self.register_session().await?;
        let url = self.control_ws_url(&session_id);
        let mut request = url
            .into_client_request()
            .map_err(|e| ResponderError::Failed(format!("voice control ws request: {e}")))?;
        if let Some(token) = &self.bearer_token {
            let value = HeaderValue::from_str(&format!("Bearer {token}"))
                .map_err(|e| ResponderError::Failed(format!("voice control bearer: {e}")))?;
            request.headers_mut().insert(AUTHORIZATION, value);
        }
        let (ws, _resp) = tokio_tungstenite::connect_async(request)
            .await
            .map_err(|e| ResponderError::Failed(format!("voice control ws connect: {e}")))?;

        let (mut sink, mut stream) = ws.split();
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(64);
        let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundEvent>(256);

        // Driver task: owns both halves, multiplexes outbound sends with inbound
        // reads, answers pings, and translates frames into events. Exits (and
        // signals `Closed`) when the socket dies or the responder drops the conn.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    out = outbound_rx.recv() => match out {
                        Some(msg) => {
                            if sink.send(msg).await.is_err() {
                                let _ = inbound_tx.send(InboundEvent::Closed).await;
                                break;
                            }
                        }
                        None => break, // responder dropped the conn
                    },
                    inb = stream.next() => match inb {
                        Some(Ok(Message::Binary(bytes))) => {
                            if inbound_tx.send(InboundEvent::Audio(bytes)).await.is_err() {
                                break;
                            }
                        }
                        Some(Ok(Message::Text(text))) => {
                            if let Some(ev) = parse_server_frame(&text) {
                                if inbound_tx.send(ev).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Some(Ok(Message::Ping(p))) => {
                            let _ = sink.send(Message::Pong(p)).await;
                        }
                        Some(Ok(Message::Close(_))) | None => {
                            let _ = inbound_tx.send(InboundEvent::Closed).await;
                            break;
                        }
                        Some(Ok(_)) => {}
                        Some(Err(_)) => {
                            let _ = inbound_tx.send(InboundEvent::Closed).await;
                            break;
                        }
                    },
                }
            }
        });

        // Start the upstream realtime session (backend-proxied PTT profile).
        outbound_tx
            .send(Message::Text(
                json!({
                    "kind": "session.start",
                    "payload": {
                        "ui_thread_id": self.thread.clone(),
                        "thread_id": self.thread.clone(),
                        "realtime_profile": self.profile.clone(),
                    }
                })
                .to_string(),
            ))
            .await
            .map_err(|e| ResponderError::Failed(format!("session.start send: {e}")))?;

        // Wait for session.ready (server minted + configured the agent realtime
        // session) before letting the first prompt through.
        tokio::time::timeout(READY_TIMEOUT, async {
            while let Some(ev) = inbound_rx.recv().await {
                match ev {
                    InboundEvent::Ready => return Ok(()),
                    InboundEvent::Error(msg) => {
                        return Err(ResponderError::Failed(format!("session.error: {msg}")));
                    },
                    InboundEvent::Closed => {
                        return Err(ResponderError::Failed("ws closed before ready".to_string()));
                    },
                    _ => {}, // drain stray audio/text that precedes ready
                }
            }
            Err(ResponderError::Failed("ws ended before ready".to_string()))
        })
        .await
        .map_err(|_| ResponderError::Failed("timed out waiting for session.ready".to_string()))??;

        Ok(Conn {
            outbound_tx,
            inbound_rx,
            session_id,
        })
    }

    /// Text path: send the transcribed prompt and let the live session voice it.
    async fn try_respond_text(&self, prompt: &str) -> Result<SpokenReply, ResponderError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_conn().await?);
        }
        let conn = guard.as_mut().expect("conn just set");
        drain_stale(&mut conn.inbound_rx)?;

        conn.outbound_tx
            .send(Message::Text(
                json!({
                    "kind": "user.text",
                    "payload": { "text": prompt, "request_response": true }
                })
                .to_string(),
            ))
            .await
            .map_err(|e| ResponderError::Failed(format!("user.text send: {e}")))?;

        collect_reply(&mut conn.inbound_rx).await
    }

    /// Audio path: stream the question PCM up (PTT) so the model HEARS it, after
    /// seeding the meeting context (only when it changed), then collect the reply.
    async fn try_respond_audio(
        &self,
        audio: &UtteranceAudio,
        context: &str,
    ) -> Result<SpokenReply, ResponderError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_conn().await?);
        }
        let conn = guard.as_mut().expect("conn just set");
        drain_stale(&mut conn.inbound_rx)?;

        // Clear any stale input audio buffer first.
        send_text(
            &conn.outbound_tx,
            json!({ "kind": "ptt.engage", "payload": {} }),
        )
        .await?;

        // (Re)seed meeting context as a silent conversation item — only when it
        // changed since the last turn (otherwise it bloats the session).
        let context = context.trim();
        let changed = {
            let mut last = self.last_context.lock().await;
            if last.as_deref() == Some(context) {
                false
            } else {
                *last = Some(context.to_string());
                true
            }
        };
        if changed && !context.is_empty() {
            send_text(
                &conn.outbound_tx,
                json!({
                    "kind": "user.text",
                    "payload": {
                        "text": format!(
                            "[Live meeting context — for grounding only, do not read aloud]\n{context}"
                        ),
                        "inject": true,
                        "request_response": false
                    }
                }),
            )
            .await?;
        }

        // Stream the question audio up as 24 kHz mono PCM16 binary frames.
        let pcm24 = resample_pcm16_mono(
            &audio.pcm,
            audio.format.sample_rate_hz,
            REALTIME_PCM_SAMPLE_RATE,
        );
        for frame in pcm24.chunks(REALTIME_FRAME_BYTES) {
            conn.outbound_tx
                .send(Message::Binary(frame.to_vec()))
                .await
                .map_err(|e| ResponderError::Failed(format!("audio frame send: {e}")))?;
        }

        // Commit the audio turn and request the spoken reply.
        send_text(
            &conn.outbound_tx,
            json!({ "kind": "ptt.release", "payload": {} }),
        )
        .await?;

        collect_reply(&mut conn.inbound_rx).await
    }
}

/// Drop any frames left from a prior turn; surface a closed socket as an error.
fn drain_stale(rx: &mut mpsc::Receiver<InboundEvent>) -> Result<(), ResponderError> {
    while let Ok(ev) = rx.try_recv() {
        if matches!(ev, InboundEvent::Closed) {
            return Err(ResponderError::Failed("ws closed".to_string()));
        }
    }
    Ok(())
}

/// Send a JSON control envelope over the outbound lane.
async fn send_text(
    tx: &mpsc::Sender<Message>,
    value: serde_json::Value,
) -> Result<(), ResponderError> {
    tx.send(Message::Text(value.to_string()))
        .await
        .map_err(|e| ResponderError::Failed(format!("ws send: {e}")))
}

/// Collect the streamed PCM reply until it goes idle / the deadline elapses (the
/// control WS forwards no explicit per-response "done" frame).
async fn collect_reply(
    rx: &mut mpsc::Receiver<InboundEvent>,
) -> Result<SpokenReply, ResponderError> {
    let mut pcm: Vec<u8> = Vec::new();
    let mut reply_text = String::new();
    let mut got_first = false;
    let deadline = tokio::time::Instant::now() + REPLY_DEADLINE;
    loop {
        let idle = if got_first {
            INTER_FRAME_IDLE
        } else {
            FIRST_FRAME_TIMEOUT
        };
        tokio::select! {
            biased;
            ev = rx.recv() => match ev {
                Some(InboundEvent::Audio(frame)) => {
                    got_first = true;
                    pcm.extend_from_slice(&frame);
                }
                Some(InboundEvent::AssistantText(text)) => reply_text = text,
                Some(InboundEvent::Error(msg)) => {
                    return Err(ResponderError::Failed(format!("realtime error: {msg}")));
                }
                Some(InboundEvent::Ready) => {}
                Some(InboundEvent::Closed) | None => {
                    if got_first { break; }
                    return Err(ResponderError::Failed("ws closed mid-turn".to_string()));
                }
            },
            _ = tokio::time::sleep(idle) => break,
            _ = tokio::time::sleep_until(deadline) => break,
        }
    }
    Ok(SpokenReply {
        text: reply_text,
        pcm: bytes::Bytes::from(pcm),
        format: StreamAudioFormat {
            sample_rate_hz: REALTIME_PCM_SAMPLE_RATE,
            channels: 1,
            sample_format: Default::default(),
        },
    })
}

/// Linear-interpolate mono PCM16 from `from_hz` to `to_hz` (returns the input
/// unchanged when the rates match). Speech-quality; not a polyphase resampler.
fn resample_pcm16_mono(pcm: &[u8], from_hz: u32, to_hz: u32) -> Vec<u8> {
    if from_hz == to_hz || from_hz == 0 || pcm.len() < 4 {
        return pcm.to_vec();
    }
    let samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]))
        .collect();
    let in_len = samples.len();
    let out_len = ((in_len as u64) * (to_hz as u64) / (from_hz as u64)) as usize;
    let mut out = Vec::with_capacity(out_len * 2);
    for i in 0..out_len {
        let src = (i as f64) * (from_hz as f64) / (to_hz as f64);
        let idx = src.floor() as usize;
        let frac = src - idx as f64;
        let s0 = samples[idx.min(in_len - 1)] as f64;
        let s1 = samples[(idx + 1).min(in_len - 1)] as f64;
        let v = (s0 + (s1 - s0) * frac).round();
        out.extend_from_slice(&(v as i16).to_le_bytes());
    }
    out
}

/// Translate a server control frame into an inbound event we care about.
fn parse_server_frame(text: &str) -> Option<InboundEvent> {
    let v: serde_json::Value = serde_json::from_str(text).ok()?;
    let kind = v.get("kind").and_then(|k| k.as_str())?;
    let payload = v.get("payload").cloned().unwrap_or(serde_json::Value::Null);
    match kind {
        "session.ready" => Some(InboundEvent::Ready),
        "transcript.assistant" => payload
            .get("text")
            .and_then(|t| t.as_str())
            .map(|t| InboundEvent::AssistantText(t.to_string())),
        "session.error" => Some(InboundEvent::Error(
            payload
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown")
                .to_string(),
        )),
        "session.ended" => Some(InboundEvent::Closed),
        // speech.*, transcript.user, transcript.assistant.delta, tool.result,
        // task.completed, etc. — not needed to produce the spoken reply.
        _ => None,
    }
}

#[async_trait]
impl MeetingResponder for OrchestratorVoiceResponder {
    /// The voice orchestrator ingests the utterance + reply into the meeting
    /// thread's chat session itself, so the session's transcript lane must not
    /// surface the same exchange again.
    fn persists_to_chat(&self) -> bool {
        true
    }

    async fn respond(&self, utterance: &str, context: &str) -> Result<SpokenReply, ResponderError> {
        let prompt = format!(
            "[Live meeting — you are participating by voice]\nMeeting context so far:\n{context}\n\n\
             A participant just addressed you and said: \"{utterance}\"\n\nReply to them now, \
             briefly, aloud."
        );
        // One attempt; if the socket was dead, drop it and reconnect once.
        match self.try_respond_text(&prompt).await {
            Ok(reply) => Ok(reply),
            Err(_first) => {
                *self.conn.lock().await = None;
                self.try_respond_text(&prompt).await
            },
        }
    }

    /// Audio-in: let the realtime model HEAR the question (Phase 2). Falls back to
    /// the text path when no audio was captured.
    async fn respond_with_audio(
        &self,
        utterance: &str,
        audio: Option<&UtteranceAudio>,
        context: &str,
    ) -> Result<SpokenReply, ResponderError> {
        let Some(audio) = audio else {
            return self.respond(utterance, context).await;
        };
        match self.try_respond_audio(audio, context).await {
            Ok(reply) => Ok(reply),
            Err(_first) => {
                // A fresh connection lost the seeded context — force a re-seed.
                *self.conn.lock().await = None;
                *self.last_context.lock().await = None;
                self.try_respond_audio(audio, context).await
            },
        }
    }
}

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
//!   1. `POST /media/sessions` mints a `voice_session_id`. Google Meet, and a
//!      WhatsApp call whose other party is not the owner's phone number,
//!      register as `meeting_bot` and seat envoy. A WhatsApp call registers as
//!      `web_desktop` and seats the personal assistant only when that other
//!      party is a phone number matching the configured owner number. A
//!      contact name is not that match. Tool catalogs are not chosen here.
//!   2. Open the control WS (non-browser: no `Origin` header).
//!   3. `session.start` with the backend-proxied profile. A WhatsApp or Google
//!      Meet call passes `turn_boundary: server_vad` and streams the capture
//!      as it arrives, so the realtime model takes the turns. `voice_mode` is
//!      not set, so this is not the hands-free cascade.
//!   4. The wake-loop fallback (a noop, a test, or `realtime-direct`) still
//!      leaves turn detection to the profile and sends one clip
//!      (`ptt.engage` → PCM → `ptt.release`). The spoken reply is 24 kHz mono
//!      PCM16. That fallback is not a WhatsApp or Meet call mode.
//!
//! Config (env): `MEET_BOT_MAGICIAN_URL` (default the local server),
//! `MEET_BOT_PRINCIPAL` / `MEET_BOT_WORKSPACE` (default `anonymous` / `default`),
//! `MEET_BOT_BEARER_TOKEN` (scoped credential; falls back to
//! `MAGICIAN_BEARER_TOKEN`),
//! `MEET_BOT_THREAD` (default `meeting-bot`), `MEET_BOT_REALTIME_PROFILE`
//! (default `voice_realtime_openai_backend`).

use std::io::Read;
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
    LiveDownlink, LiveSession, LiveUplinkChunk, MeetingResponder, ResponderError, SpokenReply,
    UtteranceAudio,
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
/// Personal assistant. Only a WhatsApp call whose other party is the owner's
/// phone number registers here. `voice_source_surface` maps this to
/// `realtime_voice`.
const OWNER_CALL_SURFACE: &str = "web_desktop";
/// Shared room. Google Meet and every other WhatsApp caller register here,
/// which seats envoy.
const ROOM_CALL_SURFACE: &str = "meeting_bot";

/// Who this call binds. Decided from the join URL and the other party's
/// phone number. It does not change that agent's tool list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallSeat {
    Envoy,
    PersonalAssistant,
}

impl CallSeat {
    fn surface_type(self) -> &'static str {
        match self {
            Self::PersonalAssistant => OWNER_CALL_SURFACE,
            Self::Envoy => ROOM_CALL_SURFACE,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::PersonalAssistant => "personal-assistant",
            Self::Envoy => "envoy",
        }
    }
}

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
    /// Taken by a live call. Push-to-talk turns cannot share it.
    inbound_rx: Option<mpsc::Receiver<InboundEvent>>,
    #[allow(dead_code)]
    session_id: String,
    /// The receiver was handed to the live reader.
    live: bool,
}

pub struct OrchestratorVoiceResponder {
    http: reqwest::Client,
    base_url: String,
    principal: String,
    workspace: String,
    bearer_token: Option<String>,
    thread: String,
    profile: String,
    /// Join URL. Empty until the meeting session binds one. Google Meet and an
    /// unbound responder stay on the envoy seat.
    meet_url: String,
    /// Seat chosen when the media session is registered. The live-context line
    /// reads the same value.
    seat: std::sync::Mutex<CallSeat>,
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
            meet_url: String::new(),
            seat: std::sync::Mutex::new(CallSeat::Envoy),
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

    /// Use this realtime profile for this responder only. A blank value keeps
    /// the profile chosen in [`Self::from_env`]. This does not write
    /// `MEET_BOT_REALTIME_PROFILE`.
    pub fn with_profile(mut self, profile: impl Into<String>) -> Self {
        let profile = profile.into();
        let profile = profile.trim();
        if !profile.is_empty() {
            self.profile = profile.to_string();
        }
        self
    }

    /// Join URL this responder seats from. A blank value keeps envoy. This
    /// does not change the process environment.
    pub fn with_meet_url(mut self, url: impl Into<String>) -> Self {
        let url = url.into();
        let url = url.trim();
        if !url.is_empty() {
            self.meet_url = url.to_string();
        }
        self
    }

    /// Use this bearer for `/media/sessions`. A blank value keeps the token
    /// from [`Self::from_env`]. A request token wins over the env token.
    /// This does not write `MEET_BOT_BEARER_TOKEN` or `MAGICIAN_BEARER_TOKEN`.
    pub fn with_bearer(mut self, token: Option<String>) -> Self {
        if let Some(token) = token
            .map(|token| token.trim().to_string())
            .filter(|token| !token.is_empty())
        {
            self.bearer_token = Some(token);
        }
        self
    }

    fn current_seat(&self) -> CallSeat {
        *self.seat.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// Envoy unless this is a WhatsApp call and the other party is the owner's
    /// phone number. Meet, a contact name, and an unknown caller stay envoy.
    fn resolve_seat(&self) -> CallSeat {
        let caller = ring_caller_for_answer(&self.meet_url);
        let owners = configured_owner_phones(&self.principal, &self.workspace);
        seat_for_call(&self.meet_url, caller.as_deref(), &owners)
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
        let seat = self.resolve_seat();
        *self.seat.lock().unwrap_or_else(|err| err.into_inner()) = seat;
        tracing::info!(
            target: "meet_bot",
            seat = seat.label(),
            surface = seat.surface_type(),
            "call agent seat"
        );
        let resp = request
            .timeout(Duration::from_secs(15))
            .json(&registration_body(&self.thread, seat))
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
    /// `turn_boundary` is `Some("server_vad")` for a live call and `None` for
    /// the wake-loop fallback, which leaves the profile's turn detection in place.
    async fn open_conn(&self, turn_boundary: Option<&str>) -> Result<Conn, ResponderError> {
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

        // Start the upstream realtime session. A live call asks for server VAD.
        // The wake-loop fallback leaves turn detection to the profile.
        outbound_tx
            .send(Message::Text(session_start_message(
                &self.thread,
                &self.profile,
                turn_boundary,
            )))
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
            inbound_rx: Some(inbound_rx),
            session_id,
            live: false,
        })
    }

    /// Text path: send the transcribed prompt and let the live session voice it.
    async fn try_respond_text(&self, prompt: &str) -> Result<SpokenReply, ResponderError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_conn(None).await?);
        }
        let conn = guard.as_mut().expect("conn just set");
        drain_stale(inbound_mut(conn)?)?;

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

        collect_reply(inbound_mut(conn)?).await
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
            *guard = Some(self.open_conn(None).await?);
        }
        let conn = guard.as_mut().expect("conn just set");
        drain_stale(inbound_mut(conn)?)?;

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
                        "text": live_context_text(context, self.current_seat()),
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

        collect_reply(inbound_mut(conn)?).await
    }
}

/// `session.start` for this responder. `turn_boundary` is omitted for the
/// wake-loop fallback. A live call sets `server_vad` and does not set
/// `voice_mode`, so the socket stays on the realtime provider rather than
/// the local cascade.
fn registration_body(thread: &str, seat: CallSeat) -> serde_json::Value {
    json!({
        "thread_id": thread,
        "surface_type": seat.surface_type(),
        "transport": "websocket",
        "display_label": "Meet Bot",
    })
}

/// Grounding for a live call. The transcript stays unspoken. The owner line
/// is only for a WhatsApp call whose other party matched the owner number.
fn live_context_text(context: &str, seat: CallSeat) -> String {
    let guidance = match seat {
        CallSeat::PersonalAssistant => {
            "The other party on this WhatsApp call is the owner. Use your tools, including memory search, and speak the answer they asked for.".to_string()
        },
        CallSeat::Envoy => format!(
            "This is a shared call. {}",
            crate::magician_v2::chat::service::PUBLIC_AGENT_NO_OWNER_BROWSER_INSTRUCTION
        ),
    };
    format!(
        "[Live meeting context — for grounding only, do not read aloud]\n\
         {guidance}\n\
         {context}"
    )
}

/// Google Meet seats envoy. A WhatsApp call seats the personal assistant
/// only when `party` is a phone number in `owner_phones`. A display name,
/// a missing caller, and an owner list that does not match stay envoy.
/// Agent id whose definition decides the teardown memory write.
///
/// Google Meet and a non-owner WhatsApp call name the configured public agent.
/// A WhatsApp call with the owner names the personal assistant. This does not
/// change who speaks on the call.
pub fn memory_agent_for_call(
    meet_url: &str,
    principal: &str,
    workspace: &str,
    envoy_agent_id: &str,
) -> String {
    let caller = ring_caller_for_answer(meet_url);
    let owners = configured_owner_phones(principal, workspace);
    memory_agent_id_for_seat(
        seat_for_call(meet_url, caller.as_deref(), &owners),
        envoy_agent_id,
    )
}

fn memory_agent_id_for_seat(seat: CallSeat, envoy_agent_id: &str) -> String {
    match seat {
        CallSeat::PersonalAssistant => CallSeat::PersonalAssistant.label().to_string(),
        CallSeat::Envoy => {
            let agent_id = envoy_agent_id.trim();
            if agent_id.is_empty() {
                CallSeat::Envoy.label().to_string()
            } else {
                agent_id.to_string()
            }
        },
    }
}

fn seat_for_call(meet_url: &str, party: Option<&str>, owner_phones: &[String]) -> CallSeat {
    use crate::magician_v2::media_seam::whatsapp_web_call::{
        whatsapp_call_intent, whatsapp_party_digits, WhatsAppCallIntent,
    };
    let Some(intent) = whatsapp_call_intent(meet_url) else {
        return CallSeat::Envoy;
    };
    let party = match intent {
        WhatsAppCallIntent::PlaceVoice { phone } => Some(phone),
        WhatsAppCallIntent::AnswerIncoming => party.map(str::to_string),
        WhatsAppCallIntent::InvalidPhone => None,
    };
    match party.as_deref().and_then(whatsapp_party_digits) {
        Some(digits) if owner_phones.iter().any(|owner| owner == &digits) => {
            CallSeat::PersonalAssistant
        },
        _ => CallSeat::Envoy,
    }
}

/// The ring's caller, and only for a WhatsApp answer. A placed call uses the
/// number in its URL. A Meet does not read the ring.
fn ring_caller_for_answer(meet_url: &str) -> Option<String> {
    use crate::magician_v2::media_seam::whatsapp_web_call::{
        whatsapp_call_intent, WhatsAppCallIntent,
    };
    match whatsapp_call_intent(meet_url) {
        Some(WhatsAppCallIntent::AnswerIncoming) => {
            crate::magician_v2::media_seam::whatsapp_call_ring::active_ring_caller()
        },
        _ => None,
    }
}

fn configured_owner_phones(principal: &str, workspace: &str) -> Vec<String> {
    let mut raw = Vec::new();
    for key in ["WHATSAPP_SELF_JID", "WHATSAPP_PHONE"] {
        if let Ok(value) = std::env::var(key) {
            raw.push(value);
        }
    }
    let root = magician_notes_root();
    raw.extend(owner_candidates_from_scope(&root, principal, workspace));
    if let Some(home) = std::env::var_os("WU_HOME") {
        raw.extend(owner_candidates_from_wu_home(std::path::Path::new(&home)));
    }
    collect_owner_phones(raw)
}

fn magician_notes_root() -> std::path::PathBuf {
    crate::magician_v2::artifact_v2::workspace::runtime_root_env().unwrap_or_else(|| {
        let home = std::env::var_os("HOME").unwrap_or_default();
        std::path::PathBuf::from(home).join("MagicianNotes")
    })
}

fn scope_dir_name(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed == "."
        || trimmed == ".."
        || trimmed.contains("..")
        || trimmed.contains(['/', '\\', '\0'])
    {
        return None;
    }
    Some(trimmed.to_string())
}

/// Owner numbers stored for this scope. The bot env file and the wu creds
/// `me.id` phone JID. Kapso's business phone-number id is not an owner number.
fn owner_candidates_from_scope(
    root: &std::path::Path,
    principal: &str,
    workspace: &str,
) -> Vec<String> {
    let Some(principal) = scope_dir_name(principal) else {
        return Vec::new();
    };
    let Some(workspace) = scope_dir_name(workspace) else {
        return Vec::new();
    };
    let scope = root.join("scopes").join(principal).join(workspace);
    let mut raw = Vec::new();
    for name in [".env.development", ".env"] {
        let path = scope.join("bots").join("whatsapp").join(name);
        raw.extend(env_file_owner_values(&path));
    }
    raw.extend(owner_candidates_from_wu_home(
        &scope.join("workdirs").join("home").join(".wu"),
    ));
    raw
}

fn owner_candidates_from_wu_home(wu_home: &std::path::Path) -> Vec<String> {
    let path = wu_home.join("auth").join("creds.json");
    read_limited(&path, 2 * 1024 * 1024)
        .and_then(|raw| creds_phone_jid(&raw))
        .into_iter()
        .collect()
}

fn env_file_owner_values(path: &std::path::Path) -> Vec<String> {
    let Some(raw) = read_limited(path, 256 * 1024) else {
        return Vec::new();
    };
    env_owner_values(&raw)
}

fn env_owner_values(raw: &str) -> Vec<String> {
    const KEYS: [&str; 2] = ["WHATSAPP_SELF_JID", "WHATSAPP_PHONE"];
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if !KEYS.contains(&key.trim()) {
            continue;
        }
        let value = value.trim().trim_matches('"').trim_matches('\'');
        if !value.is_empty() {
            out.push(value.to_string());
        }
    }
    out
}

fn creds_phone_jid(raw: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    for parent in ["me", "account"] {
        let Some(id) = value
            .get(parent)
            .and_then(|item| item.get("id"))
            .and_then(|id| id.as_str())
        else {
            continue;
        };
        if crate::magician_v2::media_seam::whatsapp_web_call::whatsapp_party_digits(id).is_some() {
            return Some(id.to_string());
        }
    }
    None
}

fn collect_owner_phones(candidates: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut phones = Vec::new();
    for candidate in candidates {
        let Some(digits) =
            crate::magician_v2::media_seam::whatsapp_web_call::whatsapp_party_digits(&candidate)
        else {
            continue;
        };
        if !phones.contains(&digits) {
            phones.push(digits);
        }
    }
    phones
}

fn read_limited(path: &std::path::Path, max: usize) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    let mut buf = Vec::new();
    std::io::Read::take(file, max as u64)
        .read_to_end(&mut buf)
        .ok()?;
    String::from_utf8(buf).ok()
}

fn session_start_message(thread: &str, profile: &str, turn_boundary: Option<&str>) -> String {
    let mut payload = json!({
        "ui_thread_id": thread,
        "thread_id": thread,
        "realtime_profile": profile,
    });
    if let Some(boundary) = turn_boundary {
        payload["turn_boundary"] = json!(boundary);
    }
    json!({ "kind": "session.start", "payload": payload }).to_string()
}

/// The event lane, or an error when a live call already owns it.
fn inbound_mut(conn: &mut Conn) -> Result<&mut mpsc::Receiver<InboundEvent>, ResponderError> {
    if conn.live && conn.inbound_rx.is_none() {
        return Err(ResponderError::Failed(
            "voice socket is streaming a live call".to_string(),
        ));
    }
    conn.inbound_rx
        .as_mut()
        .ok_or_else(|| ResponderError::Failed("voice socket has no event lane".to_string()))
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

    async fn warm(&self) -> Result<(), ResponderError> {
        let mut guard = self.conn.lock().await;
        if guard.is_none() {
            *guard = Some(self.open_conn(None).await?);
        }
        Ok(())
    }

    async fn begin_live(&self, context: &str) -> Result<Option<LiveSession>, ResponderError> {
        {
            let mut guard = self.conn.lock().await;
            *guard = None;
        }
        let mut conn = self.open_conn(Some("server_vad")).await?;
        let outbound = conn.outbound_tx.clone();
        let inbound = conn.inbound_rx.take().ok_or_else(|| {
            ResponderError::Failed("live voice socket has no event lane".to_string())
        })?;
        conn.live = true;
        *self.conn.lock().await = Some(conn);
        tracing::info!(
            target: "meet_bot",
            profile = %self.profile,
            "realtime session streaming with server vad"
        );

        let context = context.trim();
        if !context.is_empty() {
            if let Err(err) = send_text(
                &outbound,
                json!({
                    "kind": "user.text",
                    "payload": {
                        "text": live_context_text(context, self.current_seat()),
                        "inject": true,
                        "request_response": false
                    }
                }),
            )
            .await
            {
                *self.conn.lock().await = None;
                return Err(err);
            }
            *self.last_context.lock().await = Some(context.to_string());
        }

        let (down_tx, down_rx) = mpsc::channel::<LiveDownlink>(64);
        tokio::spawn(async move {
            let mut inbound = inbound;
            while let Some(ev) = inbound.recv().await {
                let down = match ev {
                    InboundEvent::Audio(pcm) => LiveDownlink::Audio(bytes::Bytes::from(pcm)),
                    InboundEvent::AssistantText(text) => LiveDownlink::Text(text),
                    InboundEvent::Error(msg) => {
                        tracing::warn!(target: "meet_bot", error = %msg, "live voice error");
                        continue;
                    },
                    InboundEvent::Closed => {
                        let _ = down_tx.send(LiveDownlink::Closed).await;
                        break;
                    },
                    InboundEvent::Ready => continue,
                };
                if down_tx.send(down).await.is_err() {
                    break;
                }
            }
        });

        let (up_tx, mut up_rx) = mpsc::channel::<LiveUplinkChunk>(64);
        tokio::spawn(async move {
            while let Some(chunk) = up_rx.recv().await {
                let pcm24 =
                    resample_pcm16_mono(&chunk.pcm, chunk.sample_rate_hz, REALTIME_PCM_SAMPLE_RATE);
                for frame in pcm24.chunks(REALTIME_FRAME_BYTES) {
                    if frame.is_empty() {
                        continue;
                    }
                    if outbound
                        .send(Message::Binary(frame.to_vec()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }
        });

        Ok(Some(LiveSession {
            uplink: up_tx,
            downlink: down_rx,
        }))
    }

    async fn end_live(&self) {
        *self.conn.lock().await = None;
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

#[cfg(test)]
mod tests {
    use super::{
        collect_owner_phones, creds_phone_jid, env_owner_values, live_context_text,
        memory_agent_id_for_seat, owner_candidates_from_scope, registration_body, seat_for_call,
        session_start_message, CallSeat,
    };

    const OWNER: &str = "12025550101";

    fn owners() -> Vec<String> {
        vec![OWNER.to_string()]
    }

    #[test]
    fn teardown_names_the_configured_public_agent_or_the_personal_assistant() {
        assert_eq!(memory_agent_id_for_seat(CallSeat::Envoy, "envoy"), "envoy");
        assert_eq!(
            memory_agent_id_for_seat(CallSeat::Envoy, "  public-face  "),
            "public-face"
        );
        assert_eq!(memory_agent_id_for_seat(CallSeat::Envoy, "  "), "envoy");
        assert_eq!(
            memory_agent_id_for_seat(CallSeat::PersonalAssistant, "envoy"),
            "personal-assistant"
        );
    }

    #[test]
    fn google_meet_seats_envoy() {
        let seat = seat_for_call(
            "https://meet.google.com/abc-defg-hij",
            Some(OWNER),
            &owners(),
        );
        assert_eq!(seat, CallSeat::Envoy);
        let body = registration_body("meeting-nainisha", seat);
        assert_eq!(body["surface_type"], "meeting_bot");
        assert_eq!(body["thread_id"], "meeting-nainisha");
        let text = live_context_text("Recent transcript: A: hello", seat);
        assert!(text.contains("do not read aloud"));
        assert!(text.contains("shared call"));
        assert!(text.contains("CDP"));
        assert!(!text.contains("memory search"));
        assert!(!text.contains("owner"));
    }

    #[test]
    fn whatsapp_seats_the_personal_assistant_only_for_the_owner_number() {
        let meet = "https://meet.google.com/abc-defg-hij";
        let answer = "https://web.whatsapp.com/";
        let place = "https://web.whatsapp.com/send?phone=+1%20(202)%20555-0101";
        assert_eq!(
            seat_for_call(answer, Some("+1 (202) 555-0101"), &owners()),
            CallSeat::PersonalAssistant
        );
        assert_eq!(
            seat_for_call(place, Some("Ada"), &owners()),
            CallSeat::PersonalAssistant
        );
        assert_eq!(
            seat_for_call(answer, Some("Ada"), &owners()),
            CallSeat::Envoy
        );
        assert_eq!(
            seat_for_call(answer, Some("Ada 12025550101"), &owners()),
            CallSeat::Envoy
        );
        assert_eq!(seat_for_call(answer, None, &owners()), CallSeat::Envoy);
        assert_eq!(
            seat_for_call(answer, Some("+1 (202) 555-0102"), &owners()),
            CallSeat::Envoy
        );
        assert_eq!(
            seat_for_call(answer, Some(OWNER), &Vec::<String>::new()),
            CallSeat::Envoy
        );
        assert_eq!(seat_for_call(meet, Some(OWNER), &owners()), CallSeat::Envoy);
        let owner = seat_for_call(answer, Some(&format!("{OWNER}@s.whatsapp.net")), &owners());
        let body = registration_body("meeting-owner", owner);
        assert_eq!(body["surface_type"], "web_desktop");
        let text = live_context_text("Recent transcript: A: what is her birthday?", owner);
        assert!(text.contains("memory search"));
        assert!(text.contains("Recent transcript:"));
        let guest = live_context_text("Recent transcript:", CallSeat::Envoy);
        assert!(!guest.contains("memory search"));
    }

    #[test]
    fn owner_numbers_come_from_the_jid_the_phone_and_wu_creds() {
        let phones = collect_owner_phones([
            "12025550101:12@s.whatsapp.net".to_string(),
            "+1 (202) 555-0101".to_string(),
            "210000000000001@lid".to_string(),
            "Ada".to_string(),
        ]);
        assert_eq!(phones, vec![OWNER.to_string()]);
        assert_eq!(
            creds_phone_jid(
                r#"{"me":{"id":"12025550101:4@s.whatsapp.net","lid":"210000000000001@lid"}}"#
            )
            .as_deref(),
            Some("12025550101:4@s.whatsapp.net")
        );
        assert_eq!(
            creds_phone_jid(r#"{"me":{"id":"210000000000001@lid"}}"#),
            None
        );
        let from_env = env_owner_values(
            &format!("# note\nWHATSAPP_SELF_JID=\"{OWNER}@s.whatsapp.net\"\nKAPSO_PHONE_NUMBER_ID=999\nWHATSAPP_PHONE=+12025550102\n"),
        );
        assert_eq!(
            from_env,
            vec![
                format!("{OWNER}@s.whatsapp.net"),
                "+12025550102".to_string()
            ]
        );
    }

    #[test]
    fn scope_files_supply_owner_numbers_and_skip_a_bad_scope_name() {
        let root = std::env::temp_dir().join(format!(
            "magician-seat-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let env_dir = root
            .join("scopes")
            .join("anonymous")
            .join("default")
            .join("bots")
            .join("whatsapp");
        let creds = root
            .join("scopes")
            .join("anonymous")
            .join("default")
            .join("workdirs")
            .join("home")
            .join(".wu")
            .join("auth");
        std::fs::create_dir_all(&env_dir).unwrap();
        std::fs::create_dir_all(&creds).unwrap();
        std::fs::write(
            env_dir.join(".env.development"),
            "WHATSAPP_PHONE=+12025550102\nOTHER=secret\n",
        )
        .unwrap();
        std::fs::write(
            creds.join("creds.json"),
            r#"{"me":{"id":"12025550101:12@s.whatsapp.net","lid":"210000000000001@lid"}}"#,
        )
        .unwrap();
        let found = owner_candidates_from_scope(&root, "anonymous", "default");
        assert!(found.iter().any(|value| value == "+12025550102"));
        assert!(found
            .iter()
            .any(|value| value == "12025550101:12@s.whatsapp.net"));
        assert!(owner_candidates_from_scope(&root, "../secrets", "default").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn live_call_asks_the_model_to_take_turns() {
        let raw = session_start_message(
            "meeting-bot",
            "voice_realtime_openai_backend",
            Some("server_vad"),
        );
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(value["kind"], "session.start");
        assert_eq!(value["payload"]["turn_boundary"], "server_vad");
        assert_eq!(
            value["payload"]["realtime_profile"],
            "voice_realtime_openai_backend"
        );
        assert!(value["payload"].get("voice_mode").is_none());
    }

    #[test]
    fn wake_fallback_leaves_turn_detection_to_the_profile() {
        let raw = session_start_message("meeting-bot", "voice_realtime_openai_backend", None);
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert!(value["payload"].get("turn_boundary").is_none());
        assert!(value["payload"].get("voice_mode").is_none());
    }
}

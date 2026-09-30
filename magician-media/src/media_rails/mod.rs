//! Realtime media + control rails — shared substrate.
//!
//! Owns the in-memory `RealtimeSessionRegistry` plus the canonical
//! types every client surface (web mobile / web desktop / tray /
//! extension) advertises against. Implements the surface-lifecycle
//! contract described in
//! `docs/archive/plans/2026-05-15-realtime-media-control-rails.md`
//! and the realtime-voice architecture in
//! `docs/archive/plans/2026-05-19-voice-architecture-refactor.md`:
//!
//! * A connected surface registers a session with its capability
//!   bitfield and initial permission snapshot.
//! * Capabilities and permissions are mutated in place via PATCH; each
//!   change emits a `media.*` event on the broadcaster so downstream
//!   subscribers (UI dashboards, automation eval harness, future tray
//!   bridge) can react.
//! * Client surfaces post per-channel lifecycle events
//!   (`media.tts.*`, `media.capture.*`, `media.transcript.*`, etc.)
//!   through the same registry so cancellation / scope routing /
//!   debug visibility all flow off one substrate.
//!
//! The registry is intentionally process-local: it is the authority
//! for "is this surface still connected", which is meaningless across
//! restarts. Heartbeats are required to keep a session alive — the
//! registry prunes stale sessions lazily on every operation.
pub use magician::magician_v2::media_seam::*;

use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use magician::magician_v2::realtime_events::{RuntimeAgentEventType, RuntimeTransportBroadcaster};

pub use magician::magician_v2::media_seam::audio_surface;
pub use magician::magician_v2::media_seam::meeting;
pub use magician::magician_v2::media_seam::tts_types as providers_tts;
pub use magician::magician_v2::media_seam::voice_fanout;

pub use magician::magician_v2::media_seam::{
    task_awaiting_diff_approval_message, validate_media_settings,
};

pub mod fluid_audio;
pub mod hands_free;
mod local_realtime_transcript;
pub mod providers;
pub mod screen_capture;
pub mod screen_observe;
pub mod self_echo;
pub mod voice_addressing;
pub mod voice_context_compactor;
pub mod voice_downstream_fanout;
pub mod voice_orchestrator;
pub mod voice_session_lifecycle;
pub mod voice_tool_dispatcher;

pub use hands_free::CascadedHandsFreeProvider;
pub use local_realtime_transcript::{
    start_local_realtime_transcript, LocalRealtimeTranscriptCommandError,
    LocalRealtimeTranscriptEvent, LocalRealtimeTranscriptHandle,
    LocalRealtimeTranscriptQueueSnapshot,
};

pub use providers::{
    default_base_url_for_location as google_cloud_speech_default_base_url_for_location,
    recognizer_name as google_cloud_speech_recognizer_name, AudioChunk, CachedTtsProvider,
    ConfiguredTtsProvider, DiarizationCapabilities, DiarizationError, DiarizationEvent,
    DiarizationProvider, DiarizationSession, DiarizationSessionConfig,
    GeminiLiveTranscribeSttProvider, GeminiSttProvider, GeminiTranscribeSttProvider,
    GeminiTtsProvider, GoogleCloudSpeechSttProvider, GrokSttProvider, GrokTtsProvider,
    MacOsSpeechProvider,
    MacOsTtsProvider, MediaProviderRegistry, MiniMaxTtsProvider, NoopDiarizationProvider,
    NoopVadProvider, OpenAiTtsProvider, OpenAiWhisperProvider, RealtimeVoiceProfileInfo,
    SpeakerSegment, StreamAudioFormat, StreamSampleFormat, StreamingSttCapabilities,
    StreamingSttEvent, StreamingSttProvider, StreamingSttSession, SttError, SttProvider,
    SttRequest, SttResponse, SttStreamEvent, TtsCacheStats, TtsEmotion, TtsError, TtsPace,
    TtsProvider, TtsRequest, TtsResponse, TtsStyle, TtsVoiceMode, VadCapabilities, VadError,
    VadEvent, VadProvider, VadSession, VadSessionConfig, GEMINI_LIVE_TRANSCRIBE_DEFAULT_MODEL,
    GEMINI_LIVE_TRANSCRIBE_DEFAULT_WS_URL, GEMINI_LIVE_TRANSCRIBE_PROVIDER_ID,
    GEMINI_STT_DEFAULT_BASE_URL, GEMINI_STT_DEFAULT_INLINE_MAX_BYTES, GEMINI_STT_DEFAULT_MODEL,
    GEMINI_STT_DEFAULT_PROMPT, GEMINI_STT_PROVIDER_ID, GEMINI_TRANSCRIBE_DEFAULT_BASE_URL,
    GEMINI_TRANSCRIBE_DEFAULT_MODEL, GEMINI_TRANSCRIBE_PROVIDER_ID, GEMINI_TTS_DEFAULT_BASE_URL,
    GEMINI_TTS_DEFAULT_FORMAT, GEMINI_TTS_DEFAULT_MODEL, GEMINI_TTS_DEFAULT_VOICE,
    GOOGLE_CLOUD_SPEECH_STT_DEFAULT_BASE_URL, GOOGLE_CLOUD_SPEECH_STT_DEFAULT_LOCATION,
    GOOGLE_CLOUD_SPEECH_STT_DEFAULT_MODEL, GOOGLE_CLOUD_SPEECH_STT_DEFAULT_RECOGNIZER,
    GOOGLE_CLOUD_SPEECH_STT_PROVIDER_ID, GROK_STT_DEFAULT_BASE_URL, GROK_STT_DEFAULT_MODEL,
    GROK_STT_PROVIDER_ID, GROK_TTS_DEFAULT_BASE_URL, GROK_TTS_DEFAULT_FORMAT,
    GROK_TTS_DEFAULT_MODEL, GROK_TTS_DEFAULT_VOICE, GROK_TTS_PROVIDER_ID,
    MACOS_SPEECH_DEFAULT_GATEWAY_URL, MACOS_TTS_DEFAULT_FORMAT, MACOS_TTS_DEFAULT_GATEWAY_URL,
    MACOS_TTS_DEFAULT_MODEL, MACOS_TTS_PROVIDER_ID, MINIMAX_TTS_DEFAULT_BASE_URL,
    MINIMAX_TTS_DEFAULT_FORMAT, MINIMAX_TTS_DEFAULT_MODEL, MINIMAX_TTS_DEFAULT_VOICE,
    OPENAI_TTS_DEFAULT_BASE_URL, OPENAI_TTS_DEFAULT_MODEL, OPENAI_WHISPER_DEFAULT_BASE_URL,
    OPENAI_WHISPER_DEFAULT_MODEL, OPENAI_WHISPER_PROVIDER_ID,
};

pub use speech_segments::{has_speech_tags, parse_speech_segments, SpeechSegment};

pub use voice_downstream_fanout::{
    task_completed_message, VoiceDownstreamFanout, VoiceDownstreamMessage,
};

pub use voice_session_lifecycle::{VoiceSessionLifecycle, VoiceSessionLifecycleStore};

pub use voice_context_compactor::{
    ReplayToolExchange, ReplayTurn, ResumeContext, VoiceContextCompactor,
};

pub use self_echo::{SelfEchoMatch, SelfEchoSuppressor};

pub use voice_addressing::{VoiceAddressing, VoiceAddressingDecision};

pub use voice_orchestrator::{
    CascadedVoiceTurnOutcome, OrchestratorError, RotateReason, RotationResult, VoiceOrchestrator,
};

pub use voice_tool_dispatcher::{VoiceToolDispatchResponse, VoiceToolDispatchStatus};

/// Synthetic agent id used for all media-rails events. Lets the
/// transport broadcaster route via the standard `emit_named` path
/// without conflating media lifecycle with any real personal-agent
/// timeline.

pub const MEDIA_AUDIO_ENGINE_STARTED: &str =
    RuntimeAgentEventType::MediaAudioEngineStarted.as_str();
pub const MEDIA_AUDIO_ENGINE_STOPPED: &str =
    RuntimeAgentEventType::MediaAudioEngineStopped.as_str();
pub const MEDIA_AUDIO_ENGINE_UNHEALTHY: &str =
    RuntimeAgentEventType::MediaAudioEngineUnhealthy.as_str();
pub const MEDIA_AUDIO_MODEL_LOADING: &str = RuntimeAgentEventType::MediaAudioModelLoading.as_str();
pub const MEDIA_AUDIO_MODEL_LOADED: &str = RuntimeAgentEventType::MediaAudioModelLoaded.as_str();
pub const MEDIA_AUDIO_MODEL_UNLOADED: &str =
    RuntimeAgentEventType::MediaAudioModelUnloaded.as_str();

/// Sessions without a heartbeat for this long are pruned lazily on
/// every registry operation. Two missed heartbeats at the recommended
/// 60s client cadence — long enough to tolerate a tab going to sleep
/// briefly, short enough that an actually-closed surface stops
/// appearing as "connected" within a couple of minutes.
pub const SESSION_STALE_AFTER: Duration = Duration::from_secs(180);

/// Hard cap on the number of active sessions per `(principal, workspace)`
/// scope. Prevents a misbehaving client from registering unbounded
/// sessions and burning DashMap memory. 50 is generous — a real human
/// has at most a handful of tabs open per workspace.
pub const MAX_SESSIONS_PER_SCOPE: usize = 50;

// ─── Surface taxonomy ────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceType {
    WebMobile,
    WebDesktop,
    MascotMacos,
    MascotWindows,
    MascotLinux,
    TrayMacos,
    TrayWindows,
    TrayLinux,
    Extension,
    /// The ESP32-C6 desk terminal (`magesp`). Serialises as `esp_terminal`,
    /// matching the `source_surface` its voice notes carry.
    EspTerminal,
    /// The meeting bot, audible to a room whose membership the owner does not
    /// control. Registered by the responder itself: a client may **narrow**
    /// itself onto a stricter surface, which is why this one is safe to accept
    /// from the request. Nothing may travel the other way — no label promotes a
    /// registration to an owner surface.
    MeetingBot,
    #[default]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum TransportType {
    #[default]
    Sse,
    Websocket,
    Webrtc,
    Bridge,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Connected,
    Paused,
    Revoked,
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PermissionState {
    #[default]
    Unknown,
    Granted,
    Denied,
    Revoked,
}

// ─── Capability + permission bitfields ───────────────────────────────

/// What a client surface claims it can do. Booleans on purpose: this
/// is advertised state from the client, not a server-side feature
/// gate. Defaults to "nothing" so an unknown surface gets the most
/// conservative treatment.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SurfaceCapabilities {
    #[serde(default)]
    pub mascot_overlay: bool,
    #[serde(default)]
    pub text_bubble: bool,
    #[serde(default)]
    pub browser_tts: bool,
    #[serde(default)]
    pub provider_tts: bool,
    #[serde(default)]
    pub realtime_voice: bool,
    #[serde(default)]
    pub mic: bool,
    #[serde(default)]
    pub camera: bool,
    #[serde(default)]
    pub screen_capture: bool,
    #[serde(default)]
    pub pointer_overlay: bool,
    #[serde(default)]
    pub system_audio: bool,
    #[serde(default)]
    pub desktop_action: bool,
}

/// User-granted permissions for media channels. Mirrors the plan doc
/// shape so a single PATCH covers every permission a surface might
/// expose. Defaults to `Unknown` everywhere — the runtime never
/// assumes permission until a surface explicitly advertises it.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MediaPermissions {
    #[serde(default)]
    pub mic: PermissionState,
    #[serde(default)]
    pub camera: PermissionState,
    #[serde(default)]
    pub screen_capture: PermissionState,
    #[serde(default)]
    pub system_audio: PermissionState,
    #[serde(default)]
    pub transcription: PermissionState,
    /// Whether raw media (audio/screen frames) may be persisted to the
    /// artifact ledger. Defaults to `Unknown` (treated as denied):
    /// raw media is ephemeral unless the user explicitly opts in.
    #[serde(default)]
    pub raw_media_persistence: PermissionState,
}

// ─── Session record ──────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeSession {
    pub session_id: String,
    pub principal: String,
    pub workspace: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub surface_type: SurfaceType,
    #[serde(default)]
    pub transport: TransportType,
    pub status: SessionStatus,
    #[serde(default)]
    pub capabilities: SurfaceCapabilities,
    #[serde(default)]
    pub permissions: MediaPermissions,
    pub created_at_ms: i64,
    pub last_seen_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_label: Option<String>,
    /// Workflow-level audio surface. Generic page-presence sessions leave this
    /// unset; audio workflow sessions persist their resolved profile below.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_surface: Option<AudioSurface>,
    /// Immutable resolution captured when the session starts. Runtime settings
    /// changes apply only after an explicit new session/restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_audio_profile: Option<ResolvedAudioProfile>,
}

// ─── Event type constants ────────────────────────────────────────────

pub const MEDIA_SESSION_UPDATED: &str = "media.session.updated";
pub const MEDIA_SESSION_DISCONNECTED: &str = "media.session.disconnected";
pub const MEDIA_SESSION_REVOKED: &str = "media.session.revoked";
pub const MEDIA_CAPABILITIES_UPDATED: &str = "media.capabilities.updated";
pub const MEDIA_PERMISSION_GRANTED: &str = "media.permission.granted";
pub const MEDIA_PERMISSION_DENIED: &str = "media.permission.denied";
pub const MEDIA_PERMISSION_REVOKED: &str = "media.permission.revoked";

pub const MEDIA_TTS_STARTED: &str = "media.tts.started";
pub const MEDIA_TTS_COMPLETED: &str = "media.tts.completed";
pub const MEDIA_TTS_CANCELLED: &str = "media.tts.cancelled";
pub const MEDIA_TTS_ERROR: &str = "media.tts.error";

pub const MEDIA_STT_STARTED: &str = "media.stt.started";
pub const MEDIA_TRANSCRIPT_DELTA: &str = "media.transcript.delta";
pub const MEDIA_TRANSCRIPT_FINAL: &str = "media.transcript.final";
pub const MEDIA_STT_ERROR: &str = "media.stt.error";
pub const MEDIA_VOICE_NOTE_RECORDING_STARTED: &str = "media.voice_note.recording_started";
pub const MEDIA_VOICE_NOTE_RECORDING_STOPPED: &str = "media.voice_note.recording_stopped";
pub const MEDIA_VOICE_NOTE_RECORDING_FAILED: &str = "media.voice_note.recording_failed";
pub const MEDIA_VOICE_NOTE_RECEIVED: &str = "media.voice_note.received";
pub const MEDIA_VOICE_NOTE_TRANSCRIPTION_STARTED: &str = "media.voice_note.transcription_started";
pub const MEDIA_VOICE_NOTE_TRANSCRIPTION_COMPLETED: &str =
    "media.voice_note.transcription_completed";
pub const MEDIA_VOICE_NOTE_TRANSCRIPTION_FAILED: &str = "media.voice_note.transcription_failed";
pub const MEDIA_VOICE_NOTE_TRANSCRIBED: &str = "media.voice_note.transcribed";
pub const MEDIA_VOICE_NOTE_CHAT_SUBMIT_STARTED: &str = "media.voice_note.chat_submit_started";
pub const MEDIA_VOICE_NOTE_CHAT_SUBMIT_COMPLETED: &str = "media.voice_note.chat_submit_completed";
pub const MEDIA_VOICE_NOTE_CHAT_SUBMIT_FAILED: &str = "media.voice_note.chat_submit_failed";
pub const MEDIA_VOICE_NOTE_SUBMITTED: &str = "media.voice_note.submitted";
pub const MEDIA_VOICE_NOTE_ERROR: &str = "media.voice_note.error";

pub const MEDIA_CAPTURE_STARTED: &str = "media.capture.started";
pub const MEDIA_CAPTURE_COMPLETED: &str = "media.capture.completed";
pub const MEDIA_CAPTURE_CANCELLED: &str = "media.capture.cancelled";
pub const MEDIA_CAPTURE_ERROR: &str = "media.capture.error";

pub const MEDIA_POINTER_COMMANDED: &str = "media.pointer.commanded";
pub const MEDIA_ARTIFACT_CREATED: &str = "media.artifact.created";

pub const MEDIA_MASCOT_VISIBLE: &str = "media.mascot.visible";
pub const MEDIA_MASCOT_HIDDEN: &str = "media.mascot.hidden";
pub const MEDIA_MASCOT_INVOKED: &str = "media.mascot.invoked";
pub const MEDIA_MASCOT_BUBBLE_OPENED: &str = "media.mascot.bubble.opened";
pub const MEDIA_MASCOT_BUBBLE_CLOSED: &str = "media.mascot.bubble.closed";
pub const MEDIA_MASCOT_STATE_CHANGED: &str = "media.mascot.state.changed";
pub const MEDIA_MASCOT_QUIET_CHANGED: &str = "media.mascot.quiet.changed";

// Voice session lifecycle events — long-lived voice sessions span
// multiple upstream rotations; these surface each transition on the
// /events stream so observability dashboards and the obs panel can
// follow recovery activity in real time.
pub const MEDIA_VOICE_SESSION_MINTED: &str = "media.voice.session.minted";
pub const MEDIA_VOICE_SESSION_ROTATED: &str = "media.voice.session.rotated";
/// Boundary a call resolved to, emitted once when the session goes ready.
/// Content-free: server-derived labels and a binding hash only.
pub const MEDIA_VOICE_SURFACE_RESOLVED: &str = "media.voice.surface.resolved";
pub const MEDIA_VOICE_SESSION_RECONNECT_ATTEMPT: &str = "media.voice.session.reconnect_attempt";
pub const MEDIA_VOICE_SESSION_RECONNECT_FAILED: &str = "media.voice.session.reconnect_failed";
pub const MEDIA_VOICE_SESSION_COMPACTION: &str = "media.voice.session.compaction";
pub const MEDIA_VOICE_BRIDGE_CONNECTED: &str = "media.voice.bridge.connected";
pub const MEDIA_VOICE_BRIDGE_DISCONNECTED: &str = "media.voice.bridge.disconnected";
pub const MEDIA_VOICE_CLIENT_MESSAGE: &str = "media.voice.client_message";
pub const MEDIA_VOICE_CLIENT_AUDIO: &str = "media.voice.client_audio";
pub const MEDIA_VOICE_CONTROLLER_COMMAND: &str = "media.voice.controller_command";
pub const MEDIA_VOICE_BRIDGE_ERROR: &str = "media.voice.bridge.error";
pub const MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE: &str = "media.voice.local_transcript.state";
pub const MEDIA_VOICE_LOCAL_TRANSCRIPT_QUEUE: &str = "media.voice.local_transcript.queue";
pub const MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN: &str = "media.voice.local_transcript.turn";
pub const MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK: &str = "media.voice.local_transcript.fallback";

/// Whitelist of event types a client surface may post via the public
/// media events endpoint. Lifecycle/permission/registry events are
/// owned by the registry itself and never re-published by clients.
pub const CLIENT_POSTABLE_EVENT_TYPES: &[&str] = &[
    MEDIA_TTS_STARTED,
    MEDIA_TTS_COMPLETED,
    MEDIA_TTS_CANCELLED,
    MEDIA_TTS_ERROR,
    MEDIA_STT_STARTED,
    MEDIA_TRANSCRIPT_DELTA,
    MEDIA_TRANSCRIPT_FINAL,
    MEDIA_STT_ERROR,
    MEDIA_CAPTURE_STARTED,
    MEDIA_CAPTURE_COMPLETED,
    MEDIA_CAPTURE_CANCELLED,
    MEDIA_CAPTURE_ERROR,
    MEDIA_POINTER_COMMANDED,
    MEDIA_ARTIFACT_CREATED,
    MEDIA_MASCOT_VISIBLE,
    MEDIA_MASCOT_HIDDEN,
    MEDIA_MASCOT_INVOKED,
    MEDIA_MASCOT_BUBBLE_OPENED,
    MEDIA_MASCOT_BUBBLE_CLOSED,
    MEDIA_MASCOT_STATE_CHANGED,
    MEDIA_MASCOT_QUIET_CHANGED,
    MEDIA_PERMISSION_GRANTED,
    MEDIA_PERMISSION_DENIED,
    MEDIA_PERMISSION_REVOKED,
    MEDIA_VOICE_BRIDGE_CONNECTED,
    MEDIA_VOICE_BRIDGE_DISCONNECTED,
    MEDIA_VOICE_CLIENT_MESSAGE,
    MEDIA_VOICE_CLIENT_AUDIO,
    MEDIA_VOICE_CONTROLLER_COMMAND,
    MEDIA_VOICE_BRIDGE_ERROR,
];

// ─── Registry errors ─────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum MediaRegistryError {
    #[error("media session not found: {0}")]
    NotFound(String),
    #[error("media event type not allowed: {0}")]
    EventTypeNotAllowed(String),
    #[error("media session scope mismatch")]
    ScopeMismatch,
    #[error("too many media sessions for this scope (limit: {0})")]
    TooManySessions(usize),
}

/// Result of attempting to claim an ephemeral upstream voice token.
/// Lets the relay endpoint surface a useful error rather than fail
/// silently on the upstream WebSocket handshake.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VoiceUpstreamTokenStatus {
    /// Token was minted and is still within its freshness window.
    Fresh(String),
    /// Token was minted but the TTL elapsed before this call. The
    /// caller can recover by asking the orchestrator to rotate the
    /// upstream session.
    Expired,
    /// No token was ever parked for this media session id.
    Missing,
}

// ─── Registry ────────────────────────────────────────────────────────

/// Process-local registry of connected realtime media surfaces.
///
/// Cloning is cheap (Arc inside). Mutations are serialised per-session
/// via DashMap. Stale sessions are pruned lazily on every operation;
/// no background task is required.
#[derive(Clone)]
pub struct RealtimeSessionRegistry {
    sessions: Arc<DashMap<String, RealtimeSession>>,
    last_prune: Arc<std::sync::Mutex<Instant>>,
    /// Serialises `register()` so the
    /// `contains_key` → `iter().count()` → `insert` sequence runs
    /// atomically. DashMap shards by hash and gives no cross-key
    /// atomicity, so without this lock two concurrent registrations
    /// in the same scope can both observe `count == MAX-1`, both
    /// pass the cap check, and both insert — bumping the scope to
    /// `MAX+1`. `register()` is per-tab-mount cold path; the lock
    /// contention is negligible.
    register_lock: Arc<std::sync::Mutex<()>>,
    /// Ephemeral upstream voice tokens (`ek_…`) minted by the realtime
    /// voice provider for each `media_session_id`. OpenAI gives these
    /// a 60 s TTL — we park them here so the upstream-relay code path
    /// (when it lands) can authenticate the upstream WebSocket
    /// connection without re-minting on every reconnect. Tuple is
    /// `(minted_at, token)`; we prune entries older than 90 s lazily.
    voice_upstream_tokens: Arc<DashMap<String, (Instant, String)>>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl std::fmt::Debug for RealtimeSessionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RealtimeSessionRegistry")
            .field("sessions", &self.sessions.len())
            .finish_non_exhaustive()
    }
}

impl RealtimeSessionRegistry {
    pub fn new(broadcaster: Arc<RuntimeTransportBroadcaster>) -> Self {
        Self {
            sessions: Arc::new(DashMap::new()),
            last_prune: Arc::new(std::sync::Mutex::new(Instant::now())),
            register_lock: Arc::new(std::sync::Mutex::new(())),
            voice_upstream_tokens: Arc::new(DashMap::new()),
            broadcaster,
        }
    }

    /// Park an ephemeral upstream voice token (`ek_…`) for the given
    /// media session. Replaces any existing token for the same id.
    pub fn store_voice_upstream_token(&self, media_session_id: &str, token: String) {
        self.voice_upstream_tokens
            .insert(media_session_id.to_string(), (Instant::now(), token));
    }

    /// Consume the ephemeral upstream voice token for the given
    /// session. Consume-once semantics: the entry is removed on
    /// the fresh path; expired entries are also removed (so the
    /// next claimant sees `Missing` not `Expired`). The typed
    /// return lets callers distinguish "never minted" from "minted
    /// but the TTL elapsed before claim" — the latter case can
    /// recover by asking the voice orchestrator to rotate the
    /// upstream session instead of silently failing the upstream
    /// handshake with an unexplained auth error.
    pub fn take_voice_upstream_token(&self, media_session_id: &str) -> VoiceUpstreamTokenStatus {
        const VOICE_TOKEN_TTL: Duration = Duration::from_secs(90);
        let entry = match self.voice_upstream_tokens.remove(media_session_id) {
            Some(entry) => entry,
            None => return VoiceUpstreamTokenStatus::Missing,
        };
        let (minted_at, token) = entry.1;
        if minted_at.elapsed() > VOICE_TOKEN_TTL {
            return VoiceUpstreamTokenStatus::Expired;
        }
        VoiceUpstreamTokenStatus::Fresh(token)
    }

    pub fn broadcaster(&self) -> Arc<RuntimeTransportBroadcaster> {
        Arc::clone(&self.broadcaster)
    }

    /// Insert / replace a session and emit `media.session.registered`.
    ///
    /// Returns `TooManySessions` when the caller's scope already has
    /// `MAX_SESSIONS_PER_SCOPE` live sessions — handlers map that to
    /// HTTP 429. Replacing an existing `session_id` (re-register) is
    /// always allowed even at the cap.
    pub fn register(
        &self,
        session: RealtimeSession,
    ) -> Result<RealtimeSession, MediaRegistryError> {
        self.prune_stale_if_due();
        // Serialise the contains/count/insert sequence so concurrent
        // registrations against the same scope can't both pass the
        // cap check. `Mutex::lock` only fails on poisoning; on poison
        // we fail closed (treat as too-many) rather than silently
        // skipping the cap.
        let _guard = match self.register_lock.lock() {
            Ok(g) => g,
            Err(_) => return Err(MediaRegistryError::TooManySessions(MAX_SESSIONS_PER_SCOPE)),
        };
        let is_replacement = self.sessions.contains_key(&session.session_id);
        if !is_replacement {
            let scope_count = self
                .sessions
                .iter()
                .filter(|entry| {
                    entry.principal == session.principal && entry.workspace == session.workspace
                })
                .count();
            if scope_count >= MAX_SESSIONS_PER_SCOPE {
                return Err(MediaRegistryError::TooManySessions(MAX_SESSIONS_PER_SCOPE));
            }
        }
        self.sessions
            .insert(session.session_id.clone(), session.clone());
        self.emit_session_event(MEDIA_SESSION_REGISTERED, &session, None);
        Ok(session)
    }

    /// Update capabilities for a known session.
    pub fn update_capabilities(
        &self,
        session_id: &str,
        capabilities: SurfaceCapabilities,
    ) -> Result<RealtimeSession, MediaRegistryError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        entry.capabilities = capabilities;
        entry.last_seen_at_ms = now_ms();
        let snapshot = entry.clone();
        drop(entry);
        self.emit_session_event(MEDIA_CAPABILITIES_UPDATED, &snapshot, None);
        self.emit_session_event(MEDIA_SESSION_UPDATED, &snapshot, None);
        Ok(snapshot)
    }

    /// Update permissions for a known session. Emits a per-channel
    /// `media.permission.granted|denied|revoked` event for each
    /// channel that transitioned, plus a single
    /// `media.session.updated` summary so generic subscribers still
    /// see one rolled-up change notice.
    pub fn update_permissions(
        &self,
        session_id: &str,
        permissions: MediaPermissions,
    ) -> Result<RealtimeSession, MediaRegistryError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        let before = entry.permissions.clone();
        entry.permissions = permissions;
        entry.last_seen_at_ms = now_ms();
        let snapshot = entry.clone();
        drop(entry);

        for (channel, prior, next) in permission_diff(&before, &snapshot.permissions) {
            let event_type = match next {
                PermissionState::Granted => MEDIA_PERMISSION_GRANTED,
                PermissionState::Denied => MEDIA_PERMISSION_DENIED,
                PermissionState::Revoked => MEDIA_PERMISSION_REVOKED,
                PermissionState::Unknown => continue,
            };
            self.emit_session_event(
                event_type,
                &snapshot,
                Some(json!({
                    "channel": channel,
                    "previous": prior,
                    "current": next,
                })),
            );
        }
        self.emit_session_event(MEDIA_SESSION_UPDATED, &snapshot, None);
        Ok(snapshot)
    }

    pub fn heartbeat(&self, session_id: &str) -> Result<RealtimeSession, MediaRegistryError> {
        let mut entry = self
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        entry.last_seen_at_ms = now_ms();
        entry.status = SessionStatus::Connected;
        let snapshot = entry.clone();
        drop(entry);
        // Constant, and cheap only because the progress router refuses to
        // journal them: `MEDIA_SESSION_HEARTBEAT` is listed in
        // `progress_channel_seam::types::EPHEMERAL_PROGRESS_EVENT_TYPES`, so this
        // broadcasts to live subscribers without a durable append. Emitted at
        // info severity without user_relevant so it stays off operator
        // surfaces. Anything added here that is *not* ephemeral pays an fsync
        // per session per interval.
        self.emit_session_event(MEDIA_SESSION_HEARTBEAT, &snapshot, None);
        Ok(snapshot)
    }

    pub fn disconnect(&self, session_id: &str) -> Result<RealtimeSession, MediaRegistryError> {
        let removed = self
            .sessions
            .remove(session_id)
            .map(|(_, s)| s)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        let mut closed = removed.clone();
        closed.status = SessionStatus::Disconnected;
        closed.last_seen_at_ms = now_ms();
        self.emit_session_event(MEDIA_SESSION_DISCONNECTED, &closed, None);
        Ok(closed)
    }

    pub fn revoke(
        &self,
        session_id: &str,
        reason: Option<&str>,
    ) -> Result<RealtimeSession, MediaRegistryError> {
        let removed = self
            .sessions
            .remove(session_id)
            .map(|(_, s)| s)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        let mut closed = removed.clone();
        closed.status = SessionStatus::Revoked;
        closed.last_seen_at_ms = now_ms();
        self.emit_session_event(
            MEDIA_SESSION_REVOKED,
            &closed,
            reason.map(|r| json!({ "reason": r })),
        );
        Ok(closed)
    }

    pub fn get(&self, session_id: &str) -> Option<RealtimeSession> {
        self.sessions.get(session_id).map(|e| e.clone())
    }

    /// List active sessions for a scope. Sessions that have gone stale
    /// since the last operation are filtered out (and dropped) before
    /// returning.
    pub fn list_for_scope(&self, principal: &str, workspace: &str) -> Vec<RealtimeSession> {
        self.prune_stale_if_due();
        self.sessions
            .iter()
            .filter(|entry| entry.principal == principal && entry.workspace == workspace)
            .map(|entry| entry.clone())
            .collect()
    }

    /// Publish a custom media event scoped to a known session. Returns
    /// `ScopeMismatch` if the caller's scope doesn't match the
    /// session's, `EventTypeNotAllowed` if `event_type` isn't on the
    /// client-postable whitelist, `NotFound` if the session is gone.
    pub fn publish_client_event(
        &self,
        session_id: &str,
        caller_principal: &str,
        caller_workspace: &str,
        event_type: &str,
        payload: Value,
    ) -> Result<RealtimeSession, MediaRegistryError> {
        if !CLIENT_POSTABLE_EVENT_TYPES.contains(&event_type) {
            return Err(MediaRegistryError::EventTypeNotAllowed(
                event_type.to_string(),
            ));
        }
        let session = self
            .get(session_id)
            .ok_or_else(|| MediaRegistryError::NotFound(session_id.to_string()))?;
        if session.principal != caller_principal || session.workspace != caller_workspace {
            return Err(MediaRegistryError::ScopeMismatch);
        }
        // Update last_seen on every client-posted event so chatty
        // surfaces (transcript deltas, capture progress) implicitly
        // act as heartbeats.
        if let Some(mut entry) = self.sessions.get_mut(session_id) {
            entry.last_seen_at_ms = now_ms();
        }
        let merged = merge_session_envelope(&session, event_type, payload);
        self.broadcaster.emit_named(
            event_type,
            MEDIA_SYSTEM_AGENT,
            Some(&session.principal),
            Some(&session.workspace),
            merged,
        );
        Ok(session)
    }

    fn emit_session_event(
        &self,
        event_type: &str,
        session: &RealtimeSession,
        extra: Option<Value>,
    ) {
        let mut payload = json!({
            "session_id": session.session_id,
            "surface_type": session.surface_type,
            "transport": session.transport,
            "status": session.status,
            "thread_id": session.thread_id,
            "capabilities": session.capabilities,
            "permissions": session.permissions,
            "last_seen_at_ms": session.last_seen_at_ms,
            "created_at_ms": session.created_at_ms,
        });
        if let Some(extra) = extra {
            if let (Some(payload_obj), Some(extra_obj)) =
                (payload.as_object_mut(), extra.as_object())
            {
                for (k, v) in extra_obj {
                    payload_obj.insert(k.clone(), v.clone());
                }
            }
        }
        self.broadcaster.emit_named(
            event_type,
            MEDIA_SYSTEM_AGENT,
            Some(&session.principal),
            Some(&session.workspace),
            payload,
        );
    }

    fn prune_stale_if_due(&self) {
        // Capture the timestamp under the lock, then DROP the guard
        // before doing any work that could re-enter the registry or
        // emit through the broadcaster. Holding the lock across the
        // emit loop risks a latent deadlock if any future code path
        // (e.g. a broadcaster subscriber that touches the registry)
        // ends up calling back in synchronously.
        {
            let mut last = match self.last_prune.lock() {
                Ok(g) => g,
                Err(_) => return,
            };
            if last.elapsed() < SESSION_STALE_AFTER {
                return;
            }
            *last = Instant::now();
        }
        let cutoff = now_ms() - SESSION_STALE_AFTER.as_millis() as i64;
        let stale: Vec<String> = self
            .sessions
            .iter()
            .filter(|e| e.last_seen_at_ms < cutoff)
            .map(|e| e.session_id.clone())
            .collect();
        for id in stale {
            if let Some((_, mut session)) = self.sessions.remove(&id) {
                session.status = SessionStatus::Disconnected;
                session.last_seen_at_ms = now_ms();
                self.emit_session_event(MEDIA_SESSION_DISCONNECTED, &session, None);
            }
        }
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn merge_session_envelope(
    session: &RealtimeSession,
    event_type: &str,
    mut payload: Value,
) -> Value {
    if let Some(obj) = payload.as_object_mut() {
        obj.entry("session_id".to_string())
            .or_insert_with(|| Value::String(session.session_id.clone()));
        obj.entry("surface_type".to_string())
            .or_insert_with(|| serde_json::to_value(session.surface_type).unwrap_or(Value::Null));
        obj.entry("thread_id".to_string())
            .or_insert_with(|| match &session.thread_id {
                Some(t) => Value::String(t.clone()),
                None => Value::Null,
            });
        obj.entry("event_type".to_string())
            .or_insert_with(|| Value::String(event_type.to_string()));
    }
    payload
}

fn permission_diff(
    before: &MediaPermissions,
    after: &MediaPermissions,
) -> Vec<(&'static str, PermissionState, PermissionState)> {
    let mut out = Vec::new();
    macro_rules! diff {
        ($field:ident, $name:literal) => {
            if before.$field != after.$field {
                out.push(($name, before.$field, after.$field));
            }
        };
    }
    diff!(mic, "mic");
    diff!(camera, "camera");
    diff!(screen_capture, "screen_capture");
    diff!(system_audio, "system_audio");
    diff!(transcription, "transcription");
    diff!(raw_media_persistence, "raw_media_persistence");
    out
}

// ─── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {

    /// The device sends `esp_terminal` as its `source_surface`; the enum has
    /// to answer to the same spelling or the surface silently resolves to
    /// `Unknown` and reads as an unrecognised client.
    #[test]
    fn esp_terminal_surface_round_trips_as_snake_case() {
        let json = serde_json::to_string(&SurfaceType::EspTerminal).expect("serialises");
        assert_eq!(json, "\"esp_terminal\"");
        let parsed: SurfaceType = serde_json::from_str(&json).expect("deserialises");
        assert_eq!(parsed, SurfaceType::EspTerminal);
    }
    use super::*;

    fn make_registry() -> RealtimeSessionRegistry {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(64));
        RealtimeSessionRegistry::new(broadcaster)
    }

    fn make_session(id: &str, principal: &str, workspace: &str) -> RealtimeSession {
        RealtimeSession {
            session_id: id.to_string(),
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            thread_id: None,
            surface_type: SurfaceType::WebMobile,
            transport: TransportType::Sse,
            status: SessionStatus::Connected,
            capabilities: SurfaceCapabilities::default(),
            permissions: MediaPermissions::default(),
            created_at_ms: now_ms(),
            last_seen_at_ms: now_ms(),
            user_agent: None,
            display_label: None,
            audio_surface: None,
            resolved_audio_profile: None,
        }
    }

    #[test]
    fn register_then_list_returns_session_in_scope_only() {
        let reg = make_registry();
        reg.register(make_session("s1", "alice", "default"))
            .unwrap();
        reg.register(make_session("s2", "bob", "default")).unwrap();
        let list = reg.list_for_scope("alice", "default");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].session_id, "s1");
    }

    #[test]
    fn update_capabilities_round_trips() {
        let reg = make_registry();
        reg.register(make_session("s1", "alice", "default"))
            .unwrap();
        let caps = SurfaceCapabilities {
            browser_tts: true,
            mic: true,
            camera: true,
            ..SurfaceCapabilities::default()
        };
        let updated = reg.update_capabilities("s1", caps.clone()).unwrap();
        assert_eq!(updated.capabilities, caps);
    }

    #[test]
    fn permission_diff_detects_per_channel_transitions() {
        let mut before = MediaPermissions::default();
        before.mic = PermissionState::Unknown;
        before.camera = PermissionState::Granted;
        let mut after = MediaPermissions::default();
        after.mic = PermissionState::Granted;
        after.camera = PermissionState::Denied;
        let diff = permission_diff(&before, &after);
        assert_eq!(diff.len(), 2);
        assert!(diff
            .iter()
            .any(|(name, _, n)| *name == "mic" && *n == PermissionState::Granted));
        assert!(diff
            .iter()
            .any(|(name, _, n)| *name == "camera" && *n == PermissionState::Denied));
    }

    #[test]
    fn publish_client_event_rejects_unknown_event_type() {
        let reg = make_registry();
        reg.register(make_session("s1", "alice", "default"))
            .unwrap();
        let err = reg
            .publish_client_event("s1", "alice", "default", "media.totally.fake", json!({}))
            .unwrap_err();
        matches!(err, MediaRegistryError::EventTypeNotAllowed(_));
    }

    #[test]
    fn publish_client_event_enforces_scope() {
        let reg = make_registry();
        reg.register(make_session("s1", "alice", "default"))
            .unwrap();
        let err = reg
            .publish_client_event(
                "s1",
                "mallory",
                "default",
                MEDIA_TTS_STARTED,
                json!({"message_id":"m1"}),
            )
            .unwrap_err();
        matches!(err, MediaRegistryError::ScopeMismatch);
    }

    #[test]
    fn revoke_removes_session_and_marks_revoked() {
        let reg = make_registry();
        reg.register(make_session("s1", "alice", "default"))
            .unwrap();
        let closed = reg.revoke("s1", Some("manual")).unwrap();
        assert_eq!(closed.status, SessionStatus::Revoked);
        assert!(reg.get("s1").is_none());
    }
}

//! Realtime provider data types — provider-neutral.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::{config::RealtimeVoiceMode, types::LLMToolSpec};

/// Identifier for a realtime provider implementation. Extend as new
/// providers land.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeProviderKind {
    OpenAi,
    /// OpenAI GPT-Live-1 full-duplex voice (`/v1/live/sessions`). Separate
    /// from Realtime: speech is the frontend; Magician is the delegated brain.
    OpenAiLive,
    /// xAI Grok speech-to-speech (`wss://api.x.ai/v1/realtime`). Same event
    /// family as backend OpenAI Realtime, with Grok's session payload.
    Grok,
    Gemini,
    /// Magician-owned VAD -> streaming STT -> agent -> TTS cascade.
    HandsFree,
}

/// How audio flows for this provider's realtime session.
///
/// Declared by each provider implementation and surfaced on the
/// [`RealtimeSessionDescriptor`]. The orchestrator + frontend
/// branch off this once at session start; everything else is
/// uniform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeAudioTopology {
    /// Audio rides a direct peer connection between browser and
    /// provider (e.g. OpenAI Realtime via WebRTC). Backend hands
    /// the frontend an ephemeral token + URL per upstream session
    /// rotation; control + events flow through the backend's own
    /// observer side-channel into the provider.
    ///
    /// Choose this when the provider supports peer-to-peer and the
    /// added latency of a backend hop would noticeably degrade the
    /// conversational experience.
    DirectPeerToPeer,
    /// Audio + control + events all flow through the backend.
    /// Frontend speaks one bidirectional channel to magician;
    /// magician speaks the provider's native protocol upstream.
    ///
    /// Choose this when the provider is WebSocket-only (Gemini Live)
    /// or when the backend needs to inspect/transform audio for
    /// compliance / logging / debugging reasons.
    BackendProxied,
}

/// What the magicllm router returns to the orchestrator after a
/// successful upstream session bootstrap. Provider-neutral — the
/// orchestrator never needs to read the inner provider's wire
/// shape directly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RealtimeSessionDescriptor {
    pub provider: RealtimeProviderKind,
    pub model: String,
    pub topology: RealtimeAudioTopology,
    /// Assistant calls and continuous translation share the transport but not
    /// the same behavioral contract. Clients use this to label the live mode;
    /// the backend uses it to disable assistant-only addressing and tools.
    #[serde(default)]
    pub mode: RealtimeVoiceMode,

    /// Voice id the provider will render assistant audio with.
    /// Optional — some providers infer this from the model. The
    /// frontend doesn't render this directly; it's surfaced for telemetry when
    /// a profile pins a provider voice. `None` means the upstream provider/model
    /// default is in use.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,

    // ── DirectPeerToPeer fields ──────────────────────────────────
    /// HTTPS endpoint the browser posts its WebRTC SDP offer to.
    /// Provider-specific URL (for OpenAI:
    /// `https://api.openai.com/v1/realtime/calls?model=…`).
    /// `None` for non-P2P topologies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webrtc_url: Option<String>,
    /// Short-lived bearer token the browser uses to authenticate
    /// the SDP exchange directly with the provider. Lives only as
    /// long as the upstream session. `None` for non-P2P topologies.
    /// **Safe to surface to the browser** — the real API key never
    /// leaves the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_token: Option<String>,

    // ── Backend-side bookkeeping (skipped on the wire) ───────────
    /// Upstream session id returned by the provider after bootstrap
    /// (OpenAI's `session.id`, Gemini's session uuid, etc.). Cached
    /// server-side so observability can attribute upstream events
    /// to a specific upstream session. `#[serde(skip)]` because the
    /// browser doesn't need it.
    #[serde(skip)]
    pub upstream_provider_session_id: Option<String>,

    // ── Lifecycle hints (read by orchestrator) ───────────────────
    /// Best-effort hard upper bound on this upstream session's
    /// lifetime in seconds. The orchestrator schedules a proactive
    /// rotation ~60 s before this elapses. `None` when uncapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_session_duration_secs: Option<u64>,
    /// Provider-native resume handle (e.g. Gemini's
    /// `sessionResumption.handle`). When present, the orchestrator
    /// can pass it back on reconnect and the provider replays
    /// context internally — no manual replay needed. `None` for
    /// providers (OpenAI today) where the client owns replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_resume_handle: Option<String>,

    /// Model id the upstream session should use to transcribe input
    /// audio. Provider-specific (`whisper-1` etc. for OpenAI). The
    /// frontend installs this in its `session.update` so we keep
    /// transcription model selection out of TypeScript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_model: Option<String>,
    /// Vendor input-transcription model used only when a requested local STT
    /// stream is unavailable. The backend consumes this value; browser and
    /// native clients receive it for protocol transparency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcription_fallback_model: Option<String>,
    /// Server-side voice-activity-detection mode. `"server_vad"` =
    /// upstream auto-commits turns on silence; `"none"` = client
    /// owns turn-taking. The frontend may still override per-call
    /// via push-to-talk, but the default ships from here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_detection_mode: Option<String>,
    /// Approximate context-window size in tokens. Used by the
    /// orchestrator's watermark calculation and surfaced to the
    /// frontend so it can decide whether to send `token.usage`
    /// frames at all.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window_tokens: Option<u64>,
    /// When true, clients must stop microphone capture while assistant audio
    /// is playing. This is negotiated for hosts without acoustic echo
    /// cancellation; browser clients with AEC remain full duplex and can
    /// barge in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub half_duplex: Option<bool>,
}

#[derive(Debug, thiserror::Error)]
pub enum RealtimeProviderError {
    #[error("realtime provider not configured: {0}")]
    NotConfigured(String),
    #[error("realtime request rejected: {0}")]
    BadRequest(String),
    #[error("realtime upstream: {0}")]
    Upstream(String),
    #[error("realtime topology not supported by provider: {0}")]
    UnsupportedTopology(String),
}

/// Provider-neutral speech segment produced by an agent and consumed by a
/// backend-proxied TTS cascade. Hint values use stable snake-case wire names so
/// magicllm does not depend on Magician's concrete TTS enums.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RealtimeSpeechSegment {
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pace: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emphasis: Option<String>,
}

impl RealtimeSpeechSegment {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }
}

/// Backend-proxied realtime control commands sent from Magician's
/// control WebSocket actor into the provider-owned upstream session.
///
/// Direct peer-to-peer providers never see these: their frontend
/// adapter sends equivalent commands over the vendor data channel.
/// Backend-proxied providers use this lane for PTT turn boundaries,
/// tool-result replay, and backend-pushed speech injections.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RealtimeAudioControl {
    ConfigureSession {
        instructions: String,
        tools: Vec<LLMToolSpec>,
        /// Optional per-session override for user-input transcription. `None`
        /// keeps the provider/profile default. `"none"`, `"off"`, or `"local"`
        /// disables vendor transcription after Magician has successfully
        /// attached its local transcript stream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        input_transcription_model: Option<String>,
        /// Correlates a mid-session catalog update with the provider's
        /// configuration acknowledgement. Initial setup uses `None`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_id: Option<String>,
        /// When true, provider VAD/commit may finalize transcription but must
        /// not create the assistant response. The caller follows with exactly
        /// one `RespondWithTurnContext` after its bounded context stage.
        #[serde(default)]
        defer_response_until_context: bool,
    },
    ClearInput,
    CommitInputAndRespond,
    InterruptResponse,
    ToolResult {
        call_id: String,
        output: String,
    },
    /// Seed provider-native history during session setup without asking for
    /// a new assistant turn. Providers may have separate initial-history and
    /// runtime-text APIs, so keep this distinct from `InjectSystemMessage`.
    InjectInitialHistory {
        text: String,
    },
    /// Replay one already-completed historical tool exchange while rotating
    /// providers. Unlike `ToolResult`, this includes the original call and
    /// never requests a new response or executes the tool again.
    InjectToolExchange {
        call_id: String,
        tool_name: String,
        arguments: Value,
        projected_result: Value,
    },
    InjectSystemMessage {
        text: String,
        request_response: bool,
    },
    /// Create one provider response after optionally installing a replaceable
    /// response-scoped context item. Providers that advertise this behavior
    /// must remove the item when that response completes.
    RespondWithTurnContext {
        context_item_id: String,
        context: Option<String>,
    },
    /// Speak an assistant response produced by Magician's normal chat agent.
    /// Segments are already display-clean and ordered by the chat service.
    SynthesizeResponse {
        response_id: String,
        text: String,
        segments: Vec<RealtimeSpeechSegment>,
    },
    End,
}

/// Provider-originated events emitted by a backend-proxied upstream
/// session. The Magician control actor translates these into the same
/// envelopes the browser direct-P2P adapter already emits locally, so
/// orchestration, tool dispatch, and chat-ledger ingestion stay uniform.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case", tag = "type")]
pub enum RealtimeProviderEvent {
    /// The provider transport is connected and actively consuming control/audio
    /// channels. This is deliberately separate from `SessionConfigured`: local
    /// cascades have no remote session-update acknowledgement, while websocket
    /// providers must not release buffered microphone audio before the socket
    /// handshake completes.
    TransportReady,
    SessionConfigured {
        #[serde(default)]
        update_id: Option<String>,
    },
    SessionConfigurationUnsupported {
        update_id: String,
    },
    SpeechStarted,
    SpeechStopped,
    UserTranscriptFinal {
        text: String,
        item_id: String,
    },
    /// Cumulative snapshot of the in-progress user caption. Gemini Live
    /// streams `inputTranscription` before `turnComplete`; OpenAI P2P
    /// already has a browser-side equivalent.
    UserTranscriptPartial {
        text: String,
        item_id: String,
    },
    AssistantTranscriptDelta {
        response_id: String,
        text: String,
    },
    AssistantTranscriptFinal {
        response_id: String,
        text: String,
    },
    AssistantAudioStarted {
        response_id: String,
    },
    AssistantAudioDone {
        response_id: String,
        interrupted: bool,
    },
    ResponseDone {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        /// Modality-split token detail (text vs audio, cached vs uncached) when the
        /// provider reports it (OpenAI Realtime does). Drives accurate realtime cost
        /// via `crate::pricing::compute_realtime_cost`. `None` when unavailable.
        #[serde(default)]
        usage: Option<crate::types::RealtimeUsage>,
    },
    ResponseFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        terminal_state: RealtimeResponseTerminalState,
    },
    /// Whether the model is still working on the user's request after an
    /// utterance ended. Gemini 3.8 Live speaks an acknowledgement, runs a
    /// non-blocking tool, then speaks the answer: the first `ResponseDone`
    /// arrives with the interaction still `in_progress: true`, and only the
    /// answer's flips it to `false`. Providers without the signal never emit
    /// it, so the absence of this event means "no information", not idle.
    InteractionStatus {
        in_progress: bool,
    },
    NativeResumeHandleUpdated {
        handle: String,
    },
    SessionExpiring {
        time_left_secs: Option<u64>,
    },
    /// The upstream WebSocket ended unexpectedly. The control actor rotates
    /// the provider session with bounded retry while keeping the client socket
    /// and microphone alive.
    TransportClosed {
        message: String,
    },
    FunctionCall {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        response_id: Option<String>,
        call_id: String,
        name: String,
        arguments_json: String,
    },
    Error {
        message: String,
        recoverable: bool,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RealtimeResponseTerminalState {
    Failed,
    Cancelled,
    Incomplete,
}

/// Bidirectional PCM audio stream returned by
/// [`crate::realtime::RealtimeProvider::open_proxied_audio`].
///
/// Only used for `RealtimeAudioTopology::BackendProxied` providers
/// (Gemini Live etc.) — for `DirectPeerToPeer` (OpenAI Realtime
/// today) the browser opens a WebRTC peer straight to the vendor
/// and audio never crosses magician.
///
/// Frame format: raw PCM little-endian, sample rate / channels
/// negotiated out-of-band via the descriptor (provider-specific).
/// Magician doesn't transcode — it pipes frames straight through
/// to / from the upstream and the browser.
///
/// Both ends are `tokio::sync::mpsc` channels for backpressure:
/// if the consumer (frontend or upstream) is slow, the producer
/// awaits rather than dropping frames.
pub struct AudioStreamChannel {
    /// Frames flowing browser → provider. Backend takes the
    /// `Sender` (it accepts browser binary WS frames and forwards
    /// them upstream).
    pub upstream_tx: mpsc::Sender<Vec<u8>>,
    /// Frames flowing provider → browser. Backend takes the
    /// `Receiver` (it reads provider audio and forwards as WS
    /// binary frames).
    pub downstream_rx: mpsc::Receiver<Vec<u8>>,
    /// Turn/tool/control commands flowing backend → provider.
    pub control_tx: mpsc::Sender<RealtimeAudioControl>,
    /// Semantic provider events flowing provider → backend.
    pub events_rx: mpsc::Receiver<RealtimeProviderEvent>,
}

#[cfg(test)]
mod tests {
    use super::{RealtimeAudioControl, RealtimeProviderEvent, RealtimeSpeechSegment};
    use serde_json::json;

    #[test]
    fn transport_ready_has_a_stable_provider_event_wire_shape() {
        assert_eq!(
            serde_json::to_value(RealtimeProviderEvent::TransportReady).unwrap(),
            json!({"type": "transport_ready"})
        );
    }

    #[test]
    fn interaction_status_has_a_stable_provider_event_wire_shape() {
        let event = RealtimeProviderEvent::InteractionStatus { in_progress: true };
        let encoded = serde_json::to_value(&event).unwrap();
        assert_eq!(
            encoded,
            json!({"type": "interaction_status", "in_progress": true})
        );
        let decoded: RealtimeProviderEvent = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, event);
    }

    #[test]
    fn cascaded_speech_control_preserves_delivery_hints() {
        let control = RealtimeAudioControl::SynthesizeResponse {
            response_id: "response-1".to_string(),
            text: "Please act now.".to_string(),
            segments: vec![RealtimeSpeechSegment {
                text: "Please act now.".to_string(),
                emotion: Some("urgent".to_string()),
                style: Some("formal".to_string()),
                pace: Some("fast".to_string()),
                voice_mode: Some("announcement".to_string()),
                emphasis: Some("now".to_string()),
            }],
        };
        let encoded = serde_json::to_value(&control).expect("serialize control");
        let decoded: RealtimeAudioControl =
            serde_json::from_value(encoded.clone()).expect("deserialize control");

        assert_eq!(decoded, control);
        assert_eq!(encoded["segments"][0]["emotion"], "urgent");
        assert_eq!(encoded["segments"][0]["voice_mode"], "announcement");
    }

    #[test]
    fn projected_resume_tool_exchange_round_trips_as_structured_values() {
        let control = RealtimeAudioControl::InjectToolExchange {
            call_id: "call-1".to_string(),
            tool_name: "search_memory".to_string(),
            arguments: json!({ "query": "birthday" }),
            projected_result: json!({ "data": { "value": "May 8" } }),
        };

        let encoded = serde_json::to_value(&control).expect("serialize control");
        let decoded: RealtimeAudioControl =
            serde_json::from_value(encoded.clone()).expect("deserialize control");

        assert_eq!(decoded, control);
        assert_eq!(encoded["type"], "inject_tool_exchange");
        assert!(encoded["arguments"].is_object());
        assert!(encoded["projected_result"].is_object());
    }
}

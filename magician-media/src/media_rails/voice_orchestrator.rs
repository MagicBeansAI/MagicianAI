//! Voice orchestrator — owns the per-call session lifecycle behind
//! the new single-WebSocket control endpoint (R4).
//!
//! The orchestrator is the natural home for everything that *isn't*
//! provider-specific or HTTP-specific:
//!
//!   - Minting the initial upstream session (via the
//!     `magicllm::realtime::RealtimeProvider` resolved by the
//!     OperationLlmRouter for `LLMOperation::VoiceController`).
//!   - Proactive rotation (`max_session_duration_secs - 60 s`),
//!     watermark-driven rotation, and recovery-driven rotation all funnel
//!     through the same `rotate(reason)` path.
//!   - Building resume context via the [`VoiceContextCompactor`] so each
//!     rotation can replay a compressed summary instead of bloating context.
//!   - Tool-call dispatch via the existing [`VoiceToolDispatcher`] — voice
//!     keeps the same tools (the refactor doesn't touch tool semantics).
//!   - Silent chat-ledger ingestion — finalised transcripts land on the active
//!     chat session so a later switch to text mode picks up the conversation.
//!   - Lifecycle telemetry — every transition emits the `media.voice.session.*`
//!     event the obs dashboard already knows.
//!   - Subscribing to the downstream fanout so backend-pushed task-completion
//!     notifications can flow back out through the control WS.
//!
//! What the orchestrator deliberately doesn't do:
//!   - It does not own the WebSocket. That lives in the control-WS Actix actor
//!     (R4) and just delegates here.
//!   - It does not transcode audio. For `DirectPeerToPeer` topology, audio
//!     never touches magician at all; for `BackendProxied` (future Gemini), the
//!     WS actor proxies frames straight to the provider WebSocket — the
//!     orchestrator only tracks lifecycle.
//!   - It does not implement provider-specific replay shape. That's the WS
//!     actor's job — it knows what wire format the active provider speaks.

mod concurrent_voice;

use std::sync::Arc;

use chrono::Utc;
use magicllm::{
    config::RealtimeVoiceProfile,
    pricing::compute_realtime_cost,
    realtime::{
        AudioStreamChannel, RealtimeAudioTopology, RealtimeProvider, RealtimeProviderError,
        RealtimeProviderKind, RealtimeSessionDescriptor, RealtimeSpeechSegment,
    },
    types::{LLMToolCall, LLMToolSpec, RealtimeUsage},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use thiserror::Error;
use tokio::sync::{mpsc, Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::media_rails::{
    voice_context_compactor::ResumeContext,
    voice_downstream_fanout::VoiceDownstreamMessage,
    voice_tool_dispatcher::{VoiceToolDispatchResponse, VoiceToolDispatchStatus},
    VoiceContextCompactor, VoiceDownstreamFanout, VoiceSessionLifecycleStore, MEDIA_SYSTEM_AGENT,
    MEDIA_VOICE_SESSION_MINTED, MEDIA_VOICE_SESSION_RECONNECT_ATTEMPT,
    MEDIA_VOICE_SESSION_RECONNECT_FAILED, MEDIA_VOICE_SESSION_ROTATED,
};
use magician::magician_v2::{
    apps::boundary::{
        AppOwnerExecutionCredential, AppRealtimeVoiceDeliveryFence,
        AppRealtimeVoiceOwnerSessionCredential,
    },
    artifact_v2::ArtifactV2Service,
    chat::{
        models::{
            ChatChannel, ChatLlmTranscriptEntry, ChatMessage, ChatMessageContent,
            ChatMessageDirection, ChatMessageMode, StoredToolCall,
        },
        service::{
            ChatService, ExternalToolCatalogUpdate, RealtimeTurnContext, VoiceSessionContext,
        },
        storage::ChatStore,
    },
};
use magician::magician_v2::{
    query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter},
    realtime_events::{RuntimeTransportBroadcaster, RuntimeTransportEvent},
};

/// Reasons we'd ask the orchestrator to rotate the upstream session.
/// Forwarded into the rotation telemetry event so dashboards can
/// distinguish a healthy proactive rotate from a recovery cascade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RotateReason {
    /// Timer-driven, before the provider's hard cutoff.
    Proactive,
    /// Token-watermark crossing — context window is filling up.
    Watermark,
    /// Reaction to a transport drop / upstream error.
    Reconnect,
    /// Operator/diagnostic-triggered rotation (rare).
    Manual,
}

/// Result of a successful rotation. Carries everything the control
/// WS needs to bring the new upstream session online AND make the
/// model remember the conversation so far: a compacted summary plus
/// the most-recent turns to replay verbatim. For
/// `direct_peer_to_peer` topology the frontend fans these into
/// `conversation.item.create` frames on the fresh data channel.
///
/// No `Clone` / `Debug` — `AudioStreamChannel` carries a
/// `mpsc::Receiver` which is intentionally !Clone (single
/// consumer) and its `Sender`/`Receiver` halves don't implement
/// `Debug`. The WS actor receives the result by-value, swaps its
/// in/out sides, and is done with it; no logging path needs to
/// format the struct.
pub struct RotationResult {
    pub descriptor: RealtimeSessionDescriptor,
    pub resume: ResumeContext,
    pub rotation_count: u32,
    /// Fresh audio channel for `BackendProxied` providers. The
    /// rotated upstream session opens its own channel; the WS
    /// actor swaps `upstream_audio_tx` to the new sender and
    /// spawns a new downstream forwarder (the old one's source
    /// channel closes when the prior provider session tears down).
    /// `None` for `DirectPeerToPeer` rotations (audio rides
    /// WebRTC, channel concept doesn't apply).
    pub new_audio_channel: Option<AudioStreamChannel>,
}

#[derive(Debug, Error)]
pub enum OrchestratorError {
    #[error("realtime voice provider not configured: {0}")]
    NotConfigured(String),
    #[error("realtime provider error: {0}")]
    Provider(#[from] RealtimeProviderError),
    #[error("voice session not started")]
    NotStarted,
    #[error("voice session ended before the guided flow completed")]
    Cancelled,
    #[error("io: {0}")]
    Io(String),
}

/// Per-call state held between method invocations. Wrapped in a
/// `Mutex` inside the orchestrator so the WS actor can call
/// methods from multiple message handlers without explicit
/// synchronisation.
struct CallState {
    principal: String,
    workspace: String,
    voice_session_id: String,
    /// The agent this call's chat session is bound to, resolved ONCE at start
    /// from `source_surface`. Both the session binding and the tool-call
    /// authorization read it, so they cannot drift: authorizing a tool as one
    /// agent while the session belongs to another is an authorization bug, not
    /// a cosmetic mismatch.
    chat_agent_id: String,
    /// Minted once, at call start, from the **registered** media session
    /// (`voice_source_surface`). It is the room-vs-owner signal chat reads, so
    /// it must never be recomputed mid-call: `rotate()` mutates named fields in
    /// place and deliberately does not rebuild `CallState`, which is what keeps
    /// this stable across a provider rotation, reconnect and resume. A refactor
    /// that reconstructs `CallState` here must carry this field across verbatim
    /// or a rotating meeting silently becomes an owner session.
    source_surface: String,
    ui_thread_id: String,
    /// Tool calls this call has had REFUSED, by the typed refusal seam rather
    /// than by matching an error message. Emitted so an operator can see a room
    /// probing sealed capabilities; a fabricated count would have been worse
    /// than none, which is why this waited for `ExternalToolRefusal` to exist.
    tool_calls_refused: u32,
    /// Digest of the meeting block last injected into this call's per-turn
    /// context, so a room is not re-told what it was already told. `rotate()`
    /// mutates named fields and keeps `CallState`, so a rotated call stays
    /// quiet; a resumed call builds a fresh `CallState` and therefore re-sends
    /// once — correct, because the provider's context was rebuilt too.
    last_meeting_summary_digest: Option<String>,
    thread_id: Option<String>,
    profile: RealtimeVoiceProfile,
    provider: Arc<dyn RealtimeProvider>,
    hands_free: bool,
    /// Per-call realtime input boundary requested by the native surface.
    /// Stored separately from the profile so rotations preserve the override.
    turn_detection_override: Option<String>,
    rotation_count: u32,
    downstream_rx: Option<mpsc::UnboundedReceiver<VoiceDownstreamMessage>>,
    /// Highest cumulative input/output tokens reported by the
    /// frontend so we can fire a watermark rotation server-side
    /// when the threshold is crossed. Reset on every rotation.
    last_input_tokens: u64,
    last_output_tokens: u64,
    /// Running audio+text realtime token totals for this call, for USD cost.
    session_usage: RealtimeUsage,
    /// Resolved chat session id for this call. Set on first
    /// `get_or_create_active_session` call (lazily, inside
    /// `chat_session_id_for_call`) so the next rotate / compaction /
    /// transcript-ingest skips re-resolving against the store.
    chat_session_id: Option<String>,
    /// Exact effective-policy snapshot advertised when this realtime
    /// session received its tool catalog. Every external tool call must
    /// match it; a definition/policy change therefore fails closed until
    /// the session is deliberately reconfigured or restarted.
    policy_snapshot_id: Option<String>,
    /// Handle to the pending proactive-rotation tokio task.
    /// Aborted on every successful rotate (then a fresh one is
    /// scheduled with the new descriptor's expiry) and on `end()`.
    /// `None` when the active profile has no `max_session_duration_secs`
    /// — those providers rely purely on watermark + drop recovery.
    proactive_rotation_task: Option<tokio::task::JoinHandle<()>>,
    /// `BackendProxied` audio channel halves, opened immediately
    /// after `create_session` when the provider's topology is
    /// `BackendProxied`. The control-WS actor takes the channel
    /// once via `take_audio_channel()` and pipes browser ↔
    /// provider PCM frames in both directions. `None` for
    /// `DirectPeerToPeer` providers (OpenAI Realtime today).
    audio_channel: Option<AudioStreamChannel>,
    /// Descriptor of the *current* upstream session — needed for
    /// `close_session` calls on rotate (to release the prior
    /// upstream) and end (to release the live one). Replaced on
    /// every successful rotate.
    current_descriptor: RealtimeSessionDescriptor,
    /// `chat_turn_id` for the current voice turn. A voice utterance
    /// IS a real chat turn (not virtual) in voice-as-chat-agent — each
    /// finalised user transcript mints a fresh id; subsequent tool
    /// dispatches and the assistant transcript at response.done share
    /// it; the next user transcript replaces it. Stamped on every
    /// `ChatMessage` voice writes to the ledger so chat-side
    /// subscribers see voice turns indistinguishable from text turns
    /// except for `voice_origin: true`.
    current_chat_turn_id: Option<String>,
    /// Set only by the control actor's response-gated path after it advances
    /// the owner turn epoch but before the concurrent ledger ingest consumes
    /// that already-minted id.
    prebegun_user_turn_pending_ingest: bool,
    /// Call-wide cancellation surface. Child tokens are passed into
    /// chat-tool dispatch so ending/rotating a voice call can stop
    /// in-flight tool work and clean up per-turn subscriptions.
    cancel_token: CancellationToken,
    /// Vendor Live's call-wide ChatService sentinel. It is a child of the
    /// call token so `/stop` can cancel one guided turn without poisoning the
    /// still-open audio call; a later takeover may reacquire a fresh child.
    chat_run_token: Option<CancellationToken>,
    /// Persisted picker choice from `session.start`. Omitted stays the
    /// configured Pi default. Voice never invents an engine from speech.
    coding_choice: Option<magician::magician_v2::vibedev::dispatch_intent::VibeDevCodingChoice>,
    /// The calling client's composer chat choice from `session.start`. Every
    /// chat turn this call makes (`delegate_to_chat`, hands-free turns)
    /// thinks with it, so a spoken turn uses the same engine as a typed one
    /// from that client; `None` follows `chat.harness_engine`.
    chat_choice: Option<magician::magician_v2::execution::plane::ChatHarnessChoice>,
    realtime_profile_name: Option<String>,
    preferred_voice: Option<String>,
    app_owner_session_credential: Option<Arc<AppRealtimeVoiceOwnerSessionCredential>>,
    app_owner_execution_credential: Option<Arc<AppOwnerExecutionCredential>>,
    governed_history_capture_disabled: bool,
    concurrent_requests: bool,
    concurrent_context_session_id: Option<String>,
}

/// Content-free description of the boundary a live call resolved to. Everything
/// here is a server-derived label or a hash — never a thread name, a meeting
/// title, a transcript, or anything a participant said. It is emitted as
/// telemetry and shown read-only in the call UI, so it must stay safe to log
/// and safe to display to whoever is in the room.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallBoundary {
    /// Server-minted source surface string for this call.
    pub source_surface: String,
    /// The `InvocationSurface` that string resolves to.
    pub surface: &'static str,
    /// `owner` or `untrusted` — the half that actually decides what the call
    /// may reach.
    pub audience: &'static str,
    /// The agent the call is bound to. A room's is the ambassador.
    pub agent_id: String,
    /// Hash of the thread this call is bound to, so two events can be
    /// correlated without logging the thread name — which carries the
    /// meeting's title, and therefore its participants' business.
    pub binding: String,
}

impl CallState {
    fn replace_current_chat_turn(&mut self, chat_turn_id: String, owner_direct: bool) {
        if let Some(credential) = self.app_owner_execution_credential.as_ref() {
            if owner_direct {
                if credential
                    .begin_realtime_turn(&chat_turn_id, Utc::now())
                    .is_err()
                {
                    credential.invalidate_realtime_turn();
                }
            } else {
                credential.invalidate_realtime_turn();
            }
        }
        self.current_chat_turn_id = Some(chat_turn_id);
    }

    /// The ONLY mutations a rotation may make. A rotation replaces the upstream
    /// provider session; it does not replace the call. `source_surface`,
    /// `chat_agent_id`, `ui_thread_id` and the room's injected-summary digest
    /// are call identity and must survive it.
    ///
    /// Naming the mutable fields in one method is what makes that structural
    /// rather than a comment on the struct. Rebuilding `CallState` on rotation
    /// would silently turn a rotating meeting into an owner session, and every
    /// existing test would still pass — which is the exact shape of failure
    /// this boundary has already hit three times.
    ///
    /// Returns the descriptor it displaced, so the caller can close the prior
    /// upstream session.
    fn apply_rotation(
        &mut self,
        rotation_count: u32,
        descriptor: RealtimeSessionDescriptor,
    ) -> RealtimeSessionDescriptor {
        self.rotation_count = rotation_count;
        self.last_input_tokens = 0;
        self.last_output_tokens = 0;
        std::mem::replace(&mut self.current_descriptor, descriptor)
    }
}

/// Maximum time a voice tool dispatch is allowed to block the
/// realtime model's response loop before we drop the future
/// (cancelling the in-flight dispatch) and return a synthetic
/// `{"status":"queued"}` so the model says "I'll let you know" and
/// stays responsive. 5 s is short enough that the model doesn't feel
/// stuck, long enough to absorb sync tools (memory lookup, list_tasks)
/// even on a slow disk. Async-by-design tools (delegate_to_agent,
/// create_task) return well under this so they're unaffected.
const VOICE_TOOL_DISPATCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const VOICE_TOOL_CANCEL_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Tool name string voice advertises for the chat-LLM escape hatch.
/// Matched verbatim in `dispatch_tool` so the chat-LLM detour stays
/// out of the general chat-service dispatcher.
pub const VOICE_DELEGATE_TO_CHAT_TOOL: &str = "delegate_to_chat";

pub fn provider_supports_turn_context_gate(descriptor: &RealtimeSessionDescriptor) -> bool {
    matches!(descriptor.provider, RealtimeProviderKind::OpenAi)
}

pub fn profile_supports_mandatory_turn_grounding(profile: &RealtimeVoiceProfile) -> bool {
    matches!(
        profile.provider.trim(),
        "openai_realtime" | "openai_realtime_backend"
    ) || profile.allow_without_turn_grounding
}

/// Speakable result summaries stay short; structured model evidence is
/// supplied independently by the shared projector.
const VOICE_TOOL_SUMMARY_MAX_CHARS: usize = 420;

/// How early (in seconds) to fire the proactive rotation timer
/// before the provider's reported hard cutoff. 60 s gives the
/// rotation roundtrip — compaction + new upstream mint + new SDP +
/// replay — comfortable headroom before the upstream closes.
const PROACTIVE_ROTATION_LEAD_SECS: u64 = 60;
/// Minimum sleep we'll wait before firing the proactive rotation.
/// Guards against degenerate profiles that set
/// `max_session_duration_secs` below the lead-time (would otherwise
/// underflow). 15 s is short enough not to delay obvious rotations
/// in tests while keeping us out of the immediate-fire trap.
const PROACTIVE_ROTATION_MIN_DELAY_SECS: u64 = 15;

/// Canonical agent persona for the realtime voice control loop.
/// Used both for chat-session ownership (orchestrator) AND task
/// dispatch ownership (voice tool dispatcher re-exports this as
/// `DEFAULT_VOICE_DELEGATION_AGENT`). One const, one truth.
/// Override at the YAML profile level (future work) when a user
/// pins their own personal assistant id.
pub const VOICE_CHAT_AGENT_ID: &str = "personal-assistant";

/// Render the provider discriminator into the same `snake_case`
/// string serde emits on the wire (`"open_ai"`, `"gemini"`, …) so
/// observability events and the descriptor's `provider` field
/// always agree. Falls back to a sentinel on the rare serde failure
/// rather than panicking inside a hot lifecycle path.
fn provider_label_from_descriptor(descriptor: &RealtimeSessionDescriptor) -> String {
    serde_json::to_value(descriptor.provider)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| "unknown".to_string())
}

fn governed_profile_matches_descriptor_provider(
    configured_provider: &str,
    descriptor_provider: RealtimeProviderKind,
) -> bool {
    matches!(
        (configured_provider, descriptor_provider),
        ("openai_realtime_backend", RealtimeProviderKind::OpenAi)
            | ("openai_live", RealtimeProviderKind::OpenAiLive)
            | ("gemini_live", RealtimeProviderKind::Gemini)
    )
}

fn apply_turn_detection_override(
    descriptor: &mut RealtimeSessionDescriptor,
    cascaded_hands_free: bool,
    turn_detection_override: Option<&str>,
) {
    if cascaded_hands_free {
        return;
    }
    if let Some(mode) = turn_detection_override {
        descriptor.turn_detection_mode = Some(mode.to_string());
    }
}

fn apply_preferred_realtime_voice(
    descriptor: &mut RealtimeSessionDescriptor,
    provider: &str,
    preferred_voice: Option<&str>,
) {
    if let Some(voice) = preferred_voice
        .and_then(|value| magicllm::realtime::canonical_realtime_voice(provider, value))
    {
        descriptor.voice = Some(voice);
    }
}

fn serialized_speech_hint<T: Serialize>(value: Option<T>) -> Option<String> {
    value
        .and_then(|value| serde_json::to_value(value).ok())
        .and_then(|value| value.as_str().map(str::to_string))
}

fn build_voice_tool_dispatch_response(
    tool_name: String,
    call_id: String,
    raw_output: Value,
    status: VoiceToolDispatchStatus,
) -> VoiceToolDispatchResponse {
    build_voice_tool_dispatch_response_with_projection(tool_name, call_id, raw_output, status, None)
}

fn build_voice_tool_dispatch_response_with_projection(
    tool_name: String,
    call_id: String,
    raw_output: Value,
    status: VoiceToolDispatchStatus,
    projection: Option<&magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
) -> VoiceToolDispatchResponse {
    // A transport-level dispatch can succeed while the tool itself returns a
    // typed failure/cancellation. Do not advertise that result as `ok` merely
    // because the dispatcher delivered it successfully.
    let outcome_status = projection
        .map(|projection| projection.outcome.status)
        .unwrap_or_else(|| {
            let dispatch_fallback = if matches!(status, VoiceToolDispatchStatus::Error) {
                magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Failed
            } else {
                magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Succeeded
            };
            magician::magician_v2::tool_result_runtime::legacy_tool_outcome_from_platform_envelope(
                &raw_output,
                dispatch_fallback,
            )
            .status
        });
    let effective_status = if matches!(status, VoiceToolDispatchStatus::Error)
        || matches!(
            outcome_status,
            magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Failed
                | magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Denied
                | magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Cancelled
                | magician::magician_v2::tool_result_projection::ToolOutcomeStatus::TimedOut
                | magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Revoked
                | magician::magician_v2::tool_result_projection::ToolOutcomeStatus::Unknown
        ) {
        VoiceToolDispatchStatus::Error
    } else {
        status
    };
    let voice_summary = projection
        .and_then(|projection| projection.spoken.as_ref())
        .map(|spoken| spoken.text.clone())
        .unwrap_or_else(|| derive_voice_tool_summary(&tool_name, &raw_output, effective_status));
    let voice_summary =
        magician::magician_v2::secrets::injection::sanitize_text_for_provider(&voice_summary);
    let output = build_voice_tool_output(
        &tool_name,
        &raw_output,
        &voice_summary,
        effective_status,
        projection,
    );
    VoiceToolDispatchResponse {
        tool_name,
        call_id,
        voice_summary,
        output,
        status: effective_status,
        catalog_update: None,
        projected_result: projection.cloned(),
        provider_delivery_fence: None,
    }
}

fn build_voice_tool_output(
    tool_name: &str,
    raw_output: &Value,
    voice_summary: &str,
    dispatch_status: VoiceToolDispatchStatus,
    projection: Option<&magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
) -> String {
    let model_result = projection
        .map(magician::magician_v2::tool_result_projection::provider_safe_model_value)
        .unwrap_or_else(|| {
            magician::magician_v2::tool_result_runtime::balanced_projection_failure_value(
                raw_output,
                "materialization_or_projection_failed",
            )
        });
    json!({
        "status": voice_tool_status_label(raw_output, dispatch_status),
        "tool_name": tool_name,
        "voice_summary": voice_summary,
        "speak": {
            "mode": "summary_only",
            "text": voice_summary,
        },
        "result": model_result,
    })
    .to_string()
}

fn derive_voice_tool_summary(
    tool_name: &str,
    raw_output: &Value,
    dispatch_status: VoiceToolDispatchStatus,
) -> String {
    let status_label = voice_tool_status_label(raw_output, dispatch_status);
    if matches!(dispatch_status, VoiceToolDispatchStatus::Error) {
        let reason = first_text_field(raw_output, &["error", "message", "reason"])
            .unwrap_or_else(|| "the tool returned an unstructured error".to_string());
        return truncate_text(
            &format!("Tool failed: {reason}"),
            VOICE_TOOL_SUMMARY_MAX_CHARS,
        )
        .0;
    }

    if status_label == "queued" {
        let reason = first_text_field(raw_output, &["reason", "message"])
            .unwrap_or_else(|| "the tool is still running".to_string());
        return truncate_text(&format!("Queued: {reason}"), VOICE_TOOL_SUMMARY_MAX_CHARS).0;
    }

    if let Some(title) = first_text_field(raw_output, &["title"]) {
        if raw_output.get("task_id").is_some() {
            return truncate_text(
                &format!("Task created: {title}"),
                VOICE_TOOL_SUMMARY_MAX_CHARS,
            )
            .0;
        }
    }

    if let Some(summary) = first_text_field(
        raw_output,
        &[
            "voice_summary",
            "summary",
            "announcement",
            "answer",
            "message",
            "title",
            "reason",
        ],
    ) {
        return truncate_text(&summary, VOICE_TOOL_SUMMARY_MAX_CHARS).0;
    }

    truncate_text(
        &format!("{} completed.", tool_name.replace('_', " ")),
        VOICE_TOOL_SUMMARY_MAX_CHARS,
    )
    .0
}

fn voice_tool_status_label(raw_output: &Value, dispatch_status: VoiceToolDispatchStatus) -> String {
    raw_output
        .get("status")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| match dispatch_status {
            VoiceToolDispatchStatus::Ok => "ok".to_string(),
            VoiceToolDispatchStatus::Error => "error".to_string(),
        })
}

fn first_text_field(raw_output: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| raw_output.get(*key))
        .filter_map(value_to_text)
        .map(|text| text.split_whitespace().collect::<Vec<_>>().join(" "))
        .find(|text| !text.trim().is_empty())
}

fn value_to_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(_) | Value::Bool(_) => Some(value.to_string()),
        _ => None,
    }
}

/// Truncate `text` at or before `max_chars`, preferring a sentence
/// boundary (`.` / `!` / `?` followed by whitespace) over a mid-word
/// cut. Returns the truncated text and a flag indicating whether
/// truncation actually happened.
///
/// Why sentence-aware: the prior char-boundary cut produced
/// "...attached project-status.pdf; the all-hands i" mid-word, which
/// the realtime voice model would dutifully read aloud. With
/// sentence-aware cutting the worst case becomes a clean end of a
/// sentence (or an ellipsis when no boundary fits in the budget),
/// which sounds natural when spoken.
///
/// Falls back to char-boundary + ellipsis when no sentence terminator
/// appears within `max_chars`. Final length is bounded by
/// `max_chars + 1` (the +1 covers the terminal punctuation when the
/// cut lands exactly on it).
fn truncate_text(text: &str, max_chars: usize) -> (String, bool) {
    if max_chars == 0 {
        return (String::new(), !text.is_empty());
    }
    if text.chars().count() <= max_chars {
        return (text.to_string(), false);
    }
    // Scan forward up to max_chars, remember the last sentence end.
    let mut last_boundary: Option<usize> = None;
    let mut count = 0usize;
    for (idx, ch) in text.char_indices() {
        if count >= max_chars {
            break;
        }
        if matches!(ch, '.' | '!' | '?') {
            let next_byte = idx + ch.len_utf8();
            let next_is_break = next_byte >= text.len()
                || text[next_byte..]
                    .chars()
                    .next()
                    .map(|c| c.is_whitespace())
                    .unwrap_or(true);
            if next_is_break {
                last_boundary = Some(next_byte);
            }
        }
        count += 1;
    }
    if let Some(end) = last_boundary {
        return (text[..end].trim_end().to_string(), true);
    }
    // No sentence boundary inside the budget. Fall back to a clean
    // char-boundary cut with an ellipsis so the listener knows it's
    // incomplete (the voice model is told to speak this verbatim, so
    // a trailing "…" reads as "and so on" rather than as an abrupt
    // mid-word silence).
    let mut chars = text.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    (format!("{}…", truncated.trim_end()), true)
}

/// Owned by one call. Drop = teardown (the WS actor's `stopped`
/// hook calls `end()` first to make teardown deterministic).
pub struct VoiceOrchestrator {
    chat_store: Arc<dyn ChatStore>,
    chat_service: Arc<ChatService>,
    /// Kept on the orchestrator because A1c will use it for artifact/task
    /// references that the chat dispatcher doesn't already wire (e.g.,
    /// resolving task-completion announcement enrichment). Today (A1a) it
    /// is unread — A1a routes tool dispatches through `chat_service` which
    /// owns its own artifact wiring.
    #[allow(dead_code)]
    artifact_service: Arc<ArtifactV2Service>,
    operation_router: OperationLlmRouter,
    compactor: Arc<VoiceContextCompactor>,
    lifecycle_store: Arc<VoiceSessionLifecycleStore>,
    downstream_fanout: Arc<VoiceDownstreamFanout>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    hands_free_provider: Option<Arc<dyn RealtimeProvider>>,
    app_owner_session_credential: Option<Arc<AppRealtimeVoiceOwnerSessionCredential>>,
    state: Mutex<Option<CallState>>,
}

#[derive(Debug, Clone)]
pub enum CascadedVoiceTurnOutcome {
    Completed {
        response_id: String,
        text: String,
        segments: Vec<RealtimeSpeechSegment>,
    },
    Queued,
    Cancelled,
}

impl VoiceOrchestrator {
    pub fn new(
        chat_store: Arc<dyn ChatStore>,
        chat_service: Arc<ChatService>,
        artifact_service: Arc<ArtifactV2Service>,
        operation_router: OperationLlmRouter,
        compactor: Arc<VoiceContextCompactor>,
        lifecycle_store: Arc<VoiceSessionLifecycleStore>,
        downstream_fanout: Arc<VoiceDownstreamFanout>,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self {
            chat_store,
            chat_service,
            artifact_service,
            operation_router,
            compactor,
            lifecycle_store,
            downstream_fanout,
            broadcaster,
            hands_free_provider: None,
            app_owner_session_credential: None,
            state: Mutex::new(None),
        }
    }

    pub fn with_hands_free_provider(mut self, provider: Arc<dyn RealtimeProvider>) -> Self {
        self.hands_free_provider = Some(provider);
        self
    }

    pub fn with_app_owner_session_credential(
        mut self,
        credential: Option<Arc<AppRealtimeVoiceOwnerSessionCredential>>,
    ) -> Self {
        self.app_owner_session_credential = credential;
        self
    }

    /// Resolve the chat session this voice call writes turns into.
    /// First call hits the chat store and caches the id on
    /// `CallState`; subsequent calls (rotation, compaction, every
    /// transcript-ingest) return the cached value without I/O.
    /// Returns `OrchestratorError::NotStarted` when no call is
    /// active, `Io` when the chat store fails.
    async fn chat_session_id_for_call(&self) -> Result<String, OrchestratorError> {
        // Fast path: cached.
        {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            if let Some(id) = &state.chat_session_id {
                return Ok(id.clone());
            }
        }
        // Slow path: resolve + cache. We re-take the lock around the
        // write to avoid holding it across the chat-store await.
        let (principal, workspace, ui_thread_id, voice_session_id, chat_agent_id) = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            (
                state.principal.clone(),
                state.workspace.clone(),
                state.ui_thread_id.clone(),
                state.voice_session_id.clone(),
                state.chat_agent_id.clone(),
            )
        };
        let voice_channel = ChatChannel::new("voice", voice_session_id);
        let session = self
            .chat_store
            .get_or_create_active_session(
                &principal,
                &workspace,
                &ui_thread_id,
                &voice_channel,
                &chat_agent_id,
            )
            .await
            .map_err(|e| OrchestratorError::Io(e.to_string()))?;
        let id = session.id.clone();
        let voice_session_id = {
            let mut guard = self.state.lock().await;
            match guard.as_mut() {
                Some(state) => {
                    state.chat_session_id = Some(id.clone());
                    Some(state.voice_session_id.clone())
                },
                None => None,
            }
        };
        // Link this chat session to the live voice session so a completing
        // task routes its completion announcement back to this call — whether
        // user-visible (create_task) or internal (delegate_to_agent /
        // orchestrate_pipeline); they all carry this chat_session_id. No
        // per-task tagging / record writes.
        if let Some(vsid) = voice_session_id {
            self.downstream_fanout
                .link_chat_session(vsid.clone(), id.clone());
            // And the scope, for tasks that carry no chat session at all — a
            // cockpit VibeDev run is the case. Its manifest omits
            // `chat_session_id` to stay out of the task sweep, not to opt out
            // of being announced, so without this a diff waiting on a cockpit
            // run is unannounceable to a caller who is by definition not
            // looking at the cockpit.
            self.downstream_fanout
                .link_scope(vsid, principal.clone(), workspace.clone());
        }
        Ok(id)
    }

    /// Build the initial replay payload for a newly minted realtime
    /// session. Rotation already replays compacted context; initial
    /// start needs the same behavior so a voice call opened mid-thread
    /// can answer references to the prior text conversation.
    pub async fn initial_resume_context(&self) -> Result<ResumeContext, OrchestratorError> {
        let (principal, workspace, voice_session_id, history_capture_allowed) = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            (
                state.principal.clone(),
                state.workspace.clone(),
                state.voice_session_id.clone(),
                !state.governed_history_capture_disabled,
            )
        };
        if !history_capture_allowed {
            return Ok(ResumeContext {
                summary: None,
                recent_turns: Vec::new(),
                tool_exchanges: Vec::new(),
                total_turns: 0,
            });
        }
        let chat_session_id = self.chat_session_id_for_call().await?;
        Ok(self
            .compact_resume_context_or_empty(
                &voice_session_id,
                &chat_session_id,
                &principal,
                &workspace,
                "initial",
            )
            .await)
    }

    async fn compact_resume_context_or_empty(
        &self,
        voice_session_id: &str,
        chat_session_id: &str,
        principal: &str,
        workspace: &str,
        phase: &str,
    ) -> ResumeContext {
        match self
            .compactor
            .compact_for_resume(voice_session_id, chat_session_id, principal, workspace)
            .await
        {
            Ok(ctx) => ctx,
            Err(err) => {
                warn!(
                    voice_session_id = %voice_session_id,
                    chat_session_id = %chat_session_id,
                    phase = %phase,
                    error = %err,
                    "[VOICE-ORCHESTRATOR] resume compaction failed; continuing with empty resume"
                );
                ResumeContext {
                    summary: None,
                    recent_turns: Vec::new(),
                    tool_exchanges: Vec::new(),
                    total_turns: 0,
                }
            },
        }
    }

    /// Start a new call. Resolves the realtime provider via the
    /// router (so the YAML config decides which model/voice the
    /// upstream session opens with), mints the upstream session,
    /// records lifecycle state, and returns the initial descriptor
    /// the caller can forward to the frontend.
    pub async fn start(
        self: &Arc<Self>,
        principal: String,
        workspace: String,
        voice_session_id: String,
        source_surface: String,
        ui_thread_id: String,
        thread_id: Option<String>,
        realtime_profile_name: Option<String>,
        hands_free: bool,
        concurrent_requests: bool,
        half_duplex: bool,
        turn_detection_override: Option<String>,
        coding_choice: Option<magician::magician_v2::vibedev::dispatch_intent::VibeDevCodingChoice>,
        chat_choice: Option<magician::magician_v2::execution::plane::ChatHarnessChoice>,
        realtime_voices: std::collections::BTreeMap<String, String>,
    ) -> Result<RealtimeSessionDescriptor, OrchestratorError> {
        // Decide before claiming a chat slot: enabling this after start would
        // already have cancelled a typed turn and blocked its queue for the call.
        let concurrent_requests =
            Self::allows_concurrent_voice(&source_surface, concurrent_requests);
        let mut resolved_realtime_profile_name = None;
        let (mut profile, mut provider) = if hands_free {
            let provider = self.hands_free_provider.clone().ok_or_else(|| {
                OrchestratorError::NotConfigured(
                    "hands-free voice provider is not configured".to_string(),
                )
            })?;
            (
                RealtimeVoiceProfile {
                    provider: "magician_hands_free".to_string(),
                    model: provider.default_model().to_string(),
                    display_name: None,
                    selectable: false,
                    mode: magicllm::config::RealtimeVoiceMode::Assistant,
                    allow_without_turn_grounding: false,
                    voice: None,
                    max_session_duration_secs: None,
                    compaction_token_watermark: Some(1.0),
                    base_url: None,
                    fallback: Vec::new(),
                    transcription_model: None,
                    transcription_fallback_model: None,
                    turn_detection_mode: Some("server_vad".to_string()),
                    context_window_tokens: None,
                    verbatim_recent_turns: None,
                    compaction_input_turn_limit: None,
                    translation_target_language: None,
                    translation_echo_target_language: false,
                    thinking_level: None,
                    tool_result_scheduling: None,
                    display_order: None,
                },
                provider,
            )
        } else if let Some(profile_name) = realtime_profile_name
            .as_deref()
            .filter(|v| !v.trim().is_empty())
        {
            resolved_realtime_profile_name = Some(profile_name.to_string());
            let profile = self
                .operation_router
                .realtime_voice_profile_by_name(profile_name)
                .ok_or_else(|| {
                    OrchestratorError::NotConfigured(format!(
                        "realtime_voice profile `{profile_name}` was not found"
                    ))
                })?;
            let provider = self
                .operation_router
                .resolve_realtime_provider_profile(profile_name)
                .ok_or_else(|| {
                    OrchestratorError::NotConfigured(format!(
                        "realtime_voice profile `{profile_name}` resolved no provider"
                    ))
                })??;
            (profile, provider)
        } else {
            resolved_realtime_profile_name =
                self.operation_router.realtime_voice_default_profile_name();
            let profile = self
                .operation_router
                .realtime_voice_profile(&LLMOperation::VoiceController)
                .ok_or_else(|| {
                    OrchestratorError::NotConfigured(
                        "no realtime_voice profile mapped for operation `voice_controller` — \
                         check magician-config.yaml > realtime_voice.operation_mapping"
                            .to_string(),
                    )
                })?;
            let provider = self
                .operation_router
                .resolve_realtime_provider(&LLMOperation::VoiceController)
                .ok_or_else(|| {
                    OrchestratorError::NotConfigured(
                        "realtime_voice operation mapping resolved no provider".to_string(),
                    )
                })??;
            (profile, provider)
        };

        // Owner-session credentials are minted for every identified owner
        // surface (web, mobile, tray). The processing-trust catalog can only
        // attest backend-proxied profiles that are declared there, and the
        // later drop-and-continue path is what keeps ordinary personal voice
        // (browser DirectPeerToPeer, native backend-proxied without a catalog
        // declaration) usable. Fail closed only when this profile can actually
        // be governed.
        let can_attest_selected_profile = !hands_free
            && resolved_realtime_profile_name
                .as_deref()
                .is_some_and(|profile_name| {
                    self.chat_service
                        .attest_realtime_voice_profile(profile_name)
                });
        let govern_owner_voice =
            self.app_owner_session_credential.is_some() && can_attest_selected_profile;
        if self.app_owner_session_credential.is_some() && !can_attest_selected_profile {
            warn!(
                profile = %resolved_realtime_profile_name.as_deref().unwrap_or(""),
                provider = %profile.provider,
                hands_free,
                "[VOICE-ORCHESTRATOR] owner credential is present but the selected realtime profile is not currently attested; continuing as ordinary personal voice"
            );
        }

        // A native realtime provider is admitted only when Magician can hold
        // response creation until the finalized transcript has received the
        // same current-turn memory/procedure retrieval as Chat. Providers such
        // as Gemini Live that do not expose that response gate must use an
        // explicitly configured grounded fallback; silently opening an
        // ungrounded session would violate cross-surface quality parity.
        if !hands_free && !profile_supports_mandatory_turn_grounding(&profile) {
            if govern_owner_voice {
                return Err(OrchestratorError::NotConfigured(
                    "authenticated governed voice cannot switch to a fallback profile".to_string(),
                ));
            }
            let mut grounded_fallback = None;
            for fallback_name in &profile.fallback {
                let Some(candidate) = self
                    .operation_router
                    .realtime_voice_profile_by_name(fallback_name)
                else {
                    continue;
                };
                if !profile_supports_mandatory_turn_grounding(&candidate) {
                    continue;
                }
                let Some(resolved) = self
                    .operation_router
                    .resolve_realtime_provider_profile(fallback_name)
                else {
                    continue;
                };
                grounded_fallback = Some((candidate, resolved?));
                break;
            }
            let Some((fallback_profile, fallback_provider)) = grounded_fallback else {
                return Err(OrchestratorError::NotConfigured(format!(
                    "realtime profile provider `{}` cannot guarantee finalized-turn semantic \
                     grounding and has no grounded OpenAI realtime fallback",
                    profile.provider
                )));
            };
            warn!(
                requested_provider = %profile.provider,
                fallback_provider = %fallback_profile.provider,
                fallback_model = %fallback_profile.model,
                "[VOICE-ORCHESTRATOR] selected grounded realtime fallback"
            );
            profile = fallback_profile;
            provider = fallback_provider;
            resolved_realtime_profile_name = None;
        }

        if govern_owner_voice && !profile.fallback.is_empty() {
            return Err(OrchestratorError::NotConfigured(
                "authenticated governed voice requires an exact no-fallback realtime profile"
                    .to_string(),
            ));
        }

        let preferred_voice = resolved_realtime_profile_name
            .as_ref()
            .and_then(|profile_id| realtime_voices.get(profile_id))
            .cloned();
        let mut descriptor = provider
            .create_session(
                &principal,
                &workspace,
                &voice_session_id,
                thread_id.as_deref(),
                preferred_voice.as_deref(),
            )
            .await?;
        apply_preferred_realtime_voice(
            &mut descriptor,
            &profile.provider,
            preferred_voice.as_deref(),
        );
        if hands_free {
            descriptor.half_duplex = Some(half_duplex);
        }
        // `open_proxied_audio` consumes the descriptor's value, so apply the
        // native per-call boundary before opening the provider channel.
        apply_turn_detection_override(
            &mut descriptor,
            hands_free,
            turn_detection_override.as_deref(),
        );

        if govern_owner_voice {
            let credential = self.app_owner_session_credential.as_ref().ok_or_else(|| {
                OrchestratorError::NotConfigured(
                    "authenticated governed voice lost its owner credential".to_string(),
                )
            })?;
            let profile_name = resolved_realtime_profile_name.as_deref().ok_or_else(|| {
                OrchestratorError::NotConfigured(
                    "authenticated governed voice lost its selected realtime profile".to_string(),
                )
            })?;
            let topology = match descriptor.topology {
                RealtimeAudioTopology::BackendProxied => "backend_proxied",
                RealtimeAudioTopology::DirectPeerToPeer => "direct_peer_to_peer",
            };
            let effective_base_url =
                magician::magician_v2::apps::processing_boundary::effective_realtime_voice_base_url(
                    profile.provider.as_str(),
                    profile.base_url.as_deref(),
                )
                .ok_or_else(|| {
                    OrchestratorError::NotConfigured(
                        "authenticated governed voice provider endpoint is not exact".to_string(),
                    )
                })?;
            if !governed_profile_matches_descriptor_provider(&profile.provider, descriptor.provider)
                || !self.chat_service.attest_realtime_voice_route(
                    profile_name,
                    &profile.provider,
                    &descriptor.model,
                    Some(effective_base_url),
                    topology,
                )
            {
                credential.invalidate();
                let _ = provider.close_session(&descriptor).await;
                return Err(OrchestratorError::NotConfigured(
                    "authenticated governed voice provider route changed during session mint"
                        .to_string(),
                ));
            }
        }

        let now_ms = Utc::now().timestamp_millis();
        let provider_label = provider_label_from_descriptor(&descriptor);
        self.lifecycle_store.record_minted(
            &voice_session_id,
            &provider_label,
            descriptor.upstream_provider_session_id.clone(),
            descriptor.max_session_duration_secs,
            now_ms,
        );
        self.broadcaster.emit_named(
            MEDIA_VOICE_SESSION_MINTED,
            MEDIA_SYSTEM_AGENT,
            Some(&principal),
            Some(&workspace),
            serde_json::json!({
                "voice_session_id": voice_session_id,
                "provider": provider_label,
                "model": descriptor.model,
                "max_session_duration_secs": descriptor.max_session_duration_secs,
                "thread_id": thread_id,
            }),
        );

        // Hook into the bridge registry so backend-pushed
        // task-completion notifications can flow back out through
        // the orchestrator (and into the control WS in R4). The
        // receiver is stored on the call state; the WS actor
        // consumes it as a stream.
        let (downstream_tx, downstream_rx) = mpsc::unbounded_channel();
        self.downstream_fanout
            .register(voice_session_id.clone(), downstream_tx);

        let max_session_duration_secs = descriptor.max_session_duration_secs;

        // For `BackendProxied` providers, open the upstream audio
        // channel immediately so the control-WS actor can take it
        // when the call attaches. Errors surface as
        // `OrchestratorError::Provider` — same shape as create_session
        // failures, so callers don't branch on topology.
        let audio_channel = if matches!(descriptor.topology, RealtimeAudioTopology::BackendProxied)
        {
            Some(provider.open_proxied_audio(&descriptor).await?)
        } else {
            None
        };

        // A browser-direct provider cannot receive server-only final-delivery
        // fences, and a backend-proxied profile that is not in the processing
        // trust catalog cannot be governed either. Keep the call usable, but
        // drop owner app authority so catalog/retrieval stays on the ordinary
        // personal-voice surface.
        let app_owner_session_credential =
            if matches!(descriptor.topology, RealtimeAudioTopology::BackendProxied)
                && !hands_free
                && resolved_realtime_profile_name.is_some()
                && can_attest_selected_profile
            {
                self.app_owner_session_credential.clone()
            } else {
                if let Some(credential) = self.app_owner_session_credential.as_ref() {
                    credential.invalidate();
                }
                None
            };

        let call_cancel_token = CancellationToken::new();
        // A room binds the configured ambassador, never the personal assistant.
        // If no ambassador is configured the room binds nothing usable and the
        // surface gate refuses the turn — fail closed rather than seat the
        // owner's agent in front of a room.
        let chat_agent_id =
            if source_surface == magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE {
                self.chat_service
                    .room_agent_id()
                    .unwrap_or(VOICE_CHAT_AGENT_ID)
                    .to_string()
            } else {
                VOICE_CHAT_AGENT_ID.to_string()
            };
        let mut state = self.state.lock().await;
        *state = Some(CallState {
            principal,
            workspace,
            voice_session_id: voice_session_id.clone(),
            chat_agent_id,
            source_surface,
            ui_thread_id,
            tool_calls_refused: 0,
            last_meeting_summary_digest: None,
            thread_id,
            profile,
            provider,
            hands_free,
            turn_detection_override,
            rotation_count: 0,
            downstream_rx: Some(downstream_rx),
            last_input_tokens: 0,
            last_output_tokens: 0,
            session_usage: RealtimeUsage::default(),
            chat_session_id: None,
            policy_snapshot_id: None,
            proactive_rotation_task: None,
            audio_channel,
            current_descriptor: descriptor.clone(),
            current_chat_turn_id: None,
            prebegun_user_turn_pending_ingest: false,
            cancel_token: call_cancel_token.clone(),
            chat_run_token: None,
            coding_choice,
            chat_choice,
            realtime_profile_name: resolved_realtime_profile_name,
            preferred_voice,
            app_owner_session_credential,
            app_owner_execution_credential: None,
            governed_history_capture_disabled: false,
            concurrent_requests,
            concurrent_context_session_id: None,
        });
        drop(state);

        // Bind owner authority for all vendor calls. Legacy calls also reserve
        // the chat lane for their lifetime. Concurrent calls execute delegated
        // work in branches and must leave the parent's typed lane available.
        if !hands_free {
            match self.chat_session_id_for_call().await {
                Ok(chat_session_id) => {
                    let bound_app_owner = {
                        let guard = self.state.lock().await;
                        guard.as_ref().and_then(|state| {
                            let owner = state.app_owner_session_credential.as_ref()?;
                            let profile_name = state.realtime_profile_name.as_deref()?;
                            let provider = state.profile.provider.as_str();
                            let model = state.current_descriptor.model.as_str();
                            let topology = match state.current_descriptor.topology {
                                RealtimeAudioTopology::BackendProxied => "backend_proxied",
                                RealtimeAudioTopology::DirectPeerToPeer => "direct_peer_to_peer",
                            };
                            let effective_base_url = magician::magician_v2::apps::processing_boundary::effective_realtime_voice_base_url(
                                state.profile.provider.as_str(),
                                state.profile.base_url.as_deref(),
                            )?;
                            owner
                                .bind_physical_profile(
                                    chat_session_id.clone(),
                                    profile_name,
                                    provider,
                                    model,
                                    Some(effective_base_url.to_owned()),
                                    topology,
                                    "no_provider_storage",
                                    self.chat_service.clone(),
                                    Utc::now(),
                                )
                                .ok()
                                .map(Arc::new)
                        })
                    };
                    if let Some(bound_app_owner) = bound_app_owner.filter(|credential| {
                        self.chat_service
                            .revalidate_realtime_voice_owner_credential(credential.as_ref())
                    }) {
                        let mut state_guard = self.state.lock().await;
                        if let Some(state) = state_guard.as_mut() {
                            state.app_owner_execution_credential = Some(bound_app_owner);
                            state.governed_history_capture_disabled = true;
                        }
                    }
                    if !concurrent_requests {
                        let chat_run_token = call_cancel_token.child_token();
                        self.chat_service
                            .register_owned_active_chat_run_with_token_admitted(
                                &chat_session_id,
                                chat_run_token.clone(),
                                &voice_session_id,
                            )
                            .await;
                        let mut state_guard = self.state.lock().await;
                        if let Some(state) = state_guard.as_mut() {
                            state.chat_run_token = Some(chat_run_token);
                        }
                        debug!(
                            chat_session_id = %chat_session_id,
                            "[VOICE-ORCHESTRATOR] acquired legacy call chat lane"
                        );
                    }
                },
                Err(err) => {
                    warn!(
                        error = %err,
                        "[VOICE-ORCHESTRATOR] active_chat_run lock acquisition skipped; \
                         chat_session_id not resolvable. Call will proceed without single-modality guard."
                    );
                },
            }
        }

        self.schedule_proactive_rotation(max_session_duration_secs)
            .await;

        Ok(descriptor)
    }

    /// Run a local hands-free utterance through the same agent path as typed
    /// chat. This preserves tools, tasks, approvals, durable messages, and
    /// speech-segment parsing without duplicating any agent logic in media
    /// code.
    pub async fn process_cascaded_voice_turn(
        &self,
        text: &str,
    ) -> Result<CascadedVoiceTurnOutcome, OrchestratorError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(CascadedVoiceTurnOutcome::Cancelled);
        }
        let chat_session_id = self.chat_session_id_for_call().await?;
        let (chat_turn_id, voice_session_id, _source_surface, hands_free, coding_choice, chat_choice) = {
            let mut guard = self.state.lock().await;
            let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
            let turn_id = format!("voice-turn-{}", uuid::Uuid::new_v4());
            state.replace_current_chat_turn(turn_id.clone(), false);
            (
                turn_id,
                state.voice_session_id.clone(),
                state.source_surface.clone(),
                state.hands_free,
                state.coding_choice.clone(),
                state.chat_choice.clone(),
            )
        };
        let profile_override = chat_choice
            .as_ref()
            .map(magician::magician_v2::execution::plane::encode_chat_harness_choice);
        if !hands_free {
            return Err(OrchestratorError::NotConfigured(
                "cascaded voice turn requested for a vendor realtime session".to_string(),
            ));
        }
        let response = self
            .chat_service
            .process_message_with_mode_on_execution_runtime(
                &chat_session_id,
                Some(trimmed),
                &[],
                profile_override.as_deref(),
                ChatMessageMode::Ask,
                None,
                None,
                Some(&chat_turn_id),
                true,
                Some("authenticated_realtime_voice"),
                Some(&voice_session_id),
                None,
                coding_choice,
            )
            .await
            .map_err(|error| {
                OrchestratorError::Io(format!("hands-free chat turn failed: {error:#}"))
            })?;
        if response.cancelled {
            return Ok(CascadedVoiceTurnOutcome::Cancelled);
        }
        if response.queued.is_some() {
            return Ok(CascadedVoiceTurnOutcome::Queued);
        }
        let Some(message) = response.assistant_message else {
            return Err(OrchestratorError::Io(
                "hands-free chat turn returned no assistant message".to_string(),
            ));
        };
        let text = message
            .content
            .text_content()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .ok_or_else(|| {
                OrchestratorError::Io(
                    "hands-free assistant message contained no speakable text".to_string(),
                )
            })?
            .to_string();
        let segments = message
            .speech_segments
            .unwrap_or_default()
            .into_iter()
            .filter_map(|segment| {
                let text = segment.text.trim().to_string();
                (!text.is_empty()).then(|| RealtimeSpeechSegment {
                    text,
                    emotion: serialized_speech_hint(segment.emotion),
                    style: serialized_speech_hint(segment.style),
                    pace: serialized_speech_hint(segment.pace),
                    voice_mode: serialized_speech_hint(segment.voice_mode),
                    emphasis: segment.emphasis,
                })
            })
            .collect::<Vec<_>>();
        let spoken_text = if segments.is_empty() {
            text
        } else {
            segments
                .iter()
                .map(|segment| segment.text.as_str())
                .collect::<Vec<_>>()
                .join(" ")
        };
        Ok(CascadedVoiceTurnOutcome::Completed {
            response_id: format!("hands-free-response-{}", uuid::Uuid::new_v4()),
            text: spoken_text,
            segments,
        })
    }

    pub async fn cancel_cascaded_voice_turn(&self) {
        if let Ok(chat_session_id) = self.chat_session_id_for_call().await {
            let _ = self.chat_service.cancel_chat_run(&chat_session_id).await;
        }
    }

    /// Take the `BackendProxied` audio channel. Must be called
    /// exactly once per `start()`, by the control-WS actor, after
    /// `take_downstream_receiver`. Returns `None` for
    /// `DirectPeerToPeer` providers (no channel opened) and after
    /// the first take. The caller pipes browser ↔ provider PCM
    /// through both halves.
    pub async fn take_audio_channel(&self) -> Option<AudioStreamChannel> {
        let mut guard = self.state.lock().await;
        guard.as_mut().and_then(|s| s.audio_channel.take())
    }

    async fn tool_cancel_token(&self) -> Result<CancellationToken, OrchestratorError> {
        let guard = self.state.lock().await;
        let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
        Ok(state.cancel_token.child_token())
    }

    /// Spawn (or replace) the proactive-rotation tokio task.
    /// Fires `rotate(Proactive)` at
    /// `max_session_duration_secs - PROACTIVE_ROTATION_LEAD_SECS`,
    /// guaranteeing the orchestrator never hits the provider's hard
    /// cutoff on a quiet call (where watermark-driven rotation never
    /// kicks in). Re-called on every successful rotate with the new
    /// descriptor's duration. No-ops when the profile doesn't report
    /// a cap.
    async fn schedule_proactive_rotation(self: &Arc<Self>, max_session_duration_secs: Option<u64>) {
        let Some(duration_secs) = max_session_duration_secs else {
            return;
        };
        if duration_secs == 0 {
            return;
        }
        let delay_secs = duration_secs
            .saturating_sub(PROACTIVE_ROTATION_LEAD_SECS)
            .max(PROACTIVE_ROTATION_MIN_DELAY_SECS);
        let orchestrator = Arc::clone(self);
        // `actix::spawn` (not tokio::spawn) because the orchestrator
        // is always called from inside an Actix-managed runtime — the
        // control WS actor's spawned future. Actix's local-set
        // executor doesn't require the spawned future to be `Send`,
        // which matters here because `rotate()` indirectly awaits
        // an `OperationLlmRouter` path that holds a `std::sync::RwLock`
        // read guard across the LLM call. Fixing that to use
        // `tokio::sync::RwLock` is the right long-term move; for now
        // we use the right spawner for the runtime we're in.
        let handle = actix::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;
            // Skip if the call ended while we were sleeping. `rotate`
            // also guards against this internally (returns
            // `NotStarted`) but checking here avoids a noisy log.
            {
                let guard = orchestrator.state.lock().await;
                if guard.is_none() {
                    return;
                }
            }
            if let Err(err) = orchestrator.rotate(RotateReason::Proactive).await {
                warn!(
                    error = %err,
                    "[VOICE-ORCHESTRATOR] proactive rotation failed"
                );
            }
        });
        let mut guard = self.state.lock().await;
        if let Some(state) = guard.as_mut() {
            if let Some(prev) = state.proactive_rotation_task.replace(handle) {
                prev.abort();
            }
        }
    }

    /// Take ownership of the bridge-registry receiver so the WS
    /// actor can plug it into an Actix `StreamHandler`. Must be
    /// called exactly once per `start()`. After this, downstream
    /// `VoiceDownstreamMessage`s reach the actor directly — the
    /// orchestrator no longer sees them.
    pub async fn take_downstream_receiver(
        &self,
    ) -> Option<mpsc::UnboundedReceiver<VoiceDownstreamMessage>> {
        let mut guard = self.state.lock().await;
        guard.as_mut().and_then(|s| s.downstream_rx.take())
    }

    /// Record cumulative token usage from a provider `response.done`
    /// event. Returns `true` when the watermark fraction has been
    /// crossed and the caller should schedule a rotation. The
    /// orchestrator otherwise doesn't peek inside provider events —
    /// the WS actor decides which provider events surface here.
    pub async fn observe_token_usage(
        &self,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        usage: Option<RealtimeUsage>,
        ttfa_ms: Option<u64>,
        response_ms: Option<u64>,
        context_window_tokens: u64,
        correlation: Option<magician::magician_v2::realtime_events::LlmEventCorrelation>,
        started_at_ms: Option<i64>,
    ) -> bool {
        let broadcaster = self.broadcaster.clone();
        let mut guard = self.state.lock().await;
        let Some(state) = guard.as_mut() else {
            return false;
        };
        if let Some(v) = input_tokens {
            state.last_input_tokens = v;
        }
        if let Some(v) = output_tokens {
            state.last_output_tokens = v;
        }
        // Realtime cost + latency accounting: bill this turn's audio+text tokens at
        // the active profile model's realtime rates, log the running call total
        // (`session_usage` accumulates across rotations), and report turn latency
        // (time-to-first-audio + full response) so the mini-vs-flagship speed/cost
        // tradeoff is visible. Coarse totals from browser-direct WebRTC are
        // still a real billable response even if a future provider omits the
        // modality breakdown: preserve those totals and mark cost unknown
        // instead of silently dropping the whole call or inventing a split.
        if usage.is_some()
            || input_tokens.is_some()
            || output_tokens.is_some()
            || response_ms.is_some()
        {
            if let Some(usage) = usage.as_ref() {
                state.session_usage.add(usage);
            }
            let model = state.profile.model.clone();
            // NOT `f64::NAN`. NaN was used to mean "cost unknown", but it is
            // written straight into the `llm_calls` ledger, survives into
            // Parquet, and poisons every aggregate that touches it: `SUM`
            // returns NaN, `COALESCE(SUM(...), 0)` does NOT catch it (NaN is
            // not NULL), and it serialises to JSON `null`, so the `/llm` page
            // silently showed a missing total. Two such rows were enough to
            // make the Spend section and the Today-vs-yesterday band disagree,
            // because `HAVING SUM(cost_usd) > 0` is false for NaN and dropped
            // the whole operation group while the hourly band kept most of it.
            //
            // The row already carries `usage_reported`, which is the honest
            // signal for "we did not get usage" — so the cost column can be a
            // plain 0.0 without losing that distinction.
            let turn_cost = usage
                .as_ref()
                .map(|usage| compute_realtime_cost(&model, usage))
                .unwrap_or(0.0);
            let session_cost = usage
                .as_ref()
                .map(|_| compute_realtime_cost(&model, &state.session_usage))
                .unwrap_or(0.0);
            let exact_input_tokens = usage
                .as_ref()
                .map(|usage| {
                    usage
                        .text_input_tokens
                        .saturating_add(usage.text_cached_input_tokens)
                        .saturating_add(usage.audio_input_tokens)
                        .saturating_add(usage.audio_cached_input_tokens)
                })
                .or(input_tokens);
            let exact_output_tokens = usage
                .as_ref()
                .map(|usage| {
                    usage
                        .text_output_tokens
                        .saturating_add(usage.audio_output_tokens)
                })
                .or(output_tokens);
            let usage_reported =
                usage.is_some() || input_tokens.is_some() || output_tokens.is_some();
            let usage_for_log = usage.unwrap_or_default();
            tracing::info!(
                target: "voice_cost",
                model = %model,
                ttfa_ms = ttfa_ms.unwrap_or(0),
                response_ms = response_ms.unwrap_or(0),
                audio_in = usage_for_log.audio_input_tokens,
                audio_cached = usage_for_log.audio_cached_input_tokens,
                audio_out = usage_for_log.audio_output_tokens,
                text_in = usage_for_log.text_input_tokens,
                text_out = usage_for_log.text_output_tokens,
                turn_cost_usd = turn_cost,
                session_cost_usd = session_cost,
                "realtime voice response cost + latency"
            );
            // Route this realtime turn into the `llm_calls` analytics ledger (the
            // same parquet sink chat uses) so voice spend + latency show up
            // alongside chat instead of only in the `voice_cost` trace. Audio+text
            // token buckets are folded into input/output/cache — the modality split
            // stays in the trace above; `cost` is the accurate realtime price
            // (compute_realtime_cost), and capability="voice.realtime" tags the row.
            let now_ms = chrono::Utc::now().timestamp_millis();
            // Normal voice paths mint this identity at response start, before
            // provider dispatch/commit. Retain a compatibility fallback for an
            // older browser that reports terminal usage without first sending
            // `response.started`; the fallback is observable through its absent
            // turn-start timing rather than silently dropping the usage row.
            let mut correlation = correlation.unwrap_or_else(|| {
                magician::magician_v2::realtime_events::LlmEventCorrelation::direct(
                    state.principal.clone(),
                    state.workspace.clone(),
                    magicllm::LlmWorkloadClass::ForegroundChat,
                )
            });
            correlation
                .chat_session_id
                .clone_from(&state.chat_session_id);
            correlation
                .chat_turn_id
                .clone_from(&state.current_chat_turn_id);
            let provider = match state.current_descriptor.provider {
                magicllm::realtime::RealtimeProviderKind::OpenAi => "openai",
                magicllm::realtime::RealtimeProviderKind::OpenAiLive => "openai_live",
                magicllm::realtime::RealtimeProviderKind::Grok => "grok",
                magicllm::realtime::RealtimeProviderKind::Gemini => "gemini",
                magicllm::realtime::RealtimeProviderKind::HandsFree => "magician_hands_free",
            };
            let to_u32 = |value: u64| u32::try_from(value).unwrap_or(u32::MAX);
            broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
                execution_id: format!("voice:{}", state.voice_session_id),
                principal: Some(state.principal.clone()),
                workspace: Some(state.workspace.clone()),
                correlation: Some(correlation),
                plan_id: String::new(),
                step_id: None,
                step_index: None,
                capability: "voice.realtime".to_string(),
                success: true,
                decision_summary: String::new(),
                cost: turn_cost,
                latency_ms: response_ms.unwrap_or(0),
                error: None,
                provider: provider.to_string(),
                model,
                usage_reported,
                input_tokens: to_u32(exact_input_tokens.unwrap_or_default()),
                output_tokens: to_u32(exact_output_tokens.unwrap_or_default()),
                reasoning_tokens: 0,
                reasoning_summary: None,
                cache_read_tokens: to_u32(
                    usage_for_log
                        .audio_cached_input_tokens
                        .saturating_add(usage_for_log.text_cached_input_tokens),
                ),
                cache_creation_tokens: 0,
                // Audio-modality split so analytics can separate audio vs text and
                // the repricer can rebuild the RealtimeUsage. Text portion is the
                // (folded total − audio) of each bucket.
                audio_input_tokens: usage.as_ref().map(|value| to_u32(value.audio_input_tokens)),
                audio_output_tokens: usage
                    .as_ref()
                    .map(|value| to_u32(value.audio_output_tokens)),
                audio_cached_tokens: usage
                    .as_ref()
                    .map(|value| to_u32(value.audio_cached_input_tokens)),
                search_calls: 0,
                ttft_ms: ttfa_ms,
                task_id: None,
                agent_id: None,
                delegated_agent_id: None,
                chat_session_id: state.chat_session_id.clone(),
                operation: "voice_controller".to_string(),
                profile: None,
                attempt: 1,
                response_kind: "voice".to_string(),
                started_at_ms: started_at_ms
                    .filter(|value| *value > 0 && *value <= now_ms)
                    .unwrap_or(0),
                timestamp: now_ms,
            });
        }
        let watermark_fraction = state
            .profile
            .compaction_token_watermark
            .unwrap_or_else(|| state.provider.compaction_token_watermark());
        let threshold = (context_window_tokens as f32 * watermark_fraction).round() as u64;
        (state.last_input_tokens + state.last_output_tokens) >= threshold
    }

    /// Close an already-started realtime response that terminated with a
    /// provider error. The caller only invokes this with the correlation minted
    /// at the response boundary, so session/configuration errors outside an LLM
    /// response cannot inflate failed-call counts. Provider error text is not
    /// persisted: `error_class` must be a bounded, content-free machine label.
    pub async fn observe_response_failure(
        &self,
        mut correlation: magician::magician_v2::realtime_events::LlmEventCorrelation,
        started_at_ms: Option<i64>,
        response_ms: Option<u64>,
        error_class: &'static str,
    ) {
        let broadcaster = self.broadcaster.clone();
        let mut guard = self.state.lock().await;
        let Some(state) = guard.as_mut() else {
            return;
        };
        let now_ms = chrono::Utc::now().timestamp_millis();
        correlation
            .chat_session_id
            .clone_from(&state.chat_session_id);
        correlation
            .chat_turn_id
            .clone_from(&state.current_chat_turn_id);
        let provider = match state.current_descriptor.provider {
            magicllm::realtime::RealtimeProviderKind::OpenAi => "openai",
            magicllm::realtime::RealtimeProviderKind::OpenAiLive => "openai_live",
            magicllm::realtime::RealtimeProviderKind::Grok => "grok",
            magicllm::realtime::RealtimeProviderKind::Gemini => "gemini",
            magicllm::realtime::RealtimeProviderKind::HandsFree => "magician_hands_free",
        };
        broadcaster.emit_transport_only(RuntimeTransportEvent::LLMResponseReceived {
            execution_id: format!("voice:{}", state.voice_session_id),
            principal: Some(state.principal.clone()),
            workspace: Some(state.workspace.clone()),
            correlation: Some(correlation),
            plan_id: String::new(),
            step_id: None,
            step_index: None,
            capability: "voice.realtime".to_string(),
            success: false,
            decision_summary: String::new(),
            // Failed responses carry no authoritative usage or bill. The
            // canonical bridge keeps both unknown for failed transport events.
            cost: 0.0,
            latency_ms: response_ms.unwrap_or(0),
            error: Some(error_class.to_string()),
            provider: provider.to_string(),
            model: state.profile.model.clone(),
            usage_reported: false,
            input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: 0,
            reasoning_summary: None,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
            audio_input_tokens: None,
            audio_output_tokens: None,
            audio_cached_tokens: None,
            search_calls: 0,
            ttft_ms: None,
            task_id: None,
            agent_id: None,
            delegated_agent_id: None,
            chat_session_id: state.chat_session_id.clone(),
            operation: "voice_controller".to_string(),
            profile: None,
            attempt: 1,
            response_kind: "voice".to_string(),
            started_at_ms: started_at_ms
                .filter(|value| *value > 0 && *value <= now_ms)
                .unwrap_or(0),
            timestamp: now_ms,
        });
    }

    /// Change the native provider's turn authority for the next mint. The
    /// control actor follows this with a rotation because Gemini Live accepts
    /// automatic-activity detection only in its initial setup message.
    pub async fn replace_turn_detection_override(
        &self,
        turn_detection_mode: Option<String>,
    ) -> Result<Option<String>, OrchestratorError> {
        let mut guard = self.state.lock().await;
        let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
        if state.hands_free {
            return Err(OrchestratorError::NotConfigured(
                "native realtime turn detection cannot replace cascaded hands-free boundaries"
                    .to_string(),
            ));
        }
        Ok(std::mem::replace(
            &mut state.turn_detection_override,
            turn_detection_mode,
        ))
    }

    /// Rotate the upstream session. Compacts older turns into a
    /// summary, mints a new upstream session, updates lifecycle
    /// state, and returns the fresh descriptor + compacted resume
    /// context so the caller (WS actor) can replay context into
    /// the new session.
    pub async fn rotate(
        self: &Arc<Self>,
        reason: RotateReason,
    ) -> Result<RotationResult, OrchestratorError> {
        let snapshot = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            (
                state.principal.clone(),
                state.workspace.clone(),
                state.voice_session_id.clone(),
                state.thread_id.clone(),
                Arc::clone(&state.provider),
                state.rotation_count,
                state.current_descriptor.half_duplex,
                state.turn_detection_override.clone(),
                state.hands_free,
                state.preferred_voice.clone(),
                state.profile.provider.clone(),
                state.app_owner_session_credential.clone(),
                state.governed_history_capture_disabled,
            )
        };
        let (
            principal,
            workspace,
            voice_session_id,
            thread_id,
            provider,
            prior_rotation_count,
            half_duplex,
            turn_detection_override,
            cascaded_hands_free,
            preferred_voice,
            profile_provider,
            app_owner_session_credential,
            governed_history_capture_disabled,
        ) = snapshot;
        if let Some(credential) = app_owner_session_credential.as_ref() {
            credential.invalidate();
        }
        {
            let mut guard = self.state.lock().await;
            if let Some(state) = guard.as_mut() {
                state.app_owner_execution_credential = None;
                state.app_owner_session_credential = None;
            }
        }
        let reason_label = match reason {
            RotateReason::Proactive => "proactive",
            RotateReason::Watermark => "watermark",
            RotateReason::Reconnect => "reconnect",
            RotateReason::Manual => "manual",
        };
        self.broadcaster.emit_named(
            MEDIA_VOICE_SESSION_RECONNECT_ATTEMPT,
            MEDIA_SYSTEM_AGENT,
            Some(&principal),
            Some(&workspace),
            serde_json::json!({
                "voice_session_id": voice_session_id,
                "reason": reason_label,
                "prior_rotation_count": prior_rotation_count,
            }),
        );

        match provider
            .create_session(
                &principal,
                &workspace,
                &voice_session_id,
                thread_id.as_deref(),
                preferred_voice.as_deref(),
            )
            .await
        {
            Ok(mut descriptor) => {
                descriptor.half_duplex = half_duplex;
                apply_preferred_realtime_voice(
                    &mut descriptor,
                    &profile_provider,
                    preferred_voice.as_deref(),
                );
                apply_turn_detection_override(
                    &mut descriptor,
                    cascaded_hands_free,
                    turn_detection_override.as_deref(),
                );
                // A backend-proxied rotation is committed only after the fresh
                // upstream audio channel opens. Keeping the prior descriptor and
                // channel alive until this succeeds prevents a transient Gemini
                // reconnect failure from replacing a working call with silence.
                let new_audio_channel =
                    if matches!(descriptor.topology, RealtimeAudioTopology::BackendProxied) {
                        match provider.open_proxied_audio(&descriptor).await {
                            Ok(channel) => Some(channel),
                            Err(err) => {
                                let _ = provider.close_session(&descriptor).await;
                                return Err(err.into());
                            },
                        }
                    } else {
                        None
                    };
                // Compact only after the replacement transport is viable. A
                // transient connection failure may be retried several times;
                // repeating the same ledger/LLM compaction on every failed
                // socket would add cost and recovery latency without changing
                // the eventual resume context.
                let chat_session_id = match self.chat_session_id_for_call().await {
                    Ok(chat_session_id) => chat_session_id,
                    Err(err) => {
                        let _ = provider.close_session(&descriptor).await;
                        return Err(err);
                    },
                };
                let resume = if governed_history_capture_disabled {
                    ResumeContext {
                        summary: None,
                        recent_turns: Vec::new(),
                        tool_exchanges: Vec::new(),
                        total_turns: 0,
                    }
                } else {
                    self.compact_resume_context_or_empty(
                        &voice_session_id,
                        &chat_session_id,
                        &principal,
                        &workspace,
                        "rotation",
                    )
                    .await
                };
                let now_ms = Utc::now().timestamp_millis();
                self.lifecycle_store.record_rotated(
                    &voice_session_id,
                    descriptor.upstream_provider_session_id.clone(),
                    descriptor.max_session_duration_secs,
                    now_ms,
                );
                let updated = self
                    .lifecycle_store
                    .get(&voice_session_id)
                    .map(|lc| lc.rotation_count)
                    .unwrap_or(prior_rotation_count + 1);
                // Swap the descriptor on CallState + reach back for
                // the prior one so we can ask the provider to close
                // it. Provider impls MUST be idempotent + best-effort
                // (default no-op for DirectPeerToPeer).
                let prior_descriptor = {
                    let mut guard = self.state.lock().await;
                    guard
                        .as_mut()
                        .map(|state| state.apply_rotation(updated, descriptor.clone()))
                };
                if let Some(prior) = prior_descriptor {
                    if let Err(err) = provider.close_session(&prior).await {
                        warn!(
                            voice_session_id = %voice_session_id,
                            error = %err,
                            "[VOICE-ORCHESTRATOR] close_session failed on rotated prior; continuing"
                        );
                    }
                }
                let provider_label = provider_label_from_descriptor(&descriptor);
                // Read the boundary AFTER the rotation has been applied, so this
                // event is evidence that the surface and agent survived it rather
                // than a restatement of what they were before. An operator
                // comparing this against `media.voice.surface.resolved` sees the
                // preservation directly.
                let boundary = self.call_boundary().await;
                self.broadcaster.emit_named(
                    MEDIA_VOICE_SESSION_ROTATED,
                    MEDIA_SYSTEM_AGENT,
                    Some(&principal),
                    Some(&workspace),
                    serde_json::json!({
                        "voice_session_id": voice_session_id,
                        "provider": provider_label,
                        "reason": reason_label,
                        "rotation_count": updated,
                        "max_session_duration_secs": descriptor.max_session_duration_secs,
                        "surface": boundary.as_ref().map(|b| b.surface),
                        "audience": boundary.as_ref().map(|b| b.audience),
                        "agent_id": boundary.as_ref().map(|b| b.agent_id.clone()),
                        "binding": boundary.as_ref().map(|b| b.binding.clone()),
                    }),
                );
                info!(
                    voice_session_id = %voice_session_id,
                    reason = %reason_label,
                    rotation_count = updated,
                    "[VOICE-ORCHESTRATOR] upstream session rotated"
                );
                // Re-arm the proactive timer with the new upstream
                // session's cutoff. The prior timer already fired
                // (we're inside its callback) OR the rotation was
                // triggered by something else and we're abandoning
                // the pending fire — `schedule_proactive_rotation`
                // aborts any prior handle before replacing.
                self.schedule_proactive_rotation(descriptor.max_session_duration_secs)
                    .await;
                Ok(RotationResult {
                    descriptor,
                    resume,
                    rotation_count: updated,
                    new_audio_channel,
                })
            },
            Err(err) => {
                let message = err.to_string();
                self.broadcaster.emit_named(
                    MEDIA_VOICE_SESSION_RECONNECT_FAILED,
                    MEDIA_SYSTEM_AGENT,
                    Some(&principal),
                    Some(&workspace),
                    serde_json::json!({
                        "voice_session_id": voice_session_id,
                        "reason": reason_label,
                        "message": message,
                    }),
                );
                Err(err.into())
            },
        }
    }

    /// Ingest a finalised transcript turn into the chat ledger.
    /// Silent — no LLM trigger. The voice provider produced/heard
    /// the audio already; we just record it so the conversation
    /// shows up in the text timeline.
    ///
    /// Each finalised user utterance mints a new `chat_turn_id` stored
    /// on call state. Subsequent tool dispatches + the assistant
    /// transcript at response.done reuse it (so chat-side subscribers
    /// see one turn lifecycle per utterance, matching text-chat
    /// behaviour). The next user utterance replaces the id.
    ///
    /// Called by the WS actor on every `transcript.user.final` /
    /// `transcript.assistant.final` event surfaced by the upstream
    /// provider observer side-channel.
    pub async fn ingest_transcript_turn(
        &self,
        direction: ChatMessageDirection,
        text: &str,
    ) -> Result<(), OrchestratorError> {
        self.ingest_transcript_turn_at_epoch(direction, text, None, true)
            .await
    }

    /// Caller-authored `user.text` is not a server-observed finalized owner
    /// utterance, so it cannot advance governed Apps authority.
    pub async fn ingest_untrusted_user_text(&self, text: &str) -> Result<(), OrchestratorError> {
        self.ingest_transcript_turn_at_epoch(ChatMessageDirection::User, text, None, false)
            .await
    }

    pub async fn ingest_prebegun_user_transcript(
        &self,
        text: &str,
        chat_turn_id: &str,
    ) -> Result<(), OrchestratorError> {
        self.ingest_transcript_turn_at_epoch(
            ChatMessageDirection::User,
            text,
            Some(chat_turn_id),
            true,
        )
        .await
    }

    async fn ingest_transcript_turn_at_epoch(
        &self,
        direction: ChatMessageDirection,
        text: &str,
        prebegun_chat_turn_id: Option<&str>,
        admit_owner_turn: bool,
    ) -> Result<(), OrchestratorError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let chat_session_id = self.chat_session_id_for_call().await?;
        // Mint a fresh chat_turn_id when the user starts a new turn;
        // reuse the existing one for the assistant reply that closes it.
        // Falls back to a freshly-minted id if (for whatever reason) the
        // assistant reply arrives before we saw a finalised user
        // transcript — keeps the message correlation usable in the rare
        // edge case rather than writing `None`.
        let (
            chat_turn_id,
            principal,
            workspace,
            voice_session_id,
            source_surface,
            history_capture_allowed,
        ) = {
            let mut guard = self.state.lock().await;
            let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
            let turn_id = match direction {
                ChatMessageDirection::User => {
                    if let Some(expected_turn_id) = prebegun_chat_turn_id {
                        if !state.prebegun_user_turn_pending_ingest
                            || state.current_chat_turn_id.as_deref() != Some(expected_turn_id)
                        {
                            return Err(OrchestratorError::Cancelled);
                        }
                        state.prebegun_user_turn_pending_ingest = false;
                        expected_turn_id.to_owned()
                    } else {
                        let id = format!("voice-turn-{}", uuid::Uuid::new_v4());
                        state.replace_current_chat_turn(id.clone(), admit_owner_turn);
                        id
                    }
                },
                // Assistant + System reuse the active turn id (system
                // messages from backend-pushed notifications like
                // task.completed land between user/assistant boundaries
                // and should hang off the most-recent turn).
                ChatMessageDirection::Assistant | ChatMessageDirection::System => state
                    .current_chat_turn_id
                    .clone()
                    .unwrap_or_else(|| format!("voice-turn-{}", uuid::Uuid::new_v4())),
            };
            (
                turn_id,
                state.principal.clone(),
                state.workspace.clone(),
                state.voice_session_id.clone(),
                state.source_surface.clone(),
                !state.governed_history_capture_disabled,
            )
        };
        if !history_capture_allowed {
            return Ok(());
        }
        let msg = ChatMessage::new(
            uuid::Uuid::new_v4().to_string(),
            chat_session_id.clone(),
            direction.clone(),
            ChatMessageContent::Text {
                text: trimmed.to_string(),
                plan_reply: None,
            },
            Utc::now().timestamp_millis(),
        )
        .with_chat_turn_id(Some(chat_turn_id))
        // Voice-originated turns. Carries through to chat surfaces
        // so they can pick auto-read / display affordances. We
        // never produce parsed speech segments here — that's a
        // chat-service responsibility on the LLM-driven path.
        .with_voice_origin(Some(true))
        .with_speech_segments(None)
        .with_source_surface(Some(source_surface))
        .with_presence_session_id(Some(voice_session_id.clone()));
        // Emit `ChatMessageReceived` onto the canonical transport bus.
        // The `ChatStoreSink` subscriber persists the message to the
        // per-session JSONL on disk; the frontend SSE subscriber
        // forwards it to the chat UI live. Prior to this we called
        // `chat_store.append_message` directly, which wrote the
        // message to disk but bypassed the bus entirely — the chat UI
        // then never saw new voice turns until the user refreshed
        // (the refresh re-read from chat_store). Single-bus model
        // (see `chat_store_sink.rs` + `ChatService::emit_chat_message`).
        self.broadcaster
            .emit_transport_only(RuntimeTransportEvent::ChatMessageReceived {
                session_id: chat_session_id.clone(),
                message: msg,
                principal: Some(principal),
                workspace: Some(workspace),
                origin_channel: Some(ChatChannel::new("voice", voice_session_id)),
                timestamp: Utc::now().timestamp_millis(),
            });

        let history_entry = match direction {
            ChatMessageDirection::User => Some(ChatLlmTranscriptEntry::UserText {
                text: trimmed.to_string(),
            }),
            ChatMessageDirection::Assistant => Some(ChatLlmTranscriptEntry::AssistantTurn {
                text: Some(trimmed.to_string()),
                tool_calls: Vec::new(),
                provider_state: None,
            }),
            ChatMessageDirection::System => None,
        };
        if let Some(entry) = history_entry {
            self.chat_store
                .append_llm_history_entries(&chat_session_id, vec![entry])
                .await
                .map_err(|e| OrchestratorError::Io(e.to_string()))?;
        }
        Ok(())
    }

    /// Advance the exact owner turn epoch before response-gated retrieval and
    /// transcript durability fan out concurrently. This is lock-only and does
    /// no storage I/O; delayed work from the previous utterance is invalidated
    /// before either new branch can discover or render governed app data.
    pub async fn prebegin_realtime_user_turn(
        &self,
        turn_invalidation: CancellationToken,
    ) -> Result<String, OrchestratorError> {
        let mut guard = self.state.lock().await;
        let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
        let id = format!("voice-turn-{}", uuid::Uuid::new_v4());
        if let Some(credential) = state.app_owner_execution_credential.as_ref() {
            if credential
                .begin_realtime_turn_with_invalidation(&id, turn_invalidation, Utc::now())
                .is_err()
            {
                credential.invalidate_realtime_turn();
            }
        }
        state.current_chat_turn_id = Some(id.clone());
        state.prebegun_user_turn_pending_ingest = true;
        Ok(id)
    }

    /// Deterministic live-voice Personal Tutor takeover.
    ///
    /// Normal live voice transcripts are ledger-only because the realtime
    /// provider owns the assistant response. Explicit tutor invoke phrases are
    /// different: they must enter the normal chat/tutor runtime so
    /// `start_tutor_run`, `screen-draw`, storyboard validation, overlay
    /// playback, activity cards, and LLM telemetry all use the same path as
    /// typed/dictated tutor prompts.
    pub async fn submit_tutor_takeover_turn(
        &self,
        text: &str,
        feature_mode: magician::magician_v2::agents::FeatureMode,
        capture_screen: bool,
        takeover_cancel_token: tokio_util::sync::CancellationToken,
    ) -> Result<(), OrchestratorError> {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let chat_session_id = self.chat_session_id_for_call().await?;
        let (
            chat_turn_id,
            voice_session_id,
            hands_free,
            concurrent_requests,
            call_cancel_token,
            existing_chat_run_token,
        ) = {
            let mut guard = self.state.lock().await;
            let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
            let id = format!("voice-turn-{}", uuid::Uuid::new_v4());
            state.replace_current_chat_turn(id.clone(), false);
            (
                id,
                state.voice_session_id.clone(),
                state.hands_free,
                state.concurrent_requests,
                state.cancel_token.clone(),
                state.chat_run_token.clone(),
            )
        };
        if call_cancel_token.is_cancelled() || takeover_cancel_token.is_cancelled() {
            return Err(OrchestratorError::Cancelled);
        }
        // The voice-control actor has already recognized an explicit Tutor or
        // App Copilot takeover inside an authenticated live voice session.
        // Convert that trusted transition into the typed feature surface;
        // ordinary realtime transcripts continue to use the voice surface and
        // never receive these tools. Batch 7 (F11): the mode→surface spelling
        // derives from the chat lane registry
        // ([`voice_takeover_feature_surface`]) instead of an inline
        // FeatureMode match.
        let feature_surface = voice_takeover_feature_surface(feature_mode)?;
        if self.chat_service.has_active_tail(&chat_session_id) {
            return Err(OrchestratorError::Io(
                "guided flow cannot start while this chat is following an active task".to_string(),
            ));
        }

        // Concurrent and hands-free calls claim a slot only for an actual
        // guided turn, then release it. They cannot displace a typed turn.
        // Legacy vendor calls reuse their call-wide sentinel.
        let (turn_cancel_token, clear_active_run_on_finish) = if hands_free || concurrent_requests {
            let token = call_cancel_token.child_token();
            if !self
                .chat_service
                .try_register_owned_active_chat_run_with_token_admitted(
                    &chat_session_id,
                    token.clone(),
                    &voice_session_id,
                )
                .await
            {
                return Err(OrchestratorError::Io(
                    "guided flow could not start while another chat turn is active".to_string(),
                ));
            }
            (token, true)
        } else if let Some(token) = existing_chat_run_token.filter(|token| !token.is_cancelled()) {
            (token, false)
        } else {
            // `/stop` removes and cancels the prior sentinel. Reacquire a fresh
            // child so the audio call stays usable for a later guided command.
            let token = call_cancel_token.child_token();
            if !self
                .chat_service
                .try_register_owned_active_chat_run_with_token_admitted(
                    &chat_session_id,
                    token.clone(),
                    &voice_session_id,
                )
                .await
            {
                return Err(OrchestratorError::Io(
                    "guided flow could not reacquire the live voice chat lane".to_string(),
                ));
            }
            let mut guard = self.state.lock().await;
            let Some(state) = guard.as_mut() else {
                token.cancel();
                self.chat_service
                    .clear_cancelled_active_chat_run(&chat_session_id);
                return Err(OrchestratorError::Cancelled);
            };
            if state.voice_session_id != voice_session_id {
                token.cancel();
                self.chat_service
                    .clear_cancelled_active_chat_run(&chat_session_id);
                return Err(OrchestratorError::Cancelled);
            }
            state.chat_run_token = Some(token.clone());
            (token, false)
        };
        let attachment_ids = if capture_screen
            || feature_mode == magician::magician_v2::agents::FeatureMode::AppCopilot
        {
            let capture =
                crate::media_rails::screen_capture::capture_full_screen_attachment_for_session(
                    self.chat_service.as_ref(),
                    &chat_session_id,
                    feature_surface,
                    &takeover_cancel_token,
                );
            let attachment_id = tokio::select! {
                biased;
                _ = turn_cancel_token.cancelled() => {
                    if clear_active_run_on_finish {
                        self.chat_service.clear_cancelled_active_chat_run(&chat_session_id);
                    }
                    return Err(OrchestratorError::Cancelled);
                },
                result = capture => result.map_err(|error| {
                    OrchestratorError::Io(format!(
                        "live voice guided-flow screen capture failed: {error}"
                    ))
                }),
            };
            match attachment_id {
                Ok(attachment_id) => vec![attachment_id],
                Err(error) => {
                    if clear_active_run_on_finish {
                        turn_cancel_token.cancel();
                        self.chat_service
                            .clear_cancelled_active_chat_run(&chat_session_id);
                    }
                    return Err(error);
                },
            }
        } else {
            Vec::new()
        };

        let process = self.chat_service.process_message_with_owned_active_run(
            &chat_session_id,
            Some(trimmed),
            &attachment_ids,
            Some(&chat_turn_id),
            true,
            Some(feature_surface),
            Some(&voice_session_id),
            turn_cancel_token.clone(),
            clear_active_run_on_finish,
            // Tutor turns never think through a harness mouth.
            None,
        );
        let response = tokio::select! {
            biased;
            _ = takeover_cancel_token.cancelled() => {
                turn_cancel_token.cancel();
                Err(OrchestratorError::Cancelled)
            },
            result = process => result.map_err(|err| {
                OrchestratorError::Io(format!(
                    "live voice tutor takeover chat turn failed: {err:#}"
                ))
            }),
        };
        let discard_uncommitted_capture = response.is_err()
            || response.as_ref().is_ok_and(|response| {
                response.cancelled || response.queued.is_some() || turn_cancel_token.is_cancelled()
            });
        if clear_active_run_on_finish && response.is_err() {
            turn_cancel_token.cancel();
            self.chat_service
                .clear_cancelled_active_chat_run(&chat_session_id);
        }
        if clear_active_run_on_finish && turn_cancel_token.is_cancelled() {
            self.chat_service
                .clear_cancelled_active_chat_run(&chat_session_id);
        }
        if discard_uncommitted_capture {
            for attachment_id in &attachment_ids {
                if let Err(error) = self
                    .chat_service
                    .discard_unreferenced_screen_capture_attachment_for_owner(
                        &chat_session_id,
                        attachment_id,
                        &voice_session_id,
                    )
                    .await
                {
                    warn!(
                        chat_session_id = %chat_session_id,
                        %attachment_id,
                        %error,
                        "[VOICE-ORCHESTRATOR] failed to discard uncommitted guided-flow capture"
                    );
                }
            }
        }
        let response = response?;
        if response.cancelled || turn_cancel_token.is_cancelled() {
            return Err(OrchestratorError::Cancelled);
        }
        if response.queued.is_some() {
            return Err(OrchestratorError::Io(
                "guided flow was unexpectedly queued instead of started".to_string(),
            ));
        }
        Ok(())
    }

    async fn append_voice_tool_call_history(&self, chat_session_id: &str, call: &LLMToolCall) {
        if !self.voice_history_capture_allowed().await {
            return;
        }
        let entry = ChatLlmTranscriptEntry::AssistantTurn {
            text: None,
            tool_calls: vec![StoredToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
            }],
            provider_state: None,
        };
        if let Err(err) = self
            .chat_store
            .append_llm_history_entries(chat_session_id, vec![entry])
            .await
        {
            warn!(
                chat_session_id = %chat_session_id,
                tool_name = %call.name,
                call_id = %call.id,
                error = %err,
                "[VOICE-ORCHESTRATOR] failed to persist voice tool-call transcript entry"
            );
        }
    }

    async fn append_voice_tool_exchange_history(
        &self,
        chat_session_id: &str,
        call: &LLMToolCall,
        output: &str,
    ) {
        if !self.voice_history_capture_allowed().await {
            return;
        }
        self.append_voice_tool_call_history(chat_session_id, call)
            .await;
        self.append_voice_tool_result_history(chat_session_id, call, output)
            .await;
    }

    async fn append_voice_projected_tool_exchange_history(
        &self,
        chat_session_id: &str,
        call: &LLMToolCall,
        output: &str,
        projection: Option<magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
    ) {
        if !self.voice_history_capture_allowed().await {
            return;
        }
        self.append_voice_tool_call_history(chat_session_id, call)
            .await;
        let entry = match projection {
            Some(projection) => ChatLlmTranscriptEntry::ToolResultProjected {
                tool_call_id: call.id.clone(),
                tool_name: Some(call.name.clone()),
                projection,
            },
            None => ChatLlmTranscriptEntry::ToolResult {
                tool_call_id: call.id.clone(),
                tool_name: Some(call.name.clone()),
                content: output.to_string(),
            },
        };
        if let Err(err) = self
            .chat_store
            .append_llm_history_entries(chat_session_id, vec![entry])
            .await
        {
            warn!(
                chat_session_id = %chat_session_id,
                tool_name = %call.name,
                call_id = %call.id,
                error = %err,
                "[VOICE-ORCHESTRATOR] failed to persist projected voice tool exchange"
            );
        }
    }

    async fn append_voice_tool_result_history(
        &self,
        chat_session_id: &str,
        call: &LLMToolCall,
        output: &str,
    ) {
        if !self.voice_history_capture_allowed().await {
            return;
        }
        let entry = ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: call.id.clone(),
            tool_name: Some(call.name.clone()),
            content: output.to_string(),
        };
        if let Err(err) = self
            .chat_store
            .append_llm_history_entries(chat_session_id, vec![entry])
            .await
        {
            warn!(
                chat_session_id = %chat_session_id,
                tool_name = %call.name,
                call_id = %call.id,
                error = %err,
                "[VOICE-ORCHESTRATOR] failed to persist voice tool-result transcript entry"
            );
        }
    }

    async fn voice_history_capture_allowed(&self) -> bool {
        let guard = self.state.lock().await;
        guard
            .as_ref()
            .is_none_or(|state| !state.governed_history_capture_disabled)
    }

    /// Finalize the durable result half of a catalog-changing realtime tool
    /// call after the transport either commits or rejects the prepared
    /// catalog. The call half is persisted at dispatch time so provider
    /// rotation cannot lose its identity; exactly one result is appended here.
    pub async fn finalize_voice_catalog_tool_result(
        &self,
        tool_name: &str,
        call_id: &str,
        output: &str,
        projection: Option<magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
    ) {
        let Ok(chat_session_id) = self.chat_session_id_for_call().await else {
            warn!(
                tool_name,
                call_id,
                "[VOICE-ORCHESTRATOR] could not resolve chat session while finalizing catalog \
                 tool result"
            );
            return;
        };
        let call = LLMToolCall {
            id: call_id.to_string(),
            name: tool_name.to_string(),
            arguments: Value::Null,
        };
        match projection {
            Some(projection) => {
                let entry = ChatLlmTranscriptEntry::ToolResultProjected {
                    tool_call_id: call.id.clone(),
                    tool_name: Some(call.name.clone()),
                    projection,
                };
                if let Err(error) = self
                    .chat_store
                    .append_llm_history_entries(&chat_session_id, vec![entry])
                    .await
                {
                    warn!(
                        chat_session_id,
                        tool_name,
                        call_id,
                        error = %error,
                        "[VOICE-ORCHESTRATOR] failed to persist committed catalog tool result"
                    );
                }
            },
            None => {
                self.append_voice_tool_result_history(&chat_session_id, &call, output)
                    .await;
            },
        }
    }

    /// Dispatch a tool call invoked by the realtime voice model.
    ///
    /// Phase A1 of voice-as-chat-agent: routes through `ChatService`'s
    /// public `dispatch_external_tool_call`, which uses the SAME private
    /// `dispatch_chat_tool_call` the text chat agent uses. The realtime
    /// model and text chat agent share one tool registry, one dispatcher.
    ///
    /// `tool_name`/`arguments_json`/`call_id` come from the provider's
    /// function-call event (OpenAI Realtime:
    /// `response.function_call_arguments.done`; Gemini Live:
    /// `toolCall.functionCalls[*]`). Returns a
    /// `VoiceToolDispatchResponse` — the control WS actor forwards it
    /// to the frontend as `tool.result`, the frontend then writes it
    /// back to the realtime peer in the provider-native function-output
    /// format.
    pub async fn dispatch_tool(
        &self,
        tool_name: String,
        arguments_json: String,
        call_id: String,
    ) -> Result<VoiceToolDispatchResponse, OrchestratorError> {
        if self.concurrent_voice_enabled().await && tool_name != VOICE_DELEGATE_TO_CHAT_TOOL {
            return Ok(build_voice_tool_dispatch_response(
                tool_name,
                call_id,
                json!({"error": "Concurrent voice work must use delegate_to_chat with the user's request."}),
                VoiceToolDispatchStatus::Error,
            ));
        }
        // The agent this call belongs to — a room's is the ambassador.
        let call_agent_id = self.call_agent_id().await;
        // Parse args JSON once. Providers always stringify their function-call
        // args, so this step is unavoidable.
        let arguments = match serde_json::from_str::<serde_json::Value>(&arguments_json) {
            Ok(v) => v,
            Err(err) => {
                return Ok(build_voice_tool_dispatch_response(
                    tool_name,
                    call_id,
                    json!({
                        "error": format!("invalid arguments_json: {err}")
                    }),
                    VoiceToolDispatchStatus::Error,
                ));
            },
        };

        let chat_session_id = self.chat_session_id_for_call().await?;
        // Use the current voice turn's chat_turn_id (set by the user
        // transcript that opened the turn) so all broadcaster events
        // from this dispatch correlate with the same turn the user/
        // assistant messages were stamped with. Falls back to the
        // realtime model's call_id if (somehow) a tool fires outside a
        // user transcript — chat-side subscribers still get a stable
        // correlator.
        let chat_turn_id = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .and_then(|s| s.current_chat_turn_id.clone())
                .unwrap_or_else(|| call_id.clone())
        };
        let call = LLMToolCall {
            id: call_id.clone(),
            name: tool_name.clone(),
            arguments,
        };
        let expected_policy_snapshot_id = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .ok_or(OrchestratorError::NotStarted)?
                .policy_snapshot_id
                .clone()
                .ok_or_else(|| {
                    OrchestratorError::Io(
                        "realtime tool policy snapshot was not advertised".to_string(),
                    )
                })?
        };

        // Voice-only Magician-brain hatch. The catalog advertises it ONLY to
        // realtime / Live sessions (`build_voice_catalog_bundle` injects the
        // spec; chat's inline-turn catalog never sees it, so this cannot
        // recurse). The speech frontend invokes it when Magician should
        // think — including tools. GPT Realtime can also call Magician tools
        // in-session; if it delegates instead, this path still runs them.
        //
        // We DON'T block the speech loop on the Magician turn. Instead we:
        //   1. Persist the tool-call entry immediately so the ledger reflects the
        //      function call.
        //   2. Spawn a background Magician chat turn on the call's owned
        //      active-run token (tools, same catalog as typed chat), pushing
        //      speakable segments as `delegate_to_chat.chunk`; on completion
        //      pushes `delegate_to_chat.done` and persists the tool-result.
        //   3. Return an ack tool-result to the speech frontend right away —
        //      `voice_summary` tells it to say something brief while chunks
        //      arrive as system / commentary injections.
        if tool_name == VOICE_DELEGATE_TO_CHAT_TOOL {
            // The SAME agent the session is bound to: authorizing as the
            // personal assistant while a room's session belongs to the
            // ambassador would check the wrong agent's policy.
            if let Err(error) = self
                .chat_service
                .authorize_external_tool_call(
                    &chat_session_id,
                    &call_agent_id,
                    &tool_name,
                    &call.arguments,
                    &expected_policy_snapshot_id,
                )
                .await
            {
                self.record_tool_refusal_if_refused(&error, &tool_name)
                    .await;
                return Err(OrchestratorError::Io(error.to_string()));
            }
            let intent = call
                .arguments
                .get("intent")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let Some(intent) = intent else {
                let raw = json!({
                    "status": "error",
                    "error": "missing required parameter: intent"
                });
                let projection = self
                    .chat_service
                    .project_external_native_tool_result(
                        &chat_session_id,
                        &call_agent_id,
                        &call,
                        &raw,
                        &expected_policy_snapshot_id,
                    )
                    .await
                    .ok();
                let response = build_voice_tool_dispatch_response_with_projection(
                    tool_name,
                    call_id,
                    raw,
                    VoiceToolDispatchStatus::Error,
                    projection.as_ref(),
                );
                self.append_voice_projected_tool_exchange_history(
                    &chat_session_id,
                    &call,
                    &response.output,
                    projection,
                )
                .await;
                return Ok(response);
            };

            if self.concurrent_voice_enabled().await {
                let context = self.state.lock().await.as_ref().and_then(|s| s.concurrent_context_session_id.clone());
                let receipt = self.submit_concurrent_voice_turn(
                    &intent, &format!("delegate-{call_id}"), context,
                ).await?;
                let raw = json!({
                    "status": "accepted", "request_id": receipt.id,
                    "context_session_id": receipt.branch_session_id,
                    "voice_summary": "I'm working on that. You can ask me another question.",
                    "reason": "The application will speak the saved result when the listener is free. Do not poll, stream, or speak this result yourself."
                });
                let projection = self.chat_service.project_external_native_tool_result(
                    &chat_session_id, &call_agent_id, &call, &raw, &expected_policy_snapshot_id,
                ).await.ok();
                let response = build_voice_tool_dispatch_response_with_projection(
                    tool_name, call_id, raw, VoiceToolDispatchStatus::Ok, projection.as_ref(),
                );
                self.append_voice_projected_tool_exchange_history(
                    &chat_session_id, &call, &response.output, projection,
                ).await;
                return Ok(response);
            }

            // Persist the tool-call right away so chat-side
            // subscribers see the call before the streamed chunks
            // start arriving. The tool-result entry is appended by
            // the background task on completion (with the full
            // answer, not the ack).
            self.append_voice_tool_call_history(&chat_session_id, &call)
                .await;

            // Resolve the voice_session_id while we still hold sync
            // access to the orchestrator. The background spawn only
            // captures the cloned id — never re-locks `self.state`.
            let (voice_session_id, chat_run_token, chat_profile_override) = {
                let guard = self.state.lock().await;
                let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
                (
                    state.voice_session_id.clone(),
                    state.chat_run_token.clone(),
                    state
                        .chat_choice
                        .as_ref()
                        .map(magician::magician_v2::execution::plane::encode_chat_harness_choice),
                )
            };

            // Background streaming dispatch. All captured handles are
            // Arc-cloned; the task outlives the dispatch_tool future.
            let chat_service = Arc::clone(&self.chat_service);
            let chat_store = Arc::clone(&self.chat_store);
            let downstream_fanout = Arc::clone(&self.downstream_fanout);
            let chat_session_id_for_task = chat_session_id.clone();
            let voice_session_id_for_task = voice_session_id.clone();
            let chat_turn_id_for_task = chat_turn_id.clone();
            let call_for_task = call.clone();
            let tool_name_for_task = tool_name.clone();
            let call_id_for_task = call_id.clone();
            let policy_snapshot_id_for_task = expected_policy_snapshot_id.clone();
            // The spawned task outlives this scope, so it takes its own copy —
            // the sites after the spawn still need the original.
            let call_agent_id_for_task = call_agent_id.clone();
            let delegate_cancel_token = self.tool_cancel_token().await?;
            let magician_engine_token = chat_run_token
                .filter(|token| !token.is_cancelled())
                .unwrap_or_else(|| delegate_cancel_token.clone());

            tokio::spawn(async move {
                use crate::media_rails::voice_downstream_fanout::{
                    delegate_to_chat_chunk_message, delegate_to_chat_done_message,
                };

                // Per-call counter shared across the streaming
                // callback. Atomic so the sync callback (called from
                // magicllm's streaming consumer task) can bump it
                // without locking. `Relaxed` is fine — we only need a
                // monotonic-ish hint for the frontend; exact ordering
                // already comes from the fanout's single mpsc.
                let sequence = Arc::new(std::sync::atomic::AtomicU64::new(0));
                let chunk_count = Arc::new(std::sync::atomic::AtomicU64::new(0));

                let on_segment: Arc<dyn Fn(crate::media_rails::SpeechSegment) + Send + Sync> = {
                    let downstream_fanout = Arc::clone(&downstream_fanout);
                    let sequence = Arc::clone(&sequence);
                    let chunk_count = Arc::clone(&chunk_count);
                    let voice_session_id = voice_session_id_for_task.clone();
                    let call_id = call_id_for_task.clone();
                    Arc::new(move |segment| {
                        let seq = sequence.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        chunk_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let message = delegate_to_chat_chunk_message(&call_id, seq, &segment.text);
                        let delivered = downstream_fanout.notify(&voice_session_id, message);
                        if !delivered {
                            // Voice session torn down mid-stream
                            // (user ended the call). The streaming
                            // task can't abort cleanly from the sync
                            // callback — the magicllm call will run
                            // to completion and the result will land
                            // on the ledger. Nothing actionable; the
                            // fanout's own debug log records the drop.
                        }
                    })
                };

                let streaming_started_at = std::time::Instant::now();
                let result = tokio::select! {
                    _ = delegate_cancel_token.cancelled() => {
                        Err(anyhow::anyhow!("voice call ended before delegate_to_chat completed"))
                    },
                    result = run_voice_magician_engine_turn(
                        Arc::clone(&chat_service),
                        chat_session_id_for_task.clone(),
                        intent.clone(),
                        chat_turn_id_for_task.clone(),
                        voice_session_id_for_task.clone(),
                        magician_engine_token.clone(),
                        Arc::clone(&on_segment),
                        chat_profile_override.clone(),
                    ) => result,
                };

                // Push the done envelope before persisting so the
                // frontend sees the "stream finished" signal as
                // promptly as possible. Ledger persistence is a
                // separate concern.
                let (success, error_msg, full_answer, speak_answer) = match &result {
                    Ok(resp) => (true, None, resp.full.clone(), resp.speak.clone()),
                    Err(err) => (false, Some(err.to_string()), String::new(), String::new()),
                };
                let chunks = chunk_count.load(std::sync::atomic::Ordering::Relaxed);
                let done_message = delegate_to_chat_done_message(
                    &call_id_for_task,
                    chunks,
                    success,
                    error_msg.as_deref(),
                );
                downstream_fanout.notify(&voice_session_id_for_task, done_message);

                let elapsed_ms = streaming_started_at.elapsed().as_millis();
                match &result {
                    Ok(_) => debug!(
                        chat_session_id = %chat_session_id_for_task,
                        voice_session_id = %voice_session_id_for_task,
                        call_id = %call_id_for_task,
                        chunks = chunks,
                        elapsed_ms = elapsed_ms as u64,
                        "[VOICE-ORCHESTRATOR] delegate_to_chat streaming completed"
                    ),
                    Err(err) => warn!(
                        chat_session_id = %chat_session_id_for_task,
                        voice_session_id = %voice_session_id_for_task,
                        call_id = %call_id_for_task,
                        elapsed_ms = elapsed_ms as u64,
                        error = %err,
                        "[VOICE-ORCHESTRATOR] delegate_to_chat streaming failed"
                    ),
                }

                // Persist the tool-result entry to the chat ledger.
                // On success the full chat-LLM answer becomes the
                // tool-result content (the screen-side text), with
                // the speak field preserved for completeness; on
                // failure we record the error so future turns see
                // the failure context.
                let result_value = if success {
                    json!({
                        "status": "ok",
                        "voice_summary": speak_answer,
                        "answer": full_answer,
                        "streamed_chunks": chunks,
                    })
                } else {
                    json!({
                        "status": "error",
                        "error": error_msg.unwrap_or_default(),
                    })
                };
                let projected = chat_service
                    .project_external_native_tool_result(
                        &chat_session_id_for_task,
                        &call_agent_id_for_task,
                        &call_for_task,
                        &result_value,
                        &policy_snapshot_id_for_task,
                    )
                    .await;
                let entry = match projected {
                    Ok(projection) => ChatLlmTranscriptEntry::ToolResultProjected {
                        tool_call_id: call_for_task.id.clone(),
                        tool_name: Some(call_for_task.name.clone()),
                        projection,
                    },
                    Err(error) => {
                        warn!(
                            chat_session_id = %chat_session_id_for_task,
                            call_id = %call_for_task.id,
                            error = %error,
                            "[VOICE-ORCHESTRATOR] streamed delegate result projection failed; persisting minimal balanced result"
                        );
                        ChatLlmTranscriptEntry::ToolResult {
                            tool_call_id: call_for_task.id.clone(),
                            tool_name: Some(call_for_task.name.clone()),
                            content: magician::magician_v2::tool_result_runtime::balanced_projection_failure_value(
                                &result_value,
                                "materialization_or_projection_failed",
                            )
                            .to_string(),
                        }
                    },
                };
                if let Err(err) = chat_store
                    .append_llm_history_entries(&chat_session_id_for_task, vec![entry])
                    .await
                {
                    warn!(
                        chat_session_id = %chat_session_id_for_task,
                        tool_name = %tool_name_for_task,
                        call_id = %call_id_for_task,
                        error = %err,
                        "[VOICE-ORCHESTRATOR] failed to persist streamed \
                         delegate_to_chat tool-result transcript entry"
                    );
                }
            });

            // Ack returned to the realtime model immediately. The
            // `voice_summary` field is what `derive_voice_tool_summary`
            // surfaces for the model to speak; we tell it to say
            // something honest and brief while chunks arrive via
            // system messages. We DO NOT call
            // `append_voice_tool_exchange_history` here — the call
            // entry was persisted above and the result entry will be
            // persisted by the background task with the full answer
            // (not this ack).
            let ack_value = json!({
                "status": "streaming",
                "voice_summary": "Let me think out loud — I'll speak each thought as it lands.",
                "reason": "delegate_to_chat is streaming; spoken segments arrive via system-message injections, no further action needed from you until they stop",
            });
            let ack_projection = self
                .chat_service
                .project_external_native_tool_result(
                    &chat_session_id,
                    &call_agent_id,
                    &call,
                    &ack_value,
                    &expected_policy_snapshot_id,
                )
                .await
                .ok();
            let response = build_voice_tool_dispatch_response_with_projection(
                tool_name,
                call_id,
                ack_value,
                VoiceToolDispatchStatus::Ok,
                ack_projection.as_ref(),
            );
            return Ok(response);
        }

        // Time-box the dispatch so a slow tool can't block the realtime
        // model's response loop. On timeout we cancel a child token and
        // briefly wait for chat dispatch to run its cleanup path (notably
        // progress-router subscription teardown) before synthesizing the
        // voice-facing result.
        let tool_cancel_token = self.tool_cancel_token().await?;
        let governed_app_tool =
            magician::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool(
                &tool_name,
            );
        let app_owner_credential = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .and_then(|state| state.app_owner_execution_credential.clone())
        };
        let dispatch_inner = self
            .chat_service
            .dispatch_external_tool_call_with_catalog_update(
                &chat_session_id,
                &call_agent_id,
                &call,
                Some(&chat_turn_id),
                &expected_policy_snapshot_id,
                Some(tool_cancel_token.clone()),
            );
        let dispatch_future =
            magician::magician_v2::chat::service::scope_app_owner_execution_credential(
                app_owner_credential,
                dispatch_inner,
            );
        tokio::pin!(dispatch_future);
        match tokio::time::timeout(VOICE_TOOL_DISPATCH_TIMEOUT, &mut dispatch_future).await {
            Ok(Ok(outcome)) => {
                let projection = outcome.projected_result;
                let mut response = build_voice_tool_dispatch_response_with_projection(
                    tool_name,
                    call_id,
                    outcome.value,
                    VoiceToolDispatchStatus::Ok,
                    projection.as_ref(),
                );
                response.catalog_update = outcome.catalog_update;
                let protected_catalog_update = response.catalog_update.as_ref().is_some_and(|update| {
                    update.tools.iter().any(|tool| {
                        magician::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool(
                            &tool.name,
                        )
                    })
                });
                if governed_app_tool || protected_catalog_update {
                    response.provider_delivery_fence =
                        self.current_realtime_app_delivery_fence().await?;
                    if response.provider_delivery_fence.is_none() {
                        return Ok(build_voice_tool_dispatch_response(
                            call.name.clone(),
                            call.id.clone(),
                            json!({
                                "status": "error",
                                "error_code": "AppVoiceDeliveryFenceUnavailable",
                                "reason": "The governed result was withheld because the authenticated realtime provider binding changed."
                            }),
                            VoiceToolDispatchStatus::Error,
                        ));
                    }
                } else if response.catalog_update.is_some() {
                    // The prepared result is provisional until the provider
                    // installs the matching catalog. Persist the call now and
                    // let the control rail append exactly one final result on
                    // commit/rejection.
                    self.append_voice_tool_call_history(&chat_session_id, &call)
                        .await;
                } else {
                    self.append_voice_projected_tool_exchange_history(
                        &chat_session_id,
                        &call,
                        &response.output,
                        projection,
                    )
                    .await;
                }
                Ok(response)
            },
            Ok(Err(err)) => {
                let response = build_voice_tool_dispatch_response(
                    tool_name,
                    call_id,
                    json!({ "error": err.to_string() }),
                    VoiceToolDispatchStatus::Error,
                );
                self.append_voice_tool_exchange_history(&chat_session_id, &call, &response.output)
                    .await;
                Ok(response)
            },
            Err(_elapsed) => {
                warn!(
                    tool_name = %tool_name,
                    chat_turn_id = %chat_turn_id,
                    timeout_secs = VOICE_TOOL_DISPATCH_TIMEOUT.as_secs(),
                    "[VOICE-ORCHESTRATOR] tool dispatch timed out"
                );
                tool_cancel_token.cancel();
                match tokio::time::timeout(VOICE_TOOL_CANCEL_CLEANUP_TIMEOUT, &mut dispatch_future)
                    .await
                {
                    Ok(Ok(outcome)) => {
                        let projection = outcome.projected_result;
                        let mut response = build_voice_tool_dispatch_response_with_projection(
                            tool_name,
                            call_id,
                            outcome.value,
                            VoiceToolDispatchStatus::Ok,
                            projection.as_ref(),
                        );
                        response.catalog_update = outcome.catalog_update;
                        let protected_catalog_update = response.catalog_update.as_ref().is_some_and(|update| {
                            update.tools.iter().any(|tool| {
                                magician::magician_v2::execution::compiled_dispatch::is_governed_app_compiled_tool(
                                    &tool.name,
                                )
                            })
                        });
                        if governed_app_tool || protected_catalog_update {
                            response.provider_delivery_fence =
                                self.current_realtime_app_delivery_fence().await?;
                            if response.provider_delivery_fence.is_none() {
                                return Ok(build_voice_tool_dispatch_response(
                                    call.name.clone(),
                                    call.id.clone(),
                                    json!({
                                        "status": "error",
                                        "error_code": "AppVoiceDeliveryFenceUnavailable",
                                        "reason": "The governed result was withheld because the authenticated realtime provider binding changed."
                                    }),
                                    VoiceToolDispatchStatus::Error,
                                ));
                            }
                        } else if response.catalog_update.is_some() {
                            self.append_voice_tool_call_history(&chat_session_id, &call)
                                .await;
                        } else {
                            self.append_voice_projected_tool_exchange_history(
                                &chat_session_id,
                                &call,
                                &response.output,
                                projection,
                            )
                            .await;
                        }
                        Ok(response)
                    },
                    Ok(Err(err)) => {
                        let response = build_voice_tool_dispatch_response(
                            tool_name,
                            call_id,
                            json!({ "error": err.to_string() }),
                            VoiceToolDispatchStatus::Error,
                        );
                        self.append_voice_tool_exchange_history(
                            &chat_session_id,
                            &call,
                            &response.output,
                        )
                        .await;
                        Ok(response)
                    },
                    Err(_cleanup_elapsed) => {
                        warn!(
                            tool_name = %tool_name,
                            chat_turn_id = %chat_turn_id,
                            cleanup_timeout_secs = VOICE_TOOL_CANCEL_CLEANUP_TIMEOUT.as_secs(),
                            "[VOICE-ORCHESTRATOR] tool dispatch cancellation cleanup timed out"
                        );
                        let response = build_voice_tool_dispatch_response(
                            tool_name,
                            call_id,
                            json!({
                                "status": "cancelled",
                                "reason": "the tool took too long, so the backend cancelled the voice-side dispatch; cleanup did not finish before the voice timeout window closed",
                                "timeout_secs": VOICE_TOOL_DISPATCH_TIMEOUT.as_secs(),
                                "cleanup_timeout_secs": VOICE_TOOL_CANCEL_CLEANUP_TIMEOUT.as_secs()
                            }),
                            VoiceToolDispatchStatus::Ok,
                        );
                        self.append_voice_tool_exchange_history(
                            &chat_session_id,
                            &call,
                            &response.output,
                        )
                        .await;
                        Ok(response)
                    },
                }
            },
        }
    }

    /// LLM tool spec list voice should advertise to the realtime model.
    /// Resolved on every `start()` so workspace skills + dynamic
    /// personality presets refresh on each call.
    /// Delegates to `ChatService::tool_specs_for_agent` so chat and voice
    /// see the SAME surface.

    /// The agent this call is bound to, resolved once at start.
    ///
    /// Every voice path that names an agent must use this. Passing
    /// `VOICE_CHAT_AGENT_ID` instead is not a cosmetic slip: a room's session
    /// belongs to the ambassador, so naming the personal assistant makes the
    /// session-agent assertions bail (`voice_session_agent_mismatch`) and asks
    /// the wrong agent's policy for tools and authorization.
    ///
    /// Takes the lock itself, so it must NOT be called while a `state` guard is
    /// held — the paths that already hold one read `chat_agent_id` from it.
    pub async fn call_agent_id(&self) -> String {
        let guard = self.state.lock().await;
        guard
            .as_ref()
            .map(|state| state.chat_agent_id.clone())
            .unwrap_or_else(|| VOICE_CHAT_AGENT_ID.to_string())
    }

    /// Count a refusal, and only a refusal.
    ///
    /// A tool that ran and failed is not a denial, and conflating the two would
    /// make the emitted number describe nothing. `external_tool_refusal_reason`
    /// is the typed seam that separates them; an untyped error is left uncounted
    /// rather than guessed at.
    async fn record_tool_refusal_if_refused(&self, error: &anyhow::Error, tool_name: &str) {
        let Some(reason) =
            magician::magician_v2::chat::service::external_tool_refusal_reason(error)
        else {
            return;
        };
        let mut guard = self.state.lock().await;
        let Some(state) = guard.as_mut() else {
            return;
        };
        state.tool_calls_refused = state.tool_calls_refused.saturating_add(1);
        warn!(
            voice_session_id = %state.voice_session_id,
            surface = %state.source_surface,
            agent_id = %state.chat_agent_id,
            tool_name = %tool_name,
            reason = %reason,
            refused_total = state.tool_calls_refused,
            "[VOICE-ORCHESTRATOR] tool call refused on this call's boundary"
        );
    }

    /// How many tool calls this call has had refused so far.
    pub async fn tool_calls_refused(&self) -> u32 {
        let guard = self.state.lock().await;
        guard
            .as_ref()
            .map(|state| state.tool_calls_refused)
            .unwrap_or(0)
    }

    /// The boundary this call resolved to, for telemetry and the call UI.
    /// `None` before `start()`. Read from `CallState`, which is the same place
    /// authorization reads from — a diagnostic that recomputed the surface
    /// could disagree with the one actually in force, which is worse than
    /// showing nothing.
    pub async fn call_boundary(&self) -> Option<CallBoundary> {
        let guard = self.state.lock().await;
        let state = guard.as_ref()?;
        let surface =
            magician::magician_v2::chat::voice_invocation_surface(Some(&state.source_surface));
        Some(CallBoundary {
            source_surface: state.source_surface.clone(),
            surface: surface.as_str(),
            audience: surface.audience().as_str(),
            agent_id: state.chat_agent_id.clone(),
            binding: blake3::hash(state.ui_thread_id.as_bytes()).to_hex()[..16].to_string(),
        })
    }

    pub async fn tool_specs(&self) -> Vec<LLMToolSpec> {
        let snapshot = {
            let guard = self.state.lock().await;
            guard.as_ref().map(|s| {
                (
                    s.principal.clone(),
                    s.workspace.clone(),
                    s.chat_agent_id.clone(),
                )
            })
        };
        let Some((principal, workspace, call_agent_id)) = snapshot else {
            return Vec::new();
        };
        self.chat_service
            .tool_specs_for_agent(&principal, &workspace, &call_agent_id)
            .await
    }

    /// Full realtime session context — instructions + tools — assembled
    /// by ChatService so chat and voice share the SAME outer-loop
    /// system prompt (with the audio modality addendum) and the SAME
    /// tool surface. Replaces the standalone `voice_controller_instructions`
    /// + `tool_specs()` calls voice formerly made separately.
    pub async fn session_context(&self) -> Result<VoiceSessionContext, OrchestratorError> {
        let (
            principal,
            workspace,
            provider_supports_turn_context_gate,
            call_agent_id,
            call_source_surface,
            app_owner_credential,
        ) = {
            let guard = self.state.lock().await;
            let state = guard.as_ref().ok_or(OrchestratorError::NotStarted)?;
            (
                state.principal.clone(),
                state.workspace.clone(),
                provider_supports_turn_context_gate(&state.current_descriptor),
                state.chat_agent_id.clone(),
                state.source_surface.clone(),
                state.app_owner_execution_credential.clone(),
            )
        };
        let chat_session_id = self.chat_session_id_for_call().await?;
        let context_future = self.chat_service.render_voice_session_context(
            &principal,
            &workspace,
            &call_agent_id,
            &chat_session_id,
            provider_supports_turn_context_gate,
            Some(&call_source_surface),
        );
        let mut context = magician::magician_v2::chat::service::scope_app_owner_execution_credential(
            app_owner_credential.clone(),
            context_future,
        )
        .await
        .map_err(|err| OrchestratorError::Io(err.to_string()))?;
        if app_owner_credential.as_ref().is_some_and(|credential| {
            !self
                .chat_service
                .revalidate_realtime_voice_owner_credential(credential)
        }) {
            return Err(OrchestratorError::NotConfigured(
                "authenticated governed voice profile changed while rendering session context"
                    .to_string(),
            ));
        }
        let mut guard = self.state.lock().await;
        let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
        state.policy_snapshot_id = context.policy_snapshot_id.clone();
        if state.concurrent_requests {
            Self::apply_concurrent_voice_context(&mut context);
        }
        Ok(context)
    }

    pub async fn prepare_realtime_turn_context(
        &self,
        text: &str,
    ) -> Result<RealtimeTurnContext, OrchestratorError> {
        self.prepare_realtime_turn_context_with_cancellation(text, CancellationToken::new())
            .await
    }

    pub async fn prepare_realtime_turn_context_with_cancellation(
        &self,
        text: &str,
        cancellation: CancellationToken,
    ) -> Result<RealtimeTurnContext, OrchestratorError> {
        let chat_session_id = self.chat_session_id_for_call().await?;
        // The call's own surface, so a room's per-turn retrieval is scoped as a
        // room rather than as the owner.
        let (call_source_surface, ui_thread_id, chat_turn_id, app_owner_credential) = {
            let guard = self.state.lock().await;
            guard
                .as_ref()
                .map(|state| {
                    (
                        state.source_surface.clone(),
                        state.ui_thread_id.clone(),
                        state.current_chat_turn_id.clone(),
                        state.app_owner_execution_credential.clone(),
                    )
                })
                .unwrap_or_default()
        };
        let agent_id = self.call_agent_id().await;
        let context_future = self
            .chat_service
            .render_realtime_turn_context_with_cancellation(
                &chat_session_id,
                &agent_id,
                text,
                chat_turn_id.as_deref(),
                cancellation,
                Some(&call_source_surface),
            );
        let mut context =
            magician::magician_v2::chat::service::scope_app_owner_execution_credential(
                app_owner_credential,
                context_future,
            )
            .await
            .map_err(|error| OrchestratorError::Io(error.to_string()))?;
        context.meeting_context = self
            .meeting_context_for_turn(&call_source_surface, &ui_thread_id)
            .await;
        Ok(context)
    }

    /// Mint a one-shot, content-free delivery fence for the exact current
    /// backend-proxied owner turn after reopening live profile trust.
    pub async fn current_realtime_app_delivery_fence(
        &self,
    ) -> Result<Option<AppRealtimeVoiceDeliveryFence>, OrchestratorError> {
        let (credential, invocation) = {
            let guard = self.state.lock().await;
            let Some(state) = guard.as_ref() else {
                return Err(OrchestratorError::NotStarted);
            };
            let Some(credential) = state.app_owner_execution_credential.clone() else {
                return Ok(None);
            };
            let Some(chat_session_id) = state.chat_session_id.clone() else {
                return Ok(None);
            };
            let Some(chat_turn_id) = state.current_chat_turn_id.clone() else {
                return Ok(None);
            };
            (
                credential,
                magician::magician_v2::agents::AgentInvocationContext {
                    principal: state.principal.clone(),
                    workspace: state.workspace.clone(),
                    source_agent_id: None,
                    target_agent_id: state.chat_agent_id.clone(),
                    surface: magician::magician_v2::agents::InvocationSurface::RealtimeVoice,
                    feature_mode: magician::magician_v2::agents::FeatureMode::None,
                    source_kind: magician::magician_v2::agents::InvocationSourceKind::Direct,
                    chat_session_id: Some(chat_session_id),
                    chat_turn_id: Some(chat_turn_id),
                },
            )
        };
        if !self
            .chat_service
            .revalidate_realtime_voice_owner_credential(credential.as_ref())
        {
            return Ok(None);
        }
        Ok(credential.delivery_fence(&invocation, Utc::now()).ok())
    }

    /// Condensed state of the meeting this call is sitting in, for a room turn
    /// only. Deliberately narrow: the transcript is already the room's own chat
    /// history, so this adds the rolling summary and the decisions / action
    /// items parsed from it, for THIS meeting, and reaches nothing else.
    ///
    /// `None` — inject nothing, never a fallback to another source — for every
    /// owner call, when no live meeting is bound to the call's thread, when the
    /// summary parses to nothing, and when it is unchanged since the last turn.
    /// A room that gets `None` is not blind meanwhile: it still has its own
    /// transcript as session history.
    async fn meeting_context_for_turn(
        &self,
        source_surface: &str,
        ui_thread_id: &str,
    ) -> Option<String> {
        // The surface decides, not "a meeting happens to exist on this thread":
        // an owner call on a meeting-hosting thread is still an owner call.
        if magician::magician_v2::chat::voice_invocation_surface(Some(source_surface)).audience()
            != magician::magician_v2::agents::SurfaceAudience::Untrusted
        {
            return None;
        }
        let summary = crate::media_rails::meeting::meeting_manager()
            .latest_summary_for_thread(ui_thread_id)
            .await?;
        let block = crate::media_rails::meeting::render_meeting_visible_block(&summary)?;
        let digest = blake3::hash(block.as_bytes()).to_hex().to_string();
        let mut guard = self.state.lock().await;
        let state = guard.as_mut()?;
        if state.last_meeting_summary_digest.as_deref() == Some(digest.as_str()) {
            return None;
        }
        state.last_meeting_summary_digest = Some(digest);
        Some(block)
    }

    /// Advance the voice dispatch snapshot only after the provider confirms it
    /// installed the prepared catalog. Holding the call-state lock across the
    /// ChatService compare-and-swap prevents a concurrent rotation from
    /// publishing a different snapshot between validation and commit.
    pub async fn acknowledge_tool_catalog_update(
        &self,
        update: &ExternalToolCatalogUpdate,
    ) -> Result<ExternalToolCatalogUpdate, OrchestratorError> {
        let agent_for_call = self.call_agent_id().await;
        let mut guard = self.state.lock().await;
        let state = guard.as_mut().ok_or(OrchestratorError::NotStarted)?;
        if state.policy_snapshot_id.as_deref() != Some(update.previous_policy_snapshot_id.as_str())
        {
            return Err(OrchestratorError::Io(
                "voice catalog acknowledgement targeted a stale snapshot".to_string(),
            ));
        }
        let chat_session_id = state.chat_session_id.as_deref().ok_or_else(|| {
            OrchestratorError::Io(
                "voice chat session is unavailable during catalog acknowledgement".to_string(),
            )
        })?;
        let app_owner_credential = state.app_owner_execution_credential.clone();
        let acknowledge = self.chat_service.acknowledge_external_tool_catalog_update(
            chat_session_id,
            &agent_for_call,
            &update.update_id,
        );
        let committed = magician::magician_v2::chat::service::scope_app_owner_execution_credential(
            app_owner_credential.clone(),
            acknowledge,
        )
        .await
        .map_err(|error| OrchestratorError::Io(error.to_string()))?;
        if app_owner_credential.as_ref().is_some_and(|credential| {
            !self
                .chat_service
                .revalidate_realtime_voice_owner_credential(credential)
        }) {
            return Err(OrchestratorError::NotConfigured(
                "authenticated governed voice profile changed during catalog acknowledgement"
                    .to_string(),
            ));
        }
        state.policy_snapshot_id = Some(committed.policy_snapshot_id.clone());
        Ok(committed)
    }

    pub async fn abort_tool_catalog_update(&self, update: &ExternalToolCatalogUpdate) -> bool {
        let agent_for_call = self.call_agent_id().await;
        let guard = self.state.lock().await;
        let Some(state) = guard.as_ref() else {
            return false;
        };
        let Some(chat_session_id) = state.chat_session_id.as_deref() else {
            return false;
        };
        self.chat_service.abort_external_tool_catalog_update(
            chat_session_id,
            &agent_for_call,
            &update.update_id,
        )
    }

    /// Clean teardown. Idempotent — calling `end()` twice is a no-op.
    /// Aborts the proactive rotation timer, asks the provider to
    /// close the live upstream session, releases the downstream
    /// fanout entry, releases the chat session's active-run lock,
    /// and clears in-memory state.
    pub async fn end(&self) {
        let agent_for_call = self.call_agent_id().await;
        let mut guard = self.state.lock().await;
        let Some(mut state) = guard.take() else {
            return;
        };
        state.cancel_token.cancel();
        if let Some(credential) = state.app_owner_session_credential.as_ref() {
            credential.invalidate();
        }
        drop(guard);
        if let Some(handle) = state.proactive_rotation_task.take() {
            handle.abort();
        }
        // Release the live upstream BEFORE dropping the provider Arc
        // so the impl has a chance to clean up gracefully. Failures
        // are non-fatal — the call is over either way.
        if let Err(err) = state
            .provider
            .close_session(&state.current_descriptor)
            .await
        {
            warn!(
                voice_session_id = %state.voice_session_id,
                error = %err,
                "[VOICE-ORCHESTRATOR] close_session failed on end; continuing"
            );
        }
        // Release only the cancelled token owned by this voice call/takeover;
        // never clear a newer typed turn that may already own the slot. Once a
        // vendor Live sentinel is released, explicitly drain messages that
        // were queued behind the call-wide single-modality boundary.
        if let Some(chat_session_id) = state.chat_session_id.as_ref() {
            let released_voice_slot = !state.concurrent_requests
                && self.chat_service.clear_cancelled_active_chat_run(chat_session_id);
            if released_voice_slot && self.chat_service.pending_messages_depth(chat_session_id) > 0
            {
                self.chat_service
                    .spawn_pending_queue_drain(chat_session_id.clone());
            }
            self.chat_service.clear_surface_working_set(
                &state.principal,
                &state.workspace,
                &agent_for_call,
                magician::magician_v2::agents::InvocationSurface::RealtimeVoice,
                magician::magician_v2::agents::FeatureMode::None,
                chat_session_id,
            );
        }
        self.downstream_fanout.unregister(&state.voice_session_id);
        debug!(
            voice_session_id = %state.voice_session_id,
            "[VOICE-ORCHESTRATOR] call ended"
        );
    }

    /// Context-window size for the current upstream session, as
    /// declared on the realtime voice profile. `None` when no call
    /// is active or the profile doesn't pin a window. The WS actor
    /// reads this when the frontend doesn't supply a window on
    /// `token.usage` frames.
    pub async fn context_window_tokens(&self) -> Option<u64> {
        let guard = self.state.lock().await;
        guard.as_ref().and_then(|s| s.profile.context_window_tokens)
    }
}

async fn run_voice_magician_engine_turn(
    chat_service: Arc<ChatService>,
    chat_session_id: String,
    intent: String,
    chat_turn_id: String,
    voice_session_id: String,
    magician_engine_token: CancellationToken,
    on_segment: Arc<dyn Fn(crate::media_rails::SpeechSegment) + Send + Sync>,
    profile_override: Option<String>,
) -> anyhow::Result<magician::magician_v2::chat::service::DelegateForVoiceResponse> {
    let response = chat_service
        .process_voice_engine_turn_on_execution_runtime(
            &chat_session_id,
            Some(&intent),
            Some(&chat_turn_id),
            Some(&voice_session_id),
            magician_engine_token,
            profile_override,
        )
        .await?;
    if response.cancelled {
        anyhow::bail!("Magician engine turn was cancelled");
    }
    if response.queued.is_some() {
        anyhow::bail!("Magician engine turn queued behind the live voice lock");
    }
    let message = response
        .assistant_message
        .ok_or_else(|| anyhow::anyhow!("Magician engine turn returned no assistant message"))?;
    let raw = message.content.text_content().unwrap_or("").to_string();
    let parsed = magician::magician_v2::media_seam::parse_speech_segments(&raw);
    let stored_segments = message
        .speech_segments
        .filter(|segments| !segments.is_empty());
    let segments = if let Some(segments) = stored_segments {
        segments
    } else if !parsed.is_empty() {
        parsed
    } else {
        let cleaned = magician::magician_v2::media_seam::strip_speech_tag_markers(&raw);
        if cleaned.trim().is_empty() {
            Vec::new()
        } else {
            vec![crate::media_rails::SpeechSegment {
                text: cleaned.trim().to_string(),
                emotion: None,
                style: None,
                pace: None,
                voice_mode: None,
                emphasis: None,
            }]
        }
    };
    for segment in &segments {
        if !segment.text.trim().is_empty() {
            on_segment(segment.clone());
        }
    }
    let speak = segments
        .iter()
        .map(|segment| segment.text.trim())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let full = magician::magician_v2::media_seam::strip_speech_tag_markers(&raw)
        .trim()
        .to_string();
    Ok(magician::magician_v2::chat::service::DelegateForVoiceResponse { speak, full })
}

/// The typed feature surface a live-voice guided-flow takeover runs on,
/// derived from the chat lane registry (Batch 7 F11) instead of an inline
/// `FeatureMode` match. Resolve the lane owning `mode` the same way
/// lane_seam's own lookup helpers do (find by `feature_mode()`), admit it
/// only if the lane keeps the Tutor chat runtime tools — exactly the
/// Tutor and App Copilot lanes today — and answer the lane's
/// `admission_surface()` wire string. `InvocationSurface::as_str()` spells
/// those two surfaces `"tutor"` / `"app_copilot"` (in
/// `magician_v2::agents::types`), byte-identical to the literals this fold
/// replaced, so the wire is unchanged. Every other mode fails closed with the takeover's
/// historical error, verbatim: the unregistered `FeatureMode::None`, and
/// the registered Brainstorm / VibeDev lanes whose turns are not
/// tutor-runtime takeovers. A future lane joins the takeover by
/// registering with a tutor-runtime surface — no edit here.
fn voice_takeover_feature_surface(
    mode: magician::magician_v2::agents::FeatureMode,
) -> Result<&'static str, OrchestratorError> {
    magician::magician_v2::chat::lane_seam::registered_lanes()
        .iter()
        .find(|lane| lane.feature_mode() == mode && lane.keeps_tutor_runtime_tools())
        .map(|lane| lane.admission_surface().as_str())
        .ok_or_else(|| {
            OrchestratorError::Io(
                "voice tutor takeover requires a typed Tutor/App Copilot feature".to_string(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal upstream provider. `rotate()` only ever asks a provider to mint
    /// and close sessions, so nothing here needs to be realistic.
    struct StubRealtimeProvider;

    #[async_trait::async_trait]
    impl RealtimeProvider for StubRealtimeProvider {
        fn id(&self) -> &str {
            "stub-realtime"
        }
        fn kind(&self) -> RealtimeProviderKind {
            RealtimeProviderKind::OpenAi
        }
        fn default_model(&self) -> &str {
            "stub-realtime-model"
        }
        fn audio_topology(&self) -> RealtimeAudioTopology {
            RealtimeAudioTopology::BackendProxied
        }
        async fn create_session(
            &self,
            _principal: &str,
            _workspace: &str,
            _voice_session_id: &str,
            _thread_id: Option<&str>,
            _preferred_voice: Option<&str>,
        ) -> Result<RealtimeSessionDescriptor, magicllm::realtime::types::RealtimeProviderError>
        {
            Ok(test_descriptor("upstream-stub"))
        }
    }

    fn test_descriptor(upstream: &str) -> RealtimeSessionDescriptor {
        RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAi,
            model: "realtime-test".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: None,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: Some(upstream.to_string()),
            max_session_duration_secs: None,
            native_resume_handle: None,
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: None,
            context_window_tokens: None,
            half_duplex: None,
        }
    }

    fn test_profile() -> RealtimeVoiceProfile {
        RealtimeVoiceProfile {
            provider: "openai_realtime".to_string(),
            model: "realtime-test".to_string(),
            display_name: None,
            selectable: false,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            allow_without_turn_grounding: false,
            voice: None,
            max_session_duration_secs: None,
            compaction_token_watermark: None,
            base_url: None,
            fallback: Vec::new(),
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: None,
            context_window_tokens: None,
            verbatim_recent_turns: None,
            compaction_input_turn_limit: None,
            translation_target_language: None,
            translation_echo_target_language: false,
            thinking_level: None,
            tool_result_scheduling: None,
            display_order: None,
        }
    }

    /// A live room call, mid-meeting: bound to the ambassador, carrying the
    /// server-minted room surface and a summary it has already injected once.
    fn room_call_state() -> CallState {
        CallState {
            principal: "owner".to_string(),
            workspace: "default".to_string(),
            voice_session_id: "voice-1".to_string(),
            chat_agent_id: "envoy".to_string(),
            source_surface: magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE.to_string(),
            ui_thread_id: "meeting-standup-2026-08-18".to_string(),
            tool_calls_refused: 3,
            last_meeting_summary_digest: Some("digest-of-the-summary-already-sent".to_string()),
            thread_id: Some("thread-1".to_string()),
            profile: test_profile(),
            provider: Arc::new(StubRealtimeProvider),
            hands_free: false,
            turn_detection_override: None,
            rotation_count: 2,
            downstream_rx: None,
            last_input_tokens: 4_096,
            last_output_tokens: 2_048,
            session_usage: RealtimeUsage::default(),
            chat_session_id: Some("chat-1".to_string()),
            policy_snapshot_id: Some("snapshot-1".to_string()),
            proactive_rotation_task: None,
            audio_channel: None,
            current_descriptor: test_descriptor("upstream-1"),
            current_chat_turn_id: None,
            prebegun_user_turn_pending_ingest: false,
            cancel_token: CancellationToken::new(),
            chat_run_token: None,
            coding_choice: None,
            chat_choice: None,
            realtime_profile_name: None,
            preferred_voice: None,
            app_owner_session_credential: None,
            app_owner_execution_credential: None,
            governed_history_capture_disabled: false,
            concurrent_requests: false,
            concurrent_context_session_id: None,
        }
    }

    /// A rotation replaces the upstream provider session, not the call. The
    /// resolved surface, the resolved agent, the thread binding and the room's
    /// injected-summary digest are call identity: if a refactor rebuilt
    /// `CallState` here, a rotating meeting would silently become an owner
    /// session while every other test stayed green. That is the failure this
    /// pins, and it was previously defended only by a comment.
    #[test]
    fn a_rotation_replaces_the_upstream_session_and_nothing_about_the_call() {
        let mut state = room_call_state();

        let prior = state.apply_rotation(7, test_descriptor("upstream-2"));

        // What a rotation IS allowed to change.
        assert_eq!(
            prior.upstream_provider_session_id.as_deref(),
            Some("upstream-1"),
            "the displaced descriptor is returned so its upstream can be closed"
        );
        assert_eq!(
            state
                .current_descriptor
                .upstream_provider_session_id
                .as_deref(),
            Some("upstream-2")
        );
        assert_eq!(state.rotation_count, 7);
        assert_eq!(
            state.last_input_tokens, 0,
            "token watermark resets per upstream session"
        );
        assert_eq!(state.last_output_tokens, 0);

        // Call identity — untouched, or a rotating room becomes an owner call.
        assert_eq!(
            state.source_surface,
            magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE
        );
        assert_eq!(state.chat_agent_id, "envoy");
        assert_eq!(state.ui_thread_id, "meeting-standup-2026-08-18");
        assert_eq!(
            state.last_meeting_summary_digest.as_deref(),
            Some("digest-of-the-summary-already-sent"),
            "a rotated room must not be re-told the summary it already has"
        );
        assert_eq!(
            state.tool_calls_refused, 3,
            "a rotation reset the refusal count, so a room probing sealed tools \
             could clear its own record by forcing one"
        );
        assert_eq!(state.chat_session_id.as_deref(), Some("chat-1"));
    }

    /// The surface a rotated room keeps must still resolve to an untrusted
    /// audience. Pins the end of the chain, not just the field: preserving the
    /// string would be worthless if it stopped mapping to `Meeting`.
    #[test]
    fn a_rotated_room_still_resolves_to_an_untrusted_audience() {
        let mut state = room_call_state();
        state.apply_rotation(7, test_descriptor("upstream-2"));

        let surface =
            magician::magician_v2::chat::voice_invocation_surface(Some(&state.source_surface));
        assert_eq!(
            surface,
            magician::magician_v2::agents::InvocationSurface::Meeting
        );
        assert_eq!(
            surface.audience(),
            magician::magician_v2::agents::SurfaceAudience::Untrusted
        );
    }

    #[test]
    fn agent_surface_runtime_realtime_context_gate_is_openai_only() {
        let descriptor = |provider| RealtimeSessionDescriptor {
            provider,
            model: "realtime-test".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: None,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: None,
            max_session_duration_secs: None,
            native_resume_handle: None,
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: None,
            context_window_tokens: None,
            half_duplex: None,
        };

        assert!(provider_supports_turn_context_gate(&descriptor(
            RealtimeProviderKind::OpenAi
        )));
        assert!(!provider_supports_turn_context_gate(&descriptor(
            RealtimeProviderKind::OpenAiLive
        )));
        assert!(!provider_supports_turn_context_gate(&descriptor(
            RealtimeProviderKind::Gemini
        )));
        assert!(!provider_supports_turn_context_gate(&descriptor(
            RealtimeProviderKind::HandsFree
        )));

        let profile = |provider: &str| RealtimeVoiceProfile {
            provider: provider.to_string(),
            model: "test".to_string(),
            display_name: None,
            selectable: false,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            allow_without_turn_grounding: false,
            voice: None,
            max_session_duration_secs: None,
            compaction_token_watermark: None,
            base_url: None,
            fallback: Vec::new(),
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: None,
            context_window_tokens: None,
            verbatim_recent_turns: None,
            compaction_input_turn_limit: None,
            translation_target_language: None,
            translation_echo_target_language: false,
            thinking_level: None,
            tool_result_scheduling: None,
            display_order: None,
        };
        assert!(profile_supports_mandatory_turn_grounding(&profile(
            "openai_realtime"
        )));
        assert!(profile_supports_mandatory_turn_grounding(&profile(
            "openai_realtime_backend"
        )));
        assert!(!profile_supports_mandatory_turn_grounding(&profile(
            "gemini_live"
        )));
        let mut opted_in_gemini = profile("gemini_live");
        opted_in_gemini.allow_without_turn_grounding = true;
        assert!(profile_supports_mandatory_turn_grounding(&opted_in_gemini));
    }

    #[test]
    fn native_turn_boundary_override_survives_profile_default() {
        let mut descriptor = RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAi,
            model: "gpt-realtime-test".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: None,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: None,
            max_session_duration_secs: None,
            native_resume_handle: None,
            transcription_model: Some("local".to_string()),
            transcription_fallback_model: Some("whisper-1".to_string()),
            turn_detection_mode: Some("none".to_string()),
            context_window_tokens: Some(128_000),
            half_duplex: None,
        };

        apply_turn_detection_override(&mut descriptor, false, Some("server_vad"));
        assert_eq!(
            descriptor.turn_detection_mode.as_deref(),
            Some("server_vad")
        );

        apply_turn_detection_override(&mut descriptor, false, Some("none"));
        assert_eq!(descriptor.turn_detection_mode.as_deref(), Some("none"));
    }

    #[test]
    fn cascaded_provider_keeps_its_own_turn_detection() {
        let mut descriptor = RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::HandsFree,
            model: "hands-free-local-fluid-v1".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: None,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: None,
            max_session_duration_secs: None,
            native_resume_handle: None,
            transcription_model: None,
            transcription_fallback_model: None,
            turn_detection_mode: Some("server_vad".to_string()),
            context_window_tokens: None,
            half_duplex: None,
        };

        apply_turn_detection_override(&mut descriptor, true, Some("none"));
        assert_eq!(
            descriptor.turn_detection_mode.as_deref(),
            Some("server_vad")
        );
    }

    #[test]
    fn unprojected_platform_result_fails_closed_without_raw_prefix_or_false_reference() {
        let raw = json!({
            "status": "ok",
            "summary": "Fetched the matching task records.",
            "rows": ["x".repeat(5_000)],
        });

        let response = build_voice_tool_dispatch_response(
            "read_trace".to_string(),
            "call-1".to_string(),
            raw,
            VoiceToolDispatchStatus::Ok,
        );
        let envelope: Value = serde_json::from_str(&response.output).unwrap();

        assert_eq!(response.voice_summary, "Fetched the matching task records.");
        assert_eq!(
            envelope["voice_summary"],
            "Fetched the matching task records."
        );
        assert_eq!(envelope["speak"]["mode"], "summary_only");
        assert_eq!(
            envelope["speak"]["text"],
            "Fetched the matching task records."
        );
        assert!(envelope["full_result"].is_null());
        assert_eq!(envelope["result"]["status"], "ok");
        assert_eq!(envelope["result"]["projection"]["available"], false);
        assert_eq!(
            envelope["result"]["projection"]["raw_result_included"],
            false
        );
        assert!(envelope["result"].get("rows").is_none());
    }

    #[test]
    fn voice_tool_output_summarizes_errors_without_raw_blob_narration() {
        let response = build_voice_tool_dispatch_response(
            "metabase_query".to_string(),
            "call-err".to_string(),
            json!({
                "error": "database rejected the SQL: missing column customer_id",
                "debug": "y".repeat(3_000),
            }),
            VoiceToolDispatchStatus::Error,
        );
        let envelope: Value = serde_json::from_str(&response.output).unwrap();

        assert_eq!(envelope["status"], "error");
        assert!(response.voice_summary.contains("database rejected the SQL"));
        assert_eq!(envelope["speak"]["text"], response.voice_summary);
        assert!(envelope["full_result"].is_null());
        assert_eq!(envelope["result"]["projection"]["available"], false);
        assert!(envelope["result"].get("debug").is_none());
    }

    #[test]
    fn delivered_tool_failure_is_not_mislabeled_as_successful_dispatch() {
        let response = build_voice_tool_dispatch_response(
            "browser_action".to_string(),
            "call-failed-value".to_string(),
            json!({
                "success": false,
                "error": "the requested element disappeared",
            }),
            VoiceToolDispatchStatus::Ok,
        );
        let envelope: Value = serde_json::from_str(&response.output).unwrap();

        assert!(matches!(response.status, VoiceToolDispatchStatus::Error));
        assert_eq!(envelope["status"], "error");
        assert!(response.voice_summary.starts_with("Tool failed:"));
    }

    #[test]
    fn voice_tool_output_never_narrates_arbitrary_unstructured_json_prefix() {
        let response = build_voice_tool_dispatch_response(
            "opaque_provider".to_string(),
            "call-unstructured".to_string(),
            json!({
                "private_blob": "never narrate this value",
                "debug": "z".repeat(3_000),
            }),
            VoiceToolDispatchStatus::Error,
        );

        assert_eq!(
            response.voice_summary,
            "Tool failed: the tool returned an unstructured error"
        );
        assert!(!response.voice_summary.contains("never narrate"));
    }

    #[test]
    fn voice_tool_output_uses_queued_reason_as_speakable_summary() {
        let response = build_voice_tool_dispatch_response(
            "delegate_to_chat".to_string(),
            "call-queued".to_string(),
            json!({
                "status": "queued",
                "reason": "the deeper reasoner is still working",
            }),
            VoiceToolDispatchStatus::Ok,
        );
        let envelope: Value = serde_json::from_str(&response.output).unwrap();

        assert_eq!(envelope["status"], "queued");
        assert_eq!(
            response.voice_summary,
            "Queued: the deeper reasoner is still working"
        );
        assert_eq!(envelope["speak"]["mode"], "summary_only");
    }

    /// Sentence-boundary truncation prefers a clean end-of-sentence
    /// over a mid-word cut. Pins the fix for the prior bug where the
    /// realtime model would read aloud a fragment like
    /// "...attached project-status.pdf; the all-hands i".
    #[test]
    fn truncate_text_prefers_sentence_boundary_over_mid_word_cut() {
        let text = "First sentence here. Second sentence is longer and contains more detail. \
                    Third sentence runs even further past the cap.";
        // Budget straddles the second sentence's terminal `.`.
        let (truncated, did_truncate) = truncate_text(text, 60);
        assert!(did_truncate, "long input must report truncation");
        assert!(
            truncated.ends_with("."),
            "must end at a sentence boundary, got: {truncated:?}"
        );
        assert!(
            !truncated.contains("…"),
            "sentence-boundary cut shouldn't append an ellipsis: {truncated:?}"
        );
        // Should land on the end of the first sentence (the only one
        // that fits cleanly in the budget).
        assert_eq!(truncated, "First sentence here.");
    }

    /// When no sentence boundary fits in the budget (a long
    /// un-punctuated string), truncation falls back to a clean
    /// char-boundary cut + ellipsis. The listener hears a trailing
    /// "…" which the voice model speaks as a natural "and so on"
    /// trail-off rather than abrupt silence mid-word.
    #[test]
    fn truncate_text_falls_back_to_ellipsis_when_no_sentence_boundary() {
        let text = "a very long string with no sentence ending punctuation that just keeps going \
                    and going past the cap with nothing to break on";
        let (truncated, did_truncate) = truncate_text(text, 40);
        assert!(did_truncate);
        assert!(
            truncated.ends_with("…"),
            "fallback path appends ellipsis: {truncated:?}"
        );
        // +1 for the trailing ellipsis character.
        assert!(truncated.chars().count() <= 41);
    }

    /// Short inputs pass through untouched — no truncation flag, no
    /// ellipsis, no surprises.
    #[test]
    fn truncate_text_passes_short_inputs_through_untouched() {
        let (truncated, did_truncate) = truncate_text("short", 100);
        assert_eq!(truncated, "short");
        assert!(!did_truncate);
    }

    /// The voice envelope speak-summary path picks up `voice_summary`
    /// before any other field. This pins the contract that
    /// `delegate_to_chat` relies on: the orchestrator sets
    /// `voice_summary` to the `<speech>`-extracted text from
    /// `delegate_for_voice`, and the model speaks it verbatim
    /// without mid-sentence truncation.
    #[test]
    fn delegate_to_chat_shaped_payload_uses_speech_extract_as_voice_summary() {
        // Simulate what voice_orchestrator's delegate_to_chat handler
        // produces after delegate_for_voice returns its (speak, full)
        // pair: `voice_summary` carries the speech-extract, `answer`
        // carries the full ledger-bound text.
        let response = build_voice_tool_dispatch_response(
            "delegate_to_chat".to_string(),
            "call-d2c".to_string(),
            json!({
                "status": "ok",
                "voice_summary": "Three meetings today — Alice at 10, Bob at 11:30, all-hands at 2.",
                "answer": "**Meetings today:**\n- 10:00 Alice (1:1 sync)\n- 11:30 Bob \
                           (project review, attached: project-status.pdf)\n\
                           - 14:00 All-hands (recurring)",
            }),
            VoiceToolDispatchStatus::Ok,
        );
        let envelope: Value = serde_json::from_str(&response.output).unwrap();

        // The voice_summary IS the speech-extract — no truncation,
        // no mid-sentence cut, no ellipsis. The realtime model
        // speaks exactly this string.
        assert_eq!(
            response.voice_summary,
            "Three meetings today — Alice at 10, Bob at 11:30, all-hands at 2."
        );
        assert_eq!(envelope["speak"]["text"], response.voice_summary);
        assert_eq!(envelope["speak"]["mode"], "summary_only");
        // Provider-facing output is bounded to the model projection and the
        // authored speech summary. The complete answer is retained only in
        // the local projected-result record used by UI/history consumers.
        assert!(envelope["full_result"].is_null());
        assert!(envelope["result"].get("answer").is_none());
    }

    /// Batch 7 F11: the voice takeover surface derives from the chat lane
    /// registry. Tutor and App Copilot answer their lanes'
    /// `admission_surface().as_str()` — `"tutor"` / `"app_copilot"`,
    /// byte-identical to the pre-fold inline literals — so the wire is
    /// unchanged by the fold.
    #[test]
    fn voice_takeover_surface_derives_from_the_lane_registry() {
        use magician::magician_v2::agents::FeatureMode;
        assert_eq!(
            voice_takeover_feature_surface(FeatureMode::Tutor).ok(),
            Some("tutor")
        );
        assert_eq!(
            voice_takeover_feature_surface(FeatureMode::AppCopilot).ok(),
            Some("app_copilot")
        );
    }

    /// Batch 7 F11: every mode that is not a tutor-runtime lane fails closed
    /// with the takeover's exact historical error — same variant, same
    /// message. That covers the unregistered `None` and the registered
    /// Brainstorm / VibeDev lanes, whose surfaces are not takeover surfaces.
    #[test]
    fn voice_takeover_surface_fails_closed_with_the_typed_feature_error() {
        use magician::magician_v2::agents::FeatureMode;
        for mode in [
            FeatureMode::None,
            FeatureMode::Brainstorm,
            FeatureMode::Vibedev,
        ] {
            match voice_takeover_feature_surface(mode) {
                Err(OrchestratorError::Io(message)) => assert_eq!(
                    message, "voice tutor takeover requires a typed Tutor/App Copilot feature",
                    "{mode:?} keeps the exact pre-fold error"
                ),
                other => panic!("{mode:?} must fail closed, got {other:?}"),
            }
        }
    }
}

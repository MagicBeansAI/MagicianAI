//! Voice control WebSocket — the single bidirectional channel
//! between frontend `VoiceClient` and backend `VoiceOrchestrator`.
//!
//! Wire protocol (JSON over text frames):
//!
//! Frontend → backend
//! ```text
//! session.start          { ui_thread_id, thread_id? }
//! ptt.engage             { }
//! ptt.release            { }
//! user.text              { text }
//! transcript.user        { text }     // direct_p2p only
//! transcript.assistant   { text, response_id? } // direct_p2p only
//! tool.dispatch          { response_id?, tool_name, arguments_json, call_id }
//! tool.catalog.ack       { update_id }
//! token.usage            { response_id?, input_tokens?, output_tokens?, context_window_tokens? }
//! response.failed        { response_id?, terminal_state: "failed" | "cancelled" | "incomplete" }
//! session.rotate         { reason }   // manual / diagnostic
//! session.turn_boundary  { turn_boundary } // server_vad / push_to_talk
//! session.end            { }
//! ```
//!
//! Backend → frontend
//! ```text
//! session.ready          { voice_session_id, descriptor, instructions, tools, resume, rotation_count, addressing }
//! session.rotating       { reason }
//! audio.rebind           { descriptor, resume, rotation_count }
//! transcript.user.partial { text, item_id, turn_generation }
//! transcript.user        { text, item_id, turn_generation }
//! transcript.user.cleared { item_id?, turn_generation?, reason, had_partial? }
//! transcript.user.ignored { item_id?, turn_generation?, reason, activation_phrases, follow_up_window_ms }
//! transcript.assistant.delta { text, response_id? }
//! transcript.assistant   { text, response_id? }
//! task.completed         { task_id, title, status, summary }
//! delegate_to_chat.chunk { call_id, sequence, text }
//! delegate_to_chat.done  { call_id, chunk_count, success, error? }
//! tool.result            { call_id, output, voice_summary, status }
//! tool.catalog.update    { update_id, policy_snapshot_id, tools }
//! session.error          { message, recoverable }
//! response.failed        { response_id?, terminal_state: "failed" | "cancelled" | "incomplete" }
//! session.ended          { }
//! ```
//!
//! Audio follows the selected topology. Direct browser WebRTC bypasses this
//! actor, while backend-proxied Live Call sends PCM binary frames through it so
//! the same bytes can feed realtime inference and local user transcription.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    sync::Arc,
};

use actix::prelude::*;
use actix_web::{web, HttpRequest, HttpResponse, Result as ActixResult};
use actix_web_actors::ws;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio_stream::wrappers::UnboundedReceiverStream;
use tracing::{debug, error, info, warn};

use magicllm::realtime::{
    RealtimeAudioControl, RealtimeAudioTopology, RealtimeProviderEvent, RealtimeProviderKind,
};
use magicllm::types::LLMToolSpec;

use crate::is_expected_websocket_disconnect_message;
// Media UX seam (plan 3.5): the voice session-control policy moved to
// `crate::media_ux::session_controls`; re-exported here so every call site
// keeps compiling unedited.
pub use crate::media_ux::session_controls::{
    descriptor_requests_local_transcript, descriptor_uses_manual_turns, guided_voice_capabilities,
    is_magios_voice_client, locked_guided_flow_message, requested_realtime_turn_detection,
    requested_screen_locked, requested_voice_prefix_override, unsupported_guided_flow_message,
    voice_source_surface, voice_surface_derivation, GuidedVoiceCapabilities,
    VoiceSurfaceDerivation,
};
use crate::scope::{resolve_optional_principal, resolve_required_workspace};
use crate::websocket_handler::validate_origin;
use magician::magician_v2::agents::definition_store::AgentDefinitionStore;
use magician::magician_v2::artifact_v2::ArtifactV2Service;
use magician::magician_v2::chat::models::ChatMessageDirection;
use magician::magician_v2::chat::service::{
    ChatService, ExternalToolCatalogUpdate, RealtimeTurnContext,
};
use magician::magician_v2::chat::storage::ChatStore;
use magician::magician_v2::execution::runtime_boundary::spawn_execution_job;
use magician::magician_v2::prompts::{constants as prompt_constants, PromptManager};
use magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter;
use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
use magician_media::media_rails::voice_orchestrator::provider_supports_turn_context_gate;
use magician_media::media_rails::{
    start_local_realtime_transcript, voice_downstream_fanout::VoiceDownstreamMessage,
    voice_tool_dispatcher::DEFAULT_VOICE_THREAD, AudioSurface, CascadedHandsFreeProvider,
    CascadedVoiceTurnOutcome, LocalRealtimeTranscriptCommandError, LocalRealtimeTranscriptEvent,
    LocalRealtimeTranscriptHandle, LocalRealtimeTranscriptQueueSnapshot, ResumeContext,
    RotateReason, SelfEchoSuppressor, VoiceAddressing, VoiceAddressingDecision,
    VoiceContextCompactor, VoiceOrchestrator, VoiceSessionLifecycleStore, MEDIA_SYSTEM_AGENT,
    MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK, MEDIA_VOICE_LOCAL_TRANSCRIPT_QUEUE,
    MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE, MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN,
    MEDIA_VOICE_SURFACE_RESOLVED,
};

use crate::media_api::{authenticated_app_scope_for_media_request, MediaApi};

/// Stable application protocol selected from browser voice-control upgrades.
/// The bearer is an auth-only offer consumed by middleware and is never
/// reflected to the client as the selected protocol.
const VOICE_CONTROL_WEBSOCKET_PROTOCOLS: &[&str] = &["magician-voice-control-v1"];
const PENDING_PROVIDER_AUDIO_MAX_BYTES: usize = 1024 * 1024;
const PENDING_PROVIDER_FRAME_MAX_COUNT: usize = 1024;
const PENDING_PROVIDER_TRIM_LOG_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
/// OpenAI Realtime rejects `input_audio_buffer.commit` with < 100ms of audio
/// ("buffer too small. Expected at least 100ms"). 100ms of 24kHz mono PCM16 =
/// 24000 * 0.1 * 2 bytes. Used only for diagnostic logging of the commit-empty
/// condition (we log rather than block, so a real turn is never silently dropped).
const MIN_PROVIDER_COMMIT_AUDIO_BYTES: usize = 4800;
const TOOL_CATALOG_ACK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const COMPLETED_TUTOR_TAKEOVER_DEDUPE_MAX: usize = 64;

enum PendingProviderFrame {
    Audio(Vec<u8>),
    Control(RealtimeAudioControl),
}

struct PendingVoiceCatalogUpdate {
    update: ExternalToolCatalogUpdate,
    tool_name: String,
    call_id: String,
    output: String,
    voice_summary: String,
    status: String,
    previous_tools: Vec<LLMToolSpec>,
    awaiting_rotation: bool,
    acknowledgement_in_flight: bool,
    projected_result: Option<magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
    provider_delivery_fence:
        Option<magician::magician_v2::apps::boundary::AppRealtimeVoiceDeliveryFence>,
}

impl PendingProviderFrame {
    fn audio_len(&self) -> usize {
        match self {
            Self::Audio(frame) => frame.len(),
            Self::Control(_) => 0,
        }
    }
}

fn should_emit_pending_provider_trim_warning(
    last_emitted_at: &mut Option<std::time::Instant>,
    now: std::time::Instant,
) -> bool {
    if last_emitted_at.is_some_and(|last| {
        now.saturating_duration_since(last) < PENDING_PROVIDER_TRIM_LOG_INTERVAL
    }) {
        return false;
    }
    *last_emitted_at = Some(now);
    true
}

fn spawn_provider_control_dispatcher(
    tx: tokio::sync::mpsc::Sender<RealtimeAudioControl>,
) -> tokio::sync::mpsc::UnboundedSender<RealtimeAudioControl> {
    let (dispatch_tx, mut dispatch_rx) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        while let Some(control) = dispatch_rx.recv().await {
            if tx.send(control).await.is_err() {
                break;
            }
        }
    });
    dispatch_tx
}

#[derive(Debug, Deserialize)]
pub struct VoiceControlQuery {}

/// Scope headers were engraved by the bearer middleware. Browser WebSockets
/// present that bearer through the dedicated auth-only subprotocol.
fn voice_control_principal(req: &HttpRequest) -> Result<String, HttpResponse> {
    resolve_optional_principal(req.headers()).ok_or_else(|| {
        HttpResponse::BadRequest().json(json!({
            "error": "missing_scope",
            "message": "A scoped bearer token is required."
        }))
    })
}

/// Top-level dispatch for the new control WS. Accepts the same
/// path shape (`/media/voice/{voice_session_id}/control`) every
/// future provider plugs into.
pub async fn voice_control_ws_handler(
    req: HttpRequest,
    stream: web::Payload,
    path: web::Path<String>,
    _query: web::Query<VoiceControlQuery>,
    media_api: web::Data<Arc<MediaApi>>,
    chat_store: web::Data<Arc<dyn ChatStore>>,
    chat_service: web::Data<Arc<ChatService>>,
    artifact_service: web::Data<Arc<ArtifactV2Service>>,
    compactor: web::Data<Arc<VoiceContextCompactor>>,
    operation_router: web::Data<OperationLlmRouter>,
    broadcaster: web::Data<Arc<RuntimeTransportBroadcaster>>,
    prompt_manager: web::Data<Arc<PromptManager>>,
    definition_store: Option<web::Data<Arc<AgentDefinitionStore>>>,
) -> ActixResult<HttpResponse> {
    if let Err((origin, host)) = validate_origin(&req) {
        warn!(
            origin = %origin,
            host = %host,
            "[VOICE-CONTROL] WebSocket Origin mismatch — rejecting"
        );
        return Ok(HttpResponse::Forbidden().json(json!({
            "error": "origin_not_allowed",
        })));
    }
    let voice_session_id = path.into_inner();
    let principal = match voice_control_principal(&req) {
        Ok(principal) => principal,
        Err(response) => return Ok(response),
    };
    let workspace = match resolve_required_workspace(req.headers(), None) {
        Ok(workspace) => workspace,
        Err(response) => return Ok(response),
    };

    let Some(media_session) = media_api.registry().get(&voice_session_id) else {
        return Ok(HttpResponse::NotFound().json(json!({
            "error": "media_session_not_found",
            "session_id": voice_session_id,
        })));
    };
    if media_session.principal != principal || media_session.workspace != workspace {
        return Ok(HttpResponse::Forbidden().json(json!({
            "error": "media_session_scope_mismatch",
        })));
    }
    let authenticated_scope =
        match authenticated_app_scope_for_media_request(&req, &principal, &workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    let app_owner_session_credential = authenticated_scope.as_ref().and_then(|authenticated| {
        media_api.take_voice_owner_credential(&voice_session_id, authenticated, chrono::Utc::now())
    });

    let hands_free_audio_profile = media_session
        .resolved_audio_profile
        .as_ref()
        .filter(|profile| profile.surface == AudioSurface::HandsFree)
        .cloned();

    let downstream_fanout = media_api.downstream_fanout();
    let lifecycle_store: Arc<VoiceSessionLifecycleStore> = media_api.lifecycle_store();
    let preferences_store = media_api.preferences();
    let audio_runtime = media_api.audio_runtime();
    let (require_voice_prefix, realtime_voices) = match preferences_store
        .load(&principal, &workspace, audio_runtime.as_ref())
        .await
    {
        Ok(preferences) => (
            preferences.require_voice_prefix,
            preferences.realtime_voices,
        ),
        Err(error) => {
            warn!(
                principal = %principal,
                workspace = %workspace,
                error = %error,
                "[VOICE-CONTROL] failed to load prefix preference; using the safe default"
            );
            (true, std::collections::BTreeMap::new())
        },
    };
    let (activation_names, assistant_identity) = match definition_store {
        Some(store) => match store.get_primary_agent().await {
            Ok(Some(record)) => {
                let identity =
                    magician::magician_v2::presentation_identity::AgentPresentationIdentity::from_definition(
                        &record.definition,
                    );
                let activation_names = record
                    .definition
                    .aliases
                    .iter()
                    .cloned()
                    .chain(std::iter::once(record.definition.name.clone()))
                    // A constrained on-device spotter may recognise the
                    // configured pronunciation spelling rather than the
                    // canonical coined name. Admission must accept every
                    // phrase a trusted client can arm, or a successful local
                    // wake would be discarded here as unaddressed.
                    .chain(record.definition.wake_spellings.iter().cloned())
                    .collect();
                (activation_names, identity)
            },
            Ok(None) => (
                Vec::new(),
                magician::magician_v2::presentation_identity::AgentPresentationIdentity::fallback(),
            ),
            Err(error) => {
                warn!(error = %error, "[VOICE-CONTROL] failed to resolve primary-agent voice aliases");
                (
                    Vec::new(),
                    magician::magician_v2::presentation_identity::AgentPresentationIdentity::fallback(
                    ),
                )
            },
        },
        None => (
            Vec::new(),
            magician::magician_v2::presentation_identity::AgentPresentationIdentity::fallback(),
        ),
    };
    let addressing = VoiceAddressing::new(require_voice_prefix, activation_names);
    if require_voice_prefix && !addressing.required() {
        warn!(
            principal = %principal,
            workspace = %workspace,
            "[VOICE-CONTROL] prefix control requested but no primary-agent name or alias is available; leaving this call ungated"
        );
    }

    let hands_free_provider: Arc<dyn magicllm::realtime::RealtimeProvider> = Arc::new(
        CascadedHandsFreeProvider::new(
            media_api.audio_runtime(),
            media_api.providers(),
            media_api.preferences(),
            broadcaster.get_ref().clone(),
        )
        .with_captured_audio_profile(hands_free_audio_profile),
    );
    let orchestrator = Arc::new(
        VoiceOrchestrator::new(
            chat_store.get_ref().clone(),
            chat_service.get_ref().clone(),
            artifact_service.get_ref().clone(),
            operation_router.get_ref().clone(),
            compactor.get_ref().clone(),
            lifecycle_store,
            downstream_fanout,
            broadcaster.get_ref().clone(),
        )
        .with_hands_free_provider(hands_free_provider)
        .with_app_owner_session_credential(app_owner_session_credential),
    );

    let guided_voice_capabilities = guided_voice_capabilities(
        media_session.surface_type,
        media_session.user_agent.as_deref(),
    );
    // Surface and the reason it was chosen, minted together from the registered
    // session — never from anything the client says on the wire afterwards.
    let derivation = voice_surface_derivation(media_session.surface_type);
    let actor = VoiceControlSession {
        voice_session_id,
        principal,
        workspace,
        source_surface: derivation.source_surface.to_string(),
        surface_derivation_reason: derivation.reason,
        guided_voice_capabilities,
        screen_locked: false,
        thread_id_hint: media_session.thread_id.clone(),
        orchestrator,
        broadcaster: broadcaster.get_ref().clone(),
        prompt_manager: prompt_manager.get_ref().clone(),
        started: false,
        upstream_audio_tx: None,
        provider_control_tx: None,
        provider_control_dispatch_tx: None,
        provider_transport_ready: false,
        provider_configured: false,
        provider_configuring: false,
        provider_config_generation: 0,
        local_fallback_update_id: None,
        rotation_in_progress: false,
        pending_provider_frames: VecDeque::new(),
        pending_provider_audio_bytes: 0,
        pending_provider_dropped_frames: 0,
        pending_provider_dropped_audio_bytes: 0,
        pending_provider_last_trim_log_at: None,
        provider_turn_audio_bytes: 0,
        logged_first_upstream_audio: false,
        session_instructions: None,
        session_tools: Vec::new(),
        audio_topology: None,
        realtime_provider: None,
        pending_catalog_update: None,
        per_turn_context_configured: false,
        per_turn_context_enabled: false,
        per_turn_context_budget_ms: 0,
        turn_context_generation: 0,
        turn_context_pending: false,
        turn_context_cancellation: None,
        owner_turn_cancellation: None,
        active_tutor_takeover_keys: HashSet::new(),
        active_tutor_takeover_cancellations: HashMap::new(),
        lock_cancelled_tutor_takeover_keys: HashSet::new(),
        completed_tutor_takeover_keys: VecDeque::new(),
        locked_tutor_rejection_keys: HashSet::new(),
        tutor_response_suppression: TutorResponseSuppression::default(),
        suppress_ambient_assistant_transcript: false,
        turn_started_at: None,
        turn_ttfa_ms: None,
        current_llm_correlation: None,
        current_llm_started_at_ms: None,
        hands_free: false,
        concurrent_requests: false,
        selected_voice_context: None,
        captured_voice_context: None,
        half_duplex: false,
        realtime_voices,
        cascaded_turn_generation: 0,
        cascaded_turn_active: false,
        current_provider_response_id: None,
        assistant_audio_response_id: None,
        local_transcript: LocalTranscriptController::default(),
        addressing,
        assistant_identity,
        echo_suppressor: SelfEchoSuppressor::default(),
    };
    ws::WsResponseBuilder::new(actor, &req, stream)
        .protocols(VOICE_CONTROL_WEBSOCKET_PROTOCOLS)
        .start()
}

fn apply_screen_lock_state(
    screen_locked: &mut bool,
    rejection_keys: &mut HashSet<String>,
    locked: bool,
) {
    let was_locked = *screen_locked;
    *screen_locked = locked;
    if was_locked && !locked {
        rejection_keys.clear();
    }
}

fn cancel_active_tutor_takeovers_for_lock(
    active: &HashMap<
        String,
        (
            tokio_util::sync::CancellationToken,
            magician::magician_v2::agents::FeatureMode,
        ),
    >,
    lock_cancelled_keys: &mut HashSet<String>,
    rejection_keys: &mut HashSet<String>,
) -> Option<magician::magician_v2::agents::FeatureMode> {
    let mut cancelled_feature = None;
    for (key, (cancellation, feature_mode)) in active {
        cancellation.cancel();
        lock_cancelled_keys.insert(key.clone());
        rejection_keys.insert(key.clone());
        cancelled_feature.get_or_insert(*feature_mode);
    }
    cancelled_feature
}

fn admit_locked_guided_flow_rejection(rejection_keys: &mut HashSet<String>, key: String) -> bool {
    if rejection_keys.len() >= COMPLETED_TUTOR_TAKEOVER_DEDUPE_MAX {
        rejection_keys.clear();
    }
    rejection_keys.insert(key)
}

enum LocalTranscriptState {
    NotRequested,
    Starting,
    Active(LocalRealtimeTranscriptHandle),
    Degrading,
    VendorFallback { reason: String },
    Ended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LocalTranscriptCommitMode {
    SendProviderCommit,
    ProviderAlreadyCommitted,
}

impl LocalTranscriptCommitMode {
    fn requires_provider_commit(self) -> bool {
        self == Self::SendProviderCommit
    }
}

struct LocalTranscriptController {
    state: LocalTranscriptState,
    turn_identities: HashMap<u64, (String, u64)>,
    turn_generation: u64,
    vendor_fallback_model: Option<String>,
    turn_timings: HashMap<u64, LocalTranscriptTurnTiming>,
    local_active_since: Option<std::time::Instant>,
    vendor_active_since: Option<std::time::Instant>,
    local_covered_ms: u64,
    vendor_fallback_ms: u64,
    queue_snapshot: LocalRealtimeTranscriptQueueSnapshot,
    manual_turns: bool,
    staged_final_segments: HashMap<u64, Vec<String>>,
    pending_commits: HashMap<u64, LocalTranscriptCommitMode>,
    retired_through_generation: u64,
}

#[derive(Default)]
struct LocalTranscriptTurnTiming {
    first_audio_at: Option<std::time::Instant>,
    first_partial_at: Option<std::time::Instant>,
    committed_at: Option<std::time::Instant>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalTranscriptTurnCompletion {
    commit_to_final_latency_ms: Option<u64>,
    had_partial: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LocalTranscriptTerminalSnapshot {
    local_covered_ms: u64,
    vendor_fallback_ms: u64,
    queue: LocalRealtimeTranscriptQueueSnapshot,
}

impl Default for LocalTranscriptController {
    fn default() -> Self {
        Self {
            state: LocalTranscriptState::NotRequested,
            turn_identities: HashMap::new(),
            turn_generation: 0,
            vendor_fallback_model: None,
            turn_timings: HashMap::new(),
            local_active_since: None,
            vendor_active_since: None,
            local_covered_ms: 0,
            vendor_fallback_ms: 0,
            queue_snapshot: LocalRealtimeTranscriptQueueSnapshot::default(),
            manual_turns: false,
            staged_final_segments: HashMap::new(),
            pending_commits: HashMap::new(),
            retired_through_generation: 0,
        }
    }
}

impl LocalTranscriptController {
    fn begin_start(&mut self, vendor_fallback_model: Option<String>, manual_turns: bool) {
        self.vendor_fallback_model = vendor_fallback_model
            .map(|model| model.trim().to_string())
            .filter(|model| !model.is_empty());
        self.state = LocalTranscriptState::Starting;
        self.manual_turns = manual_turns;
    }

    fn start_failed(&mut self, reason: String) {
        if matches!(self.state, LocalTranscriptState::Starting) {
            self.state = LocalTranscriptState::VendorFallback { reason };
        }
    }

    fn configure_requested(
        &mut self,
        requested: bool,
        vendor_fallback_model: Option<String>,
        manual_turns: bool,
    ) -> Vec<(String, u64)> {
        if requested {
            self.manual_turns = manual_turns;
            self.vendor_fallback_model = vendor_fallback_model
                .map(|model| model.trim().to_string())
                .filter(|model| !model.is_empty());
            if matches!(
                self.state,
                LocalTranscriptState::NotRequested | LocalTranscriptState::Starting
            ) {
                self.state = LocalTranscriptState::VendorFallback {
                    reason: "local_start_unavailable".to_string(),
                };
            }
            Vec::new()
        } else {
            self.stop_local_coverage();
            self.stop_vendor_coverage();
            self.vendor_fallback_model = None;
            self.manual_turns = false;
            let previous = std::mem::replace(&mut self.state, LocalTranscriptState::NotRequested);
            if let LocalTranscriptState::Active(handle) = previous {
                let _ = handle.try_end();
            }
            self.take_identities()
        }
    }

    fn attach(&mut self, handle: LocalRealtimeTranscriptHandle) {
        if matches!(self.state, LocalTranscriptState::Ended) {
            let _ = handle.try_end();
            return;
        }
        self.stop_vendor_coverage();
        self.local_active_since = Some(std::time::Instant::now());
        let previous = std::mem::replace(&mut self.state, LocalTranscriptState::Active(handle));
        if let LocalTranscriptState::Active(previous) = previous {
            let _ = previous.try_end();
        }
    }

    fn active_handle(&self) -> Option<LocalRealtimeTranscriptHandle> {
        match &self.state {
            LocalTranscriptState::Active(handle) => Some(handle.clone()),
            _ => None,
        }
    }

    fn is_active(&self) -> bool {
        matches!(self.state, LocalTranscriptState::Active(_))
    }

    fn is_requested(&self) -> bool {
        matches!(
            self.state,
            LocalTranscriptState::Starting
                | LocalTranscriptState::Active(_)
                | LocalTranscriptState::Degrading
                | LocalTranscriptState::VendorFallback { .. }
        )
    }

    fn mark_vendor_fallback_active(&mut self) {
        self.stop_local_coverage();
        if self.vendor_active_since.is_none() {
            self.vendor_active_since = Some(std::time::Instant::now());
        }
    }

    fn record_queue_snapshot(&mut self, snapshot: LocalRealtimeTranscriptQueueSnapshot) {
        self.queue_snapshot = snapshot;
    }

    fn stop_local_coverage(&mut self) {
        if let Some(started) = self.local_active_since.take() {
            self.local_covered_ms = self
                .local_covered_ms
                .saturating_add(started.elapsed().as_millis() as u64);
        }
    }

    fn stop_vendor_coverage(&mut self) {
        if let Some(started) = self.vendor_active_since.take() {
            self.vendor_fallback_ms = self
                .vendor_fallback_ms
                .saturating_add(started.elapsed().as_millis() as u64);
        }
    }

    fn input_transcription_override(&self) -> Option<String> {
        match &self.state {
            LocalTranscriptState::Active(_) => Some("none".to_string()),
            LocalTranscriptState::Starting
            | LocalTranscriptState::Degrading
            | LocalTranscriptState::VendorFallback { .. } => self.vendor_fallback_model.clone(),
            LocalTranscriptState::NotRequested | LocalTranscriptState::Ended => None,
        }
    }

    fn partial_identity(&mut self, stream_generation: u64) -> (String, u64) {
        if !self.turn_identities.contains_key(&stream_generation) {
            self.turn_generation = self.turn_generation.saturating_add(1);
            self.turn_identities.insert(
                stream_generation,
                (
                    format!("local-voice-turn-{}", uuid::Uuid::new_v4().simple()),
                    self.turn_generation,
                ),
            );
        }
        self.turn_identities
            .get(&stream_generation)
            .cloned()
            .expect("partial item identity initialized")
    }

    fn final_identity(&mut self, stream_generation: u64) -> (String, u64) {
        let identity = self.partial_identity(stream_generation);
        self.turn_identities.remove(&stream_generation);
        identity
    }

    fn manual_turns(&self) -> bool {
        self.manual_turns
    }

    fn accepts_transcript_event(&self, stream_generation: u64) -> bool {
        stream_generation > self.retired_through_generation
    }

    fn stage_final(&mut self, stream_generation: u64, text: String) -> (String, String, u64) {
        let clean = text.trim();
        let segments = self
            .staged_final_segments
            .entry(stream_generation)
            .or_default();
        let joined = segments.join(" ");
        if !clean.is_empty() && !segments.last().is_some_and(|last| last == clean) {
            if !joined.is_empty() && clean.starts_with(&joined) {
                segments.clear();
            }
            if !segments.join(" ").ends_with(clean) {
                segments.push(clean.to_string());
            }
        }
        let combined = segments.join(" ");
        let (item_id, turn_generation) = self.partial_identity(stream_generation);
        (combined, item_id, turn_generation)
    }

    fn partial_with_staged(&self, stream_generation: u64, partial: &str) -> String {
        let staged = self
            .staged_final_segments
            .get(&stream_generation)
            .map(|segments| segments.join(" "))
            .unwrap_or_default();
        let partial = partial.trim();
        if staged.is_empty() || partial.starts_with(&staged) {
            partial.to_string()
        } else if partial.is_empty() {
            staged
        } else {
            format!("{staged} {partial}")
        }
    }

    fn mark_commit_pending(&mut self, stream_generation: u64, mode: LocalTranscriptCommitMode) {
        self.pending_commits.insert(stream_generation, mode);
    }

    fn pending_commit_mode(&self, stream_generation: u64) -> Option<LocalTranscriptCommitMode> {
        self.pending_commits.get(&stream_generation).copied()
    }

    fn take_committed_final(&mut self, stream_generation: u64) -> Option<(String, String, u64)> {
        if self.pending_commits.remove(&stream_generation).is_none() {
            return None;
        }
        let text = self
            .staged_final_segments
            .remove(&stream_generation)
            .unwrap_or_default()
            .join(" ");
        let (item_id, turn_generation) = self.final_identity(stream_generation);
        self.retired_through_generation = self.retired_through_generation.max(stream_generation);
        Some((text, item_id, turn_generation))
    }

    fn retire_identity(&mut self, stream_generation: u64) -> Option<(String, u64)> {
        self.retired_through_generation = self.retired_through_generation.max(stream_generation);
        self.staged_final_segments.remove(&stream_generation);
        self.pending_commits.remove(&stream_generation);
        self.turn_timings.remove(&stream_generation);
        self.turn_identities.remove(&stream_generation)
    }

    fn take_identities(&mut self) -> Vec<(String, u64)> {
        self.staged_final_segments.clear();
        self.pending_commits.clear();
        self.turn_timings.clear();
        self.turn_identities
            .drain()
            .map(|(_, identity)| identity)
            .collect()
    }

    fn take_uncommitted_identities(&mut self) -> Vec<(String, u64)> {
        let generations = self
            .turn_identities
            .keys()
            .copied()
            .filter(|generation| !self.pending_commits.contains_key(generation))
            .collect::<Vec<_>>();
        generations
            .into_iter()
            .filter_map(|generation| self.retire_identity(generation))
            .collect()
    }

    fn record_audio(&mut self, stream_generation: u64) {
        self.turn_timings
            .entry(stream_generation)
            .or_default()
            .first_audio_at
            .get_or_insert_with(std::time::Instant::now);
    }

    fn has_audio(&self, stream_generation: u64) -> bool {
        self.turn_timings
            .get(&stream_generation)
            .and_then(|timing| timing.first_audio_at)
            .is_some()
    }

    fn record_commit(&mut self, stream_generation: u64) {
        self.turn_timings
            .entry(stream_generation)
            .or_default()
            .committed_at = Some(std::time::Instant::now());
    }

    fn record_first_partial(&mut self, stream_generation: u64) -> Option<u64> {
        let timing = self.turn_timings.entry(stream_generation).or_default();
        if timing.first_partial_at.is_some() {
            return None;
        }
        let now = std::time::Instant::now();
        timing.first_partial_at = Some(now);
        timing
            .first_audio_at
            .map(|started| now.duration_since(started).as_millis() as u64)
    }

    fn complete_turn(&mut self, stream_generation: u64) -> LocalTranscriptTurnCompletion {
        let timing = self.turn_timings.remove(&stream_generation);
        LocalTranscriptTurnCompletion {
            commit_to_final_latency_ms: timing
                .as_ref()
                .and_then(|timing| timing.committed_at)
                .map(|committed| committed.elapsed().as_millis() as u64),
            had_partial: timing
                .as_ref()
                .and_then(|timing| timing.first_partial_at)
                .is_some(),
        }
    }

    fn degrade(
        &mut self,
        reason: impl Into<String>,
    ) -> (Option<LocalRealtimeTranscriptHandle>, Vec<(String, u64)>) {
        let reason = reason.into();
        self.stop_local_coverage();
        let previous = std::mem::replace(&mut self.state, LocalTranscriptState::Degrading);
        let handle = match previous {
            LocalTranscriptState::Active(handle) => Some(handle),
            LocalTranscriptState::Starting
            | LocalTranscriptState::Degrading
            | LocalTranscriptState::VendorFallback { .. }
            | LocalTranscriptState::NotRequested
            | LocalTranscriptState::Ended => None,
        };
        self.state = LocalTranscriptState::VendorFallback { reason };
        (handle, self.take_uncommitted_identities())
    }

    fn end(
        &mut self,
    ) -> (
        Option<LocalRealtimeTranscriptHandle>,
        Vec<(String, u64)>,
        Option<LocalTranscriptTerminalSnapshot>,
    ) {
        if matches!(self.state, LocalTranscriptState::Ended) {
            return (None, Vec::new(), None);
        }
        let audited = self.is_requested();
        self.stop_local_coverage();
        self.stop_vendor_coverage();
        let previous = std::mem::replace(&mut self.state, LocalTranscriptState::Ended);
        let handle = match previous {
            LocalTranscriptState::Active(handle) => Some(handle),
            _ => None,
        };
        if let Some(handle) = handle.as_ref() {
            self.queue_snapshot = handle.queue_snapshot();
        }
        let snapshot = audited.then_some(LocalTranscriptTerminalSnapshot {
            local_covered_ms: self.local_covered_ms,
            vendor_fallback_ms: self.vendor_fallback_ms,
            queue: self.queue_snapshot,
        });
        (handle, self.take_identities(), snapshot)
    }

    fn fallback_reason(&self) -> Option<&str> {
        match &self.state {
            LocalTranscriptState::VendorFallback { reason } => Some(reason),
            _ => None,
        }
    }
}

fn local_command_error(operation: &str, error: LocalRealtimeTranscriptCommandError) -> String {
    let class = match error {
        LocalRealtimeTranscriptCommandError::QueueFull => "queue_full",
        LocalRealtimeTranscriptCommandError::Closed => "queue_closed",
    };
    format!("local_{operation}_{class}")
}

/// True for the benign OpenAI Realtime "committed an empty/too-short input audio
/// buffer" error. These mean the turn had < 100ms of committable audio, not a real
/// failure, so they are logged rather than surfaced to the client as an error.
fn is_benign_input_buffer_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    m.contains("input_audio_buffer_commit_empty")
        || m.contains("buffer too small")
        || m.contains("expected at least")
        || (m.contains("input audio buffer") && m.contains("commit"))
}

/// Google Live Translate (and similar) close with 1007 when the setup
/// JSON is illegal. Rotating retries the same payload forever and the
/// iOS overlay just says "Refreshing…".
fn is_fatal_realtime_setup_error(message: &str) -> bool {
    let m = message.to_ascii_lowercase();
    (m.contains("1007") || m.contains("invalid json payload"))
        && (m.contains("unknown name")
            || m.contains("cannot find field")
            || m.contains("generation_config")
            || m.contains("generationconfig"))
}

fn tutor_takeover_key(voice_session_id: &str, item_id: Option<&str>, text: &str) -> String {
    let normalized = text
        .to_ascii_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let item = item_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("-");
    format!("{voice_session_id}:{item}:{normalized}")
}

/// Owns the provider response that raced with a guided-flow takeover.
///
/// Interrupt acknowledgement and transcript finalization are asynchronous: the
/// Tutor/App Copilot turn can finish before the cancelled realtime response
/// emits its final caption. Keep the fence alive until both sides are done so a
/// late provider answer cannot become a second, competing assistant turn.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct TutorResponseSuppression {
    active: bool,
    response_id: Option<String>,
    provider_terminal_seen: bool,
}

impl TutorResponseSuppression {
    fn begin(&mut self, response_id: Option<String>) {
        self.active = true;
        self.response_id = response_id;
        self.provider_terminal_seen = false;
    }

    fn matches_or_claims_response(&mut self, response_id: Option<&str>) -> bool {
        if !self.active {
            return false;
        }
        let Some(response_id) = response_id.map(str::trim).filter(|id| !id.is_empty()) else {
            // Legacy direct clients did not include response_id on final
            // captions. While the fence is unidentified, fail closed.
            return true;
        };
        match self.response_id.as_deref() {
            Some(owned) => owned == response_id,
            None => {
                self.response_id = Some(response_id.to_string());
                true
            },
        }
    }

    fn suppresses_response_event(&mut self, response_id: &str) -> bool {
        self.matches_or_claims_response(Some(response_id))
    }

    /// Record terminal evidence for the response owned by this fence.
    /// Returns true when the event belonged to the fenced response.
    fn observe_provider_terminal(
        &mut self,
        response_id: Option<&str>,
        takeover_still_active: bool,
    ) -> bool {
        if !self.matches_or_claims_response(response_id) {
            return false;
        }
        if takeover_still_active {
            self.provider_terminal_seen = true;
        } else {
            *self = Self::default();
        }
        true
    }

    /// Legacy ResponseDone/ResponseFailed controls may omit an id. They are
    /// safe terminal evidence only before a concrete response id has been
    /// claimed; identified terminal events use `observe_provider_terminal`.
    fn observe_unidentified_terminal(&mut self, takeover_still_active: bool) {
        if self.active && self.response_id.is_none() {
            if takeover_still_active {
                self.provider_terminal_seen = true;
            } else {
                *self = Self::default();
            }
        }
    }

    fn finish_takeover(&mut self, another_takeover_active: bool) {
        if !another_takeover_active && self.provider_terminal_seen {
            *self = Self::default();
        }
    }

    /// A newly admitted ordinary utterance is the next provider generation.
    /// Release only an unidentified fence; a known old response id remains in
    /// place and therefore suppresses that late response without touching the
    /// new generation's different id.
    fn begin_new_user_generation(&mut self, takeover_still_active: bool) {
        if !takeover_still_active && self.response_id.is_none() {
            *self = Self::default();
        }
    }

    /// A failure announcement creates a new provider response immediately.
    /// If the interrupted response never emitted any identified event, release
    /// the ambiguous fence before injection; otherwise the exact old id stays.
    fn prepare_failure_announcement(&mut self) {
        if self.active && self.response_id.is_none() {
            *self = Self::default();
        }
    }
}

fn provider_audio_frame_is_suppressed(
    suppression: &mut TutorResponseSuppression,
    response_id: Option<&str>,
) -> bool {
    suppression.matches_or_claims_response(response_id)
}

/// How a voice session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionEndKind {
    /// The person hung up: `session.end` on the control socket.
    ClientRequested,
    /// The socket went away under us.
    TransportDropped,
}

/// How long a clean end stays up for the provider's closing report before it
/// stops the socket. The report is the session's own account of what it did —
/// for a duration-billed provider it is the entire bill — and it arrives on
/// the provider's teardown, after we ask it to stop.
const PROVIDER_CLOSING_REPORT_GRACE: std::time::Duration = std::time::Duration::from_millis(1500);

/// What to record for a realtime call still in flight when the session ends.
///
/// A call open when the person hangs up has not failed: its usage either
/// arrives in the provider's closing report or never does, and `cancelled`
/// asserts something we did not observe. This was recorded at the top of
/// `session.end`, before the provider had even been told to stop, so a
/// provider with no per-turn terminal event at all — GPT-Live reports one
/// usage figure for the whole session — ended every call as a cancelled
/// failure, unmetered and unattributed. A socket that drops under us really
/// did cut the call short.
fn in_flight_call_failure_class(end: SessionEndKind) -> Option<&'static str> {
    match end {
        SessionEndKind::ClientRequested => None,
        SessionEndKind::TransportDropped => Some("cancelled"),
    }
}

/// Whether a speech start interrupts a response, or merely begins one.
///
/// A new utterance supersedes a response the provider never terminated — but
/// only when the assistant was still speaking. The two speech-start paths
/// recorded the in-flight call as `cancelled` whatever it was, and
/// `has_interruptible_provider_response` cannot correct that: it counts the
/// in-flight call itself, so the only call there was to report was always
/// reported as cancelled. It is the right question for whether to send an
/// interrupt and the wrong one for whether anything was interrupted.
///
/// What is never stale is whether audio is still playing. A provider with
/// per-turn terminals clears its response id on the terminal; GPT-Live has
/// no per-turn terminal, so its id stays set for the rest of the session and
/// every later utterance looked like a barge-in — a turn-less `cancelled`
/// row for a response that had finished speaking.
fn speech_start_interrupts_a_response(
    assistant_audio_playing: bool,
    cascaded_response_active: bool,
) -> bool {
    assistant_audio_playing || cascaded_response_active
}

fn terminal_targets_current_provider_response(
    current_response_id: Option<&str>,
    terminal_response_id: Option<&str>,
    suppressed_response_id: Option<&str>,
) -> bool {
    let terminal_response_id = terminal_response_id
        .map(str::trim)
        .filter(|id| !id.is_empty());
    if terminal_response_id.is_some()
        && terminal_response_id == suppressed_response_id
        && terminal_response_id != current_response_id
    {
        return false;
    }
    match (terminal_response_id, current_response_id) {
        (Some(terminal), Some(current)) => terminal == current,
        _ => true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct VoiceTutorTakeoverRequest {
    feature_mode: magician::magician_v2::agents::FeatureMode,
    canvas_mode: magician::magician_v2::tutor::TutorCanvasMode,
    quick: bool,
    text: String,
    capture_screen: bool,
    reason: &'static str,
    rejection: Option<&'static str>,
    client_blackboard_handoff: bool,
}

fn voice_tutor_takeover_request(
    text: &str,
    capabilities: GuidedVoiceCapabilities,
) -> Option<VoiceTutorTakeoverRequest> {
    if capabilities.expanded_grammar {
        let invocation = magician::magician_v2::tutor::parse_voice_guided_flow_invocation(text)?;
        let capture_screen = invocation.requires_screen_capture();
        let rejection = if capture_screen && !capabilities.screen_capture {
            Some(
                if invocation.feature_mode == magician::magician_v2::agents::FeatureMode::Tutor {
                    "Tutor screen capture is not available on this device; say Tutor blackboard instead"
                } else {
                    "App Copilot screen capture is not available on this device"
                },
            )
        } else {
            None
        };
        return Some(VoiceTutorTakeoverRequest {
            feature_mode: invocation.feature_mode,
            canvas_mode: invocation.canvas_mode,
            quick: invocation.quick,
            text: invocation.canonical_text,
            capture_screen: capture_screen && capabilities.screen_capture,
            reason: "explicit_guided_flow_invoke",
            rejection,
            client_blackboard_handoff: capabilities.client_blackboard_handoff
                && invocation.feature_mode == magician::magician_v2::agents::FeatureMode::Tutor
                && invocation.canvas_mode
                    == magician::magician_v2::tutor::TutorCanvasMode::Blackboard,
        });
    }

    // Preserve the native-client contract that existed before the Web
    // guided-flow grammar: only trusted leading markers and exact `hey tutor`
    // / `hey copilot` forms take over. App Copilot keeps its pre-existing host
    // capture behavior, while Tutor remains source-free until each native
    // platform can hand off a screenshot from the speaking device.
    let feature_mode =
        magician::magician_v2::execution::agentic::parse_leading_feature_invocation(text);
    if !matches!(
        feature_mode,
        magician::magician_v2::agents::FeatureMode::Tutor
            | magician::magician_v2::agents::FeatureMode::AppCopilot
    ) {
        return None;
    }
    let capture_screen = feature_mode == magician::magician_v2::agents::FeatureMode::AppCopilot;
    let canvas_mode = if capture_screen {
        magician::magician_v2::tutor::TutorCanvasMode::ScreenOverlay
    } else {
        magician::magician_v2::tutor::TutorCanvasMode::Blackboard
    };
    Some(VoiceTutorTakeoverRequest {
        feature_mode,
        canvas_mode,
        quick: magician::magician_v2::tutor::has_tutor_quick_flag(text),
        text: text.to_string(),
        capture_screen: capture_screen && capabilities.screen_capture,
        reason: "explicit_tutor_invoke",
        rejection: (capture_screen && !capabilities.screen_capture).then_some(
            "App Copilot voice capture is not available on this device yet; no host screen was captured",
        ),
        client_blackboard_handoff: false,
    })
}

/// Per-connection actor. Owns one [`VoiceOrchestrator`] for the
/// lifetime of the WebSocket.
pub struct VoiceControlSession {
    voice_session_id: String,
    principal: String,
    workspace: String,
    source_surface: String,
    /// The rule that produced `source_surface`, minted with it at registration.
    /// Diagnostics only — nothing branches on it, so it cannot become a second
    /// authorization axis by accident.
    surface_derivation_reason: &'static str,
    /// Command/capture/presentation abilities negotiated from the registered
    /// client surface. Native iOS gets the full blackboard grammar without host
    /// capture; Web gets both blackboard and screen-bound flows.
    guided_voice_capabilities: GuidedVoiceCapabilities,
    /// Client-reported secure screen state. Unknown/unreported is deliberately
    /// not treated as locked; Web clients report an exact Idle Detection state
    /// when available and native iOS reports protected-data transitions.
    screen_locked: bool,
    /// Thread id stamped on the underlying media session at registration
    /// time. Used as a default for the upstream session when the
    /// frontend's `session.start` doesn't supply one.
    thread_id_hint: Option<String>,
    orchestrator: Arc<VoiceOrchestrator>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
    /// Prompt manager handle used to render the voice controller
    /// system prompt for the `session.ready` envelope. The frontend
    /// installs this prompt + the tool catalog on the OpenAI Realtime
    /// session via `session.update` once the data channel is open.
    prompt_manager: Arc<PromptManager>,
    /// `true` after the frontend sends `session.start` and the
    /// upstream session bootstraps cleanly. Guards against a client
    /// sending `tool.dispatch` etc. before the session is live.
    started: bool,
    /// Upstream PCM sender for `BackendProxied` providers (Gemini
    /// Live etc.). Incoming WS binary frames are pushed into this
    /// channel; the provider's backend session forwards them to the
    /// vendor. `None` for `DirectPeerToPeer` (OpenAI Realtime) —
    /// audio rides browser ↔ provider WebRTC and never crosses
    /// magician.
    upstream_audio_tx: Option<tokio::sync::mpsc::Sender<Vec<u8>>>,
    /// Control sender for `BackendProxied` providers. PTT turn
    /// boundaries, tool results, and backend-pushed announcements
    /// travel through this lane so the provider session stays
    /// backend-owned.
    provider_control_tx: Option<tokio::sync::mpsc::Sender<RealtimeAudioControl>>,
    /// Actor-ordered, non-blocking ingress for provider controls. A single
    /// forwarding task owns the bounded provider sender so controls observed in
    /// actor order cannot be reordered by independently spawned `send` tasks.
    /// This is especially important for current-turn context release followed
    /// by barge-in/interruption and for balanced tool-result replay.
    provider_control_dispatch_tx: Option<tokio::sync::mpsc::UnboundedSender<RealtimeAudioControl>>,
    /// True only after the backend provider reports that its transport is
    /// connected and consuming channels. Captured audio remains in the bounded
    /// pre-ready queue until this edge, so a slow websocket handshake cannot
    /// masquerade as a configured voice session.
    provider_transport_ready: bool,
    /// True once the active backend-proxied provider has received the
    /// session instructions/tools for the current upstream session.
    provider_configured: bool,
    /// True while provider setup is waiting for its transport/ack readiness
    /// barrier. New audio/control frames remain queued until that edge,
    /// preventing captured frames from racing ahead of `ConfigureSession`.
    provider_configuring: bool,
    provider_config_generation: u64,
    /// Correlates the vendor-transcription restore with the provider's actual
    /// `session.updated` acknowledgement. PTT commit/audio remains queued until
    /// this id is acknowledged.
    local_fallback_update_id: Option<String>,
    /// True while a voice-session rotation is in flight. Multiple
    /// triggers can arrive close together (GoAway, watermark, manual
    /// request, proactive timer); only the first should start work.
    rotation_in_progress: bool,
    /// Ordered controls/audio received before the backend-proxied
    /// provider is fully configured. This closes the tray/global PTT
    /// startup race: capture can begin immediately, but we only replay
    /// frames after `session.update` + resume context are sent.
    pending_provider_frames: VecDeque<PendingProviderFrame>,
    pending_provider_audio_bytes: usize,
    /// Aggregate pre-ready overflow telemetry. Audio arrives every few
    /// milliseconds, so logging once per dropped frame can monopolize an Actix
    /// worker precisely while it is trying to cross the first-turn boundary.
    pending_provider_dropped_frames: usize,
    pending_provider_dropped_audio_bytes: usize,
    pending_provider_last_trim_log_at: Option<std::time::Instant>,
    /// PCM bytes routed to the provider for the CURRENT turn (reset on ClearInput
    /// and after each commit). Diagnostic for the "buffer too small" commit-empty
    /// error: a manual PTT commit needs >= ~100ms (`MIN_PROVIDER_COMMIT_AUDIO_BYTES`)
    /// of appended audio or OpenAI Realtime rejects the commit.
    provider_turn_audio_bytes: usize,
    /// One-shot so a live call that is silently dropping mic audio is
    /// visible in magician.log without per-frame noise.
    logged_first_upstream_audio: bool,
    session_instructions: Option<String>,
    session_tools: Vec<LLMToolSpec>,
    audio_topology: Option<RealtimeAudioTopology>,
    /// Provider capability fence for historical tool exchange replay. A
    /// projected result is never downgraded into an ordinary user-text turn
    /// merely because the active provider lacks native balanced replay.
    realtime_provider: Option<RealtimeProviderKind>,
    pending_catalog_update: Option<PendingVoiceCatalogUpdate>,
    /// Exact rollout gate plus provider-effective response-gating state. The
    /// latter is recomputed on every rotation because a fallback provider may
    /// not support holding response creation after transcription.
    per_turn_context_configured: bool,
    per_turn_context_enabled: bool,
    per_turn_context_budget_ms: u64,
    turn_context_generation: u64,
    turn_context_pending: bool,
    /// Per-finalized-utterance fence. Replaced and cancelled on every newer
    /// turn so retrieval setup and unfinished stages cannot continue after
    /// barge-in or be injected into another provider generation.
    turn_context_cancellation: Option<tokio_util::sync::CancellationToken>,
    /// Exact owner-turn lifetime, retained after context retrieval settles so
    /// an interrupt still invalidates delayed governed tool/result fences.
    owner_turn_cancellation: Option<tokio_util::sync::CancellationToken>,
    active_tutor_takeover_keys: HashSet<String>,
    /// Per-takeover cancellation owned by the actor. A later exact lock event
    /// cancels capture/run work without cancelling the surrounding voice call.
    active_tutor_takeover_cancellations: HashMap<
        String,
        (
            tokio_util::sync::CancellationToken,
            magician::magician_v2::agents::FeatureMode,
        ),
    >,
    /// Keys cancelled specifically by a lock edge. Their async completion must
    /// not emit a second generic failure or enter completed-run dedupe; unlock
    /// permits the user to ask again.
    lock_cancelled_tutor_takeover_keys: HashSet<String>,
    /// Bounded final-transcript dedupe so provider retries cannot restart a
    /// just-completed visual flow after the active key is released.
    completed_tutor_takeover_keys: VecDeque<String>,
    /// Lock rejections have their own epoch-scoped dedupe. Clearing this set on
    /// unlock lets the user repeat the same words after unlocking without
    /// weakening completed-run dedupe.
    locked_tutor_rejection_keys: HashSet<String>,
    tutor_response_suppression: TutorResponseSuppression,
    /// Set after room speech fails prefix admission. It suppresses any provider
    /// response already racing behind that transcript until an addressed turn
    /// begins or the ignored response reaches its final event.
    suppress_ambient_assistant_transcript: bool,
    /// Realtime latency: wall-clock start of the current response turn (set when
    /// the user stops speaking) and time-to-first-assistant-token, emitted with
    /// the per-turn cost on `ResponseDone`. `None` between turns / in modes with
    /// no server-side `SpeechStopped` (client-driven PTT), where it stays unmeasured.
    turn_started_at: Option<std::time::Instant>,
    turn_ttfa_ms: Option<u64>,
    /// Stable logical-call identity minted when the response starts and moved
    /// into the terminal usage event. This prevents response completion from
    /// inventing an id after provider dispatch has already happened.
    current_llm_correlation: Option<magician::magician_v2::realtime_events::LlmEventCorrelation>,
    /// Wall-clock boundary paired with `current_llm_correlation`. Unlike the
    /// monotonic latency clock, this can be persisted directly without
    /// reconstructing an absolute timestamp from response completion.
    current_llm_started_at_ms: Option<i64>,
    /// Local cascaded mode is additive to vendor Realtime. These fields are
    /// never read by the existing vendor path.
    hands_free: bool,
    concurrent_requests: bool,
    selected_voice_context: Option<String>,
    captured_voice_context: Option<String>,
    /// Cascaded Hands-free must not feed assistant playback back into its raw
    /// microphone path when the client has no acoustic echo cancellation.
    half_duplex: bool,
    /// Speakable voice id per realtime / Live profile, loaded at connect.
    realtime_voices: std::collections::BTreeMap<String, String>,
    cascaded_turn_generation: u64,
    cascaded_turn_active: bool,
    /// Provider generation observed at response creation or first caption
    /// delta. Unlike audio ownership, this is available before playback and
    /// lets a guided-flow takeover fence the exact racing response.
    current_provider_response_id: Option<String>,
    assistant_audio_response_id: Option<String>,
    /// Owns the complete call-scoped local-caption state. Vendor transcription
    /// is disabled only while this controller is `Active`.
    local_transcript: LocalTranscriptController,
    addressing: VoiceAddressing,
    /// Scope-authoritative primary-agent identity resolved once at WebSocket
    /// admission. `session.ready` transports it to every client so native
    /// presentation surfaces never guess or hardcode an assistant label.
    assistant_identity: magician::magician_v2::presentation_identity::AgentPresentationIdentity,
    /// Tracks recent assistant speech plus its estimated device playback so
    /// transcript admission can drop the assistant's own echo. Fed from
    /// provider caption events and the provider→client PCM stream; consulted
    /// in `admit_user_transcript` before the address gate.
    echo_suppressor: SelfEchoSuppressor,
}

impl VoiceControlSession {
    fn has_interruptible_provider_response(&self) -> bool {
        self.current_provider_response_id.is_some()
            || self.assistant_audio_response_id.is_some()
            || self.current_llm_correlation.is_some()
    }

    fn has_interruptible_cascaded_response(&self) -> bool {
        self.hands_free && (self.cascaded_turn_active || self.assistant_audio_response_id.is_some())
    }

    fn provider_terminal_targets_current_response(&self, response_id: Option<&str>) -> bool {
        terminal_targets_current_provider_response(
            self.current_provider_response_id.as_deref(),
            response_id,
            self.tutor_response_suppression.response_id.as_deref(),
        )
    }

    /// ResponseDone/ResponseFailed may arrive before the final assistant
    /// caption. Preserve an identified guided-flow fence until that caption;
    /// lifecycle completion may still compare-and-clear the current telemetry
    /// owner without authorizing late output.
    fn observe_provider_response_lifecycle_terminal(&mut self, response_id: Option<&str>) {
        let response_id = response_id.map(str::trim).filter(|id| !id.is_empty());
        if let Some(response_id) = response_id {
            let _ = self
                .tutor_response_suppression
                .matches_or_claims_response(Some(response_id));
            if self.current_provider_response_id.as_deref() == Some(response_id) {
                self.current_provider_response_id = None;
            }
        } else {
            self.tutor_response_suppression
                .observe_unidentified_terminal(!self.active_tutor_takeover_keys.is_empty());
        }
    }

    fn begin_realtime_llm_call(&mut self) {
        // Named Hands-free is a cascaded STT -> chat -> TTS surface. Its model
        // call is captured by the delegated chat boundary; the local media
        // provider's speech events are not billable realtime LLM responses.
        if self.hands_free {
            return;
        }
        if self.current_llm_correlation.is_none() {
            self.current_llm_correlation = Some(new_realtime_llm_correlation(
                self.principal.clone(),
                self.workspace.clone(),
            ));
            self.current_llm_started_at_ms = Some(chrono::Utc::now().timestamp_millis());
            if self.turn_started_at.is_none() {
                self.turn_started_at = Some(std::time::Instant::now());
                self.turn_ttfa_ms = None;
            }
        }
    }

    fn reset_realtime_llm_call(&mut self) {
        self.current_llm_correlation = None;
        self.current_llm_started_at_ms = None;
        self.turn_started_at = None;
        self.turn_ttfa_ms = None;
    }

    /// Consume an in-flight realtime response when the provider reports a
    /// terminal error. Session/configuration errors that happen outside a
    /// response have no active correlation and are deliberately ignored by
    /// the LLM-call ledger.
    fn take_failed_realtime_llm_call(
        &mut self,
    ) -> Option<(
        magician::magician_v2::realtime_events::LlmEventCorrelation,
        Option<i64>,
        Option<u64>,
    )> {
        let correlation = self.current_llm_correlation.take()?;
        let started_at_ms = self.current_llm_started_at_ms.take();
        let response_ms = self
            .turn_started_at
            .take()
            .map(|started| started.elapsed().as_millis() as u64);
        self.turn_ttfa_ms = None;
        Some((correlation, started_at_ms, response_ms))
    }

    /// `_ctx` is kept for symmetry with the other observers; the write must
    /// not run on it. A ledger write spawned on the actor context dies with
    /// the actor, and the socket closing is exactly when these fire.
    fn observe_failed_realtime_llm_call(
        &mut self,
        error_class: &'static str,
        _ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some((correlation, started_at_ms, response_ms)) = self.take_failed_realtime_llm_call()
        else {
            self.reset_realtime_llm_call();
            return;
        };
        let orchestrator = Arc::clone(&self.orchestrator);
        actix::spawn(async move {
            orchestrator
                .observe_response_failure(correlation, started_at_ms, response_ms, error_class)
                .await;
        });
    }

    fn cancel_pending_turn_context(&mut self) {
        if let Some(cancellation) = self.turn_context_cancellation.take() {
            cancellation.cancel();
        }
        if let Some(cancellation) = self.owner_turn_cancellation.take() {
            cancellation.cancel();
        }
        if self.turn_context_pending {
            self.turn_context_generation = self.turn_context_generation.saturating_add(1);
            self.turn_context_pending = false;
        }
    }

    fn begin_realtime_user_turn(&mut self, text: String, ctx: &mut ws::WebsocketContext<Self>) {
        self.turn_context_generation = self.turn_context_generation.saturating_add(1);
        let generation = self.turn_context_generation;
        let voice_context = self.captured_voice_context.clone();
        let orchestrator = Arc::clone(&self.orchestrator);
        if let Some(previous) = self.turn_context_cancellation.take() {
            previous.cancel();
        }
        if let Some(previous) = self.owner_turn_cancellation.take() {
            previous.cancel();
        }
        let owner_turn_cancellation = tokio_util::sync::CancellationToken::new();
        self.owner_turn_cancellation = Some(owner_turn_cancellation.clone());
        if !self.per_turn_context_enabled {
            actix::spawn(async move {
                orchestrator.set_concurrent_voice_context(voice_context).await;
                let chat_turn_id = match orchestrator
                    .prebegin_realtime_user_turn(owner_turn_cancellation)
                    .await
                {
                    Ok(chat_turn_id) => chat_turn_id,
                    Err(error) => {
                        warn!(error = %error, "[VOICE-CONTROL] owner turn epoch unavailable");
                        return;
                    },
                };
                if let Err(error) = orchestrator
                    .ingest_prebegun_user_transcript(&text, &chat_turn_id)
                    .await
                {
                    warn!(error = %error, "[VOICE-CONTROL] user transcript ingest failed");
                }
            });
            return;
        }

        self.turn_context_pending = true;
        self.turn_context_cancellation = Some(owner_turn_cancellation.clone());
        let cancellation = self
            .turn_context_cancellation
            .as_ref()
            .expect("turn context cancellation installed")
            .clone();
        let addr = ctx.address();
        // Transcript durability is not part of the response-start gate. It was
        // previously joined with context retrieval, so a slow ledger write
        // could hold the provider response indefinitely even after the bounded
        // retrieval outcome was ready. Keep both operations concurrent, but
        // let the generation-fenced context result release independently.
        actix::spawn(async move {
            orchestrator.set_concurrent_voice_context(voice_context).await;
            let chat_turn_id = match orchestrator
                .prebegin_realtime_user_turn(owner_turn_cancellation)
                .await
            {
                Ok(chat_turn_id) => chat_turn_id,
                Err(error) => {
                    addr.do_send(TurnContextPrepared {
                        generation,
                        result: Err(error.to_string()),
                        provider_delivery_fence: None,
                    });
                    return;
                },
            };
            let ingest_orchestrator = Arc::clone(&orchestrator);
            let ingest_text = text.clone();
            let ingest_chat_turn_id = chat_turn_id;
            actix::spawn(async move {
                if let Err(error) = ingest_orchestrator
                    .ingest_prebegun_user_transcript(&ingest_text, &ingest_chat_turn_id)
                    .await
                {
                    warn!(error = %error, "[VOICE-CONTROL] user transcript ingest failed");
                }
            });
            let context = orchestrator
                .prepare_realtime_turn_context_with_cancellation(&text, cancellation)
                .await;
            let result = context.map_err(|error| error.to_string());
            let provider_delivery_fence = match result.as_ref() {
                Ok(context) if context.local_only_app_memory_rendered => orchestrator
                    .current_realtime_app_delivery_fence()
                    .await
                    .ok()
                    .flatten(),
                _ => None,
            };
            addr.do_send(TurnContextPrepared {
                generation,
                result,
                provider_delivery_fence,
            });
        });
    }

    fn interrupt_cascaded_turn(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        if !self.hands_free {
            return;
        }
        self.cascaded_turn_generation = self.cascaded_turn_generation.saturating_add(1);
        let interrupted_response_id = self.assistant_audio_response_id.take();
        let had_response = self.cascaded_turn_active || interrupted_response_id.is_some();
        self.cascaded_turn_active = false;
        // The client halts playback on `response.interrupted`, so the echo
        // windows of everything still queued collapse to the present.
        self.echo_suppressor
            .truncate_playback(std::time::Instant::now());
        if had_response {
            self.send_envelope(
                ctx,
                "response.interrupted",
                json!({ "response_id": interrupted_response_id }),
            );
        }
        if self.concurrent_requests { return; }
        let orchestrator = Arc::clone(&self.orchestrator);
        actix::spawn(async move {
            orchestrator.cancel_cascaded_voice_turn().await;
        });
    }

    fn install_provider_control_dispatcher(
        &mut self,
        tx: tokio::sync::mpsc::Sender<RealtimeAudioControl>,
    ) {
        self.provider_control_dispatch_tx = Some(spawn_provider_control_dispatcher(tx));
    }

    fn send_provider_control_now(
        tx: &tokio::sync::mpsc::UnboundedSender<RealtimeAudioControl>,
        control: RealtimeAudioControl,
    ) {
        let _ = tx.send(control);
    }

    fn send_provider_audio_now(tx: tokio::sync::mpsc::Sender<Vec<u8>>, frame: Vec<u8>) {
        actix::spawn(async move {
            let _ = tx.send(frame).await;
        });
    }

    fn send_provider_control(&self, control: RealtimeAudioControl) {
        let Some(tx) = self.provider_control_dispatch_tx.as_ref() else {
            return;
        };
        Self::send_provider_control_now(tx, control);
    }

    fn input_transcription_override(&self) -> Option<String> {
        self.local_transcript.input_transcription_override()
    }

    fn clear_local_transcript_partials(
        &self,
        identities: Vec<(String, u64)>,
        reason: &str,
        had_partial: Option<bool>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        for (item_id, turn_generation) in identities {
            self.send_envelope(
                ctx,
                "transcript.user.cleared",
                json!({
                    "item_id": item_id,
                    "turn_generation": turn_generation,
                    "reason": reason,
                    "had_partial": had_partial,
                }),
            );
            info!(
                voice_session_id = %self.voice_session_id,
                item_id = %item_id,
                turn_generation,
                reason,
                had_partial = ?had_partial,
                "[VOICE-CONTROL] cleared unfinished local transcript"
            );
            self.emit_local_transcript_telemetry(
                MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN,
                json!({
                    "state": "cleared",
                    "turn_generation": turn_generation,
                    "reason": reason,
                    "had_partial": had_partial,
                }),
            );
        }
    }

    fn degrade_local_transcript(&mut self, reason: String, ctx: &mut ws::WebsocketContext<Self>) {
        let (handle, identities) = self.local_transcript.degrade(reason.clone());
        let Some(handle) = handle else {
            return;
        };
        let queue = handle.queue_snapshot();
        self.local_transcript.record_queue_snapshot(queue);
        let _ = handle.try_end();
        self.clear_local_transcript_partials(identities, "local_transcript_failed", None, ctx);
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_QUEUE,
            json!({
                "state": "degraded",
                "error_class": &reason,
                "queued_audio_bytes": queue.queued_audio_bytes,
                "queued_audio_frames": queue.queued_audio_frames,
                "high_water_audio_bytes": queue.high_water_audio_bytes,
                "dropped_audio_frames": queue.dropped_audio_frames,
            }),
        );
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK,
            json!({
                "state": "vendor_restore_requested",
                "error_class": &reason,
                "fallback_model_configured": self.input_transcription_override().is_some(),
            }),
        );
        warn!(
            voice_session_id = %self.voice_session_id,
            error_class = %reason,
            "[VOICE-CONTROL] local transcript degraded; restoring provider transcription"
        );
        self.send_envelope(
            ctx,
            "session.error",
            json!({
                "message": "Local captions became unavailable; using provider transcription.",
                "recoverable": true,
            }),
        );
        if self.session_instructions.is_some() {
            self.configure_vendor_transcription_with_ack_barrier(ctx);
        }
    }

    fn configure_vendor_transcription_with_ack_barrier(
        &mut self,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some(tx) = self.provider_control_tx.clone() else {
            self.send_envelope(
                ctx,
                "session.error",
                json!({
                    "message": "No transcript provider is available for this call.",
                    "recoverable": false,
                }),
            );
            ctx.stop();
            return;
        };
        let Some(instructions) = self.session_instructions.clone() else {
            return;
        };
        self.provider_config_generation = self.provider_config_generation.saturating_add(1);
        let update_id = format!(
            "local-transcript-fallback-{}",
            self.provider_config_generation
        );
        self.provider_configured = false;
        self.provider_configuring = true;
        self.local_fallback_update_id = Some(update_id.clone());
        let tools = self.session_tools.clone();
        let input_transcription_model = self.input_transcription_override();
        let defer_response_until_context = self.per_turn_context_enabled;
        let addr = ctx.address();
        let failed_update_id = update_id.clone();
        actix::spawn(async move {
            if let Err(error) = tx
                .send(RealtimeAudioControl::ConfigureSession {
                    instructions,
                    tools,
                    input_transcription_model,
                    update_id: Some(failed_update_id.clone()),
                    defer_response_until_context,
                })
                .await
            {
                addr.do_send(LocalFallbackConfigureFailed {
                    update_id: failed_update_id,
                    reason: error.to_string(),
                });
            }
        });
        ctx.notify_later(
            LocalFallbackConfigureAckTimeout {
                update_id: update_id.clone(),
            },
            std::time::Duration::from_secs(5),
        );
    }

    fn send_local_transcript_audio(
        &mut self,
        frame: Vec<u8>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some(transcript) = self.local_transcript.active_handle() else {
            return;
        };
        let stream_generation = transcript.current_stream_generation();
        if let Err(error) = transcript.try_push_audio(frame) {
            self.degrade_local_transcript(local_command_error("audio", error), ctx);
        } else {
            self.local_transcript.record_audio(stream_generation);
        }
    }

    /// Returns true when the local boundary was accepted. Manual PTT sends the
    /// provider commit after the local final drains; server VAD has already
    /// committed upstream and only needs the local flush/finalization barrier.
    fn commit_local_transcript(
        &mut self,
        mode: LocalTranscriptCommitMode,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> bool {
        let Some(transcript) = self.local_transcript.active_handle() else {
            return false;
        };
        let current_generation = transcript.current_stream_generation();
        if mode == LocalTranscriptCommitMode::ProviderAlreadyCommitted
            && !self.local_transcript.has_audio(current_generation)
        {
            return false;
        }
        match transcript.try_commit() {
            Ok(stream_generation) => {
                self.local_transcript.record_commit(stream_generation);
                self.local_transcript
                    .mark_commit_pending(stream_generation, mode);
                true
            },
            Err(error) => {
                self.degrade_local_transcript(local_command_error("commit", error), ctx);
                false
            },
        }
    }

    fn queue_provider_commit_for_local_turn(
        &mut self,
        stream_generation: u64,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some(mode) = self.local_transcript.pending_commit_mode(stream_generation) else {
            return;
        };
        if !mode.requires_provider_commit() {
            self.finalize_committed_local_turn(stream_generation, ctx);
            return;
        }
        let control = RealtimeAudioControl::CommitInputAndRespond;
        if self.provider_configured && !self.provider_configuring {
            if let Some(tx) = self.provider_control_tx.clone() {
                let addr = ctx.address();
                actix::spawn(async move {
                    let result = tx.send(control).await.map_err(|error| error.to_string());
                    addr.do_send(LocalProviderCommitQueued {
                        stream_generation,
                        result,
                    });
                });
                return;
            }
        }
        if !self.started || self.provider_control_tx.is_some() {
            self.queue_pending_provider_frame(PendingProviderFrame::Control(control));
            self.finalize_committed_local_turn(stream_generation, ctx);
            return;
        }

        let identity = self
            .local_transcript
            .retire_identity(stream_generation)
            .into_iter()
            .collect();
        self.clear_local_transcript_partials(identity, "provider_commit_unavailable", None, ctx);
        self.send_envelope(
            ctx,
            "session.error",
            json!({
                "message": "The voice provider is unavailable; this turn was not sent.",
                "recoverable": false,
            }),
        );
    }

    fn finalize_committed_local_turn(
        &mut self,
        stream_generation: u64,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some((text, item_id, turn_generation)) = self
            .local_transcript
            .take_committed_final(stream_generation)
        else {
            return;
        };
        let completion = self.local_transcript.complete_turn(stream_generation);
        if text.trim().is_empty() {
            self.clear_local_transcript_partials(
                vec![(item_id, turn_generation)],
                "no_final_transcript",
                Some(completion.had_partial),
                ctx,
            );
            return;
        }
        info!(
            voice_session_id = %self.voice_session_id,
            item_id = %item_id,
            turn_generation,
            transcript_chars = text.chars().count(),
            "[VOICE-CONTROL] committed local user transcript finalized"
        );
        if self.handle_finalized_user_transcript(text, item_id, Some(turn_generation), ctx) {
            self.emit_local_transcript_telemetry(
                MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN,
                json!({
                    "state": "final",
                    "stream_generation": stream_generation,
                    "turn_generation": turn_generation,
                    "commit_to_final_latency_ms": completion.commit_to_final_latency_ms,
                }),
            );
        }
    }

    fn clear_local_transcript(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        let Some(transcript) = self.local_transcript.active_handle() else {
            return;
        };
        match transcript.try_clear() {
            Ok(stream_generation) => {
                let identities = self
                    .local_transcript
                    .retire_identity(stream_generation)
                    .into_iter()
                    .collect();
                self.clear_local_transcript_partials(identities, "input_cleared", None, ctx);
            },
            Err(error) => {
                self.degrade_local_transcript(local_command_error("clear", error), ctx);
            },
        }
    }

    fn end_local_transcript(&mut self) -> Vec<(String, u64)> {
        let (handle, identities, terminal) = self.local_transcript.end();
        if let Some(handle) = handle {
            if let Err(error) = handle.try_end() {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    error = %error,
                    "[VOICE-CONTROL] local transcript stream already closed"
                );
            }
        }
        if let Some(terminal) = terminal {
            self.emit_local_transcript_telemetry(
                MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE,
                json!({
                    "state": "ended",
                    "local_covered_ms": terminal.local_covered_ms,
                    "vendor_fallback_ms": terminal.vendor_fallback_ms,
                    "queued_audio_bytes": terminal.queue.queued_audio_bytes,
                    "queued_audio_frames": terminal.queue.queued_audio_frames,
                    "high_water_audio_bytes": terminal.queue.high_water_audio_bytes,
                    "dropped_audio_frames": terminal.queue.dropped_audio_frames,
                }),
            );
        }
        identities
    }

    fn send_or_queue_provider_control(&mut self, control: RealtimeAudioControl) {
        if self.provider_configured && !self.provider_configuring {
            if let Some(tx) = self.provider_control_dispatch_tx.as_ref() {
                Self::send_provider_control_now(tx, control);
                return;
            }
        }

        if !self.started || self.provider_control_tx.is_some() {
            self.queue_pending_provider_frame(PendingProviderFrame::Control(control));
        }
    }

    fn send_or_queue_provider_controls(&mut self, controls: Vec<RealtimeAudioControl>) {
        if self.provider_configured && !self.provider_configuring {
            if let Some(tx) = self.provider_control_dispatch_tx.as_ref() {
                for control in controls {
                    Self::send_provider_control_now(tx, control);
                }
                return;
            }
        }

        if !self.started || self.provider_control_tx.is_some() {
            for control in controls {
                self.queue_pending_provider_frame(PendingProviderFrame::Control(control));
            }
        }
    }

    fn send_or_queue_provider_audio(&mut self, frame: Vec<u8>) {
        // Track PCM routed to the provider this turn so the commit path can report
        // whether it had enough audio (diagnoses the "buffer too small" error).
        self.provider_turn_audio_bytes = self.provider_turn_audio_bytes.saturating_add(frame.len());
        if self.provider_configured && !self.provider_configuring {
            if let Some(tx) = self.upstream_audio_tx.clone() {
                Self::send_provider_audio_now(tx, frame);
                return;
            }
        }

        if !self.started || self.upstream_audio_tx.is_some() {
            self.queue_pending_provider_frame(PendingProviderFrame::Audio(frame));
        }
    }

    fn queue_pending_provider_frame(&mut self, frame: PendingProviderFrame) {
        let frame_audio_len = frame.audio_len();
        if frame_audio_len > PENDING_PROVIDER_AUDIO_MAX_BYTES {
            warn!(
                voice_session_id = %self.voice_session_id,
                frame_audio_len,
                max_audio_bytes = PENDING_PROVIDER_AUDIO_MAX_BYTES,
                "[VOICE-CONTROL] dropping oversized pre-ready provider audio frame"
            );
            return;
        }

        self.pending_provider_audio_bytes += frame_audio_len;
        self.pending_provider_frames.push_back(frame);

        let mut dropped_frames = 0usize;
        let mut dropped_audio_bytes = 0usize;
        while self.pending_provider_frames.len() > PENDING_PROVIDER_FRAME_MAX_COUNT
            || self.pending_provider_audio_bytes > PENDING_PROVIDER_AUDIO_MAX_BYTES
        {
            let Some(dropped) = self.pending_provider_frames.pop_front() else {
                break;
            };
            dropped_frames += 1;
            dropped_audio_bytes += dropped.audio_len();
            self.pending_provider_audio_bytes = self
                .pending_provider_audio_bytes
                .saturating_sub(dropped.audio_len());
        }

        if dropped_frames > 0 {
            self.pending_provider_dropped_frames = self
                .pending_provider_dropped_frames
                .saturating_add(dropped_frames);
            self.pending_provider_dropped_audio_bytes = self
                .pending_provider_dropped_audio_bytes
                .saturating_add(dropped_audio_bytes);
            let now = std::time::Instant::now();
            if should_emit_pending_provider_trim_warning(
                &mut self.pending_provider_last_trim_log_at,
                now,
            ) {
                warn!(
                    voice_session_id = %self.voice_session_id,
                    dropped_frames = self.pending_provider_dropped_frames,
                    dropped_audio_bytes = self.pending_provider_dropped_audio_bytes,
                    pending_frames = self.pending_provider_frames.len(),
                    pending_audio_bytes = self.pending_provider_audio_bytes,
                    "[VOICE-CONTROL] trimmed pre-ready provider frame buffer"
                );
            }
        }
    }

    fn clear_pending_provider_frames(&mut self) {
        self.pending_provider_frames.clear();
        self.pending_provider_audio_bytes = 0;
        self.reset_pending_provider_trim_telemetry();
    }

    fn reset_pending_provider_trim_telemetry(&mut self) {
        self.pending_provider_dropped_frames = 0;
        self.pending_provider_dropped_audio_bytes = 0;
        self.pending_provider_last_trim_log_at = None;
    }

    fn acknowledge_pending_catalog_update(
        &mut self,
        update_id: &str,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some(pending) = self.pending_catalog_update.as_mut() else {
            return;
        };
        if pending.update.update_id != update_id || pending.acknowledgement_in_flight {
            return;
        }
        pending.acknowledgement_in_flight = true;
        let update = pending.update.clone();
        let orchestrator = Arc::clone(&self.orchestrator);
        let addr = ctx.address();
        actix::spawn(async move {
            let result = orchestrator
                .acknowledge_tool_catalog_update(&update)
                .await
                .map_err(|error| error.to_string());
            addr.do_send(CatalogUpdateCommitted {
                update_id: update.update_id,
                result,
            });
        });
    }

    fn rotate_for_pending_catalog_update(
        &mut self,
        update_id: &str,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        let Some(pending) = self.pending_catalog_update.as_mut() else {
            return;
        };
        if pending.update.update_id != update_id || pending.awaiting_rotation {
            return;
        }
        pending.awaiting_rotation = true;
        pending.acknowledgement_in_flight = false;
        self.spawn_rotation(RotateReason::Reconnect, ctx);
    }

    fn configure_provider_session_and_flush(
        &mut self,
        resume: Option<ResumeContext>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        self.provider_config_generation = self.provider_config_generation.saturating_add(1);
        let generation = self.provider_config_generation;
        let Some(tx) = self.provider_control_dispatch_tx.clone() else {
            self.provider_configured = false;
            self.provider_configuring = false;
            self.clear_pending_provider_frames();
            return;
        };
        let Some(instructions) = self.session_instructions.clone() else {
            return;
        };
        let tools = self.session_tools.clone();
        let defer_response_until_context = self.per_turn_context_enabled;
        let input_transcription_model = self.input_transcription_override();
        let resume_controls = resume
            .map(|resume| provider_resume_controls(resume, self.realtime_provider))
            .unwrap_or_default();
        self.provider_configured = false;
        self.provider_configuring = true;
        if tx
            .send(RealtimeAudioControl::ConfigureSession {
                instructions,
                tools,
                input_transcription_model,
                update_id: None,
                defer_response_until_context,
            })
            .is_err()
            || resume_controls
                .into_iter()
                .any(|control| tx.send(control).is_err())
        {
            self.provider_configuring = false;
            self.clear_pending_provider_frames();
            self.send_envelope(
                ctx,
                "session.error",
                json!({
                    "message": "Realtime provider closed before session configuration.",
                    "recoverable": false,
                }),
            );
            ctx.stop();
            return;
        }
        if self.provider_transport_ready {
            self.provider_configuring = false;
            self.provider_configured = true;
            self.flush_pending_provider_frames();
        }
        ctx.notify_later(
            ProviderReadyTimeout { generation },
            std::time::Duration::from_secs(10),
        );
    }

    fn flush_pending_provider_frames(&mut self) {
        if !self.provider_configured || self.provider_configuring {
            return;
        }
        let provider_control_tx = self.provider_control_dispatch_tx.clone();
        let upstream_audio_tx = self.upstream_audio_tx.clone();
        if provider_control_tx.is_none() && upstream_audio_tx.is_none() {
            self.clear_pending_provider_frames();
            return;
        }
        let pending_frames: Vec<_> = self.pending_provider_frames.drain(..).collect();
        self.pending_provider_audio_bytes = 0;
        self.reset_pending_provider_trim_telemetry();
        actix::spawn(async move {
            for frame in pending_frames {
                match frame {
                    PendingProviderFrame::Control(control) => {
                        if let Some(tx) = provider_control_tx.as_ref() {
                            let _ = tx.send(control);
                        }
                    },
                    PendingProviderFrame::Audio(frame) => {
                        if let Some(tx) = upstream_audio_tx.as_ref() {
                            let _ = tx.send(frame).await;
                        }
                    },
                }
            }
        });
    }

    fn send_envelope(&self, ctx: &mut ws::WebsocketContext<Self>, kind: &str, payload: Value) {
        let envelope = json!({ "kind": kind, "payload": payload });
        match serde_json::to_string(&envelope) {
            Ok(text) => ctx.text(text),
            Err(err) => error!(
                voice_session_id = %self.voice_session_id,
                error = %err,
                "[VOICE-CONTROL] envelope encode failed"
            ),
        }
    }

    fn emit_local_transcript_telemetry(&self, event_type: &str, payload: Value) {
        self.broadcaster.emit_named(
            event_type,
            MEDIA_SYSTEM_AGENT,
            Some(&self.principal),
            Some(&self.workspace),
            json!({
                "voice_session_id": self.voice_session_id,
                "source_surface": self.source_surface,
                "details": payload,
            }),
        );
    }

    fn admit_user_transcript(
        &mut self,
        text: &str,
        item_id: Option<&str>,
        turn_generation: Option<u64>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> Option<String> {
        if text.trim().is_empty() {
            return None;
        }
        // Self-echo guard, deliberately BEFORE the address gate: a transcript
        // that is the assistant's own speech leaking back through the
        // microphone must not arm, spend, or refresh the follow-up window —
        // and with the prefix gate off (iOS open mic sends
        // `require_voice_prefix: false`) this check is the only thing between
        // an underperforming device AEC and an unbounded speak-hear-reply loop.
        if let Some(echo) = self
            .echo_suppressor
            .match_transcript(text, std::time::Instant::now())
        {
            info!(
                voice_session_id = %self.voice_session_id,
                containment = echo.containment,
                matched_rule = echo.matched_rule,
                window_source = echo.window_source,
                window_remaining_ms = echo.window_remaining_ms,
                "[VOICE-CONTROL] rejected self-echo of assistant speech at transcript admission"
            );
            self.reject_user_transcript("self_echo", item_id, turn_generation, ctx);
            return None;
        }
        let rejection_reason = match self.addressing.admit(text) {
            VoiceAddressingDecision::Admitted(admitted) => {
                self.suppress_ambient_assistant_transcript = false;
                return Some(admitted);
            },
            VoiceAddressingDecision::Armed => "address_prefix_armed",
            VoiceAddressingDecision::Rejected => "address_prefix_required",
        };
        self.reject_user_transcript(rejection_reason, item_id, turn_generation, ctx);
        debug!(
            voice_session_id = %self.voice_session_id,
            "[VOICE-CONTROL] ignored ambient transcript without an address prefix"
        );
        None
    }

    /// The one semantic rejection path. Address-prefix and self-echo outcomes
    /// both suppress a provider response racing behind the transcript and emit
    /// `transcript.user.ignored`; clients use the reason only to decide whether
    /// guidance is useful. Lifecycle cleanup uses `transcript.user.cleared`.
    fn reject_user_transcript(
        &mut self,
        reason: &str,
        item_id: Option<&str>,
        turn_generation: Option<u64>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        self.suppress_ambient_assistant_transcript = true;
        self.turn_started_at = None;
        self.turn_ttfa_ms = None;
        info!(
            voice_session_id = %self.voice_session_id,
            reason,
            item_id = ?item_id,
            turn_generation = ?turn_generation,
            "[VOICE-CONTROL] rejected user transcript at admission"
        );
        self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
        self.send_or_queue_provider_control(RealtimeAudioControl::ClearInput);
        self.send_envelope(
            ctx,
            "transcript.user.ignored",
            json!({
                "item_id": item_id,
                "turn_generation": turn_generation,
                "reason": reason,
                "activation_phrases": self.addressing.activation_phrases(),
                "follow_up_window_ms": self.addressing.follow_up_window_ms(),
            }),
        );
        if let Some(turn_generation) = turn_generation {
            self.emit_local_transcript_telemetry(
                MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN,
                json!({
                    "state": "ignored",
                    "turn_generation": turn_generation,
                    "reason": reason,
                }),
            );
        }
    }

    fn handle_text_message(&mut self, payload: &str, ctx: &mut ws::WebsocketContext<Self>) {
        let parsed: Value = match serde_json::from_str(payload) {
            Ok(v) => v,
            Err(err) => {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    error = %err,
                    "[VOICE-CONTROL] non-JSON text frame ignored"
                );
                return;
            },
        };
        let kind = parsed
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let body = parsed.get("payload").cloned().unwrap_or(Value::Null);
        match kind.as_str() {
            "session.start" => self.on_session_start(&body, ctx),
            "session.end" => self.on_session_end(ctx),
            "session.rotate" => self.on_session_rotate(&body, ctx),
            "session.turn_boundary" => self.on_turn_boundary_change(&body, ctx),
            "screen.state" => self.on_screen_state(&body, Some(ctx)),
            "ptt.engage" => {
                // A new utterance supersedes any interrupted response whose
                // provider never emitted a terminal usage event.
                let provider_response_active = self.has_interruptible_provider_response();
                let cascaded_response_active = self.has_interruptible_cascaded_response();
                if provider_response_active {
                    self.observe_failed_realtime_llm_call("cancelled", ctx);
                    self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
                }
                if cascaded_response_active {
                    self.interrupt_cascaded_turn(ctx);
                }
                self.send_or_queue_provider_control(RealtimeAudioControl::ClearInput);
                // New utterance: the provider buffer was just cleared, so reset the
                // per-turn audio accounting used by the release-commit diagnostic.
                self.provider_turn_audio_bytes = 0;
                debug!(
                    voice_session_id = %self.voice_session_id,
                    kind = %kind,
                    "[VOICE-CONTROL] PTT signal"
                );
            },
            "input.clear" => {
                self.clear_local_transcript(ctx);
                self.send_or_queue_provider_control(RealtimeAudioControl::ClearInput);
                self.provider_turn_audio_bytes = 0;
                debug!(
                    voice_session_id = %self.voice_session_id,
                    kind = %kind,
                    "[VOICE-CONTROL] clearing provider input buffer"
                );
            },
            "ptt.release" => {
                // Direct WebRTC responses bypass the backend provider-event
                // stream. Establish the response clock before the browser
                // commits its provider input so response.done never has to
                // invent an absolute start from its terminal timestamp.
                if self.turn_started_at.is_none() {
                    self.turn_started_at = Some(std::time::Instant::now());
                    self.turn_ttfa_ms = None;
                }
                self.begin_realtime_llm_call();
                // Commit the local stream first. WebSocket frame ordering plus
                // the transcript command queue guarantees every prior PCM
                // frame is included before the utterance is finalized.
                let turn_audio_bytes = self.provider_turn_audio_bytes;
                let via_local_transcript = self
                    .commit_local_transcript(LocalTranscriptCommitMode::SendProviderCommit, ctx);
                if !via_local_transcript {
                    self.send_or_queue_provider_control(
                        RealtimeAudioControl::CommitInputAndRespond,
                    );
                }
                // Diagnostic for the "buffer too small" commit-empty error: how much
                // audio actually reached the provider this turn, which commit path was
                // taken (local-transcript-coordinated vs direct), and whether it clears
                // the provider's ~100ms minimum. If `below_provider_minimum` is true on
                // a turn where you clearly spoke, the mic audio is not reaching the
                // provider (routing/flush), not just a short utterance.
                info!(
                    voice_session_id = %self.voice_session_id,
                    kind = %kind,
                    provider_turn_audio_bytes = turn_audio_bytes,
                    min_commit_audio_bytes = MIN_PROVIDER_COMMIT_AUDIO_BYTES,
                    below_provider_minimum = turn_audio_bytes < MIN_PROVIDER_COMMIT_AUDIO_BYTES,
                    via_local_transcript,
                    "[VOICE-CONTROL] PTT release commit"
                );
                self.provider_turn_audio_bytes = 0;
            },
            "response.interrupt" => {
                self.cancel_pending_turn_context();
                let provider_response_active = self.has_interruptible_provider_response();
                let cascaded_response_active = self.has_interruptible_cascaded_response();
                if provider_response_active {
                    self.observe_failed_realtime_llm_call("cancelled", ctx);
                    self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
                }
                if cascaded_response_active {
                    self.interrupt_cascaded_turn(ctx);
                }
                debug!(
                    voice_session_id = %self.voice_session_id,
                    kind = %kind,
                    "[VOICE-CONTROL] interrupting provider response"
                );
            },
            "voice.context" => {
                self.selected_voice_context = body.get("context_session_id").and_then(Value::as_str)
                    .filter(|id| id.len() <= 160).map(str::to_owned);
                if self.concurrent_requests && self.selected_voice_context.is_some() {
                    self.send_or_queue_provider_control(RealtimeAudioControl::InjectSystemMessage {
                        text: "The listener selected or heard an application-managed background answer. For follow-ups, delegate_to_chat with their exact words; the server resolves the addressed context. Do not guess content you have not received.".into(),
                        request_response: false,
                    });
                }
            },
            "speech.started" => {
                self.captured_voice_context = self.selected_voice_context.clone();
                self.cancel_pending_turn_context();
                if speech_start_interrupts_a_response(
                    self.assistant_audio_response_id.is_some(),
                    self.has_interruptible_cascaded_response(),
                ) {
                    self.observe_failed_realtime_llm_call("cancelled", ctx);
                }
            },
            "speech.stopped" | "response.started" => {
                // Browser-direct WebRTC has no server-owned provider observer.
                // Both events are idempotent timing anchors; response.created
                // wins when it arrives first and PTT/server-VAD remain safe
                // fallbacks.
                if kind == "response.started" {
                    let response_id = body
                        .get("response_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|id| !id.is_empty())
                        .map(str::to_string);
                    if self
                        .tutor_response_suppression
                        .matches_or_claims_response(response_id.as_deref())
                    {
                        return;
                    }
                    if let Some(response_id) = response_id {
                        self.current_provider_response_id = Some(response_id.clone());
                    }
                }
                if self.turn_started_at.is_none() {
                    self.turn_started_at = Some(std::time::Instant::now());
                    self.turn_ttfa_ms = None;
                }
                self.begin_realtime_llm_call();
            },
            "response.failed" => {
                // Browser-direct providers send only this closed terminal
                // vocabulary. Never persist a client/provider error message.
                let response_id = body.get("response_id").and_then(Value::as_str);
                let targets_current = self.provider_terminal_targets_current_response(response_id);
                self.observe_provider_response_lifecycle_terminal(response_id);
                let error_class = match body.get("terminal_state").and_then(Value::as_str) {
                    Some("cancelled") => "cancelled",
                    Some("incomplete") => "realtime_provider_incomplete",
                    _ => "realtime_provider_error",
                };
                if targets_current {
                    self.observe_failed_realtime_llm_call(error_class, ctx);
                }
            },
            "user.text" => self.on_user_text(&body, ctx),
            "transcript.user" => self.on_transcript(ChatMessageDirection::User, &body, ctx),
            "transcript.assistant" => {
                self.on_transcript(ChatMessageDirection::Assistant, &body, ctx)
            },
            "tool.dispatch"
                if matches!(
                    self.audio_topology,
                    Some(RealtimeAudioTopology::BackendProxied)
                ) =>
            {
                // Backend-proxied function calls arrive only through the
                // server-owned provider event channel. A client-authored frame
                // must never borrow the live owner turn to invoke a governed
                // App tool.
                debug!(
                    voice_session_id = %self.voice_session_id,
                    "[VOICE-CONTROL] ignored caller tool dispatch on backend-proxied session"
                );
            },
            "tool.dispatch" => self.on_tool_dispatch(&body, ctx),
            "tool.catalog.ack" => {
                // The backend provider acknowledges catalog installation over
                // `RealtimeProviderEvent::SessionConfigured`. Accepting the
                // browser compatibility ack here would let a caller skip that
                // physical-provider wait and release a governed result early.
                if matches!(
                    self.audio_topology,
                    Some(RealtimeAudioTopology::BackendProxied)
                ) {
                    debug!(
                        voice_session_id = %self.voice_session_id,
                        "[VOICE-CONTROL] ignored caller catalog ack on backend-proxied session"
                    );
                } else if let Some(update_id) = body.get("update_id").and_then(Value::as_str) {
                    self.acknowledge_pending_catalog_update(update_id, ctx);
                }
            },
            "token.usage" => self.on_token_usage(&body, ctx),
            other => {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    kind = %other,
                    "[VOICE-CONTROL] ignoring unknown frame kind"
                );
            },
        }
    }

    fn on_session_start(&mut self, body: &Value, ctx: &mut ws::WebsocketContext<Self>) {
        self.on_screen_state(body, None);
        if self.started {
            return;
        }
        if let Some(required) = requested_voice_prefix_override(body) {
            self.addressing.set_required(required);
        }
        let ui_thread_id = body
            .get("ui_thread_id")
            .and_then(|v| v.as_str())
            .unwrap_or(DEFAULT_VOICE_THREAD)
            .to_string();
        let thread_id = body
            .get("thread_id")
            .and_then(|v| v.as_str())
            .map(str::to_owned)
            .or_else(|| self.thread_id_hint.clone());
        let realtime_profile_name = body
            .get("realtime_profile")
            .or_else(|| body.get("profile"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        let hands_free = body
            .get("voice_mode")
            .and_then(|value| value.as_str())
            .is_some_and(|value| value.eq_ignore_ascii_case("hands_free"));
        let echo_cancellation = body
            .get("echo_cancellation")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let half_duplex = hands_free && !echo_cancellation;
        let turn_detection_override = requested_realtime_turn_detection(body, hands_free);
        let coding_choice = body
            .get("coding_choice")
            .cloned()
            .and_then(|value| {
                serde_json::from_value::<
                    magician::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice,
                >(value)
                .ok()
            })
            .and_then(
                magician::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice::into_internal,
            );
        let chat_choice = requested_chat_choice(body);
        let realtime_voices = self.realtime_voices.clone();
        self.concurrent_requests = body.get("concurrent_requests").and_then(Value::as_bool).unwrap_or(false)
            && matches!(self.source_surface.as_str(), "authenticated_realtime_voice" | "realtime_voice");
        let concurrent_requests = self.concurrent_requests;
        self.hands_free = hands_free;
        self.half_duplex = half_duplex;
        let orchestrator = Arc::clone(&self.orchestrator);
        let principal = self.principal.clone();
        let workspace = self.workspace.clone();
        let voice_session_id = self.voice_session_id.clone();
        let source_surface = self.source_surface.clone();
        let prompt_manager = Arc::clone(&self.prompt_manager);
        let assistant_name = self.assistant_identity.name.clone();
        let addr = ctx.address();
        let fut = async move {
            match orchestrator
                .start(
                    principal.clone(),
                    workspace.clone(),
                    voice_session_id.clone(),
                    source_surface,
                    ui_thread_id,
                    thread_id,
                    realtime_profile_name,
                    hands_free,
                    concurrent_requests,
                    half_duplex,
                    turn_detection_override,
                    coding_choice,
                    chat_choice,
                    realtime_voices,
                )
                .await
            {
                Ok(descriptor) => {
                    // Local user transcription is intentionally opened before
                    // `session.ready`: the iOS client starts sending PCM as
                    // soon as that envelope arrives, so this ordering prevents
                    // clipping the first spoken frames. A failed local chain
                    // is non-fatal; ConfigureSession retains the profile's
                    // configured vendor transcription fallback.
                    if descriptor_requests_local_transcript(&descriptor) {
                        let _ = addr
                            .send(BeginLocalTranscript {
                                vendor_fallback_model: descriptor
                                    .transcription_fallback_model
                                    .clone(),
                                manual_turns: descriptor_uses_manual_turns(&descriptor),
                            })
                            .await;
                        match start_local_realtime_transcript(
                            principal.clone(),
                            workspace.clone(),
                            voice_session_id.clone(),
                        )
                        .await
                        {
                            Ok((handle, events)) => {
                                let _ = addr.send(AttachLocalTranscript { handle, events }).await;
                            },
                            Err(error) => {
                                let _ = addr
                                    .send(LocalTranscriptStartFailed {
                                        reason: "local_start_failed".to_string(),
                                    })
                                    .await;
                                warn!(
                                    voice_session_id = %voice_session_id,
                                    error = %error,
                                    "[VOICE-CONTROL] local transcript unavailable; retaining provider transcription"
                                );
                            },
                        }
                    }
                    // Plug the downstream fanout receiver into the
                    // actor so backend task-completion notifications
                    // flow back to the frontend as `task.completed`.
                    if let Some(rx) = orchestrator.take_downstream_receiver().await {
                        let _ = addr.send(AttachBridgeStream(rx)).await;
                    }
                    // For BackendProxied providers, also take the
                    // audio channel so the actor can pipe browser
                    // PCM ↔ provider PCM over the control WS. None
                    // for DirectPeerToPeer (audio rides WebRTC,
                    // never crosses magician).
                    if let Some(channel) = orchestrator.take_audio_channel().await {
                        let _ = addr.send(AttachAudioChannel(channel)).await;
                    }
                    // GPT Live 1 is a mouth, not a Realtime tool loop. Do not
                    // install CHAT_OUTER_LOOP + voice_modality_addendum (or
                    // Magician's in-session tool catalog) on Live — that
                    // prompt is for GPT Realtime / Gemini. Live gets the
                    // isolated GPT-Live mouth prompt; Magician remains the
                    // delegated backend via delegate_to_chat.
                    //
                    // `delegate_to_chat` still authorizes against the voice
                    // catalog snapshot, so we render session_context for the
                    // snapshot id and then throw away its instructions/tools
                    // instead of advertising them to Live. Skipping that
                    // snapshot left Live hung: Magician refused the hatch
                    // with "realtime tool policy snapshot was not advertised".
                    let (
                        instructions,
                        tools,
                        policy_snapshot_id,
                        per_turn_context_configured,
                        per_turn_context_enabled,
                        per_turn_context_budget_ms,
                    ) = if matches!(descriptor.provider, RealtimeProviderKind::OpenAiLive) {
                        let mut mouth = render_openai_live_mouth_instructions(
                            prompt_manager.as_ref(),
                            &assistant_name,
                        )
                        .await;
                        if concurrent_requests {
                            mouth.push('\n');
                            mouth.push_str(VoiceOrchestrator::concurrent_voice_instructions());
                        }
                        match orchestrator.session_context().await {
                            Ok(ctx) => (mouth, Vec::new(), ctx.policy_snapshot_id, false, false, 0),
                            Err(err) => {
                                warn!(
                                    error = %err,
                                    "[VOICE-CONTROL] GPT-Live Magician-brain catalog snapshot failed; \
                                     small talk still works, delegate_to_chat will refuse"
                                );
                                (mouth, Vec::new(), None, false, false, 0)
                            },
                        }
                    } else {
                        // Voice-as-chat-agent (Phase A1b): one round-trip
                        // through ChatService that returns BOTH the rendered
                        // outer-loop prompt (chat outer-loop + audio modality
                        // addendum) AND the full chat tool surface. Same
                        // brain identity as the text chat agent — only the
                        // delivery shape (audio addendum) differs.
                        match orchestrator.session_context().await {
                            Ok(ctx) => (
                                ctx.instructions,
                                ctx.tools,
                                ctx.policy_snapshot_id,
                                ctx.per_turn_context_configured,
                                ctx.per_turn_context_enabled,
                                ctx.per_turn_context_budget_ms,
                            ),
                            Err(err) => {
                                warn!(
                                    error = %err,
                                    "[VOICE-CONTROL] failed to render voice session context; \
                                     falling back to empty prompt + tool catalog"
                                );
                                (String::new(), Vec::new(), None, false, false, 0)
                            },
                        }
                    };
                    let resume = match tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        orchestrator.initial_resume_context(),
                    )
                    .await
                    {
                        Ok(Ok(ctx)) => ctx,
                        Ok(Err(err)) => {
                            warn!(
                                error = %err,
                                "[VOICE-CONTROL] failed to build initial resume context; \
                                 opening realtime session without replay"
                            );
                            ResumeContext {
                                summary: None,
                                recent_turns: Vec::new(),
                                tool_exchanges: Vec::new(),
                                total_turns: 0,
                            }
                        },
                        Err(_) => {
                            warn!(
                                "[VOICE-CONTROL] initial resume compaction exceeded 2s; \
                                 opening realtime session without replay"
                            );
                            ResumeContext {
                                summary: None,
                                recent_turns: Vec::new(),
                                tool_exchanges: Vec::new(),
                                total_turns: 0,
                            }
                        },
                    };
                    let boundary = orchestrator.call_boundary().await;
                    addr.do_send(SessionReady {
                        descriptor,
                        instructions,
                        tools,
                        policy_snapshot_id,
                        per_turn_context_configured,
                        per_turn_context_enabled,
                        per_turn_context_budget_ms,
                        resume,
                        boundary,
                    });
                },
                Err(err) => {
                    addr.do_send(SessionError {
                        message: err.to_string(),
                        recoverable: false,
                    });
                },
            }
        };
        ctx.spawn(actix::fut::wrap_future(fut));
    }

    fn handle_finalized_user_transcript(
        &mut self,
        text: String,
        item_id: String,
        turn_generation: Option<u64>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> bool {
        let Some(text) = self.admit_user_transcript(&text, Some(&item_id), turn_generation, ctx)
        else {
            return false;
        };
        self.send_envelope(
            ctx,
            "transcript.user",
            json!({
                "text": text,
                "item_id": item_id,
                "turn_generation": turn_generation,
            }),
        );
        // Guided-flow commands must take over before the cascaded Hands-free
        // chat turn starts. Otherwise Hands-free would answer the words
        // "Tutor screen..." as ordinary chat while vendor Realtime entered
        // the deterministic Tutor/App Copilot lane for the exact same command.
        if self.handle_tutor_takeover_transcript(&text, Some(&item_id), ctx) {
            return true;
        }
        self.current_provider_response_id = None;
        self.tutor_response_suppression
            .begin_new_user_generation(!self.active_tutor_takeover_keys.is_empty());
        if self.concurrent_requests && VoiceOrchestrator::concurrent_voice_cancel_command(&text).is_some() {
            self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
            self.send_envelope(ctx, "response.interrupted", json!({}));
            let orchestrator = Arc::clone(&self.orchestrator);
            let context = self.captured_voice_context.clone();
            let addr = ctx.address();
            actix::spawn(async move {
                let result = orchestrator.cancel_concurrent_voice_command(&text, context.as_deref()).await.map_err(|e| e.to_string());
                addr.do_send(ConcurrentVoiceCancelled { result });
            });
            return true;
        }
        if self.hands_free && self.concurrent_requests {
            let orchestrator = Arc::clone(&self.orchestrator);
            let context = self.captured_voice_context.clone();
            let addr = ctx.address();
            // The provider item id is a retry-stable admission key. The actor
            // receives an acceptance, not a future it owns or can cancel.
            let submission_id = format!("hands-free-{}-{item_id}", self.voice_session_id);
            spawn_execution_job(move || async move {
                let result = orchestrator.submit_concurrent_voice_turn(&text, &submission_id, context)
                    .await.map_err(|error| error.to_string());
                addr.do_send(ConcurrentVoiceAccepted { result });
            });
            return true;
        }
        if self.hands_free {
            let generation = self.cascaded_turn_generation;
            self.cascaded_turn_active = true;
            let orchestrator = Arc::clone(&self.orchestrator);
            let addr = ctx.address();
            // A cascaded HandsFree utterance is a complete non-streaming chat
            // turn and can enter the agentic/tool/delegation machinery. Pass
            // an owned closure through the dedicated execution runtime so the
            // broad future is constructed and first-polled at its scheduler
            // root, never on this Actix voice worker.
            spawn_execution_job(move || async move {
                let result = orchestrator
                    .process_cascaded_voice_turn(&text)
                    .await
                    .map_err(|error| error.to_string());
                addr.do_send(CascadedTurnFinished { generation, result });
            });
            return true;
        }
        self.begin_realtime_user_turn(text, ctx);
        true
    }

    fn on_session_end(&mut self, ctx: &mut ws::WebsocketContext<Self>) {
        self.cancel_pending_turn_context();
        let identities = self.end_local_transcript();
        self.clear_local_transcript_partials(identities, "session_ended", None, ctx);
        let pending_catalog_update = self.pending_catalog_update.take();
        // Ask the provider to close before deciding anything about the call in
        // flight: its closing report is the session's own account, and for a
        // duration-billed provider it is the whole bill. See
        // `in_flight_call_failure_class` and `PROVIDER_CLOSING_REPORT_GRACE`.
        self.send_provider_control(RealtimeAudioControl::End);
        info!(
            voice_session_id = %self.voice_session_id,
            "[VOICE-CONTROL] session end: asked the provider to close, holding for its closing report"
        );
        let orchestrator = Arc::clone(&self.orchestrator);
        let fut = async move {
            if let Some(pending) = pending_catalog_update {
                let output = json!({
                    "status": "cancelled",
                    "error_code": "voice_session_ended_before_catalog_ack",
                    "reason": "The voice session ended before the prepared tool catalog was acknowledged."
                })
                .to_string();
                orchestrator
                    .abort_tool_catalog_update(&pending.update)
                    .await;
                orchestrator
                    .finalize_voice_catalog_tool_result(
                        &pending.tool_name,
                        &pending.call_id,
                        &output,
                        None,
                    )
                    .await;
            }
        };
        ctx.spawn(actix::fut::wrap_future(fut));
        self.send_envelope(ctx, "session.ended", json!({}));
        // Stay up for the closing report, then end the call the orchestrator
        // still holds and stop. A call still open after the grace is closed
        // without claiming a failure it did not have.
        ctx.run_later(PROVIDER_CLOSING_REPORT_GRACE, |actor, ctx| {
            match in_flight_call_failure_class(SessionEndKind::ClientRequested) {
                Some(error_class) => actor.observe_failed_realtime_llm_call(error_class, ctx),
                None => actor.reset_realtime_llm_call(),
            }
            let orchestrator = Arc::clone(&actor.orchestrator);
            actix::spawn(async move {
                orchestrator.end().await;
            });
            ctx.stop();
        });
    }

    fn on_session_rotate(&mut self, body: &Value, ctx: &mut ws::WebsocketContext<Self>) {
        if !self.started {
            return;
        }
        let reason = match body
            .get("reason")
            .and_then(|v| v.as_str())
            .unwrap_or("manual")
        {
            "proactive" => RotateReason::Proactive,
            "watermark" => RotateReason::Watermark,
            "reconnect" => RotateReason::Reconnect,
            _ => RotateReason::Manual,
        };
        self.spawn_rotation(reason, ctx);
    }

    fn on_turn_boundary_change(&mut self, body: &Value, ctx: &mut ws::WebsocketContext<Self>) {
        if !self.started || self.hands_free {
            return;
        }
        let Some(mode) = requested_realtime_turn_detection(body, false) else {
            self.send_envelope(
                ctx,
                "session.error",
                json!({
                    "message": "Unsupported realtime turn boundary.",
                    "recoverable": true,
                }),
            );
            return;
        };
        self.spawn_rotation_with_turn_detection(RotateReason::Reconnect, Some(mode), ctx);
    }

    fn on_user_text(&mut self, body: &Value, _ctx: &mut ws::WebsocketContext<Self>) {
        if !self.started {
            return;
        }
        let Some(text) = body.get("text").and_then(|v| v.as_str()) else {
            return;
        };
        let text = text.to_string();
        // When the client sets `request_response: true`, the text is both a
        // user turn AND a prompt the live realtime session should answer aloud.
        // Browsers omit the flag (they drive turns via PTT/committed audio) so
        // they're unaffected. Headless clients that own their own wake
        // detection (the meeting bot) use this to voice a reply from a
        // locally-transcribed utterance without streaming raw mic audio.
        let request_response = body
            .get("request_response")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        // Push the text into the LIVE backend-proxied session when asked.
        // `request_response: true` implies inject + voice a reply now.
        // `inject: true` alone seeds a silent conversation item only on
        // providers that support runtime silent text injection; Gemini keeps
        // silent resume seeding on the separate initial-history control path.
        // Browsers send neither, so they're unaffected (ingest-only).
        let inject = body
            .get("inject")
            .and_then(|v| v.as_bool())
            .unwrap_or(request_response);
        // Caller-authored protocol text may drive ordinary voice, but it is
        // not a provider-observed finalized owner utterance and cannot inherit
        // the prior exact governed Apps turn.
        self.cancel_pending_turn_context();
        let orchestrator = Arc::clone(&self.orchestrator);
        let ingest_text = text.clone();
        actix::spawn(async move {
            if let Err(err) = orchestrator.ingest_untrusted_user_text(&ingest_text).await {
                warn!(error = %err, "[VOICE-CONTROL] user.text ingest failed");
            }
        });
        if inject {
            if request_response {
                self.begin_realtime_llm_call();
            }
            self.send_or_queue_provider_control(RealtimeAudioControl::InjectSystemMessage {
                text,
                request_response,
            });
        }
    }

    fn on_screen_state(&mut self, body: &Value, ctx: Option<&mut ws::WebsocketContext<Self>>) {
        let Some(locked) = requested_screen_locked(body) else {
            return;
        };
        let was_locked = self.screen_locked;
        apply_screen_lock_state(
            &mut self.screen_locked,
            &mut self.locked_tutor_rejection_keys,
            locked,
        );
        if locked && !was_locked {
            let cancelled_feature = cancel_active_tutor_takeovers_for_lock(
                &self.active_tutor_takeover_cancellations,
                &mut self.lock_cancelled_tutor_takeover_keys,
                &mut self.locked_tutor_rejection_keys,
            );
            if let (Some(feature_mode), Some(ctx)) = (cancelled_feature, ctx) {
                self.report_tutor_takeover_failure_with_message(
                    "guided flow cancelled because the client screen locked while it was starting",
                    locked_guided_flow_message(feature_mode),
                    ctx,
                );
            }
        }
    }

    fn on_transcript(
        &mut self,
        direction: ChatMessageDirection,
        body: &Value,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        if !self.started {
            return;
        }
        let Some(text) = body.get("text").and_then(|v| v.as_str()) else {
            return;
        };
        let mut text = text.to_string();

        // `transcript.user` is a caller-authored compatibility frame used by
        // direct P2P clients, where Magician has no server-owned provider
        // observer. A governed Apps credential is issued only for a
        // backend-proxied provider, whose finalized user transcript arrives
        // through `RealtimeProviderEvent::UserTranscriptFinal` below. Never
        // let a client forge that provider-observed boundary and mint an
        // owner-authorized turn on the backend path.
        if direction == ChatMessageDirection::User
            && matches!(
                self.audio_topology,
                Some(RealtimeAudioTopology::BackendProxied)
            )
        {
            debug!(
                voice_session_id = %self.voice_session_id,
                "[VOICE-CONTROL] ignored caller transcript on backend-proxied session"
            );
            return;
        }

        if direction == ChatMessageDirection::Assistant {
            let response_id = body
                .get("response_id")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty());
            let suppress = self.tutor_response_suppression.observe_provider_terminal(
                response_id,
                !self.active_tutor_takeover_keys.is_empty(),
            );
            if suppress {
                if self.current_provider_response_id.as_deref() == response_id {
                    self.current_provider_response_id = None;
                }
                debug!(
                    voice_session_id = %self.voice_session_id,
                    "[VOICE-CONTROL] suppressed assistant transcript after tutor takeover"
                );
                return;
            }
            if self.current_provider_response_id.is_none() {
                self.current_provider_response_id = response_id.map(str::to_string);
            }
        }
        if direction == ChatMessageDirection::Assistant
            && self.suppress_ambient_assistant_transcript
        {
            self.suppress_ambient_assistant_transcript = false;
            return;
        }

        if direction == ChatMessageDirection::User {
            let item_id = body.get("item_id").and_then(Value::as_str);
            let turn_generation = body.get("turn_generation").and_then(Value::as_u64);
            let Some(admitted) = self.admit_user_transcript(&text, item_id, turn_generation, ctx)
            else {
                return;
            };
            text = admitted;
            self.send_envelope(
                ctx,
                "transcript.user",
                json!({
                    "text": text,
                    "item_id": item_id,
                    "turn_generation": turn_generation,
                }),
            );
        }

        if direction == ChatMessageDirection::User
            && self.handle_tutor_takeover_transcript(
                &text,
                body.get("item_id").and_then(Value::as_str),
                ctx,
            )
        {
            return;
        }

        if direction == ChatMessageDirection::User {
            self.current_provider_response_id = None;
            self.tutor_response_suppression
                .begin_new_user_generation(!self.active_tutor_takeover_keys.is_empty());
            self.begin_realtime_user_turn(text, ctx);
        } else {
            // Direct-P2P assistant caption: the audio rode WebRTC and never
            // crossed magician, so this utterance gets the fixed fallback
            // echo window instead of a byte-derived one.
            self.echo_suppressor
                .note_assistant_final(None, &text, std::time::Instant::now());
            let orchestrator = Arc::clone(&self.orchestrator);
            actix::spawn(async move {
                if let Err(err) = orchestrator.ingest_transcript_turn(direction, &text).await {
                    warn!(error = %err, "[VOICE-CONTROL] transcript ingest failed");
                }
            });
        }
    }

    fn handle_tutor_takeover_transcript(
        &mut self,
        text: &str,
        item_id: Option<&str>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) -> bool {
        let Some(request) = voice_tutor_takeover_request(text, self.guided_voice_capabilities)
        else {
            return false;
        };
        let key = tutor_takeover_key(&self.voice_session_id, item_id, &request.text);
        if self.screen_locked {
            if !admit_locked_guided_flow_rejection(&mut self.locked_tutor_rejection_keys, key) {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    "[VOICE-CONTROL] duplicate locked-screen guided-flow transcript ignored"
                );
                return true;
            }
            self.tutor_response_suppression.begin(
                self.current_provider_response_id
                    .take()
                    .or_else(|| self.assistant_audio_response_id.clone()),
            );
            self.observe_failed_realtime_llm_call("cancelled", ctx);
            self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
            let spoken = locked_guided_flow_message(request.feature_mode);
            self.report_tutor_takeover_failure_with_message(
                "guided flow blocked because the client screen is locked",
                spoken,
                ctx,
            );
            return true;
        }
        if self.completed_tutor_takeover_keys.contains(&key) {
            debug!(
                voice_session_id = %self.voice_session_id,
                "[VOICE-CONTROL] completed tutor takeover transcript retry ignored"
            );
            return true;
        }
        if !self.active_tutor_takeover_keys.insert(key.clone()) {
            debug!(
                voice_session_id = %self.voice_session_id,
                "[VOICE-CONTROL] duplicate tutor takeover transcript ignored"
            );
            return true;
        }
        if self.active_tutor_takeover_keys.len() > 1 {
            self.active_tutor_takeover_keys.remove(&key);
            self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
            self.report_tutor_takeover_failure(
                "another guided voice flow is already starting; please try again when it finishes",
                ctx,
            );
            return true;
        }
        if let Some(error) = request.rejection {
            self.active_tutor_takeover_keys.remove(&key);
            self.remember_completed_tutor_takeover_key(key);
            self.tutor_response_suppression.begin(
                self.current_provider_response_id
                    .take()
                    .or_else(|| self.assistant_audio_response_id.clone()),
            );
            self.observe_failed_realtime_llm_call("cancelled", ctx);
            self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
            self.report_tutor_takeover_failure(error, ctx);
            return true;
        }
        self.tutor_response_suppression.begin(
            self.current_provider_response_id
                .take()
                .or_else(|| self.assistant_audio_response_id.clone()),
        );
        self.observe_failed_realtime_llm_call("cancelled", ctx);
        self.send_or_queue_provider_control(RealtimeAudioControl::InterruptResponse);
        self.send_envelope(
            ctx,
            "tutor.takeover.started",
            json!({
                "voice_session_id": self.voice_session_id.as_str(),
                "reason": request.reason,
                "feature_mode": request.feature_mode.as_str(),
                "canvas_mode": request.canvas_mode.as_str(),
                "quick": request.quick,
                "text": request.text.as_str(),
                "client_handoff": request.client_blackboard_handoff,
            }),
        );
        if request.client_blackboard_handoff {
            self.active_tutor_takeover_keys.remove(&key);
            self.remember_completed_tutor_takeover_key(key);
            self.tutor_response_suppression.finish_takeover(false);
            return true;
        }
        let orchestrator = Arc::clone(&self.orchestrator);
        let addr = ctx.address();
        let takeover_text = request.text;
        let feature_mode = request.feature_mode;
        let capture_screen = request.capture_screen;
        let takeover_cancellation = tokio_util::sync::CancellationToken::new();
        self.active_tutor_takeover_cancellations
            .insert(key.clone(), (takeover_cancellation.clone(), feature_mode));
        // Tutor/App Copilot takeover enters the same complete Chat -> memory ->
        // tool/delegation state machine as a typed guided turn. Transfer an
        // owned closure before that future is constructed so the dedicated
        // execution runtime owns its first poll; an Actix voice worker must
        // only schedule the job and receive its small terminal message.
        spawn_execution_job(move || async move {
            let result = orchestrator
                .submit_tutor_takeover_turn(
                    &takeover_text,
                    feature_mode,
                    capture_screen,
                    takeover_cancellation,
                )
                .await;
            addr.do_send(TutorTakeoverFinished {
                key,
                error: result.err().map(|err| err.to_string()),
            });
        });
        true
    }

    fn remember_completed_tutor_takeover_key(&mut self, key: String) {
        if !self.completed_tutor_takeover_keys.contains(&key) {
            self.completed_tutor_takeover_keys.push_back(key);
        }
        while self.completed_tutor_takeover_keys.len() > COMPLETED_TUTOR_TAKEOVER_DEDUPE_MAX {
            self.completed_tutor_takeover_keys.pop_front();
        }
    }

    fn report_tutor_takeover_failure(&mut self, error: &str, ctx: &mut ws::WebsocketContext<Self>) {
        let spoken = unsupported_guided_flow_message(error);
        self.report_tutor_takeover_failure_with_message(error, spoken, ctx);
    }

    fn report_tutor_takeover_failure_with_message(
        &mut self,
        error: &str,
        spoken: &str,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        self.tutor_response_suppression
            .prepare_failure_announcement();
        let backend_announced = matches!(
            self.audio_topology,
            Some(RealtimeAudioTopology::BackendProxied)
        );
        if backend_announced {
            self.send_or_queue_provider_control(RealtimeAudioControl::InjectSystemMessage {
                text: spoken.to_string(),
                request_response: true,
            });
        }
        self.send_envelope(
            ctx,
            "tutor.takeover.failed",
            json!({
                "error": error,
                "message": spoken,
                "backend_announced": backend_announced,
            }),
        );
    }

    fn on_tool_dispatch(&mut self, body: &Value, ctx: &mut ws::WebsocketContext<Self>) {
        if !self.started {
            return;
        }
        let Some(tool_name) = body.get("tool_name").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(arguments_json) = body.get("arguments_json").and_then(|v| v.as_str()) else {
            return;
        };
        let Some(call_id) = body.get("call_id").and_then(|v| v.as_str()) else {
            return;
        };
        let response_id = body
            .get("response_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|id| !id.is_empty());
        if self
            .tutor_response_suppression
            .matches_or_claims_response(response_id)
        {
            self.send_envelope(
                ctx,
                "tool.result",
                json!({
                    "call_id": call_id,
                    "output": json!({ "error": "guided voice flow owns this turn" }).to_string(),
                    "voice_summary": "",
                    "status": "rejected",
                }),
            );
            self.send_envelope(
                ctx,
                "response.interrupted",
                json!({ "response_id": response_id }),
            );
            return;
        }
        self.begin_realtime_llm_call();
        if let Some(response_id) = response_id {
            self.current_provider_response_id = Some(response_id.to_string());
        }
        if self.suppress_ambient_assistant_transcript {
            self.send_envelope(
                ctx,
                "tool.result",
                json!({
                    "call_id": call_id,
                    "output": json!({ "error": "voice address phrase required" }).to_string(),
                    "voice_summary": "",
                    "status": "rejected",
                }),
            );
            return;
        }
        let tool_name = tool_name.to_string();
        let arguments_json = arguments_json.to_string();
        let call_id = call_id.to_string();
        let orchestrator = Arc::clone(&self.orchestrator);
        let addr = ctx.address();
        let fut = async move {
            let result = orchestrator
                .dispatch_tool(tool_name, arguments_json, call_id)
                .await;
            match result {
                Ok(resp) => {
                    addr.do_send(ToolResult {
                        tool_name: resp.tool_name,
                        call_id: resp.call_id,
                        output: resp.output,
                        voice_summary: resp.voice_summary,
                        status: format!("{:?}", resp.status).to_ascii_lowercase(),
                        catalog_update: resp.catalog_update,
                        projected_result: resp.projected_result,
                        provider_delivery_fence: resp.provider_delivery_fence,
                    });
                },
                Err(err) => {
                    addr.do_send(SessionError {
                        message: err.to_string(),
                        recoverable: true,
                    });
                },
            }
        };
        ctx.spawn(actix::fut::wrap_future(fut));
    }

    fn on_token_usage(&mut self, body: &Value, ctx: &mut ws::WebsocketContext<Self>) {
        if !self.started {
            return;
        }
        let response_id = body.get("response_id").and_then(Value::as_str);
        let targets_current = self.provider_terminal_targets_current_response(response_id);
        self.observe_provider_response_lifecycle_terminal(response_id);
        let input_tokens = body.get("input_tokens").and_then(|v| v.as_u64());
        let output_tokens = body.get("output_tokens").and_then(|v| v.as_u64());
        let usage = realtime_usage_from_control(body);
        // Prefer the client-reported window (it knows which model the
        // current upstream session is on). Falls back to the value
        // the orchestrator latched from the profile on `start()`;
        // ultimately to a conservative 32K if neither is configured.
        let client_window = body.get("context_window_tokens").and_then(|v| v.as_u64());
        let (ttfa_ms, correlation, started_at_ms, response_ms) = if targets_current {
            (
                self.turn_ttfa_ms.take(),
                self.current_llm_correlation.take(),
                self.current_llm_started_at_ms.take(),
                self.turn_started_at
                    .take()
                    .map(|started| started.elapsed().as_millis() as u64),
            )
        } else {
            (None, None, None, None)
        };
        let orchestrator = Arc::clone(&self.orchestrator);
        let addr = ctx.address();
        let fut = async move {
            let context_window = client_window
                .or(orchestrator.context_window_tokens().await)
                .unwrap_or(32_000);
            if orchestrator
                .observe_token_usage(
                    input_tokens,
                    output_tokens,
                    usage,
                    ttfa_ms,
                    response_ms,
                    context_window,
                    correlation,
                    started_at_ms,
                )
                .await
            {
                addr.do_send(InternalRotate(RotateReason::Watermark));
            }
        };
        ctx.spawn(actix::fut::wrap_future(fut));
    }

    fn spawn_rotation(&mut self, reason: RotateReason, ctx: &mut ws::WebsocketContext<Self>) {
        self.spawn_rotation_with_turn_detection(reason, None, ctx);
    }

    fn spawn_rotation_with_turn_detection(
        &mut self,
        reason: RotateReason,
        turn_detection_override: Option<String>,
        ctx: &mut ws::WebsocketContext<Self>,
    ) {
        if self.rotation_in_progress {
            debug!(
                voice_session_id = %self.voice_session_id,
                reason = ?reason,
                "[VOICE-CONTROL] rotation already in progress; ignoring duplicate trigger"
            );
            return;
        }
        self.rotation_in_progress = true;
        let orchestrator = Arc::clone(&self.orchestrator);
        let addr = ctx.address();
        // Surface rotating status synchronously so the frontend can
        // update its overlay before the async work finishes.
        let reason_label = format!("{:?}", reason).to_ascii_lowercase();
        let preface = json!({ "reason": reason_label });
        // Push directly via the actor (avoids `self.send_envelope` borrow):
        addr.do_send(Rotating {
            reason: reason_label,
        });
        let _ = preface;
        let fut = async move {
            let previous_turn_detection_override = if let Some(mode) = turn_detection_override {
                match orchestrator
                    .replace_turn_detection_override(Some(mode))
                    .await
                {
                    Ok(previous) => Some(previous),
                    Err(error) => {
                        addr.do_send(SessionError {
                            message: error.to_string(),
                            recoverable: true,
                        });
                        return;
                    },
                }
            } else {
                None
            };
            let delays_ms = [0_u64, 250, 750];
            let mut last_error = None;
            for delay_ms in delays_ms {
                if delay_ms > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                }
                match orchestrator.rotate(reason).await {
                    Ok(result) => {
                        addr.do_send(AudioRebind {
                            descriptor: result.descriptor,
                            resume: result.resume,
                            rotation_count: result.rotation_count,
                            new_audio_channel: result.new_audio_channel,
                        });
                        return;
                    },
                    Err(error) => last_error = Some(error.to_string()),
                }
            }
            if let Some(previous) = previous_turn_detection_override {
                let _ = orchestrator.replace_turn_detection_override(previous).await;
            }
            addr.do_send(SessionError {
                message: last_error
                    .unwrap_or_else(|| "realtime provider reconnect failed".to_string()),
                recoverable: true,
            });
        };
        ctx.spawn(actix::fut::wrap_future(fut));
    }
}

fn should_suppress_half_duplex_input(
    hands_free: bool,
    half_duplex: bool,
    assistant_audio_active: bool,
) -> bool {
    hands_free && half_duplex && assistant_audio_active
}

fn new_realtime_llm_correlation(
    principal: impl Into<String>,
    workspace: impl Into<String>,
) -> magician::magician_v2::realtime_events::LlmEventCorrelation {
    magician::magician_v2::realtime_events::LlmEventCorrelation::direct(
        principal,
        workspace,
        magicllm::LlmWorkloadClass::ForegroundChat,
    )
}

fn realtime_usage_from_control(body: &Value) -> Option<magicllm::types::RealtimeUsage> {
    body.get("usage")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok())
}

// ── Internal actor messages ─────────────────────────────────────────

#[derive(Message)]
#[rtype(result = "()")]
struct SessionReady {
    descriptor: magicllm::realtime::RealtimeSessionDescriptor,
    instructions: String,
    /// Full chat-agent tool surface for the active scope. Frontend
    /// installs these on the realtime upstream via `session.update`
    /// (OpenAI Realtime) or the provider-native equivalent.
    tools: Vec<LLMToolSpec>,
    /// Immutable effective-policy snapshot that produced `tools`.
    policy_snapshot_id: Option<String>,
    per_turn_context_configured: bool,
    per_turn_context_enabled: bool,
    per_turn_context_budget_ms: u64,
    /// Initial replay payload. Same shape as `audio.rebind.resume`
    /// so a fresh voice call opened mid-thread receives prior chat
    /// context before the first live utterance.
    resume: ResumeContext,
    /// Boundary the backend resolved for this call. Read from the live
    /// `CallState` rather than recomputed here, so what the client is shown and
    /// what authorization actually enforces cannot disagree.
    boundary: Option<magician_media::media_rails::voice_orchestrator::CallBoundary>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct AudioRebind {
    descriptor: magicllm::realtime::RealtimeSessionDescriptor,
    /// Compacted resume payload the frontend replays into the new
    /// upstream session via `conversation.item.create` once its
    /// data channel opens. Critical for context continuity across
    /// rotation — without it the model loses the conversation.
    resume: magician_media::media_rails::ResumeContext,
    rotation_count: u32,
    /// Fresh audio channel for `BackendProxied` rotations. The
    /// handler swaps `upstream_audio_tx` to the new sender +
    /// spawns a new downstream forwarder. `None` for
    /// `DirectPeerToPeer` rotations (frontend swaps the WebRTC
    /// peer itself; no backend channel involved).
    new_audio_channel: Option<magicllm::realtime::AudioStreamChannel>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct Rotating {
    reason: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct SessionError {
    message: String,
    recoverable: bool,
}

#[derive(Message)]
#[rtype(result = "()")]
struct ToolResult {
    tool_name: String,
    call_id: String,
    output: String,
    voice_summary: String,
    status: String,
    catalog_update: Option<ExternalToolCatalogUpdate>,
    projected_result: Option<magician::magician_v2::tool_result_projection::ProjectedToolResultV1>,
    provider_delivery_fence:
        Option<magician::magician_v2::apps::boundary::AppRealtimeVoiceDeliveryFence>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct CatalogUpdateCommitted {
    update_id: String,
    result: Result<ExternalToolCatalogUpdate, String>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct CatalogUpdateAckTimeout {
    update_id: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct TutorTakeoverFinished {
    key: String,
    error: Option<String>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct ProviderReadyTimeout {
    generation: u64,
}

#[derive(Message)]
#[rtype(result = "()")]
struct LocalFallbackConfigureFailed {
    update_id: String,
    reason: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct LocalFallbackConfigureAckTimeout {
    update_id: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct InternalRotate(RotateReason);

#[derive(Message)]
#[rtype(result = "()")]
struct AttachBridgeStream(tokio::sync::mpsc::UnboundedReceiver<VoiceDownstreamMessage>);

/// Attach a `BackendProxied` provider's audio channel. The actor
/// holds the upstream sender (used for browser → provider PCM) and
/// installs a stream handler for the downstream receiver (used to
/// emit provider → browser PCM as WS binary frames).
#[derive(Message)]
#[rtype(result = "()")]
struct AttachAudioChannel(magicllm::realtime::AudioStreamChannel);

#[derive(Message)]
#[rtype(result = "()")]
struct AttachLocalTranscript {
    handle: LocalRealtimeTranscriptHandle,
    events: tokio::sync::mpsc::Receiver<LocalRealtimeTranscriptEvent>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct BeginLocalTranscript {
    vendor_fallback_model: Option<String>,
    manual_turns: bool,
}

#[derive(Message)]
#[rtype(result = "()")]
struct LocalTranscriptStartFailed {
    reason: String,
}

#[derive(Message)]
#[rtype(result = "()")]
struct LocalTranscriptEventFrame(LocalRealtimeTranscriptEvent);

#[derive(Message)]
#[rtype(result = "()")]
struct LocalProviderCommitQueued {
    stream_generation: u64,
    result: Result<(), String>,
}

/// One PCM frame emitted by the provider that needs to reach the
/// browser. Forwarded as a binary WS frame.
#[derive(Message)]
#[rtype(result = "()")]
struct ProviderAudioFrame(Vec<u8>);

/// One semantic event emitted by a backend-proxied provider. The
/// actor translates it into the same frontend envelopes and ledger
/// writes the direct-P2P browser adapter already produces.
#[derive(Message)]
#[rtype(result = "()")]
struct ProviderRealtimeEventFrame(RealtimeProviderEvent);

#[derive(Message)]
#[rtype(result = "()")]
struct TurnContextPrepared {
    generation: u64,
    result: Result<RealtimeTurnContext, String>,
    provider_delivery_fence:
        Option<magician::magician_v2::apps::boundary::AppRealtimeVoiceDeliveryFence>,
}

#[derive(Message)]
#[rtype(result = "()")]
struct CascadedTurnFinished {
    generation: u64,
    result: Result<CascadedVoiceTurnOutcome, String>,
}

fn session_ready_payload(
    voice_session_id: &str,
    descriptor: magicllm::realtime::RealtimeSessionDescriptor,
    instructions: String,
    tools: Vec<LLMToolSpec>,
    policy_snapshot_id: Option<String>,
    per_turn_context_enabled: bool,
    per_turn_context_budget_ms: u64,
    resume: ResumeContext,
    addressing: &VoiceAddressing,
    assistant_identity: &magician::magician_v2::presentation_identity::AgentPresentationIdentity,
    boundary: Option<&magician_media::media_rails::voice_orchestrator::CallBoundary>,
) -> Value {
    json!({
        "voice_session_id": voice_session_id,
        "descriptor": descriptor,
        "instructions": instructions,
        "tools": tools,
        "policy_snapshot_id": policy_snapshot_id,
        "per_turn_context": {
            "enabled": per_turn_context_enabled,
            "budget_ms": per_turn_context_budget_ms,
        },
        "resume": resume,
        "rotation_count": 0,
        "agent": assistant_identity,
        "addressing": {
            "required": addressing.required(),
            "activation_phrases": addressing.activation_phrases(),
            "follow_up_window_ms": addressing.follow_up_window_ms(),
        },
        // What the backend resolved this call to be. Strictly informational:
        // `elevatable` is a constant false, and there is deliberately no field
        // a client could set to request a different surface. A control that
        // implied the boundary were negotiable would be worse than showing
        // nothing, because it would suggest the room can ask to be trusted.
        "boundary": boundary.map(|b| json!({
            "surface": b.surface,
            "audience": b.audience,
            "agent_id": b.agent_id,
            "elevatable": false,
        })),
    })
}

fn format_resume_summary_for_provider(resume: &ResumeContext) -> Option<String> {
    let summary = resume.summary.as_deref().unwrap_or_default().trim();
    if summary.is_empty() {
        return None;
    }
    Some(format!(
        "Conversation resume summary for this realtime voice session (reference only; do not re-run completed actions):\nSummary: {summary}"
    ))
}

fn format_resume_recent_turns_for_provider(resume: &ResumeContext) -> Option<String> {
    let mut lines = vec!["Recent conversation turns:".to_string()];
    for turn in &resume.recent_turns {
        let role = if turn.role.eq_ignore_ascii_case("assistant") {
            "assistant"
        } else {
            "user"
        };
        let text = turn.text.trim();
        if !text.is_empty() {
            lines.push(format!("{role}: {text}"));
        }
    }
    if lines.len() == 1 {
        return None;
    }
    Some(lines.join("\n"))
}

fn provider_resume_controls(
    resume: ResumeContext,
    provider: Option<RealtimeProviderKind>,
) -> Vec<RealtimeAudioControl> {
    let summary = format_resume_summary_for_provider(&resume);
    let recent = format_resume_recent_turns_for_provider(&resume);
    let mut controls = Vec::with_capacity(resume.tool_exchanges.len() + 2);
    if let Some(text) = summary {
        controls.push(RealtimeAudioControl::InjectInitialHistory { text });
    }
    if matches!(
        provider,
        Some(RealtimeProviderKind::OpenAi | RealtimeProviderKind::Grok)
    ) {
        controls.extend(resume.tool_exchanges.into_iter().map(|exchange| {
            RealtimeAudioControl::InjectToolExchange {
                call_id: exchange.call_id,
                tool_name: exchange.tool_name,
                arguments: magician::magician_v2::secrets::injection::sanitize_json_for_provider(
                    &exchange.arguments,
                ),
                projected_result:
                    magician::magician_v2::secrets::injection::sanitize_json_for_provider(
                        &exchange.projected_result,
                    ),
            }
        }));
    } else if !resume.tool_exchanges.is_empty() {
        warn!(
            provider = ?provider,
            omitted_exchange_count = resume.tool_exchanges.len(),
            "[VOICE-CONTROL] active provider lacks native historical tool-exchange replay; refusing unsafe text downgrade"
        );
    }
    if let Some(text) = recent {
        controls.push(RealtimeAudioControl::InjectInitialHistory { text });
    }
    controls
}

impl Handler<SessionReady> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: SessionReady, ctx: &mut Self::Context) -> Self::Result {
        self.started = true;
        self.audio_topology = Some(msg.descriptor.topology);
        self.realtime_provider = Some(msg.descriptor.provider);
        let translation_mode =
            msg.descriptor.mode == magicllm::config::RealtimeVoiceMode::Translation;
        if translation_mode {
            // A translator is not an assistant: address-prefix gating would
            // discard ordinary source speech and tools/instructions are not
            // supported by Gemini Live Translate.
            self.addressing = VoiceAddressing::new(false, Vec::<String>::new());
        }
        let local_transcript_requested = descriptor_requests_local_transcript(&msg.descriptor);
        let stale_identities = self.local_transcript.configure_requested(
            local_transcript_requested,
            msg.descriptor.transcription_fallback_model.clone(),
            descriptor_uses_manual_turns(&msg.descriptor),
        );
        self.clear_local_transcript_partials(
            stale_identities,
            "local_transcript_disabled",
            None,
            ctx,
        );
        let mut instructions = if translation_mode {
            String::new()
        } else {
            msg.instructions
        };
        if !translation_mode {
            if let Some(addressing_instruction) = self.addressing.provider_instruction() {
                if !instructions.trim().is_empty() {
                    instructions.push_str("\n\n");
                }
                instructions.push_str(&addressing_instruction);
            }
        }
        self.session_instructions = Some(instructions.clone());
        let tools = if translation_mode {
            Vec::new()
        } else {
            msg.tools
        };
        self.session_tools = tools.clone();
        self.per_turn_context_configured = msg.per_turn_context_configured;
        self.per_turn_context_enabled = !translation_mode && msg.per_turn_context_enabled;
        self.per_turn_context_budget_ms = msg.per_turn_context_budget_ms;
        self.configure_provider_session_and_flush(Some(msg.resume.clone()), ctx);
        // Content-free boundary telemetry, emitted once per call at the moment
        // the surface, the resolved agent and the admitted catalog are all
        // known. Everything here is a server-derived label, a count, or a hash:
        // no thread name, no meeting title, nothing anyone said.
        if let Some(boundary) = msg.boundary.as_ref() {
            self.broadcaster.emit_named(
                MEDIA_VOICE_SURFACE_RESOLVED,
                MEDIA_SYSTEM_AGENT,
                Some(&self.principal),
                Some(&self.workspace),
                json!({
                    "voice_session_id": self.voice_session_id,
                    "source_surface": boundary.source_surface,
                    "surface": boundary.surface,
                    "audience": boundary.audience,
                    "derivation_reason": self.surface_derivation_reason,
                    "agent_id": boundary.agent_id,
                    "binding": boundary.binding,
                    // The catalog-level admission count is the boundary signal
                    // that matters: a room advertising a large tool surface is
                    // the alarm. Dispatch-time refusals are not counted here
                    // because the voice layer cannot tell an authorization
                    // refusal from an ordinary tool error.
                    "tools_admitted": self.session_tools.len(),
                    "per_turn_context_enabled": self.per_turn_context_enabled,
                    "rotation_count": 0,
                }),
            );
        }
        let turn_detection = msg.descriptor.turn_detection_mode.clone();
        let payload = session_ready_payload(
            &self.voice_session_id,
            msg.descriptor,
            instructions,
            tools,
            msg.policy_snapshot_id,
            self.per_turn_context_enabled,
            self.per_turn_context_budget_ms,
            msg.resume,
            &self.addressing,
            &self.assistant_identity,
            msg.boundary.as_ref(),
        );
        self.send_envelope(ctx, "session.ready", payload);
        info!(
            voice_session_id = %self.voice_session_id,
            topology = ?self.audio_topology,
            provider = ?self.realtime_provider,
            tools = self.session_tools.len(),
            per_turn_context = self.per_turn_context_enabled,
            turn_detection = ?turn_detection,
            "[VOICE-CONTROL] session.ready"
        );
        if local_transcript_requested {
            if let Some(reason) = self.local_transcript.fallback_reason().map(str::to_owned) {
                self.local_transcript.mark_vendor_fallback_active();
                self.emit_local_transcript_telemetry(
                    MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE,
                    json!({
                        "state": "vendor_fallback_active",
                        "error_class": &reason,
                        "vendor_fallback_active": true,
                    }),
                );
                warn!(
                    voice_session_id = %self.voice_session_id,
                    error_class = %reason,
                    "[VOICE-CONTROL] local transcript unavailable at startup; using provider transcription"
                );
                self.send_envelope(
                    ctx,
                    "session.error",
                    json!({
                        "message": "Local captions are unavailable; using provider transcription.",
                        "recoverable": true,
                    }),
                );
            }
        }
    }
}

impl Handler<AudioRebind> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: AudioRebind, ctx: &mut Self::Context) -> Self::Result {
        self.rotation_in_progress = false;
        self.audio_topology = Some(msg.descriptor.topology);
        self.realtime_provider = Some(msg.descriptor.provider);
        let stale_identities = self.local_transcript.configure_requested(
            descriptor_requests_local_transcript(&msg.descriptor),
            msg.descriptor.transcription_fallback_model.clone(),
            descriptor_uses_manual_turns(&msg.descriptor),
        );
        self.clear_local_transcript_partials(
            stale_identities,
            "local_transcript_disabled",
            None,
            ctx,
        );
        self.per_turn_context_enabled = self.per_turn_context_configured
            && provider_supports_turn_context_gate(&msg.descriptor);
        // For BackendProxied rotations, swap the audio channel
        // halves under the WS actor BEFORE notifying the frontend.
        // The prior `upstream_audio_tx` is dropped (closes the old
        // upstream's input channel from our side; the prior
        // downstream forwarder will exit when its receiver hits
        // None and the prior provider session tears down). The
        // frontend continues streaming PCM without interruption;
        // the seam is invisible at the audio level.
        if let Some(channel) = msg.new_audio_channel {
            self.provider_configured = false;
            self.provider_transport_ready = false;
            self.upstream_audio_tx = Some(channel.upstream_tx);
            self.provider_control_tx = Some(channel.control_tx.clone());
            self.install_provider_control_dispatcher(channel.control_tx);
            let addr = ctx.address();
            let mut downstream_rx = channel.downstream_rx;
            actix::spawn(async move {
                while let Some(frame) = downstream_rx.recv().await {
                    addr.do_send(ProviderAudioFrame(frame));
                }
            });
            let addr = ctx.address();
            let mut events_rx = channel.events_rx;
            actix::spawn(async move {
                while let Some(event) = events_rx.recv().await {
                    addr.do_send(ProviderRealtimeEventFrame(event));
                }
            });
            let replay = if msg.descriptor.native_resume_handle.is_some() {
                None
            } else {
                Some(msg.resume.clone())
            };
            self.configure_provider_session_and_flush(replay, ctx);
        } else {
            self.provider_configured = false;
            self.provider_configuring = false;
            self.provider_transport_ready = false;
            self.upstream_audio_tx = None;
            self.provider_control_tx = None;
            self.provider_control_dispatch_tx = None;
            self.clear_pending_provider_frames();
        }
        self.send_envelope(
            ctx,
            "audio.rebind",
            json!({
                "descriptor": msg.descriptor,
                "resume": msg.resume,
                "rotation_count": msg.rotation_count,
                "tools": &self.session_tools,
                "per_turn_context": {
                    "enabled": self.per_turn_context_enabled,
                    "budget_ms": self.per_turn_context_budget_ms,
                },
                "catalog_update": self
                    .pending_catalog_update
                    .as_ref()
                    .filter(|pending| pending.awaiting_rotation)
                    .map(|pending| &pending.update),
            }),
        );
    }
}

impl Handler<Rotating> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: Rotating, ctx: &mut Self::Context) -> Self::Result {
        self.send_envelope(ctx, "session.rotating", json!({ "reason": msg.reason }));
    }
}

impl Handler<SessionError> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: SessionError, ctx: &mut Self::Context) -> Self::Result {
        self.rotation_in_progress = false;
        self.send_envelope(
            ctx,
            "session.error",
            json!({ "message": msg.message, "recoverable": msg.recoverable }),
        );
        if !msg.recoverable {
            ctx.stop();
        }
    }
}

impl Handler<ToolResult> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ToolResult, ctx: &mut Self::Context) -> Self::Result {
        if let Some(fence) = msg.provider_delivery_fence.as_ref() {
            if !matches!(
                self.audio_topology,
                Some(RealtimeAudioTopology::BackendProxied)
            ) || fence.ensure_current(chrono::Utc::now()).is_err()
            {
                let output = json!({
                    "status": "error",
                    "error_code": "AppVoiceDeliveryFenceExpired",
                    "reason": "The governed result was withheld because the authenticated realtime session changed."
                })
                .to_string();
                self.send_envelope(
                    ctx,
                    "tool.result",
                    json!({
                        "call_id": msg.call_id,
                        "output": output,
                        "voice_summary": "I couldn't safely deliver that app result in this voice session.",
                        "status": "error",
                    }),
                );
                return;
            }
        }
        if let Some(update) = msg.catalog_update {
            if self.pending_catalog_update.is_some() {
                let orchestrator = Arc::clone(&self.orchestrator);
                let rejected = update.clone();
                actix::spawn(async move {
                    orchestrator.abort_tool_catalog_update(&rejected).await;
                });
                let output = json!({
                    "status": "error",
                    "error_code": "catalog_update_already_pending",
                })
                .to_string();
                self.send_or_queue_provider_control(RealtimeAudioControl::ToolResult {
                    call_id: msg.call_id.clone(),
                    output: output.clone(),
                });
                self.send_envelope(
                    ctx,
                    "tool.result",
                    json!({
                        "call_id": msg.call_id.clone(),
                        "output": output.clone(),
                        "voice_summary": "I could not load that tool set yet.",
                        "status": "error",
                    }),
                );
                let orchestrator = Arc::clone(&self.orchestrator);
                let tool_name = msg.tool_name;
                let call_id = msg.call_id;
                actix::spawn(async move {
                    orchestrator
                        .finalize_voice_catalog_tool_result(&tool_name, &call_id, &output, None)
                        .await;
                });
                return;
            }

            let previous_tools = std::mem::replace(&mut self.session_tools, update.tools.clone());
            let update_id = update.update_id.clone();
            self.pending_catalog_update = Some(PendingVoiceCatalogUpdate {
                update: update.clone(),
                tool_name: msg.tool_name,
                call_id: msg.call_id,
                output: msg.output,
                voice_summary: msg.voice_summary,
                status: msg.status,
                previous_tools,
                awaiting_rotation: false,
                acknowledgement_in_flight: false,
                projected_result: msg.projected_result,
                provider_delivery_fence: msg.provider_delivery_fence,
            });
            match self.audio_topology {
                Some(RealtimeAudioTopology::BackendProxied) => {
                    if let Some(instructions) = self.session_instructions.clone() {
                        self.send_or_queue_provider_control(
                            RealtimeAudioControl::ConfigureSession {
                                instructions,
                                tools: self.session_tools.clone(),
                                input_transcription_model: self.input_transcription_override(),
                                update_id: Some(update_id.clone()),
                                defer_response_until_context: self.per_turn_context_enabled,
                            },
                        );
                    } else {
                        self.rotate_for_pending_catalog_update(&update_id, ctx);
                    }
                },
                Some(RealtimeAudioTopology::DirectPeerToPeer) => {
                    self.send_envelope(ctx, "tool.catalog.update", json!(update));
                },
                None => self.rotate_for_pending_catalog_update(&update_id, ctx),
            }
            ctx.notify_later(
                CatalogUpdateAckTimeout { update_id },
                TOOL_CATALOG_ACK_TIMEOUT,
            );
            return;
        }
        self.send_or_queue_provider_control(RealtimeAudioControl::ToolResult {
            call_id: msg.call_id.clone(),
            output: msg.output.clone(),
        });
        self.send_envelope(
            ctx,
            "tool.result",
            json!({
                "call_id": msg.call_id,
                "output": msg.output,
                "voice_summary": msg.voice_summary,
                "status": msg.status,
            }),
        );
    }
}

impl Handler<CatalogUpdateCommitted> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: CatalogUpdateCommitted, ctx: &mut Self::Context) -> Self::Result {
        let Some(pending) = self.pending_catalog_update.take() else {
            return;
        };
        if pending.update.update_id != msg.update_id {
            self.pending_catalog_update = Some(pending);
            return;
        }
        match msg.result {
            Ok(committed) => {
                self.session_tools = committed.tools;
                if pending
                    .provider_delivery_fence
                    .as_ref()
                    .is_some_and(|fence| {
                        !matches!(
                            self.audio_topology,
                            Some(RealtimeAudioTopology::BackendProxied)
                        ) || fence.ensure_current(chrono::Utc::now()).is_err()
                    })
                {
                    let output = json!({
                        "status": "error",
                        "error_code": "AppVoiceDeliveryFenceExpired",
                        "reason": "The governed result was withheld after the catalog wait because the authenticated realtime turn changed."
                    })
                    .to_string();
                    self.send_envelope(
                        ctx,
                        "tool.result",
                        json!({
                            "call_id": pending.call_id,
                            "output": output,
                            "voice_summary": "I couldn't safely deliver that app result after the voice tool update.",
                            "status": "error",
                        }),
                    );
                    return;
                }
                self.send_or_queue_provider_control(RealtimeAudioControl::ToolResult {
                    call_id: pending.call_id.clone(),
                    output: pending.output.clone(),
                });
                self.send_envelope(
                    ctx,
                    "tool.result",
                    json!({
                        "call_id": pending.call_id.clone(),
                        "output": pending.output.clone(),
                        "voice_summary": pending.voice_summary.clone(),
                        "status": pending.status.clone(),
                        "policy_snapshot_id": committed.policy_snapshot_id,
                        "working_set_generation": committed.working_set_generation,
                    }),
                );
                let orchestrator = Arc::clone(&self.orchestrator);
                let tool_name = pending.tool_name;
                let call_id = pending.call_id;
                let output = pending.output;
                let projection = pending.projected_result;
                actix::spawn(async move {
                    orchestrator
                        .finalize_voice_catalog_tool_result(
                            &tool_name, &call_id, &output, projection,
                        )
                        .await;
                });
            },
            Err(error) => {
                self.session_tools = pending.previous_tools;
                let update = pending.update;
                let orchestrator = Arc::clone(&self.orchestrator);
                actix::spawn(async move {
                    orchestrator.abort_tool_catalog_update(&update).await;
                });
                let output = json!({
                    "status": "error",
                    "error_code": "catalog_update_commit_failed",
                    "reason": error,
                })
                .to_string();
                self.send_or_queue_provider_control(RealtimeAudioControl::ToolResult {
                    call_id: pending.call_id.clone(),
                    output: output.clone(),
                });
                self.send_envelope(
                    ctx,
                    "tool.result",
                    json!({
                        "call_id": pending.call_id.clone(),
                        "output": output.clone(),
                        "voice_summary": "I could not safely load that tool set.",
                        "status": "error",
                    }),
                );
                let orchestrator = Arc::clone(&self.orchestrator);
                let tool_name = pending.tool_name;
                let call_id = pending.call_id;
                let durable_output = output;
                actix::spawn(async move {
                    orchestrator
                        .finalize_voice_catalog_tool_result(
                            &tool_name,
                            &call_id,
                            &durable_output,
                            None,
                        )
                        .await;
                });
                self.spawn_rotation(RotateReason::Reconnect, ctx);
            },
        }
    }
}

impl Handler<CatalogUpdateAckTimeout> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: CatalogUpdateAckTimeout, ctx: &mut Self::Context) -> Self::Result {
        self.rotate_for_pending_catalog_update(&msg.update_id, ctx);
    }
}

impl Handler<TutorTakeoverFinished> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: TutorTakeoverFinished, ctx: &mut Self::Context) -> Self::Result {
        self.active_tutor_takeover_keys.remove(&msg.key);
        self.active_tutor_takeover_cancellations.remove(&msg.key);
        let lock_cancelled = self.lock_cancelled_tutor_takeover_keys.remove(&msg.key);
        if !lock_cancelled {
            self.remember_completed_tutor_takeover_key(msg.key.clone());
        }
        self.tutor_response_suppression
            .finish_takeover(!self.active_tutor_takeover_keys.is_empty());
        if lock_cancelled {
            return;
        }
        if let Some(error) = msg.error {
            self.report_tutor_takeover_failure(&error, ctx);
        } else {
            self.send_envelope(ctx, "tutor.takeover.completed", json!({}));
        }
    }
}

impl Handler<ProviderReadyTimeout> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ProviderReadyTimeout, ctx: &mut Self::Context) -> Self::Result {
        if msg.generation != self.provider_config_generation
            || self.provider_configured
            || !self.provider_configuring
        {
            return;
        }
        self.provider_configuring = false;
        self.clear_pending_provider_frames();
        self.send_envelope(
            ctx,
            "session.error",
            json!({
                "message": "Realtime provider did not become ready in time.",
                "recoverable": false,
            }),
        );
        ctx.stop();
    }
}

impl Handler<LocalFallbackConfigureFailed> for VoiceControlSession {
    type Result = ();

    fn handle(
        &mut self,
        msg: LocalFallbackConfigureFailed,
        ctx: &mut Self::Context,
    ) -> Self::Result {
        if self.local_fallback_update_id.as_deref() != Some(msg.update_id.as_str()) {
            return;
        }
        self.local_fallback_update_id = None;
        self.provider_configuring = false;
        self.provider_configured = false;
        self.clear_pending_provider_frames();
        error!(
            voice_session_id = %self.voice_session_id,
            update_id = %msg.update_id,
            error = %msg.reason,
            "[VOICE-CONTROL] failed to restore vendor transcription"
        );
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK,
            json!({
                "state": "vendor_restore_failed",
                "error_class": "control_channel_closed",
            }),
        );
        self.send_envelope(
            ctx,
            "session.error",
            json!({
                "message": "Transcription recovery failed; ending the call safely.",
                "recoverable": false,
            }),
        );
        ctx.stop();
    }
}

impl Handler<LocalFallbackConfigureAckTimeout> for VoiceControlSession {
    type Result = ();

    fn handle(
        &mut self,
        msg: LocalFallbackConfigureAckTimeout,
        ctx: &mut Self::Context,
    ) -> Self::Result {
        if self.local_fallback_update_id.as_deref() != Some(msg.update_id.as_str()) {
            return;
        }
        self.local_fallback_update_id = None;
        self.provider_configuring = false;
        self.provider_configured = false;
        self.clear_pending_provider_frames();
        error!(
            voice_session_id = %self.voice_session_id,
            update_id = %msg.update_id,
            "[VOICE-CONTROL] vendor transcription restore acknowledgement timed out"
        );
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK,
            json!({
                "state": "vendor_restore_failed",
                "error_class": "ack_timeout",
            }),
        );
        self.send_envelope(
            ctx,
            "session.error",
            json!({
                "message": "Transcription recovery timed out; ending the call safely.",
                "recoverable": false,
            }),
        );
        ctx.stop();
    }
}

impl Handler<InternalRotate> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: InternalRotate, ctx: &mut Self::Context) -> Self::Result {
        self.spawn_rotation(msg.0, ctx);
    }
}

impl Handler<AttachBridgeStream> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: AttachBridgeStream, ctx: &mut Self::Context) -> Self::Result {
        <Self as StreamHandler<VoiceDownstreamMessage>>::add_stream(
            UnboundedReceiverStream::new(msg.0),
            ctx,
        );
    }
}

impl Handler<AttachAudioChannel> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: AttachAudioChannel, ctx: &mut Self::Context) -> Self::Result {
        self.provider_transport_ready = false;
        // Stash the upstream sender so incoming WS binary frames
        // can forward to the provider.
        self.upstream_audio_tx = Some(msg.0.upstream_tx);
        self.provider_control_tx = Some(msg.0.control_tx.clone());
        self.install_provider_control_dispatcher(msg.0.control_tx);
        // Spawn a task that reads provider → browser PCM frames
        // off the channel and dispatches them back to ourselves as
        // `ProviderAudioFrame` messages (the actor handler then
        // emits them as WS binary frames). Spawning rather than
        // using `add_stream` keeps the receiver type opaque.
        let addr = ctx.address();
        let mut downstream_rx = msg.0.downstream_rx;
        actix::spawn(async move {
            while let Some(frame) = downstream_rx.recv().await {
                addr.do_send(ProviderAudioFrame(frame));
            }
        });
        let addr = ctx.address();
        let mut events_rx = msg.0.events_rx;
        actix::spawn(async move {
            while let Some(event) = events_rx.recv().await {
                addr.do_send(ProviderRealtimeEventFrame(event));
            }
        });
    }
}

impl Handler<BeginLocalTranscript> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: BeginLocalTranscript, _ctx: &mut Self::Context) -> Self::Result {
        self.local_transcript
            .begin_start(msg.vendor_fallback_model, msg.manual_turns);
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE,
            json!({ "state": "starting" }),
        );
    }
}

impl Handler<LocalTranscriptStartFailed> for VoiceControlSession {
    type Result = ();

    fn handle(
        &mut self,
        msg: LocalTranscriptStartFailed,
        _ctx: &mut Self::Context,
    ) -> Self::Result {
        self.local_transcript.start_failed(msg.reason.clone());
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE,
            json!({
                "state": "startup_failed",
                "error_class": msg.reason,
            }),
        );
    }
}

impl Handler<AttachLocalTranscript> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: AttachLocalTranscript, ctx: &mut Self::Context) -> Self::Result {
        info!(
            voice_session_id = %self.voice_session_id,
            profile_id = msg.handle.profile_id(),
            "[VOICE-CONTROL] local realtime transcript attached"
        );
        self.emit_local_transcript_telemetry(
            MEDIA_VOICE_LOCAL_TRANSCRIPT_STATE,
            json!({
                "state": "attached",
                "profile_id": msg.handle.profile_id(),
                "vendor_fallback_active": false,
            }),
        );
        self.local_transcript.attach(msg.handle);
        let addr = ctx.address();
        let mut events = msg.events;
        actix::spawn(async move {
            while let Some(event) = events.recv().await {
                addr.do_send(LocalTranscriptEventFrame(event));
            }
        });
    }
}

impl Handler<LocalTranscriptEventFrame> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: LocalTranscriptEventFrame, ctx: &mut Self::Context) -> Self::Result {
        match msg.0 {
            LocalRealtimeTranscriptEvent::Partial {
                mut text,
                stream_generation,
            } => {
                if !self.local_transcript.is_active()
                    || !self
                        .local_transcript
                        .accepts_transcript_event(stream_generation)
                {
                    return;
                }
                let (item_id, turn_generation) =
                    self.local_transcript.partial_identity(stream_generation);
                text = self
                    .local_transcript
                    .partial_with_staged(stream_generation, &text);
                if let Some(first_partial_latency_ms) = self
                    .local_transcript
                    .record_first_partial(stream_generation)
                {
                    self.emit_local_transcript_telemetry(
                        MEDIA_VOICE_LOCAL_TRANSCRIPT_TURN,
                        json!({
                            "state": "first_partial",
                            "stream_generation": stream_generation,
                            "turn_generation": turn_generation,
                            "first_partial_latency_ms": first_partial_latency_ms,
                        }),
                    );
                }
                self.send_envelope(
                    ctx,
                    "transcript.user.partial",
                    json!({
                        "text": text,
                        "item_id": item_id,
                        "turn_generation": turn_generation,
                    }),
                );
            },
            LocalRealtimeTranscriptEvent::Final {
                text,
                stream_generation,
            } => {
                if !self.local_transcript.is_active()
                    || !self
                        .local_transcript
                        .accepts_transcript_event(stream_generation)
                {
                    return;
                }
                let (text, item_id, turn_generation) =
                    self.local_transcript.stage_final(stream_generation, text);
                self.send_envelope(
                    ctx,
                    "transcript.user.partial",
                    json!({
                        "text": text,
                        "item_id": item_id,
                        "turn_generation": turn_generation,
                    }),
                );
            },
            LocalRealtimeTranscriptEvent::CommitReady { stream_generation } => {
                if self.local_transcript.is_active()
                    && self
                        .local_transcript
                        .accepts_transcript_event(stream_generation)
                {
                    self.queue_provider_commit_for_local_turn(stream_generation, ctx);
                }
            },
            LocalRealtimeTranscriptEvent::Error {
                reason,
                stream_generation,
            } => {
                let commit_mode = self.local_transcript.pending_commit_mode(stream_generation);
                self.degrade_local_transcript("local_stream_error".to_string(), ctx);
                if let Some(commit_mode) = commit_mode {
                    if commit_mode.requires_provider_commit() {
                        self.send_or_queue_provider_control(
                            RealtimeAudioControl::CommitInputAndRespond,
                        );
                    }
                    self.finalize_committed_local_turn(stream_generation, ctx);
                }
                warn!(
                    voice_session_id = %self.voice_session_id,
                    stream_generation,
                    error = %reason,
                    "[VOICE-CONTROL] local transcript provider reported an asynchronous failure"
                );
            },
        }
    }
}

impl Handler<LocalProviderCommitQueued> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: LocalProviderCommitQueued, ctx: &mut Self::Context) -> Self::Result {
        match msg.result {
            Ok(()) => self.finalize_committed_local_turn(msg.stream_generation, ctx),
            Err(error) => {
                let commit_mode = self
                    .local_transcript
                    .pending_commit_mode(msg.stream_generation);
                self.degrade_local_transcript("provider_commit_queue_closed".to_string(), ctx);
                if let Some(commit_mode) = commit_mode {
                    if commit_mode.requires_provider_commit() {
                        self.send_or_queue_provider_control(
                            RealtimeAudioControl::CommitInputAndRespond,
                        );
                    }
                    self.finalize_committed_local_turn(msg.stream_generation, ctx);
                }
                warn!(
                    voice_session_id = %self.voice_session_id,
                    stream_generation = msg.stream_generation,
                    error = %error,
                    "[VOICE-CONTROL] failed to queue provider commit after local transcript flush"
                );
            },
        }
    }
}

impl Handler<ProviderAudioFrame> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ProviderAudioFrame, ctx: &mut Self::Context) -> Self::Result {
        if self.suppress_ambient_assistant_transcript {
            return;
        }
        let audio_response_id = self.assistant_audio_response_id.clone();
        if provider_audio_frame_is_suppressed(
            &mut self.tutor_response_suppression,
            audio_response_id.as_deref(),
        ) {
            return;
        }
        // These are the exact bytes the device will play (24kHz mono PCM16):
        // account them so the self-echo window is computed from real playback
        // duration rather than guessed.
        self.echo_suppressor.note_assistant_audio(
            self.assistant_audio_response_id.as_deref(),
            msg.0.len(),
            std::time::Instant::now(),
        );
        ctx.binary(msg.0);
    }
}

const OPENAI_REALTIME_ITEM_ID_MAX_BYTES: usize = 32;

fn new_turn_context_item_id() -> String {
    let item_id = uuid::Uuid::new_v4().simple().to_string();
    debug_assert_eq!(item_id.len(), OPENAI_REALTIME_ITEM_ID_MAX_BYTES);
    item_id
}

impl Handler<TurnContextPrepared> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: TurnContextPrepared, ctx: &mut Self::Context) -> Self::Result {
        if !self.per_turn_context_enabled
            || !self.turn_context_pending
            || msg.generation != self.turn_context_generation
        {
            debug!(
                voice_session_id = %self.voice_session_id,
                generation = msg.generation,
                current_generation = self.turn_context_generation,
                "[VOICE-CONTROL] discarded stale current-turn context"
            );
            return;
        }
        self.turn_context_pending = false;
        self.turn_context_cancellation = None;
        // OpenAI Realtime caps item.id at 32 characters. A simple UUID is
        // already collision-resistant for this per-call scratch item and fits
        // the provider contract exactly; the former `voice-context-` prefix
        // made every otherwise valid ID 46 characters long.
        let context_item_id = new_turn_context_item_id();
        let (context, telemetry) = match msg.result {
            Ok(outcome) => {
                let local_only_delivery_current = !outcome.local_only_app_memory_rendered
                    || (matches!(
                        self.audio_topology,
                        Some(RealtimeAudioTopology::BackendProxied)
                    ) && msg
                        .provider_delivery_fence
                        .as_ref()
                        .is_some_and(|fence| fence.ensure_current(chrono::Utc::now()).is_ok()));
                // Not `outcome.context`: a room's meeting notes ride alongside
                // retrieval and are dropped on the floor if read separately.
                let context = local_only_delivery_current
                    .then(|| outcome.injectable_context())
                    .flatten();
                let telemetry = json!({
                    "status": if local_only_delivery_current { json!(outcome.status) } else { json!("local_only_delivery_withheld") },
                    "meeting_context": outcome.meeting_context.is_some(),
                    "local_only_app_memory": outcome.local_only_app_memory_rendered,
                    "elapsed_ms": outcome.elapsed_ms,
                    "budget_ms": outcome.budget_ms,
                    "memory_backend": outcome.memory_backend,
                    "procedure_backend": outcome.procedure_backend,
                    "memory_candidate_count": outcome.memory_candidate_count,
                    "procedure_count": outcome.procedure_count,
                    "fast_memory_ms": outcome.fast_memory_ms,
                    "fast_memory_stage_status": outcome.fast_memory_stage_status,
                    "memory_stage_status": outcome.memory_stage_status,
                    "procedure_stage_status": outcome.procedure_stage_status,
                });
                (context, telemetry)
            },
            Err(error) => {
                warn!(
                    voice_session_id = %self.voice_session_id,
                    generation = msg.generation,
                    error = %error,
                    "[VOICE-CONTROL] current-turn context failed; continuing without injection"
                );
                (
                    None,
                    json!({
                        "status": "failed",
                        "elapsed_ms": Value::Null,
                        "budget_ms": self.per_turn_context_budget_ms,
                    }),
                )
            },
        };

        match self.audio_topology {
            Some(RealtimeAudioTopology::BackendProxied) => {
                self.send_or_queue_provider_control(RealtimeAudioControl::RespondWithTurnContext {
                    context_item_id: context_item_id.clone(),
                    context: context.clone(),
                });
            },
            Some(RealtimeAudioTopology::DirectPeerToPeer) => {
                self.send_envelope(
                    ctx,
                    "turn.context.ready",
                    json!({
                        "generation": msg.generation,
                        "context_item_id": context_item_id,
                        "context": context,
                        "retrieval": telemetry,
                    }),
                );
                return;
            },
            None => {
                warn!(
                    voice_session_id = %self.voice_session_id,
                    "[VOICE-CONTROL] current-turn context completed without an audio topology"
                );
                return;
            },
        }
        self.send_envelope(
            ctx,
            "turn.context.ready",
            json!({
                "generation": msg.generation,
                "context_item_id": context_item_id,
                "retrieval": telemetry,
            }),
        );
    }
}

impl Handler<ProviderRealtimeEventFrame> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ProviderRealtimeEventFrame, ctx: &mut Self::Context) -> Self::Result {
        match msg.0 {
            RealtimeProviderEvent::TransportReady => {
                self.provider_transport_ready = true;
                if self.provider_configuring && self.session_instructions.is_some() {
                    self.provider_configuring = false;
                    self.provider_configured = self.provider_control_tx.is_some();
                    self.flush_pending_provider_frames();
                    info!(
                        voice_session_id = %self.voice_session_id,
                        "[VOICE-CONTROL] realtime provider transport ready"
                    );
                }
            },
            RealtimeProviderEvent::SessionConfigured { update_id } => {
                if let Some(update_id) = update_id {
                    if self.local_fallback_update_id.as_deref() == Some(update_id.as_str()) {
                        self.local_fallback_update_id = None;
                        self.provider_configuring = false;
                        self.provider_configured = self.provider_control_tx.is_some();
                        self.local_transcript.mark_vendor_fallback_active();
                        self.flush_pending_provider_frames();
                        info!(
                            voice_session_id = %self.voice_session_id,
                            update_id = %update_id,
                            fallback_model = ?self.input_transcription_override(),
                            "[VOICE-CONTROL] vendor transcription restore acknowledged"
                        );
                        self.emit_local_transcript_telemetry(
                            MEDIA_VOICE_LOCAL_TRANSCRIPT_FALLBACK,
                            json!({
                                "state": "vendor_restore_active",
                                "fallback_model_configured": self
                                    .input_transcription_override()
                                    .is_some(),
                            }),
                        );
                        return;
                    }
                    self.acknowledge_pending_catalog_update(&update_id, ctx);
                } else if let Some(update_id) = self
                    .pending_catalog_update
                    .as_ref()
                    .filter(|pending| pending.awaiting_rotation)
                    .map(|pending| pending.update.update_id.clone())
                {
                    self.acknowledge_pending_catalog_update(&update_id, ctx);
                }
            },
            RealtimeProviderEvent::SessionConfigurationUnsupported { update_id } => {
                self.rotate_for_pending_catalog_update(&update_id, ctx);
            },
            RealtimeProviderEvent::SpeechStarted => {
                self.captured_voice_context = self.selected_voice_context.clone();
                self.cancel_pending_turn_context();
                let provider_response_active = self.has_interruptible_provider_response();
                let cascaded_response_active = self.has_interruptible_cascaded_response();
                if speech_start_interrupts_a_response(
                    self.assistant_audio_response_id.is_some(),
                    cascaded_response_active,
                ) {
                    self.observe_failed_realtime_llm_call("cancelled", ctx);
                }
                if self.hands_free {
                    if provider_response_active {
                        self.send_or_queue_provider_control(
                            RealtimeAudioControl::InterruptResponse,
                        );
                    }
                    if cascaded_response_active {
                        self.interrupt_cascaded_turn(ctx);
                    }
                }
                self.send_envelope(ctx, "speech.started", json!({}));
            },
            RealtimeProviderEvent::SpeechStopped => {
                // Realtime latency: user stopped talking → the response turn begins.
                self.turn_started_at = Some(std::time::Instant::now());
                self.turn_ttfa_ms = None;
                self.begin_realtime_llm_call();
                if self.local_transcript.is_active() && !self.local_transcript.manual_turns() {
                    let _ = self.commit_local_transcript(
                        LocalTranscriptCommitMode::ProviderAlreadyCommitted,
                        ctx,
                    );
                }
                self.send_envelope(ctx, "speech.stopped", json!({}));
            },
            RealtimeProviderEvent::UserTranscriptPartial { text, item_id } => {
                if self.local_transcript.is_active() {
                    return;
                }
                let text = text.trim();
                if text.is_empty() {
                    return;
                }
                self.send_envelope(
                    ctx,
                    "transcript.user.partial",
                    json!({ "text": text, "item_id": item_id }),
                );
            },
            RealtimeProviderEvent::UserTranscriptFinal { text, item_id } => {
                // Once a local stream is active it is the sole source of truth.
                // Ignore a late vendor transcript from the configure seam so a
                // single utterance can never be persisted twice.
                if self.local_transcript.is_active() {
                    debug!(
                        voice_session_id = %self.voice_session_id,
                        item_id = %item_id,
                        "[VOICE-CONTROL] ignored provider transcript while local transcript is active"
                    );
                    return;
                }
                let _ = self.handle_finalized_user_transcript(text, item_id, None, ctx);
            },
            RealtimeProviderEvent::AssistantTranscriptDelta { response_id, text } => {
                if self.suppress_ambient_assistant_transcript {
                    return;
                }
                // Realtime latency: first assistant token of the turn = time-to-first.
                if self.turn_ttfa_ms.is_none() {
                    if let Some(started) = self.turn_started_at {
                        self.turn_ttfa_ms = Some(started.elapsed().as_millis() as u64);
                    }
                }
                if self
                    .tutor_response_suppression
                    .suppresses_response_event(&response_id)
                {
                    return;
                }
                self.begin_realtime_llm_call();
                self.current_provider_response_id = Some(response_id.clone());
                // Track the in-progress caption for self-echo matching: the
                // mic can pick this text up and finalize it as a user
                // transcript before the provider's own final caption arrives.
                self.echo_suppressor.note_assistant_delta(
                    &response_id,
                    &text,
                    std::time::Instant::now(),
                );
                self.send_envelope(
                    ctx,
                    "transcript.assistant.delta",
                    json!({ "text": text, "response_id": response_id }),
                );
            },
            RealtimeProviderEvent::AssistantTranscriptFinal { response_id, text } => {
                if self.suppress_ambient_assistant_transcript {
                    self.suppress_ambient_assistant_transcript = false;
                    if self.current_provider_response_id.as_deref() == Some(response_id.as_str()) {
                        self.current_provider_response_id = None;
                    }
                    return;
                }
                let suppress = self.tutor_response_suppression.observe_provider_terminal(
                    Some(&response_id),
                    !self.active_tutor_takeover_keys.is_empty(),
                );
                if suppress {
                    if self.current_provider_response_id.as_deref() == Some(response_id.as_str()) {
                        self.current_provider_response_id = None;
                    }
                    debug!(
                        voice_session_id = %self.voice_session_id,
                        "[VOICE-CONTROL] suppressed provider assistant transcript after tutor takeover"
                    );
                    return;
                }
                if self.current_provider_response_id.is_none() {
                    self.begin_realtime_llm_call();
                    self.current_provider_response_id = Some(response_id.clone());
                }
                self.echo_suppressor.note_assistant_final(
                    Some(&response_id),
                    &text,
                    std::time::Instant::now(),
                );
                self.send_envelope(
                    ctx,
                    "transcript.assistant",
                    json!({ "text": text, "response_id": response_id }),
                );
                if self.hands_free {
                    return;
                }
                let orchestrator = Arc::clone(&self.orchestrator);
                actix::spawn(async move {
                    if let Err(err) = orchestrator
                        .ingest_transcript_turn(ChatMessageDirection::Assistant, &text)
                        .await
                    {
                        warn!(error = %err, "[VOICE-CONTROL] provider assistant transcript ingest failed");
                    }
                });
            },
            RealtimeProviderEvent::ResponseDone {
                response_id,
                input_tokens,
                output_tokens,
                usage,
            } => {
                let targets_current =
                    self.provider_terminal_targets_current_response(response_id.as_deref());
                self.observe_provider_response_lifecycle_terminal(response_id.as_deref());
                // Realtime latency: close out the turn timing and hand it to the
                // orchestrator so it lands on the same `voice_cost` telemetry.
                let (response_ms, ttfa_ms, correlation, started_at_ms) = if targets_current {
                    (
                        self.turn_started_at
                            .take()
                            .map(|started| started.elapsed().as_millis() as u64),
                        self.turn_ttfa_ms.take(),
                        self.current_llm_correlation.take(),
                        self.current_llm_started_at_ms.take(),
                    )
                } else {
                    (None, None, None, None)
                };
                if response_id.is_none() {
                    // The session's own bill, not a turn's: GPT-Live reports
                    // one usage figure for the whole call and nothing
                    // per-turn. One line per session, and the only record of
                    // whether that figure reached the ledger.
                    info!(
                        voice_session_id = %self.voice_session_id,
                        targets_current,
                        attributed = correlation.is_some(),
                        usage_reported = usage.is_some(),
                        "[VOICE-CONTROL] session-level realtime usage terminal"
                    );
                }
                if self.hands_free {
                    return;
                }
                let orchestrator = Arc::clone(&self.orchestrator);
                let addr = ctx.address();
                let fut = async move {
                    let context_window =
                        orchestrator.context_window_tokens().await.unwrap_or(32_000);
                    if orchestrator
                        .observe_token_usage(
                            input_tokens,
                            output_tokens,
                            usage,
                            ttfa_ms,
                            response_ms,
                            context_window,
                            correlation,
                            started_at_ms,
                        )
                        .await
                    {
                        addr.do_send(InternalRotate(RotateReason::Watermark));
                    }
                };
                // Never `ctx.spawn`: this is the turn's bill, and for a
                // duration-billed provider the session's entire bill. A
                // client that closes its socket straight after `session.end`
                // stops this actor within a millisecond — measured at 400 µs
                // — and an actor-scoped future is cancelled with it, so the
                // report that had already arrived was thrown away. It landed
                // only when it won that race.
                actix::spawn(fut);
            },
            RealtimeProviderEvent::InteractionStatus { in_progress } => {
                // Gemini 3.8 Live closes an utterance (`response.done`) while a
                // non-blocking tool still runs; the client hears "let me
                // check…" and then silence. This tells it the assistant is
                // still working so the silence is not read as done. Clients
                // that predate the kind ignore it.
                self.send_envelope(
                    ctx,
                    "interaction.status",
                    json!({
                        "status": if in_progress { "in_progress" } else { "idle" }
                    }),
                );
            },
            RealtimeProviderEvent::ResponseFailed {
                response_id,
                terminal_state,
            } => {
                let targets_current =
                    self.provider_terminal_targets_current_response(response_id.as_deref());
                self.observe_provider_response_lifecycle_terminal(response_id.as_deref());
                let (error_class, terminal_state) = match terminal_state {
                    magicllm::realtime::RealtimeResponseTerminalState::Cancelled => {
                        ("cancelled", "cancelled")
                    },
                    magicllm::realtime::RealtimeResponseTerminalState::Incomplete => {
                        ("realtime_response_incomplete", "incomplete")
                    },
                    magicllm::realtime::RealtimeResponseTerminalState::Failed => {
                        ("realtime_response_failed", "failed")
                    },
                };
                if targets_current {
                    self.observe_failed_realtime_llm_call(error_class, ctx);
                }
                self.send_envelope(
                    ctx,
                    "response.failed",
                    json!({
                        "response_id": response_id,
                        "terminal_state": terminal_state,
                    }),
                );
            },
            RealtimeProviderEvent::AssistantAudioStarted { response_id } => {
                if self.suppress_ambient_assistant_transcript {
                    return;
                }
                if self
                    .tutor_response_suppression
                    .suppresses_response_event(&response_id)
                {
                    return;
                }
                self.begin_realtime_llm_call();
                self.current_provider_response_id = Some(response_id.clone());
                self.assistant_audio_response_id = Some(response_id.clone());
                self.send_envelope(
                    ctx,
                    "audio.output.started",
                    json!({ "response_id": response_id }),
                );
            },
            RealtimeProviderEvent::AssistantAudioDone {
                response_id,
                interrupted,
            } => {
                if self.assistant_audio_response_id.as_deref() != Some(response_id.as_str()) {
                    return;
                }
                self.assistant_audio_response_id = None;
                // An interrupted response stops sounding now, so its echo
                // window must not run to the end of bytes that never played.
                self.echo_suppressor.note_assistant_audio_done(
                    &response_id,
                    interrupted,
                    std::time::Instant::now(),
                );
                self.send_envelope(
                    ctx,
                    "audio.output.ended",
                    json!({ "response_id": response_id, "interrupted": interrupted }),
                );
            },
            RealtimeProviderEvent::NativeResumeHandleUpdated { handle } => {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    handle_len = handle.len(),
                    "[VOICE-CONTROL] provider native resume handle updated"
                );
            },
            RealtimeProviderEvent::SessionExpiring { time_left_secs } => {
                self.send_envelope(
                    ctx,
                    "session.expiring",
                    json!({ "time_left_secs": time_left_secs }),
                );
                self.spawn_rotation(RotateReason::Proactive, ctx);
            },
            RealtimeProviderEvent::TransportClosed { message } => {
                if is_fatal_realtime_setup_error(&message) {
                    warn!(
                        voice_session_id = %self.voice_session_id,
                        error = %message,
                        "[VOICE-CONTROL] realtime upstream rejected session setup; not rotating"
                    );
                    self.observe_failed_realtime_llm_call("realtime_provider_setup_rejected", ctx);
                    self.send_envelope(
                        ctx,
                        "session.error",
                        json!({
                            "message": "This realtime profile rejected the session setup and will not reconnect.",
                            "recoverable": false,
                        }),
                    );
                    ctx.stop();
                    return;
                }
                warn!(
                    voice_session_id = %self.voice_session_id,
                    error = %message,
                    "[VOICE-CONTROL] realtime upstream disconnected; rotating provider session"
                );
                self.observe_failed_realtime_llm_call("realtime_provider_transport_closed", ctx);
                self.send_envelope(
                    ctx,
                    "session.error",
                    json!({
                        "message": "Realtime provider connection was interrupted; reconnecting.",
                        "recoverable": true,
                    }),
                );
                self.spawn_rotation(RotateReason::Reconnect, ctx);
            },
            RealtimeProviderEvent::FunctionCall {
                response_id,
                call_id,
                name,
                arguments_json,
            } => {
                if self
                    .tutor_response_suppression
                    .matches_or_claims_response(response_id.as_deref())
                {
                    self.send_or_queue_provider_controls(vec![
                        RealtimeAudioControl::ToolResult {
                            call_id,
                            output: json!({ "error": "guided voice flow owns this turn" })
                                .to_string(),
                        },
                        RealtimeAudioControl::InterruptResponse,
                    ]);
                    return;
                }
                self.begin_realtime_llm_call();
                if let Some(response_id) = response_id.as_ref() {
                    self.current_provider_response_id = Some(response_id.clone());
                }
                if self.suppress_ambient_assistant_transcript {
                    self.send_or_queue_provider_controls(vec![
                        RealtimeAudioControl::ToolResult {
                            call_id,
                            output: json!({ "error": "voice address phrase required" }).to_string(),
                        },
                        RealtimeAudioControl::InterruptResponse,
                    ]);
                    return;
                }
                let orchestrator = Arc::clone(&self.orchestrator);
                let addr = ctx.address();
                let fut = async move {
                    match orchestrator
                        .dispatch_tool(name, arguments_json, call_id)
                        .await
                    {
                        Ok(resp) => {
                            addr.do_send(ToolResult {
                                tool_name: resp.tool_name,
                                call_id: resp.call_id,
                                output: resp.output,
                                voice_summary: resp.voice_summary,
                                status: format!("{:?}", resp.status).to_ascii_lowercase(),
                                catalog_update: resp.catalog_update,
                                projected_result: resp.projected_result,
                                provider_delivery_fence: resp.provider_delivery_fence,
                            });
                        },
                        Err(err) => addr.do_send(SessionError {
                            message: err.to_string(),
                            recoverable: true,
                        }),
                    }
                };
                ctx.spawn(actix::fut::wrap_future(fut));
            },
            RealtimeProviderEvent::Error {
                message,
                recoverable,
            } => {
                // A commit with < 100ms of audio ("input_audio_buffer_commit_empty" /
                // "buffer too small") is BENIGN — the turn simply had too little/no
                // committable audio. Log it for diagnosis but do NOT surface a scary
                // error banner to the client (the call stays live).
                if is_benign_input_buffer_error(&message) {
                    warn!(
                        voice_session_id = %self.voice_session_id,
                        provider_message = %message,
                        "[VOICE-CONTROL] swallowing benign provider input-audio-buffer commit error (empty/short turn)"
                    );
                    self.observe_failed_realtime_llm_call(
                        "realtime_provider_input_buffer_empty",
                        ctx,
                    );
                } else {
                    let error_class = if recoverable {
                        "realtime_provider_recoverable_error"
                    } else {
                        "realtime_provider_error"
                    };
                    self.observe_failed_realtime_llm_call(error_class, ctx);
                    self.send_envelope(
                        ctx,
                        "session.error",
                        json!({ "message": message, "recoverable": recoverable }),
                    );
                    if !recoverable {
                        ctx.stop();
                    }
                }
            },
        }
    }
}

#[derive(Message)]
#[rtype(result = "()")]
struct ConcurrentVoiceCancelled { result: Result<usize, String> }
impl Handler<ConcurrentVoiceCancelled> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ConcurrentVoiceCancelled, ctx: &mut Self::Context) {
        match msg.result {
            Ok(count) => {
                self.send_envelope(ctx, "voice.request.cancelled", json!({ "count": count }));
                let text = if count == 0 { "No matching background request is running.".to_owned() }
                    else { format!("Cancellation requested for {count} background request{}.", if count == 1 { "" } else { "s" }) };
                if self.hands_free {
                    self.send_or_queue_provider_control(RealtimeAudioControl::SynthesizeResponse {
                        response_id: format!("voice-cancel-{}", uuid::Uuid::new_v4()), segments: vec![], text,
                    });
                } else {
                    self.send_or_queue_provider_control(RealtimeAudioControl::InjectSystemMessage { text: text.clone(), request_response: true });
                    self.send_envelope(ctx, "voice.control.reply", json!({ "text": text }));
                }
            },
            Err(message) => self.send_envelope(ctx, "voice.request.failed", json!({ "message": message })),
        }
    }
}

#[derive(Message)]
#[rtype(result = "()")]
struct ConcurrentVoiceAccepted {
    result: Result<magician::magician_v2::chat::voice_requests::VoiceRequest, String>,
}

impl Handler<ConcurrentVoiceAccepted> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: ConcurrentVoiceAccepted, ctx: &mut Self::Context) {
        match msg.result {
            Ok(request) => self.send_envelope(ctx, "voice.request.accepted", json!({ "request": request })),
            Err(message) => self.send_envelope(ctx, "voice.request.failed", json!({ "message": message })),
        }
    }
}

impl Handler<CascadedTurnFinished> for VoiceControlSession {
    type Result = ();

    fn handle(&mut self, msg: CascadedTurnFinished, ctx: &mut Self::Context) -> Self::Result {
        if !self.hands_free || msg.generation != self.cascaded_turn_generation {
            return;
        }
        self.cascaded_turn_active = false;
        match msg.result {
            Ok(CascadedVoiceTurnOutcome::Completed {
                response_id,
                text,
                segments,
            }) => {
                self.send_or_queue_provider_control(RealtimeAudioControl::SynthesizeResponse {
                    response_id,
                    text,
                    segments,
                });
            },
            Ok(CascadedVoiceTurnOutcome::Queued) => {
                let text = "I queued that behind the work already in progress.".to_string();
                self.send_or_queue_provider_control(RealtimeAudioControl::SynthesizeResponse {
                    response_id: format!("hands-free-queued-{}", uuid::Uuid::new_v4()),
                    segments: vec![magicllm::realtime::RealtimeSpeechSegment::plain(
                        text.clone(),
                    )],
                    text,
                });
            },
            Ok(CascadedVoiceTurnOutcome::Cancelled) => {
                self.send_envelope(ctx, "response.interrupted", json!({}));
            },
            Err(message) => {
                self.send_envelope(
                    ctx,
                    "session.error",
                    json!({ "message": message, "recoverable": true }),
                );
            },
        }
    }
}

impl Actor for VoiceControlSession {
    type Context = ws::WebsocketContext<Self>;

    fn started(&mut self, _ctx: &mut Self::Context) {
        info!(
            voice_session_id = %self.voice_session_id,
            "[VOICE-CONTROL] connected"
        );
    }

    fn stopped(&mut self, _ctx: &mut Self::Context) {
        info!(
            voice_session_id = %self.voice_session_id,
            "[VOICE-CONTROL] stopped"
        );
        self.cancel_pending_turn_context();
        if let Some((correlation, started_at_ms, response_ms)) =
            self.take_failed_realtime_llm_call()
        {
            let orchestrator = Arc::clone(&self.orchestrator);
            actix::spawn(async move {
                orchestrator
                    .observe_response_failure(correlation, started_at_ms, response_ms, "cancelled")
                    .await;
            });
        }
        let _ = self.end_local_transcript();
        let pending_catalog_update = self.pending_catalog_update.take();
        self.send_provider_control(RealtimeAudioControl::End);
        let orchestrator = Arc::clone(&self.orchestrator);
        actix::spawn(async move {
            if let Some(pending) = pending_catalog_update {
                let output = json!({
                    "status": "cancelled",
                    "error_code": "voice_session_disconnected_before_catalog_ack",
                    "reason": "The voice session disconnected before the prepared tool catalog was acknowledged."
                })
                .to_string();
                orchestrator
                    .abort_tool_catalog_update(&pending.update)
                    .await;
                orchestrator
                    .finalize_voice_catalog_tool_result(
                        &pending.tool_name,
                        &pending.call_id,
                        &output,
                        None,
                    )
                    .await;
            }
            orchestrator.end().await;
        });
    }
}

#[derive(Message)]
#[rtype(result = "()")]
struct DownstreamFrame {
    kind: String,
    payload: Value,
}

impl Handler<DownstreamFrame> for VoiceControlSession {
    type Result = ();
    fn handle(&mut self, msg: DownstreamFrame, ctx: &mut Self::Context) -> Self::Result {
        self.send_envelope(ctx, &msg.kind, msg.payload);
    }
}

impl StreamHandler<VoiceDownstreamMessage> for VoiceControlSession {
    fn handle(&mut self, msg: VoiceDownstreamMessage, ctx: &mut Self::Context) {
        // Bridge registry sends `task.completed` etc. with its own
        // `{kind, payload}` shape — we forward it on the wire since
        // the frontend wire protocol uses the same envelope. For
        // task-completion we *also* render the spoken announcement
        // string via PromptManager (async) before forwarding so the
        // frontend never has to hardcode the template. We spawn the
        // render off the actor and dispatch a `DownstreamFrame` back
        // when it's ready, mirroring the session.start pattern.
        let value = match serde_json::to_value(&msg) {
            Ok(v) => v,
            Err(err) => {
                error!(error = %err, "[VOICE-CONTROL] downstream frame encode failed");
                return;
            },
        };
        let kind = value
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("downstream")
            .to_string();
        let payload = value.get("payload").cloned().unwrap_or(Value::Null);

        if kind == "delegate_to_chat.chunk" {
            // Backend-proxied Live/Gemini: the browser injectSystemMessage
            // is a no-op, so Magician must push speakable text into the
            // upstream session here. Direct WebRTC still speaks via the
            // client data channel (provider_control_dispatch_tx is None).
            if let Some(tx) = self.provider_control_dispatch_tx.clone() {
                let spoken = payload
                    .get("text")
                    .and_then(Value::as_str)
                    .map(magician::magician_v2::media_seam::strip_speech_tag_markers)
                    .unwrap_or_default();
                let spoken = spoken.split_whitespace().collect::<Vec<_>>().join(" ");
                if !spoken.is_empty() {
                    let _ = tx.send(RealtimeAudioControl::InjectSystemMessage {
                        text: spoken,
                        request_response: true,
                    });
                }
            }
        }

        if kind == "task.completed" {
            let prompt_manager = Arc::clone(&self.prompt_manager);
            let addr = ctx.address();
            let payload_for_render = payload.clone();
            let provider_control_tx = self.provider_control_dispatch_tx.clone();
            let fut = async move {
                let announcement =
                    render_task_completion_announcement(prompt_manager, &payload_for_render).await;
                let mut enriched = payload_for_render;
                if !announcement.is_empty() {
                    if let Some(obj) = enriched.as_object_mut() {
                        obj.insert(
                            "announcement".to_string(),
                            Value::String(announcement.clone()),
                        );
                    }
                    if let Some(tx) = provider_control_tx {
                        let _ = tx.send(RealtimeAudioControl::InjectSystemMessage {
                            text: announcement,
                            request_response: true,
                        });
                    }
                }
                addr.do_send(DownstreamFrame {
                    kind: "task.completed".to_string(),
                    payload: enriched,
                });
            };
            ctx.spawn(actix::fut::wrap_future(fut));
            return;
        }

        if kind == "task.awaiting_diff_approval" {
            // Same shape as `task.completed`: render server-side, inject into
            // the provider so it is actually SPOKEN, and forward the enriched
            // envelope. Rendering here rather than in the frontend is what
            // lets this land without a `realtimeVoiceClient.ts` case — the
            // backend-proxied / cascaded road (the one `@vibedev` voice
            // actually runs on) speaks the injected system message, and the
            // data-channel road ignores an envelope kind it does not know
            // instead of breaking on it.
            let prompt_manager = Arc::clone(&self.prompt_manager);
            let addr = ctx.address();
            let payload_for_render = payload.clone();
            let provider_control_tx = self.provider_control_dispatch_tx.clone();
            let fut = async move {
                let announcement =
                    render_diff_approval_announcement(prompt_manager, &payload_for_render).await;
                let mut enriched = payload_for_render;
                if !announcement.is_empty() {
                    if let Some(obj) = enriched.as_object_mut() {
                        obj.insert(
                            "announcement".to_string(),
                            Value::String(announcement.clone()),
                        );
                    }
                    if let Some(tx) = provider_control_tx {
                        let _ = tx.send(RealtimeAudioControl::InjectSystemMessage {
                            text: announcement,
                            request_response: true,
                        });
                    }
                }
                addr.do_send(DownstreamFrame {
                    kind: "task.awaiting_diff_approval".to_string(),
                    payload: enriched,
                });
            };
            ctx.spawn(actix::fut::wrap_future(fut));
            return;
        }

        self.send_envelope(ctx, &kind, payload);
    }
}

async fn render_openai_live_mouth_instructions(
    prompt_manager: &PromptManager,
    assistant_name: &str,
) -> String {
    let trimmed = assistant_name.trim();
    let name = if trimmed.is_empty() {
        "Magican"
    } else {
        trimmed
    };
    let mut vars = HashMap::new();
    vars.insert("assistant_name".to_string(), name.to_string());
    prompt_manager
        .get_rendered_prompt(
            prompt_constants::names::VOICE_LIVE_MOUTH_SYSTEM,
            prompt_constants::versions::VOICE_LIVE_MOUTH_SYSTEM,
            vars,
        )
        .await
        .unwrap_or_else(|err| {
            warn!(
                error = %err,
                "[VOICE-CONTROL] GPT-Live mouth prompt render failed; using built-in fallback"
            );
            magicllm::realtime::OPENAI_LIVE_DEFAULT_INSTRUCTIONS.to_string()
        })
}

/// Render the task-completion announcement string the realtime voice
/// model will speak. Uses PromptManager so the template lives in
/// `data/magician_v2/prompts/voice_task_completion_announcement_v1.0.0.json`
/// — never in TypeScript, never in inline Rust. Returns an empty
/// string on render failure; the WS frame still goes out so the
/// frontend can fall back to a basic title-only message.
///
/// A verification clause is appended when — and only when — the payload's
/// `verification_state` is one of the three settled ones. See
/// [`verification_voice_clause`] for why `unknown` adds nothing.
async fn render_task_completion_announcement(
    prompt_manager: Arc<PromptManager>,
    payload: &Value,
) -> String {
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .or_else(|| payload.get("task_id").and_then(|v| v.as_str()))
        .unwrap_or("background task")
        .to_string();
    let status = payload
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("completed");
    let verb = match status {
        "completed" => "finished",
        "failed" => "failed",
        "cancelled" => "was cancelled",
        other => return format!("Background task \"{title}\" is now {other}."),
    }
    .to_string();
    let summary = payload
        .get("summary")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let summary_suffix = if summary.trim().is_empty() {
        String::new()
    } else {
        format!(" {}", summary.trim())
    };
    let vars = std::collections::HashMap::from([
        ("title".to_string(), title),
        ("verb".to_string(), verb),
        ("summary_suffix".to_string(), summary_suffix),
    ]);
    let announcement = prompt_manager
        .get_rendered_prompt(
            prompt_constants::names::VOICE_TASK_COMPLETION_ANNOUNCEMENT,
            prompt_constants::versions::VOICE_TASK_COMPLETION_ANNOUNCEMENT,
            vars,
        )
        .await
        .unwrap_or_else(|err| {
            warn!(error = %err, "[VOICE-CONTROL] announcement render failed");
            String::new()
        });
    let clause = verification_voice_clause(
        prompt_manager,
        payload
            .get("verification_state")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown"),
    )
    .await;
    match clause {
        // The join is a space, in Rust, because it is punctuation between two
        // store-owned sentences rather than prose of its own. A render that
        // produced nothing keeps the clause alone rather than emitting a
        // leading space.
        Some(clause) if announcement.is_empty() => clause,
        Some(clause) => format!("{announcement} {clause}"),
        None => announcement,
    }
}

/// The one sentence voice is allowed to say about whether a run's code was
/// checked — or `None`, which is the answer for almost every task.
///
/// **`unknown` says nothing, and that is the contract, not a gap.**
/// `verification_state_for_task` documents it directly: a legacy run, a task
/// with no gate, and an unreadable store all read `Unknown`, and the
/// verification controller is inert unless
/// `MAGICIAN_VERIFICATION_CONTROLLER=enforce`, so `Unknown` is what
/// essentially every task in a normal deployment reports. Speaking it as
/// verified would be a lie about the common case; speaking it as unverified
/// would be a different lie — it asserts that checks were expected and did not
/// happen, which for a task that was never a coding run is simply not true.
/// Silence is the only claim that is correct in both.
///
/// The unsettled states (`repairing`, `verifying`) and the ones that describe
/// the controller rather than the code (`unavailable`, `cancelled`,
/// `blocked_partial`) are silent for the same reason: an announcement fires
/// once, and a sentence about a state that is still moving would be wrong by
/// the time it is heard.
async fn verification_voice_clause(
    prompt_manager: Arc<PromptManager>,
    verification_state: &str,
) -> Option<String> {
    let (name, version) = match verification_state {
        "verified" => (
            prompt_constants::names::VOICE_VERIFICATION_VERIFIED,
            prompt_constants::versions::VOICE_VERIFICATION_VERIFIED,
        ),
        "unverified" => (
            prompt_constants::names::VOICE_VERIFICATION_UNVERIFIED,
            prompt_constants::versions::VOICE_VERIFICATION_UNVERIFIED,
        ),
        "exhausted" => (
            prompt_constants::names::VOICE_VERIFICATION_EXHAUSTED,
            prompt_constants::versions::VOICE_VERIFICATION_EXHAUSTED,
        ),
        _ => return None,
    };
    match prompt_manager
        .get_rendered_prompt(name, version, std::collections::HashMap::new())
        .await
    {
        Ok(clause) if !clause.trim().is_empty() => Some(clause.trim().to_string()),
        Ok(_) => None,
        Err(err) => {
            // No compiled fallback on purpose. Every fallback here would be a
            // second copy of a sentence whose exact wording is the feature,
            // and a drifted copy that claims checks passed is the one failure
            // this whole clause exists to prevent. A missing template
            // degrades to the announcement without it — which is what
            // `unknown` already does, so the degraded behaviour is a state
            // the system is designed for.
            warn!(
                error = %err,
                verification_state,
                "[VOICE-CONTROL] verification clause unavailable; announcing without it"
            );
            None
        },
    }
}

/// Render the spoken nudge for a run holding a staged diff.
///
/// Names the task's **title**, never its id: this string only ever exists to
/// be read aloud, and `task_` plus thirty-two hex characters is the rough edge
/// the started reply already carries (`vibedev-rail.md` §10). A payload with
/// no usable title therefore produces nothing rather than falling back to the
/// id — silence beats spelling out a uuid.
async fn render_diff_approval_announcement(
    prompt_manager: Arc<PromptManager>,
    payload: &Value,
) -> String {
    let title = payload
        .get("title")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("");
    if title.is_empty() {
        return String::new();
    }
    let changed_file_count = payload
        .get("changed_file_count")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let file_phrase = spoken_file_count(changed_file_count);
    let vars = std::collections::HashMap::from([
        ("title".to_string(), title.to_string()),
        ("file_phrase".to_string(), file_phrase),
    ]);
    prompt_manager
        .get_rendered_prompt(
            prompt_constants::names::VOICE_DIFF_APPROVAL_WAITING,
            prompt_constants::versions::VOICE_DIFF_APPROVAL_WAITING,
            vars,
        )
        .await
        .unwrap_or_else(|err| {
            warn!(error = %err, "[VOICE-CONTROL] diff-approval announcement render failed");
            String::new()
        })
}

/// Counts read aloud are words up to ten and digits after, which is how a
/// person says them. Beyond that the exact number stops being the point, so
/// the digits carry it.
fn spoken_file_count(count: u64) -> String {
    match count {
        0 => "no files".to_string(),
        1 => "one file".to_string(),
        2 => "two files".to_string(),
        3 => "three files".to_string(),
        4 => "four files".to_string(),
        5 => "five files".to_string(),
        6 => "six files".to_string(),
        7 => "seven files".to_string(),
        8 => "eight files".to_string(),
        9 => "nine files".to_string(),
        10 => "ten files".to_string(),
        other => format!("{other} files"),
    }
}

impl StreamHandler<Result<ws::Message, ws::ProtocolError>> for VoiceControlSession {
    fn handle(&mut self, msg: Result<ws::Message, ws::ProtocolError>, ctx: &mut Self::Context) {
        match msg {
            Ok(ws::Message::Ping(payload)) => ctx.pong(&payload),
            Ok(ws::Message::Pong(_)) => {},
            Ok(ws::Message::Text(text)) => {
                self.handle_text_message(text.as_ref(), ctx);
            },
            Ok(ws::Message::Binary(bytes)) => {
                // Binary frames carry browser → provider PCM for
                // `BackendProxied` providers. Forward to the
                // upstream sender once the backend-proxied provider
                // has received its configure/resume frames. Before
                // then we queue a bounded ordered buffer so live PTT
                // startup audio is not lost.
                if should_suppress_half_duplex_input(
                    self.hands_free,
                    self.half_duplex,
                    self.assistant_audio_response_id.is_some(),
                ) {
                    // This is an authoritative transport boundary, not merely
                    // transcript cleanup: echoed PCM must never reach either
                    // local STT or the cascaded provider while it is speaking.
                    return;
                }
                let frame = bytes.to_vec();
                if !self.logged_first_upstream_audio && !frame.is_empty() {
                    self.logged_first_upstream_audio = true;
                    info!(
                        voice_session_id = %self.voice_session_id,
                        bytes = frame.len(),
                        started = self.started,
                        provider_configured = self.provider_configured,
                        "[VOICE-CONTROL] first upstream PCM frame"
                    );
                }
                self.send_local_transcript_audio(frame.clone(), ctx);
                self.send_or_queue_provider_audio(frame);
            },
            Ok(ws::Message::Close(reason)) => {
                debug!(
                    voice_session_id = %self.voice_session_id,
                    reason = ?reason,
                    "[VOICE-CONTROL] close frame"
                );
                ctx.stop();
            },
            Ok(ws::Message::Continuation(_)) | Ok(ws::Message::Nop) => {},
            Err(error) => {
                if is_expected_websocket_disconnect_message(&error.to_string()) {
                    debug!(
                        voice_session_id = %self.voice_session_id,
                        error = %error,
                        "[VOICE-CONTROL] client disconnected before completing frame"
                    );
                } else {
                    error!(
                        voice_session_id = %self.voice_session_id,
                        error = %error,
                        "[VOICE-CONTROL] protocol error"
                    );
                }
                ctx.stop();
            },
        }
    }
}

/// The client's composer chat choice from `session.start` (`chat_choice`:
/// `{engine, model?, profile?}`), validated as a typed turn's is. The call's
/// chat turns think with it; an absent, malformed, or not-installed choice
/// is dropped and the call follows `chat.harness_engine`.
fn requested_chat_choice(
    body: &Value,
) -> Option<magician::magician_v2::execution::plane::ChatHarnessChoice> {
    let choice = body.get("chat_choice")?.as_object()?;
    let engine = choice.get("engine")?.as_str()?;
    match magician::magician_v2::execution::plane::validated_client_chat_harness_choice(
        engine,
        choice.get("model").and_then(Value::as_str),
        choice.get("profile").and_then(Value::as_str),
    ) {
        Ok(choice) => Some(choice),
        Err(reason) => {
            warn!(%reason, "voice session.start chat_choice ignored");
            None
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use magician_media::media_rails::SurfaceType;
    use serde_json::json;

    /// A realtime call still open when the person hangs up has not failed.
    /// `session.end` marked it `cancelled` before the provider was even told
    /// to stop, so a provider with no per-turn terminal event at all —
    /// GPT-Live, which reports one usage figure for the whole session —
    /// recorded every session as a cancelled failure, with no metered row and
    /// no chat-turn attribution. `cancelled` belongs to a call the transport
    /// really did cut short.
    #[test]
    fn a_clean_session_end_is_not_a_cancellation() {
        assert_eq!(
            in_flight_call_failure_class(SessionEndKind::ClientRequested),
            None
        );
        assert_eq!(
            in_flight_call_failure_class(SessionEndKind::TransportDropped),
            Some("cancelled")
        );
    }

    /// The window a clean end waits for the provider's closing report before
    /// it stops the socket. Long enough for a close frame to cross, short
    /// enough that hanging up still feels immediate.
    #[test]
    fn the_closing_report_grace_is_bounded() {
        assert!(PROVIDER_CLOSING_REPORT_GRACE >= std::time::Duration::from_millis(500));
        assert!(PROVIDER_CLOSING_REPORT_GRACE <= std::time::Duration::from_secs(3));
    }

    /// A speech start that interrupts nothing cancels nothing. Both
    /// speech-start paths reported the in-flight call as `cancelled`
    /// unconditionally, which on a provider with no per-turn terminal event
    /// stamped a failure on a response that had already finished speaking.
    /// The question is whether audio was still playing — never whether a
    /// call was open, which is the call being reported on.
    #[test]
    fn a_speech_start_with_nothing_speaking_cancels_nothing() {
        assert!(!speech_start_interrupts_a_response(false, false));
        assert!(speech_start_interrupts_a_response(true, false));
        assert!(speech_start_interrupts_a_response(false, true));
    }

    #[test]
    fn turn_context_item_id_fits_the_openai_realtime_limit() {
        let item_id = new_turn_context_item_id();

        assert_eq!(item_id.len(), OPENAI_REALTIME_ITEM_ID_MAX_BYTES);
        assert!(item_id.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn browser_upgrade_selects_the_voice_protocol_without_echoing_the_bearer() {
        use actix_web::http::header;

        let request = actix_web::test::TestRequest::get()
            .insert_header((header::UPGRADE, "websocket"))
            .insert_header((header::CONNECTION, "upgrade"))
            .insert_header((header::SEC_WEBSOCKET_VERSION, "13"))
            .insert_header((header::SEC_WEBSOCKET_KEY, "dGhlIHNhbXBsZSBub25jZQ=="))
            .insert_header((
                header::SEC_WEBSOCKET_PROTOCOL,
                "magician-voice-control-v1, magician-bearer.secret-token",
            ))
            .to_http_request();
        let response = ws::handshake_with_protocols(&request, VOICE_CONTROL_WEBSOCKET_PROTOCOLS)
            .expect("valid voice-control WebSocket handshake")
            .finish();

        assert_eq!(
            response
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|value| value.to_str().ok()),
            Some("magician-voice-control-v1")
        );
        assert!(!response
            .headers()
            .iter()
            .filter_map(|(_, value)| value.to_str().ok())
            .any(|value| value.contains("secret-token")));
    }

    #[test]
    fn gemini_translate_setup_1007_is_fatal_and_does_not_rotate() {
        assert!(is_fatal_realtime_setup_error(
            "realtime upstream: Gemini Live connection closed (1007: Invalid JSON payload received. Unknown name \"inputAudioTranscription\" at 'setup.generation_config': Cannot find field.)"
        ));
        assert!(!is_fatal_realtime_setup_error(
            "OpenAI Realtime connection closed (1000: normal)"
        ));
    }

    #[test]
    fn half_duplex_hands_free_drops_input_only_while_assistant_audio_is_active() {
        assert!(should_suppress_half_duplex_input(true, true, true));
        assert!(!should_suppress_half_duplex_input(true, true, false));
        assert!(!should_suppress_half_duplex_input(true, false, true));
        assert!(!should_suppress_half_duplex_input(false, true, true));
    }

    #[test]
    fn pre_ready_provider_trim_warning_is_rate_limited() {
        let started = std::time::Instant::now();
        let mut last = None;

        assert!(should_emit_pending_provider_trim_warning(
            &mut last, started
        ));
        assert!(!should_emit_pending_provider_trim_warning(
            &mut last,
            started + PENDING_PROVIDER_TRIM_LOG_INTERVAL / 2
        ));
        assert!(should_emit_pending_provider_trim_warning(
            &mut last,
            started + PENDING_PROVIDER_TRIM_LOG_INTERVAL
        ));
    }

    #[test]
    fn direct_realtime_usage_control_preserves_every_billing_bucket() {
        let usage = realtime_usage_from_control(&json!({
            "usage": {
                "text_input_tokens": 3,
                "text_cached_input_tokens": 2,
                "text_output_tokens": 1,
                "audio_input_tokens": 5,
                "audio_cached_input_tokens": 4,
                "audio_output_tokens": 6
            }
        }))
        .expect("valid direct realtime usage");

        assert_eq!(usage.text_input_tokens, 3);
        assert_eq!(usage.text_cached_input_tokens, 2);
        assert_eq!(usage.text_output_tokens, 1);
        assert_eq!(usage.audio_input_tokens, 5);
        assert_eq!(usage.audio_cached_input_tokens, 4);
        assert_eq!(usage.audio_output_tokens, 6);
    }

    #[test]
    fn malformed_direct_realtime_usage_does_not_invent_a_billing_split() {
        assert!(realtime_usage_from_control(&json!({
            "usage": { "audio_input_tokens": "unknown" }
        }))
        .is_none());
    }

    #[test]
    fn native_realtime_turn_boundary_is_independent_of_provider_mode() {
        assert_eq!(
            requested_realtime_turn_detection(
                &json!({ "voice_mode": "realtime", "turn_boundary": "server_vad" }),
                false,
            )
            .as_deref(),
            Some("server_vad")
        );
        assert_eq!(
            requested_realtime_turn_detection(
                &json!({ "voice_mode": "realtime", "turn_boundary": "push_to_talk" }),
                false,
            )
            .as_deref(),
            Some("none")
        );
    }

    #[test]
    fn cascaded_hands_free_ignores_realtime_turn_boundary_override() {
        assert_eq!(
            requested_realtime_turn_detection(
                &json!({ "voice_mode": "hands_free", "turn_boundary": "push_to_talk" }),
                true,
            ),
            None
        );
        assert_eq!(
            requested_realtime_turn_detection(&json!({ "turn_boundary": "unexpected" }), false),
            None
        );
    }

    #[test]
    fn native_client_can_override_voice_prefix_per_call() {
        assert_eq!(
            requested_voice_prefix_override(&json!({ "require_voice_prefix": true })),
            Some(true)
        );
        assert_eq!(
            requested_voice_prefix_override(&json!({ "require_voice_prefix": false })),
            Some(false)
        );
        assert_eq!(requested_voice_prefix_override(&json!({})), None);
        assert_eq!(
            requested_voice_prefix_override(&json!({ "require_voice_prefix": "true" })),
            None
        );
    }

    /// Gap-closing test for the surface mapping: it must be **total**, and the
    /// arm that means "I could not identify this caller" must resolve to the
    /// room, not to owner voice.
    ///
    /// The failure this prevents is silent: a client that omits `surface_type`
    /// deserializes as `Unknown`, and if `Unknown` mapped to an owner surface
    /// then simply saying nothing would buy owner authority.
    #[test]
    fn an_unidentified_registration_resolves_to_a_room_not_to_owner_voice() {
        use magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE;

        // The fail-closed arms.
        for surface in [SurfaceType::Unknown, SurfaceType::Extension] {
            assert_eq!(
                voice_source_surface(surface),
                MEETING_ROOM_SOURCE_SURFACE,
                "{surface:?} must not resolve to an owner surface"
            );
        }
        // The bot says what it is, and gets the room it asked for.
        assert_eq!(
            voice_source_surface(SurfaceType::MeetingBot),
            MEETING_ROOM_SOURCE_SURFACE
        );
        // Identified first-party surfaces keep owner voice — this is the
        // regression half: closing the default must not move real clients.
        for surface in [
            SurfaceType::WebDesktop,
            SurfaceType::WebMobile,
            SurfaceType::TrayMacos,
            SurfaceType::MascotMacos,
            SurfaceType::EspTerminal,
        ] {
            assert_ne!(
                voice_source_surface(surface),
                MEETING_ROOM_SOURCE_SURFACE,
                "{surface:?} is an identified owner surface and must not become a room"
            );
        }
    }

    #[test]
    fn guided_voice_capabilities_separate_grammar_capture_and_native_handoff() {
        let web = guided_voice_capabilities(SurfaceType::WebDesktop, Some("Mozilla/5.0"));
        assert_eq!(
            web,
            GuidedVoiceCapabilities {
                expanded_grammar: true,
                screen_capture: true,
                client_blackboard_handoff: false,
            }
        );

        let ios = guided_voice_capabilities(SurfaceType::WebMobile, Some("Magios-iOS/1.0"));
        assert_eq!(
            ios,
            GuidedVoiceCapabilities {
                expanded_grammar: true,
                screen_capture: false,
                client_blackboard_handoff: true,
            }
        );

        let tray = guided_voice_capabilities(SurfaceType::TrayMacos, None);
        assert_eq!(
            tray,
            GuidedVoiceCapabilities {
                expanded_grammar: false,
                screen_capture: true,
                client_blackboard_handoff: false,
            }
        );
    }

    #[test]
    fn web_takeover_normalizes_quick_and_selects_screen_capture() {
        let request = voice_tutor_takeover_request(
            "Tutor Quick screen explain this graph",
            GuidedVoiceCapabilities {
                expanded_grammar: true,
                screen_capture: true,
                client_blackboard_handoff: false,
            },
        )
        .expect("web guided-flow request");
        assert_eq!(
            request.feature_mode,
            magician::magician_v2::agents::FeatureMode::Tutor
        );
        assert_eq!(
            request.canvas_mode,
            magician::magician_v2::tutor::TutorCanvasMode::ScreenOverlay
        );
        assert!(request.quick);
        assert!(request.capture_screen);
        assert_eq!(request.text, "@tutor #quick screen explain this graph");
        assert_eq!(request.reason, "explicit_guided_flow_invoke");
        assert!(!request.client_blackboard_handoff);
    }

    #[test]
    fn ios_takeover_hands_source_free_tutor_and_quick_to_the_native_blackboard() {
        let capabilities =
            guided_voice_capabilities(SurfaceType::WebMobile, Some("Magios-iOS/1.0"));
        let tutor = voice_tutor_takeover_request("Tutor explain recursion", capabilities)
            .expect("native iOS blackboard request");
        assert_eq!(tutor.text, "@tutor explain recursion");
        assert_eq!(
            tutor.canvas_mode,
            magician::magician_v2::tutor::TutorCanvasMode::Blackboard
        );
        assert!(!tutor.capture_screen);
        assert!(tutor.client_blackboard_handoff);
        assert!(tutor.rejection.is_none());

        let quick =
            voice_tutor_takeover_request("Tutor Quick blackboard explain recursion", capabilities)
                .expect("native iOS quick blackboard request");
        assert!(quick.quick);
        assert_eq!(quick.text, "@tutor #quick blackboard explain recursion");
        assert!(quick.client_blackboard_handoff);
    }

    #[test]
    fn ios_rejects_screen_tutor_and_app_copilot_without_falling_through() {
        let capabilities =
            guided_voice_capabilities(SurfaceType::WebMobile, Some("Magios-iOS/1.0"));
        let tutor = voice_tutor_takeover_request("Tutor screen explain this", capabilities)
            .expect("native iOS screen tutor rejection");
        assert!(!tutor.capture_screen);
        assert!(!tutor.client_blackboard_handoff);
        assert!(tutor.rejection.is_some());
        assert_eq!(
            unsupported_guided_flow_message(tutor.rejection.expect("rejection")),
            "Screen tutoring isn't available on this device yet. Say Tutor blackboard instead."
        );

        let copilot =
            voice_tutor_takeover_request("App Copilot show me the next step", capabilities)
                .expect("native iOS App Copilot rejection");
        assert!(!copilot.capture_screen);
        assert!(!copilot.client_blackboard_handoff);
        assert!(copilot.rejection.is_some());
        assert_eq!(
            unsupported_guided_flow_message(copilot.rejection.expect("rejection")),
            "App Copilot isn't available on this device yet."
        );
    }

    #[test]
    fn non_web_takeover_keeps_the_legacy_exact_contract() {
        let capabilities = GuidedVoiceCapabilities {
            expanded_grammar: false,
            screen_capture: true,
            client_blackboard_handoff: false,
        };
        assert!(voice_tutor_takeover_request("Tutor screen explain this", capabilities).is_none());

        let tutor = voice_tutor_takeover_request("Hey Tutor screen explain this", capabilities)
            .expect("legacy tutor request");
        assert_eq!(
            tutor.canvas_mode,
            magician::magician_v2::tutor::TutorCanvasMode::Blackboard
        );
        assert!(!tutor.capture_screen);
        assert_eq!(tutor.text, "Hey Tutor screen explain this");

        let copilot =
            voice_tutor_takeover_request("Hey App Copilot show me the next step", capabilities)
                .expect("legacy app copilot request");
        assert!(copilot.capture_screen);
        assert_eq!(
            copilot.canvas_mode,
            magician::magician_v2::tutor::TutorCanvasMode::ScreenOverlay
        );

        let captureless_copilot = voice_tutor_takeover_request(
            "Hey App Copilot show me the next step",
            GuidedVoiceCapabilities {
                screen_capture: false,
                ..capabilities
            },
        )
        .expect("captureless native rejection");
        assert!(!captureless_copilot.capture_screen);
        assert!(captureless_copilot.rejection.is_some());
    }

    #[test]
    fn screen_lock_state_and_spoken_rejections_are_exact() {
        assert_eq!(
            requested_screen_locked(&json!({ "screen_locked": true })),
            Some(true)
        );
        assert_eq!(
            requested_screen_locked(&json!({ "locked": false })),
            Some(false)
        );
        assert_eq!(
            requested_screen_locked(&json!({ "screen_locked": "true" })),
            None
        );
        assert_eq!(
            requested_screen_locked(&json!({ "screen_locked": null, "locked": true })),
            Some(true)
        );
        assert_eq!(requested_screen_locked(&json!({})), None);
        assert_eq!(
            locked_guided_flow_message(magician::magician_v2::agents::FeatureMode::Tutor),
            "Please unlock your screen to use Tutor."
        );
        assert_eq!(
            locked_guided_flow_message(magician::magician_v2::agents::FeatureMode::AppCopilot),
            "Please unlock your screen to use App Copilot."
        );

        let mut locked = false;
        let mut keys = HashSet::new();
        apply_screen_lock_state(&mut locked, &mut keys, true);
        assert!(locked);
        assert!(admit_locked_guided_flow_rejection(
            &mut keys,
            "same-command".to_string()
        ));
        assert!(!admit_locked_guided_flow_rejection(
            &mut keys,
            "same-command".to_string()
        ));
        apply_screen_lock_state(&mut locked, &mut keys, false);
        assert!(!locked);
        assert!(keys.is_empty());
        apply_screen_lock_state(&mut locked, &mut keys, true);
        assert!(admit_locked_guided_flow_rejection(
            &mut keys,
            "same-command".to_string()
        ));
    }

    #[test]
    fn lock_edge_cancels_active_takeover_and_keeps_unlock_retryable() {
        use magician::magician_v2::agents::FeatureMode;

        let cancellation = tokio_util::sync::CancellationToken::new();
        let mut active = HashMap::new();
        active.insert(
            "takeover-1".to_string(),
            (cancellation.clone(), FeatureMode::Tutor),
        );
        let mut lock_cancelled = HashSet::new();
        let mut rejection_keys = HashSet::new();

        assert_eq!(
            cancel_active_tutor_takeovers_for_lock(
                &active,
                &mut lock_cancelled,
                &mut rejection_keys,
            ),
            Some(FeatureMode::Tutor)
        );
        assert!(cancellation.is_cancelled());
        assert!(lock_cancelled.contains("takeover-1"));
        assert!(rejection_keys.contains("takeover-1"));

        let mut locked = true;
        apply_screen_lock_state(&mut locked, &mut rejection_keys, false);
        assert!(rejection_keys.is_empty());
        assert!(lock_cancelled.contains("takeover-1"));
    }

    #[test]
    fn tutor_response_suppression_waits_for_a_late_provider_terminal() {
        let mut suppression = TutorResponseSuppression::default();
        suppression.begin(None);

        suppression.finish_takeover(false);
        assert!(suppression.suppresses_response_event("cancelled-response"));
        assert!(suppression.observe_provider_terminal(Some("cancelled-response"), false));
        assert!(!suppression.suppresses_response_event("next-response"));
    }

    #[test]
    fn tutor_response_suppression_releases_after_both_sides_finish_in_either_order() {
        let mut suppression = TutorResponseSuppression::default();
        suppression.begin(Some("cancelled-response".to_string()));

        assert!(suppression.observe_provider_terminal(Some("cancelled-response"), true));
        assert!(suppression.suppresses_response_event("cancelled-response"));
        assert!(!suppression.suppresses_response_event("other-response"));
        suppression.finish_takeover(false);
        assert!(!suppression.suppresses_response_event("cancelled-response"));
        assert!(!suppression.observe_provider_terminal(Some("cancelled-response"), false));
    }

    #[test]
    fn tutor_response_suppression_does_not_consume_the_next_user_generation() {
        let mut suppression = TutorResponseSuppression::default();
        suppression.begin(None);
        suppression.finish_takeover(false);
        assert!(suppression.active);

        suppression.begin_new_user_generation(false);
        assert!(!suppression.active);

        suppression.begin(Some("late-old-response".to_string()));
        suppression.finish_takeover(false);
        suppression.begin_new_user_generation(false);
        assert!(suppression.suppresses_response_event("late-old-response"));
        assert!(!suppression.suppresses_response_event("new-response"));

        suppression.begin(None);
        suppression.begin_new_user_generation(true);
        assert!(suppression.active);
    }

    #[test]
    fn tutor_response_suppression_does_not_swallow_failure_announcement_generation() {
        let mut unidentified = TutorResponseSuppression::default();
        unidentified.begin(None);
        unidentified.prepare_failure_announcement();
        assert!(!unidentified.suppresses_response_event("failure-announcement"));

        let mut identified = TutorResponseSuppression::default();
        identified.begin(Some("cancelled-response".to_string()));
        identified.prepare_failure_announcement();
        assert!(identified.suppresses_response_event("cancelled-response"));
        assert!(!identified.suppresses_response_event("failure-announcement"));
    }

    #[test]
    fn tutor_response_suppression_drops_only_the_owned_provider_audio() {
        let mut suppression = TutorResponseSuppression::default();
        suppression.begin(Some("cancelled-response".to_string()));

        assert!(provider_audio_frame_is_suppressed(
            &mut suppression,
            Some("cancelled-response"),
        ));
        assert!(!provider_audio_frame_is_suppressed(
            &mut suppression,
            Some("new-response"),
        ));
        assert!(provider_audio_frame_is_suppressed(&mut suppression, None));
    }

    #[test]
    fn stale_provider_terminal_does_not_target_a_newer_response() {
        assert!(!terminal_targets_current_provider_response(
            Some("new-response"),
            Some("old-response"),
            Some("old-response"),
        ));
        assert!(!terminal_targets_current_provider_response(
            Some("new-response"),
            Some("old-response"),
            None,
        ));
        assert!(terminal_targets_current_provider_response(
            Some("new-response"),
            Some("new-response"),
            Some("old-response"),
        ));
        assert!(terminal_targets_current_provider_response(
            Some("new-response"),
            None,
            Some("old-response"),
        ));
        assert!(!terminal_targets_current_provider_response(
            None,
            Some("old-response"),
            Some("old-response"),
        ));
    }

    #[test]
    fn realtime_turn_identity_is_scoped_and_unique_before_completion() {
        let first = new_realtime_llm_correlation("owner", "workspace");
        let second = new_realtime_llm_correlation("owner", "workspace");

        assert!(!first.trace_id.is_empty());
        assert!(!first.llm_call_id.is_empty());
        assert_ne!(first.llm_call_id, second.llm_call_id);
        assert_eq!(first.scope_resolution, "explicit");
        assert_eq!(first.workload_class, "foreground_chat");
        assert_eq!(first.provider_attempt_count, 1);
    }
    use magician_media::media_rails::{ReplayToolExchange, ReplayTurn};
    use magicllm::realtime::{
        RealtimeAudioTopology, RealtimeProviderKind, RealtimeSessionDescriptor,
    };

    #[test]
    fn session_ready_payload_includes_initial_resume_context() {
        let descriptor = RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAi,
            model: "gpt-realtime-2".to_string(),
            topology: RealtimeAudioTopology::DirectPeerToPeer,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: Some("marin".to_string()),
            webrtc_url: Some("https://example.invalid/realtime".to_string()),
            upstream_token: Some("token".to_string()),
            upstream_provider_session_id: None,
            max_session_duration_secs: Some(1_800),
            native_resume_handle: None,
            transcription_model: Some("whisper-1".to_string()),
            transcription_fallback_model: None,
            turn_detection_mode: Some("server_vad".to_string()),
            context_window_tokens: Some(32_000),
            half_duplex: None,
        };
        let resume = ResumeContext {
            summary: Some("Earlier text chat established the invoice date.".to_string()),
            recent_turns: vec![ReplayTurn {
                role: "user".to_string(),
                text: "Remember the invoice date for later.".to_string(),
                message_id: "msg-1".to_string(),
                created_at_ms: 123,
            }],
            tool_exchanges: Vec::new(),
            total_turns: 7,
        };

        let payload = session_ready_payload(
            "voice-1",
            descriptor,
            "instructions".to_string(),
            Vec::new(),
            Some("snapshot-1".to_string()),
            true,
            300,
            resume,
            &VoiceAddressing::new(true, ["Sam".to_string()]),
            &magician::magician_v2::presentation_identity::AgentPresentationIdentity {
                agent_id: "agent-sam".to_string(),
                name: "Sam".to_string(),
                aliases: vec!["Samantha".to_string()],
                wake_spellings: Vec::new(),
                is_primary: true,
            },
            Some(
                &magician_media::media_rails::voice_orchestrator::CallBoundary {
                    source_surface: magician::magician_v2::chat::MEETING_ROOM_SOURCE_SURFACE
                        .to_string(),
                    surface: "meeting",
                    audience: "untrusted",
                    agent_id: "envoy".to_string(),
                    binding: "0123456789abcdef".to_string(),
                },
            ),
        );

        assert_eq!(payload["voice_session_id"], "voice-1");
        assert_eq!(payload["policy_snapshot_id"], "snapshot-1");
        assert_eq!(
            payload["resume"]["summary"],
            "Earlier text chat established the invoice date."
        );
        assert_eq!(payload["resume"]["total_turns"], 7);
        assert_eq!(payload["addressing"]["required"], true);
        assert_eq!(payload["addressing"]["activation_phrases"][0], "Hey Sam");
        assert_eq!(payload["addressing"]["follow_up_window_ms"], 8_000);
        assert_eq!(payload["per_turn_context"]["enabled"], true);
        assert_eq!(payload["per_turn_context"]["budget_ms"], 300);
        assert_eq!(payload["agent"]["agent_id"], "agent-sam");
        assert_eq!(payload["agent"]["name"], "Sam");
        assert_eq!(payload["agent"]["aliases"][0], "Samantha");
        assert_eq!(payload["agent"]["is_primary"], true);
        assert_eq!(
            payload["resume"]["recent_turns"][0]["text"],
            "Remember the invoice date for later."
        );
        // The client is told what the backend resolved, and told it is not
        // negotiable. A UI that offered to change this would be advertising
        // that a room can ask to be trusted.
        assert_eq!(payload["boundary"]["surface"], "meeting");
        assert_eq!(payload["boundary"]["audience"], "untrusted");
        assert_eq!(payload["boundary"]["agent_id"], "envoy");
        assert_eq!(payload["boundary"]["elevatable"], false);
        assert!(
            payload["boundary"].get("binding").is_none(),
            "the binding hash is telemetry, not something to show a room"
        );
    }

    #[test]
    fn backend_resume_separates_summary_tools_and_recent_turns_for_ordered_native_replay() {
        let resume = ResumeContext {
            summary: Some("Earlier decisions.".to_string()),
            recent_turns: vec![
                ReplayTurn {
                    role: "user".to_string(),
                    text: "What did it find?".to_string(),
                    message_id: "message-1".to_string(),
                    created_at_ms: 1,
                },
                ReplayTurn {
                    role: "assistant".to_string(),
                    text: "It found the date.".to_string(),
                    message_id: "message-2".to_string(),
                    created_at_ms: 2,
                },
            ],
            tool_exchanges: vec![ReplayToolExchange {
                call_id: "call-1".to_string(),
                tool_name: "search_memory".to_string(),
                arguments: json!({
                    "query": "birthday",
                    "access_token": "escaped-argument-secret"
                }),
                projected_result: json!({
                    "data": { "value": "May 8" },
                    "authorization": "Bearer escaped-result-secret"
                }),
            }],
            total_turns: 2,
        };

        let summary = format_resume_summary_for_provider(&resume).expect("summary");
        let recent = format_resume_recent_turns_for_provider(&resume).expect("recent turns");

        assert!(summary.contains("Earlier decisions."));
        assert!(!summary.contains("search_memory"));
        assert!(!summary.contains("What did it find?"));
        assert!(recent.contains("user: What did it find?"));
        assert!(recent.contains("assistant: It found the date."));
        assert!(!recent.contains("search_memory"));
        assert_eq!(resume.tool_exchanges[0].call_id, "call-1");
        assert!(resume.tool_exchanges[0].projected_result.is_object());

        let gemini_controls =
            provider_resume_controls(resume.clone(), Some(RealtimeProviderKind::Gemini));
        assert_eq!(gemini_controls.len(), 2);
        assert!(!gemini_controls
            .iter()
            .any(|control| matches!(control, RealtimeAudioControl::InjectToolExchange { .. })));
        assert!(gemini_controls
            .iter()
            .all(|control| matches!(control, RealtimeAudioControl::InjectInitialHistory { .. })));

        let controls = provider_resume_controls(resume, Some(RealtimeProviderKind::OpenAi));
        assert_eq!(controls.len(), 3);
        assert!(matches!(
            &controls[0],
            RealtimeAudioControl::InjectInitialHistory { text }
                if text.contains("Earlier decisions.")
        ));
        let RealtimeAudioControl::InjectToolExchange {
            call_id,
            tool_name,
            arguments,
            projected_result,
        } = &controls[1]
        else {
            panic!("second resume control must be the balanced tool exchange")
        };
        assert_eq!(call_id, "call-1");
        assert_eq!(tool_name, "search_memory");
        assert_eq!(arguments["access_token"], "[REDACTED]");
        assert_eq!(projected_result["authorization"], "Bearer [REDACTED]");
        assert!(!arguments.to_string().contains("escaped-argument-secret"));
        assert!(!projected_result
            .to_string()
            .contains("escaped-result-secret"));
        assert!(matches!(
            &controls[2],
            RealtimeAudioControl::InjectInitialHistory { text }
                if text.contains("user: What did it find?")
        ));
        assert!(!controls
            .iter()
            .any(|control| matches!(control, RealtimeAudioControl::ToolResult { .. })));
    }

    #[test]
    fn local_transcript_is_requested_only_for_backend_openai_local_profiles() {
        let descriptor = RealtimeSessionDescriptor {
            provider: RealtimeProviderKind::OpenAi,
            model: "gpt-realtime-2.1-mini".to_string(),
            topology: RealtimeAudioTopology::BackendProxied,
            mode: magicllm::config::RealtimeVoiceMode::Assistant,
            voice: None,
            webrtc_url: None,
            upstream_token: None,
            upstream_provider_session_id: Some("upstream".to_string()),
            max_session_duration_secs: Some(1_800),
            native_resume_handle: None,
            transcription_model: Some("local".to_string()),
            transcription_fallback_model: Some("whisper-1".to_string()),
            turn_detection_mode: Some("none".to_string()),
            context_window_tokens: Some(128_000),
            half_duplex: None,
        };
        assert!(descriptor_requests_local_transcript(&descriptor));

        let mut vendor = descriptor.clone();
        vendor.transcription_model = Some("whisper-1".to_string());
        assert!(!descriptor_requests_local_transcript(&vendor));

        let mut direct = descriptor.clone();
        direct.topology = RealtimeAudioTopology::DirectPeerToPeer;
        assert!(!descriptor_requests_local_transcript(&direct));

        let mut local_cascade = descriptor;
        local_cascade.provider = RealtimeProviderKind::HandsFree;
        assert!(!descriptor_requests_local_transcript(&local_cascade));
    }

    #[test]
    fn local_transcript_controller_keeps_stream_turn_identities_separate() {
        let mut controller = LocalTranscriptController::default();
        assert!(controller
            .configure_requested(true, Some("configured-vendor-stt".to_string()), false)
            .is_empty());
        assert_eq!(
            controller.input_transcription_override().as_deref(),
            Some("configured-vendor-stt")
        );

        let first = controller.partial_identity(1);
        let second = controller.partial_identity(2);
        assert_ne!(first, second);
        assert_eq!(controller.partial_identity(1), first);
        assert_eq!(controller.final_identity(1), first);
        let next_server_vad_turn = controller.partial_identity(1);
        assert_ne!(next_server_vad_turn, first);
        assert_eq!(controller.final_identity(1), next_server_vad_turn);
        assert_eq!(controller.retire_identity(2), Some(second));
        assert!(!controller.accepts_transcript_event(2));
        assert!(controller.take_identities().is_empty());
    }

    #[test]
    fn local_transcript_controller_closes_vendor_coverage_once() {
        let mut controller = LocalTranscriptController::default();
        controller.begin_start(Some("configured-vendor-stt".to_string()), true);
        controller.start_failed("local_start_failed".to_string());
        controller.mark_vendor_fallback_active();

        let (handle, identities, terminal) = controller.end();
        assert!(handle.is_none());
        assert!(identities.is_empty());
        let terminal = terminal.expect("requested transcript path is audited");
        assert_eq!(terminal.local_covered_ms, 0);
        assert_eq!(terminal.queue.dropped_audio_frames, 0);

        let (_, _, duplicate) = controller.end();
        assert!(duplicate.is_none(), "terminal telemetry must emit once");
    }

    #[test]
    fn manual_local_transcript_stages_segments_until_commit_is_ready() {
        let mut controller = LocalTranscriptController::default();
        controller.configure_requested(true, Some("configured-vendor-stt".to_string()), true);

        let (first, item_id, turn_generation) =
            controller.stage_final(7, "schedule the review".to_string());
        assert_eq!(first, "schedule the review");
        let (combined, repeated_item_id, repeated_generation) =
            controller.stage_final(7, "tomorrow morning".to_string());
        assert_eq!(combined, "schedule the review tomorrow morning");
        assert_eq!(repeated_item_id, item_id);
        assert_eq!(repeated_generation, turn_generation);
        assert_eq!(
            controller.partial_with_staged(7, "at ten"),
            "schedule the review tomorrow morning at ten"
        );

        controller.mark_commit_pending(7, LocalTranscriptCommitMode::SendProviderCommit);
        assert_eq!(
            controller.take_committed_final(7),
            Some((combined, item_id, turn_generation))
        );
        assert!(controller.take_committed_final(7).is_none());
        assert!(!controller.accepts_transcript_event(7));
    }

    #[test]
    fn local_transcript_completion_retires_timing_for_textless_turns() {
        let mut controller = LocalTranscriptController::default();
        controller.record_audio(8);
        controller.record_commit(8);
        assert!(controller.record_first_partial(8).is_some());

        let completed = controller.complete_turn(8);
        assert!(completed.had_partial);
        assert!(completed.commit_to_final_latency_ms.is_some());
        assert!(!controller.turn_timings.contains_key(&8));

        controller.record_audio(9);
        controller.record_commit(9);
        let completed_without_partial = controller.complete_turn(9);
        assert!(!completed_without_partial.had_partial);
        assert!(!controller.turn_timings.contains_key(&9));
    }

    #[test]
    fn local_transcript_degradation_preserves_committed_generation_and_retires_other_partials() {
        let mut controller = LocalTranscriptController::default();
        let (_, committed_item, committed_turn) =
            controller.stage_final(4, "ready transcript".to_string());
        controller.mark_commit_pending(4, LocalTranscriptCommitMode::SendProviderCommit);
        let stale = controller.partial_identity(5);

        assert_eq!(controller.take_uncommitted_identities(), vec![stale]);
        assert_eq!(
            controller.take_committed_final(4),
            Some((
                "ready transcript".to_string(),
                committed_item,
                committed_turn
            ))
        );
    }

    #[test]
    fn local_transcript_server_vad_commit_mode_avoids_duplicate_provider_commit() {
        let mut controller = LocalTranscriptController::default();
        controller.stage_final(3, "server vad turn".to_string());
        controller.mark_commit_pending(3, LocalTranscriptCommitMode::ProviderAlreadyCommitted);

        assert_eq!(
            controller.pending_commit_mode(3),
            Some(LocalTranscriptCommitMode::ProviderAlreadyCommitted)
        );
        assert!(!controller
            .pending_commit_mode(3)
            .expect("pending server-VAD boundary")
            .requires_provider_commit());
        assert!(LocalTranscriptCommitMode::SendProviderCommit.requires_provider_commit());
    }

    #[test]
    fn tutor_takeover_key_normalizes_voice_transcript_spacing_and_case() {
        assert_eq!(
            tutor_takeover_key("voice-1", None, "  Hey   Tutor   Explain   Recursion  "),
            tutor_takeover_key("voice-1", None, "hey tutor explain recursion")
        );
        assert!(
            magician::magician_v2::tutor::is_tutor_or_app_copilot_prompt(
                "hey copilot show me how to create a note"
            )
        );
        assert_ne!(
            tutor_takeover_key("voice-1", None, "hey tutor explain recursion"),
            tutor_takeover_key("voice-2", None, "hey tutor explain recursion")
        );
        assert_ne!(
            tutor_takeover_key("voice-1", Some("item-a"), "hey tutor explain recursion"),
            tutor_takeover_key("voice-1", Some("item-b"), "hey tutor explain recursion")
        );
    }

    #[tokio::test]
    async fn provider_control_dispatcher_preserves_actor_enqueue_order_under_backpressure() {
        let (provider_tx, mut provider_rx) = tokio::sync::mpsc::channel(1);
        let dispatch = spawn_provider_control_dispatcher(provider_tx);

        dispatch
            .send(RealtimeAudioControl::ClearInput)
            .expect("dispatcher open");
        dispatch
            .send(RealtimeAudioControl::InterruptResponse)
            .expect("dispatcher open");
        dispatch
            .send(RealtimeAudioControl::End)
            .expect("dispatcher open");

        assert!(matches!(
            provider_rx.recv().await,
            Some(RealtimeAudioControl::ClearInput)
        ));
        assert!(matches!(
            provider_rx.recv().await,
            Some(RealtimeAudioControl::InterruptResponse)
        ));
        assert!(matches!(
            provider_rx.recv().await,
            Some(RealtimeAudioControl::End)
        ));
    }

    /// Build a `PromptManager` against the live prompt directory (the
    /// same store the orchestrator uses at boot). Pulls in the
    /// `voice_task_completion_announcement_v1.0.0.json` fixture so
    /// the announcement test exercises the real template, not a
    /// hand-rolled stand-in.
    async fn announcement_prompt_manager() -> Arc<PromptManager> {
        use magician::magician_v2::prompts::{json_storage::JsonStorageConfig, JsonPromptStorage};
        let storage = JsonPromptStorage::new(JsonStorageConfig {
            storage_dir: magician::magician_v2::prompts::json_storage::default_prompt_dir(),
            enable_cache: false,
            max_cache_entries: 10,
        })
        .expect("json storage construction");
        let manager = Arc::new(PromptManager::new(Arc::new(storage)));
        manager
            .initialize()
            .await
            .expect("prompt manager initialise");
        manager
    }

    /// Completion announcement renders the success template with
    /// title + verb + trailing summary. Pins the end-to-end contract
    /// the voice downstream-fanout → control WS actor → realtime
    /// model path depends on. Tests the rendering function the
    /// `task.completed` handler invokes directly — substantially
    /// cheaper than wiring an actix WS test harness, and the
    /// remaining hop (`addr.do_send(DownstreamFrame { ... })` +
    /// `send_envelope`) is pure JSON forwarding.
    #[tokio::test]
    async fn task_completed_announcement_renders_finished_verb_with_summary() {
        let manager = announcement_prompt_manager().await;
        let payload = json!({
            "task_id": "task-7a2",
            "title": "Send the Q3 brief",
            "status": "completed",
            "summary": "Sent to the team.",
        });
        let rendered = render_task_completion_announcement(manager, &payload).await;
        // Template shape: `Background task "{title}" {verb}.{summary_suffix}`
        // where verb=finished for completed, summary_suffix=" Sent to the team."
        assert_eq!(
            rendered,
            "Background task \"Send the Q3 brief\" finished. Sent to the team."
        );
    }

    /// Failed-status renders `failed` verb. Tests the per-status
    /// branch in the renderer.
    #[tokio::test]
    async fn task_completed_announcement_renders_failed_status() {
        let manager = announcement_prompt_manager().await;
        let payload = json!({
            "task_id": "task-7a2",
            "title": "Send the Q3 brief",
            "status": "failed",
            "summary": "",
        });
        let rendered = render_task_completion_announcement(manager, &payload).await;
        // Empty summary collapses to no trailing suffix.
        assert_eq!(rendered, "Background task \"Send the Q3 brief\" failed.");
    }

    /// Missing `title` falls back to `task_id`. Pins the safety net
    /// when the upstream service emits a task without a title.
    #[tokio::test]
    async fn task_completed_announcement_falls_back_to_task_id_when_title_missing() {
        let manager = announcement_prompt_manager().await;
        let payload = json!({
            "task_id": "task-7a2",
            "status": "completed",
            "summary": "Done.",
        });
        let rendered = render_task_completion_announcement(manager, &payload).await;
        assert_eq!(rendered, "Background task \"task-7a2\" finished. Done.");
    }

    /// Unknown status (e.g. a future state the renderer wasn't
    /// updated for) short-circuits to a generic fallback — never
    /// returns empty so the voice model always has something to
    /// speak.
    #[tokio::test]
    async fn task_completed_announcement_handles_unknown_status_gracefully() {
        let manager = announcement_prompt_manager().await;
        let payload = json!({
            "task_id": "task-7a2",
            "title": "Send the Q3 brief",
            "status": "paused",
            "summary": "Waiting for approval.",
        });
        let rendered = render_task_completion_announcement(manager, &payload).await;
        // Unknown status: renderer returns its own format directly
        // (doesn't hit the PromptManager).
        assert_eq!(
            rendered,
            "Background task \"Send the Q3 brief\" is now paused."
        );
    }

    fn completion_payload(verification_state: &str) -> Value {
        json!({
            "task_id": "task-7a2",
            "title": "Fix the footer spacing",
            "status": "completed",
            "summary": "",
            "verification_state": verification_state,
        })
    }

    /// Words that would tell a listener something about whether the code was
    /// checked. `unknown` must contain **none** of them — the assertion that
    /// matters here is the absence, because a clause that merely omitted the
    /// word "verified" while still saying "checks" would leave the same wrong
    /// impression.
    const VERIFICATION_WORDS: [&str; 6] =
        ["check", "verif", "passed", "repair", "proven", "tested"];

    /// The four states produce four different things to say, and they are
    /// distinguishable from each other — not merely non-empty. Asserted in one
    /// test because the property is about the SET: three identical clauses
    /// would satisfy any per-state assertion while telling the user nothing.
    #[tokio::test]
    async fn each_verification_state_gets_its_own_spoken_wording() {
        let manager = announcement_prompt_manager().await;
        let verified = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("verified"),
        )
        .await;
        let unverified = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("unverified"),
        )
        .await;
        let exhausted = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("exhausted"),
        )
        .await;
        let unknown = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("unknown"),
        )
        .await;

        for (label, rendered) in [
            ("verified", &verified),
            ("unverified", &unverified),
            ("exhausted", &exhausted),
        ] {
            assert!(
                rendered.len() > unknown.len(),
                "{label} must add a clause to the announcement, got {rendered:?}"
            );
            assert!(
                rendered.starts_with(&unknown),
                "{label} must EXTEND the completion sentence rather than replace it"
            );
        }
        assert_ne!(verified, unverified);
        assert_ne!(verified, exhausted);
        assert_ne!(unverified, exhausted);

        // Verified is the only one allowed to sound like good news, and
        // exhausted must not: repair giving up is a failure, and a listener
        // skimming for "passed" must not find it there.
        assert!(verified.to_lowercase().contains("passed"));
        assert!(!exhausted.to_lowercase().contains("passed"));
        assert!(!unverified.to_lowercase().contains("passed"));
    }

    /// The load-bearing one. `unknown` is the DEFAULT and, with the
    /// verification controller inert, what essentially every task reports —
    /// so anything it implied would be implied about the whole system.
    #[tokio::test]
    async fn unknown_verification_says_nothing_at_all_about_checks() {
        let manager = announcement_prompt_manager().await;
        let unknown = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("unknown"),
        )
        .await;

        assert_eq!(
            unknown, "Background task \"Fix the footer spacing\" finished.",
            "unknown must render the plain completion sentence and nothing more"
        );
        let lowered = unknown.to_lowercase();
        for word in VERIFICATION_WORDS {
            assert!(
                !lowered.contains(word),
                "unknown must not imply anything about checks, but the \
                 announcement contains {word:?}: {unknown:?}"
            );
        }
    }

    /// A producer that predates the field, and every genuinely unsettled
    /// state, land on the same silence rather than on a guess.
    #[tokio::test]
    async fn an_absent_or_unsettled_verification_state_adds_no_clause() {
        let manager = announcement_prompt_manager().await;
        let baseline = render_task_completion_announcement(
            Arc::clone(&manager),
            &completion_payload("unknown"),
        )
        .await;

        let absent = render_task_completion_announcement(
            Arc::clone(&manager),
            &json!({
                "task_id": "task-7a2",
                "title": "Fix the footer spacing",
                "status": "completed",
                "summary": "",
            }),
        )
        .await;
        assert_eq!(absent, baseline, "a payload with no field must not change");

        for unsettled in [
            "repairing",
            "verifying",
            "unavailable",
            "cancelled",
            "blocked_partial",
        ] {
            let rendered = render_task_completion_announcement(
                Arc::clone(&manager),
                &completion_payload(unsettled),
            )
            .await;
            assert_eq!(
                rendered, baseline,
                "{unsettled} is not a settled claim about the code and must be silent"
            );
        }
    }

    /// Regression bar for the ordinary case: a plain task with a summary and
    /// no verification state renders exactly what it rendered before the
    /// clause existed.
    #[tokio::test]
    async fn an_ordinary_completion_announcement_is_unchanged() {
        let manager = announcement_prompt_manager().await;
        let rendered = render_task_completion_announcement(
            manager,
            &json!({
                "task_id": "task-7a2",
                "title": "Send the Q3 brief",
                "status": "completed",
                "summary": "Sent to the team.",
            }),
        )
        .await;
        assert_eq!(
            rendered,
            "Background task \"Send the Q3 brief\" finished. Sent to the team."
        );
    }

    /// The spoken nudge names the project work, never the task id — the
    /// rough edge the started reply already has must not gain a second
    /// instance.
    #[tokio::test]
    async fn the_diff_approval_nudge_names_the_title_and_not_the_task_id() {
        let manager = announcement_prompt_manager().await;
        let rendered = render_diff_approval_announcement(
            Arc::clone(&manager),
            &json!({
                "task_id": "task_0123456789abcdef0123456789abcdef",
                "title": "Fix the footer spacing",
                "proposal_id": "ccp-1",
                "changed_file_count": 3,
            }),
        )
        .await;
        assert!(rendered.contains("Fix the footer spacing"));
        assert!(rendered.contains("three files"), "got {rendered:?}");
        assert!(
            !rendered.contains("task_0123456789abcdef0123456789abcdef"),
            "a 32-hex id read aloud is the rough edge this must not repeat"
        );
        assert!(!rendered.contains("ccp-1"), "got {rendered:?}");

        let one = render_diff_approval_announcement(
            manager,
            &json!({
                "title": "Fix the footer spacing",
                "changed_file_count": 1,
            }),
        )
        .await;
        assert!(one.contains("one file") && !one.contains("one files"));
    }

    /// No title means no sentence. Falling back to the id would produce
    /// exactly the thing the test above forbids.
    #[tokio::test]
    async fn the_diff_approval_nudge_stays_silent_without_a_title() {
        let manager = announcement_prompt_manager().await;
        let rendered = render_diff_approval_announcement(
            manager,
            &json!({
                "task_id": "task_0123456789abcdef0123456789abcdef",
                "proposal_id": "ccp-1",
                "changed_file_count": 2,
            }),
        )
        .await;
        assert!(rendered.is_empty(), "got {rendered:?}");
    }
}

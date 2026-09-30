//! HTTP surface for the realtime media + control rails.
//!
//! Endpoints, all under `/api/magician/v2/media`:
//!
//! ```text
//! POST   /sessions                       register a new realtime surface
//! GET    /sessions                       list active surfaces in scope
//! GET    /sessions/{id}                  read a single surface snapshot
//! PATCH  /sessions/{id}                  update capabilities/permissions
//! POST   /sessions/{id}/heartbeat        refresh liveness
//! DELETE /sessions/{id}                  disconnect (graceful) or revoke
//! POST   /sessions/{id}/events           publish a whitelisted client event
//! GET    /providers                      snapshot of registered TTS/STT providers
//! GET    /preferences                    read scoped STT/TTS defaults
//! PUT    /preferences                    update scoped STT/TTS defaults
//! POST   /tts/synthesize                 provider TTS — text in, audio bytes out
//! POST   /stt/transcribe                 provider STT — multipart audio in, transcript out
//! POST   /voice-notes                    voice-note audio -> transcript -> chat turn
//! GET    /bridge/{session_id}/ws         WebSocket bridge for tray surfaces
//! ```
//!
//! Realtime voice lives on its own bidirectional control WebSocket
//! at `/api/magician/v2/media/voice/{voice_session_id}/control`
//! whose actor + orchestrator are in `voice_control_handler.rs` and
//! `media_rails/voice_orchestrator.rs`. The orchestrator owns every
//! voice concern (mint, rotate, compact, reconnect, replay) so the
//! surface here stays minimal.
//!
//! Authority semantics: every endpoint resolves the caller's
//! `(principal, workspace)` from the verified workspace-bound bearer
//! (or matching body fields on POST/PATCH). Reads/writes are
//! scope-isolated — a session registered under one workspace is
//! invisible to callers in another.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use actix_multipart::Multipart;
use actix_web::HttpMessage;
use actix_web::{web, HttpRequest, HttpResponse, Result};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::{chat_api::ChatApi, scope::resolve_required_scope};
use magician::magician_v2::{
    chat::{
        models::{ChatChannel, ChatMessageMode, ChatResponse, ChatSessionStatus},
        service::{extract_chat_bad_request_message, ChatService},
    },
    process_storage,
    realtime_events::RuntimeAgentEventType,
};
use magician_media::media_rails::{
    preferences::MEDIA_PREFERENCES_SCHEMA_VERSION, AudioConfigError, AudioModelLifecycleState,
    AudioRuntimeConfigManager, AudioSettingsPatch, AudioSettingsResponse, AudioStage,
    AudioStageProfileConfig, AudioSurface, MediaPermissions, MediaPreferencesStore,
    MediaProviderRegistry, MediaRegistryError, ProviderAvailability, RealtimeSession,
    RealtimeSessionRegistry, SessionStatus, SttError, SttProvider, SttRequest, SttResponse,
    SttStreamEvent, SurfaceCapabilities, SurfaceType, TransportType, TtsEmotion, TtsError, TtsPace,
    TtsProvider, TtsRequest, TtsStyle, TtsVoiceMode, VoiceDownstreamFanout,
    VoiceSessionLifecycleStore, CLIENT_POSTABLE_EVENT_TYPES, MEDIA_SYSTEM_AGENT,
    MEDIA_VOICE_NOTE_CHAT_SUBMIT_COMPLETED, MEDIA_VOICE_NOTE_CHAT_SUBMIT_FAILED,
    MEDIA_VOICE_NOTE_CHAT_SUBMIT_STARTED, MEDIA_VOICE_NOTE_ERROR, MEDIA_VOICE_NOTE_RECEIVED,
    MEDIA_VOICE_NOTE_RECORDING_FAILED, MEDIA_VOICE_NOTE_RECORDING_STARTED,
    MEDIA_VOICE_NOTE_RECORDING_STOPPED, MEDIA_VOICE_NOTE_SUBMITTED, MEDIA_VOICE_NOTE_TRANSCRIBED,
    MEDIA_VOICE_NOTE_TRANSCRIPTION_COMPLETED, MEDIA_VOICE_NOTE_TRANSCRIPTION_FAILED,
    MEDIA_VOICE_NOTE_TRANSCRIPTION_STARTED,
};

#[derive(Clone)]
pub struct MediaApi {
    registry: Arc<RealtimeSessionRegistry>,
    providers: Arc<MediaProviderRegistry>,
    preferences: Arc<MediaPreferencesStore>,
    /// Process-local map of voice-session id → control-WS
    /// downstream sender. The voice orchestrator registers a
    /// sender when a call starts (on behalf of the control-WS
    /// actor); the artifact service uses this to push task-
    /// completion notifications back to the relevant voice call.
    downstream_fanout: Arc<VoiceDownstreamFanout>,
    /// Tracks per-voice-session upstream lifecycle (mint,
    /// rotation, compacted summary, last-activity). The voice
    /// orchestrator owns the read/write paths; the registry hosts
    /// the store so `Arc<MediaApi>` remains a single dependency
    /// for the control-WS actor.
    lifecycle_store: Arc<VoiceSessionLifecycleStore>,
    audio_runtime: Arc<AudioRuntimeConfigManager>,
    audio_settings_update_lock: Arc<tokio::sync::Mutex<()>>,
    fluid_audio: Option<Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>>,
    /// Public media-session id -> process-private random correlation. Keeping
    /// this indirection separate means a caller-authored session id is never
    /// itself the key that stores authority.
    voice_owner_correlations: Arc<DashMap<String, String>>,
    /// Move-on-connect server authority for authenticated owner voice, keyed
    /// only by the random process-private correlation above. Neither the
    /// correlation nor the credential appears in the media/control protocol.
    voice_owner_credentials: Arc<
        DashMap<
            String,
            Arc<magician::magician_v2::apps::boundary::AppRealtimeVoiceOwnerSessionCredential>,
        >,
    >,
}

impl MediaApi {
    pub fn new(registry: Arc<RealtimeSessionRegistry>) -> Self {
        let providers = Arc::new(MediaProviderRegistry::new());
        let audio_runtime = AudioRuntimeConfigManager::in_memory(
            magician::config::MagicianMediaSettings::default(),
            Arc::clone(&providers),
        )
        .expect("default audio compatibility profile must be valid");
        Self {
            registry,
            providers,
            preferences: Arc::new(MediaPreferencesStore::new(process_storage::runtime_root())),
            downstream_fanout: VoiceDownstreamFanout::new(),
            lifecycle_store: VoiceSessionLifecycleStore::new(),
            audio_runtime: Arc::new(audio_runtime),
            audio_settings_update_lock: Arc::new(tokio::sync::Mutex::new(())),
            fluid_audio: None,
            voice_owner_correlations: Arc::new(DashMap::new()),
            voice_owner_credentials: Arc::new(DashMap::new()),
        }
    }

    pub fn with_providers(mut self, providers: Arc<MediaProviderRegistry>) -> Self {
        self.providers = providers;
        self
    }

    pub fn with_preferences(mut self, preferences: Arc<MediaPreferencesStore>) -> Self {
        self.preferences = preferences;
        self
    }

    pub fn with_downstream_fanout(mut self, fanout: Arc<VoiceDownstreamFanout>) -> Self {
        self.downstream_fanout = fanout;
        self
    }

    pub fn with_lifecycle_store(mut self, store: Arc<VoiceSessionLifecycleStore>) -> Self {
        self.lifecycle_store = store;
        self
    }

    pub fn with_audio_runtime(mut self, runtime: Arc<AudioRuntimeConfigManager>) -> Self {
        self.audio_runtime = runtime;
        self
    }

    pub fn with_fluid_audio(
        mut self,
        manager: Option<Arc<magician_media::media_rails::fluid_audio::FluidAudioEngineManager>>,
    ) -> Self {
        self.fluid_audio = manager;
        self
    }

    pub fn providers(&self) -> Arc<MediaProviderRegistry> {
        Arc::clone(&self.providers)
    }

    pub fn preferences(&self) -> Arc<MediaPreferencesStore> {
        Arc::clone(&self.preferences)
    }

    pub fn registry(&self) -> Arc<RealtimeSessionRegistry> {
        Arc::clone(&self.registry)
    }

    pub fn downstream_fanout(&self) -> Arc<VoiceDownstreamFanout> {
        Arc::clone(&self.downstream_fanout)
    }

    pub fn lifecycle_store(&self) -> Arc<VoiceSessionLifecycleStore> {
        Arc::clone(&self.lifecycle_store)
    }

    pub fn audio_runtime(&self) -> Arc<AudioRuntimeConfigManager> {
        Arc::clone(&self.audio_runtime)
    }

    pub(crate) fn take_voice_owner_credential(
        &self,
        voice_session_id: &str,
        authenticated: &magician::magician_v2::apps::authority::AuthenticatedAppScope,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<Arc<magician::magician_v2::apps::boundary::AppRealtimeVoiceOwnerSessionCredential>>
    {
        let correlation = self.voice_owner_correlations.get(voice_session_id)?;
        let candidate = self.voice_owner_credentials.get(correlation.as_str())?;
        if !candidate.matches_authenticated_scope(authenticated, voice_session_id, now) {
            let expired = !candidate.is_live_at(now);
            drop(candidate);
            drop(correlation);
            if expired {
                self.invalidate_voice_owner_credential(voice_session_id);
            }
            return None;
        }
        drop(candidate);
        drop(correlation);
        let (_, correlation) = self.voice_owner_correlations.remove(voice_session_id)?;
        self.voice_owner_credentials
            .remove(&correlation)
            .map(|(_, credential)| credential)
    }

    fn invalidate_voice_owner_credential(&self, voice_session_id: &str) {
        if let Some((_, correlation)) = self.voice_owner_correlations.remove(voice_session_id) {
            if let Some((_, credential)) = self.voice_owner_credentials.remove(&correlation) {
                credential.invalidate();
            }
        }
    }
}

pub(crate) fn authenticated_app_scope_for_media_request(
    req: &HttpRequest,
    principal: &str,
    workspace: &str,
) -> std::result::Result<
    Option<magician::magician_v2::apps::authority::AuthenticatedAppScope>,
    HttpResponse,
> {
    use magician::magician_v2::{
        apps::{
            boundary::VerifiedAppTransportSession,
            models::{AppDigest, AppReference, AppRevision, AppScopeBindingRef},
            records::AppScope,
        },
        cloudflare_access::{VerifiedRequestAuthentication, VerifiedRequestIdentity},
    };

    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return Ok(None);
    };
    if identity.principal() != principal
        || identity
            .workspace()
            .is_some_and(|candidate| candidate != workspace)
    {
        return Err(HttpResponse::Forbidden().json(json!({
            "error": "media_session_authenticated_scope_mismatch",
        })));
    }
    let scope = AppScope {
        principal: AppReference::parse(principal.to_owned()).map_err(|_| {
            HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_scope"}))
        })?,
        workspace: AppReference::parse(workspace.to_owned()).map_err(|_| {
            HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_scope"}))
        })?,
    };
    let scope_digest =
        AppDigest::blake3(format!("{}\0{}", scope.principal, scope.workspace).as_bytes());
    let scope_binding_ref = AppScopeBindingRef::parse(format!(
        "scope_{}",
        scope_digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|_| {
        HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_scope"}))
    })?;
    let actor_ref = AppReference::parse(format!("actor:{}", identity.actor_fingerprint()))
        .map_err(|_| {
            HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_actor"}))
        })?;
    let session_ref = AppReference::parse(format!("session:{}", identity.session_fingerprint()))
        .map_err(|_| {
            HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_session"}))
        })?;
    let revision = AppRevision::new(identity.authentication_revision()).map_err(|_| {
        HttpResponse::Unauthorized().json(json!({"error": "invalid_authenticated_revision"}))
    })?;
    let now = chrono::Utc::now();
    let issued_at = identity.verified_at();
    if now < issued_at || now - issued_at > chrono::Duration::seconds(5) {
        return Err(HttpResponse::Unauthorized().json(json!({
            "error": "stale_authenticated_identity",
        })));
    }
    let expires_at = now + chrono::Duration::minutes(5);
    let transport = match identity.authentication() {
        VerifiedRequestAuthentication::TrustedLoopbackSingleUser => {
            let peer_ip = req.peer_addr().map(|address| address.ip()).ok_or_else(|| {
                HttpResponse::Unauthorized().json(json!({
                    "error": "trusted_loopback_peer_unavailable",
                }))
            })?;
            VerifiedAppTransportSession::from_trusted_loopback(
                peer_ip,
                true,
                scope.clone(),
                scope_binding_ref,
                actor_ref,
                session_ref,
                revision,
                issued_at,
                expires_at,
            )
        },
        VerifiedRequestAuthentication::CloudflareAccess
        | VerifiedRequestAuthentication::PairedDevice
        | VerifiedRequestAuthentication::MagicianBearer => {
            VerifiedAppTransportSession::from_verified_session(
                scope.clone(),
                scope_binding_ref,
                actor_ref,
                session_ref,
                revision,
                issued_at,
                expires_at,
            )
        },
    }
    .and_then(|transport| transport.bind_request(Some(&scope), &now))
    .map_err(|_| {
        HttpResponse::Unauthorized().json(json!({"error": "owner_credential_unavailable"}))
    })?;
    Ok(Some(transport))
}

// ─── Request / response shapes ───────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct RegisterSessionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub surface_type: Option<SurfaceType>,
    #[serde(default)]
    pub transport: Option<TransportType>,
    #[serde(default)]
    pub capabilities: Option<SurfaceCapabilities>,
    #[serde(default)]
    pub permissions: Option<MediaPermissions>,
    #[serde(default)]
    pub user_agent: Option<String>,
    #[serde(default)]
    pub display_label: Option<String>,
    #[serde(default)]
    pub audio_surface: Option<AudioSurface>,
    #[serde(default)]
    pub audio_profile: Option<String>,
    #[serde(default)]
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ListSessionsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct ScopeOnlyQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct PatchSessionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub capabilities: Option<SurfaceCapabilities>,
    #[serde(default)]
    pub permissions: Option<MediaPermissions>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DeleteSessionQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// When true, treat as a revocation (server-initiated kill) rather
    /// than a graceful client disconnect. Default false.
    #[serde(default)]
    pub revoke: bool,
}

#[derive(Debug, Deserialize)]
pub struct PostMediaEventRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub event_type: String,
    #[serde(default = "default_event_payload")]
    pub payload: Value,
}

fn default_event_payload() -> Value {
    Value::Object(serde_json::Map::new())
}

#[derive(Debug, Serialize)]
struct SessionEnvelope {
    session: RealtimeSession,
}

#[derive(Debug, Serialize)]
struct SessionListEnvelope {
    sessions: Vec<RealtimeSession>,
}

/// Which `surface_type` a registration is actually admitted on.
///
/// A caller may **narrow** itself freely — asking for a stricter surface can
/// only restrict it. What it may not do is **widen**: claiming an owner surface
/// (`tray_*`, `web_*`, `mascot_*`, `esp_terminal`) is a claim to be the owner,
/// and an unverified caller does not get to make it.
///
/// `verified` is the presence of a server-owned [`VerifiedRequestIdentity`] —
/// a Cloudflare Access assertion, a paired-device credential, or trusted
/// loopback. This is NOT a formality: `verify_access_middleware` calls through
/// **without** inserting an identity for a non-loopback peer when Cloudflare is
/// unconfigured (local development) or configured below `Require` mode. Those
/// are exactly the deployments where an unverified caller could otherwise
/// register as a tray session and be answered by the personal assistant.
///
/// Unverified therefore resolves to `Unknown`, which `voice_source_surface`
/// maps to the room — the same fail-closed answer an unidentified caller gets.
fn admitted_surface_type(requested: Option<SurfaceType>, verified: bool) -> SurfaceType {
    match requested.unwrap_or_default() {
        // Narrowing onto the room is always allowed: it only restricts.
        SurfaceType::MeetingBot => SurfaceType::MeetingBot,
        claimed if verified => claimed,
        _ => SurfaceType::Unknown,
    }
}

fn is_identified_owner_voice_surface(surface: SurfaceType) -> bool {
    matches!(
        surface,
        SurfaceType::TrayMacos
            | SurfaceType::TrayWindows
            | SurfaceType::TrayLinux
            | SurfaceType::MascotMacos
            | SurfaceType::MascotWindows
            | SurfaceType::MascotLinux
            | SurfaceType::WebMobile
            | SurfaceType::WebDesktop
            | SurfaceType::EspTerminal
    )
}

// ─── Handlers ────────────────────────────────────────────────────────

pub async fn register_media_session_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    body: web::Json<RegisterSessionRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let authenticated_scope =
        match authenticated_app_scope_for_media_request(&req, &principal, &workspace) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    let admitted_surface = admitted_surface_type(body.surface_type, authenticated_scope.is_some());
    let now = chrono::Utc::now().timestamp_millis();
    let resolved_audio_profile = if let Some(surface) = body.audio_surface {
        let preferences = match api
            .preferences
            .load(&principal, &workspace, &api.audio_runtime)
            .await
        {
            Ok(preferences) => preferences,
            Err(error) => return Ok(media_preferences_error_response(error)),
        };
        match api.audio_runtime.resolve(
            surface,
            &preferences,
            body.audio_profile.as_deref(),
            &body.audio_stage_options,
        ) {
            Ok(profile) => Some(profile),
            Err(error) => return Ok(audio_config_error_response(error)),
        }
    } else {
        None
    };
    let session = RealtimeSession {
        session_id: body
            .session_id
            .unwrap_or_else(|| Uuid::new_v4().to_string()),
        principal,
        workspace,
        thread_id: body.thread_id,
        surface_type: admitted_surface,
        transport: body.transport.unwrap_or_default(),
        status: SessionStatus::Connected,
        capabilities: body.capabilities.unwrap_or_default(),
        permissions: body.permissions.unwrap_or_default(),
        created_at_ms: now,
        last_seen_at_ms: now,
        user_agent: body.user_agent,
        display_label: body.display_label,
        audio_surface: body.audio_surface,
        resolved_audio_profile,
    };
    match api.registry.register(session) {
        Ok(registered) => {
            if is_identified_owner_voice_surface(registered.surface_type) {
                if let Some(authenticated_scope) = authenticated_scope {
                    if let Ok(credential) = magician::magician_v2::apps::boundary::AppRealtimeVoiceOwnerSessionCredential::from_authenticated_session(
                        authenticated_scope,
                        registered.session_id.clone(),
                        magician_media::media_rails::voice_orchestrator::VOICE_CHAT_AGENT_ID,
                        chrono::Utc::now(),
                    ) {
                        let correlation = Uuid::new_v4().simple().to_string();
                        api.invalidate_voice_owner_credential(&registered.session_id);
                        api.voice_owner_credentials
                            .insert(correlation.clone(), Arc::new(credential));
                        api.voice_owner_correlations
                            .insert(registered.session_id.clone(), correlation);
                    }
                }
            }
            if let Some(profile) = registered.resolved_audio_profile.as_ref() {
                api.registry.broadcaster().emit_named(
                    "media.audio.profile.resolved",
                    MEDIA_SYSTEM_AGENT,
                    Some(&registered.principal),
                    Some(&registered.workspace),
                    json!({
                        "session_id": &registered.session_id,
                        "surface": profile.surface,
                        "profile_id": &profile.profile_id,
                        "revision": &profile.revision,
                        "degradations": &profile.degradations,
                    }),
                );
            }
            Ok(HttpResponse::Ok().json(SessionEnvelope {
                session: registered,
            }))
        },
        Err(MediaRegistryError::TooManySessions(limit)) => Ok(HttpResponse::TooManyRequests()
            .json(json!({
                "error": "too_many_media_sessions",
                "limit": limit,
                "message": format!(
                    "this scope already has {limit} active media sessions — disconnect or wait for stale prune"
                ),
            }))),
        Err(other) => Ok(internal_error(other)),
    }
}

pub async fn list_media_sessions_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<ListSessionsQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let sessions = api.registry.list_for_scope(&principal, &workspace);
    Ok(HttpResponse::Ok().json(SessionListEnvelope { sessions }))
}

pub async fn get_media_session_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ScopeOnlyQuery>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(session) = api.registry.get(&session_id) else {
        return Ok(not_found(&session_id));
    };
    if session.principal != principal || session.workspace != workspace {
        return Ok(forbidden_scope());
    }
    Ok(HttpResponse::Ok().json(SessionEnvelope { session }))
}

pub async fn patch_media_session_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PatchSessionRequest>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(existing) = api.registry.get(&session_id) else {
        return Ok(not_found(&session_id));
    };
    if existing.principal != principal || existing.workspace != workspace {
        return Ok(forbidden_scope());
    }
    let mut latest = existing;
    if let Some(caps) = body.capabilities {
        latest = match api.registry.update_capabilities(&session_id, caps) {
            Ok(s) => s,
            Err(MediaRegistryError::NotFound(id)) => return Ok(not_found(&id)),
            Err(other) => return Ok(internal_error(other)),
        };
    }
    if let Some(perms) = body.permissions {
        latest = match api.registry.update_permissions(&session_id, perms) {
            Ok(s) => s,
            Err(MediaRegistryError::NotFound(id)) => return Ok(not_found(&id)),
            Err(other) => return Ok(internal_error(other)),
        };
    }
    Ok(HttpResponse::Ok().json(SessionEnvelope { session: latest }))
}

pub async fn heartbeat_media_session_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ScopeOnlyQuery>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(existing) = api.registry.get(&session_id) else {
        return Ok(not_found(&session_id));
    };
    if existing.principal != principal || existing.workspace != workspace {
        return Ok(forbidden_scope());
    }
    let session = match api.registry.heartbeat(&session_id) {
        Ok(s) => s,
        Err(MediaRegistryError::NotFound(id)) => return Ok(not_found(&id)),
        Err(other) => return Ok(internal_error(other)),
    };
    Ok(HttpResponse::Ok().json(SessionEnvelope { session }))
}

pub async fn delete_media_session_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<DeleteSessionQuery>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(existing) = api.registry.get(&session_id) else {
        return Ok(not_found(&session_id));
    };
    if existing.principal != principal || existing.workspace != workspace {
        return Ok(forbidden_scope());
    }
    let session = if query.revoke {
        api.registry.revoke(&session_id, query.reason.as_deref())
    } else {
        api.registry.disconnect(&session_id)
    };
    let session = match session {
        Ok(s) => s,
        Err(MediaRegistryError::NotFound(id)) => return Ok(not_found(&id)),
        Err(other) => return Ok(internal_error(other)),
    };
    api.invalidate_voice_owner_credential(&session_id);
    Ok(HttpResponse::Ok().json(SessionEnvelope { session }))
}

pub async fn post_media_event_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<PostMediaEventRequest>,
) -> Result<HttpResponse> {
    let session_id = path.into_inner();
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api.registry.publish_client_event(
        &session_id,
        &principal,
        &workspace,
        &body.event_type,
        body.payload,
    ) {
        Ok(session) => Ok(HttpResponse::Ok().json(json!({
            "session_id": session.session_id,
            "event_type": body.event_type,
            "accepted": true,
        }))),
        Err(MediaRegistryError::NotFound(id)) => Ok(not_found(&id)),
        Err(MediaRegistryError::EventTypeNotAllowed(t)) => {
            Ok(HttpResponse::BadRequest().json(json!({
                "error": "event_type_not_allowed",
                "event_type": t,
                "allowed": CLIENT_POSTABLE_EVENT_TYPES,
            })))
        },
        Err(MediaRegistryError::ScopeMismatch) => Ok(forbidden_scope()),
        Err(other @ MediaRegistryError::TooManySessions(_)) => Ok(internal_error(other)),
    }
}

// ─── Helpers ─────────────────────────────────────────────────────────

fn not_found(session_id: &str) -> HttpResponse {
    HttpResponse::NotFound().json(json!({
        "error": "media_session_not_found",
        "session_id": session_id,
    }))
}

fn forbidden_scope() -> HttpResponse {
    HttpResponse::Forbidden().json(json!({
        "error": "media_session_scope_mismatch",
        "message": "the caller's (principal, workspace) does not match the session's scope",
    }))
}

fn internal_error(err: MediaRegistryError) -> HttpResponse {
    HttpResponse::InternalServerError().json(json!({
        "error": "media_registry_error",
        "message": format!("{err}"),
    }))
}

// ─── Phase 3 — provider TTS / STT ────────────────────────────────────

pub async fn list_media_providers_handler(api: web::Data<Arc<MediaApi>>) -> Result<HttpResponse> {
    let legacy = api.providers.snapshot();
    let audio = audio_settings_response(api.get_ref().as_ref()).await;
    let mut response = serde_json::to_value(legacy).unwrap_or_else(|_| json!({}));
    if let Some(object) = response.as_object_mut() {
        object.insert("audio_revision".to_string(), json!(audio.revision));
        object.insert("stages".to_string(), json!(audio.stages));
        object.insert("surface_profiles".to_string(), json!(audio.profiles));
        object.insert(
            "default_surface_profiles".to_string(),
            json!(audio.default_profiles),
        );
        object.insert("engines".to_string(), json!(audio.engines));
        let hands_free_voice = audio
            .default_profiles
            .get(&AudioSurface::HandsFree)
            .and_then(|profile_id| audio.profiles.get(profile_id))
            .is_some_and(|profile| {
                hands_free_stage_available(&audio, AudioStage::Vad, &profile.vad)
                    && hands_free_stage_available(
                        &audio,
                        AudioStage::StreamingStt,
                        &profile.streaming_stt,
                    )
                    && hands_free_stage_available(&audio, AudioStage::Tts, &profile.tts)
            });
        object.insert("hands_free_voice".to_string(), json!(hands_free_voice));
    }
    Ok(HttpResponse::Ok().json(response))
}

fn hands_free_stage_available(
    audio: &AudioSettingsResponse,
    stage: AudioStage,
    config: &AudioStageProfileConfig,
) -> bool {
    config.enabled
        && !config.providers.is_empty()
        && config.providers.iter().any(|configured| {
            audio.stages.get(&stage).is_some_and(|options| {
                options.iter().any(|option| {
                    option.availability == ProviderAvailability::Available
                        && (option.option_id.eq_ignore_ascii_case(configured)
                            || option.provider_id.eq_ignore_ascii_case(configured))
                })
            })
        })
}

pub async fn get_audio_settings_handler(api: web::Data<Arc<MediaApi>>) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(audio_settings_response(api.get_ref().as_ref()).await))
}

async fn audio_settings_response(api: &MediaApi) -> AudioSettingsResponse {
    let mut response = api.audio_runtime.settings_response();
    let Some(manager) = api.fluid_audio.as_ref() else {
        return response;
    };

    let engine_status = manager.status().await;
    let configured_enabled = response
        .engines
        .get("fluid_audio")
        .is_some_and(|engine| engine.enabled);
    if let Some(engine) = response.engines.get_mut("fluid_audio") {
        engine.available = configured_enabled && engine_status.available;
        engine.healthy = Some(engine_status.healthy);
        engine.owned = Some(engine_status.owned);
        engine.endpoint = Some(engine_status.endpoint);
        engine.startup_policy = Some(engine_status.startup_policy);
        engine.download_policy = Some(engine_status.download_policy);
        engine.offline = Some(engine_status.offline);
        engine.process_idle_secs = Some(engine_status.process_idle_secs);
        engine.model_idle_secs = Some(engine_status.model_idle_secs);
        engine.max_resident_models = Some(engine_status.max_resident_models);
        engine.max_streaming_sessions = Some(engine_status.max_streaming_sessions);
        engine.process_id = engine_status.process_id;
        engine.start_count = Some(engine_status.start_count);
        engine.restart_count = Some(engine_status.restart_count);
        engine.last_started_at_ms = engine_status.last_started_at_ms;
        engine.last_error = engine_status.last_error;
        engine.can_manage_models = configured_enabled && engine_status.available;
    }

    if !configured_enabled || !engine_status.available {
        let disabled = !configured_enabled;
        for model in response
            .models
            .values_mut()
            .filter(|model| model.engine_id == "fluid_audio")
        {
            model.state = AudioModelLifecycleState::Unavailable;
            model.resident = false;
            model.can_load = false;
            model.can_unload = false;
            model.message = Some(if disabled {
                "FluidAudio is disabled".to_string()
            } else {
                "FluidAudio is unavailable on this host".to_string()
            });
        }
        return response;
    }

    match manager.model_states_if_running().await {
        Ok(Some(states)) => {
            let mut resident_models = 0usize;
            let mut active_sessions = 0usize;
            for state in states {
                resident_models = resident_models.saturating_add(usize::from(state.resident));
                active_sessions = active_sessions.saturating_add(state.active_sessions);
                for model in response
                    .models
                    .values_mut()
                    .filter(|model| model.provider_id == state.id)
                {
                    model.state = match state.state.as_str() {
                        "loaded" => AudioModelLifecycleState::Ready,
                        "loading" => AudioModelLifecycleState::Loading,
                        _ => AudioModelLifecycleState::DownloadRequired,
                    };
                    model.resident = state.resident;
                    model.active_sessions = state.active_sessions;
                    model.can_load = !state.resident;
                    model.can_unload = state.resident && state.active_sessions == 0;
                    model.message = match model.state {
                        AudioModelLifecycleState::DownloadRequired => {
                            Some("Downloads and loads on first use".to_string())
                        },
                        AudioModelLifecycleState::Loading => Some(format!(
                            "Loading locally{}",
                            if state.active_sessions > 0 {
                                format!(" for {} active session(s)", state.active_sessions)
                            } else {
                                String::new()
                            }
                        )),
                        AudioModelLifecycleState::Ready if state.resident => {
                            Some("Loaded locally".to_string())
                        },
                        _ => None,
                    };
                }
            }
            if let Some(engine) = response.engines.get_mut("fluid_audio") {
                engine.resident_models = Some(resident_models);
                engine.active_sessions = Some(active_sessions);
            }
        },
        Ok(None) => {
            // The FluidAudio sidecar is lazy-started. An installed engine that is
            // intentionally idle has no health signal and must not be presented as
            // degraded in Settings.
            if let Some(engine) = response.engines.get_mut("fluid_audio") {
                engine.healthy = None;
            }
        },
        Err(error) => {
            for model in response
                .models
                .values_mut()
                .filter(|model| model.engine_id == "fluid_audio")
            {
                model.state = AudioModelLifecycleState::Degraded;
                model.resident = false;
                model.message = Some(error.clone());
            }
        },
    }
    response
}

#[derive(Debug, Deserialize, Default)]
pub struct AudioEngineModelControlRequest {
    #[serde(default)]
    pub model_ids: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct AudioEngineModelControlResponse {
    pub engine_id: String,
    pub action: String,
    pub model_ids: Vec<String>,
    pub settings: AudioSettingsResponse,
}

pub async fn post_audio_engine_model_control_handler(
    api: web::Data<Arc<MediaApi>>,
    path: web::Path<(String, String)>,
    body: web::Json<AudioEngineModelControlRequest>,
) -> Result<HttpResponse> {
    let (engine_id, action) = path.into_inner();
    if !engine_id.eq_ignore_ascii_case("fluid_audio") {
        return Ok(HttpResponse::NotFound().json(json!({
            "error": "unknown_audio_engine",
            "engine_id": engine_id,
        })));
    }
    let Some(manager) = api.fluid_audio.as_ref() else {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "audio_engine_unavailable",
            "engine_id": engine_id,
        })));
    };
    let mut model_ids = body
        .model_ids
        .iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    model_ids.sort();
    model_ids.dedup();

    match action.as_str() {
        "prewarm" => {
            if model_ids.is_empty() {
                model_ids = manager.configured_prewarm_ids();
            }
            if model_ids.is_empty() {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "audio_model_selection_required",
                    "message": "prewarm requires model_ids when the engine prewarm list is empty",
                })));
            }
            if let Some(model_id) = model_ids
                .iter()
                .find(|model_id| manager.model(model_id).is_none())
            {
                return Ok(audio_engine_control_error_response(format!(
                    "unknown configured FluidAudio model: {model_id}"
                )));
            }
            for model_id in &model_ids {
                if let Err(error) = manager.load_model(model_id).await {
                    return Ok(audio_engine_control_error_response(error));
                }
            }
        },
        "unload" => {
            if model_ids.is_empty() {
                model_ids = manager.model_ids();
            }
            if let Some(model_id) = model_ids
                .iter()
                .find(|model_id| manager.model(model_id).is_none())
            {
                return Ok(audio_engine_control_error_response(format!(
                    "unknown configured FluidAudio model: {model_id}"
                )));
            }
            match manager.model_states_if_running().await {
                Ok(Some(states)) => {
                    if let Some(state) = states.iter().find(|state| {
                        state.active_sessions > 0
                            && model_ids
                                .iter()
                                .any(|model_id| model_id.eq_ignore_ascii_case(&state.id))
                    }) {
                        return Ok(audio_engine_control_error_response(format!(
                            "FluidAudio model busy: {} has {} active session(s)",
                            state.id, state.active_sessions
                        )));
                    }
                },
                Ok(None) => {},
                Err(error) => return Ok(audio_engine_control_error_response(error)),
            }
            for model_id in &model_ids {
                if let Err(error) = manager.unload_model(model_id).await {
                    return Ok(audio_engine_control_error_response(error));
                }
            }
        },
        _ => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "unsupported_audio_engine_action",
                "action": action,
                "supported_actions": ["prewarm", "unload"],
            })));
        },
    }

    Ok(HttpResponse::Ok().json(AudioEngineModelControlResponse {
        engine_id,
        action,
        model_ids,
        settings: audio_settings_response(api.get_ref().as_ref()).await,
    }))
}

fn audio_engine_control_error_response(error: String) -> HttpResponse {
    if error.contains("unknown configured") {
        return HttpResponse::BadRequest().json(json!({
            "error": "unknown_audio_model",
            "message": error,
        }));
    }
    if error.contains("active session")
        || error.contains("model busy")
        || error.contains("409 Conflict")
    {
        return HttpResponse::Conflict().json(json!({
            "error": "audio_model_busy",
            "message": error,
        }));
    }
    HttpResponse::ServiceUnavailable().json(json!({
        "error": "audio_engine_operation_failed",
        "message": error,
    }))
}

pub async fn put_audio_settings_handler(
    api: web::Data<Arc<MediaApi>>,
    body: web::Json<AudioSettingsPatch>,
) -> Result<HttpResponse> {
    let _update_guard = api.audio_settings_update_lock.lock().await;
    let patch = body.into_inner();
    let fluid_audio_enabled = patch
        .engines
        .get("fluid_audio")
        .and_then(|engine| engine.enabled);
    if fluid_audio_enabled == Some(true) {
        let Some(manager) = api.fluid_audio.as_ref() else {
            return Ok(HttpResponse::ServiceUnavailable().json(json!({
                "error": "audio_engine_unavailable",
                "engine_id": "fluid_audio",
                "message": "FluidAudio is not configured on this host",
            })));
        };
        if !manager.is_host_supportable().await {
            return Ok(HttpResponse::ServiceUnavailable().json(json!({
                "error": "audio_engine_unavailable",
                "engine_id": "fluid_audio",
                "message": "FluidAudio cannot be enabled on this host",
            })));
        }
        if let Err(error) =
            api.audio_runtime
                .set_engine_runtime_availability("fluid_audio", true, None)
        {
            return Ok(audio_config_error_response(error));
        }
    }
    match api.audio_runtime.update(patch) {
        Ok(snapshot) => {
            if let (Some(enabled), Some(manager)) = (fluid_audio_enabled, api.fluid_audio.as_ref())
            {
                if let Err(error) = manager.set_enabled(enabled).await {
                    let settings = audio_settings_response(api.get_ref().as_ref()).await;
                    return Ok(HttpResponse::ServiceUnavailable().json(json!({
                        "error": "audio_engine_transition_failed",
                        "engine_id": "fluid_audio",
                        "message": error,
                        "settings": settings,
                    })));
                }
            }
            api.registry.broadcaster().emit_named(
                "media.config.updated",
                MEDIA_SYSTEM_AGENT,
                None,
                None,
                json!({
                    "revision": &snapshot.revision,
                    "requires_session_restart": true,
                }),
            );
            let response = audio_settings_response(api.get_ref().as_ref()).await;
            if fluid_audio_enabled == Some(true) {
                if let Some(manager) = api
                    .fluid_audio
                    .as_ref()
                    .filter(|manager| manager.startup_prewarm_enabled())
                {
                    let manager = Arc::clone(manager);
                    tokio::spawn(async move {
                        if let Err(error) = manager.prewarm_configured_models().await {
                            tracing::warn!(%error, "FluidAudio enable prewarm degraded");
                        }
                    });
                }
            }
            Ok(HttpResponse::Ok().json(response))
        },
        Err(error) => Ok(audio_config_error_response(error)),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct ResolveAudioSurfaceQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

pub async fn get_resolved_audio_surface_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ResolveAudioSurfaceQuery>,
) -> Result<HttpResponse> {
    let surface = match path.into_inner().parse::<AudioSurface>() {
        Ok(surface) => surface,
        Err(message) => {
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "invalid_audio_surface",
                "message": message,
            })));
        },
    };
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let preferences = match api
        .preferences
        .load(&principal, &workspace, &api.audio_runtime)
        .await
    {
        Ok(preferences) => preferences,
        Err(error) => return Ok(media_preferences_error_response(error)),
    };
    let stage_options = match parse_audio_stage_options_from_query(req.query_string()) {
        Ok(options) => options,
        Err(response) => return Ok(response),
    };
    match api.audio_runtime.resolve(
        surface,
        &preferences,
        query.profile.as_deref(),
        &stage_options,
    ) {
        Ok(resolved) => Ok(HttpResponse::Ok().json(resolved)),
        Err(error) => Ok(audio_config_error_response(error)),
    }
}

pub async fn get_media_preferences_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<ScopeOnlyQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    match api
        .preferences
        .load(&principal, &workspace, &api.audio_runtime)
        .await
    {
        Ok(preferences) => Ok(HttpResponse::Ok().json(preferences)),
        Err(error) => Ok(media_preferences_error_response(error)),
    }
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct PutMediaPreferencesRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub schema_version: Option<u32>,
    #[serde(default)]
    pub auto_speak: Option<bool>,
    #[serde(default)]
    pub voice_mode: Option<String>,
    #[serde(default)]
    pub require_voice_prefix: Option<bool>,
    #[serde(default)]
    pub surface_profiles: Option<BTreeMap<AudioSurface, String>>,
    #[serde(default)]
    pub surface_stage_options: Option<BTreeMap<AudioSurface, BTreeMap<AudioStage, String>>>,
    #[serde(default)]
    pub realtime_voices: Option<BTreeMap<String, String>>,
}

pub async fn put_media_preferences_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    body: web::Json<PutMediaPreferencesRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    // Held across load -> mutate -> save. Only the fields this request names
    // are applied, so without it a concurrent PUT changing a different field
    // loads the same base and writes the whole record back over this one.
    let preferences_lock =
        magician_media::media_rails::preferences::media_preferences_lock(&principal, &workspace);
    let _preferences_guard = preferences_lock.lock().await;
    let mut preferences = match api
        .preferences
        .load(&principal, &workspace, &api.audio_runtime)
        .await
    {
        Ok(preferences) => preferences,
        Err(error) => return Ok(media_preferences_error_response(error)),
    };
    if body
        .schema_version
        .is_some_and(|version| version != MEDIA_PREFERENCES_SCHEMA_VERSION)
    {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "unsupported_media_preferences_schema",
            "supported_schema_version": MEDIA_PREFERENCES_SCHEMA_VERSION,
        })));
    }
    preferences = api.audio_runtime.sanitize_preferences(preferences);
    if let Some(value) = body.auto_speak {
        preferences.auto_speak = value;
    }
    if let Some(value) = body.voice_mode {
        preferences.voice_mode = value;
    }
    if let Some(value) = body.require_voice_prefix {
        preferences.require_voice_prefix = value;
    }
    if let Some(overrides) = body.surface_profiles {
        for (surface, profile_id) in overrides {
            let profile_id = profile_id.trim();
            if profile_id.is_empty()
                || profile_id.eq_ignore_ascii_case("auto")
                || profile_id.eq_ignore_ascii_case("default")
            {
                preferences.surface_profiles.remove(&surface);
            } else {
                preferences
                    .surface_profiles
                    .insert(surface, profile_id.to_string());
            }
        }
    }
    if let Some(surface_overrides) = body.surface_stage_options {
        for (surface, stage_overrides) in surface_overrides {
            for (stage, option_id) in stage_overrides {
                let option_id = option_id.trim();
                if option_id.is_empty()
                    || option_id.eq_ignore_ascii_case("auto")
                    || option_id.eq_ignore_ascii_case("default")
                {
                    if let Some(options) = preferences.surface_stage_options.get_mut(&surface) {
                        options.remove(&stage);
                    }
                } else {
                    preferences
                        .surface_stage_options
                        .entry(surface)
                        .or_default()
                        .insert(stage, option_id.to_string());
                }
            }
            if preferences
                .surface_stage_options
                .get(&surface)
                .is_some_and(BTreeMap::is_empty)
            {
                preferences.surface_stage_options.remove(&surface);
            }
        }
    }
    if let Some(overrides) = body.realtime_voices {
        for (profile_id, voice) in overrides {
            let profile_id = profile_id.trim();
            let voice = voice.trim();
            if profile_id.is_empty()
                || voice.is_empty()
                || voice.eq_ignore_ascii_case("auto")
                || voice.eq_ignore_ascii_case("default")
            {
                preferences.realtime_voices.remove(profile_id);
                continue;
            }
            let Some(profile) = api
                .providers
                .snapshot()
                .realtime_voice_profiles
                .iter()
                .find(|profile| profile.profile_id == profile_id)
                .cloned()
            else {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "unknown_realtime_voice_profile",
                    "profile_id": profile_id,
                })));
            };
            let Some(canonical) =
                magicllm::realtime::canonical_realtime_voice(&profile.provider, voice)
            else {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "unsupported_realtime_voice",
                    "profile_id": profile_id,
                    "voice": voice,
                })));
            };
            preferences
                .realtime_voices
                .insert(profile_id.to_string(), canonical);
        }
    }
    preferences = preferences.normalize();
    if let Err(error) = api.audio_runtime.validate_preferences(&preferences) {
        return Ok(audio_config_error_response(error));
    }
    match api
        .preferences
        .save(&principal, &workspace, preferences)
        .await
    {
        Ok(preferences) => {
            api.registry.broadcaster().emit_named(
                RuntimeAgentEventType::MediaPreferencesUpdated.as_str(),
                MEDIA_SYSTEM_AGENT,
                Some(&principal),
                Some(&workspace),
                json!({
                    "principal": principal,
                    "workspace": workspace,
                    "preferences": preferences.clone(),
                }),
            );
            Ok(HttpResponse::Ok().json(preferences))
        },
        Err(error) => Ok(media_preferences_error_response(error)),
    }
}

// Shared media rails-error contract; `pub(crate)` so the media UX seam
// (`crate::media_ux::dictation`) reuses the same wire shapes.
pub(crate) fn media_preferences_error_response(error: std::io::Error) -> HttpResponse {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_media_preferences_scope",
            "message": error.to_string(),
        }));
    }
    if error.kind() == std::io::ErrorKind::InvalidData {
        return HttpResponse::InternalServerError().json(json!({
            "error": "invalid_media_preferences_document",
            "message": error.to_string(),
        }));
    }
    HttpResponse::InternalServerError().json(json!({
        "error": "media_preferences_io_error",
        "message": error.to_string(),
    }))
}

pub(crate) fn audio_config_error_response(error: AudioConfigError) -> HttpResponse {
    match error {
        AudioConfigError::RevisionConflict { expected, current } => {
            HttpResponse::Conflict().json(json!({
                "error": "audio_settings_revision_conflict",
                "expected_revision": expected,
                "current_revision": current,
            }))
        },
        AudioConfigError::ExternalConfigDrift => HttpResponse::Conflict().json(json!({
            "error": "audio_settings_external_config_drift",
            "message": "the live media configuration changed; refresh settings before saving",
        })),
        AudioConfigError::Invalid(message) => HttpResponse::BadRequest().json(json!({
            "error": "invalid_audio_configuration",
            "message": message,
        })),
        AudioConfigError::Persistence(message) => HttpResponse::InternalServerError().json(json!({
            "error": "audio_settings_persistence_failed",
            "message": message,
        })),
        AudioConfigError::LockPoisoned => HttpResponse::ServiceUnavailable().json(json!({
            "error": "audio_settings_unavailable",
        })),
    }
}

#[derive(Debug, Deserialize)]
pub struct SynthesizeTtsRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    pub text: String,
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub rate: Option<f32>,
    #[serde(default)]
    pub format: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub emotion: Option<TtsEmotion>,
    #[serde(default)]
    pub style: Option<TtsStyle>,
    #[serde(default)]
    pub pace: Option<TtsPace>,
    #[serde(default)]
    pub voice_mode: Option<TtsVoiceMode>,
    #[serde(default)]
    pub emphasis: Option<String>,
    #[serde(default)]
    pub audio_profile: Option<String>,
    #[serde(default)]
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

pub async fn synthesize_tts_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    body: web::Json<SynthesizeTtsRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let effective_provider = match resolve_dictation_tts_provider(
        api.get_ref(),
        &principal,
        &workspace,
        body.provider.as_deref(),
        body.audio_profile.as_deref(),
        &body.audio_stage_options,
    )
    .await
    {
        Ok(provider) => provider,
        Err(response) => return Ok(response),
    };
    let chain = api.providers.tts_chain_for(effective_provider.as_deref());
    if chain.is_empty() {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "tts_provider_not_configured",
            "message": "No backend TTS provider is configured. Browser-native TTS remains available client-side.",
        })));
    }
    if body.text.trim().is_empty() {
        // Don't bill an upstream synth call for silence. The frontend
        // already drops empty blocks, but the endpoint is a public
        // contract — a manual caller asking for empty audio gets a
        // typed 400 rather than a confusing 200/empty-bytes response.
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "tts_empty_text",
            "message": "text must be non-empty after trimming",
        })));
    }
    const MAX_TEXT_LEN: usize = 32_000;
    if body.text.len() > MAX_TEXT_LEN {
        // DoS defence — cap matches the per-segment cap on the
        // streamed-segment endpoint so callers see consistent limits.
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "tts_text_too_long",
            "message": format!(
                "text has {} bytes; max is {}",
                body.text.len(),
                MAX_TEXT_LEN
            ),
        })));
    }
    let tts_request = TtsRequest {
        text: body.text,
        voice: body.voice,
        rate: body.rate,
        model: body.model,
        format: body.format,
        message_id: body.message_id,
        emotion: body.emotion,
        style: body.style,
        pace: body.pace,
        voice_mode: body.voice_mode,
        emphasis: body.emphasis,
    };

    // Walk the chain in registration order. The first
    // `Upstream`/`Transport` failure rotates to the next entry; a
    // `BadRequest` or `NotConfigured` is propagated immediately —
    // those apply to the request itself, not the provider's health.
    //
    // Voice / model ids are provider-specific (OpenAI's `alloy` ≠
    // MiniMax's `female-yujie-jingpin`). When rotating to a fallback,
    // strip `voice` and `model` so the next provider uses its own
    // native defaults rather than 400-ing on an alien identifier.
    // Expression hints (emotion / style / pace / voice_mode /
    // emphasis) stay — they're provider-agnostic and each adapter
    // translates them.
    let mut last_error: Option<TtsError> = None;
    let mut attempts_tried: Vec<String> = Vec::with_capacity(chain.len());
    for (idx, provider) in chain.iter().enumerate() {
        attempts_tried.push(provider.id().to_string());
        let mut request_for_attempt = tts_request.clone();
        if idx > 0 {
            request_for_attempt.voice = None;
            request_for_attempt.model = None;
        }
        match provider.synthesize(request_for_attempt).await {
            Ok(response) => {
                let is_fallback = idx > 0;
                let mut builder = HttpResponse::Ok();
                builder
                    .insert_header((
                        reqwest::header::CONTENT_TYPE.as_str(),
                        response.content_type,
                    ))
                    .insert_header(("X-Tts-Provider", provider.id().to_string()))
                    .insert_header(("X-Tts-Model", response.model.clone()))
                    .insert_header(("X-Tts-Chain-Attempts", attempts_tried.join(",")));
                if is_fallback {
                    builder.insert_header(("X-Tts-Fallback", "true"));
                    // Stable structured log so aggregators can count
                    // fallback-rotation events without scraping the
                    // warn-level rotation lines (which fire per
                    // attempt; this fires once per successful synth).
                    tracing::info!(
                        target: "magician::tts::fallback",
                        succeeded_with = provider.id(),
                        attempts = %attempts_tried.join(","),
                        "[TTS] synth succeeded on fallback provider"
                    );
                }
                return Ok(builder.body(response.audio));
            },
            Err(err @ TtsError::Upstream { .. }) | Err(err @ TtsError::Transport(_)) => {
                tracing::warn!(
                    provider = provider.id(),
                    attempt = idx,
                    error = %err,
                    "[TTS] provider failed; rotating to next in chain"
                );
                last_error = Some(err);
                continue;
            },
            Err(err) => return Ok(tts_error_response(&err)),
        }
    }
    let err = last_error.unwrap_or(TtsError::NotConfigured("no tts provider succeeded".into()));
    Ok(tts_error_response(&err))
}

/// Request body for the streamed-segment synth endpoint.
///
/// The frontend sends a *list* of pre-parsed `SpeechSegment`s — one
/// HTTP request instead of N round-trips through `/synthesize`. The
/// backend walks them, drives the TTS provider chain per segment,
/// and streams audio back as NDJSON envelopes so the client can
/// start playing block 1 while block N is still being synthesized.
///
/// `voice` / `model` / `format` apply to every segment; per-segment
/// delivery hints (`emotion`, `style`, etc.) live on each segment.
/// This is intentional — `voice` is a user/preferences-level
/// choice; the LLM picks emotion per segment.
#[derive(Debug, Deserialize)]
pub struct SynthesizeMessageRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    /// Optional provider id. `auto` / empty keeps boot order; a concrete id
    /// moves that provider to the front and keeps the rest as fallbacks.
    #[serde(default)]
    pub provider: Option<String>,
    /// Originating chat message id — propagated into each segment
    /// envelope so the client can correlate. Optional.
    #[serde(default)]
    pub message_id: Option<String>,
    /// Pre-parsed segments. Empty = 204 No Content.
    pub segments: Vec<magician_media::media_rails::SpeechSegment>,
    /// User-pinned voice id. Forwarded to the primary provider; the
    /// fallback path strips it because voice ids don't translate
    /// across providers.
    #[serde(default)]
    pub voice: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub rate: Option<f32>,
    #[serde(default)]
    pub format: Option<String>,
}

/// NDJSON envelope for one segment's audio. Frontend decodes the
/// base64, plays the blob, awaits the next line. The final envelope
/// is `{"type":"done", ...}`.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SegmentEnvelope {
    Segment {
        index: usize,
        message_id: Option<String>,
        audio_b64: String,
        content_type: String,
        provider: String,
        model: String,
        /// `true` when this segment was produced by a non-primary
        /// provider (the chain rotated).
        fallback: bool,
        /// Comma-joined attempt list, mirrors the per-segment
        /// `X-Tts-Chain-Attempts` header on the single-shot endpoint.
        attempts: String,
    },
    Error {
        index: usize,
        message_id: Option<String>,
        /// Stable error code (`tts_upstream`, `tts_bad_request`,
        /// `tts_provider_not_configured`, `tts_transport`).
        code: &'static str,
        message: String,
        attempts: String,
    },
    Done {
        message_id: Option<String>,
        segment_count: usize,
    },
}

/// `POST /media/tts/synthesize_message` — synthesize a whole message
/// in one request, stream segments back as the TTS provider chain
/// produces them. Lets the client start playing block 1 while block N
/// is still being synthesized; avoids N round-trips for a multi-block
/// reply.
///
/// `POST /tts/cache/clear` — drop every cached response across the
/// TTS provider chain. Used by operators after a model redeploy or
/// voice-id rotation, where stale cached audio would mask the new
/// upstream behaviour. Returns a JSON snapshot of how many entries
/// each provider held before the flush — useful for debugging
/// whether the cache was warming up at all.
///
/// Idempotent: repeated calls are cheap (subsequent flushes count 0
/// entries). Adapters that don't cache (every provider except those
/// wrapped in `CachedTtsProvider`) no-op via the trait default.
#[derive(Debug, Deserialize, Default)]
pub struct ClearTtsCacheQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Serialize)]
struct ClearTtsCacheReport {
    provider: String,
    entries_dropped: usize,
}

/// `GET /tts/cache/stats` — hit / miss / eviction counters per
/// registered TTS provider, plus the current entry count. Useful for
/// operators to verify the cache is actually saving upstream calls
/// (high hit-rate = good) and to spot eviction storms (high eviction
/// vs. hit-rate = capacity probably too small).
///
/// Process-local counters; values reset on restart.
#[derive(Debug, Serialize)]
struct TtsCacheStatsEntry {
    provider: String,
    entries: usize,
    hits: u64,
    misses: u64,
    evictions: u64,
}

pub async fn tts_cache_stats_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<ClearTtsCacheQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (_principal, _workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let chain = api.providers.tts_chain();
    let mut entries = Vec::with_capacity(chain.len());
    for provider in &chain {
        let stats = provider.cache_stats();
        entries.push(TtsCacheStatsEntry {
            provider: provider.id().to_string(),
            entries: provider.cache_len().await,
            hits: stats.hits,
            misses: stats.misses,
            evictions: stats.evictions,
        });
    }
    Ok(HttpResponse::Ok().json(json!({ "providers": entries })))
}

pub async fn clear_tts_cache_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<ClearTtsCacheQuery>,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let (_principal, _workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let chain = api.providers.tts_chain();
    let mut report = Vec::with_capacity(chain.len());
    for provider in &chain {
        let before = provider.cache_len().await;
        provider.clear_cache().await;
        report.push(ClearTtsCacheReport {
            provider: provider.id().to_string(),
            entries_dropped: before,
        });
    }
    Ok(HttpResponse::Ok().json(json!({
        "cleared": report,
        "total_entries_dropped": report.iter().map(|r| r.entries_dropped).sum::<usize>(),
    })))
}

/// Per-segment options shared by every iteration of the chain walk
/// inside the streamed-segment handler. Lifted out so the chain
/// rotation logic can be unit-tested without spinning up actix.
struct SegmentSynthOptions {
    voice: Option<String>,
    model: Option<String>,
    rate: Option<f32>,
    format: Option<String>,
    message_id: Option<String>,
    timeout: std::time::Duration,
}

/// Walk the provider chain for one segment and return a single
/// envelope — either `Segment` on success or `Error` after the chain
/// is exhausted. Voice/model are stripped on fallback so a user-
/// pinned primary voice doesn't break the fallback request.
///
/// Pure data in / data out — the only side effects are `tracing`
/// emits for fallback success and per-attempt timeout warnings.
/// Tests in this module exercise this directly with fake providers.
async fn synthesize_segment_via_chain(
    chain: &[Arc<dyn TtsProvider>],
    segment: &magician_media::media_rails::SpeechSegment,
    index: usize,
    opts: &SegmentSynthOptions,
) -> SegmentEnvelope {
    let mut last_err: Option<TtsError> = None;
    let mut attempts: Vec<String> = Vec::with_capacity(chain.len());
    for (idx, provider) in chain.iter().enumerate() {
        attempts.push(provider.id().to_string());
        let request = TtsRequest {
            text: segment.text.clone(),
            voice: if idx == 0 { opts.voice.clone() } else { None },
            rate: opts.rate,
            model: if idx == 0 { opts.model.clone() } else { None },
            format: opts.format.clone(),
            message_id: opts.message_id.clone(),
            emotion: segment.emotion,
            style: segment.style,
            pace: segment.pace,
            voice_mode: segment.voice_mode,
            emphasis: segment.emphasis.clone(),
        };
        let synth_outcome = tokio::time::timeout(opts.timeout, provider.synthesize(request)).await;
        match synth_outcome {
            Ok(Ok(response)) => {
                if idx > 0 {
                    tracing::info!(
                        target: "magician::tts::fallback",
                        succeeded_with = provider.id(),
                        attempts = %attempts.join(","),
                        segment_index = index,
                        "[TTS-STREAM] segment synth succeeded on fallback provider"
                    );
                }
                return SegmentEnvelope::Segment {
                    index,
                    message_id: opts.message_id.clone(),
                    audio_b64: base64_encode(&response.audio),
                    content_type: response.content_type,
                    provider: provider.id().to_string(),
                    model: response.model,
                    fallback: idx > 0,
                    attempts: attempts.join(","),
                };
            },
            Ok(Err(err @ TtsError::Upstream { .. })) | Ok(Err(err @ TtsError::Transport(_))) => {
                last_err = Some(err);
                continue;
            },
            Ok(Err(err)) => {
                last_err = Some(err);
                break;
            },
            Err(_elapsed) => {
                tracing::warn!(
                    provider = provider.id(),
                    attempt = idx,
                    timeout_secs = opts.timeout.as_secs(),
                    "[TTS-STREAM] segment synth timed out; rotating to next"
                );
                last_err = Some(TtsError::Transport(format!(
                    "synth timed out after {}s",
                    opts.timeout.as_secs()
                )));
                continue;
            },
        }
    }
    let (code, message) = match last_err {
        Some(TtsError::Upstream { status, body }) => {
            ("tts_upstream", format!("upstream {status}: {body}"))
        },
        Some(TtsError::Transport(msg)) => ("tts_transport", msg),
        Some(TtsError::BadRequest(msg)) => ("tts_bad_request", msg),
        Some(TtsError::NotConfigured(msg)) => ("tts_provider_not_configured", msg),
        None => ("tts_unknown", "no provider succeeded".to_string()),
    };
    SegmentEnvelope::Error {
        index,
        message_id: opts.message_id.clone(),
        code,
        message,
        attempts: attempts.join(","),
    }
}

/// Returns `application/x-ndjson` — each line is one
/// `SegmentEnvelope`. On a provider chain that exhausts all attempts
/// for a segment, an `error` envelope is emitted and the next segment
/// continues (we don't bail the whole stream on one bad segment —
/// partial audio is better than silence).
pub async fn synthesize_message_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    body: web::Json<SynthesizeMessageRequest>,
) -> Result<HttpResponse> {
    use futures_util::StreamExt;

    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let effective_provider = match resolve_dictation_tts_provider(
        api.get_ref(),
        &principal,
        &workspace,
        body.provider.as_deref(),
        None,
        &BTreeMap::new(),
    )
    .await
    {
        Ok(provider) => provider,
        Err(response) => return Ok(response),
    };
    let chain = api.providers.tts_chain_for(effective_provider.as_deref());
    if chain.is_empty() {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "tts_provider_not_configured",
            "message": "No backend TTS provider is configured.",
        })));
    }
    if body.segments.is_empty() {
        return Ok(HttpResponse::NoContent().finish());
    }
    // DoS defence: a chat reply realistically has 1–3 `<speech>`
    // blocks. A caller asking for 100+ segments in one request is
    // either buggy or hostile — bill them a 400 instead of burning
    // upstream synth credits. The single-shot endpoint enforces the
    // per-segment text cap separately; here we cap both axes.
    const MAX_SEGMENTS_PER_REQUEST: usize = 64;
    const MAX_SEGMENT_TEXT_LEN: usize = 32_000;
    if body.segments.len() > MAX_SEGMENTS_PER_REQUEST {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "tts_too_many_segments",
            "message": format!(
                "request had {} segments; max is {}",
                body.segments.len(),
                MAX_SEGMENTS_PER_REQUEST
            ),
        })));
    }
    if let Some((idx, seg)) = body
        .segments
        .iter()
        .enumerate()
        .find(|(_, s)| s.text.len() > MAX_SEGMENT_TEXT_LEN)
    {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "tts_segment_too_long",
            "message": format!(
                "segment {} has {} bytes; max per segment is {}",
                idx,
                seg.text.len(),
                MAX_SEGMENT_TEXT_LEN
            ),
        })));
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<SegmentEnvelope>(8);
    let message_id = body.message_id.clone();
    let voice = body.voice.clone();
    let model = body.model.clone();
    let rate = body.rate;
    let format = body.format.clone();
    let segments = body.segments;
    let chain_for_task = chain;
    // Per-segment synth timeout — caps the upstream wait so a hung
    // provider can't keep the NDJSON response open forever. The
    // streamed path doesn't lock the whole stream on one bad segment:
    // a timeout emits an error envelope (with code `tts_timeout`) for
    // that index and the producer moves to the next segment. Default
    // 60s; env-tunable for self-hosted setups with slower upstreams.
    let segment_timeout = std::env::var("MAGICIAN_TTS_SEGMENT_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs)
        .unwrap_or(std::time::Duration::from_secs(60));

    let opts = SegmentSynthOptions {
        voice,
        model,
        rate,
        format,
        message_id: message_id.clone(),
        timeout: segment_timeout,
    };
    tokio::spawn(async move {
        let total = segments.len();
        for (index, segment) in segments.into_iter().enumerate() {
            // Skip whitespace-only segments. Both backend parsers
            // (`parse_speech_segments`) and the frontend
            // (`extractSpeechBlocks`) drop empty bodies before
            // emitting segments, but the endpoint is a public
            // contract — a manual caller could pass `{text: "   "}`
            // and we don't want to bill an upstream synth call for
            // silence.
            if segment.text.trim().is_empty() {
                continue;
            }
            let envelope =
                synthesize_segment_via_chain(&chain_for_task, &segment, index, &opts).await;
            if tx.send(envelope).await.is_err() {
                // Client disconnected — abandon the rest of the
                // segments. Sending into a closed channel only
                // returns Err once; further iterations would also
                // return Err so we stop the producer.
                return;
            }
        }
        let _ = tx
            .send(SegmentEnvelope::Done {
                message_id: message_id.clone(),
                segment_count: total,
            })
            .await;
    });

    let ndjson_stream = tokio_stream::wrappers::ReceiverStream::new(rx).map(|envelope| {
        let json = serde_json::to_string(&envelope).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(format!("{json}\n")))
    });

    Ok(HttpResponse::Ok()
        .content_type("application/x-ndjson")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(ndjson_stream))
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::{engine::general_purpose::STANDARD, Engine};
    STANDARD.encode(bytes)
}

#[derive(Debug, Deserialize, Default)]
pub struct TranscribeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub message_id: Option<String>,
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub profile: Option<String>,
}

fn parse_audio_stage_options_from_query(
    query: &str,
) -> std::result::Result<BTreeMap<AudioStage, String>, HttpResponse> {
    let values = url::form_urlencoded::parse(query.as_bytes())
        .filter_map(|(key, value)| (key == "stage_option").then(|| value.into_owned()))
        .collect();
    parse_audio_stage_options(values)
}

fn parse_audio_stage_options(
    values: Vec<String>,
) -> std::result::Result<BTreeMap<AudioStage, String>, HttpResponse> {
    let mut stage_options = BTreeMap::new();
    for value in values {
        let Some((stage, option_id)) = value.split_once(':') else {
            return Err(HttpResponse::BadRequest().json(json!({
                "error": "invalid_audio_stage_option",
                "message": "stage_option must use stage:option_id",
            })));
        };
        let stage = stage.parse::<AudioStage>().map_err(|message| {
            HttpResponse::BadRequest().json(json!({
                "error": "invalid_audio_stage_option",
                "message": message,
            }))
        })?;
        let option_id = option_id.trim();
        if option_id.is_empty() {
            return Err(HttpResponse::BadRequest().json(json!({
                "error": "invalid_audio_stage_option",
                "message": "audio option id cannot be empty",
            })));
        }
        stage_options.insert(stage, option_id.to_string());
    }
    Ok(stage_options)
}

#[derive(Debug, Deserialize, Default)]
pub struct VoiceNoteQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub stt_provider: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct VoiceNoteLifecycleEventRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub event_type: String,
    #[serde(default = "default_event_payload")]
    pub payload: Value,
}

const VOICE_NOTE_CLIENT_EVENT_TYPES: &[&str] = &[
    MEDIA_VOICE_NOTE_RECORDING_STARTED,
    MEDIA_VOICE_NOTE_RECORDING_STOPPED,
    MEDIA_VOICE_NOTE_RECORDING_FAILED,
];

#[derive(Debug, Serialize)]
pub struct VoiceNoteResponse {
    pub chat_session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chat_turn_id: Option<String>,
    pub transcript: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_preview: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_speech_segments: Option<Vec<magician_media::media_rails::SpeechSegment>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_artifact_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued_message_id: Option<String>,
    pub transcript_provider: String,
    pub transcript_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_language: Option<String>,
}

#[derive(Debug)]
pub(crate) struct UploadedAudioForm {
    pub(crate) bytes: Vec<u8>,
    pub(crate) filename: Option<String>,
    pub(crate) mime_type: String,
    pub(crate) fields: HashMap<String, String>,
}

/// Owned inputs for the stack-heavy chat half of a voice-note submission.
///
/// Keeping these values owned is what allows the closure itself (rather than
/// an already-constructed chat future) to cross onto the execution runtime.
#[derive(Debug)]
struct VoiceNoteChatTurn {
    session_id: String,
    transcript: String,
    profile: Option<String>,
    mode: ChatMessageMode,
    source_surface: String,
    presence_session_id: Option<String>,
}

/// Run the complete transcript -> chat turn away from the Actix request
/// worker. Awaiting the small join handle on Actix is safe; constructing and
/// polling the Chat -> memory retrieval -> LanceDB/DataFusion future there is
/// not, because an ordinary Actix worker can exhaust its stack before the
/// agentic executor's own scheduling boundaries are reached.
async fn submit_voice_note_chat_turn_on_execution_runtime(
    chat_service: Arc<ChatService>,
    request: VoiceNoteChatTurn,
) -> anyhow::Result<ChatResponse> {
    chat_service
        .process_message_with_mode_on_execution_runtime(
            &request.session_id,
            Some(&request.transcript),
            &[],
            request.profile.as_deref(),
            request.mode,
            None,
            None,
            None,
            true,
            Some(request.source_surface.as_str()),
            request.presence_session_id.as_deref(),
            None,
            None,
        )
        .await
}

/// Transcribe audio via the registered STT provider.
///
/// Body shape: `multipart/form-data` with a single `file` part holding
/// the audio blob. Optional fields (language / model / prompt /
/// message_id) come through the query string so the body stays a pure
/// audio upload. Matches the chat-attachment upload shape so the
/// browser's existing `FormData` plumbing works without changes.
pub async fn submit_voice_note_handler(
    media_api: web::Data<Arc<MediaApi>>,
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<VoiceNoteQuery>,
    payload: Multipart,
) -> Result<HttpResponse> {
    let query = query.into_inner();
    let form = match read_uploaded_audio_form(payload).await {
        Ok(form) => form,
        Err(response) => return Ok(response),
    };
    // Scope is exclusively the middleware-verified bearer identity. Legacy
    // query/form fields are deliberately ignored rather than treated as input.
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };

    let source_surface = form_field(&form.fields, "source_surface")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("global_voice_note")
        .to_string();
    let presence_session_id = form_field(&form.fields, "presence_session_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let voice_session_id = form_field(&form.fields, "voice_session_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let chat_session_id = form_field(&form.fields, "chat_session_id")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let ui_thread_id = form_field(&form.fields, "thread_id")
        .or_else(|| form_field(&form.fields, "ui_thread_id"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("general")
        .to_string();
    let language = form_field(&form.fields, "language").map(str::to_owned);
    let model = form_field(&form.fields, "model").map(str::to_owned);
    let message_id = form_field(&form.fields, "message_id").map(str::to_owned);
    let prompt = form_field(&form.fields, "prompt").map(str::to_owned);
    let requested_stt_provider = form_field(&form.fields, "stt_provider")
        .or_else(|| form_field(&form.fields, "provider"))
        .map(str::to_owned)
        .or(query.stt_provider);
    let mode_value = form_field(&form.fields, "mode").map(str::to_owned);
    let profile = form_field(&form.fields, "profile")
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned);

    let session = if let Some(chat_session_id) = chat_session_id.as_deref() {
        match chat_api.chat_service.get_session(chat_session_id).await {
            Ok(Some(session))
                if session.principal != principal || session.workspace != workspace =>
            {
                return Ok(HttpResponse::Forbidden().json(json!({
                    "error": "chat_session_scope_mismatch",
                    "message": "the supplied chat_session_id belongs to a different scope",
                })));
            },
            Ok(Some(session)) if session.status == ChatSessionStatus::Archived => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "chat_session_archived",
                    "message": "voice notes cannot be submitted to an archived chat session",
                })));
            },
            Ok(Some(session)) => session,
            Ok(None) => {
                return Ok(HttpResponse::NotFound().json(json!({
                    "error": "chat_session_not_found",
                    "chat_session_id": chat_session_id,
                })));
            },
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": "chat_session_lookup_failed",
                    "details": error.to_string(),
                })));
            },
        }
    } else {
        let origin = ChatChannel {
            channel_type: "desktop_voice".to_string(),
            address: presence_session_id.clone(),
        };
        match chat_api
            .chat_service
            .get_or_create_session(&principal, &workspace, &ui_thread_id, &origin, None)
            .await
        {
            Ok(session) => session,
            Err(error) => {
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": "chat_session_create_failed",
                    "details": error.to_string(),
                })));
            },
        }
    };

    let effective_stt_provider = match resolve_dictation_stt_provider(
        media_api.get_ref(),
        &principal,
        &workspace,
        requested_stt_provider.as_deref(),
        None,
        &BTreeMap::new(),
    )
    .await
    {
        Ok(provider) => provider,
        Err(response) => return Ok(response),
    };
    let stt_chain = media_api
        .providers
        .stt_chain_for(effective_stt_provider.as_deref());
    let Some(first_stt_provider) = stt_chain.first() else {
        emit_voice_note_event(
            media_api.get_ref(),
            MEDIA_VOICE_NOTE_ERROR,
            &principal,
            &workspace,
            json!({
                "chat_session_id": session.id.as_str(),
                "source_surface": source_surface.as_str(),
                "presence_session_id": presence_session_id.as_deref(),
                "voice_session_id": voice_session_id.as_deref(),
                "error": "stt_provider_not_configured",
            }),
        );
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "stt_provider_not_configured",
            "message": "No backend STT provider is configured.",
        })));
    };
    let initial_stt_provider_id = first_stt_provider.id().to_string();

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_RECEIVED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "audio_bytes": form.bytes.len(),
            "content_type": form.mime_type.as_str(),
        }),
    );

    let retain_audio = parse_bool_field(&form.fields, "retain_audio");
    let audio_artifact_id = if retain_audio {
        let filename = form
            .filename
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or("voice-note.audio");
        match chat_api
            .chat_service
            .store_attachment(&session.id, filename, &form.mime_type, &form.bytes)
            .await
        {
            Ok(record) => Some(record.id),
            Err(error) => {
                emit_voice_note_event(
                    media_api.get_ref(),
                    MEDIA_VOICE_NOTE_ERROR,
                    &principal,
                    &workspace,
                    json!({
                        "chat_session_id": session.id.as_str(),
                        "source_surface": source_surface.as_str(),
                        "presence_session_id": presence_session_id.as_deref(),
                        "voice_session_id": voice_session_id.as_deref(),
                        "error": "audio_retention_failed",
                    }),
                );
                return Ok(HttpResponse::InternalServerError().json(json!({
                    "error": "audio_retention_failed",
                    "details": error.to_string(),
                })));
            },
        }
    } else {
        None
    };

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_TRANSCRIPTION_STARTED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_provider": initial_stt_provider_id.as_str(),
            "requested_transcript_provider": requested_stt_provider.as_deref(),
            "requested_model": model.as_deref(),
            "requested_language": language.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    let stt_request = SttRequest {
        audio: bytes::Bytes::from(form.bytes),
        content_type: form.mime_type,
        language,
        model,
        message_id,
        filename: form.filename,
        prompt,
    };
    let (stt_provider, stt_response) = match transcribe_with_stt_chain(stt_chain, stt_request).await
    {
        Ok(response) => response,
        Err(error) => {
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_TRANSCRIPTION_FAILED,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "transcript_provider": initial_stt_provider_id.as_str(),
                    "requested_transcript_provider": requested_stt_provider.as_deref(),
                    "error": "stt_failed",
                    "details": error.to_string(),
                    "audio_artifact_id": audio_artifact_id.as_deref(),
                }),
            );
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_ERROR,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "error": "stt_failed",
                    "details": error.to_string(),
                }),
            );
            return Ok(stt_error_response(&error));
        },
    };
    let transcript = stt_response.transcript.trim().to_string();
    let transcript_model = stt_response.model.clone();
    let transcript_language = stt_response.language.clone();
    if transcript.is_empty() {
        emit_voice_note_event(
            media_api.get_ref(),
            MEDIA_VOICE_NOTE_TRANSCRIPTION_FAILED,
            &principal,
            &workspace,
            json!({
                "chat_session_id": session.id.as_str(),
                "source_surface": source_surface.as_str(),
                "presence_session_id": presence_session_id.as_deref(),
                "voice_session_id": voice_session_id.as_deref(),
                "transcript_provider": stt_provider.id(),
                "requested_transcript_provider": requested_stt_provider.as_deref(),
                "error": "empty_transcript",
                "transcript_model": transcript_model.as_str(),
                "transcript_language": transcript_language.as_deref(),
                "audio_artifact_id": audio_artifact_id.as_deref(),
            }),
        );
        emit_voice_note_event(
            media_api.get_ref(),
            MEDIA_VOICE_NOTE_ERROR,
            &principal,
            &workspace,
            json!({
                "chat_session_id": session.id.as_str(),
                "source_surface": source_surface.as_str(),
                "presence_session_id": presence_session_id.as_deref(),
                "voice_session_id": voice_session_id.as_deref(),
                "error": "empty_transcript",
                "transcript_model": transcript_model.as_str(),
            }),
        );
        return Ok(
            HttpResponse::UnprocessableEntity().json(voice_note_correction_error_body(
                "empty_transcript",
                "The supplied audio did not produce a usable transcript.",
                session.id.as_str(),
                audio_artifact_id.as_deref(),
            )),
        );
    }

    // Language guard: voice currently supports English/Hinglish (Latin script)
    // only. Reject a transcript that is predominantly non-Latin — a Whisper
    // hallucination on silence, or genuinely non-English speech — instead of
    // injecting garbage into chat, and tell the user we only handle English.
    if !transcript_is_supported_language(&transcript) {
        emit_voice_note_event(
            media_api.get_ref(),
            MEDIA_VOICE_NOTE_TRANSCRIPTION_FAILED,
            &principal,
            &workspace,
            json!({
                "chat_session_id": session.id.as_str(),
                "source_surface": source_surface.as_str(),
                "presence_session_id": presence_session_id.as_deref(),
                "voice_session_id": voice_session_id.as_deref(),
                "transcript_provider": stt_provider.id(),
                "requested_transcript_provider": requested_stt_provider.as_deref(),
                "error": "unsupported_language",
                "transcript_model": transcript_model.as_str(),
                "transcript_language": transcript_language.as_deref(),
                "audio_artifact_id": audio_artifact_id.as_deref(),
            }),
        );
        emit_voice_note_event(
            media_api.get_ref(),
            MEDIA_VOICE_NOTE_ERROR,
            &principal,
            &workspace,
            json!({
                "chat_session_id": session.id.as_str(),
                "source_surface": source_surface.as_str(),
                "presence_session_id": presence_session_id.as_deref(),
                "voice_session_id": voice_session_id.as_deref(),
                "error": "unsupported_language",
                "message": "Voice notes currently support English only.",
            }),
        );
        return Ok(
            HttpResponse::UnprocessableEntity().json(voice_note_correction_error_body(
                "unsupported_language",
                "Sorry, I can only understand English (and Hinglish) right now — please try again \
                 in English.",
                session.id.as_str(),
                audio_artifact_id.as_deref(),
            )),
        );
    }

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_TRANSCRIPTION_COMPLETED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_provider": stt_provider.id(),
            "requested_transcript_provider": requested_stt_provider.as_deref(),
            "transcript_len": transcript.len(),
            "transcript_model": transcript_model.as_str(),
            "transcript_language": transcript_language.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_TRANSCRIBED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_provider": stt_provider.id(),
            "requested_transcript_provider": requested_stt_provider.as_deref(),
            "transcript_len": transcript.len(),
            "transcript_model": transcript_model.as_str(),
            "transcript_language": transcript_language.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    let mode = match parse_voice_note_mode(mode_value.as_deref()) {
        Ok(mode) => mode,
        Err(response) => {
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_ERROR,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "error": "invalid_voice_note_mode",
                    "mode": mode_value.as_deref(),
                }),
            );
            return Ok(response);
        },
    };
    if let Some(profile_name) = profile.as_deref() {
        let profiles = chat_api.chat_service.list_chat_profiles();
        if !profiles.iter().any(|profile| profile.name == profile_name) {
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_ERROR,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "error": "invalid_chat_profile",
                    "profile": profile_name,
                }),
            );
            return Ok(HttpResponse::BadRequest().json(json!({
                "error": "invalid_chat_profile",
                "profile": profile_name,
            })));
        }
    }

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_CHAT_SUBMIT_STARTED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_len": transcript.len(),
            "mode": mode_value.as_deref().unwrap_or("ask"),
            "profile": profile.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    let chat_turn = VoiceNoteChatTurn {
        session_id: session.id.clone(),
        transcript: transcript.clone(),
        profile: profile.clone(),
        mode,
        source_surface: source_surface.clone(),
        presence_session_id: presence_session_id.clone(),
    };
    let chat_response = match submit_voice_note_chat_turn_on_execution_runtime(
        Arc::clone(&chat_api.chat_service),
        chat_turn,
    )
    .await
    {
        Ok(response) => response,
        Err(error) => {
            let message = error.to_string();
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_CHAT_SUBMIT_FAILED,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "error": "chat_submit_failed",
                    "details": message.as_str(),
                    "audio_artifact_id": audio_artifact_id.as_deref(),
                }),
            );
            emit_voice_note_event(
                media_api.get_ref(),
                MEDIA_VOICE_NOTE_ERROR,
                &principal,
                &workspace,
                json!({
                    "chat_session_id": session.id.as_str(),
                    "source_surface": source_surface.as_str(),
                    "presence_session_id": presence_session_id.as_deref(),
                    "voice_session_id": voice_session_id.as_deref(),
                    "error": "chat_submit_failed",
                    "details": message,
                }),
            );
            if let Some(message) = extract_chat_bad_request_message(&error) {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "chat_submit_rejected",
                    "message": message,
                })));
            }
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "chat_submit_failed",
                "details": error.to_string(),
            })));
        },
    };

    let chat_turn_id = chat_response
        .user_message
        .as_ref()
        .and_then(|message| message.chat_turn_id.clone())
        .or_else(|| {
            chat_response
                .assistant_message
                .as_ref()
                .and_then(|message| message.chat_turn_id.clone())
        });
    let assistant_preview = chat_response
        .assistant_message
        .as_ref()
        .and_then(|message| message.content.text_content())
        .map(|text| truncate_for_voice_note_preview(text, 600));
    let assistant_speech_segments = chat_response
        .assistant_message
        .as_ref()
        .and_then(|message| message.speech_segments.clone())
        .filter(|segments| !segments.is_empty());
    let queued_message_id = chat_response
        .queued
        .as_ref()
        .map(|queued| queued.id.clone());

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_CHAT_SUBMIT_COMPLETED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "chat_turn_id": chat_turn_id.as_deref(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_len": transcript.len(),
            "queued_message_id": queued_message_id.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    emit_voice_note_event(
        media_api.get_ref(),
        MEDIA_VOICE_NOTE_SUBMITTED,
        &principal,
        &workspace,
        json!({
            "chat_session_id": session.id.as_str(),
            "chat_turn_id": chat_turn_id.as_deref(),
            "source_surface": source_surface.as_str(),
            "presence_session_id": presence_session_id.as_deref(),
            "voice_session_id": voice_session_id.as_deref(),
            "transcript_len": transcript.len(),
            "queued_message_id": queued_message_id.as_deref(),
            "audio_artifact_id": audio_artifact_id.as_deref(),
        }),
    );

    Ok(HttpResponse::Ok().json(VoiceNoteResponse {
        chat_session_id: session.id,
        chat_turn_id,
        transcript,
        assistant_preview,
        assistant_speech_segments,
        audio_artifact_id,
        queued_message_id,
        transcript_provider: stt_provider.id().to_string(),
        transcript_model,
        transcript_language,
    }))
}

pub async fn post_voice_note_event_handler(
    media_api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    body: web::Json<VoiceNoteLifecycleEventRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let event_type = body.event_type.trim();
    if !VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&event_type) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "voice_note_event_type_not_allowed",
            "event_type": event_type,
            "allowed": VOICE_NOTE_CLIENT_EVENT_TYPES,
        })));
    }
    emit_voice_note_event(
        media_api.get_ref(),
        event_type,
        &principal,
        &workspace,
        body.payload,
    );
    Ok(HttpResponse::Ok().json(json!({
        "accepted": true,
        "event_type": event_type,
    })))
}

pub(crate) async fn read_uploaded_audio_form(
    mut payload: Multipart,
) -> std::result::Result<UploadedAudioForm, HttpResponse> {
    use futures_util::StreamExt;

    const MAX_BYTES: usize = 25 * 1024 * 1024;
    const MAX_FIELD_BYTES: usize = 16 * 1024;

    let mut bytes: Option<Vec<u8>> = None;
    let mut filename: Option<String> = None;
    let mut mime_type = "application/octet-stream".to_string();
    let mut fields = HashMap::new();

    while let Some(field_res) = payload.next().await {
        let mut field = match field_res {
            Ok(field) => field,
            Err(error) => {
                return Err(HttpResponse::BadRequest().json(json!({
                    "error": "invalid_multipart_payload",
                    "details": error.to_string(),
                })));
            },
        };
        let disposition = field.content_disposition();
        let field_name = disposition.get_name().unwrap_or("").trim().to_string();
        if field_name.is_empty() {
            drain_multipart_field(&mut field).await;
            continue;
        }

        if field_name == "file" || field_name == "audio" {
            if bytes.is_some() {
                return Err(HttpResponse::BadRequest().json(json!({
                    "error": "multiple_audio_parts",
                    "message": "Provide exactly one `file` or `audio` multipart part.",
                })));
            }
            if let Some(name) = disposition.get_filename() {
                filename = Some(name.to_string());
            }
            if let Some(ct) = field.content_type() {
                mime_type = ct.to_string();
            }
            let mut audio = Vec::new();
            while let Some(chunk) = field.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        return Err(HttpResponse::BadRequest().json(json!({
                            "error": "failed_to_read_audio_chunk",
                            "details": error.to_string(),
                        })));
                    },
                };
                if audio.len() + chunk.len() > MAX_BYTES {
                    return Err(HttpResponse::PayloadTooLarge().json(json!({
                        "error": "audio_too_large",
                        "max_bytes": MAX_BYTES,
                    })));
                }
                audio.extend_from_slice(&chunk);
            }
            bytes = Some(audio);
            continue;
        }

        let mut value = Vec::new();
        while let Some(chunk) = field.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(error) => {
                    return Err(HttpResponse::BadRequest().json(json!({
                        "error": "failed_to_read_form_field",
                        "field": field_name,
                        "details": error.to_string(),
                    })));
                },
            };
            if value.len() + chunk.len() > MAX_FIELD_BYTES {
                return Err(HttpResponse::BadRequest().json(json!({
                    "error": "form_field_too_large",
                    "field": field_name,
                    "max_bytes": MAX_FIELD_BYTES,
                })));
            }
            value.extend_from_slice(&chunk);
        }
        fields.insert(
            field_name,
            String::from_utf8_lossy(&value).trim().to_string(),
        );
    }

    let Some(bytes) = bytes.filter(|bytes| !bytes.is_empty()) else {
        return Err(HttpResponse::BadRequest().json(json!({
            "error": "no_audio_supplied",
            "message": "Provide a `file` or `audio` multipart part containing the audio blob.",
        })));
    };

    Ok(UploadedAudioForm {
        bytes,
        filename,
        mime_type,
        fields,
    })
}

async fn drain_multipart_field(field: &mut actix_multipart::Field) {
    use futures_util::StreamExt;

    while let Some(chunk) = field.next().await {
        let _ = chunk;
    }
}

fn form_field<'a>(fields: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    fields
        .get(name)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn parse_bool_field(fields: &HashMap<String, String>, name: &str) -> bool {
    fields
        .get(name)
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn parse_voice_note_mode(mode: Option<&str>) -> std::result::Result<ChatMessageMode, HttpResponse> {
    match mode
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("ask")
    {
        "ask" => Ok(ChatMessageMode::Ask),
        "plan" => Ok(ChatMessageMode::Plan),
        "accept_in_scope" => Ok(ChatMessageMode::AcceptInScope),
        other => Err(HttpResponse::BadRequest().json(json!({
            "error": "invalid_voice_note_mode",
            "mode": other,
            "allowed": ["ask", "plan"],
        }))),
    }
}

fn truncate_for_voice_note_preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out = String::new();
    for ch in trimmed.chars().take(max_chars.saturating_sub(1)) {
        out.push(ch);
    }
    out.push('…');
    out
}

fn emit_voice_note_event(
    api: &MediaApi,
    event_type: &str,
    principal: &str,
    workspace: &str,
    payload: Value,
) {
    api.registry.broadcaster().emit_named(
        event_type,
        MEDIA_SYSTEM_AGENT,
        Some(principal),
        Some(workspace),
        payload,
    );
}

async fn transcribe_with_stt_chain(
    providers: Vec<Arc<dyn SttProvider>>,
    request: SttRequest,
) -> std::result::Result<(Arc<dyn SttProvider>, SttResponse), SttError> {
    let mut last_error: Option<SttError> = None;
    for provider in providers {
        match provider.transcribe(request.clone()).await {
            Ok(response) => return Ok((provider, response)),
            // A definitive "no speech / silence" verdict is TERMINAL — do not fall
            // through to the next provider (which, on silent audio, hallucinates a
            // foreign-language transcript). Return a clean empty transcript so the
            // caller's empty-transcript guard drops it without ever reaching chat.
            Err(SttError::NoSpeech) => {
                tracing::debug!(
                    provider = provider.id(),
                    "STT reported no speech (silence) — terminal, skipping fallback"
                );
                let model = provider.default_model().to_string();
                let message_id = request.message_id.clone();
                return Ok((
                    provider,
                    SttResponse {
                        transcript: String::new(),
                        model,
                        language: None,
                        message_id,
                        extras: None,
                    },
                ));
            },
            Err(error) => {
                tracing::warn!(
                    provider = provider.id(),
                    error = %error,
                    "STT provider failed; trying next provider if available"
                );
                last_error = Some(error);
            },
        }
    }
    Err(last_error.unwrap_or_else(|| SttError::NotConfigured("no STT providers".to_string())))
}

async fn resolve_dictation_stt_provider(
    api: &MediaApi,
    principal: &str,
    workspace: &str,
    explicit_provider: Option<&str>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> std::result::Result<Option<String>, HttpResponse> {
    // Media UX seam (plan 3.5): the dictation-mode policy lives in
    // `media_ux::dictation`; this wrapper keeps the `&MediaApi` signature the
    // handlers and tests already call and passes the rails pieces in.
    crate::media_ux::dictation::resolve_dictation_stt_provider(
        &api.providers,
        &api.preferences,
        &api.audio_runtime,
        principal,
        workspace,
        explicit_provider,
        explicit_profile,
        explicit_stage_options,
    )
    .await
}

async fn resolve_dictation_tts_provider(
    api: &MediaApi,
    principal: &str,
    workspace: &str,
    explicit_provider: Option<&str>,
    explicit_profile: Option<&str>,
    explicit_stage_options: &BTreeMap<AudioStage, String>,
) -> std::result::Result<Option<String>, HttpResponse> {
    crate::media_ux::dictation::resolve_dictation_tts_provider(
        &api.providers,
        &api.preferences,
        &api.audio_runtime,
        principal,
        workspace,
        explicit_provider,
        explicit_profile,
        explicit_stage_options,
    )
    .await
}

pub async fn transcribe_stt_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<TranscribeQuery>,
    mut payload: actix_multipart::Multipart,
) -> Result<HttpResponse> {
    use futures_util::StreamExt;

    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let stage_options = match parse_audio_stage_options_from_query(req.query_string()) {
        Ok(options) => options,
        Err(response) => return Ok(response),
    };
    let effective_provider = match resolve_dictation_stt_provider(
        api.get_ref(),
        &principal,
        &workspace,
        query.provider.as_deref(),
        query.profile.as_deref(),
        &stage_options,
    )
    .await
    {
        Ok(provider) => provider,
        Err(response) => return Ok(response),
    };
    let stt_chain = api.providers.stt_chain_for(effective_provider.as_deref());
    if stt_chain.is_empty() {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "stt_provider_not_configured",
            "message": "No backend STT provider is configured.",
        })));
    }

    const MAX_BYTES: usize = 25 * 1024 * 1024;
    let mut bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    let mut mime_type = "application/octet-stream".to_string();

    while let Some(field_res) = payload.next().await {
        let mut field = match field_res {
            Ok(f) => f,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "invalid_multipart_payload",
                    "details": e.to_string(),
                })));
            },
        };
        let disposition = field.content_disposition();
        let field_name = disposition.get_name().unwrap_or("").to_string();
        if field_name != "file" && field_name != "audio" {
            // Discard non-audio parts but don't fail — clients are
            // free to include hint fields that we ignore.
            while let Some(chunk) = field.next().await {
                let _ = chunk;
            }
            continue;
        }
        if let Some(name) = disposition.get_filename() {
            filename = Some(name.to_string());
        }
        if let Some(ct) = field.content_type() {
            mime_type = ct.to_string();
        }
        while let Some(chunk) = field.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    return Ok(HttpResponse::BadRequest().json(json!({
                        "error": "failed_to_read_audio_chunk",
                        "details": e.to_string(),
                    })));
                },
            };
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Ok(HttpResponse::PayloadTooLarge().json(json!({
                    "error": "audio_too_large",
                    "max_bytes": MAX_BYTES,
                })));
            }
            bytes.extend_from_slice(&chunk);
        }
    }

    if bytes.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "no_audio_supplied",
            "message": "Provide a `file` multipart part containing the audio blob.",
        })));
    }

    let stt_request = SttRequest {
        audio: bytes::Bytes::from(bytes),
        content_type: mime_type,
        language: query.language,
        model: query.model,
        message_id: query.message_id,
        filename,
        prompt: query.prompt,
    };
    match transcribe_with_stt_chain(stt_chain, stt_request).await {
        Ok((_provider, response)) => Ok(HttpResponse::Ok().json(response)),
        Err(err) => Ok(stt_error_response(&err)),
    }
}

/// Streaming variant of `/media/stt/transcribe` — returns an SSE
/// stream of `SttStreamEvent` JSON envelopes. Browsers parse the
/// stream and update the composer incrementally as `Delta` events
/// arrive; the `Final` event triggers downstream behaviour
/// (auto-send, transcript memory candidate, etc.).
///
/// Same multipart input shape + same scope semantics as the
/// non-streaming handler. Falls through to the provider's default
/// `transcribe_stream` impl (which yields one synthesised `Final`
/// event) when the configured STT adapter doesn't override —
/// preserves the same contract for non-streaming providers.
pub async fn transcribe_stt_stream_handler(
    api: web::Data<Arc<MediaApi>>,
    req: HttpRequest,
    query: web::Query<TranscribeQuery>,
    mut payload: actix_multipart::Multipart,
) -> Result<HttpResponse> {
    use futures_util::StreamExt;

    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let stage_options = match parse_audio_stage_options_from_query(req.query_string()) {
        Ok(options) => options,
        Err(response) => return Ok(response),
    };
    let effective_provider = match resolve_dictation_stt_provider(
        api.get_ref(),
        &principal,
        &workspace,
        query.provider.as_deref(),
        query.profile.as_deref(),
        &stage_options,
    )
    .await
    {
        Ok(provider) => provider,
        Err(response) => return Ok(response),
    };
    let stt_chain = api.providers.stt_chain_for(effective_provider.as_deref());
    if stt_chain.is_empty() {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "stt_provider_not_configured",
            "message": "No backend STT provider is configured.",
        })));
    }

    const MAX_BYTES: usize = 25 * 1024 * 1024;
    let mut bytes: Vec<u8> = Vec::new();
    let mut filename: Option<String> = None;
    let mut mime_type = "application/octet-stream".to_string();

    while let Some(field_res) = payload.next().await {
        let mut field = match field_res {
            Ok(f) => f,
            Err(e) => {
                return Ok(HttpResponse::BadRequest().json(json!({
                    "error": "invalid_multipart_payload",
                    "details": e.to_string(),
                })));
            },
        };
        let disposition = field.content_disposition();
        let field_name = disposition.get_name().unwrap_or("").to_string();
        if field_name != "file" && field_name != "audio" {
            while let Some(chunk) = field.next().await {
                let _ = chunk;
            }
            continue;
        }
        if let Some(name) = disposition.get_filename() {
            filename = Some(name.to_string());
        }
        if let Some(ct) = field.content_type() {
            mime_type = ct.to_string();
        }
        while let Some(chunk) = field.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    return Ok(HttpResponse::BadRequest().json(json!({
                        "error": "failed_to_read_audio_chunk",
                        "details": e.to_string(),
                    })));
                },
            };
            if bytes.len() + chunk.len() > MAX_BYTES {
                return Ok(HttpResponse::PayloadTooLarge().json(json!({
                    "error": "audio_too_large",
                    "max_bytes": MAX_BYTES,
                })));
            }
            bytes.extend_from_slice(&chunk);
        }
    }

    if bytes.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "no_audio_supplied",
        })));
    }

    let stt_request = SttRequest {
        audio: bytes::Bytes::from(bytes),
        content_type: mime_type,
        language: query.language,
        model: query.model,
        message_id: query.message_id,
        filename,
        prompt: query.prompt,
    };

    // Bridge: provider pushes `SttStreamEvent` into `event_tx`; this
    // handler converts each one to an SSE-formatted `Bytes` chunk
    // and forwards through the actix streaming response. A 64-slot
    // mpsc buffer is plenty — backpressure here just slows the
    // provider, which is desirable when the client is reading
    // slower than OpenAI streams.
    let (event_tx, event_rx) = tokio::sync::mpsc::channel::<SttStreamEvent>(64);

    tokio::spawn(async move {
        let mut last_error: Option<SttError> = None;
        for provider in stt_chain {
            match provider
                .transcribe_stream(stt_request.clone(), event_tx.clone())
                .await
            {
                Ok(_response) => {
                    return;
                },
                Err(error) => {
                    tracing::warn!(
                        provider = provider.id(),
                        error = %error,
                        "streaming STT provider failed; trying next provider if available"
                    );
                    last_error = Some(error);
                },
            }
        }
        let reason = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "stt provider not configured".to_string());
        let _ = event_tx.send(SttStreamEvent::Error { reason }).await;
        // Drop tx so the receiver stream completes.
    });

    let sse_stream = tokio_stream::wrappers::ReceiverStream::new(event_rx).map(|event| {
        let json = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(format!(
            "event: stt\ndata: {json}\n\n"
        )))
    });

    Ok(HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(sse_stream))
}

fn tts_error_response(err: &TtsError) -> HttpResponse {
    match err {
        TtsError::NotConfigured(_) => HttpResponse::ServiceUnavailable().json(json!({
            "error": "tts_provider_not_configured",
            "message": err.to_string(),
        })),
        TtsError::BadRequest(_) => HttpResponse::BadRequest().json(json!({
            "error": "tts_bad_request",
            "message": err.to_string(),
        })),
        TtsError::Upstream { status, body } => HttpResponse::BadGateway().json(json!({
            "error": "tts_upstream_error",
            "status": status,
            "body": body,
        })),
        TtsError::Transport(_) => HttpResponse::BadGateway().json(json!({
            "error": "tts_transport_error",
            "message": err.to_string(),
        })),
    }
}

fn stt_error_response(err: &SttError) -> HttpResponse {
    match err {
        SttError::NotConfigured(_) => HttpResponse::ServiceUnavailable().json(json!({
            "error": "stt_provider_not_configured",
            "message": err.to_string(),
        })),
        SttError::BadRequest(_) => HttpResponse::BadRequest().json(json!({
            "error": "stt_bad_request",
            "message": err.to_string(),
        })),
        SttError::Upstream { status, body } => HttpResponse::BadGateway().json(json!({
            "error": "stt_upstream_error",
            "status": status,
            "body": body,
        })),
        SttError::Transport(_) => HttpResponse::BadGateway().json(json!({
            "error": "stt_transport_error",
            "message": err.to_string(),
        })),
        SttError::NoSpeech => HttpResponse::UnprocessableEntity().json(json!({
            "error": "no_speech",
            "message": "The supplied audio contained no speech.",
        })),
    }
}

/// Heuristic language guard for voice transcripts. Returns false when the
/// transcript's alphabetic content is predominantly NON-Latin (CJK, Devanagari,
/// Cyrillic, Arabic, …) — the signature of a Whisper hallucination on silence
/// or genuinely non-English speech. English and Hinglish are Latin-script, so
/// they pass. Used to reject unsupported-language voice notes before they reach
/// chat (voice currently supports English/Hinglish only).
fn transcript_is_supported_language(transcript: &str) -> bool {
    let mut latin = 0usize;
    let mut non_latin = 0usize;
    for ch in transcript.chars() {
        if !ch.is_alphabetic() {
            continue;
        }
        // ASCII Latin + Latin-1 Supplement / Latin Extended-A & -B cover English
        // and accented Latin; everything else alphabetic is a non-Latin script.
        if ch.is_ascii_alphabetic() || matches!(ch, '\u{00C0}'..='\u{024F}') {
            latin += 1;
        } else {
            non_latin += 1;
        }
    }
    let total = latin + non_latin;
    if total == 0 {
        return true; // no letters (digits/punctuation only) — other guards
                     // handle it
    }
    (non_latin as f64) / (total as f64) <= 0.2
}

fn voice_note_correction_error_body(
    code: &str,
    message: &str,
    chat_session_id: &str,
    audio_artifact_id: Option<&str>,
) -> Value {
    json!({
        "error": code,
        "message": message,
        "chat_session_id": chat_session_id,
        "audio_artifact_id": audio_artifact_id,
    })
}

#[cfg(test)]
mod audio_settings_api_tests {
    use super::*;
    use actix_web::{http::StatusCode, test, App};
    use async_trait::async_trait;
    use serde_json::json;

    use magician::config::MagicianMediaSettings;
    use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;
    use magician_media::media_rails::{
        AudioRuntimeConfigManager, SttError, SttProvider, SttRequest, SttResponse, TtsResponse,
    };

    #[actix_web::test]
    async fn corrective_voice_note_error_retains_session_and_audio_receipt() {
        let body = voice_note_correction_error_body(
            "unsupported_language",
            "Please try again in English.",
            "chat-voice-1",
            Some("att-1"),
        );

        assert_eq!(body["error"], "unsupported_language");
        assert_eq!(body["message"], "Please try again in English.");
        assert_eq!(body["chat_session_id"], "chat-voice-1");
        assert_eq!(body["audio_artifact_id"], "att-1");
    }

    #[actix_web::test]
    async fn dictation_query_stage_overrides_use_the_canonical_stage_option_wire_shape() {
        let query = "principal=alice&stage_option=recording_stt%3Aqwen-local&stage_option=tts%\
                     3Akokoro-local";
        web::Query::<TranscribeQuery>::from_query(query).expect("typed query fields");
        let options = parse_audio_stage_options_from_query(query).expect("stage options");
        assert_eq!(
            options.get(&AudioStage::RecordingStt).map(String::as_str),
            Some("qwen-local")
        );
        assert_eq!(
            options.get(&AudioStage::Tts).map(String::as_str),
            Some("kokoro-local")
        );
        assert!(parse_audio_stage_options_from_query("stage_option=recording_stt").is_err());
    }

    struct NamedStt(&'static str);

    struct NamedTts(&'static str);

    #[async_trait]
    impl SttProvider for NamedStt {
        fn id(&self) -> &str {
            self.0
        }

        fn default_model(&self) -> &str {
            "test-model"
        }

        async fn transcribe(&self, _request: SttRequest) -> Result<SttResponse, SttError> {
            Ok(SttResponse {
                transcript: "test".to_string(),
                model: "test-model".to_string(),
                language: Some("en".to_string()),
                message_id: None,
                extras: None,
            })
        }
    }

    #[async_trait]
    impl TtsProvider for NamedTts {
        fn id(&self) -> &str {
            self.0
        }

        fn default_voice(&self) -> Option<&str> {
            None
        }

        fn default_model(&self) -> &str {
            "test-model"
        }

        async fn synthesize(&self, _request: TtsRequest) -> Result<TtsResponse, TtsError> {
            unreachable!("provider-resolution tests do not synthesize")
        }
    }

    fn test_api(storage_root: &std::path::Path) -> Arc<MediaApi> {
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let registry = Arc::new(RealtimeSessionRegistry::new(broadcaster));
        let providers = Arc::new(MediaProviderRegistry::new());
        let runtime = Arc::new(
            AudioRuntimeConfigManager::in_memory(
                MagicianMediaSettings::default(),
                Arc::clone(&providers),
            )
            .expect("audio runtime"),
        );
        Arc::new(
            MediaApi::new(registry)
                .with_providers(providers)
                .with_preferences(Arc::new(MediaPreferencesStore::new(storage_root)))
                .with_audio_runtime(runtime),
        )
    }

    #[actix_web::test]
    async fn media_preferences_request_rejects_removed_provider_fields() {
        let current = serde_json::from_value::<PutMediaPreferencesRequest>(json!({
            "workspace": "default",
            "schema_version": MEDIA_PREFERENCES_SCHEMA_VERSION,
            "require_voice_prefix": false,
            "surface_profiles": {}
        }))
        .expect("current schema round-trips through the update contract");
        assert_eq!(
            current.schema_version,
            Some(MEDIA_PREFERENCES_SCHEMA_VERSION)
        );
        assert_eq!(current.require_voice_prefix, Some(false));

        let error = serde_json::from_value::<PutMediaPreferencesRequest>(json!({
            "workspace": "default",
            "recording_stt_provider": "openai"
        }))
        .expect_err("legacy provider field must not be accepted");
        assert!(error.to_string().contains("unknown field"));
    }

    #[actix_web::test]
    async fn fluid_audio_dictation_provider_resolves_from_the_named_default_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let registry = Arc::new(RealtimeSessionRegistry::new(broadcaster));
        let providers =
            Arc::new(MediaProviderRegistry::new().with_stt(Arc::new(NamedStt("fluid-qwen"))));
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
recording_stt:
  providers:
    - id: fluid-qwen
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
surface_profiles:
  default_mapping:
    dictation: dictation-fluid
  profiles:
    dictation-fluid:
      surface: dictation
      turn_boundary: push_to_talk
      recording_stt:
        enabled: true
        providers: [fluid-qwen]
"#,
        )
        .expect("settings");
        let runtime = Arc::new(
            AudioRuntimeConfigManager::in_memory(settings, Arc::clone(&providers))
                .expect("audio runtime"),
        );
        let api = Arc::new(
            MediaApi::new(registry)
                .with_providers(providers)
                .with_preferences(Arc::new(MediaPreferencesStore::new(temp.path())))
                .with_audio_runtime(runtime),
        );

        let selected = resolve_dictation_stt_provider(
            api.as_ref(),
            "alice",
            "default",
            None,
            None,
            &BTreeMap::new(),
        )
        .await
        .unwrap_or_else(|_| panic!("profile should resolve"));
        assert_eq!(selected.as_deref(), Some("fluid-qwen"));

        let explicit = resolve_dictation_stt_provider(
            api.as_ref(),
            "alice",
            "default",
            Some("fluid-qwen"),
            None,
            &BTreeMap::new(),
        )
        .await
        .unwrap_or_else(|_| panic!("explicit provider should resolve"));
        assert_eq!(explicit.as_deref(), Some("fluid-qwen"));

        let unknown = resolve_dictation_stt_provider(
            api.as_ref(),
            "alice",
            "default",
            Some("missing-provider"),
            None,
            &BTreeMap::new(),
        )
        .await
        .expect_err("unknown explicit provider must fail");
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn explicit_disabled_audio_provider_is_rejected_before_dispatch() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let registry = Arc::new(RealtimeSessionRegistry::new(broadcaster));
        let providers =
            Arc::new(MediaProviderRegistry::new().with_stt(Arc::new(NamedStt("fluid-qwen"))));
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: false
recording_stt:
  providers:
    - id: fluid-qwen
      engine_id: fluid_audio
      adapter: fluid_audio_recording_stt
      model: FluidInference/qwen3-asr-0.6b-coreml
surface_profiles:
  default_mapping:
    dictation: dictation-fluid
  profiles:
    dictation-fluid:
      surface: dictation
      turn_boundary: push_to_talk
      recording_stt:
        enabled: true
        providers: [fluid-qwen]
"#,
        )
        .expect("settings");
        let runtime = Arc::new(
            AudioRuntimeConfigManager::in_memory(settings, Arc::clone(&providers))
                .expect("audio runtime"),
        );
        let api = Arc::new(
            MediaApi::new(registry)
                .with_providers(providers)
                .with_preferences(Arc::new(MediaPreferencesStore::new(temp.path())))
                .with_audio_runtime(runtime),
        );

        let response = resolve_dictation_stt_provider(
            api.as_ref(),
            "alice",
            "default",
            Some("fluid-qwen"),
            None,
            &BTreeMap::new(),
        )
        .await
        .expect_err("disabled explicit provider must fail closed");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[actix_web::test]
    async fn dictation_tts_resolves_from_profile_and_rejects_unknown_override() {
        let temp = tempfile::tempdir().expect("tempdir");
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let registry = Arc::new(RealtimeSessionRegistry::new(broadcaster));
        let providers =
            Arc::new(MediaProviderRegistry::new().with_tts(Arc::new(NamedTts("profile-tts"))));
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
tts:
  providers:
    - id: profile-tts
      adapter: configured_tts
      model: test-model
surface_profiles:
  default_mapping:
    dictation: dictation-tts
  profiles:
    dictation-tts:
      surface: dictation
      turn_boundary: push_to_talk
      tts:
        enabled: true
        providers: [profile-tts]
"#,
        )
        .expect("settings");
        let runtime = Arc::new(
            AudioRuntimeConfigManager::in_memory(settings, Arc::clone(&providers))
                .expect("audio runtime"),
        );
        let api = Arc::new(
            MediaApi::new(registry)
                .with_providers(providers)
                .with_preferences(Arc::new(MediaPreferencesStore::new(temp.path())))
                .with_audio_runtime(runtime),
        );

        let selected = resolve_dictation_tts_provider(
            api.as_ref(),
            "alice",
            "default",
            None,
            None,
            &BTreeMap::new(),
        )
        .await
        .unwrap_or_else(|_| panic!("profile should resolve"));
        assert_eq!(selected.as_deref(), Some("profile-tts"));

        let unknown = resolve_dictation_tts_provider(
            api.as_ref(),
            "alice",
            "default",
            Some("missing-provider"),
            None,
            &BTreeMap::new(),
        )
        .await
        .expect_err("unknown explicit provider must fail");
        assert_eq!(unknown.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn settings_get_and_put_preserve_contract_and_enforce_revision() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = test_api(temp.path());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .route("/audio-settings", web::get().to(get_audio_settings_handler))
                .route("/audio-settings", web::put().to(put_audio_settings_handler)),
        )
        .await;

        let get = test::TestRequest::get().uri("/audio-settings").to_request();
        let response = test::call_service(&app, get).await;
        assert_eq!(response.status(), StatusCode::OK);
        let initial: Value = test::read_body_json(response).await;
        let revision = initial["revision"].as_str().expect("revision").to_string();
        assert_eq!(initial["requires_session_restart"], true);
        assert!(initial["stages"]["recording_stt"].is_array());
        assert!(initial["models"].is_object());
        assert_eq!(
            initial["default_profiles"]["dictation"],
            "migrated-dictation-v1"
        );

        let put = test::TestRequest::put()
            .uri("/audio-settings")
            .set_json(json!({
                "expected_revision": revision,
                "default_profiles": {},
                "profiles": {
                    "migrated-dictation-v1": {
                        "stages": {
                            "tts": {"enabled": false}
                        }
                    }
                },
            }))
            .to_request();
        let response = test::call_service(&app, put).await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated: Value = test::read_body_json(response).await;
        assert!(updated["profiles"]["migrated-dictation-v1"].is_object());

        let stale_put = test::TestRequest::put()
            .uri("/audio-settings")
            .set_json(json!({
                "expected_revision": initial["revision"],
                "default_profiles": {},
                "profiles": {},
            }))
            .to_request();
        let response = test::call_service(&app, stale_put).await;
        assert_eq!(response.status(), StatusCode::CONFLICT);
        let error: Value = test::read_body_json(response).await;
        assert_eq!(error["error"], "audio_settings_revision_conflict");
    }

    #[actix_web::test]
    async fn settings_put_disables_the_live_fluid_audio_manager() {
        let temp = tempfile::tempdir().expect("tempdir");
        let settings: MagicianMediaSettings = serde_yaml::from_str(
            r#"
engines:
  fluid_audio:
    enabled: true
    startup: lazy
    download_policy: on_demand
vad:
  providers:
    - id: fluid-silero-v6
      engine_id: fluid_audio
      adapter: fluid_audio_vad
      model: FluidInference/silero-vad-coreml
"#,
        )
        .expect("settings");
        let broadcaster = Arc::new(RuntimeTransportBroadcaster::new(32));
        let registry = Arc::new(RealtimeSessionRegistry::new(broadcaster));
        let providers = Arc::new(MediaProviderRegistry::new());
        let runtime = Arc::new(
            AudioRuntimeConfigManager::in_memory(settings.clone(), Arc::clone(&providers))
                .expect("audio runtime"),
        );
        let manager = Arc::new(
            magician_media::media_rails::fluid_audio::FluidAudioEngineManager::from_media_settings(
                &settings,
                temp.path().join("magician-config.yaml"),
            )
            .expect("manager")
            .expect("configured manager"),
        );
        let api = Arc::new(
            MediaApi::new(registry)
                .with_providers(providers)
                .with_preferences(Arc::new(MediaPreferencesStore::new(temp.path())))
                .with_audio_runtime(Arc::clone(&runtime))
                .with_fluid_audio(Some(Arc::clone(&manager))),
        );
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .route("/audio-settings", web::put().to(put_audio_settings_handler)),
        )
        .await;

        let put = test::TestRequest::put()
            .uri("/audio-settings")
            .set_json(json!({
                "expected_revision": runtime.snapshot().revision,
                "engines": {
                    "fluid_audio": {"enabled": false}
                }
            }))
            .to_request();
        let response = test::call_service(&app, put).await;
        assert_eq!(response.status(), StatusCode::OK);
        let updated: Value = test::read_body_json(response).await;
        assert_eq!(updated["engines"]["fluid_audio"]["enabled"], false);
        assert_eq!(updated["engines"]["fluid_audio"]["available"], false);
        assert!(!manager.is_enabled());
    }

    #[actix_web::test]
    async fn model_controls_fail_closed_for_unknown_or_unavailable_engines() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = test_api(temp.path());
        let app = test::init_service(App::new().app_data(web::Data::new(api)).route(
            "/audio-engines/{engine_id}/models/{action}",
            web::post().to(post_audio_engine_model_control_handler),
        ))
        .await;

        let unknown = test::TestRequest::post()
            .uri("/audio-engines/unknown/models/prewarm")
            .set_json(json!({"model_ids": ["anything"]}))
            .to_request();
        assert_eq!(
            test::call_service(&app, unknown).await.status(),
            StatusCode::NOT_FOUND
        );

        let unavailable = test::TestRequest::post()
            .uri("/audio-engines/fluid_audio/models/prewarm")
            .set_json(json!({"model_ids": ["fluid-silero-v6"]}))
            .to_request();
        assert_eq!(
            test::call_service(&app, unavailable).await.status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[actix_web::test]
    async fn resolved_surface_and_registered_session_report_migrated_default_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = test_api(temp.path());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .route(
                    "/surfaces/{surface}/resolved",
                    web::get().to(get_resolved_audio_surface_handler),
                )
                .route("/sessions", web::post().to(register_media_session_handler)),
        )
        .await;

        let resolve = test::TestRequest::get()
            .uri("/surfaces/dictation/resolved?workspace=default")
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "default"))
            .to_request();
        let response = test::call_service(&app, resolve).await;
        assert_eq!(response.status(), StatusCode::OK);
        let resolved: Value = test::read_body_json(response).await;
        assert_eq!(resolved["surface"], "dictation");
        assert_eq!(resolved["profile_id"], "migrated-dictation-v1");
        assert_eq!(resolved["source"], "configured_default");

        let register = test::TestRequest::post()
            .uri("/sessions")
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "default"))
            .set_json(json!({
                "workspace": "default",
                "audio_surface": "dictation"
            }))
            .to_request();
        let response = test::call_service(&app, register).await;
        assert_eq!(response.status(), StatusCode::OK);
        let registered: Value = test::read_body_json(response).await;
        assert_eq!(registered["session"]["audio_surface"], "dictation");
        assert_eq!(
            registered["session"]["resolved_audio_profile"]["profile_id"],
            "migrated-dictation-v1"
        );
    }

    #[actix_web::test]
    async fn scoped_preferences_reject_unknown_and_inapplicable_stage_options() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = test_api(temp.path());
        let app = test::init_service(
            App::new()
                .app_data(web::Data::new(api))
                .route("/preferences", web::put().to(put_media_preferences_handler)),
        )
        .await;

        let unknown = test::TestRequest::put()
            .uri("/preferences")
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "default"))
            .set_json(json!({
                "workspace": "default",
                "surface_stage_options": {
                    "dictation": { "recording_stt": "not-configured" }
                }
            }))
            .to_request();
        let response = test::call_service(&app, unknown).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let inapplicable = test::TestRequest::put()
            .uri("/preferences")
            .insert_header(("X-Principal", "alice"))
            .insert_header(("X-Workspace", "default"))
            .set_json(json!({
                "workspace": "default",
                "surface_stage_options": {
                    "dictation": { "streaming_stt": "off" }
                }
            }))
            .to_request();
        let response = test::call_service(&app, inapplicable).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[actix_web::test]
    async fn provider_inventory_keeps_legacy_fields_and_adds_stage_catalog() {
        let temp = tempfile::tempdir().expect("tempdir");
        let api = test_api(temp.path());
        let legacy = serde_json::to_value(api.providers.snapshot()).expect("legacy snapshot");
        let response = list_media_providers_handler(web::Data::new(api))
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::OK);
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .expect("body");
        let body: Value = serde_json::from_slice(&bytes).expect("json");
        for (key, value) in legacy.as_object().expect("legacy object") {
            assert_eq!(body.get(key), Some(value));
        }
        assert!(body["audio_revision"].is_string());
        assert!(body["stages"]["streaming_stt"].is_array());
        assert!(body["surface_profiles"].is_object());
        assert!(body["hands_free_voice"].is_boolean());
    }
}

#[cfg(test)]
mod voice_note_tests {
    use super::*;
    use magician::magician_v2::chat::models::ChatMessageMode;
    use std::collections::HashMap;

    use actix_web::http::StatusCode;

    #[test]
    fn parse_voice_note_mode_defaults_to_ask() {
        assert!(matches!(
            parse_voice_note_mode(None).expect("default mode"),
            ChatMessageMode::Ask
        ));
        assert!(matches!(
            parse_voice_note_mode(Some("")).expect("empty mode"),
            ChatMessageMode::Ask
        ));
    }

    #[test]
    fn parse_voice_note_mode_accepts_plan_and_rejects_unknown() {
        assert!(matches!(
            parse_voice_note_mode(Some("plan")).expect("plan mode"),
            ChatMessageMode::Plan
        ));
        let response = parse_voice_note_mode(Some("stream")).expect_err("unknown mode rejected");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn truncate_for_voice_note_preview_preserves_short_text_and_unicode_boundary() {
        assert_eq!(truncate_for_voice_note_preview("  hello  ", 20), "hello");
        assert_eq!(truncate_for_voice_note_preview("नमस्ते दुनिया", 5), "नमस्…");
    }

    #[test]
    fn parse_bool_field_only_accepts_explicit_truthy_values() {
        let mut fields = HashMap::new();
        fields.insert("retain_audio".to_string(), " yes ".to_string());
        assert!(parse_bool_field(&fields, "retain_audio"));
        fields.insert("retain_audio".to_string(), "false".to_string());
        assert!(!parse_bool_field(&fields, "retain_audio"));
        assert!(!parse_bool_field(&fields, "missing"));
    }

    #[test]
    fn voice_note_client_event_allowlist_is_capture_only() {
        assert!(VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&MEDIA_VOICE_NOTE_RECORDING_STARTED));
        assert!(VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&MEDIA_VOICE_NOTE_RECORDING_STOPPED));
        assert!(VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&MEDIA_VOICE_NOTE_RECORDING_FAILED));
        assert!(!VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&MEDIA_VOICE_NOTE_SUBMITTED));
        assert!(!VOICE_NOTE_CLIENT_EVENT_TYPES.contains(&MEDIA_VOICE_NOTE_ERROR));
    }

    #[test]
    fn voice_note_chat_submission_constructs_the_turn_on_the_execution_runtime() {
        let source = include_str!("media_api.rs");
        let helper = source
            .split_once("async fn submit_voice_note_chat_turn_on_execution_runtime")
            .expect("voice-note execution helper")
            .1
            .split_once("pub async fn submit_voice_note_handler")
            .expect("voice-note handler after execution helper")
            .0;
        assert!(helper.contains(".process_message_with_mode_on_execution_runtime("));
        assert!(!helper.contains("with_app_owner_credential"));
        assert!(!helper.contains(".process_message_with_mode("));

        let handler = source
            .split_once("pub async fn submit_voice_note_handler")
            .expect("voice-note handler")
            .1
            .split_once("pub async fn post_voice_note_event_handler")
            .expect("voice-note event handler after submit handler")
            .0;
        assert!(handler.contains("submit_voice_note_chat_turn_on_execution_runtime("));
        assert!(!handler.contains("with_app_owner_credential"));
        assert!(!handler.contains(".process_message_with_mode("));
    }
}

#[cfg(test)]
mod synthesize_message_tests {
    //! Unit tests for the streamed-segment chain rotation logic.
    //! Exercises `synthesize_segment_via_chain` with fake providers
    //! so the handler's response semantics (success, fallback, all-
    //! fail, timeout, BadRequest short-circuit) are pinned by tests
    //! without needing actix's HTTP test harness.
    use super::*;
    use std::{sync::Arc, time::Duration};

    use async_trait::async_trait;
    use bytes::Bytes;

    use magician_media::media_rails::{
        providers::tts::{TtsError, TtsProvider, TtsRequest, TtsResponse},
        SpeechSegment,
    };

    enum Behaviour {
        Ok,
        Upstream(u16),
        Transport,
        BadRequest,
        SleepForever,
    }

    struct FakeProvider {
        id: &'static str,
        model: &'static str,
        behaviour: Behaviour,
    }

    #[async_trait]
    impl TtsProvider for FakeProvider {
        fn id(&self) -> &str {
            self.id
        }
        fn default_voice(&self) -> Option<&str> {
            None
        }
        fn default_model(&self) -> &str {
            self.model
        }
        async fn synthesize(&self, _: TtsRequest) -> Result<TtsResponse, TtsError> {
            match self.behaviour {
                Behaviour::Ok => Ok(TtsResponse {
                    audio: Bytes::from_static(b"audio-bytes"),
                    content_type: "audio/mpeg".into(),
                    model: self.model.to_string(),
                    voice: None,
                    message_id: None,
                }),
                Behaviour::Upstream(status) => Err(TtsError::Upstream {
                    status,
                    body: "upstream said no".into(),
                }),
                Behaviour::Transport => Err(TtsError::Transport("net down".into())),
                Behaviour::BadRequest => Err(TtsError::BadRequest("bad input".into())),
                Behaviour::SleepForever => {
                    // 1h is "forever" relative to the test timeout (50ms).
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    unreachable!()
                },
            }
        }
    }

    fn provider(id: &'static str, behaviour: Behaviour) -> Arc<dyn TtsProvider> {
        Arc::new(FakeProvider {
            id,
            model: "fake-model",
            behaviour,
        })
    }

    fn segment(text: &str) -> SpeechSegment {
        SpeechSegment {
            text: text.into(),
            emotion: None,
            style: None,
            pace: None,
            voice_mode: None,
            emphasis: None,
        }
    }

    fn opts() -> SegmentSynthOptions {
        SegmentSynthOptions {
            voice: Some("alloy".into()),
            model: Some("primary-model".into()),
            rate: None,
            format: None,
            message_id: Some("msg-1".into()),
            timeout: Duration::from_secs(5),
        }
    }

    #[tokio::test]
    async fn success_on_primary_returns_segment_envelope() {
        let chain = vec![provider("openai", Behaviour::Ok)];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 0, &opts()).await;
        match env {
            SegmentEnvelope::Segment {
                index,
                provider,
                fallback,
                attempts,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(provider, "openai");
                assert!(!fallback);
                assert_eq!(attempts, "openai");
            },
            other => panic!("expected Segment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn upstream_failure_rotates_to_fallback() {
        let chain = vec![
            provider("openai", Behaviour::Upstream(502)),
            provider("minimax", Behaviour::Ok),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 1, &opts()).await;
        match env {
            SegmentEnvelope::Segment {
                provider,
                fallback,
                attempts,
                ..
            } => {
                assert_eq!(provider, "minimax");
                assert!(fallback);
                assert_eq!(attempts, "openai,minimax");
            },
            other => panic!("expected fallback Segment, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn transport_failure_also_rotates() {
        let chain = vec![
            provider("openai", Behaviour::Transport),
            provider("minimax", Behaviour::Ok),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 0, &opts()).await;
        assert!(matches!(
            env,
            SegmentEnvelope::Segment { fallback: true, .. }
        ));
    }

    #[tokio::test]
    async fn all_providers_fail_emits_error_envelope() {
        let chain = vec![
            provider("openai", Behaviour::Upstream(502)),
            provider("minimax", Behaviour::Transport),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 2, &opts()).await;
        match env {
            SegmentEnvelope::Error {
                index,
                code,
                attempts,
                ..
            } => {
                assert_eq!(index, 2);
                // Last error was Transport, so the error code reflects that.
                assert_eq!(code, "tts_transport");
                assert_eq!(attempts, "openai,minimax");
            },
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn bad_request_short_circuits_chain() {
        // BadRequest is a request-shape problem, not a provider-health
        // problem — should NOT rotate. We assert the second provider
        // never ran by giving it a behaviour that would clearly differ
        // (success) and confirming we get the BadRequest error anyway.
        let chain = vec![
            provider("openai", Behaviour::BadRequest),
            provider("minimax", Behaviour::Ok),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 0, &opts()).await;
        match env {
            SegmentEnvelope::Error { code, attempts, .. } => {
                assert_eq!(code, "tts_bad_request");
                assert_eq!(attempts, "openai", "fallback must not have been tried");
            },
            other => panic!("expected BadRequest Error, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn timeout_rotates_to_next_provider() {
        let mut opts = opts();
        opts.timeout = Duration::from_millis(50);
        let chain = vec![
            provider("openai", Behaviour::SleepForever),
            provider("minimax", Behaviour::Ok),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 0, &opts).await;
        assert!(matches!(
            env,
            SegmentEnvelope::Segment { fallback: true, .. }
        ));
    }

    #[tokio::test]
    async fn timeout_on_every_provider_emits_transport_error() {
        let mut opts = opts();
        opts.timeout = Duration::from_millis(50);
        let chain = vec![
            provider("openai", Behaviour::SleepForever),
            provider("minimax", Behaviour::SleepForever),
        ];
        let env = synthesize_segment_via_chain(&chain, &segment("hi"), 0, &opts).await;
        match env {
            SegmentEnvelope::Error { code, .. } => {
                assert_eq!(code, "tts_transport");
            },
            other => panic!("expected Transport Error, got {other:?}"),
        }
    }
}

#[cfg(test)]
mod surface_admission_tests {
    use super::*;

    fn authenticated_scope(
        workspace: &str,
        now: chrono::DateTime<chrono::Utc>,
    ) -> magician::magician_v2::apps::authority::AuthenticatedAppScope {
        use magician::magician_v2::apps::{
            authority::AuthenticatedAppScope,
            models::{AppReference, AppRevision, AppScopeBindingRef},
            records::AppScope,
        };
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: AppReference::parse("anonymous").unwrap(),
                workspace: AppReference::parse(workspace).unwrap(),
            },
            AppScopeBindingRef::parse(format!("scope_anonymous_{workspace}")).unwrap(),
            AppReference::parse("actor:owner").unwrap(),
            AppReference::parse("session:voice-test").unwrap(),
            AppRevision::new(1).unwrap(),
            now - chrono::Duration::seconds(1),
            now + chrono::Duration::seconds(30),
        )
        .unwrap()
    }

    /// The widening rule. An unverified caller may narrow onto the room but may
    /// not claim to be a first-party owner surface — that claim is what would
    /// put the personal assistant in front of it.
    #[test]
    fn an_unverified_caller_cannot_claim_an_owner_surface() {
        for claimed in [
            SurfaceType::TrayMacos,
            SurfaceType::WebDesktop,
            SurfaceType::WebMobile,
            SurfaceType::MascotMacos,
            SurfaceType::EspTerminal,
        ] {
            assert_eq!(
                admitted_surface_type(Some(claimed), false),
                SurfaceType::Unknown,
                "{claimed:?} was admitted for an unverified caller"
            );
        }
    }

    /// Narrowing is always permitted, verified or not: it can only restrict the
    /// caller, so refusing it would buy nothing and break the meeting bot.
    #[test]
    fn narrowing_onto_the_room_is_always_admitted() {
        assert_eq!(
            admitted_surface_type(Some(SurfaceType::MeetingBot), false),
            SurfaceType::MeetingBot
        );
        assert_eq!(
            admitted_surface_type(Some(SurfaceType::MeetingBot), true),
            SurfaceType::MeetingBot
        );
    }

    /// A verified caller keeps the surface it asked for — this is the
    /// regression half: closing the hole must not move real first-party clients.
    #[test]
    fn a_verified_caller_keeps_its_declared_surface() {
        for claimed in [
            SurfaceType::TrayMacos,
            SurfaceType::WebDesktop,
            SurfaceType::EspTerminal,
        ] {
            assert_eq!(admitted_surface_type(Some(claimed), true), claimed);
        }
    }

    /// Saying nothing is not a way to obtain an owner surface either way.
    #[test]
    fn an_omitted_surface_stays_unknown() {
        assert_eq!(admitted_surface_type(None, true), SurfaceType::Unknown);
        assert_eq!(admitted_surface_type(None, false), SurfaceType::Unknown);
    }

    #[test]
    fn voice_owner_credential_is_limited_to_identified_owner_surfaces_and_absent_from_wire() {
        for surface in [
            SurfaceType::TrayMacos,
            SurfaceType::WebDesktop,
            SurfaceType::MascotLinux,
            SurfaceType::EspTerminal,
        ] {
            assert!(is_identified_owner_voice_surface(surface));
        }
        for surface in [
            SurfaceType::MeetingBot,
            SurfaceType::Extension,
            SurfaceType::Unknown,
        ] {
            assert!(!is_identified_owner_voice_surface(surface));
        }

        let request_shape = include_str!("media_api.rs")
            .split_once("pub struct RegisterSessionRequest")
            .expect("registration request")
            .1
            .split_once("pub struct ListSessionsQuery")
            .expect("next request shape")
            .0;
        for forbidden in ["credential", "authority", "provider_trust", "owner_token"] {
            assert!(
                !request_shape.contains(forbidden),
                "media registration wire unexpectedly exposes `{forbidden}`"
            );
        }
    }

    #[test]
    fn voice_owner_registry_is_single_move_scope_bound_and_expires_closed() {
        let now = chrono::Utc::now();
        let broadcaster =
            Arc::new(magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(8));
        let api = MediaApi::new(Arc::new(RealtimeSessionRegistry::new(broadcaster)));
        let authenticated = authenticated_scope("default", now);
        let credential = Arc::new(
            magician::magician_v2::apps::boundary::AppRealtimeVoiceOwnerSessionCredential::from_authenticated_session(
                authenticated.clone(),
                "voice-1",
                "primary",
                now,
            )
            .unwrap(),
        );
        api.voice_owner_correlations
            .insert("voice-1".to_owned(), "server-correlation-1".to_owned());
        api.voice_owner_credentials
            .insert("server-correlation-1".to_owned(), Arc::clone(&credential));

        let crossed_scope = authenticated_scope("other", now);
        assert!(api
            .take_voice_owner_credential("voice-1", &crossed_scope, now)
            .is_none());
        let moved = api
            .take_voice_owner_credential("voice-1", &authenticated, now)
            .expect("exact authenticated session consumes the credential");
        assert!(api
            .take_voice_owner_credential("voice-1", &authenticated, now)
            .is_none());
        moved.invalidate();
        assert!(!moved.matches_authenticated_scope(&authenticated, "voice-1", now));

        let expiring = Arc::new(
            magician::magician_v2::apps::boundary::AppRealtimeVoiceOwnerSessionCredential::from_authenticated_session(
                authenticated.clone(),
                "voice-2",
                "primary",
                now,
            )
            .unwrap(),
        );
        api.voice_owner_correlations
            .insert("voice-2".to_owned(), "server-correlation-2".to_owned());
        api.voice_owner_credentials
            .insert("server-correlation-2".to_owned(), Arc::clone(&expiring));
        assert!(api
            .take_voice_owner_credential(
                "voice-2",
                &authenticated,
                now + chrono::Duration::seconds(31),
            )
            .is_none());
        assert!(!expiring.is_live_at(now));
        assert!(!api.voice_owner_correlations.contains_key("voice-2"));
        assert!(!api
            .voice_owner_credentials
            .contains_key("server-correlation-2"));
    }
}

//! Chat API — HTTP endpoints for chat mode.
//!
//! Follows existing API patterns from the scoped V3 web APIs.

//!
//! Routes:
//! ```text
//! GET    /api/magician/v2/chat/active                              -> get or create active thread session
//! POST   /api/magician/v2/chat/new                                 -> archive current thread session, create fresh
//! GET    /api/magician/v2/chat/sessions                            -> list all sessions (history)
//! GET    /api/magician/v2/chat/sessions/{id}                       -> get session with messages
//! POST   /api/magician/v2/chat/sessions/{id}/results/read          -> authenticated complete-result page
//! GET    /api/magician/v2/chat/sessions/{id}/reference-catalog     -> composer @ references for session agent/delegates
//! PATCH  /api/magician/v2/chat/sessions/{id}                       -> update session title
//! POST   /api/magician/v2/chat/sessions/{id}/messages              -> send message (active only)
//! POST   /api/magician/v2/chat/sessions/{id}/actions/invoke        -> invoke structured-response server action
//! POST   /api/magician/v2/chat/sessions/{id}/messages/stream       -> send message with SSE streaming
//! POST   /api/magician/v2/chat/sessions/{id}/transcript            -> append display-only meeting-transcript line (never dispatches the agent)
//! DELETE /api/magician/v2/chat/sessions/{id}/messages/{message_id} -> delete one stored message
//! DELETE /api/magician/v2/chat/sessions/{id}/messages              -> clear ALL messages in session (session preserved)
//! DELETE /api/magician/v2/chat/sessions/{id}/run                   -> cancel in-flight chat turn (Phase 1)
//! POST   /api/magician/v2/chat/sessions/{id}/tutor/cancel          -> cancel active tutor run (also works after session-store loss)
//! GET    /api/magician/v2/chat/sessions/{id}/queue                 -> list pending-replay messages (Phase 2)
//! DELETE /api/magician/v2/chat/sessions/{id}/queue/{message_id}    -> delete one queued message (Phase 2)
//! DELETE /api/magician/v2/chat/sessions/{id}/queue                 -> clear pending-replay queue (Phase 2)
//! ```

mod concurrent_voice;
mod queue_actions;
pub use queue_actions::*;
pub use concurrent_voice::*;

use std::{path::Path, process::Stdio, sync::Arc};

use actix_multipart::Multipart;
use actix_web::{http::header, web, HttpMessage, HttpRequest, HttpResponse, Responder};
use chrono::Utc;
use futures_util::stream::StreamExt;
use magicllm::prelude::StreamDelta;
use serde::{Deserialize, Serialize};
use tokio::{process::Command, sync::mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, warn};

const MAX_ACTION_REF_CHARS: usize = 160;
const MAX_SERVER_ACTION_LOOKUP_PAGE_SIZE: usize = 100;
const MAX_SERVER_ACTION_LOOKUP_PAGES: usize = 150;

use crate::scope::{resolve_optional_principal, resolve_required_workspace};
use magician::config::EnvoyConfig;
use magician::magician_v2::counterparties::{IdentityKind, InboundVerification};
use magician::magician_v2::{
    chat::{
        chat_turn_event_sink::ChatTurnEventSink,
        enrollment::EnrollmentStoreResolver,
        envoy,
        inbound_sender::{identify_inbound_sender_in_process, InboundSender},
        models::{
            ChatChannel, ChatMessageMode, ChatResponse, ChatSession, ContentFileSource,
            ScreenCaptureAttachmentContext,
        },
        presentation::StructuredResponseActionV1,
        public_contact_profile::{IdentityResearchStatus, PublicContactProfile},
        service::{extract_chat_bad_request_message, ChatService},
        storage::ChatSessionPageQuery,
    },
    history::HistoryLane,
    tutor::{tutor_run_store, TutorRunScope, TutorUserActionEvent},
};

// ========================================================================
// Request/Response Types
// ========================================================================

/// Query parameters for the active session endpoint.
#[derive(Debug, Deserialize)]
pub struct ActiveSessionQuery {
    /// Workspace scope for the session.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Stable UI thread scope (defaults to "general")
    #[serde(default = "magician::magician_v2::storage::task_models::default_ui_thread_id")]
    pub ui_thread_id: String,
    /// Origin channel (defaults to Web). Consumer channels pass their channel
    /// type so sessions record where they originated from.
    /// Format: "web", "telegram", "discord", "whatsapp", "imessage"
    #[serde(default)]
    pub channel: Option<String>,
    /// Channel-specific address (e.g., Telegram chat_id, Discord channel_id).
    /// Required when channel is not "web".
    #[serde(default)]
    pub channel_address: Option<String>,
    /// Whether the transport authenticated the sender. The agentmail adapter
    /// sends `false` for mail carrying AgentMail's `unauthenticated` label (a
    /// spoofable `from`); omitted/`None` ⇒ treated as verified, so kapso/web
    /// and any caller that doesn't set it keep today's owner routing.
    #[serde(default)]
    pub channel_verified: Option<bool>,
    /// Whether the sender explicitly invoked an owner-control surface (for
    /// example a Kapso message prefixed with `@magic`). `false`
    /// suppresses owner allowlist routing and treats the message as an ordinary
    /// guest/envoy conversation; `true` still requires the normal owner checks.
    /// Omitted preserves legacy routing.
    #[serde(default)]
    pub control_intent: Option<bool>,
    /// What kind of address `channel_address` is — `email`, `phone`,
    /// `handle` (written `platform:handle`) or `domain` — for the counterparty
    /// register.
    ///
    /// The kind is part of every derived id, so it travels with the value
    /// rather than being guessed from the channel's name: a table that mapped a
    /// channel to a bare handle is one entry from resolving one platform's
    /// `@acme` to another's. Omitted is fail-closed — the sender is identified
    /// only where the transport itself fixes the address shape (see
    /// `magician::magician_v2::chat::inbound_sender::kind_fixed_by_channel`),
    /// and otherwise nobody is looked up and nothing is written.
    #[serde(default)]
    pub channel_address_kind: Option<String>,
    /// Explicit provenance for product-owned internal callers. Ordinary
    /// clients omit this and remain in personal history.
    #[serde(default)]
    pub history_lane: Option<String>,
}

/// Query parameters for paginated message loading.
#[derive(Debug, Deserialize)]
pub struct MessagesQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub channel_address: Option<String>,
    /// Max messages to return (default 50).
    #[serde(default = "default_message_limit")]
    pub limit: usize,
    /// Load messages before this message ID (cursor-based pagination).
    /// Omit for the most recent messages.
    #[serde(default)]
    pub before: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ReadChatResultRequest {
    pub result_ref: String,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub field_paths: Vec<String>,
    #[serde(default = "default_result_page_records")]
    pub max_records: usize,
}

fn chat_result_error_status(
    error: &magician::magician_v2::tool_result_materialization::CanonicalResultError,
) -> (actix_web::http::StatusCode, &'static str) {
    use magician::magician_v2::tool_result_materialization::CanonicalResultError;

    match error {
        CanonicalResultError::NotFound => {
            (actix_web::http::StatusCode::NOT_FOUND, "result_not_found")
        },
        CanonicalResultError::Revoked => (actix_web::http::StatusCode::FORBIDDEN, "result_revoked"),
        CanonicalResultError::Expired | CanonicalResultError::CursorExpired => {
            (actix_web::http::StatusCode::GONE, "result_expired")
        },
        CanonicalResultError::RecordTooLarge => (
            actix_web::http::StatusCode::PAYLOAD_TOO_LARGE,
            "result_record_too_large",
        ),
        CanonicalResultError::InvalidCursor | CanonicalResultError::InvalidRequest { .. } => (
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_result_request",
        ),
        CanonicalResultError::Corrupt | CanonicalResultError::IdentityConflict => {
            (actix_web::http::StatusCode::CONFLICT, "result_corrupt")
        },
        CanonicalResultError::AuthorityUnavailable | CanonicalResultError::StorageUnavailable => (
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
            "result_temporarily_unavailable",
        ),
    }
}

#[cfg(test)]
mod chat_result_api_contract_tests {
    use super::*;

    use magician::magician_v2::tool_result_materialization::CanonicalResultError;

    #[test]
    fn complete_result_failures_have_stable_non_disclosing_http_statuses() {
        for (error, expected_status, expected_code) in [
            (
                CanonicalResultError::NotFound,
                actix_web::http::StatusCode::NOT_FOUND,
                "result_not_found",
            ),
            (
                CanonicalResultError::Revoked,
                actix_web::http::StatusCode::FORBIDDEN,
                "result_revoked",
            ),
            (
                CanonicalResultError::Expired,
                actix_web::http::StatusCode::GONE,
                "result_expired",
            ),
            (
                CanonicalResultError::InvalidCursor,
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_result_request",
            ),
            (
                CanonicalResultError::Corrupt,
                actix_web::http::StatusCode::CONFLICT,
                "result_corrupt",
            ),
            (
                CanonicalResultError::AuthorityUnavailable,
                actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                "result_temporarily_unavailable",
            ),
        ] {
            assert_eq!(
                chat_result_error_status(&error),
                (expected_status, expected_code)
            );
        }
    }
}

fn default_result_page_records() -> usize {
    20
}

fn default_message_limit() -> usize {
    50
}

fn default_public_contact_profile_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
pub struct PublicContactProfilesQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub research_status: Option<String>,
    #[serde(default)]
    pub owner_review_priority: Option<bool>,
    #[serde(default = "default_public_contact_profile_limit")]
    pub limit: usize,
}

#[derive(Debug, Serialize, Default)]
pub struct PublicContactResearchCounts {
    pub not_eligible: usize,
    pub eligible: usize,
    pub queued: usize,
    pub active: usize,
    pub completed: usize,
    pub failed: usize,
    pub skipped: usize,
    pub pending: usize,
}

#[derive(Debug, Serialize, Default)]
pub struct PublicContactProfileCounts {
    pub total: usize,
    pub owner_review_priority: usize,
    pub missing_identity: usize,
    pub missing_purpose: usize,
    pub research: PublicContactResearchCounts,
}

#[derive(Debug, Serialize)]
pub struct PublicContactProfileListResponse {
    pub profiles: Vec<PublicContactProfile>,
    pub count: usize,
    pub total_matching: usize,
    pub total_profiles: usize,
    pub counts: PublicContactProfileCounts,
}

const DEFAULT_ALLOWED_ORIGINS: &[&str] = &["localhost", "127.0.0.1", "[::1]"];

fn strip_to_hostname(value: &str) -> &str {
    let without_proto = value
        .find("://")
        .map(|idx| &value[idx + 3..])
        .unwrap_or(value);

    if let Some(bracket_end) = without_proto.rfind(']') {
        if let Some(colon) = without_proto[bracket_end..].rfind(':') {
            &without_proto[..bracket_end + colon]
        } else {
            without_proto
        }
    } else if without_proto
        .as_bytes()
        .iter()
        .filter(|&&b| b == b':')
        .count()
        > 1
    {
        without_proto
    } else if let Some(colon) = without_proto.find(':') {
        &without_proto[..colon]
    } else {
        without_proto
    }
}

pub(crate) fn validate_browser_origin(req: &HttpRequest) -> Result<(), HttpResponse> {
    let origin = match req.headers().get("origin") {
        None => return Ok(()),
        Some(value) => value.to_str().map_err(|_| {
            HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Origin not allowed",
                "origin": "(non-utf8 origin)",
                "host": "",
            }))
        })?,
    };

    let origin_hostname = strip_to_hostname(origin);
    if DEFAULT_ALLOWED_ORIGINS
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(origin_hostname))
    {
        return Ok(());
    }

    let request_host = req
        .headers()
        .get("host")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !request_host.is_empty() {
        let host_hostname = strip_to_hostname(request_host);
        if origin_hostname.eq_ignore_ascii_case(host_hostname) {
            return Ok(());
        }
    }

    Err(HttpResponse::Forbidden().json(serde_json::json!({
        "error": "Origin not allowed",
        "origin": origin,
        "host": if request_host.is_empty() { "(no host)" } else { request_host },
    })))
}

fn process_message_error_response(error: &anyhow::Error) -> HttpResponse {
    if let Some(message) = extract_chat_bad_request_message(error) {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": message
        }));
    }

    HttpResponse::InternalServerError().json(serde_json::json!({
        "error": "Failed to process message",
        "details": error.to_string()
    }))
}

/// Query parameters for listing sessions.
#[derive(Debug, Deserialize)]
pub struct ListSessionsQuery {
    /// Workspace scope for the listed sessions.
    #[serde(default)]
    pub workspace: Option<String>,
    /// Optional thread filter for listing sessions.
    #[serde(default)]
    pub ui_thread_id: Option<String>,
    /// Origin channel type for enrolled clients.
    #[serde(default)]
    pub channel: Option<String>,
    /// Channel-specific address for enrolled clients.
    #[serde(default)]
    pub channel_address: Option<String>,
    /// Personal (default/user-created) or product-generated history.
    #[serde(default)]
    pub history_lane: Option<String>,
    /// Case-insensitive title, thread, agent, or session-id search.
    #[serde(default)]
    pub q: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub offset: Option<usize>,
}

/// Request body for sending a chat message.
#[derive(Debug, Deserialize)]
pub struct SendMessageRequest {
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub mode: ChatMessageMode,
    #[serde(default)]
    pub plan_task_id: Option<String>,
    #[serde(default)]
    pub plan_question_id: Option<String>,
    /// Optional LLM profile override. Must be a valid chat-eligible profile
    /// name (as returned by `GET /chat/profiles`).
    #[serde(default)]
    pub profile: Option<String>,
    /// Per-turn composer harness. Absent uses the Settings chat engine.
    #[serde(default)]
    pub harness_engine: Option<String>,
    /// Harness model, or `default` for its own selection. Pi uses `profile`.
    #[serde(default)]
    pub harness_model: Option<String>,
    /// Optional client-supplied turn ID. When set, every backend event
    /// emitted while processing this request (chat-side LLM calls, tool
    /// calls, delegated/handover spawns, sub-agent inner-loop emissions)
    /// is tagged with this ID in its payload. The UI subscribes to
    /// `/events?chat_turn_id=...` and gets exactly this request's stream,
    /// no blind catch-all. When absent, the server allocates one and
    /// returns it on the streaming Done payload.
    #[serde(default)]
    pub chat_turn_id: Option<String>,
    /// Surface that originated this turn (`web`, `mobile`, `mascot`,
    /// `voice`, `screen`, ...). This is metadata on the regular chat
    /// ledger, not a separate message type.
    #[serde(default)]
    pub source_surface: Option<String>,
    /// Media/control session that produced this turn, when available.
    #[serde(default)]
    pub presence_session_id: Option<String>,
    /// Optional channel-provided sender display name. Used only for
    /// public-contact enrichment; regular chat messages remain unchanged.
    #[serde(default)]
    pub sender_display_name: Option<String>,
    /// True when the user message originated as a voice transcript
    /// (mic → STT → composer → send). Causes the chat runtime to
    /// append a system-prompt fragment instructing the model to
    /// wrap audible parts of the reply in `<speech>…</speech>` tags
    /// so the frontend TTS can read only the conversational summary
    /// instead of the full text body.
    #[serde(default)]
    pub voice_origin: bool,
    /// Keep an accepted streaming turn running when this HTTP response is
    /// disconnected. Mobile processes and radios disappear routinely; their
    /// durable transcript and realtime subscription recover the completed turn
    /// after reconnect. Ordinary callers retain cancel-on-disconnect unless
    /// they opt in explicitly.
    #[serde(default)]
    pub continue_on_disconnect: bool,
    /// Explicit VibeDev coding choice for a `@vibedev` turn. Omitted
    /// keeps the configured Pi default. The rail never reads an engine
    /// name out of the transcript.
    #[serde(default)]
    pub coding_choice:
        Option<magician::magician_v2::vibedev::run_service::ClientVibeDevCodingChoice>,
}

fn rejects_client_selected_protected_surface(source_surface: Option<&str>) -> bool {
    source_surface.map(str::trim).is_some_and(|surface| {
        surface.eq_ignore_ascii_case("thinking_map")
            || surface.eq_ignore_ascii_case("authenticated_realtime_voice")
            || surface.eq_ignore_ascii_case("plane")
    })
}

/// Request body for updating a session.
#[derive(Debug, Deserialize)]
pub struct UpdateSessionRequest {
    pub title: Option<String>,
    /// Set to "archived" to archive, "active" to restore.
    pub status: Option<String>,
}

/// Response wrapping a chat session.
#[derive(Debug, Serialize)]
pub struct ChatSessionResponse {
    pub session: ChatSession,
}

/// Response wrapping a list of chat sessions.
#[derive(Debug, Serialize)]
pub struct ChatSessionListResponse {
    pub sessions: Vec<ChatSession>,
    pub total: usize,
    pub limit: usize,
    pub offset: usize,
}

/// Response for session detail including messages.
#[derive(Debug, Serialize)]
pub struct ChatSessionDetailResponse {
    pub session: ChatSession,
    pub messages: Vec<magician::magician_v2::chat::models::ChatMessage>,
}

/// Response returned after uploading a staged attachment.
#[derive(Debug, Serialize)]
pub struct AttachmentUploadResponse {
    pub attachment_id: String,
    pub filename: String,
    pub mime_type: String,
    pub size: u64,
}

fn parse_screen_capture_context(bytes: &[u8]) -> Result<ScreenCaptureAttachmentContext, String> {
    let mut context: ScreenCaptureAttachmentContext = serde_json::from_slice(bytes)
        .map_err(|error| format!("Invalid screen_capture JSON: {error}"))?;
    // This generic upload surface accepts display/coordinate hints but never
    // mints product authority. Only server-owned capture paths set this true.
    context.server_registered = false;
    if context.mode != "screenshot" && context.mode != "region" && context.mode != "clip" {
        return Err("screen_capture.mode must be screenshot, region, or clip".to_string());
    }
    if context.coordinate_space.trim().is_empty() {
        return Err("screen_capture.coordinate_space is required".to_string());
    }
    if let Some(size) = context.image_size.as_ref() {
        if size.width == 0 || size.height == 0 {
            return Err("screen_capture.image_size dimensions must be positive".to_string());
        }
    }
    Ok(context)
}

fn validate_uploaded_screen_capture_image(
    context: &ScreenCaptureAttachmentContext,
    mime_type: &str,
    bytes: &[u8],
) -> Result<(), String> {
    if mime_type != "image/png" {
        return Err(
            "screen_capture metadata requires an image/png attachment with verifiable dimensions"
                .to_string(),
        );
    }
    let Some((width, height)) = png_image_size(bytes) else {
        return Err("screen_capture attachment is not a valid dimensioned PNG".to_string());
    };
    let Some(declared) = context.image_size.as_ref() else {
        return Err("screen_capture.image_size is required for uploaded metadata".to_string());
    };
    if declared.width != width || declared.height != height {
        return Err(format!(
            "screen_capture.image_size does not match uploaded PNG dimensions ({width}x{height})"
        ));
    }
    Ok(())
}

fn png_image_size(bytes: &[u8]) -> Option<(u32, u32)> {
    if bytes.len() < 24 || &bytes[..8] != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let width = u32::from_be_bytes(bytes.get(16..20)?.try_into().ok()?);
    let height = u32::from_be_bytes(bytes.get(20..24)?.try_into().ok()?);
    (width > 0 && height > 0).then_some((width, height))
}

/// Request body for opening the containing folder of a rendered output file.
#[derive(Debug, Deserialize)]
pub struct OpenOutputFolderRequest {
    #[serde(default)]
    pub source: Option<ContentFileSource>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub absolute_path: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct OpenOutputFolderResponse {
    pub folder_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
}

/// Response for the `open-file` endpoint that opens a rendered output
/// in the OS-default application (Preview for images, browser for HTML,
/// etc.).
#[derive(Debug, Serialize)]
pub struct OpenOutputFileResponse {
    pub file_path: String,
}

/// Parse a `ChatChannel` from query parameters.
/// Defaults to `ChatChannel::web()` when no channel is specified.
/// Any channel type string is accepted — no hardcoded enum matching.
fn parse_channel(channel: Option<&str>, channel_address: Option<&str>) -> ChatChannel {
    match channel.map(str::trim).filter(|value| !value.is_empty()) {
        Some(channel_type) => ChatChannel {
            channel_type: channel_type.to_string(),
            address: channel_address
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
        },
        None => ChatChannel::web(),
    }
}

fn requested_ui_thread_id_for_inbound(
    query: &ActiveSessionQuery,
    channel_type: &str,
    channel_address: &str,
) -> String {
    let requested = query.ui_thread_id.trim();
    let default_thread = magician::magician_v2::storage::task_models::default_ui_thread_id();
    if query.control_intent == Some(true)
        && !channel_type.eq_ignore_ascii_case("web")
        && !channel_address.is_empty()
        && (requested.is_empty() || requested == default_thread.as_str())
    {
        format!(
            "{}:magic",
            envoy::guest_thread_id(channel_type, channel_address)
        )
    } else {
        query.ui_thread_id.clone()
    }
}

/// Whether the outer boundary proved this request.
///
/// Presence of `VerifiedRequestIdentity` is the fact: the middleware inserts it
/// only after Cloudflare Access, a paired device, or an actual loopback peer is
/// verified, and it cannot be deserialized from a payload. Absence means the
/// request arrived unauthenticated, which for ingress purposes is exactly what
/// we need to know.
pub(crate) fn request_is_authenticated(req: &HttpRequest) -> bool {
    req.extensions()
        .get::<magician::magician_v2::cloudflare_access::VerifiedRequestIdentity>()
        .is_some()
}

/// Mint the non-transport owner capability consumed by guarded app tools.
/// Unauthenticated chat remains available, but receives no capability and its
/// catalog therefore excludes the owner-only app family.
fn app_owner_execution_credential_for_request(
    req: &HttpRequest,
    session: &ChatSession,
) -> Result<
    Option<Arc<magician::magician_v2::apps::boundary::AppOwnerExecutionCredential>>,
    HttpResponse,
> {
    use magician::magician_v2::apps::{
        boundary::VerifiedAppTransportSession,
        models::{AppDigest, AppReference, AppRevision, AppScopeBindingRef},
        records::AppScope,
    };
    use magician::magician_v2::cloudflare_access::{
        VerifiedRequestAuthentication, VerifiedRequestIdentity,
    };

    let Some(identity) = req.extensions().get::<VerifiedRequestIdentity>().cloned() else {
        return Ok(None);
    };
    if identity.principal() != session.principal.as_str()
        || identity
            .workspace()
            .is_some_and(|workspace| workspace != session.workspace.as_str())
    {
        return Err(HttpResponse::Forbidden().json(serde_json::json!({
            "error": "The chat session does not match the authenticated owner scope."
        })));
    }
    let scope = AppScope {
        principal: AppReference::parse(session.principal.clone()).map_err(|_| {
            HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "The authenticated owner scope is invalid."
            }))
        })?,
        workspace: AppReference::parse(session.workspace.clone()).map_err(|_| {
            HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "The authenticated owner scope is invalid."
            }))
        })?,
    };
    let scope_digest =
        AppDigest::blake3(format!("{}\0{}", scope.principal, scope.workspace).as_bytes());
    let scope_binding_ref = AppScopeBindingRef::parse(format!(
        "scope_{}",
        scope_digest.as_str().trim_start_matches("blake3:")
    ))
    .map_err(|_| {
        HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "The authenticated owner scope is invalid."
        }))
    })?;
    let actor_ref = AppReference::parse(format!("actor:{}", identity.actor_fingerprint()))
        .map_err(|_| {
            HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "The authenticated owner identity is invalid."
            }))
        })?;
    let transport_session_ref =
        AppReference::parse(format!("session:{}", identity.session_fingerprint())).map_err(
            |_| {
                HttpResponse::Unauthorized().json(serde_json::json!({
                    "error": "The authenticated owner identity is invalid."
                }))
            },
        )?;
    let authentication_revision =
        AppRevision::new(identity.authentication_revision()).map_err(|_| {
            HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "The authenticated owner identity is invalid."
            }))
        })?;
    let now = Utc::now();
    let issued_at = identity.verified_at();
    if now < issued_at || now.to_owned() - issued_at.to_owned() > chrono::Duration::seconds(5) {
        return Err(HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "The authenticated request identity is stale."
        })));
    }
    let expires_at = now.to_owned() + chrono::Duration::minutes(5);
    let transport = match identity.authentication() {
        VerifiedRequestAuthentication::TrustedLoopbackSingleUser => {
            let peer_ip = req.peer_addr().map(|address| address.ip()).ok_or_else(|| {
                HttpResponse::Unauthorized().json(serde_json::json!({
                    "error": "The trusted local request has no verified loopback peer."
                }))
            })?;
            VerifiedAppTransportSession::from_trusted_loopback(
                peer_ip,
                true,
                scope.clone(),
                scope_binding_ref,
                actor_ref,
                transport_session_ref,
                authentication_revision,
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
                transport_session_ref,
                authentication_revision,
                issued_at,
                expires_at,
            )
        },
    }
    .and_then(|transport| transport.bind_request(Some(&scope), &now))
    .map_err(|_| {
        HttpResponse::Unauthorized().json(serde_json::json!({
            "error": "The authenticated owner credential is unavailable."
        }))
    })?;
    let credential =
        magician::magician_v2::apps::boundary::AppOwnerExecutionCredential::from_authenticated_chat(
            transport,
            session.id.clone(),
            session.agent_id.clone(),
            now,
        )
        .map_err(|_| {
            HttpResponse::Unauthorized().json(serde_json::json!({
                "error": "The authenticated owner credential is unavailable."
            }))
        })?;
    Ok(Some(Arc::new(credential)))
}

/// Identify the inbound sender against the counterparty register, and say so in
/// the log.
///
/// **This is the register's only automatic writer.** An identified address has
/// its `last_seen` advanced by one `observe` line — the one write that cannot
/// touch verification, provenance or `first_seen`, because hearing from an
/// address a hundred times is a hundred repetitions of the same unauthenticated
/// claim.
///
/// **It confers nothing.** `InboundSender::authority()` answers `Some` for
/// exactly one state and this function returns the whole enum, so routing that
/// wants to act on a proved sender has to choose that arm explicitly — see
/// `magician::magician_v2::chat::inbound_authority`, the join that turns an
/// authoritative sender into an engagement lane. Nothing here widens a thread,
/// picks an agent or reaches anything a counterparty owns.
///
/// `request_authenticated` is the server-owned fact, exactly as
/// [`route_inbound_query`] uses it; `channel_verified` may de-escalate and may
/// never raise.
fn identify_inbound_sender_for_query(
    principal: &str,
    workspace: &str,
    query: &ActiveSessionQuery,
    request_authenticated: bool,
) -> InboundSender {
    let channel = normalized_query_value(query.channel.as_deref()).unwrap_or("web");
    let address = normalized_query_value(query.channel_address.as_deref()).unwrap_or("");
    let named_kind = query
        .channel_address_kind
        .as_deref()
        .and_then(IdentityKind::parse);
    let verified = InboundVerification::from_boundary(request_authenticated)
        .with_caller_claim(query.channel_verified);
    match identify_inbound_sender_in_process(
        principal,
        workspace,
        channel,
        address,
        named_kind,
        verified,
        Utc::now(),
    ) {
        Ok(sender) => {
            if !matches!(sender, InboundSender::NoAddress) {
                debug!(
                    "[CHAT-API] inbound sender on {channel}: {} (authority={})",
                    sender.reason(),
                    sender.authority().is_some()
                );
            }
            sender
        },
        // An unreadable register is a failure, not a stranger. It is logged as
        // itself and answers with no authority; folding it into "unrecognised"
        // would put a broken process in the log as a healthy one.
        Err(error) => {
            warn!("[CHAT-API] the counterparty register could not be read: {error}");
            InboundSender::RegisterUnavailable
        },
    }
}

/// `engagement` is the lane the caller already resolved, or `None`.
///
/// It is an argument rather than something resolved here because resolving it
/// needs two stores and an `await`, and because the caller has already read the
/// register once — asking twice for one message could get two answers. `None`
/// covers every reason there is no lane (stranger, unproved sender, no live
/// engagement, ambiguous, unreadable) and they all degrade to the guest lane,
/// which is why routing needs no arm for any of them.
/// Which engagement, if any, an identified inbound sender reaches — and, in the
/// log, why not when it does not.
///
/// The reason is logged HERE rather than left to the caller because the two
/// interesting outcomes are operational facts an owner can act on:
/// `no_owner_named_agent` is one owner decision away from a working lane, and
/// `ambiguous_engagement` says two live engagements name one counterparty. Both
/// route identically to a stranger's message, so without this line they are
/// indistinguishable from one.
async fn inbound_engagement_outcome(
    principal: &str,
    workspace: &str,
    sender: &InboundSender,
) -> magician::magician_v2::chat::inbound_authority::InboundEngagementOutcome {
    let outcome = magician::magician_v2::chat::inbound_authority::engagement_lane_in_process(
        principal,
        workspace,
        sender,
        Utc::now(),
    )
    .await;
    // Silent for the overwhelmingly common cases — a stranger, and a channel
    // with nothing to identify. Those are not events.
    let noisy = !matches!(
        outcome,
        magician::magician_v2::chat::inbound_authority::InboundEngagementOutcome::NotReached(
            magician::magician_v2::chat::inbound_authority::NoEngagementLane::SenderNotAuthoritative
        )
    );
    if noisy {
        debug!("[CHAT-API] inbound engagement lane: {}", outcome.reason());
    }
    outcome
}

fn route_inbound_query(
    envoy_config: Option<&EnvoyConfig>,
    query: &ActiveSessionQuery,
    // Whether the OUTER boundary proved this request. Server-owned: the
    // middleware attaches a `VerifiedRequestIdentity` only after Cloudflare
    // Access, a paired device, or a real loopback peer. Passed in rather than
    // read from the query, which is the entire point of §9 step 6.
    request_authenticated: bool,
    engagement: Option<magician::magician_v2::chat::envoy::EngagementLane>,
) -> (String, Option<String>) {
    match envoy_config {
        Some(envoy_cfg) => {
            let channel_type = normalized_query_value(query.channel.as_deref()).unwrap_or("web");
            let channel_address =
                normalized_query_value(query.channel_address.as_deref()).unwrap_or("");
            // Server-derived, never caller-asserted. This previously read
            // `query.channel_verified.unwrap_or(true)`, so any caller reaching
            // the endpoint was verified by default and an allowlisted address
            // got OWNER routing. A caller may still de-escalate (agentmail's
            // `unauthenticated` label); it may not escalate.
            let channel_verified = envoy::channel_is_verified(
                channel_type,
                request_authenticated,
                query.channel_verified,
            );
            let requested_ui_thread_id =
                requested_ui_thread_id_for_inbound(query, channel_type, channel_address);
            // `resolve_inbound_lane`, not `route_for_inbound`: the latter is
            // the same call with `engagement: None` hardcoded, so going through
            // it made `InboundLane::Engagement` unreachable no matter what the
            // register proved. This is the Phase 3 forwarding switch's only
            // live seam.
            //
            // Still a no-op until `engagement_forwarding_enabled` is on:
            // `into_routing` projects an engagement lane to its guest fallback
            // while the flag is off. What changes here is that the lane is now
            // RESOLVED, so the flag has something to switch.
            envoy::resolve_inbound_lane(
                envoy_cfg,
                channel_type,
                channel_address,
                channel_verified,
                query.control_intent,
                &requested_ui_thread_id,
                engagement,
            )
            .into_routing(envoy_cfg)
        },
        None => (query.ui_thread_id.clone(), None),
    }
}

// ========================================================================
// Shared ChatApi Data
// ========================================================================

/// Shared state for chat API handlers.
///
/// `ChatService` is wrapped in `Arc` so the streaming endpoint can clone
/// a reference into a spawned task.
pub struct ChatApi {
    pub chat_service: Arc<ChatService>,
    enrollment_store_resolver: Option<EnrollmentStoreResolver>,
    /// Envoy routing config: classifies inbound senders as owner vs guest and
    /// names the envoy agent guests are bound to. `None` ⇒ envoy routing off
    /// (every caller keeps its requested thread + normal agent selection).
    envoy_config: Option<EnvoyConfig>,
}

#[derive(Deserialize)]
pub struct EnvoyDeliveryRequest {
    pub phase: magician::magician_v2::chat::envoy_claims::DeliveryPhase,
    #[serde(default)]
    pub binding: Option<magician::magician_v2::chat::envoy_claims::DeliveryBinding>,
}

/// Only the channel's runtime-minted bot bearer can acknowledge its outbound
/// reply. Neither text from the conversation nor an owner display name is a
/// delivery receipt. Scope, channel and recipient come from the stored session.
pub async fn envoy_delivery_handler(
    api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    body: web::Json<EnvoyDeliveryRequest>,
) -> HttpResponse {
    use magician::magician_v2::auth::{middleware::authenticated, BearerKind};
    let Some(identity) = authenticated(&req) else {
        return HttpResponse::Unauthorized().finish();
    };
    let BearerKind::Bot { bot_name } = &identity.bearer else {
        return HttpResponse::Forbidden().finish();
    };
    let (session_id, message_id) = path.into_inner();
    if message_id.len() > 256 {
        return HttpResponse::BadRequest().finish();
    }
    let session = match api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => return HttpResponse::NotFound().finish(),
        Err(_) => return HttpResponse::ServiceUnavailable().finish(),
    };
    if session.principal != identity.scope.principal()
        || session.workspace != identity.scope.workspace()
        || session.origin_channel.channel_type != *bot_name
    {
        return HttpResponse::Forbidden().finish();
    }
    let envoy_id = api
        .envoy_config
        .as_ref()
        .map(|config| config.envoy_agent_id.as_str());
    let is_envoy = envoy_id == Some(session.agent_id.as_str())
        && magician::magician_v2::chat::envoy_claims::channel_for_session(&session).is_some();
    let Some(layout) = api.chat_service.workspace_ref().cloned() else {
        return if is_envoy {
            HttpResponse::ServiceUnavailable().finish()
        } else {
            HttpResponse::Ok()
                .json(serde_json::json!({"tracked":false,"send":true,"status":"not_envoy"}))
        };
    };
    let body = body.into_inner();
    match web::block(move || -> anyhow::Result<_> {
        use magician::magician_v2::chat::envoy_claims::{
            has_prepared_reply, report_delivery, DeliveryGrant,
        };
        // Previously prepared messages stay tracked even after an Envoy config
        // change. A changed session binding is refused inside report_delivery.
        if !is_envoy && !has_prepared_reply(&layout, &session, &message_id)? {
            return Ok(DeliveryGrant {
                tracked: false,
                send: true,
                status: "not_envoy".into(),
            });
        }
        let binding = body.binding.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Envoy delivery binding is required; update the channel SDK")
        })?;
        report_delivery(&layout, &session, &message_id, body.phase, binding)
    })
    .await
    {
        Ok(Ok(grant)) => HttpResponse::Ok().json(grant),
        Ok(Err(error)) => {
            HttpResponse::Conflict().json(serde_json::json!({"error":error.to_string()}))
        },
        Err(_) => HttpResponse::ServiceUnavailable().finish(),
    }
}

impl ChatApi {
    pub fn new(chat_service: ChatService) -> Self {
        let chat_service = Arc::new(chat_service);
        ChatService::bind_self_handle(&chat_service);
        Self {
            chat_service,
            enrollment_store_resolver: None,
            envoy_config: None,
        }
    }

    pub fn with_enrollment_store_resolver(
        mut self,
        enrollment_store_resolver: EnrollmentStoreResolver,
    ) -> Self {
        self.enrollment_store_resolver = Some(enrollment_store_resolver);
        self
    }

    pub fn with_envoy_config(mut self, envoy_config: EnvoyConfig) -> Self {
        self.envoy_config = Some(envoy_config);
        self
    }
}

fn session_matches_scope(session: &ChatSession, principal: &str, workspace: &str) -> bool {
    session.principal == principal && session.workspace == workspace
}

fn normalized_query_value(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalized_optional_string(value: Option<&str>) -> Option<String> {
    normalized_query_value(value).map(ToString::to_string)
}

fn channel_query_error(
    channel: Option<&str>,
    channel_address: Option<&str>,
) -> Option<HttpResponse> {
    match (
        normalized_query_value(channel),
        normalized_query_value(channel_address),
    ) {
        (Some(_), None) => Some(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "channel_address is required when channel is provided"
        }))),
        (None, Some(_)) => Some(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "channel is required when channel_address is provided"
        }))),
        _ => None,
    }
}

pub(crate) async fn resolve_principal(
    chat_api: &ChatApi,
    workspace: &str,
    requested_principal: Option<&str>,
    channel: Option<&str>,
    channel_address: Option<&str>,
) -> Result<String, HttpResponse> {
    if let Some(response) = channel_query_error(channel, channel_address) {
        return Err(response);
    }

    let requested_principal = normalized_query_value(requested_principal);
    let channel = normalized_query_value(channel);
    let channel_address = normalized_query_value(channel_address);

    if let (Some(channel), Some(channel_address), Some(store_resolver)) = (
        channel,
        channel_address,
        chat_api.enrollment_store_resolver.as_ref(),
    ) {
        let enrolled_principal = match store_resolver
            .resolve_enrolled_principal(workspace, channel, channel_address)
            .await
        {
            Ok(Some(principal)) => principal,
            Ok(None) => {
                return Err(HttpResponse::Forbidden().json(serde_json::json!({
                    "error": "Channel identity is not enrolled",
                    "channel": channel,
                    "channel_address": channel_address
                })));
            },
            Err(_) => {
                return Err(HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to resolve enrollment store"
                })));
            },
        };

        if let Some(requested_principal) = requested_principal {
            if requested_principal != enrolled_principal {
                return Err(HttpResponse::Forbidden().json(serde_json::json!({
                    "error": "Principal does not match enrolled channel identity",
                    "channel": channel,
                    "channel_address": channel_address
                })));
            }
        }

        return Ok(enrolled_principal);
    }

    match requested_principal {
        Some(principal) => Ok(principal.to_string()),
        None => Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "missing_scope",
            "message": "A bearer with an embedded principal scope is required."
        }))),
    }
}

fn validate_send_message_request(body: &SendMessageRequest) -> Result<Option<&str>, HttpResponse> {
    let trimmed_text = body
        .text
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty());
    if trimmed_text.is_none() && body.attachment_ids.is_empty() {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Message text or attachment_ids is required"
        })));
    }

    const MAX_MESSAGE_BYTES: usize = 16_384; // 16KB
    if let Some(text) = trimmed_text {
        if text.len() > MAX_MESSAGE_BYTES {
            return Err(HttpResponse::PayloadTooLarge().json(serde_json::json!({
                "error": format!("Message too long. Maximum {} bytes allowed.", MAX_MESSAGE_BYTES)
            })));
        }
    }

    if body.mode == ChatMessageMode::Plan && trimmed_text.is_none() {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Plan mode requires message text"
        })));
    }

    if body.mode != ChatMessageMode::Plan
        && (body
            .plan_task_id
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
            || body
                .plan_question_id
                .as_deref()
                .map(str::trim)
                .is_some_and(|value| !value.is_empty()))
    {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "plan_task_id and plan_question_id require plan mode"
        })));
    }

    if body
        .plan_question_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        && body
            .plan_task_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
    {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "plan_question_id requires plan_task_id"
        })));
    }

    Ok(trimmed_text)
}

fn validate_profile_override(
    chat_api: &ChatApi,
    body: &SendMessageRequest,
) -> Option<HttpResponse> {
    let Some(profile_name) = body.profile.as_deref() else {
        return None;
    };

    let profiles = chat_api.chat_service.list_chat_profiles();
    if profiles.iter().any(|p| p.name == profile_name) {
        return None;
    }

    if chat_api
        .chat_service
        .is_public_chat_source_surface(body.source_surface.as_deref())
    {
        warn!(
            profile = %profile_name,
            source_surface = %body.source_surface.as_deref().unwrap_or("<none>"),
            "[CHAT-API] public chat profile is not configured; deferring to service fallback"
        );
        return None;
    }

    Some(HttpResponse::BadRequest().json(serde_json::json!({
        "error": format!("Profile '{}' is not a valid chat profile", profile_name)
    })))
}

fn routed_profile_override(body: &SendMessageRequest) -> Result<Option<String>, HttpResponse> {
    let Some(engine) = body.harness_engine.as_deref() else {
        return Ok(body.profile.clone());
    };
    let installed = engine == "magician"
        || magician::magician_v2::execution::plane::roster_with_install_status()
            .iter()
            .any(|(name, available)| *name == engine && *available);
    if !installed {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": format!("Chat harness '{engine}' is not installed")
        })));
    }
    if body
        .harness_model
        .as_deref()
        .is_some_and(|model| model.len() > 128 || model.chars().any(char::is_control))
    {
        return Err(HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Chat harness model is invalid"
        })));
    }
    Ok(Some(
        magician::magician_v2::execution::plane::encode_chat_harness_choice(
            &magician::magician_v2::execution::plane::ChatHarnessChoice {
                engine: engine.to_string(),
                model: body
                    .harness_model
                    .as_deref()
                    .filter(|m| !m.is_empty())
                    .unwrap_or("default")
                    .to_string(),
                profile: body.profile.clone(),
            },
        ),
    ))
}

// ========================================================================
// Handlers
// ========================================================================

/// GET /api/magician/v2/chat/active
///
/// Get or create the active chat session for the current principal + thread.
pub async fn get_active_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let requested_history_lane = match query.history_lane.as_deref() {
        Some(value) => match HistoryLane::parse_filter(value) {
            Some(lane) => lane,
            None => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "history_lane must be 'personal' or 'automated'"
                }));
            },
        },
        None => HistoryLane::Personal,
    };
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    // The counterparty register's inbound seam. Identifies the sender and
    // advances `last_seen`. It grants nothing on its own —
    // `InboundSender::authority()` is the one arm anything may act on, and the
    // lane resolver below re-checks it rather than trusting that this ran.
    //
    // Ordered BEFORE routing, which is the change: routing now consumes the
    // engagement lane, and a lane can only be minted from an identification.
    let inbound_sender = identify_inbound_sender_for_query(
        &principal,
        &workspace,
        &query,
        request_is_authenticated(&req),
    );
    let engagement_outcome =
        inbound_engagement_outcome(&principal, &workspace, &inbound_sender).await;

    // Envoy routing: classify the inbound sender as owner vs guest and, for
    // guests, override the thread (per-sender) and bind the envoy agent for a
    // NEW session. Owners (incl. web) keep their requested thread + normal
    // agent selection. No envoy config ⇒ routing is a no-op.
    let (effective_ui_thread_id, force_agent_id) = route_inbound_query(
        chat_api.envoy_config.as_ref(),
        &query,
        request_is_authenticated(&req),
        engagement_outcome.into_lane(),
    );

    debug!(
        "[CHAT-API] GET /chat/active principal={} workspace={} thread={} (requested={}) \
         force_agent={:?}",
        principal, workspace, effective_ui_thread_id, query.ui_thread_id, force_agent_id
    );

    let origin = parse_channel(query.channel.as_deref(), query.channel_address.as_deref());
    let session_result =
        if force_agent_id.is_some() || requested_history_lane == HistoryLane::Automated {
            chat_api
                .chat_service
                .get_or_create_automated_session(
                    &principal,
                    &workspace,
                    &effective_ui_thread_id,
                    &origin,
                    force_agent_id.as_deref(),
                )
                .await
        } else {
            chat_api
                .chat_service
                .get_or_create_session(
                    &principal,
                    &workspace,
                    &effective_ui_thread_id,
                    &origin,
                    None,
                )
                .await
        };
    let session = match session_result {
        Ok(s) => s,
        Err(e) => {
            error!("[CHAT-API] Failed to get/create active session: {}", e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to get or create active session",
                "details": e.to_string()
            }));
        },
    };

    // Also load messages so the frontend can display prior conversation
    let messages = match chat_api
        .chat_service
        .chat_store_ref()
        .get_messages(&session.id, 200)
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            error!(
                "[CHAT-API] Failed to get messages for active session {}: {}",
                session.id, e
            );
            Vec::new()
        },
    };
    let messages = chat_api
        .chat_service
        .reconcile_terminal_task_status_messages(&session, messages)
        .await;

    HttpResponse::Ok().json(ChatSessionDetailResponse { session, messages })
}

/// POST /api/magician/v2/chat/new
///
/// Archive the current active thread session and create a fresh one.
pub async fn new_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let requested_history_lane = match query.history_lane.as_deref() {
        Some(value) => match HistoryLane::parse_filter(value) {
            Some(lane) => lane,
            None => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "history_lane must be 'personal' or 'automated'"
                }));
            },
        },
        None => HistoryLane::Personal,
    };
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    // Same seam as `/chat/active`, in the same order and for the same reason.
    let inbound_sender = identify_inbound_sender_for_query(
        &principal,
        &workspace,
        &query,
        request_is_authenticated(&req),
    );
    let engagement_outcome =
        inbound_engagement_outcome(&principal, &workspace, &inbound_sender).await;

    let (effective_ui_thread_id, force_agent_id) = route_inbound_query(
        chat_api.envoy_config.as_ref(),
        &query,
        request_is_authenticated(&req),
        engagement_outcome.into_lane(),
    );

    debug!(
        "[CHAT-API] POST /chat/new principal={} workspace={} thread={} (requested={}) \
         force_agent={:?}",
        principal, workspace, effective_ui_thread_id, query.ui_thread_id, force_agent_id
    );

    let origin = parse_channel(query.channel.as_deref(), query.channel_address.as_deref());
    let session_result =
        if force_agent_id.is_some() || requested_history_lane == HistoryLane::Automated {
            chat_api
                .chat_service
                .new_automated_session_with_agent_override(
                    &principal,
                    &workspace,
                    &effective_ui_thread_id,
                    &origin,
                    force_agent_id.as_deref(),
                )
                .await
        } else {
            chat_api
                .chat_service
                .new_session_with_agent_override(
                    &principal,
                    &workspace,
                    &effective_ui_thread_id,
                    &origin,
                    None,
                )
                .await
        };
    match session_result {
        Ok(session) => HttpResponse::Ok().json(ChatSessionResponse { session }),
        Err(e) => {
            error!("[CHAT-API] Failed to create new session: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to create new session",
                "details": e.to_string()
            }))
        },
    }
}

/// GET /api/magician/v2/chat/sessions
///
/// List all sessions for a principal (active + archived), ordered by updated_at
/// desc.
pub async fn list_sessions_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ListSessionsQuery>,
) -> impl Responder {
    let history_lane = match query.history_lane.as_deref() {
        Some(value) => match HistoryLane::parse_filter(value) {
            Some(lane) => Some(lane),
            None => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "history_lane must be 'personal' or 'automated'"
                }));
            },
        },
        None => None,
    };
    let search = query.q.as_deref().unwrap_or("").trim();
    if search.chars().count() > 120 {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "q must be at most 120 characters"
        }));
    }
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!(
        "[CHAT-API] GET /chat/sessions principal={} workspace={}",
        principal, workspace
    );

    let paged = query.limit.is_some()
        || query.offset.is_some()
        || history_lane.is_some()
        || !search.is_empty();
    if paged {
        let limit = query.limit.unwrap_or(20).clamp(1, 100);
        let offset = query.offset.unwrap_or(0);
        return match chat_api
            .chat_service
            .list_sessions_page(
                &principal,
                &workspace,
                ChatSessionPageQuery {
                    ui_thread_id: query
                        .ui_thread_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                        .map(ToOwned::to_owned),
                    history_lane,
                    search: search.to_string(),
                    limit,
                    offset,
                },
            )
            .await
        {
            Ok(page) => HttpResponse::Ok().json(ChatSessionListResponse {
                sessions: page.sessions,
                total: page.total,
                limit: page.limit,
                offset: page.offset,
            }),
            Err(e) => {
                error!("[CHAT-API] Failed to list sessions: {}", e);
                HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": "Failed to list sessions",
                    "details": e.to_string()
                }))
            },
        };
    }

    match chat_api
        .chat_service
        .list_sessions(&principal, &workspace)
        .await
    {
        Ok(sessions) => {
            let sessions = if let Some(ui_thread_id) = query
                .ui_thread_id
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                sessions
                    .into_iter()
                    .filter(|session| session.ui_thread_id == ui_thread_id)
                    .collect()
            } else {
                sessions
            };
            let total = sessions.len();
            HttpResponse::Ok().json(ChatSessionListResponse {
                sessions,
                total,
                limit: total,
                offset: 0,
            })
        },
        Err(e) => {
            error!("[CHAT-API] Failed to list sessions: {}", e);
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to list sessions",
                "details": e.to_string()
            }))
        },
    }
}

/// GET /api/magician/v2/chat/sessions/{id}
///
/// Get a session with its messages.
pub async fn get_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!("[CHAT-API] GET /chat/sessions/{}", session_id);

    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!("[CHAT-API] Failed to get session {}: {}", session_id, e);
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to get session",
                "details": e.to_string()
            }));
        },
    };

    // Scope check
    if !session_matches_scope(&session, &principal, &workspace) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Access denied: session belongs to a different principal or workspace"
        }));
    }

    // Also load messages
    let messages = match chat_api
        .chat_service
        .chat_store_ref()
        .get_messages(&session_id, 200)
        .await
    {
        Ok(msgs) => msgs,
        Err(e) => {
            error!(
                "[CHAT-API] Failed to get messages for session {}: {}",
                session_id, e
            );
            Vec::new()
        },
    };
    let messages = chat_api
        .chat_service
        .reconcile_terminal_task_status_messages(&session, messages)
        .await;

    HttpResponse::Ok().json(ChatSessionDetailResponse { session, messages })
}

/// POST /api/magician/v2/chat/sessions/{id}/results/read
///
/// Authenticated Web/iOS display and download continuation for a canonical
/// tool result. The opaque reference is only a locator; session scope, current
/// agent/tool/action authority, retention, cursor binding and content hash are
/// all rechecked by the service.
pub async fn read_chat_result_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<ReadChatResultRequest>,
) -> impl Responder {
    use magician::magician_v2::tool_result_materialization::{
        RawResultReadRequest, ScopedResultRef, DEFAULT_RESULT_PAGE_BYTES,
    };

    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if session_matches_scope(&session, &principal, &workspace) => session,
        Ok(Some(_)) => {
            // Result reads collapse foreign-session existence to the same
            // response as an unknown reference. The opaque locator is never a
            // bearer credential or a cross-scope existence oracle.
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "result_not_found"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "result_not_found"
            }));
        },
        Err(_) => {
            return HttpResponse::ServiceUnavailable().json(serde_json::json!({
                "error": "result_storage_unavailable"
            }));
        },
    };
    let content_ref = match ScopedResultRef::parse(body.result_ref.trim()) {
        Ok(content_ref) => content_ref,
        Err(_) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "result_not_found"
            }));
        },
    };
    let read_request = RawResultReadRequest {
        content_ref,
        cursor: body.cursor.clone(),
        field_paths: body.field_paths.clone(),
        max_records: body.max_records,
        max_serialized_bytes: DEFAULT_RESULT_PAGE_BYTES,
    };
    match chat_api
        .chat_service
        .read_chat_result_page(&session, &read_request)
        .await
    {
        Ok(page) => HttpResponse::Ok().json(
            magician::magician_v2::tool_result_materialization::lossless_read_success_payload(page),
        ),
        Err(error) => {
            let (status, code) = chat_result_error_status(&error);
            HttpResponse::build(status).json(serde_json::json!({
                "error": code
            }))
        },
    }
}

/// GET /api/magician/v2/chat/sessions/{id}/reference-catalog
///
/// Return the composer `@` picker catalog scoped to the session's agent:
/// direct skills/tools first, then skills/tools owned by delegate targets.
pub async fn get_reference_catalog_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(error) => {
            error!(
                "[CHAT-API] Failed to get reference catalog session {}: {}",
                session_id, error
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to get session",
                "details": error.to_string()
            }));
        },
    };

    if !session_matches_scope(&session, &principal, &workspace) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Access denied: session belongs to a different principal or workspace"
        }));
    }

    match chat_api
        .chat_service
        .composer_reference_catalog(&session)
        .await
    {
        Ok(catalog) => HttpResponse::Ok().json(catalog),
        Err(error) => {
            error!(
                "[CHAT-API] Failed to build reference catalog for session {}: {}",
                session.id, error
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to build reference catalog",
                "details": error.to_string()
            }))
        },
    }
}

/// PATCH /api/magician/v2/chat/sessions/{id}
///
/// Update session title.
pub async fn update_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<UpdateSessionRequest>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!("[CHAT-API] PATCH /chat/sessions/{}", session_id);

    // Scope check. PATCH is for metadata updates (title, status, thread);
    // it MUST work on archived sessions because that's how unarchive is
    // performed (PATCH status=active). The previous archived-session
    // guard here was a copy-paste from the send-message endpoint and
    // made unarchive impossible.
    let scoped_session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) => session,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to verify session {} ownership: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session ownership",
                "details": e.to_string()
            }));
        },
    };

    let has_title = body.title.as_deref().is_some_and(|t| !t.trim().is_empty());
    let has_status = body.status.as_deref().is_some_and(|s| !s.trim().is_empty());

    if !has_title && !has_status {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "At least one of 'title' or 'status' is required"
        }));
    }

    if scoped_session.is_default_session
        && body
            .status
            .as_deref()
            .is_some_and(|status| status.trim().eq_ignore_ascii_case("archived"))
    {
        return HttpResponse::Conflict().json(serde_json::json!({
            "error": "The default #general session cannot be archived"
        }));
    }

    if let Some(title) = body.title.as_deref().filter(|t| !t.trim().is_empty()) {
        let title = title.trim();
        const MAX_TITLE_BYTES: usize = 256;
        if title.len() > MAX_TITLE_BYTES {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("Title too long. Maximum {} bytes allowed.", MAX_TITLE_BYTES)
            }));
        }
        if let Err(e) = chat_api
            .chat_service
            .chat_store_ref()
            .update_session_title(&session_id, title)
            .await
        {
            error!(
                "[CHAT-API] Failed to update session {} title: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to update session", "details": e.to_string()
            }));
        }
    }

    if let Some(status) = body.status.as_deref().filter(|s| !s.trim().is_empty()) {
        let status = status.trim().to_lowercase();
        if status != "archived" && status != "active" {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "status must be 'archived' or 'active'"
            }));
        }
        if let Err(e) = chat_api
            .chat_service
            .chat_store_ref()
            .update_session_status(&session_id, &status)
            .await
        {
            error!(
                "[CHAT-API] Failed to update session {} status: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to update session status", "details": e.to_string()
            }));
        }
    }

    // Return updated session
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) => HttpResponse::Ok().json(ChatSessionResponse { session }),
        Ok(None) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Session not found",
            "session_id": session_id
        })),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to get updated session",
            "details": e.to_string()
        })),
    }
}

/// DELETE /api/magician/v2/chat/sessions/{id}
///
/// Permanently delete a session and all its messages.
pub async fn delete_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!("[CHAT-API] DELETE /chat/sessions/{}", session_id);

    // Scope check
    let scoped_session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) if session.is_default_session => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "The default #general session cannot be deleted"
            }));
        },
        Ok(Some(session)) => Some(session),
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to verify session {} ownership: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session ownership",
                "details": e.to_string()
            }));
        },
    };

    // A tutor run is in-memory and can outlive a failed or interrupted chat
    // persistence path. Cancel it before dropping the session so a delegated
    // UI action cannot continue after its chat owner has been removed.
    if let Err(error) = cancel_tutor_run_for_scope(
        chat_api.chat_service.as_ref(),
        &principal,
        &workspace,
        &session_id,
        "Chat session deleted by user.".to_string(),
    )
    .await
    {
        warn!(
            session_id = %session_id,
            error = %error,
            "[CHAT-API] failed to cancel tutor run while deleting chat session"
        );
    }

    // Phase 3 — Delete Internal tasks spawned by this chat session
    // (matched by chat_session_id). Best-effort; per-task delete
    // errors are logged inside `cleanup_ephemeral_tasks_for_session`.
    // Skipped if `get_session` returned `None` (shouldn't happen
    // given the scope-check above, but guarded for safety).
    if let Some(ref session) = scoped_session {
        chat_api
            .chat_service
            .cleanup_ephemeral_tasks_for_session(session)
            .await;
    }

    // Phase 3.x — Drop in-memory ChatService state (active run,
    // pending queue, tailed-task slot + its fanout) BEFORE the
    // chat-store delete. Idempotent; the handler reads no state from
    // it, so order beyond "in-memory first, persisted second"
    // doesn't matter — keeping it before delete avoids racing the
    // delete completion against a concurrent tail-watcher poll.
    chat_api.chat_service.clear_chat_session_state(&session_id);

    if let Some(session) = scoped_session.as_ref() {
        match chat_api
            .chat_service
            .cleanup_chat_result_owner(session)
            .await
        {
            Ok(report) => debug!(
                session_id = %session_id,
                removed = report.removed,
                corrupt_manifests = report.corrupt_manifests,
                failed = report.failed,
                "[CHAT-API] cleaned canonical tool results for deleted session"
            ),
            Err(error) => warn!(
                session_id = %session_id,
                error = %error,
                "[CHAT-API] canonical tool-result cleanup failed before session deletion"
            ),
        }
    }

    if let Err(e) = chat_api
        .chat_service
        .chat_store_ref()
        .delete_session(&session_id)
        .await
    {
        error!("[CHAT-API] Failed to delete session {}: {}", session_id, e);
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "Failed to delete session",
            "details": e.to_string()
        }));
    }

    HttpResponse::Ok().json(serde_json::json!({ "deleted": true, "session_id": session_id }))
}

/// GET /api/magician/v2/chat/sessions/{id}/messages
///
/// Get paginated messages for a session. Supports cursor-based pagination
/// via `before` (message ID) and `limit` query params.
pub async fn get_messages_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    // Scope check
    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found"
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        Ok(Some(session)) => session,
    };

    let limit = query.limit.min(200).max(1);
    let before = query.before.as_deref();

    match chat_api
        .chat_service
        .chat_store_ref()
        .get_messages_paginated(&session_id, limit, before)
        .await
    {
        Ok((messages, has_more)) => {
            let messages = chat_api
                .chat_service
                .reconcile_terminal_task_status_messages(&session, messages)
                .await;
            HttpResponse::Ok().json(serde_json::json!({
                "messages": messages,
                "has_more": has_more,
            }))
        },
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to load messages: {}", e)
        })),
    }
}

/// DELETE /api/magician/v2/chat/sessions/{id}/messages/{message_id}
///
/// Permanently remove one display message from a session.
pub async fn delete_message_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let (session_id, message_id) = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }

    match chat_api
        .chat_service
        .chat_store_ref()
        .delete_message(&session_id, &message_id)
        .await
    {
        Ok(true) => HttpResponse::Ok().json(serde_json::json!({
            "deleted": true,
            "session_id": session_id,
            "message_id": message_id
        })),
        Ok(false) => HttpResponse::NotFound().json(serde_json::json!({
            "error": "Message not found",
            "session_id": session_id,
            "message_id": message_id
        })),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to delete message: {}", e)
        })),
    }
}

/// DELETE /api/magician/v2/chat/sessions/{id}/messages
///
/// Clear every display message in the session, leaving the session itself
/// intact. Returns the number of messages cleared.
pub async fn clear_messages_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let scoped_session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(Some(session)) => Some(session),
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
    };

    // Phase 3 — Delete Internal tasks spawned by this chat session
    // (matched by chat_session_id). Same hook as delete_session;
    // matches the "clear is delete-of-content; persistent tasks
    // survive" semantics already used by the chat-store outputs +
    // chat-turn-events cleanup.
    if let Some(ref session) = scoped_session {
        chat_api
            .chat_service
            .cleanup_ephemeral_tasks_for_session(session)
            .await;
    }

    // Phase 3.x — Same in-memory state cleanup as delete_session; the
    // user cleared every message, so the active run / queue / tailed
    // task slot all belong to a session that's now empty.
    chat_api.chat_service.clear_chat_session_state(&session_id);

    match chat_api
        .chat_service
        .chat_store_ref()
        .clear_messages(&session_id)
        .await
    {
        Ok(cleared) => HttpResponse::Ok().json(serde_json::json!({
            "cleared": cleared,
            "session_id": session_id,
        })),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to clear messages: {}", e)
        })),
    }
}

// ────────────────────────────────────────────────────────────────────────
// Per-chat-turn activity events
// ────────────────────────────────────────────────────────────────────────

/// `GET /api/magician/v2/chat/sessions/{session_id}/turns/{chat_turn_id}/
/// events`
///
/// Returns every activity event that fired during this chat turn —
/// LLM calls, tool calls, reasoning, delegate-status transitions, etc.
/// Backed by the `<scope>/ui/chat_turn_events/<chat_turn_id>.jsonl`
/// projection that `ChatTurnEventSink` writes to as events land on
/// the transport bus. The activity card consumes this endpoint on
/// refresh; SSE consumes the same file for live updates — so
/// refresh-view = live-view by construction (one source, no filter
/// drift).
///
/// Replaces the previous `/api/magician/v3/events/page?chat_turn_id=`
/// path which read the per-scope `events.jsonl` log directly and was
/// prone to pagination / ordering / dedupe drift.
/// Phase 3.5a follow-up — GET the task this chat session is currently
/// tailing (if any). Lets the frontend `TaskStatusUpdate` card decide
/// whether to render "Watch live →" (no active tail / different task)
/// vs "● Live + Stop watching" (this task is the active tail) on
/// page reload. Returns `{"task_id": "..." | null}`.
pub async fn get_tailed_task_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    // Scope check — same shape as the other per-session handlers.
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }
    let task_id = chat_api
        .chat_service
        .tailed_task_id_for_session(&session_id);
    HttpResponse::Ok().json(serde_json::json!({ "task_id": task_id }))
}

/// Phase 3.5a follow-up — set the single-slot tail (the
/// "Watch live →" button on a task card). Body: `{"task_id": "..."}`.
/// Wraps the same dispatcher the LLM `subscribe_to_task` tool calls.
/// Returns the dispatcher's `Value` envelope so the frontend can
/// react to success / `noop_terminal` / `already_subscribed` /
/// `error` exactly as the LLM does.
#[derive(serde::Deserialize)]
pub struct SubscribeToTaskBody {
    pub task_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TutorUserActionBody {
    #[serde(default)]
    pub storyboard_step_id: Option<String>,
    #[serde(default)]
    pub storyboard_step_label: Option<String>,
    #[serde(default)]
    pub target: Option<String>,
    #[serde(default)]
    pub evidence: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TutorCancelBody {
    /// Optional operator-facing reason retained with the failed tutor run.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct TutorCancellationResponse {
    accepted: bool,
    cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preempted_execution_id: Option<String>,
    preempt_cancelled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    preempt_cancel_error: Option<String>,
}

async fn cancel_tutor_run_for_scope(
    chat_service: &ChatService,
    principal: &str,
    workspace: &str,
    session_id: &str,
    reason: String,
) -> Result<TutorCancellationResponse, String> {
    let scope = TutorRunScope::new(principal, workspace, session_id);
    let Some(cancelled) = tutor_run_store().cancel_active_run(&scope, reason.clone())? else {
        return Ok(TutorCancellationResponse {
            accepted: true,
            cancelled: false,
            run_id: None,
            status: None,
            reason: Some("no active tutor run for session".to_string()),
            preempted_execution_id: None,
            preempt_cancelled: false,
            preempt_cancel_error: None,
        });
    };

    let mut preempt_cancelled = false;
    let mut preempt_cancel_error = None;
    if let Some(execution_id) = cancelled.preempted_execution_id.as_deref() {
        match chat_service
            .cancel_execution_for_scope(principal, workspace, execution_id)
            .await
        {
            Ok(cancelled) => preempt_cancelled = cancelled,
            Err(error) => preempt_cancel_error = Some(error),
        }
    }

    Ok(TutorCancellationResponse {
        accepted: true,
        cancelled: true,
        run_id: Some(cancelled.run.run_id),
        status: Some(cancelled.run.status.as_str().to_string()),
        reason: cancelled.run.terminal_reason.or(Some(reason)),
        preempted_execution_id: cancelled.preempted_execution_id,
        preempt_cancelled,
        preempt_cancel_error,
    })
}

pub async fn subscribe_to_tailed_task_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<SubscribeToTaskBody>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }
    let task_id = body.task_id.trim();
    if task_id.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "task_id required",
        }));
    }
    let result = chat_api
        .chat_service
        .subscribe_to_task_via_api(&session_id, task_id)
        .await;
    HttpResponse::Ok().json(result)
}

pub async fn post_tutor_user_action_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<TutorUserActionBody>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }

    let scope = TutorRunScope::new(principal.clone(), workspace.clone(), session_id.clone());
    let run = match tutor_run_store().active_run(&scope) {
        Ok(Some(run)) => run,
        Ok(None) => {
            return HttpResponse::Accepted().json(serde_json::json!({
                "accepted": false,
                "reason": "no active tutor run for session"
            }));
        },
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "accepted": false,
                "error": error
            }));
        },
    };

    let event = TutorUserActionEvent {
        run_id: run.run_id.clone(),
        storyboard_step_id: normalized_optional_string(body.storyboard_step_id.as_deref()),
        storyboard_step_label: normalized_optional_string(body.storyboard_step_label.as_deref()),
        target: normalized_optional_string(body.target.as_deref()),
        evidence: normalized_optional_string(body.evidence.as_deref()).unwrap_or_else(|| {
            "User clicked inside the active App Copilot highlighted region.".to_string()
        }),
        occurred_at_ms: Utc::now().timestamp_millis(),
    };
    match tutor_run_store().record_user_action_event_and_preempt_pending(event.clone()) {
        Ok(record) => {
            let mut preempt_cancelled = false;
            let mut preempt_cancel_error: Option<String> = None;
            if let Some(execution_id) = record.preempted_execution_id.as_deref() {
                match chat_api
                    .chat_service
                    .cancel_execution_for_scope(&principal, &workspace, execution_id)
                    .await
                {
                    Ok(cancelled) => {
                        preempt_cancelled = cancelled;
                    },
                    Err(error) => {
                        preempt_cancel_error = Some(error);
                    },
                }
            }
            if record.applied_to_pending_action && preempt_cancel_error.is_none() {
                if let Err(error) = chat_api
                    .chat_service
                    .finalize_copilot_user_preemption(&session_id, &record.event)
                    .await
                {
                    preempt_cancel_error = Some(error);
                }
            }
            HttpResponse::Ok().json(serde_json::json!({
            "accepted": true,
            "run_id": record.event.run_id,
            "storyboard_step_id": record.event.storyboard_step_id,
            "storyboard_step_label": record.event.storyboard_step_label,
            "target": record.event.target,
            "evidence": record.event.evidence,
            "occurred_at_ms": record.event.occurred_at_ms,
            "applied_to_pending_action": record.applied_to_pending_action,
            "preempted_execution_id": record.preempted_execution_id,
            "preempt_cancelled": preempt_cancelled,
            "preempt_cancel_error": preempt_cancel_error
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "accepted": false,
            "error": error
        })),
    }
}

/// POST /api/magician/v2/chat/sessions/{id}/tutor/cancel
///
/// Cancels the scoped in-memory tutor run. Unlike normal per-session handlers,
/// this intentionally does not require the chat row to still exist: it is the
/// recovery path for a run orphaned by a failed or interrupted persistence
/// operation. Scope resolution still enforces principal/workspace isolation.
pub async fn post_tutor_cancel_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: Option<web::Json<TutorCancelBody>>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    let reason = body
        .as_ref()
        .and_then(|body| normalized_optional_string(body.reason.as_deref()))
        .map(|reason| reason.chars().take(512).collect::<String>())
        .filter(|reason| !reason.is_empty())
        .unwrap_or_else(|| "Tutor run cancelled by user.".to_string());

    match cancel_tutor_run_for_scope(
        chat_api.chat_service.as_ref(),
        &principal,
        &workspace,
        &session_id,
        reason,
    )
    .await
    {
        Ok(response) => HttpResponse::Ok().json(response),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "accepted": false,
            "cancelled": false,
            "error": error,
        })),
    }
}

/// Phase 3.5a follow-up — release the single-slot tail (the
/// "Stop watching" button). Idempotent; returns the released task_id
/// or null. Drops the chat fan-out, clears the slot. The watched
/// task itself keeps running — only the chat's live attachment ends.
pub async fn delete_tailed_task_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }
    let released = chat_api.chat_service.release_tailed_task(&session_id).await;
    // v0.6.654 — explicit unsubscribe by the user removes the
    // task-watch reason for queueing. If no in-flight turn holds the
    // session and the queue has messages, drain them now. Without
    // this, messages queued while the user was watching the task
    // stayed invisible until the next manual send.
    //
    // Gate on `released.is_some()` so concurrent double-click
    // requests don't both spawn a drain — only the request that
    // actually flipped the slot triggers it. The losing request just
    // observes `released = None` (no-op) and returns.
    if released.is_some() && !chat_api.chat_service.has_active_chat_run(&session_id) {
        let depth = chat_api.chat_service.pending_messages_depth(&session_id);
        if depth > 0 {
            chat_api
                .chat_service
                .spawn_pending_queue_drain(session_id.clone());
        }
    }
    HttpResponse::Ok().json(serde_json::json!({ "released_task_id": released }))
}

pub async fn list_chat_turn_events_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let (session_id, chat_turn_id) = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };

    // Scope check the session before disclosing the per-turn file.
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id,
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }

    let Some(workspace_ref) = chat_api.chat_service.workspace_ref() else {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "artifact workspace not configured"
        }));
    };
    let path = workspace_ref.chat_turn_events_path(&principal, &workspace, &chat_turn_id);

    let raw = match read_chat_turn_events_tail(&path, CHAT_TURN_EVENTS_TAIL_MAX_BYTES).await {
        Ok(Some(raw)) => raw,
        Ok(None) => {
            return HttpResponse::Ok().json(serde_json::json!({
                "events": Vec::<serde_json::Value>::new(),
                "count": 0,
                "total": 0,
                "truncated": false,
                "chat_turn_id": chat_turn_id,
            }));
        },
        Err(err) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("read chat-turn events: {err}"),
            }));
        },
    };

    let (events, scanned) = newest_chat_turn_events(&raw.text, query.limit);
    HttpResponse::Ok().json(serde_json::json!({
        "count": events.len(),
        "events": events,
        // Rows in the scanned window, which equals the file's row count unless the
        // byte ceiling clipped it. `truncated` says which.
        "total": scanned,
        "truncated": raw.clipped_by_bytes || scanned > events.len(),
        "chat_turn_id": chat_turn_id,
    }))
}

/// Parse a JSONL window and keep the newest `limit` rows, returning them with
/// the number of rows the window held.
///
/// Newest, not first: this endpoint backs the activity card, and
/// `fetchEventsPage` in `chatTurnEventsStore.ts` documents its no-cursor call as
/// "the latest N". Rows are appended in emission order, so the tail is the tail.
///
/// `limit` is floored at 1. It used to be accepted and then ignored, so `limit=0`
/// meant "everything"; honouring it literally would answer such a request with an
/// empty list instead, which no caller can want.
fn newest_chat_turn_events(text: &str, limit: usize) -> (Vec<serde_json::Value>, usize) {
    let mut events = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            events.push(value);
        }
    }
    let scanned = events.len();
    let clipped = scanned.saturating_sub(limit.max(1));
    if clipped > 0 {
        events.drain(..clipped);
    }
    (events, scanned)
}

/// Byte ceiling on one `GET .../turns/{id}/events` read.
///
/// Per-turn projections are usually tiny (median ~4 KB) but voice and tutor turns
/// reach several MB with individual rows past 100 KB, and the handler used to read
/// and `serde_json`-parse all of it on the reactor for a single turn open. 1 MiB is
/// far more than `MessagesQuery::limit` rows of any ordinary turn while capping the
/// pathological ones.
const CHAT_TURN_EVENTS_TAIL_MAX_BYTES: u64 = 1024 * 1024;

struct ChatTurnEventsTail {
    text: String,
    clipped_by_bytes: bool,
}

/// Read at most the last `max_bytes` of a JSONL file, dropping the leading
/// partial row when the window starts mid-file. `Ok(None)` means "no file".
///
/// `ArtifactV2Workspace` is growing a `read_tail_path` for exactly this shape;
/// fold this into it once that lands. Kept local (and on `tokio::fs`, matching
/// what this handler already did with the path) so the fix doesn't wait on it.
async fn read_chat_turn_events_tail(
    path: &std::path::Path,
    max_bytes: u64,
) -> std::io::Result<Option<ChatTurnEventsTail>> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};

    let mut file = match tokio::fs::File::open(path).await {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err),
    };
    let len = file.metadata().await?.len();
    let start = len.saturating_sub(max_bytes);
    if start > 0 {
        file.seek(std::io::SeekFrom::Start(start)).await?;
    }

    let mut bytes = Vec::with_capacity(len.saturating_sub(start) as usize);
    // `take` as well as the seek: a turn still being written can grow between the
    // metadata read and this read, and the ceiling has to hold either way.
    file.take(max_bytes).read_to_end(&mut bytes).await?;
    let mut text = match String::from_utf8(bytes) {
        Ok(text) => text,
        // A window that opens mid-file can split a multi-byte character. That
        // damage is confined to the leading partial row, which is dropped below.
        Err(error) => String::from_utf8_lossy(error.as_bytes()).into_owned(),
    };
    if start > 0 {
        // The window almost certainly opened mid-row; that row is unparseable and
        // its bytes may not even be a whole UTF-8 sequence, so drop it outright.
        let first_row_end = text.find('\n').map_or(text.len(), |newline| newline + 1);
        text.replace_range(..first_row_end, "");
    }

    Ok(Some(ChatTurnEventsTail {
        text,
        clipped_by_bytes: start > 0,
    }))
}

/// `GET /api/magician/v2/chat/sessions/{session_id}/turns/{chat_turn_id}/
/// events/stream`
///
/// Live tail of the same per-turn projection that the REST list
/// handler returns. Subscribes to `ChatTurnEventSink::subscribe_live()`,
/// filters by `chat_turn_id`, and forwards each event as one NDJSON
/// line. The sink is the **sole filter applier** for "is this event
/// chat-turn-bound?"; this handler only does the cheap final demux of
/// "is the tuple's chat_turn_id equal to the one I'm watching?".
///
/// Because both this endpoint and `list_chat_turn_events_handler`
/// consume the sink's projection (file for backfill, broadcast for
/// live), live + refresh views agree by construction.
pub async fn stream_chat_turn_events_handler(
    chat_api: web::Data<ChatApi>,
    sink: web::Data<Arc<ChatTurnEventSink>>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let (session_id, chat_turn_id) = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };

    // Scope-check the session before opening the live stream.
    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id,
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        _ => {},
    }

    // Subscribe BEFORE reading the on-disk backfill so any event that
    // lands during the file read is captured in the receiver buffer
    // instead of falling into a gap between "REST snapshot" and
    // "subscribe". Backfill rows are deduped against the live tail via
    // a one-pass id set so re-delivery is idempotent for the UI.
    let mut live_rx = sink.subscribe_live();
    let backfill_path = chat_api
        .chat_service
        .workspace_ref()
        .map(|ws| ws.chat_turn_events_path(&principal, &workspace, &chat_turn_id));

    let (tx, rx) =
        mpsc::channel::<Result<actix_web::web::Bytes, std::io::Error>>(NDJSON_CHANNEL_CAPACITY);
    let watch_turn = chat_turn_id.clone();
    let watch_principal = principal.clone();
    let watch_workspace = workspace.clone();
    let live_tx = tx;

    // Race `live_rx.recv()` against `live_tx.closed()` so the task wakes
    // immediately when the client disconnects (otherwise an idle turn
    // would pin the subscription + sender + TCP socket fd forever).
    tokio::spawn(async move {
        // Phase 1 — backfill from the on-disk projection. Reuses the
        // same file the REST list endpoint reads, so refresh-view ==
        // live-view by construction. Best-effort: a missing file just
        // means "no events for this turn yet".
        let mut seen_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        if let Some(path) = backfill_path {
            match tokio::fs::read_to_string(&path).await {
                Ok(raw) => {
                    for line in raw.lines() {
                        let trimmed = line.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        if let Some(id) = extract_event_id(trimmed) {
                            seen_ids.insert(id);
                        }
                        let mut payload = trimmed.to_string();
                        payload.push('\n');
                        if live_tx
                            .send(Ok(actix_web::web::Bytes::from(payload)))
                            .await
                            .is_err()
                        {
                            return;
                        }
                    }
                },
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(err) => {
                    debug!(
                        error = %err,
                        "[chat-turn-events] backfill read failed; continuing with live tail only"
                    );
                },
            }
        }

        // Phase 2 — live tail. Demux by (chat_turn_id, principal,
        // workspace) so a chat_turn_id collision across scopes never
        // leaks events. The session-scope check above guarantees the
        // request is allowed to see this `(principal, workspace)`, but
        // re-validating per row defends against future broadcasts that
        // happen to share an id.
        loop {
            tokio::select! {
                event_result = live_rx.recv() => match event_result {
                    Ok((event_chat_turn_id, event_principal, event_workspace, serialized)) => {
                        if event_chat_turn_id != watch_turn
                            || event_principal != watch_principal
                            || event_workspace != watch_workspace
                        {
                            continue;
                        }
                        if let Some(id) = extract_event_id(&serialized) {
                            if !seen_ids.insert(id) {
                                // Already shipped during backfill; skip
                                // so the UI doesn't see duplicates.
                                continue;
                            }
                        }
                        let mut payload = serialized;
                        payload.push('\n');
                        if live_tx
                            .send(Ok(actix_web::web::Bytes::from(payload)))
                            .await
                            .is_err()
                        {
                            break;
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        debug!(
                            skipped,
                            "[chat-turn-events] live tail lagged; dropping events"
                        );
                        let sentinel = serde_json::json!({
                            "event_type": "__events_lagged__",
                            "chat_turn_id": watch_turn,
                            "skipped": skipped,
                        });
                        if let Ok(out) = serde_json::to_string(&sentinel) {
                            let mut payload = out;
                            payload.push('\n');
                            if live_tx
                                .send(Ok(actix_web::web::Bytes::from(payload)))
                                .await
                                .is_err()
                            {
                                break;
                            }
                        }
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                },
                _ = live_tx.closed() => break,
            }
        }
    });

    let stream = ReceiverStream::new(rx);
    HttpResponse::Ok()
        .content_type("application/x-ndjson")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(stream)
}

/// Pulls a stable per-event identifier out of a serialized
/// `RuntimeTransportEvent` so the SSE handler can dedupe between the
/// on-disk backfill and the live broadcast. Different variants stash
/// the id in different places, so we probe each known location:
///
///   * `AgentEvent` envelope: `data.event.payload.event_id`
///   * `ProgressEvent` envelope: `message.id` (the `ProgressMessage` struct
///     names its unique id `id`, not `event_id`)
///   * Other variants that set a top-level `event_id`
///
/// Returns `None` when no id is reachable — caller skips dedupe for
/// such events. The cost of missing a dedupe is a duplicate row in
/// the activity card; the cost of a stale dedupe (false positive)
/// would be a dropped row. So we err on the side of `None` and accept
/// rare duplicates over silent drops.
fn extract_event_id(serialized: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(serialized).ok()?;
    // `RuntimeTransportEvent` serializes with serde tag/content, so
    // every variant's body lives under `data.*`.
    let data = value.get("data");
    if let Some(serde_json::Value::String(id)) = data
        .and_then(|d| d.get("event"))
        .and_then(|e| e.get("payload"))
        .and_then(|p| p.get("event_id"))
    {
        if !id.is_empty() {
            return Some(id.clone());
        }
    }
    if let Some(serde_json::Value::String(id)) = data
        .and_then(|d| d.get("message"))
        .and_then(|m| m.get("id"))
    {
        if !id.is_empty() {
            return Some(id.clone());
        }
    }
    if let Some(serde_json::Value::String(id)) = data.and_then(|d| d.get("event_id")) {
        if !id.is_empty() {
            return Some(id.clone());
        }
    }
    None
}

/// Buffered NDJSON channel capacity for chat-turn live tails. Sized to
/// absorb a fast turn's event burst without blocking the broadcast
/// forwarder; small enough to bound memory if the client stalls.
const NDJSON_CHANNEL_CAPACITY: usize = 1024;

// ────────────────────────────────────────────────────────────────────────
// Chat-run control + pending-message queue (Phases 1 + 2)
// ────────────────────────────────────────────────────────────────────────

/// DELETE /api/magician/v2/chat/sessions/{id}/run
///
/// Cancel the in-flight chat turn and any scoped tutor run for this session.
/// Drops partial output (per implementation plan decision #1). Does NOT clear
/// the pending-message queue (decision #2) — next queued message drains as
/// soon as the cancelled turn settles. Both cancellations are idempotent.
pub async fn cancel_chat_run_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };
    if let Some(resp) =
        ensure_session_in_scope(chat_api.get_ref(), &session_id, &principal, &workspace).await
    {
        return resp;
    }

    match chat_api.chat_service.cancel_chat_run(&session_id).await {
        Ok(cancelled) => match cancel_tutor_run_for_scope(
            chat_api.chat_service.as_ref(),
            &principal,
            &workspace,
            &session_id,
            "Tutor run cancelled with the chat turn.".to_string(),
        )
        .await
        {
            Ok(tutor) => HttpResponse::Ok().json(serde_json::json!({
                "cancelled": cancelled,
                "session_id": session_id,
                "tutor": tutor,
            })),
            Err(error) => {
                warn!(
                    session_id = %session_id,
                    error = %error,
                    "[CHAT-API] chat turn cancelled but scoped tutor cleanup failed"
                );
                HttpResponse::Ok().json(serde_json::json!({
                    "cancelled": cancelled,
                    "session_id": session_id,
                    "tutor_cancel_error": error,
                }))
            },
        },
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to cancel chat run: {}", e)
        })),
    }
}

/// GET /api/magician/v2/chat/sessions/{id}/queue
///
/// List pending-replay messages for this session in FIFO order. Drives
/// the unified UI "N queued" pill and the queue inspector list. Returns
/// `{queued: [], session_id}` when the queue is empty.
pub async fn list_queued_messages_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };
    if let Some(resp) =
        ensure_session_in_scope(chat_api.get_ref(), &session_id, &principal, &workspace).await
    {
        return resp;
    }

    let queued = chat_api.chat_service.list_queued_messages(&session_id);
    HttpResponse::Ok().json(serde_json::json!({
        "queued": queued,
        "active": chat_api.chat_service.has_active_chat_run(&session_id) || chat_api.chat_service.has_active_tail(&session_id),
        "session_id": session_id,
    }))
}

/// DELETE /api/magician/v2/chat/sessions/{id}/queue/{message_id}
///
/// Remove one queued message by id. Idempotent: returns
/// `{"deleted": false}` when the id is unknown (already drained or
/// never enqueued).
pub async fn delete_queued_message_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let (session_id, message_id) = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };
    if let Some(resp) =
        ensure_session_in_scope(chat_api.get_ref(), &session_id, &principal, &workspace).await
    {
        return resp;
    }

    let deleted = chat_api
        .chat_service
        .remove_queued_message_admitted(&session_id, &message_id).await;
    HttpResponse::Ok().json(serde_json::json!({
        "deleted": deleted,
        "session_id": session_id,
        "message_id": message_id,
    }))
}

/// DELETE /api/magician/v2/chat/sessions/{id}/queue
///
/// Clear the entire pending-replay queue for this session. Returns the
/// count of messages dropped.
pub async fn clear_queued_messages_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<MessagesQuery>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(w) => w,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(response) => return response,
    };
    if let Some(resp) =
        ensure_session_in_scope(chat_api.get_ref(), &session_id, &principal, &workspace).await
    {
        return resp;
    }

    let cleared = chat_api.chat_service.clear_queued_messages_admitted(&session_id).await;
    HttpResponse::Ok().json(serde_json::json!({
        "cleared": cleared,
        "session_id": session_id,
    }))
}

/// Shared scope-check helper: returns `Some(error_response)` when the
/// session does not belong to (principal, workspace), is missing, or
/// failed to load. Returns `None` when the session is in scope and the
/// caller should proceed.
async fn ensure_session_in_scope(
    chat_api: &ChatApi,
    session_id: &str,
    principal: &str,
    workspace: &str,
) -> Option<HttpResponse> {
    match chat_api.chat_service.get_session(session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, principal, workspace) => {
            Some(HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied"
            })))
        },
        Ok(None) => Some(HttpResponse::NotFound().json(serde_json::json!({
            "error": "Session not found",
            "session_id": session_id,
        }))),
        Err(e) => Some(HttpResponse::InternalServerError().json(serde_json::json!({
            "error": e.to_string()
        }))),
        _ => None,
    }
}

#[derive(Debug)]
enum ParsedServerAction {
    Noop,
}

#[derive(Debug)]
struct ResolvedServerAction {
    action_ref: String,
    message_id: String,
}

fn parse_server_action_ref(action_ref: &str) -> ParsedServerAction {
    // Server-side structured actions are intentionally inert until there is a
    // durable, capability-scoped action registry. Never encode authority in a
    // client-visible reference string.
    let _ = action_ref;
    ParsedServerAction::Noop
}

async fn find_server_action_message(
    chat_service: &ChatService,
    session_id: &str,
    action_ref: &str,
) -> Result<Option<ResolvedServerAction>, String> {
    let mut before_id: Option<String> = None;
    for _ in 0..MAX_SERVER_ACTION_LOOKUP_PAGES {
        let (messages, has_more) = chat_service
            .chat_store_ref()
            .get_messages_paginated(
                session_id,
                MAX_SERVER_ACTION_LOOKUP_PAGE_SIZE,
                before_id.as_deref(),
            )
            .await
            .map_err(|error| error.to_string())?;

        if let Some(message) = messages.iter().rev().find(|message| {
            message.presentation.as_ref().is_some_and(|presentation| {
                presentation
                    .actions
                    .as_ref()
                    .into_iter()
                    .flatten()
                    .any(|action| {
                        matches!(
                            action,
                            StructuredResponseActionV1::InvokeServerAction {
                                action_ref: candidate,
                                ..
                            } if candidate == action_ref
                        )
                    })
            })
        }) {
            return Ok(Some(ResolvedServerAction {
                action_ref: action_ref.to_string(),
                message_id: message.id.clone(),
            }));
        }

        if !has_more || messages.is_empty() {
            break;
        }
        before_id = messages.last().map(|message| message.id.clone());
    }

    Ok(None)
}

/// GET /api/magician/v2/chat/profiles
///
/// List all chat-eligible LLM profiles.
pub async fn list_chat_profiles_handler(chat_api: web::Data<ChatApi>) -> impl Responder {
    let profiles = chat_api.chat_service.list_chat_profiles();
    let warnings = chat_api.chat_service.list_chat_profile_warnings();
    HttpResponse::Ok().json(serde_json::json!({
        "profiles": profiles,
        "warnings": warnings,
    }))
}

/// GET /api/magician/v2/chat/public-contacts
///
/// List durable public-contact profiles and research lifecycle state.
pub async fn list_public_contact_profiles_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<PublicContactProfilesQuery>,
) -> impl Responder {
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        None,
        None,
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let all_profiles = match chat_api
        .chat_service
        .list_public_contact_profiles(&principal, &workspace)
        .await
    {
        Ok(profiles) => profiles,
        Err(error) => {
            error!(
                principal = %principal,
                workspace = %workspace,
                error = %error,
                "[CHAT-API] failed to list public contact profiles"
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to list public contact profiles",
                "details": error.to_string()
            }));
        },
    };

    let counts = summarize_public_contact_profiles(&all_profiles);
    let mut profiles = all_profiles
        .into_iter()
        .filter(|profile| public_contact_profile_matches_query(profile, &query))
        .collect::<Vec<_>>();
    let total_matching = profiles.len();
    let limit = query.limit.clamp(1, 1_000);
    profiles.truncate(limit);

    HttpResponse::Ok().json(PublicContactProfileListResponse {
        count: profiles.len(),
        total_matching,
        total_profiles: counts.total,
        profiles,
        counts,
    })
}

fn public_contact_profile_matches_query(
    profile: &PublicContactProfile,
    query: &PublicContactProfilesQuery,
) -> bool {
    if let Some(required) = query.owner_review_priority {
        if profile.owner_review_priority != required {
            return false;
        }
    }
    if let Some(status) = query
        .research_status
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if !public_contact_research_status_matches(&profile.research_status, status) {
            return false;
        }
    }
    true
}

fn public_contact_research_status_matches(status: &IdentityResearchStatus, expected: &str) -> bool {
    let expected = expected.trim();
    if expected.eq_ignore_ascii_case("pending") {
        return matches!(
            status,
            IdentityResearchStatus::Queued | IdentityResearchStatus::Active
        );
    }
    public_contact_research_status_name(status).eq_ignore_ascii_case(expected)
}

fn summarize_public_contact_profiles(
    profiles: &[PublicContactProfile],
) -> PublicContactProfileCounts {
    let mut counts = PublicContactProfileCounts {
        total: profiles.len(),
        ..Default::default()
    };
    for profile in profiles {
        if profile.owner_review_priority {
            counts.owner_review_priority += 1;
        }
        if profile.missing_identity() {
            counts.missing_identity += 1;
        }
        if profile.missing_purpose() {
            counts.missing_purpose += 1;
        }
        match profile.research_status {
            IdentityResearchStatus::NotEligible => counts.research.not_eligible += 1,
            IdentityResearchStatus::Eligible => counts.research.eligible += 1,
            IdentityResearchStatus::Queued => {
                counts.research.queued += 1;
                counts.research.pending += 1;
            },
            IdentityResearchStatus::Active => {
                counts.research.active += 1;
                counts.research.pending += 1;
            },
            IdentityResearchStatus::Completed => counts.research.completed += 1,
            IdentityResearchStatus::Failed => counts.research.failed += 1,
            IdentityResearchStatus::Skipped => counts.research.skipped += 1,
        }
    }
    counts
}

fn public_contact_research_status_name(status: &IdentityResearchStatus) -> &'static str {
    match status {
        IdentityResearchStatus::NotEligible => "not_eligible",
        IdentityResearchStatus::Eligible => "eligible",
        IdentityResearchStatus::Queued => "queued",
        IdentityResearchStatus::Active => "active",
        IdentityResearchStatus::Completed => "completed",
        IdentityResearchStatus::Failed => "failed",
        IdentityResearchStatus::Skipped => "skipped",
    }
}

/// GET /api/magician/v2/chat/public-chat/status
///
/// Runtime-only public-chat admission, queue, and research status.
pub async fn get_public_chat_status_handler(chat_api: web::Data<ChatApi>) -> impl Responder {
    HttpResponse::Ok().json(chat_api.chat_service.public_chat_observability_snapshot())
}

/// POST /api/magician/v2/chat/sessions/{id}/attachments
///
/// Upload and stage a file for the next user turn.
pub async fn upload_attachment_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    mut payload: Multipart,
) -> impl Responder {
    if let Err(response) = validate_browser_origin(&req) {
        return response;
    }

    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) if matches!(session.internal_voice, Some(magician::magician_v2::chat::voice_requests::InternalVoiceSession::Coordinator { .. })) => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "Continue this topic through its voice request in the parent conversation."
            }));
        },
        Ok(Some(session))
            if session.status
                == magician::magician_v2::chat::models::ChatSessionStatus::Archived =>
        {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Cannot upload attachments to an archived session"
            }));
        },
        Ok(Some(_)) => {},
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to verify session {} for attachment upload: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session status",
                "details": e.to_string()
            }));
        },
    }

    const MAX_ATTACHMENT_BYTES: usize = 20 * 1024 * 1024;
    let mut uploaded_name: Option<String> = None;
    let mut uploaded_mime: Option<String> = None;
    let mut screen_capture: Option<ScreenCaptureAttachmentContext> = None;
    let mut bytes = Vec::new();

    while let Some(item) = payload.next().await {
        let mut field = match item {
            Ok(field) => field,
            Err(e) => {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "Invalid multipart payload",
                    "details": e.to_string()
                }));
            },
        };

        let disposition = field.content_disposition();
        let field_name = disposition.get_name().unwrap_or("file");
        if field_name == "screen_capture" {
            if screen_capture.is_some() {
                return HttpResponse::BadRequest().json(serde_json::json!({
                    "error": "Only one screen_capture context may be uploaded per request"
                }));
            }
            let mut context_bytes = Vec::new();
            while let Some(chunk) = field.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        return HttpResponse::BadRequest().json(serde_json::json!({
                            "error": "Failed to read screen_capture context",
                            "details": error.to_string()
                        }));
                    },
                };
                if context_bytes.len() + chunk.len() > 16 * 1024 {
                    return HttpResponse::PayloadTooLarge().json(serde_json::json!({
                        "error": "screen_capture context is too large"
                    }));
                }
                context_bytes.extend_from_slice(&chunk);
            }
            screen_capture = match parse_screen_capture_context(&context_bytes) {
                Ok(context) => Some(context),
                Err(reason) => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": reason
                    }));
                },
            };
            continue;
        }

        if uploaded_name.is_some() {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Only one attachment may be uploaded per request"
            }));
        }

        let filename = disposition
            .get_filename()
            .map(ToOwned::to_owned)
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "attachment".to_string());
        let mime_type = field
            .content_type()
            .map(|mime| mime.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_string());

        while let Some(chunk) = field.next().await {
            let chunk = match chunk {
                Ok(chunk) => chunk,
                Err(e) => {
                    return HttpResponse::BadRequest().json(serde_json::json!({
                        "error": "Failed to read multipart attachment",
                        "details": e.to_string()
                    }));
                },
            };
            if bytes.len() + chunk.len() > MAX_ATTACHMENT_BYTES {
                return HttpResponse::PayloadTooLarge().json(serde_json::json!({
                    "error": format!("Attachment too large. Maximum {} bytes allowed.", MAX_ATTACHMENT_BYTES)
                }));
            }
            bytes.extend_from_slice(&chunk);
        }

        uploaded_name = Some(filename);
        uploaded_mime = Some(mime_type);
    }

    let Some(filename) = uploaded_name else {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "No attachment file found in multipart payload"
        }));
    };
    let mime_type = uploaded_mime.unwrap_or_else(|| "application/octet-stream".to_string());
    if let Some(context) = screen_capture.as_ref() {
        if let Err(reason) = validate_uploaded_screen_capture_image(context, &mime_type, &bytes) {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": reason
            }));
        }
    }

    match chat_api
        .chat_service
        .store_attachment_with_context(
            &session_id,
            &filename,
            &mime_type,
            &bytes,
            None,
            screen_capture,
        )
        .await
    {
        Ok(record) => HttpResponse::Ok().json(AttachmentUploadResponse {
            attachment_id: record.id,
            filename: record.stored_name,
            mime_type: record.mime_type,
            size: record.size,
        }),
        Err(e) => {
            error!(
                "[CHAT-API] Failed to store attachment for session {}: {}",
                session_id, e
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to store attachment",
                "details": e.to_string()
            }))
        },
    }
}

/// GET /api/magician/v2/chat/sessions/{id}/outputs/{relative_path}
///
/// Serve a chat-session-local file from the session outputs directory.
pub async fn get_session_output_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<(String, String)>,
    query: web::Query<ActiveSessionQuery>,
) -> impl Responder {
    let (session_id, relative_path) = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let (session, file_path, contents, _record) = match chat_api
        .chat_service
        .resolve_session_output(&session_id, &relative_path)
        .await
    {
        Ok(resolved) => resolved,
        Err(e) if e.to_string().contains("Chat session not found") => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e)
            if e.to_string().contains("Invalid relative_path")
                || e.to_string().contains("relative_path required") =>
        {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Invalid relative_path"
            }));
        },
        Err(e) if e.to_string().contains("Session output not found") => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session output not found"
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to resolve session output {} for {}: {}",
                relative_path, session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to resolve session output",
                "details": e.to_string()
            }));
        },
    };

    if !session_matches_scope(&session, &principal, &workspace) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Access denied: session belongs to a different principal or workspace"
        }));
    }

    build_session_output_download_response(&file_path, contents)
}

fn build_session_output_download_response(
    path: &std::path::Path,
    contents: Vec<u8>,
) -> HttpResponse {
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("download")
        .replace(['"', '\\', ';', '\n', '\r'], "");
    let mut response = HttpResponse::Ok();
    response.insert_header(("X-Content-Type-Options", "nosniff"));
    if let Some(content_type) = infer_session_output_inline_media_type(path) {
        response
            .insert_header((header::CONTENT_TYPE, content_type))
            .insert_header((
                header::CONTENT_DISPOSITION,
                format!("inline; filename=\"{}\"", filename),
            ))
            .body(contents)
    } else {
        response
            .insert_header((header::CONTENT_TYPE, "application/octet-stream"))
            .insert_header((
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}\"", filename),
            ))
            .body(contents)
    }
}

fn infer_session_output_inline_media_type(path: &std::path::Path) -> Option<&'static str> {
    // Chat-session outputs are the SAME workspace-owned task deliverables the v3
    // task-output endpoint (and the deep panel) already render inline — user
    // uploads go through the separate attachment store, NOT this outputs dir,
    // so there is no distinct trust boundary that would justify forcing
    // HTML/SVG to download only in chat. (The earlier html/svg -> download
    // special-case was an inconsistency: the identical file rendered inline
    // from the deep panel but downloaded from chat.) Use the one shared inline
    // policy so HTML / SVG / PDF / text / media render in the browser on "Open"
    // consistently across both surfaces.
    crate::task_api_v3::infer_safe_inline_media_type(path)
}

fn containing_folder(path: &Path) -> &Path {
    if path.is_dir() {
        path
    } else {
        path.parent().unwrap_or(path)
    }
}

/// Block extensions that would silently auto-execute when handed to
/// `open` / `xdg-open` / `explorer`. The list is conservative — better
/// to refuse and force the operator to reveal-folder + double-click
/// than to auto-run something the agent mentioned in passing. Bundle
/// extensions (.app on macOS) are caught as well; even though `.app`
/// is technically a directory, the path-shaped match on the markdown
/// side can produce one.
fn is_unsafe_to_auto_open(path: &Path) -> bool {
    let ext = match path
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
    {
        Some(ext) => ext,
        None => return false,
    };
    matches!(
        ext.as_str(),
        // Executables / installers / scripts.
        "app"
            | "applescript"
            | "bat"
            | "cmd"
            | "com"
            | "dmg"
            | "exe"
            | "jar"
            | "msi"
            | "pkg"
            | "ps1"
            | "py"
            | "rb"
            | "scpt"
            | "scr"
            | "sh"
            | "vbs"
            | "wsf"
            // Office / docs with macro auto-run capability.
            | "docm"
            | "pptm"
            | "xlsm"
            // Shortcut files that can re-point to anything.
            | "lnk"
            | "url"
            | "webloc"
    )
}

pub(crate) async fn open_folder_in_file_manager(path: &Path) -> std::io::Result<()> {
    let folder = containing_folder(path);
    open_path_with_os_default(folder).await
}

pub(crate) async fn open_file_with_os_default(path: &Path) -> std::io::Result<()> {
    open_path_with_os_default(path).await
}

pub(crate) fn containing_folder_path(path: &Path) -> &Path {
    containing_folder(path)
}

/// Hand a path to the OS default opener. For a directory this acts as
/// "reveal in file manager"; for a regular file this lets the OS pick
/// the registered default app (Preview for PNG on macOS, browser for
/// HTML on Linux, etc.). Shared by `open-folder` and `open-file`
/// endpoints so platform branching stays in one place.
async fn open_path_with_os_default(target: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(target);
        command
    };
    #[cfg(target_os = "linux")]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(target);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("explorer");
        command.arg(target);
        command
    };

    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(false);
    let status = command.status().await?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "file manager exited with status {status}"
        )))
    }
}

/// POST /api/magician/v2/chat/sessions/{id}/outputs/open-folder
///
/// Reveal the containing folder for a session or task output rendered in chat.
pub async fn open_output_folder_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<OpenOutputFolderRequest>,
) -> impl Responder {
    if let Err(response) = validate_browser_origin(&req) {
        return response;
    }

    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let (session, resolved_path) = match chat_api
        .chat_service
        .resolve_openable_output_path(
            &session_id,
            body.source.as_ref(),
            body.relative_path.as_deref(),
            body.absolute_path.as_deref(),
        )
        .await
    {
        Ok(resolved) => resolved,
        Err(e) if e.to_string().contains("Chat session not found") => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) if e.to_string().contains("outside allowed output roots") => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Path is outside allowed output roots"
            }));
        },
        Err(e) if e.to_string().contains("required") || e.to_string().contains("Invalid") => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to resolve open-folder request for {}: {}",
                session_id, e
            );
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Output path not found",
                "details": e.to_string()
            }));
        },
    };

    if !session_matches_scope(&session, &principal, &workspace) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Access denied: session belongs to a different principal or workspace"
        }));
    }

    let folder = containing_folder(&resolved_path).to_path_buf();
    match open_folder_in_file_manager(&resolved_path).await {
        Ok(()) => HttpResponse::Ok().json(OpenOutputFolderResponse {
            folder_path: folder.to_string_lossy().into_owned(),
            file_path: Some(resolved_path.to_string_lossy().into_owned()),
        }),
        Err(e) => {
            error!(
                "[CHAT-API] Failed to open folder for {}: {}",
                resolved_path.display(),
                e
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to open folder",
                "details": e.to_string()
            }))
        },
    }
}

/// POST /api/magician/v2/chat/sessions/{id}/outputs/open-file
///
/// Open a chat-rendered output file directly in the OS-default application.
/// Distinct from `open-folder`, which reveals the parent directory: this
/// hands the *file itself* to `open` / `xdg-open` / `explorer`, so the
/// OS picks the registered app (Preview for PNG on macOS, default
/// browser for HTML on Linux, etc.). Uses the same
/// `resolve_openable_output_path` validation as `open-folder` — paths
/// must be inside scope/workspace roots OR demonstrably referenced by
/// the chat session's messages.
pub async fn open_output_file_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<OpenOutputFolderRequest>,
) -> impl Responder {
    if let Err(response) = validate_browser_origin(&req) {
        return response;
    }

    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let (session, resolved_path) = match chat_api
        .chat_service
        .resolve_openable_output_path(
            &session_id,
            body.source.as_ref(),
            body.relative_path.as_deref(),
            body.absolute_path.as_deref(),
        )
        .await
    {
        Ok(resolved) => resolved,
        Err(e) if e.to_string().contains("Chat session not found") => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) if e.to_string().contains("outside allowed output roots") => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Path is outside allowed output roots"
            }));
        },
        Err(e) if e.to_string().contains("required") || e.to_string().contains("Invalid") => {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": e.to_string()
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to resolve open-file request for {}: {}",
                session_id, e
            );
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Output path not found",
                "details": e.to_string()
            }));
        },
    };

    if !session_matches_scope(&session, &principal, &workspace) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Access denied: session belongs to a different principal or workspace"
        }));
    }

    if !resolved_path.exists() {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": "File not found",
            "path": resolved_path.to_string_lossy()
        }));
    }
    if resolved_path.is_dir() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Cannot open a directory as a file; use open-folder instead",
            "path": resolved_path.to_string_lossy()
        }));
    }
    // Safety: the session-reference fallback in
    // `resolve_openable_output_path` now honours paths the assistant
    // *mentioned in prose*, which widens the trust surface (the agent
    // may have processed external content like emails or web pages
    // that contained a path). Handing such a path to the OS-default
    // opener for an executable extension (.app/.sh/.exe/etc.) would
    // silently auto-run it. Restrict the open-file endpoint to a
    // documented "safe content" allowlist. Operators who want to run
    // a flagged file can still reveal its folder and open it manually.
    if is_unsafe_to_auto_open(&resolved_path) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "Refusing to auto-open this file type; reveal the folder and open it manually",
            "path": resolved_path.to_string_lossy()
        }));
    }

    match open_file_with_os_default(&resolved_path).await {
        Ok(()) => HttpResponse::Ok().json(OpenOutputFileResponse {
            file_path: resolved_path.to_string_lossy().into_owned(),
        }),
        Err(e) => {
            error!(
                "[CHAT-API] Failed to open file for {}: {}",
                resolved_path.display(),
                e
            );
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to open file",
                "details": e.to_string()
            }))
        },
    }
}

/// Request body for `POST /chat/sessions/{id}/transcript`.
#[derive(Debug, Deserialize)]
pub struct TranscriptLineRequest {
    /// The transcribed utterance text.
    pub text: String,
    /// Optional speaker label (prefixed to the line when present).
    #[serde(default)]
    pub speaker: Option<String>,
}

/// Request body for `POST /chat/sessions/{id}/actions/invoke`.
#[derive(Debug, Deserialize)]
pub struct InvokeServerActionRequest {
    pub action_ref: String,
}

/// POST /api/magician/v2/chat/sessions/{id}/transcript
///
/// Append a live meeting-transcript line to a session as a DISPLAY-ONLY
/// message. Unlike `send_message_handler`, this does NOT dispatch the agent —
/// the meeting bot streams every heard turn here for visibility while the
/// wake-word gate alone decides when the bot actually replies. Scope-guarded
/// like a send.
pub async fn post_transcript_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<TranscriptLineRequest>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let text = body.text.trim();
    if text.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": "text is required" }));
    }

    // Scope guard: only the owning principal/workspace may write to the session,
    // and — like send_message — archived sessions reject writes. The non-2xx on
    // archived is load-bearing: it is what lets the meeting transcript sink drop
    // its cached session id and re-resolve the thread's CURRENT active session
    // (otherwise transcript lines keep landing in the archived session forever
    // while the responder lanes move on). The fetched session is passed through
    // so the service doesn't re-read it per transcript line.
    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) if matches!(session.internal_voice, Some(magician::magician_v2::chat::voice_requests::InternalVoiceSession::Coordinator { .. })) => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "Continue this topic through its voice request in the parent conversation."
            }));
        },
        Ok(Some(session))
            if session.status
                == magician::magician_v2::chat::models::ChatSessionStatus::Archived =>
        {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Cannot append transcript to an archived session"
            }));
        },
        Ok(Some(session)) => session,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session",
                "details": e.to_string()
            }));
        },
    };

    match chat_api
        .chat_service
        .persist_meeting_transcript_line(&session, body.speaker.clone(), text.to_string())
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({ "ok": true })),
        Err(e) => {
            HttpResponse::InternalServerError().json(serde_json::json!({ "error": e.to_string() }))
        },
    }
}

/// POST /api/magician/v2/chat/sessions/{id}/actions/invoke
///
/// Invoke a structured-response server action by its opaque action reference.
/// The action reference is validated against persisted session messages so
/// the client cannot invent actions for arbitrary sessions.
pub async fn post_invoke_server_action_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<InvokeServerActionRequest>,
) -> impl Responder {
    let session_id = path.into_inner();
    if let Err(response) = validate_browser_origin(&req) {
        return response;
    }

    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };

    let normalized_action_ref = body.action_ref.trim();
    if normalized_action_ref.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "action_ref is required"
        }));
    }

    if normalized_action_ref.len() > MAX_ACTION_REF_CHARS {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "action_ref is too long"
        }));
    }

    if let Some(response) =
        ensure_session_in_scope(chat_api.get_ref(), &session_id, &principal, &workspace).await
    {
        return response;
    }

    let resolved = match find_server_action_message(
        &chat_api.chat_service,
        &session_id,
        normalized_action_ref,
    )
    .await
    {
        Ok(Some(action)) => action,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Action reference not found in this session",
                "action_ref": normalized_action_ref,
                "session_id": session_id
            }));
        },
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": error
            }));
        },
    };

    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) => session,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(error) => {
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": error.to_string()
            }));
        },
    };

    if session.status == magician::magician_v2::chat::models::ChatSessionStatus::Archived {
        return HttpResponse::BadRequest().json(serde_json::json!({
        "error": "Cannot invoke server action on an archived session"
        }));
    }

    let execution = match parse_server_action_ref(normalized_action_ref) {
        ParsedServerAction::Noop => serde_json::json!({
            "status": "unsupported",
            "kind": "server_action",
        }),
    };

    debug!(
        session_id = %session_id,
        principal = %principal,
        workspace = %workspace,
        action_kind = %resolved.action_ref,
        source_message_id = %resolved.message_id,
        action_ref = %normalized_action_ref,
        "POST /chat/sessions/:session_id/actions/invoke: server action rejected as unsupported"
    );

    HttpResponse::Ok().json(serde_json::json!({
        "ok": true,
        "status": "ok",
        "action_ref": normalized_action_ref,
        "session_id": session_id,
        "message_id": resolved.message_id,
        "execution": execution,
    }))
}

/// POST /api/magician/v2/chat/sessions/{id}/messages
///
/// Send a message in a chat session and get the assistant's response.
/// Only works on active sessions.
pub async fn send_message_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<SendMessageRequest>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!(
        "[CHAT-API] POST /chat/sessions/{}/messages text_len={}",
        session_id,
        body.text.as_deref().map(str::len).unwrap_or(0)
    );

    let trimmed_text = match validate_send_message_request(&body) {
        Ok(text) => text,
        Err(response) => return response,
    };

    if let Some(response) = validate_profile_override(chat_api.get_ref(), &body) {
        return response;
    }
    let routed_profile = match routed_profile_override(&body) {
        Ok(profile) => profile,
        Err(response) => return response,
    };
    if rejects_client_selected_protected_surface(body.source_surface.as_deref()) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "protected product surfaces must use their authenticated server endpoint"
        }));
    }

    // Guard: scope + only active sessions accept new messages
    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) if matches!(session.internal_voice, Some(magician::magician_v2::chat::voice_requests::InternalVoiceSession::Coordinator { .. })) => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "Continue this topic through its voice request in the parent conversation."
            }));
        },
        Ok(Some(session))
            if session.status
                == magician::magician_v2::chat::models::ChatSessionStatus::Archived =>
        {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Cannot send messages to an archived session"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to verify session {} status: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session status",
                "details": e.to_string()
            }));
        },
        Ok(Some(session)) => session,
    };
    let app_owner_execution_credential =
        match app_owner_execution_credential_for_request(&req, &session) {
            Ok(credential) => credential,
            Err(response) => return response,
        };

    match chat_api
        .chat_service
        .process_message_with_mode_on_execution_runtime_with_app_owner_credential(
            &session_id,
            trimmed_text,
            &body.attachment_ids,
            routed_profile.as_deref(),
            body.mode,
            body.plan_task_id.as_deref(),
            body.plan_question_id.as_deref(),
            body.chat_turn_id.as_deref(),
            body.voice_origin,
            body.source_surface.as_deref(),
            body.presence_session_id.as_deref(),
            body.sender_display_name.as_deref(),
            magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(
                body.coding_choice.clone(),
                None,
            ),
            app_owner_execution_credential,
        )
        .await
    {
        Ok(response) => {
            // Backend auto-drain — if the turn settled with pending
            // queued messages, kick off a drain task. The task owns an
            // `Arc<ChatService>` and loops on pop_next_queued_message
            // + dispatch until the queue is empty or a dispatch fails.
            // This makes UI sessions (no bot daemon) auto-drain too.
            if response.pending_queue_depth.unwrap_or(0) > 0 {
                chat_api
                    .chat_service
                    .spawn_pending_queue_drain(session_id.clone());
            }
            HttpResponse::Ok().json(response)
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to process message for session {}: {}",
                session_id, e
            );
            process_message_error_response(&e)
        },
    }
}

/// Publish completion of the watched streaming turn before a pending-message
/// drain is allowed to claim the same session's active-run slot. The SSE
/// disconnect guard uses this slot as its cross-turn fence; starting the drain
/// first creates a window where dropping the old stream cancels the new turn.
fn publish_streaming_completion_before_pending_drain<T>(
    slot: &std::sync::Arc<std::sync::Mutex<Option<T>>>,
    completion: T,
    pending_drain_required: bool,
    start_pending_drain: impl FnOnce(),
) {
    let completion_published = match slot.lock() {
        Ok(mut writer) => {
            *writer = Some(completion);
            true
        },
        Err(_) => {
            error!(
                "[CHAT-API] streaming completion slot is unavailable; pending drain retained for a later turn"
            );
            false
        },
    };
    if pending_drain_required && completion_published {
        start_pending_drain();
    }
}

/// POST /api/magician/v2/chat/sessions/{id}/messages/stream
///
/// Send a message and receive the assistant's response as an SSE stream.
/// Each SSE event has a type (`token`, `tool_call`, `done`, `error`) and
/// a JSON `data` payload:
///
/// ```text
/// event: token
/// data: {"text": "Hello"}
///
/// event: done
/// data: {"text": "Hello, how can I help?"}
/// ```
pub async fn send_message_stream_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ActiveSessionQuery>,
    body: web::Json<SendMessageRequest>,
) -> impl Responder {
    let session_id = path.into_inner();
    let workspace = match resolve_required_workspace(req.headers(), query.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match resolve_principal(
        chat_api.get_ref(),
        &workspace,
        requested_principal.as_deref(),
        query.channel.as_deref(),
        query.channel_address.as_deref(),
    )
    .await
    {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    debug!(
        "[CHAT-API] POST /chat/sessions/{}/messages/stream text_len={}",
        session_id,
        body.text.as_deref().map(str::len).unwrap_or(0)
    );

    let trimmed_text = match validate_send_message_request(&body) {
        Ok(text) => text.map(ToOwned::to_owned),
        Err(response) => return response,
    };

    if let Some(response) = validate_profile_override(chat_api.get_ref(), &body) {
        return response;
    }
    let routed_profile = match routed_profile_override(&body) {
        Ok(profile) => profile,
        Err(response) => return response,
    };
    if rejects_client_selected_protected_surface(body.source_surface.as_deref()) {
        return HttpResponse::Forbidden().json(serde_json::json!({
            "error": "protected product surfaces must use their authenticated server endpoint"
        }));
    }

    // Guard: scope + only active sessions accept new messages
    let session = match chat_api.chat_service.get_session(&session_id).await {
        Ok(Some(session)) if !session_matches_scope(&session, &principal, &workspace) => {
            return HttpResponse::Forbidden().json(serde_json::json!({
                "error": "Access denied: session belongs to a different principal or workspace"
            }));
        },
        Ok(Some(session)) if matches!(session.internal_voice, Some(magician::magician_v2::chat::voice_requests::InternalVoiceSession::Coordinator { .. })) => {
            return HttpResponse::Conflict().json(serde_json::json!({
                "error": "Continue this topic through its voice request in the parent conversation."
            }));
        },
        Ok(Some(session))
            if session.status
                == magician::magician_v2::chat::models::ChatSessionStatus::Archived =>
        {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": "Cannot send messages to an archived session"
            }));
        },
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": "Session not found",
                "session_id": session_id
            }));
        },
        Err(e) => {
            error!(
                "[CHAT-API] Failed to verify session {} status: {}",
                session_id, e
            );
            return HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "Failed to verify session status",
                "details": e.to_string()
            }));
        },
        Ok(Some(session)) => session,
    };
    let app_owner_execution_credential =
        match app_owner_execution_credential_for_request(&req, &session) {
            Ok(credential) => credential,
            Err(response) => return response,
        };

    if body.mode == ChatMessageMode::Plan {
        if body.continue_on_disconnect {
            // Plan mode does not emit token deltas, so its historical path
            // awaited completion before constructing the one-frame SSE body.
            // An opted-in mobile turn must outlive that response future just
            // like ordinary streamed chat. The oneshot owns delivery only;
            // dropping its receiver cannot cancel the detached server turn.
            let service = Arc::clone(&chat_api.chat_service);
            let durable_session_id = session_id.clone();
            let durable_text = trimmed_text.clone();
            let durable_attachment_ids = body.attachment_ids.clone();
            let durable_profile = routed_profile.clone();
            let durable_plan_task_id = body.plan_task_id.clone();
            let durable_plan_question_id = body.plan_question_id.clone();
            let durable_chat_turn_id = body.chat_turn_id.clone();
            let durable_source_surface = body.source_surface.clone();
            let durable_presence_session_id = body.presence_session_id.clone();
            let durable_sender_display_name = body.sender_display_name.clone();
            let durable_voice_origin = body.voice_origin;
            let durable_coding_choice =
                magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(
                    body.coding_choice.clone(),
                    None,
                );
            let (result_tx, result_rx) = tokio::sync::oneshot::channel();
            tokio::spawn(async move {
                let result = service
                    .process_message_with_mode_on_execution_runtime_with_app_owner_credential(
                        &durable_session_id,
                        durable_text.as_deref(),
                        &durable_attachment_ids,
                        durable_profile.as_deref(),
                        ChatMessageMode::Plan,
                        durable_plan_task_id.as_deref(),
                        durable_plan_question_id.as_deref(),
                        durable_chat_turn_id.as_deref(),
                        durable_voice_origin,
                        durable_source_surface.as_deref(),
                        durable_presence_session_id.as_deref(),
                        durable_sender_display_name.as_deref(),
                        durable_coding_choice,
                        app_owner_execution_credential,
                    )
                    .await;
                let _ = result_tx.send(result);
            });

            let logged_session_id = session_id.clone();
            let done_stream = futures_util::stream::once(async move {
                let frame = match result_rx.await {
                    Ok(Ok(response)) => {
                        let data =
                            serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                        format!("event: done\ndata: {}\n\n", data)
                    },
                    Ok(Err(error)) => {
                        tracing::error!(
                            session_id = %logged_session_id,
                            error = %error,
                            "[CHAT-API] Durable plan-mode streaming message failed"
                        );
                        format!(
                            "event: error\ndata: {}\n\n",
                            serde_json::json!({"error": format!("Processing failed: {error}")})
                        )
                    },
                    Err(_) => format!(
                        "event: error\ndata: {}\n\n",
                        serde_json::json!({"error": "Processing task ended before producing a result"})
                    ),
                };
                Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(frame))
            });

            return HttpResponse::Ok()
                .content_type("text/event-stream")
                .insert_header(("Cache-Control", "no-cache"))
                .insert_header(("X-Accel-Buffering", "no"))
                .streaming(done_stream);
        }

        match chat_api
            .chat_service
            .process_message_with_mode_on_execution_runtime_with_app_owner_credential(
                &session_id,
                trimmed_text.as_deref(),
                &body.attachment_ids,
                routed_profile.as_deref(),
                body.mode,
                body.plan_task_id.as_deref(),
                body.plan_question_id.as_deref(),
                body.chat_turn_id.as_deref(),
                body.voice_origin,
                body.source_surface.as_deref(),
                body.presence_session_id.as_deref(),
                body.sender_display_name.as_deref(),
                magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(
                    body.coding_choice.clone(),
                    None,
                ),
                app_owner_execution_credential,
            )
            .await
        {
            Ok(response) => {
                let data = serde_json::to_string(&response).unwrap_or_else(|_| "{}".to_string());
                let done_stream = futures_util::stream::once(async move {
                    Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(format!(
                        "event: done\ndata: {}\n\n",
                        data
                    )))
                });

                return HttpResponse::Ok()
                    .content_type("text/event-stream")
                    .insert_header(("Cache-Control", "no-cache"))
                    .insert_header(("X-Accel-Buffering", "no"))
                    .streaming(done_stream);
            },
            Err(e) => {
                error!(
                    "[CHAT-API] Failed to process plan-mode streaming message for session {}: {}",
                    session_id, e
                );
                return process_message_error_response(&e);
            },
        }
    }

    // Create the streaming channel + shared slot for final ChatResponse.
    // Buffer sized for typical LLM token bursts (50–100 tokens before
    // the SSE consumer drains a batch). The inline-turn `on_delta`
    // callback uses `try_send` against this channel; a 256-deep buffer
    // gives headroom for tight bursts without spilling to the
    // tracing-warn drop path. See `process_chat_inline_turn` for the
    // forwarding contract.
    let (tx, rx) = tokio::sync::mpsc::channel::<StreamDelta>(256);
    let chat_response_slot: std::sync::Arc<std::sync::Mutex<Option<ChatResponse>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let chat_response_writer = chat_response_slot.clone();

    // Spawn the streaming processing in a background task
    let service = chat_api.chat_service.clone();
    let sid = session_id.clone();
    let msg_text = trimmed_text.clone();
    let attachment_ids = body.attachment_ids.clone();
    let profile_override = routed_profile.clone();
    let mode = body.mode;
    let plan_task_id = body.plan_task_id.clone();
    let plan_question_id = body.plan_question_id.clone();
    let chat_turn_id = body.chat_turn_id.clone();
    let voice_origin = body.voice_origin;
    let source_surface = body.source_surface.clone();
    let presence_session_id = body.presence_session_id.clone();
    let sender_display_name = body.sender_display_name.clone();
    let coding_choice =
        magician::magician_v2::vibedev::run_service::coding_choice_from_client_fields(
            body.coding_choice.clone(),
            None,
        );
    let drain_service = Arc::clone(&service);
    let drain_sid = sid.clone();
    tokio::spawn(async move {
        match Box::pin(
            service
                .process_message_streaming_with_mode_on_execution_runtime_with_app_owner_credential(
                    &sid,
                    msg_text.as_deref(),
                    &attachment_ids,
                    tx.clone(),
                    profile_override.as_deref(),
                    mode,
                    plan_task_id.as_deref(),
                    plan_question_id.as_deref(),
                    chat_turn_id.as_deref(),
                    voice_origin,
                    source_surface.as_deref(),
                    presence_session_id.as_deref(),
                    sender_display_name.as_deref(),
                    coding_choice,
                    app_owner_execution_credential,
                ),
        )
        .await
        {
            Ok(response) => {
                // Backend auto-drain — kick off when the turn finished
                // with pending queued messages. See the non-streaming
                // handler above for the rationale. Completion publication is
                // deliberately first: the old stream's disconnect guard must
                // become inert before a drain can claim the same active slot.
                let pending_drain_required = response.pending_queue_depth.unwrap_or(0) > 0;
                publish_streaming_completion_before_pending_drain(
                    &chat_response_writer,
                    response,
                    pending_drain_required,
                    move || drain_service.spawn_pending_queue_drain(drain_sid),
                );
            },
            Err(e) => {
                error!(
                    "[CHAT-API] Streaming processing failed for session {}: {}",
                    sid, e
                );
                let _ = tx
                    .send(StreamDelta::Error(format!("Processing failed: {}", e)))
                    .await;
            },
        }
    });

    // Convert the receiver into an SSE byte stream.
    // Token/ToolCallDelta/Error deltas are forwarded as SSE events.
    // The provider's Done delta is suppressed (service handles it internally).
    // After the channel closes, a final "done" event is appended with the
    // full ChatResponse from the shared slot.
    let stream = ReceiverStream::new(rx)
        .filter(|delta| futures_util::future::ready(!matches!(delta, StreamDelta::Done(_))))
        .map(|delta| {
            let sse_text = match delta {
                StreamDelta::Token(text) => format!(
                    "event: token\ndata: {}\n\n",
                    serde_json::json!({"text": text})
                ),
                StreamDelta::ToolCallDelta {
                    id,
                    name,
                    arguments_chunk,
                } => format!(
                    "event: tool_call\ndata: {}\n\n",
                    serde_json::json!({
                        "id": id,
                        "name": name,
                        "arguments_chunk": arguments_chunk,
                        "status": "executing"
                    })
                ),
                StreamDelta::Error(msg) => format!(
                    "event: error\ndata: {}\n\n",
                    serde_json::json!({"error": msg})
                ),
                StreamDelta::Done(_) => unreachable!("filtered above"),
                // Fine-grained AG-UI-style lifecycle deltas (added in
                // magicllm v0.1.33). chat_api passes through to clients
                // as named SSE events; consumers that don't care can
                // ignore them.
                StreamDelta::ReasoningStart { index, signature } => format!(
                    "event: reasoning_start\ndata: {}\n\n",
                    serde_json::json!({"index": index, "signature": signature})
                ),
                StreamDelta::ReasoningDelta { index, delta } => format!(
                    "event: reasoning_delta\ndata: {}\n\n",
                    serde_json::json!({"index": index, "delta": delta})
                ),
                StreamDelta::ReasoningEnd { index, total_chars } => format!(
                    "event: reasoning_end\ndata: {}\n\n",
                    serde_json::json!({"index": index, "total_chars": total_chars})
                ),
                StreamDelta::ToolCallStart { call_id, tool_name } => format!(
                    "event: tool_call_start\ndata: {}\n\n",
                    serde_json::json!({"call_id": call_id, "tool_name": tool_name})
                ),
                StreamDelta::ToolCallArgsDelta { call_id, delta } => format!(
                    "event: tool_call_args_delta\ndata: {}\n\n",
                    serde_json::json!({"call_id": call_id, "delta": delta})
                ),
                StreamDelta::ToolCallEnd { call_id } => format!(
                    "event: tool_call_end\ndata: {}\n\n",
                    serde_json::json!({"call_id": call_id})
                ),
            };
            Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(sse_text))
        });

    // Chain with a final "done" event carrying the full ChatResponse.
    // Clone here so the same slot can also be observed by the
    // `SseDisconnectCancelGuard` below — we need to read it from Drop
    // to detect whether the watched turn already finished (in which
    // case any subsequent drain belongs to a different turn and we
    // must NOT cascade-cancel it).
    let done_slot = chat_response_slot.clone();
    let done_event = futures_util::stream::once(async move {
        let data = if let Ok(slot) = done_slot.lock() {
            if let Some(ref response) = *slot {
                serde_json::to_string(response).unwrap_or_else(|_| "{}".to_string())
            } else {
                "{}".to_string()
            }
        } else {
            "{}".to_string()
        };
        Ok::<_, actix_web::Error>(actix_web::web::Bytes::from(format!(
            "event: done\ndata: {}\n\n",
            data
        )))
    });

    // SSE client-disconnect → cancel guard. When actix drops the
    // response future (HTTP/2 RST_STREAM from the browser closing the
    // tab, network drop, frontend `AbortController.abort()`), the
    // stream below is dropped along with it. Without intervention, the
    // background `tokio::spawn` above keeps running the chat turn:
    // it generates LLM tokens nobody reads, charges the provider, and
    // burns whatever quota the turn would have consumed. The guard's
    // `Drop` impl fires the normal chat-run cancellation plus the scoped tutor
    // cleanup on the captured session id. This races the in-flight turn against
    // `cancel_token.cancelled()` and aborts the upstream LLM request (HTTP/2
    // `RST_STREAM CANCEL` — provider stops generation) without leaving a
    // preseeded tutor run behind.
    //
    // Idempotent by design: if the turn already finished normally,
    // `cancel_chat_run` finds no active slot and noops. The guard
    // attaches to the head of the stream so it lives exactly as long
    // as the streaming response.
    //
    // Cross-turn race avoidance: the guard checks `chat_response_slot`
    // first. The slot is written exactly once when the watched turn
    // finishes (success path at line ~2632). If it's `Some`, the turn
    // we were streaming is already done and any subsequent drain that
    // claimed the same `active_chat_runs` slot belongs to a DIFFERENT
    // turn — firing `cancel_chat_run` here would wrongly abort that
    // unrelated turn. Skipping the cancel in that case keeps the
    // SSE-disconnect cascade narrowly scoped to the turn this stream
    // was watching.
    struct SseDisconnectCancelGuard {
        service: Arc<ChatService>,
        session_id: String,
        principal: String,
        workspace: String,
        cancel_on_disconnect: bool,
        chat_response_slot: std::sync::Arc<std::sync::Mutex<Option<ChatResponse>>>,
    }
    impl Drop for SseDisconnectCancelGuard {
        fn drop(&mut self) {
            if !self.cancel_on_disconnect {
                return;
            }
            if let Ok(slot) = self.chat_response_slot.lock() {
                if slot.is_some() {
                    return;
                }
            }
            if !self.service.has_active_chat_run(&self.session_id) {
                return;
            }
            // Acquire the runtime handle BEFORE `tokio::spawn` so a
            // Drop fired outside a Tokio context (defensive — actix
            // workers run on Tokio, but Drops fired from a panic
            // unwind on a non-runtime thread would otherwise panic).
            let Ok(handle) = tokio::runtime::Handle::try_current() else {
                tracing::warn!(
                    session_id = %self.session_id,
                    "SSE disconnect guard: no Tokio runtime in Drop context; cancel skipped"
                );
                return;
            };
            let svc = Arc::clone(&self.service);
            let sid = self.session_id.clone();
            let principal = self.principal.clone();
            let workspace = self.workspace.clone();
            handle.spawn(async move {
                let _ = svc.cancel_chat_run(&sid).await;
                if let Err(error) = cancel_tutor_run_for_scope(
                    svc.as_ref(),
                    &principal,
                    &workspace,
                    &sid,
                    "Tutor run cancelled because its streaming chat connection closed.".to_string(),
                )
                .await
                {
                    tracing::warn!(
                        session_id = %sid,
                        error = %error,
                        "SSE disconnect guard: scoped tutor cleanup failed"
                    );
                }
            });
        }
    }
    let _disconnect_guard = SseDisconnectCancelGuard {
        service: Arc::clone(&chat_api.chat_service),
        session_id: session_id.clone(),
        principal: principal.clone(),
        workspace: workspace.clone(),
        cancel_on_disconnect: !body.continue_on_disconnect,
        chat_response_slot: Arc::clone(&chat_response_slot),
    };
    // Box::pin the chained stream before wrapping in unfold: the
    // `stream::once(async move { … })` end-cap holds a non-`Unpin`
    // future, which makes the whole `Chain<…, Once<…>>` non-`Unpin`.
    // `StreamExt::next()` requires `Unpin`. Pinning behind a box gives
    // us a `Pin<Box<S>>` that IS `Unpin` (the box itself can move; the
    // inner future stays pinned in place).
    let guarded_stream = futures_util::stream::unfold(
        (Box::pin(stream.chain(done_event)), _disconnect_guard),
        |(mut stream, guard)| async move {
            use futures_util::StreamExt;
            stream.next().await.map(|item| (item, (stream, guard)))
        },
    );

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("X-Accel-Buffering", "no"))
        .streaming(chat_sse_with_keepalive(
            guarded_stream,
            std::time::Duration::from_secs(15),
        ))
}

/// A human answer may take minutes. SSE comments keep idle intermediaries
/// alive without becoming chat events or changing disconnect cancellation.
fn chat_sse_with_keepalive<S>(
    source: S,
    idle: std::time::Duration,
) -> impl futures_util::Stream<Item = Result<web::Bytes, actix_web::Error>>
where
    S: futures_util::Stream<Item = Result<web::Bytes, actix_web::Error>>,
{
    futures_util::stream::unfold(Box::pin(source), move |mut source| async move {
        tokio::select! {
            biased;
            item = source.next() => item.map(|item| (item, source)),
            _ = tokio::time::sleep(idle) => Some((
                Ok(web::Bytes::from_static(b": keep-alive\n\n")), source,
            )),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn chat_sse_keepalive_preserves_frames_and_eof() {
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let stream = chat_sse_with_keepalive(
            tokio_stream::wrappers::ReceiverStream::new(rx),
            std::time::Duration::from_millis(5),
        );
        let mut stream = Box::pin(stream);
        let comment = tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(comment, web::Bytes::from_static(b": keep-alive\n\n"));
        let token = web::Bytes::from_static(b"event: token\ndata: {\"text\":\"answer\"}\n\n");
        let done = web::Bytes::from_static(b"event: done\ndata: {}\n\n");
        tx.send(Ok(token.clone())).await.unwrap();
        tx.send(Ok(done.clone())).await.unwrap();
        drop(tx);
        assert_eq!(stream.next().await.unwrap().unwrap(), token);
        assert_eq!(stream.next().await.unwrap().unwrap(), done);
        assert!(stream.next().await.is_none());
    }

    #[tokio::test]
    async fn chat_sse_keepalive_drop_releases_underlying_guard() {
        use std::sync::atomic::{AtomicBool, Ordering};
        struct Guard(std::sync::Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = std::sync::Arc::new(AtomicBool::new(false));
        let guard = Guard(dropped.clone());
        let source = futures_util::stream::unfold(guard, |guard| async move {
            futures_util::future::pending::<()>().await;
            Some((Ok::<_, actix_web::Error>(web::Bytes::new()), guard))
        });
        let mut stream = Box::pin(chat_sse_with_keepalive(
            source,
            std::time::Duration::from_millis(5),
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(1), stream.next())
                .await
                .is_ok()
        );
        assert!(!dropped.load(Ordering::SeqCst));
        drop(stream);
        assert!(dropped.load(Ordering::SeqCst));
    }

    use std::sync::Arc;

    use tempfile::tempdir;

    use magician::magician_v2::{
        artifact_v2::workspace::ArtifactV2Workspace,
        chat::enrollment::{EnrollResult, EnrollmentStoreResolver},
    };

    #[test]
    fn streaming_completion_is_visible_before_pending_drain_starts() {
        let slot = Arc::new(std::sync::Mutex::new(None));
        let drain_started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed_slot = Arc::clone(&slot);
        let observed_drain_started = Arc::clone(&drain_started);

        publish_streaming_completion_before_pending_drain(
            &slot,
            "watched-turn-complete",
            true,
            move || {
                assert_eq!(
                    observed_slot.lock().expect("completion slot").as_deref(),
                    Some("watched-turn-complete")
                );
                observed_drain_started.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        );

        assert!(drain_started.load(std::sync::atomic::Ordering::SeqCst));
    }

    fn create_store_resolver() -> EnrollmentStoreResolver {
        let dir = tempdir().expect("tempdir");
        let path = dir.keep();
        EnrollmentStoreResolver::new(ArtifactV2Workspace::new(path))
    }

    fn chat_api_with_store_resolver(store_resolver: EnrollmentStoreResolver) -> ChatApi {
        ChatApi {
            chat_service: Arc::new(ChatService::new(
                Arc::new(magician::magician_v2::chat::storage::FileChatStore::new(
                    std::env::temp_dir().join(format!("chat-api-test-{}", uuid::Uuid::new_v4())),
                )),
                magician::magician_v2::chat::llm_service::ChatLlmService::new(Arc::new(
                    magician::magician_v2::query_analysis::multi_llm_service::MultiLLMService::new(
                        std::collections::HashMap::new(),
                        std::collections::HashMap::new(),
                    ),
                )),
                Arc::new(magician::magician_v2::prompts::PromptManager::new(
                    Arc::new(DummyPromptStore),
                )),
                Arc::new(
                    magician::magician_v2::realtime_events::RuntimeTransportBroadcaster::new(16),
                ),
            )),
            enrollment_store_resolver: Some(store_resolver),
            envoy_config: None,
        }
    }

    #[actix_web::test]
    async fn envoy_claims_delivery_requires_the_session_channel_bot_and_scope() {
        use actix_web::{http::StatusCode, test::TestRequest, HttpMessage};
        use magician::magician_v2::auth::{middleware::AuthenticatedRequest, BearerKind, ScopeRef};
        use magician::magician_v2::chat::envoy_claims::DeliveryPhase;
        let mut api = chat_api_with_store_resolver(create_store_resolver());
        api.envoy_config = Some(test_envoy_config());
        let session = api
            .chat_service
            .chat_store_ref()
            .get_or_create_active_session(
                "alice",
                "default",
                "ext:telegram:42",
                &ChatChannel::new("telegram", "42"),
                "envoy",
            )
            .await
            .unwrap();
        let api = web::Data::new(api);
        for (principal, workspace, bearer, expected) in [
            ("alice", "default", None, StatusCode::UNAUTHORIZED),
            (
                "alice",
                "default",
                Some(BearerKind::ApiToken),
                StatusCode::FORBIDDEN,
            ),
            (
                "alice",
                "default",
                Some(BearerKind::Bot {
                    bot_name: "kapso".into(),
                }),
                StatusCode::FORBIDDEN,
            ),
            (
                "other",
                "default",
                Some(BearerKind::Bot {
                    bot_name: "telegram".into(),
                }),
                StatusCode::FORBIDDEN,
            ),
            (
                "alice",
                "other",
                Some(BearerKind::Bot {
                    bot_name: "telegram".into(),
                }),
                StatusCode::FORBIDDEN,
            ),
            // An authorized bot gets past the boundary, but no layout/prepared
            // act exists in this fixture: it must still never receive send=true.
            (
                "alice",
                "default",
                Some(BearerKind::Bot {
                    bot_name: "telegram".into(),
                }),
                StatusCode::SERVICE_UNAVAILABLE,
            ),
        ] {
            let req = TestRequest::post()
                .insert_header(("X-Principal", "alice"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request();
            if let Some(bearer) = bearer {
                req.extensions_mut().insert(AuthenticatedRequest {
                    scope: ScopeRef::system_internal_unauthenticated(principal, workspace),
                    identity_name: None,
                    bearer,
                });
            }
            let response = envoy_delivery_handler(
                api.clone(),
                req,
                web::Path::from((session.id.clone(), "message-1".into())),
                web::Json(EnvoyDeliveryRequest {
                    phase: DeliveryPhase::Begin,
                    binding: None,
                }),
            )
            .await;
            assert_eq!(response.status(), expected);
        }
    }

    #[actix_web::test]
    async fn envoy_claims_prepared_replies_stay_tracked_after_config_is_disabled() {
        use actix_web::{http::StatusCode, test::TestRequest, HttpMessage};
        use magician::magician_v2::{
            auth::{middleware::AuthenticatedRequest, BearerKind, ScopeRef},
            chat::{
                envoy_claims::{prepare_reply, DeliveryBinding, DeliveryPhase},
                models::{ChatMessage, ChatMessageContent, ChatMessageDirection},
            },
            test_support::build_test_artifact_v2_harness,
        };
        let temp = tempfile::tempdir().unwrap();
        let (service, _orchestrator) = build_test_artifact_v2_harness(temp.path());
        let layout = service.workspace().clone();
        let mut api = chat_api_with_store_resolver(create_store_resolver());
        api.chat_service = Arc::new(
            Arc::try_unwrap(api.chat_service)
                .ok()
                .unwrap()
                .with_artifact_v2_service(service),
        );
        // No current Envoy config, as after a configuration change/restart.
        assert!(api.envoy_config.is_none());
        let session = api
            .chat_service
            .chat_store_ref()
            .get_or_create_active_session(
                "anonymous",
                "default",
                "ext:telegram:42",
                &ChatChannel::new("telegram", "42"),
                "envoy",
            )
            .await
            .unwrap();
        let message = ChatMessage::new(
            "prepared-message",
            &session.id,
            ChatMessageDirection::Assistant,
            ChatMessageContent::Text {
                text: "A".into(),
                plan_reply: None,
            },
            1000,
        );
        prepare_reply(&layout, &session, &message, "envoy").unwrap();
        let api = web::Data::new(api);
        let binding = DeliveryBinding {
            attempt_id: uuid::Uuid::new_v4().to_string(),
            channel_type: "telegram".into(),
            channel_address: "42".into(),
            payload_sha256: "559aead08264d5795d3909718cdd05abd49572e84fe55590eef31a88a08fdffd"
                .into(),
        };
        for (proof, expected) in [
            (None, StatusCode::CONFLICT),
            (Some(binding), StatusCode::OK),
        ] {
            let req = TestRequest::post().to_http_request();
            req.extensions_mut().insert(AuthenticatedRequest {
                scope: ScopeRef::system_internal_unauthenticated("anonymous", "default"),
                identity_name: None,
                bearer: BearerKind::Bot {
                    bot_name: "telegram".into(),
                },
            });
            let response = envoy_delivery_handler(
                api.clone(),
                req,
                web::Path::from((session.id.clone(), message.id.clone())),
                web::Json(EnvoyDeliveryRequest {
                    phase: DeliveryPhase::Begin,
                    binding: proof,
                }),
            )
            .await;
            assert_eq!(response.status(), expected);
            if expected == StatusCode::OK {
                let bytes = actix_web::body::to_bytes(response.into_body())
                    .await
                    .unwrap();
                let grant: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(grant["tracked"], true);
                assert_eq!(grant["send"], true);
            }
        }
    }

    fn test_envoy_config() -> EnvoyConfig {
        let mut owner_identities = std::collections::HashMap::new();
        owner_identities.insert("kapso".to_string(), vec!["9199".to_string()]);
        owner_identities.insert("telegram".to_string(), vec!["12345".to_string()]);
        EnvoyConfig {
            owner_identities,
            envoy_agent_id: "envoy".to_string(),
            ..EnvoyConfig::default()
        }
    }

    fn active_session_query(
        ui_thread_id: &str,
        channel: &str,
        channel_address: &str,
        control_intent: Option<bool>,
    ) -> ActiveSessionQuery {
        ActiveSessionQuery {
            workspace: Some("default".to_string()),
            ui_thread_id: ui_thread_id.to_string(),
            channel: Some(channel.to_string()),
            channel_address: Some(channel_address.to_string()),
            channel_address_kind: None,
            channel_verified: Some(true),
            control_intent,
            history_lane: None,
        }
    }

    struct DummyPromptStore;

    #[async_trait::async_trait]
    impl runtime_core::PromptStore for DummyPromptStore {
        async fn get_prompt(
            &self,
            _name: &str,
            _version: &str,
        ) -> anyhow::Result<runtime_core::Prompt> {
            Err(anyhow::anyhow!("unused in chat_api tests"))
        }

        async fn list_versions(&self, _name: &str) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn list_prompt_names(&self) -> anyhow::Result<Vec<String>> {
            Ok(Vec::new())
        }

        async fn save_prompt(&self, _prompt: &runtime_core::Prompt) -> anyhow::Result<()> {
            Ok(())
        }

        async fn prompt_exists(&self, _name: &str, _version: &str) -> anyhow::Result<bool> {
            Ok(false)
        }

        async fn latest_version(&self, _name: &str) -> anyhow::Result<String> {
            Err(anyhow::anyhow!("unused in chat_api tests"))
        }

        async fn delete_prompt(&self, _name: &str, _version: &str) -> anyhow::Result<()> {
            Ok(())
        }

        async fn initialize(&self) -> anyhow::Result<()> {
            Ok(())
        }

        async fn health_check(&self) -> anyhow::Result<bool> {
            Ok(true)
        }
    }

    #[test]
    fn route_inbound_query_owner_control_defaults_to_channel_magic_thread() {
        let cfg = test_envoy_config();
        let query = active_session_query("general", "kapso", "9199", Some(true));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "ext:kapso:9199:magic");
        assert_eq!(force_agent_id, None);
    }

    #[test]
    fn route_inbound_query_owner_control_preserves_explicit_requested_thread() {
        let cfg = test_envoy_config();
        let query = active_session_query("owner-special", "kapso", "9199", Some(true));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "owner-special");
        assert_eq!(force_agent_id, None);
    }

    #[test]
    fn route_inbound_query_control_intent_does_not_bypass_guest_auth() {
        let cfg = test_envoy_config();
        let query = active_session_query("general", "kapso", "not-owner", Some(true));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "ext:kapso:not-owner");
        assert_eq!(force_agent_id, Some("envoy".to_string()));
    }

    #[test]
    fn route_inbound_query_owner_telegram_control_defaults_to_channel_magic_thread() {
        let cfg = test_envoy_config();
        let query = active_session_query("general", "telegram", "12345", Some(true));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "ext:telegram:12345:magic");
        assert_eq!(force_agent_id, None);
    }

    #[test]
    fn route_inbound_query_owner_telegram_non_control_uses_envoy_thread() {
        let cfg = test_envoy_config();
        let query = active_session_query("general", "telegram", "12345", Some(false));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "ext:telegram:12345");
        assert_eq!(force_agent_id, Some("envoy".to_string()));
    }

    #[test]
    fn route_inbound_query_guest_telegram_control_stays_envoy_thread() {
        let cfg = test_envoy_config();
        let query = active_session_query("general", "telegram", "other", Some(true));

        let (ui_thread_id, force_agent_id) = route_inbound_query(Some(&cfg), &query, true, None);

        assert_eq!(ui_thread_id, "ext:telegram:other");
        assert_eq!(force_agent_id, Some("envoy".to_string()));
    }

    #[test]
    fn generic_chat_rejects_only_the_protected_thinking_map_surface() {
        assert!(rejects_client_selected_protected_surface(Some(
            "thinking_map"
        )));
        assert!(rejects_client_selected_protected_surface(Some(
            " Thinking_Map "
        )));
        assert!(rejects_client_selected_protected_surface(Some("plane")));
        assert!(rejects_client_selected_protected_surface(Some(" Plane ")));
        assert!(!rejects_client_selected_protected_surface(Some("chat")));
        assert!(!rejects_client_selected_protected_surface(Some("tutor")));
        assert!(!rejects_client_selected_protected_surface(None));
    }

    #[tokio::test]
    async fn resolve_principal_uses_enrolled_channel_identity() {
        let store_resolver = create_store_resolver();
        let store = store_resolver
            .resolve_for_scope("default", "default")
            .await
            .expect("workspace store");
        let result = store
            .enroll("web", "browser-123", "default", None, true, "default")
            .await
            .expect("enroll");
        assert!(matches!(result, EnrollResult::AutoApproved { .. }));
        let chat_api = chat_api_with_store_resolver(store_resolver);

        let principal =
            resolve_principal(&chat_api, "default", None, Some("web"), Some("browser-123"))
                .await
                .expect("principal");

        assert_eq!(principal, "default");
    }

    #[tokio::test]
    async fn resolve_principal_rejects_mismatched_enrolled_identity() {
        let store_resolver = create_store_resolver();
        let store = store_resolver
            .resolve_for_scope("owner", "default")
            .await
            .expect("workspace store");
        store
            .enroll("telegram", "chat-42", "default", None, true, "owner")
            .await
            .expect("enroll");
        let chat_api = chat_api_with_store_resolver(store_resolver);

        let response = resolve_principal(
            &chat_api,
            "default",
            Some("default"),
            Some("telegram"),
            Some("chat-42"),
        )
        .await
        .expect_err("mismatch should be rejected");

        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn resolve_principal_rejects_unenrolled_channel_identity() {
        let chat_api = chat_api_with_store_resolver(create_store_resolver());

        let response = resolve_principal(
            &chat_api,
            "default",
            Some("default"),
            Some("discord"),
            Some("user-7"),
        )
        .await
        .expect_err("unenrolled identity should be rejected");

        assert_eq!(response.status(), actix_web::http::StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn resolve_principal_requires_complete_channel_identity() {
        let chat_api = chat_api_with_store_resolver(create_store_resolver());

        let response = resolve_principal(&chat_api, "default", Some("default"), Some("web"), None)
            .await
            .expect_err("channel identity must be complete");

        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn session_output_response_inlines_safe_images_with_nosniff() {
        let response = build_session_output_download_response(
            std::path::Path::new("generated-preview.png"),
            vec![1, 2, 3],
        );

        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/png")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some("inline; filename=\"generated-preview.png\"")
        );
    }

    #[test]
    fn session_output_response_renders_active_content_inline() {
        // Chat-session outputs are the SAME workspace-owned task deliverables the v3
        // task-output endpoint (and the deep panel) already render inline — there is
        // no distinct trust boundary for the chat surface (uploads use the separate
        // attachment store, not this outputs dir). So HTML/SVG render inline here too,
        // with `X-Content-Type-Options: nosniff` set. The `<script>` payload below is
        // intentional: we still serve it `inline` (the accepted same-origin tradeoff
        // for trusted workspace deliverables) rather than singling out chat to force a
        // download — that inconsistency was the bug.
        let response = build_session_output_download_response(
            std::path::Path::new("payload.svg"),
            b"<svg><script>alert(1)</script></svg>".to_vec(),
        );

        assert_eq!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .and_then(|value| value.to_str().ok()),
            Some("nosniff")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("image/svg+xml; charset=utf-8")
        );
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_DISPOSITION)
                .and_then(|value| value.to_str().ok()),
            Some("inline; filename=\"payload.svg\"")
        );
    }

    #[test]
    fn validate_send_message_request_allows_plan_mode_attachments_with_text() {
        let request = SendMessageRequest {
            text: Some("Plan around the attached brief".to_string()),
            attachment_ids: vec!["attachment-1".to_string()],
            mode: ChatMessageMode::Plan,
            plan_task_id: None,
            plan_question_id: None,
            profile: None,
            harness_engine: None,
            harness_model: None,
            chat_turn_id: None,
            source_surface: None,
            presence_session_id: None,
            sender_display_name: None,
            voice_origin: false,
            continue_on_disconnect: false,
            coding_choice: None,
        };

        let trimmed = validate_send_message_request(&request).expect("request should validate");

        assert_eq!(trimmed, Some("Plan around the attached brief"));
    }

    #[test]
    fn mobile_stream_durability_is_explicit_and_defaults_off() {
        let ordinary: SendMessageRequest = serde_json::from_value(serde_json::json!({
            "text": "hello"
        }))
        .expect("ordinary request");
        let durable: SendMessageRequest = serde_json::from_value(serde_json::json!({
            "text": "hello",
            "continue_on_disconnect": true
        }))
        .expect("durable request");

        assert!(!ordinary.continue_on_disconnect);
        assert!(durable.continue_on_disconnect);
    }

    #[test]
    fn screen_capture_context_parser_accepts_ios_screenshot_dimensions() {
        let context = parse_screen_capture_context(
            br#"{"mode":"screenshot","coordinate_space":"capture","image_size":{"width":1179,"height":2556}}"#,
        )
        .expect("valid iOS screenshot context");

        assert_eq!(context.mode, "screenshot");
        assert_eq!(context.coordinate_space, "capture");
        assert!(!context.server_registered);
        assert_eq!(context.image_size.expect("image size").width, 1179);
    }

    #[test]
    fn screen_capture_context_parser_rejects_zero_dimensions() {
        let error = parse_screen_capture_context(
            br#"{"mode":"screenshot","coordinate_space":"capture","image_size":{"width":0,"height":2556}}"#,
        )
        .expect_err("zero width should fail");

        assert!(error.contains("positive"));
    }

    #[test]
    fn uploaded_screen_capture_metadata_must_match_real_png_dimensions() {
        let context = parse_screen_capture_context(
            br#"{"mode":"screenshot","coordinate_space":"capture","image_size":{"width":320,"height":180}}"#,
        )
        .expect("context");
        let mut png = vec![0_u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&320_u32.to_be_bytes());
        png[20..24].copy_from_slice(&180_u32.to_be_bytes());

        assert!(validate_uploaded_screen_capture_image(&context, "image/png", &png).is_ok());
        let mut mismatched = context;
        mismatched.image_size = Some(
            magician::magician_v2::chat::models::ScreenCaptureImageSize {
                width: 321,
                height: 180,
            },
        );
        assert!(validate_uploaded_screen_capture_image(&mismatched, "image/png", &png).is_err());
        assert!(
            validate_uploaded_screen_capture_image(&mismatched, "text/plain", b"not image")
                .is_err()
        );
    }

    #[test]
    fn validate_send_message_request_still_requires_text_in_plan_mode() {
        let request = SendMessageRequest {
            text: None,
            attachment_ids: vec!["attachment-1".to_string()],
            mode: ChatMessageMode::Plan,
            plan_task_id: None,
            plan_question_id: None,
            profile: None,
            harness_engine: None,
            harness_model: None,
            chat_turn_id: None,
            source_surface: None,
            presence_session_id: None,
            sender_display_name: None,
            voice_origin: false,
            continue_on_disconnect: false,
            coding_choice: None,
        };

        let response =
            validate_send_message_request(&request).expect_err("plan mode still requires text");

        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_send_message_request_rejects_plan_question_without_task() {
        let request = SendMessageRequest {
            text: Some("Answer".to_string()),
            attachment_ids: Vec::new(),
            mode: ChatMessageMode::Plan,
            plan_task_id: None,
            plan_question_id: Some("question-1".to_string()),
            profile: None,
            harness_engine: None,
            harness_model: None,
            chat_turn_id: None,
            source_surface: None,
            presence_session_id: None,
            sender_display_name: None,
            voice_origin: false,
            continue_on_disconnect: false,
            coding_choice: None,
        };

        let response = validate_send_message_request(&request)
            .expect_err("plan question routing should require a task id");

        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn validate_send_message_request_rejects_plan_target_in_ask_mode() {
        let request = SendMessageRequest {
            text: Some("This should remain ordinary chat".to_string()),
            attachment_ids: Vec::new(),
            mode: ChatMessageMode::Ask,
            plan_task_id: Some("task-1".to_string()),
            plan_question_id: Some("question-1".to_string()),
            profile: None,
            harness_engine: None,
            harness_model: None,
            chat_turn_id: None,
            source_surface: None,
            presence_session_id: None,
            sender_display_name: None,
            voice_origin: false,
            continue_on_disconnect: false,
            coding_choice: None,
        };

        let response = validate_send_message_request(&request)
            .expect_err("plan targeting metadata must not be ignored in ask mode");

        assert_eq!(response.status(), actix_web::http::StatusCode::BAD_REQUEST);
    }

    /// One JSONL row of roughly `payload_bytes`, tagged with `index` so the test
    /// can tell which rows survived the clip.
    fn turn_event_row(index: usize, payload_bytes: usize) -> String {
        format!(
            "{}\n",
            serde_json::json!({ "i": index, "pad": "x".repeat(payload_bytes) })
        )
    }

    #[tokio::test]
    async fn turn_events_read_is_byte_bounded_and_honours_the_limit() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("turn.jsonl");

        // ~4 MB across 400 rows with 10 KB payloads: the shape of the real voice
        // and tutor turns (the largest on disk is 4,462,931 bytes over 440 rows).
        const ROWS: usize = 400;
        let mut file = String::new();
        for index in 0..ROWS {
            file.push_str(&turn_event_row(index, 10_000));
        }
        let file_bytes = file.len() as u64;
        assert!(
            file_bytes > CHAT_TURN_EVENTS_TAIL_MAX_BYTES * 3,
            "fixture must comfortably exceed the ceiling, got {file_bytes} bytes"
        );
        tokio::fs::write(&path, &file).await.expect("write fixture");

        let tail = read_chat_turn_events_tail(&path, CHAT_TURN_EVENTS_TAIL_MAX_BYTES)
            .await
            .expect("tail read")
            .expect("file exists");

        // Bounded bytes: the handler never materializes the whole file.
        assert!(
            (tail.text.len() as u64) <= CHAT_TURN_EVENTS_TAIL_MAX_BYTES,
            "read {} bytes, ceiling is {CHAT_TURN_EVENTS_TAIL_MAX_BYTES}",
            tail.text.len()
        );
        assert!(tail.clipped_by_bytes);

        let limit = 50;
        let (events, scanned) = newest_chat_turn_events(&tail.text, limit);
        assert_eq!(events.len(), limit, "the limit is honoured, not ignored");
        assert!(scanned >= limit);

        // The rows returned are the newest ones, contiguous and ending at the
        // file's last row.
        assert_eq!(events.last().unwrap()["i"], serde_json::json!(ROWS - 1));
        assert_eq!(events[0]["i"], serde_json::json!(ROWS - limit));
    }

    #[tokio::test]
    async fn turn_events_tail_drops_the_partial_leading_row() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("turn.jsonl");

        let mut file = String::new();
        for index in 0..8 {
            file.push_str(&turn_event_row(index, 100));
        }
        tokio::fs::write(&path, &file).await.expect("write fixture");

        // A window deliberately too small to start on a row boundary.
        let tail = read_chat_turn_events_tail(&path, 300)
            .await
            .expect("tail read")
            .expect("file exists");
        assert!(tail.clipped_by_bytes);

        // The window opens mid-row; the truncated leader must be gone rather than
        // left for the parser to reject, so every retained line is whole JSON.
        for line in tail.text.lines() {
            serde_json::from_str::<serde_json::Value>(line.trim())
                .unwrap_or_else(|error| panic!("retained a partial row ({error}): {line:.60}"));
        }

        let (events, scanned) = newest_chat_turn_events(&tail.text, 100);
        assert_eq!(events.len(), scanned);
        assert!(!events.is_empty(), "the window must still yield whole rows");
        assert_eq!(events.last().unwrap()["i"], serde_json::json!(7));
    }

    #[tokio::test]
    async fn turn_events_tail_reports_a_missing_file_as_absent() {
        let dir = tempdir().expect("tempdir");
        let missing = dir.path().join("nope.jsonl");
        let tail = read_chat_turn_events_tail(&missing, CHAT_TURN_EVENTS_TAIL_MAX_BYTES)
            .await
            .expect("missing file is not an error");
        assert!(tail.is_none());
    }

    #[test]
    fn small_turns_are_returned_whole_and_unmarked() {
        let mut file = String::new();
        for index in 0..5 {
            file.push_str(&turn_event_row(index, 10));
        }
        let (events, scanned) = newest_chat_turn_events(&file, 50);
        assert_eq!(events.len(), 5);
        assert_eq!(scanned, 5);
        assert_eq!(events[0]["i"], serde_json::json!(0));
    }

    #[test]
    fn a_zero_limit_is_floored_rather_than_answered_with_nothing() {
        let mut file = String::new();
        for index in 0..3 {
            file.push_str(&turn_event_row(index, 10));
        }
        let (events, scanned) = newest_chat_turn_events(&file, 0);
        assert_eq!(scanned, 3);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["i"], serde_json::json!(2));
    }
}

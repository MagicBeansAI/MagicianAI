use actix_web::{web, HttpRequest, HttpResponse, Responder};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::Arc;
use tracing::{debug, error};

use crate::chat_api::ChatApi;
use crate::scope::{resolve_optional_principal, resolve_required_workspace};
use magician::magician_v2::agents::{FeatureMode, InvocationSurface};
use magician::magician_v2::artifact_v2::models::{TaskLifecycle, TaskOutputMode, TaskSyncMode};
use magician::magician_v2::artifact_v2::{ArtifactV2Error, ArtifactV2Service, CreateTaskInput};
use magician::magician_v2::chat::lane_seam::{
    THINKING_MAP_PRODUCT_SOURCE_KEY, THINKING_MAP_THREAD_ID,
};
use magician::magician_v2::chat::models::{
    ChatChannel, ChatMessageMode, ChatResponse, ChatSession, ChatSessionStatus,
    ScreenCaptureAttachmentContext, ScreenCaptureImageSize, ScreenCaptureScreenRect,
};
use magician::magician_v2::chat::service::{
    extract_chat_bad_request_message, SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE,
    SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL,
};
use magician_media::media_rails::screen_capture::png_image_size;

const DEFAULT_CONTEXTUAL_WRITING_AGENT: &str = "writing-assistant";
const DEFAULT_CONTEXTUAL_WRITING_THREAD_PREFIX: &str = "contextual-writing-";
const MAX_CONTEXTUAL_SCREENSHOT_BYTES: usize = 20 * 1024 * 1024;
const MAX_CONTEXTUAL_TASK_TITLE_CHARS: usize = 120;

/// Closed vocabulary for `context.state`. Clients (desktop assist, iOS
/// keyboard/Action extension, browser flow, Thinking Map frontier) must send
/// one of these; anything else fails validation instead of flowing through
/// as an unvalidated free string.
const CONTEXTUAL_WRITING_STATES: &[&str] = &[
    "selection",
    "selection-field",
    "draft",
    "empty-context",
    "empty-no-context",
    "page-context",
    "files",
    "secure",
    "excluded",
    "unsupported",
    "thinking_map",
];

/// Closed vocabulary for `routing.targetTextKind`.
const CONTEXTUAL_WRITING_TARGET_TEXT_KINDS: &[&str] = &[
    "selected_text",
    "field_text",
    "screen_context",
    "page_url",
    "file_paths",
    "none",
    "thinking_graph",
];

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub user_prompt: Option<String>,
    pub action: ContextualWritingAction,
    pub context: ContextualWritingContext,
    pub routing: ContextualWritingRouting,
    #[serde(default)]
    pub visual_context: Option<ContextualWritingVisualContext>,
    #[serde(default)]
    pub screenshot: Option<ContextualWritingScreenshotPayload>,
    #[serde(default)]
    pub reuse_screenshot_attachment_id: Option<String>,
    /// Client-supplied turn id (exact parity with chat's `chat_turn_id`), so a
    /// client that knows `(sessionId, chatTurnId)` can tail
    /// `GET /chat/sessions/{sid}/turns/{tid}/events/stream` for honest stage
    /// labels during the synchronous wait. Minted server-side when absent.
    #[serde(default)]
    pub chat_turn_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingAction {
    pub id: String,
    pub label: String,
    pub intent: String,
    #[serde(default)]
    pub requires_screenshot: bool,
    #[serde(default)]
    pub creates_task: bool,
    #[serde(default)]
    pub opens_hud: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingContext {
    pub state: String,
    pub personality: String,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub window_title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    /// Specific frame URL when the target lives inside an iframe distinct
    /// from the top-level page URL (browser extension flow).
    #[serde(default)]
    pub frame_url: Option<String>,
    #[serde(default)]
    pub context_text: Option<String>,
    #[serde(default)]
    pub has_context_text: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingRouting {
    pub agent_id: String,
    /// Typed protected feature binding. Free-form source keys are provenance
    /// only and never grant access to a surface-only agent.
    #[serde(default)]
    pub surface: Option<InvocationSurface>,
    #[serde(default)]
    pub feature_mode: Option<FeatureMode>,
    pub source_kind: String,
    pub source_key: String,
    pub session_key: String,
    /// Stable user-visible chat thread. When omitted, the legacy behavior keeps
    /// deriving a private thread id from `session_key`.
    #[serde(default)]
    pub thread_id: Option<String>,
    /// Exact durable chat session to continue. This prevents a feature surface
    /// with multiple live sessions in one thread from accidentally attaching a
    /// turn to whichever session happened to update most recently.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional title chosen by the owning surface for the durable session.
    #[serde(default)]
    pub session_title: Option<String>,
    #[serde(default)]
    pub root_url: Option<String>,
    pub target_text_kind: String,
    pub action_intent: String,
    pub personality: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingSessionRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    pub thread_id: String,
    pub agent_id: String,
    #[serde(default)]
    pub surface: Option<InvocationSurface>,
    #[serde(default)]
    pub feature_mode: Option<FeatureMode>,
    pub source_key: String,
    pub title: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingSessionResponse {
    pub status: &'static str,
    pub session_id: String,
    pub thread_id: String,
    pub session_title: String,
}

fn resolve_contextual_agent_binding(
    requested_agent_id: &str,
    surface: Option<InvocationSurface>,
    feature_mode: Option<FeatureMode>,
) -> Result<(&str, InvocationSurface, FeatureMode), String> {
    let surface = surface.unwrap_or(InvocationSurface::ContextualAssist);
    let feature_mode = feature_mode.unwrap_or_default();
    if !magician::magician_v2::execution::agentic::feature_surface_is_authorized(
        feature_mode,
        surface,
    ) {
        return Err("featureMode is not authorized for the requested surface".to_string());
    }
    if feature_mode == FeatureMode::Brainstorm {
        let bound = magician::magician_v2::execution::agentic::feature_agent_id(feature_mode)
            .ok_or_else(|| "feature has no registered agent binding".to_string())?;
        if !requested_agent_id.trim().is_empty() {
            return Err(
                "client agentId must be omitted for a protected feature binding".to_string(),
            );
        }
        return Ok((bound, surface, feature_mode));
    }
    if feature_mode != FeatureMode::None {
        return Err(
            "Tutor and App Copilot require their dedicated authenticated product routes"
                .to_string(),
        );
    }
    if requested_agent_id.trim() == "brainstorm-facilitator" {
        return Err(
            "brainstorm-facilitator can only be entered through featureMode=brainstorm on \
             surface=thinking_map"
                .to_string(),
        );
    }
    let agent_id = if requested_agent_id.trim().is_empty() {
        DEFAULT_CONTEXTUAL_WRITING_AGENT
    } else {
        requested_agent_id.trim()
    };
    Ok((agent_id, surface, feature_mode))
}

fn contextual_origin(surface: InvocationSurface, source_key: &str) -> ChatChannel {
    if surface == InvocationSurface::ThinkingMap {
        ChatChannel::new("thinking_map", source_key)
    } else {
        ChatChannel::new("contextual-assist", source_key)
    }
}

fn validate_contextual_feature_contract(
    request: &ContextualWritingRequest,
    surface: InvocationSurface,
    feature_mode: FeatureMode,
) -> Result<(), String> {
    if feature_mode != FeatureMode::Brainstorm {
        return Ok(());
    }
    if surface != InvocationSurface::ThinkingMap
        || request.action.id.trim() != "thinking_map_frontier"
        || request.context.state.trim() != "thinking_map"
        || request.routing.target_text_kind.trim() != "thinking_graph"
        || request.routing.source_kind.trim() != "app"
        || request.routing.source_key.trim() != THINKING_MAP_PRODUCT_SOURCE_KEY
        || request
            .routing
            .thread_id
            .as_deref()
            .map(normalize_thread_id)
            .as_deref()
            != Some(THINKING_MAP_THREAD_ID)
        || request
            .routing
            .session_id
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_none()
        || request.action.requires_screenshot
        || request.screenshot.is_some()
        || request
            .reuse_screenshot_attachment_id
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        || request.visual_context.is_some()
        || request.action.creates_task
        || request.action.opens_hud
    {
        return Err(
            "brainstorm is authorized only for an exact, durable Thinking Map frontier request"
                .to_string(),
        );
    }
    Ok(())
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingVisualContext {
    pub screenshot: ContextualWritingScreenshotRequest,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingScreenshotRequest {
    pub policy: String,
    pub required: bool,
    pub source: String,
    pub reason: String,
    pub degraded: bool,
    #[serde(default)]
    pub window_rect: Option<ContextualWritingScreenRect>,
    #[serde(default)]
    pub browser_tab_id: Option<i64>,
    #[serde(default)]
    pub browser_window_id: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingScreenRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingScreenshotPayload {
    pub image_b64: String,
    pub mime_type: String,
    pub capture_mode: String,
    pub source: String,
    #[serde(default)]
    pub degraded: bool,
    #[serde(default)]
    pub window_rect: Option<ContextualWritingScreenRect>,
    #[serde(default)]
    pub browser_tab_id: Option<i64>,
    #[serde(default)]
    pub browser_window_id: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingResponse {
    pub status: String,
    pub session_id: String,
    pub thread_id: String,
    /// Echo of the (client-supplied or minted) turn id — for turn-event
    /// tailing.
    pub chat_turn_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub draft_text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_attachment_id: Option<String>,
    /// Present when a `creates_task` action produced a draft and the task
    /// was durably created. Absent when creation failed (the draft is still
    /// returned) or the action was queued.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    pub provenance: ContextualWritingProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queued: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingProvenance {
    pub generated_at: String,
    pub action: ContextualWritingActionProvenance,
    pub source: ContextualWritingSourceProvenance,
    pub target: ContextualWritingTargetProvenance,
    pub persona: ContextualWritingPersonaProvenance,
    pub visual: ContextualWritingVisualProvenance,
    pub memory: ContextualWritingMemoryProvenance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<ContextualWritingUsageProvenance>,
    pub chat: ContextualWritingProvenanceChat,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingActionProvenance {
    pub id: String,
    pub label: String,
    pub intent: String,
    pub user_guidance_supplied: bool,
    pub creates_task: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingSourceProvenance {
    pub kind: String,
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_url: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingTargetProvenance {
    pub state: String,
    pub text_kind: String,
    pub has_context_text: bool,
    pub context_text_chars: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_text_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingPersonaProvenance {
    pub agent_id: String,
    pub personality: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingVisualProvenance {
    pub screenshot_policy: String,
    pub screenshot_required: bool,
    pub screenshot_supplied: bool,
    pub screenshot_reused: bool,
    pub screenshot_source: String,
    pub screenshot_degraded: bool,
    pub screenshot_reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_attachment_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingMemoryProvenance {
    pub agent_scoped_memory: bool,
    pub shared_user_memory_allowed: bool,
    pub durable_memory_contents_exposed: bool,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingUsageProvenance {
    pub calls: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub reasoning_tokens: u32,
    pub cache_read_tokens: u32,
    pub cache_creation_tokens: u32,
    pub total_tokens: u32,
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_availability: Option<magicllm::types::UsageAvailability>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingProvenanceChat {
    pub session_id: String,
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_message_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assistant_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screenshot_attachment_id: Option<String>,
}

/// POST /api/magician/v2/contextual-writing/sessions
///
/// Allocate a durable, explicitly-agent-owned session before a potentially
/// long contextual generation turn begins. A caller can persist the returned
/// id immediately, so cancellation/retry cannot manufacture another session.
/// Non-rotating user threads intentionally retain every active session until
/// the user explicitly archives or deletes one.
pub async fn create_contextual_writing_session_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    body: web::Json<ContextualWritingSessionRequest>,
) -> impl Responder {
    let workspace = match resolve_required_workspace(req.headers(), body.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match crate::chat_api::resolve_principal(
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

    let (agent_id, surface, feature_mode) =
        match resolve_contextual_agent_binding(&body.agent_id, body.surface, body.feature_mode) {
            Ok(binding) => binding,
            Err(error) => return HttpResponse::Forbidden().json(json!({ "error": error })),
        };
    let source_key = body.source_key.trim();
    let title = body.title.split_whitespace().collect::<Vec<_>>().join(" ");
    if agent_id.is_empty() || source_key.is_empty() || title.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "threadId, agentId, sourceKey, and title are required",
        }));
    }
    if feature_mode == FeatureMode::Brainstorm && source_key != THINKING_MAP_PRODUCT_SOURCE_KEY {
        return HttpResponse::Forbidden().json(json!({
            "error": "brainstorm sessions require the registered iOS Thinking Map product source",
        }));
    }
    let thread_id = normalize_thread_id(&body.thread_id);
    if thread_id == format!("{DEFAULT_CONTEXTUAL_WRITING_THREAD_PREFIX}unknown") {
        return HttpResponse::BadRequest().json(json!({ "error": "threadId is required" }));
    }
    if feature_mode == FeatureMode::Brainstorm && thread_id != THINKING_MAP_THREAD_ID {
        return HttpResponse::Forbidden().json(json!({
            "error": "brainstorm sessions must use the registered Brainstorming thread",
        }));
    }
    let origin = contextual_origin(surface, source_key);
    let session = match chat_api
        .chat_service
        .new_automated_session_with_agent_override(
            &principal,
            &workspace,
            &thread_id,
            &origin,
            Some(agent_id),
        )
        .await
    {
        Ok(session) => session,
        Err(error) => {
            error!(%error, "[CONTEXTUAL-WRITING] failed to create durable session");
            return HttpResponse::InternalServerError().json(json!({
                "error": "Failed to create contextual writing session",
                "details": error.to_string(),
            }));
        },
    };
    let session_title = compact_title(&title, 96);
    if let Err(error) = chat_api
        .chat_service
        .chat_store_ref()
        .update_session_title(&session.id, &session_title)
        .await
    {
        error!(%error, session_id = %session.id, "[CONTEXTUAL-WRITING] failed to title durable session");
        let _ = chat_api
            .chat_service
            .chat_store_ref()
            .delete_session(&session.id)
            .await;
        return HttpResponse::InternalServerError().json(json!({
            "error": "Failed to title contextual writing session",
            "details": error.to_string(),
        }));
    }

    HttpResponse::Created().json(ContextualWritingSessionResponse {
        status: "ready",
        session_id: session.id,
        thread_id,
        session_title,
    })
}

/// POST /api/magician/v2/contextual-writing/actions
pub async fn contextual_writing_action_handler(
    chat_api: web::Data<ChatApi>,
    artifact_service: web::Data<Arc<ArtifactV2Service>>,
    req: HttpRequest,
    body: web::Json<ContextualWritingRequest>,
) -> impl Responder {
    let workspace = match resolve_required_workspace(req.headers(), body.workspace.clone()) {
        Ok(workspace) => workspace,
        Err(response) => return response,
    };
    let requested_principal = resolve_optional_principal(req.headers());
    let principal = match crate::chat_api::resolve_principal(
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

    if let Err(message) = validate_request(&body) {
        return HttpResponse::BadRequest().json(json!({ "error": message }));
    }

    let thread_id = body
        .routing
        .thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_thread_id)
        .unwrap_or_else(|| normalize_thread_id(&body.routing.session_key));
    let (agent_id, surface, feature_mode) = match resolve_contextual_agent_binding(
        &body.routing.agent_id,
        body.routing.surface,
        body.routing.feature_mode,
    ) {
        Ok(binding) => binding,
        Err(error) => return HttpResponse::Forbidden().json(json!({ "error": error })),
    };
    if let Err(error) = validate_contextual_feature_contract(&body, surface, feature_mode) {
        return HttpResponse::Forbidden().json(json!({ "error": error }));
    }
    let origin = contextual_origin(surface, &body.routing.source_key);

    debug!(
        principal,
        workspace,
        thread_id,
        action = %body.action.id,
        agent_id,
        "[CONTEXTUAL-WRITING] handling action"
    );

    let requested_session_id = body
        .routing
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let session = if let Some(session_id) = requested_session_id {
        match chat_api.chat_service.get_session(session_id).await {
            Ok(Some(session)) => {
                // Return NotFound for an out-of-scope id so this endpoint cannot
                // be used as a session-existence oracle across users/workspaces.
                if session.principal != principal || session.workspace != workspace {
                    return HttpResponse::NotFound()
                        .json(json!({ "error": "Contextual writing session not found" }));
                }
                if session.ui_thread_id != thread_id {
                    return HttpResponse::Conflict().json(json!({
                        "error": "Contextual writing session belongs to a different thread",
                    }));
                }
                if session.agent_id != agent_id {
                    return HttpResponse::Conflict().json(json!({
                        "error": "Contextual writing session belongs to a different agent",
                    }));
                }
                session
            },
            Ok(None) => {
                return HttpResponse::NotFound()
                    .json(json!({ "error": "Contextual writing session not found" }));
            },
            Err(error) => {
                error!(%error, session_id, "[CONTEXTUAL-WRITING] failed to load exact session");
                return HttpResponse::InternalServerError().json(json!({
                    "error": "Failed to load contextual writing session",
                    "details": error.to_string(),
                }));
            },
        }
    } else {
        match chat_api
            .chat_service
            .get_or_create_automated_session(
                &principal,
                &workspace,
                &thread_id,
                &origin,
                Some(agent_id),
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                error!(%error, "[CONTEXTUAL-WRITING] failed to get/create session");
                return HttpResponse::InternalServerError().json(json!({
                    "error": "Failed to get or create contextual writing session",
                    "details": error.to_string(),
                }));
            },
        }
    };

    // `get_or_create_session` may return an older active session for the same
    // agent/thread. Typed feature authorization must not silently upgrade that
    // session's provenance; legacy Loom sessions remain readable/archiveable
    // but cannot accept a new turn.
    if feature_mode == FeatureMode::Brainstorm
        && (session.origin_channel.channel_type != "thinking_map"
            || session.origin_channel.address.as_deref() != Some(body.routing.source_key.trim()))
    {
        return HttpResponse::Conflict().json(json!({
            "error": "Legacy or mismatched Loom session is readable but cannot accept turns outside its exact typed Thinking Map origin",
        }));
    }

    if session.status == ChatSessionStatus::Archived {
        return HttpResponse::Conflict().json(json!({
            "code": "contextual_session_stale",
            "error": "Cannot send contextual writing actions to an archived session",
        }));
    }

    let session_title = contextual_session_title(&body);
    if session.title.as_deref() != Some(session_title.as_str()) {
        let _ = chat_api
            .chat_service
            .chat_store_ref()
            .update_session_title(&session.id, &session_title)
            .await;
    }

    let mut attachment_ids = Vec::new();
    let mut screenshot_attachment_id = None;
    if let Some(screenshot) = body.screenshot.as_ref() {
        match store_screenshot_attachment(chat_api.get_ref(), &session, screenshot).await {
            Ok(attachment_id) => {
                screenshot_attachment_id = Some(attachment_id.clone());
                attachment_ids.push(attachment_id);
            },
            Err(error) => {
                error!(%error, "[CONTEXTUAL-WRITING] failed to stage screenshot");
                return HttpResponse::InternalServerError().json(json!({
                    "error": "Failed to stage contextual screenshot",
                    "details": error,
                }));
            },
        }
    } else if let Some(attachment_id) = body
        .reuse_screenshot_attachment_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        screenshot_attachment_id = Some(attachment_id.to_string());
        attachment_ids.push(attachment_id.to_string());
    }

    if body
        .visual_context
        .as_ref()
        .map(|visual| visual.screenshot.required)
        .unwrap_or(false)
        && screenshot_attachment_id.is_none()
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "Screenshot context is required for this action but was not supplied",
        }));
    }

    let prompt = build_contextual_writing_prompt(&body, screenshot_attachment_id.as_deref());
    // Use the client-supplied turn id when present (so it can tail turn events),
    // else mint one — exact parity with chat's `chat_turn_id` semantics.
    let chat_turn_id = body
        .chat_turn_id
        .clone()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("contextual-writing-{}", uuid::Uuid::new_v4().simple()));
    let chat_response = match chat_api
        .chat_service
        .process_message_with_mode_on_execution_runtime(
            &session.id,
            Some(prompt.as_str()),
            &attachment_ids,
            None,
            ChatMessageMode::Ask,
            None,
            None,
            Some(&chat_turn_id),
            false,
            Some(surface.as_str()),
            None,
            None,
            None,
        )
        .await
    {
        Ok(response) => response,
        Err(error) => {
            error!(%error, "[CONTEXTUAL-WRITING] failed to process chat turn");
            return contextual_chat_error_response(&error);
        },
    };

    if chat_response.pending_queue_depth.unwrap_or(0) > 0 {
        chat_api
            .chat_service
            .spawn_pending_queue_drain(session.id.clone());
    }

    // Honest createsTask: a flagged action (Task, Follow-up) that produced a
    // draft now creates a real V3 task from it. The draft is extracted
    // before `response_from_chat` consumes the chat response. A task-creation
    // failure must not fail the request — the draft is still returned — so
    // it is logged and taskId is omitted, letting the client fall back to
    // saving the proposal text manually.
    let draft_for_task = chat_response
        .assistant_message
        .as_ref()
        .and_then(|message| message.content.text_content())
        .map(ToOwned::to_owned)
        .filter(|text| !text.trim().is_empty());
    let task_id = match draft_for_task
        .as_deref()
        .filter(|_| body.action.creates_task)
    {
        Some(draft_text) => {
            match create_task_from_contextual_draft(
                artifact_service.get_ref(),
                &session,
                &principal,
                &workspace,
                &thread_id,
                &agent_id,
                draft_text,
            )
            .await
            {
                Ok(task_id) => Some(task_id),
                Err(task_error) => {
                    // Debug capture: the error type's Display impl is not
                    // worth assuming without a compiler in the loop.
                    error!(
                        action = %body.action.id,
                        "[CONTEXTUAL-WRITING] draft produced but task creation failed: {task_error:?}"
                    );
                    None
                },
            }
        },
        None => None,
    };

    HttpResponse::Ok().json(response_from_chat(
        &body,
        &session,
        Some(session_title),
        attachment_ids,
        screenshot_attachment_id,
        chat_response,
        chat_turn_id,
        task_id,
    ))
}

fn validate_request(request: &ContextualWritingRequest) -> Result<(), String> {
    if request.action.id.trim().is_empty() {
        return Err("action.id is required".to_string());
    }
    if request.action.opens_hud {
        return Err("HUD actions are not contextual writing generation actions".to_string());
    }
    if request.routing.session_key.trim().is_empty() {
        return Err("routing.sessionKey is required".to_string());
    }
    if request.context.personality.trim().is_empty() {
        return Err("context.personality is required".to_string());
    }
    let state = request.context.state.trim();
    if !CONTEXTUAL_WRITING_STATES.contains(&state) {
        return Err(format!(
            "unknown context.state `{state}` — expected one of: {}",
            CONTEXTUAL_WRITING_STATES.join(", ")
        ));
    }
    let target_text_kind = request.routing.target_text_kind.trim();
    if !CONTEXTUAL_WRITING_TARGET_TEXT_KINDS.contains(&target_text_kind) {
        return Err(format!(
            "unknown routing.targetTextKind `{target_text_kind}` — expected one of: {}",
            CONTEXTUAL_WRITING_TARGET_TEXT_KINDS.join(", ")
        ));
    }
    if !request.context.has_context_text
        && request
            .context
            .context_text
            .as_deref()
            .unwrap_or("")
            .trim()
            .is_empty()
        && request
            .visual_context
            .as_ref()
            .map(|visual| !visual.screenshot.required)
            .unwrap_or(true)
    {
        return Err("text context is required for text-only contextual writing".to_string());
    }
    Ok(())
}

fn normalize_thread_id(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return format!("{DEFAULT_CONTEXTUAL_WRITING_THREAD_PREFIX}unknown");
    }
    slugify_thread_id(trimmed)
        .unwrap_or_else(|| format!("{DEFAULT_CONTEXTUAL_WRITING_THREAD_PREFIX}unknown"))
}

fn slugify_thread_id(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut last_was_dash = false;
    for ch in value.trim().to_lowercase().chars() {
        let next = if ch.is_ascii_lowercase() || ch.is_ascii_digit() {
            ch
        } else {
            '-'
        };
        if next == '-' {
            if output.is_empty() || last_was_dash {
                continue;
            }
            last_was_dash = true;
        } else {
            last_was_dash = false;
        }
        output.push(next);
    }
    while output.ends_with('-') {
        output.pop();
    }
    if output.is_empty() {
        None
    } else {
        Some(output)
    }
}

fn contextual_session_title(request: &ContextualWritingRequest) -> String {
    if let Some(title) = request
        .routing
        .session_title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return compact_title(title, 96);
    }
    let source = request
        .routing
        .root_url
        .as_deref()
        .or(request.context.url.as_deref())
        .or(request.context.window_title.as_deref())
        .or(request.context.app.as_deref())
        .unwrap_or("Contextual Writing");
    format!("Writing - {}", compact_title(source, 64))
}

fn compact_title(value: &str, max_chars: usize) -> String {
    let compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    let compact = compact.trim();
    if compact.chars().count() <= max_chars {
        return compact.to_string();
    }
    compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>()
        + "..."
}

async fn store_screenshot_attachment(
    chat_api: &ChatApi,
    session: &ChatSession,
    screenshot: &ContextualWritingScreenshotPayload,
) -> Result<String, String> {
    let bytes = BASE64
        .decode(screenshot.image_b64.as_bytes())
        .map_err(|error| format!("invalid screenshot base64: {error}"))?;
    if bytes.is_empty() {
        return Err("screenshot payload is empty".to_string());
    }
    if bytes.len() > MAX_CONTEXTUAL_SCREENSHOT_BYTES {
        return Err(format!(
            "screenshot too large. Maximum {} bytes allowed.",
            MAX_CONTEXTUAL_SCREENSHOT_BYTES
        ));
    }
    let filename = format!(
        "contextual-writing-{}.png",
        Utc::now().format("%Y%m%d-%H%M%S")
    );
    let context = screenshot_attachment_context(screenshot, &bytes);
    let record = chat_api
        .chat_service
        .store_attachment_with_context(
            &session.id,
            &filename,
            &screenshot.mime_type,
            &bytes,
            Some("Context screenshot".to_string()),
            context,
        )
        .await
        .map_err(|error| error.to_string())?;
    Ok(record.id)
}

fn screenshot_attachment_context(
    screenshot: &ContextualWritingScreenshotPayload,
    bytes: &[u8],
) -> Option<ScreenCaptureAttachmentContext> {
    if !screenshot.mime_type.starts_with("image/") {
        return None;
    }
    let image_size =
        png_image_size(bytes).map(|(width, height)| ScreenCaptureImageSize { width, height });
    let screen_rect = screenshot
        .window_rect
        .as_ref()
        .map(|rect| ScreenCaptureScreenRect {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        });
    let coordinate_space = if screen_rect.is_some() {
        SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE
    } else {
        SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL
    };
    Some(ScreenCaptureAttachmentContext {
        // Contextual/share-extension images are client uploads. Preserve their
        // geometry for analysis, but never mint the host-capture attestation
        // required to enter App Copilot's UI-mutation lane.
        server_registered: false,
        mode: screenshot.capture_mode.clone(),
        coordinate_space: coordinate_space.to_string(),
        image_size,
        screen_rect,
    })
}

fn build_contextual_writing_prompt(
    request: &ContextualWritingRequest,
    screenshot_attachment_id: Option<&str>,
) -> String {
    let mut lines = Vec::new();
    lines.push("Contextual writing request.".to_string());
    lines.push(
        "Return only the draft text the user can use. Do not include explanations, markdown \
         fences, or labels unless the requested text itself requires them."
            .to_string(),
    );
    lines.push(format!("Action: {}", request.action.label.trim()));
    lines.push(format!(
        "Action intent: {}",
        request.routing.action_intent.trim()
    ));
    lines.push(format!(
        "Selected personality: {}",
        request.context.personality.trim()
    ));
    lines.push(format!(
        "Target text kind: {}",
        request.routing.target_text_kind.trim()
    ));
    lines.push(format!(
        "Source kind: {}",
        request.routing.source_kind.trim()
    ));
    lines.push(format!("Source key: {}", request.routing.source_key.trim()));
    if let Some(app) = request
        .context
        .app
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("App: {app}"));
    }
    if let Some(title) = request
        .context
        .window_title
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("Window/title: {title}"));
    }
    if let Some(url) = request
        .context
        .url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("URL: {url}"));
    }
    if let Some(frame_url) = request
        .context
        .frame_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("Frame URL: {frame_url}"));
    }
    if let Some(root_url) = request
        .routing
        .root_url
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push(format!("Root URL: {root_url}"));
    }
    if let Some(text) = request
        .context
        .context_text
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push("\nText context:".to_string());
        lines.push(text.to_string());
    }
    if let Some(user_prompt) = request
        .user_prompt
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        lines.push("\nUser guidance:".to_string());
        lines.push(user_prompt.to_string());
    }
    if let Some(visual) = request.visual_context.as_ref() {
        lines.push("\nVisual context policy:".to_string());
        lines.push(format!(
            "screenshot policy={}, required={}, source={}, degraded={}, reason={}",
            visual.screenshot.policy,
            visual.screenshot.required,
            visual.screenshot.source,
            visual.screenshot.degraded,
            visual.screenshot.reason
        ));
    }
    if let Some(attachment_id) = screenshot_attachment_id {
        lines.push(format!(
            "Screenshot attachment id: {attachment_id}. Use it as surrounding app/page context."
        ));
    }
    if request.action.creates_task {
        lines.push(
            "If this is a task/follow-up action, draft a concise task proposal. The first \
             line becomes the task title; the full draft becomes the description, and the \
             task is saved on your behalf."
                .to_string(),
        );
    }
    lines.join("\n")
}

fn response_from_chat(
    request: &ContextualWritingRequest,
    session: &ChatSession,
    session_title: Option<String>,
    attachment_ids: Vec<String>,
    screenshot_attachment_id: Option<String>,
    response: ChatResponse,
    chat_turn_id: String,
    task_id: Option<String>,
) -> ContextualWritingResponse {
    let draft_text = response
        .assistant_message
        .as_ref()
        .and_then(|message| message.content.text_content())
        .map(ToOwned::to_owned);
    let resolved_session_title = response.session_title.clone().or(session_title);
    let chat = ContextualWritingProvenanceChat {
        session_id: session.id.clone(),
        thread_id: session.ui_thread_id.clone(),
        session_title: resolved_session_title.clone(),
        user_message_id: response
            .user_message
            .as_ref()
            .map(|message| message.id.clone()),
        assistant_message_id: response
            .assistant_message
            .as_ref()
            .map(|message| message.id.clone()),
        attachment_ids: attachment_ids.clone(),
        screenshot_attachment_id: screenshot_attachment_id.clone(),
    };
    let usage = response.usage.clone();
    let provenance = build_contextual_writing_provenance(request, chat.clone());
    let queued = response
        .queued
        .and_then(|queued| serde_json::to_value(queued).ok());
    ContextualWritingResponse {
        status: if queued.is_some() {
            "queued".to_string()
        } else if draft_text.is_some() {
            "draft_ready".to_string()
        } else {
            "accepted".to_string()
        },
        session_id: session.id.clone(),
        thread_id: session.ui_thread_id.clone(),
        chat_turn_id,
        session_title: resolved_session_title,
        draft_text,
        user_message_id: chat.user_message_id.clone(),
        assistant_message_id: chat.assistant_message_id.clone(),
        attachment_ids,
        screenshot_attachment_id,
        task_id,
        provenance: {
            let mut provenance = provenance;
            provenance.usage = usage.as_ref().map(contextual_usage_from_chat_usage);
            provenance
        },
        queued,
    }
}

/// Create a real V3 task from a `creates_task` draft. The task lands in the
/// contextual thread (so it is trackable where the conversation happened),
/// carries the full draft as its description, and links back to the chat
/// session that produced it.
async fn create_task_from_contextual_draft(
    artifact_service: &ArtifactV2Service,
    session: &ChatSession,
    principal: &str,
    workspace: &str,
    thread_id: &str,
    agent_id: &str,
    draft_text: &str,
) -> Result<String, ArtifactV2Error> {
    let title = contextual_task_title_from_draft(draft_text);
    artifact_service
        .create_task(CreateTaskInput {
            principal: principal.to_string(),
            workspace: workspace.to_string(),
            title,
            description: draft_text.to_string(),
            agent_id: agent_id.to_string(),
            goal_id: None,
            ui_thread_id: thread_id.to_string(),
            priority: None,
            due_date: None,
            tags: Vec::new(),
            created_by: "contextual-assist".to_string(),
            depends_on: Vec::new(),
            approved: true,
            schedule: None,
            output_mode: TaskOutputMode::default(),
            chat_session_id: Some(session.id.clone()),
            lifecycle: TaskLifecycle::default(),
            sync_mode: TaskSyncMode::default(),
        })
        .await
        .map(|task| task.manifest.task_id)
}

/// First non-empty draft line, bounded — the prompt explicitly tells the
/// model the first line becomes the task title.
fn contextual_task_title_from_draft(draft_text: &str) -> String {
    draft_text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("Contextual task")
        .chars()
        .take(MAX_CONTEXTUAL_TASK_TITLE_CHARS)
        .collect()
}

fn build_contextual_writing_provenance(
    request: &ContextualWritingRequest,
    chat: ContextualWritingProvenanceChat,
) -> ContextualWritingProvenance {
    let visual = request
        .visual_context
        .as_ref()
        .map(|visual| &visual.screenshot);
    ContextualWritingProvenance {
        generated_at: Utc::now().to_rfc3339(),
        action: ContextualWritingActionProvenance {
            id: request.action.id.trim().to_string(),
            label: request.action.label.trim().to_string(),
            intent: request.routing.action_intent.trim().to_string(),
            user_guidance_supplied: request
                .user_prompt
                .as_deref()
                .map(str::trim)
                .map(|value| !value.is_empty())
                .unwrap_or(false),
            creates_task: request.action.creates_task,
        },
        source: ContextualWritingSourceProvenance {
            kind: request.routing.source_kind.trim().to_string(),
            key: request.routing.source_key.trim().to_string(),
            app: trimmed_optional(request.context.app.as_deref()),
            window_title: trimmed_optional(request.context.window_title.as_deref()),
            url: trimmed_optional(request.context.url.as_deref()),
            root_url: trimmed_optional(request.routing.root_url.as_deref()),
        },
        target: ContextualWritingTargetProvenance {
            state: request.context.state.trim().to_string(),
            text_kind: request.routing.target_text_kind.trim().to_string(),
            has_context_text: request.context.has_context_text,
            context_text_chars: request
                .context
                .context_text
                .as_deref()
                .map(|text| text.chars().count())
                .unwrap_or(0),
            context_text_preview: preview_text(request.context.context_text.as_deref(), 180),
        },
        persona: ContextualWritingPersonaProvenance {
            agent_id: if request.routing.agent_id.trim().is_empty() {
                DEFAULT_CONTEXTUAL_WRITING_AGENT.to_string()
            } else {
                request.routing.agent_id.trim().to_string()
            },
            personality: request.context.personality.trim().to_string(),
        },
        visual: ContextualWritingVisualProvenance {
            screenshot_policy: visual
                .map(|screenshot| screenshot.policy.trim().to_string())
                .unwrap_or_else(|| "none".to_string()),
            screenshot_required: visual
                .map(|screenshot| screenshot.required)
                .unwrap_or(false),
            screenshot_supplied: chat.screenshot_attachment_id.is_some(),
            screenshot_reused: request.screenshot.is_none()
                && request
                    .reuse_screenshot_attachment_id
                    .as_deref()
                    .map(str::trim)
                    .is_some_and(|value| !value.is_empty())
                && chat.screenshot_attachment_id.is_some(),
            screenshot_source: visual
                .map(|screenshot| screenshot.source.trim().to_string())
                .unwrap_or_else(|| "none".to_string()),
            screenshot_degraded: visual
                .map(|screenshot| screenshot.degraded)
                .unwrap_or(false),
            screenshot_reason: visual
                .map(|screenshot| screenshot.reason.trim().to_string())
                .unwrap_or_else(|| "not_requested".to_string()),
            screenshot_attachment_id: chat.screenshot_attachment_id.clone(),
        },
        memory: ContextualWritingMemoryProvenance {
            agent_scoped_memory: true,
            shared_user_memory_allowed: true,
            durable_memory_contents_exposed: false,
            note: "Writing memory is resolved through the agent runtime; provenance exposes only \
                   routing flags, not memory contents."
                .to_string(),
        },
        usage: None,
        chat,
    }
}

fn contextual_usage_from_chat_usage(
    usage: &magician::magician_v2::chat::models::ChatTurnUsage,
) -> ContextualWritingUsageProvenance {
    ContextualWritingUsageProvenance {
        calls: usage.calls,
        provider: usage.provider.clone(),
        model: usage.model.clone(),
        profile: usage.profile.clone(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_creation_tokens: usage.cache_creation_tokens,
        total_tokens: usage.total_tokens,
        cost_usd: usage.cost_usd,
        usage_availability: usage.usage_availability,
    }
}

fn trimmed_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn preview_text(value: Option<&str>, max_chars: usize) -> Option<String> {
    let text = value.map(str::trim).filter(|value| !value.is_empty())?;
    let mut preview = text.chars().take(max_chars).collect::<String>();
    if text.chars().count() > max_chars {
        preview.push_str("...");
    }
    Some(preview)
}

fn contextual_chat_error_response(error: &anyhow::Error) -> HttpResponse {
    if let Some(message) = extract_chat_bad_request_message(error) {
        return HttpResponse::BadRequest().json(json!({ "error": message }));
    }
    HttpResponse::InternalServerError().json(json!({
        "error": "Failed to process contextual writing action",
        "details": error.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_request(action_id: &str, requires_screenshot: bool) -> ContextualWritingRequest {
        ContextualWritingRequest {
            workspace: Some("default".to_string()),
            user_prompt: None,
            action: ContextualWritingAction {
                id: action_id.to_string(),
                label: "Improve".to_string(),
                intent: "improve_draft".to_string(),
                requires_screenshot,
                creates_task: false,
                opens_hud: false,
            },
            context: ContextualWritingContext {
                state: "draft".to_string(),
                personality: "professional".to_string(),
                app: Some("Notes".to_string()),
                window_title: Some("Draft".to_string()),
                url: None,
                frame_url: None,
                context_text: Some("hello".to_string()),
                has_context_text: true,
            },
            routing: ContextualWritingRouting {
                agent_id: DEFAULT_CONTEXTUAL_WRITING_AGENT.to_string(),
                source_kind: "app".to_string(),
                source_key: "app:Notes".to_string(),
                session_key: "contextual-writing:app-notes".to_string(),
                thread_id: None,
                session_id: None,
                session_title: None,
                root_url: None,
                target_text_kind: "field_text".to_string(),
                action_intent: "improve_draft".to_string(),
                personality: "professional".to_string(),
                surface: None,
                feature_mode: None,
            },
            visual_context: Some(ContextualWritingVisualContext {
                screenshot: ContextualWritingScreenshotRequest {
                    policy: if requires_screenshot {
                        "required"
                    } else {
                        "skipped"
                    }
                    .to_string(),
                    required: requires_screenshot,
                    source: if requires_screenshot {
                        "active_window_region"
                    } else {
                        "none"
                    }
                    .to_string(),
                    reason: "test".to_string(),
                    degraded: false,
                    window_rect: None,
                    browser_tab_id: None,
                    browser_window_id: None,
                },
            }),
            screenshot: None,
            reuse_screenshot_attachment_id: None,
            chat_turn_id: None,
        }
    }

    #[test]
    fn contextual_writing_validation_rejects_required_screenshot_without_payload_later() {
        let request = base_request("shorten", false);
        assert!(validate_request(&request).is_ok());
    }

    #[test]
    fn contextual_screenshot_never_mints_host_capture_attestation() {
        let mut png = vec![0_u8; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&320_u32.to_be_bytes());
        png[20..24].copy_from_slice(&180_u32.to_be_bytes());
        let screenshot = ContextualWritingScreenshotPayload {
            image_b64: String::new(),
            mime_type: "image/png".to_string(),
            capture_mode: "screenshot".to_string(),
            source: "ios_share_extension".to_string(),
            degraded: false,
            window_rect: Some(ContextualWritingScreenRect {
                x: 20,
                y: 40,
                width: 320,
                height: 180,
            }),
            browser_tab_id: None,
            browser_window_id: None,
        };

        let context =
            screenshot_attachment_context(&screenshot, &png).expect("image screenshot context");

        assert!(!context.server_registered);
        assert_eq!(
            context.coordinate_space,
            SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE
        );
        assert_eq!(context.image_size.expect("PNG dimensions").width, 320);
    }

    #[test]
    fn thinking_map_feature_binds_loom_server_side() {
        let (agent_id, surface, feature) = resolve_contextual_agent_binding(
            "",
            Some(InvocationSurface::ThinkingMap),
            Some(FeatureMode::Brainstorm),
        )
        .expect("typed route");
        assert_eq!(agent_id, "brainstorm-facilitator");
        assert_eq!(surface, InvocationSurface::ThinkingMap);
        assert_eq!(feature, FeatureMode::Brainstorm);
    }

    #[test]
    fn direct_or_mismatched_loom_binding_is_rejected() {
        assert!(resolve_contextual_agent_binding("brainstorm-facilitator", None, None,).is_err());
        assert!(resolve_contextual_agent_binding(
            "writing-assistant",
            Some(InvocationSurface::ThinkingMap),
            Some(FeatureMode::Brainstorm),
        )
        .is_err());
        assert!(resolve_contextual_agent_binding(
            "brainstorm-facilitator",
            Some(InvocationSurface::ThinkingMap),
            Some(FeatureMode::Brainstorm),
        )
        .is_err());
        assert!(resolve_contextual_agent_binding(
            "",
            Some(InvocationSurface::ContextualAssist),
            Some(FeatureMode::Brainstorm),
        )
        .is_err());
        assert!(resolve_contextual_agent_binding(
            "writing-assistant",
            Some(InvocationSurface::Tutor),
            Some(FeatureMode::Tutor),
        )
        .is_err());
    }

    #[test]
    fn brainstorm_contract_rejects_generic_or_side_effecting_contextual_requests() {
        let mut request = base_request("thinking_map_frontier", false);
        // Thinking Map is deliberately text-only. The ordinary contextual
        // assist fixture carries visual metadata even when capture is skipped,
        // so remove it for the valid typed-product baseline.
        request.visual_context = None;
        request.context.state = "thinking_map".to_string();
        request.routing.agent_id.clear();
        request.routing.surface = Some(InvocationSurface::ThinkingMap);
        request.routing.feature_mode = Some(FeatureMode::Brainstorm);
        request.routing.source_kind = "app".to_string();
        request.routing.source_key = THINKING_MAP_PRODUCT_SOURCE_KEY.to_string();
        request.routing.thread_id = Some(THINKING_MAP_THREAD_ID.to_string());
        request.routing.target_text_kind = "thinking_graph".to_string();
        request.routing.session_id = Some("loom-session-1".to_string());

        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_ok());

        request.action.creates_task = true;
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());
        request.action.creates_task = false;
        request.routing.source_key = "app:ios:other".to_string();
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());
        request.routing.source_key = THINKING_MAP_PRODUCT_SOURCE_KEY.to_string();
        request.routing.thread_id = Some("other-thread".to_string());
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());
        request.routing.thread_id = Some(THINKING_MAP_THREAD_ID.to_string());
        request.routing.session_id = None;
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());

        request.routing.session_id = Some("loom-session-1".to_string());
        request.reuse_screenshot_attachment_id = Some("attachment-1".to_string());
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());

        request.reuse_screenshot_attachment_id = None;
        request.visual_context = Some(ContextualWritingVisualContext {
            screenshot: ContextualWritingScreenshotRequest {
                policy: "skipped".to_string(),
                required: false,
                source: "none".to_string(),
                reason: "client metadata must not widen the product route".to_string(),
                degraded: false,
                window_rect: None,
                browser_tab_id: None,
                browser_window_id: None,
            },
        });
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());

        request.visual_context = None;
        request.screenshot = Some(ContextualWritingScreenshotPayload {
            image_b64: "not-used-by-contract-validation".to_string(),
            mime_type: "image/png".to_string(),
            capture_mode: "screenshot".to_string(),
            source: "ios_share_extension".to_string(),
            degraded: false,
            window_rect: None,
            browser_tab_id: None,
            browser_window_id: None,
        });
        assert!(validate_contextual_feature_contract(
            &request,
            InvocationSurface::ThinkingMap,
            FeatureMode::Brainstorm,
        )
        .is_err());
    }

    #[test]
    fn contextual_writing_thread_id_slugifies_routing_key() {
        assert_eq!(
            normalize_thread_id("contextual-writing:site-http-localhost-5173"),
            "contextual-writing-site-http-localhost-5173"
        );
        assert_eq!(normalize_thread_id("   "), "contextual-writing-unknown");
    }

    #[test]
    fn contextual_writing_explicit_thread_and_title_preserve_feature_session_identity() {
        let mut request = base_request("thinking_map_frontier", false);
        request.routing.thread_id = Some(" Brainstorming ".to_string());
        request.routing.session_id = Some("session-for-this-map".to_string());
        request.routing.session_title = Some("  Brainstorm — Reversible decisions  ".to_string());

        assert_eq!(
            request
                .routing
                .thread_id
                .as_deref()
                .map(str::trim)
                .map(normalize_thread_id),
            Some("brainstorming".to_string())
        );
        assert_eq!(
            contextual_session_title(&request),
            "Brainstorm — Reversible decisions"
        );
        assert_eq!(
            request.routing.session_id.as_deref(),
            Some("session-for-this-map")
        );
    }

    #[test]
    fn contextual_writing_prompt_includes_action_source_and_text() {
        let mut request = base_request("improve_draft", true);
        request.user_prompt = Some("make it warmer but still concise".to_string());
        let prompt = build_contextual_writing_prompt(&request, Some("attachment-1"));

        assert!(prompt.contains("Action intent: improve_draft"));
        assert!(prompt.contains("Selected personality: professional"));
        assert!(prompt.contains("Source key: app:Notes"));
        assert!(prompt.contains("hello"));
        assert!(prompt.contains("User guidance:"));
        assert!(prompt.contains("make it warmer but still concise"));
        assert!(prompt.contains("Screenshot attachment id: attachment-1"));
    }

    #[test]
    fn contextual_writing_provenance_summarizes_context_without_memory_contents() {
        let mut request = base_request("improve_draft", true);
        request.user_prompt = Some("make it warmer".to_string());
        let provenance = build_contextual_writing_provenance(
            &request,
            ContextualWritingProvenanceChat {
                session_id: "session-1".to_string(),
                thread_id: "contextual-writing-app-notes".to_string(),
                session_title: Some("Writing - Notes".to_string()),
                user_message_id: Some("user-1".to_string()),
                assistant_message_id: Some("assistant-1".to_string()),
                attachment_ids: vec!["attachment-1".to_string()],
                screenshot_attachment_id: Some("attachment-1".to_string()),
            },
        );

        assert_eq!(provenance.action.intent, "improve_draft");
        assert!(provenance.action.user_guidance_supplied);
        assert_eq!(provenance.source.app.as_deref(), Some("Notes"));
        assert_eq!(provenance.target.text_kind, "field_text");
        assert_eq!(
            provenance.target.context_text_preview.as_deref(),
            Some("hello")
        );
        assert!(provenance.visual.screenshot_required);
        assert!(provenance.visual.screenshot_supplied);
        assert!(!provenance.visual.screenshot_reused);
        assert_eq!(
            provenance.visual.screenshot_attachment_id.as_deref(),
            Some("attachment-1")
        );
        assert!(provenance.memory.agent_scoped_memory);
        assert!(!provenance.memory.durable_memory_contents_exposed);
        assert_eq!(
            provenance.chat.assistant_message_id.as_deref(),
            Some("assistant-1")
        );
    }

    #[test]
    fn contextual_writing_provenance_marks_reused_screenshot() {
        let mut request = base_request("improve_draft", true);
        request.reuse_screenshot_attachment_id = Some("attachment-1".to_string());

        let provenance = build_contextual_writing_provenance(
            &request,
            ContextualWritingProvenanceChat {
                session_id: "session-1".to_string(),
                thread_id: "contextual-writing-app-notes".to_string(),
                session_title: None,
                user_message_id: None,
                assistant_message_id: None,
                attachment_ids: vec!["attachment-1".to_string()],
                screenshot_attachment_id: Some("attachment-1".to_string()),
            },
        );

        assert!(provenance.visual.screenshot_supplied);
        assert!(provenance.visual.screenshot_reused);
    }

    #[test]
    fn contextual_writing_usage_provenance_preserves_tokens_and_cost() {
        let mut usage = magician::magician_v2::chat::models::ChatTurnUsage::default();
        usage.record_call(
            "openai",
            "gpt-5.6-terra",
            Some("chat-mini".to_string()),
            1200,
            300,
            40,
            900,
            0,
            0.0123,
        );

        let provenance = contextual_usage_from_chat_usage(&usage);

        assert_eq!(provenance.calls, 1);
        assert_eq!(provenance.provider.as_deref(), Some("openai"));
        assert_eq!(provenance.model.as_deref(), Some("gpt-5.6-terra"));
        assert_eq!(provenance.total_tokens, 1540);
        assert_eq!(provenance.cache_read_tokens, 900);
        assert_eq!(provenance.cost_usd, Some(0.0123));
        usage.cost_usd = None;
        usage.usage_availability = Some(Default::default());
        let unknown = serde_json::to_value(contextual_usage_from_chat_usage(&usage)).unwrap();
        assert!(unknown["costUsd"].is_null());
        assert_eq!(unknown["usageAvailability"]["cost"], false);
    }

    #[test]
    fn png_image_size_reads_dimensions() {
        let mut png = vec![0; 24];
        png[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        png[16..20].copy_from_slice(&640_u32.to_be_bytes());
        png[20..24].copy_from_slice(&480_u32.to_be_bytes());

        assert_eq!(png_image_size(&png), Some((640, 480)));
    }
}

// ─── Action catalog (single source for all contextual clients) ─────────────
//
// `GET /contextual-writing/catalog` serves the states→actions catalog that
// the desktop previously owned alone (desktop `contextual_assist.rs`). The
// desktop remains the serving authority for its own menu today; this
// endpoint is the canonical contract copy so new clients (iOS, browser,
// future surfaces) no longer hardcode their own action lists. Any change to
// action ids/intents must update BOTH this catalog and the desktop's until
// the desktop fetches from here.

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingCatalogState {
    pub id: String,
    pub label: String,
    pub actions: Vec<ContextualWritingCatalogAction>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextualWritingCatalogAction {
    pub id: String,
    pub label: String,
    pub description: String,
    pub kind: String,
    pub intent: String,
    pub requires_context: bool,
    pub requires_observation: bool,
    pub requires_screenshot: bool,
    pub requires_writable_target: bool,
    pub mutates_text: bool,
    pub creates_task: bool,
    pub opens_hud: bool,
}

fn catalog_action(
    id: &str,
    label: &str,
    description: &str,
    kind: &str,
    intent: &str,
    requires_context: bool,
    requires_observation: bool,
    requires_writable_target: bool,
    mutates_text: bool,
) -> ContextualWritingCatalogAction {
    // Mirrors the desktop's `default_action_requires_screenshot` exactly:
    // `shorten` and HUD handoffs skip the capture; context-, observation-,
    // text-mutation-, and task-creating actions are grounded by one.
    let creates_task = id == "create_task" || id == "schedule_followup";
    let requires_screenshot = id != "shorten"
        && (requires_context || requires_observation || mutates_text || creates_task);
    ContextualWritingCatalogAction {
        id: id.to_string(),
        label: label.to_string(),
        description: description.to_string(),
        kind: kind.to_string(),
        intent: intent.to_string(),
        requires_context,
        requires_observation,
        requires_screenshot,
        requires_writable_target,
        mutates_text,
        creates_task,
        opens_hud: id == "open_hud",
    }
}

fn catalog_task_action(
    description: &str,
    requires_observation: bool,
) -> ContextualWritingCatalogAction {
    catalog_action(
        "create_task",
        "Task",
        description,
        "secondary",
        "create_task",
        !requires_observation,
        requires_observation,
        false,
        false,
    )
}

fn catalog_followup_action(
    description: &str,
    requires_observation: bool,
) -> ContextualWritingCatalogAction {
    catalog_action(
        "schedule_followup",
        "Follow-up",
        description,
        "secondary",
        "schedule_followup",
        !requires_observation,
        requires_observation,
        false,
        false,
    )
}

fn catalog_hud_action(description: &str) -> ContextualWritingCatalogAction {
    ContextualWritingCatalogAction {
        id: "open_hud".to_string(),
        label: "HUD".to_string(),
        description: description.to_string(),
        kind: "secondary".to_string(),
        intent: "open_hud".to_string(),
        requires_context: false,
        requires_observation: false,
        requires_screenshot: false,
        requires_writable_target: false,
        mutates_text: false,
        creates_task: false,
        opens_hud: true,
    }
}

fn contextual_writing_catalog() -> Vec<ContextualWritingCatalogState> {
    vec![
        ContextualWritingCatalogState {
            id: "selection".to_string(),
            label: "Selected text".to_string(),
            actions: vec![
                catalog_action(
                    "rewrite",
                    "Rewrite",
                    "Rewrite the selected text.",
                    "primary",
                    "rewrite_selection",
                    true,
                    false,
                    false,
                    true,
                ),
                catalog_action(
                    "summarize",
                    "Summarize",
                    "Summarize the selected text.",
                    "primary",
                    "summarize_selection",
                    true,
                    false,
                    false,
                    false,
                ),
                catalog_action(
                    "save_to_notes",
                    "Save to Notes",
                    "Save the selected text to Notes with where it came from.",
                    "secondary",
                    "save_selection_to_notes",
                    true,
                    false,
                    false,
                    false,
                ),
                catalog_action(
                    "draft_reply",
                    "Reply",
                    "Draft a reply using selected and visible context.",
                    "primary",
                    "draft_reply_to_selection",
                    true,
                    false,
                    false,
                    true,
                ),
                catalog_task_action("Create an unscheduled task proposal from this text.", false),
                catalog_followup_action("Create a dated follow-up from this text.", false),
                catalog_hud_action("Open the full HUD with this context."),
            ],
        },
        ContextualWritingCatalogState {
            id: "selection-field".to_string(),
            label: "Selection in field".to_string(),
            actions: vec![
                catalog_action(
                    "rewrite",
                    "Rewrite",
                    "Replace only the selected text.",
                    "primary",
                    "rewrite_field_selection",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "shorten",
                    "Shorten",
                    "Make the selected text tighter.",
                    "primary",
                    "shorten_field_selection",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "clarify",
                    "Clarify",
                    "Clarify the selected text.",
                    "primary",
                    "clarify_field_selection",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "continue_draft",
                    "Continue",
                    "Continue after the selected text.",
                    "primary",
                    "continue_after_selection",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_task_action("Turn the selection into a task proposal.", false),
                catalog_hud_action("Open the full HUD with this selection."),
            ],
        },
        ContextualWritingCatalogState {
            id: "empty-context".to_string(),
            label: "Empty field + context".to_string(),
            actions: vec![
                catalog_action(
                    "draft_reply",
                    "Draft reply",
                    "Use current context to draft a reply.",
                    "primary",
                    "draft_reply_from_context",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "write_from_context",
                    "Write",
                    "Start writing from the current context.",
                    "primary",
                    "write_from_context",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_task_action("Create a task proposal from the current context.", false),
                catalog_followup_action(
                    "Create a scheduled follow-up from the current context.",
                    false,
                ),
                catalog_hud_action("Open the full HUD with this context."),
            ],
        },
        ContextualWritingCatalogState {
            id: "empty-no-context".to_string(),
            label: "Empty field".to_string(),
            actions: vec![
                catalog_action(
                    "observe_then_draft",
                    "Observe + draft",
                    "Read the current screen, then draft.",
                    "primary",
                    "observe_then_draft",
                    false,
                    true,
                    true,
                    true,
                ),
                catalog_action(
                    "write_from_context",
                    "Start writing",
                    "Start a blank draft with the selected personality.",
                    "primary",
                    "start_blank_draft",
                    false,
                    false,
                    true,
                    true,
                ),
                catalog_task_action("Read the screen and create a task proposal.", true),
                catalog_followup_action("Read the screen and create a scheduled follow-up.", true),
                catalog_hud_action("Use the larger HUD for a broad request."),
            ],
        },
        ContextualWritingCatalogState {
            id: "page-context".to_string(),
            label: "Web page".to_string(),
            actions: vec![
                catalog_action(
                    "summarize_page",
                    "Summarize page",
                    "Summarize the current page from its URL and visible content.",
                    "primary",
                    "summarize_page",
                    true,
                    false,
                    false,
                    false,
                ),
                catalog_task_action("Create a task proposal from this page.", false),
                catalog_hud_action("Ask about this page in the full HUD."),
            ],
        },
        ContextualWritingCatalogState {
            id: "files".to_string(),
            label: "Finder selection".to_string(),
            actions: vec![
                catalog_action(
                    "summarize",
                    "Summarize",
                    "Summarize the selected files from their names, paths, and the visible window.",
                    "primary",
                    "summarize_file_selection",
                    false,
                    true,
                    false,
                    false,
                ),
                catalog_task_action("Create a task proposal from the selected files.", true),
                catalog_hud_action("Ask about these files in the full HUD."),
            ],
        },
        ContextualWritingCatalogState {
            id: "draft".to_string(),
            label: "Non-empty field".to_string(),
            actions: vec![
                catalog_action(
                    "continue_draft",
                    "Continue",
                    "Continue the existing draft.",
                    "primary",
                    "continue_draft",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "improve_draft",
                    "Improve",
                    "Improve the draft without changing intent.",
                    "primary",
                    "improve_draft",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "shorten",
                    "Shorten",
                    "Make the draft more concise.",
                    "primary",
                    "shorten_draft",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_action(
                    "clarify",
                    "Clarify",
                    "Clarify the draft.",
                    "primary",
                    "clarify_draft",
                    true,
                    false,
                    true,
                    true,
                ),
                catalog_task_action("Create a task proposal from the draft.", false),
                catalog_followup_action("Create a dated follow-up from the draft.", false),
                catalog_hud_action("Open the full HUD with this draft."),
            ],
        },
        ContextualWritingCatalogState {
            id: "secure".to_string(),
            label: "Secure field".to_string(),
            actions: Vec::new(),
        },
        ContextualWritingCatalogState {
            id: "excluded".to_string(),
            label: "Excluded app".to_string(),
            actions: vec![catalog_hud_action("Open the HUD without contextual text.")],
        },
        ContextualWritingCatalogState {
            id: "unsupported".to_string(),
            label: "Unsupported target".to_string(),
            actions: vec![catalog_hud_action(
                "Use the full HUD and copy text manually.",
            )],
        },
    ]
}

/// GET /api/magician/v2/contextual-writing/catalog
pub async fn contextual_writing_catalog_handler() -> impl Responder {
    HttpResponse::Ok().json(json!({
        "states": contextual_writing_catalog(),
    }))
}

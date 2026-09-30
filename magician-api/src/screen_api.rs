//! Screen Capture API — on-demand "capture and ask"
//! (plan: `docs/archive/plans/2026-06-11-screen-capture-and-ask.md`).
//!
//! Thin endpoints, NOT a media rail — a capture is a single explicit act
//! (the clip recorder's only state is "one recording may be in flight").
//! The desktop shortcuts (or the `screen-observation` skill) call these,
//! then open the asking surface (HUD) bound to the returned chat session
//! with the capture already staged.
//!
//! ```text
//! POST /screen/capture        { mode?: "screenshot" | "region",
//!                                region_rect?: {x,y,width,height},
//!                                session_id?: "existing chat session id" }
//!   -> { capture_id, mode, thread_id, session_id, session_title,
//!        attachments: [{ attachment_id, stored_name, mime_type, size_bytes }] }
//! POST /screen/capture/discard { session_id, attachment_ids }
//!   -> { discarded_attachment_ids, retained_attachment_ids }
//! POST /screen/clip/toggle    {}
//!   -> { phase: "recording", clip_id, max_duration_s }       (first press)
//!   -> { phase: "staged", ...same shape as capture,           (second press)
//!        frame_count, duration_s }
//! ```
//!
//! What a capture does:
//!   1. Grabs pixels BEFORE any asking UI appears (the asking surface is
//!      never in frame). Screenshot: macOS built-in `screencapture -x`.
//!      Clip: cua-driver's daemon recorder (`start_recording` with
//!      `record_video`, held open by one `cua-driver mcp` session — the
//!      daemon ends a recording when its client disconnects — and ended
//!      by `stop_recording`, whose `last_video_path` names the mp4) →
//!      main-display H.264 mp4, hard-capped at
//!      [`SCREEN_CLIP_MAX_SECS`] by a watchdog; frames sampled via ffmpeg
//!      (vision models read frames, not mp4s — the mp4 is staged as the
//!      artifact, the frames as prompt images).
//!   2. Resolves the stable `screens` thread and TODAY's dated session under
//!      it (find-or-create; rotates via `new_session` when the active session
//!      is titled for an older day) — mirroring the meet-bot's dated threads.
//!   3. Stages the file(s) as chat attachments on that session. Attachment
//!      storage (the session outputs dir) IS the artifact store — it is the
//!      same directory the vision prompt-loader reads image bytes from, so
//!      there is no second copy.
//!   4. Appends a bounded provenance entry to the `user.screen_observations`
//!      memory tier (newest-N retention, same merge the meet-bot uses).
//!
//! The ask itself is a normal chat message carrying `attachment_ids` — the
//! existing vision path (`TranscriptBlock::ImageFile` →
//! `RouterContentBlock::Image`) takes it from there.
//!
//! Scope comes from the verified bearer like the
//! sibling APIs. Captures are explicit-invocation only; a tracing line per
//! capture (target `screen_capture`) keeps every capture observable. While
//! the clip recorder runs, macOS shows its system screen-recording indicator
//! — the OS-level truth, not an app claim.

use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Instant,
};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::chat_api::ChatApi;
use crate::scope::resolve_required_scope;
use magician::magician_v2::chat::models::{ChatChannel, ChatSession, ChatSessionStatus};
use magician_media::media_rails::screen_capture::{
    capture_display, screen_capture_attachment_context, ScreenCaptureRequestRect,
    ScreenCaptureTarget,
};
// Test-only, re-added after `72fae8b75` cleaned imports against the production
// build alone. The compiler's own note on the failure was `these functions
// exist but are inaccessible`, which is what an import removal looks like from
// the test module's side. The test module reaches it through `use super::*`;
// a second explicit import inside that module would shadow this one and make
// it read as unused. `chat_api` still carries a private copy of the same
// parser — `contextual_writing_api`'s copy was removed in favour of this one.
use magician::magician_v2::chat::service::{
    merge_user_memory_tier_fields_with_retention, normalized_user_memory_tier_name,
    SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE, SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL,
    SCREEN_CAPTURE_CROP_LOCAL_LABEL,
};
use magician::magician_v2::execution::agent_resources::AgentResources;
pub use magician_media::media_rails::screen_capture::capture_full_screen_attachment_for_session;
#[cfg(test)]
use magician_media::media_rails::screen_capture::png_image_size;

/// Stable UI thread all captures land in — the user-facing "#screens" thread.
const SCREENS_THREAD_ID: &str = "screens";
/// Target user tier for capture provenance. Must be accepted by
/// `chat::service::normalized_user_memory_tier_name`.
const SCREEN_MEMORY_TIER: &str = "user.screen_observations";
/// Bounded append: tiers feed prompts, so only the newest N capture entries
/// survive (oldest pruned by `updated_at`).
const SCREEN_MEMORY_MAX_ENTRIES: usize = 20;
/// Clip hard cap — a watchdog stops the recorder at this length even if the
/// user never presses stop; staging still happens on their next press.
const SCREEN_CLIP_MAX_SECS: u64 = 30;
/// How many frames a clip contributes to the prompt (evenly spaced).
const SCREEN_CLIP_MAX_FRAMES: u32 = 8;

/// One clip recording may be in flight at a time (single-operator desktop).
struct ActiveClip {
    clip_id: String,
    started_at: Instant,
    output_dir: PathBuf,
    /// The MCP session that owns the daemon's recording. `None` once the
    /// recorder was stopped (by the cap watchdog or the stop press).
    recorder: Option<CuaRecorderSession>,
    /// What `stop_recording` reported as the finalized mp4, when the
    /// watchdog stopped the recorder before the user's stop press.
    stopped_video: Option<PathBuf>,
}

fn active_clip() -> &'static tokio::sync::Mutex<Option<ActiveClip>> {
    static ACTIVE: OnceLock<tokio::sync::Mutex<Option<ActiveClip>>> = OnceLock::new();
    ACTIVE.get_or_init(|| tokio::sync::Mutex::new(None))
}

/// Resolve a CLI that may live outside the server's PATH (user-local installs,
/// homebrew). First existing fallback wins; bare name (PATH lookup) otherwise.
fn resolve_bin(name: &str, fallbacks: &[&str]) -> String {
    for candidate in fallbacks {
        let expanded = if let Some(rest) = candidate.strip_prefix("~/") {
            match std::env::var("HOME") {
                Ok(home) => format!("{home}/{rest}"),
                Err(_) => continue,
            }
        } else {
            (*candidate).to_string()
        };
        if Path::new(&expanded).exists() {
            return expanded;
        }
    }
    name.to_string()
}

fn cua_driver_bin() -> String {
    resolve_bin("cua-driver", &["~/.local/bin/cua-driver"])
}

fn ffmpeg_bin() -> String {
    resolve_bin(
        "ffmpeg",
        &["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"],
    )
}

#[derive(Debug, Deserialize)]
pub struct ScreenScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScreenCaptureRequest {
    /// `"screenshot"` (default). `"clip"` is planned (P4) and rejected for now.
    #[serde(default)]
    pub mode: Option<String>,
    /// Optional real screen rect for region capture. When present, the backend
    /// captures that exact rectangle and stages metadata that lets
    /// `screen-draw` map crop-local image coordinates back to the live overlay.
    #[serde(default)]
    pub region_rect: Option<ScreenCaptureRequestRect>,
    /// Optional existing chat session to stage the capture into. When omitted,
    /// captures continue to land in today's `screens` session.
    #[serde(default)]
    pub session_id: Option<String>,
    /// Provenance from the desktop: the app that was frontmost when the chord
    /// fired. Not trusted for anything but display — trimmed, bounded, and
    /// echoed back so the HUD can label the staged chip.
    #[serde(default)]
    pub source_app: Option<String>,
    #[serde(default)]
    pub source_window_title: Option<String>,
}

/// Trim, bound, and drop-empty a client-supplied provenance string. Control
/// characters are stripped first — window titles can carry them, and these
/// values flow straight into UI chip labels.
fn sanitized_capture_provenance(value: Option<&str>) -> Option<String> {
    const MAX_PROVENANCE_CHARS: usize = 200;
    let filtered: String = value?.chars().filter(|ch| !ch.is_control()).collect();
    let sanitized: String = filtered.trim().chars().take(MAX_PROVENANCE_CHARS).collect();
    if sanitized.is_empty() {
        return None;
    }
    Some(sanitized)
}

#[derive(Debug, Default, Deserialize)]
pub struct ScreenCaptureDiscardRequest {
    pub session_id: String,
    #[serde(default)]
    pub attachment_ids: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct ScreenDesktopAppLaunchRequest {
    /// Allowlisted Desktop App Copilot target: Notes, Calculator, TextEdit, or Music.
    #[serde(default)]
    pub app: String,
}

async fn resolve_capture_target_session(
    chat_api: &ChatApi,
    principal: &str,
    workspace: &str,
    session_id: &str,
) -> Result<ChatSession, HttpResponse> {
    match chat_api.chat_service.get_session(session_id).await {
        Ok(Some(session)) if session.principal != principal || session.workspace != workspace => {
            Err(HttpResponse::Forbidden().json(json!({
                "error": "chat_session_scope_mismatch",
                "message": "the supplied session_id belongs to a different principal or workspace",
            })))
        },
        Ok(Some(session)) if session.status == ChatSessionStatus::Archived => {
            Err(HttpResponse::BadRequest().json(json!({
                "error": "chat_session_archived",
                "message": "screen captures cannot be staged into an archived chat session",
            })))
        },
        Ok(Some(session)) => Ok(session),
        Ok(None) => Err(HttpResponse::NotFound().json(json!({
            "error": "chat_session_not_found",
            "session_id": session_id,
        }))),
        Err(error) => {
            tracing::warn!(
                target: "screen_capture",
                %error,
                session_id,
                "failed to resolve requested capture chat session"
            );
            Err(HttpResponse::InternalServerError().json(json!({
                "error": format!("failed to resolve requested chat session: {error}"),
            })))
        },
    }
}

fn desktop_app_tutor_launch_target(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "notes" | "apple notes" => Some("Notes"),
        "calculator" => Some("Calculator"),
        "textedit" | "text edit" => Some("TextEdit"),
        "music" | "apple music" => Some("Music"),
        _ => None,
    }
}

async fn launch_desktop_app(app: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        let output = Command::new("/usr/bin/open")
            .arg("-a")
            .arg(app)
            .output()
            .await
            .map_err(|error| format!("failed to launch `{app}`: {error}"))?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`open -a {app}` exited with {}: {}",
            output.status,
            stderr.trim()
        ));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Err("Desktop App Copilot launch is only available on macOS".to_string())
    }
}

/// POST /api/magician/v2/screen/desktop-app/launch
pub async fn screen_desktop_app_launch_handler(
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: web::Json<ScreenDesktopAppLaunchRequest>,
) -> impl Responder {
    if let Err(response) = resolve_required_scope(req.headers(), query.workspace.clone()) {
        return response;
    }
    let Some(app) = desktop_app_tutor_launch_target(&body.app) else {
        return HttpResponse::BadRequest().json(json!({
            "error": "unsupported_desktop_app",
            "message": "Desktop App Copilot can launch only Notes, Calculator, TextEdit, or Music.",
        }));
    };
    match launch_desktop_app(app).await {
        Ok(()) => HttpResponse::Ok().json(json!({
            "status": "ok",
            "app": app,
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": "desktop_app_launch_failed",
            "message": error,
            "app": app,
        })),
    }
}

/// POST /api/magician/v2/screen/capture
pub async fn screen_capture_handler(
    chat_api: web::Data<ChatApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: Option<web::Json<ScreenCaptureRequest>>,
) -> impl Responder {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };

    let mode = body
        .as_ref()
        .and_then(|b| b.mode.as_deref())
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .unwrap_or("screenshot");
    let region_rect = body.as_ref().and_then(|b| b.region_rect.clone());
    let requested_session_id = body
        .as_ref()
        .and_then(|b| b.session_id.as_deref())
        .map(str::trim)
        .filter(|session_id| !session_id.is_empty())
        .map(str::to_owned);
    if let Some(rect) = region_rect.as_ref() {
        if rect.width == 0 || rect.height == 0 {
            return HttpResponse::BadRequest().json(json!({
                "error": "region_rect width and height must be positive",
            }));
        }
        if mode != "region" {
            return HttpResponse::BadRequest().json(json!({
                "error": "region_rect is only valid with mode `region`",
            }));
        }
    }
    let capture_target = match mode {
        "screenshot" => ScreenCaptureTarget::Full,
        // macOS's native picker: drag = region, spacebar = window-pick. One
        // mode covers both — the OS draws all the UI. This legacy path cannot
        // report the real screen origin; callers that know the rect should
        // pass `region_rect` so live overlay drawing can be exact.
        "region" => match region_rect.clone() {
            Some(rect) => ScreenCaptureTarget::Rect(rect),
            None => ScreenCaptureTarget::InteractiveRegion,
        },
        other => {
            return HttpResponse::BadRequest().json(json!({
                "error": format!(
                    "unsupported capture mode `{other}` — use `screenshot` or `region` (clips go via /screen/clip/toggle)"
                ),
            }));
        },
    };
    let requested_session = match requested_session_id.as_deref() {
        Some(session_id) => {
            match resolve_capture_target_session(
                chat_api.get_ref(),
                &principal,
                &workspace,
                session_id,
            )
            .await
            {
                Ok(session) => Some(session),
                Err(response) => return response,
            }
        },
        None => None,
    };

    // 1. Capture BEFORE any asking UI can appear.
    let bytes = match capture_display(capture_target).await {
        Ok(Some(bytes)) => bytes,
        Ok(None) => {
            // The user dismissed the picker (Escape) — a deliberate choice,
            // not an error. The caller must NOT open an asking surface.
            tracing::debug!(target: "screen_capture", "region selection cancelled by the user");
            return HttpResponse::Ok().json(json!({ "cancelled": true, "mode": mode }));
        },
        Err(error) => {
            tracing::warn!(target: "screen_capture", %error, "screen capture failed");
            return HttpResponse::InternalServerError().json(json!({
                "error": format!("screen capture failed: {error}"),
            }));
        },
    };

    // 2. Resolve the session that owns the staged attachment. Most captures use
    // the stable daily `screens` session; debug/HUD flows can opt into an
    // already-created session so returned attachment_ids are immediately valid
    // for the following chat message.
    let (session, session_title) = match requested_session {
        Some(session) => {
            let title = session
                .title
                .clone()
                .unwrap_or_else(|| session.ui_thread_id.clone());
            (session, title)
        },
        None => {
            let today_title = format!("Screens — {}", chrono::Local::now().format("%Y-%m-%d"));
            match resolve_daily_screens_session(
                chat_api.get_ref(),
                &principal,
                &workspace,
                &today_title,
            )
            .await
            {
                Ok(session) => (session, today_title),
                Err(error) => {
                    tracing::warn!(target: "screen_capture", %error, "screens session resolution failed");
                    return HttpResponse::InternalServerError().json(json!({
                        "error": format!("failed to resolve the screens session: {error}"),
                    }));
                },
            }
        },
    };

    // 3+4. Stage the capture as a chat attachment on that session and append
    //      bounded provenance to the screen_observations tier.
    let capture_id = uuid::Uuid::new_v4().simple().to_string();
    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let files = vec![(
        format!("screen-{stamp}.png"),
        "image/png".to_string(),
        bytes,
    )];
    let attachments = match stage_capture_files(
        &chat_api,
        resources.get_ref(),
        &principal,
        &workspace,
        &capture_id,
        mode,
        region_rect.as_ref(),
        &session.id,
        files,
    )
    .await
    {
        Ok(attachments) => attachments,
        Err(error) => {
            tracing::warn!(target: "screen_capture", %error, "failed to stage capture attachment");
            return HttpResponse::InternalServerError().json(json!({
                "error": format!("failed to store the capture: {error}"),
            }));
        },
    };

    let source_app =
        sanitized_capture_provenance(body.as_ref().and_then(|b| b.source_app.as_deref()));
    let source_window_title =
        sanitized_capture_provenance(body.as_ref().and_then(|b| b.source_window_title.as_deref()));

    HttpResponse::Ok().json(json!({
        "capture_id": capture_id,
        "mode": mode,
        "thread_id": session.ui_thread_id,
        "session_id": session.id,
        "session_title": session_title,
        "attachments": attachments,
        "source_app": source_app,
        "source_window_title": source_window_title,
        "region_rect": region_rect.map(|rect| json!({
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
        })),
    }))
}

/// POST /api/magician/v2/screen/capture/discard
///
/// Cleans up a capture whose browser-side draft/session ownership changed
/// before send. The ChatService refuses anything except an unreferenced,
/// server-attested screen attachment, so this cannot act as a general file
/// deletion endpoint or race a committed/queued chat turn.
pub async fn screen_capture_discard_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: web::Json<ScreenCaptureDiscardRequest>,
) -> impl Responder {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let session_id = body.session_id.trim();
    if session_id.is_empty() || body.attachment_ids.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "session_id and attachment_ids are required",
        }));
    }
    if body.attachment_ids.len() > 16 {
        return HttpResponse::BadRequest().json(json!({
            "error": "at most 16 capture attachments may be discarded at once",
        }));
    }
    if let Err(response) =
        resolve_capture_target_session(chat_api.get_ref(), &principal, &workspace, session_id).await
    {
        return response;
    }

    let mut discarded = Vec::new();
    let mut retained = Vec::new();
    for attachment_id in body
        .attachment_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
    {
        if discarded.iter().any(|id| id == attachment_id)
            || retained.iter().any(|id| id == attachment_id)
        {
            continue;
        }
        match chat_api
            .chat_service
            .discard_unreferenced_screen_capture_attachment(session_id, attachment_id)
            .await
        {
            Ok(true) => discarded.push(attachment_id.to_string()),
            Ok(false) => retained.push(attachment_id.to_string()),
            Err(error) => {
                tracing::warn!(
                    target: "screen_capture",
                    %principal,
                    %workspace,
                    %session_id,
                    %attachment_id,
                    %error,
                    "failed to discard unreferenced screen capture"
                );
                return HttpResponse::InternalServerError().json(json!({
                    "error": "screen capture cleanup failed",
                }));
            },
        }
    }

    HttpResponse::Ok().json(json!({
        "discarded_attachment_ids": discarded,
        "retained_attachment_ids": retained,
    }))
}

/// POST /api/magician/v2/screen/clip/toggle
///
/// Single toggle so the desktop chord stays stateless: first call starts the
/// recorder, second call stops it and stages the result. If the watchdog
/// already stopped the recorder (cap hit), the second call just stages.
pub async fn screen_clip_toggle_handler(
    chat_api: web::Data<ChatApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
) -> impl Responder {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };

    let mut clip = active_clip().lock().await;
    match clip.take() {
        None => {
            // ── Start ────────────────────────────────────────────────
            if let Err(error) = ensure_cua_daemon().await {
                tracing::warn!(target: "screen_capture", %error, "clip start failed: no recorder daemon");
                return HttpResponse::ServiceUnavailable().json(json!({
                    "error": format!("clip recorder unavailable: {error}"),
                }));
            }
            let clip_id = uuid::Uuid::new_v4().simple().to_string();
            let output_dir = std::env::temp_dir().join(format!("magician-screen-clip-{clip_id}"));
            if let Err(error) = tokio::fs::create_dir_all(&output_dir).await {
                return HttpResponse::InternalServerError().json(json!({
                    "error": format!("clip dir: {error}"),
                }));
            }
            let recorder = match CuaRecorderSession::start(&output_dir).await {
                Ok(recorder) => recorder,
                Err(error) => {
                    let _ = tokio::fs::remove_dir_all(&output_dir).await;
                    tracing::warn!(target: "screen_capture", %error, "clip recorder start failed");
                    return HttpResponse::InternalServerError().json(json!({
                        "error": format!("failed to start the clip recorder: {error}"),
                    }));
                },
            };
            *clip = Some(ActiveClip {
                clip_id: clip_id.clone(),
                started_at: Instant::now(),
                output_dir,
                recorder: Some(recorder),
                stopped_video: None,
            });
            // Watchdog: stop the RECORDER at the cap (bounds the video);
            // staging still waits for the user's next press.
            let watchdog_clip_id = clip_id.clone();
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(SCREEN_CLIP_MAX_SECS)).await;
                let mut clip = active_clip().lock().await;
                let Some(active) = clip.as_mut().filter(|c| c.clip_id == watchdog_clip_id) else {
                    return;
                };
                let Some(recorder) = active.recorder.take() else {
                    return;
                };
                tracing::info!(
                    target: "screen_capture",
                    clip_id = %watchdog_clip_id,
                    "clip cap reached — stopping recorder"
                );
                match recorder.stop().await {
                    Ok(video) => active.stopped_video = video,
                    Err(error) => {
                        tracing::warn!(target: "screen_capture", %error, "clip watchdog stop failed");
                    },
                }
            });
            tracing::info!(
                target: "screen_capture",
                %principal, %workspace, clip_id = %clip_id,
                "clip recording started"
            );
            HttpResponse::Ok().json(json!({
                "phase": "recording",
                "clip_id": clip_id,
                "max_duration_s": SCREEN_CLIP_MAX_SECS,
            }))
        },
        Some(mut active) => {
            // ── Stop + stage ─────────────────────────────────────────
            // The watchdog may already have stopped the recorder at the
            // cap; then its reported video path is all that is left.
            // `stop_recording` replies after the mp4 is finalized.
            let reported_video = match active.recorder.take() {
                Some(recorder) => recorder.stop().await.unwrap_or_else(|error| {
                    tracing::warn!(target: "screen_capture", %error, "clip recorder stop (continuing)");
                    None
                }),
                None => active.stopped_video.take(),
            };
            let video_path =
                reported_video.unwrap_or_else(|| active.output_dir.join("recording.mp4"));
            let duration_s = active
                .started_at
                .elapsed()
                .as_secs()
                .min(SCREEN_CLIP_MAX_SECS)
                .max(1);
            let result = stage_clip(
                &chat_api,
                resources.get_ref(),
                &principal,
                &workspace,
                &active,
                &video_path,
                duration_s,
            )
            .await;
            let _ = tokio::fs::remove_dir_all(&active.output_dir).await;
            match result {
                Ok(response) => HttpResponse::Ok().json(response),
                Err(error) => {
                    tracing::warn!(target: "screen_capture", %error, "clip staging failed");
                    HttpResponse::InternalServerError().json(json!({
                        "error": format!("failed to stage the clip: {error}"),
                    }))
                },
            }
        },
    }
}

/// Locate the finished mp4, sample frames, stage everything on today's
/// screens session, append provenance — the clip twin of the screenshot path.
async fn stage_clip(
    chat_api: &web::Data<ChatApi>,
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    active: &ActiveClip,
    video_path: &Path,
    duration_s: u64,
) -> Result<serde_json::Value, String> {
    let video_bytes = tokio::fs::read(video_path)
        .await
        .map_err(|e| format!("read {}: {e}", video_path.display()))?;
    if video_bytes.is_empty() {
        return Err("recorder produced an empty video".to_string());
    }

    let frame_paths = sample_clip_frames(video_path, &active.output_dir, duration_s).await?;
    if frame_paths.is_empty() {
        return Err("no frames could be sampled from the clip".to_string());
    }

    let today_title = format!("Screens — {}", chrono::Local::now().format("%Y-%m-%d"));
    let session =
        resolve_daily_screens_session(chat_api.get_ref(), principal, workspace, &today_title)
            .await
            .map_err(|e| format!("screens session: {e}"))?;

    let stamp = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let mut files: Vec<(String, String, Vec<u8>)> = Vec::new();
    for (index, frame_path) in frame_paths.iter().enumerate() {
        let bytes = tokio::fs::read(frame_path)
            .await
            .map_err(|e| format!("read frame: {e}"))?;
        files.push((
            format!("clip-{stamp}-frame{:02}.png", index + 1),
            "image/png".to_string(),
            bytes,
        ));
    }
    let frame_count = files.len();
    files.push((
        format!("clip-{stamp}.mp4"),
        "video/mp4".to_string(),
        video_bytes,
    ));

    let attachments = stage_capture_files(
        chat_api,
        resources,
        principal,
        workspace,
        &active.clip_id,
        "clip",
        None,
        &session.id,
        files,
    )
    .await?;

    Ok(json!({
        "phase": "staged",
        "capture_id": active.clip_id,
        "mode": "clip",
        "thread_id": SCREENS_THREAD_ID,
        "session_id": session.id,
        "session_title": today_title,
        "attachments": attachments,
        "frame_count": frame_count,
        "duration_s": duration_s,
    }))
}

/// Extract up to [`SCREEN_CLIP_MAX_FRAMES`] evenly spaced PNG frames.
async fn sample_clip_frames(
    video: &Path,
    out_dir: &Path,
    duration_s: u64,
) -> Result<Vec<PathBuf>, String> {
    // fps = frames/duration spreads the budget across the whole clip; the
    // explicit -frames:v cap protects against duration under-estimates.
    let fps = format!("fps={}/{}", SCREEN_CLIP_MAX_FRAMES, duration_s.max(1));
    let pattern = out_dir.join("frame-%02d.png");
    let output = Command::new(ffmpeg_bin())
        .arg("-y")
        .arg("-i")
        .arg(video)
        .arg("-vf")
        .arg(&fps)
        .arg("-frames:v")
        .arg(SCREEN_CLIP_MAX_FRAMES.to_string())
        .arg(&pattern)
        .output()
        .await
        .map_err(|e| format!("spawn ffmpeg: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "ffmpeg frame sampling failed ({}): {}",
            output.status,
            stderr.lines().last().unwrap_or("").trim()
        ));
    }
    let mut frames = Vec::new();
    for index in 1..=SCREEN_CLIP_MAX_FRAMES {
        let path = out_dir.join(format!("frame-{index:02}.png"));
        if path.exists() {
            frames.push(path);
        }
    }
    Ok(frames)
}

/// The recorder and every `cua-driver call` / `cua-driver mcp` proxy need
/// the CuaDriver DAEMON (it holds the Screen Recording grant). Verify (exit
/// 0 = running), attempting the documented idempotent launch once if it
/// isn't.
async fn ensure_cua_daemon() -> Result<(), String> {
    let bin = cua_driver_bin();
    let running = |bin: String| async move {
        Command::new(&bin)
            .arg("status")
            .output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false)
    };
    if running(bin.clone()).await {
        return Ok(());
    }
    let _ = Command::new("/usr/bin/open")
        .args(["-n", "-g", "-a", "CuaDriver", "--args", "serve"])
        .output()
        .await;
    for _ in 0..6 {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        if running(bin.clone()).await {
            return Ok(());
        }
    }
    Err(format!(
        "CuaDriver daemon is not running and could not be started (check \
         `{bin} permissions status` — the daemon app needs the Screen Recording grant)"
    ))
}

/// A clip recording held open by one `cua-driver mcp` stdio session.
///
/// The daemon ties a recording to the lifecycle session that started it and
/// tears it down when that client disconnects: a one-shot `cua-driver call
/// start_recording` returns `recording: true`, and the recorder is already
/// gone (no mp4) by the next call from another process. The ownerless
/// `cua-driver recording start` survives reconnects but has no video option.
/// So the clip keeps an MCP client connected for its lifetime and stops the
/// recording on that same connection. If the server dies mid-clip, the
/// child's stdin closes and the daemon's own teardown ends the recording —
/// no orphaned screen capture.
struct CuaRecorderSession {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::io::Lines<tokio::io::BufReader<tokio::process::ChildStdout>>,
    next_id: u64,
}

/// Bound on any single recorder JSON-RPC exchange (stop includes the mp4
/// finalize).
const CUA_RECORDER_RPC_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

impl CuaRecorderSession {
    async fn start(output_dir: &Path) -> Result<Self, String> {
        use tokio::io::AsyncBufReadExt as _;

        let mut child = Command::new(cua_driver_bin())
            .arg("mcp")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("spawn cua-driver mcp: {e}"))?;
        let stdin = child.stdin.take().ok_or("cua-driver mcp: no stdin")?;
        let stdout = child.stdout.take().ok_or("cua-driver mcp: no stdout")?;
        let mut session = Self {
            child,
            stdin,
            stdout: tokio::io::BufReader::new(stdout).lines(),
            next_id: 1,
        };
        session
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2025-06-18",
                    "capabilities": {},
                    "clientInfo": { "name": "magician-screen-clip", "version": env!("CARGO_PKG_VERSION") },
                }),
            )
            .await?;
        session
            .send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
            .await?;
        let state = session
            .call_tool(
                "start_recording",
                json!({ "output_dir": output_dir.to_string_lossy(), "record_video": true }),
            )
            .await?;
        if state.get("video_active").and_then(|v| v.as_bool()) == Some(false) {
            let detail = state
                .get("last_error")
                .and_then(|v| v.as_str())
                .unwrap_or("video capture did not start");
            let _ = session.call_tool("stop_recording", json!({})).await;
            return Err(format!("cua-driver start_recording: {detail}"));
        }
        Ok(session)
    }

    /// Stop on the owning connection, then disconnect. Returns the
    /// finalized mp4 the daemon reported (`last_video_path`), if any.
    async fn stop(mut self) -> Result<Option<PathBuf>, String> {
        let result = self.call_tool("stop_recording", json!({})).await;
        drop(self.stdin);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), self.child.wait()).await;
        let state = result?;
        if let Some(error) = state.get("last_error").and_then(|v| v.as_str()) {
            tracing::warn!(target: "screen_capture", %error, "clip recorder reported an error");
        }
        Ok(state
            .get("last_video_path")
            .and_then(|v| v.as_str())
            .filter(|p| !p.is_empty())
            .map(PathBuf::from))
    }

    /// `tools/call` → the tool's `structuredContent`. A tool refusal (MCP
    /// `isError`, or a 0.28 `{code, suggestion}` payload) becomes `Err`.
    async fn call_tool(
        &mut self,
        name: &str,
        arguments: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await?;
        let structured = result
            .get("structuredContent")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let refused = result.get("isError").and_then(|v| v.as_bool()) == Some(true)
            || structured.get("code").is_some();
        if refused {
            let text = result
                .pointer("/content/0/text")
                .and_then(|v| v.as_str())
                .unwrap_or_default();
            return Err(format!("cua-driver {name} refused: {structured} {text}")
                .trim_end()
                .to_string());
        }
        Ok(structured)
    }

    async fn request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await?;
        let read_reply = async {
            loop {
                let line = self
                    .stdout
                    .next_line()
                    .await
                    .map_err(|e| format!("cua-driver mcp read: {e}"))?
                    .ok_or_else(|| format!("cua-driver mcp exited during {method}"))?;
                // Skip notifications and anything that is not our reply.
                let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
                    continue;
                };
                if message.get("id").and_then(|v| v.as_u64()) != Some(id) {
                    continue;
                }
                if let Some(error) = message.get("error") {
                    return Err(format!("cua-driver mcp {method}: {error}"));
                }
                return Ok(message
                    .get("result")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null));
            }
        };
        tokio::time::timeout(CUA_RECORDER_RPC_TIMEOUT, read_reply)
            .await
            .map_err(|_| format!("cua-driver mcp {method} timed out"))?
    }

    async fn send(&mut self, message: &serde_json::Value) -> Result<(), String> {
        use tokio::io::AsyncWriteExt as _;
        let mut line = message.to_string();
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| format!("cua-driver mcp write: {e}"))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| format!("cua-driver mcp write: {e}"))
    }
}

/// Bound on the accessibility tree inlined into the describe prompt (the
/// driver's default walk is up to 2 000 nodes — Electron trees blow past
/// any sane prompt budget).
const SCREEN_DESCRIBE_AX_MAX_ELEMENTS: u32 = 400;

/// One-shot `cua-driver call` → the reply JSON. CuaDriver 0.28 reports a
/// tool refusal with exit 0 and a `{code, suggestion}` payload, so a
/// clean exit is not success: a top-level `code` becomes `Err`.
async fn cua_call_json(tool: &str, args: serde_json::Value) -> Result<serde_json::Value, String> {
    let output = Command::new(cua_driver_bin())
        .arg("call")
        .arg(tool)
        .arg(args.to_string())
        .output()
        .await
        .map_err(|e| format!("spawn cua-driver {tool}: {e}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "cua-driver {tool} failed ({}): {}",
            output.status,
            stderr.trim()
        ));
    }
    let reply: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("cua-driver {tool}: unparseable reply: {e}"))?;
    if let Some(code) = reply.get("code").and_then(|c| c.as_str()) {
        let suggestion = reply
            .get("suggestion")
            .and_then(|s| s.as_str())
            .unwrap_or_default();
        return Err(format!("cua-driver {tool} refused: {code} {suggestion}")
            .trim_end()
            .to_string());
    }
    Ok(reply)
}

/// The frontmost app's front window: on screen, on the current Space, max
/// `z_index`. `list_windows` order is NOT stacking order and includes
/// minimized / other-Space windows; when every candidate's `z_index` is
/// null, fall back to the first on-screen candidate.
fn frontmost_window_id(windows: &[serde_json::Value]) -> Option<u64> {
    let candidates: Vec<&serde_json::Value> = windows
        .iter()
        .filter(|w| w.get("is_on_screen").and_then(|v| v.as_bool()) == Some(true))
        .filter(|w| w.get("on_current_space").and_then(|v| v.as_bool()) != Some(false))
        .collect();
    candidates
        .iter()
        .filter_map(|w| Some((w.get("z_index")?.as_i64()?, *w)))
        .max_by_key(|(z, _)| *z)
        .map(|(_, w)| w)
        .or_else(|| candidates.first().copied())
        .and_then(|w| w.get("window_id")?.as_u64())
}

async fn get_deep_ui_structure() -> Result<String, String> {
    ensure_cua_daemon().await?;

    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg("tell application \"System Events\" to get unix id of first application process whose frontmost is true")
        .output()
        .await
        .map_err(|e| format!("osascript failed: {}", e))?;
    let pid_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let pid: u32 = pid_str
        .parse()
        .map_err(|_| "Invalid PID from osascript".to_string())?;

    let listing = cua_call_json("list_windows", json!({ "pid": pid })).await?;
    let windows = listing
        .get("windows")
        .and_then(|w| w.as_array())
        .ok_or("No windows found")?;
    let window_id = frontmost_window_id(windows)
        .ok_or("frontmost app has no on-screen window on the current Space")?;

    // Tree only: the describe path already sends its own frame, so skip
    // the driver's screenshot (`capture_mode` is ignored since 0.28).
    let state = cua_call_json(
        "get_window_state",
        json!({
            "pid": pid,
            "window_id": window_id,
            "include_screenshot": false,
            "max_elements": SCREEN_DESCRIBE_AX_MAX_ELEMENTS,
        }),
    )
    .await?;
    let tree = state
        .get("tree_markdown")
        .and_then(|t| t.as_str())
        .ok_or("get_window_state reply has no tree_markdown")?;
    Ok(tree.to_string())
}

// ─── describe (the agent path's eyes) ───────────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct ScreenDescribeRequest {
    /// The question to answer about the captured frame.
    pub question: Option<String>,
    /// `"screenshot"` (default) or `"region"` — ignored when `image_b64`
    /// is provided.
    pub mode: Option<String>,
    /// Pre-captured PNG (base64, no data-URL prefix) — e.g. a window-scoped
    /// cua-driver capture from the skill. Skips the capture step.
    pub image_b64: Option<String>,
}

/// POST /api/magician/v2/screen/describe
///
/// One-shot "look at the screen and answer this" for the AGENT path (the
/// `screen-observation` skill): captures (or accepts) a frame and answers
/// via the `screen_understanding` operation → vision profile in
/// `magician-config.yaml` — the same config-managed engine as everything
/// else, instead of ad-hoc vision CLIs. Ephemeral by design: no thread, no
/// memory entry — the asking agent owns what to do with the answer.
pub async fn screen_describe_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: Option<web::Json<ScreenDescribeRequest>>,
) -> impl Responder {
    use base64::Engine as _;
    use magician::magician_v2::query_analysis::operation_llm_router::{
        global_operation_router, LLMOperation,
    };
    use magician::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let request = body.map(|b| b.into_inner()).unwrap_or_default();
    let question = match request
        .question
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
    {
        Some(question) => question.to_string(),
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": "`question` is required",
            }));
        },
    };
    let Some(router) = global_operation_router() else {
        return HttpResponse::ServiceUnavailable().json(json!({
            "error": "operation router unavailable — check magician-config.yaml llm.router",
        }));
    };

    // Frame: caller-provided, else capture (screenshot / interactive region).
    let (image_b64, mode) = if let Some(provided) = request
        .image_b64
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        (provided.to_string(), "provided")
    } else {
        let interactive = matches!(request.mode.as_deref().map(str::trim), Some("region"));
        let target = if interactive {
            ScreenCaptureTarget::InteractiveRegion
        } else {
            ScreenCaptureTarget::Full
        };
        match capture_display(target).await {
            Ok(Some(bytes)) => (
                base64::engine::general_purpose::STANDARD.encode(bytes),
                if interactive { "region" } else { "screenshot" },
            ),
            Ok(None) => {
                return HttpResponse::Ok().json(json!({ "cancelled": true }));
            },
            Err(error) => {
                tracing::warn!(target: "screen_capture", %error, "describe capture failed");
                return HttpResponse::InternalServerError().json(json!({
                    "error": format!("screen capture failed: {error}"),
                }));
            },
        }
    };

    let image = ImageData::with_detail(image_b64, "image/png".to_string(), ImageDetail::High);
    // Store-managed prompt (data/magician_v2/prompts/); the literal is the
    // degrade-loudly fallback, not the source of truth.
    let system = magician::magician_v2::prompts::rendered_prompt_or(
        magician::magician_v2::prompts::names::SCREEN_DESCRIBE_SYSTEM,
        magician::magician_v2::prompts::versions::SCREEN_DESCRIBE_SYSTEM,
        std::collections::HashMap::new(),
        "You are looking at a screenshot of the user's screen on their behalf. \
         Answer their question about it directly and concretely. Quote on-screen \
         text exactly when the question is about exact text.",
    )
    .await;

    let mut system_prompt = system.as_str().to_string();
    let deep_ui = get_deep_ui_structure().await;
    if let Err(error) = &deep_ui {
        tracing::warn!(target: "screen_capture", %error, "describe: no accessibility tree (answering from pixels only)");
    }
    if let Ok(tree) = deep_ui {
        if !tree.is_empty() {
            system_prompt.push_str("\n\nHere is the exact structural UI (Accessibility tree) of the frontmost window for precise reference:\n```markdown\n");
            system_prompt.push_str(&tree);
            system_prompt.push_str("\n```\n");
            tracing::info!(target: "screen_capture", "Appended deep structural UI context to describe prompt.");
        }
    }

    let llm_started = std::time::Instant::now();
    let scoped_router = router.with_scope_context(Some(magicllm::LlmScope::new(
        principal.clone(),
        workspace.clone(),
    )));
    let response = scoped_router
        .generate_for_execution_native_tools(
            &LLMOperation::ScreenUnderstanding,
            Some(system_prompt.as_str()),
            &question,
            Vec::new(),
            None,
            Some(std::slice::from_ref(&image)),
            None,
            None,
        )
        .await;
    match response {
        Ok(result) => {
            let answer = result.text.as_deref().unwrap_or("").trim().to_string();
            if let Some(broadcaster) = resources.event_broadcaster.as_ref() {
                let telemetry = magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                    Arc::clone(broadcaster),
                    &principal,
                    &workspace,
                    "screen_assist",
                );
                let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
                if answer.is_empty() {
                    telemetry.emit_native_validation_failure(
                        LLMOperation::ScreenUnderstanding.as_str(),
                        &result,
                        latency_ms,
                        Default::default(),
                        "screen_answer_nonempty",
                        "screen understanding response was empty",
                    );
                } else {
                    telemetry.emit_native_validated_success(
                        LLMOperation::ScreenUnderstanding.as_str(),
                        &result,
                        latency_ms,
                        Default::default(),
                        "screen_answer_nonempty",
                    );
                }
            }
            tracing::info!(
                target: "screen_capture",
                mode = %mode,
                question_chars = question.len(),
                answer_chars = answer.len(),
                "screen describe answered"
            );
            HttpResponse::Ok().json(json!({ "answer": answer, "mode": mode }))
        },
        Err(error) => {
            tracing::warn!(target: "screen_capture", %error, "describe vision call failed");
            HttpResponse::InternalServerError().json(json!({
                "error": format!("vision call failed: {error}"),
            }))
        },
    }
}

// ─── ground (pixel-precise click targeting) ─────────────────────────────

#[derive(Debug, Default, Deserialize)]
pub struct ScreenGroundRequest {
    /// What to locate, in plain language ("the play button").
    pub target: Option<String>,
    /// The frame to ground against (base64 PNG, no data-URL prefix).
    /// REQUIRED — grounding against a frame the caller never saw invites
    /// stale-frame clicks, so there is deliberately no capture mode here
    /// (unlike `/screen/describe`).
    pub image_b64: Option<String>,
}

/// Image dimensions + mime from magic bytes — PNG (IHDR) or JPEG (SOF
/// scan). JPEG matters in practice: cua-driver's `zoom` emits JPEG crops,
/// and the zoom-refinement step grounds on those. No image crate in the
/// workspace, and a full decode is unnecessary: the dimensions exist to
/// BIND the coordinate space of the model's answer.
fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32, &'static str)> {
    const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if bytes.len() >= 24 && bytes[..8] == PNG_SIGNATURE {
        let width = u32::from_be_bytes(bytes[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(bytes[20..24].try_into().ok()?);
        return (width > 0 && height > 0).then_some((width, height, "image/png"));
    }
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        // Walk JPEG segments to a frame header (SOF0..SOF15 except
        // DHT/DAC/RST markers): dims live at offset 5..9 of the segment.
        let mut pos = 2usize;
        while pos + 9 < bytes.len() {
            if bytes[pos] != 0xFF {
                return None;
            }
            let marker = bytes[pos + 1];
            if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
                let height = u32::from(u16::from_be_bytes([bytes[pos + 5], bytes[pos + 6]]));
                let width = u32::from(u16::from_be_bytes([bytes[pos + 7], bytes[pos + 8]]));
                return (width > 0 && height > 0).then_some((width, height, "image/jpeg"));
            }
            let len = u16::from_be_bytes([bytes[pos + 2], bytes[pos + 3]]) as usize;
            pos += 2 + len;
        }
    }
    None
}

#[derive(Debug, Deserialize)]
struct GroundVerdict {
    found: bool,
    /// PER-MILLE (0–1000) of image width — live testing showed that answering
    /// in raw pixels carries a gross, systematic vertical bias
    /// (placed a bottom-bar control at 73% height; normalized asking put
    /// it correctly at 93%), while normalized coordinates track the
    /// image edges. Converted to pixels server-side.
    #[serde(default)]
    x_pm: f64,
    /// PER-MILLE (0–1000) of image height.
    #[serde(default)]
    y_pm: f64,
    #[serde(default)]
    confidence: f64,
    #[serde(default)]
    reasoning: String,
}

/// POST /api/magician/v2/screen/ground
///
/// Pixel grounding for AX-empty surfaces (plan:
/// `docs/plans/2026-06-12-screen-grounding.md`): locate a click target in
/// a provided frame and return its CENTER in that frame's own pixel space.
/// Routed via the `screen_grounding` operation → vision profile in
/// `magician-config.yaml` — swapping the grounding model is a config edit.
/// Ephemeral like `/screen/describe`: no thread, no memory. `found: false`
/// is a SUCCESS response meaning "stop, don't click" — the recipe treats a
/// refusal as strictly better than a low-confidence guess.
pub async fn screen_ground_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: Option<web::Json<ScreenGroundRequest>>,
) -> impl Responder {
    use base64::Engine as _;
    use magician::magician_v2::query_analysis::operation_llm_router::{
        global_operation_router, LLMOperation,
    };
    use magician::magician_v2::slot_graph::extraction::{ImageData, ImageDetail};

    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let request = body.map(|b| b.into_inner()).unwrap_or_default();
    let target = match request
        .target
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
    {
        Some(target) => target.to_string(),
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": "`target` is required",
            }));
        },
    };
    let image_b64 = match request
        .image_b64
        .as_deref()
        .map(str::trim)
        .filter(|b| !b.is_empty())
    {
        Some(image) => image.to_string(),
        None => {
            return HttpResponse::BadRequest().json(json!({
                "error": "`image_b64` is required — ground against the frame you just captured",
            }));
        },
    };
    let Some(router) = global_operation_router() else {
        return HttpResponse::ServiceUnavailable().json(json!({
            "error": "operation router unavailable — check magician-config.yaml llm.router",
        }));
    };
    let Some((width, height, mime)) = base64::engine::general_purpose::STANDARD
        .decode(&image_b64)
        .ok()
        .as_deref()
        .and_then(image_dimensions)
    else {
        return HttpResponse::BadRequest().json(json!({
            "error": "`image_b64` is not a decodable PNG or JPEG",
        }));
    };

    let image = ImageData::with_detail(image_b64, mime.to_string(), ImageDetail::High);
    // Store-managed prompt (data/magician_v2/prompts/); the literal is the
    // degrade-loudly fallback, not the source of truth. PER-MILLE
    // coordinates are load-bearing — see the prompt file's metadata.
    let system = magician::magician_v2::prompts::rendered_prompt_or(
        magician::magician_v2::prompts::names::SCREEN_GROUNDING_SYSTEM,
        magician::magician_v2::prompts::versions::SCREEN_GROUNDING_SYSTEM,
        std::collections::HashMap::new(),
        "You locate click targets in screenshots. Reply with ONLY a JSON object, \
         no prose, no code fences: {\"found\": bool, \"x_pm\": int, \"y_pm\": int, \
         \"confidence\": 0..1, \"reasoning\": \"one short line\"}. x_pm and y_pm are \
         PER-MILLE (0-1000) positions of the target's CENTER: x_pm=0 is the image's \
         left edge and x_pm=1000 its right edge; y_pm=0 the top edge, y_pm=1000 the \
         bottom edge. Measure carefully against the edges. If the target is not \
         visible or you are unsure which element is meant, return found=false — a \
         refusal is better than a guess.",
    )
    .await;
    let prompt = format!("Locate: {target}");
    let llm_started = std::time::Instant::now();
    let scoped_router = router.with_scope_context(Some(magicllm::LlmScope::new(
        principal.clone(),
        workspace.clone(),
    )));
    let response = scoped_router
        .generate_for_execution_native_tools(
            &LLMOperation::ScreenGrounding,
            Some(system.as_str()),
            &prompt,
            Vec::new(),
            None,
            Some(std::slice::from_ref(&image)),
            None,
            None,
        )
        .await;
    let result = match response {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(target: "screen_capture", %error, "ground vision call failed");
            return HttpResponse::InternalServerError().json(json!({
                "error": format!("vision call failed: {error}"),
            }));
        },
    };
    let raw = result.text.as_deref().unwrap_or_default().to_string();
    // Strict-JSON expected; tolerate fenced/prefixed output by slicing the
    // outermost object before giving up.
    let parsed: Option<GroundVerdict> = serde_json::from_str(raw.trim()).ok().or_else(|| {
        let start = raw.find('{')?;
        let end = raw.rfind('}')?;
        serde_json::from_str(&raw[start..=end]).ok()
    });
    let Some(verdict) = parsed else {
        if let Some(broadcaster) = resources.event_broadcaster.as_ref() {
            magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                &principal,
                &workspace,
                "screen_assist",
            )
            .emit_native_validation_failure(
                LLMOperation::ScreenGrounding.as_str(),
                &result,
                llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                Default::default(),
                "screen_grounding_json",
                "grounding model returned an unparseable verdict",
            );
        }
        tracing::warn!(
            target: "screen_capture",
            raw = %raw.chars().take(200).collect::<String>(),
            "ground verdict unparseable"
        );
        return HttpResponse::InternalServerError().json(json!({
            "error": "grounding model returned an unparseable verdict",
        }));
    };
    if let Some(broadcaster) = resources.event_broadcaster.as_ref() {
        magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            &principal,
            &workspace,
            "screen_assist",
        )
        .emit_native_validated_success(
            LLMOperation::ScreenGrounding.as_str(),
            &result,
            llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
            Default::default(),
            "screen_grounding_json",
        );
    }
    let x = (verdict.x_pm / 1000.0 * width as f64)
        .round()
        .clamp(0.0, (width.saturating_sub(1)) as f64) as u32;
    let y = (verdict.y_pm / 1000.0 * height as f64)
        .round()
        .clamp(0.0, (height.saturating_sub(1)) as f64) as u32;
    tracing::info!(
        target: "screen_capture",
        %target,
        found = verdict.found,
        x,
        y,
        confidence = verdict.confidence,
        "screen ground answered"
    );
    HttpResponse::Ok().json(json!({
        "found": verdict.found,
        "x": x,
        "y": y,
        "confidence": verdict.confidence,
        "reasoning": verdict.reasoning,
        "image_width": width,
        "image_height": height,
    }))
}

// ─── continuous observation (P7) ────────────────────────────────────────

/// UI thread observation sessions live under — SEPARATE from the one-shot
/// `screens` thread: the one-shot path rotates that thread's active session
/// to today's date, which would tear down a running observation's session
/// mid-stream. Observations are serial, so per-observation rotation inside
/// their own thread is harmless.
const SCREEN_WATCH_THREAD_ID: &str = "screen-watch";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScreenObserveStartRequest {
    pub purpose: Option<String>,
    /// `"notes"` (default — never interrupts) or `"watch"`.
    pub mode: Option<String>,
    /// `watch` mode: the alert condition in plain language.
    pub watch_for: Option<String>,
    pub cadence_s: Option<u64>,
    pub max_minutes: Option<u64>,
    /// Stop once the watch condition matches (default true in watch mode).
    pub stop_on_match: Option<bool>,
    /// Capture audio alongside the screen: `"system"`, `"mic"`, `"both"`, or
    /// `"none"` (default). System audio rides the Screen-Recording grant the
    /// frame loop already holds; mic needs its own Microphone grant.
    pub audio: Option<String>,
    /// Optional request-level Listening profile and stage overrides. Omitted
    /// values use the scoped preference and configured default.
    pub audio_profile: Option<String>,
    #[serde(default)]
    pub audio_stage_options:
        std::collections::BTreeMap<magician_media::media_rails::AudioStage, String>,
    /// Optional stable-screen deep-read pass. When true, the observer runs one
    /// high-detail understanding pass after the same screen is stable for 20s,
    /// then repeats at most every 5 minutes while unchanged.
    #[serde(default, alias = "deep_understanding")]
    pub deep_observation: Option<bool>,
}

fn observe_view_json(
    view: &magician_media::media_rails::screen_observe::ObserveStatusView,
) -> serde_json::Value {
    json!({
        "observe_id": view.observe_id,
        "status": view.status.as_str(),
        "purpose": view.purpose,
        "mode": view.mode,
        "watch_for": view.watch_for,
        "thread_id": view.thread,
        "started_at_ms": view.started_at_ms,
        "note_count": view.note_count,
        "alert_count": view.alert_count,
        "transcript_count": view.transcript_count,
        "audio_source": view.audio_source,
        "audio_profile": view.audio_profile,
        "stt_provider": view.stt_provider,
        "deep_observation": view.deep_observation,
        "deep_dwell_s": view.deep_dwell_s,
        "deep_repeat_s": view.deep_repeat_s,
        "deep_note_count": view.deep_note_count,
        "latest_summary": view.latest_summary,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct ScreenObserveRetargetRequest {
    pub purpose: Option<String>,
    pub mode: Option<String>,
    pub watch_for: Option<String>,
    pub stop_on_match: Option<bool>,
    #[serde(default, alias = "deep_understanding")]
    pub deep_observation: Option<bool>,
}

/// POST /api/magician/v2/screen/observe/retarget
///
/// Change WHAT the running observation looks for, mid-session: a chord-
/// started notes session upgrades to `watch` the moment the user says what
/// to watch for (typed into the HUD or chat), or drops back to notes.
pub async fn screen_observe_retarget_handler(
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: Option<web::Json<ScreenObserveRetargetRequest>>,
) -> impl Responder {
    use magician_media::media_rails::screen_observe::{
        retarget_screen_observation, ObserveMode, ObserveTargetUpdate,
    };
    if let Err(response) = resolve_required_scope(req.headers(), query.workspace.clone()) {
        return response;
    }
    let request = body.map(|b| b.into_inner()).unwrap_or_default();
    let update = ObserveTargetUpdate {
        purpose: request
            .purpose
            .as_deref()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string),
        mode: request
            .mode
            .as_deref()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(ObserveMode::parse),
        watch_for: request
            .watch_for
            .as_deref()
            .map(str::trim)
            .filter(|w| !w.is_empty())
            .map(str::to_string),
        stop_on_match: request.stop_on_match,
        deep_observation: request.deep_observation,
    };
    match retarget_screen_observation(update).await {
        Ok(Some(view)) => HttpResponse::Ok().json(observe_view_json(&view)),
        Ok(None) => HttpResponse::Conflict().json(json!({
            "error": "no observation is running — start one first",
        })),
        Err(error) => HttpResponse::BadRequest().json(json!({ "error": error })),
    }
}

/// Create the per-observation chat session under the watch thread (archives
/// the PREVIOUS observation's session — observations are serial) and title
/// it; returns the session id the HUD binds to.
async fn create_watch_session(
    chat_api: &ChatApi,
    principal: &str,
    workspace: &str,
    title: &str,
) -> anyhow::Result<String> {
    let session = chat_api
        .chat_service
        .new_automated_session(
            principal,
            workspace,
            SCREEN_WATCH_THREAD_ID,
            &ChatChannel::web(),
        )
        .await?;
    chat_api
        .chat_service
        .chat_store_ref()
        .update_session_title(&session.id, title)
        .await?;
    Ok(session.id)
}

async fn start_observation(
    chat_api: &web::Data<ChatApi>,
    resources: &web::Data<Arc<AgentResources>>,
    principal: &str,
    workspace: &str,
    request: ScreenObserveStartRequest,
) -> Result<serde_json::Value, (u16, String)> {
    use magician_media::media_rails::screen_observe::{
        start_screen_observation, AudioCaptureSource, ObserveAudioConfig, ObserveConfig,
        ObserveMode, ObserveTarget,
    };
    let purpose = request
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("general activity")
        .to_string();
    let mode = ObserveMode::parse(request.mode.as_deref().unwrap_or("notes"));
    let watch_for = request
        .watch_for
        .as_deref()
        .map(str::trim)
        .filter(|w| !w.is_empty())
        .map(str::to_string);
    if mode == ObserveMode::Watch && watch_for.is_none() {
        return Err((400, "watch mode requires `watch_for`".to_string()));
    }
    let session_title = format!(
        "Watching: {} — {}",
        purpose,
        chrono::Local::now().format("%Y-%m-%d %H:%M")
    );
    let session_id = create_watch_session(chat_api.get_ref(), principal, workspace, &session_title)
        .await
        .map_err(|e| (500, format!("watch session: {e}")))?;
    let audio = ObserveAudioConfig {
        source: request
            .audio
            .as_deref()
            .map(AudioCaptureSource::parse)
            .unwrap_or(AudioCaptureSource::None),
        audio_profile: request.audio_profile,
        audio_stage_options: request.audio_stage_options,
    };
    let config = ObserveConfig {
        // Caller cadence wins when sane (≥2s); else the env-tunable default.
        cadence_s: request
            .cadence_s
            .filter(|c| *c >= 2)
            .unwrap_or_else(magician_media::media_rails::screen_observe::default_cadence_s),
        max_minutes: request
            .max_minutes
            .unwrap_or(magician_media::media_rails::screen_observe::DEFAULT_MAX_MINUTES),
        thread: SCREEN_WATCH_THREAD_ID.to_string(),
        session_title,
        audio,
    };
    let target = ObserveTarget {
        purpose,
        mode,
        watch_for,
        stop_on_match: request.stop_on_match.unwrap_or(true),
        deep_observation: request.deep_observation.unwrap_or(false),
    };
    let view = start_screen_observation(
        config,
        target,
        resources.get_ref().clone(),
        principal.to_string(),
        workspace.to_string(),
    )
    .await
    .map_err(|e| (409, e))?;
    tracing::info!(
        target: "screen_observe",
        observe_id = %view.observe_id,
        purpose = %view.purpose,
        mode = %view.mode,
        "screen observation started"
    );
    let mut body = observe_view_json(&view);
    body["phase"] = json!("observing");
    body["session_id"] = json!(session_id);
    Ok(body)
}

/// POST /api/magician/v2/screen/observe/start
pub async fn screen_observe_start_handler(
    chat_api: web::Data<ChatApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: Option<web::Json<ScreenObserveStartRequest>>,
) -> impl Responder {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let request = body.map(|b| b.into_inner()).unwrap_or_default();
    match start_observation(&chat_api, &resources, &principal, &workspace, request).await {
        Ok(body) => HttpResponse::Ok().json(body),
        Err((409, error)) => HttpResponse::Conflict().json(json!({ "error": error })),
        Err((400, error)) => HttpResponse::BadRequest().json(json!({ "error": error })),
        Err((_, error)) => HttpResponse::InternalServerError().json(json!({ "error": error })),
    }
}

/// POST /api/magician/v2/screen/observe/stop
pub async fn screen_observe_stop_handler(
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
) -> impl Responder {
    if let Err(response) = resolve_required_scope(req.headers(), query.workspace.clone()) {
        return response;
    }
    match magician_media::media_rails::screen_observe::stop_screen_observation().await {
        Ok(Some(view)) => {
            let mut body = observe_view_json(&view);
            body["phase"] = json!("stopped");
            HttpResponse::Ok().json(body)
        },
        Ok(None) => HttpResponse::Ok().json(json!({ "phase": "idle" })),
        Err(error) => HttpResponse::InternalServerError().json(json!({ "error": error })),
    }
}

/// GET /api/magician/v2/screen/observe/status
pub async fn screen_observe_status_handler() -> impl Responder {
    match magician_media::media_rails::screen_observe::screen_observation_status().await {
        Some(view) => HttpResponse::Ok().json(observe_view_json(&view)),
        None => HttpResponse::Ok().json(json!({ "status": "idle" })),
    }
}

/// POST /api/magician/v2/screen/observe/toggle
///
/// The ⇧⌥W chord's endpoint — stateless for the caller, like the clip
/// toggle: no observation running → start a notes-mode session instantly
/// (zero words needed); one running → stop it and return the final view
/// (the tray then opens the HUD on the observation's session).
pub async fn screen_observe_toggle_handler(
    chat_api: web::Data<ChatApi>,
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
) -> impl Responder {
    use magician_media::media_rails::screen_observe::{
        screen_observation_status, stop_screen_observation, ObserveStatus,
    };
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let running = matches!(
        screen_observation_status().await,
        Some(view) if view.status == ObserveStatus::Observing
    );
    if running {
        match stop_screen_observation().await {
            Ok(Some(view)) => {
                let mut body = observe_view_json(&view);
                body["phase"] = json!("stopped");
                HttpResponse::Ok().json(body)
            },
            Ok(None) => HttpResponse::Ok().json(json!({ "phase": "idle" })),
            Err(error) => HttpResponse::InternalServerError().json(json!({ "error": error })),
        }
    } else {
        let request = ScreenObserveStartRequest {
            purpose: Some("general activity (started by shortcut)".to_string()),
            mode: Some("notes".to_string()),
            ..Default::default()
        };
        match start_observation(&chat_api, &resources, &principal, &workspace, request).await {
            Ok(body) => HttpResponse::Ok().json(body),
            Err((409, error)) => HttpResponse::Conflict().json(json!({ "error": error })),
            Err((_, error)) => HttpResponse::InternalServerError().json(json!({ "error": error })),
        }
    }
}

/// Stage a capture's files as chat attachments + append ONE provenance entry.
/// Shared by the screenshot and clip paths. Returns the attachment
/// descriptors in the response/event shape the desktop and HUD consume.
#[allow(clippy::too_many_arguments)]
async fn stage_capture_files(
    chat_api: &web::Data<ChatApi>,
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    capture_id: &str,
    mode: &str,
    region_rect: Option<&ScreenCaptureRequestRect>,
    session_id: &str,
    files: Vec<(String, String, Vec<u8>)>,
) -> Result<Vec<serde_json::Value>, String> {
    let mut attachments = Vec::with_capacity(files.len());
    let mut stored_names = Vec::with_capacity(files.len());
    for (original_name, mime_type, bytes) in files {
        let size = bytes.len();
        let screen_capture_context =
            screen_capture_attachment_context(mode, region_rect, &mime_type, &bytes);
        let label = match screen_capture_context.as_ref() {
            Some(context) if context.mode == "screenshot" => Some(
                "full screen capture (server-registered; capture-local coordinates)".to_string(),
            ),
            Some(context)
                if context.coordinate_space == SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL =>
            {
                Some(SCREEN_CAPTURE_CROP_LOCAL_LABEL.to_string())
            },
            Some(context)
                if context.coordinate_space == SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE =>
            {
                Some(
                    "screen selection (rect-aware; live overlay coordinates available)".to_string(),
                )
            },
            _ => None,
        };
        let record = chat_api
            .chat_service
            .store_attachment_with_context(
                session_id,
                &original_name,
                &mime_type,
                &bytes,
                label,
                screen_capture_context.clone(),
            )
            .await
            .map_err(|e| format!("store {original_name}: {e}"))?;
        stored_names.push(record.stored_name.clone());
        let mut attachment = json!({
            "attachment_id": record.id,
            "stored_name": record.stored_name,
            "mime_type": mime_type,
            "size_bytes": size,
        });
        if let Some(context) = screen_capture_context {
            if let Some(object) = attachment.as_object_mut() {
                object.insert("screen_capture".to_string(), json!(context));
            }
        }
        attachments.push(attachment);
    }

    append_capture_memory(
        resources,
        principal,
        workspace,
        capture_id,
        mode,
        session_id,
        &stored_names,
    )
    .await;

    tracing::info!(
        target: "screen_capture",
        %principal,
        %workspace,
        capture_id = %capture_id,
        mode = %mode,
        session_id = %session_id,
        attachment_count = attachments.len(),
        "screen capture staged"
    );
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;
    use std::sync::Arc;

    use magician::magician_v2::chat::llm_service::ChatLlmService;
    use magician::magician_v2::chat::service::ChatService;
    use magician::magician_v2::chat::storage::FileChatStore;
    use magician::magician_v2::prompts::PromptManager;
    use magician::magician_v2::query_analysis::multi_llm_service::MultiLLMService;
    use magician::magician_v2::realtime_events::RuntimeTransportBroadcaster;

    struct DummyPromptStore;

    #[async_trait::async_trait]
    impl runtime_core::PromptStore for DummyPromptStore {
        async fn get_prompt(
            &self,
            _name: &str,
            _version: &str,
        ) -> anyhow::Result<runtime_core::Prompt> {
            Err(anyhow::anyhow!("unused in screen_api tests"))
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
            Err(anyhow::anyhow!("unused in screen_api tests"))
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

    fn test_chat_api() -> ChatApi {
        ChatApi::new(ChatService::new(
            Arc::new(FileChatStore::new(
                std::env::temp_dir().join(format!("screen-api-test-{}", uuid::Uuid::new_v4())),
            )),
            ChatLlmService::new(Arc::new(MultiLLMService::new(
                std::collections::HashMap::new(),
                std::collections::HashMap::new(),
            ))),
            Arc::new(PromptManager::new(Arc::new(DummyPromptStore))),
            Arc::new(RuntimeTransportBroadcaster::new(16)),
        ))
    }

    fn png_header(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = vec![0_u8; 24];
        bytes[..8].copy_from_slice(b"\x89PNG\r\n\x1a\n");
        bytes[16..20].copy_from_slice(&width.to_be_bytes());
        bytes[20..24].copy_from_slice(&height.to_be_bytes());
        bytes
    }

    #[test]
    fn png_image_size_reads_header_dimensions() {
        assert_eq!(png_image_size(&png_header(640, 360)), Some((640, 360)));
        assert_eq!(png_image_size(b"not-png"), None);
    }

    #[test]
    fn frontmost_window_id_takes_max_z_among_visible_current_space_windows() {
        // Listing order is not stacking order: the minimized window comes
        // first and carries the highest z_index.
        let windows = vec![
            json!({ "window_id": 1, "is_on_screen": false, "on_current_space": null, "z_index": 900 }),
            json!({ "window_id": 2, "is_on_screen": true, "on_current_space": true, "z_index": 10 }),
            json!({ "window_id": 3, "is_on_screen": true, "on_current_space": false, "z_index": 800 }),
            json!({ "window_id": 4, "is_on_screen": true, "on_current_space": true, "z_index": 40 }),
        ];
        assert_eq!(frontmost_window_id(&windows), Some(4));

        let unordered = vec![
            json!({ "window_id": 5, "is_on_screen": true, "on_current_space": true, "z_index": null }),
            json!({ "window_id": 6, "is_on_screen": true, "on_current_space": true, "z_index": null }),
        ];
        assert_eq!(frontmost_window_id(&unordered), Some(5));

        let hidden = vec![json!({ "window_id": 7, "is_on_screen": false, "z_index": 1 })];
        assert_eq!(frontmost_window_id(&hidden), None);
    }

    #[test]
    fn desktop_app_tutor_launch_target_is_allowlisted() {
        assert_eq!(desktop_app_tutor_launch_target("Notes"), Some("Notes"));
        assert_eq!(
            desktop_app_tutor_launch_target("apple music"),
            Some("Music")
        );
        assert_eq!(
            desktop_app_tutor_launch_target("Text Edit"),
            Some("TextEdit")
        );
        assert_eq!(desktop_app_tutor_launch_target("Terminal"), None);
        assert_eq!(desktop_app_tutor_launch_target("open -a Terminal"), None);
    }

    #[test]
    fn region_capture_context_marks_known_rect_as_capture_space() {
        let rect = ScreenCaptureRequestRect {
            x: 120,
            y: 80,
            width: 640,
            height: 360,
        };
        let context = screen_capture_attachment_context(
            "region",
            Some(&rect),
            "image/png",
            &png_header(640, 360),
        )
        .expect("region context");

        assert!(context.server_registered);
        assert_eq!(
            context.coordinate_space,
            SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE
        );
        assert_eq!(
            context.image_size.as_ref().map(|s| (s.width, s.height)),
            Some((640, 360))
        );
        assert_eq!(
            context
                .screen_rect
                .as_ref()
                .map(|r| (r.x, r.y, r.width, r.height)),
            Some((120, 80, 640, 360))
        );
    }

    #[test]
    fn region_capture_context_marks_native_picker_as_crop_local() {
        let context =
            screen_capture_attachment_context("region", None, "image/png", &png_header(320, 180))
                .expect("region context");

        assert_eq!(
            context.coordinate_space,
            SCREEN_CAPTURE_COORDINATE_SPACE_CROP_LOCAL
        );
        assert!(context.screen_rect.is_none());
    }

    #[test]
    fn full_screenshot_context_is_server_registered_and_dimensioned() {
        let context = screen_capture_attachment_context(
            "screenshot",
            None,
            "image/png",
            &png_header(1179, 2556),
        )
        .expect("full screenshot context");

        assert!(context.server_registered);
        assert_eq!(
            context.coordinate_space,
            SCREEN_CAPTURE_COORDINATE_SPACE_CAPTURE
        );
        assert_eq!(
            context
                .image_size
                .as_ref()
                .map(|size| (size.width, size.height)),
            Some((1179, 2556))
        );
        assert_eq!(
            context
                .screen_rect
                .as_ref()
                .map(|rect| (rect.x, rect.y, rect.width, rect.height)),
            Some((0, 0, 1179, 2556))
        );
    }

    #[test]
    fn observe_requests_accept_deep_observation_flags() {
        let start: ScreenObserveStartRequest =
            serde_json::from_value(json!({ "deep_observation": true })).unwrap();
        assert_eq!(start.deep_observation, Some(true));

        let start_alias: ScreenObserveStartRequest =
            serde_json::from_value(json!({ "deep_understanding": true })).unwrap();
        assert_eq!(start_alias.deep_observation, Some(true));

        let retarget: ScreenObserveRetargetRequest =
            serde_json::from_value(json!({ "deep_observation": false })).unwrap();
        assert_eq!(retarget.deep_observation, Some(false));

        let retarget_alias: ScreenObserveRetargetRequest =
            serde_json::from_value(json!({ "deep_understanding": true })).unwrap();
        assert_eq!(retarget_alias.deep_observation, Some(true));
    }

    #[tokio::test]
    async fn daily_screens_session_resolution_keeps_default_capture_contract() {
        let chat_api = test_chat_api();
        let today_title = "Screens — 2099-01-02";

        let first = resolve_daily_screens_session(&chat_api, "anonymous", "default", today_title)
            .await
            .expect("daily screens session");

        assert_eq!(first.ui_thread_id, SCREENS_THREAD_ID);
        assert_eq!(first.title.as_deref(), Some(today_title));

        let second = resolve_daily_screens_session(&chat_api, "anonymous", "default", today_title)
            .await
            .expect("same-day screens session");

        assert_eq!(second.id, first.id);
        assert_eq!(second.ui_thread_id, SCREENS_THREAD_ID);
        assert_eq!(second.title.as_deref(), Some(today_title));
    }
}

/// Find-or-create TODAY's session under the `screens` thread.
///
/// The active session is reused only when it already carries today's title;
/// an older day's session rotates out via `new_session` (which archives the
/// previous active session for the thread — the thread keeps the history).
/// A fresh/untitled session with no messages is claimed for today instead of
/// rotating, so the first-ever capture doesn't archive an empty session.
async fn resolve_daily_screens_session(
    chat_api: &ChatApi,
    principal: &str,
    workspace: &str,
    today_title: &str,
) -> anyhow::Result<magician::magician_v2::chat::models::ChatSession> {
    let mut session = chat_api
        .chat_service
        .get_or_create_automated_session(
            principal,
            workspace,
            SCREENS_THREAD_ID,
            &ChatChannel::web(),
            None,
        )
        .await?;
    if session.title.as_deref() == Some(today_title) {
        return Ok(session);
    }
    let has_messages = !chat_api
        .chat_service
        .chat_store_ref()
        .get_messages(&session.id, 1)
        .await?
        .is_empty();
    let already_titled = session
        .title
        .as_deref()
        .is_some_and(|t| !t.trim().is_empty());
    if has_messages || already_titled {
        session = chat_api
            .chat_service
            .new_automated_session(principal, workspace, SCREENS_THREAD_ID, &ChatChannel::web())
            .await?;
    }
    chat_api
        .chat_service
        .chat_store_ref()
        .update_session_title(&session.id, today_title)
        .await?;
    session.title = Some(today_title.to_string());
    Ok(session)
}

/// Body for `POST /screen/observations/distill`.
#[derive(serde::Deserialize)]
pub struct ScreenObservationsDistillRequest {
    /// Look-back window in days (default 1, min 1).
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// WEG Phase 4 (desktop connector): distil the user's `screen_observations`
/// tier into user-owned evidence. Mirrors the ambient-browser lane
/// (`POST /ambient/distill`) — reads the per-session observation entries,
/// clusters them by `(purpose, day)`, salience-gates, distils each promotable
/// cluster (one LLM call) into an `EvidenceRecord` tagged
/// `producer = "screen_observation"`, routes a review-gated `user.knowledge`
/// candidate, and appends to the user-owned evidence + entity lanes. Generic /
/// facet-agnostic; idempotent per `(purpose, day)`.
pub async fn post_screen_observations_distill_handler(
    resources: web::Data<Arc<AgentResources>>,
    operation_router: web::Data<
        magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    >,
    prompt_manager: web::Data<Arc<magician::magician_v2::prompts::PromptManager>>,
    req: HttpRequest,
    body: web::Json<ScreenObservationsDistillRequest>,
) -> HttpResponse {
    use magician::magician_v2::evidence::{
        cluster_screen_observations, distill_screen_cluster_source_bound,
        entity_candidates_from_evidence, is_salient, is_screen_cluster_salient,
        stamp_screen_evidence, ScreenObservationRow,
    };

    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let days = body.days.unwrap_or(1).max(1);
    let since_day = (chrono::Utc::now() - chrono::Duration::days(days))
        .format("%Y-%m-%d")
        .to_string();

    let memory = match resources
        .memory_resolver
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(err) => {
            return HttpResponse::BadRequest().json(json!({ "error": err.to_string() }));
        },
    };
    let knowledge = match memory.load_user_knowledge().await {
        Ok(knowledge) => knowledge,
        Err(err) => {
            return HttpResponse::InternalServerError().json(json!({ "error": err.to_string() }));
        },
    };
    let tier = normalized_user_memory_tier_name(SCREEN_MEMORY_TIER).unwrap_or_default();
    let rows: Vec<ScreenObservationRow> = knowledge
        .get(tier.as_str())
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    serde_json::from_value::<ScreenObservationRow>(entry.clone()).ok()
                })
                // Keep entries within the window; undated entries are kept (rare,
                // legacy) rather than silently dropped.
                .filter(|r| {
                    r.date
                        .as_deref()
                        .map(|d| d >= since_day.as_str())
                        .unwrap_or(true)
                })
                .collect()
        })
        .unwrap_or_default();

    if rows.is_empty() {
        return HttpResponse::Ok().json(json!({
            "observations": 0, "clusters": 0, "promotable": 0, "distilled": 0,
        }));
    }

    let clusters = cluster_screen_observations(&rows);
    let promotable: Vec<_> = clusters
        .iter()
        .filter(|c| is_screen_cluster_salient(c))
        .cloned()
        .collect();
    let now = chrono::Utc::now().to_rfc3339();
    let mut distilled = 0usize;
    let telemetry = resources.event_broadcaster.as_ref().map(|broadcaster| {
        magician::magician_v2::analytics::operation_llm_telemetry::OperationLlmTelemetryContext::new(
            Arc::clone(broadcaster),
            principal.clone(),
            workspace.clone(),
            "evidence_distillation",
        )
    });
    let scoped_operation_router = operation_router.with_scope_context(Some(
        magicllm::LlmScope::new(principal.clone(), workspace.clone()),
    ));

    for cluster in &promotable {
        let proposal = match distill_screen_cluster_source_bound(
            cluster,
            &scoped_operation_router,
            &prompt_manager,
            &memory,
            &knowledge[tier.as_str()],
            telemetry.as_ref(),
        )
        .await
        {
            Ok(proposal) => proposal,
            Err(err) => {
                tracing::warn!(purpose = %cluster.purpose, error = %err, "screen distill failed (skipped)");
                continue;
            },
        };
        if let Some(record) = stamp_screen_evidence(&proposal, cluster, &now) {
            if is_salient(&record) {
                let mut candidates = entity_candidates_from_evidence(&record);
                for c in &mut candidates {
                    c.producer = "screen_observation".to_string();
                }
                // WEG retrieval bridge: route this user-owned observation into a
                // review-gated `user.knowledge` memory candidate (same as the
                // ambient lane). Fail-soft — never blocks the evidence append.
                let learning_scope = magician::magician_v2::learning::LearningScope::new(
                    principal.clone(),
                    workspace.clone(),
                );
                if let Err(err) = magician::magician_v2::learning::route_user_evidence_to_memory(
                    &resources.artifact_workspace,
                    &learning_scope,
                    &record,
                )
                .await
                {
                    tracing::warn!(
                        evidence_id = %record.evidence_id,
                        error = %err,
                        "screen evidence → memory candidate route failed (non-fatal, skipped)"
                    );
                }
                if let Err(err) = memory.append_user_work_evidence(record).await {
                    return HttpResponse::InternalServerError()
                        .json(json!({ "error": err.to_string() }));
                }
                if !candidates.is_empty() {
                    let _ = memory.resolve_user_work_entities(candidates).await;
                }
                distilled += 1;
            }
        }
    }

    HttpResponse::Ok().json(json!({
        "observations": rows.len(),
        "clusters": clusters.len(),
        "promotable": promotable.len(),
        "distilled": distilled,
    }))
}

/// Body for the generic `POST /evidence/distill/{producer}`.
#[derive(serde::Deserialize)]
pub struct TierDistillRequest {
    /// Look-back window in days (default 1, min 1).
    #[serde(default)]
    pub days: Option<i64>,
    #[serde(default)]
    pub workspace: Option<String>,
}

/// WEG Phase 4 (generic tier connector): distil ANY producer's per-`(key, day)`
/// roll-up tier into user-owned evidence — ONE handler for `meeting` | `email` |
/// `calendar` | future lanes, selected by the `{producer}` path segment via
/// `evidence::tier_distill::producer_spec`. Reads the producer's tier, salience-gates,
/// distils each promotable cluster (one generic LLM call) into an `EvidenceRecord`
/// tagged with the spec's producer, routes a review-gated `user.knowledge`
/// candidate, and appends to the user-owned evidence + entity lanes. Generic /
/// facet-agnostic; idempotent per the spec's id. Adding a connector = a registry
/// entry + a writer that fills its tier — no new handler.
pub async fn post_tier_distill_handler(
    resources: web::Data<Arc<AgentResources>>,
    operation_router: web::Data<
        magician::magician_v2::query_analysis::operation_llm_router::OperationLlmRouter,
    >,
    prompt_manager: web::Data<Arc<magician::magician_v2::prompts::PromptManager>>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<TierDistillRequest>,
) -> HttpResponse {
    // Thin scope-resolution shell over the shared `distill_tier_producer`
    // routine — the SAME path the `distill_evidence` tool drives, so an HTTP
    // call and a writer-triggered tool call distil identically.
    let producer = path.into_inner();
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(resp) => return resp,
    };
    let days = body.days.unwrap_or(1).max(1) as u64;

    match magician::magician_v2::evidence::distill_tier_producer_with_broadcaster(
        resources.memory_resolver.as_ref(),
        &resources.artifact_workspace,
        &principal,
        &workspace,
        &producer,
        days,
        operation_router.get_ref(),
        prompt_manager.get_ref(),
        resources.event_broadcaster.as_ref(),
    )
    .await
    {
        Ok(summary) => HttpResponse::Ok().json(summary),
        Err(err) => HttpResponse::BadRequest().json(json!({ "error": err })),
    }
}

/// Append one provenance entry per capture (`key = screen:<capture_id>`) to
/// the screen_observations tier — same bounded merge the meet-bot uses, so
/// captures APPEND (newest N retained) instead of overwriting one entry.
async fn append_capture_memory(
    resources: &Arc<AgentResources>,
    principal: &str,
    workspace: &str,
    capture_id: &str,
    mode: &str,
    session_id: &str,
    stored_names: &[String],
) {
    let Some(tier) = normalized_user_memory_tier_name(SCREEN_MEMORY_TIER) else {
        tracing::warn!(
            target: "screen_capture",
            "screen memory tier name rejected: {SCREEN_MEMORY_TIER}"
        );
        return;
    };
    if tier.is_empty() {
        return;
    }
    let now = chrono::Local::now();
    let entry_key = format!("screen:{capture_id}");
    let entry = json!({
        "key": entry_key,
        "source_type": "screen_capture",
        "mode": mode,
        "date": now.format("%Y-%m-%d").to_string(),
        "time": now.format("%H:%M").to_string(),
        "session_id": session_id,
        "files": stored_names,
    });
    let mut tier_fields = serde_json::Map::new();
    tier_fields.insert(entry_key, entry);
    let result = merge_user_memory_tier_fields_with_retention(
        resources.memory_resolver.as_ref(),
        principal,
        workspace,
        &tier,
        &tier_fields,
        Some(("screen:", SCREEN_MEMORY_MAX_ENTRIES)),
    )
    .await;
    tracing::debug!(
        target: "screen_capture",
        ?result,
        "capture provenance appended to memory tier {tier}"
    );
}

// ─── client-pushed screen observation (iOS broadcast) ─────────────────────

/// Hard per-frame body cap. A downscaled JPEG keyframe is well under this; the
/// route-level `PayloadConfig` in `bin/magician.rs` backs it.
const MAX_OBSERVE_FRAME_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, serde::Deserialize)]
pub struct ScreenObserveClientStartRequest {
    /// UI thread the screen narration lands in — pass the meeting thread to weave
    /// screen notes into the meeting transcript.
    pub thread: String,
    pub title: Option<String>,
    pub purpose: Option<String>,
    pub mode: Option<String>,
    pub deep_observation: Option<bool>,
    pub cadence_s: Option<u64>,
    pub max_minutes: Option<u64>,
}

/// POST /api/magician/v2/screen/observe/client/start — start a screen
/// observation fed by CLIENT-PUSHED frames (the iOS broadcast extension),
/// narrating into `thread`. Returns the observe id + the frame upload token the
/// client echoes on every `POST /screen/observe/frame`.
pub async fn screen_observe_client_start_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<ScreenScopeQuery>,
    body: web::Json<ScreenObserveClientStartRequest>,
) -> impl Responder {
    use magician_media::media_rails::screen_observe::{
        default_cadence_s, start_client_screen_observation, ObserveAudioConfig, ObserveConfig,
        ObserveMode, ObserveTarget, DEFAULT_MAX_MINUTES,
    };
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let request = body.into_inner();
    let thread = request.thread.trim().to_string();
    if thread.is_empty() {
        return HttpResponse::BadRequest().json(json!({ "error": "`thread` is required" }));
    }
    let purpose = request
        .purpose
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .unwrap_or("Meeting screen share — note slides, shared documents, and on-screen content.")
        .to_string();
    let mode = ObserveMode::parse(request.mode.as_deref().unwrap_or("notes"));
    let config = ObserveConfig {
        cadence_s: request
            .cadence_s
            .filter(|c| *c >= 2)
            .unwrap_or_else(default_cadence_s),
        max_minutes: request.max_minutes.unwrap_or(DEFAULT_MAX_MINUTES),
        thread: thread.clone(),
        session_title: request
            .title
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .unwrap_or("Meeting screen")
            .to_string(),
        audio: ObserveAudioConfig::default(),
    };
    let target = ObserveTarget {
        purpose,
        mode,
        watch_for: None,
        stop_on_match: false,
        deep_observation: request.deep_observation.unwrap_or(false),
    };
    match start_client_screen_observation(
        config,
        target,
        resources.get_ref().clone(),
        principal,
        workspace,
    )
    .await
    {
        Ok((view, upload_token)) => HttpResponse::Ok().json(json!({
            "observe_id": view.observe_id,
            "upload_token": upload_token,
            "thread": thread,
            "status": view.status.as_str(),
        })),
        // A host observation already holds the single observation slot.
        Err(e) => HttpResponse::Conflict().json(json!({ "error": e })),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ObserveFrameQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    pub observe_id: String,
}

/// POST /api/magician/v2/screen/observe/frame — one client-pushed keyframe
/// (raw JPEG/PNG bytes) for a running client observation. 202 accepted, 410 when
/// the session is gone / scope or token mismatch.
pub async fn ingest_observe_frame_handler(
    req: HttpRequest,
    query: web::Query<ObserveFrameQuery>,
    body: web::Bytes,
) -> impl Responder {
    use magician_media::media_rails::screen_observe::{push_observe_frame, FrameIngestResult};
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if body.len() > MAX_OBSERVE_FRAME_BYTES {
        return HttpResponse::PayloadTooLarge()
            .json(json!({ "error": "frame_too_large", "max_bytes": MAX_OBSERVE_FRAME_BYTES }));
    }
    if body.is_empty() {
        return HttpResponse::BadRequest().json(json!({ "error": "empty_frame" }));
    }
    let token = req
        .headers()
        .get("x-upload-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    match push_observe_frame(
        &query.observe_id,
        &principal,
        &workspace,
        &token,
        body.to_vec(),
    ) {
        FrameIngestResult::Accepted => HttpResponse::Accepted().json(json!({ "accepted": true })),
        FrameIngestResult::Gone => {
            HttpResponse::Gone().json(json!({ "error": "observation_gone" }))
        },
    }
}

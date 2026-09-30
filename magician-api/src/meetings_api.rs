//! Meetings API — the HTTP surface behind the Meetings page.
//!
//! Two rails, one surface (plan:
//! `docs/archive/plans/2026-06-10-meetings-surface-passive-listener-plan.md`):
//!   * `POST /meetings/listen` — the PASSIVE listener (local display-audio + mic
//!     capture, live transcript into the meeting thread, never joins the call).
//!   * `POST /meetings/join`  — the AGENT ATTENDEE (the existing meet-bot:
//!     browser auto-join + wake-word voice), via the same shared join path the
//!     compiled `meeting` tool uses.
//! Both rails resolve the meeting thread through the shared resolver, so a
//! listen and a join of the same meeting converge on the same dated thread.
//!
//! Routes (registered under `/api/magician/v2` in `bin/magician.rs`):
//! ```text
//! GET  /meetings              -> active sessions (both rails) + recent meeting threads
//! GET  /meetings/active       -> active sessions only
//! POST /meetings/listen       -> start a passive listener  { url?, title?, date?, mic? }
//! POST /meetings/join         -> join as the agent          { url, title?, date?, display_name? }
//! GET  /meetings/{id}         -> one session's status (either rail)
//! POST /meetings/{id}/stop    -> stop/leave (either rail); passive stop acks immediately
//! ```
//!
//! Scope comes from the verified bearer like the
//! sibling APIs; it keys where the transcript thread + teardown memories land.
//! The session registries themselves are process-global (single-operator
//! deployment), so `active` lists every live capture regardless of scope —
//! by design: active capture must never be hidden from the operator.

use std::{collections::BTreeMap, sync::Arc};

use actix_web::{web, HttpRequest, HttpResponse, Responder};
use serde::Deserialize;
use serde_json::json;

use crate::chat_api::ChatApi;
use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::CapabilityWorkspaceManager;
use magician::magician_v2::execution::agent_resources::AgentResources;
use magician::magician_v2::execution::compiled_providers::join_meeting_with_scope;
// The gws calendar read, its account resolution, dedupe/merge and its
// short-lived per-scope cache live in `media_seam::meeting_calendar` — the ONE
// owner shared with the `meetings_data` app read binder. This module keeps the
// route, the scope check and the response shape; it does not re-implement the
// read.
use magician::magician_v2::media_seam::meeting_calendar::upcoming_meetings_cached;
// Capture starts and stops are audited on the shared path, not inside either
// caller: these handlers and the reviewed `magician.meeting-control` app
// destination append to the same per-scope log.
use magician::magician_v2::media_seam::meeting_control_audit::{
    record_capture_control_detached, CaptureControlOrigin, CaptureControlOutcome,
    CaptureControlRecord, CaptureControlVerb,
};
use magician_media::media_rails::meeting::{
    meeting_manager, passive_meeting_manager, push_ingest_chunk, resolve_meeting_thread,
    start_client_passive_listener, start_passive_listener, IngestPushResult, MarkerContext,
    MeetingConfig, MeetingStatus, PassiveListenerConfig, PassiveStatus, PassiveStatusView,
    PushChannel, ScopedMeetingMemoryWriter, StartClientListenerError,
};
use magician_media::media_rails::providers::AudioChunk;
use magician_media::AudioStage;

#[derive(Debug, Deserialize)]
pub struct MeetingsScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ListenMeetingRequest {
    /// Meeting URL, when known (metadata + thread naming only — never opened).
    #[serde(default)]
    pub url: Option<String>,
    /// Meeting / calendar-event title (keys the thread + session title).
    #[serde(default)]
    pub title: Option<String>,
    /// Scheduled date `YYYY-MM-DD`; defaults to today.
    #[serde(default)]
    pub date: Option<String>,
    /// Capture the user's microphone as the `You` track (default true).
    #[serde(default)]
    pub mic: Option<bool>,
    #[serde(default)]
    pub audio_profile: Option<String>,
    #[serde(default)]
    pub audio_stage_options: BTreeMap<AudioStage, String>,
    /// Capture source: `"host"` (default — the Mac captures its own audio) or
    /// `"client"` (a remote client, e.g. the iOS in-app mic listener, pushes
    /// PCM chunks to `POST /meetings/{id}/audio`).
    #[serde(default)]
    pub capture: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinMeetingRequest {
    pub url: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub audio_profile: Option<String>,
    #[serde(default)]
    pub audio_stage_options: BTreeMap<AudioStage, String>,
}

/// Query for `POST /meetings/{id}/audio` (client capture chunk ingest).
#[derive(Debug, Deserialize)]
pub struct AudioIngestQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// `primary` (diarized room audio) or `mic` (hard-"You").
    pub channel: String,
    /// Monotonic best-effort chunk sequence for the channel.
    #[serde(default)]
    pub seq: u64,
}

/// Hard per-chunk body cap enforced in-handler (a route-level `PayloadConfig`
/// backs this in `bin/magician.rs`). ~2 MiB comfortably holds a 6 s PCM16 chunk
/// (192 KB) with slack.
const MAX_INGEST_CHUNK_BYTES: usize = 2 * 1024 * 1024;

/// Append one first-party control act to the shared capture-control audit.
/// Best-effort by contract — an audit failure is logged, never surfaced — so a
/// stop can never be blocked by the record of it.
#[allow(clippy::too_many_arguments)]
fn audit_first_party_control(
    resources: &AgentResources,
    principal: &str,
    workspace: &str,
    verb: CaptureControlVerb,
    outcome: CaptureControlOutcome,
    session_id: Option<String>,
    thread_id: Option<String>,
    title: Option<String>,
    url: Option<String>,
    detail: Option<String>,
) {
    let workdirs_root = resources
        .artifact_workspace
        .capability_workdirs_root(principal, workspace);
    record_capture_control_detached(
        &workdirs_root,
        CaptureControlRecord::new(
            CaptureControlOrigin::FirstPartyApi,
            verb,
            outcome,
            principal,
            workspace,
        )
        .with_session(session_id)
        .with_meeting(thread_id, title, url)
        .with_detail(detail),
    );
}

fn passive_json(v: &PassiveStatusView) -> serde_json::Value {
    json!({
        "session_id": v.session_id,
        "mode": "passive",
        "status": format!("{:?}", v.status),
        "thread_id": v.thread,
        "title": v.title,
        "url": v.url,
        "mic": v.capture_mic,
        "paused": v.paused,
        "latest_summary": v.latest_summary,
    })
}

/// One attendee-row shape for BOTH the active list and the status endpoint, so
/// the two can't drift field-by-field.
#[allow(clippy::too_many_arguments)]
fn attendee_json(
    session_id: &str,
    status: &MeetingStatus,
    url: &str,
    thread: &Option<String>,
    title: &Option<String>,
    paused: bool,
    latest_summary: &Option<String>,
) -> serde_json::Value {
    json!({
        "session_id": session_id,
        "mode": "attendee",
        "status": format!("{:?}", status),
        "thread_id": thread,
        "title": title,
        "url": url,
        "paused": paused,
        "latest_summary": latest_summary,
    })
}

/// Liveness — ended sessions are retained in the registries for 5 minutes so
/// `status` stays queryable, but they must NOT count as active capture (the
/// TopBar recording dot and the Active grid key off this).
fn attendee_is_live(status: &MeetingStatus) -> bool {
    !matches!(status, MeetingStatus::Left | MeetingStatus::Failed)
}

fn passive_is_live(status: &PassiveStatus) -> bool {
    matches!(status, PassiveStatus::Listening)
}

async fn active_sessions_json() -> Vec<serde_json::Value> {
    let mut rows = Vec::new();
    for v in passive_meeting_manager().list().await {
        if passive_is_live(&v.status) {
            rows.push(passive_json(&v));
        }
    }
    for r in meeting_manager().list().await {
        if attendee_is_live(&r.status) {
            rows.push(attendee_json(
                &r.session_id,
                &r.status,
                &r.url,
                &r.thread,
                &r.title,
                r.paused,
                &r.latest_summary,
            ));
        }
    }
    rows
}

/// GET /meetings — active sessions (both rails) + the scope's recent
/// `meeting-*` chat threads (latest session per thread, newest first).
pub async fn list_meetings_handler(
    chat_api: web::Data<ChatApi>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };

    let active = active_sessions_json().await;

    // Index-backed: only `meeting-*` sessions' documents are loaded — listing
    // never scales with the scope's full chat history.
    let recent = match chat_api
        .chat_service
        .list_sessions_for_thread_prefix(&principal, &workspace, "meeting-")
        .await
    {
        Ok(sessions) => {
            let mut meeting_sessions = sessions;
            meeting_sessions.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
            // Latest session per thread (a thread can carry rotated sessions).
            let mut seen = std::collections::HashSet::new();
            meeting_sessions
                .into_iter()
                .filter(|s| seen.insert(s.ui_thread_id.clone()))
                .take(50)
                .map(|s| {
                    json!({
                        "thread_id": s.ui_thread_id,
                        "session_id": s.id,
                        "title": s.title,
                        "agent_id": s.agent_id,
                        "updated_at": s.updated_at,
                    })
                })
                .collect::<Vec<_>>()
        },
        Err(e) => {
            return HttpResponse::InternalServerError()
                .json(json!({ "error": format!("list sessions: {e}") }));
        },
    };

    HttpResponse::Ok().json(json!({ "active": active, "recent": recent }))
}

/// GET /meetings/active — active sessions only (both rails).
pub async fn active_meetings_handler(
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
) -> impl Responder {
    let query = query.into_inner();
    if let Err(response) = resolve_required_scope(req.headers(), query.workspace) {
        return response;
    }
    HttpResponse::Ok().json(json!({ "active": active_sessions_json().await }))
}

/// POST /meetings/listen — start the passive listener (does NOT join the call).
pub async fn listen_meeting_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    body: web::Json<ListenMeetingRequest>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();

    // Normalize once: the resolver trims internally, but the stored config is
    // also what status views echo back — `Some("")` would render blank labels.
    let title = body
        .title
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);
    let url = body
        .url
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
        .map(str::to_string);
    let resolved = resolve_meeting_thread(url.as_deref(), title.as_deref(), body.date.as_deref());
    // Retained for the shared capture-control audit: the config takes ownership
    // of `url` below, and the audit row must name the meeting the operator
    // actually asked for.
    let audited_url = url.clone();
    let config = PassiveListenerConfig {
        thread: resolved.thread.clone(),
        session_title: resolved.session_title.clone(),
        title,
        url,
        date: resolved.date.clone(),
        // OPT-IN: the mic tap reads the OS default input DEVICE, which an
        // app-level mute (Meet's mute button) does not silence — it hears
        // the room (background conversations included) whenever it's on.
        // Recording the user's side must be an explicit choice.
        capture_mic: body.mic.unwrap_or(false),
        summarize_every_turns: 40,
        audio_profile: body.audio_profile,
        audio_stage_options: body.audio_stage_options,
    };
    let writer = Arc::new(ScopedMeetingMemoryWriter::new(
        resources.get_ref().clone(),
        principal.clone(),
        workspace.clone(),
    ));
    // Crash marker: lets the boot sweep explain an interrupted capture
    // in-thread after a server death.
    let marker = MarkerContext::for_scope(
        resources
            .artifact_workspace
            .capability_workdirs_root(&principal, &workspace),
        &principal,
        &workspace,
    );
    // Client capture: a remote client (e.g. the iOS in-app mic listener) will
    // push PCM chunks to POST /meetings/{id}/audio instead of the host
    // capturing its own audio. Everything downstream (transcript, summaries,
    // memory) is identical.
    if body.capture.as_deref() == Some("client") {
        return match start_client_passive_listener(
            config,
            writer,
            Some((principal.clone(), workspace.clone())),
            Some(marker),
            resources.event_broadcaster.clone(),
        )
        .await
        {
            Ok(res) => {
                audit_first_party_control(
                    resources.get_ref(),
                    &principal,
                    &workspace,
                    CaptureControlVerb::Listen,
                    CaptureControlOutcome::Accepted,
                    Some(res.session_id.clone()),
                    Some(resolved.thread.clone()),
                    Some(resolved.session_title.clone()),
                    audited_url.clone(),
                    Some("client capture".to_owned()),
                );
                HttpResponse::Ok().json(json!({
                    "session_id": res.session_id,
                    "mode": "passive",
                    "status": "Listening",
                    "thread_id": resolved.thread,
                    "title": resolved.session_title,
                    "capture_source": "client",
                    "upload_token": res.upload_token,
                    "reused": res.reused,
                }))
            },
            Err(StartClientListenerError::Conflict {
                existing_session_id,
            }) => {
                audit_first_party_control(
                    resources.get_ref(),
                    &principal,
                    &workspace,
                    CaptureControlVerb::Listen,
                    CaptureControlOutcome::Refused,
                    Some(existing_session_id.clone()),
                    Some(resolved.thread.clone()),
                    Some(resolved.session_title.clone()),
                    audited_url.clone(),
                    Some("already observed from another source".to_owned()),
                );
                HttpResponse::Conflict().json(json!({
                    "error": "already_observed",
                    "message": "This meeting is already being observed from another source (e.g. the Mac).",
                    "existing_session_id": existing_session_id,
                    "capture_source": "host",
                }))
            },
            Err(StartClientListenerError::Internal(e)) => {
                audit_first_party_control(
                    resources.get_ref(),
                    &principal,
                    &workspace,
                    CaptureControlVerb::Listen,
                    CaptureControlOutcome::Refused,
                    None,
                    Some(resolved.thread.clone()),
                    Some(resolved.session_title.clone()),
                    audited_url.clone(),
                    Some(e.clone()),
                );
                HttpResponse::InternalServerError().json(json!({ "error": e }))
            },
        };
    }

    match start_passive_listener(
        config,
        writer,
        Some((principal.clone(), workspace.clone())),
        Some(marker),
        resources.event_broadcaster.clone(),
    )
    .await
    {
        Ok(session_id) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Listen,
                CaptureControlOutcome::Accepted,
                Some(session_id.clone()),
                Some(resolved.thread.clone()),
                Some(resolved.session_title.clone()),
                audited_url.clone(),
                None,
            );
            HttpResponse::Ok().json(json!({
                "session_id": session_id,
                "mode": "passive",
                "status": "Listening",
                "thread_id": resolved.thread,
                "title": resolved.session_title,
                "capture_source": "host",
            }))
        },
        Err(e) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Listen,
                CaptureControlOutcome::Refused,
                None,
                Some(resolved.thread.clone()),
                Some(resolved.session_title.clone()),
                audited_url.clone(),
                Some(e.clone()),
            );
            HttpResponse::InternalServerError().json(json!({ "error": e }))
        },
    }
}

/// POST /meetings/{id}/audio — ingest one client-captured audio chunk into a
/// `capture: "client"` passive listener. v1 accepts raw PCM16 LE 16 kHz mono
/// (`audio/pcm`); AAC is a fast-follow. Non-blocking: a chunk beyond the queue
/// is newest-dropped (counted), never awaited. `410 Gone` is the terminal signal
/// (unknown/ended session, wrong scope, or bad token — uniform, no oracle).
pub async fn ingest_meeting_audio_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AudioIngestQuery>,
    body: web::Bytes,
) -> impl Responder {
    let session_id = path.into_inner();
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let channel = match PushChannel::parse(&query.channel) {
        Some(c) => c,
        None => {
            return HttpResponse::BadRequest()
                .json(json!({ "error": "unknown_channel", "channel": query.channel }))
        },
    };
    if body.len() > MAX_INGEST_CHUNK_BYTES {
        return HttpResponse::PayloadTooLarge()
            .json(json!({ "error": "chunk_too_large", "max_bytes": MAX_INGEST_CHUNK_BYTES }));
    }
    // v1 accepts only raw PCM. AAC (audio/aac) is a documented fast-follow.
    let content_type = req
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if content_type.starts_with("audio/aac") {
        return HttpResponse::UnsupportedMediaType()
            .json(json!({ "error": "aac_not_supported_in_v1" }));
    }
    let token = req
        .headers()
        .get("x-upload-token")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let chunk = AudioChunk {
        seq: query.seq,
        pcm: body,
    };
    match push_ingest_chunk(&session_id, channel, &principal, &workspace, &token, chunk) {
        IngestPushResult::Accepted { dropped } => HttpResponse::Accepted()
            .json(json!({ "accepted": true, "dropped_total": dropped, "paused": false })),
        IngestPushResult::Gone => HttpResponse::Gone().json(json!({ "error": "session_gone" })),
    }
}

/// POST /meetings/join — join as the agent attendee (the existing meet-bot),
/// through the same shared path the compiled `meeting` tool uses.
pub async fn join_meeting_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    body: web::Json<JoinMeetingRequest>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let body = body.into_inner();
    if body.url.trim().is_empty() {
        return HttpResponse::BadRequest().json(json!({ "error": "url is required" }));
    }

    let resolved =
        resolve_meeting_thread(Some(&body.url), body.title.as_deref(), body.date.as_deref());
    let mut config = MeetingConfig {
        meet_url: body.url.trim().to_string(),
        ..Default::default()
    };
    if let Some(name) = body.display_name.filter(|n| !n.trim().is_empty()) {
        config.display_name = name;
    }
    config.title = body.title.filter(|t| !t.trim().is_empty());
    config.meeting_date = body.date.filter(|d| !d.trim().is_empty());
    config.audio_profile = body.audio_profile;
    config.audio_stage_options = body.audio_stage_options;

    let memory_ctx = Some((
        resources.get_ref().clone(),
        principal.clone(),
        workspace.clone(),
    ));
    // The audit row for a join is written INSIDE `join_meeting_with_scope`, which
    // takes the origin as a required argument — one appender for the attendee
    // rail, so the compiled `meeting` tool's joins are recorded too. Writing a
    // second row here would double-count the same act.
    match join_meeting_with_scope(memory_ctx, config, CaptureControlOrigin::FirstPartyApi).await {
        Ok(session_id) => HttpResponse::Ok().json(json!({
            "session_id": session_id,
            "mode": "attendee",
            "status": "joining",
            "thread_id": resolved.thread,
            "title": resolved.session_title,
        })),
        Err(e) => HttpResponse::InternalServerError().json(json!({ "error": e })),
    }
}

/// GET /meetings/{id} — one session's status, whichever rail owns it.
pub async fn meeting_status_handler(
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    path: web::Path<String>,
) -> impl Responder {
    let query = query.into_inner();
    if let Err(response) = resolve_required_scope(req.headers(), query.workspace) {
        return response;
    }
    let session_id = path.into_inner();
    if let Some(v) = passive_meeting_manager().status(&session_id).await {
        return HttpResponse::Ok().json(passive_json(&v));
    }
    if let Some(v) = meeting_manager().status(&session_id).await {
        return HttpResponse::Ok().json(attendee_json(
            &v.session_id,
            &v.status,
            &v.url,
            &v.thread,
            &v.title,
            v.paused,
            &v.latest_summary,
        ));
    }
    HttpResponse::NotFound()
        .json(json!({ "error": "unknown meeting session", "session_id": session_id }))
}

/// POST /meetings/{id}/stop — stop a passive listener or leave an attendee
/// session. Passive capture is acknowledged immediately; transcript-tail STT,
/// final summarization, and memory persistence continue asynchronously.
pub async fn stop_meeting_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    path: web::Path<String>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let session_id = path.into_inner();
    // Dispatch by the rail-prefixed id (`listen-*` vs `meet-*`) so each rail's
    // own error surfaces — a fallthrough would mask a real passive-stop failure
    // behind the attendee manager's "unknown meeting session_id".
    if session_id.starts_with("listen-") {
        return match passive_meeting_manager().request_stop(&session_id).await {
            Ok((status, latest_summary)) => {
                audit_first_party_control(
                    resources.get_ref(),
                    &principal,
                    &workspace,
                    CaptureControlVerb::Stop,
                    CaptureControlOutcome::Accepted,
                    Some(session_id.clone()),
                    None,
                    None,
                    None,
                    None,
                );
                HttpResponse::Accepted().json(json!({
                    "session_id": session_id,
                    "mode": "passive",
                    "status": format!("{:?}", status).to_ascii_lowercase(),
                    "final_summary": latest_summary,
                }))
            },
            Err(e) => {
                audit_first_party_control(
                    resources.get_ref(),
                    &principal,
                    &workspace,
                    CaptureControlVerb::Stop,
                    CaptureControlOutcome::Refused,
                    Some(session_id.clone()),
                    None,
                    None,
                    None,
                    Some(e.clone()),
                );
                HttpResponse::NotFound().json(json!({ "error": e }))
            },
        };
    }
    match meeting_manager().leave(&session_id).await {
        Ok(final_summary) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Stop,
                CaptureControlOutcome::Accepted,
                Some(session_id.clone()),
                None,
                None,
                None,
                None,
            );
            HttpResponse::Ok().json(json!({
                "session_id": session_id,
                "mode": "attendee",
                "status": "left",
                "final_summary": final_summary,
            }))
        },
        Err(e) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Stop,
                CaptureControlOutcome::Refused,
                Some(session_id.clone()),
                None,
                None,
                None,
                Some(e.clone()),
            );
            HttpResponse::NotFound().json(json!({ "error": e }))
        },
    }
}

/// Pause is "stop the ears, stay in the meeting": the passive rail drops
/// capture chunks before STT; the attendee rail mutes Magician (no transcript,
/// no wake responses) without leaving the call. Same rail-prefix dispatch as
/// stop, for the same reason.
async fn set_meeting_paused(session_id: &str, paused: bool) -> Result<(), String> {
    if session_id.starts_with("listen-") {
        passive_meeting_manager()
            .set_paused(session_id, paused)
            .await
    } else {
        meeting_manager().set_paused(session_id, paused).await
    }
}

/// POST /meetings/{id}/pause — pause capture on a live session (either rail).
pub async fn pause_meeting_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    path: web::Path<String>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let session_id = path.into_inner();
    match set_meeting_paused(&session_id, true).await {
        Ok(()) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Pause,
                CaptureControlOutcome::Accepted,
                Some(session_id.clone()),
                None,
                None,
                None,
                None,
            );
            HttpResponse::Ok().json(json!({ "session_id": session_id, "paused": true }))
        },
        Err(e) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Pause,
                CaptureControlOutcome::Refused,
                Some(session_id.clone()),
                None,
                None,
                None,
                Some(e.clone()),
            );
            HttpResponse::NotFound().json(json!({ "error": e }))
        },
    }
}

/// POST /meetings/{id}/resume — resume capture on a paused session.
pub async fn resume_meeting_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<MeetingsScopeQuery>,
    path: web::Path<String>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let session_id = path.into_inner();
    match set_meeting_paused(&session_id, false).await {
        Ok(()) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Resume,
                CaptureControlOutcome::Accepted,
                Some(session_id.clone()),
                None,
                None,
                None,
                None,
            );
            HttpResponse::Ok().json(json!({ "session_id": session_id, "paused": false }))
        },
        Err(e) => {
            audit_first_party_control(
                resources.get_ref(),
                &principal,
                &workspace,
                CaptureControlVerb::Resume,
                CaptureControlOutcome::Refused,
                Some(session_id.clone()),
                None,
                None,
                None,
                Some(e.clone()),
            );
            HttpResponse::NotFound().json(json!({ "error": e }))
        },
    }
}

// ---------------------------------------------------------------------------
// Calendar context — upcoming meetings from the owner's Google Calendar
// ---------------------------------------------------------------------------
// Source: the `gws` CLI with the owner's profile (same CLI the calendar skill
// shells out to), NOT a new OAuth integration. Window/browser-tab sniffing was
// considered and CUT: it needs screen-recording entitlements + per-app
// heuristics for a worse signal than the calendar already provides.

#[derive(Debug, Deserialize)]
pub struct UpcomingMeetingsQuery {
    #[serde(default)]
    pub workspace: Option<String>,
    /// `?refresh=true` busts the cache (the page's manual refresh button).
    #[serde(default)]
    pub refresh: Option<bool>,
}

/// GET /meetings/upcoming — the owner's next ~12h of calendar meetings.
/// Deliberately its OWN endpoint (not folded into GET /meetings) so a slow or
/// broken gws CLI can never stall the sessions listing. Reads the scope's
/// capability auth root — the gws profiles the skills/bots are already
/// authenticated into.
pub async fn upcoming_meetings_handler(
    resources: web::Data<Arc<AgentResources>>,
    req: HttpRequest,
    query: web::Query<UpcomingMeetingsQuery>,
) -> impl Responder {
    let query = query.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), query.workspace) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let auth_root = resources
        .artifact_workspace
        .capability_auth_root(&principal, &workspace);
    // Build the scope's tool-bin paths so the gws calendar spawn gets the same
    // PATH augmentation as skill/preflight subprocesses. repo_root = process
    // CWD (the real repo root for every shipped launcher, same assumption
    // `gws_binary()` already makes for `skillshub/node_modules/.bin/gws`).
    let repo_root = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let scope_paths =
        CapabilityWorkspaceManager::new(resources.artifact_workspace.clone(), repo_root)
            .scope_paths(&principal, &workspace);
    HttpResponse::Ok().json(
        upcoming_meetings_cached(
            query.refresh.unwrap_or(false),
            &auth_root,
            Some(scope_paths),
        )
        .await,
    )
}

#[cfg(test)]
mod wire_parity {
    use super::*;
    use magician_media::media_rails::meeting::{MeetingStatus, PassiveStatus};

    fn keys(value: &serde_json::Value) -> Vec<&str> {
        let mut names = value
            .as_object()
            .expect("row object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        names.sort_unstable();
        names
    }

    /// Four consumer families read these rows unchanged — the web UI, iOS
    /// `MeetingsAPI.swift`, Android `magdroid/.../meetings`, and the compiled
    /// `meeting` tool. The meetings-surface increment is ADDITIVE: it adds a
    /// new consumer (the app binder) and a shared audit, and moves the calendar
    /// read behind one owner. None of that may drop or rename a wire field, so
    /// the two row shapes are pinned by key set.
    #[test]
    fn passive_and_attendee_rows_keep_their_exact_wire_keys() {
        let passive = passive_json(&PassiveStatusView {
            session_id: "listen-1".to_owned(),
            status: PassiveStatus::Listening,
            thread: "meeting-acme-2026-09-02".to_owned(),
            title: Some("Acme".to_owned()),
            url: Some("https://meet.example/abc".to_owned()),
            capture_mic: true,
            paused: false,
            latest_summary: None,
            started_seconds_ago: 42,
            ended_seconds_ago: None,
            scope: Some(("owner".to_owned(), "default".to_owned())),
        });
        assert_eq!(
            keys(&passive),
            vec![
                "latest_summary",
                "mic",
                "mode",
                "paused",
                "session_id",
                "status",
                "thread_id",
                "title",
                "url",
            ]
        );

        let attendee = attendee_json(
            "meet-1",
            &MeetingStatus::Joining,
            "https://meet.example/abc",
            &Some("meeting-acme-2026-09-02".to_owned()),
            &Some("Acme".to_owned()),
            false,
            &None,
        );
        assert_eq!(
            keys(&attendee),
            vec![
                "latest_summary",
                "mode",
                "paused",
                "session_id",
                "status",
                "thread_id",
                "title",
                "url",
            ]
        );
    }

    /// Liveness is what the TopBar recording dot and the Active grid key off.
    /// The app binder derives its own `live` flag from the same two rules, so
    /// pinning them here keeps the two surfaces from drifting apart.
    #[test]
    fn liveness_rules_stay_exactly_the_terminal_state_checks() {
        assert!(attendee_is_live(&MeetingStatus::Joining));
        assert!(!attendee_is_live(&MeetingStatus::Left));
        assert!(!attendee_is_live(&MeetingStatus::Failed));
        assert!(passive_is_live(&PassiveStatus::Listening));
        assert!(!passive_is_live(&PassiveStatus::Stopping));
        assert!(!passive_is_live(&PassiveStatus::Stopped));
        assert!(!passive_is_live(&PassiveStatus::Failed));
    }
}

//! Owner surfaces for preference alignment (P6, P7.2).
//!
//! ```text
//! GET /api/magician/v2/hitl/{correlation_id}/alignment?surface=web|ios|android
//!     -> the owner-private detail of one held act
//! GET /api/magician/v2/preference-alignment/policy
//! PUT /api/magician/v2/preference-alignment/policy
//!     -> the owner's intervention policy (expected-revision write)
//! GET /api/magician/v2/preference-alignment/status
//!     -> mode in force, the saved file's verdict, the judge route
//! GET  /api/magician/v2/preference-alignment/notices?surface=web|ios|android
//! POST /api/magician/v2/preference-alignment/notices/{id}/answer  {answer, token?}
//!     -> nudges that let an act through, and their remember offers
//! GET    /api/magician/v2/preference-alignment/intentions?surface=web|ios|android
//!     -> the owner's preferences exactly as checks see them
//! POST   /api/magician/v2/preference-alignment/intentions?surface=  {kind, label?, wording, conditions?}
//! PATCH  /api/magician/v2/preference-alignment/intentions/{id}?surface=  {action, expected_revision, wording?}
//! DELETE /api/magician/v2/preference-alignment/intentions/{id}?surface=&expected_revision=
//!     -> the owner's own edits (add, edit, confirm, retire, forget)
//! GET    /api/magician/v2/preference-alignment/history?surface=&offset=&limit=
//! GET    /api/magician/v2/preference-alignment/intentions/{id}/history?surface=
//!     -> the preference history, newest first
//! ```
//!
//! [`ApiVoiceReviewSink`] answers a hold from a live voice call
//! (`preference_alignment::voice`) through the respond route's own resume
//! and answer policy, under channel `voice`.
//!
//! # Private detail, and who gets it
//!
//! The shared `hitl.requested` for a hold carries only a generic prompt and
//! non-revealing choice ids (plan §10.1). The reason, the cited preferences
//! and the exact wording of a proposed lasting change are served here, to the
//! owner of the paused run, and only to a surface that declares itself an
//! owner-private app surface: `web` and `ios` in the v1 canary (P0 §8.6). Any
//! other or unknown surface gets the public projection, so a client that is
//! not private cannot obtain the readback token a lasting change needs.
//! Channel adapters, bots and voice never call this route.
//!
//! The read takes nothing: it is a snapshot of the live pause, not a claim on
//! its resume authority.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use magician::config::{
    dry_run_magician_config_from_path, magician_config_path, MagicianConfig,
    PreferenceAlignmentConfig, PreferenceAlignmentConfigError,
    PREFERENCE_ALIGNMENT_JUDGE_OPERATION,
};
use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::execution::agentic::{split_hitl_correlation_id, AlignmentAskSnapshot};
use magician::magician_v2::preference_alignment::actor::{with_actor, WriteActor};
use magician::magician_v2::preference_alignment::ask::{AlignmentAsk, GENERIC_ASK_TEXT};
use magician::magician_v2::preference_alignment::history::read_history;
use magician::magician_v2::preference_alignment::intention::IntentionKind;
use magician::magician_v2::preference_alignment::notices::{
    answer_notice, list_notices, Notice, NoticeAnswer, NoticeOutcome, NoticeState,
};
use magician::magician_v2::preference_alignment::owner_edit::{
    apply_owner_edit, EditOutcome, OwnerEdit,
};
use magician::magician_v2::preference_alignment::policy::{
    read_policy, write_policy, OwnerPolicy, PolicyChange, PolicyError, PolicyWrite,
};
use magician::magician_v2::preference_alignment::service::current_config;
use magician::magician_v2::preference_alignment::view::build_view;

use crate::web_api::{AgenticResumeRequest, AgenticResumeValue, MagicianV2Api};

/// The owner's own app surfaces, which render the owner-private detail. Android
/// joined web and iOS on 2026-10-11 (owner decision); lock-screen and push text
/// stay generic on every surface.
pub const PRIVATE_ALIGNMENT_SURFACES: &[&str] = &["web", "ios", "android"];

/// Surfaces a policy change may name as where it came from.
const POLICY_SURFACES: &[&str] = &["web", "ios", "android", "desktop"];

fn scope_or_response(req: &HttpRequest) -> Result<(String, String), HttpResponse> {
    resolve_required_scope(req.headers(), None).map_err(|_| {
        HttpResponse::BadRequest().json(json!({
            "error": "missing_scope",
            "message": "A bearer with embedded principal/workspace scope is required."
        }))
    })
}

#[derive(Debug, Deserialize)]
pub struct AlignmentDetailQuery {
    #[serde(default)]
    pub surface: Option<String>,
}

/// What one surface may see of a held act. Pure, so the audience rule is
/// testable without a pause store.
pub fn alignment_detail_body(
    snapshot: &AlignmentAskSnapshot,
    principal: &str,
    workspace: &str,
    requested_ask: Option<&str>,
    surface: Option<&str>,
) -> Result<Value, (u16, Value)> {
    // Another owner's pause, or none: the same answer, so existence leaks
    // nothing.
    let not_found = || {
        (
            404,
            json!({"error": "alignment_ask_not_found",
                   "message": "No held action with that id is waiting for you."}),
        )
    };
    if snapshot.principal.as_deref() != Some(principal)
        || snapshot.workspace.as_deref() != Some(workspace)
    {
        return Err(not_found());
    }
    if let Some(requested) = requested_ask {
        if snapshot.ask_id.as_deref() != Some(requested) {
            return Err((
                409,
                json!({"error": "superseded_ask",
                       "message": "That review was replaced; open the current one."}),
            ));
        }
    }
    let private = surface
        .map(str::trim)
        .is_some_and(|surface| PRIVATE_ALIGNMENT_SURFACES.contains(&surface));
    let alignment = if private {
        snapshot.ask.private_projection()
    } else {
        snapshot.ask.public_projection()
    };
    Ok(json!({
        "private": private,
        "prompt": GENERIC_ASK_TEXT,
        "execution_id": snapshot.execution_id,
        "retry_reason": snapshot.retry_reason,
        "alignment": alignment,
    }))
}

/// `GET /hitl/{correlation_id}/alignment`.
pub async fn alignment_detail_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<AlignmentDetailQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (key, ask) = split_hitl_correlation_id(path.as_str());
    let Some(snapshot) = api.full_pause_store().alignment_ask_snapshot(key) else {
        return HttpResponse::NotFound().json(json!({
            "error": "alignment_ask_not_found",
            "message": "No held action with that id is waiting for you."
        }));
    };
    match alignment_detail_body(
        &snapshot,
        &principal,
        &workspace,
        ask,
        declared_surface(&req, query.surface.as_deref()),
    ) {
        Ok(body) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(body),
        Err((status, body)) => HttpResponse::build(
            actix_web::http::StatusCode::from_u16(status)
                .unwrap_or(actix_web::http::StatusCode::BAD_REQUEST),
        )
        .json(body),
    }
}

pub use magician::magician_v2::preference_alignment::ask::CONSENT_SURFACES;

/// The surface a private read may treat as declared. A bot daemon's or a
/// plane terminal's bearer is never a private surface, whatever it claims.
fn declared_surface<'a>(req: &HttpRequest, claimed: Option<&'a str>) -> Option<&'a str> {
    match effective_answer_channel(req, "web").as_str() {
        "bot" | "plane" => None,
        _ => claimed,
    }
}

/// The channel an answer is attributed to. A bot daemon's or a plane
/// terminal's bearer answers as what it is, whatever `channel` it claims:
/// the bearer kind is authenticated, the claim is not.
pub fn effective_answer_channel(request: &HttpRequest, claimed: &str) -> String {
    use magician::magician_v2::auth::sessions::BearerKind;
    match magician::magician_v2::auth::middleware::authenticated(request).map(|auth| auth.bearer) {
        Some(BearerKind::Bot { .. }) => "bot".to_string(),
        Some(BearerKind::Grant { .. }) => "plane".to_string(),
        _ => claimed.trim().to_string(),
    }
}

fn is_voice_channel(channel: &str) -> bool {
    let channel = channel.trim().to_ascii_lowercase();
    channel == "voice" || channel.starts_with("voice_") || channel.starts_with("voice:")
}

/// Whether `value`, from `channel`, may answer this hold.
///
/// - A voice answer must name the exact ask (`storage_key~ask_id`): an
///   answer is bound to the ask that was read aloud, never inferred from
///   speech about something else, and never applied to a newer ask.
/// - Off the owner's app surfaces, a reply can cancel, retry, ask for
///   something else or say what to do — but not consent to the act
///   (`allow_once`, `proceed_without_check`, a bare approve) and not change
///   what Magician remembers.
pub fn alignment_answer_policy(
    ask: &AlignmentAsk,
    channel: &str,
    names_ask: bool,
    value: &AgenticResumeValue,
) -> Result<(), (u16, Value)> {
    if is_voice_channel(channel) && !names_ask {
        return Err((
            409,
            json!({
                "error": "alignment_ask_identity_required",
                "message": "A spoken answer must name the review it answers."
            }),
        ));
    }
    if CONSENT_SURFACES.contains(&channel.trim()) {
        return Ok(());
    }
    // Decided on what the reply resolves to, so a typed label or id ("allow
    // once") is refused like the tapped choice it names. The resume enforces
    // the same rule on the taken pause; this answers early with a reason.
    let value: magician::magician_v2::execution::agentic::UserInputValue = value.clone().into();
    if magician::magician_v2::execution::agentic::alignment_reply_needs_consent_surface(ask, &value)
    {
        return Err((
            403,
            json!({
                "error": "private_surface_required",
                "message": "Approve this in Magician on the web or your phone, where you can see why it was held."
            }),
        ));
    }
    Ok(())
}

/// Applies a spoken answer to a hold exactly as `POST /hitl/{id}/respond`
/// with `source: agentic` and `channel: voice` would: the same answer policy,
/// then the same admitted-then-detached resume. The call names the ask it
/// read out; an answer for an ask that is no longer the pause's live one is
/// refused as already resolved.
pub struct ApiVoiceReviewSink {
    api: std::sync::Arc<MagicianV2Api>,
}

impl ApiVoiceReviewSink {
    /// Install from the first API instance; later instances are equivalent.
    pub fn install(api: std::sync::Arc<MagicianV2Api>) {
        magician::magician_v2::preference_alignment::voice::install_voice_answer_sink(
            std::sync::Arc::new(Self { api }),
        );
    }
}

#[async_trait::async_trait]
impl magician::magician_v2::preference_alignment::voice::VoiceReviewAnswerSink
    for ApiVoiceReviewSink
{
    async fn answer(
        &self,
        principal: &str,
        workspace: &str,
        review: &magician::magician_v2::preference_alignment::voice::VoiceReview,
        choice_id: &str,
    ) -> magician::magician_v2::preference_alignment::voice::VoiceAnswerOutcome {
        use magician::magician_v2::preference_alignment::voice::{
            VoiceAnswerOutcome, VOICE_ANSWER_CHANNEL,
        };
        let (bare_key, named) = split_hitl_correlation_id(&review.correlation_id);
        if named != Some(review.ask_id.as_str()) {
            return VoiceAnswerOutcome::Refused("the answer does not name its ask".to_string());
        }
        let Some(hold) = self.api.full_pause_store().alignment_ask_snapshot(bare_key) else {
            return VoiceAnswerOutcome::AlreadyResolved;
        };
        // Answered or re-staged since it was read out: the choices the call
        // offered may no longer be the ones the ask takes.
        let stage = serde_json::to_value(hold.ask.stage)
            .ok()
            .and_then(|stage| stage.as_str().map(str::to_string))
            .unwrap_or_default();
        if hold.ask.ask_id != review.ask_id || (!review.stage.is_empty() && stage != review.stage) {
            return VoiceAnswerOutcome::AlreadyResolved;
        }
        if hold.principal.as_deref() != Some(principal)
            || hold.workspace.as_deref() != Some(workspace)
        {
            return VoiceAnswerOutcome::Refused("the hold belongs to another scope".to_string());
        }
        let value = AgenticResumeValue::Choice {
            selected_id: choice_id.to_string(),
            other_value: None,
        };
        if let Err((_, refusal)) =
            alignment_answer_policy(&hold.ask, VOICE_ANSWER_CHANNEL, true, &value)
        {
            return VoiceAnswerOutcome::Refused(
                refusal["error"].as_str().unwrap_or("refused").to_string(),
            );
        }
        let Some(execution_id) = hold
            .execution_id
            .clone()
            .or_else(|| bare_key.split(':').next().map(str::to_string))
            .filter(|id| !id.trim().is_empty())
        else {
            return VoiceAnswerOutcome::Refused("the hold names no run".to_string());
        };
        let request = AgenticResumeRequest {
            pause_state_id: Some(review.correlation_id.clone()),
            plan_id: None,
            step_id: None,
            input_type: "choice".to_string(),
            value,
            agent_id: None,
            goal_id: None,
            cycle_id: None,
            answer_channel: Some(VOICE_ANSWER_CHANNEL.to_string()),
        };
        let response = self
            .api
            .resume_agentic_execution_acknowledged_when_admitted(
                web::Path::from(execution_id),
                web::Json(request),
                Some((principal.to_string(), workspace.to_string())),
                "agentic".to_string(),
            )
            .await;
        let status = |response: &HttpResponse| response.status();
        match response {
            Ok(response) if status(&response).is_success() => {
                // A re-ask is a 200 too: the hold stays and nothing was done.
                use actix_web::body::MessageBody as _;
                let reasked = response
                    .into_body()
                    .try_into_bytes()
                    .ok()
                    .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                    .is_some_and(|body| body["status"] == json!("reask_required"));
                if reasked {
                    VoiceAnswerOutcome::Refused("the review asked again".to_string())
                } else {
                    VoiceAnswerOutcome::Accepted
                }
            },
            Ok(response) if matches!(status(&response).as_u16(), 404 | 409 | 410) => {
                VoiceAnswerOutcome::AlreadyResolved
            },
            Ok(response) => VoiceAnswerOutcome::Refused(format!(
                "the resume answered HTTP {}",
                status(&response)
            )),
            Err(error) => VoiceAnswerOutcome::Refused(format!("the resume failed: {error}")),
        }
    }
}

fn policy_body(policy: &OwnerPolicy) -> Value {
    json!({
        "status": "ok",
        "revision": policy.revision,
        "global": policy.global,
        "intentions": policy.intentions,
        "updated_at_ms": policy.updated_at_ms,
        "updated_via": policy.updated_via,
        "version": policy.effective().version,
    })
}

fn policy_error_response(error: PolicyError) -> HttpResponse {
    match error {
        PolicyError::Invalid(message) => {
            HttpResponse::BadRequest().json(json!({"error": "invalid_policy", "message": message}))
        },
        PolicyError::Corrupt(message) => HttpResponse::Conflict().json(json!({
            "error": "policy_unreadable",
            "message": format!(
                "Your saved policy cannot be read ({message}). Save a new one with \
                 replace_unreadable to clear it."
            ),
        })),
        PolicyError::Scope(message) => {
            HttpResponse::BadRequest().json(json!({"error": "missing_scope", "message": message}))
        },
        other => HttpResponse::InternalServerError()
            .json(json!({"error": "policy_store_failed", "message": other.to_string()})),
    }
}

/// `GET /preference-alignment/policy`.
pub async fn get_policy_handler(req: HttpRequest, api: web::Data<MagicianV2Api>) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match read_policy(api.memory_resolver(), &principal, &workspace).await {
        Ok(policy) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(policy_body(&policy)),
        // Unreadable is shown, not hidden: checks are unresolved until the
        // owner replaces it.
        Err(PolicyError::Corrupt(message)) => HttpResponse::Ok().json(json!({
            "status": "unreadable",
            "message": message,
        })),
        Err(error) => policy_error_response(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct PolicyPutBody {
    #[serde(flatten)]
    pub change: PolicyChange,
    #[serde(default)]
    pub replace_unreadable: bool,
    /// The surface the owner changed it from.
    #[serde(default)]
    pub surface: Option<String>,
}

/// `PUT /preference-alignment/policy`. Replaces the whole policy when it is
/// still at `expected_revision`; a stale write is `409 stale_policy` with
/// the current policy.
pub async fn put_policy_handler(
    req: HttpRequest,
    body: web::Json<PolicyPutBody>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    // The policy decides which acts hold for the owner: only the owner's own
    // app sets it, never a bot daemon's or a plane terminal's bearer.
    if !CONSENT_SURFACES.contains(&effective_answer_channel(&req, "web").as_str()) {
        return HttpResponse::Forbidden().json(json!({
            "error": "private_surface_required",
            "message": "Change your check policy in Magician on the web or your phone."
        }));
    }
    let PolicyPutBody {
        change,
        replace_unreadable,
        surface,
    } = body.into_inner();
    let via = surface
        .as_deref()
        .map(str::trim)
        .filter(|surface| POLICY_SURFACES.contains(surface));
    match write_policy(
        api.memory_resolver(),
        &principal,
        &workspace,
        change,
        replace_unreadable,
        via,
        chrono::Utc::now().timestamp_millis(),
    )
    .await
    {
        Ok(PolicyWrite::Applied(policy)) => HttpResponse::Ok().json(policy_body(&policy)),
        Ok(PolicyWrite::Stale(current)) => HttpResponse::Conflict().json(json!({
            "error": "stale_policy",
            "message": "Your policy changed on another device. Review it and save again.",
            "current": policy_body(&current),
        })),
        Err(error) => policy_error_response(error),
    }
}

fn mode_body(config: &PreferenceAlignmentConfig, principal: &str, workspace: &str) -> Value {
    json!({
        "mode": config.mode().as_str(),
        "configured_mode": config.mode,
        "mode_for_you": config.mode_for_scope(principal, workspace).as_str(),
        "canary_scopes": config.enforce_scopes.len(),
        "hold_deadline_secs": {
            "foreground": config.holds.foreground_deadline_secs,
            "background": config.holds.background_deadline_secs,
        },
        "proceed_without_check_allowed": config.recovery.allow_proceed_without_check,
    })
}

/// The settings status (P6.4). Pure over the configuration in force and the
/// saved file's dry-run result.
pub fn status_body(
    effective: &PreferenceAlignmentConfig,
    saved: &anyhow::Result<MagicianConfig>,
    principal: &str,
    workspace: &str,
) -> Value {
    let saved_body = match saved {
        Ok(config) => json!({
            "status": "accepted",
            "preference_alignment": mode_body(&config.preference_alignment, principal, workspace),
            "judge_route_bound": config
                .llm
                .router
                .as_ref()
                .is_some_and(|router| router
                    .operation_mapping
                    .contains_key(PREFERENCE_ALIGNMENT_JUDGE_OPERATION)),
        }),
        Err(error) => match error.downcast_ref::<PreferenceAlignmentConfigError>() {
            // The typed rejection: a code a client can key on and the exact
            // property to fix.
            Some(rejection) => json!({
                "status": "rejected",
                "error": {
                    "code": rejection.code,
                    "property": rejection.property,
                    "reason": rejection.reason,
                },
            }),
            None => json!({
                "status": "rejected",
                "error": {
                    "code": "config_invalid",
                    "property": Value::Null,
                    "reason": format!("{error:#}"),
                },
            }),
        },
    };
    let in_sync = saved
        .as_ref()
        .is_ok_and(|config| &config.preference_alignment == effective);
    json!({
        "effective": mode_body(effective, principal, workspace),
        "saved": saved_body,
        // A rejected save never reaches the running service: the last
        // accepted configuration stays in force until a valid one loads.
        "in_sync": in_sync,
    })
}

/// `GET /preference-alignment/status`.
pub async fn status_handler(req: HttpRequest) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let effective = current_config();
    let saved = web::block(|| dry_run_magician_config_from_path(&magician_config_path()))
        .await
        .unwrap_or_else(|error| Err(anyhow::anyhow!("config read failed: {error}")));
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(status_body(&effective, &saved, &principal, &workspace))
}

#[derive(Debug, Deserialize)]
pub struct NoticesQuery {
    #[serde(default)]
    pub surface: Option<String>,
}

fn notice_body(notice: &Notice) -> Value {
    json!({
        "id": notice.id,
        "created_at_ms": notice.created_at_ms,
        "act_summary": notice.act_summary,
        "explanation": notice.explanation,
        "intentions": notice.intentions.iter().map(|intention| json!({
            "id": intention.id,
            "statement": intention.statement,
            "assertion_status": intention.assertion_status,
        })).collect::<Vec<_>>(),
        "remember": notice.remember.as_ref().map(|offer| json!({
            "statement": offer.intention.statement,
            "wording": offer.write.wording,
            "token": offer.write.token,
        })),
        "state": notice.state,
        "resolved_at_ms": notice.resolved_at_ms,
    })
}

/// The notices one surface may see. A non-private surface learns only how
/// many are open, never what they say.
pub fn notices_body(notices: &[Notice], surface: Option<&str>) -> Value {
    let private = surface
        .map(str::trim)
        .is_some_and(|surface| PRIVATE_ALIGNMENT_SURFACES.contains(&surface));
    let open = notices
        .iter()
        .filter(|notice| notice.state == NoticeState::Open)
        .count();
    if private {
        json!({
            "private": true,
            "open": open,
            "notices": notices.iter().map(notice_body).collect::<Vec<_>>(),
        })
    } else {
        json!({"private": false, "open": open, "notices": []})
    }
}

/// `GET /preference-alignment/notices?surface=web|ios|android`.
pub async fn list_notices_handler(
    req: HttpRequest,
    query: web::Query<NoticesQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match list_notices(api.memory_resolver(), &principal, &workspace).await {
        Ok(notices) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(notices_body(
                &notices,
                declared_surface(&req, query.surface.as_deref()),
            )),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "notices_unavailable", "message": error.to_string()})),
    }
}

#[derive(Debug, Deserialize)]
pub struct NoticeAnswerBody {
    /// `remember` or `dismiss`.
    pub answer: String,
    /// The notice's remember token, shown only on a private surface.
    #[serde(default)]
    pub token: Option<String>,
}

/// `POST /preference-alignment/notices/{id}/answer`.
pub async fn answer_notice_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<NoticeAnswerBody>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let answer = match (body.answer.as_str(), body.token.as_deref()) {
        ("dismiss", _) => NoticeAnswer::Dismiss,
        ("remember", Some(token)) => NoticeAnswer::Remember { token },
        ("remember", None) => {
            return HttpResponse::BadRequest().json(json!({
                "error": "token_required",
                "message": "Confirm the exact wording from a private surface."
            }))
        },
        _ => {
            return HttpResponse::BadRequest()
                .json(json!({"error": "invalid_answer", "message": "Answer remember or dismiss."}))
        },
    };
    match answer_notice(
        api.memory_resolver(),
        &principal,
        &workspace,
        path.as_str(),
        answer,
        chrono::Utc::now(),
    )
    .await
    {
        Ok(NoticeOutcome::Resolved(notice)) => HttpResponse::Ok().json(notice_body(&notice)),
        Ok(NoticeOutcome::AlreadyResolved(notice)) => HttpResponse::Conflict().json(json!({
            "error": "already_resolved",
            "notice": notice_body(&notice),
        })),
        Ok(NoticeOutcome::NotFound) => HttpResponse::NotFound()
            .json(json!({"error": "notice_not_found", "message": "No such note."})),
        Ok(NoticeOutcome::WrongToken) => HttpResponse::Conflict().json(json!({
            "error": "wording_not_confirmed",
            "message": "Confirm the exact wording shown on a private surface."
        })),
        Ok(NoticeOutcome::NothingToRemember) => HttpResponse::BadRequest().json(json!({
            "error": "nothing_to_remember",
            "message": "This note offers nothing to remember."
        })),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "notice_answer_failed", "message": error.to_string()})),
    }
}

// ── The owner's preferences: view, edits, history ──────────────────────────

/// The private surface a preferences request comes from, or `403`. Only the
/// owner's web and iOS apps see or change preferences; a bot daemon's or a
/// plane terminal's bearer never does, whatever surface it claims.
fn private_surface<'a>(
    req: &HttpRequest,
    claimed: Option<&'a str>,
) -> Result<&'a str, HttpResponse> {
    match declared_surface(req, claimed).map(str::trim) {
        Some(surface) if PRIVATE_ALIGNMENT_SURFACES.contains(&surface) => Ok(surface),
        _ => Err(HttpResponse::Forbidden().json(json!({
            "error": "private_surface_required",
            "message": "See and change your preferences in Magician on the web or your phone."
        }))),
    }
}

#[derive(Debug, Deserialize)]
pub struct SurfaceQuery {
    #[serde(default)]
    pub surface: Option<String>,
}

/// `GET /preference-alignment/intentions?surface=web|ios|android`.
pub async fn list_intentions_handler(
    req: HttpRequest,
    query: web::Query<SurfaceQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = private_surface(&req, query.surface.as_deref()) {
        return response;
    }
    match build_view(api.memory_resolver(), &principal, &workspace).await {
        Ok(view) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(view),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "preferences_unavailable", "message": error.to_string()})),
    }
}

/// The response for an applied, stale, missing or invalid owner edit.
pub fn edit_response(outcome: &EditOutcome, created: bool) -> HttpResponse {
    match outcome {
        EditOutcome::Applied { record_id } => {
            let body = json!({"record_id": record_id});
            if created {
                HttpResponse::Created().json(body)
            } else {
                HttpResponse::Ok().json(body)
            }
        },
        EditOutcome::Stale { current_revision } => HttpResponse::Conflict().json(json!({
            "error": "stale_preference",
            "message": "This preference changed since you opened it. Review it and try again.",
            "current_revision": current_revision,
        })),
        EditOutcome::NotFound => HttpResponse::NotFound().json(json!({
            "error": "preference_not_found",
            "message": "No editable preference with that id. Preferences from your taste profile or an app are changed where they live."
        })),
        EditOutcome::Invalid { reason } => {
            HttpResponse::BadRequest().json(json!({"error": "invalid_edit", "message": reason}))
        },
    }
}

async fn apply_edit(
    api: &MagicianV2Api,
    principal: &str,
    workspace: &str,
    surface: &str,
    edit: OwnerEdit,
) -> HttpResponse {
    let memory = match api
        .memory_resolver()
        .resolve_for_scope(principal, workspace)
    {
        Ok(memory) => memory,
        Err(error) => {
            return HttpResponse::InternalServerError()
                .json(json!({"error": "preferences_unavailable", "message": error.to_string()}))
        },
    };
    let request_id = uuid::Uuid::new_v4().to_string();
    let mut outcome = EditOutcome::NotFound;
    let result = with_actor(
        WriteActor::surface(surface).with_cause(request_id.clone()),
        memory.update_user_knowledge_as("owner_surface", |knowledge| {
            outcome = apply_owner_edit(knowledge, &edit, surface, &request_id, chrono::Utc::now());
            Ok(matches!(outcome, EditOutcome::Applied { .. }))
        }),
    )
    .await;
    match result {
        Ok(()) => edit_response(&outcome, matches!(edit, OwnerEdit::Add { .. })),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "preference_write_failed", "message": error.to_string()})),
    }
}

#[derive(Debug, Deserialize)]
pub struct AddPreferenceBody {
    pub kind: IntentionKind,
    #[serde(default)]
    pub label: Option<String>,
    pub wording: String,
    #[serde(default)]
    pub conditions: Option<String>,
}

/// `POST /preference-alignment/intentions?surface=web|ios|android`.
pub async fn add_intention_handler(
    req: HttpRequest,
    query: web::Query<SurfaceQuery>,
    body: web::Json<AddPreferenceBody>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let surface = match private_surface(&req, query.surface.as_deref()) {
        Ok(surface) => surface,
        Err(response) => return response,
    };
    let AddPreferenceBody {
        kind,
        label,
        wording,
        conditions,
    } = body.into_inner();
    apply_edit(
        &api,
        &principal,
        &workspace,
        surface,
        OwnerEdit::Add {
            kind,
            label,
            wording,
            conditions,
        },
    )
    .await
}

#[derive(Debug, Deserialize)]
pub struct PatchPreferenceBody {
    /// `edit`, `confirm` or `retire`.
    pub action: String,
    pub expected_revision: String,
    #[serde(default)]
    pub wording: Option<String>,
}

/// The edit a PATCH body names, or why it names none.
pub fn patch_edit(id: String, body: PatchPreferenceBody) -> Result<OwnerEdit, String> {
    let PatchPreferenceBody {
        action,
        expected_revision,
        wording,
    } = body;
    match action.as_str() {
        "edit" => Ok(OwnerEdit::Edit {
            id,
            expected_revision,
            wording: wording.ok_or("an edit needs wording")?,
        }),
        "confirm" => Ok(OwnerEdit::Confirm {
            id,
            expected_revision,
        }),
        "retire" => Ok(OwnerEdit::Retire {
            id,
            expected_revision,
        }),
        other => Err(format!(
            "unknown action `{other}`: use edit, confirm or retire"
        )),
    }
}

/// `PATCH /preference-alignment/intentions/{id}?surface=web|ios|android`.
pub async fn patch_intention_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<SurfaceQuery>,
    body: web::Json<PatchPreferenceBody>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let surface = match private_surface(&req, query.surface.as_deref()) {
        Ok(surface) => surface,
        Err(response) => return response,
    };
    match patch_edit(path.into_inner(), body.into_inner()) {
        Ok(edit) => apply_edit(&api, &principal, &workspace, surface, edit).await,
        Err(reason) => {
            HttpResponse::BadRequest().json(json!({"error": "invalid_edit", "message": reason}))
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct ForgetQuery {
    #[serde(default)]
    pub surface: Option<String>,
    pub expected_revision: String,
}

/// `DELETE /preference-alignment/intentions/{id}?surface=&expected_revision=`:
/// forget. The preference history erases its wording too.
pub async fn forget_intention_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<ForgetQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(&req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let surface = match private_surface(&req, query.surface.as_deref()) {
        Ok(surface) => surface,
        Err(response) => return response,
    };
    let edit = OwnerEdit::Forget {
        id: path.into_inner(),
        expected_revision: query.expected_revision.clone(),
    };
    apply_edit(&api, &principal, &workspace, surface, edit).await
}

#[derive(Debug, Deserialize)]
pub struct HistoryQuery {
    #[serde(default)]
    pub surface: Option<String>,
    #[serde(default)]
    pub offset: Option<usize>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// Most history entries one page returns.
const MAX_HISTORY_PAGE: usize = 500;

async fn history_response(
    req: &HttpRequest,
    query: &HistoryQuery,
    api: &MagicianV2Api,
    record: Option<&str>,
) -> HttpResponse {
    let (principal, workspace) = match scope_or_response(req) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if let Err(response) = private_surface(req, query.surface.as_deref()) {
        return response;
    }
    let memory = match api
        .memory_resolver()
        .resolve_for_scope(&principal, &workspace)
    {
        Ok(memory) => memory,
        Err(error) => {
            return HttpResponse::InternalServerError()
                .json(json!({"error": "history_unavailable", "message": error.to_string()}))
        },
    };
    let limit = query.limit.unwrap_or(50).clamp(1, MAX_HISTORY_PAGE);
    match read_history(memory.storage(), record, query.offset.unwrap_or(0), limit).await {
        Ok(entries) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(json!({"entries": entries})),
        Err(error) => HttpResponse::InternalServerError()
            .json(json!({"error": "history_unavailable", "message": error.to_string()})),
    }
}

/// `GET /preference-alignment/history?surface=&offset=&limit=`.
pub async fn history_handler(
    req: HttpRequest,
    query: web::Query<HistoryQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    history_response(&req, &query, &api, None).await
}

/// `GET /preference-alignment/intentions/{id}/history?surface=`.
pub async fn intention_history_handler(
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<HistoryQuery>,
    api: web::Data<MagicianV2Api>,
) -> HttpResponse {
    history_response(&req, &query, &api, Some(path.as_str())).await
}

pub fn configure_preference_alignment_routes(cfg: &mut web::ServiceConfig) {
    cfg.route(
        "/hitl/{correlation_id}/alignment",
        web::get().to(alignment_detail_handler),
    )
    .route(
        "/preference-alignment/policy",
        web::get().to(get_policy_handler),
    )
    .route(
        "/preference-alignment/policy",
        web::put().to(put_policy_handler),
    )
    .route(
        "/preference-alignment/status",
        web::get().to(status_handler),
    )
    .route(
        "/preference-alignment/notices",
        web::get().to(list_notices_handler),
    )
    .route(
        "/preference-alignment/notices/{id}/answer",
        web::post().to(answer_notice_handler),
    )
    .route(
        "/preference-alignment/intentions",
        web::get().to(list_intentions_handler),
    )
    .route(
        "/preference-alignment/intentions",
        web::post().to(add_intention_handler),
    )
    .route(
        "/preference-alignment/intentions/{id}",
        web::patch().to(patch_intention_handler),
    )
    .route(
        "/preference-alignment/intentions/{id}",
        web::delete().to(forget_intention_handler),
    )
    .route(
        "/preference-alignment/intentions/{id}/history",
        web::get().to(intention_history_handler),
    )
    .route(
        "/preference-alignment/history",
        web::get().to(history_handler),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::preference_alignment::ask::{AlignmentAsk, AskDetail};

    fn snapshot() -> AlignmentAskSnapshot {
        let mut ask = AlignmentAsk::for_hold(
            "ask-1".into(),
            "alignment_binding.v1:x".into(),
            Some("exec-1".into()),
            vec!["pref-1".into()],
            &[],
            1_000,
            60,
            false,
            Some(AskDetail {
                explanation: "You asked to keep dinners vegetarian.".into(),
                act_summary: "order".into(),
                intentions: vec![],
                remember: None,
            }),
        );
        ask.ask_id = "ask-1".into();
        AlignmentAskSnapshot {
            ask,
            ask_id: Some("ask-1".into()),
            principal: Some("owner".into()),
            workspace: Some("ws".into()),
            execution_id: Some("exec-1".into()),
            retry_reason: None,
        }
    }

    #[test]
    fn preferences_need_the_owners_own_app() {
        let req = actix_web::test::TestRequest::get().to_http_request();
        assert_eq!(private_surface(&req, Some("web")).ok(), Some("web"));
        assert_eq!(private_surface(&req, Some("ios")).ok(), Some("ios"));
        assert_eq!(private_surface(&req, Some("android")).ok(), Some("android"));
        for claimed in [Some("telegram"), Some("whatsapp"), Some(""), None] {
            let refused = private_surface(&req, claimed).unwrap_err();
            assert_eq!(refused.status(), actix_web::http::StatusCode::FORBIDDEN);
        }
    }

    #[test]
    fn owner_edits_map_to_their_status_codes() {
        use actix_web::http::StatusCode;
        let applied = EditOutcome::Applied {
            record_id: "r".into(),
        };
        assert_eq!(edit_response(&applied, true).status(), StatusCode::CREATED);
        assert_eq!(edit_response(&applied, false).status(), StatusCode::OK);
        assert_eq!(
            edit_response(
                &EditOutcome::Stale {
                    current_revision: "x".into()
                },
                false
            )
            .status(),
            StatusCode::CONFLICT
        );
        assert_eq!(
            edit_response(&EditOutcome::NotFound, false).status(),
            StatusCode::NOT_FOUND
        );
        let body = |action: &str, wording: Option<&str>| PatchPreferenceBody {
            action: action.into(),
            expected_revision: "r1".into(),
            wording: wording.map(str::to_string),
        };
        assert!(matches!(
            patch_edit("a".into(), body("edit", Some("x"))),
            Ok(OwnerEdit::Edit { .. })
        ));
        assert!(patch_edit("a".into(), body("edit", None)).is_err());
        assert!(matches!(
            patch_edit("a".into(), body("retire", None)),
            Ok(OwnerEdit::Retire { .. })
        ));
        assert!(patch_edit("a".into(), body("forget", None)).is_err());
    }

    #[test]
    fn only_a_private_surface_of_the_owner_gets_the_reason() {
        let snapshot = snapshot();
        let web =
            alignment_detail_body(&snapshot, "owner", "ws", Some("ask-1"), Some("web")).unwrap();
        assert_eq!(web["private"], json!(true));
        assert_eq!(
            web["alignment"]["explanation"],
            json!("You asked to keep dinners vegetarian.")
        );
        let android =
            alignment_detail_body(&snapshot, "owner", "ws", None, Some("android")).unwrap();
        assert_eq!(android["private"], json!(true));
        for surface in [Some("telegram"), Some("whatsapp"), Some(""), None] {
            let body = alignment_detail_body(&snapshot, "owner", "ws", None, surface).unwrap();
            assert_eq!(body["private"], json!(false), "{surface:?}");
            assert!(!body.to_string().contains("vegetarian"), "{surface:?}");
        }
        // Another owner cannot tell the pause exists.
        assert_eq!(
            alignment_detail_body(&snapshot, "other", "ws", None, Some("web"))
                .unwrap_err()
                .0,
            404
        );
        assert_eq!(
            alignment_detail_body(&snapshot, "owner", "ws", Some("ask-0"), Some("web"))
                .unwrap_err()
                .0,
            409
        );
    }

    #[test]
    fn voice_answers_name_their_ask_and_shared_surfaces_never_consent() {
        let ask = snapshot().ask;
        let approve = AgenticResumeValue::Confirmation { confirmed: true };
        let cancel = AgenticResumeValue::Choice {
            selected_id: "cancel".into(),
            other_value: None,
        };
        let allow = AgenticResumeValue::Choice {
            selected_id: "allow_once".into(),
            other_value: None,
        };
        let words = AgenticResumeValue::Text {
            value: "order the paneer one instead".into(),
        };
        // The owner's apps answer anything the ask offers.
        for channel in CONSENT_SURFACES {
            assert!(alignment_answer_policy(&ask, channel, false, &approve).is_ok());
        }
        // Voice must name the ask it answers.
        assert_eq!(
            alignment_answer_policy(&ask, "voice", false, &cancel)
                .unwrap_err()
                .0,
            409
        );
        assert!(alignment_answer_policy(&ask, "voice", true, &cancel).is_ok());
        assert!(alignment_answer_policy(&ask, "voice", true, &words).is_ok());
        // A typed label or id is judged as the choice it names.
        for typed in ["Allow this once", "allow once", "allow_once"] {
            let text = AgenticResumeValue::Text {
                value: typed.into(),
            };
            assert_eq!(
                alignment_answer_policy(&ask, "whatsapp", true, &text)
                    .unwrap_err()
                    .0,
                403,
                "{typed}"
            );
        }
        let reply_choice = AgenticResumeValue::Choice {
            selected_id: "respond".into(),
            other_value: Some("Allow this once".into()),
        };
        assert_eq!(
            alignment_answer_policy(&ask, "voice", true, &reply_choice)
                .unwrap_err()
                .0,
            403
        );
        // Neither voice nor a bot channel can consent.
        for channel in ["voice", "whatsapp", "telegram", "unknown"] {
            assert_eq!(
                alignment_answer_policy(&ask, channel, true, &approve)
                    .unwrap_err()
                    .0,
                403,
                "{channel}"
            );
            assert_eq!(
                alignment_answer_policy(&ask, channel, true, &allow)
                    .unwrap_err()
                    .0,
                403,
                "{channel}"
            );
        }
        assert!(alignment_answer_policy(
            &ask,
            "whatsapp",
            false,
            &AgenticResumeValue::Confirmation { confirmed: false }
        )
        .is_ok());
    }

    #[test]
    fn a_shared_surface_learns_only_how_many_notes_are_open() {
        let notice = Notice::for_nudge(
            None,
            "b",
            "order".into(),
            "You usually order vegetarian.".into(),
            vec![],
            None,
            1,
        );
        let shared = notices_body(std::slice::from_ref(&notice), Some("telegram"));
        assert_eq!(shared["open"], json!(1));
        assert!(!shared.to_string().contains("vegetarian"));
        let private = notices_body(std::slice::from_ref(&notice), Some("ios"));
        assert_eq!(
            private["notices"][0]["explanation"],
            json!("You usually order vegetarian.")
        );
    }

    #[test]
    fn a_rejected_save_reports_its_code_and_property_and_the_mode_in_force() {
        let effective = PreferenceAlignmentConfig {
            mode: "shadow".into(),
            ..PreferenceAlignmentConfig::default()
        };
        let saved: anyhow::Result<MagicianConfig> =
            Err(anyhow::Error::new(PreferenceAlignmentConfigError {
                code: "alignment_enforce_route_required",
                property: "llm.router.operation_mapping.action_alignment_judge".into(),
                reason: "is required".into(),
            }));
        let body = status_body(&effective, &saved, "owner", "ws");
        assert_eq!(body["effective"]["mode"], json!("shadow"));
        assert_eq!(body["saved"]["status"], json!("rejected"));
        assert_eq!(
            body["saved"]["error"]["code"],
            json!("alignment_enforce_route_required")
        );
        assert_eq!(
            body["saved"]["error"]["property"],
            json!("llm.router.operation_mapping.action_alignment_judge")
        );
        assert_eq!(body["in_sync"], json!(false));
    }
}

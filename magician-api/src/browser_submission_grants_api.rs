//! Standing "Approve Always" grants for browser form submissions.
//!
//! - `GET /api/magician/v2/browser/submission-grants` — the sites the owner
//!   has approved to submit forms without asking, newest first.
//! - `DELETE /api/magician/v2/browser/submission-grants[?origin=<origin>]` —
//!   revoke one site's grant, or every grant when `origin` is omitted; the
//!   next submission on a revoked site asks again.
//!
//! Both routes are session-bearer only (`plane_api::owner_identity`). The
//! guard that creates the cards is `outward_actions.browser_submission_guard`;
//! grants exist only for sites the owner approved on such a card.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::execution::primitive_dispatch::browser::submission_grants;

use crate::web_api::MagicianV2Api;

fn require_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
) -> Result<(), HttpResponse> {
    crate::plane_api::owner_identity(req, auth.map(|data| data.get_ref())).map(|_| ())
}

fn grants_file(api: &MagicianV2Api, req: &HttpRequest) -> Result<std::path::PathBuf, HttpResponse> {
    let scope = crate::scope::resolve_required_scope_ref(req.headers(), None)?;
    let Some(service) = api.orchestrator().artifact_v2_service() else {
        return Err(HttpResponse::ServiceUnavailable().json(json!({
            "error": "task_store_unavailable",
            "message": "The task store is not available.",
        })));
    };
    Ok(submission_grants::grants_path(
        service.workspace(),
        &scope.principal(),
        &scope.workspace(),
    ))
}

/// `GET /api/magician/v2/browser/submission-grants`
pub async fn submission_grants_get_handler(
    api: web::Data<MagicianV2Api>,
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    let path = match grants_file(&api, &req) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let mut grants = submission_grants::list(&path);
    grants.sort_by_key(|grant| std::cmp::Reverse(grant.granted_at_ms));
    HttpResponse::Ok().json(json!({ "grants": grants }))
}

#[derive(Debug, Deserialize)]
pub struct RevokeQuery {
    origin: Option<String>,
}

/// `DELETE /api/magician/v2/browser/submission-grants[?origin=…]`
pub async fn submission_grants_delete_handler(
    api: web::Data<MagicianV2Api>,
    req: HttpRequest,
    query: web::Query<RevokeQuery>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()) {
        return response;
    }
    let path = match grants_file(&api, &req) {
        Ok(path) => path,
        Err(response) => return response,
    };
    let origin = query
        .origin
        .as_deref()
        .map(str::trim)
        .filter(|origin| !origin.is_empty());
    // "https://X.com/path" and "https://x.com" name the same site.
    let normalised =
        origin.map(|raw| submission_grants::origin_of(raw).unwrap_or_else(|| raw.to_owned()));
    match submission_grants::revoke(&path, normalised.as_deref()) {
        Ok(removed) => HttpResponse::Ok().json(json!({ "revoked": removed })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": "revoke_failed",
            "message": error.to_string(),
        })),
    }
}

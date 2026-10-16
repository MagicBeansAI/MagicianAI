//! Track B `/storage/activation` evidence. Cutover is fail-closed until Gate 3.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;

use magician::magician_v2::storage_activation::ActivationOperation;
use magician::magician_v2::storage_activation::{
    evaluate_preconditions, ActivationContext, ActivationReport, CUTOVER_CONFIRM,
};

use crate::scope::resolve_required_scope;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageScopeQuery {
    #[serde(default)]
    workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CutoverRequest {
    confirmation: String,
}

pub fn configure(config: &mut web::ServiceConfig) {
    config
        .route("/storage/activation", web::get().to(activation_status))
        .route(
            "/storage/activation/cutover",
            web::post().to(activation_cutover),
        );
}

pub async fn activation_status(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match evaluate_preconditions(&ActivationContext::default()) {
        Ok(pre) => {
            let report = ActivationReport::from_preconditions(
                ActivationOperation::Status,
                &principal,
                &workspace,
                &pre,
                "local_embedded",
                "remote_durable",
            )
            .redact();
            HttpResponse::Ok().json(report)
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn activation_cutover(
    request: HttpRequest,
    query: web::Query<StorageScopeQuery>,
    body: web::Json<CutoverRequest>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(request.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    if body.confirmation != CUTOVER_CONFIRM {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "confirmation_mismatch",
            "expected": CUTOVER_CONFIRM
        }));
    }
    match evaluate_preconditions(&ActivationContext::default()) {
        Ok(pre) => {
            let mut report = ActivationReport::from_preconditions(
                ActivationOperation::Cutover,
                &principal,
                &workspace,
                &pre,
                "local_embedded",
                "remote_durable",
            )
            .redact();
            report.ok = false;
            HttpResponse::Conflict().json(report)
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

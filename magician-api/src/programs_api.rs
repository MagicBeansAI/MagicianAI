//! Program-document read API — the Fleet Civilization game's window onto the
//! scope's `programs/*.md` (guild "mission chips" and, later, the CEO
//! decomposition surfaces; see `docs/archive/plans/2026-07-10-ceo-decomposition-design.md`).
//!
//! - `GET /api/magician/v2/programs` — `{ programs: [{name, title}] }`
//! - `GET /api/magician/v2/programs/{name}` — full doc content + the managed
//!   `## Missions (CEO)` section when present + its `.history/` snapshot list
//!   (404 when absent, 400 on an invalid name — names are bare `*.md` file
//!   names, no traversal).
//! - `POST /api/magician/v2/programs/{name}/revert` — the owner's revert
//!   affordance (P3 of the CEO-decomposition design): restore the program from
//!   a history snapshot (`{snapshot?: string}`; default = newest). The current
//!   content is snapshotted first, so a revert is itself revertible.
//!
//! Scope-aware in the house style (`resolve_required_scope`); the workspace
//! comes from the shared `ArtifactV2Service` app-data.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::service::ArtifactV2Service;
use magician::magician_v2::harness::program_doc::{
    list_history_snapshots, list_program_docs, read_program_doc, ProgramDocEditor,
};

#[derive(Debug, Deserialize)]
pub struct ProgramsScopeQuery {
    pub workspace: Option<String>,
}

/// GET /api/magician/v2/programs
pub async fn list_programs_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    match list_program_docs(service.workspace(), &principal, &workspace).await {
        Ok(programs) => HttpResponse::Ok().json(json!({ "programs": programs })),
        Err(err) => HttpResponse::InternalServerError()
            .json(json!({ "error": format!("failed to list programs: {err}") })),
    }
}

/// GET /api/magician/v2/programs/{name}
pub async fn get_program_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    path: web::Path<String>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    match read_program_doc(service.workspace(), &principal, &workspace, &name).await {
        Ok(Some(doc)) => {
            let history =
                list_history_snapshots(service.workspace(), &principal, &workspace, &name)
                    .await
                    .unwrap_or_default();
            let mut body = serde_json::to_value(&doc).unwrap_or_else(|_| json!({}));
            if let Some(map) = body.as_object_mut() {
                map.insert(
                    "history".into(),
                    serde_json::to_value(history).unwrap_or_default(),
                );
            }
            HttpResponse::Ok().json(body)
        },
        Ok(None) => HttpResponse::NotFound().json(json!({ "error": "program not found" })),
        Err(err) => {
            HttpResponse::BadRequest().json(json!({ "error": format!("invalid program: {err}") }))
        },
    }
}

#[derive(Debug, Deserialize)]
pub struct RevertProgramBody {
    /// History snapshot to restore; omitted = the newest one.
    pub snapshot: Option<String>,
}

/// POST /api/magician/v2/programs/{name}/revert
pub async fn revert_program_handler(
    service: web::Data<Arc<ArtifactV2Service>>,
    path: web::Path<String>,
    req: HttpRequest,
    query: web::Query<ProgramsScopeQuery>,
    body: Option<web::Json<RevertProgramBody>>,
) -> HttpResponse {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    let snapshot = body.as_ref().and_then(|b| b.snapshot.clone());
    let editor = ProgramDocEditor::new(service.workspace().clone());
    match editor
        .revert_from_history(&principal, &workspace, &name, snapshot.as_deref())
        .await
    {
        Ok(reverted) => HttpResponse::Ok().json(json!({
            "reverted": true,
            "program": reverted.program,
            "restored_from": reverted.restored_from,
            "pre_revert_snapshot": reverted.pre_revert_snapshot,
        })),
        Err(err) => {
            HttpResponse::BadRequest().json(json!({ "error": format!("revert failed: {err}") }))
        },
    }
}

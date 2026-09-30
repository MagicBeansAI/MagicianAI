//! # Durable Artifact API
//!
//! REST endpoints for listing and reading scoped durable artifacts.

use actix_web::{web, HttpResponse, Responder};
use serde::{Deserialize, Serialize};

use magician::magician_v2::api_scope::ResolvedScope;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::artifacts::durable_store::{
    open_local_durable_artifacts, DurableArtifactStore,
};

#[derive(Clone)]
pub struct ArtifactApi {
    workspace_layout: ArtifactV2Workspace,
}

impl ArtifactApi {
    pub fn new(workspace_layout: ArtifactV2Workspace) -> Self {
        Self { workspace_layout }
    }
}

#[derive(Serialize)]
struct DurableArtifactListEntry {
    namespace: String,
    name: String,
    content_type: Option<String>,
    last_updated: Option<String>,
    last_updated_by: Option<String>,
}

#[derive(Serialize)]
struct DurableArtifactListResponse {
    artifacts: Vec<DurableArtifactListEntry>,
}

#[derive(Deserialize)]
pub struct ListDurableQuery {
    pub namespace: Option<String>,
    pub workspace: Option<String>,
}

fn resolve_scoped_store(
    api: &ArtifactApi,
    principal: &str,
    workspace: &str,
) -> Result<DurableArtifactStore, HttpResponse> {
    open_local_durable_artifacts(&api.workspace_layout, principal, workspace).map_err(|error| {
        HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to resolve scoped durable artifact store: {}", error)
        }))
    })
}

/// `GET /artifacts/durable` — list all durable artifacts.
pub async fn list_durable_artifacts(
    scope: ResolvedScope,
    api: web::Data<ArtifactApi>,
    query: web::Query<ListDurableQuery>,
) -> impl Responder {
    let query = query.into_inner();
    let store = match resolve_scoped_store(api.get_ref(), scope.principal(), scope.workspace()) {
        Ok(store) => store,
        Err(response) => return response,
    };

    match store.list(query.namespace.as_deref()) {
        Ok(entries) => {
            let artifacts: Vec<DurableArtifactListEntry> = entries
                .into_iter()
                .map(|e| DurableArtifactListEntry {
                    namespace: e.namespace,
                    name: e.name,
                    content_type: e
                        .frontmatter
                        .as_ref()
                        .and_then(|fm| fm.content_type.clone()),
                    last_updated: e
                        .frontmatter
                        .as_ref()
                        .map(|fm| fm.last_updated.to_rfc3339()),
                    last_updated_by: e.frontmatter.as_ref().map(|fm| fm.last_updated_by.clone()),
                })
                .collect();
            HttpResponse::Ok().json(DurableArtifactListResponse { artifacts })
        },
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to list artifacts: {}", e)
        })),
    }
}

/// `GET /artifacts/durable/{namespace}/{name:.*}` — read a durable artifact.
pub async fn read_durable_artifact(
    scope: ResolvedScope,
    api: web::Data<ArtifactApi>,
    path: web::Path<(String, String)>,
    _query: web::Query<ListDurableQuery>,
) -> impl Responder {
    let (namespace, name) = path.into_inner();

    // Path traversal guard
    if namespace.contains("..") || name.contains("..") {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "Invalid path"
        }));
    }

    let store = match resolve_scoped_store(api.get_ref(), scope.principal(), scope.workspace()) {
        Ok(store) => store,
        Err(response) => return response,
    };

    if !store.exists(&namespace, &name) {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("Artifact {}/{} not found", namespace, name)
        }));
    }

    match store.read(&namespace, &name).await {
        Ok((frontmatter, body)) => HttpResponse::Ok().json(serde_json::json!({
            "namespace": frontmatter.namespace,
            "name": frontmatter.name,
            "content_type": frontmatter.content_type,
            "last_updated": frontmatter.last_updated.to_rfc3339(),
            "last_updated_by": frontmatter.last_updated_by,
            "created_by": frontmatter.created_by,
            "content": body,
        })),
        Err(e) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("Failed to read artifact: {}", e)
        })),
    }
}

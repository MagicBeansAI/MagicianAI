use std::sync::Arc;

use actix_web::{web, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use magician::magician_v2::workspace_storage_settings::{
    WorkspaceStorageSettings, WorkspaceStorageSettingsStore,
};

#[derive(Clone)]
pub struct WorkspaceStorageApi {
    settings: Arc<WorkspaceStorageSettingsStore>,
}

impl WorkspaceStorageApi {
    pub fn new<P: AsRef<std::path::Path>>(bootstrap_root: P) -> Self {
        Self {
            settings: Arc::new(WorkspaceStorageSettingsStore::new(bootstrap_root)),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct PutWorkspaceStorageSettingsRequest {
    #[serde(flatten)]
    pub settings: WorkspaceStorageSettings,
}

pub async fn get_workspace_storage_settings_handler(
    api: web::Data<Arc<WorkspaceStorageApi>>,
) -> Result<HttpResponse> {
    match api.settings.load_envelope().await {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(workspace_storage_io_error_response(error)),
    }
}

pub async fn put_workspace_storage_settings_handler(
    api: web::Data<Arc<WorkspaceStorageApi>>,
    body: web::Json<PutWorkspaceStorageSettingsRequest>,
) -> Result<HttpResponse> {
    match api.settings.save(body.into_inner().settings).await {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(workspace_storage_io_error_response(error)),
    }
}

fn workspace_storage_io_error_response(error: std::io::Error) -> HttpResponse {
    let status = match error.kind() {
        std::io::ErrorKind::InvalidInput => actix_web::http::StatusCode::BAD_REQUEST,
        std::io::ErrorKind::PermissionDenied => actix_web::http::StatusCode::FORBIDDEN,
        _ => actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
    };
    HttpResponse::build(status).json(json!({
        "error": "workspace_storage_settings_error",
        "message": error.to_string(),
    }))
}

pub fn configure_workspace_storage_routes(cfg: &mut web::ServiceConfig) {
    cfg.service(
        web::scope("/workspace-storage")
            .route(
                "/settings",
                web::get().to(get_workspace_storage_settings_handler),
            )
            .route(
                "/settings",
                web::put().to(put_workspace_storage_settings_handler),
            ),
    );
}

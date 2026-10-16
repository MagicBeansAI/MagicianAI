use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::scope::resolve_required_scope;
use magician::magician_v2::process_storage;
use magician::magician_v2::realtime_events::RuntimeAgentEventType;
use magician::magician_v2::ui_preferences::{UiPreferences, UiPreferencesStore};
use magician_media::RealtimeSessionRegistry;

const UI_PREFERENCES_SYSTEM_AGENT: &str = "ui-preferences";

#[derive(Clone)]
pub struct UiPreferencesApi {
    registry: Arc<RealtimeSessionRegistry>,
    preferences: Arc<UiPreferencesStore>,
}

impl UiPreferencesApi {
    pub fn new(registry: Arc<RealtimeSessionRegistry>) -> Self {
        Self {
            registry,
            preferences: Arc::new(UiPreferencesStore::new(process_storage::runtime_root())),
        }
    }

    pub fn with_preferences(mut self, preferences: Arc<UiPreferencesStore>) -> Self {
        self.preferences = preferences;
        self
    }
}

#[derive(Debug, Deserialize)]
pub struct PutUiPreferencesRequest {
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,
    /// `ask` or `accept_in_scope`. Absent leaves the stored posture alone, so a
    /// theme write can never move a permission by omission.
    #[serde(default)]
    pub composer_permission_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UiPreferencesResponse {
    pub theme: String,
    pub composer_permission_mode: String,
    pub saved: bool,
}

impl UiPreferencesResponse {
    fn new(value: UiPreferences, saved: bool) -> Self {
        Self {
            theme: value.theme,
            composer_permission_mode: value.composer_permission_mode,
            saved,
        }
    }
}

pub async fn get_ui_preferences_handler(
    api: web::Data<Arc<UiPreferencesApi>>,
    req: HttpRequest,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let saved = match api.preferences.exists(&principal, &workspace).await {
        Ok(saved) => saved,
        Err(error) => return Ok(ui_preferences_error_response(error)),
    };
    match api.preferences.load(&principal, &workspace).await {
        Ok(preferences) => {
            Ok(HttpResponse::Ok().json(UiPreferencesResponse::new(preferences, saved)))
        },
        Err(error) => Ok(ui_preferences_error_response(error)),
    }
}

pub async fn put_ui_preferences_handler(
    api: web::Data<Arc<UiPreferencesApi>>,
    req: HttpRequest,
    body: web::Json<PutUiPreferencesRequest>,
) -> Result<HttpResponse> {
    let body = body.into_inner();
    let (principal, workspace) = match resolve_required_scope(req.headers(), body.workspace.clone())
    {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let mut preferences = match api.preferences.load(&principal, &workspace).await {
        Ok(preferences) => preferences,
        Err(error) => return Ok(ui_preferences_error_response(error)),
    };
    if let Some(theme) = body.theme {
        preferences.theme = theme;
    }
    if let Some(mode) = body.composer_permission_mode {
        preferences.composer_permission_mode = mode;
    }
    match api
        .preferences
        .save(&principal, &workspace, preferences)
        .await
    {
        Ok(preferences) => {
            api.registry.broadcaster().emit_named(
                RuntimeAgentEventType::UiPreferencesUpdated.as_str(),
                UI_PREFERENCES_SYSTEM_AGENT,
                Some(&principal),
                Some(&workspace),
                json!({
                    "principal": principal,
                    "workspace": workspace,
                    "preferences": preferences.clone(),
                }),
            );
            Ok(HttpResponse::Ok().json(UiPreferencesResponse::new(preferences, true)))
        },
        Err(error) => Ok(ui_preferences_error_response(error)),
    }
}

fn ui_preferences_error_response(error: std::io::Error) -> HttpResponse {
    if error.kind() == std::io::ErrorKind::InvalidInput {
        return HttpResponse::BadRequest().json(json!({
            "error": "invalid_ui_preferences_scope",
            "message": error.to_string(),
        }));
    }
    HttpResponse::InternalServerError().json(json!({
        "error": "ui_preferences_io_error",
        "message": error.to_string(),
    }))
}

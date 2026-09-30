//! Bot management API — runtime control plane for consumer-channel bot processes.
//!
//! Routes:
//! ```text
//! GET    /api/magician/v2/bots
//! POST   /api/magician/v2/bots/{name}/start
//! POST   /api/magician/v2/bots/{name}/stop
//! POST   /api/magician/v2/bots/{name}/restart
//! GET    /api/magician/v2/bots/{name}/logs
//! GET    /api/magician/v2/bots/{name}/qr
//! GET    /api/magician/v2/bots/auth
//! GET    /api/magician/v2/bots/auth/state
//! GET    /api/magician/v2/bots/{name}/auth
//! POST   /api/magician/v2/bots/{name}/auth/start
//! POST   /api/magician/v2/bots/{name}/auth/input
//! GET    /api/magician/v2/bots/{name}/env
//! PUT    /api/magician/v2/bots/{name}/env
//! GET    /api/magician/v2/bots/{name}/config
//! PUT    /api/magician/v2/bots/{name}/config
//! DELETE /api/magician/v2/bots/{name}/config
//! ```

use std::sync::Arc;

use actix_web::{http::header, web, HttpRequest, HttpResponse, Responder};
use qrcode::{render::svg, QrCode};
use serde::{Deserialize, Serialize};
use tokio::fs;

use magician::config::BotProcessConfig;

use crate::scope::resolve_required_scope;
use crate::secret_vault_api::SecretVaultApi;

const DEFAULT_LOG_LIMIT: usize = 100;

pub struct BotApi {
    pub runtime: Arc<ScopedBotRuntime>,
    /// Optional canonical HITL broker — when present, every
    /// `list_bots_auth` call records each snapshot through it so
    /// transitions in/out of needs-auth states fire canonical
    /// `HitlRequested` / `HitlResolved` events. `None` in tests +
    /// CLI subcommands that boot without a broadcaster.
    pub auth_hitl_broker: Option<Arc<magician_learning::bots::auth_hitl_broker::AuthHitlBroker>>,
}

impl BotApi {
    pub fn new(runtime: Arc<ScopedBotRuntime>) -> Self {
        Self {
            runtime,
            auth_hitl_broker: None,
        }
    }

    pub fn with_auth_hitl_broker(
        mut self,
        broker: Arc<magician_learning::bots::auth_hitl_broker::AuthHitlBroker>,
    ) -> Self {
        self.auth_hitl_broker = Some(broker);
        self
    }
}

#[derive(Debug, Serialize)]
pub struct ListBotsResponse {
    pub bots: Vec<BotStatusSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct BotLogsResponse {
    pub name: String,
    pub lines: Vec<BotLogLine>,
}

#[derive(Debug, Serialize)]
pub struct BotAuthResponse {
    pub name: String,
    pub auth: BotAuthSnapshot,
}

#[derive(Debug, Serialize)]
pub struct ListBotsAuthResponse {
    pub bots: Vec<BotAuthSnapshot>,
}

#[derive(Debug, Serialize)]
pub struct BotConfigResponse {
    pub name: String,
    pub config: BotProcessConfig,
}

#[derive(Debug, Serialize)]
pub struct DeletedBotConfigResponse {
    pub name: String,
}

#[derive(Debug, Serialize)]
pub struct BotAuthStateResponse {
    pub auth_state: BotAuthStateSnapshot,
}

#[derive(Debug, Deserialize)]
pub struct BotLogsQuery {
    #[serde(default = "default_log_limit")]
    pub limit: usize,
}

#[derive(Debug, Deserialize)]
pub struct BotScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BotAuthInputRequest {
    pub input: String,
}

#[derive(Debug, Deserialize)]
pub struct BotEnvUpdateRequest {
    pub updates: std::collections::HashMap<String, Option<String>>,
}

#[derive(Debug, Serialize)]
pub struct BotEnvStatus {
    pub name: String,
    pub keys: Vec<String>,
}

fn default_log_limit() -> usize {
    DEFAULT_LOG_LIMIT
}

pub async fn list_bots_handler(bot_api: web::Data<BotApi>, req: HttpRequest) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api.runtime.list(&principal, &workspace).await {
        Ok(bots) => HttpResponse::Ok().json(ListBotsResponse { bots }),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn start_bot_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api
        .runtime
        .start(&principal, &workspace, path.as_str())
        .await
    {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn stop_bot_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api
        .runtime
        .stop(&principal, &workspace, path.as_str())
        .await
    {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn restart_bot_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api
        .runtime
        .restart(&principal, &workspace, path.as_str())
        .await
    {
        Ok(snapshot) => HttpResponse::Ok().json(snapshot),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn get_bot_logs_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<BotLogsQuery>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    match bot_api
        .runtime
        .logs(&principal, &workspace, &name, query.limit)
        .await
    {
        Ok(lines) => HttpResponse::Ok().json(BotLogsResponse { name, lines }),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn get_bot_qr_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
    query: web::Query<BotScopeQuery>,
) -> impl Responder {
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return response,
        };
    let name = path.into_inner();
    let qr_path = match bot_api
        .runtime
        .qr_code_path(&principal, &workspace, &name)
        .await
    {
        Ok(Some(path)) => path,
        Ok(None) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": format!("bot `{name}` has no QR artifact configured")
            }))
        },
        Err(err) => return scoped_runtime_error_response(err),
    };

    match fs::read(&qr_path).await {
        Ok(bytes) if qr_path.extension().and_then(|value| value.to_str()) == Some("txt") => {
            let value = match std::str::from_utf8(&bytes) {
                Ok(value) if !value.trim().is_empty() && value.len() <= 8192 => value.trim(),
                _ => {
                    return HttpResponse::InternalServerError().json(serde_json::json!({
                        "error": format!("QR data for bot `{name}` is invalid")
                    }))
                },
            };
            match QrCode::new(value.as_bytes()) {
                Ok(code) => HttpResponse::Ok()
                    .insert_header((header::CONTENT_TYPE, "image/svg+xml"))
                    .insert_header((header::CACHE_CONTROL, "no-store"))
                    .body(
                        code.render::<svg::Color>()
                            .min_dimensions(360, 360)
                            .dark_color(svg::Color("#111111"))
                            .light_color(svg::Color("#ffffff"))
                            .build(),
                    ),
                Err(_) => HttpResponse::InternalServerError().json(serde_json::json!({
                    "error": format!("QR data for bot `{name}` could not be rendered")
                })),
            }
        },
        Ok(bytes) => HttpResponse::Ok()
            .insert_header((header::CONTENT_TYPE, "image/png"))
            .insert_header((header::CACHE_CONTROL, "no-store"))
            .body(bytes),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            HttpResponse::NotFound().json(serde_json::json!({
                "error": format!("QR image for bot `{name}` is not available yet")
            }))
        },
        Err(err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to read QR image for bot `{name}`"),
            "details": err.to_string(),
        })),
    }
}

pub async fn get_bot_auth_state_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api.runtime.auth_state(&principal, &workspace).await {
        Ok(auth_state) => HttpResponse::Ok().json(BotAuthStateResponse { auth_state }),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn get_bot_auth_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    match bot_api
        .runtime
        .auth_info(&principal, &workspace, &name)
        .await
    {
        Ok(auth) => HttpResponse::Ok().json(BotAuthResponse { name, auth }),
        Err(err) => scoped_runtime_error_response(err),
    }
}

/// Bulk auth snapshot — returns one `BotAuthSnapshot` per bot in the
/// scope so the attention bar can surface needs-auth bots. Polling
/// consumers filter by `status == NeedsAuth`.
///
/// Side effect: every snapshot is recorded through
/// [`AuthHitlBroker::record_snapshot`] when a broker is installed.
/// Status transitions into / out of NeedsAuth / AccountMismatch emit
/// canonical `HitlRequested` / `HitlResolved` events so bot auth flows
/// surface in the AttentionBar's Requests bucket (same pipeline as
/// every other HITL source) instead of the frontend-synthesized
/// escalation row that pre-consolidation code shipped.
pub async fn list_bots_auth_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api.runtime.list_auth(&principal, &workspace).await {
        Ok(bots) => {
            if let Some(broker) = bot_api.auth_hitl_broker.as_ref() {
                for snapshot in &bots {
                    broker.record_snapshot(&principal, &workspace, snapshot);
                }
            }
            HttpResponse::Ok().json(ListBotsAuthResponse { bots })
        },
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn start_bot_auth_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match bot_api
        .runtime
        .start_auth(&principal, &workspace, path.as_str())
        .await
    {
        Ok(response) => HttpResponse::Accepted().json(response),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn submit_bot_auth_input_handler(
    bot_api: web::Data<BotApi>,
    vault: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BotAuthInputRequest>,
) -> impl Responder {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    match bot_api
        .runtime
        .submit_auth_input(&principal, &workspace, &name, &body.input)
        .await
    {
        Ok(()) => HttpResponse::Accepted().json(serde_json::json!({
            "name": name,
            "accepted": true,
        })),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn get_bot_env_handler(
    bot_api: web::Data<BotApi>,
    vault: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    let (manager, _) = match bot_api.runtime.resolve_scope(&principal, &workspace).await {
        Ok(parts) => parts,
        Err(error) => return scoped_runtime_error_response(error),
    };
    let Some(path) = (match manager.env_file_path(&name).await {
        Ok(path) => path,
        Err(error) => return bot_error_response(error),
    }) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("bot `{name}` has no configured env file")
        }));
    };
    let mut keys: Vec<String> =
        magician::magician_v2::runtime_settings::read_env_file_values(&path)
            .into_keys()
            .collect();
    keys.sort();
    HttpResponse::Ok().json(BotEnvStatus { name, keys })
}

pub async fn put_bot_env_handler(
    bot_api: web::Data<BotApi>,
    vault: web::Data<SecretVaultApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BotEnvUpdateRequest>,
) -> impl Responder {
    if let Err(response) = vault.require_setup_token(&req) {
        return response;
    }
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    let (manager, _) = match bot_api.runtime.resolve_scope(&principal, &workspace).await {
        Ok(parts) => parts,
        Err(error) => return scoped_runtime_error_response(error),
    };
    let Some(path) = (match manager.env_file_path(&name).await {
        Ok(path) => path,
        Err(error) => return bot_error_response(error),
    }) else {
        return HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("bot `{name}` has no configured env file")
        }));
    };
    if body.updates.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "updates must contain at least one key"
        }));
    }
    let mut pairs: Vec<(&str, Option<String>)> = Vec::new();
    for (key, value) in &body.updates {
        let valid = !key.is_empty()
            && key
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_uppercase())
            && key
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            && key != "MAGICIAN_BEARER_TOKEN";
        if !valid {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("invalid bot env key {key:?}")
            }));
        }
        if value.as_ref().is_some_and(|value| value.trim().is_empty()) {
            return HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("{key}: empty values are not allowed; send null to delete")
            }));
        }
        pairs.push((key.as_str(), value.clone()));
    }
    if let Err(error) = magician::magician_v2::runtime_settings::update_env_file(&path, &pairs) {
        return HttpResponse::InternalServerError().json(serde_json::json!({
            "error": format!("failed to update bot env: {error}")
        }));
    }
    let was_running = manager
        .config_state(&name)
        .await
        .is_some_and(|state| state.desired_running);
    if was_running {
        if let Err(error) = manager.restart(&name).await {
            return bot_error_response(error);
        }
    }
    let mut keys: Vec<String> = body.updates.keys().cloned().collect();
    keys.sort();
    HttpResponse::Ok().json(serde_json::json!({
        "name": name,
        "updated": keys,
    }))
}

pub async fn get_bot_config_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    match bot_api.runtime.config(&principal, &workspace, &name).await {
        Ok(config) => HttpResponse::Ok().json(BotConfigResponse { name, config }),
        Err(err) => scoped_runtime_error_response(err),
    }
}

pub async fn put_bot_config_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<BotProcessConfig>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    if let Err(message) = validate_bot_name(&name) {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": message }));
    }

    let config = match normalize_bot_process_config(body.into_inner()) {
        Ok(config) => config,
        Err(message) => {
            return HttpResponse::BadRequest().json(serde_json::json!({ "error": message }))
        },
    };
    let (manager, config_store) = match bot_api.runtime.resolve_scope(&principal, &workspace).await
    {
        Ok(parts) => parts,
        Err(err) => return scoped_runtime_error_response(err),
    };
    let previous = manager.config_state(&name).await;

    if let Err(err) = manager.upsert_config(name.clone(), config.clone()).await {
        return bot_error_response(err);
    }

    if let Err(err) = config_store.upsert_bot(&name, &config).await {
        return rollback_runtime_after_config_store_failure(&manager, &name, previous, err).await;
    }

    HttpResponse::Ok().json(BotConfigResponse { name, config })
}

pub async fn delete_bot_config_handler(
    bot_api: web::Data<BotApi>,
    req: HttpRequest,
    path: web::Path<String>,
) -> impl Responder {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let name = path.into_inner();
    if let Err(message) = validate_bot_name(&name) {
        return HttpResponse::BadRequest().json(serde_json::json!({ "error": message }));
    }

    let (manager, config_store) = match bot_api.runtime.resolve_scope(&principal, &workspace).await
    {
        Ok(parts) => parts,
        Err(err) => return scoped_runtime_error_response(err),
    };
    let removed_from_file = match config_store.delete_bot(&name).await {
        Ok(removed) => removed,
        Err(err) => return config_store_error_response(err),
    };

    match manager.delete_config(&name).await {
        Ok(()) => HttpResponse::Ok().json(DeletedBotConfigResponse { name }),
        Err(BotManagerError::UnknownBot(_)) if removed_from_file => {
            HttpResponse::Ok().json(DeletedBotConfigResponse { name })
        },
        Err(err) => bot_error_response(err),
    }
}

fn bot_error_response(err: BotManagerError) -> HttpResponse {
    match err {
        BotManagerError::UnknownBot(name) => HttpResponse::NotFound().json(serde_json::json!({
            "error": format!("bot `{name}` is not configured")
        })),
        BotManagerError::StartFailed { name, reason } => {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("failed to start bot `{name}`"),
                "details": reason,
            }))
        },
        BotManagerError::AuthUnsupported { name } => {
            HttpResponse::BadRequest().json(serde_json::json!({
                "error": format!("bot `{name}` does not support managed auth"),
            }))
        },
        BotManagerError::AuthFailed { name, reason } => {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": format!("managed auth failed for bot `{name}`"),
                "details": reason,
            }))
        },
    }
}

fn config_store_error_response(err: BotConfigStoreError) -> HttpResponse {
    HttpResponse::InternalServerError().json(serde_json::json!({
        "error": "failed to persist bot configuration",
        "details": err.to_string(),
    }))
}

async fn rollback_runtime_after_config_store_failure(
    manager: &Arc<BotManager>,
    name: &str,
    previous: Option<BotConfigStateSnapshot>,
    error: BotConfigStoreError,
) -> HttpResponse {
    let rollback_result = match previous {
        Some(previous) => manager
            .restore_config(name.to_string(), previous.config, previous.desired_running)
            .await
            .map(|_| ()),
        None => manager.delete_config(name).await,
    };

    match rollback_result {
        Ok(()) => config_store_error_response(error),
        Err(rollback_err) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": "failed to persist bot configuration and failed to restore the previous bot runtime",
            "details": error.to_string(),
            "rollback_details": rollback_err.to_string(),
        })),
    }
}

fn scoped_runtime_error_response(err: ScopedBotRuntimeError) -> HttpResponse {
    match err {
        ScopedBotRuntimeError::Bot(err) => bot_error_response(err),
        ScopedBotRuntimeError::ConfigStore(err) => config_store_error_response(err),
        ScopedBotRuntimeError::CapabilityWorkspace(err) => HttpResponse::InternalServerError()
            .json(serde_json::json!({
                "error": "failed to resolve scoped bot runtime",
                "details": err.to_string(),
            })),
        ScopedBotRuntimeError::ArtifactWorkspace(err) => {
            HttpResponse::InternalServerError().json(serde_json::json!({
                "error": "failed to enumerate scoped bot runtime",
                "details": err.to_string(),
            }))
        },
    }
}

fn validate_bot_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("bot name cannot be empty".to_string());
    }
    if name.trim() != name {
        return Err("bot name cannot have leading or trailing whitespace".to_string());
    }
    if name.contains('/') {
        return Err("bot name cannot contain `/`".to_string());
    }
    Ok(())
}

fn normalize_bot_process_config(config: BotProcessConfig) -> Result<BotProcessConfig, String> {
    let command = config.command.trim().to_string();
    if command.is_empty() {
        return Err("command is required".to_string());
    }
    if config.restart_max_backoff_secs == 0 {
        return Err("restart_max_backoff_secs must be at least 1".to_string());
    }

    let args = config
        .args
        .into_iter()
        .map(|arg| arg.trim().to_string())
        .filter(|arg| !arg.is_empty())
        .collect();

    let cwd = match config.cwd {
        Some(cwd) => {
            let trimmed = cwd.trim().to_string();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        },
        None => None,
    };

    let mut env = std::collections::BTreeMap::new();
    for (key, value) in config.env {
        let trimmed_key = key.trim().to_string();
        if trimmed_key.is_empty() {
            return Err("environment variable names cannot be empty".to_string());
        }
        if env.insert(trimmed_key.clone(), value).is_some() {
            return Err(format!("duplicate environment variable `{trimmed_key}`"));
        }
    }

    Ok(BotProcessConfig {
        enabled: config.enabled,
        command,
        args,
        env,
        cwd,
        auto_restart: config.auto_restart,
        restart_max_backoff_secs: config.restart_max_backoff_secs,
    })
}

use magician_learning::bots::{
    BotAuthSnapshot, BotAuthStateSnapshot, BotConfigStateSnapshot, BotConfigStoreError, BotLogLine,
    BotManager, BotManagerError, BotStatusSnapshot, ScopedBotRuntime, ScopedBotRuntimeError,
};

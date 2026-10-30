//! God mode settings (`execution.god_mode`).
//!
//! - `GET /api/magician/v2/settings/god-mode`: `{ enabled, auto_approve,
//!   always_grants }`.
//! - `PUT /api/magician/v2/settings/god-mode`: `{ "enabled": bool }` and/or
//!   `{ "auto_approve": bool }`. Writes the live `magician-config.yaml` and
//!   applies at once. Turning god mode on or off forgets every session and
//!   always grant (`god_mode::configure`); auto-approve keeps them. With
//!   auto-approve on, god mode's asks are approved without asking except
//!   deletes, root, code the line does not show and the blocked-command list
//!   (`god_mode::auto_approve_holds_back`).
//! - `DELETE /api/magician/v2/settings/god-mode/grants`: forget every grant
//!   without changing the setting.
//! - `GET/PUT /api/magician/v2/chat/sessions/{session_id}/god-mode`: one
//!   chat's override (the composer's Magician orb). PUT `{ "enabled": bool }`
//!   sets it, `{ "enabled": null }` follows the default again; `auto_approve`
//!   (bool or null) sets or clears the chat's auto-approve the same way. The
//!   body is `{ enabled, override, default, auto_approve,
//!   auto_approve_override, auto_approve_default }`: what applies, the chat's
//!   own choice, and the Settings default, for each.
//!
//! - `GET/PUT /api/magician/v2/settings/super-god-mode` (debug,
//!   `execution.super_god_mode`): PUT `{ "enabled": true, "duration": "1h" |
//!   "forever" }` or `{ "enabled": false }`. The body is `{ active, enabled,
//!   until_ms, remaining_secs }`. While active a harness launch asks the
//!   owner (once / session / always), then starts with no restrictions (chat/agent-run engines with their native tools and the
//!   owner's setup; VibeDev engines with no fence). Magician's own tools and
//!   god mode's approvals are unchanged; grants are kept.
//!
//! Owner login session or middleware-verified paired owner phone (not an
//! automation device or agent token). Errors are JSON `{ error, message }`.

use std::path::{Path, PathBuf};

use actix_web::{web, HttpMessage, HttpRequest, HttpResponse};
use serde_json::{json, Value};

use magician::config::{GodModeSettings, SuperGodModeSettings};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::cloudflare_access::{
    VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::god_mode;
use magician::magician_v2::runtime_settings::{
    apply_yaml_section, config_file_lock, runtime_settings_paths, write_text_file_atomic,
};

use crate::budgets_api::execution_settings_in;

fn error(
    status: actix_web::http::StatusCode,
    code: &str,
    message: impl Into<String>,
) -> HttpResponse {
    HttpResponse::build(status).json(json!({ "error": code, "message": message.into() }))
}

async fn require_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
) -> Result<(), HttpResponse> {
    crate::programs_api::require_owner_session_or_phone(req, auth.map(|data| data.get_ref())).await
}

/// The orb's rule, also applied to `/god` typed into a web chat
/// (`chat_api::god_mode_command_response`).
pub(crate) async fn require_chat_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
    session_id: &str,
) -> Result<(), HttpResponse> {
    require_owner(req, auth).await?;
    let identity = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .cloned()
        .filter(|identity| {
            identity.authentication() == VerifiedRequestAuthentication::PairedDevice
        });
    if let Some(identity) = identity {
        let chat = req
            .app_data::<web::Data<crate::chat_api::ChatApi>>()
            .ok_or_else(|| {
                error(
                    actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                    "chat_unavailable",
                    "Chat service is unavailable.",
                )
            })?;
        let session = chat
            .chat_service
            .get_session(session_id)
            .await
            .map_err(|_| {
                error(
                    actix_web::http::StatusCode::SERVICE_UNAVAILABLE,
                    "chat_unavailable",
                    "Chat session is unavailable.",
                )
            })?;
        if !session.is_some_and(|session| {
            crate::chat_api::session_matches_scope(
                &session,
                identity.principal(),
                identity.workspace().unwrap_or_default(),
            )
        }) {
            return Err(error(
                actix_web::http::StatusCode::NOT_FOUND,
                "chat_not_found",
                "Chat session not found in this phone's workspace.",
            ));
        }
    }
    Ok(())
}

fn body(settings: &GodModeSettings) -> Value {
    json!({
        "enabled": settings.enabled,
        "auto_approve": settings.auto_approve,
        "always_grants": god_mode::always_grant_count(),
    })
}

fn parse_update(body: &Value) -> Result<GodModeSettings, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut settings = god_mode::settings();
    let mut named = false;
    for (key, value) in object {
        match key.as_str() {
            "enabled" => {
                settings.enabled = value
                    .as_bool()
                    .ok_or_else(|| "enabled must be true or false".to_string())?;
                named = true;
            },
            "auto_approve" => {
                settings.auto_approve = value
                    .as_bool()
                    .ok_or_else(|| "auto_approve must be true or false".to_string())?;
                named = true;
            },
            other => return Err(format!("unknown field `{other}`")),
        }
    }
    if !named {
        return Err("name `enabled` or `auto_approve`".to_string());
    }
    Ok(settings)
}

/// Write `settings` as `execution.god_mode`, changing only the values that
/// differ from what the file holds; comments and siblings survive.
fn replace_god_mode(contents: &str, settings: &GodModeSettings) -> anyhow::Result<String> {
    let before = execution_settings_in(contents)?.god_mode;
    Ok(apply_yaml_section(
        contents,
        &["execution", "god_mode"],
        &before,
        settings,
    )?)
}

/// Read-modify-write the live config under the shared config-file lock, then
/// publish before releasing it.
fn save(path: &Path, settings: &GodModeSettings) -> anyhow::Result<()> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let next = replace_god_mode(&original, settings)?;
    anyhow::ensure!(
        execution_settings_in(&next)?.god_mode == *settings,
        "god_mode did not round-trip"
    );
    if next != original {
        write_text_file_atomic(path, &next, None)?;
    }
    god_mode::configure(settings);
    Ok(())
}

/// `GET /api/magician/v2/settings/god-mode`
pub async fn god_mode_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    HttpResponse::Ok().json(body(&god_mode::settings()))
}

/// `PUT /api/magician/v2/settings/god-mode`
pub async fn god_mode_put_handler(
    req: HttpRequest,
    payload: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let settings = match parse_update(&payload.into_inner()) {
        Ok(settings) => settings,
        Err(message) => {
            return error(
                actix_web::http::StatusCode::BAD_REQUEST,
                "invalid_god_mode",
                message,
            )
        },
    };
    put_at(runtime_settings_paths().config_path, settings).await
}

async fn put_at(path: PathBuf, settings: GodModeSettings) -> HttpResponse {
    let saved = settings.clone();
    match tokio::task::spawn_blocking(move || save(&path, &saved)).await {
        Ok(Ok(())) => {
            tracing::warn!(
                enabled = settings.enabled,
                auto_approve = settings.auto_approve,
                "[GOD-MODE] owner changed god mode"
            );
            HttpResponse::Ok().json(body(&settings))
        },
        result => {
            tracing::error!(?result, "[GOD-MODE] could not save execution.god_mode");
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "god_mode_not_saved",
                "Could not save god mode to magician-config.yaml.",
            )
        },
    }
}

fn chat_body(session_id: &str) -> Value {
    json!({
        "enabled": god_mode::chat_effective(session_id),
        "override": god_mode::chat_override(session_id),
        "default": god_mode::enabled(),
        "auto_approve": god_mode::chat_auto_effective(session_id),
        "auto_approve_override": god_mode::chat_auto_override(session_id),
        "auto_approve_default": god_mode::settings().auto_approve,
    })
}

/// A chat update: for each field named, the override to set (`None` follows
/// the default again).
#[derive(Debug, Default, PartialEq)]
struct ChatUpdate {
    enabled: Option<Option<bool>>,
    auto_approve: Option<Option<bool>>,
}

fn parse_chat_update(body: &Value) -> Result<ChatUpdate, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut update = ChatUpdate::default();
    for (key, value) in object {
        let choice = match value {
            Value::Bool(value) => Some(*value),
            Value::Null => None,
            _ => return Err(format!("{key} must be true, false or null")),
        };
        match key.as_str() {
            "enabled" => update.enabled = Some(choice),
            "auto_approve" => update.auto_approve = Some(choice),
            other => return Err(format!("unknown field `{other}`")),
        }
    }
    if update == ChatUpdate::default() {
        return Err("name `enabled` or `auto_approve`".to_string());
    }
    Ok(update)
}

/// `GET /api/magician/v2/chat/sessions/{session_id}/god-mode`
pub async fn chat_god_mode_get_handler(
    req: HttpRequest,
    path: web::Path<String>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_chat_owner(&req, auth.as_ref(), path.as_str()).await {
        return response;
    }
    HttpResponse::Ok().json(chat_body(path.as_str()))
}

/// `PUT /api/magician/v2/chat/sessions/{session_id}/god-mode`
pub async fn chat_god_mode_put_handler(
    req: HttpRequest,
    path: web::Path<String>,
    payload: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_chat_owner(&req, auth.as_ref(), path.as_str()).await {
        return response;
    }
    let session_id = path.into_inner();
    if session_id.trim().is_empty() {
        return error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_session",
            "session id is empty",
        );
    }
    match parse_chat_update(&payload.into_inner()) {
        Ok(update) => {
            if let Some(choice) = update.enabled {
                god_mode::set_chat_override(&session_id, choice);
                tracing::warn!(session = %session_id, ?choice, "[GOD-MODE] chat override changed");
            }
            if let Some(choice) = update.auto_approve {
                god_mode::set_chat_auto_override(&session_id, choice);
                tracing::warn!(session = %session_id, ?choice, "[GOD-MODE] chat auto-approve changed");
            }
            HttpResponse::Ok().json(chat_body(&session_id))
        },
        Err(message) => error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_god_mode",
            message,
        ),
    }
}

/// `DELETE /api/magician/v2/settings/god-mode/grants`
pub async fn god_mode_grants_delete_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let _ = tokio::task::spawn_blocking(god_mode::forget_all_grants).await;
    HttpResponse::Ok().json(body(&god_mode::settings()))
}

const SUPER_HOUR_MS: i64 = 3_600_000;

fn super_body(settings: &SuperGodModeSettings) -> Value {
    let now = chrono::Utc::now().timestamp_millis();
    let active = god_mode::super_on_at(settings, now);
    json!({
        "active": active,
        "enabled": settings.enabled,
        "until_ms": settings.until_ms,
        "remaining_secs": settings.until_ms.filter(|_| active).map(|until| (until - now).max(0) / 1000),
    })
}

/// `{ "enabled": true, "duration": "1h" | "forever" }` or `{ "enabled": false }`.
fn parse_super_update(body: &Value, now_ms: i64) -> Result<SuperGodModeSettings, String> {
    let object = body
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    if let Some(other) = object
        .keys()
        .find(|key| !matches!(key.as_str(), "enabled" | "duration"))
    {
        return Err(format!("unknown field `{other}`"));
    }
    let enabled = object
        .get("enabled")
        .and_then(Value::as_bool)
        .ok_or_else(|| "enabled must be true or false".to_string())?;
    if !enabled {
        return Ok(SuperGodModeSettings::default());
    }
    let until_ms = match object.get("duration").and_then(Value::as_str) {
        Some("1h") => Some(now_ms + SUPER_HOUR_MS),
        Some("forever") => None,
        _ => return Err("duration must be \"1h\" or \"forever\"".to_string()),
    };
    Ok(SuperGodModeSettings {
        enabled: true,
        until_ms,
    })
}

/// Write `settings` as `execution.super_god_mode`, changing only the values
/// that differ from what the file holds; comments and siblings survive.
fn replace_super_god_mode(
    contents: &str,
    settings: &SuperGodModeSettings,
) -> anyhow::Result<String> {
    let before = execution_settings_in(contents)?.super_god_mode;
    Ok(apply_yaml_section(
        contents,
        &["execution", "super_god_mode"],
        &before,
        settings,
    )?)
}

fn save_super(path: &Path, settings: &SuperGodModeSettings) -> anyhow::Result<()> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let next = replace_super_god_mode(&original, settings)?;
    anyhow::ensure!(
        execution_settings_in(&next)?.super_god_mode == *settings,
        "super_god_mode did not round-trip"
    );
    if next != original {
        write_text_file_atomic(path, &next, None)?;
    }
    god_mode::configure_super(settings);
    Ok(())
}

/// `GET /api/magician/v2/settings/super-god-mode`
pub async fn super_god_mode_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    HttpResponse::Ok().json(super_body(&god_mode::super_settings()))
}

/// `PUT /api/magician/v2/settings/super-god-mode`
pub async fn super_god_mode_put_handler(
    req: HttpRequest,
    payload: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let settings =
        match parse_super_update(&payload.into_inner(), chrono::Utc::now().timestamp_millis()) {
            Ok(settings) => settings,
            Err(message) => {
                return error(
                    actix_web::http::StatusCode::BAD_REQUEST,
                    "invalid_super_god_mode",
                    message,
                )
            },
        };
    let path = runtime_settings_paths().config_path;
    let saved = settings.clone();
    match tokio::task::spawn_blocking(move || save_super(&path, &saved)).await {
        Ok(Ok(())) => {
            tracing::warn!(
                enabled = settings.enabled,
                until_ms = ?settings.until_ms,
                "[SUPER-GOD] owner changed super god mode"
            );
            HttpResponse::Ok().json(super_body(&settings))
        },
        result => {
            tracing::error!(
                ?result,
                "[SUPER-GOD] could not save execution.super_god_mode"
            );
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "super_god_mode_not_saved",
                "Could not save super god mode to magician-config.yaml.",
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::device_pairing::DevicePairingStore;
    use std::sync::Arc;

    #[actix_web::test]
    async fn only_fresh_verified_owner_phones_can_manage_modes_without_a_login_cookie() {
        use actix_web::test::TestRequest;
        use magician::magician_v2::{
            cloudflare_access::MOBILE_DEVICE_ID_HEADER, device_bridge::DeviceKey,
        };
        let dir = tempfile::tempdir().unwrap();
        let pairing = Arc::new(DevicePairingStore::open(dir.path()).await.unwrap());
        pairing
            .pair(DeviceKey::new("owner", "default", "phone"), "Phone", 0)
            .await
            .unwrap();
        pairing
            .pair_esp32(DeviceKey::new("owner", "default", "badge"), "Badge", 0)
            .await
            .unwrap();
        for case in [
            "phone",
            "badge",
            "unknown",
            "spoofed",
            "stale",
            "wrong_scope",
            "agent",
        ] {
            let device = if case == "badge" || case == "unknown" {
                case
            } else {
                "phone"
            };
            let builder = TestRequest::default()
                .app_data(web::Data::new(pairing.clone()))
                .insert_header((MOBILE_DEVICE_ID_HEADER, device))
                .insert_header(("Authorization", "Bearer device-credential"));
            let builder = if case == "wrong_scope" {
                builder.insert_header(("X-Workspace", "other"))
            } else {
                builder
            };
            let req = builder.to_http_request();
            if case != "spoofed" {
                let authentication = if case == "agent" {
                    VerifiedRequestAuthentication::MagicianBearer
                } else {
                    VerifiedRequestAuthentication::PairedDevice
                };
                let issued_at = chrono::Utc::now()
                    - chrono::Duration::seconds(if case == "stale" { 10 } else { 0 });
                req.extensions_mut()
                    .insert(VerifiedRequestIdentity::for_test_at(
                        "owner",
                        Some("default"),
                        authentication,
                        issued_at,
                    ));
            }
            assert_eq!(
                require_owner(&req, None).await.is_ok(),
                case == "phone",
                "{case}"
            );
        }
    }

    #[test]
    fn super_god_mode_takes_an_hour_or_forever_and_off_clears_the_end() {
        let hour = parse_super_update(&json!({"enabled": true, "duration": "1h"}), 1_000).unwrap();
        assert_eq!(
            hour,
            SuperGodModeSettings {
                enabled: true,
                until_ms: Some(1_000 + SUPER_HOUR_MS)
            }
        );
        let forever =
            parse_super_update(&json!({"enabled": true, "duration": "forever"}), 1_000).unwrap();
        assert_eq!(
            forever,
            SuperGodModeSettings {
                enabled: true,
                until_ms: None
            }
        );
        assert_eq!(
            parse_super_update(&json!({"enabled": false}), 1).unwrap(),
            SuperGodModeSettings::default()
        );
        assert!(
            parse_super_update(&json!({"enabled": true}), 1).is_err(),
            "a duration is required"
        );
        assert!(parse_super_update(&json!({"enabled": true, "duration": "2h"}), 1).is_err());
        assert!(
            parse_super_update(&json!({"enabled": true, "duration": "1h", "x": 1}), 1).is_err()
        );
    }

    #[test]
    fn super_god_mode_is_written_beside_god_mode_and_round_trips() {
        let original = "execution:\n  god_mode:\n    enabled: true\nchat:\n  x: 1\n";
        let on = replace_super_god_mode(
            original,
            &SuperGodModeSettings {
                enabled: true,
                until_ms: Some(42),
            },
        )
        .unwrap();
        let parsed = execution_settings_in(&on).unwrap();
        assert!(parsed.god_mode.enabled, "{on}");
        assert_eq!(
            parsed.super_god_mode,
            SuperGodModeSettings {
                enabled: true,
                until_ms: Some(42)
            }
        );
        let off = replace_super_god_mode(&on, &SuperGodModeSettings::default()).unwrap();
        assert_eq!(off.matches("super_god_mode:").count(), 1, "{off}");
        assert!(!off.contains("until_ms"), "{off}");
        assert_eq!(
            execution_settings_in(&off).unwrap().super_god_mode,
            SuperGodModeSettings::default()
        );
    }

    #[test]
    fn a_chat_update_sets_or_clears_the_override() {
        assert_eq!(
            parse_chat_update(&json!({"enabled": true})),
            Ok(ChatUpdate {
                enabled: Some(Some(true)),
                auto_approve: None
            })
        );
        assert_eq!(
            parse_chat_update(&json!({"enabled": null})),
            Ok(ChatUpdate {
                enabled: Some(None),
                auto_approve: None
            })
        );
        assert_eq!(
            parse_chat_update(&json!({"auto_approve": true})),
            Ok(ChatUpdate {
                enabled: None,
                auto_approve: Some(Some(true))
            })
        );
        assert!(parse_chat_update(&json!({})).is_err());
        assert!(parse_chat_update(&json!({"enabled": 1})).is_err());
        assert!(parse_chat_update(&json!({"auto_approve": "yes"})).is_err());
        assert!(parse_chat_update(&json!({"enabled": true, "x": true})).is_err());
    }

    #[test]
    fn the_update_names_enabled_and_nothing_else() {
        assert!(parse_update(&json!({"enabled": true})).unwrap().enabled);
        assert!(
            parse_update(&json!({"auto_approve": true}))
                .unwrap()
                .auto_approve
        );
        assert!(parse_update(&json!({"auto_approve": 1})).is_err());
        assert!(parse_update(&json!({})).is_err());
        assert!(parse_update(&json!({"enabled": "yes"})).is_err());
        assert!(parse_update(&json!({"enabled": true, "x": 1})).is_err());
    }

    #[test]
    fn god_mode_is_written_into_the_execution_block_keeping_its_siblings() {
        let original = "# top\nexecution:\n  # sandbox\n  shell_sandbox:\n    mode: workspace_write\n  budgets:\n    native_max_cost_usd: 5\nchat:\n  x: 1\n";
        let on = replace_god_mode(
            original,
            &GodModeSettings {
                enabled: true,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(
            on.contains("  # sandbox\n  shell_sandbox:\n    mode: workspace_write\n"),
            "{on}"
        );
        assert!(on.contains("  god_mode:\n    enabled: true\n"), "{on}");
        assert!(on.ends_with("chat:\n  x: 1\n"), "{on}");
        assert!(execution_settings_in(&on).unwrap().god_mode.enabled);
        let off = replace_god_mode(
            &on,
            &GodModeSettings {
                enabled: false,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(off.matches("god_mode:").count(), 1, "{off}");
        assert!(!execution_settings_in(&off).unwrap().god_mode.enabled);
        assert_eq!(
            off,
            on.replace("    enabled: true\n", "    enabled: false\n")
        );
        let auto = GodModeSettings {
            enabled: true,
            auto_approve: true,
        };
        let auto_on = replace_god_mode(&on, &auto).unwrap();
        assert!(auto_on.contains("    auto_approve: true\n"), "{auto_on}");
        assert_eq!(execution_settings_in(&auto_on).unwrap().god_mode, auto);
    }
}

//! Harness credentials (`execution.harness_auth`).
//!
//! - `GET /api/magician/v2/settings/harness-auth`: `{ mode, engines, status }`.
//!   `status` lists each CLI (`claude_code`, `codex`, `grok`, `agy`, `pi`) with
//!   the mode that applies, its sign-in (`signed_in` boolean, and `sign_in`:
//!   `signed_in` | `expired` | `signed_out` with `sign_in_detail` in the CLI's
//!   words and a `remedy`), its `last_failure` (`class`, `reason`, `remedy`,
//!   `at_ms`), whether its launches get Magician's provider key, and
//!   `key_fallback`: `ask` (`login_first`: the key only after the owner
//!   approves), `always` (`magician`) or `never`.
//! - `PUT /api/magician/v2/settings/harness-auth`: `{ "mode"?: "login_first" |
//!   "login_only" | "magician", "engines"?: { "<cli>": <mode> | null } }`. An
//!   engine set to `null` follows `mode` again. Writes the live
//!   `magician-config.yaml` and applies at once.
//!
//! The owner's session (`plane_api::owner_identity`) or the owner's paired
//! iOS/Android app (`programs_api::is_paired_owner_phone`): whose account a
//! CLI bills is owner authority. Errors are JSON `{ "error", "message" }`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse};
use serde_json::{json, Value};

use magician::config::{HarnessAuthMode, HarnessAuthSettings, MagicianCodingSettings};
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::device_pairing::DevicePairingStore;
use magician::magician_v2::harness_auth;
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

/// The owner's signed-in session or the owner's paired iOS/Android app.
async fn require_owner(
    req: &HttpRequest,
    auth: Option<&web::Data<AuthRuntime>>,
    pairing: Option<&web::Data<Arc<DevicePairingStore>>>,
) -> Result<(), HttpResponse> {
    if crate::programs_api::is_paired_owner_phone(req, pairing.map(|data| data.get_ref().as_ref()))
        .await
    {
        return Ok(());
    }
    crate::plane_api::owner_identity(req, auth.map(|data| data.get_ref())).map(|_| ())
}

fn mode_name(mode: HarnessAuthMode) -> &'static str {
    match mode {
        HarnessAuthMode::LoginFirst => "login_first",
        HarnessAuthMode::LoginOnly => "login_only",
        HarnessAuthMode::Magician => "magician",
    }
}

fn parse_mode(value: &Value, field: &str) -> Result<HarnessAuthMode, String> {
    match value.as_str() {
        Some("login_first") => Ok(HarnessAuthMode::LoginFirst),
        Some("login_only") => Ok(HarnessAuthMode::LoginOnly),
        Some("magician") => Ok(HarnessAuthMode::Magician),
        _ => Err(format!(
            "{field} must be `login_first`, `login_only` or `magician`"
        )),
    }
}

/// The stored settings (without the legacy `use_api_key` folding).
fn stored(contents: &str) -> anyhow::Result<HarnessAuthSettings> {
    Ok(execution_settings_in(contents)?.harness_auth)
}

fn body(settings: &HarnessAuthSettings) -> Value {
    json!({
        "mode": mode_name(settings.mode),
        "engines": settings
            .engines
            .iter()
            .map(|(engine, mode)| (engine.clone(), json!(mode_name(*mode))))
            .collect::<serde_json::Map<_, _>>(),
        "status": harness_auth::status(),
    })
}

fn apply_update(
    mut settings: HarnessAuthSettings,
    update: &Value,
) -> Result<HarnessAuthSettings, String> {
    let object = update
        .as_object()
        .ok_or_else(|| "the body must be a JSON object".to_string())?;
    let mut named = false;
    for (key, value) in object {
        match key.as_str() {
            "mode" => {
                settings.mode = parse_mode(value, "mode")?;
                named = true;
            },
            "engines" => {
                let engines = value
                    .as_object()
                    .ok_or_else(|| "engines must be an object".to_string())?;
                for (engine, mode) in engines {
                    if !harness_auth::ENGINES.contains(&engine.as_str()) {
                        return Err(format!("unknown engine `{engine}`"));
                    }
                    if mode.is_null() {
                        settings.engines.remove(engine);
                    } else {
                        settings
                            .engines
                            .insert(engine.clone(), parse_mode(mode, engine)?);
                    }
                }
                named = true;
            },
            other => return Err(format!("unknown field `{other}`")),
        }
    }
    if !named {
        return Err("name `mode` or `engines`".to_string());
    }
    Ok(settings)
}

fn coding_in(contents: &str) -> MagicianCodingSettings {
    serde_yaml::from_str::<serde_yaml::Value>(contents)
        .ok()
        .and_then(|document| document.get("coding").cloned())
        .and_then(|coding| serde_yaml::from_value(coding).ok())
        .unwrap_or_default()
}

/// Read-modify-write the live config under the shared config-file lock, then
/// publish before releasing it.
fn save(path: &Path, update: &Value) -> anyhow::Result<Result<HarnessAuthSettings, String>> {
    let lock = config_file_lock(path);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let original = std::fs::read_to_string(path)?;
    let before = stored(&original)?;
    let settings = match apply_update(before.clone(), update) {
        Ok(settings) => settings,
        Err(message) => return Ok(Err(message)),
    };
    // Only the changed keys: the owner's comments in `execution:` stay.
    let next = apply_yaml_section(
        &original,
        &["execution", "harness_auth"],
        &before,
        &settings,
    )?;
    anyhow::ensure!(
        stored(&next)? == settings,
        "harness_auth did not round-trip"
    );
    write_text_file_atomic(path, &next, None)?;
    harness_auth::configure(&settings, &coding_in(&next));
    Ok(Ok(settings))
}

/// `GET /api/magician/v2/settings/harness-auth`
pub async fn harness_auth_get_handler(
    req: HttpRequest,
    auth: Option<web::Data<AuthRuntime>>,
    pairing: Option<web::Data<Arc<DevicePairingStore>>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref(), pairing.as_ref()).await {
        return response;
    }
    let path = runtime_settings_paths().config_path;
    let settings = std::fs::read_to_string(&path)
        .ok()
        .and_then(|contents| stored(&contents).ok())
        .unwrap_or_default();
    HttpResponse::Ok().json(body(&settings))
}

/// `PUT /api/magician/v2/settings/harness-auth`
pub async fn harness_auth_put_handler(
    req: HttpRequest,
    payload: web::Json<Value>,
    auth: Option<web::Data<AuthRuntime>>,
    pairing: Option<web::Data<Arc<DevicePairingStore>>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref(), pairing.as_ref()).await {
        return response;
    }
    put_at(runtime_settings_paths().config_path, payload.into_inner()).await
}

async fn put_at(path: PathBuf, update: Value) -> HttpResponse {
    match tokio::task::spawn_blocking(move || save(&path, &update)).await {
        Ok(Ok(Ok(settings))) => {
            tracing::warn!(
                mode = mode_name(settings.mode),
                overrides = settings.engines.len(),
                "[HARNESS-AUTH] owner changed harness credentials"
            );
            HttpResponse::Ok().json(body(&settings))
        },
        Ok(Ok(Err(message))) => error(
            actix_web::http::StatusCode::BAD_REQUEST,
            "invalid_harness_auth",
            message,
        ),
        result => {
            tracing::error!(
                ?result,
                "[HARNESS-AUTH] could not save execution.harness_auth"
            );
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "harness_auth_not_saved",
                "Could not save harness credentials to magician-config.yaml.",
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONFIG: &str = "chat:\n  harness_engine: magician\nexecution:\n  # owner note\n  harness_engine: magician\n  god_mode:\n    enabled: false\n";

    #[test]
    fn an_update_round_trips_through_the_execution_block() {
        let settings = apply_update(
            stored(CONFIG).unwrap(),
            &json!({"mode": "login_only", "engines": {"codex": "magician"}}),
        )
        .unwrap();
        let next = apply_yaml_section(
            CONFIG,
            &["execution", "harness_auth"],
            &stored(CONFIG).unwrap(),
            &settings,
        )
        .unwrap();
        assert_eq!(stored(&next).unwrap(), settings);
        assert!(
            next.starts_with(CONFIG),
            "only appended to its block: {next}"
        );
        assert!(next.contains("  # owner note\n"));
        let cleared = apply_update(settings, &json!({"engines": {"codex": null}})).unwrap();
        assert!(cleared.engines.is_empty());
        assert_eq!(cleared.mode, HarnessAuthMode::LoginOnly);
    }

    #[test]
    fn unknown_engines_and_modes_are_refused() {
        let base = HarnessAuthSettings::default();
        assert!(apply_update(base.clone(), &json!({"engines": {"cursor": "magician"}})).is_err());
        assert!(apply_update(base.clone(), &json!({"mode": "api"})).is_err());
        assert!(apply_update(base, &json!({})).is_err());
    }
}

//! Owner identities: which channel addresses are the owner, and claim codes.
//!
//! - `GET /api/magician/v2/owner-identities`: per channel, the owner
//!   identities from both sources — `envoy.owner_identities` in
//!   `magician-config.yaml` and the `envoy.owner_identity_envs` variable —
//!   masked to their last two characters, plus which channels a claim code
//!   can be redeemed from.
//! - `POST /api/magician/v2/owner-identities/claim-code` `{ "channel"?: str }`:
//!   mint a one-time code (8 characters, 10 minutes, single use). The owner
//!   sends `/claim <code>` to the Magician bot from the chat to record
//!   (`owner_claim`, answered by `chat_api`). Body `{ code, expires_at_ms,
//!   channel, claimable_channels, instructions }`.
//! - `DELETE /api/magician/v2/owner-identities/{channel}/{index}`: remove one
//!   identity by its index in the GET list. Only variable-held identities
//!   (claimed, or hand-set in the env files); a config-file identity is
//!   edited in `magician-config.yaml`.
//!
//! Owner login session or middleware-verified paired owner phone, like the
//! god-mode settings. Errors are JSON `{ error, message }`.

use actix_web::{web, HttpRequest, HttpResponse};
use serde::Deserialize;
use serde_json::{json, Value};

use magician::config::EnvoyConfig;
use magician::magician_v2::auth::middleware::AuthRuntime;
use magician::magician_v2::critical_delivery_settings::mask;
use magician::magician_v2::owner_claim::{
    claim_channel, claimable_channels, identity_channels, mint_claim_code, remove_env_identity,
    ClaimChannel, CLAIM_CODE_TTL_MS,
};
use magician::magician_v2::runtime_settings::runtime_settings_paths;

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

/// The live `envoy` section (the runtime `magician-config.yaml`).
fn load_envoy_config() -> EnvoyConfig {
    #[derive(Default, Deserialize)]
    struct View {
        #[serde(default)]
        envoy: EnvoyConfig,
    }
    std::fs::read_to_string(runtime_settings_paths().config_path)
        .ok()
        .and_then(|text| serde_yaml::from_str::<View>(&text).ok())
        .unwrap_or_default()
        .envoy
}

/// Every identity of a channel in `owner_identities_for` order, with where
/// it is held.
fn channel_identities(cfg: &EnvoyConfig, channel: &str) -> Vec<(String, &'static str)> {
    let configured: Vec<String> = cfg
        .owner_identities
        .iter()
        .filter(|(name, _)| name.trim().eq_ignore_ascii_case(channel))
        .flat_map(|(_, list)| list.iter().map(|identity| identity.trim().to_string()))
        .collect();
    cfg.owner_identities_for(channel)
        .into_iter()
        .map(|identity| {
            let source = if configured.contains(&identity) {
                "config"
            } else {
                "env"
            };
            (identity, source)
        })
        .collect()
}

fn body(cfg: &EnvoyConfig) -> Value {
    let claimable = claimable_channels(cfg);
    let channels: Vec<Value> = identity_channels(cfg)
        .into_iter()
        .map(|channel| {
            let identities: Vec<Value> = channel_identities(cfg, &channel)
                .into_iter()
                .enumerate()
                .map(|(index, (identity, source))| {
                    json!({
                        "index": index,
                        "masked": mask(&identity),
                        "source": source,
                        "removable": source == "env",
                    })
                })
                .collect();
            let is_claimable = claimable.contains(&channel);
            json!({
                "channel": channel,
                "identities": identities,
                "claimable": is_claimable,
            })
        })
        .collect();
    json!({
        "channels": channels,
        "claimable_channels": claimable,
        "code_ttl_secs": CLAIM_CODE_TTL_MS / 1000,
    })
}

/// `GET /api/magician/v2/owner-identities`
pub async fn list_handler(req: HttpRequest, auth: Option<web::Data<AuthRuntime>>) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(body(&load_envoy_config()))
}

#[derive(Debug, Default, Deserialize)]
pub struct ClaimCodeRequest {
    #[serde(default)]
    pub channel: Option<String>,
}

fn channel_label(channel: &str) -> &'static str {
    match channel {
        "kapso" | "whatsapp" => "WhatsApp",
        "telegram" => "Telegram",
        "sms" => "SMS",
        "imessage" => "iMessage",
        _ => "",
    }
}

/// `POST /api/magician/v2/owner-identities/claim-code`
pub async fn claim_code_handler(
    req: HttpRequest,
    payload: Option<web::Json<ClaimCodeRequest>>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let cfg = load_envoy_config();
    let channel = payload
        .and_then(|payload| payload.into_inner().channel)
        .map(|channel| channel.trim().to_ascii_lowercase())
        .filter(|channel| !channel.is_empty());
    let claimable = claimable_channels(&cfg);
    if let Some(channel) = channel.as_deref() {
        match claim_channel(&cfg, channel) {
            ClaimChannel::Claimable { .. } => {},
            ClaimChannel::Unverified => {
                return error(
                    actix_web::http::StatusCode::BAD_REQUEST,
                    "channel_not_claimable",
                    format!(
                        "`{channel}` does not authenticate its sender, so ownership cannot be \
                         claimed from it."
                    ),
                )
            },
            ClaimChannel::NoIdentityVariable => {
                return error(
                    actix_web::http::StatusCode::BAD_REQUEST,
                    "channel_not_claimable",
                    format!(
                        "`{channel}` has no owner identity variable \
                         (envoy.owner_identity_envs.{channel}); set one, or add the address \
                         to envoy.owner_identities by hand."
                    ),
                )
            },
        }
    } else if claimable.is_empty() {
        return error(
            actix_web::http::StatusCode::CONFLICT,
            "no_claimable_channel",
            "No channel can be claimed from: envoy.owner_identity_envs names no variable for \
             a WhatsApp (kapso) or Telegram channel.",
        );
    }
    let minted = mint_claim_code(channel.as_deref());
    let names: Vec<&str> = match channel.as_deref() {
        Some(channel) => vec![channel_label(channel)],
        None => claimable
            .iter()
            .map(|channel| channel_label(channel))
            .collect(),
    };
    let mut unique: Vec<&str> = Vec::new();
    for name in names {
        if !name.is_empty() && !unique.contains(&name) {
            unique.push(name);
        }
    }
    let names = unique;
    let place = if names.is_empty() {
        "the Magician bot".to_string()
    } else {
        format!("the Magician bot on {}", names.join(" or "))
    };
    tracing::warn!(
        channel = channel.as_deref().unwrap_or("any"),
        "[OWNER-CLAIM] owner minted a claim code"
    );
    let instructions = format!(
        "Send /claim {} to {place} within {} minutes.",
        minted.code,
        CLAIM_CODE_TTL_MS / 60_000
    );
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store"))
        .json(json!({
            "code": minted.code,
            "expires_at_ms": minted.expires_at_ms,
            "channel": minted.channel,
            "claimable_channels": claimable,
            "instructions": instructions,
        }))
}

/// `DELETE /api/magician/v2/owner-identities/{channel}/{index}`
pub async fn delete_handler(
    req: HttpRequest,
    path: web::Path<(String, usize)>,
    auth: Option<web::Data<AuthRuntime>>,
) -> HttpResponse {
    if let Err(response) = require_owner(&req, auth.as_ref()).await {
        return response;
    }
    let (channel, index) = path.into_inner();
    let channel = channel.trim().to_ascii_lowercase();
    let cfg = load_envoy_config();
    let Some((identity, source)) = channel_identities(&cfg, &channel).into_iter().nth(index) else {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            "identity_not_found",
            "No such owner identity; reload the list.",
        );
    };
    if source != "env" {
        return error(
            actix_web::http::StatusCode::CONFLICT,
            "identity_in_config",
            format!(
                "This identity is set in magician-config.yaml (envoy.owner_identities.{channel}); \
                 remove it there."
            ),
        );
    }
    let Some(env_name) = cfg
        .owner_identity_envs
        .iter()
        .find(|(name, _)| name.trim().eq_ignore_ascii_case(&channel))
        .map(|(_, env)| env.trim().to_string())
    else {
        return error(
            actix_web::http::StatusCode::NOT_FOUND,
            "identity_not_found",
            "No such owner identity; reload the list.",
        );
    };
    let paths = runtime_settings_paths();
    match tokio::task::spawn_blocking(move || remove_env_identity(&paths, &env_name, &identity))
        .await
    {
        Ok(Ok(true)) => {
            tracing::warn!(channel = %channel, "[OWNER-CLAIM] owner removed an owner identity");
            HttpResponse::Ok().json(body(&load_envoy_config()))
        },
        Ok(Ok(false)) => error(
            actix_web::http::StatusCode::CONFLICT,
            "identity_not_in_env_files",
            "This identity comes from the process environment, not the env files; change it \
             where Magician is launched.",
        ),
        result => {
            tracing::error!(?result, "[OWNER-CLAIM] could not remove an owner identity");
            error(
                actix_web::http::StatusCode::INTERNAL_SERVER_ERROR,
                "identity_not_removed",
                "Could not update the env files.",
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn the_list_shows_both_sources_masked_and_only_env_ones_removable() {
        let env_name = "MAGICIAN_TEST_OWNER_IDENTITIES_API_LIST";
        std::env::set_var(env_name, "919876543210");
        let mut owner_identities = HashMap::new();
        owner_identities.insert("kapso".to_string(), vec!["15551234567".to_string()]);
        let mut owner_identity_envs = HashMap::new();
        owner_identity_envs.insert("kapso".to_string(), env_name.to_string());
        owner_identity_envs.insert("agentmail".to_string(), format!("{env_name}_MAIL"));
        let cfg = EnvoyConfig {
            owner_identities,
            owner_identity_envs,
            ..EnvoyConfig::default()
        };
        let body = body(&cfg);
        assert_eq!(body["claimable_channels"], json!(["kapso"]));
        let kapso = body["channels"]
            .as_array()
            .unwrap()
            .iter()
            .find(|channel| channel["channel"] == "kapso")
            .unwrap();
        assert_eq!(kapso["claimable"], json!(true));
        assert_eq!(
            kapso["identities"],
            json!([
                { "index": 0, "masked": "…67", "source": "config", "removable": false },
                { "index": 1, "masked": "…10", "source": "env", "removable": true },
            ])
        );
        assert!(!body.to_string().contains("919876543210"));
        std::env::remove_var(env_name);
    }
}

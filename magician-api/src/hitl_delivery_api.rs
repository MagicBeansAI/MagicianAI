//! Critical-request delivery API (secure HITL plan §6.1, P5).
//!
//! Two audiences. The channel bots claim and report deliveries with their
//! runtime-minted `mag_bot_` bearer — a claim hands over the owner address
//! for exactly one delivery of the bot's own channel type and binds the
//! record to that bot and its realtime connection generation. The owner
//! reads value-free status, edits the `hitl.critical_delivery` section, and
//! sends a test alert; opening or saving the settings sends nothing.
use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::Deserialize;
use serde_json::json;

use magician::config::HitlCriticalDeliverySettings;
use magician::magician_v2::{
    api_scope::resolve_required_scope,
    auth::{middleware::authenticated, BearerKind},
    critical_delivery_settings::CriticalDeliverySettingsStore,
    hitl_delivery::{self, ClaimError, DeliveryOutcome, DeliveryPolicy, ReportError},
};

use crate::web_api::MagicianV2Api;
use magician::magician_v2::execution::agent_resources::AgentResources;

const MAX_REASON_BYTES: usize = 200;
const MAX_PROVIDER_ID_BYTES: usize = 128;

#[derive(Debug, Deserialize)]
pub struct ClaimRequest {
    pub channel_type: String,
    /// The bot's current realtime connection generation (an opaque string
    /// the bot changes each time it reconnects).
    #[serde(default)]
    pub connection_generation: String,
}

#[derive(Debug, Deserialize)]
pub struct ReportRequest {
    /// `provider_accepted`, `confirmed_delivered` or `failed`.
    pub status: String,
    #[serde(default)]
    pub provider_message_id: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    /// The connection that claimed this delivery. Only it may say what the
    /// provider did with the send: one bot NAME can be two processes, and an
    /// orphan must not take a delivery terminal for its replacement's send.
    /// Absent from an older SDK, and then unchecked.
    #[serde(default)]
    pub connection_generation: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct StatusQuery {
    #[serde(default)]
    pub correlation_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PutCriticalDeliveryRequest {
    #[serde(default)]
    pub critical_delivery: HitlCriticalDeliverySettings,
}

fn coordinator() -> Option<std::sync::Arc<hitl_delivery::DeliveryCoordinator>> {
    hitl_delivery::global()
}

fn unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(json!({
        "error": "critical_delivery_unavailable",
        "message": "the delivery coordinator is not running in this process",
    }))
}

fn bounded(text: &str, max: usize) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if out.len() + ch.len_utf8() > max {
            break;
        }
        out.push(ch);
    }
    out
}

/// The bot behind a bearer: its name (the bot's channel type by
/// convention) and the scope engraved at spawn. Anything else is refused —
/// a person's session or API token never claims a delivery.
fn bot_caller(req: &HttpRequest) -> Result<(String, String, String), HttpResponse> {
    let Some(stamped) = authenticated(req) else {
        return Err(HttpResponse::Unauthorized().json(json!({
            "error": "bot_token_required",
            "message": "a runtime-minted bot bearer is required to claim or report a delivery",
        })));
    };
    match &stamped.bearer {
        BearerKind::Bot { bot_name } => Ok((
            bot_name.clone(),
            stamped.scope.principal().to_string(),
            stamped.scope.workspace().to_string(),
        )),
        _ => Err(HttpResponse::Forbidden().json(json!({
            "error": "bot_token_required",
            "message": "only the channel's bot may claim or report a delivery",
        }))),
    }
}

/// `POST /api/magician/v2/hitl/deliveries/{id}/claim`
pub async fn claim_delivery_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ClaimRequest>,
) -> Result<HttpResponse> {
    let (bot_name, principal, workspace) = match bot_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return Ok(response),
    };
    let Some(coordinator) = coordinator() else {
        return Ok(unavailable());
    };
    let channel_type = body.channel_type.trim().to_ascii_lowercase();
    // A bot claims only its own channel type: the bot's name is the channel
    // type the runtime spawned it for (`skillshub/bots/<name>`).
    if !bot_name.eq_ignore_ascii_case(&channel_type) {
        return Ok(HttpResponse::Forbidden().json(json!({
            "error": "channel_mismatch",
            "message": format!("bot `{bot_name}` cannot claim a `{channel_type}` delivery"),
        })));
    }
    let bot_identity = format!("bot:{bot_name}");
    let generation = bounded(body.connection_generation.trim(), 64);
    match coordinator
        .claim(
            &path,
            &principal,
            &workspace,
            &channel_type,
            &bot_identity,
            &generation,
        )
        .await
    {
        Ok(grant) => Ok(HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(grant)),
        Err(ClaimError::UnknownDelivery) | Err(ClaimError::WrongScope) => {
            // One answer for "unknown" and "not yours": the id alone reveals
            // nothing about another scope's deliveries.
            Ok(HttpResponse::NotFound().json(json!({"error": "delivery_not_found"})))
        },
        Err(ClaimError::WrongChannel) => {
            Ok(HttpResponse::Forbidden().json(json!({"error": "channel_mismatch"})))
        },
        Err(ClaimError::NotClaimable(state)) => Ok(HttpResponse::Conflict().json(json!({
            "error": "delivery_not_claimable",
            "state": state.as_str(),
        }))),
    }
}

/// `POST /api/magician/v2/hitl/deliveries/{id}/report`
pub async fn report_delivery_handler(
    req: HttpRequest,
    path: web::Path<String>,
    body: web::Json<ReportRequest>,
) -> Result<HttpResponse> {
    let (bot_name, principal, workspace) = match bot_caller(&req) {
        Ok(caller) => caller,
        Err(response) => return Ok(response),
    };
    let Some(coordinator) = coordinator() else {
        return Ok(unavailable());
    };
    let outcome = match body.status.trim() {
        "provider_accepted" => DeliveryOutcome::ProviderAccepted {
            provider_message_id: body
                .provider_message_id
                .as_deref()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|id| bounded(id, MAX_PROVIDER_ID_BYTES)),
        },
        "confirmed_delivered" => DeliveryOutcome::ConfirmedDelivered,
        "failed" => DeliveryOutcome::Failed {
            reason: bounded(
                body.reason
                    .as_deref()
                    .unwrap_or("the bot reported a failure")
                    .trim(),
                MAX_REASON_BYTES,
            ),
        },
        other => return Ok(HttpResponse::BadRequest().json(json!({
            "error": "invalid_report_status",
            "message": format!("`{other}` is not provider_accepted, confirmed_delivered or failed"),
        }))),
    };
    let bot_identity = format!("bot:{bot_name}");
    let reporting_generation = body
        .connection_generation
        .as_deref()
        .map(str::trim)
        .filter(|generation| !generation.is_empty())
        .map(|generation| bounded(generation, 64));
    match coordinator
        .report(
            &path,
            &principal,
            &workspace,
            &bot_identity,
            reporting_generation.as_deref(),
            outcome,
        )
        .await
    {
        Ok(record) => Ok(HttpResponse::Ok().json(json!({
            "delivery_id": record.id,
            "state": record.state.as_str(),
        }))),
        Err(ReportError::UnknownDelivery) | Err(ReportError::WrongScope) => {
            Ok(HttpResponse::NotFound().json(json!({"error": "delivery_not_found"})))
        },
        Err(ReportError::NotTheClaimant) => {
            Ok(HttpResponse::Forbidden().json(json!({"error": "not_the_claimant"})))
        },
        Err(ReportError::NotReportable(state)) => Ok(HttpResponse::Conflict().json(json!({
            "error": "delivery_not_reportable",
            "state": state.as_str(),
        }))),
    }
}

/// The owner's own surfaces refuse a machine bearer.
///
/// The middleware stamps a bot grant's scope headers like anyone else's, so
/// `resolve_required_scope` alone let a channel bot — the least-trusted process
/// in this design, and one that only ever needs `claim` and `report` — read the
/// whole delivery log, fire a real test alert at every destination, and turn
/// critical delivery off. A person's session or API token is the caller here;
/// `device_pairing_api`'s `pairing_session_scope` draws the same line.
fn owner_caller(req: &HttpRequest) -> Result<(), HttpResponse> {
    if let Some(stamped) = authenticated(req) {
        if matches!(
            stamped.bearer,
            BearerKind::Bot { .. } | BearerKind::Grant { .. }
        ) {
            return Err(HttpResponse::Forbidden().json(json!({
                "error": "owner_surface",
                "message": "a machine bearer may claim and report a delivery, nothing else",
            })));
        }
    }
    Ok(())
}

/// `GET /api/magician/v2/hitl/deliveries?correlation_id=` — the owner's
/// value-free status (addresses masked, no alert bodies).
pub async fn delivery_status_handler(
    req: HttpRequest,
    query: web::Query<StatusQuery>,
) -> Result<HttpResponse> {
    if let Err(response) = owner_caller(&req) {
        return Ok(response);
    }
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(coordinator) = coordinator() else {
        return Ok(unavailable());
    };
    let status = coordinator
        .status(
            &principal,
            &workspace,
            query
                .correlation_id
                .as_deref()
                .map(str::trim)
                .filter(|c| !c.is_empty()),
        )
        .await;
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(status))
}

fn settings_store() -> CriticalDeliverySettingsStore {
    CriticalDeliverySettingsStore::new(
        magician::config::magician_config_path()
            .parent()
            .unwrap_or(&std::path::PathBuf::from(".")),
    )
}

/// `GET /api/magician/v2/settings/critical-delivery`
pub async fn get_critical_delivery_settings_handler(req: HttpRequest) -> Result<HttpResponse> {
    if let Err(response) = owner_caller(&req) {
        return Ok(response);
    }
    match settings_store().load_envelope().await {
        Ok(envelope) => Ok(HttpResponse::Ok().json(envelope)),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "critical_delivery_settings_io_error",
            "message": error.to_string(),
        }))),
    }
}

/// `PUT /api/magician/v2/settings/critical-delivery` — durable write, live
/// reload, and the coordinator takes the new policy. Sends nothing.
pub async fn put_critical_delivery_settings_handler(
    req: HttpRequest,
    api: web::Data<MagicianV2Api>,
    agent_resources: web::Data<std::sync::Arc<AgentResources>>,
    llm_content_settings: Option<
        web::Data<
            magician::magician_v2::analytics::llm_trace_content::LlmContentCaptureSettingsHandle,
        >,
    >,
    body: web::Json<PutCriticalDeliveryRequest>,
) -> Result<HttpResponse> {
    if let Err(response) = owner_caller(&req) {
        return Ok(response);
    }
    let settings = body.into_inner().critical_delivery;
    if let Err(message) = CriticalDeliverySettingsStore::validate(&settings) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "invalid_critical_delivery_settings",
            "message": message,
        })));
    }
    let store = settings_store();
    let envelope = match store.save(settings).await {
        Ok(envelope) => envelope,
        Err(error) => {
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "critical_delivery_settings_io_error",
                "message": error.to_string(),
            })))
        },
    };
    let reload = api
        .reload_magician_config(
            Some(agent_resources.get_ref().as_ref()),
            llm_content_settings.as_deref().map(std::sync::Arc::as_ref),
        )
        .await;
    let reload_error = match reload {
        Ok(response) if response.status().is_success() => None,
        Ok(response) => Some(format!("reload returned HTTP {}", response.status())),
        Err(error) => Some(format!("reload failed: {error}")),
    };
    // The coordinator's policy is the envelope's own view of the written
    // section — the same file the reload read — so the next request uses it
    // even when an unrelated section made the whole-config reload refuse.
    if let Some(coordinator) = coordinator() {
        let config_path = store.settings_path();
        let policy = tokio::task::spawn_blocking(move || {
            magician::config::load_magician_config_from_path(&config_path)
                .ok()
                .map(|config| DeliveryPolicy::from_config(&config))
        })
        .await
        .ok()
        .flatten();
        if let Some(policy) = policy {
            coordinator.reload(policy).await;
        }
    }
    let mut body = serde_json::to_value(&envelope).unwrap_or_else(|_| json!({}));
    if let Some(object) = body.as_object_mut() {
        object.insert("reload_applied".to_string(), json!(reload_error.is_none()));
        if let Some(error) = reload_error.clone() {
            object.insert("reload_error".to_string(), json!(error));
        }
    }
    if reload_error.is_none() {
        Ok(HttpResponse::Ok().json(body))
    } else {
        Ok(HttpResponse::InternalServerError().json(body))
    }
}

/// `POST /api/magician/v2/settings/critical-delivery/test` — the owner's
/// explicit test: a real delivery through every enabled destination.
pub async fn test_critical_delivery_handler(req: HttpRequest) -> Result<HttpResponse> {
    if let Err(response) = owner_caller(&req) {
        return Ok(response);
    }
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(coordinator) = coordinator() else {
        return Ok(unavailable());
    };
    let records = coordinator.send_test(&principal, &workspace).await;
    let correlation_id = records.first().map(|r| r.correlation_id.clone());
    Ok(HttpResponse::Accepted().json(json!({
        "correlation_id": correlation_id,
        "destinations": records.len(),
        "deliveries": records.iter().map(|r| r.status_view()).collect::<Vec<_>>(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_and_put_bodies_parse_their_shapes() {
        let report: ReportRequest =
            serde_json::from_str(r#"{"status":"provider_accepted","provider_message_id":"m1"}"#)
                .unwrap();
        assert_eq!(report.status, "provider_accepted");
        let put: PutCriticalDeliveryRequest = serde_json::from_str(
            r#"{"critical_delivery":{"enabled_channels":["telegram"],"policy":"staged"}}"#,
        )
        .unwrap();
        assert_eq!(
            put.critical_delivery.enabled_channels,
            vec!["telegram".to_string()]
        );
        assert_eq!(
            put.critical_delivery.staged_fallback_secs, 45,
            "defaults fill in"
        );
        let empty: PutCriticalDeliveryRequest = serde_json::from_str("{}").unwrap();
        assert!(empty.critical_delivery.push_enabled);
    }
}

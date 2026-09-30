//! Automatic verification-code retrieval API (secure HITL plan §6.2, P6).
//!
//! - The agentic answer sink: an agentic pause is answered through the very
//!   resume path a person's answer runs (`resume_agentic_execution_with_scope`),
//!   installed once from the first API instance.
//! - The owner's purpose grants: `PUT /channel-assist/channels/purpose` on an
//!   Observe account, `PUT /devices/policy/verification-codes` on a paired
//!   device.
//! - Value-free retrieval status for one ask: `GET /hitl/{id}/retrieval`.
use std::sync::Arc;

use actix_web::{web, HttpMessage, HttpRequest, HttpResponse, Result};
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

use magician::magician_v2::{
    api_scope::resolve_required_scope,
    cloudflare_access::{VerifiedRequestAuthentication, VerifiedRequestIdentity},
    device_governance::DevicePolicyStore,
    device_pairing::DevicePairingStore,
    observe_connectors::VERIFICATION_CODES_PURPOSE,
    user_requests::{ANDROID_NOTIFICATION_CHANNEL, VERIFICATION_CODE_RESOLVER_CHANNEL},
    verification_codes::{self, AnswerOutcome, AnswerTarget, ChallengeAnswerSink},
};

use crate::web_api::{AgenticResumeRequest, AgenticResumeValue, MagicianV2Api};

/// Answers an agentic pause with a retrieved code exactly as `POST
/// /hitl/{id}/respond` with `source: agentic` would: the pause spec decides
/// the sensitivity route, the resume validates and continues the run.
pub struct ApiAgenticAnswerSink {
    api: Arc<MagicianV2Api>,
}

impl ApiAgenticAnswerSink {
    pub fn new(api: Arc<MagicianV2Api>) -> Self {
        Self { api }
    }

    /// Install from the first API instance; later instances are equivalent.
    pub fn install(api: Arc<MagicianV2Api>) {
        verification_codes::install_agentic_sink(Arc::new(Self::new(api)));
    }
}

#[async_trait]
impl ChallengeAnswerSink for ApiAgenticAnswerSink {
    async fn answer(&self, target: &AnswerTarget, code: Zeroizing<String>) -> AnswerOutcome {
        let Some(execution_id) = target
            .execution_id
            .clone()
            .filter(|id| !id.trim().is_empty())
        else {
            return AnswerOutcome::Refused("the pause names no execution".to_string());
        };
        let request = AgenticResumeRequest {
            pause_state_id: Some(target.correlation_id.clone()),
            plan_id: None,
            step_id: None,
            input_type: "otp".to_string(),
            value: AgenticResumeValue::Password {
                value: code.to_string(),
            },
            agent_id: None,
            goal_id: None,
            cycle_id: None,
        };
        let response = self
            .api
            .resume_agentic_execution_with_scope(
                web::Path::from(execution_id),
                web::Json(request),
                Some((target.principal.clone(), target.workspace.clone())),
            )
            .await;
        match response {
            Ok(response) if response.status().is_success() => AnswerOutcome::Accepted,
            Ok(response)
                if response.status() == actix_web::http::StatusCode::CONFLICT
                    || response.status() == actix_web::http::StatusCode::NOT_FOUND
                    || response.status() == actix_web::http::StatusCode::GONE =>
            {
                AnswerOutcome::AlreadyResolved
            },
            Ok(response) => {
                AnswerOutcome::Refused(format!("the resume answered HTTP {}", response.status()))
            },
            Err(error) => AnswerOutcome::Refused(format!("the resume failed: {error}")),
        }
    }
}

/// Who may answer under which channel (`POST /hitl/{id}/respond`):
/// `verification_code_resolver` is the in-process resolver's own name and
/// no HTTP caller's; `android_notification` is the companion's trusted
/// handoff and needs the paired device's own credential *and* the owner's
/// verification-code grant for that device — withdrawing the grant refuses
/// the phone's answer even while a watch is in flight. Every other channel
/// is attribution only.
pub async fn answer_channel_policy(
    request: &HttpRequest,
    channel: &str,
    device_policy: Option<&Arc<DevicePolicyStore>>,
) -> std::result::Result<(), HttpResponse> {
    match channel {
        VERIFICATION_CODE_RESOLVER_CHANNEL => Err(HttpResponse::BadRequest().json(json!({
            "error": "reserved_channel",
            "message": "this channel names the runtime's own resolver; an answer over HTTP carries its own channel",
        }))),
        ANDROID_NOTIFICATION_CHANNEL => {
            let identity = request.extensions().get::<VerifiedRequestIdentity>().cloned();
            let device_id = device_id_header(request);
            let (Some(identity), Some(device_id)) = (identity, device_id) else {
                return Err(HttpResponse::Unauthorized().json(json!({
                    "error": "paired_device_required",
                    "message": "an answer from a phone's notifications rides the paired device's own credential",
                })));
            };
            if identity.authentication() != VerifiedRequestAuthentication::PairedDevice {
                return Err(HttpResponse::Unauthorized().json(json!({
                    "error": "paired_device_required",
                    "message": "an answer from a phone's notifications rides the paired device's own credential",
                })));
            }
            let permitted = match device_policy {
                Some(policy) => policy.permits_verification_codes(&device_id).await,
                None => false,
            };
            if !permitted {
                return Err(HttpResponse::Forbidden().json(json!({
                    "error": "device_not_permitted_for_verification_codes",
                    "message": "this device is not permitted to answer with codes from its notifications",
                })));
            }
            Ok(())
        },
        _ => Ok(()),
    }
}

/// The paired device the middleware authenticated names itself in this
/// header (the legacy spelling included).
fn device_id_header(request: &HttpRequest) -> Option<String> {
    ["X-Magician-Device-Id", "X-Magdroid-Device"]
        .into_iter()
        .find_map(|name| {
            request
                .headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
                })
                .map(str::to_owned)
        })
}

#[derive(Debug, Deserialize)]
pub struct ChannelPurposeRequest {
    pub provider: String,
    pub account_alias: String,
    /// Only `verification_codes` is a purpose today.
    #[serde(default = "default_purpose")]
    pub purpose: String,
    pub granted: bool,
}

fn default_purpose() -> String {
    VERIFICATION_CODES_PURPOSE.to_string()
}

#[derive(Debug, Deserialize)]
pub struct ScopeQuery {
    #[serde(default)]
    pub workspace: Option<String>,
}

/// `PUT /api/magician/v2/channel-assist/channels/purpose` — grant or
/// withdraw the verification-code purpose on one configured account. The
/// account must already be configured; a purpose is never granted to an
/// account that is not there, and observation consent grants none.
pub async fn put_channel_purpose_handler(
    req: HttpRequest,
    query: web::Query<ScopeQuery>,
    body: web::Json<ChannelPurposeRequest>,
    api: web::Data<crate::channel_assist_api::ChannelAssistApi>,
) -> Result<HttpResponse> {
    let workspace_layout = api.workspace_layout();
    let (principal, workspace) =
        match resolve_required_scope(req.headers(), query.workspace.clone()) {
            Ok(scope) => scope,
            Err(response) => return Ok(response),
        };
    if body.purpose != VERIFICATION_CODES_PURPOSE {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "unknown_purpose",
            "message": format!("`{}` is not a purpose an account can carry", body.purpose),
        })));
    }
    let channel =
        magician_comms::channel_assist::channel_observe::provider_to_channel(&body.provider)
            .to_string();
    match magician_comms::channel_assist::channel_observe::set_channel_purpose(
        workspace_layout,
        &principal,
        &workspace,
        &channel,
        body.account_alias.trim(),
        VERIFICATION_CODES_PURPOSE,
        body.granted,
    )
    .await
    {
        Ok(true) => Ok(HttpResponse::Ok().json(json!({
            "provider": body.provider,
            "account_alias": body.account_alias,
            "purpose": VERIFICATION_CODES_PURPOSE,
            "granted": body.granted,
        }))),
        Ok(false) => Ok(HttpResponse::NotFound().json(json!({
            "error": "account_not_configured",
            "message": "enable the account on the Observe surface before granting it a purpose",
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "channel_purpose_write_failed",
            "message": error.to_string(),
        }))),
    }
}

#[derive(Debug, Deserialize)]
pub struct DeviceVerificationCodesRequest {
    pub device_id: String,
    pub permitted: bool,
}

/// `PUT /api/magician/v2/devices/policy/verification-codes` — permit or
/// withdraw a paired device's notifications as a code source. The device
/// must be paired in this scope; pairing alone grants nothing.
pub async fn put_device_verification_codes_handler(
    req: HttpRequest,
    body: web::Json<DeviceVerificationCodesRequest>,
    policy: web::Data<Arc<DevicePolicyStore>>,
    pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let device_id = body.device_id.trim();
    let paired = pairing
        .list(&principal, &workspace)
        .await
        .into_iter()
        .any(|device| device.device_id == device_id);
    if !paired {
        return Ok(HttpResponse::NotFound().json(json!({
            "error": "device_not_paired",
            "message": "pair the device in this scope before permitting it",
        })));
    }
    match policy
        .set_verification_code_device(device_id, body.permitted)
        .await
    {
        Ok(devices) => Ok(HttpResponse::Ok().json(json!({
            "verification_code_devices": devices,
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "policy_persist_failed",
            "detail": error.to_string(),
        }))),
    }
}

/// `GET /api/magician/v2/hitl/{correlation_id}/retrieval` — the value-free
/// retrieval status of one ask (`waiting`, `code_used`, `ambiguous`,
/// `unavailable`, `stopped`, or `none` when nothing is watching).
pub async fn get_retrieval_status_handler(
    req: HttpRequest,
    path: web::Path<String>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match resolve_required_scope(req.headers(), None) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let Some(resolver) = verification_codes::global() else {
        return Ok(
            HttpResponse::Ok().json(json!({"correlation_id": path.as_str(), "status": "none"}))
        );
    };
    match resolver.status(&principal, &workspace, &path).await {
        Some(status) => Ok(HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(status)),
        None => {
            Ok(HttpResponse::Ok().json(json!({"correlation_id": path.as_str(), "status": "none"})))
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use actix_web::test::TestRequest;

    #[actix_web::test]
    async fn the_answer_channel_policy_refuses_the_reserved_name_and_an_unpermitted_phone() {
        let plain = TestRequest::default().to_http_request();
        assert!(answer_channel_policy(&plain, "web", None).await.is_ok());
        assert!(answer_channel_policy(&plain, "telegram", None)
            .await
            .is_ok());
        let reserved = answer_channel_policy(&plain, VERIFICATION_CODE_RESOLVER_CHANNEL, None)
            .await
            .unwrap_err();
        assert_eq!(reserved.status(), actix_web::http::StatusCode::BAD_REQUEST);
        // A browser session claiming the phone's channel is refused; so is a
        // phone the grant does not cover.
        let spoof = answer_channel_policy(&plain, ANDROID_NOTIFICATION_CHANNEL, None)
            .await
            .unwrap_err();
        assert_eq!(spoof.status(), actix_web::http::StatusCode::UNAUTHORIZED);
        let dir = tempfile::tempdir().unwrap();
        let policy = Arc::new(DevicePolicyStore::open(dir.path()).await.unwrap());
        let phone = TestRequest::default()
            .insert_header(("X-Magician-Device-Id", "pixel-1"))
            .to_http_request();
        phone
            .extensions_mut()
            .insert(VerifiedRequestIdentity::for_test(
                "owner",
                Some("ws"),
                VerifiedRequestAuthentication::PairedDevice,
            ));
        let unpermitted =
            answer_channel_policy(&phone, ANDROID_NOTIFICATION_CHANNEL, Some(&policy))
                .await
                .unwrap_err();
        assert_eq!(unpermitted.status(), actix_web::http::StatusCode::FORBIDDEN);
        policy
            .set_verification_code_device("pixel-1", true)
            .await
            .unwrap();
        assert!(
            answer_channel_policy(&phone, ANDROID_NOTIFICATION_CHANNEL, Some(&policy))
                .await
                .is_ok()
        );
        policy
            .set_verification_code_device("pixel-1", false)
            .await
            .unwrap();
        assert!(
            answer_channel_policy(&phone, ANDROID_NOTIFICATION_CHANNEL, Some(&policy))
                .await
                .is_err(),
            "withdrawing the grant refuses the next answer"
        );
    }

    #[test]
    fn purpose_and_device_bodies_parse() {
        let purpose: ChannelPurposeRequest = serde_json::from_str(
            r#"{"provider":"gmail","account_alias":"personal","granted":true}"#,
        )
        .unwrap();
        assert_eq!(purpose.purpose, VERIFICATION_CODES_PURPOSE);
        let device: DeviceVerificationCodesRequest =
            serde_json::from_str(r#"{"device_id":"pixel-1","permitted":false}"#).unwrap();
        assert!(!device.permitted);
    }
}

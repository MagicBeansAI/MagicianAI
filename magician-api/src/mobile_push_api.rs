//! Authenticated native-device push registration.
//!
//! The mobile credential middleware has already replaced caller-controlled
//! scope headers with the paired device's scope. This boundary additionally
//! resolves the device kind from the server-owned roster and never accepts a
//! principal, workspace, or device id in the request body.

use std::sync::Arc;

use actix_web::{web, HttpRequest, HttpResponse, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;

use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::cloudflare_access::{
    LEGACY_ANDROID_DEVICE_ID_HEADER, MOBILE_DEVICE_ID_HEADER,
};
use magician::magician_v2::device_pairing::{DevicePairingStore, MobileClientKind};
use magician::magician_v2::mobile_push::{
    MobilePushConfig, MobilePushError, MobilePushStore, PushEnvironment, PushPlatform,
    PushRegistrationKind, RegisterPushInput,
};

#[derive(Debug, Deserialize)]
pub struct RegisterMobilePushRequest {
    pub platform: PushPlatform,
    pub kind: PushRegistrationKind,
    pub token: String,
    #[serde(default)]
    pub environment: PushEnvironment,
    #[serde(default)]
    pub task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RemoveMobilePushQuery {
    pub kind: PushRegistrationKind,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub revision: Option<i64>,
}

#[derive(Debug, Serialize)]
struct MobilePushRegistrationResponse {
    id: String,
    revision: i64,
    platform: PushPlatform,
    kind: PushRegistrationKind,
    environment: PushEnvironment,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
}

/// `PUT /api/magician/v2/devices/me/push`
pub async fn put_mobile_push_handler(
    req: HttpRequest,
    body: web::Json<RegisterMobilePushRequest>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    store: web::Data<Arc<MobilePushStore>>,
    provider_config: web::Data<MobilePushConfig>,
) -> Result<HttpResponse> {
    let Some((principal, workspace, device_id, client_kind)) =
        authenticated_device(&req, pairing.get_ref()).await
    else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "mobile_device_credential_required"
        })));
    };

    if !platform_matches_client(body.platform, client_kind) {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "push_platform_mismatch",
            "detail": "push platform does not match the paired device kind"
        })));
    }
    if !provider_config.supports(body.platform) {
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": "mobile_push_provider_not_configured",
            "detail": "this deployment has no provider credentials for the paired device platform"
        })));
    }

    let task_id = body
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let input = RegisterPushInput {
        principal,
        workspace,
        device_id,
        platform: body.platform,
        kind: body.kind,
        token: body.token.trim().to_string(),
        environment: body.environment,
        task_id,
        updated_at_ms: chrono::Utc::now().timestamp_millis(),
    };
    match store.upsert(input).await {
        Ok(row) => Ok(HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(MobilePushRegistrationResponse {
                id: row.id,
                revision: row.updated_at_ms,
                platform: row.platform,
                kind: row.kind,
                environment: row.environment,
                task_id: row.task_id,
            })),
        Err(error) => push_error_response(error),
    }
}

/// `DELETE /api/magician/v2/devices/me/push`
pub async fn delete_mobile_push_handler(
    req: HttpRequest,
    query: web::Query<RemoveMobilePushQuery>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    store: web::Data<Arc<MobilePushStore>>,
) -> Result<HttpResponse> {
    let Some((principal, workspace, device_id, _)) =
        authenticated_device(&req, pairing.get_ref()).await
    else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "mobile_device_credential_required"
        })));
    };
    let task_id = query
        .task_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match store
        .remove_for_device_kind(
            &principal,
            &workspace,
            &device_id,
            query.kind,
            task_id,
            query.revision,
        )
        .await
    {
        Ok(removed) => Ok(HttpResponse::Ok().json(json!({ "removed": removed }))),
        Err(error) => push_error_response(error),
    }
}

async fn authenticated_device(
    req: &HttpRequest,
    pairing: &Arc<DevicePairingStore>,
) -> Option<(String, String, String, MobileClientKind)> {
    let header = |name: &str| {
        req.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let device_id =
        header(MOBILE_DEVICE_ID_HEADER).or_else(|| header(LEGACY_ANDROID_DEVICE_ID_HEADER))?;
    let (principal, workspace) = resolve_required_scope(req.headers(), None).ok()?;
    let client_kind = pairing
        .client_kind(&principal, &workspace, &device_id)
        .await?;
    Some((principal, workspace, device_id, client_kind))
}

fn platform_matches_client(platform: PushPlatform, client_kind: MobileClientKind) -> bool {
    matches!(
        (platform, client_kind),
        (PushPlatform::Apns, MobileClientKind::Ios)
            | (PushPlatform::Fcm, MobileClientKind::Android)
    )
}

fn push_error_response(error: MobilePushError) -> Result<HttpResponse> {
    let status = match error {
        MobilePushError::InvalidToken
        | MobilePushError::InvalidBinding
        | MobilePushError::TaskRequired
        | MobilePushError::UnexpectedTask => actix_web::http::StatusCode::BAD_REQUEST,
        MobilePushError::Capacity => actix_web::http::StatusCode::TOO_MANY_REQUESTS,
        MobilePushError::Io(_) | MobilePushError::Corrupt(_) => {
            actix_web::http::StatusCode::INTERNAL_SERVER_ERROR
        },
    };
    Ok(HttpResponse::build(status).json(json!({
        "error": "mobile_push_registration_failed",
        "detail": error.to_string()
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_is_derived_from_the_server_owned_device_kind() {
        assert!(platform_matches_client(
            PushPlatform::Apns,
            MobileClientKind::Ios
        ));
        assert!(platform_matches_client(
            PushPlatform::Fcm,
            MobileClientKind::Android
        ));
        assert!(!platform_matches_client(
            PushPlatform::Fcm,
            MobileClientKind::Ios
        ));
        assert!(!platform_matches_client(
            PushPlatform::Apns,
            MobileClientKind::Android
        ));
        assert!(!platform_matches_client(
            PushPlatform::Apns,
            MobileClientKind::Desktop
        ));
        assert!(!platform_matches_client(
            PushPlatform::Fcm,
            MobileClientKind::Desktop
        ));
        assert!(!platform_matches_client(
            PushPlatform::Apns,
            MobileClientKind::Esp32
        ));
        assert!(!platform_matches_client(
            PushPlatform::Fcm,
            MobileClientKind::Esp32
        ));
    }

    #[test]
    fn registration_response_cannot_serialize_a_provider_token() {
        let value = serde_json::to_value(MobilePushRegistrationResponse {
            id: "push_1".to_string(),
            revision: 42,
            platform: PushPlatform::Apns,
            kind: PushRegistrationKind::Application,
            environment: PushEnvironment::Sandbox,
            task_id: None,
        })
        .unwrap();

        assert!(value.get("token").is_none());
        assert_eq!(value["id"], "push_1");
        assert_eq!(value["revision"], 42);
    }
}

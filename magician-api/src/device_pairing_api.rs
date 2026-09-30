//! Enrolling a companion device, and the roster of the ones already trusted.
//!
//! The bridge socket refuses anything it cannot recognise, which means there
//! has to be a way to become recognised. That is this file — [`DevicePairingStore`]
//! has done the work of minting and verifying tokens since it was written, and
//! nothing ever called it, so no device could connect at all.
//!
//! A token is as good as the device: it lets a socket receive taps, typing and
//! screenshots meant for somebody's phone. So it is shown exactly once, at
//! pairing, and only its digest is kept. There is deliberately no way to read
//! one back — an endpoint that could would turn the roster into a set of
//! working credentials.

use std::sync::Arc;

use actix_web::{web, HttpMessage, HttpRequest, HttpResponse, Result};
use qrcode::{render::svg, QrCode};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use magician::magician_v2::api_scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::cloudflare_access::{
    VerifiedEspPairBootstrap, VerifiedRequestAuthentication, VerifiedRequestIdentity,
};
use magician::magician_v2::device_bridge::{DeviceBridgeHub, DeviceKey};
use magician::magician_v2::device_governance::{
    DeviceActionAudit, DevicePolicyStore, ScreenshotPolicy,
};
use magician::magician_v2::device_pairing::{DevicePairingStore, MobileClientKind, PairingError};
use magician::magician_v2::mobile_push::MobilePushStore;
use magician::magician_v2::runtime::edge_sessions::{EdgeSessionKey, EdgeSessionRegistry};

#[derive(Debug, Deserialize)]
pub struct PairRequest {
    /// The device's own stable id, the same one it dials the bridge with.
    pub device_id: String,
    /// What the owner will see in the roster. Falls back to the id.
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutAutomationReviewRequest {
    pub expected_generation: u64,
    pub allowed_packages: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RevokeAutomationReviewQuery {
    pub expected_generation: u64,
}

#[derive(Debug, Deserialize)]
pub struct BeginEnrollmentRequest {
    #[serde(default)]
    pub client_kind: MobileClientKind,
    #[serde(default)]
    pub connection_mode: EnrollmentConnectionMode,
    /// Retained only for older web clients. The server-owned runtime origin
    /// always wins; a browser must not decide where a phone sends credentials.
    #[serde(default, rename = "public_origin")]
    pub _legacy_public_origin: Option<String>,
}

/// The owner chooses a server-owned route, never an arbitrary destination.
/// `same_wifi` is advertised only when the native service has a private-LAN
/// listener; `remote` is the deployment's HTTPS tunnel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EnrollmentConnectionMode {
    SameWifi,
    #[default]
    Remote,
}

#[derive(Debug, Clone)]
pub struct MobileEnrollmentConfig {
    pub public_origin: Option<String>,
    pub local_origin: Option<String>,
    pub android_apps_signing_sha256: Vec<String>,
    pub android_attestation_root_sha256: Vec<String>,
    pub android_apps_apk_sha256: Vec<String>,
    pub android_apps_version_codes: Vec<u64>,
    pub android_play_integrity_cloud_project_number: Option<u64>,
    pub android_play_integrity_version_codes: Vec<u64>,
    pub android_play_integrity_required_device_verdicts: Vec<String>,
    pub android_play_integrity_service_account_path: Option<String>,
}

impl MobileEnrollmentConfig {
    pub fn resolve(configured: Option<&str>) -> std::result::Result<Self, &'static str> {
        Self::resolve_routes(configured, None)
    }

    pub fn resolve_routes(
        configured: Option<&str>,
        local_origin: Option<&str>,
    ) -> std::result::Result<Self, &'static str> {
        Self::resolve_with_apps_pins(configured, local_origin, &[], &[], &[], &[], None, &[], &[])
    }

    pub fn resolve_with_apps_pins(
        configured: Option<&str>,
        local_origin: Option<&str>,
        android_apps_signing_sha256: &[String],
        android_attestation_root_sha256: &[String],
        android_apps_apk_sha256: &[String],
        android_apps_version_codes: &[u64],
        android_play_integrity_cloud_project_number: Option<u64>,
        android_play_integrity_version_codes: &[u64],
        android_play_integrity_required_device_verdicts: &[String],
    ) -> std::result::Result<Self, &'static str> {
        let service_account_path = std::env::var("MAGICIAN_PLAY_INTEGRITY_SERVICE_ACCOUNT_PATH")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());
        Ok(Self {
            public_origin: configured.map(normalized_public_origin).transpose()?,
            local_origin: local_origin.map(normalized_local_origin).transpose()?,
            android_apps_signing_sha256: android_apps_signing_sha256.to_vec(),
            android_attestation_root_sha256: android_attestation_root_sha256.to_vec(),
            android_apps_apk_sha256: android_apps_apk_sha256.to_vec(),
            android_apps_version_codes: android_apps_version_codes.to_vec(),
            android_play_integrity_cloud_project_number,
            android_play_integrity_version_codes: android_play_integrity_version_codes.to_vec(),
            android_play_integrity_required_device_verdicts:
                android_play_integrity_required_device_verdicts.to_vec(),
            android_play_integrity_service_account_path: service_account_path,
        })
    }
}

#[derive(Debug, Clone, serde::Serialize)]
struct CloudflareBootstrapCredential {
    client_id: String,
    client_secret: String,
}

fn cloudflare_bootstrap_credential() -> Option<CloudflareBootstrapCredential> {
    let value = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    };
    value("CF_ACCESS_CLIENT_ID")
        .zip(value("CF_ACCESS_CLIENT_SECRET"))
        .map(|(client_id, client_secret)| CloudflareBootstrapCredential {
            client_id,
            client_secret,
        })
}

#[derive(Debug, Deserialize)]
pub struct ExchangeEnrollmentRequest {
    pub enrollment_id: String,
    pub secret: String,
    pub device_id: String,
    #[serde(default)]
    pub label: Option<String>,
}

fn scope(req: &HttpRequest) -> std::result::Result<(String, String), HttpResponse> {
    resolve_required_scope(req.headers(), None)
}

fn normalized_public_origin(raw: &str) -> std::result::Result<String, &'static str> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| "public_origin_invalid")?;
    let loopback_http = parsed.scheme() == "http"
        && parsed.host_str().is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost") || host == "::1" || host.starts_with("127.")
        });
    if !matches!(parsed.scheme(), "https" | "http")
        || (parsed.scheme() != "https" && !loopback_http)
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err("public_origin_invalid");
    }
    let origin = parsed.origin().ascii_serialization();
    if origin == "null" {
        return Err("public_origin_invalid");
    }
    Ok(origin)
}

fn normalized_local_origin(raw: &str) -> std::result::Result<String, &'static str> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| "local_origin_invalid")?;
    let private_host = parsed
        .host_str()
        .and_then(|host| host.parse::<std::net::IpAddr>().ok())
        .is_some_and(|address| match address {
            std::net::IpAddr::V4(address) => {
                address.is_private() || address.is_loopback() || address.is_link_local()
            },
            std::net::IpAddr::V6(address) => {
                address.is_loopback()
                    || address.is_unique_local()
                    || address.is_unicast_link_local()
            },
        });
    if !matches!(parsed.scheme(), "https" | "http")
        || !private_host
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !matches!(parsed.path(), "" | "/")
    {
        return Err("local_origin_invalid");
    }
    let origin = parsed.origin().ascii_serialization();
    if origin == "null" {
        return Err("local_origin_invalid");
    }
    Ok(origin)
}

fn enrollment_uri(
    origin: &str,
    enrollment_id: &str,
    secret: &str,
    client_kind: MobileClientKind,
) -> String {
    let mut uri = url::Url::parse("magican://connect").expect("static enrollment URI");
    uri.query_pairs_mut()
        .append_pair("base", origin)
        .append_pair("id", enrollment_id)
        .append_pair("secret", secret)
        .append_pair(
            "kind",
            match client_kind {
                MobileClientKind::Ios => "ios",
                MobileClientKind::Android => "android",
                MobileClientKind::Esp32 => "esp32",
                MobileClientKind::Desktop => "desktop",
            },
        );
    uri.to_string()
}

fn qr_svg(value: &str) -> std::result::Result<String, qrcode::types::QrError> {
    Ok(QrCode::new(value.as_bytes())?
        .render::<svg::Color>()
        .quiet_zone(true)
        .min_dimensions(288, 288)
        .dark_color(svg::Color("#17191f"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

/// `POST /api/magician/v2/devices/enrollment`
///
/// Starts the owner side of passwordless setup. The response is deliberately
/// non-cacheable because the URI is a bearer capability. The durable bridge
/// token does not exist yet and therefore cannot leak through this response.
pub async fn begin_device_enrollment_handler(
    req: HttpRequest,
    body: web::Json<BeginEnrollmentRequest>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> Result<HttpResponse> {
    let origin = match body.connection_mode {
        EnrollmentConnectionMode::SameWifi => enrollment_config.local_origin.as_deref(),
        EnrollmentConnectionMode::Remote => enrollment_config.public_origin.as_deref(),
    };
    let Some(origin) = origin else {
        let error = match body.connection_mode {
            EnrollmentConnectionMode::SameWifi => "mobile_local_origin_not_available",
            EnrollmentConnectionMode::Remote => "mobile_public_origin_not_configured",
        };
        return Ok(HttpResponse::ServiceUnavailable().json(json!({
            "error": error,
        })));
    };
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let now_ms = chrono::Utc::now().timestamp_millis();
    let ticket = match pairing
        .begin_mobile_enrollment_at_origin(
            &principal,
            &workspace,
            body.client_kind,
            Some(origin.to_owned()),
            now_ms,
        )
        .await
    {
        Ok(ticket) => ticket,
        Err(PairingError::TooManyPendingEnrollments) => {
            return Ok(HttpResponse::TooManyRequests().json(json!({
                "error": "too_many_pending_enrollments",
            })));
        },
        Err(PairingError::SealUnavailable(_) | PairingError::Corrupt(_)) => {
            return Ok(HttpResponse::ServiceUnavailable().json(json!({
                "error": "pairing_authority_unavailable",
            })));
        },
        Err(error) => {
            warn!("[DEVICE-PAIRING] could not begin enrollment: {error}");
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "enrollment_failed",
            })));
        },
    };
    let uri = enrollment_uri(
        origin,
        &ticket.enrollment_id,
        &ticket.secret,
        body.client_kind,
    );
    let svg = match qr_svg(&uri) {
        Ok(svg) => svg,
        Err(error) => {
            let _ = pairing
                .cancel_enrollment(&principal, &workspace, &ticket.enrollment_id)
                .await;
            warn!("[DEVICE-PAIRING] could not render enrollment QR: {error}");
            return Ok(HttpResponse::InternalServerError().json(json!({
                "error": "qr_render_failed",
            })));
        },
    };

    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .insert_header(("Pragma", "no-cache"))
        .json(json!({
            "enrollment_id": ticket.enrollment_id,
            "enrollment_uri": uri,
            "qr_svg": svg,
            "expires_at_ms": ticket.expires_at_ms,
            "principal": principal,
            "workspace": workspace,
            "client_kind": body.client_kind,
            "connection_mode": body.connection_mode,
            "origin": origin,
        })))
}

/// `POST /api/magician/v2/devices/enrollment/exchange`
///
/// The unpaired phone presents the QR secret and its stable id. Scope comes
/// from the ticket, never from phone-controlled headers. Successful exchange
/// consumes the ticket and returns the durable token exactly once.
pub async fn exchange_device_enrollment_handler(
    body: web::Json<ExchangeEnrollmentRequest>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    hub: web::Data<Arc<DeviceBridgeHub>>,
    edge: Option<web::Data<Arc<EdgeSessionRegistry>>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> Result<HttpResponse> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let label = body.label.as_deref().unwrap_or(&body.device_id);
    match pairing
        .exchange_enrollment(
            body.enrollment_id.trim(),
            body.secret.trim(),
            body.device_id.trim(),
            label,
            now_ms,
        )
        .await
    {
        Ok(grant) => {
            let cloudflare_access = cloudflare_bootstrap_credential();
            hub.revoke(&DeviceKey::new(
                &grant.principal,
                &grant.workspace,
                &grant.device_id,
            ));
            if let Some(edge) = edge {
                edge.revoke(&EdgeSessionKey::new(
                    &grant.principal,
                    &grant.workspace,
                    &grant.device_id,
                ));
            }
            info!(
                "[DEVICE-PAIRING] enrolled {} ({}::{})",
                grant.device_id, grant.principal, grant.workspace
            );
            Ok(HttpResponse::Ok()
                .insert_header(("Cache-Control", "no-store, max-age=0"))
                .insert_header(("Pragma", "no-cache"))
                .json(json!({
                    "device_id": grant.device_id,
                    "label": grant.label,
                    "token": grant.token,
                    "principal": grant.principal,
                    "workspace": grant.workspace,
                    "paired_at_ms": grant.paired_at_ms,
                    "client_kind": grant.client_kind,
                    "capabilities": grant.capabilities,
                    "public_origin": grant.connection_origin.or_else(|| enrollment_config.public_origin.clone()),
                    "cloudflare_access": cloudflare_access,
                })))
        },
        Err(PairingError::InvalidEnrollment) => Ok(HttpResponse::Gone()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(json!({ "error": "enrollment_invalid_or_expired" }))),
        Err(PairingError::AppsOwnerRequired) => Ok(HttpResponse::Forbidden().json(json!({
            "error": "android_apps_owner_channel_required",
        }))),
        Err(PairingError::SealUnavailable(_) | PairingError::Corrupt(_)) => {
            Ok(HttpResponse::ServiceUnavailable().json(json!({
                "error": "pairing_authority_unavailable",
            })))
        },
        Err(error) => {
            warn!("[DEVICE-PAIRING] enrollment exchange failed: {error}");
            Ok(HttpResponse::InternalServerError().json(json!({
                "error": "enrollment_exchange_failed",
            })))
        },
    }
}

/// `DELETE /api/magician/v2/devices/enrollment/{enrollment_id}`
pub async fn cancel_device_enrollment_handler(
    req: HttpRequest,
    path: web::Path<String>,
    pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let cancelled = pairing
        .cancel_enrollment(&principal, &workspace, &path.into_inner())
        .await;
    Ok(HttpResponse::Ok().json(json!({ "cancelled": cancelled })))
}

/// `POST /api/magician/v2/devices/pair`
///
/// Returns the token once. Pairing an id that is already paired replaces its
/// token rather than failing — re-pairing is how an owner recovers a phone
/// whose token was lost, and the alternative is unpairing first, which leaves
/// them one failed step away from a device they cannot re-enrol.

/// Route-local session resolution for the pairing doors. These routes are
/// public paths — the middleware discards the bearer there by design — so a
/// member pairing their phone presents a session bearer that ONLY this
/// resolver sees. Returns the member's (principal, workspace) when a valid
/// session backs the request; `None` keeps the anonymous/default bootstrap.
///
/// This is member-aware pairing (owner rule 2026-08-30): a family member's
/// handset lands in the MEMBER's scope, not the owner's. Refuses API tokens
/// and grants — a device key is a long-lived capability and only an
/// interactive session should mint one into a member scope.
fn pairing_session_scope(
    req: &HttpRequest,
    auth: Option<&actix_web::web::Data<magician::magician_v2::auth::middleware::AuthRuntime>>,
) -> Option<(String, String)> {
    use magician::magician_v2::auth::BearerKind;
    let auth = auth?;
    let token = magician::magician_v2::auth::middleware::bearer_token(req.headers())?;
    let resolved = auth.store.resolve_bearer(&token).ok()??;
    let workspace = match &resolved.kind {
        BearerKind::Session(_) => resolved.workspace.clone(),
        BearerKind::ApiToken | BearerKind::Grant { .. } | BearerKind::Bot { .. } => return None,
    };
    let identity = auth.store.find_identity(&resolved.identity).ok()??;
    Some((identity.scope_root, workspace))
}

pub async fn pair_device_handler(
    req: HttpRequest,
    body: web::Json<PairRequest>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    hub: web::Data<Arc<DeviceBridgeHub>>,
    edge: Option<web::Data<Arc<EdgeSessionRegistry>>>,
    auth: Option<actix_web::web::Data<magician::magician_v2::auth::middleware::AuthRuntime>>,
) -> Result<HttpResponse> {
    let verified_access_bootstrap = req.extensions().get::<VerifiedEspPairBootstrap>().is_some();
    let verified_loopback = req
        .extensions()
        .get::<VerifiedRequestIdentity>()
        .is_some_and(|identity| {
            identity.authentication() == VerifiedRequestAuthentication::TrustedLoopbackSingleUser
        });
    if !verified_access_bootstrap && !verified_loopback {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "device_pairing_bootstrap_required",
            "message": "ESP pairing requires loopback or verified Cloudflare Access.",
        })));
    }

    let device_id = body.device_id.trim().to_string();
    if device_id.is_empty() {
        return Ok(HttpResponse::BadRequest().json(json!({
            "error": "device_id_required",
        })));
    }

    // The ESP bootstrap exception stays: with no session bearer the pair
    // engraves the local single-user scope. But a MEMBER pairing their own
    // phone presents a session (resolved route-locally — the middleware
    // skips public paths) and the device lands in the member's scope.
    let (principal, workspace) = pairing_session_scope(&req, auth.as_ref()).unwrap_or_else(|| {
        (
            DEFAULT_SCOPE_PRINCIPAL.to_string(),
            DEFAULT_SCOPE_WORKSPACE.to_string(),
        )
    });
    let key = DeviceKey::new(&principal, &workspace, &device_id);
    let label = body
        .label
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&device_id)
        .to_string();

    let now_ms = chrono::Utc::now().timestamp_millis();
    match pairing.pair_esp32(key.clone(), label.clone(), now_ms).await {
        Ok(token) => {
            hub.revoke(&key);
            if let Some(edge) = edge {
                edge.revoke(&EdgeSessionKey::new(&principal, &workspace, &device_id));
            }
            // The id and scope are logged; the token never is. A credential in
            // a log file outlives every control that was placed around it.
            info!("[DEVICE-PAIRING] paired {device_id} ({principal}::{workspace})");
            Ok(HttpResponse::Ok()
                .insert_header(("Cache-Control", "no-store, max-age=0"))
                .insert_header(("Pragma", "no-cache"))
                .json(json!({
                    "device_id": device_id,
                    "label": label,
                    "principal": principal,
                    "workspace": workspace,
                    "token": token,
                    "paired_at_ms": now_ms,
                    "note": "Store this now. It is not recoverable.",
                })))
        },
        Err(PairingError::AppsOwnerRequired) => Ok(HttpResponse::Forbidden().json(json!({
            "error": "android_apps_owner_channel_required",
        }))),
        Err(error) => {
            warn!("[DEVICE-PAIRING] could not pair {device_id}: {error}");
            Ok(HttpResponse::InternalServerError().json(json!({
                "error": "pairing_failed",
                "detail": error.to_string(),
            })))
        },
    }
}

/// `GET /api/magician/v2/devices`
///
/// The roster for this scope. `last_seen_ms` is the useful column: it is how an
/// owner spots a device that has gone quiet, or one they do not recognise.
pub async fn list_devices_handler(
    req: HttpRequest,
    pairing: web::Data<Arc<DevicePairingStore>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    // `list` blanks the digests before returning, so this cannot leak one even
    // by accident.
    let devices = pairing.list_mobile(&principal, &workspace).await;
    Ok(HttpResponse::Ok().json(json!({
        "principal": principal,
        "workspace": workspace,
        "devices": devices,
        "pairing_available": pairing.ensure_available().is_ok(),
        "connection_options": {
            "same_wifi": enrollment_config.local_origin,
            "remote": enrollment_config.public_origin,
        },
    })))
}

/// `PUT /api/magician/v2/devices/{device_id}/automation-review`
///
/// Permanently denied compatibility symbol.
///
/// This raw-device-id route used the general V2 scope headers and was never an
/// owner authority boundary. It is intentionally not mounted, and direct
/// callers also fail closed. Review is available only through the trusted,
/// opaque-target, one-shot channel in `android_apps_owner_api`.
pub async fn put_device_automation_review_handler(
    _req: HttpRequest,
    _path: web::Path<String>,
    _body: web::Json<PutAutomationReviewRequest>,
    _pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    Ok(HttpResponse::Forbidden().json(json!({
        "error": "android_apps_owner_channel_required",
    })))
}

/// Denied raw-device-id counterpart to [`put_device_automation_review_handler`].
pub async fn revoke_device_automation_review_handler(
    _req: HttpRequest,
    _path: web::Path<String>,
    _query: web::Query<RevokeAutomationReviewQuery>,
    _pairing: web::Data<Arc<DevicePairingStore>>,
) -> Result<HttpResponse> {
    Ok(HttpResponse::Forbidden().json(json!({
        "error": "android_apps_owner_channel_required",
    })))
}

/// `GET /api/magician/v2/devices/me`
///
/// A content-free post-exchange probe used before a native client commits its
/// connection profile. Middleware has already authenticated the device token
/// and replaced caller scope with the roster-owned scope.
pub async fn get_mobile_device_handler(req: HttpRequest) -> Result<HttpResponse> {
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let device_id = ["X-Magician-Device-Id", "X-Magdroid-Device"]
        .into_iter()
        .find_map(|name| {
            req.headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        });
    let Some(device_id) = device_id else {
        return Ok(HttpResponse::Unauthorized().json(json!({
            "error": "mobile_device_credential_required",
        })));
    };
    Ok(HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(json!({
            "device_id": device_id,
            "principal": principal,
            "workspace": workspace,
        })))
}

/// `DELETE /api/magician/v2/devices/{device_id}`
///
/// Revocation. The device keeps its copy of the token, so this has to be the
/// side that decides — the next socket it opens is refused.
pub async fn unpair_device_handler(
    req: HttpRequest,
    path: web::Path<String>,
    pairing: web::Data<Arc<DevicePairingStore>>,
    hub: web::Data<Arc<DeviceBridgeHub>>,
    edge: Option<web::Data<Arc<EdgeSessionRegistry>>>,
    push_store: Option<web::Data<Arc<MobilePushStore>>>,
) -> Result<HttpResponse> {
    let device_id = path.into_inner();
    // Reserved path segments. Registering `/devices/policy` and
    // `/devices/audit` before this route only shields the METHODS they
    // define — actix's router walks past a resource whose method guard
    // fails, so a DELETE on either literal path lands here with the segment
    // as a device id. Refuse it by name rather than unpairing a device that
    // happens to be called "policy".
    if matches!(
        device_id.as_str(),
        "policy" | "audit" | "me" | "enrollment" | "pair" | "bridge"
    ) {
        return Ok(HttpResponse::MethodNotAllowed().json(json!({
            "error": "method_not_allowed",
            "detail": format!("`/devices/{device_id}` does not support this method"),
        })));
    }
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let key = DeviceKey::new(&principal, &workspace, &device_id);

    let outcome = pairing.unpair_mobile(&key).await;
    // `unpair` removes from the in-memory authority before persisting. Even if
    // that persistence reports an error, the current process must fail closed
    // and terminate the already-authorized socket; the owner's idempotent retry
    // will settle the durable absent snapshot.
    if !matches!(&outcome, Ok(false)) {
        hub.revoke(&key);
        if let Some(edge) = edge {
            edge.revoke(&EdgeSessionKey::new(&principal, &workspace, &device_id));
        }
    }
    match outcome {
        Ok(removed) => {
            if removed {
                info!("[DEVICE-PAIRING] unpaired {device_id} ({principal}::{workspace})");
            }
            if let Some(push_store) = push_store {
                if let Err(error) = push_store
                    .remove_for_device(&principal, &workspace, &device_id)
                    .await
                {
                    return Ok(HttpResponse::InternalServerError().json(json!({
                        "error": "unpair_push_cleanup_failed",
                        "detail": error.to_string(),
                    })));
                }
            }
            Ok(HttpResponse::Ok().json(json!({
                "device_id": device_id,
                // Distinguishes "revoked" from "was never paired" — for the
                // owner's own roster that is information they are entitled to,
                // unlike the socket handler, where the same distinction would
                // be an oracle for an unauthenticated caller.
                "removed": removed,
            })))
        },
        Err(PairingError::AppsOwnerRequired) => Ok(HttpResponse::Forbidden().json(json!({
            "error": "android_apps_owner_channel_required",
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "unpair_failed",
            "detail": error.to_string(),
        }))),
    }
}

/// `GET /api/magician/v2/devices/policy`
///
/// The scope-wide device policy. The per-app protected list lives on the
/// phone, owned there; this is the Magician-side switch above it.
pub async fn get_device_policy_handler(
    policy: web::Data<Arc<DevicePolicyStore>>,
) -> Result<HttpResponse> {
    Ok(HttpResponse::Ok().json(json!({
        "screenshot_policy": policy.screenshot_policy().await,
        // Secure HITL P6: paired devices permitted as verification-code sources.
        "verification_code_devices": policy.verification_code_devices().await,
    })))
}

#[derive(serde::Deserialize)]
pub struct PutDevicePolicyRequest {
    pub screenshot_policy: ScreenshotPolicy,
}

/// `PUT /api/magician/v2/devices/policy`
pub async fn put_device_policy_handler(
    policy: web::Data<Arc<DevicePolicyStore>>,
    body: web::Json<PutDevicePolicyRequest>,
) -> Result<HttpResponse> {
    match policy.set_screenshot_policy(body.screenshot_policy).await {
        Ok(applied) => Ok(HttpResponse::Ok().json(json!({
            "screenshot_policy": applied,
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "policy_persist_failed",
            "detail": error.to_string(),
        }))),
    }
}

#[derive(serde::Deserialize)]
pub struct DeviceAuditQuery {
    pub limit: Option<usize>,
}

/// `GET /api/magician/v2/devices/audit?limit=`
///
/// The reviewable trail: what was done on the phone, to which app, newest
/// first. Loss classes are surfaced, never hidden — `corrupt_lines` is
/// committed damage, `torn_tail_bytes` is an interrupted append that never
/// committed.
pub async fn device_audit_handler(
    req: HttpRequest,
    audit: web::Data<Arc<DeviceActionAudit>>,
    query: web::Query<DeviceAuditQuery>,
) -> Result<HttpResponse> {
    let (principal, workspace) = match scope(&req) {
        Ok(scope) => scope,
        Err(response) => return Ok(response),
    };
    let limit = query.limit.unwrap_or(100).clamp(1, 1000);
    match audit
        .read_recent_for_scope(&principal, &workspace, limit)
        .await
    {
        Ok(read) => Ok(HttpResponse::Ok().json(json!({
            "principal": principal,
            "workspace": workspace,
            "records": read.records,
            // Deliberately deployment-global, and named so: a corrupt line
            // has no readable scope to attribute it to, and integrity damage
            // hidden from whoever happens to ask is worse than shared.
            "global_corrupt_lines": read.corrupt_lines,
            "global_torn_tail_bytes": read.torn_tail_bytes,
        }))),
        Err(error) => Ok(HttpResponse::InternalServerError().json(json!({
            "error": "audit_read_failed",
            "detail": error.to_string(),
        }))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[actix_web::test]
    async fn unavailable_roster_reports_pairing_readiness_without_leaking_owner_error() {
        let temp = tempfile::tempdir().unwrap();
        let response = list_devices_handler(
            actix_web::test::TestRequest::default()
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request(),
            web::Data::new(Arc::new(DevicePairingStore::unavailable(
                temp.path(),
                "private owner diagnostic",
            ))),
            web::Data::new(
                MobileEnrollmentConfig::resolve_routes(
                    Some("https://connect.magican.ai"),
                    Some("http://192.168.1.10:3002"),
                )
                .unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), actix_web::http::StatusCode::OK);
        let bytes = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["pairing_available"], false);
        assert_eq!(body["devices"], json!([]));
        assert!(!String::from_utf8_lossy(&bytes).contains("private owner diagnostic"));
    }

    #[actix_web::test]
    async fn unavailable_durable_owner_refuses_enrollment_before_minting() {
        let temp = tempfile::tempdir().unwrap();
        let pairing = web::Data::new(Arc::new(DevicePairingStore::unavailable(
            temp.path(),
            "keychain unavailable",
        )));
        let response = begin_device_enrollment_handler(
            actix_web::test::TestRequest::default()
                .insert_header(("X-Principal", "anonymous"))
                .insert_header(("X-Workspace", "default"))
                .to_http_request(),
            web::Json(BeginEnrollmentRequest {
                client_kind: MobileClientKind::Ios,
                connection_mode: EnrollmentConnectionMode::Remote,
                _legacy_public_origin: None,
            }),
            pairing,
            web::Data::new(
                MobileEnrollmentConfig::resolve(Some("https://mobile.example")).unwrap(),
            ),
        )
        .await
        .unwrap();
        assert_eq!(
            response.status(),
            actix_web::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }

    #[actix_web::test]
    async fn direct_esp_pairing_without_verified_bootstrap_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let pairing = web::Data::new(Arc::new(DevicePairingStore::unavailable(
            temp.path(),
            "unused because authorization fails first",
        )));
        let response = pair_device_handler(
            actix_web::test::TestRequest::default().to_http_request(),
            web::Json(PairRequest {
                device_id: "esp-review".to_string(),
                label: None,
            }),
            pairing,
            web::Data::new(Arc::new(DeviceBridgeHub::new())),
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(response.status(), actix_web::http::StatusCode::UNAUTHORIZED);
    }

    #[test]
    fn enrollment_origin_accepts_only_an_http_origin() {
        assert_eq!(
            normalized_public_origin("https://magican.example/").unwrap(),
            "https://magican.example"
        );
        assert_eq!(
            normalized_public_origin("http://127.0.0.1:3000").unwrap(),
            "http://127.0.0.1:3000"
        );
        for invalid in [
            "javascript:alert(1)",
            "http://magican.example",
            "https://user:secret@magican.example",
            "https://magican.example/a/path",
            "https://magican.example?redirect=elsewhere",
        ] {
            assert!(normalized_public_origin(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn same_wifi_origin_accepts_only_private_network_hosts() {
        for valid in [
            "http://192.168.1.20:3002/",
            "http://10.0.0.7:3002",
            "http://172.20.0.4:3002",
            "http://[fd00::7]:3002",
        ] {
            assert!(normalized_local_origin(valid).is_ok(), "{valid}");
        }
        for invalid in [
            "http://connect.magican.ai:3002",
            "https://connect.magican.ai",
            "http://192.168.1.20:3002/path",
            "http://user:secret@192.168.1.20:3002",
        ] {
            assert!(normalized_local_origin(invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn enrollment_runtime_may_be_deliberately_unconfigured() {
        assert_eq!(
            MobileEnrollmentConfig::resolve(Some("https://mobile.example/"))
                .unwrap()
                .public_origin
                .as_deref(),
            Some("https://mobile.example")
        );
        assert!(MobileEnrollmentConfig::resolve(None)
            .unwrap()
            .public_origin
            .is_none());
        assert_eq!(
            MobileEnrollmentConfig::resolve_routes(
                Some("https://connect.magican.ai"),
                Some("http://192.168.1.20:3002/"),
            )
            .unwrap()
            .local_origin
            .as_deref(),
            Some("http://192.168.1.20:3002")
        );
    }

    #[test]
    fn enrollment_uri_roundtrips_reserved_characters_without_exposing_fields() {
        let uri = enrollment_uri(
            "https://magican.example:8443",
            "ticket/id",
            "secret+value=",
            MobileClientKind::Ios,
        );
        let parsed = url::Url::parse(&uri).unwrap();
        assert_eq!(parsed.scheme(), "magican");
        assert_eq!(parsed.host_str(), Some("connect"));
        let values = parsed
            .query_pairs()
            .collect::<std::collections::HashMap<_, _>>();
        assert_eq!(
            values.get("base").map(|value| value.as_ref()),
            Some("https://magican.example:8443")
        );
        assert_eq!(
            values.get("id").map(|value| value.as_ref()),
            Some("ticket/id")
        );
        assert_eq!(
            values.get("secret").map(|value| value.as_ref()),
            Some("secret+value=")
        );
        assert_eq!(values.get("kind").map(|value| value.as_ref()), Some("ios"));
    }

    #[test]
    fn enrollment_qr_is_rendered_locally_as_svg() {
        let svg =
            qr_svg("magican://connect?base=https%3A%2F%2Fmagican.example&id=a&secret=b&kind=ios")
                .unwrap();
        assert!(svg.starts_with("<?xml"));
        assert!(svg.contains("<svg"));
        assert!(!svg.contains("secret=b"));
    }
}

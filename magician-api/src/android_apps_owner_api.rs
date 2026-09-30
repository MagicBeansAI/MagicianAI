//! Trusted owner review channel for Android Apps automation.
//!
//! General V2 device routes and caller-supplied loopback/browser headers are
//! compatibility/mobile surfaces, never an authority boundary. Production
//! enrollment and review mutations are mounted only below the signed native
//! owner surface. First trust is established exclusively through the private,
//! reciprocal code-identity-verified desktop Unix socket; no HTTP bootstrap
//! handler is mounted.

use std::{
    collections::{BTreeSet, HashMap},
    sync::{Arc, OnceLock},
    time::Duration,
};

use actix_web::{http::StatusCode, web, HttpResponse};
use base64::Engine as _;
use chrono::Utc;
use qrcode::{render::svg, QrCode};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};

use magician::magician_v2::apps::android_owner::{
    AppAndroidOwnerStore, AppAndroidPendingOwnerEnrollment,
};
use magician::magician_v2::apps::android_owner_bootstrap::revalidate_owner_status;
use magician::magician_v2::artifact_v2::workspace::{
    DEFAULT_SCOPE_PRINCIPAL, DEFAULT_SCOPE_WORKSPACE,
};
use magician::magician_v2::device_pairing::{
    prospective_attested_automation_identity, DevicePairingStore, PairedAutomationTarget,
    PairingError,
};
use magician_app_contract::android_owner::{
    app_android_owner_native_body_digest, AppAndroidAttestationSecurityLevel,
    AppAndroidAutomationTrustMode, AppAndroidEnrollmentConnectionMode,
    AppAndroidOwnerNativeBeginEnrollmentRequest, AppAndroidOwnerNativeCancelEnrollmentRequest,
    AppAndroidOwnerNativeCancelEnrollmentResponse, AppAndroidOwnerNativeControlStatus,
    AppAndroidOwnerNativeEmptyRequest, AppAndroidOwnerNativeEnrollmentResponse,
    AppAndroidOwnerNativeEnvelope, AppAndroidOwnerNativeOperation,
    AppAndroidOwnerNativePendingResponse, AppAndroidOwnerNativeProposeReviewRequest,
    AppAndroidOwnerNativeTarget, AppAndroidOwnerNativeTargetsResponse, AppAndroidOwnerOperation,
    AppAndroidOwnerProposal, AppAndroidOwnerReceipt, AppAndroidOwnerRecoverySnapshot,
    APP_ANDROID_OWNER_ACTION_ROSTER, APP_ANDROID_OWNER_MAX_PACKAGES,
    APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS,
};

use crate::{
    android_apps_attestation::{
        android_apps_enrollment_signing_bytes, verify_android_apps_enrollment,
        AndroidAppsAttestationError, AndroidAppsEnrollmentEvidence,
    },
    android_automation_trust::AndroidAutomationTrustPolicy,
    android_play_integrity::{
        play_integrity_request_hash, verify_play_integrity_token, AndroidPlayIntegrityError,
    },
    device_pairing_api::MobileEnrollmentConfig,
};

const MAX_PACKAGE_BYTES: usize = 255;
const APPS_ENROLLMENT_TTL_MS: i64 = 5 * 60 * 1_000;
const MAX_PENDING_APPS_ENROLLMENTS: usize = 64;
const MAX_APPS_ENROLLMENT_ATTEMPTS: u8 = 5;
const NATIVE_OWNER_JSON_LIMIT: usize = 2_304 * 1024;
const HANDSET_EXCHANGE_JSON_LIMIT: usize = 256 * 1024;
const MAX_CONCURRENT_ATTESTATION_VERIFICATIONS: usize = 2;
const ATTESTATION_VERIFICATION_DEADLINE: Duration = Duration::from_secs(30);

#[derive(Clone)]
struct PendingAppsEnrollment {
    principal: String,
    workspace: String,
    public_origin: String,
    secret_digest: [u8; 32],
    challenge: [u8; 32],
    expires_at_ms: i64,
    attempts: u8,
    consuming: bool,
    trust_policy: AndroidAutomationTrustPolicy,
}

#[derive(Default)]
struct PendingAppsEnrollments {
    entries: HashMap<String, PendingAppsEnrollment>,
}

fn pending_apps_enrollments() -> &'static tokio::sync::Mutex<PendingAppsEnrollments> {
    static STORE: OnceLock<tokio::sync::Mutex<PendingAppsEnrollments>> = OnceLock::new();
    STORE.get_or_init(|| tokio::sync::Mutex::new(PendingAppsEnrollments::default()))
}

fn attestation_verification_capacity() -> &'static Arc<tokio::sync::Semaphore> {
    static CAPACITY: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    CAPACITY.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            MAX_CONCURRENT_ATTESTATION_VERIFICATIONS,
        ))
    })
}

#[derive(Default)]
struct PendingOwnerProposals {
    proposal: Option<AppAndroidOwnerProposal>,
}

fn pending_owner_proposals() -> &'static tokio::sync::Mutex<PendingOwnerProposals> {
    static STORE: OnceLock<tokio::sync::Mutex<PendingOwnerProposals>> = OnceLock::new();
    STORE.get_or_init(|| tokio::sync::Mutex::new(PendingOwnerProposals::default()))
}

/// Serializes the two proposal producers (attested enrollment and snapshot
/// review) across their final predecessor read and durable/in-memory publish.
fn owner_proposal_creation_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExchangeAndroidAppsEnrollmentRequest {
    enrollment_id: String,
    secret: String,
    device_id: String,
    label: String,
    key_id: String,
    public_key_spki_base64: String,
    certificate_chain_base64: Vec<String>,
    app_package: String,
    app_version_code: u64,
    app_signing_sha256: String,
    apk_sha256: String,
    connection_secret_sha256: String,
    signature_base64: String,
    play_integrity_token: String,
}

fn error(status: StatusCode, code: &'static str) -> HttpResponse {
    HttpResponse::build(status)
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(serde_json::json!({ "error": code }))
}

fn valid_package(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PACKAGE_BYTES
        && value.split('.').count() >= 2
        && value.split('.').all(|component| {
            component
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphabetic)
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn random_token(bytes: usize) -> String {
    let mut value = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut value);
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value)
}

fn apps_exchange_request_digest(
    request: &ExchangeAndroidAppsEnrollmentRequest,
) -> Result<String, HttpResponse> {
    let mut value = serde_json::to_value(request)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid"))?;
    // Provider tokens are one-time transport evidence. Exclude the opaque
    // token bytes so the handset can refresh a verdict with the exact same
    // hardware-key-signed request after a proven provider rejection.
    value
        .as_object_mut()
        .ok_or_else(|| error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid"))?
        .remove("play_integrity_token");
    let encoded = serde_json::to_vec(&value)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid"))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-apps-enrollment-request.v2\0");
    hasher.update(&(encoded.len() as u64).to_le_bytes());
    hasher.update(&encoded);
    Ok(hasher.finalize().to_hex().to_string())
}

fn apps_enrollment_pending_response() -> HttpResponse {
    HttpResponse::Accepted()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .insert_header(("Retry-After", "2"))
        .json(serde_json::json!({ "status": "owner_approval_pending" }))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingEnrollmentResume {
    AwaitOwnerReceipt,
    PublishOrReconcile,
    InconsistentPublishedAuthority,
}

fn pending_enrollment_resume(
    approved: bool,
    pairing_is_published: bool,
) -> PendingEnrollmentResume {
    match (approved, pairing_is_published) {
        (false, false) => PendingEnrollmentResume::AwaitOwnerReceipt,
        (true, _) => PendingEnrollmentResume::PublishOrReconcile,
        (false, true) => PendingEnrollmentResume::InconsistentPublishedAuthority,
    }
}

fn apps_enrollment_active_response(
    target: &PairedAutomationTarget,
    public_origin: &str,
) -> HttpResponse {
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .insert_header(("Pragma", "no-cache"))
        .json(serde_json::json!({
            "status": "active",
            "principal": &target.principal,
            "workspace": &target.workspace,
            "public_origin": public_origin,
            "client_kind": "android",
            "capabilities": ["mobile_client", "device_automation"],
            "key_id": &target.key_id,
            "apk_sha256": &target.apk_sha256,
            "attestation_policy_digest": &target.attestation_policy_digest,
            "cloudflare_access": apps_cloudflare_bootstrap(),
        }))
}

async fn reconciled_apps_enrollment_gone(
    body: &ExchangeAndroidAppsEnrollmentRequest,
    pairing: &DevicePairingStore,
    owner_store: &AppAndroidOwnerStore,
    public_origin: &str,
) -> HttpResponse {
    // Receipt publication takes this same lock. A 410 may therefore be issued
    // only after proving there is neither an exact durable proposal nor an
    // already-published pairing at this linearization point.
    let _proposal_creation = owner_proposal_creation_lock().lock().await;
    match owner_store.pending_enrollment().await {
        Ok(Some(pending))
            if pending.proposal.enrollment_id.as_deref() == Some(&body.enrollment_id) =>
        {
            return apps_enrollment_pending_response();
        },
        Ok(Some(_)) => return error(StatusCode::CONFLICT, "android_owner_proposal_pending"),
        Ok(None) => {},
        Err(_) => return owner_store_error(),
    }
    match pairing
        .approved_automation_enrollment(
            &body.enrollment_id,
            body.device_id.trim(),
            &body.key_id,
            &body.connection_secret_sha256,
        )
        .await
    {
        Ok(Some(active)) => apps_enrollment_active_response(&active, public_origin),
        Ok(None) => error(StatusCode::GONE, "android_apps_enrollment_unavailable"),
        Err(_) => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "android_apps_pairing_failed",
        ),
    }
}

fn apps_enrollment_uri(
    origin: &str,
    enrollment_id: &str,
    secret: &str,
    challenge: &[u8; 32],
    trust_mode: AppAndroidAutomationTrustMode,
    play_project_number: Option<u64>,
) -> String {
    let mut uri = url::Url::parse("magican://apps-connect").expect("static Apps enrollment URI");
    uri.query_pairs_mut()
        .append_pair("base", origin)
        .append_pair("id", enrollment_id)
        .append_pair("secret", secret)
        .append_pair(
            "challenge",
            &base64::engine::general_purpose::STANDARD.encode(challenge),
        )
        .append_pair("trust", trust_mode.as_str());
    if let Some(project) = play_project_number {
        uri.query_pairs_mut()
            .append_pair("project", &project.to_string());
    }
    uri.to_string()
}

fn qr_svg(value: &str) -> Result<String, qrcode::types::QrError> {
    Ok(QrCode::new(value.as_bytes())?
        .render::<svg::Color>()
        .quiet_zone(true)
        .min_dimensions(288, 288)
        .dark_color(svg::Color("#17191f"))
        .light_color(svg::Color("#ffffff"))
        .build())
}

fn apps_cloudflare_bootstrap() -> Option<serde_json::Value> {
    let value = |name: &str| {
        std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    value("CF_ACCESS_CLIENT_ID")
        .zip(value("CF_ACCESS_CLIENT_SECRET"))
        .map(|(client_id, client_secret)| {
            serde_json::json!({
                "client_id": client_id,
                "client_secret": client_secret,
            })
        })
}

#[derive(Serialize)]
struct AndroidAutomationTrustOption {
    mode: AppAndroidAutomationTrustMode,
    label: &'static str,
    description: &'static str,
    recommended: bool,
    ready: bool,
    missing: Vec<&'static str>,
}

#[derive(Serialize)]
struct AndroidAutomationTrustOptionsResponse {
    options: Vec<AndroidAutomationTrustOption>,
}

pub async fn android_automation_trust_options_handler(
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> HttpResponse {
    let common_missing = || {
        let mut missing = Vec::new();
        if enrollment_config.public_origin.is_none() {
            missing.push("Remote Magician address (public_origin)");
        }
        if enrollment_config.android_apps_signing_sha256.is_empty() {
            missing.push("App signer (android_apps_signing_sha256)");
        }
        if enrollment_config.android_attestation_root_sha256.is_empty() {
            missing.push("Android trust root (android_attestation_root_sha256)");
        }
        if enrollment_config.android_apps_version_codes.is_empty() {
            missing.push("App version (android_apps_version_codes)");
        }
        missing
    };
    // A private build pins its signer and version at the owner's approval of
    // the first pairing, and verifies against Google's published attestation
    // roots when the config lists none; only the address is a prerequisite.
    let mut private_missing = Vec::new();
    if enrollment_config.public_origin.is_none() {
        private_missing.push("Remote Magician address (public_origin)");
    }
    let mut play_missing = common_missing();
    if enrollment_config
        .android_play_integrity_cloud_project_number
        .is_none_or(|value| value == 0)
    {
        play_missing.push("Google Cloud project (android_play_integrity_cloud_project_number)");
    }
    if enrollment_config
        .android_play_integrity_version_codes
        .is_empty()
    {
        play_missing.push("Play version (android_play_integrity_version_codes)");
    } else if enrollment_config
        .android_apps_version_codes
        .iter()
        .collect::<BTreeSet<_>>()
        != enrollment_config
            .android_play_integrity_version_codes
            .iter()
            .collect::<BTreeSet<_>>()
    {
        play_missing.push("Matching common and Google Play app versions");
    }
    if enrollment_config
        .android_play_integrity_required_device_verdicts
        .is_empty()
    {
        play_missing.push("Device verdicts (android_play_integrity_required_device_verdicts)");
    }
    if enrollment_config
        .android_play_integrity_service_account_path
        .is_none()
    {
        play_missing.push("Play service account (MAGICIAN_PLAY_INTEGRITY_SERVICE_ACCOUNT_PATH)");
    }
    let option = |mode, label, description, recommended, mut missing: Vec<&'static str>| {
        let policy_ready =
            AndroidAutomationTrustPolicy::reviewed(mode, enrollment_config.as_ref()).is_ok();
        if missing.is_empty() && !policy_ready {
            missing.push("Valid reviewed policy values");
        }
        let ready = missing.is_empty() && policy_ready;
        AndroidAutomationTrustOption {
            mode,
            label,
            description,
            recommended,
            ready,
            missing,
        }
    };
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .json(AndroidAutomationTrustOptionsResponse {
            options: vec![
                option(
                    AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
                    "Private / self-hosted build",
                    if enrollment_config.android_apps_signing_sha256.is_empty()
                        || enrollment_config.android_apps_version_codes.is_empty()
                    {
                        "For Magdroid installed directly by you. The phone's hardware attestation proves the app's signer and version; you approve that exact build on this computer at the first pairing, and it stays pinned for every reconnect. No Google Play, no config pins to copy between servers."
                    } else {
                        "For Magdroid installed directly by you. Uses exact signer, app-version and Android hardware-attestation pins without contacting Google Play."
                    },
                    true,
                    private_missing,
                ),
                option(
                    AppAndroidAutomationTrustMode::PlayIntegrity,
                    "Google Play release",
                    "For a Play-distributed build. Adds a fresh server-decoded Play Integrity verdict to the same hardware and owner checks.",
                    false,
                    play_missing,
                ),
            ],
        })
}

pub async fn exchange_android_apps_enrollment_handler(
    body: web::Json<ExchangeAndroidAppsEnrollmentRequest>,
    pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> HttpResponse {
    let body = body.into_inner();
    if body.enrollment_id.len() > 128
        || body.secret.len() > 256
        || body.device_id.is_empty()
        || body.device_id.len() > 256
        || body.label.is_empty()
        || body.label.len() > 128
        || body.certificate_chain_base64.len() > 8
        || body.connection_secret_sha256.len() != 64
        || !body
            .connection_secret_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid");
    }
    let request_digest = match apps_exchange_request_digest(&body) {
        Ok(value) => value,
        Err(response) => return response,
    };
    let Some(public_origin) = enrollment_config.public_origin.as_deref() else {
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "mobile_public_origin_not_configured",
        );
    };
    let active = match pairing
        .approved_automation_enrollment(
            &body.enrollment_id,
            body.device_id.trim(),
            &body.key_id,
            &body.connection_secret_sha256,
        )
        .await
    {
        Ok(value) => value,
        Err(_) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "android_apps_pairing_failed",
            )
        },
    };
    let deadline = tokio::time::Instant::now() + ATTESTATION_VERIFICATION_DEADLINE;
    let preflight_now_ms = Utc::now().timestamp_millis();
    let presented = *blake3::hash(body.secret.as_bytes()).as_bytes();
    let presented_hex = hex::encode(presented);
    match owner_store.pending_enrollment().await {
        Ok(Some(pending))
            if pending.proposal.enrollment_id.as_deref() == Some(&body.enrollment_id) =>
        {
            if pending.enrollment_secret_digest != presented_hex
                || pending.exchange_request_digest != request_digest
                || pending.connection_secret_sha256 != body.connection_secret_sha256
                || pending.device_id != body.device_id.trim()
                || pending.identity.key_id != body.key_id
            {
                return error(StatusCode::FORBIDDEN, "android_apps_enrollment_rejected");
            }
            let proposal_digest = match pending.proposal.digest() {
                Ok(value) => value,
                Err(_) => return owner_store_error(),
            };
            let approved = match owner_store
                .pending_enrollment_is_approved(&proposal_digest)
                .await
            {
                Ok(value) => value,
                Err(_) => return owner_store_error(),
            };
            let resume = pending_enrollment_resume(approved, active.is_some());
            if resume == PendingEnrollmentResume::InconsistentPublishedAuthority {
                return owner_store_error();
            }
            if resume == PendingEnrollmentResume::AwaitOwnerReceipt {
                if pending.proposal.expires_at_ms <= preflight_now_ms {
                    let _proposal_creation = owner_proposal_creation_lock().lock().await;
                    if revalidate_owner_status(owner_store.as_ref()).await.is_err()
                        || owner_store
                            .clear_expired_unapproved_enrollment(&proposal_digest, preflight_now_ms)
                            .await
                            .is_err()
                    {
                        return owner_store_error();
                    }
                    pending_apps_enrollments()
                        .lock()
                        .await
                        .entries
                        .remove(&body.enrollment_id);
                    drop(_proposal_creation);
                    return reconciled_apps_enrollment_gone(
                        &body,
                        pairing.as_ref(),
                        owner_store.as_ref(),
                        public_origin,
                    )
                    .await;
                }
                return apps_enrollment_pending_response();
            }
            let key = magician::magician_v2::device_bridge::DeviceKey::new(
                &pending.proposal.principal,
                &pending.proposal.workspace,
                &pending.device_id,
            );
            let target_ref = match pairing
                .pair_approved_attested_automation(
                    key,
                    pending.device_label.clone(),
                    pending.identity.clone(),
                    &pending.connection_secret_sha256,
                    preflight_now_ms,
                )
                .await
            {
                Ok(value) if value == pending.proposal.target_ref => value,
                _ => {
                    return error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "android_apps_pairing_failed",
                    )
                },
            };
            if owner_store
                .clear_pending_enrollment(&proposal_digest)
                .await
                .is_err()
            {
                return owner_store_error();
            }
            let Some(active) = pairing
                .list_all_automation_targets()
                .await
                .into_iter()
                .find(|target| target.target_ref == target_ref)
            else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "android_apps_pairing_failed",
                );
            };
            return apps_enrollment_active_response(&active, &pending.public_origin);
        },
        Ok(_) => {
            if let Some(active) = active.as_ref() {
                return apps_enrollment_active_response(active, public_origin);
            }
        },
        Err(_) => return owner_store_error(),
    }
    // A caller without the exact live QR ticket never reaches the scarce
    // blocking verifier. This is a read-only preflight; the exact same fields
    // are revalidated when the ticket is atomically marked consuming below.
    let preflight_gone = {
        let mut store = pending_apps_enrollments().lock().await;
        match store.entries.get(&body.enrollment_id) {
            None => true,
            Some(candidate) if candidate.expires_at_ms <= preflight_now_ms => {
                store.entries.remove(&body.enrollment_id);
                true
            },
            Some(candidate) => {
                if candidate.consuming
                    || !constant_time_eq(&candidate.secret_digest, &presented)
                    || candidate.attempts >= MAX_APPS_ENROLLMENT_ATTEMPTS
                {
                    return error(StatusCode::FORBIDDEN, "android_apps_enrollment_rejected");
                }
                false
            },
        }
    };
    if preflight_gone {
        return reconciled_apps_enrollment_gone(
            &body,
            pairing.as_ref(),
            owner_store.as_ref(),
            public_origin,
        )
        .await;
    }
    // Capacity is acquired before the one-time ticket enters `consuming`.
    // The owned permit moves into the blocking closure, so cancellation or a
    // handler deadline cannot release capacity while OpenSSL is still running.
    let capacity = match tokio::time::timeout_at(
        deadline,
        Arc::clone(attestation_verification_capacity()).acquire_owned(),
    )
    .await
    {
        Ok(Ok(capacity)) => capacity,
        Ok(Err(_)) => {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "android_apps_attestation_unavailable",
            );
        },
        Err(_) => {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "android_apps_attestation_busy",
            );
        },
    };
    let now_ms = Utc::now().timestamp_millis();
    let pending = {
        let mut store = pending_apps_enrollments().lock().await;
        let unavailable = store
            .entries
            .get(&body.enrollment_id)
            .is_none_or(|candidate| candidate.expires_at_ms <= now_ms);
        if unavailable {
            store.entries.remove(&body.enrollment_id);
            None
        } else {
            let candidate = store
                .entries
                .get_mut(&body.enrollment_id)
                .expect("checked live enrollment entry");
            if candidate.consuming
                || !constant_time_eq(&candidate.secret_digest, &presented)
                || candidate.attempts >= MAX_APPS_ENROLLMENT_ATTEMPTS
            {
                return error(StatusCode::FORBIDDEN, "android_apps_enrollment_rejected");
            }
            candidate.attempts = candidate.attempts.saturating_add(1);
            candidate.consuming = true;
            Some(candidate.clone())
        }
    };
    let Some(pending) = pending else {
        drop(capacity);
        return reconciled_apps_enrollment_gone(
            &body,
            pairing.as_ref(),
            owner_store.as_ref(),
            public_origin,
        )
        .await;
    };
    let trust_policy = pending.trust_policy.clone();
    let attestation_policy = trust_policy.attestation().clone();
    let verification_body = body.clone();
    let verification_challenge = pending.challenge;
    let consumption = AppsEnrollmentConsumptionGuard::new(body.enrollment_id.clone());
    let (result_sender, result_receiver) = tokio::sync::oneshot::channel();
    // The supervisor is deliberately detached. If the HTTP future is dropped
    // or times out, it waits for the retained blocking owner and only then
    // reopens the ticket for an exact retry.
    let _supervisor = tokio::spawn(async move {
        let verification = tokio::task::spawn_blocking(move || {
            let _capacity = capacity;
            verify_android_apps_enrollment(
                &attestation_policy,
                AndroidAppsEnrollmentEvidence {
                    enrollment_id: &verification_body.enrollment_id,
                    device_id: &verification_body.device_id,
                    label: &verification_body.label,
                    key_id: &verification_body.key_id,
                    public_key_spki_base64: &verification_body.public_key_spki_base64,
                    certificate_chain_base64: &verification_body.certificate_chain_base64,
                    app_package: &verification_body.app_package,
                    app_version_code: verification_body.app_version_code,
                    app_signing_sha256: &verification_body.app_signing_sha256,
                    apk_sha256: &verification_body.apk_sha256,
                    connection_secret_sha256: &verification_body.connection_secret_sha256,
                    signature_base64: &verification_body.signature_base64,
                },
                &verification_challenge,
                now_ms,
            )
        })
        .await;
        // The guard travels with the result. If the receiver disappears after
        // this send is buffered but before it polls, dropping the oneshot value
        // still reopens the exact ticket. A send failure does the same.
        let _ = result_sender.send((verification, consumption));
    });
    let (verification, mut consumption) =
        match tokio::time::timeout_at(deadline, result_receiver).await {
            Ok(Ok(value)) => value,
            Ok(Err(_)) => {
                release_apps_enrollment(&body.enrollment_id).await;
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "android_apps_attestation_unavailable",
                );
            },
            Err(_) => {
                // The detached supervisor still owns both the ticket state and
                // blocking capacity. It reopens the ticket only after work ends.
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "android_apps_attestation_timed_out",
                );
            },
        };
    // From this point until the durable owner proposal is staged, dropping the
    // HTTP future drops the guard received with the verifier result and reopens
    // the one-time QR across Play verification and every later await.
    let identity = match verification {
        Ok(Ok(identity)) => identity,
        Err(_) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "android_apps_attestation_unavailable",
            );
        },
        Ok(Err(error_value)) => {
            release_apps_enrollment(&body.enrollment_id).await;
            // Content-free: the error class, the policy shape, and what the
            // phone declared — enough to answer "why was my build refused?"
            // from the service log, which before said nothing at all.
            tracing::warn!(
                error = ?error_value,
                trust_mode = ?trust_policy.mode(),
                learning_build = trust_policy.learns_build(),
                app_package = %body.app_package,
                app_version_code = body.app_version_code,
                chain_certificates = body.certificate_chain_base64.len(),
                "[ANDROID-ATTESTATION] Apps enrollment rejected"
            );
            let status = match error_value {
                AndroidAppsAttestationError::PolicyUnavailable => StatusCode::SERVICE_UNAVAILABLE,
                AndroidAppsAttestationError::Malformed => StatusCode::BAD_REQUEST,
                AndroidAppsAttestationError::Untrusted
                | AndroidAppsAttestationError::InvalidProof => StatusCode::FORBIDDEN,
            };
            return error(status, "android_apps_attestation_rejected");
        },
    };
    if let Some(play_policy) = trust_policy.play_integrity() {
        let signature =
            match base64::engine::general_purpose::STANDARD.decode(&body.signature_base64) {
                Ok(value) if value.len() <= 256 => value,
                _ => {
                    release_apps_enrollment(&body.enrollment_id).await;
                    return error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid");
                },
            };
        let play_request_hash = play_integrity_request_hash(
            b"magician.android-play-integrity.enrollment.v1",
            &android_apps_enrollment_signing_bytes(
                &AndroidAppsEnrollmentEvidence {
                    enrollment_id: &body.enrollment_id,
                    device_id: &body.device_id,
                    label: &body.label,
                    key_id: &body.key_id,
                    public_key_spki_base64: &body.public_key_spki_base64,
                    certificate_chain_base64: &body.certificate_chain_base64,
                    app_package: &body.app_package,
                    app_version_code: body.app_version_code,
                    app_signing_sha256: &body.app_signing_sha256,
                    apk_sha256: &body.apk_sha256,
                    connection_secret_sha256: &body.connection_secret_sha256,
                    signature_base64: &body.signature_base64,
                },
                &pending.challenge,
            ),
            &signature,
        );
        let play_verification = tokio::time::timeout_at(
            deadline,
            verify_play_integrity_token(
                play_policy,
                &body.play_integrity_token,
                &play_request_hash,
                Utc::now().timestamp_millis(),
            ),
        )
        .await;
        match play_verification {
            Ok(Ok(_)) => {},
            Ok(Err(
                AndroidPlayIntegrityError::Malformed | AndroidPlayIntegrityError::Untrusted,
            )) => {
                release_apps_enrollment(&body.enrollment_id).await;
                return error(StatusCode::FORBIDDEN, "android_play_integrity_rejected");
            },
            Ok(Err(
                AndroidPlayIntegrityError::Unavailable | AndroidPlayIntegrityError::Transport,
            ))
            | Err(_) => {
                release_apps_enrollment(&body.enrollment_id).await;
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "android_play_integrity_unavailable",
                );
            },
        }
    } else if !body.play_integrity_token.is_empty() {
        release_apps_enrollment(&body.enrollment_id).await;
        return error(
            StatusCode::BAD_REQUEST,
            "android_private_build_token_unexpected",
        );
    }
    let key = magician::magician_v2::device_bridge::DeviceKey::new(
        &pending.principal,
        &pending.workspace,
        body.device_id.trim(),
    );
    let (target_ref, automation_identity_digest) = match prospective_attested_automation_identity(
        &key,
        &body.connection_secret_sha256,
        &identity,
    ) {
        Ok(value) => value,
        Err(_) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid");
        },
    };
    let _proposal_creation = owner_proposal_creation_lock().lock().await;
    // Re-check the durable enrollment slot under the shared creation lock;
    // a review proposal that won the race must not share this predecessor.
    match owner_store.pending_enrollment().await {
        Ok(None) => {},
        Ok(Some(_)) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return error(StatusCode::CONFLICT, "android_owner_proposal_pending");
        },
        Err(_) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return owner_store_error();
        },
    }
    let (owner_generation, previous_receipt_digest, binding) = match owner_store.owner_head().await
    {
        Ok(value) => value,
        Err(_) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return owner_store_error();
        },
    };
    let Some(next_owner_generation) = owner_generation.checked_add(1) else {
        release_apps_enrollment(&body.enrollment_id).await;
        return owner_store_error();
    };
    let security = match identity.attestation_security_level.as_str() {
        "tee" => AppAndroidAttestationSecurityLevel::Tee,
        "strongbox" => AppAndroidAttestationSecurityLevel::Strongbox,
        _ => {
            release_apps_enrollment(&body.enrollment_id).await;
            return error(StatusCode::FORBIDDEN, "android_apps_attestation_rejected");
        },
    };
    let proposal = match AppAndroidOwnerProposal::mint(
        random_token(18),
        next_owner_generation,
        previous_receipt_digest,
        binding.desktop_identity_digest,
        pending.principal.clone(),
        pending.workspace.clone(),
        AppAndroidOwnerOperation::EnrollAttestedDevice,
        Some(body.enrollment_id.clone()),
        target_ref,
        body.label.trim().to_owned(),
        identity.key_id.clone(),
        automation_identity_digest,
        identity.app_package.clone(),
        identity.app_version_code,
        identity.app_signing_sha256.clone(),
        identity.apk_sha256.clone(),
        identity.attestation_root_sha256.clone(),
        security,
        identity.attestation_policy_digest.clone(),
        0,
        0,
        Vec::new(),
        Vec::new(),
        now_ms,
        pending
            .expires_at_ms
            .min(now_ms.saturating_add(APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS)),
    ) {
        Ok(value) => value,
        Err(_) => {
            release_apps_enrollment(&body.enrollment_id).await;
            return error(StatusCode::BAD_REQUEST, "android_apps_enrollment_invalid");
        },
    };
    {
        let proposals = pending_owner_proposals().lock().await;
        if proposals
            .proposal
            .as_ref()
            .is_some_and(|current| current.expires_at_ms > now_ms)
        {
            drop(proposals);
            release_apps_enrollment(&body.enrollment_id).await;
            return error(StatusCode::CONFLICT, "android_owner_proposal_pending");
        }
    }
    let staged = AppAndroidPendingOwnerEnrollment {
        proposal,
        device_id: body.device_id.trim().to_owned(),
        device_label: body.label.trim().to_owned(),
        connection_secret_sha256: body.connection_secret_sha256.to_ascii_lowercase(),
        enrollment_secret_digest: presented_hex,
        exchange_request_digest: request_digest,
        public_origin: pending.public_origin,
        identity,
    };
    if owner_store
        .stage_pending_enrollment(staged, now_ms)
        .await
        .is_err()
    {
        release_apps_enrollment(&body.enrollment_id).await;
        return owner_store_error();
    }
    consumption.mark_durably_staged();
    release_apps_enrollment(&body.enrollment_id).await;
    apps_enrollment_pending_response()
}

/// Drop-owned recovery for a QR entry that has been marked `consuming` but has
/// not yet produced a durable owner proposal. Once staging succeeds, retry is
/// driven by that durable proposal and the in-memory ticket is no longer an
/// authority boundary.
struct AppsEnrollmentConsumptionGuard {
    enrollment_id: Option<String>,
}

impl AppsEnrollmentConsumptionGuard {
    fn new(enrollment_id: String) -> Self {
        Self {
            enrollment_id: Some(enrollment_id),
        }
    }

    fn mark_durably_staged(&mut self) {
        self.enrollment_id = None;
    }
}

impl Drop for AppsEnrollmentConsumptionGuard {
    fn drop(&mut self) {
        let Some(enrollment_id) = self.enrollment_id.take() else {
            return;
        };
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                release_apps_enrollment(&enrollment_id).await;
            });
        }
    }
}

async fn release_apps_enrollment(enrollment_id: &str) {
    if let Some(entry) = pending_apps_enrollments()
        .lock()
        .await
        .entries
        .get_mut(enrollment_id)
    {
        entry.consuming = false;
    }
}

// The caller-header/loopback review prototype is intentionally kept out of
// every build. It remains only as local design archaeology until the next
// mechanical cleanup; production has no symbol that can be mounted by mistake.
#[cfg(any())]
mod removed_ambient_owner_prototype {
    use super::*;

    fn confirmation_digest(
        proposal_id: &str,
        authorization_digest: &[u8; 32],
        owner: &TrustedOwnerScope,
        target_ref: &str,
        expected_generation: u64,
        operation: ReviewOperation,
        packages: &[String],
        expires_at_ms: i64,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"magician.android-owner-review-capability.v1\0");
        for component in [
            proposal_id.as_bytes(),
            authorization_digest,
            owner.principal.as_bytes(),
            owner.workspace.as_bytes(),
            owner.actor_fingerprint.as_bytes(),
            owner.session_fingerprint.as_bytes(),
            &owner.authentication_revision.to_le_bytes(),
            target_ref.as_bytes(),
            &expected_generation.to_le_bytes(),
            match operation {
                ReviewOperation::ApproveSnapshot => b"approve_snapshot".as_slice(),
                ReviewOperation::RevokeSnapshot => b"revoke_snapshot".as_slice(),
            },
            &expires_at_ms.to_le_bytes(),
        ] {
            hasher.update(&(component.len() as u64).to_le_bytes());
            hasher.update(component);
        }
        for package in packages {
            hasher.update(&(package.len() as u64).to_le_bytes());
            hasher.update(package.as_bytes());
        }
        hasher.finalize().to_hex().to_string()
    }

    pub async fn list_android_automation_targets_handler(
        req: HttpRequest,
        pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
    ) -> HttpResponse {
        let owner = match trusted_owner(&req) {
            Ok(owner) => owner,
            Err(response) => return response,
        };
        let targets: Vec<PairedAutomationTarget> = pairing
            .list_automation_targets(&owner.principal, &owner.workspace)
            .await;
        HttpResponse::Ok()
            .insert_header(("Cache-Control", "private, no-store"))
            .json(serde_json::json!({
                "principal": owner.principal,
                "workspace": owner.workspace,
                "targets": targets,
            }))
    }

    pub async fn propose_android_review_handler(
        req: HttpRequest,
        body: web::Json<ProposeAndroidReviewRequest>,
        pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
    ) -> HttpResponse {
        let owner = match trusted_owner(&req) {
            Ok(owner) => owner,
            Err(response) => return response,
        };
        let packages = match normalize_packages(body.operation, &body.allowed_packages) {
            Ok(packages) => packages,
            Err(response) => return response,
        };
        let target = pairing
            .list_automation_targets(&owner.principal, &owner.workspace)
            .await
            .into_iter()
            .find(|target| {
                target.target_ref == body.target_ref
                    && target.review_generation == body.expected_generation
                    && match body.operation {
                        ReviewOperation::ApproveSnapshot => true,
                        ReviewOperation::RevokeSnapshot => target.review.is_some(),
                    }
            });
        let Some(target) = target else {
            return error(
                StatusCode::CONFLICT,
                "android_review_target_or_generation_changed",
            );
        };

        let now_ms = Utc::now().timestamp_millis();
        let expires_at_ms = now_ms.saturating_add(REVIEW_CAPABILITY_TTL_MS);
        let proposal_id = random_token(18);
        let authorization = random_token(32);
        let authorization_digest = *blake3::hash(authorization.as_bytes()).as_bytes();
        let digest = confirmation_digest(
            &proposal_id,
            &authorization_digest,
            &owner,
            &target.target_ref,
            body.expected_generation,
            body.operation,
            &packages,
            expires_at_ms,
        );
        let pending = PendingReviewCapability {
            authorization_digest,
            confirmation_digest: digest.clone(),
            owner,
            target_ref: target.target_ref.clone(),
            expected_generation: body.expected_generation,
            operation: body.operation,
            allowed_packages: packages.clone(),
            expires_at_ms,
        };
        let mut store = match pending_capabilities().lock() {
            Ok(store) => store,
            Err(_) => {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "android_review_store_unavailable",
                )
            },
        };
        store
            .entries
            .retain(|_, entry| entry.expires_at_ms > now_ms);
        if store.entries.len() >= MAX_PENDING_REVIEW_CAPABILITIES {
            return error(StatusCode::TOO_MANY_REQUESTS, "android_review_store_full");
        }
        if store.entries.contains_key(&proposal_id) {
            return error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "android_review_capability_collision",
            );
        }
        store.entries.insert(proposal_id.clone(), pending);
        drop(store);

        HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .insert_header(("Pragma", "no-cache"))
            .json(AndroidReviewProposalResponse {
                proposal_id,
                authorization,
                confirmation_digest: digest,
                target_ref: target.target_ref,
                label: target.label,
                expected_generation: body.expected_generation,
                operation: body.operation,
                allowed_packages: packages,
                expires_at_ms,
            })
    }

    pub async fn commit_android_review_handler(
        req: HttpRequest,
        body: web::Json<CommitAndroidReviewRequest>,
        pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
    ) -> HttpResponse {
        let owner = match trusted_owner(&req) {
            Ok(owner) => owner,
            Err(response) => return response,
        };
        if body.proposal_id.len() > 128 || body.authorization.len() > 256 {
            return error(StatusCode::BAD_REQUEST, "android_review_capability_invalid");
        }
        let now_ms = Utc::now().timestamp_millis();
        let presented = *blake3::hash(body.authorization.as_bytes()).as_bytes();
        let pending = {
            let mut store = match pending_capabilities().lock() {
                Ok(store) => store,
                Err(_) => {
                    return error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "android_review_store_unavailable",
                    )
                },
            };
            let Some(candidate) = store.entries.get(&body.proposal_id) else {
                return error(StatusCode::GONE, "android_review_capability_unavailable");
            };
            let valid = candidate.expires_at_ms > now_ms
                && owner_matches(&candidate.owner, &owner)
                && constant_time_eq(&candidate.authorization_digest, &presented);
            if !valid {
                if candidate.expires_at_ms <= now_ms {
                    store.entries.remove(&body.proposal_id);
                }
                return error(StatusCode::FORBIDDEN, "android_review_capability_rejected");
            }
            store
                .entries
                .remove(&body.proposal_id)
                .expect("validated pending review remains under the same lock")
        };

        match pending.operation {
            ReviewOperation::ApproveSnapshot => pairing
                .review_automation_snapshot_target(
                    &owner.principal,
                    &owner.workspace,
                    &pending.target_ref,
                    pending.expected_generation,
                    pending.allowed_packages,
                    now_ms,
                )
                .await
                .map(|review| review_response(pending.confirmation_digest, Some(review), None)),
            ReviewOperation::RevokeSnapshot => pairing
                .revoke_automation_review_target(
                    &owner.principal,
                    &owner.workspace,
                    &pending.target_ref,
                    pending.expected_generation,
                )
                .await
                .map(|generation| {
                    review_response(pending.confirmation_digest, None, Some(generation))
                }),
        }
        .unwrap_or_else(pairing_error_response)
    }

    fn review_response(
        confirmation_digest: String,
        review: Option<DeviceAutomationReview>,
        generation: Option<u64>,
    ) -> HttpResponse {
        HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(serde_json::json!({
                "confirmation_digest": confirmation_digest,
                "review": review,
                "review_generation": generation,
            }))
    }
} // removed_ambient_owner_prototype

fn pairing_error_response(error_value: PairingError) -> HttpResponse {
    match error_value {
        PairingError::AutomationReviewConflict => {
            error(StatusCode::CONFLICT, "android_review_generation_changed")
        },
        PairingError::InvalidAutomationReview => {
            error(StatusCode::BAD_REQUEST, "android_review_invalid")
        },
        PairingError::NotPaired(_) => error(StatusCode::NOT_FOUND, "android_review_target_absent"),
        _ => error(
            StatusCode::SERVICE_UNAVAILABLE,
            "android_review_persist_failed",
        ),
    }
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    for index in 0..left.len().max(right.len()) {
        difference |= usize::from(
            left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0),
        );
    }
    difference == 0
}

fn owner_store_error() -> HttpResponse {
    error(
        StatusCode::SERVICE_UNAVAILABLE,
        "android_apps_owner_unavailable",
    )
}

async fn authorize_native_empty(
    owner_store: &AppAndroidOwnerStore,
    request: &AppAndroidOwnerNativeEmptyRequest,
    operation: AppAndroidOwnerNativeOperation,
) -> Result<i64, HttpResponse> {
    let now_ms = Utc::now().timestamp_millis();
    owner_store
        .authorize_native(&request.authorization, operation, None, now_ms)
        .await
        .map_err(|_| {
            error(
                StatusCode::FORBIDDEN,
                "android_apps_owner_authorization_rejected",
            )
        })?;
    Ok(now_ms)
}

async fn authorize_native_body<T: Serialize>(
    owner_store: &AppAndroidOwnerStore,
    request: &AppAndroidOwnerNativeEnvelope<T>,
    operation: AppAndroidOwnerNativeOperation,
) -> Result<i64, HttpResponse> {
    let body_digest = app_android_owner_native_body_digest(operation, &request.body)
        .map_err(|_| error(StatusCode::BAD_REQUEST, "android_apps_owner_body_invalid"))?;
    let now_ms = Utc::now().timestamp_millis();
    owner_store
        .authorize_native(
            &request.authorization,
            operation,
            Some(&body_digest),
            now_ms,
        )
        .await
        .map_err(|_| {
            error(
                StatusCode::FORBIDDEN,
                "android_apps_owner_authorization_rejected",
            )
        })?;
    Ok(now_ms)
}

pub async fn native_begin_android_apps_enrollment_handler(
    body: web::Json<AppAndroidOwnerNativeEnvelope<AppAndroidOwnerNativeBeginEnrollmentRequest>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
    enrollment_config: web::Data<MobileEnrollmentConfig>,
) -> HttpResponse {
    if let Err(response) = authorize_native_body(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::BeginEnrollment,
    )
    .await
    {
        return response;
    }
    let trust_policy = match AndroidAutomationTrustPolicy::reviewed(
        body.body.trust_mode,
        enrollment_config.as_ref(),
    ) {
        Ok(policy) => policy,
        Err(code) => return error(StatusCode::SERVICE_UNAVAILABLE, code),
    };
    match owner_store.pending_enrollment().await {
        Ok(Some(_)) => return error(StatusCode::CONFLICT, "android_owner_proposal_pending"),
        Ok(None) => {},
        Err(_) => return owner_store_error(),
    }
    let origin = match body.body.connection_mode {
        AppAndroidEnrollmentConnectionMode::SameWifi => {
            let Some(origin) = enrollment_config.local_origin.as_deref() else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "mobile_local_origin_not_available",
                );
            };
            origin
        },
        AppAndroidEnrollmentConnectionMode::Remote => {
            let Some(origin) = enrollment_config.public_origin.as_deref() else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "mobile_public_origin_not_configured",
                );
            };
            origin
        },
    };
    let now_ms = Utc::now().timestamp_millis();
    let expires_at_ms = now_ms.saturating_add(APPS_ENROLLMENT_TTL_MS);
    let enrollment_id = random_token(18);
    let secret = random_token(32);
    let mut challenge = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut challenge);
    let uri = apps_enrollment_uri(
        origin,
        &enrollment_id,
        &secret,
        &challenge,
        trust_policy.mode(),
        trust_policy.cloud_project_number(),
    );
    let svg = match qr_svg(&uri) {
        Ok(svg) => svg,
        Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "android_apps_qr_failed"),
    };
    let pending = PendingAppsEnrollment {
        principal: DEFAULT_SCOPE_PRINCIPAL.to_owned(),
        workspace: DEFAULT_SCOPE_WORKSPACE.to_owned(),
        public_origin: origin.to_owned(),
        secret_digest: *blake3::hash(secret.as_bytes()).as_bytes(),
        challenge,
        expires_at_ms,
        attempts: 0,
        consuming: false,
        trust_policy: trust_policy.clone(),
    };
    let mut enrollments = pending_apps_enrollments().lock().await;
    enrollments
        .entries
        .retain(|_, enrollment| enrollment.expires_at_ms > now_ms);
    if enrollments.entries.len() >= MAX_PENDING_APPS_ENROLLMENTS
        || enrollments.entries.contains_key(&enrollment_id)
    {
        return error(
            StatusCode::TOO_MANY_REQUESTS,
            "android_apps_enrollment_store_full",
        );
    }
    enrollments.entries.insert(enrollment_id.clone(), pending);
    drop(enrollments);
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .insert_header(("Pragma", "no-cache"))
        .json(AppAndroidOwnerNativeEnrollmentResponse {
            enrollment_id,
            enrollment_uri: uri,
            qr_svg: svg,
            expires_at_ms,
            challenge_base64: base64::engine::general_purpose::STANDARD.encode(challenge),
            principal: DEFAULT_SCOPE_PRINCIPAL.to_owned(),
            workspace: DEFAULT_SCOPE_WORKSPACE.to_owned(),
            trust_mode: trust_policy.mode(),
            connection_mode: body.body.connection_mode,
        })
}

pub async fn native_cancel_android_apps_enrollment_handler(
    body: web::Json<AppAndroidOwnerNativeEnvelope<AppAndroidOwnerNativeCancelEnrollmentRequest>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
) -> HttpResponse {
    if let Err(response) = authorize_native_body(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::CancelEnrollment,
    )
    .await
    {
        return response;
    }
    let enrollment_id = body.body.enrollment_id.clone();
    let staged = match owner_store.pending_enrollment().await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    let staged_cancelled = if staged.as_ref().is_some_and(|pending| {
        pending.proposal.enrollment_id.as_deref() == Some(enrollment_id.as_str())
    }) {
        if revalidate_owner_status(owner_store.as_ref()).await.is_err() {
            return owner_store_error();
        }
        match owner_store
            .cancel_unapproved_enrollment(&enrollment_id)
            .await
        {
            Ok(value) => value,
            Err(_) => return owner_store_error(),
        }
    } else {
        false
    };
    let qr_cancelled = pending_apps_enrollments()
        .lock()
        .await
        .entries
        .remove(&enrollment_id)
        .is_some();
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(AppAndroidOwnerNativeCancelEnrollmentResponse {
            cancelled: staged_cancelled || qr_cancelled,
        })
}

fn native_target(
    target: PairedAutomationTarget,
) -> Result<AppAndroidOwnerNativeTarget, HttpResponse> {
    let security = match target.attestation_security_level.as_str() {
        "tee" => AppAndroidAttestationSecurityLevel::Tee,
        "strongbox" => AppAndroidAttestationSecurityLevel::Strongbox,
        _ => return Err(owner_store_error()),
    };
    let (reviewed_actions, reviewed_packages) = target
        .review
        .as_ref()
        .map(|review| {
            (
                APP_ANDROID_OWNER_ACTION_ROSTER.to_vec(),
                review.allowed_packages.clone(),
            )
        })
        .unwrap_or_default();
    Ok(AppAndroidOwnerNativeTarget {
        principal: target.principal,
        workspace: target.workspace,
        target_ref: target.target_ref,
        device_label: target.label,
        automation_identity_digest: target.automation_identity_digest,
        owner_app_package: target.app_package,
        app_version_code: target.app_version_code,
        app_signing_sha256: target.app_signing_sha256,
        apk_sha256: target.apk_sha256,
        attestation_root_sha256: target.attestation_root_sha256,
        attestation_security_level: security,
        attestation_policy_digest: target.attestation_policy_digest,
        paired_at_ms: target.paired_at_ms,
        last_seen_ms: target.last_seen_ms,
        review_generation: target.review_generation,
        reviewed_actions,
        reviewed_packages,
    })
}

pub async fn native_list_android_targets_handler(
    body: web::Json<AppAndroidOwnerNativeEmptyRequest>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
    pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
) -> HttpResponse {
    if let Err(response) = authorize_native_empty(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::ListTargets,
    )
    .await
    {
        return response;
    }
    let mut targets = Vec::new();
    for target in pairing.list_all_automation_targets().await {
        match native_target(target) {
            Ok(target) => targets.push(target),
            Err(response) => return response,
        }
    }
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "private, no-store"))
        .json(AppAndroidOwnerNativeTargetsResponse { targets })
}

fn normalize_native_packages(
    operation: AppAndroidOwnerOperation,
    values: &[String],
) -> Result<Vec<String>, HttpResponse> {
    let mut packages = values
        .iter()
        .map(|value| value.trim().to_owned())
        .collect::<Vec<_>>();
    packages.sort();
    packages.dedup();
    if packages.is_empty()
        || packages.len() > APP_ANDROID_OWNER_MAX_PACKAGES
        || packages.iter().any(|package| !valid_package(package))
        || !matches!(
            operation,
            AppAndroidOwnerOperation::ApproveActions | AppAndroidOwnerOperation::RevokeActions
        )
    {
        return Err(error(
            StatusCode::BAD_REQUEST,
            "android_review_packages_invalid",
        ));
    }
    Ok(packages)
}

fn packages_include_owner_identity(packages: &[String], owner_app_package: &str) -> bool {
    packages.iter().any(|package| {
        package == owner_app_package
            || magician_app_contract::android_owner::is_android_owner_companion_package(package)
            || package == "com.android.systemui"
            || package.starts_with("com.android.launcher")
            || package == "com.google.android.apps.nexuslauncher"
    })
}

fn snapshot_receipt_matches_durable_target(
    receipt: &AppAndroidOwnerReceipt,
    target: &PairedAutomationTarget,
) -> bool {
    let security_level_matches = matches!(
        (
            receipt.attestation_security_level,
            target.attestation_security_level.as_str()
        ),
        (AppAndroidAttestationSecurityLevel::Tee, "tee")
            | (AppAndroidAttestationSecurityLevel::Strongbox, "strongbox")
    );
    let review_matches = match receipt.operation {
        AppAndroidOwnerOperation::ApproveActions => target.review.as_ref().is_some_and(|review| {
            review.generation == receipt.resulting_review_generation
                && review.actions.len() == APP_ANDROID_OWNER_ACTION_ROSTER.len()
                && review.allowed_packages == receipt.allowed_packages
        }),
        AppAndroidOwnerOperation::RevokeActions => target.review.is_none(),
        _ => false,
    };
    target.principal == receipt.principal
        && target.workspace == receipt.workspace
        && target.target_ref == receipt.target_ref
        && target.label == receipt.device_label
        && target.key_id == receipt.key_id
        && target.automation_identity_digest == receipt.automation_identity_digest
        && target.app_package == receipt.owner_app_package
        && target.app_version_code == receipt.app_version_code
        && target.app_signing_sha256 == receipt.app_signing_sha256
        && target.apk_sha256 == receipt.apk_sha256
        && target.attestation_root_sha256 == receipt.attestation_root_sha256
        && security_level_matches
        && target.attestation_policy_digest == receipt.attestation_policy_digest
        && target.review_generation == receipt.resulting_review_generation
        && review_matches
}

fn snapshot_transition_applied_recovery(
    owner_generation: u64,
    latest_receipt_digest: Option<&str>,
    receipt: &AppAndroidOwnerReceipt,
    target: Option<&PairedAutomationTarget>,
) -> bool {
    matches!(
        receipt.operation,
        AppAndroidOwnerOperation::ApproveActions | AppAndroidOwnerOperation::RevokeActions
    ) && owner_generation.checked_add(1) == Some(receipt.owner_generation)
        && receipt.previous_receipt_digest.as_deref() == latest_receipt_digest
        && target.is_some_and(|target| snapshot_receipt_matches_durable_target(receipt, target))
}

pub async fn native_propose_android_review_handler(
    body: web::Json<AppAndroidOwnerNativeEnvelope<AppAndroidOwnerNativeProposeReviewRequest>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
    pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
) -> HttpResponse {
    let now_ms = match authorize_native_body(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::ProposeReview,
    )
    .await
    {
        Ok(now_ms) => now_ms,
        Err(response) => return response,
    };
    let _proposal_creation = owner_proposal_creation_lock().lock().await;
    let packages = match normalize_native_packages(body.body.operation, &body.body.allowed_packages)
    {
        Ok(packages) => packages,
        Err(response) => return response,
    };
    match owner_store.pending_enrollment().await {
        Ok(Some(_)) => {
            return error(StatusCode::CONFLICT, "android_owner_proposal_pending");
        },
        Ok(None) => {},
        Err(_) => return owner_store_error(),
    }
    let target = pairing
        .list_all_automation_targets()
        .await
        .into_iter()
        .find(|target| {
            target.target_ref == body.body.target_ref
                && target.review_generation == body.body.expected_review_generation
        });
    let Some(target) = target else {
        return error(
            StatusCode::CONFLICT,
            "android_review_target_or_generation_changed",
        );
    };
    if packages_include_owner_identity(&packages, &target.app_package)
        || (body.body.operation == AppAndroidOwnerOperation::RevokeActions
            && !target
                .review
                .as_ref()
                .is_some_and(|review| review.allowed_packages == packages))
    {
        return error(StatusCode::BAD_REQUEST, "android_review_packages_invalid");
    }
    let (owner_generation, previous_receipt_digest, binding) = match owner_store.owner_head().await
    {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    let security = match target.attestation_security_level.as_str() {
        "tee" => AppAndroidAttestationSecurityLevel::Tee,
        "strongbox" => AppAndroidAttestationSecurityLevel::Strongbox,
        _ => return owner_store_error(),
    };
    let Some(next_owner_generation) = owner_generation.checked_add(1) else {
        return owner_store_error();
    };
    let Some(next_review_generation) = body.body.expected_review_generation.checked_add(1) else {
        return error(StatusCode::CONFLICT, "android_review_generation_exhausted");
    };
    let proposal = match AppAndroidOwnerProposal::mint(
        random_token(18),
        next_owner_generation,
        previous_receipt_digest,
        binding.desktop_identity_digest,
        target.principal,
        target.workspace,
        body.body.operation,
        Some(target.enrollment_id),
        target.target_ref,
        target.label,
        target.key_id,
        target.automation_identity_digest,
        target.app_package,
        target.app_version_code,
        target.app_signing_sha256,
        target.apk_sha256,
        target.attestation_root_sha256,
        security,
        target.attestation_policy_digest,
        body.body.expected_review_generation,
        next_review_generation,
        APP_ANDROID_OWNER_ACTION_ROSTER.to_vec(),
        packages,
        now_ms,
        now_ms.saturating_add(APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS),
    ) {
        Ok(value) => value,
        Err(_) => return error(StatusCode::BAD_REQUEST, "android_review_proposal_invalid"),
    };
    let mut pending = pending_owner_proposals().lock().await;
    if pending
        .proposal
        .as_ref()
        .is_some_and(|current| current.expires_at_ms > now_ms)
    {
        return error(StatusCode::CONFLICT, "android_review_proposal_pending");
    }
    pending.proposal = Some(proposal.clone());
    drop(pending);
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(proposal)
}

pub async fn native_pending_android_owner_handler(
    body: web::Json<AppAndroidOwnerNativeEmptyRequest>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
) -> HttpResponse {
    let now_ms = match authorize_native_empty(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::ListPending,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let mut pending = pending_owner_proposals().lock().await;
    if pending
        .proposal
        .as_ref()
        .is_some_and(|proposal| proposal.expires_at_ms <= now_ms)
    {
        pending.proposal = None;
    }
    let mut proposals = pending.proposal.clone().into_iter().collect::<Vec<_>>();
    drop(pending);
    match owner_store.pending_enrollment().await {
        Ok(Some(enrollment)) => proposals.push(enrollment.proposal),
        Ok(None) => {},
        Err(_) => return owner_store_error(),
    }
    proposals.sort_by(|left, right| left.owner_generation.cmp(&right.owner_generation));
    if proposals.len() > 1 {
        return error(StatusCode::CONFLICT, "android_owner_proposal_conflict");
    }
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(AppAndroidOwnerNativePendingResponse { proposals })
}

pub async fn native_submit_android_receipt_handler(
    body: web::Json<AppAndroidOwnerNativeEnvelope<AppAndroidOwnerReceipt>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
    pairing: web::Data<std::sync::Arc<DevicePairingStore>>,
) -> HttpResponse {
    let now_ms = match authorize_native_body(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::SubmitReceipt,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let _proposal_creation = owner_proposal_creation_lock().lock().await;
    let review_proposal = pending_owner_proposals().lock().await.proposal.clone();
    let enrollment = match owner_store.pending_enrollment().await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    let proposal = review_proposal
        .filter(|proposal| proposal.proposal_id == body.body.proposal_id)
        .or_else(|| {
            enrollment
                .as_ref()
                .map(|pending| pending.proposal.clone())
                .filter(|proposal| proposal.proposal_id == body.body.proposal_id)
        });
    let (owner_generation, latest_receipt_digest, binding) = match owner_store.owner_head().await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    let receipt_digest = match body.body.digest() {
        Ok(value) => value,
        Err(_) => return error(StatusCode::FORBIDDEN, "android_owner_receipt_rejected"),
    };
    let current_target = pairing
        .list_all_automation_targets()
        .await
        .into_iter()
        .find(|target| target.target_ref == body.body.target_ref);
    let published_replay = body.body.owner_generation == owner_generation
        && latest_receipt_digest.as_deref() == Some(receipt_digest.as_str());
    let transition_applied_recovery = snapshot_transition_applied_recovery(
        owner_generation,
        latest_receipt_digest.as_deref(),
        &body.body,
        current_target.as_ref(),
    );
    if let Some(proposal) = proposal.as_ref() {
        if body
            .body
            .verify(
                proposal,
                &binding.desktop_identity_key_id,
                &binding.desktop_identity_public_key_hex,
                &binding.desktop_identity_digest,
                now_ms,
            )
            .is_err()
        {
            return error(StatusCode::FORBIDDEN, "android_owner_receipt_rejected");
        }
    } else if (!published_replay && !transition_applied_recovery)
        || body
            .body
            .verify_desktop_identity(
                &binding.desktop_identity_key_id,
                &binding.desktop_identity_public_key_hex,
                &binding.desktop_identity_digest,
                now_ms,
            )
            .is_err()
    {
        return error(StatusCode::FORBIDDEN, "android_owner_receipt_rejected");
    }
    if body.body.operation == AppAndroidOwnerOperation::RevokeDevice {
        return error(
            StatusCode::BAD_REQUEST,
            "android_device_revoke_not_supported_by_v1",
        );
    }
    let receipt = &body.body;
    let accepted: String;
    match receipt.operation {
        AppAndroidOwnerOperation::EnrollAttestedDevice => {
            accepted = match owner_store.apply_receipt(body.body.clone(), now_ms).await {
                Ok(value) => value,
                Err(_) => return owner_store_error(),
            };
            if current_target.is_none() {
                let Some(pending) = enrollment.as_ref().filter(|pending| {
                    pending.proposal.digest().ok().as_deref()
                        == Some(receipt.proposal_digest.as_str())
                }) else {
                    return error(StatusCode::GONE, "android_owner_proposal_unavailable");
                };
                let key = magician::magician_v2::device_bridge::DeviceKey::new(
                    &receipt.principal,
                    &receipt.workspace,
                    &pending.device_id,
                );
                match pairing
                    .pair_approved_attested_automation(
                        key,
                        pending.device_label.clone(),
                        pending.identity.clone(),
                        &pending.connection_secret_sha256,
                        now_ms,
                    )
                    .await
                {
                    Ok(target_ref) if target_ref == receipt.target_ref => {},
                    Ok(_) => return owner_store_error(),
                    Err(error_value) => return pairing_error_response(error_value),
                }
            }
            if owner_store
                .clear_pending_enrollment(&receipt.proposal_digest)
                .await
                .is_err()
                && enrollment.is_some()
            {
                return owner_store_error();
            }
            if let Some(enrollment_id) = receipt.enrollment_id.as_deref() {
                pending_apps_enrollments()
                    .lock()
                    .await
                    .entries
                    .remove(enrollment_id);
            }
        },
        AppAndroidOwnerOperation::ApproveActions => {
            let already_applied = current_target
                .as_ref()
                .is_some_and(|target| snapshot_receipt_matches_durable_target(receipt, target));
            if !already_applied {
                if let Err(error_value) = pairing
                    .review_automation_actions_target(
                        &receipt.principal,
                        &receipt.workspace,
                        &receipt.target_ref,
                        receipt.expected_review_generation,
                        receipt.allowed_packages.clone(),
                        now_ms,
                    )
                    .await
                {
                    return pairing_error_response(error_value);
                }
            }
            accepted = match owner_store.apply_receipt(body.body.clone(), now_ms).await {
                Ok(value) => value,
                Err(_) => return owner_store_error(),
            };
            pending_owner_proposals().lock().await.proposal = None;
        },
        AppAndroidOwnerOperation::RevokeActions => {
            let already_applied = current_target
                .as_ref()
                .is_some_and(|target| snapshot_receipt_matches_durable_target(receipt, target));
            if !already_applied {
                if let Err(error_value) = pairing
                    .revoke_automation_review_target(
                        &receipt.principal,
                        &receipt.workspace,
                        &receipt.target_ref,
                        receipt.expected_review_generation,
                    )
                    .await
                {
                    return pairing_error_response(error_value);
                }
            }
            accepted = match owner_store.apply_receipt(body.body.clone(), now_ms).await {
                Ok(value) => value,
                Err(_) => return owner_store_error(),
            };
            pending_owner_proposals().lock().await.proposal = None;
        },
        AppAndroidOwnerOperation::RevokeDevice => {
            unreachable!("rejected before receipt persistence")
        },
    }
    let (owner_generation, latest_receipt_digest, _) = match owner_store.owner_head().await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(AppAndroidOwnerNativeControlStatus {
            owner_generation,
            latest_receipt_digest,
            accepted_receipt_digest: Some(accepted),
            target_ref: Some(receipt.target_ref.clone()),
            review_generation: Some(receipt.resulting_review_generation),
        })
}

pub async fn native_begin_android_recovery_handler(
    body: web::Json<AppAndroidOwnerNativeEmptyRequest>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
) -> HttpResponse {
    let now_ms = match authorize_native_empty(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::BeginRecovery,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    match owner_store.begin_recovery(now_ms).await {
        Ok(value) => HttpResponse::Ok()
            .insert_header(("Cache-Control", "no-store, max-age=0"))
            .json(value),
        Err(_) => owner_store_error(),
    }
}

pub async fn native_submit_android_recovery_handler(
    body: web::Json<AppAndroidOwnerNativeEnvelope<AppAndroidOwnerRecoverySnapshot>>,
    owner_store: web::Data<std::sync::Arc<AppAndroidOwnerStore>>,
) -> HttpResponse {
    let now_ms = match authorize_native_body(
        owner_store.as_ref(),
        &body,
        AppAndroidOwnerNativeOperation::SubmitRecovery,
    )
    .await
    {
        Ok(value) => value,
        Err(response) => return response,
    };
    let accepted = match owner_store.apply_recovery(body.body.clone(), now_ms).await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    let (owner_generation, latest_receipt_digest, _) = match owner_store.owner_head().await {
        Ok(value) => value,
        Err(_) => return owner_store_error(),
    };
    HttpResponse::Ok()
        .insert_header(("Cache-Control", "no-store, max-age=0"))
        .json(AppAndroidOwnerNativeControlStatus {
            owner_generation,
            latest_receipt_digest,
            accepted_receipt_digest: Some(accepted),
            target_ref: None,
            review_generation: None,
        })
}

pub fn configure_android_apps_owner_routes(config: &mut web::ServiceConfig) {
    config
        .service(
            web::resource("/devices/apps-automation/enrollment/exchange")
                .app_data(web::JsonConfig::default().limit(HANDSET_EXCHANGE_JSON_LIMIT))
                .route(web::post().to(exchange_android_apps_enrollment_handler)),
        )
        .service(
            web::resource("/devices/apps-automation/trust-options")
                .route(web::get().to(android_automation_trust_options_handler)),
        )
        .service(
            web::scope("/android-apps-owner")
                .app_data(web::JsonConfig::default().limit(NATIVE_OWNER_JSON_LIMIT))
                .route(
                    "/native/enrollment/begin",
                    web::post().to(native_begin_android_apps_enrollment_handler),
                )
                .route(
                    "/native/enrollment/cancel",
                    web::post().to(native_cancel_android_apps_enrollment_handler),
                )
                .route(
                    "/native/targets",
                    web::post().to(native_list_android_targets_handler),
                )
                .route(
                    "/native/review/propose",
                    web::post().to(native_propose_android_review_handler),
                )
                .route(
                    "/native/pending",
                    web::post().to(native_pending_android_owner_handler),
                )
                .route(
                    "/native/receipt",
                    web::post().to(native_submit_android_receipt_handler),
                )
                .route(
                    "/native/recovery/challenge",
                    web::post().to(native_begin_android_recovery_handler),
                )
                .route(
                    "/native/recovery",
                    web::post().to(native_submit_android_recovery_handler),
                ),
        );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[actix_web::test]
    async fn cancelled_pre_stage_exchange_reopens_exact_consuming_ticket() {
        let enrollment_id = format!("cancelled-pre-stage-{}", uuid::Uuid::new_v4());
        pending_apps_enrollments().lock().await.entries.insert(
            enrollment_id.clone(),
            PendingAppsEnrollment {
                principal: "owner".to_owned(),
                workspace: "default".to_owned(),
                public_origin: "https://devices.example.test".to_owned(),
                secret_digest: [7; 32],
                challenge: [9; 32],
                expires_at_ms: i64::MAX,
                attempts: 1,
                consuming: true,
                trust_policy: AndroidAutomationTrustPolicy::reviewed(
                    AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
                    &MobileEnrollmentConfig {
                        public_origin: Some("https://devices.example.test".to_owned()),
                        local_origin: None,
                        android_apps_signing_sha256: vec!["a".repeat(64)],
                        android_attestation_root_sha256: vec!["b".repeat(64)],
                        android_apps_apk_sha256: vec![],
                        android_apps_version_codes: vec![21],
                        android_play_integrity_cloud_project_number: None,
                        android_play_integrity_version_codes: vec![],
                        android_play_integrity_required_device_verdicts: vec![],
                        android_play_integrity_service_account_path: None,
                    },
                )
                .unwrap(),
            },
        );

        drop(AppsEnrollmentConsumptionGuard::new(enrollment_id.clone()));
        for _ in 0..16 {
            let reopened = pending_apps_enrollments()
                .lock()
                .await
                .entries
                .get(&enrollment_id)
                .is_some_and(|entry| !entry.consuming);
            if reopened {
                pending_apps_enrollments()
                    .lock()
                    .await
                    .entries
                    .remove(&enrollment_id);
                return;
            }
            tokio::task::yield_now().await;
        }
        panic!("dropping an unspent consumption guard did not reopen its exact ticket");
    }

    #[test]
    fn receipt_persisted_before_roster_pair_retries_publication() {
        assert_eq!(
            pending_enrollment_resume(true, false),
            PendingEnrollmentResume::PublishOrReconcile,
        );
    }

    #[test]
    fn roster_pair_before_pending_clear_retries_idempotent_publication() {
        assert_eq!(
            pending_enrollment_resume(true, true),
            PendingEnrollmentResume::PublishOrReconcile,
        );
        assert_eq!(
            pending_enrollment_resume(false, true),
            PendingEnrollmentResume::InconsistentPublishedAuthority,
        );
    }

    #[test]
    fn native_package_projection_is_exact_bounded_and_excludes_owner() {
        assert_eq!(
            normalize_native_packages(
                AppAndroidOwnerOperation::ApproveActions,
                &["com.example.z".to_owned(), "com.example.a".to_owned()],
            )
            .unwrap(),
            vec!["com.example.a", "com.example.z"]
        );
        assert!(normalize_native_packages(
            AppAndroidOwnerOperation::ApproveActions,
            &["not a package".to_owned()],
        )
        .is_err());
        assert!(!packages_include_owner_identity(
            &["com.example.target".to_owned()],
            "ai.magicbeans.magican",
        ));
        assert!(packages_include_owner_identity(
            &["ai.magicbeans.magican".to_owned()],
            "ai.magicbeans.magican",
        ));
        assert!(packages_include_owner_identity(
            &["ai.magicbeans.magdroid".to_owned()],
            "ai.magicbeans.magican",
        ));
    }

    #[actix_web::test]
    async fn active_enrollment_response_never_delivers_the_connection_secret() {
        let secret = "handset-only-connection-secret";
        let response = apps_enrollment_active_response(
            &PairedAutomationTarget {
                principal: "owner".to_owned(),
                workspace: "default".to_owned(),
                target_ref: "android-device:opaque".to_owned(),
                label: "Pixel".to_owned(),
                enrollment_id: "enrollment-123456".to_owned(),
                key_id: "key-id-123456789".to_owned(),
                automation_identity_digest: format!("blake3:{}", "a".repeat(64)),
                app_package: "ai.magicbeans.magdroid".to_owned(),
                app_version_code: 11,
                app_signing_sha256: "b".repeat(64),
                apk_sha256: "c".repeat(64),
                attestation_root_sha256: "d".repeat(64),
                attestation_security_level: "tee".to_owned(),
                attestation_policy_digest: format!("blake3:{}", "e".repeat(64)),
                paired_at_ms: 1,
                last_seen_ms: None,
                review_generation: 0,
                review: None,
            },
            "https://devices.example.test",
        );
        let body = actix_web::body::to_bytes(response.into_body())
            .await
            .unwrap();
        let json = std::str::from_utf8(&body).unwrap();
        assert!(!json.contains(secret));
        assert!(!json.contains("token"));
        assert!(!json.contains("connection_secret"));
    }

    #[test]
    fn roster_first_snapshot_transition_recovers_only_the_exact_signed_head_successor() {
        let target = PairedAutomationTarget {
            principal: "owner".to_owned(),
            workspace: "default".to_owned(),
            target_ref: "android-device:opaque".to_owned(),
            label: "Pixel".to_owned(),
            enrollment_id: "enrollment-123456".to_owned(),
            key_id: "android-key:opaque".to_owned(),
            automation_identity_digest: format!("blake3:{}", "a".repeat(64)),
            app_package: "ai.magicbeans.magdroid".to_owned(),
            app_version_code: 11,
            app_signing_sha256: "b".repeat(64),
            apk_sha256: "c".repeat(64),
            attestation_root_sha256: "d".repeat(64),
            attestation_security_level: "tee".to_owned(),
            attestation_policy_digest: format!("blake3:{}", "e".repeat(64)),
            paired_at_ms: 1,
            last_seen_ms: None,
            review_generation: 4,
            review: None,
        };
        let receipt = AppAndroidOwnerReceipt {
            schema: magician_app_contract::android_owner::APP_ANDROID_OWNER_V1.to_owned(),
            proposal_digest: format!("blake3:{}", "1".repeat(64)),
            proposal_id: "proposal-123456".to_owned(),
            owner_generation: 8,
            previous_receipt_digest: Some(format!("blake3:{}", "2".repeat(64))),
            desktop_identity_digest: format!("blake3:{}", "3".repeat(64)),
            principal: target.principal.clone(),
            workspace: target.workspace.clone(),
            operation: AppAndroidOwnerOperation::RevokeActions,
            enrollment_id: None,
            target_ref: target.target_ref.clone(),
            device_label: target.label.clone(),
            key_id: target.key_id.clone(),
            automation_identity_digest: target.automation_identity_digest.clone(),
            owner_app_package: target.app_package.clone(),
            app_version_code: target.app_version_code,
            app_signing_sha256: target.app_signing_sha256.clone(),
            apk_sha256: target.apk_sha256.clone(),
            attestation_root_sha256: target.attestation_root_sha256.clone(),
            attestation_security_level: AppAndroidAttestationSecurityLevel::Tee,
            attestation_policy_digest: target.attestation_policy_digest.clone(),
            expected_review_generation: 3,
            resulting_review_generation: 4,
            actions: APP_ANDROID_OWNER_ACTION_ROSTER.to_vec(),
            allowed_packages: vec!["com.example.target".to_owned()],
            display_digest: format!("blake3:{}", "4".repeat(64)),
            issued_at_ms: 1,
            expires_at_ms: 10,
            signed_at_ms: 2,
            desktop_identity_key_id: "desktop-key".to_owned(),
            desktop_identity_signature_hex: "5".repeat(128),
        };
        let previous = receipt.previous_receipt_digest.as_deref();
        assert!(snapshot_transition_applied_recovery(
            7,
            previous,
            &receipt,
            Some(&target),
        ));

        let mut substituted = receipt.clone();
        substituted.workspace = "other".to_owned();
        assert!(!snapshot_transition_applied_recovery(
            7,
            previous,
            &substituted,
            Some(&target),
        ));
        assert!(!snapshot_transition_applied_recovery(
            6,
            previous,
            &receipt,
            Some(&target),
        ));
    }
}

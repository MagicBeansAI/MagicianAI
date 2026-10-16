//! Closed desktop-owner wire for Android Apps automation authority.
//!
//! Android attestation proves which handset key and application produced an
//! enrollment claim. It does not prove that the desktop owner reviewed that
//! device, package set, or action. These DTOs carry only the exact material
//! displayed by the trusted native Settings surface. The Keychain-backed
//! desktop identity signs each monotonic transition and a separate status
//! snapshot used for recovery. General HTTP callers cannot mint either type.

use serde::{Deserialize, Serialize};

use crate::macos_host::{
    app_macos_desktop_identity_digest, AppMacosDesktopIdentityAttestation,
    AppMacosDesktopIdentityChallenge,
};

pub const APP_ANDROID_OWNER_V1: &str = "magician.android-apps-owner.v1";
pub const APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1: &str =
    "magician.android-apps-owner.bootstrap-socket.v1";
pub const APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY: &str = "Magican";
pub const APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY: &str = "app-android-owner-v1";
pub const APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME: &str = "bootstrap-v1.sock";
pub const APP_ANDROID_OWNER_DESKTOP_BUNDLE_ID: &str = "ai.magicbeans.magican.desktop";
pub const APP_ANDROID_OWNER_COMPANION_PACKAGE: &str = "ai.magicbeans.magican";
pub const APP_ANDROID_OWNER_LEGACY_COMPANION_PACKAGE: &str = "ai.magicbeans.magdroid";
pub const APP_ANDROID_OWNER_RUNTIME_CODE_IDENTIFIER: &str = "com.magicbeans.magician";
pub const APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES: usize = 64 * 1024;
pub const APP_ANDROID_OWNER_ACTION_SNAPSHOT: &str = "snapshot";
pub const APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS: i64 = 2 * 60 * 1_000;
pub const APP_ANDROID_OWNER_MAX_STATUS_LIFETIME_MS: i64 = 30_000;
pub const APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS: i64 = 5_000;
pub const APP_ANDROID_OWNER_MAX_PACKAGES: usize = 128;
pub const APP_ANDROID_OWNER_MAX_STATUS_RECORDS: usize = 256;

pub fn is_android_owner_companion_package(package: &str) -> bool {
    package == APP_ANDROID_OWNER_COMPANION_PACKAGE
        || package == APP_ANDROID_OWNER_LEGACY_COMPANION_PACKAGE
}

const PROPOSAL_DIGEST_DOMAIN: &str = "magician.android-apps-owner.proposal.v1";
const DISPLAY_DIGEST_DOMAIN: &str = "magician.android-apps-owner.display.v1";
const RECEIPT_SIGNATURE_DOMAIN: &str = "magician.android-apps-owner.receipt.v1";
const STATUS_CHALLENGE_DIGEST_DOMAIN: &str = "magician.android-apps-owner.status-challenge.v1";
const STATUS_RECEIPT_SIGNATURE_DOMAIN: &str = "magician.android-apps-owner.status-receipt.v1";
const RECOVERY_CHALLENGE_DIGEST_DOMAIN: &str = "magician.android-apps-owner.recovery-challenge.v1";
const RECOVERY_DISPLAY_DIGEST_DOMAIN: &str = "magician.android-apps-owner.recovery-display.v1";
const RECOVERY_SNAPSHOT_SIGNATURE_DOMAIN: &str = "magician.android-apps-owner.recovery-snapshot.v1";
const NATIVE_REQUEST_SIGNATURE_DOMAIN: &str = "magician.android-apps-owner.native-request.v1";
const BOOTSTRAP_NONCE_DIGEST_DOMAIN: &str = "magician.android-apps-owner.bootstrap-nonce.v1";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidOwnerOperation {
    EnrollAttestedDevice,
    ApproveActions,
    RevokeActions,
    RevokeDevice,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidOwnerAction {
    Snapshot,
    Screenshot,
    Launch,
    Close,
    Tap,
    Type,
    Key,
    Scroll,
}

/// The only action roster that the Android Apps owner may sign. Keeping the
/// order canonical makes the displayed proposal, durable review digest and
/// point-of-use grant byte-stable; callers cannot request a subset or add a
/// generic Android/MCP action.
pub const APP_ANDROID_OWNER_ACTION_ROSTER: [AppAndroidOwnerAction; 8] = [
    AppAndroidOwnerAction::Snapshot,
    AppAndroidOwnerAction::Screenshot,
    AppAndroidOwnerAction::Launch,
    AppAndroidOwnerAction::Close,
    AppAndroidOwnerAction::Tap,
    AppAndroidOwnerAction::Type,
    AppAndroidOwnerAction::Key,
    AppAndroidOwnerAction::Scroll,
];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidOwnerNativeOperation {
    BeginEnrollment,
    CancelEnrollment,
    ListTargets,
    ProposeReview,
    ListPending,
    SubmitReceipt,
    BeginRecovery,
    SubmitRecovery,
}

/// The independently reviewed authority used in addition to Android's
/// hardware-backed key attestation. Private/self-hosted builds deliberately
/// omit a Google Play token; they remain bound to exact signer, app-version,
/// attestation-root, verified-boot, owner-review, and signed-socket checks.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidAutomationTrustMode {
    PlayIntegrity,
    OwnerPinnedPrivateBuild,
}

impl AppAndroidAutomationTrustMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlayIntegrity => "play_integrity",
            Self::OwnerPinnedPrivateBuild => "owner_pinned_private_build",
        }
    }
}

/// Closed request authentication used only by native Tauri commands when they
/// call the runtime owner-control endpoints. JavaScript never receives a
/// signer or reusable bearer. The runtime consumes `request_nonce` once.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeRequest {
    pub schema: String,
    pub operation: AppAndroidOwnerNativeOperation,
    pub request_nonce: String,
    pub body_digest: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub desktop_identity_digest: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

/// Canonical digest for the typed body paired with a closed native operation.
/// The operation is included so identical JSON cannot be replayed at another
/// owner endpoint.
pub fn app_android_owner_native_body_digest<T: Serialize>(
    operation: AppAndroidOwnerNativeOperation,
    body: &T,
) -> Result<String, AppAndroidOwnerError> {
    domain_digest(
        "magician.android-apps-owner.native-body.v1",
        &(operation, body),
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeEmptyRequest {
    pub authorization: AppAndroidOwnerNativeRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeBeginEnrollmentRequest {
    pub trust_mode: AppAndroidAutomationTrustMode,
    #[serde(default)]
    pub connection_mode: AppAndroidEnrollmentConnectionMode,
}

/// Server-owned transport selected for one Android Apps enrollment. The native
/// owner chooses only between origins already configured by Magician; it never
/// supplies an address.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidEnrollmentConnectionMode {
    SameWifi,
    #[default]
    Remote,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeEnvelope<T> {
    pub authorization: AppAndroidOwnerNativeRequest,
    pub body: T,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeEnrollmentResponse {
    pub enrollment_id: String,
    pub enrollment_uri: String,
    pub qr_svg: String,
    pub expires_at_ms: i64,
    pub challenge_base64: String,
    pub principal: String,
    pub workspace: String,
    pub trust_mode: AppAndroidAutomationTrustMode,
    pub connection_mode: AppAndroidEnrollmentConnectionMode,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeCancelEnrollmentRequest {
    pub enrollment_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeCancelEnrollmentResponse {
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeTarget {
    pub principal: String,
    pub workspace: String,
    pub target_ref: String,
    pub device_label: String,
    pub automation_identity_digest: String,
    /// Independently attested Magdroid owner package. This is physical-owner
    /// identity, not a package authorized for observation.
    pub owner_app_package: String,
    pub app_version_code: u64,
    pub app_signing_sha256: String,
    pub apk_sha256: String,
    pub attestation_root_sha256: String,
    pub attestation_security_level: AppAndroidAttestationSecurityLevel,
    pub attestation_policy_digest: String,
    pub paired_at_ms: i64,
    pub last_seen_ms: Option<i64>,
    pub review_generation: u64,
    pub reviewed_actions: Vec<AppAndroidOwnerAction>,
    pub reviewed_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeTargetsResponse {
    pub targets: Vec<AppAndroidOwnerNativeTarget>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeProposeReviewRequest {
    pub target_ref: String,
    pub expected_review_generation: u64,
    pub operation: AppAndroidOwnerOperation,
    pub allowed_packages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativePendingResponse {
    pub proposals: Vec<AppAndroidOwnerProposal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerNativeControlStatus {
    pub owner_generation: u64,
    pub latest_receipt_digest: Option<String>,
    pub accepted_receipt_digest: Option<String>,
    pub target_ref: Option<String>,
    pub review_generation: Option<u64>,
}

/// Runtime-minted first message for desktop-owner enrollment. It deliberately
/// contains no caller-selected identity: the native Settings surface must
/// first obtain this one-shot nonce, then bind the displayed Keychain identity
/// and owner code to it in a separate request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapNonce {
    pub schema: String,
    pub bootstrap_nonce: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppAndroidOwnerBootstrapNonce {
    pub fn mint(
        bootstrap_nonce: String,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        let value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            bootstrap_nonce,
            issued_at_ms,
            expires_at_ms,
        };
        value.validate(issued_at_ms)?;
        Ok(value)
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_ANDROID_OWNER_MAX_STATUS_LIFETIME_MS
            || self.issued_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_token(&self.bootstrap_nonce, 192)
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate(self.issued_at_ms)?;
        domain_digest(BOOTSTRAP_NONCE_DIGEST_DOMAIN, self)
    }
}

/// Second bootstrap message, constructed only inside the trusted Tauri
/// command after the owner has been shown the Keychain fingerprint and
/// one-time approval code. The runtime consumes `bootstrap_nonce` before it
/// mints the exact macOS identity challenge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapChallengeRequest {
    pub bootstrap: AppAndroidOwnerBootstrapNonce,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_digest: String,
    pub owner_approval_code_digest: String,
}

/// Exact runtime correlation used only after the desktop reports that a
/// surviving finalized owner exists for a fresh runtime bootstrap nonce.
/// The offer digest is minted by the code-identity-authenticated desktop and
/// persisted by the runtime before this distinct recovery hello is sent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapRecoveryHello {
    pub bootstrap: AppAndroidOwnerBootstrapNonce,
    pub rebind_offer_digest: String,
    pub expected_desktop_identity_digest: String,
}

impl AppAndroidOwnerBootstrapRecoveryHello {
    pub fn validate(&self) -> Result<(), AppAndroidOwnerError> {
        // A retained recovery correlation can outlive the short owner-display
        // window after the desktop has incurred and persisted Completion.
        self.bootstrap.validate(self.bootstrap.issued_at_ms)?;
        validate_digest(&self.rebind_offer_digest)?;
        validate_digest(&self.expected_desktop_identity_digest)
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate()?;
        domain_digest(
            "magician.android-apps-owner.bootstrap-recovery-hello.v1",
            self,
        )
    }
}

/// Desktop-authenticated recovery offer displayed before the existing owner
/// identity is rebound to a runtime that lost its local authority document.
/// It exposes no target records, but binds the non-decreasing global head that
/// the subsequent signed full recovery snapshot must restore exactly.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapRebindOffer {
    pub schema: String,
    pub bootstrap: AppAndroidOwnerBootstrapNonce,
    pub desktop_identity_key_id: String,
    pub desktop_identity_digest: String,
    pub previous_bootstrap_status_digest: String,
    pub owner_generation: u64,
    pub latest_receipt_digest: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub display_digest: String,
}

impl AppAndroidOwnerBootstrapRebindOffer {
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        bootstrap: AppAndroidOwnerBootstrapNonce,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        previous_bootstrap_status_digest: String,
        owner_generation: u64,
        latest_receipt_digest: Option<String>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        let mut value = Self {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            bootstrap,
            desktop_identity_key_id,
            desktop_identity_digest,
            previous_bootstrap_status_digest,
            owner_generation,
            latest_receipt_digest,
            issued_at_ms,
            expires_at_ms,
            display_digest: String::new(),
        };
        value.display_digest = value.recompute_display_digest()?;
        value.validate(value.issued_at_ms)?;
        Ok(value)
    }

    pub fn recompute_display_digest(&self) -> Result<String, AppAndroidOwnerError> {
        domain_digest(
            "magician.android-apps-owner.bootstrap-rebind-display.v1",
            &AppAndroidOwnerBootstrapRebindOfferMaterial {
                schema: &self.schema,
                bootstrap: &self.bootstrap,
                desktop_identity_key_id: &self.desktop_identity_key_id,
                desktop_identity_digest: &self.desktop_identity_digest,
                previous_bootstrap_status_digest: &self.previous_bootstrap_status_digest,
                owner_generation: self.owner_generation,
                latest_receipt_digest: self.latest_receipt_digest.as_deref(),
                issued_at_ms: self.issued_at_ms,
                expires_at_ms: self.expires_at_ms,
            },
        )
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1
            || self.issued_at_ms < self.bootstrap.issued_at_ms
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms > self.bootstrap.expires_at_ms
            || (self.owner_generation == 0) != self.latest_receipt_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        self.bootstrap.validate(now_ms)?;
        validate_token(&self.desktop_identity_key_id, 96)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_digest(&self.previous_bootstrap_status_digest)?;
        if let Some(value) = &self.latest_receipt_digest {
            validate_digest(value)?;
        }
        validate_digest(&self.display_digest)?;
        if self.display_digest != self.recompute_display_digest()? {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate(self.issued_at_ms)?;
        domain_digest(
            "magician.android-apps-owner.bootstrap-rebind-offer.v1",
            self,
        )
    }
}

#[derive(Serialize)]
struct AppAndroidOwnerBootstrapRebindOfferMaterial<'a> {
    schema: &'a str,
    bootstrap: &'a AppAndroidOwnerBootstrapNonce,
    desktop_identity_key_id: &'a str,
    desktop_identity_digest: &'a str,
    previous_bootstrap_status_digest: &'a str,
    owner_generation: u64,
    latest_receipt_digest: Option<&'a str>,
    issued_at_ms: i64,
    expires_at_ms: i64,
}

/// Derive the one exact desktop identity challenge from the runtime-offered
/// bootstrap nonce and the native owner's displayed Keychain identity/code.
/// Runtime and desktop both compare this value byte-for-byte on the UDS.
pub fn app_android_owner_bootstrap_challenge(
    request: &AppAndroidOwnerBootstrapChallengeRequest,
) -> Result<AppMacosDesktopIdentityChallenge, AppAndroidOwnerError> {
    request.validate(request.bootstrap.issued_at_ms)?;
    let bootstrap_digest = request.bootstrap.digest()?;
    let material = serde_json::to_vec(&(
        "magician.android-apps-owner.bootstrap-challenge-nonce.v1",
        bootstrap_digest,
        request.desktop_identity_key_id.as_str(),
        request.desktop_identity_public_key_hex.as_str(),
        request.owner_approval_code_digest.as_str(),
    ))
    .map_err(|_| AppAndroidOwnerError::Encoding)?;
    AppMacosDesktopIdentityChallenge::mint(
        request.desktop_identity_key_id.clone(),
        request.desktop_identity_public_key_hex.clone(),
        format!("android-bootstrap:{}", blake3::hash(&material).to_hex()),
        request.owner_approval_code_digest.clone(),
        request.bootstrap.issued_at_ms,
        request.bootstrap.expires_at_ms,
    )
    .map_err(|_| AppAndroidOwnerError::InvalidClaims)
}

impl AppAndroidOwnerBootstrapChallengeRequest {
    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        self.bootstrap.validate(now_ms)?;
        validate_desktop_identity(
            &self.desktop_identity_key_id,
            &self.desktop_identity_public_key_hex,
            &self.desktop_identity_digest,
        )?;
        validate_digest(&self.owner_approval_code_digest)
    }
}

/// Final bootstrap message. The attestation is returned only by the trusted
/// native owner and never exposed to ordinary web content. The runtime accepts
/// it only for the exact retained challenge and then persists the pinned
/// desktop identity before enabling signed native requests.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapCompletion {
    pub challenge: AppMacosDesktopIdentityChallenge,
    pub attestation: AppMacosDesktopIdentityAttestation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerBootstrapStatus {
    pub schema: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_digest: String,
    pub desktop_identity_attestation_digest: String,
    pub host_identity_digest: String,
    pub pinned_at_ms: i64,
}

impl AppAndroidOwnerBootstrapStatus {
    pub fn from_verified(
        challenge: &AppMacosDesktopIdentityChallenge,
        attestation: &AppMacosDesktopIdentityAttestation,
        now_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        // Owner approval happens at the attested instant. Delivery of that
        // exact signed proof is retryable after the short challenge lifetime;
        // expiry prevents a new attestation, not recovery of an existing one.
        attestation
            .verify(challenge, attestation.attested_at_ms)
            .map_err(|_| AppAndroidOwnerError::InvalidSignature)?;
        if now_ms < attestation.attested_at_ms
            || attestation.attested_at_ms
                > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        let desktop_identity_digest = app_macos_desktop_identity_digest(
            &challenge.desktop_identity_key_id,
            &challenge.desktop_identity_public_key_hex,
        )
        .map_err(|_| AppAndroidOwnerError::InvalidClaims)?;
        let desktop_identity_attestation_digest = attestation
            .digest()
            .map_err(|_| AppAndroidOwnerError::InvalidDigest)?;
        Ok(Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            desktop_identity_key_id: challenge.desktop_identity_key_id.clone(),
            desktop_identity_public_key_hex: challenge.desktop_identity_public_key_hex.clone(),
            desktop_identity_digest,
            desktop_identity_attestation_digest,
            host_identity_digest: attestation.host_identity_digest.clone(),
            pinned_at_ms: now_ms,
        })
    }

    pub fn validate(&self) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1 || self.pinned_at_ms < 0 {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_desktop_identity(
            &self.desktop_identity_key_id,
            &self.desktop_identity_public_key_hex,
            &self.desktop_identity_digest,
        )?;
        validate_digest(&self.desktop_identity_attestation_digest)?;
        validate_digest(&self.host_identity_digest)
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate()?;
        domain_digest("magician.android-apps-owner.bootstrap-status.v1", self)
    }
}

/// Closed runtime-to-desktop bootstrap messages carried only by the
/// code-identity-verified private Unix socket.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "message", content = "payload", rename_all = "snake_case")]
pub enum AppAndroidOwnerBootstrapRuntimeMessage {
    Hello {
        schema: String,
        bootstrap: AppAndroidOwnerBootstrapNonce,
    },
    Challenge {
        schema: String,
        challenge: AppMacosDesktopIdentityChallenge,
    },
    RecoveryHello {
        schema: String,
        recovery: AppAndroidOwnerBootstrapRecoveryHello,
    },
    Finalized {
        schema: String,
        status: AppAndroidOwnerBootstrapStatus,
    },
}

impl AppAndroidOwnerBootstrapRuntimeMessage {
    pub fn hello(bootstrap: AppAndroidOwnerBootstrapNonce) -> Self {
        Self::Hello {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            bootstrap,
        }
    }

    pub fn challenge(challenge: AppMacosDesktopIdentityChallenge) -> Self {
        Self::Challenge {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            challenge,
        }
    }

    pub fn recovery_hello(recovery: AppAndroidOwnerBootstrapRecoveryHello) -> Self {
        Self::RecoveryHello {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            recovery,
        }
    }

    pub fn finalized(status: AppAndroidOwnerBootstrapStatus) -> Self {
        Self::Finalized {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            status,
        }
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        match self {
            Self::Hello { schema, bootstrap } => {
                validate_bootstrap_socket_schema(schema)?;
                // An exact retained Hello is also the correlation used to
                // recover a desktop-signed Completion after the short nonce
                // lifetime. New minting remains expiry-gated by the store.
                bootstrap.digest().map(|_| ())
            },
            Self::Challenge { schema, challenge } => {
                validate_bootstrap_socket_schema(schema)?;
                challenge
                    .validate(now_ms)
                    .map_err(|_| AppAndroidOwnerError::InvalidClaims)
            },
            Self::RecoveryHello { schema, recovery } => {
                validate_bootstrap_socket_schema(schema)?;
                recovery.validate()
            },
            Self::Finalized { schema, status } => {
                validate_bootstrap_socket_schema(schema)?;
                status.validate()
            },
        }
    }
}

/// Closed desktop-to-runtime messages. A retained `Completion` may be replayed
/// byte-identically after expiry; its attestation must still have been signed
/// within the original challenge lifetime.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "message", content = "payload", rename_all = "snake_case")]
pub enum AppAndroidOwnerBootstrapDesktopMessage {
    /// Connection-local readiness barrier emitted only after the desktop has
    /// verified the connected runtime's audit token and live/static code.
    /// The runtime does not recover or mint its short-lived nonce before this
    /// barrier, so code verification and admission waits cannot consume the
    /// replay-protection window.
    PeerVerified { schema: String },
    AwaitingNativeApproval {
        schema: String,
        bootstrap_digest: String,
    },
    ExpiredUnspent {
        schema: String,
        bootstrap_digest: String,
    },
    RebindAwaitingNativeApproval {
        schema: String,
        offer: AppAndroidOwnerBootstrapRebindOffer,
    },
    ChallengeRequest {
        schema: String,
        request: AppAndroidOwnerBootstrapChallengeRequest,
    },
    Completion {
        schema: String,
        completion: AppAndroidOwnerBootstrapCompletion,
    },
    FinalizedAck {
        schema: String,
        status_digest: String,
    },
}

impl AppAndroidOwnerBootstrapDesktopMessage {
    pub fn peer_verified() -> Self {
        Self::PeerVerified {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
        }
    }

    pub fn awaiting_native_approval(bootstrap_digest: String) -> Self {
        Self::AwaitingNativeApproval {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            bootstrap_digest,
        }
    }

    pub fn challenge_request(request: AppAndroidOwnerBootstrapChallengeRequest) -> Self {
        Self::ChallengeRequest {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            request,
        }
    }

    pub fn rebind_awaiting_native_approval(offer: AppAndroidOwnerBootstrapRebindOffer) -> Self {
        Self::RebindAwaitingNativeApproval {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            offer,
        }
    }

    pub fn expired_unspent(bootstrap_digest: String) -> Self {
        Self::ExpiredUnspent {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            bootstrap_digest,
        }
    }

    pub fn completion(completion: AppAndroidOwnerBootstrapCompletion) -> Self {
        Self::Completion {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            completion,
        }
    }

    pub fn finalized_ack(status_digest: String) -> Self {
        Self::FinalizedAck {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1.to_owned(),
            status_digest,
        }
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        match self {
            Self::PeerVerified { schema } => validate_bootstrap_socket_schema(schema),
            Self::AwaitingNativeApproval {
                schema,
                bootstrap_digest,
            } => {
                validate_bootstrap_socket_schema(schema)?;
                validate_digest(bootstrap_digest)
            },
            Self::ExpiredUnspent {
                schema,
                bootstrap_digest,
            } => {
                validate_bootstrap_socket_schema(schema)?;
                validate_digest(bootstrap_digest)
            },
            Self::RebindAwaitingNativeApproval { schema, offer } => {
                validate_bootstrap_socket_schema(schema)?;
                offer.validate(now_ms)
            },
            Self::ChallengeRequest { schema, request } => {
                validate_bootstrap_socket_schema(schema)?;
                request.validate(now_ms)
            },
            Self::Completion { schema, completion } => {
                validate_bootstrap_socket_schema(schema)?;
                AppAndroidOwnerBootstrapStatus::from_verified(
                    &completion.challenge,
                    &completion.attestation,
                    now_ms,
                )
                .map(|_| ())
            },
            Self::FinalizedAck {
                schema,
                status_digest,
            } => {
                validate_bootstrap_socket_schema(schema)?;
                validate_digest(status_digest)
            },
        }
    }
}

fn validate_bootstrap_socket_schema(schema: &str) -> Result<(), AppAndroidOwnerError> {
    (schema == APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1)
        .then_some(())
        .ok_or(AppAndroidOwnerError::InvalidClaims)
}

impl AppAndroidOwnerNativeRequest {
    pub fn unsigned(
        operation: AppAndroidOwnerNativeOperation,
        request_nonce: String,
        body_digest: Option<String>,
        issued_at_ms: i64,
        expires_at_ms: i64,
        desktop_identity_digest: String,
        desktop_identity_key_id: String,
    ) -> Result<Self, AppAndroidOwnerError> {
        let value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            operation,
            request_nonce,
            body_digest,
            issued_at_ms,
            expires_at_ms,
            desktop_identity_digest,
            desktop_identity_key_id,
            desktop_identity_signature_hex: String::new(),
        };
        value.validate_shape(issued_at_ms)?;
        Ok(value)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppAndroidOwnerError> {
        self.validate_shape(self.issued_at_ms)?;
        serde_json::to_vec(&(
            NATIVE_REQUEST_SIGNATURE_DOMAIN,
            self.schema.as_str(),
            self.operation,
            self.request_nonce.as_str(),
            self.body_digest.as_deref(),
            self.issued_at_ms,
            self.expires_at_ms,
            self.desktop_identity_digest.as_str(),
            self.desktop_identity_key_id.as_str(),
        ))
        .map_err(|_| AppAndroidOwnerError::Encoding)
    }

    pub fn verify(
        &self,
        expected_operation: AppAndroidOwnerNativeOperation,
        expected_body_digest: Option<&str>,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_public_key_hex: &str,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerError> {
        self.validate_shape(now_ms)?;
        validate_desktop_identity(
            expected_desktop_identity_key_id,
            expected_desktop_identity_public_key_hex,
            expected_desktop_identity_digest,
        )?;
        if self.operation != expected_operation
            || self.body_digest.as_deref() != expected_body_digest
            || self.desktop_identity_key_id != expected_desktop_identity_key_id
            || self.desktop_identity_digest != expected_desktop_identity_digest
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        verify_ed25519(
            expected_desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_shape(self.issued_at_ms)?;
        domain_digest("magician.android-apps-owner.native-request-digest.v1", self)
    }

    fn validate_shape(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_ANDROID_OWNER_MAX_STATUS_LIFETIME_MS
            || self.issued_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
            || matches!(
                self.operation,
                AppAndroidOwnerNativeOperation::ListTargets
                    | AppAndroidOwnerNativeOperation::ListPending
                    | AppAndroidOwnerNativeOperation::BeginRecovery
            ) != self.body_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_token(&self.request_nonce, 192)?;
        if let Some(value) = &self.body_digest {
            validate_digest(value)?;
        }
        validate_digest(&self.desktop_identity_digest)?;
        validate_token(&self.desktop_identity_key_id, 96)?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAndroidAttestationSecurityLevel {
    Tee,
    Strongbox,
}

/// Runtime-created, short-lived description of one exact owner decision.
/// `display_digest` covers the canonical display material and is recomputed by
/// both the native Settings UI and the desktop owner before signing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerProposal {
    pub schema: String,
    pub proposal_id: String,
    pub owner_generation: u64,
    pub previous_receipt_digest: Option<String>,
    pub desktop_identity_digest: String,
    pub principal: String,
    pub workspace: String,
    pub operation: AppAndroidOwnerOperation,
    pub enrollment_id: Option<String>,
    pub target_ref: String,
    pub device_label: String,
    pub key_id: String,
    pub automation_identity_digest: String,
    /// Independently attested Magdroid owner package. This is not part of the
    /// reviewed foreground package allowlist.
    pub owner_app_package: String,
    pub app_version_code: u64,
    pub app_signing_sha256: String,
    pub apk_sha256: String,
    pub attestation_root_sha256: String,
    pub attestation_security_level: AppAndroidAttestationSecurityLevel,
    pub attestation_policy_digest: String,
    pub expected_review_generation: u64,
    pub resulting_review_generation: u64,
    pub actions: Vec<AppAndroidOwnerAction>,
    pub allowed_packages: Vec<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub display_digest: String,
}

impl AppAndroidOwnerProposal {
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        proposal_id: String,
        owner_generation: u64,
        previous_receipt_digest: Option<String>,
        desktop_identity_digest: String,
        principal: String,
        workspace: String,
        operation: AppAndroidOwnerOperation,
        enrollment_id: Option<String>,
        target_ref: String,
        device_label: String,
        key_id: String,
        automation_identity_digest: String,
        owner_app_package: String,
        app_version_code: u64,
        app_signing_sha256: String,
        apk_sha256: String,
        attestation_root_sha256: String,
        attestation_security_level: AppAndroidAttestationSecurityLevel,
        attestation_policy_digest: String,
        expected_review_generation: u64,
        resulting_review_generation: u64,
        actions: Vec<AppAndroidOwnerAction>,
        allowed_packages: Vec<String>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        let mut value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            proposal_id,
            owner_generation,
            previous_receipt_digest,
            desktop_identity_digest,
            principal,
            workspace,
            operation,
            enrollment_id,
            target_ref,
            device_label,
            key_id,
            automation_identity_digest,
            owner_app_package,
            app_version_code,
            app_signing_sha256,
            apk_sha256,
            attestation_root_sha256,
            attestation_security_level,
            attestation_policy_digest,
            expected_review_generation,
            resulting_review_generation,
            actions,
            allowed_packages,
            issued_at_ms,
            expires_at_ms,
            display_digest: String::new(),
        };
        value.validate_shape(issued_at_ms, true)?;
        value.display_digest = value.recompute_display_digest()?;
        Ok(value)
    }

    /// Validate a proposal while it is eligible for a new native signature.
    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        self.validate_shape(now_ms, true)?;
        if self.display_digest != self.recompute_display_digest()? {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        Ok(())
    }

    /// Digest of the complete proposal, including its canonical display
    /// digest. This is the identity repeated in the signed receipt.
    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_shape(self.issued_at_ms, false)?;
        if self.display_digest != self.recompute_display_digest()? {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        domain_digest(PROPOSAL_DIGEST_DOMAIN, self)
    }

    pub fn recompute_display_digest(&self) -> Result<String, AppAndroidOwnerError> {
        domain_digest(DISPLAY_DIGEST_DOMAIN, &self.display_material())
    }

    fn validate_shape(&self, now_ms: i64, require_live: bool) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.owner_generation == 0
            || (self.owner_generation == 1) != self.previous_receipt_digest.is_none()
            || self.issued_at_ms < 0
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS
            || (require_live && self.expires_at_ms <= now_ms)
            || self.issued_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
            || self.app_version_code == 0
            || self.allowed_packages.len() > APP_ANDROID_OWNER_MAX_PACKAGES
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_token(&self.proposal_id, 192)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_scope_component(&self.principal)?;
        validate_scope_component(&self.workspace)?;
        if let Some(value) = &self.enrollment_id {
            validate_token(value, 192)?;
        }
        validate_token(&self.target_ref, 192)?;
        if !self.target_ref.starts_with("android-device:") {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_display_text(&self.device_label, 128)?;
        validate_token(&self.key_id, 192)?;
        validate_digest(&self.automation_identity_digest)?;
        validate_package(&self.owner_app_package)?;
        validate_sha256(&self.app_signing_sha256)?;
        validate_sha256(&self.apk_sha256)?;
        validate_sha256(&self.attestation_root_sha256)?;
        validate_digest(&self.attestation_policy_digest)?;
        if let Some(value) = &self.previous_receipt_digest {
            validate_digest(value)?;
        }
        if !self
            .allowed_packages
            .windows(2)
            .all(|values| values[0] < values[1])
            || self.allowed_packages.iter().any(|value| {
                validate_package(value).is_err()
                    || value == &self.owner_app_package
                    || is_android_owner_companion_package(value)
            })
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        match self.operation {
            AppAndroidOwnerOperation::EnrollAttestedDevice => {
                if self.enrollment_id.is_none()
                    || self.expected_review_generation != 0
                    || self.resulting_review_generation != 0
                    || !self.actions.is_empty()
                    || !self.allowed_packages.is_empty()
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::ApproveActions => {
                if self.enrollment_id.is_none()
                    || self.allowed_packages.is_empty()
                    || self.actions.as_slice() != APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::RevokeActions => {
                if self.enrollment_id.is_none()
                    || self.allowed_packages.is_empty()
                    || self.actions.as_slice() != APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::RevokeDevice => {
                if self.enrollment_id.is_none()
                    || !self.actions.is_empty()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
        }
        Ok(())
    }

    fn display_material(&self) -> AppAndroidOwnerDisplayMaterial<'_> {
        AppAndroidOwnerDisplayMaterial {
            schema: &self.schema,
            proposal_id: &self.proposal_id,
            owner_generation: self.owner_generation,
            previous_receipt_digest: self.previous_receipt_digest.as_deref(),
            desktop_identity_digest: &self.desktop_identity_digest,
            principal: &self.principal,
            workspace: &self.workspace,
            operation: self.operation,
            enrollment_id: self.enrollment_id.as_deref(),
            target_ref: &self.target_ref,
            device_label: &self.device_label,
            key_id: &self.key_id,
            automation_identity_digest: &self.automation_identity_digest,
            owner_app_package: &self.owner_app_package,
            app_version_code: self.app_version_code,
            app_signing_sha256: &self.app_signing_sha256,
            apk_sha256: &self.apk_sha256,
            attestation_root_sha256: &self.attestation_root_sha256,
            attestation_security_level: self.attestation_security_level,
            attestation_policy_digest: &self.attestation_policy_digest,
            expected_review_generation: self.expected_review_generation,
            resulting_review_generation: self.resulting_review_generation,
            actions: &self.actions,
            allowed_packages: &self.allowed_packages,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
        }
    }
}

#[derive(Serialize)]
struct AppAndroidOwnerDisplayMaterial<'a> {
    schema: &'a str,
    proposal_id: &'a str,
    owner_generation: u64,
    previous_receipt_digest: Option<&'a str>,
    desktop_identity_digest: &'a str,
    principal: &'a str,
    workspace: &'a str,
    operation: AppAndroidOwnerOperation,
    enrollment_id: Option<&'a str>,
    target_ref: &'a str,
    device_label: &'a str,
    key_id: &'a str,
    automation_identity_digest: &'a str,
    owner_app_package: &'a str,
    app_version_code: u64,
    app_signing_sha256: &'a str,
    apk_sha256: &'a str,
    attestation_root_sha256: &'a str,
    attestation_security_level: AppAndroidAttestationSecurityLevel,
    attestation_policy_digest: &'a str,
    expected_review_generation: u64,
    resulting_review_generation: u64,
    actions: &'a [AppAndroidOwnerAction],
    allowed_packages: &'a [String],
    issued_at_ms: i64,
    expires_at_ms: i64,
}

/// Durable, desktop-signed transition. It repeats the complete authority
/// material so a status snapshot remains useful even if the runtime proposal
/// was lost. `proposal_digest` prevents rebinding it to a different display.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerReceipt {
    pub schema: String,
    pub proposal_digest: String,
    pub proposal_id: String,
    pub owner_generation: u64,
    pub previous_receipt_digest: Option<String>,
    pub desktop_identity_digest: String,
    pub principal: String,
    pub workspace: String,
    pub operation: AppAndroidOwnerOperation,
    pub enrollment_id: Option<String>,
    pub target_ref: String,
    pub device_label: String,
    pub key_id: String,
    pub automation_identity_digest: String,
    /// Independently attested Magdroid owner package. Reviewed target
    /// packages remain exclusively in `allowed_packages`.
    pub owner_app_package: String,
    pub app_version_code: u64,
    pub app_signing_sha256: String,
    pub apk_sha256: String,
    pub attestation_root_sha256: String,
    pub attestation_security_level: AppAndroidAttestationSecurityLevel,
    pub attestation_policy_digest: String,
    pub expected_review_generation: u64,
    pub resulting_review_generation: u64,
    pub actions: Vec<AppAndroidOwnerAction>,
    pub allowed_packages: Vec<String>,
    pub display_digest: String,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub signed_at_ms: i64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppAndroidOwnerReceipt {
    pub fn unsigned(
        proposal: &AppAndroidOwnerProposal,
        desktop_identity_key_id: String,
        signed_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        proposal.validate(signed_at_ms)?;
        validate_token(&desktop_identity_key_id, 96)?;
        Ok(Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            proposal_digest: proposal.digest()?,
            proposal_id: proposal.proposal_id.clone(),
            owner_generation: proposal.owner_generation,
            previous_receipt_digest: proposal.previous_receipt_digest.clone(),
            desktop_identity_digest: proposal.desktop_identity_digest.clone(),
            principal: proposal.principal.clone(),
            workspace: proposal.workspace.clone(),
            operation: proposal.operation,
            enrollment_id: proposal.enrollment_id.clone(),
            target_ref: proposal.target_ref.clone(),
            device_label: proposal.device_label.clone(),
            key_id: proposal.key_id.clone(),
            automation_identity_digest: proposal.automation_identity_digest.clone(),
            owner_app_package: proposal.owner_app_package.clone(),
            app_version_code: proposal.app_version_code,
            app_signing_sha256: proposal.app_signing_sha256.clone(),
            apk_sha256: proposal.apk_sha256.clone(),
            attestation_root_sha256: proposal.attestation_root_sha256.clone(),
            attestation_security_level: proposal.attestation_security_level,
            attestation_policy_digest: proposal.attestation_policy_digest.clone(),
            expected_review_generation: proposal.expected_review_generation,
            resulting_review_generation: proposal.resulting_review_generation,
            actions: proposal.actions.clone(),
            allowed_packages: proposal.allowed_packages.clone(),
            display_digest: proposal.display_digest.clone(),
            issued_at_ms: proposal.issued_at_ms,
            expires_at_ms: proposal.expires_at_ms,
            signed_at_ms,
            desktop_identity_key_id,
            desktop_identity_signature_hex: String::new(),
        })
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppAndroidOwnerError> {
        self.validate_self_shape()?;
        serde_json::to_vec(&(RECEIPT_SIGNATURE_DOMAIN, self.unsigned_material()))
            .map_err(|_| AppAndroidOwnerError::Encoding)
    }

    pub fn verify(
        &self,
        proposal: &AppAndroidOwnerProposal,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_public_key_hex: &str,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerError> {
        proposal.validate_shape(self.signed_at_ms, true)?;
        if self.signed_at_ms < proposal.issued_at_ms
            || self.signed_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
            || self.proposal_digest != proposal.digest()?
            || self.proposal_id != proposal.proposal_id
            || self.owner_generation != proposal.owner_generation
            || self.previous_receipt_digest != proposal.previous_receipt_digest
            || self.desktop_identity_digest != proposal.desktop_identity_digest
            || self.principal != proposal.principal
            || self.workspace != proposal.workspace
            || self.operation != proposal.operation
            || self.enrollment_id != proposal.enrollment_id
            || self.target_ref != proposal.target_ref
            || self.device_label != proposal.device_label
            || self.key_id != proposal.key_id
            || self.automation_identity_digest != proposal.automation_identity_digest
            || self.owner_app_package != proposal.owner_app_package
            || self.app_version_code != proposal.app_version_code
            || self.app_signing_sha256 != proposal.app_signing_sha256
            || self.apk_sha256 != proposal.apk_sha256
            || self.attestation_root_sha256 != proposal.attestation_root_sha256
            || self.attestation_security_level != proposal.attestation_security_level
            || self.attestation_policy_digest != proposal.attestation_policy_digest
            || self.expected_review_generation != proposal.expected_review_generation
            || self.resulting_review_generation != proposal.resulting_review_generation
            || self.actions != proposal.actions
            || self.allowed_packages != proposal.allowed_packages
            || self.display_digest != proposal.display_digest
            || self.issued_at_ms != proposal.issued_at_ms
            || self.expires_at_ms != proposal.expires_at_ms
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        self.verify_desktop_identity(
            expected_desktop_identity_key_id,
            expected_desktop_identity_public_key_hex,
            expected_desktop_identity_digest,
            now_ms,
        )
    }

    /// Verify a receipt recovered from a signed desktop status response when
    /// the original short-lived proposal is no longer present.
    pub fn verify_desktop_identity(
        &self,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_public_key_hex: &str,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerError> {
        self.validate_self_shape()?;
        validate_desktop_identity(
            expected_desktop_identity_key_id,
            expected_desktop_identity_public_key_hex,
            expected_desktop_identity_digest,
        )?;
        if self.desktop_identity_key_id != expected_desktop_identity_key_id
            || self.desktop_identity_digest != expected_desktop_identity_digest
            || self.signed_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        verify_ed25519(
            expected_desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_self_shape()?;
        domain_digest("magician.android-apps-owner.receipt-digest.v1", self)
    }

    fn validate_self_shape(&self) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.owner_generation == 0
            || (self.owner_generation == 1) != self.previous_receipt_digest.is_none()
            || self.signed_at_ms < 0
            || self.signed_at_ms < self.issued_at_ms
            || self.signed_at_ms >= self.expires_at_ms
            || self.app_version_code == 0
            || self.allowed_packages.len() > APP_ANDROID_OWNER_MAX_PACKAGES
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_digest(&self.proposal_digest)?;
        validate_token(&self.proposal_id, 192)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_scope_component(&self.principal)?;
        validate_scope_component(&self.workspace)?;
        if let Some(value) = &self.enrollment_id {
            validate_token(value, 192)?;
        }
        validate_token(&self.target_ref, 192)?;
        if !self.target_ref.starts_with("android-device:") {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_display_text(&self.device_label, 128)?;
        validate_token(&self.key_id, 192)?;
        validate_digest(&self.automation_identity_digest)?;
        validate_package(&self.owner_app_package)?;
        validate_sha256(&self.app_signing_sha256)?;
        validate_sha256(&self.apk_sha256)?;
        validate_sha256(&self.attestation_root_sha256)?;
        validate_digest(&self.attestation_policy_digest)?;
        validate_digest(&self.display_digest)?;
        validate_token(&self.desktop_identity_key_id, 96)?;
        if let Some(value) = &self.previous_receipt_digest {
            validate_digest(value)?;
        }
        if !self
            .allowed_packages
            .windows(2)
            .all(|values| values[0] < values[1])
            || self.allowed_packages.iter().any(|value| {
                validate_package(value).is_err()
                    || value == &self.owner_app_package
                    || is_android_owner_companion_package(value)
            })
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        match self.operation {
            AppAndroidOwnerOperation::EnrollAttestedDevice => {
                if self.enrollment_id.is_none()
                    || self.expected_review_generation != 0
                    || self.resulting_review_generation != 0
                    || !self.actions.is_empty()
                    || !self.allowed_packages.is_empty()
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::ApproveActions => {
                if self.enrollment_id.is_none()
                    || self.allowed_packages.is_empty()
                    || self.actions.as_slice() != APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::RevokeActions => {
                if self.enrollment_id.is_none()
                    || self.allowed_packages.is_empty()
                    || self.actions.as_slice() != APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
            AppAndroidOwnerOperation::RevokeDevice => {
                if self.enrollment_id.is_none()
                    || !self.actions.is_empty()
                    || self.resulting_review_generation
                        != self.expected_review_generation.saturating_add(1)
                {
                    return Err(AppAndroidOwnerError::InvalidClaims);
                }
            },
        }
        let reconstructed = AppAndroidOwnerProposal {
            schema: self.schema.clone(),
            proposal_id: self.proposal_id.clone(),
            owner_generation: self.owner_generation,
            previous_receipt_digest: self.previous_receipt_digest.clone(),
            desktop_identity_digest: self.desktop_identity_digest.clone(),
            principal: self.principal.clone(),
            workspace: self.workspace.clone(),
            operation: self.operation,
            enrollment_id: self.enrollment_id.clone(),
            target_ref: self.target_ref.clone(),
            device_label: self.device_label.clone(),
            key_id: self.key_id.clone(),
            automation_identity_digest: self.automation_identity_digest.clone(),
            owner_app_package: self.owner_app_package.clone(),
            app_version_code: self.app_version_code,
            app_signing_sha256: self.app_signing_sha256.clone(),
            apk_sha256: self.apk_sha256.clone(),
            attestation_root_sha256: self.attestation_root_sha256.clone(),
            attestation_security_level: self.attestation_security_level,
            attestation_policy_digest: self.attestation_policy_digest.clone(),
            expected_review_generation: self.expected_review_generation,
            resulting_review_generation: self.resulting_review_generation,
            actions: self.actions.clone(),
            allowed_packages: self.allowed_packages.clone(),
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
            display_digest: self.display_digest.clone(),
        };
        reconstructed.validate_shape(self.signed_at_ms, true)?;
        if reconstructed.digest()? != self.proposal_digest {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        Ok(())
    }

    fn unsigned_material(&self) -> AppAndroidOwnerReceiptMaterial<'_> {
        AppAndroidOwnerReceiptMaterial {
            schema: &self.schema,
            proposal_digest: &self.proposal_digest,
            proposal_id: &self.proposal_id,
            owner_generation: self.owner_generation,
            previous_receipt_digest: self.previous_receipt_digest.as_deref(),
            desktop_identity_digest: &self.desktop_identity_digest,
            principal: &self.principal,
            workspace: &self.workspace,
            operation: self.operation,
            enrollment_id: self.enrollment_id.as_deref(),
            target_ref: &self.target_ref,
            device_label: &self.device_label,
            key_id: &self.key_id,
            automation_identity_digest: &self.automation_identity_digest,
            owner_app_package: &self.owner_app_package,
            app_version_code: self.app_version_code,
            app_signing_sha256: &self.app_signing_sha256,
            apk_sha256: &self.apk_sha256,
            attestation_root_sha256: &self.attestation_root_sha256,
            attestation_security_level: self.attestation_security_level,
            attestation_policy_digest: &self.attestation_policy_digest,
            expected_review_generation: self.expected_review_generation,
            resulting_review_generation: self.resulting_review_generation,
            actions: &self.actions,
            allowed_packages: &self.allowed_packages,
            display_digest: &self.display_digest,
            issued_at_ms: self.issued_at_ms,
            expires_at_ms: self.expires_at_ms,
            signed_at_ms: self.signed_at_ms,
            desktop_identity_key_id: &self.desktop_identity_key_id,
        }
    }
}

#[derive(Serialize)]
struct AppAndroidOwnerReceiptMaterial<'a> {
    schema: &'a str,
    proposal_digest: &'a str,
    proposal_id: &'a str,
    owner_generation: u64,
    previous_receipt_digest: Option<&'a str>,
    desktop_identity_digest: &'a str,
    principal: &'a str,
    workspace: &'a str,
    operation: AppAndroidOwnerOperation,
    enrollment_id: Option<&'a str>,
    target_ref: &'a str,
    device_label: &'a str,
    key_id: &'a str,
    automation_identity_digest: &'a str,
    owner_app_package: &'a str,
    app_version_code: u64,
    app_signing_sha256: &'a str,
    apk_sha256: &'a str,
    attestation_root_sha256: &'a str,
    attestation_security_level: AppAndroidAttestationSecurityLevel,
    attestation_policy_digest: &'a str,
    expected_review_generation: u64,
    resulting_review_generation: u64,
    actions: &'a [AppAndroidOwnerAction],
    allowed_packages: &'a [String],
    display_digest: &'a str,
    issued_at_ms: i64,
    expires_at_ms: i64,
    signed_at_ms: i64,
    desktop_identity_key_id: &'a str,
}

/// Fresh runtime nonce for a signed desktop high-water/status snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerStatusChallenge {
    pub schema: String,
    pub challenge_nonce: String,
    pub desktop_identity_digest: String,
    pub known_owner_generation: u64,
    pub known_latest_receipt_digest: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
}

impl AppAndroidOwnerStatusChallenge {
    #[allow(clippy::too_many_arguments)]
    pub fn mint(
        challenge_nonce: String,
        desktop_identity_digest: String,
        known_owner_generation: u64,
        known_latest_receipt_digest: Option<String>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        let value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            challenge_nonce,
            desktop_identity_digest,
            known_owner_generation,
            known_latest_receipt_digest,
            issued_at_ms,
            expires_at_ms,
        };
        value.validate(issued_at_ms)?;
        Ok(value)
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_ANDROID_OWNER_MAX_STATUS_LIFETIME_MS
            || self.issued_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
            || (self.known_owner_generation == 0) != self.known_latest_receipt_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_token(&self.challenge_nonce, 192)?;
        validate_digest(&self.desktop_identity_digest)?;
        if let Some(value) = &self.known_latest_receipt_digest {
            validate_digest(value)?;
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        domain_digest(STATUS_CHALLENGE_DIGEST_DOMAIN, self)
    }
}

/// Latest signed transition for one scope/target in an explicitly
/// user-approved recovery snapshot. Routine status never exposes records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerStatusRecord {
    pub target_ref: String,
    pub receipt: AppAndroidOwnerReceipt,
}

/// Desktop-signed authoritative high-water view. It deliberately exposes only
/// opaque global equality values, so an unauthenticated loopback caller cannot
/// enumerate owner scopes, labels, targets, or attestation evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerStatusReceipt {
    pub schema: String,
    pub challenge_digest: String,
    pub desktop_identity_digest: String,
    pub owner_generation: u64,
    pub latest_receipt_digest: Option<String>,
    pub signed_at_ms: i64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppAndroidOwnerStatusReceipt {
    #[allow(clippy::too_many_arguments)]
    pub fn unsigned(
        challenge: &AppAndroidOwnerStatusChallenge,
        owner_generation: u64,
        latest_receipt_digest: Option<String>,
        signed_at_ms: i64,
        desktop_identity_key_id: String,
    ) -> Result<Self, AppAndroidOwnerError> {
        challenge.validate(signed_at_ms)?;
        let value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            challenge_digest: challenge.digest()?,
            desktop_identity_digest: challenge.desktop_identity_digest.clone(),
            owner_generation,
            latest_receipt_digest,
            signed_at_ms,
            desktop_identity_key_id,
            desktop_identity_signature_hex: String::new(),
        };
        value.validate_shape()?;
        Ok(value)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppAndroidOwnerError> {
        self.validate_shape()?;
        serde_json::to_vec(&(STATUS_RECEIPT_SIGNATURE_DOMAIN, self.unsigned_material()))
            .map_err(|_| AppAndroidOwnerError::Encoding)
    }

    pub fn verify(
        &self,
        challenge: &AppAndroidOwnerStatusChallenge,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_public_key_hex: &str,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerError> {
        challenge.validate(now_ms)?;
        self.validate_shape()?;
        validate_desktop_identity(
            expected_desktop_identity_key_id,
            expected_desktop_identity_public_key_hex,
            expected_desktop_identity_digest,
        )?;
        if self.challenge_digest != challenge.digest()?
            || self.desktop_identity_digest != challenge.desktop_identity_digest
            || self.desktop_identity_digest != expected_desktop_identity_digest
            || self.desktop_identity_key_id != expected_desktop_identity_key_id
            || self.signed_at_ms < challenge.issued_at_ms
            || self.signed_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        verify_ed25519(
            expected_desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_shape()?;
        domain_digest("magician.android-apps-owner.status-receipt-digest.v1", self)
    }

    fn validate_shape(&self) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.signed_at_ms < 0
            || (self.owner_generation == 0) != self.latest_receipt_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_digest(&self.challenge_digest)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_token(&self.desktop_identity_key_id, 96)?;
        if let Some(value) = &self.latest_receipt_digest {
            validate_digest(value)?;
        }
        Ok(())
    }

    fn unsigned_material(&self) -> AppAndroidOwnerStatusReceiptMaterial<'_> {
        AppAndroidOwnerStatusReceiptMaterial {
            schema: &self.schema,
            challenge_digest: &self.challenge_digest,
            desktop_identity_digest: &self.desktop_identity_digest,
            owner_generation: self.owner_generation,
            latest_receipt_digest: self.latest_receipt_digest.as_deref(),
            signed_at_ms: self.signed_at_ms,
            desktop_identity_key_id: &self.desktop_identity_key_id,
        }
    }
}

#[derive(Serialize)]
struct AppAndroidOwnerStatusReceiptMaterial<'a> {
    schema: &'a str,
    challenge_digest: &'a str,
    desktop_identity_digest: &'a str,
    owner_generation: u64,
    latest_receipt_digest: Option<&'a str>,
    signed_at_ms: i64,
    desktop_identity_key_id: &'a str,
}

/// Short-lived material displayed by the trusted Settings UI before it asks
/// the desktop owner to disclose current signed records for runtime recovery.
/// This is intentionally distinct from routine status and must never be
/// served by the ordinary desktop loopback gateway.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerRecoveryChallenge {
    pub schema: String,
    pub recovery_nonce: String,
    pub desktop_identity_digest: String,
    pub known_owner_generation: u64,
    pub known_latest_receipt_digest: Option<String>,
    pub issued_at_ms: i64,
    pub expires_at_ms: i64,
    pub display_digest: String,
}

impl AppAndroidOwnerRecoveryChallenge {
    pub fn mint(
        recovery_nonce: String,
        desktop_identity_digest: String,
        known_owner_generation: u64,
        known_latest_receipt_digest: Option<String>,
        issued_at_ms: i64,
        expires_at_ms: i64,
    ) -> Result<Self, AppAndroidOwnerError> {
        let mut value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            recovery_nonce,
            desktop_identity_digest,
            known_owner_generation,
            known_latest_receipt_digest,
            issued_at_ms,
            expires_at_ms,
            display_digest: String::new(),
        };
        value.validate_shape(issued_at_ms)?;
        value.display_digest = value.recompute_display_digest()?;
        Ok(value)
    }

    pub fn validate(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        self.validate_shape(now_ms)?;
        if self.display_digest != self.recompute_display_digest()? {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_shape(self.issued_at_ms)?;
        if self.display_digest != self.recompute_display_digest()? {
            return Err(AppAndroidOwnerError::InvalidDigest);
        }
        domain_digest(RECOVERY_CHALLENGE_DIGEST_DOMAIN, self)
    }

    pub fn recompute_display_digest(&self) -> Result<String, AppAndroidOwnerError> {
        domain_digest(
            RECOVERY_DISPLAY_DIGEST_DOMAIN,
            &(
                self.schema.as_str(),
                self.recovery_nonce.as_str(),
                self.desktop_identity_digest.as_str(),
                self.known_owner_generation,
                self.known_latest_receipt_digest.as_deref(),
                self.issued_at_ms,
                self.expires_at_ms,
            ),
        )
    }

    fn validate_shape(&self, now_ms: i64) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.issued_at_ms < 0
            || self.expires_at_ms <= now_ms
            || self.expires_at_ms <= self.issued_at_ms
            || self.expires_at_ms.saturating_sub(self.issued_at_ms)
                > APP_ANDROID_OWNER_MAX_PROPOSAL_LIFETIME_MS
            || self.issued_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
            || (self.known_owner_generation == 0) != self.known_latest_receipt_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_token(&self.recovery_nonce, 192)?;
        validate_digest(&self.desktop_identity_digest)?;
        if let Some(value) = &self.known_latest_receipt_digest {
            validate_digest(value)?;
        }
        Ok(())
    }
}

/// Full owner state returned only by the trusted native recovery command after
/// explicit confirmation of `display_digest`. The latest signed transition
/// for each scope/target is sorted canonically, and the global high-water
/// transition must be among the returned records.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidOwnerRecoverySnapshot {
    pub schema: String,
    pub challenge_digest: String,
    pub desktop_identity_digest: String,
    pub owner_generation: u64,
    pub latest_receipt_digest: Option<String>,
    pub records: Vec<AppAndroidOwnerStatusRecord>,
    pub signed_at_ms: i64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_signature_hex: String,
}

impl AppAndroidOwnerRecoverySnapshot {
    pub fn unsigned(
        challenge: &AppAndroidOwnerRecoveryChallenge,
        owner_generation: u64,
        latest_receipt_digest: Option<String>,
        records: Vec<AppAndroidOwnerStatusRecord>,
        signed_at_ms: i64,
        desktop_identity_key_id: String,
    ) -> Result<Self, AppAndroidOwnerError> {
        challenge.validate(signed_at_ms)?;
        let value = Self {
            schema: APP_ANDROID_OWNER_V1.to_owned(),
            challenge_digest: challenge.digest()?,
            desktop_identity_digest: challenge.desktop_identity_digest.clone(),
            owner_generation,
            latest_receipt_digest,
            records,
            signed_at_ms,
            desktop_identity_key_id,
            desktop_identity_signature_hex: String::new(),
        };
        value.validate_shape()?;
        Ok(value)
    }

    pub fn signing_bytes(&self) -> Result<Vec<u8>, AppAndroidOwnerError> {
        self.validate_shape()?;
        serde_json::to_vec(&(RECOVERY_SNAPSHOT_SIGNATURE_DOMAIN, self.unsigned_material()))
            .map_err(|_| AppAndroidOwnerError::Encoding)
    }

    pub fn verify(
        &self,
        challenge: &AppAndroidOwnerRecoveryChallenge,
        expected_desktop_identity_key_id: &str,
        expected_desktop_identity_public_key_hex: &str,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerError> {
        // The explicit native owner decision is made at `signed_at_ms`;
        // delivery can be retried after expiry without authorizing another
        // potentially different recovery snapshot.
        challenge.validate(self.signed_at_ms)?;
        self.validate_shape()?;
        validate_desktop_identity(
            expected_desktop_identity_key_id,
            expected_desktop_identity_public_key_hex,
            expected_desktop_identity_digest,
        )?;
        if self.challenge_digest != challenge.digest()?
            || self.desktop_identity_digest != challenge.desktop_identity_digest
            || self.desktop_identity_digest != expected_desktop_identity_digest
            || self.desktop_identity_key_id != expected_desktop_identity_key_id
            || self.signed_at_ms < challenge.issued_at_ms
            || self.signed_at_ms > now_ms.saturating_add(APP_ANDROID_OWNER_MAX_CLOCK_SKEW_MS)
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        for record in &self.records {
            record.receipt.verify_desktop_identity(
                expected_desktop_identity_key_id,
                expected_desktop_identity_public_key_hex,
                expected_desktop_identity_digest,
                now_ms,
            )?;
        }
        verify_ed25519(
            expected_desktop_identity_public_key_hex,
            &self.signing_bytes()?,
            &self.desktop_identity_signature_hex,
        )
    }

    pub fn digest(&self) -> Result<String, AppAndroidOwnerError> {
        self.validate_shape()?;
        domain_digest(
            "magician.android-apps-owner.recovery-snapshot-digest.v1",
            self,
        )
    }

    fn validate_shape(&self) -> Result<(), AppAndroidOwnerError> {
        if self.schema != APP_ANDROID_OWNER_V1
            || self.signed_at_ms < 0
            || self.records.len() > APP_ANDROID_OWNER_MAX_STATUS_RECORDS
            || (self.owner_generation == 0) != self.latest_receipt_digest.is_none()
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        validate_digest(&self.challenge_digest)?;
        validate_digest(&self.desktop_identity_digest)?;
        validate_token(&self.desktop_identity_key_id, 96)?;
        if let Some(value) = &self.latest_receipt_digest {
            validate_digest(value)?;
        }
        let mut contains_latest = self.owner_generation == 0;
        for record in &self.records {
            record.receipt.validate_self_shape()?;
            if record.target_ref != record.receipt.target_ref
                || record.receipt.desktop_identity_digest != self.desktop_identity_digest
                || record.receipt.owner_generation > self.owner_generation
            {
                return Err(AppAndroidOwnerError::InvalidClaims);
            }
            if record.receipt.owner_generation == self.owner_generation
                && self.latest_receipt_digest.as_deref() == Some(record.receipt.digest()?.as_str())
            {
                contains_latest = true;
            }
        }
        if !contains_latest
            || !self.records.windows(2).all(|values| {
                let left = (
                    values[0].receipt.principal.as_str(),
                    values[0].receipt.workspace.as_str(),
                    values[0].target_ref.as_str(),
                );
                let right = (
                    values[1].receipt.principal.as_str(),
                    values[1].receipt.workspace.as_str(),
                    values[1].target_ref.as_str(),
                );
                left < right
            })
        {
            return Err(AppAndroidOwnerError::InvalidClaims);
        }
        Ok(())
    }

    fn unsigned_material(&self) -> AppAndroidOwnerRecoverySnapshotMaterial<'_> {
        AppAndroidOwnerRecoverySnapshotMaterial {
            schema: &self.schema,
            challenge_digest: &self.challenge_digest,
            desktop_identity_digest: &self.desktop_identity_digest,
            owner_generation: self.owner_generation,
            latest_receipt_digest: self.latest_receipt_digest.as_deref(),
            records: &self.records,
            signed_at_ms: self.signed_at_ms,
            desktop_identity_key_id: &self.desktop_identity_key_id,
        }
    }
}

#[derive(Serialize)]
struct AppAndroidOwnerRecoverySnapshotMaterial<'a> {
    schema: &'a str,
    challenge_digest: &'a str,
    desktop_identity_digest: &'a str,
    owner_generation: u64,
    latest_receipt_digest: Option<&'a str>,
    records: &'a [AppAndroidOwnerStatusRecord],
    signed_at_ms: i64,
    desktop_identity_key_id: &'a str,
}

fn validate_desktop_identity(
    expected_key_id: &str,
    expected_public_key_hex: &str,
    expected_digest: &str,
) -> Result<(), AppAndroidOwnerError> {
    let actual = app_macos_desktop_identity_digest(expected_key_id, expected_public_key_hex)
        .map_err(|_| AppAndroidOwnerError::InvalidClaims)?;
    if actual != expected_digest {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn verify_ed25519(
    public_key_hex: &str,
    message: &[u8],
    signature_hex: &str,
) -> Result<(), AppAndroidOwnerError> {
    let public_key = decode_lower_hex::<32>(public_key_hex)?;
    let signature = decode_lower_hex::<64>(signature_hex)?;
    if public_key == [0_u8; 32] {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    ring::signature::UnparsedPublicKey::new(&ring::signature::ED25519, public_key)
        .verify(message, &signature)
        .map_err(|_| AppAndroidOwnerError::InvalidSignature)
}

fn domain_digest<T: Serialize>(
    domain: &'static str,
    value: &T,
) -> Result<String, AppAndroidOwnerError> {
    let bytes = serde_json::to_vec(&(domain, value)).map_err(|_| AppAndroidOwnerError::Encoding)?;
    Ok(format!("blake3:{}", blake3::hash(&bytes).to_hex()))
}

fn validate_digest(value: &str) -> Result<(), AppAndroidOwnerError> {
    let Some(value) = value.strip_prefix("blake3:") else {
        return Err(AppAndroidOwnerError::InvalidClaims);
    };
    if value.len() != 64 || !value.bytes().all(is_lower_hex) {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn validate_sha256(value: &str) -> Result<(), AppAndroidOwnerError> {
    if value.len() != 64 || !value.bytes().all(is_lower_hex) {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn is_lower_hex(value: u8) -> bool {
    value.is_ascii_digit() || matches!(value, b'a'..=b'f')
}

fn validate_token(value: &str, maximum_bytes: usize) -> Result<(), AppAndroidOwnerError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || !value.bytes().all(|value| {
            value.is_ascii_alphanumeric()
                || matches!(value, b'_' | b'-' | b'.' | b':' | b'/' | b'@' | b'#')
        })
    {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn validate_scope_component(value: &str) -> Result<(), AppAndroidOwnerError> {
    validate_display_text(value, 192)
}

fn validate_display_text(value: &str, maximum_bytes: usize) -> Result<(), AppAndroidOwnerError> {
    if value.is_empty()
        || value.len() > maximum_bytes
        || value.trim() != value
        || value.chars().any(char::is_control)
    {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn validate_package(value: &str) -> Result<(), AppAndroidOwnerError> {
    if value.len() < 3
        || value.len() > 255
        || !value.contains('.')
        || value.split('.').any(|component| {
            component.is_empty()
                || !component
                    .bytes()
                    .next()
                    .is_some_and(|value| value.is_ascii_alphabetic())
                || !component
                    .bytes()
                    .all(|value| value.is_ascii_alphanumeric() || value == b'_')
        })
    {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    Ok(())
}

fn decode_lower_hex<const N: usize>(value: &str) -> Result<[u8; N], AppAndroidOwnerError> {
    if value.len() != N.saturating_mul(2) || !value.bytes().all(is_lower_hex) {
        return Err(AppAndroidOwnerError::InvalidClaims);
    }
    let mut output = [0_u8; N];
    for (index, slot) in output.iter_mut().enumerate() {
        *slot = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| AppAndroidOwnerError::InvalidClaims)?;
    }
    Ok(output)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAndroidOwnerError {
    InvalidClaims,
    InvalidDigest,
    InvalidSignature,
    Encoding,
}

#[cfg(test)]
mod tests {
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::*;

    fn lower_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|value| format!("{value:02x}")).collect()
    }

    fn digest(label: &str) -> String {
        format!("blake3:{}", blake3::hash(label.as_bytes()).to_hex())
    }

    fn proposal(identity_digest: String) -> AppAndroidOwnerProposal {
        AppAndroidOwnerProposal::mint(
            "proposal_1".to_owned(),
            1,
            None,
            identity_digest,
            "principal".to_owned(),
            "workspace".to_owned(),
            AppAndroidOwnerOperation::ApproveActions,
            Some("enrollment_1".to_owned()),
            "android-device:opaque".to_owned(),
            "Pixel".to_owned(),
            "android-key:opaque".to_owned(),
            digest("identity"),
            "ai.magicbeans.magdroid".to_owned(),
            11,
            "11".repeat(32),
            "22".repeat(32),
            "33".repeat(32),
            AppAndroidAttestationSecurityLevel::Tee,
            digest("policy"),
            0,
            1,
            APP_ANDROID_OWNER_ACTION_ROSTER.to_vec(),
            vec!["com.example.target".to_owned()],
            1_000,
            61_000,
        )
        .expect("proposal")
    }

    #[test]
    fn begin_enrollment_native_request_requires_the_typed_body_digest() {
        let identity = digest("desktop-identity");
        let body = digest("private-build-trust-mode");

        AppAndroidOwnerNativeRequest::unsigned(
            AppAndroidOwnerNativeOperation::BeginEnrollment,
            "request-1".to_owned(),
            Some(body),
            1_000,
            21_000,
            identity.clone(),
            "desktop-key".to_owned(),
        )
        .expect("typed enrollment request");

        assert_eq!(
            AppAndroidOwnerNativeRequest::unsigned(
                AppAndroidOwnerNativeOperation::BeginEnrollment,
                "request-2".to_owned(),
                None,
                1_000,
                21_000,
                identity,
                "desktop-key".to_owned(),
            ),
            Err(AppAndroidOwnerError::InvalidClaims),
        );
    }

    #[test]
    fn receipt_binds_every_displayed_field_and_rejects_substitution() {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("key");
        let key = Ed25519KeyPair::from_pkcs8(document.as_ref()).expect("decode");
        let public = lower_hex(key.public_key().as_ref());
        let key_id = "desktop-key".to_owned();
        let identity = app_macos_desktop_identity_digest(&key_id, &public).expect("identity");
        let proposal = proposal(identity.clone());
        let mut receipt =
            AppAndroidOwnerReceipt::unsigned(&proposal, key_id.clone(), 2_000).expect("receipt");
        receipt.desktop_identity_signature_hex =
            lower_hex(key.sign(&receipt.signing_bytes().expect("bytes")).as_ref());
        receipt
            .verify(&proposal, &key_id, &public, &identity, 70_000)
            .expect("valid historical receipt");

        let mut substituted = receipt;
        substituted
            .allowed_packages
            .push("com.example.other".to_owned());
        assert_eq!(
            substituted.verify(&proposal, &key_id, &public, &identity, 70_000),
            Err(AppAndroidOwnerError::InvalidClaims)
        );
    }

    #[test]
    fn observed_package_is_distinct_from_and_cannot_be_the_owner_package() {
        let identity = digest("desktop-identity");
        let reviewed = proposal(identity.clone());
        assert_eq!(reviewed.owner_app_package, "ai.magicbeans.magdroid");
        assert_eq!(reviewed.allowed_packages, vec!["com.example.target"]);

        let mut owner_observation = reviewed.clone();
        owner_observation.allowed_packages = vec!["ai.magicbeans.magdroid".to_owned()];
        owner_observation.display_digest = owner_observation
            .recompute_display_digest()
            .expect("display digest");
        assert_eq!(
            owner_observation.validate(1_000),
            Err(AppAndroidOwnerError::InvalidClaims),
        );

        let mut magican_observation = reviewed;
        magican_observation.allowed_packages = vec!["ai.magicbeans.magican".to_owned()];
        magican_observation.display_digest = magican_observation
            .recompute_display_digest()
            .expect("display digest");
        assert_eq!(
            magican_observation.validate(1_000),
            Err(AppAndroidOwnerError::InvalidClaims),
        );
    }

    #[test]
    fn owner_cannot_sign_a_subset_or_reordered_action_roster() {
        let identity = digest("desktop-identity-roster");
        let reviewed = proposal(identity);
        assert_eq!(
            reviewed.actions.as_slice(),
            APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
        );

        let mut subset = reviewed.clone();
        subset.actions.pop();
        subset.display_digest = subset.recompute_display_digest().expect("display digest");
        assert_eq!(
            subset.validate(1_000),
            Err(AppAndroidOwnerError::InvalidClaims),
        );

        let mut reordered = reviewed;
        reordered.actions.swap(0, 1);
        reordered.display_digest = reordered
            .recompute_display_digest()
            .expect("display digest");
        assert_eq!(
            reordered.validate(1_000),
            Err(AppAndroidOwnerError::InvalidClaims),
        );
    }

    #[test]
    fn routine_status_is_nonce_bound_and_discloses_only_opaque_high_water() {
        let document = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("key");
        let key = Ed25519KeyPair::from_pkcs8(document.as_ref()).expect("decode");
        let public = lower_hex(key.public_key().as_ref());
        let key_id = "desktop-key".to_owned();
        let identity = app_macos_desktop_identity_digest(&key_id, &public).expect("identity");
        let proposal = proposal(identity.clone());
        let mut receipt =
            AppAndroidOwnerReceipt::unsigned(&proposal, key_id.clone(), 2_000).expect("receipt");
        receipt.desktop_identity_signature_hex =
            lower_hex(key.sign(&receipt.signing_bytes().expect("bytes")).as_ref());
        let latest = receipt.digest().expect("digest");
        let challenge = AppAndroidOwnerStatusChallenge::mint(
            "nonce_1".to_owned(),
            identity.clone(),
            1,
            Some(latest.clone()),
            3_000,
            33_000,
        )
        .expect("challenge");
        let mut status = AppAndroidOwnerStatusReceipt::unsigned(
            &challenge,
            1,
            Some(latest),
            4_000,
            key_id.clone(),
        )
        .expect("status");
        status.desktop_identity_signature_hex =
            lower_hex(key.sign(&status.signing_bytes().expect("bytes")).as_ref());
        status
            .verify(&challenge, &key_id, &public, &identity, 5_000)
            .expect("status valid");

        let other = AppAndroidOwnerStatusChallenge::mint(
            "nonce_2".to_owned(),
            identity.clone(),
            1,
            status.latest_receipt_digest.clone(),
            3_000,
            33_000,
        )
        .expect("challenge");
        assert_eq!(
            status.verify(&other, &key_id, &public, &identity, 5_000),
            Err(AppAndroidOwnerError::InvalidClaims)
        );
    }
}

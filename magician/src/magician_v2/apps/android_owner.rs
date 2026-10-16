//! Durable runtime half of the Android Apps desktop-owner authority.
//!
//! Ordinary HTTP identity is never an owner. The runtime first persists an
//! identity-free nonce, consumes it into one exact Keychain identity
//! challenge, and pins only a verified Ed25519 attestation. Native requests
//! and monotonic receipts are then accepted under that exact identity.

use std::{
    fs::OpenOptions,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
};

use base64::Engine as _;
use chrono::Utc;
use magician_app_contract::{
    android_owner::{
        app_android_owner_bootstrap_challenge, AppAndroidOwnerBootstrapChallengeRequest,
        AppAndroidOwnerBootstrapCompletion, AppAndroidOwnerBootstrapNonce,
        AppAndroidOwnerBootstrapRebindOffer, AppAndroidOwnerBootstrapRecoveryHello,
        AppAndroidOwnerBootstrapStatus, AppAndroidOwnerNativeOperation,
        AppAndroidOwnerNativeRequest, AppAndroidOwnerOperation, AppAndroidOwnerProposal,
        AppAndroidOwnerReceipt, AppAndroidOwnerRecoveryChallenge, AppAndroidOwnerRecoverySnapshot,
        AppAndroidOwnerStatusChallenge, AppAndroidOwnerStatusReceipt,
        APP_ANDROID_OWNER_ACTION_ROSTER, APP_ANDROID_OWNER_MAX_STATUS_RECORDS,
    },
    macos_host::{AppMacosDesktopIdentityAttestation, AppMacosDesktopIdentityChallenge},
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Mutex};

use crate::magician_v2::{
    artifact_v2::io::write_bytes_durably_with_mode_sync,
    device_bridge::DeviceKey,
    device_pairing::{prospective_attested_automation_identity, DeviceAutomationIdentity},
};

const STORE_SCHEMA: &str = "magician.android-apps-runtime-owner.v1";
const MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
const BOOTSTRAP_LIFETIME_MS: i64 = 30_000;
const MAX_CONSUMED_NONCES: usize = 512;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct StoredBootstrap {
    request: AppAndroidOwnerBootstrapChallengeRequest,
    challenge: AppMacosDesktopIdentityChallenge,
    attestation: AppMacosDesktopIdentityAttestation,
    status: AppAndroidOwnerBootstrapStatus,
}

/// Crash-recoverable intent created only after the handset's hardware
/// attestation and before the desktop owner publishes Apps authority. Only a
/// digest of the handset-generated connection secret is retained.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAndroidPendingOwnerEnrollment {
    pub proposal: AppAndroidOwnerProposal,
    pub device_id: String,
    pub device_label: String,
    pub connection_secret_sha256: String,
    pub enrollment_secret_digest: String,
    pub exchange_request_digest: String,
    pub public_origin: String,
    pub identity: DeviceAutomationIdentity,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct AndroidOwnerDocument {
    schema: String,
    pending_nonce: Option<AppAndroidOwnerBootstrapNonce>,
    pending_request: Option<AppAndroidOwnerBootstrapChallengeRequest>,
    #[serde(default)]
    pending_rebind_offer: Option<AppAndroidOwnerBootstrapRebindOffer>,
    bootstrap: Option<StoredBootstrap>,
    pending_recovery: Option<AppAndroidOwnerRecoveryChallenge>,
    #[serde(default)]
    pending_enrollment: Option<AppAndroidPendingOwnerEnrollment>,
    owner_generation: u64,
    latest_receipt_digest: Option<String>,
    latest_receipt: Option<AppAndroidOwnerReceipt>,
    receipts: Vec<AppAndroidOwnerReceipt>,
    consumed_native_nonces: Vec<(String, i64)>,
}

impl Default for AndroidOwnerDocument {
    fn default() -> Self {
        Self {
            schema: STORE_SCHEMA.to_owned(),
            pending_nonce: None,
            pending_request: None,
            pending_rebind_offer: None,
            bootstrap: None,
            pending_recovery: None,
            pending_enrollment: None,
            owner_generation: 0,
            latest_receipt_digest: None,
            latest_receipt: None,
            receipts: Vec::new(),
            consumed_native_nonces: Vec::new(),
        }
    }
}

struct StoreState {
    document: AndroidOwnerDocument,
}

struct StoreInner {
    path: PathBuf,
    staged_path: PathBuf,
    unavailable_reason: Option<String>,
    peer_identity_verified: AtomicBool,
    peer_verified: AtomicBool,
    state: Mutex<StoreState>,
}

#[derive(Clone)]
pub struct AppAndroidOwnerStore {
    inner: Arc<StoreInner>,
}

impl std::fmt::Debug for AppAndroidOwnerStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppAndroidOwnerStore")
            .field("path", &self.inner.path)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppAndroidOwnerStoreError {
    Unavailable,
    InvalidBootstrap,
    BootstrapAlreadyPinned,
    NativeAuthorizationRejected,
    StaleOwnerGeneration,
    InvalidReceipt,
    Io(String),
}

impl std::fmt::Display for AppAndroidOwnerStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for AppAndroidOwnerStoreError {}

static GLOBAL_ANDROID_OWNER_STORE: OnceLock<Arc<AppAndroidOwnerStore>> = OnceLock::new();

pub fn install_global_android_owner_store(store: Arc<AppAndroidOwnerStore>) {
    let _ = GLOBAL_ANDROID_OWNER_STORE.set(store);
}

pub(crate) fn global_android_owner_store() -> Option<Arc<AppAndroidOwnerStore>> {
    GLOBAL_ANDROID_OWNER_STORE.get().cloned()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AppAndroidOwnerSnapshotFence {
    pub receipt_generation: u64,
    pub receipt_digest: String,
    pub desktop_identity_digest: String,
    pub desktop_identity_attestation_digest: String,
}

impl AppAndroidOwnerStore {
    pub async fn open(system_root: &Path) -> Result<Self, AppAndroidOwnerStoreError> {
        let root = system_root.join("android-apps-owner");
        let path = root.join("authority.json");
        let staged_path = root.join("authority.staged.json");
        let document = tokio::task::spawn_blocking({
            let root = root.clone();
            let path = path.clone();
            let staged_path = staged_path.clone();
            move || load_document(&root, &path, &staged_path)
        })
        .await
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))??;
        validate_document(&document, Utc::now().timestamp_millis())?;
        Ok(Self {
            inner: Arc::new(StoreInner {
                path,
                staged_path,
                unavailable_reason: None,
                peer_identity_verified: AtomicBool::new(false),
                peer_verified: AtomicBool::new(false),
                state: Mutex::new(StoreState { document }),
            }),
        })
    }

    pub fn unavailable(system_root: &Path, reason: impl Into<String>) -> Self {
        let root = system_root.join("android-apps-owner");
        Self {
            inner: Arc::new(StoreInner {
                path: root.join("authority.json"),
                staged_path: root.join("authority.staged.json"),
                unavailable_reason: Some(reason.into()),
                peer_identity_verified: AtomicBool::new(false),
                peer_verified: AtomicBool::new(false),
                state: Mutex::new(StoreState {
                    document: AndroidOwnerDocument::default(),
                }),
            }),
        }
    }

    pub fn is_available(&self) -> bool {
        self.inner.unavailable_reason.is_none()
    }

    pub fn is_peer_verified(&self) -> bool {
        self.is_available() && self.inner.peer_verified.load(Ordering::Acquire)
    }

    pub(crate) fn is_peer_identity_verified(&self) -> bool {
        self.is_available() && self.inner.peer_identity_verified.load(Ordering::Acquire)
    }

    pub(crate) fn mark_peer_identity_verified(&self) {
        self.inner
            .peer_identity_verified
            .store(true, Ordering::Release);
    }

    pub(crate) fn mark_peer_verified(&self) {
        self.mark_peer_identity_verified();
        self.inner.peer_verified.store(true, Ordering::Release);
    }

    pub(crate) fn mark_peer_stale(&self) {
        self.inner.peer_verified.store(false, Ordering::Release);
    }

    /// Mint a short-lived, content-free challenge for the desktop owner's
    /// signed global high-water. This is deliberately usable before the
    /// process-local peer latch is raised: reconciling this value is what
    /// proves that a locally rolled-back authority document is not current.
    pub(crate) async fn status_challenge(
        &self,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerStatusChallenge, AppAndroidOwnerStoreError> {
        if !self.is_available() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let state = self.inner.state.lock().await;
        let binding = state
            .document
            .bootstrap
            .as_ref()
            .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
        let mut random = [0_u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut random);
        AppAndroidOwnerStatusChallenge::mint(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random),
            binding.status.desktop_identity_digest.clone(),
            state.document.owner_generation,
            state.document.latest_receipt_digest.clone(),
            now_ms,
            now_ms.saturating_add(BOOTSTRAP_LIFETIME_MS),
        )
        .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)
    }

    /// Verify and compare the desktop-signed global high-water to the exact
    /// local authority head. Equality is required in both directions: a
    /// rolled-back local file and an uncommitted desktop transition both keep
    /// Android Apps authority unavailable until explicit recovery converges.
    pub(crate) async fn reconcile_status(
        &self,
        challenge: &AppAndroidOwnerStatusChallenge,
        receipt: &AppAndroidOwnerStatusReceipt,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        let empty_rebind = {
            let state = self.inner.state.lock().await;
            let binding = state
                .document
                .bootstrap
                .as_ref()
                .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
            receipt
                .verify(
                    challenge,
                    &binding.status.desktop_identity_key_id,
                    &binding.status.desktop_identity_public_key_hex,
                    &binding.status.desktop_identity_digest,
                    now_ms,
                )
                .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
            if let Some(offer) = state.document.pending_rebind_offer.as_ref() {
                if receipt.owner_generation != offer.owner_generation
                    || receipt.latest_receipt_digest != offer.latest_receipt_digest
                {
                    return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                }
                // A surviving desktop may have a finalized identity but no
                // Android authority transition at all. There is no recovery
                // document to import in that state: the exact signed empty
                // global head is the complete reconciliation proof.
                if offer.owner_generation == 0
                    && offer.latest_receipt_digest.is_none()
                    && state.document.owner_generation == 0
                    && state.document.latest_receipt_digest.is_none()
                    && state.document.receipts.is_empty()
                {
                    Some(offer.clone())
                } else {
                    return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                }
            } else {
                if receipt.owner_generation != state.document.owner_generation
                    || receipt.latest_receipt_digest != state.document.latest_receipt_digest
                {
                    return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                }
                None
            }
        };
        if let Some(expected_offer) = empty_rebind {
            self.mutate(move |document| {
                if document.pending_rebind_offer.as_ref() != Some(&expected_offer)
                    || document.owner_generation != 0
                    || document.latest_receipt_digest.is_some()
                    || !document.receipts.is_empty()
                {
                    return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                }
                document.pending_rebind_offer = None;
                document.pending_recovery = None;
                Ok(())
            })
            .await?;
        }
        Ok(())
    }

    pub async fn bootstrap_status(&self) -> Option<AppAndroidOwnerBootstrapStatus> {
        self.inner
            .state
            .lock()
            .await
            .document
            .bootstrap
            .as_ref()
            .map(|binding| binding.status.clone())
    }

    /// Exact runtime-offered nonce used to resume the UDS handshake. Active
    /// bindings retain it so a lost Finalized frame can be replayed after a
    /// runtime or desktop restart without minting another identity decision.
    pub async fn bootstrap_hello(
        &self,
        now_ms: i64,
    ) -> Result<Option<AppAndroidOwnerBootstrapNonce>, AppAndroidOwnerStoreError> {
        let candidate = {
            let state = self.inner.state.lock().await;
            if let Some(binding) = &state.document.bootstrap {
                return Ok(Some(binding.request.bootstrap.clone()));
            }
            if let Some(request) = &state.document.pending_request {
                // A desktop may already have durably signed Completion after
                // the runtime sent Challenge. Replay this exact correlation
                // until the authenticated desktop proves it is unspent.
                return Ok(Some(request.bootstrap.clone()));
            }
            if let Some(offer) = &state.document.pending_rebind_offer {
                // As with a retained ChallengeRequest, the desktop may have
                // crossed its durable Completion boundary after the recovery
                // hello. Only its authenticated ExpiredUnspent response may
                // clear this historical correlation.
                return Ok(Some(offer.bootstrap.clone()));
            }
            state.document.pending_nonce.clone()
        };
        let Some(candidate) = candidate else {
            return Ok(None);
        };
        if candidate.expires_at_ms > now_ms {
            return Ok(Some(candidate));
        }
        self.mutate(move |document| {
            if document.pending_nonce.as_ref() == Some(&candidate)
                && candidate.expires_at_ms <= now_ms
            {
                document.pending_nonce = None;
            }
            Ok(None)
        })
        .await
    }

    pub(crate) async fn bootstrap_recovery_hello(
        &self,
        bootstrap: &AppAndroidOwnerBootstrapNonce,
    ) -> Result<Option<AppAndroidOwnerBootstrapRecoveryHello>, AppAndroidOwnerStoreError> {
        let state = self.inner.state.lock().await;
        let Some(offer) = state.document.pending_rebind_offer.as_ref() else {
            return Ok(None);
        };
        if &offer.bootstrap != bootstrap {
            return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
        }
        let rebind_offer_digest = offer
            .digest()
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        let recovery = AppAndroidOwnerBootstrapRecoveryHello {
            bootstrap: bootstrap.clone(),
            rebind_offer_digest,
            expected_desktop_identity_digest: offer.desktop_identity_digest.clone(),
        };
        recovery
            .validate()
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        Ok(Some(recovery))
    }

    pub(crate) async fn retain_rebind_offer(
        &self,
        offer: AppAndroidOwnerBootstrapRebindOffer,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        offer
            .validate(now_ms)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        self.mutate(move |document| {
            if document.bootstrap.is_some() || document.pending_request.is_some() {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            if let Some(existing) = document.pending_rebind_offer.as_ref() {
                return (existing == &offer)
                    .then_some(())
                    .ok_or(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            if document.pending_nonce.as_ref() != Some(&offer.bootstrap) {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            document.pending_rebind_offer = Some(offer);
            Ok(())
        })
        .await
    }

    pub async fn abandon_expired_request(
        &self,
        bootstrap: AppAndroidOwnerBootstrapNonce,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        self.mutate(move |document| {
            if document.bootstrap.is_some() {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            let request = document
                .pending_request
                .as_ref()
                .filter(|request| request.bootstrap == bootstrap);
            let rebind = document
                .pending_rebind_offer
                .as_ref()
                .filter(|offer| offer.bootstrap == bootstrap);
            if (request.is_none() && rebind.is_none()) || bootstrap.expires_at_ms > now_ms {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            document.pending_request = None;
            document.pending_rebind_offer = None;
            document.pending_nonce = None;
            Ok(())
        })
        .await
    }

    pub async fn begin_bootstrap(
        &self,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapNonce, AppAndroidOwnerStoreError> {
        let mut random = [0_u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let nonce = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random);
        let bootstrap = AppAndroidOwnerBootstrapNonce::mint(
            nonce,
            now_ms,
            now_ms.saturating_add(BOOTSTRAP_LIFETIME_MS),
        )
        .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        self.mutate(move |document| {
            if document.bootstrap.is_some() {
                return Err(AppAndroidOwnerStoreError::BootstrapAlreadyPinned);
            }
            if let Some(pending) = document
                .pending_nonce
                .as_ref()
                .filter(|pending| pending.expires_at_ms > now_ms)
            {
                return Ok(pending.clone());
            }
            if document
                .pending_request
                .as_ref()
                .is_some_and(|pending| pending.bootstrap.expires_at_ms > now_ms)
            {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            document.pending_nonce = None;
            document.pending_request = None;
            document.pending_nonce = Some(bootstrap.clone());
            Ok(bootstrap)
        })
        .await
    }

    pub async fn mint_bootstrap_challenge(
        &self,
        request: AppAndroidOwnerBootstrapChallengeRequest,
        now_ms: i64,
    ) -> Result<AppMacosDesktopIdentityChallenge, AppAndroidOwnerStoreError> {
        request
            .validate(now_ms)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        let challenge = app_android_owner_bootstrap_challenge(&request)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        self.mutate(move |document| {
            let ordinary = document.pending_nonce.as_ref() == Some(&request.bootstrap)
                && document.pending_rebind_offer.is_none();
            let recovery = document.pending_rebind_offer.as_ref().is_some_and(|offer| {
                offer.bootstrap == request.bootstrap
                    && offer.desktop_identity_key_id == request.desktop_identity_key_id
                    && offer.desktop_identity_digest == request.desktop_identity_digest
            });
            if document.bootstrap.is_some()
                || (!ordinary && !recovery)
                || document.pending_request.is_some()
            {
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            document.pending_nonce = None;
            document.pending_request = Some(request);
            Ok(challenge)
        })
        .await
    }

    pub async fn complete_bootstrap(
        &self,
        completion: AppAndroidOwnerBootstrapCompletion,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapStatus, AppAndroidOwnerStoreError> {
        let status = AppAndroidOwnerBootstrapStatus::from_verified(
            &completion.challenge,
            &completion.attestation,
            now_ms,
        )
        .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        self.mutate(move |document| {
            if let Some(existing) = document.bootstrap.as_ref() {
                if existing.challenge == completion.challenge
                    && existing.attestation == completion.attestation
                    && existing.status.desktop_identity_key_id == status.desktop_identity_key_id
                    && existing.status.desktop_identity_attestation_digest
                        == status.desktop_identity_attestation_digest
                {
                    return Ok(existing.status.clone());
                }
                return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
            }
            let request = if let Some(request) = document.pending_request.as_ref() {
                if app_android_owner_bootstrap_challenge(request).ok().as_ref()
                    != Some(&completion.challenge)
                {
                    return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
                }
                request.clone()
            } else {
                let Some(pending) = document.pending_nonce.as_ref() else {
                    return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
                };
                let recovered_request = AppAndroidOwnerBootstrapChallengeRequest {
                    bootstrap: pending.clone(),
                    desktop_identity_key_id: completion.challenge.desktop_identity_key_id.clone(),
                    desktop_identity_public_key_hex: completion
                        .challenge
                        .desktop_identity_public_key_hex
                        .clone(),
                    desktop_identity_digest: status.desktop_identity_digest.clone(),
                    owner_approval_code_digest: completion
                        .challenge
                        .owner_approval_code_digest
                        .clone(),
                };
                if app_android_owner_bootstrap_challenge(&recovered_request)
                    .ok()
                    .as_ref()
                    != Some(&completion.challenge)
                {
                    return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
                }
                recovered_request
            };
            document.bootstrap = Some(StoredBootstrap {
                request,
                challenge: completion.challenge,
                attestation: completion.attestation,
                status: status.clone(),
            });
            document.pending_request = None;
            document.pending_nonce = None;
            Ok(status)
        })
        .await
    }

    pub async fn authorize_native(
        &self,
        authorization: &AppAndroidOwnerNativeRequest,
        operation: AppAndroidOwnerNativeOperation,
        body_digest: Option<&str>,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        let recovery_operation = matches!(
            operation,
            AppAndroidOwnerNativeOperation::BeginRecovery
                | AppAndroidOwnerNativeOperation::SubmitRecovery
        );
        if !self.is_peer_identity_verified() || (!recovery_operation && !self.is_peer_verified()) {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let binding = self
            .inner
            .state
            .lock()
            .await
            .document
            .bootstrap
            .as_ref()
            .map(|binding| binding.status.clone())
            .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
        authorization
            .verify(
                operation,
                body_digest,
                &binding.desktop_identity_key_id,
                &binding.desktop_identity_public_key_hex,
                &binding.desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| AppAndroidOwnerStoreError::NativeAuthorizationRejected)?;
        let nonce = authorization.request_nonce.clone();
        let expires_at_ms = authorization.expires_at_ms;
        self.mutate(move |document| {
            document
                .consumed_native_nonces
                .retain(|(_, expires_at)| *expires_at > now_ms);
            if document
                .consumed_native_nonces
                .iter()
                .any(|(consumed, _)| consumed == &nonce)
                || document.consumed_native_nonces.len() >= MAX_CONSUMED_NONCES
            {
                return Err(AppAndroidOwnerStoreError::NativeAuthorizationRejected);
            }
            document.consumed_native_nonces.push((nonce, expires_at_ms));
            Ok(())
        })
        .await
    }

    pub async fn owner_head(
        &self,
    ) -> Result<(u64, Option<String>, AppAndroidOwnerBootstrapStatus), AppAndroidOwnerStoreError>
    {
        if !self.is_peer_identity_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let state = self.inner.state.lock().await;
        let bootstrap = state
            .document
            .bootstrap
            .as_ref()
            .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
        Ok((
            state.document.owner_generation,
            state.document.latest_receipt_digest.clone(),
            bootstrap.status.clone(),
        ))
    }

    /// Persist one exact post-attestation enrollment intent before it is
    /// exposed to the native owner. A byte-identical retry is idempotent;
    /// competing proposals or a stale desktop head fail closed.
    pub async fn stage_pending_enrollment(
        &self,
        pending: AppAndroidPendingOwnerEnrollment,
        now_ms: i64,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        validate_pending_enrollment(&pending, now_ms, true)?;
        self.mutate(move |document| {
            let bootstrap = document
                .bootstrap
                .as_ref()
                .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
            if pending.proposal.desktop_identity_digest != bootstrap.status.desktop_identity_digest
                || pending.proposal.owner_generation != document.owner_generation.saturating_add(1)
                || pending.proposal.previous_receipt_digest != document.latest_receipt_digest
            {
                return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
            }
            if let Some(existing) = document.pending_enrollment.as_ref() {
                return (existing == &pending)
                    .then_some(())
                    .ok_or(AppAndroidOwnerStoreError::StaleOwnerGeneration);
            }
            document.pending_enrollment = Some(pending);
            Ok(())
        })
        .await
    }

    pub async fn pending_enrollment(
        &self,
    ) -> Result<Option<AppAndroidPendingOwnerEnrollment>, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        Ok(self
            .inner
            .state
            .lock()
            .await
            .document
            .pending_enrollment
            .clone())
    }

    /// True only after the exact desktop receipt for the retained enrollment
    /// intent is durably part of this runtime's owner chain.
    pub async fn pending_enrollment_is_approved(
        &self,
        proposal_digest: &str,
    ) -> Result<bool, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let state = self.inner.state.lock().await;
        let Some(pending) = state.document.pending_enrollment.as_ref() else {
            return Ok(false);
        };
        let digest = pending
            .proposal
            .digest()
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
        Ok(digest == proposal_digest
            && state
                .document
                .latest_receipt
                .as_ref()
                .is_some_and(|receipt| {
                    receipt.operation == AppAndroidOwnerOperation::EnrollAttestedDevice
                        && receipt.proposal_digest == digest
                }))
    }

    pub async fn clear_pending_enrollment(
        &self,
        proposal_digest: &str,
    ) -> Result<(), AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let proposal_digest = proposal_digest.to_owned();
        self.mutate(move |document| {
            let Some(pending) = document.pending_enrollment.as_ref() else {
                return Ok(());
            };
            if pending.proposal.digest().ok().as_deref() != Some(proposal_digest.as_str())
                || document
                    .latest_receipt
                    .as_ref()
                    .is_none_or(|receipt| receipt.proposal_digest != proposal_digest)
            {
                return Err(AppAndroidOwnerStoreError::InvalidReceipt);
            }
            document.pending_enrollment = None;
            Ok(())
        })
        .await
    }

    /// Clear an expired intent only after the caller has just obtained an
    /// exact signed desktop high-water equal to the proposal predecessor.
    /// A locally delivered or remotely surviving receipt therefore wins over
    /// wall-clock cleanup.
    pub async fn clear_expired_unapproved_enrollment(
        &self,
        proposal_digest: &str,
        now_ms: i64,
    ) -> Result<bool, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let proposal_digest = proposal_digest.to_owned();
        self.mutate(move |document| {
            let Some(pending) = document.pending_enrollment.as_ref() else {
                return Ok(false);
            };
            if !expired_enrollment_is_proven_unspent(
                &pending.proposal,
                &proposal_digest,
                document.owner_generation,
                document.latest_receipt_digest.as_deref(),
                document
                    .latest_receipt
                    .as_ref()
                    .map(|receipt| receipt.proposal_digest.as_str()),
                now_ms,
            ) {
                return Err(AppAndroidOwnerStoreError::InvalidReceipt);
            }
            document.pending_enrollment = None;
            Ok(true)
        })
        .await
    }

    /// Consume an explicit, desktop-signed Settings cancellation after the
    /// caller has revalidated the exact desktop predecessor head.
    pub async fn cancel_unapproved_enrollment(
        &self,
        enrollment_id: &str,
    ) -> Result<bool, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let enrollment_id = enrollment_id.to_owned();
        self.mutate(move |document| {
            let Some(pending) = document.pending_enrollment.as_ref() else {
                return Ok(false);
            };
            if pending.proposal.enrollment_id.as_deref() != Some(enrollment_id.as_str())
                || pending.proposal.owner_generation != document.owner_generation.saturating_add(1)
                || pending.proposal.previous_receipt_digest != document.latest_receipt_digest
                || document.latest_receipt.as_ref().is_some_and(|receipt| {
                    pending.proposal.digest().ok().as_deref()
                        == Some(receipt.proposal_digest.as_str())
                })
            {
                return Err(AppAndroidOwnerStoreError::InvalidReceipt);
            }
            document.pending_enrollment = None;
            Ok(true)
        })
        .await
    }

    pub(crate) async fn require_snapshot_receipt(
        &self,
        principal: &str,
        workspace: &str,
        target_ref: &str,
        review_generation: u64,
        allowed_packages: &std::collections::BTreeSet<String>,
        automation_identity_digest: &str,
    ) -> Result<AppAndroidOwnerSnapshotFence, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let state = self.inner.state.lock().await;
        let bootstrap = state
            .document
            .bootstrap
            .as_ref()
            .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
        if state.document.pending_enrollment.is_some()
            && state
                .document
                .latest_receipt
                .as_ref()
                .is_some_and(|receipt| {
                    receipt.operation == AppAndroidOwnerOperation::EnrollAttestedDevice
                })
        {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let receipt = state
            .document
            .receipts
            .iter()
            .find(|receipt| {
                receipt.principal == principal
                    && receipt.workspace == workspace
                    && receipt.target_ref == target_ref
            })
            .ok_or(AppAndroidOwnerStoreError::InvalidReceipt)?;
        let packages = allowed_packages.iter().cloned().collect::<Vec<_>>();
        if receipt.operation != AppAndroidOwnerOperation::ApproveActions
            || receipt.actions.as_slice() != APP_ANDROID_OWNER_ACTION_ROSTER.as_slice()
            || receipt.resulting_review_generation != review_generation
            || receipt.allowed_packages != packages
            || receipt.automation_identity_digest != automation_identity_digest
        {
            return Err(AppAndroidOwnerStoreError::InvalidReceipt);
        }
        Ok(AppAndroidOwnerSnapshotFence {
            receipt_generation: receipt.owner_generation,
            receipt_digest: receipt
                .digest()
                .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?,
            desktop_identity_digest: bootstrap.status.desktop_identity_digest.clone(),
            desktop_identity_attestation_digest: bootstrap
                .status
                .desktop_identity_attestation_digest
                .clone(),
        })
    }

    pub async fn apply_receipt(
        &self,
        receipt: AppAndroidOwnerReceipt,
        now_ms: i64,
    ) -> Result<String, AppAndroidOwnerStoreError> {
        if !self.is_peer_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        self.mutate(move |document| {
            let bootstrap = document
                .bootstrap
                .as_ref()
                .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
            receipt
                .verify_desktop_identity(
                    &bootstrap.status.desktop_identity_key_id,
                    &bootstrap.status.desktop_identity_public_key_hex,
                    &bootstrap.status.desktop_identity_digest,
                    now_ms,
                )
                .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
            let digest = receipt
                .digest()
                .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
            if receipt.owner_generation == document.owner_generation
                && document.latest_receipt_digest.as_deref() == Some(digest.as_str())
            {
                return Ok(digest);
            }
            if receipt.owner_generation != document.owner_generation.saturating_add(1)
                || receipt.previous_receipt_digest != document.latest_receipt_digest
                || document.receipts.len() >= APP_ANDROID_OWNER_MAX_STATUS_RECORDS
            {
                return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
            }
            document.owner_generation = receipt.owner_generation;
            document.latest_receipt_digest = Some(digest.clone());
            document.latest_receipt = Some(receipt.clone());
            if let Some(current) = document.receipts.iter_mut().find(|current| {
                current.principal == receipt.principal
                    && current.workspace == receipt.workspace
                    && current.target_ref == receipt.target_ref
            }) {
                *current = receipt;
            } else {
                document.receipts.push(receipt);
            }
            document.receipts.sort_by(|left, right| {
                (&left.principal, &left.workspace, &left.target_ref).cmp(&(
                    &right.principal,
                    &right.workspace,
                    &right.target_ref,
                ))
            });
            Ok(digest)
        })
        .await
    }

    pub async fn begin_recovery(
        &self,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerRecoveryChallenge, AppAndroidOwnerStoreError> {
        let (generation, latest, binding) = self.owner_head().await?;
        let mut random = [0_u8; 24];
        rand::rngs::OsRng.fill_bytes(&mut random);
        let challenge = AppAndroidOwnerRecoveryChallenge::mint(
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random),
            binding.desktop_identity_digest,
            generation,
            latest,
            now_ms,
            now_ms.saturating_add(BOOTSTRAP_LIFETIME_MS),
        )
        .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
        self.mutate(move |document| {
            if let Some(existing) = document
                .pending_recovery
                .as_ref()
                .filter(|existing| existing.expires_at_ms > now_ms)
            {
                return Ok(existing.clone());
            }
            document.pending_recovery = Some(challenge.clone());
            Ok(challenge)
        })
        .await
    }

    pub async fn apply_recovery(
        &self,
        snapshot: AppAndroidOwnerRecoverySnapshot,
        now_ms: i64,
    ) -> Result<String, AppAndroidOwnerStoreError> {
        if !self.is_peer_identity_verified() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let result = self
            .mutate(move |document| {
                let bootstrap = document
                    .bootstrap
                    .as_ref()
                    .ok_or(AppAndroidOwnerStoreError::Unavailable)?;
                let challenge = document
                    .pending_recovery
                    .as_ref()
                    .ok_or(AppAndroidOwnerStoreError::InvalidReceipt)?;
                snapshot
                    .verify(
                        challenge,
                        &bootstrap.status.desktop_identity_key_id,
                        &bootstrap.status.desktop_identity_public_key_hex,
                        &bootstrap.status.desktop_identity_digest,
                        now_ms,
                    )
                    .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
                if snapshot.owner_generation < document.owner_generation
                    || (snapshot.owner_generation == document.owner_generation
                        && snapshot.latest_receipt_digest != document.latest_receipt_digest)
                {
                    return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                }
                if let Some(offer) = document.pending_rebind_offer.as_ref() {
                    if snapshot.owner_generation != offer.owner_generation
                        || snapshot.latest_receipt_digest != offer.latest_receipt_digest
                    {
                        return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
                    }
                }
                let digest = snapshot
                    .digest()
                    .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
                document.owner_generation = snapshot.owner_generation;
                document.latest_receipt_digest = snapshot.latest_receipt_digest.clone();
                document.latest_receipt = snapshot
                    .records
                    .iter()
                    .find(|record| {
                        record.receipt.owner_generation == snapshot.owner_generation
                            && record.receipt.digest().ok().as_ref()
                                == snapshot.latest_receipt_digest.as_ref()
                    })
                    .map(|record| record.receipt.clone());
                document.receipts = snapshot
                    .records
                    .into_iter()
                    .map(|record| record.receipt)
                    .collect();
                document.pending_recovery = None;
                document.pending_rebind_offer = None;
                Ok(digest)
            })
            .await?;
        self.mark_peer_verified();
        Ok(result)
    }

    async fn mutate<T, F>(&self, mutation: F) -> Result<T, AppAndroidOwnerStoreError>
    where
        T: Send + 'static,
        F: FnOnce(&mut AndroidOwnerDocument) -> Result<T, AppAndroidOwnerStoreError>
            + Send
            + 'static,
    {
        if !self.is_available() {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
        let inner = Arc::clone(&self.inner);
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(async move {
            let mut state = inner.state.lock().await;
            let mut candidate = state.document.clone();
            let result = match mutation(&mut candidate) {
                Ok(value) => {
                    let path = inner.path.clone();
                    let staged_path = inner.staged_path.clone();
                    let persisted = candidate.clone();
                    match tokio::task::spawn_blocking(move || {
                        persist_document(&path, &staged_path, &persisted)
                    })
                    .await
                    {
                        Ok(Ok(())) => {
                            state.document = candidate;
                            Ok(value)
                        },
                        Ok(Err(error)) => Err(error),
                        Err(error) => Err(AppAndroidOwnerStoreError::Io(error.to_string())),
                    }
                },
                Err(error) => Err(error),
            };
            let _ = sender.send(result);
        });
        receiver
            .await
            .map_err(|_| AppAndroidOwnerStoreError::Unavailable)?
    }
}

fn expired_enrollment_is_proven_unspent(
    proposal: &AppAndroidOwnerProposal,
    proposal_digest: &str,
    owner_generation: u64,
    latest_receipt_digest: Option<&str>,
    latest_receipt_proposal_digest: Option<&str>,
    now_ms: i64,
) -> bool {
    proposal.digest().ok().as_deref() == Some(proposal_digest)
        && proposal.expires_at_ms <= now_ms
        && proposal.owner_generation == owner_generation.saturating_add(1)
        && proposal.previous_receipt_digest.as_deref() == latest_receipt_digest
        && latest_receipt_proposal_digest != Some(proposal_digest)
}

fn validate_document(
    document: &AndroidOwnerDocument,
    now_ms: i64,
) -> Result<(), AppAndroidOwnerStoreError> {
    if document.schema != STORE_SCHEMA
        || document.receipts.len() > APP_ANDROID_OWNER_MAX_STATUS_RECORDS
        || document.consumed_native_nonces.len() > MAX_CONSUMED_NONCES
        || (document.owner_generation == 0) != document.latest_receipt_digest.is_none()
        || (document.owner_generation == 0) != document.latest_receipt.is_none()
        || (document.bootstrap.is_some()
            && (document.pending_nonce.is_some() || document.pending_request.is_some()))
        || (document.pending_nonce.is_some() && document.pending_request.is_some())
    {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    if let Some(offer) = document.pending_rebind_offer.as_ref() {
        offer
            .validate(offer.issued_at_ms)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
        let correlated = document
            .pending_nonce
            .as_ref()
            .is_some_and(|nonce| nonce == &offer.bootstrap)
            || document.pending_request.as_ref().is_some_and(|request| {
                request.bootstrap == offer.bootstrap
                    && request.desktop_identity_key_id == offer.desktop_identity_key_id
                    && request.desktop_identity_digest == offer.desktop_identity_digest
            })
            || document.bootstrap.as_ref().is_some_and(|binding| {
                binding.request.bootstrap == offer.bootstrap
                    && binding.status.desktop_identity_key_id == offer.desktop_identity_key_id
                    && binding.status.desktop_identity_digest == offer.desktop_identity_digest
            });
        if !correlated {
            return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
        }
    }
    let Some(binding) = document.bootstrap.as_ref() else {
        return (document.owner_generation == 0
            && document.latest_receipt.is_none()
            && document.receipts.is_empty()
            && document.pending_recovery.is_none())
        .then_some(())
        .ok_or(AppAndroidOwnerStoreError::Unavailable);
    };
    binding
        .attestation
        .verify(&binding.challenge, binding.attestation.attested_at_ms)
        .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
    binding
        .request
        .validate(binding.request.bootstrap.issued_at_ms)
        .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
    if app_android_owner_bootstrap_challenge(&binding.request)
        .ok()
        .as_ref()
        != Some(&binding.challenge)
    {
        return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
    }
    binding
        .status
        .validate()
        .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
    let expected = AppAndroidOwnerBootstrapStatus::from_verified(
        &binding.challenge,
        &binding.attestation,
        binding.attestation.attested_at_ms,
    )
    .map_err(|_| AppAndroidOwnerStoreError::InvalidBootstrap)?;
    if expected.desktop_identity_key_id != binding.status.desktop_identity_key_id
        || expected.desktop_identity_public_key_hex
            != binding.status.desktop_identity_public_key_hex
        || expected.desktop_identity_digest != binding.status.desktop_identity_digest
        || expected.desktop_identity_attestation_digest
            != binding.status.desktop_identity_attestation_digest
        || expected.host_identity_digest != binding.status.host_identity_digest
    {
        return Err(AppAndroidOwnerStoreError::InvalidBootstrap);
    }
    if let Some(challenge) = &document.pending_recovery {
        if challenge.desktop_identity_digest != binding.status.desktop_identity_digest {
            return Err(AppAndroidOwnerStoreError::InvalidReceipt);
        }
        // A retained recovery decision may expire while the process is down;
        // it remains usable only as the exact historical signing challenge.
        challenge
            .validate(challenge.issued_at_ms)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    }
    if let Some(pending) = &document.pending_enrollment {
        validate_pending_enrollment(pending, now_ms, false)?;
        if pending.proposal.desktop_identity_digest != binding.status.desktop_identity_digest {
            return Err(AppAndroidOwnerStoreError::InvalidReceipt);
        }
        let proposal_digest = pending
            .proposal
            .digest()
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
        let approved = document.latest_receipt.as_ref().is_some_and(|receipt| {
            receipt.operation == AppAndroidOwnerOperation::EnrollAttestedDevice
                && receipt.proposal_digest == proposal_digest
        });
        if !approved
            && (pending.proposal.owner_generation != document.owner_generation.saturating_add(1)
                || pending.proposal.previous_receipt_digest != document.latest_receipt_digest)
        {
            return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
        }
    }
    if document.owner_generation == 0 {
        return (document.latest_receipt_digest.is_none()
            && document.latest_receipt.is_none()
            && document.receipts.is_empty())
        .then_some(())
        .ok_or(AppAndroidOwnerStoreError::StaleOwnerGeneration);
    }
    for receipt in &document.receipts {
        if receipt.operation == AppAndroidOwnerOperation::RevokeDevice {
            return Err(AppAndroidOwnerStoreError::InvalidReceipt);
        }
        receipt
            .verify_desktop_identity(
                &binding.status.desktop_identity_key_id,
                &binding.status.desktop_identity_public_key_hex,
                &binding.status.desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    }
    if !document.receipts.windows(2).all(|values| {
        (
            &values[0].principal,
            &values[0].workspace,
            &values[0].target_ref,
        ) < (
            &values[1].principal,
            &values[1].workspace,
            &values[1].target_ref,
        )
    }) {
        return Err(AppAndroidOwnerStoreError::InvalidReceipt);
    }
    let latest = document
        .latest_receipt
        .as_ref()
        .ok_or(AppAndroidOwnerStoreError::StaleOwnerGeneration)?;
    if latest.operation == AppAndroidOwnerOperation::RevokeDevice {
        return Err(AppAndroidOwnerStoreError::InvalidReceipt);
    }
    latest
        .verify_desktop_identity(
            &binding.status.desktop_identity_key_id,
            &binding.status.desktop_identity_public_key_hex,
            &binding.status.desktop_identity_digest,
            now_ms,
        )
        .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    if latest.owner_generation != document.owner_generation
        || latest.digest().ok().as_ref() != document.latest_receipt_digest.as_ref()
        || !document.receipts.iter().any(|receipt| receipt == latest)
    {
        return Err(AppAndroidOwnerStoreError::StaleOwnerGeneration);
    }
    Ok(())
}

fn validate_pending_enrollment(
    pending: &AppAndroidPendingOwnerEnrollment,
    now_ms: i64,
    require_live: bool,
) -> Result<(), AppAndroidOwnerStoreError> {
    if require_live {
        pending
            .proposal
            .validate(now_ms)
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    } else {
        pending
            .proposal
            .digest()
            .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    }
    let valid_hex =
        |value: &str| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit());
    if pending.proposal.operation != AppAndroidOwnerOperation::EnrollAttestedDevice
        || pending.device_id.is_empty()
        || pending.device_id.len() > 256
        || pending.device_label.is_empty()
        || pending.device_label.len() > 128
        || !valid_hex(&pending.connection_secret_sha256)
        || !valid_hex(&pending.enrollment_secret_digest)
        || !valid_hex(&pending.exchange_request_digest)
        || pending.proposal.enrollment_id.as_deref()
            != Some(pending.identity.enrollment_id.as_str())
        || pending.proposal.device_label != pending.device_label
        || pending.proposal.key_id != pending.identity.key_id
        || pending.proposal.owner_app_package != pending.identity.app_package
        || pending.proposal.app_version_code != pending.identity.app_version_code
        || pending.proposal.app_signing_sha256 != pending.identity.app_signing_sha256
        || pending.proposal.apk_sha256 != pending.identity.apk_sha256
        || pending.proposal.attestation_root_sha256 != pending.identity.attestation_root_sha256
        || pending.proposal.attestation_policy_digest != pending.identity.attestation_policy_digest
    {
        return Err(AppAndroidOwnerStoreError::InvalidReceipt);
    }
    let origin = url::Url::parse(&pending.public_origin)
        .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    if !matches!(origin.scheme(), "http" | "https")
        || origin.host_str().is_none()
        || !origin.username().is_empty()
        || origin.password().is_some()
        || !matches!(origin.path(), "" | "/")
        || origin.query().is_some()
        || origin.fragment().is_some()
    {
        return Err(AppAndroidOwnerStoreError::InvalidReceipt);
    }
    let key = DeviceKey::new(
        &pending.proposal.principal,
        &pending.proposal.workspace,
        &pending.device_id,
    );
    let (target_ref, identity_digest) = prospective_attested_automation_identity(
        &key,
        &pending.connection_secret_sha256,
        &pending.identity,
    )
    .map_err(|_| AppAndroidOwnerStoreError::InvalidReceipt)?;
    if target_ref != pending.proposal.target_ref
        || identity_digest != pending.proposal.automation_identity_digest
    {
        return Err(AppAndroidOwnerStoreError::InvalidReceipt);
    }
    Ok(())
}

fn load_document(
    root: &Path,
    path: &Path,
    staged_path: &Path,
) -> Result<AndroidOwnerDocument, AppAndroidOwnerStoreError> {
    ensure_private_directory(root)?;
    if staged_path.exists() {
        remove_regular_file(staged_path)?;
    }
    match read_private_file(path)? {
        Some(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string())),
        None => Ok(AndroidOwnerDocument::default()),
    }
}

fn persist_document(
    path: &Path,
    staged_path: &Path,
    document: &AndroidOwnerDocument,
) -> Result<(), AppAndroidOwnerStoreError> {
    let bytes = serde_json::to_vec(document)
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    if staged_path.exists() {
        remove_regular_file(staged_path)?;
    }
    write_bytes_durably_with_mode_sync(path, &bytes, Some(0o600))
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))
}

fn ensure_private_directory(path: &Path) -> Result<(), AppAndroidOwnerStoreError> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
        }
    }
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o700 {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
    }
    Ok(())
}

fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>, AppAndroidOwnerStoreError> {
    let named = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(AppAndroidOwnerStoreError::Io(error.to_string())),
    };
    if named.file_type().is_symlink() || !named.is_file() || named.len() > MAX_STORE_BYTES {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if named.uid() != unsafe { libc::geteuid() }
            || named.mode() & 0o777 != 0o600
            || named.nlink() != 1
        {
            return Err(AppAndroidOwnerStoreError::Unavailable);
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(path)
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_STORE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
    if bytes.len() as u64 > MAX_STORE_BYTES {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    Ok(Some(bytes))
}

fn remove_regular_file(path: &Path) -> Result<(), AppAndroidOwnerStoreError> {
    let metadata = std::fs::symlink_metadata(path)
        .map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppAndroidOwnerStoreError::Unavailable);
    }
    std::fs::remove_file(path).map_err(|error| AppAndroidOwnerStoreError::Io(error.to_string()))
}

#[cfg(test)]
mod enrollment_recovery_tests {
    use magician_app_contract::android_owner::AppAndroidAttestationSecurityLevel;

    use super::*;

    fn expired_enrollment_proposal() -> AppAndroidOwnerProposal {
        AppAndroidOwnerProposal::mint(
            "proposal-enrollment-123456".to_owned(),
            2,
            Some(format!("blake3:{}", "a".repeat(64))),
            format!("blake3:{}", "b".repeat(64)),
            "owner".to_owned(),
            "default".to_owned(),
            AppAndroidOwnerOperation::EnrollAttestedDevice,
            Some("enrollment-123456".to_owned()),
            "android-device:opaque-enrollment".to_owned(),
            "Pixel".to_owned(),
            "android-key-123456".to_owned(),
            format!("blake3:{}", "c".repeat(64)),
            "ai.magicbeans.magdroid".to_owned(),
            11,
            "d".repeat(64),
            "e".repeat(64),
            "f".repeat(64),
            AppAndroidAttestationSecurityLevel::Tee,
            format!("blake3:{}", "1".repeat(64)),
            0,
            0,
            Vec::new(),
            Vec::new(),
            100,
            200,
        )
        .unwrap()
    }

    #[test]
    fn expired_intent_clears_only_at_the_signed_predecessor_head() {
        let proposal = expired_enrollment_proposal();
        let digest = proposal.digest().unwrap();
        let predecessor = proposal.previous_receipt_digest.as_deref();
        assert!(expired_enrollment_is_proven_unspent(
            &proposal,
            &digest,
            1,
            predecessor,
            None,
            201,
        ));
        assert!(!expired_enrollment_is_proven_unspent(
            &proposal,
            &digest,
            1,
            predecessor,
            None,
            199,
        ));
    }

    #[test]
    fn advanced_desktop_receipt_wins_over_expired_cleanup() {
        let proposal = expired_enrollment_proposal();
        let digest = proposal.digest().unwrap();
        let advanced_head = format!("blake3:{}", "9".repeat(64));
        assert!(!expired_enrollment_is_proven_unspent(
            &proposal,
            &digest,
            2,
            Some(advanced_head.as_str()),
            Some(&digest),
            300,
        ));
    }
}

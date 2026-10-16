//! Durable owner-scoped lifecycle for the private macOS host pairing.
//!
//! A desktop approval is not runtime authority by itself. The store advances
//! one generation through `Pending -> ApprovedPendingFinalize -> Active`, and
//! publishes `Active` only after verifying the desktop's keyed finalization
//! acknowledgement. Rotation keeps the previous active generation usable
//! until that point; revocation removes both active and pre-active material.

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::Read as _,
    path::{Path, PathBuf},
    time::{Duration as StdDuration, Instant},
};

use chrono::{DateTime, Utc};
use fs2::FileExt as _;
use magician_app_contract::macos_host::{
    app_macos_desktop_identity_digest, app_macos_host_protected_bundle_id, decode_pairing_key,
    AppMacosDesktopIdentityAttestation, AppMacosDesktopIdentityChallenge,
    AppMacosHostPairingApproval, AppMacosHostPairingFinalization, AppMacosHostPairingFinalized,
    AppMacosHostPairingProposal, AppMacosHostPairingResetAck, AppMacosHostPairingResetChallenge,
    AppMacosHostPairingRevoked, AppMacosHostPairingStatusRequest,
    AppMacosHostPairingTargetIdentity, AppMacosHostPairingTargetRequest,
    APP_MACOS_HOST_MAX_CLOCK_SKEW_MS, APP_MACOS_HOST_MAX_PAIRED_TARGETS,
    APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS, APP_MACOS_HOST_PAIRING_V1,
};
use rand::RngCore as _;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::Mutex;
use url::Url;
use zeroize::{Zeroize as _, Zeroizing};

use super::{
    macos_host::APP_MACOS_HOST_PROFILE_V1,
    models::{AppDigest, AppReference, AppScopeBindingRef},
};
use crate::magician_v2::artifact_v2::io::write_bytes_durably_with_mode;

const APP_MACOS_PAIRING_STORE_V2: &str = "magician.app-macos-pairing-store.v2";
const APP_MACOS_PAIRING_STORE_V3: &str = "magician.app-macos-pairing-store.v3";
const APP_MACOS_PAIRING_RESET_CHALLENGE_V1: &str = "magician.app-macos-pairing-reset-challenge.v1";
const APP_MACOS_PAIRING_SCOPE_V1: &str = "magician.app-macos-pairing-scope.v1";
const APP_MACOS_PAIRING_NAMESPACE: &str = "app-macos-pairings";
const APP_MACOS_PAIRING_MAX_STORE_BYTES: usize = 512 * 1024;
const APP_MACOS_PAIRING_LOCK_WAIT: StdDuration = StdDuration::from_secs(5);
const APP_MACOS_PAIRING_LOCK_POLL: StdDuration = StdDuration::from_millis(20);
const APP_MACOS_PAIRING_STATUS_TTL_MS: i64 = 30_000;
const APP_MACOS_PAIRING_RESET_TTL_MS: i64 = 120_000;
const APP_MACOS_PAIRING_ID_RANDOM_BYTES: usize = 16;

pub(crate) use magician_app_contract::macos_host::{
    AppMacosHostPairingApproval as AppMacosPairingApproval,
    AppMacosHostPairingFinalization as AppMacosPairingFinalization,
    AppMacosHostPairingFinalized as AppMacosPairingFinalized,
    AppMacosHostPairingProposal as AppMacosPairingProposal,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum AppMacosPairingStoreError {
    #[error("macOS pairing endpoint is not an exact supported local typed endpoint")]
    InvalidEndpoint,
    #[error("macOS pairing target is invalid or protected")]
    InvalidTarget,
    #[error("macOS pairing handshake is invalid")]
    InvalidHandshake,
    #[error("macOS pairing setup has expired")]
    Expired,
    #[error("macOS pairing generation is stale")]
    StaleGeneration,
    #[error("macOS pairing is not in the required lifecycle state")]
    InvalidState,
    #[error("macOS pairing generation space is exhausted")]
    GenerationExhausted,
    #[error("macOS pairing store is unavailable: {0}")]
    Unavailable(String),
    #[error("macOS pairing store is corrupt: {0}")]
    Corrupt(String),
    #[error("macOS pairing store I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Exact typed action endpoint established by the authenticated pairing owner.
/// Generic configuration, environment variables, redirects and URL suffixes
/// are deliberately outside this type.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct AppMacosPairingEndpoint {
    action_url: Url,
    gateway_endpoint_digest: AppDigest,
}

impl std::fmt::Debug for AppMacosPairingEndpoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosPairingEndpoint")
            .field("action_url", &self.action_url.as_str())
            .field("gateway_endpoint_digest", &self.gateway_endpoint_digest)
            .finish()
    }
}

#[allow(dead_code)] // URL inspection is retained for setup UI/qualification consumers.
impl AppMacosPairingEndpoint {
    pub(crate) fn parse(value: &str) -> Result<Self, AppMacosPairingStoreError> {
        if value.is_empty() || value.len() > 2_048 {
            return Err(AppMacosPairingStoreError::InvalidEndpoint);
        }
        let action_url =
            Url::parse(value).map_err(|_| AppMacosPairingStoreError::InvalidEndpoint)?;
        if action_url.scheme() != "http"
            || action_url.host_str() != Some("127.0.0.1")
            || action_url.port() != Some(3017)
            || action_url.path() != "/host/apps/macos/action"
            || action_url.query().is_some()
            || action_url.fragment().is_some()
            || !action_url.username().is_empty()
            || action_url.password().is_some()
        {
            return Err(AppMacosPairingStoreError::InvalidEndpoint);
        }
        let gateway_endpoint_digest = AppDigest::blake3_canonical_json(&json!({
            "profile": APP_MACOS_HOST_PROFILE_V1,
            "typed_action_url": action_url.as_str(),
        }))
        .map_err(|_| AppMacosPairingStoreError::InvalidEndpoint)?;
        Ok(Self {
            action_url,
            gateway_endpoint_digest,
        })
    }

    pub(crate) fn action_url(&self) -> &Url {
        &self.action_url
    }

    pub(crate) fn gateway_endpoint_digest(&self) -> &AppDigest {
        &self.gateway_endpoint_digest
    }

    pub(crate) fn proposal_url(&self) -> Url {
        let mut value = self.action_url.clone();
        value.set_path("/host/apps/macos/pairing/propose");
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppMacosPairingRequestedTarget {
    target_ref: AppReference,
    bundle_id: String,
}

#[allow(dead_code)] // Target inspectors remain part of the reviewed pairing DTO.
impl AppMacosPairingRequestedTarget {
    pub(crate) fn new(
        target_ref: AppReference,
        bundle_id: impl Into<String>,
    ) -> Result<Self, AppMacosPairingStoreError> {
        let bundle_id = bundle_id.into();
        if invalid_bundle_id(&bundle_id) || app_macos_host_protected_bundle_id(&bundle_id) {
            return Err(AppMacosPairingStoreError::InvalidTarget);
        }
        Ok(Self {
            target_ref,
            bundle_id,
        })
    }

    pub(crate) fn target_ref(&self) -> &AppReference {
        &self.target_ref
    }

    pub(crate) fn bundle_id(&self) -> &str {
        &self.bundle_id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppMacosPairingLifecyclePhase {
    Unpaired,
    Pending,
    ApprovedPendingFinalize,
    Active,
    Revoked,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AppMacosPairingStatus {
    phase: AppMacosPairingLifecyclePhase,
    active_generation: Option<u64>,
    transition_generation: Option<u64>,
    generation_high_water: u64,
    finalization_ready: bool,
}

/// Move-only desktop revocation capability derived from the exact live
/// pairing generation. The public control plane never supplies either field:
/// the lifecycle owner reloads the durable proposal/key and mints a fresh,
/// short-lived signed request immediately before contacting the literal
/// loopback desktop owner.
pub(crate) struct AppMacosPairingRevocationRequest {
    action_url: Url,
    capability: AppMacosHostPairingStatusRequest,
}

impl AppMacosPairingRevocationRequest {
    pub(crate) fn action_url(&self) -> &Url {
        &self.action_url
    }

    pub(crate) fn capability(&self) -> &AppMacosHostPairingStatusRequest {
        &self.capability
    }
}

impl AppMacosPairingStatus {
    pub(crate) fn phase(&self) -> AppMacosPairingLifecyclePhase {
        self.phase
    }

    pub(crate) fn active_generation(&self) -> Option<u64> {
        self.active_generation
    }

    pub(crate) fn transition_generation(&self) -> Option<u64> {
        self.transition_generation
    }

    pub(crate) fn generation_high_water(&self) -> u64 {
        self.generation_high_water
    }

    pub(crate) fn finalization_ready(&self) -> bool {
        self.finalization_ready
    }
}

struct AppMacosPairingSecret([u8; 32]);

impl AppMacosPairingSecret {
    fn decode(value: &str) -> Result<Self, AppMacosPairingStoreError> {
        decode_pairing_key(value)
            .map(Self)
            .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid signing key".to_owned()))
    }

    fn expose(&self) -> [u8; 32] {
        self.0
    }
}

impl Drop for AppMacosPairingSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Approved native identities which may be used to compute the exact runtime
/// owner profile and implementation digests. This is not execution authority:
/// workflows must consume [`AppMacosActivePairingSnapshot`] only.
pub(crate) struct AppMacosApprovedPairingSnapshot {
    setup_id: String,
    generation: u64,
    key_id: String,
    signing_key: AppMacosPairingSecret,
    scope_binding_ref: AppScopeBindingRef,
    proposal_digest: AppDigest,
    approval_digest: AppDigest,
    desktop_identity_digest: AppDigest,
    desktop_identity_attestation_digest: AppDigest,
    action_url: Url,
    gateway_endpoint_digest: AppDigest,
    host_identity_digest: AppDigest,
    cua_driver_binary_digest: AppDigest,
    tcc_policy_digest: AppDigest,
    tcc_epoch: u64,
    #[allow(dead_code)] // Retained as the exact signed approval material.
    reviewed_targets: Vec<AppMacosHostPairingTargetIdentity>,
}

impl std::fmt::Debug for AppMacosApprovedPairingSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosApprovedPairingSnapshot")
            .field("setup_id", &self.setup_id)
            .field("generation", &self.generation)
            .field("key_id", &self.key_id)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("proposal_digest", &self.proposal_digest)
            .field("approval_digest", &self.approval_digest)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)] // Setup/scope/target inspectors remain available to the approval owner.
impl AppMacosApprovedPairingSnapshot {
    pub(crate) fn setup_id(&self) -> &str {
        &self.setup_id
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) fn signing_key(&self) -> [u8; 32] {
        self.signing_key.expose()
    }

    pub(crate) fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub(crate) fn action_url(&self) -> &Url {
        &self.action_url
    }

    pub(crate) fn gateway_endpoint_digest(&self) -> &AppDigest {
        &self.gateway_endpoint_digest
    }

    pub(crate) fn desktop_identity_digest(&self) -> &AppDigest {
        &self.desktop_identity_digest
    }

    pub(crate) fn desktop_identity_attestation_digest(&self) -> &AppDigest {
        &self.desktop_identity_attestation_digest
    }

    pub(crate) fn host_identity_digest(&self) -> &AppDigest {
        &self.host_identity_digest
    }

    pub(crate) fn cua_driver_binary_digest(&self) -> &AppDigest {
        &self.cua_driver_binary_digest
    }

    pub(crate) fn tcc_policy_digest(&self) -> &AppDigest {
        &self.tcc_policy_digest
    }

    pub(crate) fn tcc_epoch(&self) -> u64 {
        self.tcc_epoch
    }

    pub(crate) fn reviewed_targets(&self) -> &[AppMacosHostPairingTargetIdentity] {
        &self.reviewed_targets
    }
}

/// The only snapshot from which the workflow layer may construct a live
/// macOS host pairing capability.
pub(crate) struct AppMacosActivePairingSnapshot {
    generation: u64,
    key_id: String,
    signing_key: AppMacosPairingSecret,
    desktop_identity_key_id: String,
    desktop_identity_public_key_hex: String,
    scope_binding_ref: AppScopeBindingRef,
    action_url: Url,
    gateway_endpoint_digest: AppDigest,
    desktop_identity_digest: AppDigest,
    desktop_identity_attestation_digest: AppDigest,
    host_identity_digest: AppDigest,
    cua_driver_binary_digest: AppDigest,
    tcc_policy_digest: AppDigest,
    tcc_epoch: u64,
    reviewed_targets: Vec<AppMacosHostPairingTargetIdentity>,
    owner_profile_digest: AppDigest,
    owner_implementation_digest: AppDigest,
    finalization_digest: AppDigest,
    activated_at_ms: i64,
}

impl std::fmt::Debug for AppMacosActivePairingSnapshot {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosActivePairingSnapshot")
            .field("generation", &self.generation)
            .field("key_id", &self.key_id)
            .field("scope_binding_ref", &self.scope_binding_ref)
            .field("gateway_endpoint_digest", &self.gateway_endpoint_digest)
            .field("desktop_identity_digest", &self.desktop_identity_digest)
            .field("host_identity_digest", &self.host_identity_digest)
            .field("owner_profile_digest", &self.owner_profile_digest)
            .field(
                "owner_implementation_digest",
                &self.owner_implementation_digest,
            )
            .field("finalization_digest", &self.finalization_digest)
            .field("activated_at_ms", &self.activated_at_ms)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)] // Activation timestamp remains available for audit projections.
impl AppMacosActivePairingSnapshot {
    pub(crate) fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn key_id(&self) -> &str {
        &self.key_id
    }

    pub(crate) fn signing_key(&self) -> [u8; 32] {
        self.signing_key.expose()
    }

    pub(crate) fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub(crate) fn action_url(&self) -> &Url {
        &self.action_url
    }

    pub(crate) fn gateway_endpoint_digest(&self) -> &AppDigest {
        &self.gateway_endpoint_digest
    }

    pub(crate) fn desktop_identity_digest(&self) -> &AppDigest {
        &self.desktop_identity_digest
    }

    pub(crate) fn desktop_identity_key_id(&self) -> &str {
        &self.desktop_identity_key_id
    }

    pub(crate) fn desktop_identity_public_key_hex(&self) -> &str {
        &self.desktop_identity_public_key_hex
    }

    pub(crate) fn desktop_identity_attestation_digest(&self) -> &AppDigest {
        &self.desktop_identity_attestation_digest
    }

    pub(crate) fn host_identity_digest(&self) -> &AppDigest {
        &self.host_identity_digest
    }

    pub(crate) fn cua_driver_binary_digest(&self) -> &AppDigest {
        &self.cua_driver_binary_digest
    }

    pub(crate) fn tcc_policy_digest(&self) -> &AppDigest {
        &self.tcc_policy_digest
    }

    pub(crate) fn tcc_epoch(&self) -> u64 {
        self.tcc_epoch
    }

    pub(crate) fn reviewed_targets(&self) -> &[AppMacosHostPairingTargetIdentity] {
        &self.reviewed_targets
    }

    pub(crate) fn owner_profile_digest(&self) -> &AppDigest {
        &self.owner_profile_digest
    }

    pub(crate) fn owner_implementation_digest(&self) -> &AppDigest {
        &self.owner_implementation_digest
    }

    pub(crate) fn finalization_digest(&self) -> &AppDigest {
        &self.finalization_digest
    }

    pub(crate) fn activated_at_ms(&self) -> i64 {
        self.activated_at_ms
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMacosPairingDocument {
    schema: String,
    scope_binding_ref: String,
    scope_binding_digest: String,
    revision: u64,
    generation_high_water: u64,
    active: Option<StoredActivePairing>,
    transition: Option<StoredPairingTransition>,
    tombstone: Option<StoredPairingTombstone>,
    #[serde(default)]
    reset_anchor: Option<StoredPairingResetAnchor>,
}

impl Drop for AppMacosPairingDocument {
    fn drop(&mut self) {
        if let Some(active) = self.active.as_mut() {
            active.proposal.signing_key_hex.zeroize();
        }
        if let Some(transition) = self.transition.as_mut() {
            transition.proposal_mut().signing_key_hex.zeroize();
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum StoredPairingTransition {
    Pending {
        proposal: AppMacosHostPairingProposal,
        desktop_identity: StoredDesktopIdentityBinding,
    },
    ApprovedPendingFinalize {
        proposal: AppMacosHostPairingProposal,
        desktop_identity: StoredDesktopIdentityBinding,
        approval: AppMacosHostPairingApproval,
        finalization: Option<AppMacosHostPairingFinalization>,
    },
}

impl StoredPairingTransition {
    fn proposal(&self) -> &AppMacosHostPairingProposal {
        match self {
            Self::Pending { proposal, .. } | Self::ApprovedPendingFinalize { proposal, .. } => {
                proposal
            },
        }
    }

    fn desktop_identity(&self) -> &StoredDesktopIdentityBinding {
        match self {
            Self::Pending {
                desktop_identity, ..
            }
            | Self::ApprovedPendingFinalize {
                desktop_identity, ..
            } => desktop_identity,
        }
    }

    fn proposal_mut(&mut self) -> &mut AppMacosHostPairingProposal {
        match self {
            Self::Pending { proposal, .. } | Self::ApprovedPendingFinalize { proposal, .. } => {
                proposal
            },
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredDesktopIdentityBinding {
    challenge: AppMacosDesktopIdentityChallenge,
    attestation: AppMacosDesktopIdentityAttestation,
    desktop_identity_digest: String,
    attestation_digest: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredActivePairing {
    proposal: AppMacosHostPairingProposal,
    desktop_identity: StoredDesktopIdentityBinding,
    approval: AppMacosHostPairingApproval,
    finalization: AppMacosHostPairingFinalization,
    finalized: AppMacosHostPairingFinalized,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPairingTombstone {
    generation: u64,
    revoked_at_ms: i64,
    desktop_identity: StoredDesktopIdentityBinding,
    revoked: AppMacosHostPairingRevoked,
    revoked_digest: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPairingResetAnchor {
    challenge: AppMacosHostPairingResetChallenge,
    acknowledgment: AppMacosHostPairingResetAck,
    acknowledgment_digest: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredPairingResetChallengeDocument {
    schema: String,
    scope_binding_ref: String,
    scope_binding_digest: String,
    challenge: AppMacosHostPairingResetChallenge,
    #[serde(default)]
    acknowledgment_digest: Option<String>,
    #[serde(default)]
    generation_floor: Option<u64>,
}

pub(crate) struct AppMacosPairingStore {
    system_root: PathBuf,
    namespace: PathBuf,
    path: PathBuf,
    lock_path: PathBuf,
    reset_challenge_path: PathBuf,
    scope_binding_ref: AppScopeBindingRef,
    scope_binding_digest: AppDigest,
    write_lock: Mutex<()>,
}

impl AppMacosPairingStore {
    pub(crate) async fn open(
        system_root: &Path,
        scope_binding_ref: &AppScopeBindingRef,
    ) -> Result<Self, AppMacosPairingStoreError> {
        ensure_real_directory(system_root, false).await?;
        let system_root = tokio::fs::canonicalize(system_root)
            .await
            .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?;
        ensure_real_directory(&system_root, false).await?;
        let namespace = system_root.join(APP_MACOS_PAIRING_NAMESPACE);
        ensure_private_namespace(&namespace).await?;
        let scope_binding_digest = pairing_scope_digest(scope_binding_ref)?;
        let scope_file = scope_binding_digest
            .as_str()
            .strip_prefix("blake3:")
            .ok_or_else(|| AppMacosPairingStoreError::Corrupt("invalid scope digest".to_owned()))?;
        let path = namespace.join(format!("{scope_file}.json"));
        let lock_path = namespace.join(format!("{scope_file}.lock"));
        let reset_challenge_path = namespace.join(format!("{scope_file}.reset.json"));
        let store = Self {
            system_root,
            namespace,
            path,
            lock_path,
            reset_challenge_path,
            scope_binding_ref: scope_binding_ref.clone(),
            scope_binding_digest,
            write_lock: Mutex::new(()),
        };
        let _ = store.load_document().await?;
        Ok(store)
    }

    /// Open only the exceptional owner-confirmed reset lane. A corrupt main
    /// document is allowed here because the signed desktop floor is the
    /// recovery source, but unsafe storage topology/permissions remain fatal.
    pub(crate) async fn open_for_reset(
        system_root: &Path,
        scope_binding_ref: &AppScopeBindingRef,
    ) -> Result<Self, AppMacosPairingStoreError> {
        ensure_real_directory(system_root, false).await?;
        let system_root = tokio::fs::canonicalize(system_root)
            .await
            .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?;
        ensure_real_directory(&system_root, false).await?;
        let namespace = system_root.join(APP_MACOS_PAIRING_NAMESPACE);
        ensure_private_namespace(&namespace).await?;
        let scope_binding_digest = pairing_scope_digest(scope_binding_ref)?;
        let scope_file = scope_binding_digest
            .as_str()
            .strip_prefix("blake3:")
            .ok_or_else(|| AppMacosPairingStoreError::Corrupt("invalid scope digest".to_owned()))?;
        let store = Self {
            system_root,
            namespace: namespace.clone(),
            path: namespace.join(format!("{scope_file}.json")),
            lock_path: namespace.join(format!("{scope_file}.lock")),
            reset_challenge_path: namespace.join(format!("{scope_file}.reset.json")),
            scope_binding_ref: scope_binding_ref.clone(),
            scope_binding_digest,
            write_lock: Mutex::new(()),
        };
        store.validate_storage_paths().await?;
        match store.load_document().await {
            Ok(document) if document.active.is_some() || document.transition.is_some() => {
                return Err(AppMacosPairingStoreError::InvalidState);
            },
            Ok(_) | Err(AppMacosPairingStoreError::Corrupt(_)) => {},
            Err(error) => return Err(error),
        }
        Ok(store)
    }

    pub(crate) async fn begin_setup(
        &self,
        endpoint: AppMacosPairingEndpoint,
        requested_targets: Vec<AppMacosPairingRequestedTarget>,
        desktop_identity_challenge: AppMacosDesktopIdentityChallenge,
        desktop_identity_attestation: AppMacosDesktopIdentityAttestation,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingProposal, AppMacosPairingStoreError> {
        let requested_targets = normalize_requested_targets(requested_targets)?;
        let now_ms = now.timestamp_millis();
        let desktop_identity = desktop_identity_binding(
            desktop_identity_challenge,
            desktop_identity_attestation,
            now_ms,
        )?;
        self.commit(move |document| {
            if let Some(transition) = document.transition.as_ref() {
                let may_have_activated = matches!(
                    transition,
                    StoredPairingTransition::ApprovedPendingFinalize {
                        finalization: Some(_),
                        ..
                    }
                );
                if may_have_activated || transition.proposal().expires_at_ms > now_ms {
                    return Err(AppMacosPairingStoreError::InvalidState);
                }
                if transition.desktop_identity().desktop_identity_digest
                    != desktop_identity.desktop_identity_digest
                {
                    return Err(AppMacosPairingStoreError::InvalidHandshake);
                }
            }
            if document.active.as_ref().is_some_and(|active| {
                active.desktop_identity.desktop_identity_digest
                    != desktop_identity.desktop_identity_digest
            }) {
                // V1 identity-key rotation is deliberately unsupported. A
                // replacement key needs an independently reviewed prior-key
                // cross-sign/reset protocol, not merely another approval.
                return Err(AppMacosPairingStoreError::InvalidHandshake);
            }
            if let Some(anchor) = document.reset_anchor.as_ref() {
                let identity = &desktop_identity.challenge;
                if anchor.acknowledgment.desktop_identity_key_id != identity.desktop_identity_key_id
                    || anchor.acknowledgment.desktop_identity_public_key_hex
                        != identity.desktop_identity_public_key_hex
                    || anchor.acknowledgment.desktop_identity_digest
                        != desktop_identity.desktop_identity_digest
                    || anchor.acknowledgment.host_identity_digest
                        != desktop_identity.attestation.host_identity_digest
                {
                    return Err(AppMacosPairingStoreError::InvalidHandshake);
                }
            }
            let generation = document
                .generation_high_water
                .checked_add(1)
                .ok_or(AppMacosPairingStoreError::GenerationExhausted)?;
            let mut signing_key = Zeroizing::new([0_u8; 32]);
            rand::rngs::OsRng.fill_bytes(&mut signing_key[..]);
            if signing_key.iter().all(|byte| *byte == 0) {
                return Err(AppMacosPairingStoreError::InvalidHandshake);
            }
            let setup_id = random_pairing_id("setup")?;
            let key_id = random_pairing_id("key")?;
            let proposal = if let Some(active) = document.active.as_ref() {
                let previous_signing_key = Zeroizing::new(
                    decode_pairing_key(&active.proposal.signing_key_hex).map_err(|_| {
                        AppMacosPairingStoreError::Corrupt("invalid active key".to_owned())
                    })?,
                );
                AppMacosHostPairingProposal::mint_rotation(
                    setup_id,
                    generation,
                    active.proposal.generation,
                    key_id,
                    &signing_key,
                    &previous_signing_key,
                    self.scope_binding_ref.to_string(),
                    endpoint.action_url().as_str().to_owned(),
                    endpoint.gateway_endpoint_digest().to_string(),
                    desktop_identity.attestation_digest.clone(),
                    requested_targets,
                    now_ms,
                    now_ms.saturating_add(APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS),
                )
            } else {
                AppMacosHostPairingProposal::mint(
                    setup_id,
                    generation,
                    key_id,
                    &signing_key,
                    self.scope_binding_ref.to_string(),
                    endpoint.action_url().as_str().to_owned(),
                    endpoint.gateway_endpoint_digest().to_string(),
                    desktop_identity.attestation_digest.clone(),
                    requested_targets,
                    now_ms,
                    now_ms.saturating_add(APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS),
                )
            }
            .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            document.generation_high_water = generation;
            document.transition = Some(StoredPairingTransition::Pending {
                proposal: proposal.clone(),
                desktop_identity,
            });
            Ok(AppMacosPairingMutation::changed(proposal))
        })
        .await
    }

    pub(crate) async fn apply_approved(
        &self,
        approval: AppMacosPairingApproval,
        now: DateTime<Utc>,
    ) -> Result<AppMacosApprovedPairingSnapshot, AppMacosPairingStoreError> {
        let now_ms = now.timestamp_millis();
        self.commit(move |document| {
            let transition = document
                .transition
                .as_ref()
                .ok_or(AppMacosPairingStoreError::StaleGeneration)?;
            if transition.proposal().desktop_identity_attestation_digest
                != transition.desktop_identity().attestation_digest
            {
                return Err(AppMacosPairingStoreError::Corrupt(
                    "proposal desktop identity attestation mismatch".to_owned(),
                ));
            }
            match transition {
                StoredPairingTransition::Pending {
                    proposal,
                    desktop_identity,
                } => {
                    approval
                        .verify(proposal, now_ms)
                        .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
                    verify_desktop_approval(&approval, desktop_identity)?;
                    let proposal = proposal.clone();
                    let desktop_identity = desktop_identity.clone();
                    let snapshot = approved_snapshot(&proposal, &approval, &desktop_identity)?;
                    document.transition = Some(StoredPairingTransition::ApprovedPendingFinalize {
                        proposal,
                        desktop_identity,
                        approval,
                        finalization: None,
                    });
                    Ok(AppMacosPairingMutation::changed(snapshot))
                },
                StoredPairingTransition::ApprovedPendingFinalize {
                    proposal,
                    desktop_identity,
                    approval: stored,
                    ..
                } => {
                    if stored.digest().ok() != approval.digest().ok() {
                        return Err(AppMacosPairingStoreError::StaleGeneration);
                    }
                    approval
                        .verify(proposal, now_ms)
                        .map_err(|_| AppMacosPairingStoreError::Expired)?;
                    verify_desktop_approval(&approval, desktop_identity)?;
                    Ok(AppMacosPairingMutation::unchanged(approved_snapshot(
                        proposal,
                        stored,
                        desktop_identity,
                    )?))
                },
            }
        })
        .await
    }

    /// Recover the exact durably retained bootstrap message after a runtime
    /// restart. The returned wire DTO redacts its key in `Debug`; callers must
    /// still treat it as private authenticated desktop transport material.
    pub(crate) async fn setup_proposal(
        &self,
    ) -> Result<Option<AppMacosPairingProposal>, AppMacosPairingStoreError> {
        let document = self.load_document().await?;
        Ok(document
            .transition
            .as_ref()
            .map(|transition| transition.proposal().clone()))
    }

    pub(crate) async fn approved_snapshot(
        &self,
    ) -> Result<Option<AppMacosApprovedPairingSnapshot>, AppMacosPairingStoreError> {
        let document = self.load_document().await?;
        match document.transition.as_ref() {
            Some(StoredPairingTransition::ApprovedPendingFinalize {
                proposal,
                desktop_identity,
                approval,
                ..
            }) => approved_snapshot(proposal, approval, desktop_identity).map(Some),
            _ => Ok(None),
        }
    }

    /// Mint and durably retain the exact finalization before it is sent. An
    /// acknowledgement after a runtime restart can therefore be checked
    /// against the same bytes instead of reconstructing authority.
    pub(crate) async fn finalization(
        &self,
        approved: &AppMacosApprovedPairingSnapshot,
        owner_profile_digest: AppDigest,
        owner_implementation_digest: AppDigest,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingFinalization, AppMacosPairingStoreError> {
        let expected_setup_id = approved.setup_id.clone();
        let expected_generation = approved.generation;
        let expected_proposal_digest = approved.proposal_digest.clone();
        let expected_approval_digest = approved.approval_digest.clone();
        let now_ms = now.timestamp_millis();
        self.commit(move |document| {
            let Some(StoredPairingTransition::ApprovedPendingFinalize {
                proposal,
                desktop_identity: _,
                approval,
                finalization,
            }) = document.transition.as_mut()
            else {
                return Err(AppMacosPairingStoreError::StaleGeneration);
            };
            if proposal.setup_id != expected_setup_id
                || proposal.generation != expected_generation
                || proposal.digest().ok().as_deref() != Some(expected_proposal_digest.as_str())
                || approval.digest().ok().as_deref() != Some(expected_approval_digest.as_str())
            {
                return Err(AppMacosPairingStoreError::StaleGeneration);
            }
            if let Some(existing) = finalization {
                if existing.owner_profile_digest != owner_profile_digest.as_str()
                    || existing.owner_implementation_digest != owner_implementation_digest.as_str()
                {
                    return Err(AppMacosPairingStoreError::InvalidState);
                }
                return Ok(AppMacosPairingMutation::unchanged(existing.clone()));
            }
            // Expiry prevents minting fresh authority. It must not make an
            // exact finalization that was durably retained before a crash
            // unavailable: the owner may need to re-send those same signed
            // bytes and recover the desktop's idempotent acknowledgement.
            if now_ms >= approval.expires_at_ms {
                return Err(AppMacosPairingStoreError::Expired);
            }
            let signing_key = Zeroizing::new(
                decode_pairing_key(&proposal.signing_key_hex)
                    .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid key".to_owned()))?,
            );
            let value = AppMacosHostPairingFinalization::mint(
                proposal,
                approval,
                owner_profile_digest.to_string(),
                owner_implementation_digest.to_string(),
                &signing_key,
                now_ms,
            )
            .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            *finalization = Some(value.clone());
            Ok(AppMacosPairingMutation::changed(value))
        })
        .await
    }

    pub(crate) async fn mark_finalized(
        &self,
        finalized: AppMacosPairingFinalized,
        now: DateTime<Utc>,
    ) -> Result<AppMacosActivePairingSnapshot, AppMacosPairingStoreError> {
        let now_ms = now.timestamp_millis();
        self.commit(move |document| {
            if let Some(active) = document.active.as_ref() {
                if active.finalized == finalized {
                    return Ok(AppMacosPairingMutation::unchanged(active_snapshot(active)?));
                }
            }
            let Some(StoredPairingTransition::ApprovedPendingFinalize {
                proposal,
                desktop_identity,
                approval,
                finalization: Some(finalization),
            }) = document.transition.as_ref()
            else {
                return Err(AppMacosPairingStoreError::StaleGeneration);
            };
            if finalized.setup_id != proposal.setup_id
                || finalized.generation != proposal.generation
                || finalized.activated_at_ms
                    > now_ms.saturating_add(APP_MACOS_HOST_MAX_CLOCK_SKEW_MS)
            {
                return Err(AppMacosPairingStoreError::InvalidHandshake);
            }
            let signing_key = Zeroizing::new(
                decode_pairing_key(&proposal.signing_key_hex)
                    .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid key".to_owned()))?,
            );
            finalized
                .verify(finalization, &signing_key)
                .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            finalized
                .verify_desktop_identity(
                    &desktop_identity.challenge.desktop_identity_key_id,
                    &desktop_identity.challenge.desktop_identity_public_key_hex,
                )
                .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            let active = StoredActivePairing {
                proposal: proposal.clone(),
                desktop_identity: desktop_identity.clone(),
                approval: approval.clone(),
                finalization: finalization.clone(),
                finalized,
            };
            let snapshot = active_snapshot(&active)?;
            document.active = Some(active);
            document.transition = None;
            Ok(AppMacosPairingMutation::changed(snapshot))
        })
        .await
    }

    /// Verify the desktop owner's exact signed, durable revocation before
    /// deleting the runtime's corresponding key and authority. The key remains
    /// available inside the locked transaction until verification succeeds;
    /// the committed tombstone retains only the signed acknowledgement and its
    /// digest, never the signing key or proposal.
    pub(crate) async fn revoke_with_ack(
        &self,
        expected_generation: u64,
        revoked: AppMacosHostPairingRevoked,
        now: DateTime<Utc>,
    ) -> Result<(), AppMacosPairingStoreError> {
        if expected_generation == 0 {
            return Err(AppMacosPairingStoreError::StaleGeneration);
        }
        self.commit(move |document| {
            let proposal = document
                .transition
                .as_ref()
                .map(StoredPairingTransition::proposal)
                .or_else(|| document.active.as_ref().map(|active| &active.proposal))
                .ok_or(AppMacosPairingStoreError::InvalidState)?;
            let desktop_identity = document
                .transition
                .as_ref()
                .map(StoredPairingTransition::desktop_identity)
                .or_else(|| {
                    document
                        .active
                        .as_ref()
                        .map(|active| &active.desktop_identity)
                })
                .ok_or(AppMacosPairingStoreError::InvalidState)?;
            if proposal.generation != expected_generation {
                return Err(AppMacosPairingStoreError::StaleGeneration);
            }
            let signing_key = Zeroizing::new(
                decode_pairing_key(&proposal.signing_key_hex)
                    .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid key".to_owned()))?,
            );
            revoked
                .verify(proposal, &signing_key, now.timestamp_millis())
                .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            revoked
                .verify_desktop_identity(
                    &desktop_identity.challenge.desktop_identity_key_id,
                    &desktop_identity.challenge.desktop_identity_public_key_hex,
                )
                .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            let desktop_identity = desktop_identity.clone();
            let revoked_digest = revoked
                .digest()
                .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
            let tombstone_generation = document
                .generation_high_water
                .checked_add(1)
                .ok_or(AppMacosPairingStoreError::GenerationExhausted)?;
            document.generation_high_water = tombstone_generation;
            document.active = None;
            document.transition = None;
            document.tombstone = Some(StoredPairingTombstone {
                generation: tombstone_generation,
                revoked_at_ms: revoked.revoked_at_ms,
                desktop_identity,
                revoked,
                revoked_digest,
            });
            Ok(AppMacosPairingMutation::changed(()))
        })
        .await
    }

    /// Mint a fresh desktop capability for the exact generation that
    /// [`Self::revoke_with_ack`] would tombstone. Rotation selects the
    /// transition first, matching revocation; an Active generation carries its
    /// exact approval and finalization digests so the desktop cannot accept a
    /// stale key-only request. This method does not mutate local state: the
    /// caller must first receive the desktop's signed `Revoked` response, then
    /// durably revoke the same expected generation with `revoke_with_ack`.
    pub(crate) async fn revocation_request(
        &self,
        expected_generation: u64,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingRevocationRequest, AppMacosPairingStoreError> {
        if expected_generation == 0 {
            return Err(AppMacosPairingStoreError::StaleGeneration);
        }
        let document = self.load_document().await?;
        let (proposal, recovery) = if let Some(transition) = document.transition.as_ref() {
            match transition {
                StoredPairingTransition::Pending { proposal, .. } => (proposal, None),
                StoredPairingTransition::ApprovedPendingFinalize {
                    proposal,
                    approval,
                    finalization,
                    ..
                } => (
                    proposal,
                    finalization
                        .as_ref()
                        .map(|finalization| (approval, finalization)),
                ),
            }
        } else if let Some(active) = document.active.as_ref() {
            (
                &active.proposal,
                Some((&active.approval, &active.finalization)),
            )
        } else {
            return Err(AppMacosPairingStoreError::InvalidState);
        };
        if proposal.generation != expected_generation {
            return Err(AppMacosPairingStoreError::StaleGeneration);
        }
        Ok(AppMacosPairingRevocationRequest {
            action_url: parse_action_url(&proposal.gateway_action_url)?,
            capability: mint_status_capability(proposal, recovery, now)?,
        })
    }

    pub(crate) async fn active_snapshot(
        &self,
    ) -> Result<Option<AppMacosActivePairingSnapshot>, AppMacosPairingStoreError> {
        let document = self.load_document().await?;
        document.active.as_ref().map(active_snapshot).transpose()
    }

    pub(crate) async fn status(&self) -> Result<AppMacosPairingStatus, AppMacosPairingStoreError> {
        let document = self.load_document().await?;
        pairing_status_from_document(&document)
    }

    pub(crate) async fn status_request(
        &self,
        now: DateTime<Utc>,
    ) -> Result<AppMacosHostPairingStatusRequest, AppMacosPairingStoreError> {
        let document = self.load_document().await?;
        let transition = document
            .transition
            .as_ref()
            .ok_or(AppMacosPairingStoreError::InvalidState)?;
        let recovery = match transition {
            StoredPairingTransition::ApprovedPendingFinalize {
                approval,
                finalization: Some(finalization),
                ..
            } => Some((approval, finalization)),
            _ => None,
        };
        mint_status_capability(transition.proposal(), recovery, now)
    }

    /// Mint or recover the exact short-lived reset challenge. It is persisted
    /// separately from the possibly corrupt/lost authority document, so no
    /// local state is overwritten before the desktop supplies an owner-approved
    /// signed monotonic floor.
    pub(crate) async fn reset_challenge(
        &self,
        now: DateTime<Utc>,
    ) -> Result<AppMacosHostPairingResetChallenge, AppMacosPairingStoreError> {
        let _local_guard = self.write_lock.lock().await;
        self.validate_storage_paths().await?;
        let _process_guard = acquire_store_lock(self.lock_path.clone()).await?;
        let anchored_challenge_digest = match self.load_document().await {
            Ok(document) if document.active.is_some() || document.transition.is_some() => {
                return Err(AppMacosPairingStoreError::InvalidState);
            },
            Ok(document) => document
                .reset_anchor
                .as_ref()
                .and_then(|anchor| anchor.challenge.digest().ok()),
            Err(AppMacosPairingStoreError::Corrupt(_)) => None,
            Err(error) => return Err(error),
        };
        match self.load_reset_challenge().await {
            Ok(Some(stored))
                if stored.acknowledgment_digest.is_none()
                    && stored.challenge.validate(now.timestamp_millis()).is_ok()
                    && stored.challenge.digest().ok() != anchored_challenge_digest =>
            {
                return Ok(stored.challenge);
            },
            Ok(_) | Err(AppMacosPairingStoreError::Corrupt(_)) => {},
            Err(error) => return Err(error),
        }
        let issued_at_ms = now.timestamp_millis();
        let challenge = AppMacosHostPairingResetChallenge::mint(
            self.scope_binding_ref.to_string(),
            random_pairing_id("reset")?,
            issued_at_ms,
            issued_at_ms
                .checked_add(APP_MACOS_PAIRING_RESET_TTL_MS)
                .ok_or(AppMacosPairingStoreError::InvalidHandshake)?,
        )
        .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
        self.persist_reset_challenge(&StoredPairingResetChallengeDocument {
            schema: APP_MACOS_PAIRING_RESET_CHALLENGE_V1.to_owned(),
            scope_binding_ref: self.scope_binding_ref.to_string(),
            scope_binding_digest: self.scope_binding_digest.to_string(),
            challenge: challenge.clone(),
            acknowledgment_digest: None,
            generation_floor: None,
        })
        .await?;
        Ok(challenge)
    }

    /// Accept only the desktop's Ed25519-signed acknowledgment of the exact
    /// retained challenge. This is the sole path allowed to replace a corrupt
    /// runtime document: it preserves the desktop's generation floor and
    /// pinned identity in a keyless durable anchor before returning Unpaired.
    pub(crate) async fn apply_reset_ack(
        &self,
        acknowledgment: AppMacosHostPairingResetAck,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingStatus, AppMacosPairingStoreError> {
        let _local_guard = self.write_lock.lock().await;
        self.validate_storage_paths().await?;
        let _process_guard = acquire_store_lock(self.lock_path.clone()).await?;

        let existing = match self.load_document().await {
            Ok(document) => Some(document),
            Err(AppMacosPairingStoreError::Corrupt(_)) => None,
            Err(error) => return Err(error),
        };
        if let Some(document) = existing.as_ref() {
            if document.active.is_some() || document.transition.is_some() {
                return Err(AppMacosPairingStoreError::InvalidState);
            }
            if let Some(anchor) = document.reset_anchor.as_ref() {
                let incoming_digest = acknowledgment
                    .digest()
                    .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
                if incoming_digest == anchor.acknowledgment_digest
                    && acknowledgment == anchor.acknowledgment
                {
                    let challenge = anchor.challenge.clone();
                    let digest = anchor.acknowledgment_digest.clone();
                    let floor = anchor.acknowledgment.prior_generation_floor;
                    self.mark_reset_challenge_consumed(&challenge, &digest, floor)
                        .await?;
                    return pairing_status_from_document(document);
                }
            }
        }
        let stored_challenge = self
            .load_reset_challenge()
            .await?
            .ok_or(AppMacosPairingStoreError::InvalidState)?;
        if stored_challenge.acknowledgment_digest.is_some()
            || stored_challenge.generation_floor.is_some()
        {
            return Err(AppMacosPairingStoreError::StaleGeneration);
        }
        acknowledgment
            .verify(&stored_challenge.challenge, now.timestamp_millis())
            .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
        if acknowledgment.scope_binding_ref != self.scope_binding_ref.as_str() {
            return Err(AppMacosPairingStoreError::InvalidHandshake);
        }
        let acknowledgment_digest = acknowledgment
            .digest()
            .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
        parse_stored_digest(&acknowledgment.host_identity_digest)?;
        parse_stored_digest(&acknowledgment.desktop_identity_digest)?;
        parse_stored_digest(&acknowledgment_digest)?;

        if let Some(document) = existing.as_ref() {
            if acknowledgment.prior_generation_floor < document.generation_high_water {
                return Err(AppMacosPairingStoreError::StaleGeneration);
            }
            if let Some(anchor) = document.reset_anchor.as_ref() {
                if acknowledgment.desktop_identity_digest
                    != anchor.acknowledgment.desktop_identity_digest
                    || acknowledgment.desktop_identity_key_id
                        != anchor.acknowledgment.desktop_identity_key_id
                    || acknowledgment.desktop_identity_public_key_hex
                        != anchor.acknowledgment.desktop_identity_public_key_hex
                    || acknowledgment.host_identity_digest
                        != anchor.acknowledgment.host_identity_digest
                {
                    return Err(AppMacosPairingStoreError::InvalidHandshake);
                }
            }
        }
        let revision = existing
            .as_ref()
            .map(|document| document.revision)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(AppMacosPairingStoreError::GenerationExhausted)?;
        let document = AppMacosPairingDocument {
            schema: APP_MACOS_PAIRING_STORE_V3.to_owned(),
            scope_binding_ref: self.scope_binding_ref.to_string(),
            scope_binding_digest: self.scope_binding_digest.to_string(),
            revision,
            generation_high_water: acknowledgment.prior_generation_floor,
            active: None,
            transition: None,
            tombstone: None,
            reset_anchor: Some(StoredPairingResetAnchor {
                challenge: stored_challenge.challenge,
                acknowledgment,
                acknowledgment_digest,
            }),
        };
        validate_document(
            &document,
            &self.scope_binding_ref,
            &self.scope_binding_digest,
        )?;
        self.persist_document(&document).await?;
        let anchor = document
            .reset_anchor
            .as_ref()
            .ok_or(AppMacosPairingStoreError::Corrupt(
                "reset anchor vanished before challenge settlement".to_owned(),
            ))?;
        let challenge = anchor.challenge.clone();
        let digest = anchor.acknowledgment_digest.clone();
        let floor = anchor.acknowledgment.prior_generation_floor;
        self.mark_reset_challenge_consumed(&challenge, &digest, floor)
            .await?;
        pairing_status_from_document(&document)
    }

    async fn commit<T>(
        &self,
        mutate: impl FnOnce(
            &mut AppMacosPairingDocument,
        ) -> Result<AppMacosPairingMutation<T>, AppMacosPairingStoreError>,
    ) -> Result<T, AppMacosPairingStoreError> {
        let _local_guard = self.write_lock.lock().await;
        self.validate_storage_paths().await?;
        let _process_guard = acquire_store_lock(self.lock_path.clone()).await?;
        let mut document = self.load_document().await?;
        let mutation = mutate(&mut document)?;
        if mutation.changed {
            document.schema = APP_MACOS_PAIRING_STORE_V3.to_owned();
            document.revision = document
                .revision
                .checked_add(1)
                .ok_or(AppMacosPairingStoreError::GenerationExhausted)?;
            validate_document(
                &document,
                &self.scope_binding_ref,
                &self.scope_binding_digest,
            )?;
            self.persist_document(&document).await?;
        }
        Ok(mutation.output)
    }

    async fn load_document(&self) -> Result<AppMacosPairingDocument, AppMacosPairingStoreError> {
        self.validate_storage_paths().await?;
        let bytes = read_private_bounded_file(self.path.clone()).await?;
        let Some(bytes) = bytes else {
            return Ok(AppMacosPairingDocument {
                schema: APP_MACOS_PAIRING_STORE_V2.to_owned(),
                scope_binding_ref: self.scope_binding_ref.to_string(),
                scope_binding_digest: self.scope_binding_digest.to_string(),
                revision: 0,
                generation_high_water: 0,
                active: None,
                transition: None,
                tombstone: None,
                reset_anchor: None,
            });
        };
        let document: AppMacosPairingDocument = serde_json::from_slice(&bytes)
            .map_err(|error| AppMacosPairingStoreError::Corrupt(error.to_string()))?;
        validate_document(
            &document,
            &self.scope_binding_ref,
            &self.scope_binding_digest,
        )?;
        Ok(document)
    }

    async fn load_reset_challenge(
        &self,
    ) -> Result<Option<StoredPairingResetChallengeDocument>, AppMacosPairingStoreError> {
        let Some(bytes) = read_private_bounded_file(self.reset_challenge_path.clone()).await?
        else {
            return Ok(None);
        };
        let value: StoredPairingResetChallengeDocument = serde_json::from_slice(&bytes)
            .map_err(|error| AppMacosPairingStoreError::Corrupt(error.to_string()))?;
        if value.schema != APP_MACOS_PAIRING_RESET_CHALLENGE_V1
            || value.scope_binding_ref != self.scope_binding_ref.as_str()
            || value.scope_binding_digest != self.scope_binding_digest.as_str()
            || value.challenge.scope_binding_ref != self.scope_binding_ref.as_str()
            || value.challenge.digest().is_err()
            || value.acknowledgment_digest.is_some() != value.generation_floor.is_some()
            || value
                .acknowledgment_digest
                .as_deref()
                .is_some_and(|digest| parse_stored_digest(digest).is_err())
            || value.generation_floor == Some(0)
        {
            return Err(AppMacosPairingStoreError::Corrupt(
                "reset challenge scope or schema mismatch".to_owned(),
            ));
        }
        Ok(Some(value))
    }

    async fn persist_reset_challenge(
        &self,
        value: &StoredPairingResetChallengeDocument,
    ) -> Result<(), AppMacosPairingStoreError> {
        let bytes = serde_json::to_vec_pretty(value)
            .map_err(|error| AppMacosPairingStoreError::Corrupt(error.to_string()))?;
        if bytes.len() > APP_MACOS_PAIRING_MAX_STORE_BYTES {
            return Err(AppMacosPairingStoreError::Corrupt(
                "reset challenge exceeds its byte ceiling".to_owned(),
            ));
        }
        write_bytes_durably_with_mode(&self.reset_challenge_path, &bytes, Some(0o600)).await?;
        validate_private_regular_file(&self.reset_challenge_path).await
    }

    async fn mark_reset_challenge_consumed(
        &self,
        expected: &AppMacosHostPairingResetChallenge,
        acknowledgment_digest: &str,
        generation_floor: u64,
    ) -> Result<(), AppMacosPairingStoreError> {
        let mut value = self
            .load_reset_challenge()
            .await?
            .ok_or(AppMacosPairingStoreError::InvalidState)?;
        // A later owner request may already have installed a fresh challenge.
        // An exact older ack remains idempotent via the durable main anchor and
        // must not consume or overwrite that newer one.
        if value.challenge != *expected {
            return Ok(());
        }
        if generation_floor == 0
            || (value.acknowledgment_digest.is_some()
                && (value.acknowledgment_digest.as_deref() != Some(acknowledgment_digest)
                    || value.generation_floor != Some(generation_floor)))
        {
            return Err(AppMacosPairingStoreError::InvalidState);
        }
        value.acknowledgment_digest = Some(acknowledgment_digest.to_owned());
        value.generation_floor = Some(generation_floor);
        self.persist_reset_challenge(&value).await
    }

    async fn persist_document(
        &self,
        document: &AppMacosPairingDocument,
    ) -> Result<(), AppMacosPairingStoreError> {
        self.validate_storage_paths().await?;
        let bytes = Zeroizing::new(
            serde_json::to_vec_pretty(document)
                .map_err(|error| AppMacosPairingStoreError::Corrupt(error.to_string()))?,
        );
        if bytes.len() > APP_MACOS_PAIRING_MAX_STORE_BYTES {
            return Err(AppMacosPairingStoreError::Corrupt(
                "serialized store exceeds its byte ceiling".to_owned(),
            ));
        }
        write_bytes_durably_with_mode(&self.path, &bytes, Some(0o600)).await?;
        self.validate_storage_paths().await?;
        validate_private_regular_file(&self.path).await?;
        Ok(())
    }

    async fn validate_storage_paths(&self) -> Result<(), AppMacosPairingStoreError> {
        ensure_real_directory(&self.system_root, false).await?;
        ensure_real_directory(&self.namespace, true).await
    }
}

struct AppMacosPairingMutation<T> {
    output: T,
    changed: bool,
}

fn pairing_status_from_document(
    document: &AppMacosPairingDocument,
) -> Result<AppMacosPairingStatus, AppMacosPairingStoreError> {
    let active_generation = document
        .active
        .as_ref()
        .map(|active| active.proposal.generation);
    let transition_generation = document
        .transition
        .as_ref()
        .map(|transition| transition.proposal().generation);
    let (phase, finalization_ready) = match document.transition.as_ref() {
        Some(StoredPairingTransition::Pending { .. }) => {
            (AppMacosPairingLifecyclePhase::Pending, false)
        },
        Some(StoredPairingTransition::ApprovedPendingFinalize { finalization, .. }) => (
            AppMacosPairingLifecyclePhase::ApprovedPendingFinalize,
            finalization.is_some(),
        ),
        None if document.active.is_some() => (AppMacosPairingLifecyclePhase::Active, false),
        None if document.tombstone.is_some() => (AppMacosPairingLifecyclePhase::Revoked, false),
        None => (AppMacosPairingLifecyclePhase::Unpaired, false),
    };
    Ok(AppMacosPairingStatus {
        phase,
        active_generation,
        transition_generation,
        generation_high_water: document.generation_high_water,
        finalization_ready,
    })
}

impl<T> AppMacosPairingMutation<T> {
    fn changed(output: T) -> Self {
        Self {
            output,
            changed: true,
        }
    }

    fn unchanged(output: T) -> Self {
        Self {
            output,
            changed: false,
        }
    }
}

fn pairing_scope_digest(
    scope_binding_ref: &AppScopeBindingRef,
) -> Result<AppDigest, AppMacosPairingStoreError> {
    AppDigest::blake3_canonical_json(&json!({
        "schema": APP_MACOS_PAIRING_SCOPE_V1,
        "scope_binding_ref": scope_binding_ref,
    }))
    .map_err(|error| AppMacosPairingStoreError::Corrupt(error.to_string()))
}

fn normalize_requested_targets(
    targets: Vec<AppMacosPairingRequestedTarget>,
) -> Result<Vec<AppMacosHostPairingTargetRequest>, AppMacosPairingStoreError> {
    if targets.is_empty() || targets.len() > APP_MACOS_HOST_MAX_PAIRED_TARGETS {
        return Err(AppMacosPairingStoreError::InvalidTarget);
    }
    let mut target_refs = HashSet::with_capacity(targets.len());
    let mut bundle_ids = HashSet::with_capacity(targets.len());
    let mut targets = targets
        .into_iter()
        .map(|target| AppMacosHostPairingTargetRequest {
            target_ref: target.target_ref.to_string(),
            bundle_id: target.bundle_id,
        })
        .collect::<Vec<_>>();
    targets.sort_by(|left, right| left.target_ref.cmp(&right.target_ref));
    for target in &targets {
        if !target_refs.insert(target.target_ref.as_str())
            || !bundle_ids.insert(target.bundle_id.to_ascii_lowercase())
            || invalid_bundle_id(&target.bundle_id)
            || app_macos_host_protected_bundle_id(&target.bundle_id)
        {
            return Err(AppMacosPairingStoreError::InvalidTarget);
        }
    }
    Ok(targets)
}

fn invalid_bundle_id(value: &str) -> bool {
    value.is_empty()
        || value.len() > 255
        || value.starts_with('.')
        || value.ends_with('.')
        || !value.contains('.')
        || value.split('.').any(|segment| segment.is_empty())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-'))
}

fn random_pairing_id(prefix: &str) -> Result<String, AppMacosPairingStoreError> {
    let mut bytes = Zeroizing::new([0_u8; APP_MACOS_PAIRING_ID_RANDOM_BYTES]);
    rand::rngs::OsRng.fill_bytes(&mut bytes[..]);
    if bytes.iter().all(|byte| *byte == 0) {
        return Err(AppMacosPairingStoreError::InvalidHandshake);
    }
    let value = format!("{prefix}:{}", hex::encode(&*bytes));
    if value.len() > 64 {
        return Err(AppMacosPairingStoreError::InvalidHandshake);
    }
    Ok(value)
}

fn desktop_identity_binding(
    challenge: AppMacosDesktopIdentityChallenge,
    attestation: AppMacosDesktopIdentityAttestation,
    now_ms: i64,
) -> Result<StoredDesktopIdentityBinding, AppMacosPairingStoreError> {
    attestation
        .verify(&challenge, now_ms)
        .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
    let desktop_identity_digest = app_macos_desktop_identity_digest(
        &challenge.desktop_identity_key_id,
        &challenge.desktop_identity_public_key_hex,
    )
    .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
    let attestation_digest = attestation
        .digest()
        .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
    Ok(StoredDesktopIdentityBinding {
        challenge,
        attestation,
        desktop_identity_digest,
        attestation_digest,
    })
}

fn validate_desktop_identity_binding(
    identity: &StoredDesktopIdentityBinding,
) -> Result<(), AppMacosPairingStoreError> {
    identity
        .attestation
        .verify(&identity.challenge, identity.attestation.attested_at_ms)
        .map_err(|_| {
            AppMacosPairingStoreError::Corrupt("invalid desktop identity attestation".to_owned())
        })?;
    let expected_identity_digest = app_macos_desktop_identity_digest(
        &identity.challenge.desktop_identity_key_id,
        &identity.challenge.desktop_identity_public_key_hex,
    )
    .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid desktop identity key".to_owned()))?;
    let expected_attestation_digest = identity.attestation.digest().map_err(|_| {
        AppMacosPairingStoreError::Corrupt("invalid desktop identity attestation".to_owned())
    })?;
    if expected_identity_digest != identity.desktop_identity_digest
        || expected_attestation_digest != identity.attestation_digest
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "desktop identity digest mismatch".to_owned(),
        ));
    }
    parse_stored_digest(&identity.desktop_identity_digest)?;
    parse_stored_digest(&identity.attestation_digest)?;
    Ok(())
}

fn verify_desktop_approval(
    approval: &AppMacosHostPairingApproval,
    identity: &StoredDesktopIdentityBinding,
) -> Result<(), AppMacosPairingStoreError> {
    approval
        .verify_desktop_identity(
            &identity.challenge.desktop_identity_key_id,
            &identity.challenge.desktop_identity_public_key_hex,
        )
        .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?;
    if approval.host_identity_digest != identity.attestation.host_identity_digest {
        return Err(AppMacosPairingStoreError::InvalidHandshake);
    }
    Ok(())
}

fn approved_snapshot(
    proposal: &AppMacosHostPairingProposal,
    approval: &AppMacosHostPairingApproval,
    desktop_identity: &StoredDesktopIdentityBinding,
) -> Result<AppMacosApprovedPairingSnapshot, AppMacosPairingStoreError> {
    Ok(AppMacosApprovedPairingSnapshot {
        setup_id: proposal.setup_id.clone(),
        generation: proposal.generation,
        key_id: proposal.key_id.clone(),
        signing_key: AppMacosPairingSecret::decode(&proposal.signing_key_hex)?,
        scope_binding_ref: AppScopeBindingRef::parse(proposal.scope_binding_ref.clone())
            .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid scope binding".to_owned()))?,
        proposal_digest: parse_wire_digest(proposal.digest())?,
        approval_digest: parse_wire_digest(approval.digest())?,
        desktop_identity_digest: parse_stored_digest(&desktop_identity.desktop_identity_digest)?,
        desktop_identity_attestation_digest: parse_stored_digest(
            &desktop_identity.attestation_digest,
        )?,
        action_url: parse_action_url(&proposal.gateway_action_url)?,
        gateway_endpoint_digest: parse_stored_digest(&proposal.gateway_endpoint_digest)?,
        host_identity_digest: parse_stored_digest(&approval.host_identity_digest)?,
        cua_driver_binary_digest: parse_stored_digest(&approval.cua_driver_binary_digest)?,
        tcc_policy_digest: parse_stored_digest(&approval.tcc_policy_digest)?,
        tcc_epoch: approval.tcc_epoch,
        reviewed_targets: approval.reviewed_targets.clone(),
    })
}

fn active_snapshot(
    active: &StoredActivePairing,
) -> Result<AppMacosActivePairingSnapshot, AppMacosPairingStoreError> {
    Ok(AppMacosActivePairingSnapshot {
        generation: active.proposal.generation,
        key_id: active.proposal.key_id.clone(),
        signing_key: AppMacosPairingSecret::decode(&active.proposal.signing_key_hex)?,
        desktop_identity_key_id: active
            .desktop_identity
            .challenge
            .desktop_identity_key_id
            .clone(),
        desktop_identity_public_key_hex: active
            .desktop_identity
            .challenge
            .desktop_identity_public_key_hex
            .clone(),
        scope_binding_ref: AppScopeBindingRef::parse(active.proposal.scope_binding_ref.clone())
            .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid scope binding".to_owned()))?,
        action_url: parse_action_url(&active.proposal.gateway_action_url)?,
        gateway_endpoint_digest: parse_stored_digest(&active.proposal.gateway_endpoint_digest)?,
        desktop_identity_digest: parse_stored_digest(
            &active.desktop_identity.desktop_identity_digest,
        )?,
        desktop_identity_attestation_digest: parse_stored_digest(
            &active.desktop_identity.attestation_digest,
        )?,
        host_identity_digest: parse_stored_digest(&active.approval.host_identity_digest)?,
        cua_driver_binary_digest: parse_stored_digest(&active.approval.cua_driver_binary_digest)?,
        tcc_policy_digest: parse_stored_digest(&active.approval.tcc_policy_digest)?,
        tcc_epoch: active.approval.tcc_epoch,
        reviewed_targets: active.approval.reviewed_targets.clone(),
        owner_profile_digest: parse_stored_digest(&active.finalization.owner_profile_digest)?,
        owner_implementation_digest: parse_stored_digest(
            &active.finalization.owner_implementation_digest,
        )?,
        finalization_digest: parse_wire_digest(active.finalization.digest())?,
        activated_at_ms: active.finalized.activated_at_ms,
    })
}

fn parse_wire_digest(
    value: Result<String, magician_app_contract::macos_host::AppMacosHostWireError>,
) -> Result<AppDigest, AppMacosPairingStoreError> {
    parse_stored_digest(&value.map_err(|_| AppMacosPairingStoreError::InvalidHandshake)?)
}

fn parse_stored_digest(value: &str) -> Result<AppDigest, AppMacosPairingStoreError> {
    AppDigest::parse(value.to_owned())
        .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid digest".to_owned()))
}

fn parse_action_url(value: &str) -> Result<Url, AppMacosPairingStoreError> {
    AppMacosPairingEndpoint::parse(value).map(|endpoint| endpoint.action_url)
}

fn mint_status_capability(
    proposal: &AppMacosHostPairingProposal,
    recovery: Option<(
        &AppMacosHostPairingApproval,
        &AppMacosHostPairingFinalization,
    )>,
    now: DateTime<Utc>,
) -> Result<AppMacosHostPairingStatusRequest, AppMacosPairingStoreError> {
    let issued_at_ms = now.timestamp_millis();
    let expires_at_ms = issued_at_ms
        .checked_add(APP_MACOS_PAIRING_STATUS_TTL_MS)
        .ok_or(AppMacosPairingStoreError::InvalidHandshake)?;
    let signing_key = Zeroizing::new(
        decode_pairing_key(&proposal.signing_key_hex)
            .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid key".to_owned()))?,
    );
    match recovery {
        Some((approval, finalization)) => AppMacosHostPairingStatusRequest::mint_recovery(
            proposal,
            approval,
            finalization,
            &signing_key,
            issued_at_ms,
            expires_at_ms,
        ),
        None => AppMacosHostPairingStatusRequest::mint(
            proposal,
            &signing_key,
            issued_at_ms,
            expires_at_ms,
        ),
    }
    .map_err(|_| AppMacosPairingStoreError::InvalidHandshake)
}

fn validate_document(
    document: &AppMacosPairingDocument,
    expected_scope: &AppScopeBindingRef,
    expected_scope_digest: &AppDigest,
) -> Result<(), AppMacosPairingStoreError> {
    if (document.schema != APP_MACOS_PAIRING_STORE_V2
        && document.schema != APP_MACOS_PAIRING_STORE_V3)
        || document.scope_binding_ref != expected_scope.as_str()
        || document.scope_binding_digest != expected_scope_digest.as_str()
        || document.revision == 0
        || (document.schema == APP_MACOS_PAIRING_STORE_V2 && document.reset_anchor.is_some())
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "schema or scope binding mismatch".to_owned(),
        ));
    }
    if document.generation_high_water == 0
        || (document.active.is_none()
            && document.transition.is_none()
            && document.tombstone.is_none()
            && document.reset_anchor.is_none())
        || document.active.as_ref().is_some_and(|active| {
            active.proposal.generation == 0
                || active.proposal.generation > document.generation_high_water
        })
        || document.transition.as_ref().is_some_and(|transition| {
            transition.proposal().generation == 0
                || transition.proposal().generation > document.generation_high_water
        })
        || document.tombstone.as_ref().is_some_and(|tombstone| {
            tombstone.generation == 0 || tombstone.generation > document.generation_high_water
        })
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "invalid generation high-water state".to_owned(),
        ));
    }
    let maximum_generation = document
        .active
        .as_ref()
        .map(|active| active.proposal.generation)
        .into_iter()
        .chain(
            document
                .transition
                .as_ref()
                .map(|transition| transition.proposal().generation),
        )
        .chain(
            document
                .tombstone
                .as_ref()
                .map(|tombstone| tombstone.generation),
        )
        .chain(
            document
                .reset_anchor
                .as_ref()
                .map(|anchor| anchor.acknowledgment.prior_generation_floor),
        )
        .max()
        .unwrap_or(0);
    if maximum_generation != document.generation_high_water {
        return Err(AppMacosPairingStoreError::Corrupt(
            "generation high-water does not match durable state".to_owned(),
        ));
    }
    if let (Some(active), Some(transition)) = (&document.active, &document.transition) {
        if transition.proposal().generation <= active.proposal.generation {
            return Err(AppMacosPairingStoreError::Corrupt(
                "rotation generation does not advance active generation".to_owned(),
            ));
        }
    }
    if let Some(tombstone) = &document.tombstone {
        validate_desktop_identity_binding(&tombstone.desktop_identity)?;
        if tombstone.revoked_at_ms < 0 {
            return Err(AppMacosPairingStoreError::Corrupt(
                "invalid revocation timestamp".to_owned(),
            ));
        }
        let current_digest = tombstone.revoked.digest().map_err(|_| {
            AppMacosPairingStoreError::Corrupt(
                "invalid signed revocation acknowledgement".to_owned(),
            )
        })?;
        if current_digest != tombstone.revoked_digest
            || parse_stored_digest(&tombstone.revoked_digest).is_err()
            || tombstone.revoked.revoked_at_ms != tombstone.revoked_at_ms
            || tombstone.revoked.generation.checked_add(1) != Some(tombstone.generation)
        {
            return Err(AppMacosPairingStoreError::Corrupt(
                "revocation acknowledgement correlation mismatch".to_owned(),
            ));
        }
        tombstone
            .revoked
            .verify_desktop_identity(
                &tombstone.desktop_identity.challenge.desktop_identity_key_id,
                &tombstone
                    .desktop_identity
                    .challenge
                    .desktop_identity_public_key_hex,
            )
            .map_err(|_| {
                AppMacosPairingStoreError::Corrupt(
                    "invalid desktop-signed revocation acknowledgement".to_owned(),
                )
            })?;
        if document
            .active
            .as_ref()
            .is_some_and(|active| active.proposal.generation <= tombstone.generation)
            || document
                .transition
                .as_ref()
                .is_some_and(|transition| transition.proposal().generation <= tombstone.generation)
        {
            return Err(AppMacosPairingStoreError::Corrupt(
                "live generation is not ordered after tombstone".to_owned(),
            ));
        }
    }
    if let Some(anchor) = &document.reset_anchor {
        anchor
            .acknowledgment
            .verify(&anchor.challenge, anchor.acknowledgment.approved_at_ms)
            .map_err(|_| {
                AppMacosPairingStoreError::Corrupt(
                    "invalid desktop-signed reset acknowledgment".to_owned(),
                )
            })?;
        let acknowledgment_digest = anchor.acknowledgment.digest().map_err(|_| {
            AppMacosPairingStoreError::Corrupt("invalid reset acknowledgment digest".to_owned())
        })?;
        if anchor.challenge.scope_binding_ref != expected_scope.as_str()
            || anchor.acknowledgment.scope_binding_ref != expected_scope.as_str()
            || anchor.acknowledgment.prior_generation_floor == 0
            || anchor.acknowledgment.prior_generation_floor > document.generation_high_water
            || acknowledgment_digest != anchor.acknowledgment_digest
            || parse_stored_digest(&anchor.acknowledgment_digest).is_err()
            || parse_stored_digest(&anchor.acknowledgment.host_identity_digest).is_err()
            || parse_stored_digest(&anchor.acknowledgment.desktop_identity_digest).is_err()
        {
            return Err(AppMacosPairingStoreError::Corrupt(
                "reset anchor scope, floor or digest mismatch".to_owned(),
            ));
        }
    }
    if let Some(active) = &document.active {
        validate_active(active, expected_scope)?;
    }
    if let Some(transition) = &document.transition {
        match document.active.as_ref() {
            Some(active) => {
                let previous_signing_key = Zeroizing::new(
                    decode_pairing_key(&active.proposal.signing_key_hex).map_err(|_| {
                        AppMacosPairingStoreError::Corrupt("invalid active key".to_owned())
                    })?,
                );
                transition
                    .proposal()
                    .verify_rotation(active.proposal.generation, &previous_signing_key)
                    .map_err(|_| {
                        AppMacosPairingStoreError::Corrupt(
                            "rotation is not authorized by active generation".to_owned(),
                        )
                    })?;
            },
            None if transition.proposal().previous_generation.is_some() => {
                return Err(AppMacosPairingStoreError::Corrupt(
                    "initial setup carries rotation authority".to_owned(),
                ));
            },
            None => {},
        }
        validate_transition(transition, expected_scope)?;
    }
    Ok(())
}

fn validate_transition(
    transition: &StoredPairingTransition,
    expected_scope: &AppScopeBindingRef,
) -> Result<(), AppMacosPairingStoreError> {
    let proposal = transition.proposal();
    let desktop_identity = transition.desktop_identity();
    validate_proposal(proposal, expected_scope)?;
    validate_desktop_identity_binding(desktop_identity)?;
    if proposal.desktop_identity_attestation_digest != desktop_identity.attestation_digest {
        return Err(AppMacosPairingStoreError::Corrupt(
            "proposal desktop identity attestation mismatch".to_owned(),
        ));
    }
    match transition {
        StoredPairingTransition::Pending { .. } => Ok(()),
        StoredPairingTransition::ApprovedPendingFinalize {
            approval,
            finalization,
            ..
        } => {
            approval
                .verify(proposal, approval.approved_at_ms)
                .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid approval".to_owned()))?;
            verify_desktop_approval(approval, desktop_identity).map_err(|_| {
                AppMacosPairingStoreError::Corrupt(
                    "approval is not signed by the pinned desktop identity".to_owned(),
                )
            })?;
            if let Some(finalization) = finalization {
                finalization
                    .verify(proposal, approval, finalization.finalized_at_ms)
                    .map_err(|_| {
                        AppMacosPairingStoreError::Corrupt("invalid finalization".to_owned())
                    })?;
            }
            Ok(())
        },
    }
}

fn validate_active(
    active: &StoredActivePairing,
    expected_scope: &AppScopeBindingRef,
) -> Result<(), AppMacosPairingStoreError> {
    validate_proposal(&active.proposal, expected_scope)?;
    validate_desktop_identity_binding(&active.desktop_identity)?;
    if active.proposal.desktop_identity_attestation_digest
        != active.desktop_identity.attestation_digest
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "active proposal desktop identity attestation mismatch".to_owned(),
        ));
    }
    active
        .approval
        .verify(&active.proposal, active.approval.approved_at_ms)
        .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid active approval".to_owned()))?;
    verify_desktop_approval(&active.approval, &active.desktop_identity).map_err(|_| {
        AppMacosPairingStoreError::Corrupt(
            "active approval is not signed by the pinned desktop identity".to_owned(),
        )
    })?;
    active
        .finalization
        .verify(
            &active.proposal,
            &active.approval,
            active.finalization.finalized_at_ms,
        )
        .map_err(|_| {
            AppMacosPairingStoreError::Corrupt("invalid active finalization".to_owned())
        })?;
    let signing_key = Zeroizing::new(
        decode_pairing_key(&active.proposal.signing_key_hex)
            .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid active key".to_owned()))?,
    );
    active
        .finalized
        .verify(&active.finalization, &signing_key)
        .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid finalized ack".to_owned()))?;
    active
        .finalized
        .verify_desktop_identity(
            &active.desktop_identity.challenge.desktop_identity_key_id,
            &active
                .desktop_identity
                .challenge
                .desktop_identity_public_key_hex,
        )
        .map_err(|_| {
            AppMacosPairingStoreError::Corrupt(
                "finalized ack is not signed by the pinned desktop identity".to_owned(),
            )
        })?;
    // Approval expiry gates minting new finalization authority, not delivery
    // of the exact finalization already retained durably before that expiry.
    // Its acknowledgement may therefore be signed later during crash
    // recovery. Exact HMAC/desktop signatures and the wire-level
    // `activated_at_ms >= finalized_at_ms` check retain its lineage.
    Ok(())
}

fn validate_proposal(
    proposal: &AppMacosHostPairingProposal,
    expected_scope: &AppScopeBindingRef,
) -> Result<(), AppMacosPairingStoreError> {
    if proposal.schema != APP_MACOS_HOST_PAIRING_V1
        || proposal.scope_binding_ref != expected_scope.as_str()
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "proposal scope mismatch".to_owned(),
        ));
    }
    proposal
        .verify(proposal.issued_at_ms)
        .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid proposal".to_owned()))?;
    let endpoint = AppMacosPairingEndpoint::parse(&proposal.gateway_action_url)
        .map_err(|_| AppMacosPairingStoreError::Corrupt("invalid endpoint".to_owned()))?;
    if endpoint.gateway_endpoint_digest().as_str() != proposal.gateway_endpoint_digest {
        return Err(AppMacosPairingStoreError::Corrupt(
            "endpoint digest mismatch".to_owned(),
        ));
    }
    Ok(())
}

async fn ensure_private_namespace(path: &Path) -> Result<(), AppMacosPairingStoreError> {
    match tokio::fs::create_dir(path).await {
        Ok(()) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).await?;
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(error.into()),
    }
    ensure_real_directory(path, true).await
}

async fn ensure_real_directory(
    path: &Path,
    require_private: bool,
) -> Result<(), AppMacosPairingStoreError> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AppMacosPairingStoreError::Unavailable(format!(
            "{} is not a real directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    if require_private {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o777 != 0o700 {
            return Err(AppMacosPairingStoreError::Unavailable(format!(
                "{} does not have mode 0700",
                path.display()
            )));
        }
    }
    #[cfg(not(unix))]
    let _ = require_private;
    Ok(())
}

async fn validate_private_regular_file(path: &Path) -> Result<(), AppMacosPairingStoreError> {
    let metadata = tokio::fs::symlink_metadata(path).await?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AppMacosPairingStoreError::Unavailable(format!(
            "{} is not a real regular file",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(AppMacosPairingStoreError::Unavailable(format!(
                "{} does not have mode 0600",
                path.display()
            )));
        }
    }
    Ok(())
}

async fn read_private_bounded_file(
    path: PathBuf,
) -> Result<Option<Zeroizing<Vec<u8>>>, AppMacosPairingStoreError> {
    let permit = magician_core::blocking_admission::acquire_blocking_admission()
        .await
        .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?;
    magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
        read_private_bounded_file_blocking(&path)
    })
    .await
    .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?
}

fn read_private_bounded_file_blocking(
    path: &Path,
) -> Result<Option<Zeroizing<Vec<u8>>>, AppMacosPairingStoreError> {
    let named_metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if named_metadata.file_type().is_symlink() || !named_metadata.is_file() {
        return Err(AppMacosPairingStoreError::Unavailable(
            "pairing store is not a real regular file".to_owned(),
        ));
    }
    if named_metadata.len() > APP_MACOS_PAIRING_MAX_STORE_BYTES as u64 {
        return Err(AppMacosPairingStoreError::Corrupt(
            "pairing store exceeds its byte ceiling".to_owned(),
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
        if named_metadata.permissions().mode() & 0o777 != 0o600 {
            return Err(AppMacosPairingStoreError::Unavailable(
                "pairing store does not have mode 0600".to_owned(),
            ));
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
        let mut file = options.open(path)?;
        let opened_metadata = file.metadata()?;
        if !opened_metadata.is_file()
            || opened_metadata.dev() != named_metadata.dev()
            || opened_metadata.ino() != named_metadata.ino()
        {
            return Err(AppMacosPairingStoreError::Unavailable(
                "pairing store changed during open".to_owned(),
            ));
        }
        return read_exact_bounded(&mut file, &opened_metadata);
    }
    #[cfg(not(unix))]
    {
        let mut file = File::open(path)?;
        let opened_metadata = file.metadata()?;
        if !opened_metadata.is_file() {
            return Err(AppMacosPairingStoreError::Unavailable(
                "pairing store changed during open".to_owned(),
            ));
        }
        read_exact_bounded(&mut file, &opened_metadata)
    }
}

fn read_exact_bounded(
    file: &mut File,
    opened_metadata: &std::fs::Metadata,
) -> Result<Option<Zeroizing<Vec<u8>>>, AppMacosPairingStoreError> {
    let mut bytes = Zeroizing::new(Vec::with_capacity(
        usize::try_from(opened_metadata.len())
            .unwrap_or(0)
            .min(APP_MACOS_PAIRING_MAX_STORE_BYTES),
    ));
    std::io::Read::by_ref(file)
        .take((APP_MACOS_PAIRING_MAX_STORE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() > APP_MACOS_PAIRING_MAX_STORE_BYTES
        || after.len() != opened_metadata.len()
        || after.modified().ok() != opened_metadata.modified().ok()
    {
        return Err(AppMacosPairingStoreError::Corrupt(
            "pairing store changed during bounded read".to_owned(),
        ));
    }
    Ok(Some(bytes))
}

struct AppMacosPairingFileLock(File);

impl Drop for AppMacosPairingFileLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.0);
    }
}

async fn acquire_store_lock(
    path: PathBuf,
) -> Result<AppMacosPairingFileLock, AppMacosPairingStoreError> {
    let started = Instant::now();
    loop {
        let candidate = path.clone();
        let permit = magician_core::blocking_admission::acquire_blocking_admission()
            .await
            .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))?;
        let attempt =
            magician_core::blocking_admission::spawn_blocking_with_admission(permit, move || {
                try_acquire_store_lock(&candidate)
            })
            .await
            .map_err(|error| AppMacosPairingStoreError::Unavailable(error.to_string()))??;
        if let Some(file) = attempt {
            return Ok(AppMacosPairingFileLock(file));
        }
        if started.elapsed() >= APP_MACOS_PAIRING_LOCK_WAIT {
            return Err(AppMacosPairingStoreError::Unavailable(
                "pairing store writer lock timed out".to_owned(),
            ));
        }
        tokio::time::sleep(APP_MACOS_PAIRING_LOCK_POLL).await;
    }
}

fn try_acquire_store_lock(path: &Path) -> Result<Option<File>, AppMacosPairingStoreError> {
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    match file.try_lock_exclusive() {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
mod tests {
    use ring::{
        rand::SystemRandom,
        signature::{Ed25519KeyPair, KeyPair as _},
    };

    use super::*;

    struct DesktopIdentityFixture {
        key_pair: Ed25519KeyPair,
        key_id: String,
    }

    impl DesktopIdentityFixture {
        fn new(label: &str) -> Self {
            let pkcs8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap();
            Self {
                key_pair: Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).unwrap(),
                key_id: format!("desktop-{label}"),
            }
        }

        fn challenge_and_attestation(
            &self,
            now_ms: i64,
        ) -> (
            AppMacosDesktopIdentityChallenge,
            AppMacosDesktopIdentityAttestation,
        ) {
            let mut nonce = [0_u8; 16];
            rand::rngs::OsRng.fill_bytes(&mut nonce);
            let challenge = AppMacosDesktopIdentityChallenge::mint(
                self.key_id.clone(),
                hex::encode(self.key_pair.public_key().as_ref()),
                hex::encode(nonce),
                magician_app_contract::macos_host::app_macos_desktop_owner_approval_code_digest(
                    "test-owner-approval-code-1234",
                )
                .unwrap(),
                now_ms,
                now_ms + 30_000,
            )
            .unwrap();
            let mut attestation =
                AppMacosDesktopIdentityAttestation::unsigned(&challenge, digest("host"), now_ms)
                    .unwrap();
            attestation.signature_hex = hex::encode(
                self.key_pair
                    .sign(&attestation.signing_bytes().unwrap())
                    .as_ref(),
            );
            (challenge, attestation)
        }

        fn sign_approval(
            &self,
            proposal: &AppMacosPairingProposal,
            now_ms: i64,
        ) -> AppMacosPairingApproval {
            let reviewed_targets = proposal
                .requested_targets
                .iter()
                .map(|target| AppMacosHostPairingTargetIdentity {
                    target_ref: target.target_ref.clone(),
                    bundle_id: target.bundle_id.clone(),
                    application_identity_digest: digest(&target.bundle_id),
                })
                .collect();
            let mut approval = AppMacosHostPairingApproval {
                schema: APP_MACOS_HOST_PAIRING_V1.to_owned(),
                setup_id: proposal.setup_id.clone(),
                generation: proposal.generation,
                key_id: proposal.key_id.clone(),
                scope_binding_ref: proposal.scope_binding_ref.clone(),
                proposal_digest: proposal.digest().unwrap(),
                gateway_action_url: proposal.gateway_action_url.clone(),
                gateway_endpoint_digest: proposal.gateway_endpoint_digest.clone(),
                host_identity_digest: digest("host"),
                cua_driver_binary_digest: digest("cua"),
                tcc_policy_digest: digest("tcc"),
                tcc_epoch: 1,
                reviewed_targets,
                approved_at_ms: now_ms,
                expires_at_ms: proposal.expires_at_ms,
                signature: String::new(),
                desktop_identity_key_id: self.key_id.clone(),
                desktop_identity_signature_hex: String::new(),
            }
            .sign(
                proposal,
                &decode_pairing_key(&proposal.signing_key_hex).unwrap(),
            )
            .unwrap();
            approval.desktop_identity_signature_hex = hex::encode(
                self.key_pair
                    .sign(&approval.desktop_identity_signing_bytes().unwrap())
                    .as_ref(),
            );
            approval
        }

        fn sign_finalized(
            &self,
            proposal: &AppMacosPairingProposal,
            finalization: &AppMacosPairingFinalization,
            now_ms: i64,
        ) -> AppMacosPairingFinalized {
            let mut finalized = AppMacosPairingFinalized::sign(
                finalization,
                &decode_pairing_key(&proposal.signing_key_hex).unwrap(),
                now_ms,
            )
            .unwrap();
            finalized.desktop_identity_key_id = self.key_id.clone();
            finalized.desktop_identity_signature_hex = hex::encode(
                self.key_pair
                    .sign(&finalized.desktop_identity_signing_bytes().unwrap())
                    .as_ref(),
            );
            finalized
        }

        fn sign_revoked(
            &self,
            proposal: &AppMacosPairingProposal,
            now_ms: i64,
        ) -> AppMacosHostPairingRevoked {
            let mut revoked = AppMacosHostPairingRevoked::sign(
                proposal,
                &decode_pairing_key(&proposal.signing_key_hex).unwrap(),
                now_ms,
            )
            .unwrap();
            revoked.desktop_identity_key_id = self.key_id.clone();
            revoked.desktop_identity_signature_hex = hex::encode(
                self.key_pair
                    .sign(&revoked.desktop_identity_signing_bytes().unwrap())
                    .as_ref(),
            );
            revoked
        }

        fn sign_reset_ack(
            &self,
            challenge: &AppMacosHostPairingResetChallenge,
            generation_floor: u64,
            now_ms: i64,
        ) -> AppMacosHostPairingResetAck {
            let mut acknowledgment = AppMacosHostPairingResetAck::unsigned(
                challenge,
                digest("host"),
                self.key_id.clone(),
                hex::encode(self.key_pair.public_key().as_ref()),
                generation_floor,
                now_ms,
            )
            .unwrap();
            acknowledgment.desktop_identity_signature_hex = hex::encode(
                self.key_pair
                    .sign(&acknowledgment.signing_bytes().unwrap())
                    .as_ref(),
            );
            acknowledgment
        }
    }

    fn scope(value: &str) -> AppScopeBindingRef {
        AppScopeBindingRef::parse(value).unwrap()
    }

    fn target(value: &str, bundle_id: &str) -> AppMacosPairingRequestedTarget {
        AppMacosPairingRequestedTarget::new(AppReference::parse(value).unwrap(), bundle_id).unwrap()
    }

    fn endpoint(_port: u16) -> AppMacosPairingEndpoint {
        AppMacosPairingEndpoint::parse("http://127.0.0.1:3017/host/apps/macos/action").unwrap()
    }

    fn digest(label: &str) -> String {
        AppDigest::blake3(label.as_bytes()).to_string()
    }

    async fn begin_setup(
        store: &AppMacosPairingStore,
        identity: &DesktopIdentityFixture,
        targets: Vec<AppMacosPairingRequestedTarget>,
        now: DateTime<Utc>,
    ) -> AppMacosPairingProposal {
        let (challenge, attestation) = identity.challenge_and_attestation(now.timestamp_millis());
        store
            .begin_setup(endpoint(3017), targets, challenge, attestation, now)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn activation_requires_signed_finalize_ack_and_survives_reopen() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_a");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("activation");
        let proposal = begin_setup(
            &store,
            &identity,
            vec![target("target:textedit", "com.apple.TextEdit")],
            now,
        )
        .await;
        let approved = store
            .apply_approved(
                identity.sign_approval(&proposal, now.timestamp_millis()),
                now,
            )
            .await
            .unwrap();
        assert!(store.active_snapshot().await.unwrap().is_none());
        let finalization = store
            .finalization(
                &approved,
                AppDigest::blake3(b"profile"),
                AppDigest::blake3(b"implementation"),
                now,
            )
            .await
            .unwrap();
        assert!(store.active_snapshot().await.unwrap().is_none());
        drop(store);
        let reopened = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let recovery_now =
            now + chrono::Duration::milliseconds(APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS + 1);
        let recovered_finalization = reopened
            .finalization(
                &approved,
                AppDigest::blake3(b"profile"),
                AppDigest::blake3(b"implementation"),
                recovery_now,
            )
            .await
            .unwrap();
        assert_eq!(recovered_finalization, finalization);
        let recovery = reopened.status_request(recovery_now).await.unwrap();
        let finalization_digest = finalization.digest().unwrap();
        assert_eq!(
            recovery.finalization_digest.as_deref(),
            Some(finalization_digest.as_str())
        );
        assert!(recovery.approval_digest.is_some());
        // The desktop may receive the exact retained finalization only after
        // approval expiry following a runtime crash. Its fresh acknowledgement
        // must still activate that historical authority.
        let finalized =
            identity.sign_finalized(&proposal, &finalization, recovery_now.timestamp_millis());
        let active = reopened
            .mark_finalized(finalized, recovery_now)
            .await
            .unwrap();
        assert_eq!(active.generation(), proposal.generation);
        assert_eq!(
            active.desktop_identity_digest(),
            approved.desktop_identity_digest()
        );
        assert_eq!(
            active.desktop_identity_attestation_digest(),
            approved.desktop_identity_attestation_digest()
        );
        drop(reopened);
        let reopened_again = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        assert_eq!(
            reopened_again
                .active_snapshot()
                .await
                .unwrap()
                .unwrap()
                .generation(),
            proposal.generation
        );
    }

    #[tokio::test]
    async fn desktop_revocation_capability_binds_exact_pending_and_active_generation() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_revoke");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("revoke");
        let proposal = begin_setup(
            &store,
            &identity,
            vec![target("target:revoke", "com.apple.TextEdit")],
            now,
        )
        .await;
        let pending = store
            .revocation_request(proposal.generation, now)
            .await
            .unwrap();
        assert_eq!(
            pending.action_url().as_str(),
            proposal.gateway_action_url.as_str()
        );
        assert!(pending.capability().approval_digest.is_none());
        pending
            .capability()
            .verify(
                &proposal,
                &decode_pairing_key(&proposal.signing_key_hex).unwrap(),
                now.timestamp_millis(),
            )
            .unwrap();
        assert!(matches!(
            store.revocation_request(proposal.generation + 1, now).await,
            Err(AppMacosPairingStoreError::StaleGeneration)
        ));

        let approved = store
            .apply_approved(
                identity.sign_approval(&proposal, now.timestamp_millis()),
                now,
            )
            .await
            .unwrap();
        let finalization = store
            .finalization(
                &approved,
                AppDigest::blake3(b"profile"),
                AppDigest::blake3(b"implementation"),
                now,
            )
            .await
            .unwrap();
        let finalized = identity.sign_finalized(&proposal, &finalization, now.timestamp_millis());
        store.mark_finalized(finalized, now).await.unwrap();
        let active = store
            .revocation_request(proposal.generation, now)
            .await
            .unwrap();
        assert_eq!(
            active.capability().approval_digest.as_deref(),
            Some(finalization.approval_digest.as_str())
        );
        assert_eq!(
            active.capability().finalization_digest.as_deref(),
            Some(finalization.digest().unwrap().as_str())
        );
        active
            .capability()
            .verify(
                &proposal,
                &decode_pairing_key(&proposal.signing_key_hex).unwrap(),
                now.timestamp_millis(),
            )
            .unwrap();
        let revoked = identity.sign_revoked(&proposal, now.timestamp_millis());
        store
            .revoke_with_ack(proposal.generation, revoked, now)
            .await
            .unwrap();
        assert_eq!(
            store.status().await.unwrap().phase(),
            AppMacosPairingLifecyclePhase::Revoked
        );
    }

    #[tokio::test]
    async fn rotation_and_revoke_make_old_approvals_and_acks_stale() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_b");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("rotation");
        let first = begin_setup(
            &store,
            &identity,
            vec![target("target:first", "com.apple.TextEdit")],
            now,
        )
        .await;
        let stale_approval = identity.sign_approval(&first, now.timestamp_millis());
        let first_approved = store
            .apply_approved(stale_approval.clone(), now)
            .await
            .unwrap();
        let first_finalization = store
            .finalization(
                &first_approved,
                AppDigest::blake3(b"profile-1"),
                AppDigest::blake3(b"implementation-1"),
                now,
            )
            .await
            .unwrap();
        let first_finalized =
            identity.sign_finalized(&first, &first_finalization, now.timestamp_millis());
        store.mark_finalized(first_finalized, now).await.unwrap();
        let rotation_now = now + chrono::Duration::milliseconds(1);
        let second = begin_setup(
            &store,
            &identity,
            vec![target("target:second", "com.apple.Preview")],
            rotation_now,
        )
        .await;
        assert_eq!(second.previous_generation, Some(first.generation));
        second
            .verify_rotation(
                first.generation,
                &decode_pairing_key(&first.signing_key_hex).unwrap(),
            )
            .unwrap();
        assert!(matches!(
            store.apply_approved(stale_approval, rotation_now).await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
                | Err(AppMacosPairingStoreError::StaleGeneration)
        ));
        let approved = store
            .apply_approved(
                identity.sign_approval(&second, rotation_now.timestamp_millis()),
                rotation_now,
            )
            .await
            .unwrap();
        let finalization = store
            .finalization(
                &approved,
                AppDigest::blake3(b"profile-2"),
                AppDigest::blake3(b"implementation-2"),
                rotation_now,
            )
            .await
            .unwrap();
        let finalized =
            identity.sign_finalized(&second, &finalization, rotation_now.timestamp_millis());
        store
            .mark_finalized(finalized.clone(), rotation_now)
            .await
            .unwrap();
        let revoked = identity.sign_revoked(&second, rotation_now.timestamp_millis());
        store
            .revoke_with_ack(second.generation, revoked, rotation_now)
            .await
            .unwrap();
        assert!(store.active_snapshot().await.unwrap().is_none());
        assert!(matches!(
            store.mark_finalized(finalized, rotation_now).await,
            Err(AppMacosPairingStoreError::StaleGeneration)
        ));
    }

    #[test]
    fn endpoint_parser_denies_injection_and_non_exact_routes() {
        for value in [
            "https://127.0.0.1:3013/host/apps/macos/action",
            "http://user@127.0.0.1:3013/host/apps/macos/action",
            "http://127.0.0.1:3013/host/apps/macos/actions",
            "http://127.0.0.1:3013/host/apps/macos/action?next=evil",
            "http://example.com:3013/host/apps/macos/action",
            "http://[::1]:3013/host/apps/macos/action",
            "http://host.docker.internal:3013/host/apps/macos/action",
            "http://localhost/host/apps/macos/action",
        ] {
            assert!(AppMacosPairingEndpoint::parse(value).is_err(), "{value}");
        }
    }

    #[tokio::test]
    async fn target_set_substitution_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_c");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("substitution");
        let proposal = begin_setup(
            &store,
            &identity,
            vec![target("target:textedit", "com.apple.TextEdit")],
            now,
        )
        .await;
        let mut substituted = identity.sign_approval(&proposal, now.timestamp_millis());
        substituted.reviewed_targets[0].bundle_id = "com.apple.Preview".to_owned();
        assert!(matches!(
            store.apply_approved(substituted, now).await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
        ));
    }

    #[tokio::test]
    async fn desktop_identity_is_verified_before_key_creation_and_remains_pinned() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_identity");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let owner = DesktopIdentityFixture::new("owner");
        let attacker = DesktopIdentityFixture::new("attacker");

        let (challenge, mut invalid_attestation) =
            owner.challenge_and_attestation(now.timestamp_millis());
        invalid_attestation.signature_hex = "00".repeat(64);
        assert!(matches!(
            store
                .begin_setup(
                    endpoint(3017),
                    vec![target("target:identity", "com.apple.TextEdit")],
                    challenge,
                    invalid_attestation,
                    now,
                )
                .await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
        ));
        assert_eq!(
            store.status().await.unwrap().phase(),
            AppMacosPairingLifecyclePhase::Unpaired
        );

        let proposal = begin_setup(
            &store,
            &owner,
            vec![target("target:identity", "com.apple.TextEdit")],
            now,
        )
        .await;
        assert!(matches!(
            store
                .apply_approved(
                    attacker.sign_approval(&proposal, now.timestamp_millis()),
                    now,
                )
                .await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
        ));

        let replacement_now =
            now + chrono::Duration::milliseconds(APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS + 1);
        let (replacement_challenge, replacement_attestation) =
            attacker.challenge_and_attestation(replacement_now.timestamp_millis());
        assert!(matches!(
            store
                .begin_setup(
                    endpoint(3017),
                    vec![target("target:identity", "com.apple.TextEdit")],
                    replacement_challenge,
                    replacement_attestation,
                    replacement_now,
                )
                .await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
        ));
    }

    #[tokio::test]
    async fn setup_keys_and_ids_are_random_and_nonzero() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_d");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("random");
        let first = begin_setup(
            &store,
            &identity,
            vec![target("target:a", "com.apple.TextEdit")],
            now,
        )
        .await;
        let second_now =
            now + chrono::Duration::milliseconds(APP_MACOS_HOST_PAIRING_MAX_LIFETIME_MS + 1);
        let second = begin_setup(
            &store,
            &identity,
            vec![target("target:a", "com.apple.TextEdit")],
            second_now,
        )
        .await;
        assert_ne!(first.setup_id, second.setup_id);
        assert_ne!(first.key_id, second.key_id);
        assert_ne!(first.signing_key_hex, second.signing_key_hex);
        assert_ne!(
            decode_pairing_key(&first.signing_key_hex).unwrap(),
            [0_u8; 32]
        );
    }

    #[tokio::test]
    async fn signed_reset_recovers_lost_runtime_state_and_preserves_generation_floor() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_reset");
        let store = AppMacosPairingStore::open_for_reset(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("reset");
        let challenge = store.reset_challenge(now).await.unwrap();
        let acknowledgment = identity.sign_reset_ack(&challenge, 41, now.timestamp_millis());

        let status = store
            .apply_reset_ack(acknowledgment.clone(), now + chrono::Duration::minutes(3))
            .await
            .unwrap();
        assert_eq!(status.phase(), AppMacosPairingLifecyclePhase::Unpaired);
        assert_eq!(status.generation_high_water(), 41);
        let next_challenge = store
            .reset_challenge(now + chrono::Duration::seconds(1))
            .await
            .unwrap();
        assert_ne!(
            next_challenge.digest().unwrap(),
            challenge.digest().unwrap(),
            "a consumed challenge never becomes reset authority again"
        );
        // Exact delivery replay is idempotent even after challenge expiry.
        assert_eq!(
            store
                .apply_reset_ack(acknowledgment, now + chrono::Duration::minutes(4))
                .await
                .unwrap()
                .generation_high_water(),
            41
        );

        let reopened = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let proposal = begin_setup(
            &reopened,
            &identity,
            vec![target("target:reset", "com.apple.TextEdit")],
            now + chrono::Duration::minutes(5),
        )
        .await;
        assert_eq!(proposal.generation, 42);
    }

    #[tokio::test]
    async fn reset_rejects_live_authority_wrong_identity_and_floor_rollback() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_reset_guards");
        let store = AppMacosPairingStore::open_for_reset(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("reset-guard");
        let challenge = store.reset_challenge(now).await.unwrap();
        store
            .apply_reset_ack(
                identity.sign_reset_ack(&challenge, 9, now.timestamp_millis()),
                now,
            )
            .await
            .unwrap();

        let stale = identity.sign_reset_ack(&challenge, 8, now.timestamp_millis());
        assert!(matches!(
            store.apply_reset_ack(stale, now).await,
            Err(AppMacosPairingStoreError::StaleGeneration)
        ));
        let fresh_challenge = store
            .reset_challenge(now + chrono::Duration::seconds(1))
            .await
            .unwrap();
        let attacker = DesktopIdentityFixture::new("reset-attacker");
        let replacement =
            attacker.sign_reset_ack(&fresh_challenge, 10, now.timestamp_millis() + 1_000);
        assert!(matches!(
            store
                .apply_reset_ack(replacement, now + chrono::Duration::seconds(1))
                .await,
            Err(AppMacosPairingStoreError::InvalidHandshake)
        ));

        let ordinary = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let _ = begin_setup(
            &ordinary,
            &identity,
            vec![target("target:live-after-reset", "com.apple.TextEdit")],
            now + chrono::Duration::seconds(1),
        )
        .await;
        assert!(matches!(
            AppMacosPairingStore::open_for_reset(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::InvalidState)
        ));
    }

    #[tokio::test]
    async fn consumed_reset_ack_cannot_roll_back_after_main_document_loss() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_reset_replay");
        let store = AppMacosPairingStore::open_for_reset(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("reset-replay");
        let challenge = store.reset_challenge(now).await.unwrap();
        let old_ack = identity.sign_reset_ack(&challenge, 21, now.timestamp_millis());
        store.apply_reset_ack(old_ack.clone(), now).await.unwrap();
        tokio::fs::remove_file(&store.path).await.unwrap();

        let lost = AppMacosPairingStore::open_for_reset(root.path(), &binding)
            .await
            .unwrap();
        assert!(matches!(
            lost.apply_reset_ack(old_ack, now).await,
            Err(AppMacosPairingStoreError::StaleGeneration)
        ));
        let fresh = lost
            .reset_challenge(now + chrono::Duration::seconds(1))
            .await
            .unwrap();
        assert_ne!(fresh.digest().unwrap(), challenge.digest().unwrap());
        assert_eq!(
            lost.apply_reset_ack(
                identity.sign_reset_ack(&fresh, 22, now.timestamp_millis() + 1_000),
                now + chrono::Duration::seconds(1),
            )
            .await
            .unwrap()
            .generation_high_water(),
            22
        );
    }

    #[tokio::test]
    async fn owner_confirmed_reset_can_replace_a_corrupt_regular_store() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_reset_corrupt");
        let ordinary = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let path = ordinary.path.clone();
        tokio::fs::write(&path, b"corrupt-pairing-state")
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        let reset = AppMacosPairingStore::open_for_reset(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("reset-corrupt");
        let challenge = reset.reset_challenge(now).await.unwrap();
        reset
            .apply_reset_ack(
                identity.sign_reset_ack(&challenge, 73, now.timestamp_millis()),
                now,
            )
            .await
            .unwrap();
        assert_eq!(
            AppMacosPairingStore::open(root.path(), &binding)
                .await
                .unwrap()
                .status()
                .await
                .unwrap()
                .generation_high_water(),
            73
        );
    }

    #[tokio::test]
    async fn scope_mismatch_and_corrupt_or_oversized_store_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_e");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let now = Utc::now();
        let identity = DesktopIdentityFixture::new("corrupt");
        let _ = begin_setup(
            &store,
            &identity,
            vec![target("target:a", "com.apple.TextEdit")],
            now,
        )
        .await;
        let path = store.path.clone();
        let mut value: serde_json::Value =
            serde_json::from_slice(&tokio::fs::read(&path).await.unwrap()).unwrap();
        value["scope_binding_ref"] = json!("scope_substituted");
        tokio::fs::write(&path, serde_json::to_vec(&value).unwrap())
            .await
            .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .await
                .unwrap();
        }
        assert!(matches!(
            AppMacosPairingStore::open(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::Corrupt(_))
        ));

        tokio::fs::write(&path, b"not-json").await.unwrap();
        assert!(matches!(
            AppMacosPairingStore::open(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::Corrupt(_))
        ));
        tokio::fs::write(&path, vec![b'x'; APP_MACOS_PAIRING_MAX_STORE_BYTES + 1])
            .await
            .unwrap();
        assert!(matches!(
            AppMacosPairingStore::open(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::Corrupt(_))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn symlink_store_is_unavailable_and_never_followed() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let binding = scope("scope_pairing_f");
        let store = AppMacosPairingStore::open(root.path(), &binding)
            .await
            .unwrap();
        let path = store.path.clone();
        let target = root.path().join("outside.json");
        tokio::fs::write(&target, b"outside").await.unwrap();
        symlink(&target, &path).unwrap();
        assert!(matches!(
            AppMacosPairingStore::open(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::Unavailable(_))
        ));
        assert!(matches!(
            AppMacosPairingStore::open_for_reset(root.path(), &binding).await,
            Err(AppMacosPairingStoreError::Unavailable(_))
        ));
        assert_eq!(tokio::fs::read(&target).await.unwrap(), b"outside");
    }
}

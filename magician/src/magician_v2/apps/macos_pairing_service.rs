//! Authenticated control-plane owner for the runtime/desktop macOS pairing.
//!
//! HTTP handlers supply an already verified [`AuthenticatedAppScope`]. This
//! service owns the durable state transitions and the exact private desktop
//! wire; callers never receive proposal key material or construct approval,
//! finalization, status, endpoint-path, or target-reference bytes themselves.

use std::{sync::Arc, time::Duration as StdDuration};

use chrono::{DateTime, Utc};
use futures_util::StreamExt as _;
use magician_app_contract::macos_host::{
    app_macos_desktop_identity_digest, app_macos_desktop_owner_approval_code_digest,
    AppMacosDesktopIdentityAttestation, AppMacosDesktopIdentityChallenge,
    AppMacosHostPairingStatusResponse, APP_MACOS_HOST_PAIRING_V1,
};
pub use magician_app_contract::macos_host::{
    AppMacosHostPairingResetAck, AppMacosHostPairingResetChallenge,
};
use reqwest::{StatusCode, Url};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use zeroize::Zeroize as _;

use super::{
    authority::AuthenticatedAppScope,
    macos_host::{
        macos_host_owner_implementation_digest, macos_host_owner_profile_digest,
        AppMacosHostPairing,
    },
    macos_pairing::{
        AppMacosPairingEndpoint, AppMacosPairingLifecyclePhase, AppMacosPairingRequestedTarget,
        AppMacosPairingStatus, AppMacosPairingStore,
    },
    models::{AppDigest, AppReference},
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

const APP_MACOS_PAIRING_CONTROL_RESPONSE_CEILING: usize = 512 * 1024;
const APP_MACOS_PAIRING_CONTROL_TIMEOUT: StdDuration = StdDuration::from_secs(15);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppMacosPairingControlPhase {
    Unpaired,
    PendingNativeApproval,
    ApprovedPendingFinalize,
    Active,
    Revoked,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosPairingControlStatus {
    pub phase: AppMacosPairingControlPhase,
    pub active_generation: Option<u64>,
    pub transition_generation: Option<u64>,
    pub generation_high_water: u64,
    pub finalization_ready: bool,
}

/// Public half of the currently active, code-attested desktop identity. The
/// private Keychain signer never crosses this boundary.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosActiveDesktopIdentity {
    pub pairing_generation: u64,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_digest: String,
    pub desktop_identity_attestation_digest: String,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosPairingSetupRequest {
    pub action_url: String,
    pub bundle_id: String,
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_fingerprint: String,
    pub owner_approval_code: String,
}

impl std::fmt::Debug for AppMacosPairingSetupRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosPairingSetupRequest")
            .field("action_url", &self.action_url)
            .field("bundle_id", &self.bundle_id)
            .field("desktop_identity_key_id", &self.desktop_identity_key_id)
            .field(
                "desktop_identity_fingerprint",
                &self.desktop_identity_fingerprint,
            )
            .finish_non_exhaustive()
    }
}

impl Drop for AppMacosPairingSetupRequest {
    fn drop(&mut self) {
        self.owner_approval_code.zeroize();
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMacosPairingRevokeRequest {
    pub expected_generation: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum AppMacosPairingControlError {
    #[error("the macOS pairing control request is invalid")]
    InvalidRequest,
    #[error("the macOS pairing lifecycle state is unavailable or corrupt")]
    StoreUnavailable,
    #[error("the exact typed desktop pairing endpoint is unavailable")]
    DesktopUnavailable,
    #[error("the desktop pairing response is invalid")]
    InvalidDesktopResponse,
}

#[derive(Clone)]
pub struct AppMacosPairingOwnerService {
    workspace: ArtifactV2Workspace,
    client: reqwest::Client,
    control: Arc<tokio::sync::Mutex<()>>,
}

impl AppMacosPairingOwnerService {
    pub fn new(workspace: ArtifactV2Workspace) -> Result<Self, AppMacosPairingControlError> {
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(StdDuration::from_secs(5))
            .timeout(APP_MACOS_PAIRING_CONTROL_TIMEOUT)
            .build()
            .map_err(|_| AppMacosPairingControlError::DesktopUnavailable)?;
        Ok(Self {
            workspace,
            client,
            control: Arc::new(tokio::sync::Mutex::new(())),
        })
    }

    pub async fn begin_setup(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppMacosPairingSetupRequest,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        let _control = self.control.lock().await;
        let endpoint = AppMacosPairingEndpoint::parse(&request.action_url)
            .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
        let expected_fingerprint = app_macos_desktop_identity_digest(
            &request.desktop_identity_key_id,
            &request.desktop_identity_public_key_hex,
        )
        .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
        if request.desktop_identity_fingerprint != expected_fingerprint {
            return Err(AppMacosPairingControlError::InvalidRequest);
        }
        let approval_code_digest =
            app_macos_desktop_owner_approval_code_digest(&request.owner_approval_code)
                .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
        let challenge = AppMacosDesktopIdentityChallenge::mint(
            request.desktop_identity_key_id.clone(),
            request.desktop_identity_public_key_hex.clone(),
            format!("nonce:app-macos-desktop:{}", uuid::Uuid::new_v4()),
            approval_code_digest,
            now.timestamp_millis(),
            (now + chrono::Duration::seconds(30)).timestamp_millis(),
        )
        .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
        let attestation = self
            .submit_identity_attestation(endpoint.action_url(), &challenge)
            .await?;
        attestation
            .verify(&challenge, Utc::now().timestamp_millis())
            .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)?;
        let target_ref = pairing_target_ref(authenticated, &request.bundle_id)?;
        let target = AppMacosPairingRequestedTarget::new(target_ref, request.bundle_id.clone())
            .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
        let store = self.store(authenticated).await?;
        let proposal = store
            .begin_setup(endpoint, vec![target], challenge, attestation, now)
            .await
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
        self.submit_proposal(&proposal).await?;
        self.advance_store(&store, now).await
    }

    pub async fn advance(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        let _control = self.control.lock().await;
        let store = self.store(authenticated).await?;
        self.advance_store(&store, now).await
    }

    pub async fn status(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        let store = self.store(authenticated).await?;
        store
            .status()
            .await
            .map(control_status)
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    pub async fn active_desktop_identity(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<Option<AppMacosActiveDesktopIdentity>, AppMacosPairingControlError> {
        let store = self.store(authenticated).await?;
        let snapshot = store
            .active_snapshot()
            .await
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
        Ok(snapshot.map(|snapshot| AppMacosActiveDesktopIdentity {
            pairing_generation: snapshot.generation(),
            desktop_identity_key_id: snapshot.desktop_identity_key_id().to_owned(),
            desktop_identity_public_key_hex: snapshot.desktop_identity_public_key_hex().to_owned(),
            desktop_identity_digest: snapshot.desktop_identity_digest().to_string(),
            desktop_identity_attestation_digest: snapshot
                .desktop_identity_attestation_digest()
                .to_string(),
        }))
    }

    pub async fn revoke(
        &self,
        authenticated: &AuthenticatedAppScope,
        request: AppMacosPairingRevokeRequest,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        if request.expected_generation == 0 {
            return Err(AppMacosPairingControlError::InvalidRequest);
        }
        let _control = self.control.lock().await;
        let store = self.store(authenticated).await?;
        let revocation = store
            .revocation_request(request.expected_generation, now)
            .await
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
        let response = match self
            .submit_revoke(revocation.action_url(), revocation.capability())
            .await
        {
            Ok(response) => response,
            Err(first_error) => {
                // A transition proposal might not have reached the desktop.
                // Re-post only the exact retained proposal, then retry the
                // same fresh revocation capability. Active/native-revoked
                // generations skip this branch or return their retained ack.
                if let Some(proposal) = store
                    .setup_proposal()
                    .await
                    .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?
                {
                    self.submit_proposal(&proposal).await?;
                    self.submit_revoke(revocation.action_url(), revocation.capability())
                        .await?
                } else {
                    return Err(first_error);
                }
            },
        };
        let AppMacosHostPairingStatusResponse::Revoked { revoked } = response else {
            return Err(AppMacosPairingControlError::InvalidDesktopResponse);
        };
        store
            .revoke_with_ack(request.expected_generation, revoked, now)
            .await
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
        store
            .status()
            .await
            .map(control_status)
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    /// Persist a one-shot reset challenge without touching the possibly lost
    /// or corrupt pairing authority document. The trusted desktop UI must show
    /// the exact identity/floor and return its Ed25519-signed acknowledgment.
    pub async fn reset_challenge(
        &self,
        authenticated: &AuthenticatedAppScope,
        now: DateTime<Utc>,
    ) -> Result<AppMacosHostPairingResetChallenge, AppMacosPairingControlError> {
        let _control = self.control.lock().await;
        let store = self.reset_store(authenticated).await?;
        store
            .reset_challenge(now)
            .await
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    /// Complete the exceptional two-sided reset only after the desktop has
    /// durably cleared its verifier and signed the retained challenge with its
    /// displayed identity and monotonic generation floor.
    pub async fn reset(
        &self,
        authenticated: &AuthenticatedAppScope,
        acknowledgment: AppMacosHostPairingResetAck,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        let _control = self.control.lock().await;
        let store = self.reset_store(authenticated).await?;
        store
            .apply_reset_ack(acknowledgment, now)
            .await
            .map(control_status)
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    async fn store(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<AppMacosPairingStore, AppMacosPairingControlError> {
        AppMacosPairingStore::open(
            self.workspace.base_root(),
            authenticated.scope_binding_ref(),
        )
        .await
        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    async fn reset_store(
        &self,
        authenticated: &AuthenticatedAppScope,
    ) -> Result<AppMacosPairingStore, AppMacosPairingControlError> {
        AppMacosPairingStore::open_for_reset(
            self.workspace.base_root(),
            authenticated.scope_binding_ref(),
        )
        .await
        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    async fn advance_store(
        &self,
        store: &AppMacosPairingStore,
        now: DateTime<Utc>,
    ) -> Result<AppMacosPairingControlStatus, AppMacosPairingControlError> {
        // At most one Pending -> Approved -> Active progression is possible in
        // one call. The bound also prevents a hostile desktop from inducing an
        // unbounded status loop with validly shaped but stale responses.
        for _ in 0..3 {
            let current = store
                .status()
                .await
                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
            match current.phase() {
                AppMacosPairingLifecyclePhase::Unpaired
                | AppMacosPairingLifecyclePhase::Revoked => {
                    return Ok(control_status(current));
                },
                AppMacosPairingLifecyclePhase::Active => {
                    let generation = current
                        .active_generation()
                        .ok_or(AppMacosPairingControlError::StoreUnavailable)?;
                    let recovery = store
                        .revocation_request(generation, now)
                        .await
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                    match self
                        .submit_status(recovery.action_url().as_str(), recovery.capability())
                        .await?
                    {
                        AppMacosHostPairingStatusResponse::Active { finalized } => {
                            store
                                .mark_finalized(finalized, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                        },
                        AppMacosHostPairingStatusResponse::Revoked { revoked } => {
                            store
                                .revoke_with_ack(generation, revoked, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                        },
                        AppMacosHostPairingStatusResponse::Pending
                        | AppMacosHostPairingStatusResponse::Approved { .. } => {
                            return Err(AppMacosPairingControlError::InvalidDesktopResponse);
                        },
                    }
                    return store
                        .status()
                        .await
                        .map(control_status)
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable);
                },
                AppMacosPairingLifecyclePhase::Pending => {
                    let proposal = store
                        .setup_proposal()
                        .await
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?
                        .ok_or(AppMacosPairingControlError::StoreUnavailable)?;
                    // Proposal delivery is idempotent at the desktop owner and
                    // makes a lost initial 202 response safely retryable.
                    self.submit_proposal(&proposal).await?;
                    let request = store
                        .status_request(now)
                        .await
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                    match self
                        .submit_status(&proposal.gateway_action_url, &request)
                        .await?
                    {
                        AppMacosHostPairingStatusResponse::Pending => {
                            return Ok(control_status(current));
                        },
                        AppMacosHostPairingStatusResponse::Approved { approval } => {
                            store
                                .apply_approved(approval, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                        },
                        AppMacosHostPairingStatusResponse::Active { finalized } => {
                            store
                                .mark_finalized(finalized, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                        },
                        AppMacosHostPairingStatusResponse::Revoked { revoked } => {
                            store
                                .revoke_with_ack(proposal.generation, revoked, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                            return store
                                .status()
                                .await
                                .map(control_status)
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable);
                        },
                    }
                },
                AppMacosPairingLifecyclePhase::ApprovedPendingFinalize => {
                    let approved = store
                        .approved_snapshot()
                        .await
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?
                        .ok_or(AppMacosPairingControlError::StoreUnavailable)?;
                    let mut key = approved.signing_key();
                    let pairing_result = AppMacosHostPairing::from_pairing_owner(
                        approved.key_id().to_owned(),
                        key,
                        approved.desktop_identity_digest().clone(),
                        approved.desktop_identity_attestation_digest().clone(),
                        approved.host_identity_digest().clone(),
                        approved.cua_driver_binary_digest().clone(),
                        approved.gateway_endpoint_digest().clone(),
                        approved.tcc_policy_digest().clone(),
                        approved.tcc_epoch(),
                    );
                    key.zeroize();
                    let pairing = pairing_result
                        .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)?;
                    let profile = macos_host_owner_profile_digest(&pairing)
                        .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)?;
                    let implementation =
                        macos_host_owner_implementation_digest(approved.cua_driver_binary_digest())
                            .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)?;
                    let finalization = store
                        .finalization(&approved, profile, implementation, now)
                        .await
                        .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                    match self
                        .submit_finalization(approved.action_url(), &finalization)
                        .await
                    {
                        Ok(finalized) => {
                            store
                                .mark_finalized(finalized, now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                        },
                        Err(_) => {
                            // Finalization was durable before its first send.
                            // If the response was lost, recover the exact signed
                            // ack; otherwise retain this state for a byte-identical
                            // retry and never mint/replace authority.
                            let request = store
                                .status_request(now)
                                .await
                                .map_err(|_| AppMacosPairingControlError::StoreUnavailable)?;
                            match self
                                .submit_status(approved.action_url().as_str(), &request)
                                .await
                            {
                                Ok(AppMacosHostPairingStatusResponse::Active { finalized }) => {
                                    store.mark_finalized(finalized, now).await.map_err(|_| {
                                        AppMacosPairingControlError::StoreUnavailable
                                    })?;
                                },
                                Ok(AppMacosHostPairingStatusResponse::Revoked { revoked }) => {
                                    store
                                        .revoke_with_ack(approved.generation(), revoked, now)
                                        .await
                                        .map_err(|_| {
                                            AppMacosPairingControlError::StoreUnavailable
                                        })?;
                                    return store.status().await.map(control_status).map_err(
                                        |_| AppMacosPairingControlError::StoreUnavailable,
                                    );
                                },
                                Ok(
                                    AppMacosHostPairingStatusResponse::Pending
                                    | AppMacosHostPairingStatusResponse::Approved { .. },
                                )
                                | Err(_) => {
                                    return store.status().await.map(control_status).map_err(
                                        |_| AppMacosPairingControlError::StoreUnavailable,
                                    );
                                },
                            }
                        },
                    }
                },
            }
        }
        store
            .status()
            .await
            .map(control_status)
            .map_err(|_| AppMacosPairingControlError::StoreUnavailable)
    }

    async fn submit_proposal(
        &self,
        proposal: &super::macos_pairing::AppMacosPairingProposal,
    ) -> Result<(), AppMacosPairingControlError> {
        let url = pairing_url(
            &proposal.gateway_action_url,
            "/host/apps/macos/pairing/propose",
        )?;
        let ack: AppMacosPairingProposalAck = self
            .post_json(url, proposal, &[StatusCode::ACCEPTED])
            .await?;
        if ack.schema != APP_MACOS_HOST_PAIRING_V1
            || ack.status != "pending_native_approval"
            || ack.setup_id != proposal.setup_id
            || ack.generation != proposal.generation
        {
            return Err(AppMacosPairingControlError::InvalidDesktopResponse);
        }
        Ok(())
    }

    async fn submit_identity_attestation(
        &self,
        action_url: &Url,
        challenge: &AppMacosDesktopIdentityChallenge,
    ) -> Result<AppMacosDesktopIdentityAttestation, AppMacosPairingControlError> {
        self.post_json(
            pairing_url(action_url.as_str(), "/host/apps/macos/pairing/attest")?,
            challenge,
            &[StatusCode::OK],
        )
        .await
    }

    async fn submit_status(
        &self,
        action_url: &str,
        request: &magician_app_contract::macos_host::AppMacosHostPairingStatusRequest,
    ) -> Result<AppMacosHostPairingStatusResponse, AppMacosPairingControlError> {
        self.post_json(
            pairing_url(action_url, "/host/apps/macos/pairing/status")?,
            request,
            &[StatusCode::OK],
        )
        .await
    }

    async fn submit_finalization(
        &self,
        action_url: &Url,
        finalization: &super::macos_pairing::AppMacosPairingFinalization,
    ) -> Result<super::macos_pairing::AppMacosPairingFinalized, AppMacosPairingControlError> {
        self.post_json(
            pairing_url(action_url.as_str(), "/host/apps/macos/pairing/finalize")?,
            finalization,
            &[StatusCode::OK],
        )
        .await
    }

    async fn submit_revoke(
        &self,
        action_url: &Url,
        request: &magician_app_contract::macos_host::AppMacosHostPairingStatusRequest,
    ) -> Result<AppMacosHostPairingStatusResponse, AppMacosPairingControlError> {
        self.post_json(
            pairing_url(action_url.as_str(), "/host/apps/macos/pairing/revoke")?,
            request,
            &[StatusCode::OK],
        )
        .await
    }

    async fn post_json<T, R>(
        &self,
        url: Url,
        request: &T,
        accepted: &[StatusCode],
    ) -> Result<R, AppMacosPairingControlError>
    where
        T: Serialize + ?Sized,
        R: DeserializeOwned,
    {
        let response = self
            .client
            .post(url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(request)
            .send()
            .await
            .map_err(|_| AppMacosPairingControlError::DesktopUnavailable)?;
        if !accepted.contains(&response.status())
            || response
                .content_length()
                .is_some_and(|length| length > APP_MACOS_PAIRING_CONTROL_RESPONSE_CEILING as u64)
        {
            return Err(AppMacosPairingControlError::DesktopUnavailable);
        }
        let mut bytes = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| AppMacosPairingControlError::DesktopUnavailable)?;
            if bytes
                .len()
                .checked_add(chunk.len())
                .is_none_or(|length| length > APP_MACOS_PAIRING_CONTROL_RESPONSE_CEILING)
            {
                return Err(AppMacosPairingControlError::InvalidDesktopResponse);
            }
            bytes.extend_from_slice(&chunk);
        }
        if bytes.is_empty() {
            return Err(AppMacosPairingControlError::InvalidDesktopResponse);
        }
        serde_json::from_slice(&bytes)
            .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AppMacosPairingProposalAck {
    schema: String,
    status: String,
    setup_id: String,
    generation: u64,
}

fn pairing_target_ref(
    authenticated: &AuthenticatedAppScope,
    bundle_id: &str,
) -> Result<AppReference, AppMacosPairingControlError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "schema": "magician.app-macos-pairing-target.v1",
        "scope_binding_ref": authenticated.scope_binding_ref(),
        "bundle_id": bundle_id,
    }))
    .map_err(|_| AppMacosPairingControlError::InvalidRequest)?;
    let value = digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppMacosPairingControlError::InvalidRequest)?;
    AppReference::parse(format!("runtime:app-macos-target:v1:{value}"))
        .map_err(|_| AppMacosPairingControlError::InvalidRequest)
}

fn pairing_url(action_url: &str, path: &'static str) -> Result<Url, AppMacosPairingControlError> {
    let endpoint = AppMacosPairingEndpoint::parse(action_url)
        .map_err(|_| AppMacosPairingControlError::InvalidDesktopResponse)?;
    let mut value = endpoint.action_url().clone();
    value.set_path(path);
    Ok(value)
}

fn control_status(status: AppMacosPairingStatus) -> AppMacosPairingControlStatus {
    AppMacosPairingControlStatus {
        phase: match status.phase() {
            AppMacosPairingLifecyclePhase::Unpaired => AppMacosPairingControlPhase::Unpaired,
            AppMacosPairingLifecyclePhase::Pending => {
                AppMacosPairingControlPhase::PendingNativeApproval
            },
            AppMacosPairingLifecyclePhase::ApprovedPendingFinalize => {
                AppMacosPairingControlPhase::ApprovedPendingFinalize
            },
            AppMacosPairingLifecyclePhase::Active => AppMacosPairingControlPhase::Active,
            AppMacosPairingLifecyclePhase::Revoked => AppMacosPairingControlPhase::Revoked,
        },
        active_generation: status.active_generation(),
        transition_generation: status.transition_generation(),
        generation_high_water: status.generation_high_water(),
        finalization_ready: status.finalization_ready(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_request_rejects_ambient_and_raw_owner_fields() {
        let value = serde_json::json!({
            "action_url": "http://127.0.0.1:8787/host/apps/macos/action",
            "bundle_id": "com.example.Editor",
            "cua_driver_binary": "/tmp/cua-driver",
        });
        assert!(serde_json::from_value::<AppMacosPairingSetupRequest>(value).is_err());
        assert!(AppMacosPairingEndpoint::parse(
            "http://127.0.0.1:8787/host/apps/macos/action?binary=/tmp/cua"
        )
        .is_err());
        assert!(
            AppMacosPairingEndpoint::parse("http://localhost:8787/host/apps/macos/action").is_err()
        );
        assert!(AppMacosPairingEndpoint::parse(
            "http://host.docker.internal:8787/host/apps/macos/action"
        )
        .is_err());
    }
}

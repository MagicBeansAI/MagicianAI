//! Native Android Apps authority owner.
//!
//! The runtime may verify handset attestation and propose one exact transition,
//! but it cannot sign or advance Android Apps authority. This owner stages that
//! closed proposal for the bundled Settings window, requires confirmation of
//! the exact displayed digest, signs with the existing Keychain Ed25519
//! identity, and durably advances a monotonic Keychain high-water mark before
//! releasing the receipt. It never accepts arbitrary signing bytes.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use futures_util::StreamExt;
use magician_app_contract::android_owner::{
    app_android_owner_bootstrap_challenge, app_android_owner_native_body_digest,
    AppAndroidAutomationTrustMode, AppAndroidEnrollmentConnectionMode,
    AppAndroidOwnerBootstrapChallengeRequest, AppAndroidOwnerBootstrapCompletion,
    AppAndroidOwnerBootstrapDesktopMessage, AppAndroidOwnerBootstrapNonce,
    AppAndroidOwnerBootstrapRebindOffer, AppAndroidOwnerBootstrapRecoveryHello,
    AppAndroidOwnerBootstrapRuntimeMessage, AppAndroidOwnerBootstrapStatus,
    AppAndroidOwnerNativeBeginEnrollmentRequest, AppAndroidOwnerNativeCancelEnrollmentRequest,
    AppAndroidOwnerNativeCancelEnrollmentResponse, AppAndroidOwnerNativeControlStatus,
    AppAndroidOwnerNativeEmptyRequest, AppAndroidOwnerNativeEnrollmentResponse,
    AppAndroidOwnerNativeEnvelope, AppAndroidOwnerNativeOperation,
    AppAndroidOwnerNativePendingResponse, AppAndroidOwnerNativeProposeReviewRequest,
    AppAndroidOwnerNativeRequest, AppAndroidOwnerNativeTargetsResponse, AppAndroidOwnerOperation,
    AppAndroidOwnerProposal, AppAndroidOwnerReceipt, AppAndroidOwnerRecoveryChallenge,
    AppAndroidOwnerRecoverySnapshot, AppAndroidOwnerStatusChallenge, AppAndroidOwnerStatusReceipt,
    AppAndroidOwnerStatusRecord, APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY,
    APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY, APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1,
    APP_ANDROID_OWNER_MAX_PACKAGES, APP_ANDROID_OWNER_MAX_STATUS_RECORDS,
    APP_ANDROID_OWNER_RUNTIME_CODE_IDENTIFIER,
};
use magician_app_contract::macos_host::{
    app_macos_desktop_owner_approval_code_digest, AppMacosDesktopIdentityChallenge,
};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, WebviewWindow};
use zeroize::Zeroize;

const APP_ANDROID_OWNER_STORE_V1: &str = "magician.desktop.android-owner-store.v1";
const APP_ANDROID_OWNER_ANCHOR_V1: &str = "magician.desktop.android-owner-anchor.v1";
const APP_ANDROID_OWNER_RECOVERY_REPLAY_V1: &str =
    "magician.desktop.android-owner-recovery-replay.v1";
const APP_ANDROID_OWNER_BOOTSTRAP_REPLAY_V1: &str =
    "magician.desktop.android-owner-bootstrap-replay.v1";
#[cfg(not(test))]
const APP_ANDROID_OWNER_KEYCHAIN_SERVICE: &str = "com.magicbeans.magician.app-android-owner-anchor";
const APP_ANDROID_OWNER_MAX_STORE_BYTES: u64 = 2 * 1024 * 1024;
#[cfg(not(test))]
const APP_ANDROID_OWNER_MAX_ANCHOR_BYTES: usize = 4 * 1024;
const APP_ANDROID_OWNER_BLOCKING_SLOTS: usize = 2;
const APP_ANDROID_OWNER_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);
// Strict live + static validation hashes the staged Magician executable. The
// bundled debug binary is close to 1 GiB once DuckDB/Parquet is linked, so the
// ordinary Keychain/disk-operation deadline is too short for this one physical
// proof. Keep it below the 30-second bootstrap nonce lifetime while allowing a
// cold code-sign validation to complete.
const APP_ANDROID_OWNER_PEER_CODE_VERIFY_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(25);
const APP_ANDROID_OWNER_NATIVE_REQUEST_LIFETIME_MS: i64 = 20_000;
const APP_ANDROID_OWNER_NATIVE_CONNECT_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(3);
const APP_ANDROID_OWNER_NATIVE_REQUEST_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(20);
// The durable record projection itself is capped at 2 MiB. The fixed extra
// quarter-MiB admits only its typed envelope, challenge and signature fields.
const APP_ANDROID_OWNER_NATIVE_MAX_REQUEST_BYTES: usize = 2_304 * 1024;
const APP_ANDROID_OWNER_NATIVE_MAX_RESPONSE_BYTES: usize = 2_304 * 1024;
const APP_ANDROID_OWNER_NATIVE_PREFIX: &str = "/api/magician/v2/android-apps-owner";
const APP_ANDROID_OWNER_BOOTSTRAP_FRAME_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(20);
#[cfg(test)]
const APP_ANDROID_OWNER_MAGDROID_PACKAGE: &str =
    magician_app_contract::android_owner::APP_ANDROID_OWNER_LEGACY_COMPANION_PACKAGE;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppAndroidOwnerNativeStatus {
    pub desktop_identity_key_id: String,
    #[serde(skip_serializing)]
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_digest: String,
    pub owner_generation: u64,
    pub latest_receipt_digest: Option<String>,
    pub identity_bootstrap_available: bool,
    pub pending: Option<AppAndroidOwnerProposal>,
    pub pending_recovery: Option<AppAndroidOwnerRecoveryChallenge>,
    pub pending_identity_bootstrap: Option<AppAndroidOwnerPendingBootstrapStatus>,
    pub pending_rebind_offer: Option<AppAndroidOwnerBootstrapRebindOffer>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppAndroidOwnerPendingBootstrapStatus {
    pub bootstrap: AppAndroidOwnerBootstrapNonce,
    pub desktop_identity_fingerprint: String,
    pub completion_ready: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppAndroidOwnerBootstrapEnrollment {
    pub bootstrap: AppAndroidOwnerBootstrapNonce,
    pub desktop_identity_key_id: String,
    pub desktop_identity_fingerprint: String,
    pub owner_approval_code: String,
    pub expires_at_ms: i64,
    pub rebind_display_digest: Option<String>,
}

impl std::fmt::Debug for AppAndroidOwnerBootstrapEnrollment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppAndroidOwnerBootstrapEnrollment")
            .field("bootstrap", &self.bootstrap)
            .field("desktop_identity_key_id", &self.desktop_identity_key_id)
            .field(
                "desktop_identity_fingerprint",
                &self.desktop_identity_fingerprint,
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .field("rebind_display_digest", &self.rebind_display_digest)
            .finish_non_exhaustive()
    }
}

impl Drop for AppAndroidOwnerBootstrapEnrollment {
    fn drop(&mut self) {
        self.owner_approval_code.zeroize();
    }
}

#[derive(Debug, Clone)]
struct PendingAndroidOwnerBootstrap {
    request: AppAndroidOwnerBootstrapChallengeRequest,
    desktop_identity_fingerprint: String,
    approval_confirmed: bool,
    completion: Option<AppAndroidOwnerBootstrapCompletion>,
    finalized_status: Option<AppAndroidOwnerBootstrapStatus>,
    rebind_offer: Option<AppAndroidOwnerBootstrapRebindOffer>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAndroidOwnerStore {
    schema: String,
    desktop_identity_digest: String,
    owner_generation: u64,
    latest_receipt_digest: Option<String>,
    latest_receipt: Option<AppAndroidOwnerReceipt>,
    records: Vec<AppAndroidOwnerReceipt>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAndroidOwnerRecoveryReplay {
    schema: String,
    challenge: AppAndroidOwnerRecoveryChallenge,
    snapshot: AppAndroidOwnerRecoverySnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAndroidOwnerBootstrapReplay {
    schema: String,
    request: AppAndroidOwnerBootstrapChallengeRequest,
    desktop_identity_fingerprint: String,
    completion: AppAndroidOwnerBootstrapCompletion,
    finalized_status: Option<AppAndroidOwnerBootstrapStatus>,
    rebind_offer: Option<AppAndroidOwnerBootstrapRebindOffer>,
}

impl PersistedAndroidOwnerStore {
    fn empty(desktop_identity_digest: String) -> Self {
        Self {
            schema: APP_ANDROID_OWNER_STORE_V1.to_owned(),
            desktop_identity_digest,
            owner_generation: 0,
            latest_receipt_digest: None,
            latest_receipt: None,
            records: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidOwnerAnchorPoint {
    generation: u64,
    document_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AndroidOwnerAnchor {
    schema: String,
    desktop_identity_digest: String,
    committed: Option<AndroidOwnerAnchorPoint>,
    pending: Option<AndroidOwnerAnchorPoint>,
}

#[derive(Default)]
pub(crate) struct AppAndroidAuthorityOwner {
    store_path: Option<PathBuf>,
    staged_path: Option<PathBuf>,
    recovery_path: Option<PathBuf>,
    recovery_staged_path: Option<PathBuf>,
    bootstrap_path: Option<PathBuf>,
    bootstrap_staged_path: Option<PathBuf>,
    anchor_account: Option<String>,
    document: Option<PersistedAndroidOwnerStore>,
    recovery_replay: Option<PersistedAndroidOwnerRecoveryReplay>,
    offered_bootstrap: Option<AppAndroidOwnerBootstrapNonce>,
    rebind_offer: Option<AppAndroidOwnerBootstrapRebindOffer>,
    pending: Option<AppAndroidOwnerProposal>,
    pending_recovery: Option<AppAndroidOwnerRecoveryChallenge>,
    pending_bootstrap: Option<PendingAndroidOwnerBootstrap>,
    pending_rebind_bootstrap: Option<PendingAndroidOwnerBootstrap>,
    ready: bool,
    #[cfg(test)]
    test_anchor: Option<AndroidOwnerAnchor>,
}

impl AppAndroidAuthorityOwner {
    pub(crate) fn initialize(&mut self, app_data_root: &Path, now_ms: i64) -> Result<(), String> {
        let owner_root = ensure_private_owner_directory(app_data_root)?;
        let store_path = owner_root.join("authority.json");
        let staged_path = owner_root.join("authority.staged.json");
        let recovery_path = owner_root.join("recovery.json");
        let recovery_staged_path = owner_root.join("recovery.staged.json");
        let bootstrap_path = owner_root.join("bootstrap.json");
        let bootstrap_staged_path = owner_root.join("bootstrap.staged.json");
        let anchor_account = android_owner_anchor_account(&owner_root);
        if self.ready {
            if self.store_path.as_deref() != Some(store_path.as_path())
                || self.recovery_path.as_deref() != Some(recovery_path.as_path())
                || self.bootstrap_path.as_deref() != Some(bootstrap_path.as_path())
                || self.anchor_account.as_deref() != Some(anchor_account.as_str())
            {
                return Err("Android owner storage identity changed".to_owned());
            }
            return Ok(());
        }

        let (key_id, public_key_hex, desktop_identity_digest) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        let anchor = self.load_anchor(&anchor_account)?;
        let main = read_private_document(&store_path)?;
        let staged = read_private_document(&staged_path)?;
        let (document, resolved_anchor) = recover_document(
            anchor,
            main,
            staged,
            &store_path,
            &staged_path,
            &desktop_identity_digest,
            &key_id,
            &public_key_hex,
            now_ms,
        )?;

        if let Some(anchor) = resolved_anchor.as_ref() {
            self.store_anchor(&anchor_account, anchor)?;
        }
        if resolved_anchor
            .as_ref()
            .map_or(true, |anchor| anchor.pending.is_none())
            && staged_path.exists()
        {
            remove_regular_file_if_present(&staged_path)?;
        }

        let document = document
            .unwrap_or_else(|| PersistedAndroidOwnerStore::empty(desktop_identity_digest.clone()));
        let recovery_replay = match read_private_document(&recovery_path)? {
            Some(bytes) => Some(decode_and_validate_recovery_replay(
                &bytes,
                &document,
                &key_id,
                &public_key_hex,
                now_ms,
            )?),
            None => None,
        };
        if recovery_replay.is_some() {
            // Repeat the directory durability fence after any prior crash
            // between rename and directory sync.
            sync_parent_directory(&recovery_path)?;
        }
        if recovery_staged_path.exists() {
            remove_regular_file_if_present(&recovery_staged_path)?;
        }
        let bootstrap_replay = match read_private_document(&bootstrap_path)? {
            Some(bytes) => Some(decode_and_validate_bootstrap_replay(
                &bytes,
                &desktop_identity_digest,
                &document,
                now_ms,
            )?),
            None => None,
        };
        if bootstrap_replay.is_some() {
            sync_parent_directory(&bootstrap_path)?;
        }
        if bootstrap_staged_path.exists() {
            remove_regular_file_if_present(&bootstrap_staged_path)?;
        }

        self.store_path = Some(store_path);
        self.staged_path = Some(staged_path);
        self.recovery_path = Some(recovery_path);
        self.recovery_staged_path = Some(recovery_staged_path);
        self.bootstrap_path = Some(bootstrap_path);
        self.bootstrap_staged_path = Some(bootstrap_staged_path);
        self.anchor_account = Some(anchor_account);
        self.document = Some(document);
        self.offered_bootstrap = None;
        self.rebind_offer = None;
        self.pending = None;
        self.pending_recovery = recovery_replay
            .as_ref()
            .map(|replay| replay.challenge.clone());
        self.recovery_replay = recovery_replay;
        self.pending_bootstrap = bootstrap_replay.map(|replay| PendingAndroidOwnerBootstrap {
            request: replay.request,
            desktop_identity_fingerprint: replay.desktop_identity_fingerprint,
            approval_confirmed: true,
            completion: Some(replay.completion),
            finalized_status: replay.finalized_status,
            rebind_offer: replay.rebind_offer,
        });
        self.pending_rebind_bootstrap = None;
        self.ready = true;
        Ok(())
    }

    fn accept_runtime_hello(
        &mut self,
        bootstrap: AppAndroidOwnerBootstrapNonce,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapDesktopMessage, String> {
        if let Some(pending) = self.pending_bootstrap.clone() {
            if pending.request.bootstrap == bootstrap {
                if let Some(completion) = pending.completion.clone() {
                    bootstrap.validate(bootstrap.issued_at_ms).map_err(|_| {
                        "retained Android owner runtime nonce is invalid".to_owned()
                    })?;
                    return Ok(AppAndroidOwnerBootstrapDesktopMessage::completion(
                        completion,
                    ));
                }
                bootstrap
                    .validate(bootstrap.issued_at_ms)
                    .map_err(|_| "retained Android owner runtime nonce is invalid".to_owned())?;
                let bootstrap_digest = bootstrap
                    .digest()
                    .map_err(|_| "Android owner runtime bootstrap digest is invalid".to_owned())?;
                if bootstrap.expires_at_ms <= now_ms {
                    self.pending_bootstrap = None;
                    if self.offered_bootstrap.as_ref() == Some(&bootstrap) {
                        self.offered_bootstrap = None;
                    }
                    return Ok(AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(
                        bootstrap_digest,
                    ));
                }
                if pending.approval_confirmed {
                    return Ok(AppAndroidOwnerBootstrapDesktopMessage::challenge_request(
                        pending.request.clone(),
                    ));
                }
                return Ok(
                    AppAndroidOwnerBootstrapDesktopMessage::awaiting_native_approval(
                        bootstrap_digest,
                    ),
                );
            }
            if let Some(finalized) = pending.finalized_status.as_ref() {
                let previous_bootstrap_status_digest = finalized
                    .digest()
                    .map_err(|_| "finalized Android owner identity is invalid".to_owned())?;
                return self.offer_runtime_rebind(
                    bootstrap,
                    finalized.desktop_identity_key_id.clone(),
                    finalized.desktop_identity_digest.clone(),
                    previous_bootstrap_status_digest,
                    now_ms,
                );
            }
            if pending.completion.is_some() || pending.request.bootstrap.expires_at_ms > now_ms {
                return Err("another Android owner bootstrap owns the native ceremony".to_owned());
            }
            self.pending_bootstrap = None;
        }
        let (owner_generation, latest_receipt_digest, document_identity) = {
            let document = self.ready_document()?;
            (
                document.owner_generation,
                document.latest_receipt_digest.clone(),
                document.desktop_identity_digest.clone(),
            )
        };
        if owner_generation > 0 {
            let (key_id, _, current_identity) =
                crate::app_macos_identity::app_android_owner_desktop_identity()?;
            if current_identity != document_identity {
                return Err("Android owner Keychain identity changed".to_owned());
            }
            let previous_bootstrap_status_digest = missing_bootstrap_status_digest(
                &document_identity,
                owner_generation,
                latest_receipt_digest.as_deref(),
            );
            return self.offer_runtime_rebind(
                bootstrap,
                key_id,
                document_identity,
                previous_bootstrap_status_digest,
                now_ms,
            );
        }
        bootstrap
            .validate(now_ms)
            .map_err(|_| "Android owner runtime bootstrap nonce is invalid".to_owned())?;
        let bootstrap_digest = bootstrap
            .digest()
            .map_err(|_| "Android owner runtime bootstrap digest is invalid".to_owned())?;
        if let Some(offered) = self.offered_bootstrap.as_ref() {
            if offered == &bootstrap {
                return Ok(
                    AppAndroidOwnerBootstrapDesktopMessage::awaiting_native_approval(
                        bootstrap_digest,
                    ),
                );
            }
            if offered.expires_at_ms > now_ms {
                return Err("another Android owner runtime nonce is already offered".to_owned());
            }
        }
        self.offered_bootstrap = Some(bootstrap);
        Ok(AppAndroidOwnerBootstrapDesktopMessage::awaiting_native_approval(bootstrap_digest))
    }

    fn offer_runtime_rebind(
        &mut self,
        bootstrap: AppAndroidOwnerBootstrapNonce,
        desktop_identity_key_id: String,
        desktop_identity_digest: String,
        previous_bootstrap_status_digest: String,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapDesktopMessage, String> {
        if self
            .pending
            .as_ref()
            .is_some_and(|proposal| proposal.expires_at_ms > now_ms)
            || self
                .pending_recovery
                .as_ref()
                .is_some_and(|challenge| challenge.expires_at_ms > now_ms)
            || self.recovery_replay.is_some()
        {
            return Err(
                "Android owner decision or recovery must finish before runtime rebind".to_owned(),
            );
        }
        self.pending = None;
        self.pending_recovery = None;
        bootstrap
            .validate(now_ms)
            .map_err(|_| "Android owner runtime recovery nonce is invalid".to_owned())?;
        if let Some(offer) = self.rebind_offer.clone() {
            if offer.bootstrap == bootstrap {
                if offer.expires_at_ms <= now_ms {
                    let bootstrap_digest = bootstrap
                        .digest()
                        .map_err(|_| "Android owner expired rebind digest is invalid".to_owned())?;
                    self.rebind_offer = None;
                    self.pending_rebind_bootstrap = None;
                    return Ok(AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(
                        bootstrap_digest,
                    ));
                }
                offer
                    .validate(now_ms)
                    .map_err(|_| "Android owner rebind offer expired".to_owned())?;
                return Ok(
                    AppAndroidOwnerBootstrapDesktopMessage::rebind_awaiting_native_approval(offer),
                );
            }
            if offer.expires_at_ms > now_ms {
                return Err("another Android owner rebind is awaiting review".to_owned());
            }
            self.rebind_offer = None;
            self.pending_rebind_bootstrap = None;
        }
        let (owner_generation, latest_receipt_digest) = {
            let document = self.ready_document()?;
            (
                document.owner_generation,
                document.latest_receipt_digest.clone(),
            )
        };
        let offer = AppAndroidOwnerBootstrapRebindOffer::mint(
            bootstrap.clone(),
            desktop_identity_key_id,
            desktop_identity_digest,
            previous_bootstrap_status_digest,
            owner_generation,
            latest_receipt_digest,
            bootstrap.issued_at_ms,
            bootstrap.expires_at_ms,
        )
        .map_err(|_| "Android owner rebind offer could not be constructed".to_owned())?;
        self.offered_bootstrap = None;
        self.rebind_offer = Some(offer.clone());
        Ok(AppAndroidOwnerBootstrapDesktopMessage::rebind_awaiting_native_approval(offer))
    }

    fn rebind_identity_context(&self) -> Result<Option<(String, String, String)>, String> {
        if let Some(finalized) = self
            .pending_bootstrap
            .as_ref()
            .and_then(|pending| pending.finalized_status.as_ref())
        {
            return Ok(Some((
                finalized.desktop_identity_key_id.clone(),
                finalized.desktop_identity_digest.clone(),
                finalized
                    .digest()
                    .map_err(|_| "finalized Android owner identity is invalid".to_owned())?,
            )));
        }
        let document = self.ready_document()?;
        if document.owner_generation == 0 {
            return Ok(None);
        }
        let (key_id, _, current_identity) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        if current_identity != document.desktop_identity_digest {
            return Err("Android owner Keychain identity changed".to_owned());
        }
        Ok(Some((
            key_id,
            current_identity.clone(),
            missing_bootstrap_status_digest(
                &current_identity,
                document.owner_generation,
                document.latest_receipt_digest.as_deref(),
            ),
        )))
    }

    fn accept_runtime_recovery_hello(
        &mut self,
        recovery: AppAndroidOwnerBootstrapRecoveryHello,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapDesktopMessage, String> {
        recovery
            .validate()
            .map_err(|_| "Android owner recovery hello is invalid".to_owned())?;
        let mut pending = self
            .pending_rebind_bootstrap
            .as_ref()
            .or_else(|| {
                self.pending_bootstrap
                    .as_ref()
                    .filter(|pending| pending.rebind_offer.is_some())
            })
            .cloned();
        if pending.is_none()
            && self.rebind_offer.is_none()
            && recovery.bootstrap.expires_at_ms <= now_ms
        {
            return Ok(AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(
                recovery
                    .bootstrap
                    .digest()
                    .map_err(|_| "Android owner rebind digest is invalid".to_owned())?,
            ));
        }
        if pending.is_none() && self.rebind_offer.is_none() {
            let (key_id, identity_digest, previous_status_digest) = self
                .rebind_identity_context()?
                .ok_or_else(|| "Android owner has no established authority to rebind".to_owned())?;
            self.offer_runtime_rebind(
                recovery.bootstrap.clone(),
                key_id,
                identity_digest,
                previous_status_digest,
                now_ms,
            )?;
            pending = self
                .pending_rebind_bootstrap
                .as_ref()
                .or_else(|| {
                    self.pending_bootstrap
                        .as_ref()
                        .filter(|pending| pending.rebind_offer.is_some())
                })
                .cloned();
        }
        let offer = pending
            .as_ref()
            .and_then(|pending| pending.rebind_offer.clone())
            .or_else(|| self.rebind_offer.clone())
            .ok_or_else(|| "Android owner has no authenticated rebind offer".to_owned())?;
        let offer_digest = offer
            .digest()
            .map_err(|_| "Android owner rebind offer digest is invalid".to_owned())?;
        if recovery.bootstrap != offer.bootstrap
            || recovery.rebind_offer_digest != offer_digest
            || recovery.expected_desktop_identity_digest != offer.desktop_identity_digest
        {
            return Err("Android owner recovery hello changed the displayed rebind".to_owned());
        }
        let document = self.ready_document()?;
        if offer.owner_generation != document.owner_generation
            || offer.latest_receipt_digest != document.latest_receipt_digest
            || offer.desktop_identity_digest != document.desktop_identity_digest
        {
            self.pending_rebind_bootstrap = None;
            self.rebind_offer = None;
            return Err("Android owner durable head changed during runtime rebind".to_owned());
        }
        if let Some(pending) = pending {
            if let Some(completion) = pending.completion {
                recovery
                    .bootstrap
                    .validate(recovery.bootstrap.issued_at_ms)
                    .map_err(|_| "retained Android owner rebind nonce is invalid".to_owned())?;
                return Ok(AppAndroidOwnerBootstrapDesktopMessage::completion(
                    completion,
                ));
            }
            if offer.expires_at_ms <= now_ms {
                self.pending_rebind_bootstrap = None;
                self.rebind_offer = None;
                return Ok(AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(
                    recovery
                        .bootstrap
                        .digest()
                        .map_err(|_| "Android owner rebind digest is invalid".to_owned())?,
                ));
            }
            if pending.approval_confirmed {
                return Ok(AppAndroidOwnerBootstrapDesktopMessage::challenge_request(
                    pending.request,
                ));
            }
        }
        if offer.expires_at_ms <= now_ms {
            self.pending_rebind_bootstrap = None;
            self.rebind_offer = None;
            return Ok(AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(
                recovery
                    .bootstrap
                    .digest()
                    .map_err(|_| "Android owner rebind digest is invalid".to_owned())?,
            ));
        }
        offer
            .validate(now_ms)
            .map_err(|_| "Android owner rebind offer expired".to_owned())?;
        Ok(AppAndroidOwnerBootstrapDesktopMessage::rebind_awaiting_native_approval(offer))
    }

    fn accept_runtime_challenge(
        &self,
        challenge: &AppMacosDesktopIdentityChallenge,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapChallengeRequest, String> {
        challenge
            .validate(now_ms)
            .map_err(|_| "Android owner runtime challenge is invalid or expired".to_owned())?;
        let pending = self
            .pending_rebind_bootstrap
            .as_ref()
            .or(self.pending_bootstrap.as_ref())
            .filter(|pending| pending.approval_confirmed && pending.completion.is_none())
            .ok_or_else(|| "Android owner bootstrap is not owner-confirmed".to_owned())?;
        let expected = app_android_owner_bootstrap_challenge(&pending.request)
            .map_err(|_| "Android owner canonical bootstrap challenge is invalid".to_owned())?;
        if &expected != challenge {
            return Err("Android owner runtime challenge changed canonical bytes".to_owned());
        }
        Ok(pending.request.clone())
    }

    fn accept_runtime_finalized(
        &mut self,
        status: &AppAndroidOwnerBootstrapStatus,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapDesktopMessage, String> {
        let pending = self
            .pending_bootstrap
            .as_ref()
            .filter(|pending| pending.completion.is_some())
            .ok_or_else(|| "Android owner has no retained completion to finalize".to_owned())?;
        let request = pending.request.clone();
        let completion = pending
            .completion
            .clone()
            .expect("filtered retained completion");
        self.complete_bootstrap(&request, &completion, status, now_ms)
    }

    pub(crate) fn stage_bootstrap(
        &mut self,
        request: AppAndroidOwnerBootstrapChallengeRequest,
        desktop_identity_fingerprint: String,
        expected_rebind_display_digest: Option<String>,
        now_ms: i64,
    ) -> Result<(), String> {
        request
            .validate(now_ms)
            .map_err(|_| "Android owner bootstrap request is invalid".to_owned())?;
        if request.desktop_identity_digest != desktop_identity_fingerprint {
            return Err("Android owner bootstrap fingerprint changed".to_owned());
        }
        if let Some(offer) = self.rebind_offer.clone() {
            offer
                .validate(now_ms)
                .map_err(|_| "Android owner rebind offer expired".to_owned())?;
            let finalized = self
                .pending_bootstrap
                .as_ref()
                .and_then(|pending| pending.finalized_status.as_ref());
            let document = self.ready_document()?;
            let previous_bootstrap_status_digest = match finalized {
                Some(finalized) => finalized
                    .digest()
                    .map_err(|_| "Android owner finalized identity is invalid".to_owned())?,
                None if document.owner_generation > 0 => missing_bootstrap_status_digest(
                    &document.desktop_identity_digest,
                    document.owner_generation,
                    document.latest_receipt_digest.as_deref(),
                ),
                None => return Err("Android owner rebind has no established authority".to_owned()),
            };
            if offer.bootstrap != request.bootstrap
                || offer.desktop_identity_key_id != request.desktop_identity_key_id
                || offer.desktop_identity_digest != request.desktop_identity_digest
                || offer.previous_bootstrap_status_digest != previous_bootstrap_status_digest
                || offer.owner_generation != document.owner_generation
                || offer.latest_receipt_digest != document.latest_receipt_digest
                || expected_rebind_display_digest.as_deref() != Some(offer.display_digest.as_str())
            {
                return Err(
                    "Android owner rebind does not match the displayed durable head".to_owned(),
                );
            }
            if let Some(pending) = self.pending_rebind_bootstrap.as_ref() {
                if pending.request == request
                    && pending.desktop_identity_fingerprint == desktop_identity_fingerprint
                    && pending.rebind_offer.as_ref() == Some(&offer)
                {
                    return Ok(());
                }
                if !pending.approval_confirmed
                    && pending.completion.is_none()
                    && pending.request.bootstrap == request.bootstrap
                    && pending.request.desktop_identity_key_id == request.desktop_identity_key_id
                    && pending.request.desktop_identity_public_key_hex
                        == request.desktop_identity_public_key_hex
                    && pending.desktop_identity_fingerprint == desktop_identity_fingerprint
                    && pending.rebind_offer.as_ref() == Some(&offer)
                {
                    self.pending_rebind_bootstrap = Some(PendingAndroidOwnerBootstrap {
                        request,
                        desktop_identity_fingerprint,
                        approval_confirmed: false,
                        completion: None,
                        finalized_status: None,
                        rebind_offer: Some(offer),
                    });
                    return Ok(());
                }
                return Err("another Android owner rebind is pending".to_owned());
            }
            self.pending_rebind_bootstrap = Some(PendingAndroidOwnerBootstrap {
                request,
                desktop_identity_fingerprint,
                approval_confirmed: false,
                completion: None,
                finalized_status: None,
                rebind_offer: Some(offer),
            });
            return Ok(());
        }
        if expected_rebind_display_digest.is_some() {
            return Err("Android owner rebind offer is no longer pending".to_owned());
        }
        if let Some(pending) = self.pending_bootstrap.as_ref() {
            if pending.request == request
                && pending.desktop_identity_fingerprint == desktop_identity_fingerprint
            {
                return Ok(());
            }
            if !pending.approval_confirmed
                && pending.completion.is_none()
                && pending.rebind_offer.is_none()
                && pending.request.bootstrap == request.bootstrap
                && pending.request.desktop_identity_key_id == request.desktop_identity_key_id
                && pending.request.desktop_identity_public_key_hex
                    == request.desktop_identity_public_key_hex
                && pending.desktop_identity_fingerprint == desktop_identity_fingerprint
            {
                self.pending_bootstrap = Some(PendingAndroidOwnerBootstrap {
                    request,
                    desktop_identity_fingerprint,
                    approval_confirmed: false,
                    completion: None,
                    finalized_status: None,
                    rebind_offer: None,
                });
                return Ok(());
            }
            if pending.request.bootstrap.expires_at_ms > now_ms {
                return Err("another Android owner identity bootstrap is pending".to_owned());
            }
            if pending.completion.is_some() {
                return Err(
                    "an approved Android owner identity bootstrap awaits delivery".to_owned(),
                );
            }
        }
        if self.offered_bootstrap.as_ref() != Some(&request.bootstrap) {
            return Err("Android owner bootstrap was not offered by the runtime peer".to_owned());
        }
        self.pending_bootstrap = Some(PendingAndroidOwnerBootstrap {
            request,
            desktop_identity_fingerprint,
            approval_confirmed: false,
            completion: None,
            finalized_status: None,
            rebind_offer: None,
        });
        self.offered_bootstrap = None;
        Ok(())
    }

    fn offered_bootstrap(
        &self,
        now_ms: i64,
    ) -> Result<(AppAndroidOwnerBootstrapNonce, Option<String>), String> {
        if let Some(offer) = self.rebind_offer.as_ref() {
            offer
                .validate(now_ms)
                .map_err(|_| "the verified runtime rebind offer expired".to_owned())?;
            return Ok((offer.bootstrap.clone(), Some(offer.display_digest.clone())));
        }
        if let Some(pending) = self
            .pending_rebind_bootstrap
            .as_ref()
            .or(self.pending_bootstrap.as_ref())
            .filter(|pending| !pending.approval_confirmed && pending.completion.is_none())
        {
            pending
                .request
                .bootstrap
                .validate(now_ms)
                .map_err(|_| "the verified runtime bootstrap nonce expired".to_owned())?;
            return Ok((
                pending.request.bootstrap.clone(),
                pending
                    .rebind_offer
                    .as_ref()
                    .map(|offer| offer.display_digest.clone()),
            ));
        }
        let bootstrap = self
            .offered_bootstrap
            .as_ref()
            .ok_or_else(|| "the verified runtime has not offered a bootstrap nonce".to_owned())?;
        bootstrap
            .validate(now_ms)
            .map_err(|_| "the verified runtime bootstrap nonce expired".to_owned())?;
        Ok((bootstrap.clone(), None))
    }

    fn discard_expired_unconfirmed_bootstrap(&mut self, now_ms: i64) {
        let stale_pending = |pending: &PendingAndroidOwnerBootstrap| {
            !pending.approval_confirmed
                && pending.completion.is_none()
                && pending.request.bootstrap.expires_at_ms <= now_ms
        };
        if self.pending_bootstrap.as_ref().is_some_and(stale_pending) {
            self.pending_bootstrap = None;
        }
        if self
            .pending_rebind_bootstrap
            .as_ref()
            .is_some_and(stale_pending)
        {
            self.pending_rebind_bootstrap = None;
        }
        if self
            .offered_bootstrap
            .as_ref()
            .is_some_and(|bootstrap| bootstrap.expires_at_ms <= now_ms)
        {
            self.offered_bootstrap = None;
        }
        if self
            .rebind_offer
            .as_ref()
            .is_some_and(|offer| offer.expires_at_ms <= now_ms)
        {
            self.rebind_offer = None;
        }
    }

    pub(crate) fn arm_bootstrap(
        &mut self,
        expected_bootstrap_nonce: &str,
        expected_desktop_identity_fingerprint: &str,
        expected_rebind_display_digest: Option<&str>,
        now_ms: i64,
    ) -> Result<(), String> {
        let use_rebind = self
            .pending_rebind_bootstrap
            .as_ref()
            .is_some_and(|pending| {
                pending.request.bootstrap.bootstrap_nonce == expected_bootstrap_nonce
            });
        let pending = if use_rebind {
            self.pending_rebind_bootstrap.as_mut()
        } else {
            self.pending_bootstrap.as_mut().filter(|pending| {
                pending.request.bootstrap.bootstrap_nonce == expected_bootstrap_nonce
            })
        }
        .ok_or_else(|| "Android owner identity bootstrap is not pending".to_owned())?;
        let validation_time = pending
            .completion
            .as_ref()
            .map_or(now_ms, |completion| completion.challenge.issued_at_ms);
        pending
            .request
            .validate(validation_time)
            .map_err(|_| "Android owner identity bootstrap expired".to_owned())?;
        if pending.request.bootstrap.bootstrap_nonce != expected_bootstrap_nonce
            || pending.desktop_identity_fingerprint != expected_desktop_identity_fingerprint
            || pending
                .rebind_offer
                .as_ref()
                .map(|offer| offer.display_digest.as_str())
                != expected_rebind_display_digest
        {
            return Err("Android owner bootstrap does not match the displayed identity".to_owned());
        }
        pending.approval_confirmed = true;
        Ok(())
    }

    pub(crate) fn retain_bootstrap_completion(
        &mut self,
        request: &AppAndroidOwnerBootstrapChallengeRequest,
        completion: AppAndroidOwnerBootstrapCompletion,
        now_ms: i64,
    ) -> Result<(), String> {
        let is_rebind = self
            .pending_rebind_bootstrap
            .as_ref()
            .is_some_and(|pending| pending.request == *request);
        let pending = if is_rebind {
            self.pending_rebind_bootstrap.as_ref()
        } else {
            self.pending_bootstrap.as_ref()
        }
        .filter(|pending| pending.request == *request)
        .ok_or_else(|| "Android owner bootstrap changed before attestation".to_owned())?;
        verify_bootstrap_completion(&completion, now_ms)?;
        if completion.challenge.desktop_identity_key_id != request.desktop_identity_key_id
            || completion.challenge.desktop_identity_public_key_hex
                != request.desktop_identity_public_key_hex
            || completion.challenge.owner_approval_code_digest != request.owner_approval_code_digest
        {
            return Err("Android owner bootstrap challenge changed identity".to_owned());
        }
        let replay = PersistedAndroidOwnerBootstrapReplay {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_REPLAY_V1.to_owned(),
            request: pending.request.clone(),
            desktop_identity_fingerprint: pending.desktop_identity_fingerprint.clone(),
            completion: completion.clone(),
            finalized_status: None,
            rebind_offer: pending.rebind_offer.clone(),
        };
        self.persist_bootstrap_replay(&replay)?;
        if is_rebind {
            let mut rebound = self
                .pending_rebind_bootstrap
                .take()
                .filter(|pending| pending.request == *request)
                .ok_or_else(|| "Android owner rebind changed after persistence".to_owned())?;
            rebound.completion = Some(completion);
            self.pending_bootstrap = Some(rebound);
            self.rebind_offer = None;
        } else {
            self.pending_bootstrap
                .as_mut()
                .filter(|pending| pending.request == *request)
                .ok_or_else(|| "Android owner bootstrap changed after persistence".to_owned())?
                .completion = Some(completion);
        }
        Ok(())
    }

    pub(crate) fn complete_bootstrap(
        &mut self,
        request: &AppAndroidOwnerBootstrapChallengeRequest,
        completion: &AppAndroidOwnerBootstrapCompletion,
        status: &AppAndroidOwnerBootstrapStatus,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerBootstrapDesktopMessage, String> {
        status
            .validate()
            .map_err(|_| "Android owner bootstrap status is invalid".to_owned())?;
        verify_bootstrap_completion(completion, now_ms)?;
        let attestation_digest = completion
            .attestation
            .digest()
            .map_err(|_| "Android owner bootstrap attestation digest is invalid".to_owned())?;
        if self
            .pending_bootstrap
            .as_ref()
            .map_or(true, |pending| pending.request != *request)
            || status.desktop_identity_key_id != request.desktop_identity_key_id
            || status.desktop_identity_public_key_hex != request.desktop_identity_public_key_hex
            || status.desktop_identity_digest != request.desktop_identity_digest
            || status.desktop_identity_attestation_digest != attestation_digest
            || status.host_identity_digest != completion.attestation.host_identity_digest
            || status.pinned_at_ms < completion.attestation.attested_at_ms
            || status.pinned_at_ms > now_ms.saturating_add(5_000)
        {
            return Err("Android owner bootstrap status changed identity".to_owned());
        }
        let status_digest = status
            .digest()
            .map_err(|_| "Android owner bootstrap status digest is invalid".to_owned())?;
        if let Some(finalized) = self
            .pending_bootstrap
            .as_ref()
            .and_then(|pending| pending.finalized_status.as_ref())
        {
            if finalized != status {
                return Err("Android owner bootstrap finalized status changed".to_owned());
            }
            return Ok(AppAndroidOwnerBootstrapDesktopMessage::finalized_ack(
                status_digest,
            ));
        }
        let pending = self
            .pending_bootstrap
            .as_ref()
            .ok_or_else(|| "Android owner bootstrap disappeared before finalization".to_owned())?;
        let replay = PersistedAndroidOwnerBootstrapReplay {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_REPLAY_V1.to_owned(),
            request: pending.request.clone(),
            desktop_identity_fingerprint: pending.desktop_identity_fingerprint.clone(),
            completion: pending
                .completion
                .as_ref()
                .ok_or_else(|| "Android owner bootstrap completion is absent".to_owned())?
                .clone(),
            finalized_status: Some(status.clone()),
            rebind_offer: pending.rebind_offer.clone(),
        };
        self.persist_bootstrap_replay(&replay)?;
        self.pending_bootstrap
            .as_mut()
            .ok_or_else(|| "Android owner bootstrap disappeared after persistence".to_owned())?
            .finalized_status = Some(status.clone());
        Ok(AppAndroidOwnerBootstrapDesktopMessage::finalized_ack(
            status_digest,
        ))
    }

    pub(crate) fn stage(
        &mut self,
        proposal: AppAndroidOwnerProposal,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerProposal, String> {
        if self.runtime_rebind_incomplete() {
            return Err(
                "Android owner runtime rebind must finish before another decision".to_owned(),
            );
        }
        if self.recovery_replay.is_some() {
            return Err("Android owner recovery must finish before another decision".to_owned());
        }
        if self
            .pending_recovery
            .as_ref()
            .is_some_and(|challenge| challenge.expires_at_ms > now_ms)
        {
            return Err("Android owner recovery must finish before another decision".to_owned());
        }
        self.pending_recovery = None;
        let document = self.ready_document()?;
        proposal
            .validate(now_ms)
            .map_err(|_| "Android owner proposal is invalid or expired".to_owned())?;
        if proposal.desktop_identity_digest != document.desktop_identity_digest
            || proposal.owner_generation != document.owner_generation.saturating_add(1)
            || proposal.previous_receipt_digest != document.latest_receipt_digest
        {
            return Err("Android owner proposal is stale or belongs to another desktop".to_owned());
        }
        validate_transition(document, &proposal)?;
        if let Some(pending) = self.pending.as_ref() {
            if pending.digest().ok() == proposal.digest().ok() {
                return Ok(pending.clone());
            }
            if pending.expires_at_ms > now_ms {
                return Err("another Android owner decision is awaiting confirmation".to_owned());
            }
        }
        self.pending = Some(proposal.clone());
        Ok(proposal)
    }

    pub(crate) fn status(&self) -> Result<AppAndroidOwnerNativeStatus, String> {
        let document = self.ready_document()?;
        let (key_id, public_key_hex, desktop_identity_digest) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        if desktop_identity_digest != document.desktop_identity_digest {
            return Err("Android owner desktop identity changed".to_owned());
        }
        Ok(AppAndroidOwnerNativeStatus {
            desktop_identity_key_id: key_id,
            desktop_identity_public_key_hex: public_key_hex,
            desktop_identity_digest,
            owner_generation: document.owner_generation,
            latest_receipt_digest: document.latest_receipt_digest.clone(),
            identity_bootstrap_available: self.offered_bootstrap.is_some()
                || self.rebind_offer.is_some()
                || self
                    .pending_rebind_bootstrap
                    .as_ref()
                    .or(self.pending_bootstrap.as_ref())
                    .is_some_and(|pending| {
                        !pending.approval_confirmed && pending.completion.is_none()
                    }),
            pending: self.pending.clone(),
            pending_recovery: self.pending_recovery.clone(),
            pending_identity_bootstrap: self
                .pending_rebind_bootstrap
                .as_ref()
                .or(self.pending_bootstrap.as_ref())
                .filter(|pending| pending.finalized_status.is_none())
                .map(|pending| AppAndroidOwnerPendingBootstrapStatus {
                    bootstrap: pending.request.bootstrap.clone(),
                    desktop_identity_fingerprint: pending.desktop_identity_fingerprint.clone(),
                    completion_ready: pending.completion.is_some(),
                }),
            pending_rebind_offer: self
                .rebind_offer
                .clone()
                .or_else(|| {
                    self.pending_rebind_bootstrap
                        .as_ref()
                        .and_then(|pending| pending.rebind_offer.clone())
                })
                .or_else(|| {
                    self.pending_bootstrap
                        .as_ref()
                        .filter(|pending| pending.finalized_status.is_none())
                        .and_then(|pending| pending.rebind_offer.clone())
                }),
        })
    }

    pub(crate) fn confirm(
        &mut self,
        proposal_id: &str,
        owner_generation: u64,
        expected_display_digest: &str,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerReceipt, String> {
        if self.runtime_rebind_incomplete() {
            return Err("Android owner runtime rebind must finish before confirmation".to_owned());
        }
        if let Some(receipt) =
            self.find_replay(proposal_id, owner_generation, expected_display_digest)?
        {
            return Ok(receipt);
        }
        let proposal = self
            .pending
            .as_ref()
            .filter(|proposal| {
                proposal.proposal_id == proposal_id
                    && proposal.owner_generation == owner_generation
                    && proposal.display_digest == expected_display_digest
            })
            .cloned()
            .ok_or_else(|| {
                "Android owner confirmation does not match the displayed proposal".to_owned()
            })?;
        proposal
            .validate(now_ms)
            .map_err(|_| "Android owner proposal expired before confirmation".to_owned())?;
        let current = self.ready_document()?.clone();
        if proposal.desktop_identity_digest != current.desktop_identity_digest
            || proposal.owner_generation != current.owner_generation.saturating_add(1)
            || proposal.previous_receipt_digest != current.latest_receipt_digest
        {
            return Err("Android owner authority changed before confirmation".to_owned());
        }
        validate_transition(&current, &proposal)?;

        let (key_id, public_key_hex, desktop_identity_digest) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        if desktop_identity_digest != current.desktop_identity_digest {
            return Err("Android owner desktop identity changed".to_owned());
        }
        let unsigned = AppAndroidOwnerReceipt::unsigned(&proposal, key_id.clone(), now_ms)
            .map_err(|_| "Android owner receipt could not be constructed".to_owned())?;
        let receipt = crate::app_macos_identity::sign_app_android_owner_receipt(unsigned)?;
        receipt
            .verify(
                &proposal,
                &key_id,
                &public_key_hex,
                &desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| "Android owner receipt failed self-verification".to_owned())?;

        let mut candidate = current;
        apply_receipt(&mut candidate, receipt.clone())?;
        if let Err(error) = self.publish(candidate, now_ms) {
            self.ready = false;
            return Err(error);
        }
        self.pending = None;
        Ok(receipt)
    }

    pub(crate) fn signed_status(
        &self,
        challenge: &AppAndroidOwnerStatusChallenge,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerStatusReceipt, String> {
        let document = self.ready_document()?;
        challenge
            .validate(now_ms)
            .map_err(|_| "Android owner status challenge is invalid or expired".to_owned())?;
        let (key_id, public_key_hex, desktop_identity_digest) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        if challenge.desktop_identity_digest != desktop_identity_digest
            || document.desktop_identity_digest != desktop_identity_digest
        {
            return Err("Android owner status challenge targets another desktop".to_owned());
        }
        let unsigned = AppAndroidOwnerStatusReceipt::unsigned(
            challenge,
            document.owner_generation,
            document.latest_receipt_digest.clone(),
            now_ms,
            key_id.clone(),
        )
        .map_err(|_| "Android owner status could not be constructed".to_owned())?;
        let status = crate::app_macos_identity::sign_app_android_owner_status(unsigned)?;
        status
            .verify(
                challenge,
                &key_id,
                &public_key_hex,
                &desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| "Android owner status failed self-verification".to_owned())?;
        Ok(status)
    }

    pub(crate) fn stage_recovery(
        &mut self,
        challenge: AppAndroidOwnerRecoveryChallenge,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerRecoveryChallenge, String> {
        if self.runtime_rebind_incomplete() {
            return Err("Android owner runtime rebind must finish before recovery".to_owned());
        }
        if self
            .pending
            .as_ref()
            .is_some_and(|proposal| proposal.expires_at_ms > now_ms)
        {
            return Err("Android owner decision must finish before recovery".to_owned());
        }
        self.pending = None;
        if let Some(replay) = self.recovery_replay.as_ref() {
            if replay.challenge.digest().ok() == challenge.digest().ok() {
                return Ok(replay.challenge.clone());
            }
            return Err("another approved Android owner recovery awaits delivery".to_owned());
        }
        let document = self.ready_document()?;
        require_nonempty_recovery_head(document)?;
        challenge
            .validate(now_ms)
            .map_err(|_| "Android owner recovery challenge is invalid or expired".to_owned())?;
        if challenge.desktop_identity_digest != document.desktop_identity_digest {
            return Err("Android owner recovery targets another desktop".to_owned());
        }
        if let Some(pending) = self.pending_recovery.as_ref() {
            if pending.digest().ok() == challenge.digest().ok() {
                return Ok(pending.clone());
            }
            if pending.expires_at_ms > now_ms {
                return Err("another Android owner recovery is awaiting confirmation".to_owned());
            }
        }
        self.pending_recovery = Some(challenge.clone());
        Ok(challenge)
    }

    pub(crate) fn confirm_recovery(
        &mut self,
        recovery_nonce: &str,
        expected_display_digest: &str,
        now_ms: i64,
    ) -> Result<AppAndroidOwnerRecoverySnapshot, String> {
        if self.runtime_rebind_incomplete() {
            return Err(
                "Android owner runtime rebind must finish before recovery confirmation".to_owned(),
            );
        }
        if let Some(replay) = self.recovery_replay.as_ref() {
            if replay.challenge.recovery_nonce == recovery_nonce
                && replay.challenge.display_digest == expected_display_digest
            {
                return Ok(replay.snapshot.clone());
            }
            return Err("another approved Android owner recovery awaits delivery".to_owned());
        }
        let challenge = self
            .pending_recovery
            .as_ref()
            .filter(|challenge| {
                challenge.recovery_nonce == recovery_nonce
                    && challenge.display_digest == expected_display_digest
            })
            .cloned()
            .ok_or_else(|| {
                "Android owner recovery does not match the displayed challenge".to_owned()
            })?;
        challenge
            .validate(now_ms)
            .map_err(|_| "Android owner recovery expired before confirmation".to_owned())?;
        let document = self.ready_document()?;
        require_nonempty_recovery_head(document)?;
        let (key_id, public_key_hex, desktop_identity_digest) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        if challenge.desktop_identity_digest != desktop_identity_digest
            || document.desktop_identity_digest != desktop_identity_digest
        {
            return Err("Android owner recovery desktop identity changed".to_owned());
        }
        let records = document
            .records
            .iter()
            .cloned()
            .map(|receipt| AppAndroidOwnerStatusRecord {
                target_ref: receipt.target_ref.clone(),
                receipt,
            })
            .collect::<Vec<_>>();
        let unsigned = AppAndroidOwnerRecoverySnapshot::unsigned(
            &challenge,
            document.owner_generation,
            document.latest_receipt_digest.clone(),
            records,
            now_ms,
            key_id.clone(),
        )
        .map_err(|_| "Android owner recovery snapshot could not be constructed".to_owned())?;
        let snapshot = crate::app_macos_identity::sign_app_android_owner_recovery(unsigned)?;
        snapshot
            .verify(
                &challenge,
                &key_id,
                &public_key_hex,
                &desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| "Android owner recovery snapshot failed self-verification".to_owned())?;
        let replay = PersistedAndroidOwnerRecoveryReplay {
            schema: APP_ANDROID_OWNER_RECOVERY_REPLAY_V1.to_owned(),
            challenge,
            snapshot: snapshot.clone(),
        };
        self.persist_recovery_replay(&replay)?;
        self.recovery_replay = Some(replay);
        Ok(snapshot)
    }

    fn runtime_rebind_incomplete(&self) -> bool {
        self.rebind_offer.is_some()
            || self.pending_rebind_bootstrap.is_some()
            || self.pending_bootstrap.as_ref().is_some_and(|pending| {
                pending.rebind_offer.is_some() && pending.finalized_status.is_none()
            })
    }

    pub(crate) fn complete_recovery_submission(
        &mut self,
        snapshot_digest: &str,
    ) -> Result<(), String> {
        let replay = self
            .recovery_replay
            .as_ref()
            .ok_or_else(|| "Android owner recovery replay is unavailable".to_owned())?;
        if replay.snapshot.digest().ok().as_deref() != Some(snapshot_digest) {
            return Err("Android owner recovery acknowledgement changed identity".to_owned());
        }
        self.remove_recovery_replay()?;
        self.recovery_replay = None;
        self.pending_recovery = None;
        Ok(())
    }

    fn find_replay(
        &self,
        proposal_id: &str,
        owner_generation: u64,
        display_digest: &str,
    ) -> Result<Option<AppAndroidOwnerReceipt>, String> {
        let document = self.ready_document()?;
        Ok(document
            .records
            .iter()
            .find(|receipt| {
                receipt.proposal_id == proposal_id
                    && receipt.owner_generation == owner_generation
                    && receipt.display_digest == display_digest
            })
            .cloned())
    }

    fn publish(
        &mut self,
        candidate: PersistedAndroidOwnerStore,
        now_ms: i64,
    ) -> Result<(), String> {
        let store_path = self
            .store_path
            .as_ref()
            .ok_or_else(|| "Android owner store path is unavailable".to_owned())?
            .clone();
        let staged_path = self
            .staged_path
            .as_ref()
            .ok_or_else(|| "Android owner staged path is unavailable".to_owned())?
            .clone();
        let anchor_account = self
            .anchor_account
            .as_ref()
            .ok_or_else(|| "Android owner anchor identity is unavailable".to_owned())?
            .clone();
        let current = self.ready_document()?.clone();
        let (key_id, public_key_hex, _) =
            crate::app_macos_identity::app_android_owner_desktop_identity()?;
        validate_document(&candidate, &key_id, &public_key_hex, now_ms)?;
        let bytes = encode_document(&candidate)?;
        let digest = document_digest(&bytes);
        write_private_staged(&staged_path, &bytes)?;

        let committed = if current.owner_generation == 0 {
            None
        } else {
            Some(AndroidOwnerAnchorPoint {
                generation: current.owner_generation,
                document_digest: document_digest(&encode_document(&current)?),
            })
        };
        let point = AndroidOwnerAnchorPoint {
            generation: candidate.owner_generation,
            document_digest: digest,
        };
        let pending_anchor = AndroidOwnerAnchor {
            schema: APP_ANDROID_OWNER_ANCHOR_V1.to_owned(),
            desktop_identity_digest: candidate.desktop_identity_digest.clone(),
            committed,
            pending: Some(point.clone()),
        };
        self.store_anchor(&anchor_account, &pending_anchor)?;
        std::fs::rename(&staged_path, &store_path)
            .map_err(|error| format!("could not publish Android owner state: {error}"))?;
        sync_parent_directory(&store_path)?;
        let committed_anchor = AndroidOwnerAnchor {
            schema: APP_ANDROID_OWNER_ANCHOR_V1.to_owned(),
            desktop_identity_digest: candidate.desktop_identity_digest.clone(),
            committed: Some(point),
            pending: None,
        };
        self.store_anchor(&anchor_account, &committed_anchor)?;
        self.document = Some(candidate);
        Ok(())
    }

    fn persist_recovery_replay(
        &self,
        replay: &PersistedAndroidOwnerRecoveryReplay,
    ) -> Result<(), String> {
        let path = self
            .recovery_path
            .as_ref()
            .ok_or_else(|| "Android owner recovery path is unavailable".to_owned())?;
        let staged = self
            .recovery_staged_path
            .as_ref()
            .ok_or_else(|| "Android owner recovery stage is unavailable".to_owned())?;
        let bytes = serde_json::to_vec(replay)
            .map_err(|_| "Android owner recovery replay could not be encoded".to_owned())?;
        write_private_staged(staged, &bytes)?;
        std::fs::rename(staged, path)
            .map_err(|error| format!("could not publish Android owner recovery: {error}"))?;
        sync_parent_directory(path)
    }

    fn remove_recovery_replay(&self) -> Result<(), String> {
        if let Some(path) = self.recovery_path.as_ref() {
            remove_regular_file_if_present(path)?;
            sync_parent_directory(path)?;
        }
        if let Some(path) = self.recovery_staged_path.as_ref() {
            remove_regular_file_if_present(path)?;
        }
        Ok(())
    }

    fn persist_bootstrap_replay(
        &self,
        replay: &PersistedAndroidOwnerBootstrapReplay,
    ) -> Result<(), String> {
        let path = self
            .bootstrap_path
            .as_ref()
            .ok_or_else(|| "Android owner bootstrap path is unavailable".to_owned())?;
        let staged = self
            .bootstrap_staged_path
            .as_ref()
            .ok_or_else(|| "Android owner bootstrap stage is unavailable".to_owned())?;
        let bytes = serde_json::to_vec(replay)
            .map_err(|_| "Android owner bootstrap replay could not be encoded".to_owned())?;
        write_private_staged(staged, &bytes)?;
        std::fs::rename(staged, path)
            .map_err(|error| format!("could not publish Android owner bootstrap: {error}"))?;
        sync_parent_directory(path)
    }

    fn ready_document(&self) -> Result<&PersistedAndroidOwnerStore, String> {
        if !self.ready {
            return Err("Android owner is not initialized".to_owned());
        }
        self.document
            .as_ref()
            .ok_or_else(|| "Android owner state is unavailable".to_owned())
    }

    fn load_anchor(&self, account: &str) -> Result<Option<AndroidOwnerAnchor>, String> {
        #[cfg(test)]
        {
            let _ = account;
            return Ok(self.test_anchor.clone());
        }
        #[cfg(not(test))]
        {
            let entry = keyring::Entry::new(APP_ANDROID_OWNER_KEYCHAIN_SERVICE, account)
                .map_err(|_| "Android owner Keychain anchor is unavailable".to_owned())?;
            match entry.get_password() {
                Ok(encoded) => {
                    if encoded.len() > APP_ANDROID_OWNER_MAX_ANCHOR_BYTES {
                        return Err("Android owner Keychain anchor is oversized".to_owned());
                    }
                    serde_json::from_str(&encoded)
                        .map(Some)
                        .map_err(|_| "Android owner Keychain anchor is corrupt".to_owned())
                },
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(_) => Err("Android owner Keychain anchor is unavailable".to_owned()),
            }
        }
    }

    fn store_anchor(&mut self, account: &str, anchor: &AndroidOwnerAnchor) -> Result<(), String> {
        validate_anchor(anchor)?;
        #[cfg(test)]
        {
            let _ = account;
            self.test_anchor = Some(anchor.clone());
            return Ok(());
        }
        #[cfg(not(test))]
        {
            let encoded = serde_json::to_string(anchor)
                .map_err(|_| "Android owner Keychain anchor could not be encoded".to_owned())?;
            if encoded.len() > APP_ANDROID_OWNER_MAX_ANCHOR_BYTES {
                return Err("Android owner Keychain anchor is oversized".to_owned());
            }
            keyring::Entry::new(APP_ANDROID_OWNER_KEYCHAIN_SERVICE, account)
                .map_err(|_| "Android owner Keychain anchor is unavailable".to_owned())?
                .set_password(&encoded)
                .map_err(|_| "Android owner Keychain anchor could not be persisted".to_owned())
        }
    }
}

fn validate_transition(
    document: &PersistedAndroidOwnerStore,
    proposal: &AppAndroidOwnerProposal,
) -> Result<(), String> {
    if proposal.allowed_packages.iter().any(|package| {
        package == &proposal.owner_app_package
            || magician_app_contract::android_owner::is_android_owner_companion_package(package)
    }) {
        return Err("Android owner app identity cannot be an observation target".to_owned());
    }
    let current = document.records.iter().find(|receipt| {
        receipt.principal == proposal.principal
            && receipt.workspace == proposal.workspace
            && receipt.target_ref == proposal.target_ref
    });
    match proposal.operation {
        AppAndroidOwnerOperation::EnrollAttestedDevice => {
            if current.is_some()
                || document.records.iter().any(|receipt| {
                    receipt.principal == proposal.principal
                        && receipt.workspace == proposal.workspace
                        && (receipt.enrollment_id == proposal.enrollment_id
                            || receipt.key_id == proposal.key_id)
                })
            {
                return Err("Android device identity is already enrolled in this scope".to_owned());
            }
            if document.records.len() >= APP_ANDROID_OWNER_MAX_STATUS_RECORDS {
                return Err("Android owner record ceiling is exhausted".to_owned());
            }
        },
        AppAndroidOwnerOperation::ApproveActions | AppAndroidOwnerOperation::RevokeActions => {
            let current = current
                .ok_or_else(|| "Android owner target is not enrolled in this scope".to_owned())?;
            if current.operation == AppAndroidOwnerOperation::RevokeDevice
                || !same_device_identity(current, proposal)
                || current.resulting_review_generation != proposal.expected_review_generation
            {
                return Err("Android owner target identity or review generation changed".to_owned());
            }
            match proposal.operation {
                AppAndroidOwnerOperation::ApproveActions => {
                    if current.operation == AppAndroidOwnerOperation::ApproveActions {
                        return Err("Android action authority is already approved".to_owned());
                    }
                },
                AppAndroidOwnerOperation::RevokeActions => {
                    if current.operation != AppAndroidOwnerOperation::ApproveActions
                        || current.allowed_packages != proposal.allowed_packages
                    {
                        return Err(
                            "Android action revocation does not match current authority".to_owned()
                        );
                    }
                },
                AppAndroidOwnerOperation::EnrollAttestedDevice
                | AppAndroidOwnerOperation::RevokeDevice => unreachable!(),
            }
        },
        AppAndroidOwnerOperation::RevokeDevice => {
            return Err(
                "Android device revocation is unavailable in the V1 desktop owner".to_owned(),
            );
        },
    }
    Ok(())
}

fn same_device_identity(
    current: &AppAndroidOwnerReceipt,
    proposal: &AppAndroidOwnerProposal,
) -> bool {
    current.enrollment_id == proposal.enrollment_id
        && current.device_label == proposal.device_label
        && current.key_id == proposal.key_id
        && current.automation_identity_digest == proposal.automation_identity_digest
        && current.owner_app_package == proposal.owner_app_package
        && current.app_version_code == proposal.app_version_code
        && current.app_signing_sha256 == proposal.app_signing_sha256
        && current.apk_sha256 == proposal.apk_sha256
        && current.attestation_root_sha256 == proposal.attestation_root_sha256
        && current.attestation_security_level == proposal.attestation_security_level
        && current.attestation_policy_digest == proposal.attestation_policy_digest
}

fn apply_receipt(
    document: &mut PersistedAndroidOwnerStore,
    receipt: AppAndroidOwnerReceipt,
) -> Result<(), String> {
    let digest = receipt
        .digest()
        .map_err(|_| "Android owner receipt digest is invalid".to_owned())?;
    if let Some(current) = document.records.iter_mut().find(|current| {
        current.principal == receipt.principal
            && current.workspace == receipt.workspace
            && current.target_ref == receipt.target_ref
    }) {
        *current = receipt.clone();
    } else {
        document.records.push(receipt.clone());
    }
    document.records.sort_by(|left, right| {
        (&left.principal, &left.workspace, &left.target_ref).cmp(&(
            &right.principal,
            &right.workspace,
            &right.target_ref,
        ))
    });
    document.owner_generation = receipt.owner_generation;
    document.latest_receipt_digest = Some(digest);
    document.latest_receipt = Some(receipt);
    Ok(())
}

fn recover_document(
    anchor: Option<AndroidOwnerAnchor>,
    main: Option<Vec<u8>>,
    staged: Option<Vec<u8>>,
    store_path: &Path,
    staged_path: &Path,
    desktop_identity_digest: &str,
    key_id: &str,
    public_key_hex: &str,
    now_ms: i64,
) -> Result<
    (
        Option<PersistedAndroidOwnerStore>,
        Option<AndroidOwnerAnchor>,
    ),
    String,
> {
    let Some(mut anchor) = anchor else {
        if main.is_some() {
            return Err("Android owner state has no Keychain high-water anchor".to_owned());
        }
        return Ok((None, None));
    };
    validate_anchor(&anchor)?;
    if anchor.desktop_identity_digest != desktop_identity_digest {
        return Err("Android owner Keychain identity changed".to_owned());
    }

    if let Some(pending) = anchor.pending.clone() {
        if bytes_match_point(main.as_deref(), &pending) {
            let document = decode_and_validate_document(
                main.as_deref().expect("matched pending main document"),
                key_id,
                public_key_hex,
                now_ms,
            )?;
            // A prior process may have crashed after rename but before the
            // directory fsync. Repeat that durability fence before promoting
            // the pending Keychain point to committed.
            sync_parent_directory(store_path)?;
            anchor.committed = Some(pending);
            anchor.pending = None;
            return Ok((Some(document), Some(anchor)));
        }
        if bytes_match_point(staged.as_deref(), &pending)
            && match anchor.committed.as_ref() {
                Some(committed) => bytes_match_point(main.as_deref(), committed),
                None => main.is_none(),
            }
        {
            let document = decode_and_validate_document(
                staged.as_deref().expect("matched pending staged document"),
                key_id,
                public_key_hex,
                now_ms,
            )?;
            std::fs::rename(staged_path, store_path).map_err(|error| {
                format!("could not recover Android owner staged state: {error}")
            })?;
            sync_parent_directory(store_path)?;
            anchor.committed = Some(pending);
            anchor.pending = None;
            return Ok((Some(document), Some(anchor)));
        }
        return Err("Android owner staged transaction is incomplete or corrupt".to_owned());
    }

    let committed = anchor
        .committed
        .as_ref()
        .ok_or_else(|| "Android owner Keychain anchor has no committed state".to_owned())?;
    let bytes = main
        .as_deref()
        .filter(|bytes| bytes_match_point(Some(bytes), committed))
        .ok_or_else(|| "Android owner state was rolled back or replaced".to_owned())?;
    let document = decode_and_validate_document(bytes, key_id, public_key_hex, now_ms)?;
    // The committed point already matches the durable document. Returning no
    // replacement avoids rewriting the Keychain high-water on every startup.
    Ok((Some(document), None))
}

fn decode_and_validate_document(
    bytes: &[u8],
    key_id: &str,
    public_key_hex: &str,
    now_ms: i64,
) -> Result<PersistedAndroidOwnerStore, String> {
    let document: PersistedAndroidOwnerStore =
        serde_json::from_slice(bytes).map_err(|_| "Android owner state is malformed".to_owned())?;
    validate_document(&document, key_id, public_key_hex, now_ms)?;
    Ok(document)
}

fn decode_and_validate_recovery_replay(
    bytes: &[u8],
    document: &PersistedAndroidOwnerStore,
    key_id: &str,
    public_key_hex: &str,
    now_ms: i64,
) -> Result<PersistedAndroidOwnerRecoveryReplay, String> {
    let replay: PersistedAndroidOwnerRecoveryReplay = serde_json::from_slice(bytes)
        .map_err(|_| "Android owner recovery replay is malformed".to_owned())?;
    let actual_identity = magician_app_contract::macos_host::app_macos_desktop_identity_digest(
        key_id,
        public_key_hex,
    )
    .map_err(|_| "Android owner desktop identity is invalid".to_owned())?;
    if replay.schema != APP_ANDROID_OWNER_RECOVERY_REPLAY_V1
        || replay.challenge.desktop_identity_digest != document.desktop_identity_digest
        || replay.snapshot.owner_generation != document.owner_generation
        || replay.snapshot.latest_receipt_digest != document.latest_receipt_digest
        || replay.snapshot.records.len() != document.records.len()
        || !replay
            .snapshot
            .records
            .iter()
            .zip(&document.records)
            .all(|(record, receipt)| {
                record.target_ref == receipt.target_ref && &record.receipt == receipt
            })
    {
        return Err("Android owner recovery replay no longer matches durable authority".to_owned());
    }
    replay
        .snapshot
        .verify(
            &replay.challenge,
            key_id,
            public_key_hex,
            &actual_identity,
            now_ms,
        )
        .map_err(|_| "Android owner recovery replay signature is invalid".to_owned())?;
    Ok(replay)
}

fn decode_and_validate_bootstrap_replay(
    bytes: &[u8],
    desktop_identity_digest: &str,
    document: &PersistedAndroidOwnerStore,
    now_ms: i64,
) -> Result<PersistedAndroidOwnerBootstrapReplay, String> {
    let replay: PersistedAndroidOwnerBootstrapReplay = serde_json::from_slice(bytes)
        .map_err(|_| "Android owner bootstrap replay is malformed".to_owned())?;
    let challenge_issued_at_ms = replay.completion.challenge.issued_at_ms;
    replay
        .request
        .validate(challenge_issued_at_ms)
        .map_err(|_| "Android owner bootstrap replay request is invalid".to_owned())?;
    verify_bootstrap_completion(&replay.completion, now_ms)?;
    if replay.schema != APP_ANDROID_OWNER_BOOTSTRAP_REPLAY_V1
        || replay.desktop_identity_fingerprint != desktop_identity_digest
        || replay.request.desktop_identity_digest != desktop_identity_digest
        || replay.completion.challenge.desktop_identity_key_id
            != replay.request.desktop_identity_key_id
        || replay.completion.challenge.desktop_identity_public_key_hex
            != replay.request.desktop_identity_public_key_hex
        || replay.completion.challenge.owner_approval_code_digest
            != replay.request.owner_approval_code_digest
    {
        return Err("Android owner bootstrap replay changed identity".to_owned());
    }
    if let Some(status) = replay.finalized_status.as_ref() {
        let expected = AppAndroidOwnerBootstrapStatus::from_verified(
            &replay.completion.challenge,
            &replay.completion.attestation,
            status.pinned_at_ms,
        )
        .map_err(|_| "Android owner finalized bootstrap replay is invalid".to_owned())?;
        if &expected != status || status.pinned_at_ms > now_ms.saturating_add(5_000) {
            return Err("Android owner finalized bootstrap replay changed identity".to_owned());
        }
    }
    if let Some(offer) = replay.rebind_offer.as_ref() {
        offer
            .validate(offer.issued_at_ms)
            .map_err(|_| "Android owner persisted rebind offer is invalid".to_owned())?;
        if offer.bootstrap != replay.request.bootstrap
            || offer.desktop_identity_key_id != replay.request.desktop_identity_key_id
            || offer.desktop_identity_digest != replay.request.desktop_identity_digest
            || offer.desktop_identity_digest != desktop_identity_digest
            || offer.owner_generation != document.owner_generation
            || offer.latest_receipt_digest != document.latest_receipt_digest
        {
            return Err("Android owner persisted rebind changed the durable head".to_owned());
        }
    }
    Ok(replay)
}

fn verify_bootstrap_completion(
    completion: &AppAndroidOwnerBootstrapCompletion,
    now_ms: i64,
) -> Result<(), String> {
    let attested_at_ms = completion.attestation.attested_at_ms;
    completion
        .attestation
        .verify(&completion.challenge, attested_at_ms)
        .map_err(|_| "Android owner bootstrap attestation is invalid".to_owned())?;
    if attested_at_ms > now_ms.saturating_add(5_000) {
        return Err("Android owner bootstrap attestation is from the future".to_owned());
    }
    Ok(())
}

fn validate_document(
    document: &PersistedAndroidOwnerStore,
    key_id: &str,
    public_key_hex: &str,
    now_ms: i64,
) -> Result<(), String> {
    let actual_identity = magician_app_contract::macos_host::app_macos_desktop_identity_digest(
        key_id,
        public_key_hex,
    )
    .map_err(|_| "Android owner desktop identity is invalid".to_owned())?;
    if document.schema != APP_ANDROID_OWNER_STORE_V1
        || document.desktop_identity_digest != actual_identity
        || document.records.len() > APP_ANDROID_OWNER_MAX_STATUS_RECORDS
        || (document.owner_generation == 0) != document.latest_receipt_digest.is_none()
        || (document.owner_generation == 0) != document.latest_receipt.is_none()
    {
        return Err("Android owner state has invalid identity or high-water fields".to_owned());
    }
    if !document.records.windows(2).all(|values| {
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
        return Err("Android owner records are not uniquely sorted".to_owned());
    }
    for receipt in &document.records {
        if receipt.operation == AppAndroidOwnerOperation::RevokeDevice {
            return Err(
                "Android device revocation is unsupported by the V1 owner store".to_owned(),
            );
        }
        receipt
            .verify_desktop_identity(key_id, public_key_hex, &actual_identity, now_ms)
            .map_err(|_| "Android owner record signature is invalid".to_owned())?;
        if receipt.owner_generation > document.owner_generation {
            return Err("Android owner record is above the durable high-water".to_owned());
        }
    }
    if let Some(latest) = document.latest_receipt.as_ref() {
        latest
            .verify_desktop_identity(key_id, public_key_hex, &actual_identity, now_ms)
            .map_err(|_| "Android owner latest receipt signature is invalid".to_owned())?;
        if latest.operation == AppAndroidOwnerOperation::RevokeDevice
            || latest.owner_generation != document.owner_generation
            || document.latest_receipt_digest.as_deref()
                != Some(
                    latest
                        .digest()
                        .map_err(|_| "Android owner latest receipt digest is invalid".to_owned())?
                        .as_str(),
                )
            || !document.records.iter().any(|record| record == latest)
        {
            return Err("Android owner latest receipt is not the durable high-water".to_owned());
        }
    }
    Ok(())
}

fn require_nonempty_recovery_head(document: &PersistedAndroidOwnerStore) -> Result<(), String> {
    if document.owner_generation == 0
        || document.latest_receipt_digest.is_none()
        || document.latest_receipt.is_none()
        || document.records.is_empty()
    {
        return Err("Android owner has no established authority head to recover".to_owned());
    }
    Ok(())
}

fn validate_anchor(anchor: &AndroidOwnerAnchor) -> Result<(), String> {
    if anchor.schema != APP_ANDROID_OWNER_ANCHOR_V1
        || !valid_blake3_digest(&anchor.desktop_identity_digest)
        || anchor.committed.as_ref().is_some_and(|point| {
            point.generation == 0 || !valid_blake3_digest(&point.document_digest)
        })
        || anchor.pending.as_ref().is_some_and(|point| {
            point.generation == 0
                || !valid_blake3_digest(&point.document_digest)
                || anchor.committed.as_ref().is_some_and(|committed| {
                    committed.generation.checked_add(1) != Some(point.generation)
                })
        })
        || (anchor.committed.is_none()
            && anchor
                .pending
                .as_ref()
                .is_some_and(|pending| pending.generation != 1))
    {
        return Err("Android owner Keychain anchor is invalid".to_owned());
    }
    Ok(())
}

fn valid_blake3_digest(value: &str) -> bool {
    value.strip_prefix("blake3:").is_some_and(|value| {
        value.len() == 64
            && value
                .bytes()
                .all(|value| value.is_ascii_digit() || matches!(value, b'a'..=b'f'))
    })
}

fn valid_android_package(value: &str) -> bool {
    value.len() >= 3
        && value.len() <= 255
        && value.contains('.')
        && value.split('.').all(|component| {
            component
                .bytes()
                .next()
                .is_some_and(|byte| byte.is_ascii_alphabetic())
                && component
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        })
}

fn valid_owner_token(value: &str, maximum_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= maximum_bytes
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'_' | b'-' | b'.' | b':' | b'/' | b'@' | b'#')
        })
}

fn bytes_match_point(bytes: Option<&[u8]>, point: &AndroidOwnerAnchorPoint) -> bool {
    bytes.is_some_and(|bytes| document_digest(bytes) == point.document_digest)
}

fn encode_document(document: &PersistedAndroidOwnerStore) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(document)
        .map_err(|_| "Android owner state could not be encoded".to_owned())?;
    if bytes.is_empty() || bytes.len() as u64 > APP_ANDROID_OWNER_MAX_STORE_BYTES {
        return Err("Android owner state exceeds its byte ceiling".to_owned());
    }
    Ok(bytes)
}

fn document_digest(bytes: &[u8]) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.desktop.android-owner-document.v1\0");
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn missing_bootstrap_status_digest(
    desktop_identity_digest: &str,
    owner_generation: u64,
    latest_receipt_digest: Option<&str>,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.desktop.android-owner-missing-bootstrap.v1\0");
    for bytes in [
        desktop_identity_digest.as_bytes(),
        latest_receipt_digest.unwrap_or("").as_bytes(),
    ] {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    }
    hasher.update(&owner_generation.to_le_bytes());
    hasher.update(&[u8::from(latest_receipt_digest.is_some())]);
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn android_owner_anchor_account(owner_root: &Path) -> String {
    let bytes = owner_root.as_os_str().as_encoded_bytes();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.desktop.android-owner-anchor-account.v1\0");
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
    format!("authority-{}", hasher.finalize().to_hex())
}

fn ensure_private_owner_directory(app_data_root: &Path) -> Result<PathBuf, String> {
    ensure_private_directory(
        app_data_root,
        APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY,
    )
}

fn ensure_private_directory(app_data_root: &Path, directory_name: &str) -> Result<PathBuf, String> {
    match std::fs::symlink_metadata(app_data_root) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {},
        Ok(_) => return Err("desktop app-data root is not a regular directory".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(app_data_root)
                .map_err(|error| format!("could not create desktop app-data root: {error}"))?;
        },
        Err(error) => {
            return Err(format!("could not inspect desktop app-data root: {error}"));
        },
    }
    let app_data_metadata = std::fs::symlink_metadata(app_data_root)
        .map_err(|_| "desktop app-data root is unavailable".to_owned())?;
    if !app_data_metadata.is_dir() || app_data_metadata.file_type().is_symlink() {
        return Err("desktop app-data root is not a regular directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if app_data_metadata.uid() != unsafe { libc::geteuid() } {
            return Err("desktop app-data root has another owner".to_owned());
        }
    }
    let canonical_app_data = std::fs::canonicalize(app_data_root)
        .map_err(|_| "desktop app-data root is unavailable".to_owned())?;
    let owner_root = canonical_app_data.join(directory_name);
    match std::fs::create_dir(&owner_root) {
        Ok(()) => {},
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {},
        Err(error) => return Err(format!("could not create Android owner directory: {error}")),
    }
    let metadata = std::fs::symlink_metadata(&owner_root)
        .map_err(|_| "Android owner directory is unavailable".to_owned())?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("Android owner directory is not a regular directory".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let parent_metadata = std::fs::metadata(&canonical_app_data)
            .map_err(|_| "desktop app-data owner is unavailable".to_owned())?;
        if metadata.uid() != parent_metadata.uid() {
            return Err("Android owner directory has another owner".to_owned());
        }
        std::fs::set_permissions(&owner_root, std::fs::Permissions::from_mode(0o700)).map_err(
            |_| "Android owner directory permissions could not be restricted".to_owned(),
        )?;
    }
    let canonical_owner = std::fs::canonicalize(&owner_root)
        .map_err(|_| "Android owner directory could not be resolved".to_owned())?;
    if canonical_owner != owner_root
        || canonical_owner.parent() != Some(canonical_app_data.as_path())
        || std::fs::symlink_metadata(&owner_root)
            .map(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
            .unwrap_or(true)
    {
        return Err("Android owner directory escaped the app-data root".to_owned());
    }
    Ok(canonical_owner)
}

fn read_private_document(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("could not inspect Android owner state: {error}")),
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > APP_ANDROID_OWNER_MAX_STORE_BYTES
    {
        return Err("Android owner state is not a bounded regular file".to_owned());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let parent = std::fs::metadata(
            path.parent()
                .ok_or_else(|| "Android owner state has no parent".to_owned())?,
        )
        .map_err(|_| "Android owner directory is unavailable".to_owned())?;
        if metadata.uid() != parent.uid() || metadata.mode() & 0o077 != 0 {
            return Err("Android owner state is not owner-only".to_owned());
        }
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(path)
        .map_err(|error| format!("could not open Android owner state: {error}"))?;
    let opened = file
        .metadata()
        .map_err(|_| "could not inspect opened Android owner state".to_owned())?;
    if opened.len() != metadata.len() {
        return Err("Android owner state changed while opening".to_owned());
    }
    let capacity = usize::try_from(opened.len())
        .map_err(|_| "Android owner state length is out of range".to_owned())?;
    let mut bytes = Vec::with_capacity(capacity);
    file.take(APP_ANDROID_OWNER_MAX_STORE_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read Android owner state: {error}"))?;
    if bytes.is_empty() || bytes.len() as u64 > APP_ANDROID_OWNER_MAX_STORE_BYTES {
        return Err("Android owner state exceeds its byte ceiling".to_owned());
    }
    Ok(Some(bytes))
}

fn write_private_staged(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() as u64 > APP_ANDROID_OWNER_MAX_STORE_BYTES {
        return Err("Android owner staged state exceeds its byte ceiling".to_owned());
    }
    remove_regular_file_if_present(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .map_err(|error| format!("could not create Android owner staged state: {error}"))?;
    file.write_all(bytes)
        .map_err(|error| format!("could not write Android owner staged state: {error}"))?;
    file.sync_all()
        .map_err(|error| format!("could not sync Android owner staged state: {error}"))
}

fn remove_regular_file_if_present(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            std::fs::remove_file(path)
                .map_err(|error| format!("could not remove stale Android owner stage: {error}"))
        },
        Ok(_) => Err("Android owner staged path is not a regular file".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "could not inspect Android owner staged state: {error}"
        )),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), String> {
    File::open(
        path.parent()
            .ok_or_else(|| "Android owner state has no parent".to_owned())?,
    )
    .and_then(|directory| directory.sync_all())
    .map_err(|error| format!("could not sync Android owner directory: {error}"))
}

fn current_unix_ms() -> Result<i64, String> {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_owned())?
        .as_millis();
    i64::try_from(millis).map_err(|_| "system clock is out of range".to_owned())
}

fn ensure_trusted_settings_window(window: &WebviewWindow) -> Result<(), String> {
    if window.label() != "settings" {
        return Err("Android authority is available only in native Settings".to_owned());
    }
    let url = window
        .url()
        .map_err(|_| "native Settings origin is unavailable".to_owned())?;
    #[cfg(debug_assertions)]
    let expected_scheme = "magician-desktop";
    #[cfg(not(debug_assertions))]
    let expected_scheme = "tauri";
    if url.scheme() != expected_scheme
        || url.host_str() != Some("localhost")
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Android authority requires the bundled native Settings origin".to_owned());
    }
    Ok(())
}

async fn with_owner<T, F>(app: &AppHandle, operation: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce(&mut AppAndroidAuthorityOwner, i64) -> Result<T, String> + Send + 'static,
{
    let root = app
        .path()
        .app_data_dir()
        .map_err(|_| "Android owner app-data root is unavailable".to_owned())?;
    let now_ms = current_unix_ms()?;
    let owner = std::sync::Arc::clone(&app.state::<crate::AppState>().app_android_authority);
    let permit = tokio::time::timeout(
        APP_ANDROID_OWNER_OPERATION_TIMEOUT,
        Arc::clone(android_owner_blocking_slots()).acquire_owned(),
    )
    .await
    .map_err(|_| "Android owner blocking capacity timed out".to_owned())?
    .map_err(|_| "Android owner blocking capacity is closed".to_owned())?;
    let operation = tokio::task::spawn_blocking(move || {
        // The permit intentionally lives inside the blocking closure. A caller
        // timeout cannot pretend capacity was released while Keychain or disk
        // I/O is still running.
        let _blocking_permit = permit;
        let mut owner = owner
            .lock()
            .map_err(|_| "Android owner lock is poisoned".to_owned())?;
        owner.initialize(&root, now_ms)?;
        operation(&mut owner, now_ms)
    });
    tokio::time::timeout(APP_ANDROID_OWNER_OPERATION_TIMEOUT, operation)
        .await
        .map_err(|_| "Android owner operation exceeded its deadline".to_owned())?
        .map_err(|_| "Android owner operation was interrupted".to_owned())?
}

fn android_owner_blocking_slots() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOTS: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOTS.get_or_init(|| {
        Arc::new(tokio::sync::Semaphore::new(
            APP_ANDROID_OWNER_BLOCKING_SLOTS,
        ))
    })
}

fn android_owner_native_http_client() -> Result<&'static reqwest::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::Client, String>> = OnceLock::new();
    match CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(APP_ANDROID_OWNER_NATIVE_CONNECT_TIMEOUT)
            .timeout(APP_ANDROID_OWNER_NATIVE_REQUEST_TIMEOUT)
            .build()
            .map_err(|_| "Android native HTTP client could not be constructed".to_owned())
    }) {
        Ok(client) => Ok(client),
        Err(error) => Err(error.clone()),
    }
}

fn android_owner_native_control_slot() -> &'static Arc<tokio::sync::Semaphore> {
    static SLOT: OnceLock<Arc<tokio::sync::Semaphore>> = OnceLock::new();
    SLOT.get_or_init(|| Arc::new(tokio::sync::Semaphore::new(1)))
}

async fn reserve_android_owner_native_control() -> Result<tokio::sync::OwnedSemaphorePermit, String>
{
    tokio::time::timeout(
        APP_ANDROID_OWNER_NATIVE_REQUEST_TIMEOUT,
        Arc::clone(android_owner_native_control_slot()).acquire_owned(),
    )
    .await
    .map_err(|_| "Android native control plane is busy".to_owned())?
    .map_err(|_| "Android native control plane is unavailable".to_owned())
}

async fn mint_native_authorization(
    app: &AppHandle,
    operation: AppAndroidOwnerNativeOperation,
    body_digest: Option<String>,
) -> Result<AppAndroidOwnerNativeRequest, String> {
    let request_nonce = uuid::Uuid::new_v4().to_string();
    with_owner(app, move |owner, now_ms| {
        let identity = owner.status()?;
        let unsigned = AppAndroidOwnerNativeRequest::unsigned(
            operation,
            request_nonce,
            body_digest.clone(),
            now_ms,
            now_ms.saturating_add(APP_ANDROID_OWNER_NATIVE_REQUEST_LIFETIME_MS),
            identity.desktop_identity_digest.clone(),
            identity.desktop_identity_key_id.clone(),
        )
        .map_err(|_| "Android native authorization could not be constructed".to_owned())?;
        let authorization =
            crate::app_macos_identity::sign_app_android_owner_native_request(unsigned)?;
        authorization
            .verify(
                operation,
                body_digest.as_deref(),
                &identity.desktop_identity_key_id,
                &identity.desktop_identity_public_key_hex,
                &identity.desktop_identity_digest,
                now_ms,
            )
            .map_err(|_| "Android native authorization failed self-verification".to_owned())?;
        Ok(authorization)
    })
    .await
}

async fn post_native_json<R: DeserializeOwned>(
    app: &AppHandle,
    path: &'static str,
    request_bytes: Vec<u8>,
) -> Result<R, String> {
    if request_bytes.is_empty() || request_bytes.len() > APP_ANDROID_OWNER_NATIVE_MAX_REQUEST_BYTES
    {
        return Err("Android native request exceeds its byte ceiling".to_owned());
    }
    let url = {
        let state = app.state::<crate::AppState>();
        let config = state.config.lock().await;
        config.engine_url(&format!("{APP_ANDROID_OWNER_NATIVE_PREFIX}{path}"))
    };
    let response = crate::magician_auth::authorize(android_owner_native_http_client()?.post(url))
        .header(reqwest::header::CONTENT_TYPE, "application/json")
        .body(request_bytes)
        .send()
        .await
        .map_err(|_| "Android native owner endpoint is unavailable".to_owned())?;
    if response
        .content_length()
        .is_some_and(|length| length > APP_ANDROID_OWNER_NATIVE_MAX_RESPONSE_BYTES as u64)
    {
        return Err("Android native response exceeds its byte ceiling".to_owned());
    }
    let status = response.status();
    let mut bytes = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|_| "Android native response was interrupted".to_owned())?;
        if bytes.len().saturating_add(chunk.len()) > APP_ANDROID_OWNER_NATIVE_MAX_RESPONSE_BYTES {
            return Err("Android native response exceeds its byte ceiling".to_owned());
        }
        bytes.extend_from_slice(&chunk);
    }
    if !status.is_success() {
        let detail = serde_json::from_slice::<serde_json::Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("error")
                    .or_else(|| value.get("error_code"))
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            })
            .or_else(|| {
                String::from_utf8(bytes)
                    .ok()
                    .map(|body| body.trim().to_owned())
                    .filter(|body| !body.is_empty())
            });
        return Err(match detail {
            Some(detail) => {
                format!("Android native owner endpoint returned {status}: {detail}")
            },
            None => format!("Android native owner endpoint returned {status}"),
        });
    }
    if bytes.is_empty() {
        return Err("Android native owner endpoint returned an empty response".to_owned());
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| "Android native owner response is malformed".to_owned())
}

async fn call_native_empty<R: DeserializeOwned>(
    app: &AppHandle,
    operation: AppAndroidOwnerNativeOperation,
    path: &'static str,
) -> Result<R, String> {
    let authorization = mint_native_authorization(app, operation, None).await?;
    let bytes = serde_json::to_vec(&AppAndroidOwnerNativeEmptyRequest { authorization })
        .map_err(|_| "Android native request could not be encoded".to_owned())?;
    post_native_json(app, path, bytes).await
}

async fn call_native_body<B: Serialize, R: DeserializeOwned>(
    app: &AppHandle,
    operation: AppAndroidOwnerNativeOperation,
    path: &'static str,
    body: B,
) -> Result<R, String> {
    let body_digest = app_android_owner_native_body_digest(operation, &body)
        .map_err(|_| "Android native body digest could not be constructed".to_owned())?;
    let authorization = mint_native_authorization(app, operation, Some(body_digest)).await?;
    let bytes = serde_json::to_vec(&AppAndroidOwnerNativeEnvelope {
        authorization,
        body,
    })
    .map_err(|_| "Android native request could not be encoded".to_owned())?;
    post_native_json(app, path, bytes).await
}

fn validate_native_receipt_ack(
    status: &AppAndroidOwnerNativeControlStatus,
    receipt_digest: &str,
    owner_generation: u64,
    target_ref: &str,
    review_generation: u64,
) -> Result<(), String> {
    if status.owner_generation != owner_generation
        || status.latest_receipt_digest.as_deref() != Some(receipt_digest)
        || status.accepted_receipt_digest.as_deref() != Some(receipt_digest)
        || status.target_ref.as_deref() != Some(target_ref)
        || status.review_generation != Some(review_generation)
    {
        return Err(
            "Android owner runtime returned a mismatched receipt acknowledgment".to_owned(),
        );
    }
    Ok(())
}

fn validate_native_recovery_ack(
    status: &AppAndroidOwnerNativeControlStatus,
    snapshot_digest: &str,
    owner_generation: u64,
    latest_receipt_digest: &Option<String>,
) -> Result<(), String> {
    if status.owner_generation != owner_generation
        || &status.latest_receipt_digest != latest_receipt_digest
        || status.accepted_receipt_digest.as_deref() != Some(snapshot_digest)
        || status.target_ref.is_some()
        || status.review_generation.is_some()
    {
        return Err(
            "Android owner runtime returned a mismatched recovery acknowledgment".to_owned(),
        );
    }
    Ok(())
}

/// Start the only bootstrap transport for Android Apps authority. The socket
/// is unavailable on non-macOS targets by construction; ordinary desktop and
/// runtime features continue without silently falling back to TCP trust-on-
/// first-use.
pub(crate) async fn start_bootstrap_socket_listener(app: AppHandle) -> Result<(), String> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        return Err("Android owner bootstrap requires a signed macOS desktop".to_owned());
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _, PermissionsExt as _};

        // Fail once at desktop boot instead of exposing a socket that can never
        // authenticate a runtime. The Magician client deliberately retries a
        // missing socket so a signed desktop may appear later; binding from an
        // ad-hoc desktop turned that useful retry into one rejection warning
        // every two seconds.
        tokio::task::spawn_blocking(verified_android_bootstrap_desktop_team_id)
            .await
            .map_err(|_| "desktop signing preflight was interrupted".to_owned())??;
        with_owner(&app, |_, _| Ok(())).await?;
        let support_root = dirs::data_dir()
            .filter(|root| root.is_absolute())
            .ok_or_else(|| "Android bootstrap support root is unavailable".to_owned())?
            .join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_SUPPORT_DIRECTORY);
        let owner_root = ensure_private_directory(
            &support_root,
            APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_OWNER_DIRECTORY,
        )?;
        let socket_path = owner_root.join(APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME);
        if let Ok(metadata) = std::fs::symlink_metadata(&socket_path) {
            if !metadata.file_type().is_socket()
                || metadata.file_type().is_symlink()
                || metadata.uid() != unsafe { libc::geteuid() }
            {
                return Err("Android owner bootstrap socket path is unsafe".to_owned());
            }
            std::fs::remove_file(&socket_path).map_err(|error| {
                format!("could not remove stale Android bootstrap socket: {error}")
            })?;
        }
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .map_err(|error| format!("could not bind Android bootstrap socket: {error}"))?;
        std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o600)).map_err(
            |_| "Android bootstrap socket permissions could not be restricted".to_owned(),
        )?;
        let metadata = std::fs::symlink_metadata(&socket_path)
            .map_err(|_| "Android bootstrap socket could not be inspected".to_owned())?;
        if !metadata.file_type().is_socket()
            || metadata.file_type().is_symlink()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || std::fs::canonicalize(&owner_root).ok().as_deref() != Some(owner_root.as_path())
        {
            return Err("Android bootstrap socket identity is unsafe".to_owned());
        }
        let parent_metadata = std::fs::symlink_metadata(&owner_root)
            .map_err(|_| "Android bootstrap socket parent could not be inspected".to_owned())?;
        let socket_identity = AppAndroidBootstrapSocketIdentity {
            path: socket_path.clone(),
            parent_device: parent_metadata.dev(),
            parent_inode: parent_metadata.ino(),
            socket_device: metadata.dev(),
            socket_inode: metadata.ino(),
        };
        validate_android_bootstrap_socket_identity(&socket_identity)?;
        let guard = AppAndroidBootstrapSocketGuard(socket_identity.clone());
        tracing::info!(path = %socket_path.display(), "Android owner bootstrap socket is listening");
        tauri::async_runtime::spawn(async move {
            let _socket_guard = guard;
            loop {
                let (stream, _) = match listener.accept().await {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::warn!(error = %error, "Android bootstrap socket stopped");
                        break;
                    },
                };
                if let Err(error) = handle_android_bootstrap_socket_connection(
                    &app,
                    stream,
                    socket_identity.clone(),
                )
                .await
                {
                    tracing::warn!(error = %error, "Android bootstrap socket rejected a peer");
                }
            }
        });
        Ok(())
    }
}

#[cfg(target_os = "macos")]
#[derive(Clone)]
struct AppAndroidBootstrapSocketIdentity {
    path: PathBuf,
    parent_device: u64,
    parent_inode: u64,
    socket_device: u64,
    socket_inode: u64,
}

#[cfg(target_os = "macos")]
struct AppAndroidBootstrapSocketGuard(AppAndroidBootstrapSocketIdentity);

#[cfg(target_os = "macos")]
impl Drop for AppAndroidBootstrapSocketGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

        let removable = std::fs::symlink_metadata(&self.0.path).is_ok_and(|metadata| {
            metadata.file_type().is_socket()
                && !metadata.file_type().is_symlink()
                && metadata.dev() == self.0.socket_device
                && metadata.ino() == self.0.socket_inode
        });
        if removable {
            let _ = std::fs::remove_file(&self.0.path);
        }
    }
}

#[cfg(target_os = "macos")]
fn validate_android_bootstrap_socket_identity(
    expected: &AppAndroidBootstrapSocketIdentity,
) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt as _, MetadataExt as _};

    let parent = expected
        .path
        .parent()
        .ok_or_else(|| "Android bootstrap socket parent is absent".to_owned())?;
    let parent_metadata = std::fs::symlink_metadata(parent)
        .map_err(|_| "Android bootstrap socket parent is unavailable".to_owned())?;
    let socket_metadata = std::fs::symlink_metadata(&expected.path)
        .map_err(|_| "Android bootstrap socket path is unavailable".to_owned())?;
    let uid = unsafe { libc::geteuid() };
    if !parent_metadata.is_dir()
        || parent_metadata.file_type().is_symlink()
        || parent_metadata.uid() != uid
        || parent_metadata.mode() & 0o777 != 0o700
        || parent_metadata.dev() != expected.parent_device
        || parent_metadata.ino() != expected.parent_inode
        || !socket_metadata.file_type().is_socket()
        || socket_metadata.file_type().is_symlink()
        || socket_metadata.uid() != uid
        || socket_metadata.mode() & 0o777 != 0o600
        || socket_metadata.nlink() != 1
        || socket_metadata.dev() != expected.socket_device
        || socket_metadata.ino() != expected.socket_inode
    {
        return Err("Android bootstrap socket identity changed".to_owned());
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn verify_android_bootstrap_runtime_peer(
    stream: &tokio::net::UnixStream,
    socket_identity: AppAndroidBootstrapSocketIdentity,
) -> Result<libc::pid_t, String> {
    use std::os::fd::{AsRawFd as _, FromRawFd as _};

    let duplicated = unsafe { libc::dup(stream.as_raw_fd()) };
    if duplicated < 0 {
        return Err("Android bootstrap runtime descriptor could not be pinned".to_owned());
    }
    let descriptor = unsafe { std::os::fd::OwnedFd::from_raw_fd(duplicated) };
    let permit = tokio::time::timeout(
        APP_ANDROID_OWNER_OPERATION_TIMEOUT,
        Arc::clone(android_owner_blocking_slots()).acquire_owned(),
    )
    .await
    .map_err(|_| "Android bootstrap peer verification capacity timed out".to_owned())?
    .map_err(|_| "Android bootstrap peer verification capacity is closed".to_owned())?;
    let verification = tokio::task::spawn_blocking(move || {
        let _blocking_permit = permit;
        verify_android_bootstrap_runtime_code(descriptor, &socket_identity)
    });
    tokio::time::timeout(APP_ANDROID_OWNER_PEER_CODE_VERIFY_TIMEOUT, verification)
        .await
        .map_err(|_| "Android bootstrap peer code verification timed out".to_owned())?
        .map_err(|_| "Android bootstrap peer code verification was interrupted".to_owned())?
}

#[cfg(target_os = "macos")]
fn verify_android_bootstrap_runtime_code(
    descriptor: std::os::fd::OwnedFd,
    socket_identity: &AppAndroidBootstrapSocketIdentity,
) -> Result<libc::pid_t, String> {
    use core_foundation::base::TCFType as _;
    use core_foundation::data::CFData;
    use security_framework::os::macos::code_signing::{
        Flags, GuestAttributes, SecCode, SecRequirement, SecStaticCode,
    };
    use std::os::fd::AsRawFd as _;
    use std::str::FromStr as _;

    let mut peer_uid: libc::uid_t = 0;
    let mut peer_gid: libc::gid_t = 0;
    let peer_result =
        unsafe { libc::getpeereid(descriptor.as_raw_fd(), &mut peer_uid, &mut peer_gid) };
    if peer_result != 0 || peer_uid != unsafe { libc::geteuid() } {
        return Err("Android bootstrap runtime peer has another OS owner".to_owned());
    }
    let mut peer_pid: libc::pid_t = 0;
    let mut peer_pid_length = std::mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let pid_result = unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            (&mut peer_pid as *mut libc::pid_t).cast(),
            &mut peer_pid_length,
        )
    };
    if pid_result != 0
        || peer_pid_length as usize != std::mem::size_of::<libc::pid_t>()
        || peer_pid <= 1
        || peer_pid == unsafe { libc::getpid() }
    {
        return Err("Android bootstrap runtime peer PID is unavailable".to_owned());
    }
    let mut audit_token = [0_u8; 32];
    let mut audit_token_length = audit_token.len() as libc::socklen_t;
    let token_result = unsafe {
        libc::getsockopt(
            descriptor.as_raw_fd(),
            libc::SOL_LOCAL,
            libc::LOCAL_PEERTOKEN,
            audit_token.as_mut_ptr().cast(),
            &mut audit_token_length,
        )
    };
    if token_result != 0 || audit_token_length as usize != audit_token.len() {
        return Err("Android bootstrap runtime audit token is unavailable".to_owned());
    }
    let token_word = |index: usize| {
        let offset = index * std::mem::size_of::<u32>();
        u32::from_ne_bytes(
            audit_token[offset..offset + std::mem::size_of::<u32>()]
                .try_into()
                .expect("fixed audit-token word"),
        )
    };
    if token_word(1) != peer_uid || token_word(5) != peer_pid as u32 {
        return Err("Android bootstrap runtime audit token changed peer identity".to_owned());
    }
    let team_id = verified_android_bootstrap_desktop_team_id()?;
    let runtime_requirement = SecRequirement::from_str(&format!(
        "identifier \"{}\" and anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
        APP_ANDROID_OWNER_RUNTIME_CODE_IDENTIFIER, team_id,
    ))
    .map_err(|_| "runtime signing requirement is invalid".to_owned())?;
    // Magician is staged as one standalone Mach-O, not an app bundle.
    // CHECK_NESTED_CODE is only valid for bundle-form code and makes
    // SecCodeCheckValidity reject this otherwise valid signed peer with
    // errSecCSInvalidFlags. Strict live/static validation still verifies the
    // complete executable against the exact identifier, Apple anchor and
    // desktop signing team below.
    let flags = Flags::STRICT_VALIDATE | Flags::NO_NETWORK_ACCESS;
    let token_data = CFData::from_buffer(&audit_token);
    let mut attributes = GuestAttributes::new();
    attributes.set_audit_token(token_data.as_concrete_TypeRef());
    let guest = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
        .map_err(|_| "runtime live code identity is unavailable".to_owned())?;
    guest
        .check_validity(flags, &runtime_requirement)
        .map_err(|error| {
            format!("runtime live code identity is not approved (pid={peer_pid}, status={error})")
        })?;
    if android_bootstrap_signing_team_id(&guest)? != team_id {
        return Err("runtime signing team changed".to_owned());
    }
    let guest_url = guest
        .path(Flags::NONE)
        .map_err(|_| "runtime executable path is unavailable".to_owned())?;
    let static_code = SecStaticCode::from_path(&guest_url, Flags::NONE)
        .map_err(|_| "runtime static code identity is unavailable".to_owned())?;
    static_code
        .check_validity(flags, &runtime_requirement)
        .map_err(|error| {
            format!("runtime static code identity is not approved (pid={peer_pid}, status={error})")
        })?;
    validate_android_bootstrap_socket_identity(socket_identity)?;
    let _ = peer_gid;
    Ok(peer_pid)
}

#[cfg(target_os = "macos")]
fn verified_android_bootstrap_desktop_team_id() -> Result<String, String> {
    use security_framework::os::macos::code_signing::{Flags, SecCode, SecRequirement};
    use std::str::FromStr as _;

    let self_code = SecCode::for_self(Flags::NONE)
        .map_err(|_| "desktop signing identity is unavailable".to_owned())?;
    let team_id = android_bootstrap_signing_team_id(&self_code)?;
    if team_id.is_empty()
        || team_id.len() > 32
        || !team_id.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("desktop signing team is unavailable".to_owned());
    }
    let team_requirement = SecRequirement::from_str(&format!(
        "anchor apple generic and certificate leaf[subject.OU] = \"{}\"",
        team_id,
    ))
    .map_err(|_| "desktop signing requirement is invalid".to_owned())?;
    self_code
        .check_validity(Flags::NO_NETWORK_ACCESS, &team_requirement)
        .map_err(|_| "desktop signing identity is not production-valid".to_owned())?;
    Ok(team_id)
}

#[cfg(target_os = "macos")]
fn android_bootstrap_signing_team_id(
    code: &security_framework::os::macos::code_signing::SecCode,
) -> Result<String, String> {
    use core_foundation::base::TCFType as _;
    use core_foundation::string::CFString;
    use core_foundation_sys::base::{CFGetTypeID, CFRelease, CFTypeRef};
    use core_foundation_sys::dictionary::{CFDictionaryGetValue, CFDictionaryRef};
    use core_foundation_sys::string::{CFStringGetTypeID, CFStringRef};

    const SIGNING_INFORMATION: u32 = 1 << 1;
    extern "C" {
        static kSecCodeInfoTeamIdentifier: CFStringRef;
        fn SecCodeCopySigningInformation(
            code: security_framework_sys::code_signing::SecCodeRef,
            flags: u32,
            information: *mut CFDictionaryRef,
        ) -> i32;
    }

    let mut information: CFDictionaryRef = std::ptr::null();
    let status = unsafe {
        SecCodeCopySigningInformation(
            code.as_concrete_TypeRef(),
            SIGNING_INFORMATION,
            &mut information,
        )
    };
    if status != 0 || information.is_null() {
        return Err("code signing information is unavailable".to_owned());
    }
    let value = unsafe {
        CFDictionaryGetValue(
            information,
            kSecCodeInfoTeamIdentifier.cast::<std::ffi::c_void>(),
        )
    };
    if value.is_null()
        || unsafe { CFGetTypeID(value as CFTypeRef) } != unsafe { CFStringGetTypeID() }
    {
        unsafe { CFRelease(information as CFTypeRef) };
        return Err("code signing team is unavailable".to_owned());
    }
    let team = unsafe { CFString::wrap_under_get_rule(value as CFStringRef) }.to_string();
    unsafe { CFRelease(information as CFTypeRef) };
    Ok(team)
}

#[cfg(target_os = "macos")]
async fn handle_android_bootstrap_socket_connection(
    app: &AppHandle,
    mut stream: tokio::net::UnixStream,
    socket_identity: AppAndroidBootstrapSocketIdentity,
) -> Result<(), String> {
    let peer_pid = verify_android_bootstrap_runtime_peer(&stream, socket_identity).await?;
    let _control = reserve_android_owner_native_control().await?;
    // Do not let the runtime recover or mint its 30-second correlation until
    // the connected runtime has passed audit-token/live/static verification
    // and this handler owns the serialized native-control admission slot.
    // Both potentially slow boundaries therefore precede the nonce lifetime.
    write_android_bootstrap_frame(
        &mut stream,
        &AppAndroidOwnerBootstrapDesktopMessage::peer_verified(),
    )
    .await?;
    let first: AppAndroidOwnerBootstrapRuntimeMessage =
        read_android_bootstrap_frame(&mut stream).await?;
    match first {
        AppAndroidOwnerBootstrapRuntimeMessage::Hello { schema, bootstrap } => {
            if schema != APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_V1 {
                return Err("Android bootstrap hello schema is unsupported".to_owned());
            }
            let desktop_message = with_owner(app, move |owner, now_ms| {
                owner.accept_runtime_hello(bootstrap, now_ms)
            })
            .await?;
            continue_android_bootstrap_exchange(app, &mut stream, desktop_message).await?;
        },
        message @ AppAndroidOwnerBootstrapRuntimeMessage::RecoveryHello { .. } => {
            message
                .validate(current_unix_ms()?)
                .map_err(|_| "Android bootstrap recovery hello is malformed".to_owned())?;
            let AppAndroidOwnerBootstrapRuntimeMessage::RecoveryHello { recovery, .. } = message
            else {
                unreachable!("matched recovery hello")
            };
            let desktop_message = with_owner(app, move |owner, now_ms| {
                owner.accept_runtime_recovery_hello(recovery, now_ms)
            })
            .await?;
            continue_android_bootstrap_exchange(app, &mut stream, desktop_message).await?;
        },
        message @ AppAndroidOwnerBootstrapRuntimeMessage::Finalized { .. } => {
            message
                .validate(current_unix_ms()?)
                .map_err(|_| "Android bootstrap finalized status is malformed".to_owned())?;
            let AppAndroidOwnerBootstrapRuntimeMessage::Finalized { status, .. } = message else {
                unreachable!("matched finalized message")
            };
            let acknowledgment = with_owner(app, move |owner, now_ms| {
                owner.accept_runtime_finalized(&status, now_ms)
            })
            .await?;
            write_android_bootstrap_frame(&mut stream, &acknowledgment).await?;
        },
        AppAndroidOwnerBootstrapRuntimeMessage::Challenge { .. } => {
            return Err("Android bootstrap challenge has no owned hello".to_owned());
        },
    }
    tracing::debug!(
        peer_pid,
        "Android owner bootstrap socket exchange completed"
    );
    Ok(())
}

#[cfg(target_os = "macos")]
async fn continue_android_bootstrap_exchange(
    app: &AppHandle,
    stream: &mut tokio::net::UnixStream,
    desktop_message: AppAndroidOwnerBootstrapDesktopMessage,
) -> Result<(), String> {
    let needs_challenge = matches!(
        &desktop_message,
        AppAndroidOwnerBootstrapDesktopMessage::ChallengeRequest { .. }
    );
    let needs_finalized = matches!(
        &desktop_message,
        AppAndroidOwnerBootstrapDesktopMessage::Completion { .. }
    );
    write_android_bootstrap_frame(stream, &desktop_message).await?;
    if needs_challenge {
        let message: AppAndroidOwnerBootstrapRuntimeMessage =
            read_android_bootstrap_frame(stream).await?;
        message
            .validate(current_unix_ms()?)
            .map_err(|_| "Android bootstrap runtime challenge is malformed".to_owned())?;
        let AppAndroidOwnerBootstrapRuntimeMessage::Challenge { challenge, .. } = message else {
            return Err("Android bootstrap runtime omitted the canonical challenge".to_owned());
        };
        let request = with_owner(app, {
            let challenge = challenge.clone();
            move |owner, now_ms| owner.accept_runtime_challenge(&challenge, now_ms)
        })
        .await?;
        let attestation =
            crate::host_gateway::attest_app_android_desktop_identity(app, &challenge).await?;
        let completion = AppAndroidOwnerBootstrapCompletion {
            challenge,
            attestation,
        };
        with_owner(app, {
            let request = request.clone();
            let completion = completion.clone();
            move |owner, now_ms| owner.retain_bootstrap_completion(&request, completion, now_ms)
        })
        .await?;
        write_android_bootstrap_frame(
            stream,
            &AppAndroidOwnerBootstrapDesktopMessage::completion(completion),
        )
        .await?;
        accept_android_bootstrap_finalized(app, stream).await?;
    } else if needs_finalized {
        accept_android_bootstrap_finalized(app, stream).await?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
async fn accept_android_bootstrap_finalized(
    app: &AppHandle,
    stream: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let message: AppAndroidOwnerBootstrapRuntimeMessage =
        read_android_bootstrap_frame(stream).await?;
    message
        .validate(current_unix_ms()?)
        .map_err(|_| "Android bootstrap finalized status is malformed".to_owned())?;
    let AppAndroidOwnerBootstrapRuntimeMessage::Finalized { status, .. } = message else {
        return Err("Android bootstrap runtime omitted the finalized acknowledgment".to_owned());
    };
    status
        .validate()
        .map_err(|_| "Android bootstrap finalized status is malformed".to_owned())?;
    let acknowledgment = with_owner(app, move |owner, now_ms| {
        owner.accept_runtime_finalized(&status, now_ms)
    })
    .await?;
    write_android_bootstrap_frame(stream, &acknowledgment).await
}

#[cfg(target_os = "macos")]
async fn read_android_bootstrap_frame<T: DeserializeOwned>(
    stream: &mut tokio::net::UnixStream,
) -> Result<T, String> {
    use tokio::io::AsyncReadExt as _;

    tokio::time::timeout(APP_ANDROID_OWNER_BOOTSTRAP_FRAME_TIMEOUT, async {
        let mut length_bytes = [0_u8; 4];
        stream
            .read_exact(&mut length_bytes)
            .await
            .map_err(|_| "Android bootstrap frame header is unavailable".to_owned())?;
        let length = u32::from_be_bytes(length_bytes) as usize;
        if length == 0 || length > APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES {
            return Err("Android bootstrap frame exceeds its byte ceiling".to_owned());
        }
        let mut bytes = vec![0_u8; length];
        stream
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "Android bootstrap frame body is unavailable".to_owned())?;
        serde_json::from_slice(&bytes)
            .map_err(|_| "Android bootstrap frame is malformed".to_owned())
    })
    .await
    .map_err(|_| "Android bootstrap frame read timed out".to_owned())?
}

#[cfg(target_os = "macos")]
async fn write_android_bootstrap_frame<T: Serialize>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> Result<(), String> {
    use tokio::io::AsyncWriteExt as _;

    let bytes = serde_json::to_vec(value)
        .map_err(|_| "Android bootstrap response could not be encoded".to_owned())?;
    if bytes.is_empty() || bytes.len() > APP_ANDROID_OWNER_BOOTSTRAP_MAX_FRAME_BYTES {
        return Err("Android bootstrap response exceeds its byte ceiling".to_owned());
    }
    let length = u32::try_from(bytes.len())
        .map_err(|_| "Android bootstrap response length is out of range".to_owned())?;
    tokio::time::timeout(APP_ANDROID_OWNER_BOOTSTRAP_FRAME_TIMEOUT, async {
        stream
            .write_all(&length.to_be_bytes())
            .await
            .map_err(|_| "Android bootstrap response header could not be written".to_owned())?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| "Android bootstrap response body could not be written".to_owned())?;
        stream
            .flush()
            .await
            .map_err(|_| "Android bootstrap response could not be flushed".to_owned())
    })
    .await
    .map_err(|_| "Android bootstrap response write timed out".to_owned())?
}

#[tauri::command]
pub(crate) async fn get_app_android_authority_status(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<AppAndroidOwnerNativeStatus, String> {
    ensure_trusted_settings_window(&window)?;
    with_owner(&app, |owner, now_ms| {
        owner.discard_expired_unconfirmed_bootstrap(now_ms);
        owner.status()
    })
    .await
}

#[tauri::command]
pub(crate) async fn begin_app_android_authority_identity_bootstrap(
    app: AppHandle,
    window: WebviewWindow,
    expected_rebind_display_digest: Option<String>,
) -> Result<AppAndroidOwnerBootstrapEnrollment, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    let (bootstrap, rebind_display_digest) = with_owner(&app, |owner, now_ms| {
        owner.discard_expired_unconfirmed_bootstrap(now_ms);
        owner.offered_bootstrap(now_ms)
    })
    .await?;
    if expected_rebind_display_digest != rebind_display_digest {
        return Err("Android owner bootstrap does not match the displayed rebind".to_owned());
    }
    let enrollment = crate::host_gateway::begin_app_android_desktop_identity_approval(&app).await?;
    let owner_approval_code_digest =
        app_macos_desktop_owner_approval_code_digest(&enrollment.owner_approval_code)
            .map_err(|_| "Android owner approval code is invalid".to_owned())?;
    let request = AppAndroidOwnerBootstrapChallengeRequest {
        bootstrap: bootstrap.clone(),
        desktop_identity_key_id: enrollment.desktop_identity_key_id.clone(),
        desktop_identity_public_key_hex: enrollment.desktop_identity_public_key_hex.clone(),
        desktop_identity_digest: enrollment.desktop_identity_fingerprint.clone(),
        owner_approval_code_digest,
    };
    let now_ms = current_unix_ms()?;
    request
        .validate(now_ms)
        .map_err(|_| "Android owner bootstrap identity is invalid".to_owned())?;
    let fingerprint = enrollment.desktop_identity_fingerprint.clone();
    with_owner(&app, move |owner, now_ms| {
        owner.stage_bootstrap(request, fingerprint, expected_rebind_display_digest, now_ms)
    })
    .await?;
    let expires_at_ms = enrollment.expires_at_ms.min(bootstrap.expires_at_ms);
    Ok(AppAndroidOwnerBootstrapEnrollment {
        bootstrap,
        desktop_identity_key_id: enrollment.desktop_identity_key_id.clone(),
        desktop_identity_fingerprint: enrollment.desktop_identity_fingerprint.clone(),
        owner_approval_code: enrollment.owner_approval_code.clone(),
        expires_at_ms,
        rebind_display_digest,
    })
}

#[tauri::command]
pub(crate) async fn complete_app_android_authority_identity_bootstrap(
    app: AppHandle,
    window: WebviewWindow,
    expected_bootstrap_nonce: String,
    expected_desktop_identity_fingerprint: String,
    expected_rebind_display_digest: Option<String>,
) -> Result<AppAndroidOwnerNativeStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    if !valid_owner_token(&expected_bootstrap_nonce, 192)
        || !valid_blake3_digest(&expected_desktop_identity_fingerprint)
    {
        return Err("Android owner bootstrap confirmation is malformed".to_owned());
    }
    with_owner(&app, {
        let nonce = expected_bootstrap_nonce.clone();
        let fingerprint = expected_desktop_identity_fingerprint.clone();
        move |owner, now_ms| {
            owner.arm_bootstrap(
                &nonce,
                &fingerprint,
                expected_rebind_display_digest.as_deref(),
                now_ms,
            )?;
            owner.status()
        }
    })
    .await
}

#[tauri::command]
pub(crate) async fn begin_app_android_apps_enrollment(
    app: AppHandle,
    window: WebviewWindow,
    trust_mode: AppAndroidAutomationTrustMode,
    connection_mode: AppAndroidEnrollmentConnectionMode,
) -> Result<AppAndroidOwnerNativeEnrollmentResponse, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    call_native_body(
        &app,
        AppAndroidOwnerNativeOperation::BeginEnrollment,
        "/native/enrollment/begin",
        AppAndroidOwnerNativeBeginEnrollmentRequest {
            trust_mode,
            connection_mode,
        },
    )
    .await
}

#[tauri::command]
pub(crate) async fn cancel_app_android_apps_enrollment(
    app: AppHandle,
    window: WebviewWindow,
    enrollment_id: String,
) -> Result<AppAndroidOwnerNativeCancelEnrollmentResponse, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    if !valid_owner_token(&enrollment_id, 192) {
        return Err("Android enrollment identity is invalid".to_owned());
    }
    call_native_body(
        &app,
        AppAndroidOwnerNativeOperation::CancelEnrollment,
        "/native/enrollment/cancel",
        AppAndroidOwnerNativeCancelEnrollmentRequest { enrollment_id },
    )
    .await
}

#[tauri::command]
pub(crate) async fn list_app_android_authority_targets(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<AppAndroidOwnerNativeTargetsResponse, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    call_native_empty(
        &app,
        AppAndroidOwnerNativeOperation::ListTargets,
        "/native/targets",
    )
    .await
}

#[tauri::command]
pub(crate) async fn propose_app_android_authority_review(
    app: AppHandle,
    window: WebviewWindow,
    body: AppAndroidOwnerNativeProposeReviewRequest,
) -> Result<AppAndroidOwnerNativeStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    if body.target_ref.is_empty()
        || body.target_ref.len() > 192
        || !body.target_ref.starts_with("android-device:")
        || body.operation == AppAndroidOwnerOperation::EnrollAttestedDevice
        || body.operation == AppAndroidOwnerOperation::RevokeDevice
        || body.allowed_packages.len() > APP_ANDROID_OWNER_MAX_PACKAGES
        || !body
            .allowed_packages
            .windows(2)
            .all(|values| values[0] < values[1])
        || body
            .allowed_packages
            .iter()
            .any(|value| !valid_android_package(value))
    {
        return Err("Android review request exceeds its field ceilings".to_owned());
    }
    let proposal: AppAndroidOwnerProposal = call_native_body(
        &app,
        AppAndroidOwnerNativeOperation::ProposeReview,
        "/native/review/propose",
        body,
    )
    .await?;
    with_owner(&app, move |owner, now_ms| {
        owner.stage(proposal, now_ms)?;
        owner.status()
    })
    .await
}

#[tauri::command]
pub(crate) async fn refresh_app_android_authority_pending(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<AppAndroidOwnerNativeStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    let pending: AppAndroidOwnerNativePendingResponse = call_native_empty(
        &app,
        AppAndroidOwnerNativeOperation::ListPending,
        "/native/pending",
    )
    .await?;
    if pending.proposals.len() > 1 {
        return Err("runtime returned multiple competing Android owner proposals".to_owned());
    }
    let proposal = pending.proposals.into_iter().next();
    with_owner(&app, move |owner, now_ms| {
        owner.discard_expired_unconfirmed_bootstrap(now_ms);
        if let Some(proposal) = proposal {
            owner.stage(proposal, now_ms)?;
        }
        owner.status()
    })
    .await
}

#[tauri::command]
pub(crate) async fn confirm_app_android_authority_proposal(
    app: AppHandle,
    window: WebviewWindow,
    proposal_id: String,
    expected_owner_generation: u64,
    expected_display_digest: String,
) -> Result<AppAndroidOwnerNativeControlStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    if !valid_owner_token(&proposal_id, 192)
        || !valid_blake3_digest(&expected_display_digest)
        || expected_owner_generation == 0
    {
        return Err("Android owner confirmation is malformed".to_owned());
    }
    let receipt = with_owner(&app, move |owner, now_ms| {
        owner.confirm(
            &proposal_id,
            expected_owner_generation,
            &expected_display_digest,
            now_ms,
        )
    })
    .await?;
    let receipt_digest = receipt
        .digest()
        .map_err(|_| "Android owner receipt digest is invalid".to_owned())?;
    let expected_owner_generation = receipt.owner_generation;
    let expected_target_ref = receipt.target_ref.clone();
    let expected_review_generation = receipt.resulting_review_generation;
    let status: AppAndroidOwnerNativeControlStatus = call_native_body(
        &app,
        AppAndroidOwnerNativeOperation::SubmitReceipt,
        "/native/receipt",
        receipt,
    )
    .await?;
    validate_native_receipt_ack(
        &status,
        &receipt_digest,
        expected_owner_generation,
        &expected_target_ref,
        expected_review_generation,
    )?;
    Ok(status)
}

#[tauri::command]
pub(crate) async fn begin_app_android_authority_recovery(
    app: AppHandle,
    window: WebviewWindow,
) -> Result<AppAndroidOwnerNativeStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    let challenge: AppAndroidOwnerRecoveryChallenge = call_native_empty(
        &app,
        AppAndroidOwnerNativeOperation::BeginRecovery,
        "/native/recovery/challenge",
    )
    .await?;
    with_owner(&app, move |owner, now_ms| {
        owner.stage_recovery(challenge, now_ms)?;
        owner.status()
    })
    .await
}

#[tauri::command]
pub(crate) async fn confirm_app_android_authority_recovery(
    app: AppHandle,
    window: WebviewWindow,
    recovery_nonce: String,
    expected_display_digest: String,
) -> Result<AppAndroidOwnerNativeControlStatus, String> {
    ensure_trusted_settings_window(&window)?;
    let _control = reserve_android_owner_native_control().await?;
    if !valid_owner_token(&recovery_nonce, 192) || !valid_blake3_digest(&expected_display_digest) {
        return Err("Android owner recovery confirmation is malformed".to_owned());
    }
    let snapshot = with_owner(&app, move |owner, now_ms| {
        owner.confirm_recovery(&recovery_nonce, &expected_display_digest, now_ms)
    })
    .await?;
    let snapshot_digest = snapshot
        .digest()
        .map_err(|_| "Android owner recovery digest is invalid".to_owned())?;
    let expected_owner_generation = snapshot.owner_generation;
    let expected_latest_receipt_digest = snapshot.latest_receipt_digest.clone();
    let status: AppAndroidOwnerNativeControlStatus = call_native_body(
        &app,
        AppAndroidOwnerNativeOperation::SubmitRecovery,
        "/native/recovery",
        snapshot,
    )
    .await?;
    validate_native_recovery_ack(
        &status,
        &snapshot_digest,
        expected_owner_generation,
        &expected_latest_receipt_digest,
    )?;
    with_owner(&app, move |owner, _| {
        owner.complete_recovery_submission(&snapshot_digest)
    })
    .await?;
    Ok(status)
}

/// The loopback gateway may request only a record-free, nonce-bound high-water
/// snapshot. Full recovery records are available exclusively through the
/// trusted Settings commands above.
pub(crate) async fn signed_status_for_gateway(
    app: &AppHandle,
    challenge: AppAndroidOwnerStatusChallenge,
) -> Result<AppAndroidOwnerStatusReceipt, String> {
    with_owner(app, move |owner, now_ms| {
        owner.signed_status(&challenge, now_ms)
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician_app_contract::android_owner::{
        AppAndroidAttestationSecurityLevel, APP_ANDROID_OWNER_ACTION_ROSTER,
    };
    use magician_app_contract::macos_host::AppMacosDesktopIdentityAttestation;
    use ring::rand::SystemRandom;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn identity() -> (Ed25519KeyPair, String, String, String) {
        let pkcs8 =
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("test desktop identity");
        let pair = Ed25519KeyPair::from_pkcs8(pkcs8.as_ref()).expect("test key pair");
        let public = hex(pair.public_key().as_ref());
        let key_id = format!(
            "desktop-identity:{}",
            blake3::hash(public.as_bytes()).to_hex()
        );
        let digest =
            magician_app_contract::macos_host::app_macos_desktop_identity_digest(&key_id, &public)
                .expect("desktop digest");
        (pair, key_id, public, digest)
    }

    fn digest(byte: u8) -> String {
        format!("blake3:{}", hex(&[byte; 32]))
    }

    #[test]
    fn owner_directory_creates_a_missing_app_data_root() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let app_data_root = temporary.path().join("nested").join("app-data");

        let owner_root =
            ensure_private_owner_directory(&app_data_root).expect("private owner directory");

        assert_eq!(
            owner_root,
            std::fs::canonicalize(app_data_root.join("app-android-owner-v1"))
                .expect("canonical owner directory"),
        );
        let metadata = std::fs::symlink_metadata(&owner_root).expect("owner metadata");
        assert!(metadata.is_dir());
        assert!(!metadata.file_type().is_symlink());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            assert_eq!(metadata.permissions().mode() & 0o077, 0);
        }
    }

    fn enrollment_proposal(
        owner_generation: u64,
        previous_receipt_digest: Option<String>,
        desktop_identity_digest: String,
        now_ms: i64,
    ) -> AppAndroidOwnerProposal {
        AppAndroidOwnerProposal::mint(
            format!("proposal:{owner_generation}"),
            owner_generation,
            previous_receipt_digest,
            desktop_identity_digest,
            "principal-a".to_owned(),
            "workspace-a".to_owned(),
            AppAndroidOwnerOperation::EnrollAttestedDevice,
            Some("enrollment-a".to_owned()),
            "android-device:opaque-a".to_owned(),
            "Reviewed handset".to_owned(),
            "device-key:a".to_owned(),
            digest(1),
            "ai.magicbeans.magdroid".to_owned(),
            11,
            hex(&[2; 32]),
            hex(&[3; 32]),
            hex(&[4; 32]),
            AppAndroidAttestationSecurityLevel::Strongbox,
            digest(5),
            0,
            0,
            Vec::new(),
            Vec::new(),
            now_ms,
            now_ms + 60_000,
        )
        .expect("enrollment proposal")
    }

    fn signed_receipt(
        proposal: &AppAndroidOwnerProposal,
        pair: &Ed25519KeyPair,
        key_id: &str,
        signed_at_ms: i64,
    ) -> AppAndroidOwnerReceipt {
        let mut receipt =
            AppAndroidOwnerReceipt::unsigned(proposal, key_id.to_owned(), signed_at_ms)
                .expect("unsigned receipt");
        receipt.desktop_identity_signature_hex = hex(pair
            .sign(&receipt.signing_bytes().expect("receipt bytes"))
            .as_ref());
        receipt
    }

    #[test]
    fn transition_lineage_rejects_stale_generation_and_cross_scope_identity() {
        let (pair, key_id, public_key, identity_digest) = identity();
        let now_ms = 1_000_000;
        let enrollment = enrollment_proposal(1, None, identity_digest.clone(), now_ms);
        let receipt = signed_receipt(&enrollment, &pair, &key_id, now_ms + 1);
        let previous_digest = receipt.digest().expect("receipt digest");
        let document = PersistedAndroidOwnerStore {
            schema: APP_ANDROID_OWNER_STORE_V1.to_owned(),
            desktop_identity_digest: identity_digest.clone(),
            owner_generation: 1,
            latest_receipt_digest: Some(previous_digest.clone()),
            latest_receipt: Some(receipt.clone()),
            records: vec![receipt],
        };
        let approve = AppAndroidOwnerProposal::mint(
            "proposal:approve".to_owned(),
            2,
            Some(previous_digest.clone()),
            identity_digest.clone(),
            "principal-a".to_owned(),
            "workspace-a".to_owned(),
            AppAndroidOwnerOperation::ApproveActions,
            Some("enrollment-a".to_owned()),
            "android-device:opaque-a".to_owned(),
            "Reviewed handset".to_owned(),
            "device-key:a".to_owned(),
            digest(1),
            "ai.magicbeans.magdroid".to_owned(),
            11,
            hex(&[2; 32]),
            hex(&[3; 32]),
            hex(&[4; 32]),
            AppAndroidAttestationSecurityLevel::Strongbox,
            digest(5),
            0,
            1,
            APP_ANDROID_OWNER_ACTION_ROSTER.to_vec(),
            vec!["com.example.notes".to_owned()],
            now_ms + 2,
            now_ms + 60_000,
        )
        .expect("approval proposal");
        assert!(validate_transition(&document, &approve).is_ok());

        let mut stale = approve.clone();
        stale.expected_review_generation = 1;
        stale.resulting_review_generation = 2;
        assert!(validate_transition(&document, &stale).is_err());
        let mut cross_scope = approve;
        cross_scope.workspace = "workspace-b".to_owned();
        assert!(validate_transition(&document, &cross_scope).is_err());

        let mut owner_app_target = cross_scope;
        owner_app_target.workspace = "workspace-a".to_owned();
        owner_app_target.allowed_packages = vec![APP_ANDROID_OWNER_MAGDROID_PACKAGE.to_owned()];
        assert!(validate_transition(&document, &owner_app_target).is_err());

        let mut device_revoke = owner_app_target;
        device_revoke.allowed_packages.clear();
        device_revoke.actions.clear();
        device_revoke.operation = AppAndroidOwnerOperation::RevokeDevice;
        device_revoke.display_digest = device_revoke
            .recompute_display_digest()
            .expect("device revoke display digest");
        assert!(validate_transition(&document, &device_revoke).is_err());

        let revoke_receipt = signed_receipt(&device_revoke, &pair, &key_id, now_ms + 3);
        let revoke_digest = revoke_receipt
            .digest()
            .expect("device revoke receipt digest");
        let persisted_revoke = PersistedAndroidOwnerStore {
            schema: APP_ANDROID_OWNER_STORE_V1.to_owned(),
            desktop_identity_digest: identity_digest,
            owner_generation: revoke_receipt.owner_generation,
            latest_receipt_digest: Some(revoke_digest),
            latest_receipt: Some(revoke_receipt.clone()),
            records: vec![revoke_receipt],
        };
        assert!(validate_document(&persisted_revoke, &key_id, &public_key, now_ms + 4).is_err());
    }

    #[test]
    fn committed_document_restart_does_not_rewrite_keychain_anchor() {
        let (pair, key_id, public_key, identity_digest) = identity();
        let now_ms = 2_000_000;
        let proposal = enrollment_proposal(1, None, identity_digest.clone(), now_ms);
        let receipt = signed_receipt(&proposal, &pair, &key_id, now_ms + 1);
        let mut document = PersistedAndroidOwnerStore::empty(identity_digest.clone());
        apply_receipt(&mut document, receipt).expect("apply receipt");
        let bytes = encode_document(&document).expect("document bytes");
        let point = AndroidOwnerAnchorPoint {
            generation: 1,
            document_digest: document_digest(&bytes),
        };
        let anchor = AndroidOwnerAnchor {
            schema: APP_ANDROID_OWNER_ANCHOR_V1.to_owned(),
            desktop_identity_digest: identity_digest.clone(),
            committed: Some(point),
            pending: None,
        };
        let placeholder = PathBuf::from("/private/tmp/android-owner-unrun");
        let (recovered, replacement) = recover_document(
            Some(anchor),
            Some(bytes),
            None,
            &placeholder,
            &placeholder,
            &identity_digest,
            &key_id,
            &public_key,
            now_ms + 2,
        )
        .expect("recover committed document");
        assert_eq!(recovered.expect("document").owner_generation, 1);
        assert!(replacement.is_none());
    }

    #[test]
    fn bootstrap_completion_remains_valid_for_exact_post_expiry_replay() {
        let (pair, key_id, public_key, identity_digest) = identity();
        let owner_code = "owner-code-abcdefghijklmnopqrstuvwxyz";
        let code_digest =
            app_macos_desktop_owner_approval_code_digest(owner_code).expect("owner code digest");
        let bootstrap =
            AppAndroidOwnerBootstrapNonce::mint("bootstrap:one".to_owned(), 1_000, 10_000)
                .expect("bootstrap nonce");
        let request = AppAndroidOwnerBootstrapChallengeRequest {
            bootstrap,
            desktop_identity_key_id: key_id.clone(),
            desktop_identity_public_key_hex: public_key.clone(),
            desktop_identity_digest: identity_digest.clone(),
            owner_approval_code_digest: code_digest.clone(),
        };
        let challenge = AppMacosDesktopIdentityChallenge::mint(
            key_id,
            public_key,
            "challenge:one".to_owned(),
            code_digest,
            5_000,
            30_000,
        )
        .expect("identity challenge");
        let mut attestation =
            AppMacosDesktopIdentityAttestation::unsigned(&challenge, digest(9), 20_000)
                .expect("identity attestation");
        attestation.signature_hex = hex(pair
            .sign(&attestation.signing_bytes().expect("attestation bytes"))
            .as_ref());
        let replay = PersistedAndroidOwnerBootstrapReplay {
            schema: APP_ANDROID_OWNER_BOOTSTRAP_REPLAY_V1.to_owned(),
            request,
            desktop_identity_fingerprint: identity_digest.clone(),
            completion: AppAndroidOwnerBootstrapCompletion {
                challenge,
                attestation,
            },
            finalized_status: None,
            rebind_offer: None,
        };
        let bytes = serde_json::to_vec(&replay).expect("bootstrap replay bytes");
        let document = PersistedAndroidOwnerStore::empty(identity_digest.clone());
        assert!(
            decode_and_validate_bootstrap_replay(&bytes, &identity_digest, &document, 90_000,)
                .is_ok()
        );
    }

    #[test]
    fn expired_unspent_bootstrap_is_cleared_without_minting_an_attestation() {
        let (_, key_id, public_key, identity_digest) = identity();
        let bootstrap = AppAndroidOwnerBootstrapNonce::mint(
            "bootstrap:expired-unspent".to_owned(),
            1_000,
            10_000,
        )
        .expect("bootstrap nonce");
        let bootstrap_digest = bootstrap.digest().expect("bootstrap digest");
        let request = AppAndroidOwnerBootstrapChallengeRequest {
            bootstrap: bootstrap.clone(),
            desktop_identity_key_id: key_id,
            desktop_identity_public_key_hex: public_key,
            desktop_identity_digest: identity_digest.clone(),
            owner_approval_code_digest: digest(8),
        };
        let mut owner = AppAndroidAuthorityOwner {
            offered_bootstrap: Some(bootstrap.clone()),
            pending_bootstrap: Some(PendingAndroidOwnerBootstrap {
                request,
                desktop_identity_fingerprint: identity_digest,
                approval_confirmed: true,
                completion: None,
                finalized_status: None,
                rebind_offer: None,
            }),
            ..Default::default()
        };

        let response = owner
            .accept_runtime_hello(bootstrap, 10_001)
            .expect("expired-unspent response");

        assert_eq!(
            response,
            AppAndroidOwnerBootstrapDesktopMessage::expired_unspent(bootstrap_digest)
        );
        assert!(owner.pending_bootstrap.is_none());
        assert!(owner.offered_bootstrap.is_none());
    }

    #[test]
    fn empty_desktop_authority_cannot_emit_a_recovery_snapshot() {
        let (_, _, _, identity_digest) = identity();
        assert!(
            require_nonempty_recovery_head(&PersistedAndroidOwnerStore::empty(identity_digest,))
                .is_err()
        );
    }

    #[test]
    fn unapproved_bootstrap_can_be_reissued_after_settings_reload() {
        let (_, key_id, public_key, identity_digest) = identity();
        let bootstrap = AppAndroidOwnerBootstrapNonce::mint(
            "bootstrap:settings-reload".to_owned(),
            1_000,
            10_000,
        )
        .expect("bootstrap nonce");
        let mut owner = AppAndroidAuthorityOwner {
            offered_bootstrap: Some(bootstrap.clone()),
            ..Default::default()
        };
        let first = AppAndroidOwnerBootstrapChallengeRequest {
            bootstrap: bootstrap.clone(),
            desktop_identity_key_id: key_id.clone(),
            desktop_identity_public_key_hex: public_key.clone(),
            desktop_identity_digest: identity_digest.clone(),
            owner_approval_code_digest: digest(6),
        };
        owner
            .stage_bootstrap(first, identity_digest.clone(), None, 2_000)
            .expect("first native enrollment");
        assert_eq!(
            owner.offered_bootstrap(2_001).expect("retryable nonce"),
            (bootstrap.clone(), None)
        );

        let replacement = AppAndroidOwnerBootstrapChallengeRequest {
            bootstrap,
            desktop_identity_key_id: key_id,
            desktop_identity_public_key_hex: public_key,
            desktop_identity_digest: identity_digest.clone(),
            owner_approval_code_digest: digest(7),
        };
        owner
            .stage_bootstrap(replacement.clone(), identity_digest, None, 2_002)
            .expect("replacement native enrollment");
        assert_eq!(
            owner
                .pending_bootstrap
                .as_ref()
                .expect("replacement pending")
                .request,
            replacement
        );
    }

    #[test]
    fn settings_status_discards_only_expired_unconfirmed_bootstraps() {
        let (_, key_id, public_key, identity_digest) = identity();
        let request = |nonce: &str, approval_confirmed: bool| PendingAndroidOwnerBootstrap {
            request: AppAndroidOwnerBootstrapChallengeRequest {
                bootstrap: AppAndroidOwnerBootstrapNonce::mint(nonce.to_owned(), 1_000, 10_000)
                    .expect("bootstrap nonce"),
                desktop_identity_key_id: key_id.clone(),
                desktop_identity_public_key_hex: public_key.clone(),
                desktop_identity_digest: identity_digest.clone(),
                owner_approval_code_digest: digest(6),
            },
            desktop_identity_fingerprint: identity_digest.clone(),
            approval_confirmed,
            completion: None,
            finalized_status: None,
            rebind_offer: None,
        };

        let mut unconfirmed = AppAndroidAuthorityOwner {
            pending_bootstrap: Some(request("bootstrap:expired-settings", false)),
            ..Default::default()
        };
        unconfirmed.discard_expired_unconfirmed_bootstrap(10_001);
        assert!(unconfirmed.pending_bootstrap.is_none());

        let mut confirmed = AppAndroidAuthorityOwner {
            pending_bootstrap: Some(request("bootstrap:confirmed-settings", true)),
            ..Default::default()
        };
        confirmed.discard_expired_unconfirmed_bootstrap(10_001);
        assert!(confirmed.pending_bootstrap.is_some());
    }

    #[test]
    fn incomplete_runtime_rebind_blocks_full_record_recovery() {
        let (_, key_id, _, identity_digest) = identity();
        let bootstrap = AppAndroidOwnerBootstrapNonce::mint(
            "bootstrap:rebind-blocks-recovery".to_owned(),
            1_000,
            10_000,
        )
        .expect("bootstrap nonce");
        let offer = AppAndroidOwnerBootstrapRebindOffer::mint(
            bootstrap,
            key_id,
            identity_digest.clone(),
            digest(4),
            1,
            Some(digest(5)),
            1_000,
            10_000,
        )
        .expect("rebind offer");
        let mut owner = AppAndroidAuthorityOwner {
            rebind_offer: Some(offer),
            ..Default::default()
        };
        let challenge = AppAndroidOwnerRecoveryChallenge::mint(
            "recovery:blocked-during-rebind".to_owned(),
            identity_digest,
            0,
            None,
            2_000,
            9_000,
        )
        .expect("recovery challenge");

        assert!(owner.stage_recovery(challenge, 2_001).is_err());
    }

    #[test]
    fn native_receipt_and_recovery_acknowledgments_are_exact() {
        let receipt_digest = digest(10);
        let target_ref = "android-device:exact-ack";
        let receipt_ack = AppAndroidOwnerNativeControlStatus {
            owner_generation: 3,
            latest_receipt_digest: Some(receipt_digest.clone()),
            accepted_receipt_digest: Some(receipt_digest.clone()),
            target_ref: Some(target_ref.to_owned()),
            review_generation: Some(0),
        };
        assert!(
            validate_native_receipt_ack(&receipt_ack, &receipt_digest, 3, target_ref, 0,).is_ok()
        );
        let mut wrong_target = receipt_ack;
        wrong_target.target_ref = Some("android-device:other".to_owned());
        assert!(
            validate_native_receipt_ack(&wrong_target, &receipt_digest, 3, target_ref, 0,).is_err()
        );

        let snapshot_digest = digest(11);
        let latest_receipt_digest = Some(receipt_digest);
        let recovery_ack = AppAndroidOwnerNativeControlStatus {
            owner_generation: 3,
            latest_receipt_digest: latest_receipt_digest.clone(),
            accepted_receipt_digest: Some(snapshot_digest.clone()),
            target_ref: None,
            review_generation: None,
        };
        assert!(validate_native_recovery_ack(
            &recovery_ack,
            &snapshot_digest,
            3,
            &latest_receipt_digest,
        )
        .is_ok());
        let mut wrong_recovery = recovery_ack;
        wrong_recovery.review_generation = Some(0);
        assert!(validate_native_recovery_ack(
            &wrong_recovery,
            &snapshot_digest,
            3,
            &latest_receipt_digest,
        )
        .is_err());
    }

    #[test]
    fn fresh_runtime_nonce_requires_exact_rebind_without_lowering_desktop_head() {
        let (pair, key_id, public_key, identity_digest) = identity();
        let code_digest = app_macos_desktop_owner_approval_code_digest(
            "rebind-owner-code-abcdefghijklmnopqrstuvwxyz",
        )
        .expect("owner code digest");
        let old_bootstrap =
            AppAndroidOwnerBootstrapNonce::mint("bootstrap:old-runtime".to_owned(), 1_000, 20_000)
                .expect("old bootstrap");
        let old_request = AppAndroidOwnerBootstrapChallengeRequest {
            bootstrap: old_bootstrap,
            desktop_identity_key_id: key_id.clone(),
            desktop_identity_public_key_hex: public_key.clone(),
            desktop_identity_digest: identity_digest.clone(),
            owner_approval_code_digest: code_digest,
        };
        let old_challenge =
            app_android_owner_bootstrap_challenge(&old_request).expect("old challenge");
        let mut old_attestation =
            AppMacosDesktopIdentityAttestation::unsigned(&old_challenge, digest(9), 2_000)
                .expect("old attestation");
        old_attestation.signature_hex = hex(pair
            .sign(&old_attestation.signing_bytes().expect("attestation bytes"))
            .as_ref());
        let old_completion = AppAndroidOwnerBootstrapCompletion {
            challenge: old_challenge.clone(),
            attestation: old_attestation.clone(),
        };
        let old_status =
            AppAndroidOwnerBootstrapStatus::from_verified(&old_challenge, &old_attestation, 2_001)
                .expect("old finalized status");
        let proposal = enrollment_proposal(1, None, identity_digest.clone(), 2_100);
        let receipt = signed_receipt(&proposal, &pair, &key_id, 2_101);
        let mut document = PersistedAndroidOwnerStore::empty(identity_digest.clone());
        apply_receipt(&mut document, receipt).expect("desktop authority head");
        let head_digest = document.latest_receipt_digest.clone();
        let mut owner = AppAndroidAuthorityOwner {
            document: Some(document),
            pending_bootstrap: Some(PendingAndroidOwnerBootstrap {
                request: old_request,
                desktop_identity_fingerprint: identity_digest.clone(),
                approval_confirmed: true,
                completion: Some(old_completion),
                finalized_status: Some(old_status),
                rebind_offer: None,
            }),
            ready: true,
            ..Default::default()
        };
        let fresh = AppAndroidOwnerBootstrapNonce::mint(
            "bootstrap:fresh-runtime".to_owned(),
            3_000,
            30_000,
        )
        .expect("fresh bootstrap");
        let response = owner
            .accept_runtime_hello(fresh.clone(), 3_001)
            .expect("rebind offer");
        let AppAndroidOwnerBootstrapDesktopMessage::RebindAwaitingNativeApproval { offer, .. } =
            response
        else {
            panic!("fresh runtime must receive a rebind offer")
        };
        assert_eq!(offer.owner_generation, 1);
        assert_eq!(offer.latest_receipt_digest, head_digest);
        let recovery = AppAndroidOwnerBootstrapRecoveryHello {
            bootstrap: fresh,
            rebind_offer_digest: offer.digest().expect("offer digest"),
            expected_desktop_identity_digest: identity_digest,
        };
        assert!(matches!(
            owner
                .accept_runtime_recovery_hello(recovery, 3_002)
                .expect("pending rebind"),
            AppAndroidOwnerBootstrapDesktopMessage::RebindAwaitingNativeApproval { .. }
        ));
        assert_eq!(
            owner.ready_document().expect("document").owner_generation,
            1
        );
        assert_eq!(
            owner
                .ready_document()
                .expect("document")
                .latest_receipt_digest,
            head_digest
        );
    }

    #[test]
    fn settings_surface_has_no_http_or_generic_signing_seam() {
        let source = include_str!("../../src/lib/AndroidAppsAuthority.svelte");
        assert!(!source.contains("fetch("));
        assert!(!source.contains("http://"));
        assert!(!source.contains("authorization"));
        assert!(!source.contains("signature"));
        assert!(source.contains("display_digest"));
        assert!(source.contains("desktopIdentityFingerprint"));
    }

    #[test]
    fn bootstrap_transport_uses_audit_token_code_identity_and_no_http_route() {
        let source = include_str!("app_android_authority.rs");
        assert!(source.contains("libc::LOCAL_PEERTOKEN"));
        assert!(source.contains("set_audit_token"));
        assert!(source.contains("APP_ANDROID_OWNER_RUNTIME_CODE_IDENTIFIER"));
        assert!(source.contains("APP_ANDROID_OWNER_BOOTSTRAP_SOCKET_FILENAME"));
        let retired_bootstrap_route = ["/native", "/bootstrap"].concat();
        assert!(!source.contains(&retired_bootstrap_route));
        let retired_pid_identity_helper = ["copy_guest", "_with_pid"].concat();
        assert!(!source.contains(&retired_pid_identity_helper));
    }

    #[test]
    fn cold_debug_code_verification_is_bounded_before_nonce_readiness() {
        assert!(APP_ANDROID_OWNER_PEER_CODE_VERIFY_TIMEOUT > APP_ANDROID_OWNER_OPERATION_TIMEOUT);
        let source = include_str!("app_android_authority.rs");
        let handler = source
            .split_once("async fn handle_android_bootstrap_socket_connection(")
            .expect("bootstrap handler")
            .1
            .split_once("async fn continue_android_bootstrap_exchange(")
            .expect("bootstrap handler boundary")
            .0;
        let peer_verification = handler
            .find("verify_android_bootstrap_runtime_peer(&stream")
            .expect("runtime peer verification");
        let admission = handler
            .find("reserve_android_owner_native_control().await")
            .expect("native control admission");
        let readiness = handler
            .find("AppAndroidOwnerBootstrapDesktopMessage::peer_verified()")
            .expect("verified-peer readiness barrier");
        let first_runtime_frame = handler
            .find("read_android_bootstrap_frame(&mut stream)")
            .expect("first runtime frame");
        assert!(peer_verification < admission);
        assert!(admission < readiness);
        assert!(readiness < first_runtime_frame);
    }
}

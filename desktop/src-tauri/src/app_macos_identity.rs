//! Keychain-backed desktop identity for the typed Apps macOS host.
//!
//! The runtime must prove an explicitly displayed one-time owner code and an
//! Ed25519 challenge before it may disclose a pairing HMAC key. The private
//! identity seed never enters the host-gateway wire or the pairing document.

use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use magician_app_contract::android_owner::{
    AppAndroidOwnerNativeRequest, AppAndroidOwnerOperation, AppAndroidOwnerReceipt,
    AppAndroidOwnerRecoverySnapshot, AppAndroidOwnerStatusReceipt,
};
use magician_app_contract::contribution::{
    AppMemoryOwnerDecisionEnvelopeV1, AppMemoryOwnerDecisionV1, AppMemoryOwnerReviewV1,
};
use magician_app_contract::macos_host::{
    app_macos_desktop_identity_digest, app_macos_desktop_owner_approval_code_digest,
    AppMacosDesktopIdentityAttestation, AppMacosDesktopIdentityChallenge,
    AppMacosHostPairingApproval, AppMacosHostPairingFinalized, AppMacosHostPairingResetAck,
    AppMacosHostPairingRevoked,
};
use ring::rand::{SecureRandom as _, SystemRandom};
use ring::signature::{Ed25519KeyPair, KeyPair as _};
use serde::Serialize;
use zeroize::{Zeroize, Zeroizing};

const APP_MACOS_DESKTOP_IDENTITY_KEYCHAIN_SERVICE: &str =
    "com.magicbeans.magician.app-macos-desktop-identity";
const APP_MACOS_DESKTOP_IDENTITY_KEYCHAIN_ACCOUNT: &str = "ed25519-pkcs8-v1";
const APP_MACOS_DESKTOP_OWNER_APPROVAL_TTL_MS: i64 = 5 * 60 * 1_000;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
pub struct AppMacosDesktopIdentityEnrollment {
    pub desktop_identity_key_id: String,
    pub desktop_identity_public_key_hex: String,
    pub desktop_identity_fingerprint: String,
    pub owner_approval_code: String,
    pub expires_at_ms: i64,
}

impl std::fmt::Debug for AppMacosDesktopIdentityEnrollment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AppMacosDesktopIdentityEnrollment")
            .field("desktop_identity_key_id", &self.desktop_identity_key_id)
            .field(
                "desktop_identity_fingerprint",
                &self.desktop_identity_fingerprint,
            )
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

impl Drop for AppMacosDesktopIdentityEnrollment {
    fn drop(&mut self) {
        self.owner_approval_code.zeroize();
    }
}

struct PendingOwnerApproval {
    code_digest: String,
    expires_at_ms: i64,
}

#[derive(Default)]
pub(crate) struct AppMacosDesktopIdentityOwner {
    pending: Option<PendingOwnerApproval>,
    #[cfg(test)]
    test_pkcs8: Option<Vec<u8>>,
}

impl AppMacosDesktopIdentityOwner {
    pub(crate) fn public_identity(&self) -> Result<(String, String, String), String> {
        let key_pair = self.load_key_pair()?;
        let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
        let key_id = desktop_identity_key_id(&public_key_hex);
        let digest = app_macos_desktop_identity_digest(&key_id, &public_key_hex)
            .map_err(|_| "desktop identity fingerprint is invalid".to_owned())?;
        Ok((key_id, public_key_hex, digest))
    }

    pub(crate) fn begin_owner_approval(
        &mut self,
        now_ms: i64,
    ) -> Result<AppMacosDesktopIdentityEnrollment, String> {
        let key_pair = self.load_key_pair()?;
        let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
        let key_id = desktop_identity_key_id(&public_key_hex);
        let fingerprint = app_macos_desktop_identity_digest(&key_id, &public_key_hex)
            .map_err(|_| "desktop identity fingerprint is invalid".to_owned())?;
        let mut code_bytes = [0_u8; 24];
        SystemRandom::new()
            .fill(&mut code_bytes)
            .map_err(|_| "could not generate desktop owner approval".to_owned())?;
        let owner_approval_code = encode_lower_hex(&code_bytes);
        code_bytes.zeroize();
        let code_digest = app_macos_desktop_owner_approval_code_digest(&owner_approval_code)
            .map_err(|_| "desktop owner approval code is invalid".to_owned())?;
        let expires_at_ms = now_ms
            .checked_add(APP_MACOS_DESKTOP_OWNER_APPROVAL_TTL_MS)
            .ok_or_else(|| "desktop owner approval lifetime is out of range".to_owned())?;
        self.pending = Some(PendingOwnerApproval {
            code_digest,
            expires_at_ms,
        });
        Ok(AppMacosDesktopIdentityEnrollment {
            desktop_identity_key_id: key_id,
            desktop_identity_public_key_hex: public_key_hex,
            desktop_identity_fingerprint: fingerprint,
            owner_approval_code,
            expires_at_ms,
        })
    }

    pub(crate) fn attest(
        &mut self,
        challenge: &AppMacosDesktopIdentityChallenge,
        host_identity_digest: String,
        now_ms: i64,
    ) -> Result<AppMacosDesktopIdentityAttestation, String> {
        challenge
            .validate(now_ms)
            .map_err(|_| "desktop identity challenge is invalid".to_owned())?;
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| pending.expires_at_ms > now_ms)
            .ok_or_else(|| "native desktop owner approval is absent or expired".to_owned())?;
        if pending.code_digest != challenge.owner_approval_code_digest {
            return Err("native desktop owner approval does not match the challenge".to_owned());
        }
        let key_pair = self.load_key_pair()?;
        let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
        let key_id = desktop_identity_key_id(&public_key_hex);
        if challenge.desktop_identity_key_id != key_id
            || challenge.desktop_identity_public_key_hex != public_key_hex
        {
            return Err("desktop identity challenge is not pinned to this owner".to_owned());
        }
        let mut attestation =
            AppMacosDesktopIdentityAttestation::unsigned(challenge, host_identity_digest, now_ms)
                .map_err(|_| "desktop identity attestation is invalid".to_owned())?;
        attestation.signature_hex = encode_lower_hex(
            key_pair
                .sign(
                    &attestation
                        .signing_bytes()
                        .map_err(|_| "desktop identity attestation encoding failed".to_owned())?,
                )
                .as_ref(),
        );
        Ok(attestation)
    }

    pub(crate) fn consume_owner_approval(
        &mut self,
        challenge: &AppMacosDesktopIdentityChallenge,
        now_ms: i64,
    ) -> Result<(), String> {
        let pending = self
            .pending
            .as_ref()
            .filter(|pending| pending.expires_at_ms > now_ms)
            .ok_or_else(|| "native desktop owner approval is absent or expired".to_owned())?;
        if pending.code_digest != challenge.owner_approval_code_digest {
            return Err("native desktop owner approval does not match the challenge".to_owned());
        }
        self.pending = None;
        Ok(())
    }

    pub(crate) fn sign_approval(
        &self,
        mut approval: AppMacosHostPairingApproval,
        proposal: &magician_app_contract::macos_host::AppMacosHostPairingProposal,
        pairing_key: &[u8; 32],
    ) -> Result<AppMacosHostPairingApproval, String> {
        let key_pair = self.load_key_pair()?;
        approval.desktop_identity_key_id =
            desktop_identity_key_id(&encode_lower_hex(key_pair.public_key().as_ref()));
        approval = approval
            .sign(proposal, pairing_key)
            .map_err(|_| "typed macOS pairing approval could not be signed".to_owned())?;
        approval.desktop_identity_signature_hex = encode_lower_hex(
            key_pair
                .sign(
                    &approval
                        .desktop_identity_signing_bytes()
                        .map_err(|_| "desktop identity approval encoding failed".to_owned())?,
                )
                .as_ref(),
        );
        Ok(approval)
    }

    pub(crate) fn sign_finalized(
        &self,
        mut finalized: AppMacosHostPairingFinalized,
    ) -> Result<AppMacosHostPairingFinalized, String> {
        let key_pair = self.load_key_pair()?;
        finalized.desktop_identity_key_id =
            desktop_identity_key_id(&encode_lower_hex(key_pair.public_key().as_ref()));
        finalized.desktop_identity_signature_hex = encode_lower_hex(
            key_pair
                .sign(
                    &finalized
                        .desktop_identity_signing_bytes()
                        .map_err(|_| "desktop identity finalization encoding failed".to_owned())?,
                )
                .as_ref(),
        );
        Ok(finalized)
    }

    pub(crate) fn sign_reset_ack(
        &self,
        mut ack: AppMacosHostPairingResetAck,
    ) -> Result<AppMacosHostPairingResetAck, String> {
        let key_pair = self.load_key_pair()?;
        let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
        let key_id = desktop_identity_key_id(&public_key_hex);
        if ack.desktop_identity_key_id != key_id
            || ack.desktop_identity_public_key_hex != public_key_hex
        {
            return Err("typed macOS reset acknowledgment identity changed".to_owned());
        }
        ack.desktop_identity_signature_hex =
            encode_lower_hex(
                key_pair
                    .sign(&ack.signing_bytes().map_err(|_| {
                        "typed macOS reset acknowledgment encoding failed".to_owned()
                    })?)
                    .as_ref(),
            );
        Ok(ack)
    }

    pub(crate) fn sign_revoked(
        &self,
        mut revoked: AppMacosHostPairingRevoked,
    ) -> Result<AppMacosHostPairingRevoked, String> {
        let key_pair = self.load_key_pair()?;
        revoked.desktop_identity_key_id =
            desktop_identity_key_id(&encode_lower_hex(key_pair.public_key().as_ref()));
        revoked.desktop_identity_signature_hex = encode_lower_hex(
            key_pair
                .sign(
                    &revoked
                        .desktop_identity_signing_bytes()
                        .map_err(|_| "desktop identity revocation encoding failed".to_owned())?,
                )
                .as_ref(),
        );
        Ok(revoked)
    }

    pub(crate) fn verify_approval(
        &self,
        approval: &AppMacosHostPairingApproval,
    ) -> Result<(), String> {
        let (key_id, public_key_hex) = self.public_binding()?;
        approval
            .verify_desktop_identity(&key_id, &public_key_hex)
            .map_err(|_| "persisted desktop identity approval is invalid".to_owned())
    }

    pub(crate) fn verify_attestation(
        &self,
        challenge: &AppMacosDesktopIdentityChallenge,
        attestation: &AppMacosDesktopIdentityAttestation,
    ) -> Result<(), String> {
        let (key_id, public_key_hex) = self.public_binding()?;
        if challenge.desktop_identity_key_id != key_id
            || challenge.desktop_identity_public_key_hex != public_key_hex
        {
            return Err("persisted desktop identity attestation key is invalid".to_owned());
        }
        attestation
            .verify(challenge, attestation.attested_at_ms)
            .map_err(|_| "persisted desktop identity attestation is invalid".to_owned())
    }

    pub(crate) fn verify_finalized(
        &self,
        finalized: &AppMacosHostPairingFinalized,
    ) -> Result<(), String> {
        let (key_id, public_key_hex) = self.public_binding()?;
        finalized
            .verify_desktop_identity(&key_id, &public_key_hex)
            .map_err(|_| "persisted desktop identity finalization is invalid".to_owned())
    }

    pub(crate) fn verify_revoked(
        &self,
        revoked: &AppMacosHostPairingRevoked,
    ) -> Result<(), String> {
        let (key_id, public_key_hex) = self.public_binding()?;
        revoked
            .verify_desktop_identity(&key_id, &public_key_hex)
            .map_err(|_| "persisted desktop identity revocation is invalid".to_owned())
    }

    fn public_binding(&self) -> Result<(String, String), String> {
        let key_pair = self.load_key_pair()?;
        let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
        let key_id = desktop_identity_key_id(&public_key_hex);
        Ok((key_id, public_key_hex))
    }

    fn load_key_pair(&self) -> Result<Ed25519KeyPair, String> {
        #[cfg(test)]
        if let Some(pkcs8) = self.test_pkcs8.as_ref() {
            return Ed25519KeyPair::from_pkcs8(pkcs8)
                .map_err(|_| "test desktop identity is invalid".to_owned());
        }
        load_or_create_key_pair()
    }

    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let pkcs8 =
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).expect("test desktop identity");
        Self {
            pending: None,
            test_pkcs8: Some(pkcs8.as_ref().to_vec()),
        }
    }
}

/// Return the public half of the same Keychain identity used by the typed
/// macOS pairing owner. Android Apps authority deliberately reuses this
/// independently owner-enrolled identity instead of creating another trust
/// bootstrap or exporting its private key.
pub(crate) fn app_android_owner_desktop_identity() -> Result<(String, String, String), String> {
    AppMacosDesktopIdentityOwner::default().public_identity()
}

/// Sign exactly one closed Android authority transition. Keeping this typed
/// helper beside the Keychain loader prevents the Android owner from becoming
/// an arbitrary-message signing oracle.
pub(crate) fn sign_app_android_owner_receipt(
    mut receipt: AppAndroidOwnerReceipt,
) -> Result<AppAndroidOwnerReceipt, String> {
    if receipt.operation == AppAndroidOwnerOperation::RevokeDevice {
        return Err("Android device revocation is unavailable in the V1 desktop owner".to_owned());
    }
    let key_pair = load_or_create_key_pair()?;
    let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
    let key_id = desktop_identity_key_id(&public_key_hex);
    if receipt.desktop_identity_key_id != key_id {
        return Err("Android owner receipt is not pinned to this desktop identity".to_owned());
    }
    receipt.desktop_identity_signature_hex = encode_lower_hex(
        key_pair
            .sign(
                &receipt
                    .signing_bytes()
                    .map_err(|_| "Android owner receipt encoding failed".to_owned())?,
            )
            .as_ref(),
    );
    Ok(receipt)
}

/// Sign one exact app-memory proposal/head/decision document. The typed input
/// is reconstructed before signing so this helper cannot become a generic
/// Keychain signing oracle and cannot preserve caller-supplied signature bytes.
pub(crate) fn sign_app_memory_owner_decision(
    review: AppMemoryOwnerReviewV1,
    decision: AppMemoryOwnerDecisionV1,
    retained_until_ms: Option<i64>,
) -> Result<AppMemoryOwnerDecisionEnvelopeV1, String> {
    review
        .validate()
        .map_err(|_| "app-memory owner review is invalid".to_owned())?;
    let (key_id, public_key_hex, desktop_identity_digest) =
        AppMacosDesktopIdentityOwner::default().public_identity()?;
    if review.desktop_identity_key_id != key_id
        || review.desktop_identity_digest != desktop_identity_digest
    {
        return Err("app-memory owner review is not pinned to this Keychain identity".to_owned());
    }
    let envelope = AppMemoryOwnerDecisionEnvelopeV1::prepare(review, decision, retained_until_ms)
        .map_err(|_| "app-memory owner decision is invalid".to_owned())?;
    let key_pair = load_or_create_key_pair()?;
    if encode_lower_hex(key_pair.public_key().as_ref()) != public_key_hex {
        return Err("app-memory Keychain identity changed before signing".to_owned());
    }
    let signature_hex = encode_lower_hex(
        key_pair
            .sign(
                &envelope
                    .signing_bytes()
                    .map_err(|_| "app-memory owner decision encoding failed".to_owned())?,
            )
            .as_ref(),
    );
    envelope
        .with_signature_hex(signature_hex)
        .map_err(|_| "app-memory owner decision sealing failed".to_owned())
}

/// Sign a nonce-bound Android authority status snapshot. This is intentionally
/// distinct from transition signing so no caller can substitute another wire
/// shape under the desktop identity.
pub(crate) fn sign_app_android_owner_status(
    mut status: AppAndroidOwnerStatusReceipt,
) -> Result<AppAndroidOwnerStatusReceipt, String> {
    let key_pair = load_or_create_key_pair()?;
    let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
    let key_id = desktop_identity_key_id(&public_key_hex);
    if status.desktop_identity_key_id != key_id {
        return Err("Android owner status is not pinned to this desktop identity".to_owned());
    }
    status.desktop_identity_signature_hex = encode_lower_hex(
        key_pair
            .sign(
                &status
                    .signing_bytes()
                    .map_err(|_| "Android owner status encoding failed".to_owned())?,
            )
            .as_ref(),
    );
    Ok(status)
}

/// Sign the explicit Settings-approved recovery projection. Recovery has its
/// own closed wire domain and therefore cannot be confused with either a
/// transition receipt or the record-free routine status response.
pub(crate) fn sign_app_android_owner_recovery(
    mut snapshot: AppAndroidOwnerRecoverySnapshot,
) -> Result<AppAndroidOwnerRecoverySnapshot, String> {
    let key_pair = load_or_create_key_pair()?;
    let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
    let key_id = desktop_identity_key_id(&public_key_hex);
    if snapshot.desktop_identity_key_id != key_id {
        return Err("Android owner recovery is not pinned to this desktop identity".to_owned());
    }
    snapshot.desktop_identity_signature_hex = encode_lower_hex(
        key_pair
            .sign(
                &snapshot
                    .signing_bytes()
                    .map_err(|_| "Android owner recovery encoding failed".to_owned())?,
            )
            .as_ref(),
    );
    Ok(snapshot)
}

/// Sign one closed native runtime-control request. The operation and optional
/// typed body digest are already fixed by the contract; JavaScript never sees
/// either this helper or reusable signing authority.
pub(crate) fn sign_app_android_owner_native_request(
    mut request: AppAndroidOwnerNativeRequest,
) -> Result<AppAndroidOwnerNativeRequest, String> {
    let key_pair = load_or_create_key_pair()?;
    let public_key_hex = encode_lower_hex(key_pair.public_key().as_ref());
    let key_id = desktop_identity_key_id(&public_key_hex);
    if request.desktop_identity_key_id != key_id {
        return Err("Android native request is not pinned to this desktop identity".to_owned());
    }
    request.desktop_identity_signature_hex = encode_lower_hex(
        key_pair
            .sign(
                &request
                    .signing_bytes()
                    .map_err(|_| "Android native request encoding failed".to_owned())?,
            )
            .as_ref(),
    );
    Ok(request)
}

fn load_or_create_key_pair() -> Result<Ed25519KeyPair, String> {
    let entry = keyring::Entry::new(
        APP_MACOS_DESKTOP_IDENTITY_KEYCHAIN_SERVICE,
        APP_MACOS_DESKTOP_IDENTITY_KEYCHAIN_ACCOUNT,
    )
    .map_err(|_| "desktop identity Keychain entry is unavailable".to_owned())?;
    let pkcs8 = match entry.get_password() {
        Ok(encoded) => {
            let encoded = Zeroizing::new(encoded);
            Zeroizing::new(
                BASE64
                    .decode(encoded.as_bytes())
                    .map_err(|_| "desktop identity Keychain material is corrupt".to_owned())?,
            )
        },
        Err(keyring::Error::NoEntry) => {
            let generated = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map_err(|_| "could not generate desktop identity".to_owned())?;
            let bytes = Zeroizing::new(generated.as_ref().to_vec());
            let encoded = Zeroizing::new(BASE64.encode(&*bytes));
            entry
                .set_password(encoded.as_str())
                .map_err(|_| "could not persist desktop identity in Keychain".to_owned())?;
            bytes
        },
        Err(_) => return Err("desktop identity Keychain is unavailable".to_owned()),
    };
    Ed25519KeyPair::from_pkcs8(&pkcs8)
        .map_err(|_| "desktop identity Keychain material is invalid".to_owned())
}

fn desktop_identity_key_id(public_key_hex: &str) -> String {
    let digest = blake3::hash(public_key_hex.as_bytes()).to_hex();
    format!("desktop-identity:{digest}")
}

fn encode_lower_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

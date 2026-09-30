//! Durable, owner-mediated bootstrap for the typed Apps macOS host.
//!
//! A loopback runtime may submit a self-authenticating proposal, but that only
//! creates a pending record. Native owner approval fixes the CUA binary and
//! current application identities; a second runtime-signed finalization binds
//! those identities to the reviewed runtime implementation before the verifier
//! becomes active. Rotation is authorized by the previous active key.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use magician_app_contract::macos_host::{
    decode_pairing_key, AppMacosDesktopIdentityAttestation, AppMacosDesktopIdentityChallenge,
    AppMacosHostPairingApproval, AppMacosHostPairingFinalization, AppMacosHostPairingFinalized,
    AppMacosHostPairingProposal, AppMacosHostPairingResetAck, AppMacosHostPairingResetChallenge,
    AppMacosHostPairingRevoked, AppMacosHostPairingStatusRequest,
    AppMacosHostPairingStatusResponse, APP_MACOS_HOST_PAIRING_V1,
};
use serde::{Deserialize, Serialize};

use crate::app_macos_host::{AppMacosHostState, AppMacosPrevalidatedCuaBinary};

const APP_MACOS_DESKTOP_PAIRING_STORE_V3: &str = "magician.desktop.app-macos-host-pairing.v3";
const APP_MACOS_DESKTOP_PAIRING_MAX_BYTES: u64 = 512 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPendingPairing {
    bootstrap: PersistedAttestedBootstrap,
    proposal: AppMacosHostPairingProposal,
    #[serde(default)]
    approval: Option<AppMacosHostPairingApproval>,
    #[serde(default)]
    cua_driver_binary: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedActivePairing {
    bootstrap: PersistedAttestedBootstrap,
    proposal: AppMacosHostPairingProposal,
    approval: AppMacosHostPairingApproval,
    finalization: AppMacosHostPairingFinalization,
    finalized: AppMacosHostPairingFinalized,
    cua_driver_binary: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedAttestedBootstrap {
    challenge: AppMacosDesktopIdentityChallenge,
    attestation: AppMacosDesktopIdentityAttestation,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPairingResetAnchor {
    challenge: AppMacosHostPairingResetChallenge,
    acknowledgment: AppMacosHostPairingResetAck,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedDesktopPairingStore {
    schema: String,
    host_identity_digest: String,
    generation_floor: u64,
    tcc_epoch_floor: u64,
    #[serde(default)]
    active: Option<PersistedActivePairing>,
    #[serde(default)]
    pending: Option<PersistedPendingPairing>,
    #[serde(default)]
    revocation: Option<AppMacosHostPairingRevoked>,
    #[serde(default)]
    attested_bootstrap: Option<PersistedAttestedBootstrap>,
    #[serde(default)]
    reset_anchor: Option<PersistedPairingResetAnchor>,
}

pub(crate) struct AppMacosDesktopPairingOwner {
    path: Option<PathBuf>,
    record: Option<PersistedDesktopPairingStore>,
    unavailable: bool,
}

#[derive(Clone)]
pub(crate) struct AppMacosVerifierBinaryRequirement {
    setup_id: String,
    generation: u64,
    key_id: String,
    binary: PathBuf,
    binary_digest: String,
    owner_profile_digest: String,
    owner_implementation_digest: String,
}

impl AppMacosVerifierBinaryRequirement {
    pub(crate) fn binary(&self) -> &Path {
        &self.binary
    }

    pub(crate) fn binary_digest(&self) -> &str {
        &self.binary_digest
    }
}

impl Default for AppMacosDesktopPairingOwner {
    fn default() -> Self {
        Self {
            path: None,
            record: None,
            unavailable: false,
        }
    }
}

impl AppMacosDesktopPairingOwner {
    pub(crate) fn ensure_host_identity_digest(&mut self) -> Result<String, String> {
        self.ensure_available()?;
        if let Some(record) = self.record.as_ref() {
            return Ok(record.host_identity_digest.clone());
        }
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let record = PersistedDesktopPairingStore {
            schema: APP_MACOS_DESKTOP_PAIRING_STORE_V3.to_owned(),
            host_identity_digest: new_host_identity_digest(),
            generation_floor: 0,
            tcc_epoch_floor: 0,
            active: None,
            pending: None,
            revocation: None,
            attested_bootstrap: None,
            reset_anchor: None,
        };
        persist_pairing_store(&path, &record)?;
        let identity = record.host_identity_digest.clone();
        self.record = Some(record);
        Ok(identity)
    }

    pub(crate) fn native_pending_review(
        &self,
        now_ms: i64,
    ) -> Option<(AppMacosHostPairingProposal, bool)> {
        self.record
            .as_ref()
            .and_then(|record| record.pending.as_ref())
            .filter(|pending| pending.proposal.expires_at_ms > now_ms)
            .map(|pending| (pending.proposal.clone(), pending.approval.is_none()))
    }

    pub(crate) fn native_reset_context(&self) -> Result<Option<(String, u64, bool)>, String> {
        self.ensure_available()?;
        let Some(record) = self.record.as_ref() else {
            return Ok(None);
        };
        Ok(Some((
            record.host_identity_digest.clone(),
            record.generation_floor,
            record.generation_floor > 0 && record.active.is_none() && record.pending.is_none(),
        )))
    }

    pub(crate) fn reset_after_owner_confirmation(
        &mut self,
        challenge: AppMacosHostPairingResetChallenge,
        expected_host_identity_digest: &str,
        expected_generation_floor: u64,
        expected_desktop_identity_digest: &str,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<AppMacosHostPairingResetAck, String> {
        self.ensure_available()?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| "typed macOS pairing store is empty".to_owned())?;
        let (desktop_key_id, desktop_public_key, desktop_digest) =
            desktop_identity.public_identity()?;
        if let Some(anchor) = record.reset_anchor.as_ref() {
            if anchor.challenge == challenge {
                anchor
                    .acknowledgment
                    .verify(&anchor.challenge, now_ms)
                    .map_err(|_| {
                        "persisted typed macOS reset acknowledgment is invalid".to_owned()
                    })?;
                if desktop_digest == expected_desktop_identity_digest
                    && desktop_digest == anchor.acknowledgment.desktop_identity_digest
                    && record.host_identity_digest == expected_host_identity_digest
                    && record.generation_floor == expected_generation_floor
                {
                    return Ok(anchor.acknowledgment.clone());
                }
            }
        }
        challenge
            .validate(now_ms)
            .map_err(|_| "typed macOS reset challenge is invalid or expired".to_owned())?;
        if record.active.is_some()
            || record.pending.is_some()
            || record.generation_floor == 0
            || record.generation_floor != expected_generation_floor
            || record.host_identity_digest != expected_host_identity_digest
            || desktop_digest != expected_desktop_identity_digest
        {
            return Err("typed macOS reset does not match the displayed revoked owner".to_owned());
        }
        let acknowledgment = desktop_identity.sign_reset_ack(
            AppMacosHostPairingResetAck::unsigned(
                &challenge,
                record.host_identity_digest.clone(),
                desktop_key_id,
                desktop_public_key,
                record.generation_floor,
                now_ms,
            )
            .map_err(|_| "typed macOS reset acknowledgment is invalid".to_owned())?,
        )?;
        acknowledgment
            .verify(&challenge, now_ms)
            .map_err(|_| "typed macOS reset acknowledgment signature is invalid".to_owned())?;
        let mut reset = record.clone();
        reset.active = None;
        reset.pending = None;
        reset.revocation = None;
        reset.attested_bootstrap = None;
        reset.reset_anchor = Some(PersistedPairingResetAnchor {
            challenge,
            acknowledgment: acknowledgment.clone(),
        });
        persist_pairing_store(&path, &reset)?;
        self.record = Some(reset);
        verifier.clear_verifier();
        Ok(acknowledgment)
    }

    pub(crate) fn initialize(
        &mut self,
        path: PathBuf,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<(), String> {
        if self.path.as_ref().is_some_and(|current| current != &path) {
            self.unavailable = true;
            verifier.clear_verifier();
            return Err("typed macOS pairing store path changed during the process".to_owned());
        }
        if self.path.is_some() {
            return if self.unavailable {
                Err("typed macOS pairing store is unavailable".to_owned())
            } else {
                Ok(())
            };
        }
        self.path = Some(path.clone());
        let record = match read_pairing_store(&path) {
            Ok(record) => record,
            Err(error) => {
                self.unavailable = true;
                verifier.clear_verifier();
                return Err(error);
            },
        };
        if let Some(record) = record.as_ref() {
            if let Err(error) = validate_persisted_pairing_record(record, desktop_identity) {
                self.unavailable = true;
                verifier.clear_verifier();
                return Err(error);
            }
        }
        self.record = record;
        Ok(())
    }

    pub(crate) fn active_binary_requirement(
        &self,
        verifier: &AppMacosHostState,
    ) -> Result<Option<AppMacosVerifierBinaryRequirement>, String> {
        self.ensure_available()?;
        let Some(active) = self
            .record
            .as_ref()
            .and_then(|record| record.active.as_ref())
        else {
            return Ok(None);
        };
        let requirement = verifier_binary_requirement(active);
        if verifier.verifier_matches(
            &requirement.key_id,
            &requirement.binary_digest,
            &requirement.owner_profile_digest,
            &requirement.owner_implementation_digest,
        ) {
            Ok(None)
        } else {
            Ok(Some(requirement))
        }
    }

    pub(crate) fn pending_binary_requirement(
        &self,
        finalization: &AppMacosHostPairingFinalization,
    ) -> Result<AppMacosVerifierBinaryRequirement, String> {
        self.ensure_available()?;
        let pending = self
            .record
            .as_ref()
            .and_then(|record| record.pending.as_ref())
            .filter(|pending| pending.proposal.setup_id == finalization.setup_id)
            .ok_or_else(|| "typed macOS finalization has no exact pending transition".to_owned())?;
        let approval = pending
            .approval
            .as_ref()
            .ok_or_else(|| "typed macOS finalization precedes native approval".to_owned())?;
        Ok(AppMacosVerifierBinaryRequirement {
            setup_id: pending.proposal.setup_id.clone(),
            generation: pending.proposal.generation,
            key_id: pending.proposal.key_id.clone(),
            binary: pending
                .cua_driver_binary
                .clone()
                .ok_or_else(|| "typed macOS approved CUA binary is missing".to_owned())?,
            binary_digest: approval.cua_driver_binary_digest.clone(),
            owner_profile_digest: finalization.owner_profile_digest.clone(),
            owner_implementation_digest: finalization.owner_implementation_digest.clone(),
        })
    }

    pub(crate) fn install_active_prevalidated(
        &mut self,
        requirement: &AppMacosVerifierBinaryRequirement,
        binary: AppMacosPrevalidatedCuaBinary,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<(), String> {
        self.ensure_available()?;
        let active = self
            .record
            .as_ref()
            .and_then(|record| record.active.as_ref())
            .filter(|active| active_matches_requirement(active, requirement))
            .ok_or_else(|| "typed macOS active verifier changed during prevalidation".to_owned())?;
        install_active_pairing(active, verifier, desktop_identity, binary)
    }

    pub(crate) fn retained_identity_attestation(
        &self,
        challenge: &AppMacosDesktopIdentityChallenge,
        now_ms: i64,
    ) -> Result<Option<AppMacosDesktopIdentityAttestation>, String> {
        self.ensure_available()?;
        let Some(record) = self.record.as_ref() else {
            return Ok(None);
        };
        let Some(bootstrap) = record.attested_bootstrap.as_ref() else {
            return Ok(None);
        };
        if &bootstrap.challenge != challenge {
            return Ok(None);
        }
        bootstrap
            .attestation
            .verify(&bootstrap.challenge, now_ms)
            .map_err(|_| {
                "retained desktop identity attestation is invalid or expired".to_owned()
            })?;
        if bootstrap.attestation.host_identity_digest != record.host_identity_digest {
            return Err("retained desktop identity host binding is invalid".to_owned());
        }
        Ok(Some(bootstrap.attestation.clone()))
    }

    pub(crate) fn record_identity_attestation(
        &mut self,
        challenge: AppMacosDesktopIdentityChallenge,
        attestation: AppMacosDesktopIdentityAttestation,
        now_ms: i64,
    ) -> Result<(), String> {
        self.ensure_available()?;
        attestation
            .verify(&challenge, now_ms)
            .map_err(|_| "desktop identity attestation is invalid".to_owned())?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let mut candidate = self
            .record
            .clone()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        if attestation.host_identity_digest != candidate.host_identity_digest
            || candidate
                .pending
                .as_ref()
                .is_some_and(|pending| pending.proposal.expires_at_ms > now_ms)
        {
            return Err("desktop identity attestation is not eligible for setup".to_owned());
        }
        if let Some(stored) = candidate.attested_bootstrap.as_ref() {
            if stored.challenge == challenge && stored.attestation == attestation {
                return Ok(());
            }
            if stored.challenge.expires_at_ms > now_ms {
                return Err("another desktop identity bootstrap is already pending".to_owned());
            }
        }
        candidate.attested_bootstrap = Some(PersistedAttestedBootstrap {
            challenge,
            attestation,
        });
        persist_pairing_store(&path, &candidate)?;
        self.record = Some(candidate);
        Ok(())
    }

    pub(crate) fn receive_proposal(
        &mut self,
        proposal: AppMacosHostPairingProposal,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
    ) -> Result<(), String> {
        self.ensure_available()?;
        proposal
            .verify(now_ms)
            .map_err(|error| format!("{error:?}"))?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let mut candidate = self
            .record
            .clone()
            .unwrap_or_else(|| PersistedDesktopPairingStore {
                schema: APP_MACOS_DESKTOP_PAIRING_STORE_V3.to_owned(),
                host_identity_digest: new_host_identity_digest(),
                generation_floor: 0,
                tcc_epoch_floor: 0,
                active: None,
                pending: None,
                revocation: None,
                attested_bootstrap: None,
                reset_anchor: None,
            });
        let record = &mut candidate;
        if proposal.generation <= record.generation_floor {
            return Err("typed macOS pairing proposal generation is stale".to_owned());
        }
        if let Some(pending) = &record.pending {
            if pending.proposal == proposal {
                return Ok(());
            }
            if pending.proposal.expires_at_ms > now_ms {
                return Err("another typed macOS pairing approval is already pending".to_owned());
            }
        }
        let bootstrap = record
            .attested_bootstrap
            .as_ref()
            .filter(|bootstrap| bootstrap.challenge.expires_at_ms > now_ms)
            .ok_or_else(|| {
                "typed macOS pairing proposal has no live attested bootstrap".to_owned()
            })?;
        let attestation_digest = bootstrap
            .attestation
            .digest()
            .map_err(|_| "typed macOS bootstrap attestation digest is invalid".to_owned())?;
        if proposal.desktop_identity_attestation_digest != attestation_digest
            || bootstrap.attestation.host_identity_digest != record.host_identity_digest
        {
            return Err("typed macOS pairing proposal attestation binding is invalid".to_owned());
        }
        let bootstrap = bootstrap.clone();
        match &record.active {
            Some(active) => {
                let previous_key = decode_pairing_key(&active.proposal.signing_key_hex)
                    .map_err(|_| "active typed macOS pairing key is invalid".to_owned())?;
                if proposal.generation != active.proposal.generation.saturating_add(1) {
                    return Err("typed macOS rotation generation is not consecutive".to_owned());
                }
                proposal
                    .verify_rotation(active.proposal.generation, &previous_key)
                    .map_err(|_| {
                        "typed macOS rotation is not authorized by the active owner".to_owned()
                    })?;
            },
            None if proposal.previous_generation.is_some() => {
                return Err("typed macOS rotation has no active predecessor".to_owned());
            },
            None => {},
        }
        record.pending = Some(PersistedPendingPairing {
            bootstrap,
            proposal,
            approval: None,
            cua_driver_binary: None,
        });
        record.revocation = None;
        record.attested_bootstrap = None;
        persist_pairing_store(&path, record)?;
        self.record = Some(candidate);
        // A proposal alone never replaces the active verifier. Rotation only
        // changes authority after native approval and signed finalization.
        let _ = verifier;
        Ok(())
    }

    pub(crate) fn pending_proposal(
        &self,
        setup_id: &str,
        now_ms: i64,
    ) -> Result<AppMacosHostPairingProposal, String> {
        self.ensure_available()?;
        self.record
            .as_ref()
            .and_then(|record| record.pending.as_ref())
            .filter(|pending| {
                pending.proposal.setup_id == setup_id
                    && pending.proposal.expires_at_ms > now_ms
                    && pending.approval.is_none()
            })
            .map(|pending| pending.proposal.clone())
            .ok_or_else(|| {
                "typed macOS pairing proposal is absent, expired or already approved".to_owned()
            })
    }

    pub(crate) fn retained_approval(
        &self,
        setup_id: &str,
        now_ms: i64,
    ) -> Result<Option<AppMacosHostPairingApproval>, String> {
        self.ensure_available()?;
        Ok(self
            .record
            .as_ref()
            .and_then(|record| record.pending.as_ref())
            .filter(|pending| {
                pending.proposal.setup_id == setup_id && pending.proposal.expires_at_ms > now_ms
            })
            .and_then(|pending| pending.approval.clone()))
    }

    pub(crate) fn host_identity_digest(&self) -> Result<&str, String> {
        self.ensure_available()?;
        self.record
            .as_ref()
            .map(|record| record.host_identity_digest.as_str())
            .ok_or_else(|| "typed macOS pairing proposal is unavailable".to_owned())
    }

    pub(crate) fn next_tcc_epoch(&self) -> Result<u64, String> {
        self.ensure_available()?;
        self.record
            .as_ref()
            .ok_or_else(|| "typed macOS pairing proposal is unavailable".to_owned())?
            .tcc_epoch_floor
            .checked_add(1)
            .ok_or_else(|| "typed macOS TCC epoch space is exhausted".to_owned())
    }

    pub(crate) fn record_approval(
        &mut self,
        setup_id: &str,
        approval: AppMacosHostPairingApproval,
        cua_driver_binary: PathBuf,
        now_ms: i64,
    ) -> Result<(), String> {
        self.ensure_available()?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let mut candidate = self
            .record
            .clone()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        let record = &mut candidate;
        let pending = record
            .pending
            .as_mut()
            .filter(|pending| pending.proposal.setup_id == setup_id)
            .ok_or_else(|| "typed macOS pairing proposal changed before approval".to_owned())?;
        if let Some(stored) = pending.approval.as_ref() {
            if stored == &approval && pending.cua_driver_binary.as_ref() == Some(&cua_driver_binary)
            {
                return Ok(());
            }
            return Err("typed macOS pairing approval cannot be replaced".to_owned());
        }
        let expected_tcc_epoch = record
            .tcc_epoch_floor
            .checked_add(1)
            .ok_or_else(|| "typed macOS TCC epoch space is exhausted".to_owned())?;
        approval
            .verify(&pending.proposal, now_ms)
            .map_err(|_| "typed macOS pairing approval signature is invalid".to_owned())?;
        if approval.tcc_epoch != expected_tcc_epoch {
            return Err("typed macOS pairing approval TCC epoch is stale".to_owned());
        }
        if !cua_driver_binary.is_absolute()
            || std::fs::symlink_metadata(&cua_driver_binary)
                .map(|metadata| !metadata.is_file() || metadata.file_type().is_symlink())
                .unwrap_or(true)
        {
            return Err(
                "typed macOS CUA binary must be an exact regular absolute file inside its app \
                 bundle (/Applications/CuaDriver.app/Contents/MacOS/cua-driver), not a symlink"
                    .to_owned(),
            );
        }
        pending.approval = Some(approval);
        pending.cua_driver_binary = Some(cua_driver_binary);
        record.tcc_epoch_floor = expected_tcc_epoch;
        persist_pairing_store(&path, record)?;
        self.record = Some(candidate);
        Ok(())
    }

    pub(crate) fn status(
        &self,
        request: &AppMacosHostPairingStatusRequest,
        now_ms: i64,
    ) -> Result<AppMacosHostPairingStatusResponse, String> {
        self.ensure_available()?;
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        if let Some(pending) = &record.pending {
            if pending.proposal.setup_id == request.setup_id {
                if request.approval_digest.is_some() || request.finalization_digest.is_some() {
                    return Err(
                        "typed macOS pending status request has recovery material".to_owned()
                    );
                }
                let key = decode_pairing_key(&pending.proposal.signing_key_hex)
                    .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
                request
                    .verify(&pending.proposal, &key, now_ms)
                    .map_err(|_| "typed macOS pairing status capability is invalid".to_owned())?;
                return Ok(match &pending.approval {
                    Some(approval) => AppMacosHostPairingStatusResponse::Approved {
                        approval: approval.clone(),
                    },
                    None => AppMacosHostPairingStatusResponse::Pending,
                });
            }
        }
        if let Some(active) = &record.active {
            if active.proposal.setup_id == request.setup_id {
                if request.approval_digest.as_deref()
                    != Some(active.finalization.approval_digest.as_str())
                    || request.finalization_digest.as_deref()
                        != Some(active.finalized.finalization_digest.as_str())
                {
                    return Err("typed macOS activation recovery identity is stale".to_owned());
                }
                let key = decode_pairing_key(&active.proposal.signing_key_hex)
                    .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
                request
                    .verify(&active.proposal, &key, now_ms)
                    .map_err(|_| "typed macOS pairing status capability is invalid".to_owned())?;
                return Ok(AppMacosHostPairingStatusResponse::Active {
                    finalized: active.finalized.clone(),
                });
            }
        }
        if let Some(revoked) = record.revocation.as_ref() {
            if request.setup_id == revoked.setup_id
                && request.generation == revoked.generation
                && request.key_id == revoked.key_id
                && request.proposal_digest == revoked.proposal_digest
            {
                return Ok(AppMacosHostPairingStatusResponse::Revoked {
                    revoked: revoked.clone(),
                });
            }
        }
        Err("typed macOS pairing status capability is unknown".to_owned())
    }

    /// Consume a fresh runtime-owned pairing capability before clearing the
    /// physical verifier. This is separate from the native UI revoke command:
    /// an authenticated runtime control request must not leave the desktop's
    /// old signing key live after its own durable tombstone is committed.
    pub(crate) fn revoke_from_runtime(
        &mut self,
        request: &AppMacosHostPairingStatusRequest,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<AppMacosHostPairingRevoked, String> {
        self.ensure_available()?;
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        if let Some(revoked) = record.revocation.as_ref() {
            if request.setup_id == revoked.setup_id
                && request.generation == revoked.generation
                && request.key_id == revoked.key_id
                && request.proposal_digest == revoked.proposal_digest
            {
                return Ok(revoked.clone());
            }
        }
        if let Some(pending) = record
            .pending
            .as_ref()
            .filter(|pending| pending.proposal.setup_id == request.setup_id)
        {
            let key = decode_pairing_key(&pending.proposal.signing_key_hex)
                .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
            request
                .verify(&pending.proposal, &key, now_ms)
                .map_err(|_| "typed macOS revoke capability is invalid".to_owned())?;
        } else if let Some(active) = record
            .active
            .as_ref()
            .filter(|active| active.proposal.setup_id == request.setup_id)
        {
            let approval_digest = active
                .approval
                .digest()
                .map_err(|_| "typed macOS active approval digest is invalid".to_owned())?;
            let finalization_digest = active
                .finalization
                .digest()
                .map_err(|_| "typed macOS active finalization digest is invalid".to_owned())?;
            if request.approval_digest.as_deref() != Some(approval_digest.as_str())
                || request.finalization_digest.as_deref() != Some(finalization_digest.as_str())
            {
                return Err("typed macOS revoke recovery identity is stale".to_owned());
            }
            let key = decode_pairing_key(&active.proposal.signing_key_hex)
                .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
            request
                .verify(&active.proposal, &key, now_ms)
                .map_err(|_| "typed macOS revoke capability is invalid".to_owned())?;
        } else {
            return Err("typed macOS revoke capability is stale".to_owned());
        }
        self.revoke_exact(now_ms, verifier, desktop_identity)
            .map(|(_, revoked)| revoked)
    }

    pub(crate) fn finalize(
        &mut self,
        finalization: AppMacosHostPairingFinalization,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
        requirement: &AppMacosVerifierBinaryRequirement,
        prevalidated_binary: AppMacosPrevalidatedCuaBinary,
    ) -> Result<AppMacosHostPairingFinalized, String> {
        self.ensure_available()?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        let pending = record
            .pending
            .as_ref()
            .filter(|pending| pending.proposal.setup_id == finalization.setup_id)
            .ok_or_else(|| "typed macOS pairing finalization has no pending approval".to_owned())?;
        let approval = pending.approval.as_ref().ok_or_else(|| {
            "typed macOS pairing finalization precedes native approval".to_owned()
        })?;
        let key = decode_pairing_key(&pending.proposal.signing_key_hex)
            .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
        finalization
            .verify(&pending.proposal, approval, now_ms)
            .map_err(|_| "typed macOS pairing finalization is invalid".to_owned())?;
        let finalized = desktop_identity.sign_finalized(
            AppMacosHostPairingFinalized::sign(&finalization, &key, now_ms)
                .map_err(|_| "typed macOS pairing acknowledgment could not be signed".to_owned())?,
        )?;
        let active = PersistedActivePairing {
            bootstrap: pending.bootstrap.clone(),
            proposal: pending.proposal.clone(),
            approval: approval.clone(),
            finalization,
            finalized: finalized.clone(),
            cua_driver_binary: pending
                .cua_driver_binary
                .clone()
                .ok_or_else(|| "typed macOS approved CUA binary is missing".to_owned())?,
        };
        let mut activated = record.clone();
        activated.generation_floor = activated.generation_floor.max(active.proposal.generation);
        activated.active = Some(active.clone());
        activated.pending = None;
        activated.revocation = None;
        // Validate and install while the host gateway mutex excludes action
        // dispatch, then durably publish a cloned transaction. On persistence
        // failure the owner record remains the retryable pending transition
        // and the provisional verifier is removed.
        if !active_matches_requirement(&active, requirement)
            || prevalidated_binary.path() != requirement.binary.as_path()
            || prevalidated_binary.digest() != requirement.binary_digest
        {
            return Err("typed macOS verifier identity changed during finalization".to_owned());
        }
        install_active_pairing(&active, verifier, desktop_identity, prevalidated_binary)?;
        if let Err(error) = persist_pairing_store(&path, &activated) {
            verifier.clear_verifier();
            return Err(error);
        }
        self.record = Some(activated);
        Ok(finalized)
    }

    pub(crate) fn revoke(
        &mut self,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<u64, String> {
        self.revoke_exact(now_ms, verifier, desktop_identity)
            .map(|(generation, _)| generation)
    }

    fn revoke_exact(
        &mut self,
        now_ms: i64,
        verifier: &mut AppMacosHostState,
        desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    ) -> Result<(u64, AppMacosHostPairingRevoked), String> {
        self.ensure_available()?;
        let path = self
            .path
            .clone()
            .ok_or_else(|| "pairing store is not initialized".to_owned())?;
        let record = self
            .record
            .as_ref()
            .ok_or_else(|| "pairing store is empty".to_owned())?;
        if record.active.is_none() && record.pending.is_none() {
            return record
                .revocation
                .clone()
                .map(|revoked| (record.generation_floor, revoked))
                .ok_or_else(|| "typed macOS pairing has no live generation to revoke".to_owned());
        }
        // Match the runtime store's transition-first generation semantics.
        let proposal = record
            .pending
            .as_ref()
            .map(|pending| &pending.proposal)
            .or_else(|| record.active.as_ref().map(|active| &active.proposal))
            .ok_or_else(|| "typed macOS pairing has no live proposal".to_owned())?;
        let key = decode_pairing_key(&proposal.signing_key_hex)
            .map_err(|_| "typed macOS pairing key is invalid".to_owned())?;
        let signed_revocation = desktop_identity.sign_revoked(
            AppMacosHostPairingRevoked::sign(proposal, &key, now_ms)
                .map_err(|_| "typed macOS revocation could not be signed".to_owned())?,
        )?;
        let current = proposal.generation;
        let mut revoked = record.clone();
        revoked.generation_floor = revoked
            .generation_floor
            .max(current)
            .checked_add(1)
            .ok_or_else(|| "typed macOS pairing generation space is exhausted".to_owned())?;
        revoked.active = None;
        revoked.pending = None;
        revoked.revocation = Some(signed_revocation.clone());
        revoked.attested_bootstrap = None;
        // Commit the physical-owner tombstone before acknowledging or changing
        // the in-memory record. A failed durable write therefore remains
        // retryable with the same signed request and cannot resurrect a
        // memory-only revoke after restart.
        persist_pairing_store(&path, &revoked)?;
        let generation = revoked.generation_floor;
        self.record = Some(revoked);
        verifier.clear_verifier();
        Ok((generation, signed_revocation))
    }

    fn ensure_available(&self) -> Result<(), String> {
        if self.unavailable {
            Err("typed macOS pairing store is unavailable or corrupt".to_owned())
        } else {
            Ok(())
        }
    }
}

fn validate_persisted_pairing_record(
    record: &PersistedDesktopPairingStore,
    desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
) -> Result<(), String> {
    if record.pending.is_some() && record.attested_bootstrap.is_some() {
        return Err("persisted typed macOS pairing has two bootstrap owners".to_owned());
    }
    if let Some(bootstrap) = record.attested_bootstrap.as_ref() {
        validate_persisted_bootstrap(bootstrap, &record.host_identity_digest, desktop_identity)?;
    }
    if let Some(anchor) = record.reset_anchor.as_ref() {
        anchor
            .acknowledgment
            .verify(&anchor.challenge, anchor.acknowledgment.approved_at_ms)
            .map_err(|_| "persisted typed macOS reset acknowledgment is invalid".to_owned())?;
        let (key_id, public_key, digest) = desktop_identity.public_identity()?;
        if anchor.acknowledgment.host_identity_digest != record.host_identity_digest
            || anchor.acknowledgment.prior_generation_floor > record.generation_floor
            || anchor.acknowledgment.desktop_identity_key_id != key_id
            || anchor.acknowledgment.desktop_identity_public_key_hex != public_key
            || anchor.acknowledgment.desktop_identity_digest != digest
        {
            return Err("persisted typed macOS reset owner is invalid".to_owned());
        }
    }
    if let Some(pending) = record.pending.as_ref() {
        validate_persisted_bootstrap(
            &pending.bootstrap,
            &record.host_identity_digest,
            desktop_identity,
        )?;
        pending
            .proposal
            .verify(pending.proposal.issued_at_ms)
            .map_err(|_| "persisted typed macOS proposal is invalid".to_owned())?;
        validate_persisted_rotation_lineage(
            &pending.proposal,
            record.active.as_ref().map(|active| &active.proposal),
            record.generation_floor,
        )?;
        let attestation_digest = pending
            .bootstrap
            .attestation
            .digest()
            .map_err(|_| "persisted typed macOS attestation digest is invalid".to_owned())?;
        if pending.proposal.desktop_identity_attestation_digest != attestation_digest {
            return Err("persisted typed macOS proposal bootstrap is invalid".to_owned());
        }
        match (&pending.approval, &pending.cua_driver_binary) {
            (Some(approval), Some(binary)) => {
                approval
                    .verify(&pending.proposal, approval.approved_at_ms)
                    .map_err(|_| "persisted typed macOS approval is invalid".to_owned())?;
                desktop_identity.verify_approval(approval)?;
                if approval.host_identity_digest != record.host_identity_digest
                    || !valid_cua_driver_binary(binary)
                {
                    return Err("persisted typed macOS approval owner is invalid".to_owned());
                }
            },
            (None, None) => {},
            _ => return Err("persisted typed macOS approval is incomplete".to_owned()),
        }
    }
    if let Some(revoked) = record.revocation.as_ref() {
        if !valid_blake3_digest(&revoked.proposal_digest) {
            return Err("persisted typed macOS revocation digest is invalid".to_owned());
        }
        desktop_identity.verify_revoked(revoked)?;
    }
    Ok(())
}

fn validate_persisted_bootstrap(
    bootstrap: &PersistedAttestedBootstrap,
    host_identity_digest: &str,
    desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
) -> Result<(), String> {
    if bootstrap.attestation.host_identity_digest != host_identity_digest {
        return Err("persisted desktop bootstrap host identity is invalid".to_owned());
    }
    desktop_identity.verify_attestation(&bootstrap.challenge, &bootstrap.attestation)
}

fn validate_persisted_rotation_lineage(
    pending: &AppMacosHostPairingProposal,
    active: Option<&AppMacosHostPairingProposal>,
    generation_floor: u64,
) -> Result<(), String> {
    if pending.generation <= generation_floor {
        return Err("persisted typed macOS proposal generation is stale".to_owned());
    }
    match active {
        Some(active) => {
            if pending.generation != active.generation.saturating_add(1) {
                return Err("persisted typed macOS rotation generation is invalid".to_owned());
            }
            let previous_key = decode_pairing_key(&active.signing_key_hex)
                .map_err(|_| "persisted active typed macOS key is invalid".to_owned())?;
            pending
                .verify_rotation(active.generation, &previous_key)
                .map_err(|_| "persisted typed macOS rotation is unauthorized".to_owned())
        },
        None if pending.previous_generation.is_some() => {
            Err("persisted typed macOS rotation has no active predecessor".to_owned())
        },
        None => Ok(()),
    }
}

fn max_persisted_tcc_epoch(active: Option<u64>, pending: Option<u64>) -> u64 {
    active.unwrap_or(0).max(pending.unwrap_or(0))
}

fn valid_cua_driver_binary(binary: &Path) -> bool {
    binary.is_absolute()
        && std::fs::symlink_metadata(binary)
            .map(|metadata| metadata.is_file() && !metadata.file_type().is_symlink())
            .unwrap_or(false)
}

fn valid_blake3_digest(value: &str) -> bool {
    value.len() == 71
        && value.starts_with("blake3:")
        && value[7..]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn verifier_binary_requirement(
    active: &PersistedActivePairing,
) -> AppMacosVerifierBinaryRequirement {
    AppMacosVerifierBinaryRequirement {
        setup_id: active.proposal.setup_id.clone(),
        generation: active.proposal.generation,
        key_id: active.proposal.key_id.clone(),
        binary: active.cua_driver_binary.clone(),
        binary_digest: active.approval.cua_driver_binary_digest.clone(),
        owner_profile_digest: active.finalization.owner_profile_digest.clone(),
        owner_implementation_digest: active.finalization.owner_implementation_digest.clone(),
    }
}

fn active_matches_requirement(
    active: &PersistedActivePairing,
    requirement: &AppMacosVerifierBinaryRequirement,
) -> bool {
    active.proposal.setup_id == requirement.setup_id
        && active.proposal.generation == requirement.generation
        && active.proposal.key_id == requirement.key_id
        && active.cua_driver_binary == requirement.binary
        && active.approval.cua_driver_binary_digest == requirement.binary_digest
        && active.finalization.owner_profile_digest == requirement.owner_profile_digest
        && active.finalization.owner_implementation_digest
            == requirement.owner_implementation_digest
}

fn install_active_pairing(
    active: &PersistedActivePairing,
    verifier: &mut AppMacosHostState,
    desktop_identity: &crate::app_macos_identity::AppMacosDesktopIdentityOwner,
    prevalidated_binary: AppMacosPrevalidatedCuaBinary,
) -> Result<(), String> {
    validate_persisted_bootstrap(
        &active.bootstrap,
        &active.approval.host_identity_digest,
        desktop_identity,
    )?;
    if active.proposal.desktop_identity_attestation_digest
        != active
            .bootstrap
            .attestation
            .digest()
            .map_err(|_| "persisted typed macOS attestation digest is invalid".to_owned())?
        || !valid_cua_driver_binary(&active.cua_driver_binary)
    {
        return Err("persisted typed macOS activation owner is invalid".to_owned());
    }
    if prevalidated_binary.path() != active.cua_driver_binary.as_path()
        || prevalidated_binary.digest() != active.approval.cua_driver_binary_digest
    {
        return Err("persisted typed macOS CUA binary identity changed".to_owned());
    }
    let key = decode_pairing_key(&active.proposal.signing_key_hex)
        .map_err(|_| "persisted typed macOS pairing key is invalid".to_owned())?;
    active
        .finalization
        .verify(
            &active.proposal,
            &active.approval,
            active.finalization.finalized_at_ms,
        )
        .map_err(|_| "persisted typed macOS finalization is invalid".to_owned())?;
    active
        .finalized
        .verify(&active.finalization, &key)
        .map_err(|_| "persisted typed macOS activation acknowledgment is invalid".to_owned())?;
    desktop_identity.verify_approval(&active.approval)?;
    desktop_identity.verify_finalized(&active.finalized)?;
    let application_identities = active
        .approval
        .reviewed_targets
        .iter()
        .map(|target| {
            (
                target.bundle_id.clone(),
                target.application_identity_digest.clone(),
            )
        })
        .collect::<HashMap<_, _>>();
    verifier
        .install_verifier(
            active.proposal.key_id.clone(),
            key,
            active.approval.host_identity_digest.clone(),
            active.finalization.owner_profile_digest.clone(),
            active.finalization.owner_implementation_digest.clone(),
            active.approval.cua_driver_binary_digest.clone(),
            active.approval.tcc_policy_digest.clone(),
            active.approval.tcc_epoch,
            application_identities,
            prevalidated_binary,
        )
        .map_err(|error| error.to_string())
}

fn new_host_identity_digest() -> String {
    let id = uuid::Uuid::new_v4();
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.desktop.app-macos-host-identity.v1\0");
    hasher.update(id.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn read_pairing_store(path: &Path) -> Result<Option<PersistedDesktopPairingStore>, String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "could not inspect typed macOS pairing store: {error}"
            ))
        },
    };
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() == 0
        || metadata.len() > APP_MACOS_DESKTOP_PAIRING_MAX_BYTES
        || !owner_only_permissions(&metadata)
    {
        return Err(
            "typed macOS pairing store is not a bounded owner-only regular file".to_owned(),
        );
    }
    let file = File::open(path)
        .map_err(|error| format!("could not open typed macOS pairing store: {error}"))?;
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    file.take(APP_MACOS_DESKTOP_PAIRING_MAX_BYTES.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read typed macOS pairing store: {error}"))?;
    if bytes.len() as u64 > APP_MACOS_DESKTOP_PAIRING_MAX_BYTES {
        return Err("typed macOS pairing store exceeds its byte ceiling".to_owned());
    }
    let record: PersistedDesktopPairingStore = serde_json::from_slice(&bytes)
        .map_err(|_| "typed macOS pairing store is corrupt".to_owned())?;
    if record.schema != APP_MACOS_DESKTOP_PAIRING_STORE_V3
        || record.tcc_epoch_floor
            < max_persisted_tcc_epoch(
                record
                    .active
                    .as_ref()
                    .map(|active| active.approval.tcc_epoch),
                record
                    .pending
                    .as_ref()
                    .and_then(|pending| pending.approval.as_ref())
                    .map(|approval| approval.tcc_epoch),
            )
        || record.generation_floor
            < record
                .active
                .as_ref()
                .map(|active| active.proposal.generation)
                .unwrap_or(0)
        || record.active.as_ref().is_some_and(|active| {
            active.approval.host_identity_digest != record.host_identity_digest
        })
        || record.attested_bootstrap.as_ref().is_some_and(|bootstrap| {
            bootstrap.attestation.host_identity_digest != record.host_identity_digest
                || bootstrap
                    .attestation
                    .verify(&bootstrap.challenge, bootstrap.attestation.attested_at_ms)
                    .is_err()
        })
        || record.revocation.as_ref().is_some_and(|revoked| {
            record.active.is_some()
                || record.pending.is_some()
                || revoked.schema != APP_MACOS_HOST_PAIRING_V1
                || revoked.generation == 0
                || revoked.revoked_at_ms < 0
                || record.generation_floor <= revoked.generation
        })
        || record.reset_anchor.as_ref().is_some_and(|anchor| {
            anchor.acknowledgment.host_identity_digest != record.host_identity_digest
                || anchor.acknowledgment.prior_generation_floor > record.generation_floor
                || anchor.acknowledgment.challenge_digest
                    != anchor.challenge.digest().unwrap_or_default()
        })
    {
        return Err("typed macOS pairing store has invalid identity or generation".to_owned());
    }
    Ok(Some(record))
}

fn persist_pairing_store(path: &Path, record: &PersistedDesktopPairingStore) -> Result<(), String> {
    let bytes = serde_json::to_vec(record)
        .map_err(|_| "typed macOS pairing store could not be encoded".to_owned())?;
    if bytes.is_empty() || bytes.len() as u64 > APP_MACOS_DESKTOP_PAIRING_MAX_BYTES {
        return Err("typed macOS pairing store exceeds its byte ceiling".to_owned());
    }
    let parent = path
        .parent()
        .ok_or_else(|| "typed macOS pairing store has no parent".to_owned())?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("could not create typed macOS pairing directory: {error}"))?;
    if std::fs::symlink_metadata(parent)
        .map(|metadata| !metadata.is_dir() || metadata.file_type().is_symlink())
        .unwrap_or(true)
    {
        return Err("typed macOS pairing directory is not a regular directory".to_owned());
    }
    let temporary = path.with_extension(format!("json.{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), String> {
        let mut file = options.open(&temporary).map_err(|error| {
            format!("could not create typed macOS pairing transaction: {error}")
        })?;
        file.write_all(&bytes)
            .map_err(|error| format!("could not write typed macOS pairing transaction: {error}"))?;
        file.sync_all()
            .map_err(|error| format!("could not sync typed macOS pairing transaction: {error}"))?;
        std::fs::rename(&temporary, path).map_err(|error| {
            format!("could not publish typed macOS pairing transaction: {error}")
        })?;
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("could not sync typed macOS pairing directory: {error}"))?;
        Ok(())
    })();
    if write_result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    write_result
}

#[cfg(unix)]
fn owner_only_permissions(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o077 == 0
}

#[cfg(not(unix))]
fn owner_only_permissions(_metadata: &std::fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(byte: u8) -> String {
        format!("blake3:{}", format!("{byte:02x}").repeat(32))
    }

    #[test]
    fn corrupt_or_overbroad_pairing_files_never_become_empty_authority() {
        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-pairing-test-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&root).expect("root");
        let path = root.join("pairing.json");
        std::fs::write(&path, b"not-json").expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("mode");
        }
        assert!(read_pairing_store(&path).is_err());
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }

    #[test]
    fn persisted_rotation_requires_exact_predecessor_and_tcc_high_water() {
        let now_ms = 1_000_000_i64;
        let first_key = [7_u8; 32];
        let second_key = [8_u8; 32];
        let targets = vec![
            magician_app_contract::macos_host::AppMacosHostPairingTargetRequest {
                target_ref: "runtime:macos-target:editor".to_owned(),
                bundle_id: "com.example.Editor".to_owned(),
            },
        ];
        let active = AppMacosHostPairingProposal::mint(
            "setup:active".to_owned(),
            1,
            "key:active".to_owned(),
            &first_key,
            "scope:rotation".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(1),
            digest(2),
            targets.clone(),
            now_ms,
            now_ms + 60_000,
        )
        .expect("active proposal");
        let rotation = AppMacosHostPairingProposal::mint_rotation(
            "setup:rotation".to_owned(),
            2,
            1,
            "key:rotation".to_owned(),
            &second_key,
            &first_key,
            "scope:rotation".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(1),
            digest(3),
            targets.clone(),
            now_ms,
            now_ms + 60_000,
        )
        .expect("rotation proposal");
        assert!(validate_persisted_rotation_lineage(&rotation, Some(&active), 1).is_ok());
        assert!(validate_persisted_rotation_lineage(&rotation, None, 1).is_err());

        let self_hmac = AppMacosHostPairingProposal::mint(
            "setup:self-hmac".to_owned(),
            2,
            "key:self-hmac".to_owned(),
            &second_key,
            "scope:rotation".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(1),
            digest(4),
            targets,
            now_ms,
            now_ms + 60_000,
        )
        .expect("self-HMAC proposal");
        assert!(validate_persisted_rotation_lineage(&self_hmac, Some(&active), 1).is_err());
        assert_eq!(max_persisted_tcc_epoch(Some(4), Some(9)), 9);
        assert_eq!(max_persisted_tcc_epoch(Some(11), Some(6)), 11);
    }

    #[test]
    fn signed_runtime_revoke_durably_tombstones_the_physical_owner() {
        let root = std::env::temp_dir().join(format!(
            "magician-app-macos-revoke-test-{}",
            uuid::Uuid::new_v4()
        ));
        let path = root.join("pairing.json");
        let now_ms = 1_000_000_i64;
        let key = [9_u8; 32];
        let mut owner = AppMacosDesktopPairingOwner::default();
        let mut verifier = AppMacosHostState::default();
        let mut desktop_identity =
            crate::app_macos_identity::AppMacosDesktopIdentityOwner::for_test();
        let enrollment = desktop_identity
            .begin_owner_approval(now_ms)
            .expect("desktop identity");
        owner
            .initialize(path.clone(), &mut verifier, &desktop_identity)
            .expect("initialize");
        let challenge = AppMacosDesktopIdentityChallenge::mint(
            enrollment.desktop_identity_key_id.clone(),
            enrollment.desktop_identity_public_key_hex.clone(),
            "nonce:desktop-bootstrap:test".to_owned(),
            magician_app_contract::macos_host::app_macos_desktop_owner_approval_code_digest(
                &enrollment.owner_approval_code,
            )
            .expect("owner code digest"),
            now_ms,
            now_ms + 30_000,
        )
        .expect("challenge");
        let attestation = desktop_identity
            .attest(
                &challenge,
                owner.ensure_host_identity_digest().expect("host identity"),
                now_ms,
            )
            .expect("attestation");
        let attestation_digest = attestation.digest().expect("attestation digest");
        owner
            .record_identity_attestation(challenge.clone(), attestation, now_ms)
            .expect("persist attestation");
        desktop_identity
            .consume_owner_approval(&challenge, now_ms)
            .expect("consume native approval");
        let unbound_proposal = AppMacosHostPairingProposal::mint(
            "setup:direct-bypass".to_owned(),
            1,
            "key:direct-bypass".to_owned(),
            &key,
            "scope:runtime-revoke".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(1),
            digest(99),
            vec![
                magician_app_contract::macos_host::AppMacosHostPairingTargetRequest {
                    target_ref: "runtime:macos-target:editor".to_owned(),
                    bundle_id: "com.example.Editor".to_owned(),
                },
            ],
            now_ms,
            now_ms + 60_000,
        )
        .expect("self-HMAC proposal");
        assert!(owner
            .receive_proposal(unbound_proposal, now_ms, &mut verifier)
            .is_err());
        let proposal = AppMacosHostPairingProposal::mint(
            "setup:runtime-revoke".to_owned(),
            1,
            "key:runtime-revoke".to_owned(),
            &key,
            "scope:runtime-revoke".to_owned(),
            "http://127.0.0.1:3017/host/apps/macos/action".to_owned(),
            digest(1),
            attestation_digest,
            vec![
                magician_app_contract::macos_host::AppMacosHostPairingTargetRequest {
                    target_ref: "runtime:macos-target:editor".to_owned(),
                    bundle_id: "com.example.Editor".to_owned(),
                },
            ],
            now_ms,
            now_ms + 60_000,
        )
        .expect("proposal");
        let request =
            AppMacosHostPairingStatusRequest::mint(&proposal, &key, now_ms, now_ms + 30_000)
                .expect("revoke capability");
        owner
            .receive_proposal(proposal.clone(), now_ms, &mut verifier)
            .expect("pending proposal");
        let revoked = owner
            .revoke_from_runtime(&request, now_ms, &mut verifier, &desktop_identity)
            .expect("revoke");
        revoked
            .verify(&proposal, &key, now_ms)
            .expect("signed revoke receipt");
        revoked
            .verify_desktop_identity(
                &enrollment.desktop_identity_key_id,
                &enrollment.desktop_identity_public_key_hex,
            )
            .expect("desktop identity revoke receipt");
        let persisted = read_pairing_store(&path).expect("read").expect("record");
        assert!(persisted.active.is_none());
        assert!(persisted.pending.is_none());
        assert_eq!(persisted.generation_floor, 2);
        assert_eq!(persisted.revocation.as_ref(), Some(&revoked));
        assert_eq!(
            owner
                .revoke_from_runtime(&request, now_ms + 1, &mut verifier, &desktop_identity,)
                .expect("idempotent revoke receipt"),
            revoked
        );
        let host_identity = persisted.host_identity_digest.clone();
        let (_, _, desktop_digest) = desktop_identity
            .public_identity()
            .expect("desktop identity");
        let reset_challenge = AppMacosHostPairingResetChallenge::mint(
            "scope:runtime-revoke".to_owned(),
            "reset:runtime-store-loss:1".to_owned(),
            now_ms + 2,
            now_ms + 30_000,
        )
        .expect("reset challenge");
        let reset_ack = owner
            .reset_after_owner_confirmation(
                reset_challenge.clone(),
                &host_identity,
                2,
                &desktop_digest,
                now_ms + 3,
                &mut verifier,
                &desktop_identity,
            )
            .expect("explicit reset");
        reset_ack
            .verify(&reset_challenge, now_ms + 90_000)
            .expect("late reset delivery");
        let reset_record = read_pairing_store(&path)
            .expect("read reset")
            .expect("reset record");
        assert_eq!(reset_record.generation_floor, 2);
        assert!(reset_record.revocation.is_none());
        assert!(reset_record.reset_anchor.is_some());
        assert_eq!(
            owner
                .reset_after_owner_confirmation(
                    reset_challenge,
                    &host_identity,
                    2,
                    &desktop_digest,
                    now_ms + 90_000,
                    &mut verifier,
                    &desktop_identity,
                )
                .expect("idempotent late reset"),
            reset_ack
        );
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir(root);
    }
}

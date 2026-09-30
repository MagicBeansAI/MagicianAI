//! Reviewed authority choices for Android Apps automation.
//!
//! Both choices retain hardware-backed Android Key Attestation and the
//! desktop owner's signed approval. Google Play builds add a freshly decoded
//! Play Integrity verdict. Private builds replace only that Google dependency
//! with exact deployment-owned signer, root, and version pins.

use std::collections::BTreeSet;

use magician_app_contract::android_owner::AppAndroidAutomationTrustMode;

use crate::{
    android_apps_attestation::AndroidAppsAttestationPolicy,
    android_play_integrity::AndroidPlayIntegrityPolicy, device_pairing_api::MobileEnrollmentConfig,
};

#[derive(Clone)]
pub(crate) struct AndroidAutomationTrustPolicy {
    mode: AppAndroidAutomationTrustMode,
    attestation: AndroidAppsAttestationPolicy,
    play_integrity: Option<AndroidPlayIntegrityPolicy>,
}

impl AndroidAutomationTrustPolicy {
    pub(crate) fn reviewed(
        mode: AppAndroidAutomationTrustMode,
        config: &MobileEnrollmentConfig,
    ) -> Result<Self, &'static str> {
        let play_integrity = match mode {
            AppAndroidAutomationTrustMode::PlayIntegrity => {
                let common_versions = config
                    .android_apps_version_codes
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                let play_versions = config
                    .android_play_integrity_version_codes
                    .iter()
                    .copied()
                    .collect::<BTreeSet<_>>();
                if common_versions != play_versions {
                    return Err("android_play_integrity_version_policy_mismatch");
                }
                Some(
                    AndroidPlayIntegrityPolicy::reviewed(
                        config.android_play_integrity_cloud_project_number,
                        &config.android_play_integrity_version_codes,
                        &config.android_apps_signing_sha256,
                        &config.android_play_integrity_required_device_verdicts,
                        config
                            .android_play_integrity_service_account_path
                            .as_deref(),
                    )
                    .map_err(|_| "android_play_integrity_unavailable")?,
                )
            },
            AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild => None,
        };
        let external_digest = play_integrity
            .as_ref()
            .map(AndroidPlayIntegrityPolicy::digest)
            .map(str::to_owned)
            .unwrap_or_else(private_build_authority_digest);
        // A private build with no signer or version pinned in the config
        // learns them from the first attestation the owner approves: the
        // hardware attestation proves the build, the desktop approval shows
        // it, and the approved identity is pinned to it. A Play build keeps
        // needing its reviewed pins — the Play verdict policy is keyed to
        // explicit version codes.
        let learn_private_build = mode == AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild
            && (config.android_apps_signing_sha256.is_empty()
                || config.android_apps_version_codes.is_empty());
        let attestation = if learn_private_build {
            AndroidAppsAttestationPolicy::learning(
                &config.android_attestation_root_sha256,
                &config.android_apps_apk_sha256,
                &external_digest,
            )
        } else {
            AndroidAppsAttestationPolicy::reviewed(
                &config.android_apps_signing_sha256,
                &config.android_attestation_root_sha256,
                &config.android_apps_apk_sha256,
                &config.android_apps_version_codes,
                &external_digest,
            )
        }
        .map_err(|_| "android_apps_attestation_policy_unavailable")?;
        Ok(Self {
            mode,
            attestation,
            play_integrity,
        })
    }

    /// Whether this policy pins the build at the owner's approval rather
    /// than from the config.
    pub(crate) fn learns_build(&self) -> bool {
        self.attestation.learns_build()
    }

    /// This policy pinned to one build — the shape an identity enrolled
    /// under a learning policy was recorded with.
    pub(crate) fn pinned_to_build(
        &self,
        app_signing_sha256: &str,
        app_version_code: u64,
    ) -> Option<Self> {
        let attestation = self
            .attestation
            .pinned_to_build(app_signing_sha256, app_version_code)
            .ok()?;
        Some(Self {
            mode: self.mode,
            attestation,
            play_integrity: self.play_integrity.clone(),
        })
    }

    pub(crate) fn mode(&self) -> AppAndroidAutomationTrustMode {
        self.mode
    }

    pub(crate) fn attestation(&self) -> &AndroidAppsAttestationPolicy {
        &self.attestation
    }

    pub(crate) fn play_integrity(&self) -> Option<&AndroidPlayIntegrityPolicy> {
        self.play_integrity.as_ref()
    }

    pub(crate) fn cloud_project_number(&self) -> Option<u64> {
        self.play_integrity
            .as_ref()
            .map(|policy| policy.cloud_project_number)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn admits_persisted_identity(
        &self,
        app_package: &str,
        app_version_code: u64,
        app_signing_sha256: &str,
        apk_sha256: &str,
        attestation_root_sha256: &str,
        attestation_security_level: &str,
        policy_digest: &str,
    ) -> bool {
        self.attestation.admits_persisted_identity(
            app_package,
            app_version_code,
            app_signing_sha256,
            apk_sha256,
            attestation_root_sha256,
            attestation_security_level,
            policy_digest,
        )
    }

    pub(crate) fn private_socket_verdict_digest(
        &self,
        signed_material: &[u8],
        signature: &[u8],
    ) -> Option<String> {
        (self.mode == AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild).then(|| {
            let mut hasher = blake3::Hasher::new();
            hasher.update(b"magician.android-private-build.socket-verdict.v1\0");
            for value in [
                self.attestation.digest().as_bytes(),
                signed_material,
                signature,
            ] {
                hasher.update(&(value.len() as u64).to_le_bytes());
                hasher.update(value);
            }
            format!("blake3:{}", hasher.finalize().to_hex())
        })
    }
}

/// The reviewed policy an identity was enrolled under, found by its digest:
/// each mode's config policy, and — for an identity enrolled under a
/// learning private-build policy — that policy pinned to the identity's
/// own signer and version, which is exactly what the enrollment recorded.
/// A config change or an unapproved build still finds nothing.
pub(crate) fn reviewed_policy_for_identity(
    config: &MobileEnrollmentConfig,
    policy_digest: &str,
    build: (&str, u64),
) -> Option<AndroidAutomationTrustPolicy> {
    [
        AppAndroidAutomationTrustMode::PlayIntegrity,
        AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
    ]
    .into_iter()
    .filter_map(|mode| AndroidAutomationTrustPolicy::reviewed(mode, config).ok())
    .flat_map(|policy| {
        let pinned = if policy.learns_build() {
            policy.pinned_to_build(build.0, build.1)
        } else {
            None
        };
        [Some(policy), pinned].into_iter().flatten()
    })
    .find(|policy| policy.attestation.digest() == policy_digest)
}

fn private_build_authority_digest() -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-private-build-authority.v1\0");
    hasher.update(
        AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild
            .as_str()
            .as_bytes(),
    );
    format!("blake3:{}", hasher.finalize().to_hex())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> MobileEnrollmentConfig {
        MobileEnrollmentConfig {
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
        }
    }

    #[test]
    fn private_build_needs_common_pins_but_no_google_credentials() {
        assert!(AndroidAutomationTrustPolicy::reviewed(
            AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
            &config(),
        )
        .is_ok());
        assert!(AndroidAutomationTrustPolicy::reviewed(
            AppAndroidAutomationTrustMode::PlayIntegrity,
            &config(),
        )
        .is_err());
    }

    #[test]
    fn a_private_build_without_config_pins_learns_them_at_approval() {
        let mut unpinned = config();
        unpinned.android_apps_signing_sha256.clear();
        unpinned.android_apps_version_codes.clear();
        unpinned.android_attestation_root_sha256.clear();
        // Reviewed, learning, on Google's published roots.
        let learning = AndroidAutomationTrustPolicy::reviewed(
            AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
            &unpinned,
        )
        .expect("a private build learns its pins");
        assert!(learning.learns_build());
        // A Play build never learns: its verdict policy is keyed to versions.
        assert!(AndroidAutomationTrustPolicy::reviewed(
            AppAndroidAutomationTrustMode::PlayIntegrity,
            &unpinned,
        )
        .is_err());
        // Pinned to the attested build, the digest is what an approved
        // identity records; a different build is a different digest.
        let pinned = learning
            .pinned_to_build(&"c".repeat(64), 42)
            .expect("pinned");
        assert!(!pinned.learns_build());
        let other = learning
            .pinned_to_build(&"d".repeat(64), 42)
            .expect("pinned");
        assert_ne!(pinned.attestation().digest(), other.attestation().digest());
        assert_ne!(
            pinned.attestation().digest(),
            learning.attestation().digest()
        );
        // Reconnect finds the pinned policy from the identity's own build…
        let found = reviewed_policy_for_identity(
            &unpinned,
            pinned.attestation().digest(),
            (&"c".repeat(64), 42),
        )
        .expect("the identity's build reopens its policy");
        assert!(found.admits_persisted_identity(
            magician_app_contract::android_owner::APP_ANDROID_OWNER_COMPANION_PACKAGE,
            42,
            &"c".repeat(64),
            "",
            super::super::android_apps_attestation::DEFAULT_ANDROID_ATTESTATION_ROOT_SHA256[0],
            "tee",
            pinned.attestation().digest(),
        ));
        // …and nothing for a build the owner never approved, or after the
        // config pins something else.
        assert!(reviewed_policy_for_identity(
            &unpinned,
            pinned.attestation().digest(),
            (&"d".repeat(64), 42),
        )
        .is_none());
        assert!(reviewed_policy_for_identity(
            &config(),
            pinned.attestation().digest(),
            (&"c".repeat(64), 42),
        )
        .is_none());
        // A malformed build never pins.
        assert!(learning.pinned_to_build("nope", 42).is_none());
        assert!(learning.pinned_to_build(&"c".repeat(64), 0).is_none());
    }

    #[test]
    fn a_config_pinned_identity_keeps_its_digest_when_pinned_to_its_own_build() {
        let pinned = AndroidAutomationTrustPolicy::reviewed(
            AppAndroidAutomationTrustMode::OwnerPinnedPrivateBuild,
            &config(),
        )
        .unwrap();
        assert!(!pinned.learns_build());
        let same = pinned.pinned_to_build(&"a".repeat(64), 21).unwrap();
        assert_eq!(same.attestation().digest(), pinned.attestation().digest());
        assert!(reviewed_policy_for_identity(
            &config(),
            pinned.attestation().digest(),
            (&"a".repeat(64), 21),
        )
        .is_some());
    }

    #[test]
    fn play_versions_must_match_the_common_reviewed_versions() {
        let mut config = config();
        config.android_play_integrity_cloud_project_number = Some(123);
        config.android_play_integrity_version_codes = vec![20];
        config.android_play_integrity_required_device_verdicts =
            vec!["MEETS_DEVICE_INTEGRITY".to_owned()];
        config.android_play_integrity_service_account_path = Some("/private/key.json".to_owned());
        assert_eq!(
            AndroidAutomationTrustPolicy::reviewed(
                AppAndroidAutomationTrustMode::PlayIntegrity,
                &config,
            )
            .err()
            .unwrap(),
            "android_play_integrity_version_policy_mismatch",
        );
    }
}

//! Android hardware-key evidence for the Apps automation owner.
//!
//! A legacy device bearer proves only that somebody obtained a pairing token.
//! This verifier additionally proves that the one-time owner challenge reached
//! the reviewed Magdroid package and that a hardware-backed Android Keystore
//! key signed the exact enrollment. Only the small pinned identity projection
//! leaves this module; certificate bytes and Android authorization-list details
//! are deliberately not persisted.

use std::{cmp::Ordering, collections::BTreeSet};

use base64::Engine as _;
use openssl::{
    asn1::Asn1Time,
    hash::{hash, MessageDigest},
    nid::Nid,
    pkey::{PKey, Public},
    sign::Verifier,
    stack::Stack,
    x509::{
        store::X509StoreBuilder,
        verify::{X509VerifyFlags, X509VerifyParam},
        X509StoreContext, X509,
    },
};

use magician::magician_v2::device_pairing::DeviceAutomationIdentity;

const EXPECTED_PACKAGE: &str =
    magician_app_contract::android_owner::APP_ANDROID_OWNER_COMPANION_PACKAGE;
const MAX_CERTIFICATES: usize = 8;
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;
const MAX_CHAIN_BYTES: usize = 64 * 1024;
const MAX_KEY_ID_BYTES: usize = 128;
const ANDROID_KEY_ATTESTATION_OID: &[u8] =
    &[0x2b, 0x06, 0x01, 0x04, 0x01, 0xd6, 0x79, 0x02, 0x01, 0x11];

#[derive(Debug, thiserror::Error)]
pub(crate) enum AndroidAppsAttestationError {
    #[error("Android Apps attestation policy is not configured")]
    PolicyUnavailable,
    #[error("Android Apps attestation evidence is malformed")]
    Malformed,
    #[error("Android Apps attestation chain is not trusted")]
    Untrusted,
    #[error("Android Apps enrollment proof is invalid")]
    InvalidProof,
}

/// Google's published Android hardware-attestation roots
/// (`https://android.googleapis.com/attestation/root`), the same two the
/// config seed carries. A private-build deployment whose live config
/// predates them, or never listed any, verifies against these; a device
/// whose chain ends elsewhere still fails closed.
pub(crate) const DEFAULT_ANDROID_ATTESTATION_ROOT_SHA256: [&str; 2] = [
    "cedb1cb6dc896ae5ec797348bce9286753c2b38ee71ce0fbe34a9a1248800dfc",
    "6d9db4ce6c5c0b293166d08986e05774a8776ceb525d9e4329520de12ba4bcc0",
];

#[derive(Clone)]
pub(crate) struct AndroidAppsAttestationPolicy {
    app_signing_sha256: BTreeSet<String>,
    attestation_root_sha256: BTreeSet<String>,
    apk_sha256: BTreeSet<String>,
    app_version_codes: BTreeSet<u64>,
    digest: String,
    /// The external authority digest this policy was reviewed with (Play
    /// Integrity's, or the private-build authority's), kept so a learning
    /// policy can be pinned to a build with the same authority.
    external_authority_policy_digest: String,
    /// A private-build policy with no signer or version pinned yet. The
    /// enrollment verifies the chain, the hardware-enforced claims, and the
    /// proof exactly as a pinned policy does; the build the attestation
    /// proves becomes the pin ([`Self::pinned_to_build`]) for the identity
    /// the owner then approves — the desktop approval shows the package,
    /// version and signer it is approving. Never persisted as-is: an
    /// identity always carries the digest of the pinned policy.
    learn_build: bool,
}

impl AndroidAppsAttestationPolicy {
    /// A private-build policy that learns its signer and version from the
    /// first attestation the owner approves. Roots still have to be
    /// reviewed: the config's, or [`DEFAULT_ANDROID_ATTESTATION_ROOT_SHA256`]
    /// when the config lists none.
    pub(crate) fn learning(
        attestation_root_sha256: &[String],
        apk_sha256: &[String],
        external_authority_policy_digest: &str,
    ) -> Result<Self, AndroidAppsAttestationError> {
        let roots: Vec<String> = if attestation_root_sha256.is_empty() {
            DEFAULT_ANDROID_ATTESTATION_ROOT_SHA256
                .iter()
                .map(|root| (*root).to_owned())
                .collect()
        } else {
            attestation_root_sha256.to_vec()
        };
        let attestation_root_sha256 = digest_set(&roots)?;
        let apk_sha256 = optional_digest_set(apk_sha256)?;
        if !external_authority_policy_digest.starts_with("blake3:")
            || external_authority_policy_digest.len() != 71
        {
            return Err(AndroidAppsAttestationError::PolicyUnavailable);
        }
        let app_signing_sha256 = BTreeSet::new();
        let app_version_codes = BTreeSet::new();
        let digest = attestation_policy_digest(
            &app_signing_sha256,
            &attestation_root_sha256,
            &apk_sha256,
            &app_version_codes,
            external_authority_policy_digest,
        );
        Ok(Self {
            app_signing_sha256,
            attestation_root_sha256,
            apk_sha256,
            app_version_codes,
            digest,
            external_authority_policy_digest: external_authority_policy_digest.to_owned(),
            learn_build: true,
        })
    }

    pub(crate) fn learns_build(&self) -> bool {
        self.learn_build
    }

    /// This policy with one build pinned: its signer and version added to
    /// the pins, the digest recomputed, learning off. A learning policy
    /// pinned to the attested build is what an approved identity records.
    /// Pinning a build a pinned policy already carries leaves its digest
    /// unchanged, so a config-pinned identity keeps its digest.
    pub(crate) fn pinned_to_build(
        &self,
        app_signing_sha256: &str,
        app_version_code: u64,
    ) -> Result<Self, AndroidAppsAttestationError> {
        let signer = app_signing_sha256.trim().to_ascii_lowercase();
        if signer.len() != 64
            || !signer.bytes().all(|b| b.is_ascii_hexdigit())
            || app_version_code == 0
        {
            return Err(AndroidAppsAttestationError::PolicyUnavailable);
        }
        let mut app_signing_sha256 = self.app_signing_sha256.clone();
        app_signing_sha256.insert(signer);
        let mut app_version_codes = self.app_version_codes.clone();
        app_version_codes.insert(app_version_code);
        let digest = attestation_policy_digest(
            &app_signing_sha256,
            &self.attestation_root_sha256,
            &self.apk_sha256,
            &app_version_codes,
            &self.external_authority_policy_digest,
        );
        Ok(Self {
            app_signing_sha256,
            attestation_root_sha256: self.attestation_root_sha256.clone(),
            apk_sha256: self.apk_sha256.clone(),
            app_version_codes,
            digest,
            external_authority_policy_digest: self.external_authority_policy_digest.clone(),
            learn_build: false,
        })
    }

    pub(crate) fn reviewed(
        app_signing_sha256: &[String],
        attestation_root_sha256: &[String],
        apk_sha256: &[String],
        app_version_codes: &[u64],
        external_authority_policy_digest: &str,
    ) -> Result<Self, AndroidAppsAttestationError> {
        let app_signing_sha256 = digest_set(app_signing_sha256)?;
        let attestation_root_sha256 = digest_set(attestation_root_sha256)?;
        let apk_sha256 = optional_digest_set(apk_sha256)?;
        let app_version_codes = app_version_codes
            .iter()
            .copied()
            .filter(|value| *value > 0)
            .collect::<BTreeSet<_>>();
        if app_version_codes.is_empty()
            || !external_authority_policy_digest.starts_with("blake3:")
            || external_authority_policy_digest.len() != 71
        {
            return Err(AndroidAppsAttestationError::PolicyUnavailable);
        }
        let digest = attestation_policy_digest(
            &app_signing_sha256,
            &attestation_root_sha256,
            &apk_sha256,
            &app_version_codes,
            external_authority_policy_digest,
        );
        Ok(Self {
            app_signing_sha256,
            attestation_root_sha256,
            apk_sha256,
            app_version_codes,
            digest,
            external_authority_policy_digest: external_authority_policy_digest.to_owned(),
            learn_build: false,
        })
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
        app_package == EXPECTED_PACKAGE
            && self.app_version_codes.contains(&app_version_code)
            && self.app_signing_sha256.contains(app_signing_sha256)
            && (self.apk_sha256.is_empty() || self.apk_sha256.contains(apk_sha256))
            && self
                .attestation_root_sha256
                .contains(attestation_root_sha256)
            && matches!(attestation_security_level, "tee" | "strongbox")
            && policy_digest == self.digest
    }

    pub(crate) fn digest(&self) -> &str {
        &self.digest
    }
}

fn attestation_policy_digest(
    app_signing_sha256: &BTreeSet<String>,
    attestation_root_sha256: &BTreeSet<String>,
    apk_sha256: &BTreeSet<String>,
    app_version_codes: &BTreeSet<u64>,
    external_authority_policy_digest: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"magician.android-apps-attestation-policy.v2\0");
    for component in [
        EXPECTED_PACKAGE.as_bytes(),
        b"hardware:sign:ec:p256:sha256:generated:tee-or-strongbox",
    ] {
        hasher.update(&(component.len() as u64).to_le_bytes());
        hasher.update(component);
    }
    for values in [app_signing_sha256, attestation_root_sha256, apk_sha256] {
        hasher.update(&(values.len() as u64).to_le_bytes());
        for value in values {
            hasher.update(&(value.len() as u64).to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    for version in app_version_codes {
        hasher.update(&version.to_le_bytes());
    }
    hasher.update(&(external_authority_policy_digest.len() as u64).to_le_bytes());
    hasher.update(external_authority_policy_digest.as_bytes());
    format!("blake3:{}", hasher.finalize().to_hex())
}

fn digest_set(values: &[String]) -> Result<BTreeSet<String>, AndroidAppsAttestationError> {
    let values = values
        .iter()
        .map(String::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    if values.is_empty()
        || values
            .iter()
            .any(|value| value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(AndroidAppsAttestationError::PolicyUnavailable);
    }
    Ok(values)
}

fn optional_digest_set(values: &[String]) -> Result<BTreeSet<String>, AndroidAppsAttestationError> {
    if values.is_empty() {
        return Ok(BTreeSet::new());
    }
    digest_set(values)
}

pub(crate) struct AndroidAppsEnrollmentEvidence<'a> {
    pub enrollment_id: &'a str,
    pub device_id: &'a str,
    pub label: &'a str,
    pub key_id: &'a str,
    pub public_key_spki_base64: &'a str,
    pub certificate_chain_base64: &'a [String],
    pub app_package: &'a str,
    pub app_version_code: u64,
    pub app_signing_sha256: &'a str,
    pub apk_sha256: &'a str,
    pub connection_secret_sha256: &'a str,
    pub signature_base64: &'a str,
}

/// Verify the full one-time challenge, X.509 lineage, Android authorization
/// list, reviewed package signer and enrollment signature.
/// Name the verifier stage a failure came from, content-free, so a
/// `Malformed` from a phone can be placed: which of bounds, decoding,
/// chain, key, extension, claims, or proof refused it.
fn at_stage<T>(
    stage: &'static str,
    result: Result<T, AndroidAppsAttestationError>,
) -> Result<T, AndroidAppsAttestationError> {
    if let Err(error) = &result {
        tracing::warn!(stage, error = ?error, "[ANDROID-ATTESTATION] refused at stage");
    }
    result
}

pub(crate) fn verify_android_apps_enrollment(
    policy: &AndroidAppsAttestationPolicy,
    evidence: AndroidAppsEnrollmentEvidence<'_>,
    challenge: &[u8],
    now_ms: i64,
) -> Result<DeviceAutomationIdentity, AndroidAppsAttestationError> {
    // Shape only. Whether the signer and version are the reviewed ones is a
    // trust decision made below with the signer, so a pinned policy refuses
    // an unreviewed version as Untrusted (`pins_mismatch`), not Malformed —
    // and a learning policy, whose version set is empty until it pins the
    // attested build, is not refused here at all (it was, on the first live
    // attempt).
    if evidence.enrollment_id.len() < 16
        || evidence.enrollment_id.len() > 128
        || evidence.device_id.is_empty()
        || evidence.device_id.len() > 256
        || evidence.label.is_empty()
        || evidence.label.len() > 256
        || evidence.key_id.len() < 16
        || evidence.key_id.len() > MAX_KEY_ID_BYTES
        || evidence.app_package != EXPECTED_PACKAGE
        || evidence.app_version_code == 0
        || evidence.connection_secret_sha256.len() != 64
        || !evidence
            .connection_secret_sha256
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
        || challenge.len() != 32
    {
        tracing::warn!(stage = "bounds", "[ANDROID-ATTESTATION] refused at stage");
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let app_signing_sha256 = evidence.app_signing_sha256.to_ascii_lowercase();
    // A pinned policy admits only its reviewed signer and version — here,
    // at enrollment, not first at reconnect. A learning policy admits the
    // build the hardware attestation proves below and is pinned to it
    // before the identity leaves this function.
    if !policy.learn_build
        && (!policy.app_signing_sha256.contains(&app_signing_sha256)
            || !policy
                .app_version_codes
                .contains(&evidence.app_version_code))
    {
        tracing::warn!(reason = "pins_mismatch", "[ANDROID-ATTESTATION] rejected");
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    let apk_sha256 = evidence.apk_sha256.to_ascii_lowercase();
    // Diagnostic only. The application can sign a PackageManager checksum it
    // obtained itself; exact artifact authority comes from the independently
    // server-decoded Play Integrity verdict bound to this enrollment proof.

    let spki = at_stage(
        "spki",
        decode_bounded(evidence.public_key_spki_base64, 1024),
    )?;
    let signature = at_stage("signature", decode_bounded(evidence.signature_base64, 256))?;
    let chain_der = at_stage(
        "chain_decode",
        decode_chain(evidence.certificate_chain_base64),
    )?;
    let certificates = at_stage(
        "chain_x509",
        chain_der
            .iter()
            .map(|der| X509::from_der(der).map_err(|_| AndroidAppsAttestationError::Malformed))
            .collect::<Result<Vec<_>, _>>(),
    )?;
    at_stage(
        "chain_validate",
        validate_certificate_chain(&certificates, &chain_der, policy, now_ms),
    )?;

    let leaf_key = certificates[0]
        .public_key()
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    validate_p256_key(&leaf_key)?;
    if leaf_key
        .public_key_to_der()
        .map_err(|_| AndroidAppsAttestationError::Malformed)?
        != spki
    {
        return Err(AndroidAppsAttestationError::InvalidProof);
    }

    let extension = at_stage(
        "attestation_extension",
        android_key_attestation_extension(&chain_der[0]),
    )?;
    let claims = at_stage("key_description", parse_key_description(extension))?;
    if claims.challenge != challenge
        || !matches!(claims.attestation_security_level, 1 | 2)
        || !matches!(claims.keymaster_security_level, 1 | 2)
        || claims.package_name != evidence.app_package.as_bytes()
        || claims.package_version_code != evidence.app_version_code
        || claims.signing_certificate_sha256 != decode_hex_32(&app_signing_sha256)?
        || !claims.device_locked
        || claims.verified_boot_state != 0
    {
        tracing::warn!(reason = "claims_mismatch", "[ANDROID-ATTESTATION] rejected");
        return Err(AndroidAppsAttestationError::Untrusted);
    }

    let signing_bytes = android_apps_enrollment_signing_bytes(&evidence, challenge);
    let mut verifier = Verifier::new(MessageDigest::sha256(), &leaf_key)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    verifier
        .update(&signing_bytes)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    if !verifier
        .verify(&signature)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?
    {
        return Err(AndroidAppsAttestationError::InvalidProof);
    }

    let chain_digest = framed_sha256(chain_der.iter().map(Vec::as_slice));
    let root_digest = sha256_hex(chain_der.last().expect("validated nonempty chain"));
    // A learning policy's identity carries the digest of the policy pinned
    // to the build the attestation proved, never the unpinned one.
    let attestation_policy_digest = if policy.learn_build {
        policy
            .pinned_to_build(&app_signing_sha256, evidence.app_version_code)?
            .digest
    } else {
        policy.digest.clone()
    };
    Ok(DeviceAutomationIdentity {
        enrollment_id: evidence.enrollment_id.to_owned(),
        key_id: evidence.key_id.to_owned(),
        public_key_spki_base64: evidence.public_key_spki_base64.to_owned(),
        app_package: evidence.app_package.to_owned(),
        app_version_code: evidence.app_version_code,
        app_signing_sha256,
        apk_sha256,
        attestation_chain_sha256: chain_digest,
        attestation_root_sha256: root_digest,
        attestation_security_level: if claims.attestation_security_level == 2 {
            "strongbox".to_owned()
        } else {
            "tee".to_owned()
        },
        attestation_policy_digest,
        enrolled_at_ms: now_ms,
    })
}

pub(crate) fn android_apps_enrollment_signing_bytes(
    evidence: &AndroidAppsEnrollmentEvidence<'_>,
    challenge: &[u8],
) -> Vec<u8> {
    let app_version_code = evidence.app_version_code.to_le_bytes();
    framed_bytes(
        b"magician.android-apps-enrollment-proof.v2\0",
        [
            evidence.enrollment_id.as_bytes(),
            evidence.device_id.as_bytes(),
            evidence.label.as_bytes(),
            evidence.key_id.as_bytes(),
            evidence.public_key_spki_base64.as_bytes(),
            evidence.app_package.as_bytes(),
            app_version_code.as_slice(),
            evidence.app_signing_sha256.as_bytes(),
            evidence.apk_sha256.as_bytes(),
            evidence.connection_secret_sha256.as_bytes(),
            challenge,
        ],
    )
}

pub(crate) fn android_apps_socket_signing_bytes(
    connection_id: &str,
    key_id: &str,
    target_ref: &str,
    review_generation: u64,
    protocol_version: &str,
    server_nonce: &[u8],
    apk_sha256: &str,
    attestation_policy_digest: &str,
) -> Vec<u8> {
    let generation = review_generation.to_le_bytes();
    framed_bytes(
        b"magician.android-apps-socket-proof.v1\0",
        [
            connection_id.as_bytes(),
            key_id.as_bytes(),
            target_ref.as_bytes(),
            generation.as_slice(),
            protocol_version.as_bytes(),
            server_nonce,
            apk_sha256.as_bytes(),
            attestation_policy_digest.as_bytes(),
        ],
    )
}

pub(crate) fn verify_p256_signature(
    public_key_spki_base64: &str,
    message: &[u8],
    signature_base64: &str,
) -> Result<(), AndroidAppsAttestationError> {
    let key_der = decode_bounded(public_key_spki_base64, 1024)?;
    let signature = decode_bounded(signature_base64, 256)?;
    let key =
        PKey::public_key_from_der(&key_der).map_err(|_| AndroidAppsAttestationError::Malformed)?;
    validate_p256_key(&key)?;
    let mut verifier = Verifier::new(MessageDigest::sha256(), &key)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    verifier
        .update(message)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    verifier
        .verify(&signature)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?
        .then_some(())
        .ok_or(AndroidAppsAttestationError::InvalidProof)
}

fn validate_p256_key(key: &PKey<Public>) -> Result<(), AndroidAppsAttestationError> {
    let ec = key
        .ec_key()
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    if ec.group().curve_name() != Some(Nid::X9_62_PRIME256V1) {
        tracing::warn!(reason = "leaf_key_curve", "[ANDROID-ATTESTATION] rejected");
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    Ok(())
}

fn validate_certificate_chain(
    certificates: &[X509],
    chain_der: &[Vec<u8>],
    policy: &AndroidAppsAttestationPolicy,
    now_ms: i64,
) -> Result<(), AndroidAppsAttestationError> {
    if certificates.len() < 2 || certificates.len() != chain_der.len() {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let now_seconds = now_ms.div_euclid(1_000);
    let now =
        Asn1Time::from_unix(now_seconds).map_err(|_| AndroidAppsAttestationError::Malformed)?;
    for certificate in certificates {
        if certificate
            .not_before()
            .compare(&now)
            .map_err(|_| AndroidAppsAttestationError::Malformed)?
            == Ordering::Greater
            || certificate
                .not_after()
                .compare(&now)
                .map_err(|_| AndroidAppsAttestationError::Malformed)?
                != Ordering::Greater
        {
            tracing::warn!(reason = "chain_order", "[ANDROID-ATTESTATION] rejected");
            return Err(AndroidAppsAttestationError::Untrusted);
        }
    }

    // The caller presents the complete leaf-to-root chain. The final
    // certificate is admitted as an anchor only when its exact DER digest was
    // reviewed; the process/system trust store is deliberately never loaded.
    let root = certificates.last().expect("validated nonempty chain");
    let root_key = root
        .public_key()
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    let root_is_self_issued = root
        .subject_name()
        .to_der()
        .and_then(|subject| root.issuer_name().to_der().map(|issuer| subject == issuer))
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    if !root_is_self_issued
        || !root
            .verify(&root_key)
            .map_err(|_| AndroidAppsAttestationError::Malformed)?
        || !policy.attestation_root_sha256.contains(&sha256_hex(
            chain_der.last().expect("validated nonempty chain"),
        ))
    {
        tracing::warn!(
            reason = "root_not_reviewed",
            "[ANDROID-ATTESTATION] rejected"
        );
        return Err(AndroidAppsAttestationError::Untrusted);
    }

    let mut parameters =
        X509VerifyParam::new().map_err(|_| AndroidAppsAttestationError::Malformed)?;
    parameters.set_time(
        now_seconds
            .try_into()
            .map_err(|_| AndroidAppsAttestationError::Malformed)?,
    );
    parameters.set_depth(
        certificates
            .len()
            .saturating_sub(1)
            .try_into()
            .map_err(|_| AndroidAppsAttestationError::Malformed)?,
    );
    parameters
        .set_flags(X509VerifyFlags::CHECK_SS_SIGNATURE | X509VerifyFlags::NO_ALT_CHAINS)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;

    let mut store_builder =
        X509StoreBuilder::new().map_err(|_| AndroidAppsAttestationError::Malformed)?;
    store_builder
        .set_param(&parameters)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    store_builder
        .add_cert(root.clone())
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    let store = store_builder.build();

    // Only the interior certificates are untrusted path-building candidates;
    // the leaf and the exact pinned root have distinct OpenSSL roles. Path
    // validation enforces basicConstraints, CA/keyCertSign, pathLen, validity
    // and unsupported critical extensions for every selected path. Android
    // attestation leaves are not required to carry a generic X.509 AKI.
    let mut intermediates = Stack::new().map_err(|_| AndroidAppsAttestationError::Malformed)?;
    for certificate in &certificates[1..certificates.len().saturating_sub(1)] {
        intermediates
            .push(certificate.clone())
            .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    }
    let mut context =
        X509StoreContext::new().map_err(|_| AndroidAppsAttestationError::Malformed)?;
    let verified_chain = context
        .init(&store, &certificates[0], &intermediates, |context| {
            if !context.verify_cert()? {
                return Ok(None);
            }
            let Some(chain) = context.chain() else {
                return Ok(None);
            };
            chain
                .iter()
                .map(|certificate| certificate.to_der())
                .collect::<Result<Vec<_>, _>>()
                .map(Some)
        })
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    if verified_chain.as_deref() != Some(chain_der) {
        // Reject unused/injected certificates and alternate paths. Android Key
        // Attestation parsing below is therefore applied only to the validated
        // end-entity certificate at index zero.
        tracing::warn!(reason = "chain_path", "[ANDROID-ATTESTATION] rejected");
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    Ok(())
}

fn decode_chain(values: &[String]) -> Result<Vec<Vec<u8>>, AndroidAppsAttestationError> {
    if values.is_empty() || values.len() > MAX_CERTIFICATES {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let mut total = 0usize;
    values
        .iter()
        .map(|value| {
            let der = decode_bounded(value, MAX_CERTIFICATE_BYTES)?;
            total = total.saturating_add(der.len());
            if total > MAX_CHAIN_BYTES {
                return Err(AndroidAppsAttestationError::Malformed);
            }
            Ok(der)
        })
        .collect()
}

fn decode_bounded(value: &str, maximum: usize) -> Result<Vec<u8>, AndroidAppsAttestationError> {
    if value.is_empty() || value.len() > maximum.saturating_mul(2) {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| AndroidAppsAttestationError::Malformed)?;
    if decoded.is_empty() || decoded.len() > maximum {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    Ok(decoded)
}

fn decode_hex_32(value: &str) -> Result<Vec<u8>, AndroidAppsAttestationError> {
    if value.len() != 64 {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    (0..32)
        .map(|index| {
            u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
                .map_err(|_| AndroidAppsAttestationError::Malformed)
        })
        .collect()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = hash(MessageDigest::sha256(), bytes).expect("SHA-256 accepts bytes");
    hex::encode(digest.as_ref())
}

fn framed_sha256<'a>(components: impl IntoIterator<Item = &'a [u8]>) -> String {
    let mut context = openssl::sha::Sha256::new();
    context.update(b"magician.android-attestation-chain.v1\0");
    for component in components {
        context.update(&(component.len() as u64).to_le_bytes());
        context.update(component);
    }
    hex::encode(context.finish())
}

fn framed_bytes<'a, const N: usize>(domain: &[u8], components: [&'a [u8]; N]) -> Vec<u8> {
    let capacity = domain.len()
        + components
            .iter()
            .map(|component| 8usize.saturating_add(component.len()))
            .sum::<usize>();
    let mut bytes = Vec::with_capacity(capacity);
    bytes.extend_from_slice(domain);
    for component in components {
        bytes.extend_from_slice(&(component.len() as u64).to_le_bytes());
        bytes.extend_from_slice(component);
    }
    bytes
}

#[derive(Clone, Copy)]
struct DerValue<'a> {
    class: u8,
    constructed: bool,
    number: u32,
    content: &'a [u8],
}

fn take_der<'a>(input: &mut &'a [u8]) -> Result<DerValue<'a>, AndroidAppsAttestationError> {
    let first = *input
        .first()
        .ok_or(AndroidAppsAttestationError::Malformed)?;
    *input = &input[1..];
    let class = first >> 6;
    let constructed = first & 0x20 != 0;
    let mut number = u32::from(first & 0x1f);
    if number == 0x1f {
        number = 0;
        let mut groups = 0;
        loop {
            let byte = *input
                .first()
                .ok_or(AndroidAppsAttestationError::Malformed)?;
            *input = &input[1..];
            groups += 1;
            if groups > 5 || (groups == 1 && byte == 0x80) {
                return Err(AndroidAppsAttestationError::Malformed);
            }
            number = number
                .checked_mul(128)
                .and_then(|number| number.checked_add(u32::from(byte & 0x7f)))
                .ok_or(AndroidAppsAttestationError::Malformed)?;
            if byte & 0x80 == 0 {
                break;
            }
        }
    }
    let first_length = *input
        .first()
        .ok_or(AndroidAppsAttestationError::Malformed)?;
    *input = &input[1..];
    let length = if first_length & 0x80 == 0 {
        usize::from(first_length)
    } else {
        let width = usize::from(first_length & 0x7f);
        if width == 0 || width > std::mem::size_of::<usize>() || input.len() < width {
            return Err(AndroidAppsAttestationError::Malformed);
        }
        let mut length = 0usize;
        for byte in &input[..width] {
            length = length
                .checked_mul(256)
                .and_then(|length| length.checked_add(usize::from(*byte)))
                .ok_or(AndroidAppsAttestationError::Malformed)?;
        }
        if length < 128 {
            return Err(AndroidAppsAttestationError::Malformed);
        }
        *input = &input[width..];
        length
    };
    if input.len() < length {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let content = &input[..length];
    *input = &input[length..];
    Ok(DerValue {
        class,
        constructed,
        number,
        content,
    })
}

fn only_der(input: &[u8]) -> Result<DerValue<'_>, AndroidAppsAttestationError> {
    let mut rest = input;
    let value = take_der(&mut rest)?;
    if !rest.is_empty() {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    Ok(value)
}

fn universal<'a>(
    value: DerValue<'a>,
    number: u32,
) -> Result<&'a [u8], AndroidAppsAttestationError> {
    (value.class == 0 && value.number == number)
        .then_some(value.content)
        .ok_or(AndroidAppsAttestationError::Malformed)
}

fn small_integer(value: DerValue<'_>) -> Result<u32, AndroidAppsAttestationError> {
    let bytes = if value.class == 0 && matches!(value.number, 2 | 10) {
        value.content
    } else {
        return Err(AndroidAppsAttestationError::Malformed);
    };
    if bytes.is_empty()
        || bytes.len() > 5
        || bytes[0] & 0x80 != 0
        || (bytes.len() > 1 && bytes[0] == 0 && bytes[1] & 0x80 == 0)
    {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    bytes
        .iter()
        .try_fold(0u64, |value, byte| {
            value
                .checked_mul(256)
                .and_then(|value| value.checked_add(u64::from(*byte)))
        })
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(AndroidAppsAttestationError::Malformed)
}

fn android_key_attestation_extension(
    certificate_der: &[u8],
) -> Result<&[u8], AndroidAppsAttestationError> {
    let certificate = universal(only_der(certificate_der)?, 16)?;
    let mut certificate = certificate;
    let tbs = universal(take_der(&mut certificate)?, 16)?;
    let mut fields = tbs;
    while !fields.is_empty() {
        let field = take_der(&mut fields)?;
        if field.class == 2 && field.constructed && field.number == 3 {
            let extensions = universal(only_der(field.content)?, 16)?;
            let mut extensions = extensions;
            while !extensions.is_empty() {
                let extension = universal(take_der(&mut extensions)?, 16)?;
                let mut extension = extension;
                let oid = universal(take_der(&mut extension)?, 6)?;
                if extension.first().is_some_and(|byte| *byte == 0x01) {
                    let _ = universal(take_der(&mut extension)?, 1)?;
                }
                let body = universal(take_der(&mut extension)?, 4)?;
                if !extension.is_empty() {
                    return Err(AndroidAppsAttestationError::Malformed);
                }
                if oid == ANDROID_KEY_ATTESTATION_OID {
                    return Ok(body);
                }
            }
        }
    }
    Err(AndroidAppsAttestationError::Untrusted)
}

struct KeyDescription<'a> {
    attestation_security_level: u32,
    keymaster_security_level: u32,
    challenge: &'a [u8],
    package_name: &'a [u8],
    package_version_code: u64,
    signing_certificate_sha256: Vec<u8>,
    device_locked: bool,
    verified_boot_state: u32,
}

fn parse_key_description(bytes: &[u8]) -> Result<KeyDescription<'_>, AndroidAppsAttestationError> {
    let sequence = universal(only_der(bytes)?, 16)?;
    let mut fields = sequence;
    let _attestation_version = small_integer(take_der(&mut fields)?)?;
    let attestation_security_level = small_integer(take_der(&mut fields)?)?;
    let _keymaster_version = small_integer(take_der(&mut fields)?)?;
    let keymaster_security_level = small_integer(take_der(&mut fields)?)?;
    let challenge = universal(take_der(&mut fields)?, 4)?;
    let _unique_id = universal(take_der(&mut fields)?, 4)?;
    let software = universal(take_der(&mut fields)?, 16)?;
    let hardware = universal(take_der(&mut fields)?, 16)?;
    if !fields.is_empty() {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    validate_hardware_key_authorizations(software, hardware)?;
    let app_id =
        tagged_authorization(software, 709)?.ok_or(AndroidAppsAttestationError::Untrusted)?;
    let app_id = universal(only_der(app_id)?, 4)?;
    let (package_name, package_version_code, signing_certificate_sha256) =
        parse_attestation_application_id(app_id)?;
    let root_of_trust =
        tagged_authorization(hardware, 704)?.ok_or(AndroidAppsAttestationError::Untrusted)?;
    let root_of_trust = universal(only_der(root_of_trust)?, 16)?;
    let (_verified_boot_key, device_locked, verified_boot_state) =
        parse_root_of_trust(root_of_trust)?;
    Ok(KeyDescription {
        attestation_security_level,
        keymaster_security_level,
        challenge,
        package_name,
        package_version_code,
        signing_certificate_sha256,
        device_locked,
        verified_boot_state,
    })
}

fn validate_hardware_key_authorizations(
    software: &[u8],
    hardware: &[u8],
) -> Result<(), AndroidAppsAttestationError> {
    // Android KeyMint values: PURPOSE_SIGN=2, ALGORITHM_EC=3,
    // DIGEST_SHA_2_256=4, EC_CURVE_P_256=1, ORIGIN_GENERATED=0. The
    // authorization must be enforced by the attested TEE/StrongBox, not merely
    // repeated in the caller-controlled/software authorization list.
    const PURPOSE: u32 = 1;
    const ALGORITHM: u32 = 2;
    const KEY_SIZE: u32 = 3;
    const DIGEST: u32 = 5;
    const EC_CURVE: u32 = 10;
    const ORIGIN: u32 = 702;
    for tag in [PURPOSE, ALGORITHM, KEY_SIZE, DIGEST, EC_CURVE, ORIGIN] {
        if tagged_authorization(software, tag)?.is_some() {
            tracing::warn!(
                reason = "software_enforced_key",
                "[ANDROID-ATTESTATION] rejected"
            );
            return Err(AndroidAppsAttestationError::Untrusted);
        }
    }
    require_integer_set_authorization(hardware, PURPOSE, &[2])?;
    require_integer_authorization(hardware, ALGORITHM, 3)?;
    require_integer_authorization(hardware, KEY_SIZE, 256)?;
    require_integer_set_authorization(hardware, DIGEST, &[4])?;
    require_integer_authorization(hardware, EC_CURVE, 1)?;
    require_integer_authorization(hardware, ORIGIN, 0)
}

fn require_integer_authorization(
    authorization: &[u8],
    tag: u32,
    expected: u32,
) -> Result<(), AndroidAppsAttestationError> {
    let value =
        tagged_authorization(authorization, tag)?.ok_or(AndroidAppsAttestationError::Untrusted)?;
    if small_integer(only_der(value)?)? != expected {
        tracing::warn!(
            reason = "hardware_authorization",
            "[ANDROID-ATTESTATION] rejected"
        );
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    Ok(())
}

fn require_integer_set_authorization(
    authorization: &[u8],
    tag: u32,
    expected: &[u32],
) -> Result<(), AndroidAppsAttestationError> {
    let value =
        tagged_authorization(authorization, tag)?.ok_or(AndroidAppsAttestationError::Untrusted)?;
    let mut values = universal(only_der(value)?, 17)?;
    let mut actual = Vec::new();
    while !values.is_empty() {
        actual.push(small_integer(take_der(&mut values)?)?);
    }
    actual.sort_unstable();
    if actual != expected {
        tracing::warn!(
            reason = "hardware_authorization_set",
            "[ANDROID-ATTESTATION] rejected"
        );
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    Ok(())
}

fn tagged_authorization(
    authorization: &[u8],
    tag: u32,
) -> Result<Option<&[u8]>, AndroidAppsAttestationError> {
    let mut fields = authorization;
    let mut found = None;
    while !fields.is_empty() {
        let field = take_der(&mut fields)?;
        if field.class == 2 && field.number == tag {
            if found.replace(field.content).is_some() {
                return Err(AndroidAppsAttestationError::Malformed);
            }
        }
    }
    Ok(found)
}

fn parse_attestation_application_id(
    bytes: &[u8],
) -> Result<(&[u8], u64, Vec<u8>), AndroidAppsAttestationError> {
    let sequence = universal(only_der(bytes)?, 16)?;
    let mut fields = sequence;
    let packages = universal(take_der(&mut fields)?, 17)?;
    let signatures = universal(take_der(&mut fields)?, 17)?;
    if !fields.is_empty() {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let mut packages_rest = packages;
    let package = universal(take_der(&mut packages_rest)?, 16)?;
    if !packages_rest.is_empty() {
        tracing::warn!(
            reason = "attestation_application_id",
            "[ANDROID-ATTESTATION] rejected"
        );
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    let mut package = package;
    let package_name = universal(take_der(&mut package)?, 4)?;
    let version = positive_integer_u64(take_der(&mut package)?)?;
    if !package.is_empty() {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    let mut signatures_rest = signatures;
    let signature = universal(take_der(&mut signatures_rest)?, 4)?;
    if signature.len() != 32 || !signatures_rest.is_empty() {
        return Err(AndroidAppsAttestationError::Untrusted);
    }
    Ok((package_name, version, signature.to_vec()))
}

fn positive_integer_u64(value: DerValue<'_>) -> Result<u64, AndroidAppsAttestationError> {
    let bytes = universal(value, 2)?;
    if bytes.is_empty()
        || bytes.len() > 9
        || bytes[0] & 0x80 != 0
        || (bytes.len() > 1 && bytes[0] == 0 && bytes[1] & 0x80 == 0)
    {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    bytes
        .iter()
        .try_fold(0u64, |value, byte| {
            value
                .checked_mul(256)
                .and_then(|value| value.checked_add(u64::from(*byte)))
        })
        .ok_or(AndroidAppsAttestationError::Malformed)
}

fn parse_root_of_trust(bytes: &[u8]) -> Result<(&[u8], bool, u32), AndroidAppsAttestationError> {
    let mut fields = bytes;
    let verified_boot_key = universal(take_der(&mut fields)?, 4)?;
    let locked = universal(take_der(&mut fields)?, 1)?;
    let state = small_integer(take_der(&mut fields)?)?;
    let _verified_boot_hash = universal(take_der(&mut fields)?, 4)?;
    if !fields.is_empty() || locked.len() != 1 || !matches!(locked[0], 0 | 0xff) {
        return Err(AndroidAppsAttestationError::Malformed);
    }
    Ok((verified_boot_key, locked[0] == 0xff, state))
}

#[cfg(test)]
mod tests {
    use std::{
        sync::atomic::{AtomicU32, Ordering as AtomicOrdering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use openssl::{
        asn1::{Asn1Object, Asn1OctetString},
        bn::BigNum,
        ec::{EcGroup, EcKey},
        pkey::Private,
        x509::{
            extension::{AuthorityKeyIdentifier, BasicConstraints, KeyUsage, SubjectKeyIdentifier},
            X509Extension, X509NameBuilder,
        },
    };

    use super::*;

    static TEST_SERIAL: AtomicU32 = AtomicU32::new(1);

    fn test_key() -> PKey<Private> {
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap()
    }

    fn test_certificate(
        common_name: &str,
        key: &PKey<Private>,
        issuer: Option<&X509>,
        issuer_key: &PKey<Private>,
        ca_path_len: Option<u32>,
        unknown_critical_extension: bool,
    ) -> X509 {
        test_certificate_with_authority_key_identifier(
            common_name,
            key,
            issuer,
            issuer_key,
            ca_path_len,
            unknown_critical_extension,
            true,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn test_certificate_with_authority_key_identifier(
        common_name: &str,
        key: &PKey<Private>,
        issuer: Option<&X509>,
        issuer_key: &PKey<Private>,
        ca_path_len: Option<u32>,
        unknown_critical_extension: bool,
        include_authority_key_identifier: bool,
    ) -> X509 {
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_nid(Nid::COMMONNAME, common_name)
            .unwrap();
        let name = name.build();
        let serial = BigNum::from_u32(TEST_SERIAL.fetch_add(1, AtomicOrdering::Relaxed)).unwrap();
        let serial = serial.to_asn1_integer().unwrap();
        let mut builder = X509::builder().unwrap();
        builder.set_version(2).unwrap();
        builder.set_serial_number(&serial).unwrap();
        builder.set_subject_name(&name).unwrap();
        if let Some(issuer) = issuer {
            builder.set_issuer_name(issuer.subject_name()).unwrap();
        } else {
            builder.set_issuer_name(&name).unwrap();
        }
        builder.set_pubkey(key).unwrap();
        builder
            .set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        builder
            .set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();

        let basic_constraints = if let Some(path_len) = ca_path_len {
            BasicConstraints::new()
                .critical()
                .ca()
                .pathlen(path_len)
                .build()
                .unwrap()
        } else {
            BasicConstraints::new().critical().build().unwrap()
        };
        builder.append_extension(basic_constraints).unwrap();
        let key_usage = if ca_path_len.is_some() {
            KeyUsage::new()
                .critical()
                .key_cert_sign()
                .crl_sign()
                .build()
                .unwrap()
        } else {
            KeyUsage::new()
                .critical()
                .digital_signature()
                .build()
                .unwrap()
        };
        builder.append_extension(key_usage).unwrap();
        let subject_key_identifier = SubjectKeyIdentifier::new()
            .build(&builder.x509v3_context(issuer.map(|certificate| certificate.as_ref()), None))
            .unwrap();
        builder.append_extension(subject_key_identifier).unwrap();
        if include_authority_key_identifier {
            let mut authority_key_identifier = AuthorityKeyIdentifier::new();
            authority_key_identifier.keyid(true);
            if issuer.is_some() {
                authority_key_identifier.issuer(true);
            }
            let authority_key_identifier = authority_key_identifier
                .build(
                    &builder.x509v3_context(issuer.map(|certificate| certificate.as_ref()), None),
                )
                .unwrap();
            builder.append_extension(authority_key_identifier).unwrap();
        }
        if unknown_critical_extension {
            let oid = Asn1Object::from_str("1.3.6.1.4.1.55555.1").unwrap();
            let contents = Asn1OctetString::new_from_bytes(&[0x05, 0x00]).unwrap();
            builder
                .append_extension(X509Extension::new_from_der(&oid, true, &contents).unwrap())
                .unwrap();
        }
        builder.sign(issuer_key, MessageDigest::sha256()).unwrap();
        builder.build()
    }

    fn test_policy(root_der: &[u8]) -> AndroidAppsAttestationPolicy {
        let app_signing_sha256 = BTreeSet::from(["a".repeat(64)]);
        let attestation_root_sha256 = BTreeSet::from([sha256_hex(root_der)]);
        let apk_sha256 = BTreeSet::from(["b".repeat(64)]);
        let app_version_codes = BTreeSet::from([21]);
        let digest = attestation_policy_digest(
            &app_signing_sha256,
            &attestation_root_sha256,
            &apk_sha256,
            &app_version_codes,
            &format!("blake3:{}", "f".repeat(64)),
        );
        AndroidAppsAttestationPolicy {
            app_signing_sha256,
            attestation_root_sha256,
            apk_sha256,
            app_version_codes,
            digest,
            external_authority_policy_digest: format!("blake3:{}", "f".repeat(64)),
            learn_build: false,
        }
    }

    fn now_ms() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis()
            .try_into()
            .unwrap()
    }

    fn test_der(identifier: &[u8], content: &[u8]) -> Vec<u8> {
        assert!(content.len() < 128);
        let mut encoded = Vec::with_capacity(identifier.len() + 1 + content.len());
        encoded.extend_from_slice(identifier);
        encoded.push(content.len() as u8);
        encoded.extend_from_slice(content);
        encoded
    }

    fn test_integer(value: u32) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        let mut content = bytes[first..].to_vec();
        if content[0] & 0x80 != 0 {
            content.insert(0, 0);
        }
        test_der(&[0x02], &content)
    }

    fn test_integer_set(values: &[u32]) -> Vec<u8> {
        let content = values
            .iter()
            .flat_map(|value| test_integer(*value))
            .collect::<Vec<_>>();
        test_der(&[0x31], &content)
    }

    fn test_explicit(tag: u32, body: Vec<u8>) -> Vec<u8> {
        let identifier = if tag < 31 {
            vec![0xa0 | tag as u8]
        } else {
            let mut chunks = Vec::new();
            let mut remaining = tag;
            loop {
                chunks.push((remaining & 0x7f) as u8);
                remaining >>= 7;
                if remaining == 0 {
                    break;
                }
            }
            chunks.reverse();
            let last = chunks.len().saturating_sub(1);
            for chunk in &mut chunks[..last] {
                *chunk |= 0x80;
            }
            let mut identifier = vec![0xbf];
            identifier.extend(chunks);
            identifier
        };
        test_der(&identifier, &body)
    }

    fn test_hardware_authorizations(
        purpose: u32,
        algorithm: u32,
        digest: u32,
        curve: u32,
        origin: u32,
    ) -> Vec<u8> {
        [
            test_explicit(1, test_integer_set(&[purpose])),
            test_explicit(2, test_integer(algorithm)),
            test_explicit(3, test_integer(256)),
            test_explicit(5, test_integer_set(&[digest])),
            test_explicit(10, test_integer(curve)),
            test_explicit(702, test_integer(origin)),
        ]
        .concat()
    }

    #[test]
    fn hardware_authorization_requires_the_exact_generated_signing_key_policy() {
        let exact = test_hardware_authorizations(2, 3, 4, 1, 0);
        assert!(validate_hardware_key_authorizations(&[], &exact).is_ok());
        for rejected in [
            test_hardware_authorizations(3, 3, 4, 1, 0),
            test_hardware_authorizations(2, 1, 4, 1, 0),
            test_hardware_authorizations(2, 3, 2, 1, 0),
            test_hardware_authorizations(2, 3, 4, 2, 0),
            test_hardware_authorizations(2, 3, 4, 1, 2),
        ] {
            assert!(matches!(
                validate_hardware_key_authorizations(&[], &rejected),
                Err(AndroidAppsAttestationError::Untrusted)
            ));
        }
    }

    #[test]
    fn software_only_key_policy_is_not_treated_as_hardware_enforced() {
        let hardware = [
            test_explicit(2, test_integer(3)),
            test_explicit(3, test_integer(256)),
            test_explicit(5, test_integer_set(&[4])),
            test_explicit(10, test_integer(1)),
            test_explicit(702, test_integer(0)),
        ]
        .concat();
        let software = test_explicit(1, test_integer_set(&[2]));
        assert!(matches!(
            validate_hardware_key_authorizations(&software, &hardware),
            Err(AndroidAppsAttestationError::Untrusted)
        ));
    }

    #[test]
    fn strict_path_accepts_only_the_complete_reviewed_chain() {
        let root_key = test_key();
        let root = test_certificate("reviewed root", &root_key, None, &root_key, Some(2), false);
        let intermediate_key = test_key();
        let intermediate = test_certificate(
            "real ca",
            &intermediate_key,
            Some(&root),
            &root_key,
            Some(0),
            false,
        );
        let leaf_key = test_key();
        let leaf = test_certificate(
            "attested leaf",
            &leaf_key,
            Some(&intermediate),
            &intermediate_key,
            None,
            false,
        );
        let certificates = vec![leaf, intermediate, root];
        let chain_der = certificates
            .iter()
            .map(|certificate| certificate.to_der().unwrap())
            .collect::<Vec<_>>();
        assert!(validate_certificate_chain(
            &certificates,
            &chain_der,
            &test_policy(chain_der.last().unwrap()),
            now_ms(),
        )
        .is_ok());
    }

    #[test]
    fn android_attestation_leaf_without_authority_key_identifier_is_accepted() {
        let root_key = test_key();
        let root = test_certificate("reviewed root", &root_key, None, &root_key, Some(2), false);
        let intermediate_key = test_key();
        let intermediate = test_certificate(
            "real ca",
            &intermediate_key,
            Some(&root),
            &root_key,
            Some(0),
            false,
        );
        let leaf_key = test_key();
        let leaf = test_certificate_with_authority_key_identifier(
            "Android Keystore Key",
            &leaf_key,
            Some(&intermediate),
            &intermediate_key,
            None,
            false,
            false,
        );
        let certificates = vec![leaf, intermediate, root];
        let chain_der = certificates
            .iter()
            .map(|certificate| certificate.to_der().unwrap())
            .collect::<Vec<_>>();

        assert!(validate_certificate_chain(
            &certificates,
            &chain_der,
            &test_policy(chain_der.last().unwrap()),
            now_ms(),
        )
        .is_ok());
    }

    #[test]
    fn strict_path_rejects_a_non_ca_as_an_illicit_intermediate() {
        let root_key = test_key();
        let root = test_certificate("reviewed root", &root_key, None, &root_key, Some(2), false);
        let non_ca_key = test_key();
        let non_ca = test_certificate(
            "hardware attested non-ca",
            &non_ca_key,
            Some(&root),
            &root_key,
            None,
            false,
        );
        let forged_leaf_key = test_key();
        let forged_leaf = test_certificate(
            "forged attestation leaf",
            &forged_leaf_key,
            Some(&non_ca),
            &non_ca_key,
            None,
            false,
        );
        let certificates = vec![forged_leaf, non_ca, root];
        let chain_der = certificates
            .iter()
            .map(|certificate| certificate.to_der().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_certificate_chain(
                &certificates,
                &chain_der,
                &test_policy(chain_der.last().unwrap()),
                now_ms(),
            ),
            Err(AndroidAppsAttestationError::Untrusted)
        ));
    }

    #[test]
    fn strict_path_rejects_an_unhandled_critical_extension() {
        let root_key = test_key();
        let root = test_certificate("reviewed root", &root_key, None, &root_key, Some(2), false);
        let intermediate_key = test_key();
        let intermediate = test_certificate(
            "real ca",
            &intermediate_key,
            Some(&root),
            &root_key,
            Some(0),
            false,
        );
        let leaf_key = test_key();
        let leaf = test_certificate(
            "attested leaf",
            &leaf_key,
            Some(&intermediate),
            &intermediate_key,
            None,
            true,
        );
        let certificates = vec![leaf, intermediate, root];
        let chain_der = certificates
            .iter()
            .map(|certificate| certificate.to_der().unwrap())
            .collect::<Vec<_>>();
        assert!(matches!(
            validate_certificate_chain(
                &certificates,
                &chain_der,
                &test_policy(chain_der.last().unwrap()),
                now_ms(),
            ),
            Err(AndroidAppsAttestationError::Untrusted)
        ));
    }

    #[test]
    fn signing_bytes_bind_socket_generation_and_review() {
        let first = android_apps_socket_signing_bytes(
            "00000000-0000-0000-0000-000000000001",
            "device-key-123456",
            "android-device:opaque",
            7,
            "2026-07-28",
            &[3; 32],
            &"a".repeat(64),
            &format!("blake3:{}", "b".repeat(64)),
        );
        assert_ne!(
            first,
            android_apps_socket_signing_bytes(
                "00000000-0000-0000-0000-000000000002",
                "device-key-123456",
                "android-device:opaque",
                7,
                "2026-07-28",
                &[3; 32],
                &"a".repeat(64),
                &format!("blake3:{}", "b".repeat(64)),
            )
        );
        assert_ne!(
            first,
            android_apps_socket_signing_bytes(
                "00000000-0000-0000-0000-000000000001",
                "device-key-123456",
                "android-device:opaque",
                8,
                "2026-07-28",
                &[3; 32],
                &"a".repeat(64),
                &format!("blake3:{}", "b".repeat(64)),
            )
        );
    }

    #[test]
    fn der_parser_rejects_indefinite_and_noncanonical_lengths() {
        assert!(only_der(&[0x30, 0x80, 0, 0]).is_err());
        assert!(only_der(&[0x04, 0x81, 0x01, 0]).is_err());
    }

    #[test]
    fn packaged_android_build_declares_the_reviewed_package_and_a_version() {
        let build = include_str!("../../magdroid/android/app/build.gradle.kts");
        assert!(build.contains(&format!("applicationId = \"{EXPECTED_PACKAGE}\"")));
        let version = build
            .lines()
            .find_map(|line| line.trim().strip_prefix("versionCode = "))
            .and_then(|value| value.parse::<u64>().ok())
            .expect("Android build declares a numeric versionCode");
        assert!(version > 0);
    }
}

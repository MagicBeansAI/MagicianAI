//! Physical, bounded codec for data and combined app-portability archives.
//!
//! Logical package/data authority remains in `portability`; this module only
//! serializes the exact logical archive and, for combined archives, the exact
//! already-admitted package ZIP. Encrypted envelopes use a passphrase-derived
//! key that is independent of app-store at-rest encryption keys.

use std::io::Cursor;

use argon2::{Algorithm, Argon2, Params, Version};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit, Payload},
    XChaCha20Poly1305, XNonce,
};
use chrono::{DateTime, Utc};
use rand::{rngs::OsRng, RngCore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use super::{
    authority::AuthenticatedAppScope,
    models::{AppContractLimits, AppDigest},
    package_transfer::{
        admit_package_archive, AppPackageTransferError, APP_PACKAGE_ARCHIVE_MAX_BYTES,
    },
    portability::{
        AppArchiveProtectionPlan, AppArchiveWritePlan, AppLogicalArchive, AppPortabilityError,
        AppVerifiedEncryptedArchive, APP_ARCHIVE_ENVELOPE_VERSION,
    },
};
use crate::magician_v2::json_traversal::{
    canonical_json_bytes, json_bytes_nesting_is_bounded, json_bytes_nodes_are_bounded,
};

pub const APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE: &str =
    "application/vnd.magician.app-archive.encrypted+octet-stream";
pub const APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE: &str =
    "application/vnd.magician.app-archive+json";
pub const APP_PORTABLE_ARCHIVE_MAX_BYTES: usize = 132 * 1_024 * 1_024;
const MAGIC: &[u8; 8] = b"MAGAPP01";
const HEADER_MAX_BYTES: usize = 16 * 1_024;
// A maximum package ZIP expands to 96 MiB as base64url and a data archive may
// carry 16 MiB of record payload plus bounded metadata. Keep the logical
// ceiling below the physical ceiling while admitting that complete contract.
const LOGICAL_PAYLOAD_MAX_BYTES: usize = 128 * 1_024 * 1_024;
const CHUNK_BYTES: usize = 1_048_576;
const TAG_BYTES: usize = 16;
const LOGICAL_PAYLOAD_MAX_JSON_NODES: usize = 500_000;

/// Secret transport input. It is neither serializable nor printable and is
/// zeroized after the bounded archive operation.
pub struct AppArchivePassphrase(Zeroizing<Vec<u8>>);

impl AppArchivePassphrase {
    pub fn parse(bytes: &[u8]) -> Result<Self, AppPortableArchiveTransferError> {
        if !(12..=1_024).contains(&bytes.len()) {
            return Err(AppPortableArchiveTransferError::InvalidPassphrase);
        }
        Ok(Self(Zeroizing::new(bytes.to_vec())))
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArchiveWriteReceipt {
    pub envelope_version: u8,
    pub logical_payload_digest: AppDigest,
    pub envelope_header_digest: AppDigest,
    pub ciphertext_digest: AppDigest,
    pub byte_count: u64,
    pub encrypted: bool,
}

#[derive(Debug)]
pub struct EncodedAppPortableArchive {
    pub bytes: Vec<u8>,
    pub receipt: AppArchiveWriteReceipt,
    pub media_type: &'static str,
}

#[derive(Debug)]
pub struct DecodedAppPortableArchive {
    pub logical: AppLogicalArchive,
    pub package_archive: Option<Vec<u8>>,
    pub receipt: AppArchiveWriteReceipt,
}

#[derive(Debug, Error)]
pub enum AppPortableArchiveTransferError {
    #[error("portable archive exceeds its fixed byte ceiling")]
    ArchiveTooLarge,
    #[error("portable archive passphrase must contain 12-1024 bytes")]
    InvalidPassphrase,
    #[error("portable archive envelope is malformed or unsupported")]
    InvalidEnvelope,
    #[error("portable archive authentication failed")]
    AuthenticationFailed,
    #[error("portable archive package payload is missing or mismatched")]
    PackageBindingMismatch,
    #[error("portable archive JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("portable archive contract failed: {0}")]
    Contract(#[from] AppPortabilityError),
    #[error("portable package archive failed: {0}")]
    Package(#[from] AppPackageTransferError),
    #[error("portable archive cryptography failed")]
    Crypto,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PhysicalPayload {
    logical: AppLogicalArchive,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    package_archive_base64url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeHeader {
    envelope_version: u8,
    mode: EnvelopeMode,
    logical_payload_digest: AppDigest,
    payload_bytes: u64,
    chunk_bytes: u32,
    chunk_count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    salt_base64url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    nonce_base64url: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum EnvelopeMode {
    Xchacha20Poly1305Argon2id,
    ExplicitPlaintext,
}

pub fn encode_app_portable_archive(
    logical: AppLogicalArchive,
    package_archive: Option<&[u8]>,
    plan: &AppArchiveWritePlan,
    passphrase: Option<&AppArchivePassphrase>,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<EncodedAppPortableArchive, AppPortableArchiveTransferError> {
    let limits = AppContractLimits::default();
    let logical_digest = logical.logical_digest(&limits)?;
    if &logical_digest != plan.logical_payload_digest() {
        return Err(AppPortableArchiveTransferError::PackageBindingMismatch);
    }
    let package_archive_base64url = bind_package_archive(&logical, package_archive)?;
    let value = serde_json::to_value(PhysicalPayload {
        logical,
        package_archive_base64url,
    })?;
    let mut plaintext = canonical_json_bytes(&value)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    if plaintext.is_empty() || plaintext.len() > LOGICAL_PAYLOAD_MAX_BYTES {
        plaintext.zeroize();
        return Err(AppPortableArchiveTransferError::ArchiveTooLarge);
    }

    let encoded = match plan.protection() {
        AppArchiveProtectionPlan::Encrypted { .. } => {
            let passphrase =
                passphrase.ok_or(AppPortableArchiveTransferError::InvalidPassphrase)?;
            encode_encrypted(
                plaintext.as_slice(),
                logical_digest,
                plan,
                passphrase,
                authenticated,
                now,
            )
        },
        AppArchiveProtectionPlan::Plaintext { .. } => {
            encode_plaintext(plaintext.as_slice(), logical_digest)
        },
    };
    plaintext.zeroize();
    encoded
}

pub fn decode_app_portable_archive(
    bytes: &[u8],
    passphrase: Option<&AppArchivePassphrase>,
) -> Result<DecodedAppPortableArchive, AppPortableArchiveTransferError> {
    if bytes.len() < MAGIC.len() + 4 || bytes.len() > APP_PORTABLE_ARCHIVE_MAX_BYTES {
        return Err(AppPortableArchiveTransferError::ArchiveTooLarge);
    }
    if &bytes[..MAGIC.len()] != MAGIC {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    let header_len = u32::from_be_bytes(
        bytes[MAGIC.len()..MAGIC.len() + 4]
            .try_into()
            .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?,
    ) as usize;
    if header_len == 0 || header_len > HEADER_MAX_BYTES {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    let header_start = MAGIC.len() + 4;
    let header_end = header_start
        .checked_add(header_len)
        .filter(|end| *end <= bytes.len())
        .ok_or(AppPortableArchiveTransferError::InvalidEnvelope)?;
    let header_bytes = &bytes[header_start..header_end];
    if !json_bytes_nesting_is_bounded(header_bytes, 8)
        || !json_bytes_nodes_are_bounded(header_bytes, 64)
    {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    let header: EnvelopeHeader = serde_json::from_slice(header_bytes)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    validate_header(&header)?;
    if canonical_header(&header)?.as_slice() != header_bytes {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    let body = &bytes[header_end..];
    let mut plaintext = match header.mode {
        EnvelopeMode::ExplicitPlaintext => {
            if header.chunk_count != 1 || body.len() != header.payload_bytes as usize {
                return Err(AppPortableArchiveTransferError::InvalidEnvelope);
            }
            body.to_vec()
        },
        EnvelopeMode::Xchacha20Poly1305Argon2id => {
            let passphrase =
                passphrase.ok_or(AppPortableArchiveTransferError::InvalidPassphrase)?;
            decode_encrypted(body, header_bytes, &header, passphrase)?
        },
    };
    let ciphertext_digest = AppDigest::blake3(body);
    if !json_bytes_nesting_is_bounded(
        &plaintext,
        AppContractLimits::default()
            .max_json_depth()
            .saturating_add(8),
    ) || !json_bytes_nodes_are_bounded(&plaintext, LOGICAL_PAYLOAD_MAX_JSON_NODES)
    {
        plaintext.zeroize();
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    let payload: PhysicalPayload = serde_json::from_slice(&plaintext)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    plaintext.zeroize();
    let actual_digest = payload
        .logical
        .logical_digest(&AppContractLimits::default())?;
    if actual_digest != header.logical_payload_digest {
        return Err(AppPortableArchiveTransferError::AuthenticationFailed);
    }
    let package_archive =
        decode_and_bind_package(&payload.logical, payload.package_archive_base64url)?;
    Ok(DecodedAppPortableArchive {
        logical: payload.logical,
        package_archive,
        receipt: AppArchiveWriteReceipt {
            envelope_version: header.envelope_version,
            logical_payload_digest: header.logical_payload_digest,
            envelope_header_digest: AppDigest::blake3(header_bytes),
            ciphertext_digest,
            byte_count: bytes.len() as u64,
            encrypted: header.mode == EnvelopeMode::Xchacha20Poly1305Argon2id,
        },
    })
}

fn bind_package_archive(
    logical: &AppLogicalArchive,
    package_archive: Option<&[u8]>,
) -> Result<Option<String>, AppPortableArchiveTransferError> {
    match logical {
        AppLogicalArchive::Data { .. } if package_archive.is_none() => Ok(None),
        AppLogicalArchive::Package { package } | AppLogicalArchive::Combined { package, .. } => {
            let bytes =
                package_archive.ok_or(AppPortableArchiveTransferError::PackageBindingMismatch)?;
            if bytes.len() > APP_PACKAGE_ARCHIVE_MAX_BYTES {
                return Err(AppPortableArchiveTransferError::ArchiveTooLarge);
            }
            let admitted = admit_package_archive(bytes)?;
            if admitted.package() != package {
                return Err(AppPortableArchiveTransferError::PackageBindingMismatch);
            }
            Ok(Some(URL_SAFE_NO_PAD.encode(bytes)))
        },
        AppLogicalArchive::Data { .. } => {
            Err(AppPortableArchiveTransferError::PackageBindingMismatch)
        },
    }
}

fn decode_and_bind_package(
    logical: &AppLogicalArchive,
    encoded: Option<String>,
) -> Result<Option<Vec<u8>>, AppPortableArchiveTransferError> {
    let decoded = encoded
        .map(|value| {
            URL_SAFE_NO_PAD
                .decode(value)
                .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)
        })
        .transpose()?;
    bind_package_archive(logical, decoded.as_deref())?;
    Ok(decoded)
}

fn encode_plaintext(
    plaintext: &[u8],
    logical_digest: AppDigest,
) -> Result<EncodedAppPortableArchive, AppPortableArchiveTransferError> {
    let header = EnvelopeHeader {
        envelope_version: APP_ARCHIVE_ENVELOPE_VERSION,
        mode: EnvelopeMode::ExplicitPlaintext,
        logical_payload_digest: logical_digest.clone(),
        payload_bytes: plaintext.len() as u64,
        chunk_bytes: plaintext.len() as u32,
        chunk_count: 1,
        salt_base64url: None,
        nonce_base64url: None,
    };
    let header_bytes = canonical_header(&header)?;
    let mut bytes = envelope_prefix(&header_bytes)?;
    bytes.extend_from_slice(plaintext);
    let receipt = AppArchiveWriteReceipt {
        envelope_version: header.envelope_version,
        logical_payload_digest: logical_digest,
        envelope_header_digest: AppDigest::blake3(&header_bytes),
        ciphertext_digest: AppDigest::blake3(plaintext),
        byte_count: bytes.len() as u64,
        encrypted: false,
    };
    Ok(EncodedAppPortableArchive {
        bytes,
        receipt,
        media_type: APP_PORTABLE_ARCHIVE_PLAINTEXT_MEDIA_TYPE,
    })
}

fn encode_encrypted(
    plaintext: &[u8],
    logical_digest: AppDigest,
    plan: &AppArchiveWritePlan,
    passphrase: &AppArchivePassphrase,
    authenticated: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<EncodedAppPortableArchive, AppPortableArchiveTransferError> {
    let chunk_count = plaintext.len().div_ceil(CHUNK_BYTES);
    let mut salt = [0u8; 16];
    let mut base_nonce = [0u8; 24];
    OsRng.fill_bytes(&mut salt);
    OsRng.fill_bytes(&mut base_nonce);
    let header = EnvelopeHeader {
        envelope_version: APP_ARCHIVE_ENVELOPE_VERSION,
        mode: EnvelopeMode::Xchacha20Poly1305Argon2id,
        logical_payload_digest: logical_digest.clone(),
        payload_bytes: plaintext.len() as u64,
        chunk_bytes: CHUNK_BYTES as u32,
        chunk_count: u32::try_from(chunk_count)
            .map_err(|_| AppPortableArchiveTransferError::ArchiveTooLarge)?,
        salt_base64url: Some(URL_SAFE_NO_PAD.encode(salt)),
        nonce_base64url: Some(URL_SAFE_NO_PAD.encode(base_nonce)),
    };
    let header_bytes = canonical_header(&header)?;
    let mut key = derive_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|_| AppPortableArchiveTransferError::Crypto)?;
    key.zeroize();
    let mut bytes = envelope_prefix(&header_bytes)?;
    let body_start = bytes.len();
    bytes.reserve(plaintext.len() + chunk_count * (4 + TAG_BYTES));
    for (index, chunk) in plaintext.chunks(CHUNK_BYTES).enumerate() {
        let nonce = chunk_nonce(base_nonce, index as u64);
        let aad = chunk_aad(&header_bytes, index as u64);
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: chunk,
                    aad: &aad,
                },
            )
            .map_err(|_| AppPortableArchiveTransferError::Crypto)?;
        bytes.extend_from_slice(&(ciphertext.len() as u32).to_be_bytes());
        bytes.extend_from_slice(&ciphertext);
    }
    let ciphertext_digest = AppDigest::blake3(&bytes[body_start..]);
    let header_digest = AppDigest::blake3(&header_bytes);
    if bytes.len() > APP_PORTABLE_ARCHIVE_MAX_BYTES {
        return Err(AppPortableArchiveTransferError::ArchiveTooLarge);
    }
    let _verified = AppVerifiedEncryptedArchive::from_verified_writer(
        plan,
        authenticated,
        now,
        header_digest.clone(),
        ciphertext_digest.clone(),
        bytes.len() as u64,
    )?;
    Ok(EncodedAppPortableArchive {
        receipt: AppArchiveWriteReceipt {
            envelope_version: header.envelope_version,
            logical_payload_digest: logical_digest,
            envelope_header_digest: header_digest,
            ciphertext_digest,
            byte_count: bytes.len() as u64,
            encrypted: true,
        },
        bytes,
        media_type: APP_PORTABLE_ARCHIVE_ENCRYPTED_MEDIA_TYPE,
    })
}

fn decode_encrypted(
    body: &[u8],
    header_bytes: &[u8],
    header: &EnvelopeHeader,
    passphrase: &AppArchivePassphrase,
) -> Result<Vec<u8>, AppPortableArchiveTransferError> {
    let salt = decode_fixed::<16>(header.salt_base64url.as_deref())?;
    let base_nonce = decode_fixed::<24>(header.nonce_base64url.as_deref())?;
    let mut key = derive_key(passphrase, &salt)?;
    let cipher = XChaCha20Poly1305::new_from_slice(&key)
        .map_err(|_| AppPortableArchiveTransferError::Crypto)?;
    key.zeroize();
    let mut cursor = Cursor::new(body);
    let mut plaintext = Vec::with_capacity(header.payload_bytes as usize);
    for index in 0..header.chunk_count {
        let position = cursor.position() as usize;
        let length_bytes = body
            .get(position..position + 4)
            .ok_or(AppPortableArchiveTransferError::InvalidEnvelope)?;
        let length = u32::from_be_bytes(length_bytes.try_into().unwrap()) as usize;
        let expected_plaintext = if index + 1 == header.chunk_count {
            let remainder = header.payload_bytes as usize % CHUNK_BYTES;
            if remainder == 0 {
                CHUNK_BYTES
            } else {
                remainder
            }
        } else {
            CHUNK_BYTES
        };
        if length != expected_plaintext + TAG_BYTES {
            return Err(AppPortableArchiveTransferError::InvalidEnvelope);
        }
        cursor.set_position((position + 4) as u64);
        let start = cursor.position() as usize;
        let end = start
            .checked_add(length)
            .filter(|end| *end <= body.len())
            .ok_or(AppPortableArchiveTransferError::InvalidEnvelope)?;
        let nonce = chunk_nonce(base_nonce, index as u64);
        let aad = chunk_aad(header_bytes, index as u64);
        let chunk = cipher
            .decrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &body[start..end],
                    aad: &aad,
                },
            )
            .map_err(|_| AppPortableArchiveTransferError::AuthenticationFailed)?;
        plaintext.extend_from_slice(&chunk);
        if plaintext.len() > LOGICAL_PAYLOAD_MAX_BYTES {
            plaintext.zeroize();
            return Err(AppPortableArchiveTransferError::ArchiveTooLarge);
        }
        cursor.set_position(end as u64);
    }
    if cursor.position() as usize != body.len() || plaintext.len() != header.payload_bytes as usize
    {
        plaintext.zeroize();
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    Ok(plaintext)
}

fn validate_header(header: &EnvelopeHeader) -> Result<(), AppPortableArchiveTransferError> {
    let payload_bytes = usize::try_from(header.payload_bytes)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    if header.envelope_version != APP_ARCHIVE_ENVELOPE_VERSION
        || payload_bytes == 0
        || payload_bytes > LOGICAL_PAYLOAD_MAX_BYTES
        || header.chunk_count == 0
    {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    match header.mode {
        EnvelopeMode::Xchacha20Poly1305Argon2id
            if header.chunk_bytes == CHUNK_BYTES as u32
                && header.salt_base64url.is_some()
                && header.nonce_base64url.is_some()
                && header.chunk_count as usize == payload_bytes.div_ceil(CHUNK_BYTES) =>
        {
            Ok(())
        },
        EnvelopeMode::ExplicitPlaintext
            if header.salt_base64url.is_none()
                && header.nonce_base64url.is_none()
                && header.chunk_count == 1
                && header.chunk_bytes == header.payload_bytes as u32 =>
        {
            Ok(())
        },
        _ => Err(AppPortableArchiveTransferError::InvalidEnvelope),
    }
}

fn canonical_header(header: &EnvelopeHeader) -> Result<Vec<u8>, AppPortableArchiveTransferError> {
    let value = serde_json::to_value(header)?;
    let bytes = canonical_json_bytes(&value)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    if bytes.is_empty() || bytes.len() > HEADER_MAX_BYTES {
        return Err(AppPortableArchiveTransferError::InvalidEnvelope);
    }
    Ok(bytes)
}

fn envelope_prefix(header: &[u8]) -> Result<Vec<u8>, AppPortableArchiveTransferError> {
    let length = u32::try_from(header.len())
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    let mut bytes = Vec::with_capacity(MAGIC.len() + 4 + header.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(header);
    Ok(bytes)
}

fn derive_key(
    passphrase: &AppArchivePassphrase,
    salt: &[u8; 16],
) -> Result<[u8; 32], AppPortableArchiveTransferError> {
    let params =
        Params::new(65_536, 3, 1, Some(32)).map_err(|_| AppPortableArchiveTransferError::Crypto)?;
    let mut key = [0u8; 32];
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.0.as_slice(), salt, &mut key)
        .map_err(|_| AppPortableArchiveTransferError::Crypto)?;
    Ok(key)
}

fn chunk_nonce(mut base: [u8; 24], index: u64) -> [u8; 24] {
    let suffix = u64::from_be_bytes(base[16..24].try_into().unwrap()).wrapping_add(index);
    base[16..24].copy_from_slice(&suffix.to_be_bytes());
    base
}

fn chunk_aad(header: &[u8], index: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(header.len() + 8);
    aad.extend_from_slice(header);
    aad.extend_from_slice(&index.to_be_bytes());
    aad
}

fn decode_fixed<const N: usize>(
    value: Option<&str>,
) -> Result<[u8; N], AppPortableArchiveTransferError> {
    let decoded = URL_SAFE_NO_PAD
        .decode(value.ok_or(AppPortableArchiveTransferError::InvalidEnvelope)?)
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)?;
    decoded
        .try_into()
        .map_err(|_| AppPortableArchiveTransferError::InvalidEnvelope)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::{
        models::{AppDataClassification, AppName, AppReference, AppRevision},
        portability::{
            authorize_archive_write, AppArchiveProtectionRequest, AppDataArchiveManifest,
            AppExportSourceState, AppPortableAlias, AppPortableAliasKind, AppPortableProvenance,
            AppPortableRecordRevision,
        },
        records::AppRecordActorKind,
        registry::tests::{authenticated_scope, time},
    };

    fn data_archive() -> AppDataArchiveManifest {
        let payload = json!({"title": "portable"});
        AppDataArchiveManifest::from_trusted_export_projection(
            AppReference::parse("package:portable").unwrap(),
            AppDigest::blake3(b"package"),
            AppDigest::blake3(b"schema"),
            AppExportSourceState::Enabled,
            vec![AppPortableRecordRevision {
                record_alias: AppPortableAlias::new(AppPortableAliasKind::Record, 1).unwrap(),
                entity_name: AppName::parse("item").unwrap(),
                record_revision: AppRevision::new(1).unwrap(),
                schema_revision: AppRevision::new(1).unwrap(),
                payload_digest: AppDigest::blake3_canonical_json(&payload).unwrap(),
                payload,
                classification: AppDataClassification::Personal,
                created_at: time(1),
                updated_at: time(1),
                deleted_at: None,
                provenance: AppPortableProvenance {
                    actor_kind: AppRecordActorKind::User,
                    actor_alias: None,
                    execution_alias: None,
                    source_aliases: Vec::new(),
                },
            }],
            Vec::new(),
            &AppContractLimits::default(),
        )
        .unwrap()
    }

    #[test]
    fn encrypted_data_archive_round_trip_and_tamper_fail_closed() {
        let authenticated = authenticated_scope("anonymous", "default");
        let logical = AppLogicalArchive::Data {
            data: data_archive(),
        };
        let plan = authorize_archive_write(
            &logical,
            AppArchiveProtectionRequest::Default,
            None,
            &authenticated,
            time(2),
            &AppContractLimits::default(),
        )
        .unwrap();
        let passphrase = AppArchivePassphrase::parse(b"correct horse battery staple").unwrap();
        let encoded = encode_app_portable_archive(
            logical.clone(),
            None,
            &plan,
            Some(&passphrase),
            &authenticated,
            time(2),
        )
        .unwrap();
        assert!(encoded.receipt.encrypted);
        assert_eq!(
            decode_app_portable_archive(&encoded.bytes, Some(&passphrase))
                .unwrap()
                .logical,
            logical
        );
        let wrong = AppArchivePassphrase::parse(b"this is definitely wrong").unwrap();
        assert!(matches!(
            decode_app_portable_archive(&encoded.bytes, Some(&wrong)),
            Err(AppPortableArchiveTransferError::AuthenticationFailed)
        ));
        let mut tampered = encoded.bytes;
        let index = tampered.len() - 1;
        tampered[index] ^= 1;
        assert!(matches!(
            decode_app_portable_archive(&tampered, Some(&passphrase)),
            Err(AppPortableArchiveTransferError::AuthenticationFailed)
        ));
    }
}

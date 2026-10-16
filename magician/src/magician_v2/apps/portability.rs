//! Package/data portability and import-trust contracts for installable apps.
//!
//! This contract module deliberately does not read physical archives, encrypt
//! bytes, verify signatures, allocate package IDs, or mutate the app store. It
//! defines the bounded logical payloads and server-minted evidence consumed by
//! package transfer and the Phase-2D entity-portability adapter. Package
//! software, personal data, authority and publisher lineage therefore cannot
//! be conflated by a generic archive flag.

use std::{collections::HashSet, num::NonZeroU32};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::{AppScopeAuthentication, AuthenticatedAppScope},
    manifest::{AppBundlePath, AppPackageCandidate},
    models::{
        validate_bounded, validate_json_value, validate_nonempty_bounded, AppContractError,
        AppContractLimits, AppDataClassification, AppDigest, AppInstallationId, AppName,
        AppRecordId, AppReference, AppRevision, AppScopeBindingRef, ValidateAppContract,
    },
    package_lock::{AppPackageLock, AppPortablePackageLockClaim},
    records::{AppPackageRevision, AppRecordActorKind},
};
use crate::magician_v2::json_traversal::discard_json_iteratively;

pub const APP_PORTABLE_ARCHIVE_VERSION: u8 = 2;
pub const APP_ARCHIVE_ENVELOPE_VERSION: u8 = 1;
pub const APP_IMPORT_PREVIEW_VERSION: u8 = 1;
const APP_ARCHIVE_CHUNK_BYTES: u32 = 1_048_576;
const APP_MAX_DATA_ARCHIVE_ATTACHMENT_BYTES: u64 = 4 * 1_024 * 1_024 * 1_024;
pub const APP_MAX_DATA_ARCHIVE_RECORDS: usize = 10_000;
pub const APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_BYTES: usize = 16 * 1_024 * 1_024;
pub const APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_NODES: usize = 160_000;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppExportSourceState {
    Enabled,
    Disabled,
    Quarantined,
    UninstalledRetained,
    RetiredPackage,
    SecurityRevokedPackage,
    Purged,
}

impl AppExportSourceState {
    fn permits_package_export(self) -> bool {
        matches!(
            self,
            Self::Enabled
                | Self::Disabled
                | Self::Quarantined
                | Self::UninstalledRetained
                | Self::RetiredPackage
        )
    }

    fn permits_data_export(self) -> bool {
        !matches!(self, Self::Purged)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPortableEvidenceKind {
    DeterministicConformance,
    CustomSurfaceVerification,
    CompatibilityQualification,
}

/// Advisory evidence may help a destination explain an imported package, but
/// never satisfies local conformance, rebuild, sandbox or permission review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPortableVerificationEvidence {
    pub kind: AppPortableEvidenceKind,
    pub evidence_digest: AppDigest,
    pub producer_ref: AppReference,
    pub produced_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPortablePackageMember {
    pub path: AppBundlePath,
    pub content_digest: AppDigest,
    pub byte_len: u64,
}

/// Logical package payload. It has no installation, scope, grant, credential,
/// schedule, memory or provider-session fields by construction.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageArchiveManifest {
    pub archive_version: u8,
    pub package_revision_ref: AppReference,
    pub package_id: AppReference,
    pub publisher_identity: AppReference,
    pub semantic_version: String,
    pub package_content_digest: AppDigest,
    pub manifest_digest: AppDigest,
    pub dependency_lock: AppPortablePackageLockClaim,
    pub dependency_lock_digest: AppDigest,
    pub members: Vec<AppPortablePackageMember>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub advisory_verification_evidence: Vec<AppPortableVerificationEvidence>,
    pub logical_payload_digest: AppDigest,
}

#[derive(Serialize)]
struct AppPackageArchiveDigestMaterial<'a> {
    archive_version: u8,
    package_revision_ref: &'a AppReference,
    package_id: &'a AppReference,
    publisher_identity: &'a AppReference,
    semantic_version: &'a str,
    package_content_digest: &'a AppDigest,
    manifest_digest: &'a AppDigest,
    dependency_lock: &'a AppPortablePackageLockClaim,
    dependency_lock_digest: &'a AppDigest,
    members: &'a [AppPortablePackageMember],
    advisory_verification_evidence: &'a [AppPortableVerificationEvidence],
}

#[allow(clippy::too_many_arguments)]
pub fn build_package_archive_manifest(
    package_revision_ref: AppReference,
    revision: &AppPackageRevision,
    candidate: &AppPackageCandidate,
    lock: &AppPackageLock,
    source_state: AppExportSourceState,
    mut advisory_verification_evidence: Vec<AppPortableVerificationEvidence>,
    limits: &AppContractLimits,
) -> Result<AppPackageArchiveManifest, AppPortabilityError> {
    if !source_state.permits_package_export() {
        return Err(AppPortabilityError::PackageExportDenied(source_state));
    }
    revision
        .validate_app_contract(limits)
        .map_err(AppPortabilityError::InvalidContract)?;
    if &revision.content_digest != candidate.bundle_digest() {
        return Err(AppPortabilityError::PackageDigestMismatch);
    }
    if lock.bundle_digest() != candidate.bundle_digest()
        || &revision.dependency_lock_digest != lock.lock_digest()
    {
        return Err(AppPortabilityError::DependencyLockMismatch);
    }
    validate_bounded(
        "package_archive.advisory_verification_evidence",
        advisory_verification_evidence.len(),
        limits.max_collection_items(),
    )
    .map_err(AppPortabilityError::InvalidContract)?;

    let mut evidence_keys = HashSet::with_capacity(advisory_verification_evidence.len());
    for evidence in &advisory_verification_evidence {
        if !evidence_keys.insert((evidence.kind, &evidence.evidence_digest)) {
            return Err(AppPortabilityError::DuplicatePortableEvidence);
        }
    }

    advisory_verification_evidence.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.evidence_digest.cmp(&right.evidence_digest))
    });
    let members = candidate
        .members()
        .iter()
        .map(|member| AppPortablePackageMember {
            path: member.path().clone(),
            content_digest: member.content_digest().clone(),
            byte_len: u64::try_from(member.bytes().len()).unwrap_or(u64::MAX),
        })
        .collect::<Vec<_>>();
    validate_nonempty_bounded(
        "package_archive.members",
        members.len(),
        limits.max_collection_items().saturating_mul(4),
    )
    .map_err(AppPortabilityError::InvalidContract)?;

    let mut manifest = AppPackageArchiveManifest {
        archive_version: APP_PORTABLE_ARCHIVE_VERSION,
        package_revision_ref,
        package_id: revision.package_id.clone(),
        publisher_identity: revision.publisher_identity.clone(),
        semantic_version: revision.semantic_version.clone(),
        package_content_digest: revision.content_digest.clone(),
        manifest_digest: candidate.manifest().manifest_digest().clone(),
        dependency_lock: AppPortablePackageLockClaim::from_trusted(lock),
        dependency_lock_digest: revision.dependency_lock_digest.clone(),
        members,
        advisory_verification_evidence,
        logical_payload_digest: AppDigest::blake3(b"pending"),
    };
    manifest.logical_payload_digest = manifest.recompute_digest()?;
    manifest
        .validate_app_contract(limits)
        .map_err(AppPortabilityError::InvalidContract)?;
    Ok(manifest)
}

impl AppPackageArchiveManifest {
    pub fn recompute_digest(&self) -> Result<AppDigest, AppPortabilityError> {
        let value = serde_json::to_value(AppPackageArchiveDigestMaterial {
            archive_version: self.archive_version,
            package_revision_ref: &self.package_revision_ref,
            package_id: &self.package_id,
            publisher_identity: &self.publisher_identity,
            semantic_version: &self.semantic_version,
            package_content_digest: &self.package_content_digest,
            manifest_digest: &self.manifest_digest,
            dependency_lock: &self.dependency_lock,
            dependency_lock_digest: &self.dependency_lock_digest,
            members: &self.members,
            advisory_verification_evidence: &self.advisory_verification_evidence,
        })
        .map_err(|error| AppPortabilityError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppPortabilityError::Digest(error.to_string()))
    }
}

impl ValidateAppContract for AppPackageArchiveManifest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.archive_version != APP_PORTABLE_ARCHIVE_VERSION {
            return Err(AppContractError::invalid(
                "package_archive.archive_version",
                "unsupported package archive version",
            ));
        }
        semver::Version::parse(&self.semantic_version).map_err(|error| {
            AppContractError::invalid("package_archive.semantic_version", error.to_string())
        })?;
        let claimed_lock = self.dependency_lock.claimed_lock();
        if self.dependency_lock_digest != *claimed_lock.lock_digest()
            || self.manifest_digest != *claimed_lock.manifest_digest()
            || self.package_content_digest != *claimed_lock.bundle_digest()
        {
            return Err(AppContractError::invalid(
                "package_archive.dependency_lock",
                "portable lock identity does not match the package manifest",
            ));
        }
        validate_nonempty_bounded(
            "package_archive.members",
            self.members.len(),
            limits.max_collection_items().saturating_mul(4),
        )?;
        validate_bounded(
            "package_archive.advisory_verification_evidence",
            self.advisory_verification_evidence.len(),
            limits.max_collection_items(),
        )?;
        if !self
            .advisory_verification_evidence
            .windows(2)
            .all(|evidence| {
                (evidence[0].kind, &evidence[0].evidence_digest)
                    < (evidence[1].kind, &evidence[1].evidence_digest)
            })
        {
            return Err(AppContractError::invalid(
                "package_archive.advisory_verification_evidence",
                "must be strictly ordered by kind and digest",
            ));
        }
        if !self
            .members
            .windows(2)
            .all(|members| members[0].path.collision_key() < members[1].path.collision_key())
        {
            return Err(AppContractError::invalid(
                "package_archive.members.path",
                "members must be strictly ordered by normalized path",
            ));
        }
        let mut paths = HashSet::with_capacity(self.members.len());
        for member in &self.members {
            if !paths.insert(member.path.collision_key()) {
                return Err(AppContractError::invalid(
                    "package_archive.members.path",
                    "contains a normalized path collision",
                ));
            }
        }
        let expected = self.recompute_digest().map_err(|error| {
            AppContractError::invalid("logical_payload_digest", error.to_string())
        })?;
        if self.logical_payload_digest != expected {
            return Err(AppContractError::invalid(
                "logical_payload_digest",
                "does not match the canonical package archive manifest",
            ));
        }
        Ok(())
    }
}

/// Portable aliases are archive-local ordinals. They cannot smuggle source
/// installation, scope, actor, record or execution IDs into another scope.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(deny_unknown_fields)]
pub struct AppPortableAlias {
    pub kind: AppPortableAliasKind,
    pub ordinal: NonZeroU32,
}

impl AppPortableAlias {
    pub fn new(kind: AppPortableAliasKind, ordinal: u32) -> Result<Self, AppContractError> {
        let ordinal = NonZeroU32::new(ordinal).ok_or_else(|| {
            AppContractError::invalid("portable_alias.ordinal", "must be greater than zero")
        })?;
        Ok(Self { kind, ordinal })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPortableAliasKind {
    Record,
    Actor,
    Execution,
    Attachment,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPortableProvenance {
    pub actor_kind: AppRecordActorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor_alias: Option<AppPortableAlias>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_alias: Option<AppPortableAlias>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_aliases: Vec<AppPortableAlias>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppPortableRecordRevision {
    pub record_alias: AppPortableAlias,
    pub entity_name: AppName,
    pub record_revision: AppRevision,
    pub schema_revision: AppRevision,
    pub payload: Value,
    pub payload_digest: AppDigest,
    pub classification: AppDataClassification,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at: Option<DateTime<Utc>>,
    pub provenance: AppPortableProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPortableAttachment {
    pub attachment_alias: AppPortableAlias,
    pub content_digest: AppDigest,
    pub byte_len: u64,
    pub media_type: String,
    pub classification: AppDataClassification,
}

/// Logical data payload. Source scope and installation IDs are deliberately
/// absent; imported records always receive destination-local IDs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AppDataArchiveManifest {
    pub archive_version: u8,
    pub package_id: AppReference,
    pub package_content_digest: AppDigest,
    pub entity_schema_digest: AppDigest,
    pub records: Vec<AppPortableRecordRevision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AppPortableAttachment>,
    pub maximum_classification: AppDataClassification,
    pub logical_payload_digest: AppDigest,
}

#[derive(Serialize)]
struct AppDataArchiveDigestMaterial<'a> {
    archive_version: u8,
    package_id: &'a AppReference,
    package_content_digest: &'a AppDigest,
    entity_schema_digest: &'a AppDigest,
    records: Vec<AppPortableRecordDigestMaterial<'a>>,
    attachments: &'a [AppPortableAttachment],
    maximum_classification: AppDataClassification,
}

/// Payload bytes are bound through their already revalidated canonical digest.
/// Keeping the aggregate digest material metadata-only avoids cloning every
/// record payload into a second JSON tree during export and validation.
#[derive(Serialize)]
struct AppPortableRecordDigestMaterial<'a> {
    record_alias: AppPortableAlias,
    entity_name: &'a AppName,
    record_revision: AppRevision,
    schema_revision: AppRevision,
    payload_digest: &'a AppDigest,
    classification: AppDataClassification,
    created_at: &'a DateTime<Utc>,
    updated_at: &'a DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    deleted_at: &'a Option<DateTime<Utc>>,
    provenance: &'a AppPortableProvenance,
}

impl AppDataArchiveManifest {
    pub fn from_trusted_export_projection(
        package_id: AppReference,
        package_content_digest: AppDigest,
        entity_schema_digest: AppDigest,
        source_state: AppExportSourceState,
        mut records: Vec<AppPortableRecordRevision>,
        attachments: Vec<AppPortableAttachment>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppPortabilityError> {
        if let Err(error) = preflight_data_archive_inputs(&records, &attachments, limits) {
            discard_portable_record_payloads(&mut records);
            return Err(AppPortabilityError::InvalidContract(error));
        }
        if !source_state.permits_data_export() {
            return Err(AppPortabilityError::DataExportDenied(source_state));
        }
        let maximum_classification = records
            .iter()
            .map(|record| record.classification)
            .chain(
                attachments
                    .iter()
                    .map(|attachment| attachment.classification),
            )
            .max()
            .unwrap_or(AppDataClassification::Ordinary);
        records.sort_by_key(|record| record.record_alias);
        let mut attachments = attachments;
        attachments.sort_by_key(|attachment| attachment.attachment_alias);
        let mut manifest = Self {
            archive_version: APP_PORTABLE_ARCHIVE_VERSION,
            package_id,
            package_content_digest,
            entity_schema_digest,
            records,
            attachments,
            maximum_classification,
            logical_payload_digest: AppDigest::blake3(b"pending"),
        };
        manifest.logical_payload_digest = manifest.recompute_digest()?;
        manifest
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        Ok(manifest)
    }

    fn recompute_digest(&self) -> Result<AppDigest, AppPortabilityError> {
        let records = self
            .records
            .iter()
            .map(|record| AppPortableRecordDigestMaterial {
                record_alias: record.record_alias,
                entity_name: &record.entity_name,
                record_revision: record.record_revision,
                schema_revision: record.schema_revision,
                payload_digest: &record.payload_digest,
                classification: record.classification,
                created_at: &record.created_at,
                updated_at: &record.updated_at,
                deleted_at: &record.deleted_at,
                provenance: &record.provenance,
            })
            .collect();
        let value = serde_json::to_value(AppDataArchiveDigestMaterial {
            archive_version: self.archive_version,
            package_id: &self.package_id,
            package_content_digest: &self.package_content_digest,
            entity_schema_digest: &self.entity_schema_digest,
            records,
            attachments: &self.attachments,
            maximum_classification: self.maximum_classification,
        })
        .map_err(|error| AppPortabilityError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppPortabilityError::Digest(error.to_string()))
    }
}

fn preflight_data_archive_inputs(
    records: &[AppPortableRecordRevision],
    attachments: &[AppPortableAttachment],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_nonempty_bounded(
        "data_archive.records",
        records.len(),
        APP_MAX_DATA_ARCHIVE_RECORDS,
    )?;
    validate_bounded(
        "data_archive.attachments",
        attachments.len(),
        limits.max_collection_items(),
    )?;
    let mut aggregate_nodes = 0usize;
    let mut aggregate_bytes = 0usize;
    for record in records {
        let (nodes, bytes) = validate_json_value(&record.payload, limits)?;
        aggregate_nodes = aggregate_nodes.checked_add(nodes).ok_or_else(|| {
            AppContractError::invalid(
                "data_archive.records.payload",
                "aggregate payload node count overflows",
            )
        })?;
        aggregate_bytes = aggregate_bytes.checked_add(bytes).ok_or_else(|| {
            AppContractError::invalid(
                "data_archive.records.payload",
                "aggregate payload byte count overflows",
            )
        })?;
        if aggregate_nodes > APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_NODES
            || aggregate_bytes > APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_BYTES
        {
            return Err(AppContractError::invalid(
                "data_archive.records.payload",
                "aggregate record payload exceeds the fixed archive ceiling",
            ));
        }
        let expected = AppDigest::blake3_canonical_json(&record.payload).map_err(|error| {
            AppContractError::invalid("data_archive.records.payload_digest", error.to_string())
        })?;
        if record.payload_digest != expected {
            return Err(AppContractError::invalid(
                "data_archive.records.payload_digest",
                "does not match canonical payload bytes",
            ));
        }
    }
    Ok(())
}

fn discard_portable_record_payloads(records: &mut Vec<AppPortableRecordRevision>) {
    for record in records {
        discard_json_iteratively(std::mem::take(&mut record.payload));
    }
}

impl ValidateAppContract for AppDataArchiveManifest {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.archive_version != APP_PORTABLE_ARCHIVE_VERSION {
            return Err(AppContractError::invalid(
                "data_archive.archive_version",
                "unsupported data archive version",
            ));
        }
        preflight_data_archive_inputs(&self.records, &self.attachments, limits)?;
        validate_nonempty_bounded(
            "data_archive.records",
            self.records.len(),
            APP_MAX_DATA_ARCHIVE_RECORDS,
        )?;
        validate_bounded(
            "data_archive.attachments",
            self.attachments.len(),
            limits.max_collection_items(),
        )?;
        if !self
            .records
            .windows(2)
            .all(|records| records[0].record_alias < records[1].record_alias)
            || !self.attachments.windows(2).all(|attachments| {
                attachments[0].attachment_alias < attachments[1].attachment_alias
            })
        {
            return Err(AppContractError::invalid(
                "data_archive",
                "records and attachments must be strictly ordered by portable alias",
            ));
        }
        let mut aliases = HashSet::with_capacity(self.records.len() + self.attachments.len());
        let mut maximum = AppDataClassification::Public;
        for record in &self.records {
            if record.record_alias.kind != AppPortableAliasKind::Record {
                return Err(AppContractError::invalid(
                    "data_archive.records.record_alias",
                    "must use a record alias",
                ));
            }
            if !aliases.insert(record.record_alias) {
                return Err(AppContractError::invalid(
                    "data_archive.records.record_alias",
                    "contains a duplicate portable alias",
                ));
            }
            if record.updated_at < record.created_at
                || record
                    .deleted_at
                    .as_ref()
                    .is_some_and(|deleted| deleted < &record.created_at)
            {
                return Err(AppContractError::invalid(
                    "data_archive.records.timestamps",
                    "updated/deleted timestamps cannot precede creation",
                ));
            }
            validate_portable_provenance(&record.provenance, limits)?;
            maximum = maximum.max(record.classification);
        }
        let mut declared_attachment_bytes = 0u64;
        for attachment in &self.attachments {
            if attachment.attachment_alias.kind != AppPortableAliasKind::Attachment {
                return Err(AppContractError::invalid(
                    "data_archive.attachments.attachment_alias",
                    "must use an attachment alias",
                ));
            }
            if attachment.byte_len == 0 || !is_portable_media_type(&attachment.media_type) {
                return Err(AppContractError::invalid(
                    "data_archive.attachments",
                    "attachments require a non-zero length and portable type/subtype media type",
                ));
            }
            declared_attachment_bytes = declared_attachment_bytes
                .checked_add(attachment.byte_len)
                .ok_or_else(|| {
                    AppContractError::invalid(
                        "data_archive.attachments.byte_len",
                        "declared attachment byte count overflows",
                    )
                })?;
            if declared_attachment_bytes > APP_MAX_DATA_ARCHIVE_ATTACHMENT_BYTES {
                return Err(AppContractError::invalid(
                    "data_archive.attachments.byte_len",
                    "declared attachments exceed the fixed archive byte ceiling",
                ));
            }
            if !aliases.insert(attachment.attachment_alias) {
                return Err(AppContractError::invalid(
                    "data_archive.attachments.attachment_alias",
                    "contains a duplicate portable alias",
                ));
            }
            maximum = maximum.max(attachment.classification);
        }
        if self.records.iter().any(|record| {
            record
                .provenance
                .source_aliases
                .iter()
                .any(|alias| !aliases.contains(alias))
        }) {
            return Err(AppContractError::invalid(
                "data_archive.records.provenance.source_aliases",
                "must resolve to a record or attachment declared by this archive",
            ));
        }
        if self.maximum_classification != maximum {
            return Err(AppContractError::invalid(
                "data_archive.maximum_classification",
                "must equal the maximum record/attachment classification",
            ));
        }
        let expected = self.recompute_digest().map_err(|error| {
            AppContractError::invalid("logical_payload_digest", error.to_string())
        })?;
        if self.logical_payload_digest != expected {
            return Err(AppContractError::invalid(
                "logical_payload_digest",
                "does not match the canonical data archive manifest",
            ));
        }
        Ok(())
    }
}

fn is_portable_media_type(value: &str) -> bool {
    if value.is_empty() || value.len() > 128 || !value.is_ascii() {
        return false;
    }
    let mut parts = value.split('/');
    let Some(top_level) = parts.next() else {
        return false;
    };
    let Some(subtype) = parts.next() else {
        return false;
    };
    if parts.next().is_some() {
        return false;
    }
    [top_level, subtype].into_iter().all(|part| {
        !part.is_empty()
            && part.bytes().all(|byte| {
                byte.is_ascii_alphanumeric()
                    || matches!(
                        byte,
                        b'!' | b'#' | b'$' | b'&' | b'^' | b'_' | b'.' | b'+' | b'-'
                    )
            })
    })
}

fn validate_portable_provenance(
    provenance: &AppPortableProvenance,
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_bounded(
        "portable_provenance.source_aliases",
        provenance.source_aliases.len(),
        limits.max_collection_items(),
    )?;
    if provenance
        .actor_alias
        .is_some_and(|alias| alias.kind != AppPortableAliasKind::Actor)
        || provenance
            .execution_alias
            .is_some_and(|alias| alias.kind != AppPortableAliasKind::Execution)
    {
        return Err(AppContractError::invalid(
            "portable_provenance",
            "actor/execution fields require matching portable alias kinds",
        ));
    }
    if !provenance
        .source_aliases
        .windows(2)
        .all(|aliases| aliases[0] < aliases[1])
        || provenance.source_aliases.iter().any(|alias| {
            !matches!(
                alias.kind,
                AppPortableAliasKind::Record | AppPortableAliasKind::Attachment
            )
        })
    {
        return Err(AppContractError::invalid(
            "portable_provenance.source_aliases",
            "must be strictly ordered record/attachment aliases",
        ));
    }
    let mut aliases = HashSet::with_capacity(provenance.source_aliases.len());
    if provenance
        .source_aliases
        .iter()
        .any(|alias| !aliases.insert(*alias))
    {
        return Err(AppContractError::invalid(
            "portable_provenance.source_aliases",
            "contains duplicate aliases",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppLogicalArchive {
    Package {
        package: AppPackageArchiveManifest,
    },
    Data {
        data: AppDataArchiveManifest,
    },
    Combined {
        package: AppPackageArchiveManifest,
        data: AppDataArchiveManifest,
    },
}

impl AppLogicalArchive {
    pub fn logical_digest(
        &self,
        limits: &AppContractLimits,
    ) -> Result<AppDigest, AppPortabilityError> {
        self.validate(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum LogicalDigestMaterial<'a> {
            Package {
                package_digest: &'a AppDigest,
            },
            Data {
                data_digest: &'a AppDigest,
            },
            Combined {
                package_digest: &'a AppDigest,
                data_digest: &'a AppDigest,
            },
        }
        let material = match self {
            Self::Package { package } => LogicalDigestMaterial::Package {
                package_digest: &package.logical_payload_digest,
            },
            Self::Data { data } => LogicalDigestMaterial::Data {
                data_digest: &data.logical_payload_digest,
            },
            Self::Combined { package, data } => LogicalDigestMaterial::Combined {
                package_digest: &package.logical_payload_digest,
                data_digest: &data.logical_payload_digest,
            },
        };
        let value = serde_json::to_value(material)
            .map_err(|error| AppPortabilityError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppPortabilityError::Digest(error.to_string()))
    }

    pub fn maximum_classification(&self) -> AppDataClassification {
        match self {
            Self::Package { .. } => AppDataClassification::Public,
            Self::Data { data } | Self::Combined { data, .. } => data.maximum_classification,
        }
    }

    fn validate(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        match self {
            Self::Package { package } => package.validate_app_contract(limits),
            Self::Data { data } => data.validate_app_contract(limits),
            Self::Combined { package, data } => {
                package.validate_app_contract(limits)?;
                data.validate_app_contract(limits)?;
                if package.package_id != data.package_id
                    || package.package_content_digest != data.package_content_digest
                {
                    return Err(AppContractError::invalid(
                        "combined_archive",
                        "package and data payloads must bind the same package identity and digest",
                    ));
                }
                Ok(())
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppArchiveProtectionRequest {
    Default,
    Encrypted,
    ExplicitPlaintext,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppArchiveCipher {
    Xchacha20Poly1305,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppArchiveKdf {
    Argon2idV19,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArchiveEncryptionProfile {
    pub envelope_version: u8,
    pub cipher: AppArchiveCipher,
    pub kdf: AppArchiveKdf,
    pub kdf_memory_kib: u32,
    pub kdf_iterations: u32,
    pub kdf_parallelism: u8,
    pub salt_bytes: u8,
    pub chunk_bytes: u32,
}

impl Default for AppArchiveEncryptionProfile {
    fn default() -> Self {
        Self {
            envelope_version: APP_ARCHIVE_ENVELOPE_VERSION,
            cipher: AppArchiveCipher::Xchacha20Poly1305,
            kdf: AppArchiveKdf::Argon2idV19,
            kdf_memory_kib: 65_536,
            kdf_iterations: 3,
            kdf_parallelism: 1,
            salt_bytes: 16,
            chunk_bytes: APP_ARCHIVE_CHUNK_BYTES,
        }
    }
}

/// Exact warned approval for one plaintext logical payload. It is
/// serialization-only and cannot be manufactured from a request body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPlaintextExportApproval {
    approval_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    logical_payload_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppPlaintextExportApproval {
    pub fn from_warned_user_action(
        authenticated_scope: &AuthenticatedAppScope,
        approval_ref: AppReference,
        logical_payload_digest: AppDigest,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<Self, AppPortabilityError> {
        if expires_at <= issued_at {
            return Err(AppPortabilityError::InvalidPlaintextApprovalWindow);
        }
        authenticated_scope
            .ensure_live_at(&issued_at)
            .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
        if expires_at > authenticated_scope.expires_at().to_owned() {
            return Err(AppPortabilityError::InvalidPlaintextApprovalWindow);
        }
        Ok(Self {
            approval_ref,
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            authentication: authenticated_scope.authentication(),
            authentication_revision: authenticated_scope.authentication_revision(),
            logical_payload_digest,
            issued_at,
            expires_at,
        })
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppArchiveProtectionPlan {
    Encrypted {
        profile: AppArchiveEncryptionProfile,
    },
    Plaintext {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        warned_approval_ref: Option<AppReference>,
    },
}

/// Server-produced write plan. It carries no key, passphrase, salt or nonce;
/// the existing key-management/encryption owner supplies those only inside the
/// streaming archive writer.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppArchiveWritePlan {
    archive_version: u8,
    logical_payload_digest: AppDigest,
    maximum_classification: AppDataClassification,
    protection: AppArchiveProtectionPlan,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    authorized_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppArchiveWritePlan {
    pub fn logical_payload_digest(&self) -> &AppDigest {
        &self.logical_payload_digest
    }

    pub fn protection(&self) -> &AppArchiveProtectionPlan {
        &self.protection
    }

    fn ensure_current_writer_authority(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
    ) -> Result<(), AppPortabilityError> {
        authenticated_scope
            .ensure_live_at(now)
            .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
        if now < &self.authorized_at
            || now >= &self.expires_at
            || self.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || self.actor_ref != *authenticated_scope.actor_ref()
            || self.session_ref != *authenticated_scope.session_ref()
            || self.authentication != authenticated_scope.authentication()
            || self.authentication_revision != authenticated_scope.authentication_revision()
        {
            return Err(AppPortabilityError::ArchiveWriteAuthorityMismatch);
        }
        Ok(())
    }
}

pub fn authorize_archive_write(
    archive: &AppLogicalArchive,
    request: AppArchiveProtectionRequest,
    plaintext_approval: Option<&AppPlaintextExportApproval>,
    authenticated_scope: &AuthenticatedAppScope,
    now: DateTime<Utc>,
    limits: &AppContractLimits,
) -> Result<AppArchiveWritePlan, AppPortabilityError> {
    authenticated_scope
        .ensure_live_at(&now)
        .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
    let logical_payload_digest = archive.logical_digest(limits)?;
    let maximum_classification = archive.maximum_classification();
    let has_data = !matches!(archive, AppLogicalArchive::Package { .. });
    let default_encrypt = has_data;

    let protection = match request {
        AppArchiveProtectionRequest::Encrypted => AppArchiveProtectionPlan::Encrypted {
            profile: AppArchiveEncryptionProfile::default(),
        },
        AppArchiveProtectionRequest::Default if default_encrypt => {
            AppArchiveProtectionPlan::Encrypted {
                profile: AppArchiveEncryptionProfile::default(),
            }
        },
        AppArchiveProtectionRequest::Default => AppArchiveProtectionPlan::Plaintext {
            warned_approval_ref: None,
        },
        AppArchiveProtectionRequest::ExplicitPlaintext => {
            if maximum_classification == AppDataClassification::Secret {
                return Err(AppPortabilityError::SecretPlaintextDenied);
            }
            let approval =
                plaintext_approval.ok_or(AppPortabilityError::PlaintextApprovalRequired)?;
            if now < approval.issued_at
                || now >= approval.expires_at
                || approval.scope_binding_ref != *authenticated_scope.scope_binding_ref()
                || approval.actor_ref != *authenticated_scope.actor_ref()
                || approval.session_ref != *authenticated_scope.session_ref()
                || approval.authentication != authenticated_scope.authentication()
                || approval.authentication_revision != authenticated_scope.authentication_revision()
                || approval.logical_payload_digest != logical_payload_digest
            {
                return Err(AppPortabilityError::PlaintextApprovalMismatch);
            }
            AppArchiveProtectionPlan::Plaintext {
                warned_approval_ref: Some(approval.approval_ref.clone()),
            }
        },
    };
    Ok(AppArchiveWritePlan {
        archive_version: APP_PORTABLE_ARCHIVE_VERSION,
        logical_payload_digest,
        maximum_classification,
        protection,
        scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
        actor_ref: authenticated_scope.actor_ref().clone(),
        session_ref: authenticated_scope.session_ref().clone(),
        authentication: authenticated_scope.authentication(),
        authentication_revision: authenticated_scope.authentication_revision(),
        authorized_at: now,
        expires_at: authenticated_scope.expires_at().to_owned(),
    })
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppVerifiedEncryptedArchive {
    logical_payload_digest: AppDigest,
    envelope_header_digest: AppDigest,
    ciphertext_digest: AppDigest,
    ciphertext_bytes: u64,
    profile: AppArchiveEncryptionProfile,
}

impl AppVerifiedEncryptedArchive {
    /// Adapter seam for the existing streaming encryption/key-management
    /// owner after it has authenticated and durably finalized every chunk.
    pub fn from_verified_writer(
        plan: &AppArchiveWritePlan,
        authenticated_scope: &AuthenticatedAppScope,
        now: DateTime<Utc>,
        envelope_header_digest: AppDigest,
        ciphertext_digest: AppDigest,
        ciphertext_bytes: u64,
    ) -> Result<Self, AppPortabilityError> {
        plan.ensure_current_writer_authority(authenticated_scope, &now)?;
        let AppArchiveProtectionPlan::Encrypted { profile } = &plan.protection else {
            return Err(AppPortabilityError::EncryptionEvidenceForPlaintextPlan);
        };
        if ciphertext_bytes == 0 {
            return Err(AppPortabilityError::EmptyArchiveOutput);
        }
        Ok(Self {
            logical_payload_digest: plan.logical_payload_digest.clone(),
            envelope_header_digest,
            ciphertext_digest,
            ciphertext_bytes,
            profile: profile.clone(),
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPublisherSignatureClaim {
    pub publisher_identity: AppReference,
    pub package_id: AppReference,
    pub update_lineage_digest: AppDigest,
    pub signature_chain_ref: AppReference,
}

/// Non-deserializable result of signature-chain verification against the exact
/// candidate bytes. A digest match without this proof never grants publisher
/// or update identity.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppVerifiedPublisherEvidence {
    publisher_identity: AppReference,
    package_id: AppReference,
    package_content_digest: AppDigest,
    update_lineage_digest: AppDigest,
    signature_chain_ref: AppReference,
    trust_revision: AppRevision,
    verified_at: DateTime<Utc>,
}

/// Exact trust-registry revision used for one import decision. It is minted by
/// the registry adapter at the decision boundary and cannot be reconstructed
/// from archive or client data.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentAppPublisherTrust {
    trust_revision: AppRevision,
    resolved_at: DateTime<Utc>,
}

impl CurrentAppPublisherTrust {
    pub fn from_trusted_registry(trust_revision: AppRevision, resolved_at: DateTime<Utc>) -> Self {
        Self {
            trust_revision,
            resolved_at,
        }
    }
}

impl AppVerifiedPublisherEvidence {
    pub fn from_signature_verifier(
        claim: AppPublisherSignatureClaim,
        verified_package_content_digest: AppDigest,
        current_trust: &CurrentAppPublisherTrust,
    ) -> Self {
        Self {
            publisher_identity: claim.publisher_identity,
            package_id: claim.package_id,
            package_content_digest: verified_package_content_digest,
            update_lineage_digest: claim.update_lineage_digest,
            signature_chain_ref: claim.signature_chain_ref,
            trust_revision: current_trust.trust_revision,
            verified_at: current_trust.resolved_at.to_owned(),
        }
    }
}

/// New local identity allocated outside the request payload. Unsigned imports
/// and explicit forks cannot choose an existing publisher/package namespace.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppLocalForkIdentity {
    publisher_identity: AppReference,
    package_id: AppReference,
    origin_package_content_digest: AppDigest,
    allocation_ref: AppReference,
}

impl AppLocalForkIdentity {
    pub fn from_trusted_allocator(
        publisher_identity: AppReference,
        package_id: AppReference,
        origin_package_content_digest: AppDigest,
        allocation_ref: AppReference,
    ) -> Self {
        Self {
            publisher_identity,
            package_id,
            origin_package_content_digest,
            allocation_ref,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppKnownPackageIdentity {
    pub publisher_identity: AppReference,
    pub package_id: AppReference,
    pub update_lineage_digest: AppDigest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPackageCollisionDisposition {
    Reject,
    ExplicitFork,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(tag = "identity", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppResolvedImportIdentity {
    VerifiedPublisherLineage {
        publisher_identity: AppReference,
        package_id: AppReference,
        update_lineage_digest: AppDigest,
        signature_chain_ref: AppReference,
        trust_revision: AppRevision,
        verified_at: DateTime<Utc>,
    },
    LocalFork {
        publisher_identity: AppReference,
        package_id: AppReference,
        origin_package_content_digest: AppDigest,
        allocation_ref: AppReference,
    },
}

/// Requirements shared by every imported package, regardless of digest match
/// or portable evidence. No foreign grant, conformance result or executable
/// trust is accepted as local authority.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPackageImportRequirements {
    pub reverify_complete_bundle_digest: bool,
    pub run_local_conformance: bool,
    pub run_local_permission_review: bool,
    pub rebuild_verify_and_sandbox_executable_content_if_present: bool,
    pub foreign_grants_transfer: bool,
    pub portable_evidence_is_advisory_only: bool,
}

impl AppPackageImportRequirements {
    pub const fn required_for_every_import() -> Self {
        Self {
            reverify_complete_bundle_digest: true,
            run_local_conformance: true,
            run_local_permission_review: true,
            rebuild_verify_and_sandbox_executable_content_if_present: true,
            foreign_grants_transfer: false,
            portable_evidence_is_advisory_only: true,
        }
    }
}

pub fn resolve_package_import_identity(
    package: &AppPackageArchiveManifest,
    verified_publisher: Option<&AppVerifiedPublisherEvidence>,
    current_publisher_trust: Option<&CurrentAppPublisherTrust>,
    known_identity: Option<&AppKnownPackageIdentity>,
    collision_disposition: AppPackageCollisionDisposition,
    local_fork: Option<AppLocalForkIdentity>,
    resolved_at: DateTime<Utc>,
    limits: &AppContractLimits,
) -> Result<AppResolvedImportIdentity, AppPortabilityError> {
    package
        .validate_app_contract(limits)
        .map_err(AppPortabilityError::InvalidContract)?;
    if let Some(evidence) = verified_publisher {
        let current_trust =
            current_publisher_trust.ok_or(AppPortabilityError::PublisherEvidenceNotCurrent)?;
        if current_trust.resolved_at != resolved_at
            || evidence.trust_revision != current_trust.trust_revision
            || evidence.verified_at != current_trust.resolved_at
        {
            return Err(AppPortabilityError::PublisherEvidenceNotCurrent);
        }
        if evidence.package_content_digest != package.package_content_digest
            || evidence.package_id != package.package_id
            || evidence.publisher_identity != package.publisher_identity
        {
            return Err(AppPortabilityError::PublisherEvidenceMismatch);
        }
        let collision = known_identity.is_some_and(|known| {
            known.package_id != evidence.package_id
                || known.publisher_identity != evidence.publisher_identity
                || known.update_lineage_digest != evidence.update_lineage_digest
        });
        if !collision {
            return Ok(AppResolvedImportIdentity::VerifiedPublisherLineage {
                publisher_identity: evidence.publisher_identity.clone(),
                package_id: evidence.package_id.clone(),
                update_lineage_digest: evidence.update_lineage_digest.clone(),
                signature_chain_ref: evidence.signature_chain_ref.clone(),
                trust_revision: evidence.trust_revision,
                verified_at: evidence.verified_at.to_owned(),
            });
        }
        if collision_disposition == AppPackageCollisionDisposition::Reject {
            return Err(AppPortabilityError::PackageIdentityCollision);
        }
    }

    if collision_disposition == AppPackageCollisionDisposition::Reject && known_identity.is_some() {
        return Err(AppPortabilityError::PackageIdentityCollision);
    }
    let local_fork = local_fork.ok_or(AppPortabilityError::LocalForkIdentityRequired)?;
    if local_fork.origin_package_content_digest != package.package_content_digest {
        return Err(AppPortabilityError::LocalForkDigestMismatch);
    }
    if local_fork.package_id == package.package_id
        || local_fork.publisher_identity == package.publisher_identity
    {
        return Err(AppPortabilityError::LocalForkIdentityCollision);
    }
    if known_identity.is_some_and(|known| {
        known.package_id == local_fork.package_id
            || known.publisher_identity == local_fork.publisher_identity
    }) {
        return Err(AppPortabilityError::LocalForkIdentityCollision);
    }
    Ok(AppResolvedImportIdentity::LocalFork {
        publisher_identity: local_fork.publisher_identity,
        package_id: local_fork.package_id,
        origin_package_content_digest: local_fork.origin_package_content_digest,
        allocation_ref: local_fork.allocation_ref,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "compatibility", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppDataImportCompatibility {
    Exact {
        package_content_digest: AppDigest,
        entity_schema_digest: AppDigest,
    },
    ReviewedMigration {
        migration_ref: AppReference,
        source_schema_digest: AppDigest,
        destination_schema_digest: AppDigest,
        migration_digest: AppDigest,
    },
    Incompatible {
        reason_ref: AppReference,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppDataImportRecordDecision {
    Create {
        record_alias: AppPortableAlias,
        new_local_record_id: AppRecordId,
    },
    Merge {
        record_alias: AppPortableAlias,
        existing_local_record_id: AppRecordId,
        expected_revision: AppRevision,
        reviewed_merge_rule_ref: AppReference,
    },
    Conflict {
        record_alias: AppPortableAlias,
        existing_local_record_id: AppRecordId,
    },
    RejectedSensitiveFields {
        record_alias: AppPortableAlias,
        rejected_field_count: u32,
    },
}

impl AppDataImportRecordDecision {
    fn alias(&self) -> AppPortableAlias {
        match self {
            Self::Create { record_alias, .. }
            | Self::Merge { record_alias, .. }
            | Self::Conflict { record_alias, .. }
            | Self::RejectedSensitiveFields { record_alias, .. } => *record_alias,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppMissingImportAttachment {
    pub attachment_alias: AppPortableAlias,
    pub expected_content_digest: AppDigest,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppDataImportPreviewStatus {
    Ready,
    RequiresReview,
    Blocked,
}

/// Current destination identity resolved by the trusted app-store adapter at
/// the preview/replay boundary. Import code receives this proof instead of
/// accepting installation generations and schema/package digests as unrelated
/// scalar arguments.
#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CurrentAppImportDestination {
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    store_revision: AppRevision,
    installation_id: AppInstallationId,
    installation_generation: u64,
    package_content_digest: AppDigest,
    schema_digest: AppDigest,
    resolved_at: DateTime<Utc>,
}

impl CurrentAppImportDestination {
    #[allow(clippy::too_many_arguments)]
    pub fn from_trusted_import_planner(
        authenticated_scope: &AuthenticatedAppScope,
        store_revision: AppRevision,
        installation_id: AppInstallationId,
        installation_generation: u64,
        package_content_digest: AppDigest,
        schema_digest: AppDigest,
        resolved_at: DateTime<Utc>,
    ) -> Result<Self, AppPortabilityError> {
        authenticated_scope
            .ensure_live_at(&resolved_at)
            .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
        if installation_generation == 0 {
            return Err(AppPortabilityError::ImportDestinationNotCurrent);
        }
        Ok(Self {
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            authentication: authenticated_scope.authentication(),
            authentication_revision: authenticated_scope.authentication_revision(),
            store_revision,
            installation_id,
            installation_generation,
            package_content_digest,
            schema_digest,
            resolved_at,
        })
    }

    fn ensure_current(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        now: &DateTime<Utc>,
    ) -> Result<(), AppPortabilityError> {
        authenticated_scope
            .ensure_live_at(now)
            .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
        if &self.resolved_at != now
            || self.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || self.actor_ref != *authenticated_scope.actor_ref()
            || self.session_ref != *authenticated_scope.session_ref()
            || self.authentication != authenticated_scope.authentication()
            || self.authentication_revision != authenticated_scope.authentication_revision()
        {
            return Err(AppPortabilityError::ImportDestinationNotCurrent);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataImportPreview {
    pub preview_version: u8,
    pub scope_binding_ref: AppScopeBindingRef,
    pub import_batch_key: AppDigest,
    pub source_archive_digest: AppDigest,
    pub source_package_content_digest: AppDigest,
    pub source_schema_digest: AppDigest,
    pub source_record_count: u32,
    pub destination_installation_id: AppInstallationId,
    pub destination_installation_generation: u64,
    pub destination_package_content_digest: AppDigest,
    pub destination_schema_digest: AppDigest,
    pub compatibility: AppDataImportCompatibility,
    pub record_decisions: Vec<AppDataImportRecordDecision>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_attachments: Vec<AppMissingImportAttachment>,
    pub status: AppDataImportPreviewStatus,
    pub preview_digest: AppDigest,
}

#[derive(Serialize)]
struct AppDataImportPreviewDigestMaterial<'a> {
    preview_version: u8,
    scope_binding_ref: &'a AppScopeBindingRef,
    import_batch_key: &'a AppDigest,
    source_archive_digest: &'a AppDigest,
    source_package_content_digest: &'a AppDigest,
    source_schema_digest: &'a AppDigest,
    source_record_count: u32,
    destination_installation_id: &'a AppInstallationId,
    destination_installation_generation: u64,
    destination_package_content_digest: &'a AppDigest,
    destination_schema_digest: &'a AppDigest,
    compatibility: &'a AppDataImportCompatibility,
    record_decisions: &'a [AppDataImportRecordDecision],
    missing_attachments: &'a [AppMissingImportAttachment],
    status: AppDataImportPreviewStatus,
}

impl AppDataImportPreview {
    pub fn from_trusted_import_planner(
        import_batch_key: AppDigest,
        source: &AppDataArchiveManifest,
        destination: &CurrentAppImportDestination,
        authenticated_scope: &AuthenticatedAppScope,
        previewed_at: DateTime<Utc>,
        compatibility: AppDataImportCompatibility,
        record_decisions: Vec<AppDataImportRecordDecision>,
        missing_attachments: Vec<AppMissingImportAttachment>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppPortabilityError> {
        destination.ensure_current(authenticated_scope, &previewed_at)?;
        validate_bounded(
            "import_preview.record_decisions",
            record_decisions.len(),
            APP_MAX_DATA_ARCHIVE_RECORDS,
        )
        .map_err(AppPortabilityError::InvalidContract)?;
        validate_bounded(
            "import_preview.missing_attachments",
            missing_attachments.len(),
            limits.max_collection_items(),
        )
        .map_err(AppPortabilityError::InvalidContract)?;
        source
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        let source_record_aliases = source
            .records
            .iter()
            .map(|record| record.record_alias)
            .collect::<HashSet<_>>();
        let decision_aliases = record_decisions
            .iter()
            .map(AppDataImportRecordDecision::alias)
            .collect::<HashSet<_>>();
        if record_decisions.len() != source.records.len()
            || decision_aliases != source_record_aliases
        {
            return Err(AppPortabilityError::IncompleteImportPreview);
        }
        let source_attachment_aliases = source
            .attachments
            .iter()
            .map(|attachment| attachment.attachment_alias)
            .collect::<HashSet<_>>();
        let missing_aliases = missing_attachments
            .iter()
            .map(|attachment| attachment.attachment_alias)
            .collect::<HashSet<_>>();
        if missing_aliases.len() != missing_attachments.len()
            || !missing_aliases.is_subset(&source_attachment_aliases)
            || missing_attachments.iter().any(|missing| {
                source
                    .attachments
                    .iter()
                    .find(|source| source.attachment_alias == missing.attachment_alias)
                    .map_or(true, |source| {
                        source.content_digest != missing.expected_content_digest
                    })
            })
        {
            return Err(AppPortabilityError::InvalidMissingAttachmentPreview);
        }
        match &compatibility {
            AppDataImportCompatibility::Exact {
                package_content_digest,
                entity_schema_digest,
            } if package_content_digest != &source.package_content_digest
                || package_content_digest != &destination.package_content_digest
                || entity_schema_digest != &source.entity_schema_digest
                || entity_schema_digest != &destination.schema_digest =>
            {
                return Err(AppPortabilityError::CompatibilityMismatch);
            },
            AppDataImportCompatibility::ReviewedMigration {
                source_schema_digest,
                destination_schema_digest: migration_destination,
                ..
            } if source_schema_digest != &source.entity_schema_digest
                || migration_destination != &destination.schema_digest =>
            {
                return Err(AppPortabilityError::CompatibilityMismatch);
            },
            _ => {},
        }
        let mut record_decisions = record_decisions;
        record_decisions.sort_by_key(AppDataImportRecordDecision::alias);
        let mut missing_attachments = missing_attachments;
        missing_attachments.sort_by_key(|attachment| attachment.attachment_alias);
        let requires_review = record_decisions.iter().any(|decision| {
            matches!(
                decision,
                AppDataImportRecordDecision::Merge { .. }
                    | AppDataImportRecordDecision::Conflict { .. }
                    | AppDataImportRecordDecision::RejectedSensitiveFields { .. }
            )
        }) || !missing_attachments.is_empty();
        let status = if matches!(
            &compatibility,
            AppDataImportCompatibility::Incompatible { .. }
        ) {
            AppDataImportPreviewStatus::Blocked
        } else if requires_review {
            AppDataImportPreviewStatus::RequiresReview
        } else {
            AppDataImportPreviewStatus::Ready
        };
        let mut preview = Self {
            preview_version: APP_IMPORT_PREVIEW_VERSION,
            scope_binding_ref: destination.scope_binding_ref.clone(),
            import_batch_key,
            source_archive_digest: source.logical_payload_digest.clone(),
            source_package_content_digest: source.package_content_digest.clone(),
            source_schema_digest: source.entity_schema_digest.clone(),
            source_record_count: u32::try_from(source.records.len())
                .map_err(|_| AppPortabilityError::IncompleteImportPreview)?,
            destination_installation_id: destination.installation_id.clone(),
            destination_installation_generation: destination.installation_generation,
            destination_package_content_digest: destination.package_content_digest.clone(),
            destination_schema_digest: destination.schema_digest.clone(),
            compatibility,
            record_decisions,
            missing_attachments,
            status,
            preview_digest: AppDigest::blake3(b"pending"),
        };
        preview.preview_digest = preview.recompute_digest()?;
        preview
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        Ok(preview)
    }

    fn recompute_digest(&self) -> Result<AppDigest, AppPortabilityError> {
        let value = serde_json::to_value(AppDataImportPreviewDigestMaterial {
            preview_version: self.preview_version,
            scope_binding_ref: &self.scope_binding_ref,
            import_batch_key: &self.import_batch_key,
            source_archive_digest: &self.source_archive_digest,
            source_package_content_digest: &self.source_package_content_digest,
            source_schema_digest: &self.source_schema_digest,
            source_record_count: self.source_record_count,
            destination_installation_id: &self.destination_installation_id,
            destination_installation_generation: self.destination_installation_generation,
            destination_package_content_digest: &self.destination_package_content_digest,
            destination_schema_digest: &self.destination_schema_digest,
            compatibility: &self.compatibility,
            record_decisions: &self.record_decisions,
            missing_attachments: &self.missing_attachments,
            status: self.status,
        })
        .map_err(|error| AppPortabilityError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppPortabilityError::Digest(error.to_string()))
    }
}

impl ValidateAppContract for AppDataImportPreview {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.preview_version != APP_IMPORT_PREVIEW_VERSION {
            return Err(AppContractError::invalid(
                "import_preview.preview_version",
                "unsupported import preview version",
            ));
        }
        if self.destination_installation_generation == 0 {
            return Err(AppContractError::invalid(
                "import_preview.destination_installation_generation",
                "must be greater than zero",
            ));
        }
        validate_nonempty_bounded(
            "import_preview.record_decisions",
            self.record_decisions.len(),
            APP_MAX_DATA_ARCHIVE_RECORDS,
        )?;
        if usize::try_from(self.source_record_count).unwrap_or(usize::MAX)
            != self.record_decisions.len()
        {
            return Err(AppContractError::invalid(
                "import_preview.source_record_count",
                "must equal the number of record decisions",
            ));
        }
        validate_bounded(
            "import_preview.missing_attachments",
            self.missing_attachments.len(),
            limits.max_collection_items(),
        )?;
        let mut aliases = HashSet::with_capacity(self.record_decisions.len());
        let mut local_refs = HashSet::with_capacity(self.record_decisions.len());
        let mut requires_review = !self.missing_attachments.is_empty();
        if !self
            .record_decisions
            .windows(2)
            .all(|decisions| decisions[0].alias() < decisions[1].alias())
            || !self.missing_attachments.windows(2).all(|attachments| {
                attachments[0].attachment_alias < attachments[1].attachment_alias
            })
        {
            return Err(AppContractError::invalid(
                "import_preview",
                "record decisions and missing attachments must be canonically ordered",
            ));
        }
        for decision in &self.record_decisions {
            let alias = decision.alias();
            if alias.kind != AppPortableAliasKind::Record || !aliases.insert(alias) {
                return Err(AppContractError::invalid(
                    "import_preview.record_decisions",
                    "requires one decision per unique record alias",
                ));
            }
            match decision {
                AppDataImportRecordDecision::Create {
                    new_local_record_id,
                    ..
                } => {
                    if !local_refs.insert(new_local_record_id) {
                        return Err(AppContractError::invalid(
                            "import_preview.record_decisions",
                            "destination record IDs must be unique",
                        ));
                    }
                },
                AppDataImportRecordDecision::Merge {
                    existing_local_record_id,
                    ..
                } => {
                    requires_review = true;
                    if !local_refs.insert(existing_local_record_id) {
                        return Err(AppContractError::invalid(
                            "import_preview.record_decisions",
                            "destination record IDs must be unique",
                        ));
                    }
                },
                AppDataImportRecordDecision::Conflict { .. } => requires_review = true,
                AppDataImportRecordDecision::RejectedSensitiveFields {
                    rejected_field_count,
                    ..
                } => {
                    requires_review = true;
                    if *rejected_field_count == 0 {
                        return Err(AppContractError::invalid(
                            "import_preview.record_decisions.rejected_field_count",
                            "must be greater than zero",
                        ));
                    }
                },
            }
        }
        let mut missing_aliases = HashSet::with_capacity(self.missing_attachments.len());
        if self.missing_attachments.iter().any(|attachment| {
            attachment.attachment_alias.kind != AppPortableAliasKind::Attachment
                || !missing_aliases.insert(attachment.attachment_alias)
        }) {
            return Err(AppContractError::invalid(
                "import_preview.missing_attachments",
                "requires unique attachment aliases",
            ));
        }
        let blocked = matches!(
            &self.compatibility,
            AppDataImportCompatibility::Incompatible { .. }
        );
        let expected_status = if blocked {
            AppDataImportPreviewStatus::Blocked
        } else if requires_review {
            AppDataImportPreviewStatus::RequiresReview
        } else {
            AppDataImportPreviewStatus::Ready
        };
        if self.status != expected_status {
            return Err(AppContractError::invalid(
                "import_preview.status",
                "does not match compatibility/conflict/review state",
            ));
        }
        match &self.compatibility {
            AppDataImportCompatibility::Exact {
                package_content_digest,
                entity_schema_digest,
            } if package_content_digest != &self.destination_package_content_digest
                || package_content_digest != &self.source_package_content_digest
                || entity_schema_digest != &self.source_schema_digest
                || entity_schema_digest != &self.destination_schema_digest =>
            {
                return Err(AppContractError::invalid(
                    "import_preview.compatibility",
                    "exact compatibility must bind the destination package and schema digests",
                ));
            },
            AppDataImportCompatibility::ReviewedMigration {
                source_schema_digest,
                destination_schema_digest,
                ..
            } if source_schema_digest != &self.source_schema_digest
                || destination_schema_digest != &self.destination_schema_digest =>
            {
                return Err(AppContractError::invalid(
                    "import_preview.compatibility",
                    "reviewed migration must bind the destination schema digest",
                ));
            },
            _ => {},
        }
        let expected = self
            .recompute_digest()
            .map_err(|error| AppContractError::invalid("preview_digest", error.to_string()))?;
        if self.preview_digest != expected {
            return Err(AppContractError::invalid(
                "preview_digest",
                "does not match the canonical import preview",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppApprovedDataImport {
    approval_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    preview_digest: AppDigest,
    import_batch_key: AppDigest,
    source_archive_digest: AppDigest,
    source_record_count: u32,
    destination_installation_id: AppInstallationId,
    destination_installation_generation: u64,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppApprovedDataImport {
    pub fn from_reviewed_preview(
        authenticated_scope: &AuthenticatedAppScope,
        approval_ref: AppReference,
        preview: &AppDataImportPreview,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppPortabilityError> {
        preview
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        if preview.status == AppDataImportPreviewStatus::Blocked || expires_at <= issued_at {
            return Err(AppPortabilityError::ImportApprovalDenied);
        }
        authenticated_scope
            .ensure_live_at(&issued_at)
            .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
        if preview.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || expires_at > authenticated_scope.expires_at().to_owned()
        {
            return Err(AppPortabilityError::ImportApprovalDenied);
        }
        Ok(Self {
            approval_ref,
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            authentication: authenticated_scope.authentication(),
            authentication_revision: authenticated_scope.authentication_revision(),
            preview_digest: preview.preview_digest.clone(),
            import_batch_key: preview.import_batch_key.clone(),
            source_archive_digest: preview.source_archive_digest.clone(),
            source_record_count: u32::try_from(preview.record_decisions.len())
                .map_err(|_| AppPortabilityError::ImportApprovalDenied)?,
            destination_installation_id: preview.destination_installation_id.clone(),
            destination_installation_generation: preview.destination_installation_generation,
            issued_at,
            expires_at,
        })
    }

    pub fn ensure_matches_preview(
        &self,
        preview: &AppDataImportPreview,
    ) -> Result<(), AppPortabilityError> {
        if self.preview_digest != preview.preview_digest
            || self.import_batch_key != preview.import_batch_key
            || self.source_archive_digest != preview.source_archive_digest
            || self.source_record_count != preview.source_record_count
            || self.destination_installation_id != preview.destination_installation_id
            || self.destination_installation_generation
                != preview.destination_installation_generation
        {
            return Err(AppPortabilityError::ImportApprovalMismatch);
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn build_committed_receipt(
        &self,
        preview: &AppDataImportPreview,
        receipt_ref: AppReference,
        created_count: u32,
        merged_count: u32,
        skipped_count: u32,
        committed_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<AppDataImportReceipt, AppPortabilityError> {
        preview
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        self.ensure_matches_preview(preview)?;
        if committed_at < self.issued_at || committed_at >= self.expires_at {
            return Err(AppPortabilityError::ImportApprovalMismatch);
        }
        let receipt = AppDataImportReceipt {
            receipt_ref,
            approval_ref: self.approval_ref.clone(),
            scope_binding_ref: self.scope_binding_ref.clone(),
            import_batch_key: self.import_batch_key.clone(),
            source_archive_digest: self.source_archive_digest.clone(),
            preview_digest: self.preview_digest.clone(),
            destination_installation_id: self.destination_installation_id.clone(),
            destination_installation_generation: self.destination_installation_generation,
            source_record_count: self.source_record_count,
            created_count,
            merged_count,
            skipped_count,
            committed_at,
        };
        receipt
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        Ok(receipt)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppDataImportReceipt {
    pub receipt_ref: AppReference,
    pub approval_ref: AppReference,
    pub scope_binding_ref: AppScopeBindingRef,
    pub import_batch_key: AppDigest,
    pub source_archive_digest: AppDigest,
    pub preview_digest: AppDigest,
    pub destination_installation_id: AppInstallationId,
    pub destination_installation_generation: u64,
    pub source_record_count: u32,
    pub created_count: u32,
    pub merged_count: u32,
    pub skipped_count: u32,
    pub committed_at: DateTime<Utc>,
}

impl ValidateAppContract for AppDataImportReceipt {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.destination_installation_generation == 0 {
            return Err(AppContractError::invalid(
                "import_receipt.destination_installation_generation",
                "must be greater than zero",
            ));
        }
        let settled = self
            .created_count
            .checked_add(self.merged_count)
            .and_then(|count| count.checked_add(self.skipped_count))
            .ok_or_else(|| {
                AppContractError::invalid("import_receipt", "record counters overflow")
            })?;
        if settled != self.source_record_count {
            return Err(AppContractError::invalid(
                "import_receipt",
                "created, merged and skipped counts must settle every source record",
            ));
        }
        Ok(())
    }
}

/// Store-resolved receipt evidence used for idempotent replay. The durable
/// receipt remains readable, but a transport-deserialized copy cannot suppress
/// an import without this non-deserializable app-store wrapper.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppStoredDataImportReceipt {
    receipt: AppDataImportReceipt,
    store_revision: AppRevision,
    observed_at: DateTime<Utc>,
}

impl AppStoredDataImportReceipt {
    pub fn from_app_store(
        receipt: AppDataImportReceipt,
        store_revision: AppRevision,
        destination: &CurrentAppImportDestination,
        authenticated_scope: &AuthenticatedAppScope,
        observed_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppPortabilityError> {
        receipt
            .validate_app_contract(limits)
            .map_err(AppPortabilityError::InvalidContract)?;
        destination.ensure_current(authenticated_scope, &observed_at)?;
        if store_revision != destination.store_revision
            || observed_at < receipt.committed_at
            || receipt.scope_binding_ref != destination.scope_binding_ref
            || receipt.destination_installation_id != destination.installation_id
            || receipt.destination_installation_generation != destination.installation_generation
        {
            return Err(AppPortabilityError::InvalidStoredImportReceipt);
        }
        Ok(Self {
            receipt,
            store_revision,
            observed_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppImportReplayDecision<'a> {
    New,
    Replay(&'a AppDataImportReceipt),
}

pub fn resolve_import_replay<'a>(
    approval: &AppApprovedDataImport,
    source_archive_digest: &AppDigest,
    destination: &CurrentAppImportDestination,
    existing: Option<&'a AppStoredDataImportReceipt>,
    authenticated_scope: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<AppImportReplayDecision<'a>, AppPortabilityError> {
    authenticated_scope
        .ensure_live_at(&now)
        .map_err(|_| AppPortabilityError::AuthenticationUnavailable)?;
    destination.ensure_current(authenticated_scope, &now)?;
    let approval_use_at = existing
        .map(|receipt| receipt.receipt.committed_at)
        .unwrap_or(now);
    if approval_use_at < approval.issued_at
        || approval_use_at >= approval.expires_at
        || approval.scope_binding_ref != *authenticated_scope.scope_binding_ref()
        || approval.actor_ref != *authenticated_scope.actor_ref()
        || approval.session_ref != *authenticated_scope.session_ref()
        || approval.authentication != authenticated_scope.authentication()
        || approval.authentication_revision != authenticated_scope.authentication_revision()
        || approval.source_archive_digest != *source_archive_digest
        || approval.destination_installation_id != destination.installation_id
        || approval.destination_installation_generation != destination.installation_generation
    {
        return Err(AppPortabilityError::ImportApprovalMismatch);
    }
    let Some(existing_evidence) = existing else {
        return Ok(AppImportReplayDecision::New);
    };
    if existing_evidence.observed_at != now {
        return Err(AppPortabilityError::InvalidStoredImportReceipt);
    }
    if existing_evidence.store_revision != destination.store_revision {
        return Err(AppPortabilityError::InvalidStoredImportReceipt);
    }
    let existing = &existing_evidence.receipt;
    if existing.scope_binding_ref != approval.scope_binding_ref
        || existing.approval_ref != approval.approval_ref
        || existing.import_batch_key != approval.import_batch_key
        || existing.source_archive_digest != *source_archive_digest
        || existing.preview_digest != approval.preview_digest
        || existing.destination_installation_id != approval.destination_installation_id
        || existing.destination_installation_generation
            != approval.destination_installation_generation
        || existing.source_record_count != approval.source_record_count
    {
        return Err(AppPortabilityError::ImportBatchCollision);
    }
    existing
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppPortabilityError::InvalidContract)?;
    Ok(AppImportReplayDecision::Replay(existing))
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppPortabilityError {
    #[error("invalid app portability contract: {0}")]
    InvalidContract(AppContractError),
    #[error("package export is denied from source state {0:?}")]
    PackageExportDenied(AppExportSourceState),
    #[error("data export is denied from source state {0:?}")]
    DataExportDenied(AppExportSourceState),
    #[error("package revision digest does not match staged package bytes")]
    PackageDigestMismatch,
    #[error("dependency lock does not match package revision and staged bytes")]
    DependencyLockMismatch,
    #[error("portable verification evidence contains duplicates")]
    DuplicatePortableEvidence,
    #[error("failed to compute canonical portability digest: {0}")]
    Digest(String),
    #[error("authenticated export/import scope is unavailable")]
    AuthenticationUnavailable,
    #[error("secret-class data cannot be exported as plaintext")]
    SecretPlaintextDenied,
    #[error("explicit plaintext data export requires exact warned approval")]
    PlaintextApprovalRequired,
    #[error("plaintext export approval does not bind this scope/session/payload/time")]
    PlaintextApprovalMismatch,
    #[error("plaintext export approval has an invalid validity window")]
    InvalidPlaintextApprovalWindow,
    #[error("archive write plan no longer matches live authenticated writer authority")]
    ArchiveWriteAuthorityMismatch,
    #[error("encrypted archive evidence was supplied for a plaintext plan")]
    EncryptionEvidenceForPlaintextPlan,
    #[error("archive writer produced no bytes")]
    EmptyArchiveOutput,
    #[error("publisher evidence does not bind the imported package bytes and identity")]
    PublisherEvidenceMismatch,
    #[error("publisher evidence does not use the current trust-registry revision")]
    PublisherEvidenceNotCurrent,
    #[error("package/publisher identity collides with an existing lineage")]
    PackageIdentityCollision,
    #[error("unsigned or explicitly forked import requires a server-allocated local identity")]
    LocalForkIdentityRequired,
    #[error("local fork allocation does not bind the imported package bytes")]
    LocalForkDigestMismatch,
    #[error("local fork allocation collides with an existing identity")]
    LocalForkIdentityCollision,
    #[error("import preview does not contain exactly one decision per source record")]
    IncompleteImportPreview,
    #[error("missing-attachment preview is duplicated or not present in the source archive")]
    InvalidMissingAttachmentPreview,
    #[error("import compatibility does not bind source and destination package/schema digests")]
    CompatibilityMismatch,
    #[error("data import preview cannot be approved")]
    ImportApprovalDenied,
    #[error("data import approval does not bind this scope, destination, archive or time")]
    ImportApprovalMismatch,
    #[error("import destination evidence is not current at the preview/replay boundary")]
    ImportDestinationNotCurrent,
    #[error(
        "import batch key was replayed with different scope/archive/preview/destination identity"
    )]
    ImportBatchCollision,
    #[error("stored import receipt was observed before it committed")]
    InvalidStoredImportReceipt,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use chrono::{Duration, TimeZone};
    use serde_json::json;

    use super::*;
    use crate::magician_v2::apps::records::AppScope;

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("valid reference")
    }

    fn record_id(value: &str) -> AppRecordId {
        AppRecordId::parse(value).expect("valid record id")
    }

    fn installation_id(value: &str) -> AppInstallationId {
        AppInstallationId::parse(value).expect("valid installation id")
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).expect("positive revision")
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 4, 0, 0)
            .single()
            .expect("valid test time")
    }

    fn scope(session: &str) -> AuthenticatedAppScope {
        scope_with_revision(session, 7)
    }

    fn scope_with_revision(session: &str, authentication_revision: u64) -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: reference("principal:owner"),
                workspace: reference("workspace:default"),
            },
            AppScopeBindingRef::parse("scope_owner_default").expect("scope ref"),
            reference("actor:owner"),
            reference(session),
            revision(authentication_revision),
            now() - Duration::minutes(5),
            now() + Duration::minutes(30),
        )
        .expect("authenticated scope")
    }

    fn import_destination(
        auth: &AuthenticatedAppScope,
        source: &AppDataArchiveManifest,
        installation: &str,
        generation: u64,
    ) -> CurrentAppImportDestination {
        CurrentAppImportDestination::from_trusted_import_planner(
            auth,
            revision(3),
            installation_id(installation),
            generation,
            source.package_content_digest.clone(),
            source.entity_schema_digest.clone(),
            now(),
        )
        .expect("current import destination")
    }

    fn alias(kind: AppPortableAliasKind, ordinal: u32) -> AppPortableAlias {
        AppPortableAlias::new(kind, ordinal).expect("portable alias")
    }

    fn record(ordinal: u32, classification: AppDataClassification) -> AppPortableRecordRevision {
        let payload = json!({"title": format!("record-{ordinal}")});
        AppPortableRecordRevision {
            record_alias: alias(AppPortableAliasKind::Record, ordinal),
            entity_name: AppName::parse("items").expect("entity"),
            record_revision: revision(ordinal.into()),
            schema_revision: revision(4),
            payload_digest: AppDigest::blake3_canonical_json(&payload).expect("payload digest"),
            payload,
            classification,
            created_at: now() - Duration::days(1),
            updated_at: now(),
            deleted_at: None,
            provenance: AppPortableProvenance {
                actor_kind: AppRecordActorKind::User,
                actor_alias: Some(alias(AppPortableAliasKind::Actor, 1)),
                execution_alias: None,
                source_aliases: Vec::new(),
            },
        }
    }

    fn data_archive_with_records(
        records: Vec<AppPortableRecordRevision>,
    ) -> AppDataArchiveManifest {
        AppDataArchiveManifest::from_trusted_export_projection(
            reference("package:journal"),
            digest("package"),
            digest("schema"),
            AppExportSourceState::UninstalledRetained,
            records,
            Vec::new(),
            &AppContractLimits::default(),
        )
        .expect("valid data archive")
    }

    #[test]
    fn data_archive_contract_accepts_the_reference_installation_scale() {
        let records = (1
            ..=magician_apps::apps::benchmark_fixtures::APP_BENCHMARK_ACTIVE_INSTALLATION_RECORDS)
            .map(|ordinal| record(ordinal, AppDataClassification::Personal))
            .collect::<Vec<_>>();

        let archive = AppDataArchiveManifest::from_trusted_export_projection(
            reference("package:journal"),
            digest("package"),
            digest("schema"),
            AppExportSourceState::UninstalledRetained,
            records,
            Vec::new(),
            &AppContractLimits::default(),
        );

        assert!(
            archive.is_ok(),
            "the data archive must carry the checked-in 10,000-record reference installation"
        );
    }

    fn package_archive() -> AppPackageArchiveManifest {
        let manifest_digest = digest("manifest");
        let package_content_digest = digest("package");
        let dependency_lock = super::super::package_lock::portable_test_lock(
            manifest_digest.clone(),
            package_content_digest.clone(),
        );
        let mut package = AppPackageArchiveManifest {
            archive_version: APP_PORTABLE_ARCHIVE_VERSION,
            package_revision_ref: reference("package-revision:1"),
            package_id: reference("package:journal"),
            publisher_identity: reference("publisher:example"),
            semantic_version: "1.0.0".to_owned(),
            package_content_digest,
            manifest_digest,
            dependency_lock_digest: dependency_lock.lock_digest().clone(),
            dependency_lock: AppPortablePackageLockClaim::from_trusted(&dependency_lock),
            members: vec![AppPortablePackageMember {
                path: AppBundlePath::parse("SKILL.md").expect("bundle path"),
                content_digest: digest("member"),
                byte_len: 128,
            }],
            advisory_verification_evidence: Vec::new(),
            logical_payload_digest: digest("pending"),
        };
        package.logical_payload_digest = package.recompute_digest().expect("package digest");
        package
            .validate_app_contract(&AppContractLimits::default())
            .expect("valid package archive");
        package
    }

    #[test]
    fn package_and_data_payloads_have_disjoint_authority_shapes() {
        let package = serde_json::to_value(package_archive()).expect("package JSON");
        let data = serde_json::to_value(data_archive_with_records(vec![record(
            1,
            AppDataClassification::Personal,
        )]))
        .expect("data JSON");
        for forbidden in [
            "grant_revision",
            "scope_binding_ref",
            "installation_id",
            "credential",
            "schedule",
            "memory",
            "provider_session",
        ] {
            assert!(!json_has_key(&package, forbidden));
            assert!(!json_has_key(&data, forbidden));
        }
    }

    #[test]
    fn data_export_is_deterministic_independent_of_store_iteration_order() {
        let first = data_archive_with_records(vec![
            record(2, AppDataClassification::Personal),
            record(1, AppDataClassification::Public),
        ]);
        let second = data_archive_with_records(vec![
            record(1, AppDataClassification::Public),
            record(2, AppDataClassification::Personal),
        ]);
        assert_eq!(first.logical_payload_digest, second.logical_payload_digest);
        assert_eq!(first.records[0].record_alias.ordinal.get(), 1);
        assert_eq!(
            first.maximum_classification,
            AppDataClassification::Personal
        );
    }

    #[test]
    fn package_only_defaults_plaintext_but_data_and_combined_default_encrypted() {
        let auth = scope("session:one");
        let package = package_archive();
        let data = data_archive_with_records(vec![record(1, AppDataClassification::Personal)]);
        let package_plan = authorize_archive_write(
            &AppLogicalArchive::Package {
                package: package.clone(),
            },
            AppArchiveProtectionRequest::Default,
            None,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("package plan");
        assert!(matches!(
            package_plan.protection(),
            AppArchiveProtectionPlan::Plaintext {
                warned_approval_ref: None
            }
        ));
        let explicitly_encrypted_package = authorize_archive_write(
            &AppLogicalArchive::Package {
                package: package.clone(),
            },
            AppArchiveProtectionRequest::Encrypted,
            None,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("explicitly encrypted package plan");
        assert!(matches!(
            explicitly_encrypted_package.protection(),
            AppArchiveProtectionPlan::Encrypted { .. }
        ));

        for archive in [
            AppLogicalArchive::Data { data: data.clone() },
            AppLogicalArchive::Combined {
                package: package.clone(),
                data: data.clone(),
            },
        ] {
            let plan = authorize_archive_write(
                &archive,
                AppArchiveProtectionRequest::Default,
                None,
                &auth,
                now(),
                &AppContractLimits::default(),
            )
            .expect("encrypted plan");
            assert!(matches!(
                plan.protection(),
                AppArchiveProtectionPlan::Encrypted { .. }
            ));
        }
    }

    #[test]
    fn plaintext_data_requires_exact_warned_approval_and_secret_is_never_eligible() {
        let auth = scope("session:one");
        let ordinary = AppLogicalArchive::Data {
            data: data_archive_with_records(vec![record(1, AppDataClassification::Personal)]),
        };
        assert_eq!(
            authorize_archive_write(
                &ordinary,
                AppArchiveProtectionRequest::ExplicitPlaintext,
                None,
                &auth,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::PlaintextApprovalRequired)
        );
        let approval = AppPlaintextExportApproval::from_warned_user_action(
            &auth,
            reference("approval:plain"),
            ordinary
                .logical_digest(&AppContractLimits::default())
                .expect("logical digest"),
            now() - Duration::seconds(1),
            now() + Duration::minutes(2),
        )
        .expect("plaintext approval");
        let plan = authorize_archive_write(
            &ordinary,
            AppArchiveProtectionRequest::ExplicitPlaintext,
            Some(&approval),
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("approved plaintext");
        assert!(matches!(
            plan.protection(),
            AppArchiveProtectionPlan::Plaintext {
                warned_approval_ref: Some(_)
            }
        ));
        assert_eq!(
            authorize_archive_write(
                &ordinary,
                AppArchiveProtectionRequest::ExplicitPlaintext,
                Some(&approval),
                &scope_with_revision("session:one", 8),
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::PlaintextApprovalMismatch)
        );

        let secret = AppLogicalArchive::Data {
            data: data_archive_with_records(vec![record(1, AppDataClassification::Secret)]),
        };
        assert_eq!(
            authorize_archive_write(
                &secret,
                AppArchiveProtectionRequest::ExplicitPlaintext,
                None,
                &auth,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::SecretPlaintextDenied)
        );
    }

    #[test]
    fn archive_writer_rechecks_the_exact_authenticated_session() {
        let auth = scope("session:one");
        let archive = AppLogicalArchive::Data {
            data: data_archive_with_records(vec![record(1, AppDataClassification::Personal)]),
        };
        let plan = authorize_archive_write(
            &archive,
            AppArchiveProtectionRequest::Encrypted,
            None,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("encrypted write plan");
        assert_eq!(
            AppVerifiedEncryptedArchive::from_verified_writer(
                &plan,
                &scope("session:other"),
                now(),
                digest("header"),
                digest("ciphertext"),
                128,
            ),
            Err(AppPortabilityError::ArchiveWriteAuthorityMismatch)
        );
    }

    #[test]
    fn trusted_export_import_evidence_is_not_deserializable() {
        static_assertions::assert_not_impl_any!(
            AppPlaintextExportApproval: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppArchiveWritePlan: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppVerifiedPublisherEvidence: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppLocalForkIdentity: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppApprovedDataImport: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppDataImportPreview: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            AppStoredDataImportReceipt: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(
            CurrentAppPublisherTrust: serde::de::DeserializeOwned, Clone
        );
        static_assertions::assert_not_impl_any!(
            CurrentAppImportDestination: serde::de::DeserializeOwned, Clone
        );
    }

    #[test]
    fn unsigned_import_gets_new_local_identity_while_verified_signature_keeps_lineage() {
        let package = package_archive();
        let fork = AppLocalForkIdentity::from_trusted_allocator(
            reference("publisher:local-owner"),
            reference("package:local-fork"),
            package.package_content_digest.clone(),
            reference("allocation:1"),
        );
        let unsigned = resolve_package_import_identity(
            &package,
            None,
            None,
            None,
            AppPackageCollisionDisposition::ExplicitFork,
            Some(fork),
            now(),
            &AppContractLimits::default(),
        )
        .expect("unsigned local fork");
        assert!(matches!(
            unsigned,
            AppResolvedImportIdentity::LocalFork { .. }
        ));

        let claim = AppPublisherSignatureClaim {
            publisher_identity: package.publisher_identity.clone(),
            package_id: package.package_id.clone(),
            update_lineage_digest: digest("lineage"),
            signature_chain_ref: reference("signature-chain:1"),
        };
        let current_trust = CurrentAppPublisherTrust::from_trusted_registry(revision(9), now());
        let evidence = AppVerifiedPublisherEvidence::from_signature_verifier(
            claim,
            package.package_content_digest.clone(),
            &current_trust,
        );
        let signed = resolve_package_import_identity(
            &package,
            Some(&evidence),
            Some(&current_trust),
            None,
            AppPackageCollisionDisposition::Reject,
            None,
            now(),
            &AppContractLimits::default(),
        )
        .expect("verified publisher lineage");
        assert!(matches!(
            signed,
            AppResolvedImportIdentity::VerifiedPublisherLineage { .. }
        ));
    }

    #[test]
    fn digest_identity_never_substitutes_for_publisher_identity() {
        let package = package_archive();
        assert_eq!(
            resolve_package_import_identity(
                &package,
                None,
                None,
                None,
                AppPackageCollisionDisposition::Reject,
                None,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::LocalForkIdentityRequired)
        );
        let requirements = AppPackageImportRequirements::required_for_every_import();
        assert!(requirements.run_local_conformance);
        assert!(requirements.run_local_permission_review);
        assert!(requirements.rebuild_verify_and_sandbox_executable_content_if_present);
        assert!(!requirements.foreign_grants_transfer);
        assert!(requirements.portable_evidence_is_advisory_only);
    }

    #[test]
    fn publisher_signature_evidence_must_match_the_current_trust_snapshot() {
        let package = package_archive();
        let verified_trust = CurrentAppPublisherTrust::from_trusted_registry(revision(9), now());
        let evidence = AppVerifiedPublisherEvidence::from_signature_verifier(
            AppPublisherSignatureClaim {
                publisher_identity: package.publisher_identity.clone(),
                package_id: package.package_id.clone(),
                update_lineage_digest: digest("lineage"),
                signature_chain_ref: reference("signature-chain:1"),
            },
            package.package_content_digest.clone(),
            &verified_trust,
        );
        let current_trust = CurrentAppPublisherTrust::from_trusted_registry(
            revision(10),
            now() + Duration::seconds(1),
        );
        assert_eq!(
            resolve_package_import_identity(
                &package,
                Some(&evidence),
                Some(&current_trust),
                None,
                AppPackageCollisionDisposition::Reject,
                None,
                now() + Duration::seconds(1),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::PublisherEvidenceNotCurrent)
        );
    }

    #[test]
    fn import_preview_is_complete_schema_bound_and_idempotent() {
        let source = data_archive_with_records(vec![record(1, AppDataClassification::Personal)]);
        let auth = scope("session:one");
        let destination = import_destination(&auth, &source, "installation-destination", 8);
        let decision = AppDataImportRecordDecision::Create {
            record_alias: alias(AppPortableAliasKind::Record, 1),
            new_local_record_id: record_id("local-record-1"),
        };
        let preview = AppDataImportPreview::from_trusted_import_planner(
            digest("batch"),
            &source,
            &destination,
            &auth,
            now(),
            AppDataImportCompatibility::Exact {
                package_content_digest: source.package_content_digest.clone(),
                entity_schema_digest: source.entity_schema_digest.clone(),
            },
            vec![decision],
            Vec::new(),
            &AppContractLimits::default(),
        )
        .expect("complete preview");
        assert_eq!(preview.status, AppDataImportPreviewStatus::Ready);

        let approval = AppApprovedDataImport::from_reviewed_preview(
            &auth,
            reference("approval:import"),
            &preview,
            now() - Duration::seconds(1),
            now() + Duration::minutes(2),
            &AppContractLimits::default(),
        )
        .expect("import approval");
        let receipt = AppDataImportReceipt {
            receipt_ref: reference("receipt:import"),
            approval_ref: reference("approval:import"),
            scope_binding_ref: auth.scope_binding_ref().clone(),
            import_batch_key: preview.import_batch_key.clone(),
            source_archive_digest: preview.source_archive_digest.clone(),
            preview_digest: preview.preview_digest.clone(),
            destination_installation_id: preview.destination_installation_id.clone(),
            destination_installation_generation: 8,
            source_record_count: 1,
            created_count: 1,
            merged_count: 0,
            skipped_count: 0,
            committed_at: now(),
        };
        let stored = AppStoredDataImportReceipt::from_app_store(
            receipt.clone(),
            revision(3),
            &destination,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("stored receipt");
        assert!(matches!(
            resolve_import_replay(
                &approval,
                &preview.source_archive_digest,
                &destination,
                Some(&stored),
                &auth,
                now(),
            )
            .expect("exact replay"),
            AppImportReplayDecision::Replay(_)
        ));
        assert_eq!(
            resolve_import_replay(
                &approval,
                &digest("different-archive"),
                &destination,
                Some(&stored),
                &auth,
                now(),
            ),
            Err(AppPortabilityError::ImportApprovalMismatch)
        );
        let other_destination = import_destination(&auth, &source, "installation-other", 8);
        assert_eq!(
            resolve_import_replay(
                &approval,
                &preview.source_archive_digest,
                &other_destination,
                Some(&stored),
                &auth,
                now(),
            ),
            Err(AppPortabilityError::ImportApprovalMismatch)
        );
        let next_destination = import_destination(&auth, &source, "installation-destination", 9);
        assert_eq!(
            resolve_import_replay(
                &approval,
                &preview.source_archive_digest,
                &next_destination,
                Some(&stored),
                &auth,
                now(),
            ),
            Err(AppPortabilityError::ImportApprovalMismatch)
        );
        let mut wrong_installation = receipt.clone();
        wrong_installation.destination_installation_id = installation_id("installation-other");
        let wrong_installation = AppStoredDataImportReceipt::from_app_store(
            wrong_installation,
            revision(3),
            &other_destination,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("stored cross-installation collision receipt");
        assert_eq!(
            resolve_import_replay(
                &approval,
                &preview.source_archive_digest,
                &destination,
                Some(&wrong_installation),
                &auth,
                now(),
            ),
            Err(AppPortabilityError::ImportBatchCollision)
        );
        let mut collided = receipt;
        collided.preview_digest = digest("another-preview");
        let collided = AppStoredDataImportReceipt::from_app_store(
            collided,
            revision(3),
            &destination,
            &auth,
            now(),
            &AppContractLimits::default(),
        )
        .expect("stored collision receipt");
        assert_eq!(
            resolve_import_replay(
                &approval,
                &preview.source_archive_digest,
                &destination,
                Some(&collided),
                &auth,
                now(),
            ),
            Err(AppPortabilityError::ImportBatchCollision)
        );
    }

    #[test]
    fn incomplete_preview_and_archive_tamper_fail_closed() {
        let source = data_archive_with_records(vec![record(1, AppDataClassification::Personal)]);
        let auth = scope("session:one");
        let destination = import_destination(&auth, &source, "installation-destination", 8);
        assert_eq!(
            AppDataImportPreview::from_trusted_import_planner(
                digest("batch"),
                &source,
                &destination,
                &auth,
                now(),
                AppDataImportCompatibility::Exact {
                    package_content_digest: source.package_content_digest.clone(),
                    entity_schema_digest: source.entity_schema_digest.clone(),
                },
                Vec::new(),
                Vec::new(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::IncompleteImportPreview)
        );

        let mut tampered = source;
        tampered.records[0].payload = json!({"title": "tampered"});
        assert!(tampered
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        assert_eq!(
            AppDataImportPreview::from_trusted_import_planner(
                digest("stale-destination"),
                &tampered,
                &destination,
                &auth,
                now() + Duration::seconds(1),
                AppDataImportCompatibility::Exact {
                    package_content_digest: tampered.package_content_digest.clone(),
                    entity_schema_digest: tampered.entity_schema_digest.clone(),
                },
                vec![AppDataImportRecordDecision::Create {
                    record_alias: alias(AppPortableAliasKind::Record, 1),
                    new_local_record_id: record_id("stale-destination-record"),
                }],
                Vec::new(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::ImportDestinationNotCurrent)
        );
    }

    #[test]
    fn aggregate_record_payload_ceiling_precedes_archive_digest_materialization() {
        let records = (1..=256)
            .map(|ordinal| {
                let mut record = record(ordinal, AppDataClassification::Personal);
                record.payload = json!({"body": "x".repeat(70_000)});
                record.payload_digest =
                    AppDigest::blake3_canonical_json(&record.payload).expect("payload digest");
                record
            })
            .collect();
        assert!(matches!(
            AppDataArchiveManifest::from_trusted_export_projection(
                reference("package:journal"),
                digest("package"),
                digest("schema"),
                AppExportSourceState::UninstalledRetained,
                records,
                Vec::new(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::InvalidContract(_))
        ));
    }

    #[test]
    fn portable_provenance_cannot_reference_an_undeclared_archive_member() {
        let mut source = record(1, AppDataClassification::Personal);
        source.provenance.source_aliases = vec![alias(AppPortableAliasKind::Record, 2)];
        assert!(matches!(
            AppDataArchiveManifest::from_trusted_export_projection(
                reference("package:journal"),
                digest("package"),
                digest("schema"),
                AppExportSourceState::Enabled,
                vec![source],
                Vec::new(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::InvalidContract(_))
        ));
    }

    #[test]
    fn trusted_export_preflight_rejects_and_iteratively_discards_deep_payloads() {
        let mut payload = Value::Null;
        for _ in 0..10_000 {
            payload = Value::Array(vec![payload]);
        }
        let mut deep_record = record(1, AppDataClassification::Personal);
        deep_record.payload = payload;
        deep_record.payload_digest = digest("untrusted-deep-payload");

        assert!(matches!(
            AppDataArchiveManifest::from_trusted_export_projection(
                reference("package:test"),
                digest("package-content"),
                digest("schema"),
                AppExportSourceState::Enabled,
                vec![deep_record],
                Vec::new(),
                &AppContractLimits::default(),
            ),
            Err(AppPortabilityError::InvalidContract(_))
        ));
    }

    #[test]
    fn logical_archive_digest_refuses_an_unvalidated_manifest() {
        let mut data = data_archive_with_records(vec![record(1, AppDataClassification::Personal)]);
        data.logical_payload_digest = digest("tampered-manifest");
        let archive = AppLogicalArchive::Data { data };

        assert!(matches!(
            archive.logical_digest(&AppContractLimits::default()),
            Err(AppPortabilityError::InvalidContract(_))
        ));
    }

    fn json_has_key(value: &Value, key: &str) -> bool {
        match value {
            Value::Object(object) => {
                object.contains_key(key) || object.values().any(|value| json_has_key(value, key))
            },
            Value::Array(values) => values.iter().any(|value| json_has_key(value, key)),
            _ => false,
        }
    }
}

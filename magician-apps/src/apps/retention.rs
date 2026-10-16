//! Exact app-data retention, encryption-at-rest and purge contracts.
//!
//! Storage Governance remains the cross-store inventory/retention executor and
//! the app store remains the source of record truth. This module defines the
//! bounded policy, inventory, approval and receipt protocol; the Phase-2D
//! entity-retention adapter consumes it for app-record history and SQLite WAL.
//! The contract layer itself never deletes files, checkpoints SQLite, destroys
//! keys, or claims deletion from a provider.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::{AppScopeAuthentication, AuthenticatedAppScope},
    lifecycle::{AppInstallationLifecycle, AppInstallationStatus},
    models::{
        validate_nonempty_bounded, AppContractError, AppContractLimits, AppDataClassification,
        AppDigest, AppInstallationId, AppName, AppRecordId, AppReference, AppRevision,
        AppScopeBindingRef, ValidateAppContract,
    },
    records::AppInstallation,
};

pub const APP_RETENTION_POLICY_VERSION: u8 = 2;
pub const APP_PURGE_PROTOCOL_VERSION: u8 = 2;

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAtRestCipher {
    /// Legacy contract-only value. It remains decodable so an old policy is
    /// reported as incompatible instead of becoming corrupt input.
    Xchacha20Poly1305,
    Sqlcipher4Aes256CbcHmacSha512,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppAtRestKeyOwnership {
    ScopeBoundPlatformKey,
}

/// Policy metadata only. Key bytes and provider handles never enter an app
/// record or archive contract.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppAtRestEncryptionContract {
    pub cipher: AppAtRestCipher,
    pub key_ownership: AppAtRestKeyOwnership,
    pub key_revision: AppRevision,
    pub encryption_context_digest: AppDigest,
    pub required_from: AppDataClassification,
}

impl ValidateAppContract for AppAtRestEncryptionContract {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.cipher != AppAtRestCipher::Sqlcipher4Aes256CbcHmacSha512 {
            return Err(AppContractError::invalid(
                "at_rest_encryption.cipher",
                "must match the production SQLCipher 4 registry owner",
            ));
        }
        if self.required_from != AppDataClassification::Personal {
            return Err(AppContractError::invalid(
                "at_rest_encryption.required_from",
                "personal, sensitive and secret app data must be encrypted at rest",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRetentionCeiling {
    pub max_age_seconds: u64,
    pub max_bytes: u64,
}

impl AppRetentionCeiling {
    fn validate(&self, field: &'static str) -> Result<(), AppContractError> {
        if self.max_age_seconds == 0 || self.max_bytes == 0 {
            return Err(AppContractError::invalid(
                field,
                "requires explicit positive time and byte ceilings",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRecordRevisionRetention {
    pub max_revisions_per_record: u32,
    pub ceiling: AppRetentionCeiling,
}

/// Explicit byte/time bounds for append-only and auxiliary stores. A record
/// count alone can never stand in for these ceilings.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRetentionPolicy {
    pub policy_version: u8,
    pub policy_revision: AppRevision,
    pub policy_digest: AppDigest,
    pub record_revisions: AppRecordRevisionRetention,
    pub database_wal_and_temp: AppRetentionCeiling,
    pub backups_and_exports: AppRetentionCeiling,
    pub attachments: AppRetentionCeiling,
    pub analytics_and_captures: AppRetentionCeiling,
    pub evaluation_artifacts: AppRetentionCeiling,
    pub audit_tombstones: AppRetentionCeiling,
    pub at_rest_encryption: AppAtRestEncryptionContract,
}

#[derive(Serialize)]
struct AppRetentionPolicyDigestMaterial<'a> {
    policy_version: u8,
    policy_revision: AppRevision,
    record_revisions: &'a AppRecordRevisionRetention,
    database_wal_and_temp: &'a AppRetentionCeiling,
    backups_and_exports: &'a AppRetentionCeiling,
    attachments: &'a AppRetentionCeiling,
    analytics_and_captures: &'a AppRetentionCeiling,
    evaluation_artifacts: &'a AppRetentionCeiling,
    audit_tombstones: &'a AppRetentionCeiling,
    at_rest_encryption: &'a AppAtRestEncryptionContract,
}

impl AppRetentionPolicy {
    // Phase 6's policy owner is the first production constructor.
    #[cfg_attr(not(test), allow(dead_code))]
    #[allow(clippy::too_many_arguments)]
    pub fn from_policy_parts(
        policy_revision: AppRevision,
        record_revisions: AppRecordRevisionRetention,
        database_wal_and_temp: AppRetentionCeiling,
        backups_and_exports: AppRetentionCeiling,
        attachments: AppRetentionCeiling,
        analytics_and_captures: AppRetentionCeiling,
        evaluation_artifacts: AppRetentionCeiling,
        audit_tombstones: AppRetentionCeiling,
        at_rest_encryption: AppAtRestEncryptionContract,
        limits: &AppContractLimits,
    ) -> Result<Self, AppRetentionError> {
        let mut policy = Self {
            policy_version: APP_RETENTION_POLICY_VERSION,
            policy_revision,
            policy_digest: AppDigest::blake3(b"pending"),
            record_revisions,
            database_wal_and_temp,
            backups_and_exports,
            attachments,
            analytics_and_captures,
            evaluation_artifacts,
            audit_tombstones,
            at_rest_encryption,
        };
        policy.policy_digest = policy.recompute_digest()?;
        policy
            .validate_app_contract(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        Ok(policy)
    }

    fn recompute_digest(&self) -> Result<AppDigest, AppRetentionError> {
        let value = serde_json::to_value(AppRetentionPolicyDigestMaterial {
            policy_version: self.policy_version,
            policy_revision: self.policy_revision,
            record_revisions: &self.record_revisions,
            database_wal_and_temp: &self.database_wal_and_temp,
            backups_and_exports: &self.backups_and_exports,
            attachments: &self.attachments,
            analytics_and_captures: &self.analytics_and_captures,
            evaluation_artifacts: &self.evaluation_artifacts,
            audit_tombstones: &self.audit_tombstones,
            at_rest_encryption: &self.at_rest_encryption,
        })
        .map_err(|error| AppRetentionError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppRetentionError::Digest(error.to_string()))
    }
}

impl ValidateAppContract for AppRetentionPolicy {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.policy_version != APP_RETENTION_POLICY_VERSION {
            return Err(AppContractError::invalid(
                "retention.policy_version",
                "unsupported retention policy version",
            ));
        }
        if self.record_revisions.max_revisions_per_record == 0 {
            return Err(AppContractError::invalid(
                "retention.record_revisions.max_revisions_per_record",
                "must be greater than zero",
            ));
        }
        self.record_revisions
            .ceiling
            .validate("retention.record_revisions.ceiling")?;
        self.database_wal_and_temp
            .validate("retention.database_wal_and_temp")?;
        self.backups_and_exports
            .validate("retention.backups_and_exports")?;
        self.attachments.validate("retention.attachments")?;
        self.analytics_and_captures
            .validate("retention.analytics_and_captures")?;
        self.evaluation_artifacts
            .validate("retention.evaluation_artifacts")?;
        self.audit_tombstones
            .validate("retention.audit_tombstones")?;
        self.at_rest_encryption.validate_app_contract(limits)?;
        let expected = self.recompute_digest().map_err(|error| {
            AppContractError::invalid("retention.policy_digest", error.to_string())
        })?;
        if self.policy_digest != expected {
            return Err(AppContractError::invalid(
                "retention.policy_digest",
                "does not match canonical retention policy bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum AppPurgeTargetKind {
    ActiveRows,
    AppendOnlyRecordRevisions,
    DatabaseWalAndTemp,
    ScalarAndSearchIndexes,
    PackageAndCacheBytes,
    RetainedAttachments,
    ExportArchives,
    ArtifactV2References,
    MemoryCandidatesAndPromotions,
    Analytics,
    DebugAndPromptCaptures,
    EvaluationArtifacts,
    ProviderSideContinuations,
    RoutesAndPublishedSurfaces,
    SchedulesAndOutbox,
    DisclosureSessions,
    DirectoryAndSearchProjections,
}

impl AppPurgeTargetKind {
    pub const ALL: [Self; 17] = [
        Self::ActiveRows,
        Self::AppendOnlyRecordRevisions,
        Self::DatabaseWalAndTemp,
        Self::ScalarAndSearchIndexes,
        Self::PackageAndCacheBytes,
        Self::RetainedAttachments,
        Self::ExportArchives,
        Self::ArtifactV2References,
        Self::MemoryCandidatesAndPromotions,
        Self::Analytics,
        Self::DebugAndPromptCaptures,
        Self::EvaluationArtifacts,
        Self::ProviderSideContinuations,
        Self::RoutesAndPublishedSurfaces,
        Self::SchedulesAndOutbox,
        Self::DisclosureSessions,
        Self::DirectoryAndSearchProjections,
    ];
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum AppPurgeSelection {
    WholeInstallation,
    Records {
        entity_name: AppName,
        record_ids: Vec<AppRecordId>,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum AppPurgeSelectionKind {
    WholeInstallation,
    Records,
}

fn purge_selection_evidence(
    selection: &AppPurgeSelection,
) -> Result<(AppPurgeSelectionKind, AppDigest), AppRetentionError> {
    let kind = match selection {
        AppPurgeSelection::WholeInstallation => AppPurgeSelectionKind::WholeInstallation,
        AppPurgeSelection::Records { .. } => AppPurgeSelectionKind::Records,
    };
    let digest = AppDigest::blake3_canonical_json(
        &serde_json::to_value(selection)
            .map_err(|error| AppRetentionError::Digest(error.to_string()))?,
    )
    .map_err(|error| AppRetentionError::Digest(error.to_string()))?;
    Ok((kind, digest))
}

impl AppPurgeSelection {
    fn validate(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if let Self::Records { record_ids, .. } = self {
            validate_nonempty_bounded(
                "purge.selection.record_ids",
                record_ids.len(),
                limits.max_collection_items(),
            )?;
            let unique = record_ids.iter().collect::<HashSet<_>>();
            if unique.len() != record_ids.len() {
                return Err(AppContractError::invalid(
                    "purge.selection.record_ids",
                    "contains duplicates",
                ));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgeInventoryEntry {
    pub target: AppPurgeTargetKind,
    pub item_count: u64,
    pub byte_count: u64,
    pub maximum_classification: AppDataClassification,
    pub inventory_digest: AppDigest,
}

/// Trusted, point-in-time multi-store inventory. It is intentionally not
/// deserializable from an API payload.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgeInventorySnapshot {
    scope_binding_ref: AppScopeBindingRef,
    installation_id: AppInstallationId,
    installation_generation: u64,
    entries: Vec<AppPurgeInventoryEntry>,
    snapshot_digest: AppDigest,
    observed_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct AppPurgeInventoryDigestMaterial<'a> {
    scope_binding_ref: &'a AppScopeBindingRef,
    installation_id: &'a AppInstallationId,
    installation_generation: u64,
    entries: &'a [AppPurgeInventoryEntry],
    observed_at: &'a DateTime<Utc>,
}

impl AppPurgeInventorySnapshot {
    pub fn from_storage_governance(
        scope_binding_ref: AppScopeBindingRef,
        installation_id: AppInstallationId,
        installation_generation: u64,
        mut entries: Vec<AppPurgeInventoryEntry>,
        observed_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppRetentionError> {
        if installation_generation == 0 {
            return Err(AppRetentionError::InvalidInstallationGeneration);
        }
        if entries.len() != AppPurgeTargetKind::ALL.len() {
            return Err(AppRetentionError::InvalidContract(
                AppContractError::invalid(
                    "purge.inventory.entries",
                    "must enumerate every purge target exactly once",
                ),
            ));
        }
        entries.sort_by_key(|entry| entry.target);
        validate_exact_inventory(&entries, limits).map_err(AppRetentionError::InvalidContract)?;
        let digest_value = serde_json::to_value(AppPurgeInventoryDigestMaterial {
            scope_binding_ref: &scope_binding_ref,
            installation_id: &installation_id,
            installation_generation,
            entries: &entries,
            observed_at: &observed_at,
        })
        .map_err(|error| AppRetentionError::Digest(error.to_string()))?;
        let snapshot_digest = AppDigest::blake3_canonical_json(&digest_value)
            .map_err(|error| AppRetentionError::Digest(error.to_string()))?;
        Ok(Self {
            scope_binding_ref,
            installation_id,
            installation_generation,
            entries,
            snapshot_digest,
            observed_at,
        })
    }
}

fn validate_exact_inventory(
    entries: &[AppPurgeInventoryEntry],
    limits: &AppContractLimits,
) -> Result<(), AppContractError> {
    validate_nonempty_bounded(
        "purge.inventory.entries",
        entries.len(),
        limits.max_collection_items(),
    )?;
    if entries.len() != AppPurgeTargetKind::ALL.len()
        || entries
            .iter()
            .map(|entry| entry.target)
            .ne(AppPurgeTargetKind::ALL)
    {
        return Err(AppContractError::invalid(
            "purge.inventory.entries",
            "must enumerate every purge target exactly once in canonical order",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgePreview {
    pub protocol_version: u8,
    pub preview_ref: AppReference,
    pub scope_binding_ref: AppScopeBindingRef,
    pub installation_id: AppInstallationId,
    pub installation_generation: u64,
    pub selection: AppPurgeSelection,
    pub inventory_entries: Vec<AppPurgeInventoryEntry>,
    pub inventory_snapshot_digest: AppDigest,
    pub preview_digest: AppDigest,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct AppPurgePreviewDigestMaterial<'a> {
    protocol_version: u8,
    preview_ref: &'a AppReference,
    scope_binding_ref: &'a AppScopeBindingRef,
    installation_id: &'a AppInstallationId,
    installation_generation: u64,
    selection: &'a AppPurgeSelection,
    inventory_entries: &'a [AppPurgeInventoryEntry],
    inventory_snapshot_digest: &'a AppDigest,
    observed_at: &'a DateTime<Utc>,
    expires_at: &'a DateTime<Utc>,
}

impl AppPurgePreview {
    pub fn from_inventory(
        preview_ref: AppReference,
        selection: AppPurgeSelection,
        inventory: &AppPurgeInventorySnapshot,
        expires_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppRetentionError> {
        selection
            .validate(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        if expires_at <= inventory.observed_at {
            return Err(AppRetentionError::InvalidPreviewWindow);
        }
        let mut preview = Self {
            protocol_version: APP_PURGE_PROTOCOL_VERSION,
            preview_ref,
            scope_binding_ref: inventory.scope_binding_ref.clone(),
            installation_id: inventory.installation_id.clone(),
            installation_generation: inventory.installation_generation,
            selection,
            inventory_entries: inventory.entries.clone(),
            inventory_snapshot_digest: inventory.snapshot_digest.clone(),
            preview_digest: AppDigest::blake3(b"pending"),
            observed_at: inventory.observed_at.to_owned(),
            expires_at,
        };
        preview.preview_digest = preview.recompute_digest()?;
        preview
            .validate_app_contract(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        Ok(preview)
    }

    fn recompute_digest(&self) -> Result<AppDigest, AppRetentionError> {
        let value = serde_json::to_value(AppPurgePreviewDigestMaterial {
            protocol_version: self.protocol_version,
            preview_ref: &self.preview_ref,
            scope_binding_ref: &self.scope_binding_ref,
            installation_id: &self.installation_id,
            installation_generation: self.installation_generation,
            selection: &self.selection,
            inventory_entries: &self.inventory_entries,
            inventory_snapshot_digest: &self.inventory_snapshot_digest,
            observed_at: &self.observed_at,
            expires_at: &self.expires_at,
        })
        .map_err(|error| AppRetentionError::Digest(error.to_string()))?;
        AppDigest::blake3_canonical_json(&value)
            .map_err(|error| AppRetentionError::Digest(error.to_string()))
    }
}

impl ValidateAppContract for AppPurgePreview {
    fn validate_app_contract(&self, limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.protocol_version != APP_PURGE_PROTOCOL_VERSION
            || self.installation_generation == 0
            || self.expires_at <= self.observed_at
        {
            return Err(AppContractError::invalid(
                "purge.preview",
                "has an unsupported version, zero generation or invalid validity window",
            ));
        }
        self.selection.validate(limits)?;
        validate_exact_inventory(&self.inventory_entries, limits)?;
        let expected = self
            .recompute_digest()
            .map_err(|error| AppContractError::invalid("preview_digest", error.to_string()))?;
        if self.preview_digest != expected {
            return Err(AppContractError::invalid(
                "preview_digest",
                "does not match the exact canonical purge preview",
            ));
        }
        Ok(())
    }
}

/// Exact user approval for one preview and installation generation. It cannot
/// be deserialized from a request body.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgeApproval {
    approval_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    session_ref: AppReference,
    authentication: AppScopeAuthentication,
    authentication_revision: AppRevision,
    installation_id: AppInstallationId,
    installation_generation: u64,
    preview_digest: AppDigest,
    issued_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl AppPurgeApproval {
    pub fn from_reviewed_preview(
        authenticated_scope: &AuthenticatedAppScope,
        approval_ref: AppReference,
        preview: &AppPurgePreview,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<Self, AppRetentionError> {
        preview
            .validate_app_contract(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        authenticated_scope
            .ensure_live_at(&issued_at)
            .map_err(|_| AppRetentionError::AuthenticationUnavailable)?;
        if preview.scope_binding_ref != *authenticated_scope.scope_binding_ref() {
            return Err(AppRetentionError::PreviewScopeMismatch);
        }
        if expires_at <= issued_at
            || issued_at < preview.observed_at
            || issued_at >= preview.expires_at
        {
            return Err(AppRetentionError::InvalidApprovalWindow);
        }
        Ok(Self {
            approval_ref,
            scope_binding_ref: authenticated_scope.scope_binding_ref().clone(),
            actor_ref: authenticated_scope.actor_ref().clone(),
            session_ref: authenticated_scope.session_ref().clone(),
            authentication: authenticated_scope.authentication(),
            authentication_revision: authenticated_scope.authentication_revision(),
            installation_id: preview.installation_id.clone(),
            installation_generation: preview.installation_generation,
            preview_digest: preview.preview_digest.clone(),
            issued_at,
            expires_at: expires_at
                .min(preview.expires_at.to_owned())
                .min(authenticated_scope.expires_at().to_owned()),
        })
    }

    pub fn approval_ref(&self) -> &AppReference {
        &self.approval_ref
    }

    /// Revalidate the move-only approval immediately before a destructive
    /// operation starts. Recovery may pass the original commit timestamp so
    /// an already-authorized deletion can finish after a crash without
    /// extending or silently replacing the approval.
    pub fn ensure_matches_preview(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        preview: &AppPurgePreview,
        use_at: &DateTime<Utc>,
        limits: &AppContractLimits,
    ) -> Result<(), AppRetentionError> {
        preview
            .validate_app_contract(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        authenticated_scope
            .ensure_live_at(use_at)
            .map_err(|_| AppRetentionError::AuthenticationUnavailable)?;
        if *use_at < self.issued_at
            || *use_at >= self.expires_at
            || self.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            || self.actor_ref != *authenticated_scope.actor_ref()
            || self.session_ref != *authenticated_scope.session_ref()
            || self.authentication != authenticated_scope.authentication()
            || self.authentication_revision != authenticated_scope.authentication_revision()
            || self.installation_id != preview.installation_id
            || self.installation_generation != preview.installation_generation
            || self.preview_digest != preview.preview_digest
        {
            return Err(AppRetentionError::ApprovalMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPurgeTargetStatus {
    Deleted,
    CryptographicallyErased,
    RetainedShared,
    RetainedByPolicy,
    ProviderRetentionUnknown,
    Failed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgeTargetOutcome {
    target: AppPurgeTargetKind,
    status: AppPurgeTargetStatus,
    affected_items: u64,
    affected_bytes: u64,
    outcome_digest: AppDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy_or_ownership_ref: Option<AppReference>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    failure_ref: Option<AppReference>,
}

impl AppPurgeTargetOutcome {
    pub fn from_verified_local_settlement(
        target: AppPurgeTargetKind,
        status: AppPurgeTargetStatus,
        affected_items: u64,
        affected_bytes: u64,
        outcome_digest: AppDigest,
        policy_or_ownership_ref: Option<AppReference>,
        failure_ref: Option<AppReference>,
    ) -> Result<Self, AppRetentionError> {
        let outcome = Self {
            target,
            status,
            affected_items,
            affected_bytes,
            outcome_digest,
            policy_or_ownership_ref,
            failure_ref,
        };
        outcome.validate_semantics()?;
        Ok(outcome)
    }

    fn validate_semantics(&self) -> Result<(), AppRetentionError> {
        if self.status == AppPurgeTargetStatus::CryptographicallyErased
            && matches!(
                self.target,
                AppPurgeTargetKind::ActiveRows
                    | AppPurgeTargetKind::AppendOnlyRecordRevisions
                    | AppPurgeTargetKind::ScalarAndSearchIndexes
                    | AppPurgeTargetKind::RoutesAndPublishedSurfaces
                    | AppPurgeTargetKind::SchedulesAndOutbox
                    | AppPurgeTargetKind::DisclosureSessions
                    | AppPurgeTargetKind::DirectoryAndSearchProjections
                    | AppPurgeTargetKind::DatabaseWalAndTemp
            )
        {
            // These classes share one scope-encrypted SQLite file. Deleting an
            // installation's rows is physical deletion, but the scope key
            // remains live for sibling installations, so per-installation
            // cryptographic erasure would be a false claim.
            return Err(AppRetentionError::SharedDatabaseCryptoErasureOverclaim);
        }
        if self.target == AppPurgeTargetKind::ProviderSideContinuations
            && (self.affected_items > 0 || self.affected_bytes > 0)
            && matches!(
                self.status,
                AppPurgeTargetStatus::Deleted | AppPurgeTargetStatus::CryptographicallyErased
            )
        {
            return Err(AppRetentionError::ProviderDeletionOverclaim);
        }
        match self.status {
            AppPurgeTargetStatus::RetainedShared | AppPurgeTargetStatus::RetainedByPolicy
                if self.policy_or_ownership_ref.is_none() =>
            {
                return Err(AppRetentionError::MissingRetentionEvidence)
            },
            AppPurgeTargetStatus::Failed if self.failure_ref.is_none() => {
                return Err(AppRetentionError::MissingFailureEvidence)
            },
            AppPurgeTargetStatus::Deleted
            | AppPurgeTargetStatus::CryptographicallyErased
            | AppPurgeTargetStatus::ProviderRetentionUnknown
                if self.failure_ref.is_some() =>
            {
                return Err(AppRetentionError::UnexpectedFailureEvidence)
            },
            _ => {},
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum AppPurgeCompletion {
    FullyErased,
    CompletedWithDisclosedRetention,
    Incomplete,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppPurgeReceipt {
    protocol_version: u8,
    receipt_ref: AppReference,
    approval_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    installation_id: AppInstallationId,
    installation_generation: u64,
    selection_kind: AppPurgeSelectionKind,
    selection_digest: AppDigest,
    preview_digest: AppDigest,
    outcomes: Vec<AppPurgeTargetOutcome>,
    completion: AppPurgeCompletion,
    committed_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
pub fn build_purge_receipt(
    receipt_ref: AppReference,
    preview: &AppPurgePreview,
    approval: &AppPurgeApproval,
    authenticated_scope: &AuthenticatedAppScope,
    current_installation: &AppInstallation,
    existing_receipt: Option<&AppPurgeReceipt>,
    mut outcomes: Vec<AppPurgeTargetOutcome>,
    committed_at: DateTime<Utc>,
    limits: &AppContractLimits,
) -> Result<AppPurgeReceipt, AppRetentionError> {
    preview
        .validate_app_contract(limits)
        .map_err(AppRetentionError::InvalidContract)?;
    let approval_use_at = existing_receipt
        .map(|receipt| receipt.committed_at)
        .unwrap_or(committed_at);
    approval.ensure_matches_preview(authenticated_scope, preview, &approval_use_at, limits)?;
    let (selection_kind, selection_digest) = purge_selection_evidence(&preview.selection)?;
    if let Some(existing) = existing_receipt {
        current_installation
            .validate_app_contract(limits)
            .map_err(AppRetentionError::InvalidContract)?;
        let current_generation_match = match existing.selection_kind {
            AppPurgeSelectionKind::WholeInstallation => {
                current_installation.lifecycle.status == AppInstallationStatus::UninstalledRetained
                    && current_installation.lifecycle.generation == existing.installation_generation
                    && current_installation.purged_at.is_none()
            },
            AppPurgeSelectionKind::Records => {
                current_installation.lifecycle.status != AppInstallationStatus::Purged
                    && current_installation.lifecycle.generation == existing.installation_generation
                    && current_installation.purged_at.is_none()
            },
        };
        let terminal_generation_match = existing.permits_terminal_installation_transition()
            && current_installation.lifecycle.status == AppInstallationStatus::Purged
            && current_installation.lifecycle.generation
                == existing.installation_generation.checked_add(1).unwrap_or(0)
            && current_installation.purged_at.is_some();
        if existing.approval_ref != approval.approval_ref
            || existing.scope_binding_ref != approval.scope_binding_ref
            || existing.installation_id != approval.installation_id
            || existing.installation_generation != approval.installation_generation
            || existing.selection_kind != selection_kind
            || existing.selection_digest != selection_digest
            || existing.preview_digest != approval.preview_digest
            || existing.committed_at > committed_at
            || current_installation.scope != *authenticated_scope.scope()
            || current_installation.installation_id != existing.installation_id
            || (!current_generation_match && !terminal_generation_match)
        {
            return Err(AppRetentionError::PurgeOperationCollision);
        }
        return Ok(existing.clone());
    }
    current_installation
        .validate_app_contract(limits)
        .map_err(AppRetentionError::InvalidContract)?;
    let lifecycle_matches_selection = match &preview.selection {
        AppPurgeSelection::WholeInstallation => {
            current_installation.lifecycle.status == AppInstallationStatus::UninstalledRetained
        },
        AppPurgeSelection::Records { .. } => {
            current_installation.lifecycle.status != AppInstallationStatus::Purged
        },
    };
    if current_installation.scope != *authenticated_scope.scope()
        || current_installation.installation_id != preview.installation_id
        || current_installation.lifecycle.generation != preview.installation_generation
        || !lifecycle_matches_selection
        || current_installation.purged_at.is_some()
    {
        return Err(AppRetentionError::InstallationNotCurrentForPurge);
    }
    if outcomes.len() != AppPurgeTargetKind::ALL.len() {
        return Err(AppRetentionError::IncompleteOutcomeInventory);
    }
    outcomes.sort_by_key(|outcome| outcome.target);
    if outcomes
        .iter()
        .map(|outcome| outcome.target)
        .ne(AppPurgeTargetKind::ALL)
    {
        return Err(AppRetentionError::IncompleteOutcomeInventory);
    }
    for (inventory, outcome) in preview.inventory_entries.iter().zip(&outcomes) {
        outcome.validate_semantics()?;
        if inventory.target != outcome.target
            || inventory.item_count != outcome.affected_items
            || inventory.byte_count != outcome.affected_bytes
        {
            return Err(AppRetentionError::OutcomeInventoryMismatch(outcome.target));
        }
    }
    let completion = if outcomes
        .iter()
        .any(|outcome| outcome.status == AppPurgeTargetStatus::Failed)
    {
        AppPurgeCompletion::Incomplete
    } else if outcomes.iter().any(|outcome| {
        matches!(
            outcome.status,
            AppPurgeTargetStatus::RetainedShared
                | AppPurgeTargetStatus::RetainedByPolicy
                | AppPurgeTargetStatus::ProviderRetentionUnknown
        )
    }) {
        AppPurgeCompletion::CompletedWithDisclosedRetention
    } else {
        AppPurgeCompletion::FullyErased
    };
    Ok(AppPurgeReceipt {
        protocol_version: APP_PURGE_PROTOCOL_VERSION,
        receipt_ref,
        approval_ref: approval.approval_ref.clone(),
        scope_binding_ref: approval.scope_binding_ref.clone(),
        installation_id: preview.installation_id.clone(),
        installation_generation: preview.installation_generation,
        selection_kind,
        selection_digest,
        preview_digest: preview.preview_digest.clone(),
        outcomes,
        completion,
        committed_at,
    })
}

impl AppPurgeReceipt {
    pub fn receipt_ref(&self) -> &AppReference {
        &self.receipt_ref
    }

    pub fn approval_ref(&self) -> &AppReference {
        &self.approval_ref
    }

    pub fn installation_id(&self) -> &AppInstallationId {
        &self.installation_id
    }

    pub fn installation_generation(&self) -> u64 {
        self.installation_generation
    }

    pub fn scope_binding_ref(&self) -> &AppScopeBindingRef {
        &self.scope_binding_ref
    }

    pub fn preview_digest(&self) -> &AppDigest {
        &self.preview_digest
    }

    pub fn selection_digest(&self) -> &AppDigest {
        &self.selection_digest
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub fn committed_at(&self) -> &DateTime<Utc> {
        &self.committed_at
    }

    /// Installation state may become `purged` only after the exact whole-app
    /// protocol settles without failed targets. Shared/policy/provider-retained
    /// bytes remain disclosed and never render as fully erased.
    pub fn permits_terminal_installation_transition(&self) -> bool {
        self.selection_kind == AppPurgeSelectionKind::WholeInstallation
            && self.completion != AppPurgeCompletion::Incomplete
            && self.outcomes.iter().all(|outcome| match outcome.target {
                AppPurgeTargetKind::ActiveRows
                | AppPurgeTargetKind::AppendOnlyRecordRevisions
                | AppPurgeTargetKind::ScalarAndSearchIndexes
                | AppPurgeTargetKind::RoutesAndPublishedSurfaces
                | AppPurgeTargetKind::DisclosureSessions
                | AppPurgeTargetKind::DirectoryAndSearchProjections => matches!(
                    outcome.status,
                    AppPurgeTargetStatus::Deleted | AppPurgeTargetStatus::CryptographicallyErased
                ),
                AppPurgeTargetKind::RetainedAttachments
                | AppPurgeTargetKind::ArtifactV2References
                | AppPurgeTargetKind::PackageAndCacheBytes
                | AppPurgeTargetKind::SchedulesAndOutbox => matches!(
                    outcome.status,
                    AppPurgeTargetStatus::Deleted
                        | AppPurgeTargetStatus::CryptographicallyErased
                        | AppPurgeTargetStatus::RetainedShared
                ),
                _ => outcome.status != AppPurgeTargetStatus::Failed,
            })
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPurgeTargetOutcome {
    target: AppPurgeTargetKind,
    status: AppPurgeTargetStatus,
    affected_items: u64,
    affected_bytes: u64,
    outcome_digest: AppDigest,
    #[serde(default)]
    policy_or_ownership_ref: Option<AppReference>,
    #[serde(default)]
    failure_ref: Option<AppReference>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PersistedPurgeReceipt {
    protocol_version: u8,
    receipt_ref: AppReference,
    approval_ref: AppReference,
    scope_binding_ref: AppScopeBindingRef,
    installation_id: AppInstallationId,
    installation_generation: u64,
    selection_kind: AppPurgeSelectionKind,
    selection_digest: AppDigest,
    preview_digest: AppDigest,
    outcomes: Vec<PersistedPurgeTargetOutcome>,
    completion: AppPurgeCompletion,
    committed_at: DateTime<Utc>,
}

/// Decode a receipt only from the registry-owned persisted row. Keeping the
/// public receipt serialization-only prevents request JSON from becoming purge
/// authority while still permitting exact replay after restart.
pub fn decode_stored_purge_receipt(
    bytes: &[u8],
    limits: &AppContractLimits,
) -> Result<AppPurgeReceipt, AppRetentionError> {
    let value = super::models::decode_bounded_json_value(bytes, limits)
        .map_err(AppRetentionError::InvalidContract)?;
    let stored: PersistedPurgeReceipt = serde_json::from_value(value).map_err(|error| {
        AppRetentionError::InvalidContract(AppContractError::invalid(
            "purge_receipt",
            error.to_string(),
        ))
    })?;
    if stored.protocol_version != APP_PURGE_PROTOCOL_VERSION
        || stored.installation_generation == 0
        || stored.outcomes.len() != AppPurgeTargetKind::ALL.len()
    {
        return Err(AppRetentionError::InvalidContract(
            AppContractError::invalid("purge_receipt", "stored receipt shape is invalid"),
        ));
    }
    let mut outcomes = stored
        .outcomes
        .into_iter()
        .map(|outcome| {
            AppPurgeTargetOutcome::from_verified_local_settlement(
                outcome.target,
                outcome.status,
                outcome.affected_items,
                outcome.affected_bytes,
                outcome.outcome_digest,
                outcome.policy_or_ownership_ref,
                outcome.failure_ref,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    outcomes.sort_by_key(|outcome| outcome.target);
    if outcomes
        .iter()
        .map(|outcome| outcome.target)
        .ne(AppPurgeTargetKind::ALL)
    {
        return Err(AppRetentionError::IncompleteOutcomeInventory);
    }
    let completion = if outcomes
        .iter()
        .any(|outcome| outcome.status == AppPurgeTargetStatus::Failed)
    {
        AppPurgeCompletion::Incomplete
    } else if outcomes.iter().any(|outcome| {
        matches!(
            outcome.status,
            AppPurgeTargetStatus::RetainedShared
                | AppPurgeTargetStatus::RetainedByPolicy
                | AppPurgeTargetStatus::ProviderRetentionUnknown
        )
    }) {
        AppPurgeCompletion::CompletedWithDisclosedRetention
    } else {
        AppPurgeCompletion::FullyErased
    };
    if completion != stored.completion {
        return Err(AppRetentionError::InvalidContract(
            AppContractError::invalid("purge_receipt.completion", "does not match stored outcomes"),
        ));
    }
    Ok(AppPurgeReceipt {
        protocol_version: stored.protocol_version,
        receipt_ref: stored.receipt_ref,
        approval_ref: stored.approval_ref,
        scope_binding_ref: stored.scope_binding_ref,
        installation_id: stored.installation_id,
        installation_generation: stored.installation_generation,
        selection_kind: stored.selection_kind,
        selection_digest: stored.selection_digest,
        preview_digest: stored.preview_digest,
        outcomes,
        completion,
        committed_at: stored.committed_at,
    })
}

/// Apply the terminal lifecycle transition only when the receipt binds the
/// exact current retained installation and authenticated scope.
pub fn transition_installation_to_purged(
    installation: &AppInstallation,
    receipt: &AppPurgeReceipt,
    authenticated_scope: &AuthenticatedAppScope,
    now: DateTime<Utc>,
) -> Result<AppInstallationLifecycle, AppRetentionError> {
    authenticated_scope
        .ensure_live_at(&now)
        .map_err(|_| AppRetentionError::AuthenticationUnavailable)?;
    installation
        .validate_app_contract(&AppContractLimits::default())
        .map_err(AppRetentionError::InvalidContract)?;
    if installation.scope != *authenticated_scope.scope()
        || receipt.scope_binding_ref != *authenticated_scope.scope_binding_ref()
        || installation.installation_id != receipt.installation_id
        || installation.lifecycle.generation != receipt.installation_generation
        || installation.lifecycle.status != AppInstallationStatus::UninstalledRetained
        || installation.purged_at.is_some()
        || receipt.committed_at > now
        || !receipt.permits_terminal_installation_transition()
    {
        return Err(AppRetentionError::TerminalPurgeEvidenceMismatch);
    }
    let generation = installation
        .lifecycle
        .generation
        .checked_add(1)
        .ok_or(AppRetentionError::TerminalPurgeEvidenceMismatch)?;
    Ok(AppInstallationLifecycle {
        status: AppInstallationStatus::Purged,
        generation,
        update_return_status: None,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AppRetentionRunReceipt {
    pub receipt_ref: AppReference,
    pub scope_binding_ref: AppScopeBindingRef,
    pub installation_id: AppInstallationId,
    pub policy_revision: AppRevision,
    pub policy_digest: AppDigest,
    pub examined_items: u64,
    pub examined_bytes: u64,
    pub deleted_items: u64,
    pub deleted_bytes: u64,
    pub cryptographically_erased_items: u64,
    pub cryptographically_erased_bytes: u64,
    pub retained_items: u64,
    pub retained_bytes: u64,
    pub failed_items: u64,
    pub failed_bytes: u64,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
}

impl ValidateAppContract for AppRetentionRunReceipt {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.cryptographically_erased_items != 0 || self.cryptographically_erased_bytes != 0 {
            return Err(AppContractError::invalid(
                "retention_receipt.cryptographically_erased",
                "the shared scope database has no per-record encryption key to erase",
            ));
        }
        if self.completed_at < self.started_at {
            return Err(AppContractError::invalid(
                "retention_receipt.completed_at",
                "cannot precede started_at",
            ));
        }
        let settled = self
            .deleted_items
            .checked_add(self.cryptographically_erased_items)
            .and_then(|count| count.checked_add(self.retained_items))
            .and_then(|count| count.checked_add(self.failed_items))
            .ok_or_else(|| {
                AppContractError::invalid("retention_receipt", "item counters overflow")
            })?;
        if settled != self.examined_items {
            return Err(AppContractError::invalid(
                "retention_receipt",
                "settled item counts must equal examined items",
            ));
        }
        let settled_bytes = self
            .deleted_bytes
            .checked_add(self.cryptographically_erased_bytes)
            .and_then(|bytes| bytes.checked_add(self.retained_bytes))
            .and_then(|bytes| bytes.checked_add(self.failed_bytes))
            .ok_or_else(|| {
                AppContractError::invalid("retention_receipt", "byte counters overflow")
            })?;
        if settled_bytes != self.examined_bytes {
            return Err(AppContractError::invalid(
                "retention_receipt",
                "settled byte counts must equal examined bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum AppRetentionError {
    #[error("invalid app retention/purge contract: {0}")]
    InvalidContract(AppContractError),
    #[error("installation generation must be greater than zero")]
    InvalidInstallationGeneration,
    #[error("shared scope database bytes cannot claim per-installation cryptographic erasure")]
    SharedDatabaseCryptoErasureOverclaim,
    #[error("failed to compute canonical retention/purge digest: {0}")]
    Digest(String),
    #[error("purge preview has an invalid validity window")]
    InvalidPreviewWindow,
    #[error("authenticated purge scope is unavailable")]
    AuthenticationUnavailable,
    #[error("purge preview belongs to a different authenticated scope")]
    PreviewScopeMismatch,
    #[error("purge approval has an invalid or stale validity window")]
    InvalidApprovalWindow,
    #[error("purge approval does not bind this scope/session/preview/generation")]
    ApprovalMismatch,
    #[error("current installation does not match the retained purge target")]
    InstallationNotCurrentForPurge,
    #[error(
        "terminal purge transition lacks a complete receipt for the exact current installation"
    )]
    TerminalPurgeEvidenceMismatch,
    #[error("purge replay collided with another scope/preview/generation")]
    PurgeOperationCollision,
    #[error("purge outcome omitted or duplicated an inventory target")]
    IncompleteOutcomeInventory,
    #[error("purge outcome does not match preview inventory for {0:?}")]
    OutcomeInventoryMismatch(AppPurgeTargetKind),
    #[error("provider-side history cannot be claimed deleted by removing local state")]
    ProviderDeletionOverclaim,
    #[error("retained target requires policy or ownership evidence")]
    MissingRetentionEvidence,
    #[error("failed target requires failure evidence")]
    MissingFailureEvidence,
    #[error("non-failed target cannot carry failure evidence")]
    UnexpectedFailureEvidence,
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, TimeZone};

    use super::*;
    use magician::magician_v2::apps::records::AppScope;

    fn reference(value: &str) -> AppReference {
        AppReference::parse(value).expect("valid reference")
    }

    fn revision(value: u64) -> AppRevision {
        AppRevision::new(value).expect("positive revision")
    }

    fn digest(value: &str) -> AppDigest {
        AppDigest::blake3(value.as_bytes())
    }

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 15, 5, 0, 0)
            .single()
            .expect("valid time")
    }

    fn authenticated_scope() -> AuthenticatedAppScope {
        AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: reference("principal:owner"),
                workspace: reference("workspace:default"),
            },
            AppScopeBindingRef::parse("scope_owner_default").expect("scope binding"),
            reference("actor:owner"),
            reference("session:purge"),
            revision(3),
            now() - Duration::minutes(5),
            now() + Duration::minutes(30),
        )
        .expect("authenticated scope")
    }

    fn retained_installation() -> AppInstallation {
        AppInstallation {
            scope: authenticated_scope().scope().clone(),
            installation_id: AppInstallationId::parse("installation-purge")
                .expect("installation id"),
            package_revision_ref: reference("package:purge:1"),
            lifecycle: AppInstallationLifecycle {
                status: AppInstallationStatus::UninstalledRetained,
                generation: 9,
                update_return_status: None,
            },
            grant_revision: Some(revision(3)),
            active_schema_revision: Some(revision(4)),
            active_surface_revision: Some(revision(5)),
            created_at: now() - Duration::days(1),
            updated_at: now() - Duration::minutes(3),
            disabled_at: None,
            quarantined_at: None,
            uninstalled_at: Some(now() - Duration::minutes(3)),
            purged_at: None,
        }
    }

    fn ceiling() -> AppRetentionCeiling {
        AppRetentionCeiling {
            max_age_seconds: 86_400,
            max_bytes: 16 * 1_024 * 1_024,
        }
    }

    fn retention_policy() -> AppRetentionPolicy {
        AppRetentionPolicy::from_policy_parts(
            revision(4),
            AppRecordRevisionRetention {
                max_revisions_per_record: 20,
                ceiling: ceiling(),
            },
            ceiling(),
            ceiling(),
            ceiling(),
            ceiling(),
            ceiling(),
            ceiling(),
            AppAtRestEncryptionContract {
                cipher: AppAtRestCipher::Sqlcipher4Aes256CbcHmacSha512,
                key_ownership: AppAtRestKeyOwnership::ScopeBoundPlatformKey,
                key_revision: revision(2),
                encryption_context_digest: digest("encryption-context"),
                required_from: AppDataClassification::Personal,
            },
            &AppContractLimits::default(),
        )
        .expect("valid retention policy")
    }

    fn inventory_entries() -> Vec<AppPurgeInventoryEntry> {
        AppPurgeTargetKind::ALL
            .into_iter()
            .map(|target| {
                let has_bytes = matches!(
                    target,
                    AppPurgeTargetKind::ActiveRows
                        | AppPurgeTargetKind::AppendOnlyRecordRevisions
                        | AppPurgeTargetKind::RetainedAttachments
                        | AppPurgeTargetKind::ProviderSideContinuations
                );
                AppPurgeInventoryEntry {
                    target,
                    item_count: u64::from(has_bytes),
                    byte_count: if has_bytes { 128 } else { 0 },
                    maximum_classification: AppDataClassification::Personal,
                    inventory_digest: digest(&format!("inventory-{target:?}")),
                }
            })
            .collect()
    }

    fn preview() -> AppPurgePreview {
        let auth = authenticated_scope();
        let inventory = AppPurgeInventorySnapshot::from_storage_governance(
            auth.scope_binding_ref().clone(),
            AppInstallationId::parse("installation-purge").expect("installation id"),
            9,
            inventory_entries(),
            now() - Duration::minutes(2),
            &AppContractLimits::default(),
        )
        .expect("inventory");
        AppPurgePreview::from_inventory(
            reference("preview:purge"),
            AppPurgeSelection::WholeInstallation,
            &inventory,
            now() + Duration::minutes(10),
            &AppContractLimits::default(),
        )
        .expect("preview")
    }

    fn approval(preview: &AppPurgePreview) -> AppPurgeApproval {
        AppPurgeApproval::from_reviewed_preview(
            &authenticated_scope(),
            reference("approval:purge"),
            preview,
            now() - Duration::seconds(1),
            now() + Duration::minutes(5),
            &AppContractLimits::default(),
        )
        .expect("approval")
    }

    fn outcomes_with(
        override_target: Option<(AppPurgeTargetKind, AppPurgeTargetStatus)>,
    ) -> Vec<AppPurgeTargetOutcome> {
        inventory_entries()
            .into_iter()
            .map(|entry| {
                let mut status = if entry.target == AppPurgeTargetKind::ProviderSideContinuations
                    && entry.item_count > 0
                {
                    AppPurgeTargetStatus::ProviderRetentionUnknown
                } else {
                    AppPurgeTargetStatus::Deleted
                };
                if override_target.is_some_and(|(target, _)| target == entry.target) {
                    status = override_target.expect("checked override").1;
                }
                let policy_or_ownership_ref = matches!(
                    status,
                    AppPurgeTargetStatus::RetainedShared | AppPurgeTargetStatus::RetainedByPolicy
                )
                .then(|| reference("retention:evidence"));
                let failure_ref =
                    (status == AppPurgeTargetStatus::Failed).then(|| reference("failure:purge"));
                AppPurgeTargetOutcome::from_verified_local_settlement(
                    entry.target,
                    status,
                    entry.item_count,
                    entry.byte_count,
                    digest(&format!("outcome-{:?}-{status:?}", entry.target)),
                    policy_or_ownership_ref,
                    failure_ref,
                )
                .expect("valid outcome")
            })
            .collect()
    }

    #[test]
    fn retention_policy_has_time_byte_revision_and_encryption_bounds() {
        let policy = retention_policy();
        policy
            .validate_app_contract(&AppContractLimits::default())
            .expect("valid policy");
        let mut tampered = policy.clone();
        tampered.record_revisions.max_revisions_per_record += 1;
        assert!(tampered
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let mut weak = policy;
        weak.at_rest_encryption.required_from = AppDataClassification::Sensitive;
        weak.policy_digest = weak.recompute_digest().expect("policy digest");
        assert!(weak
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }

    #[test]
    fn purge_inventory_must_enumerate_every_store_class_exactly_once() {
        let auth = authenticated_scope();
        let mut entries = inventory_entries();
        entries.pop();
        assert!(matches!(
            AppPurgeInventorySnapshot::from_storage_governance(
                auth.scope_binding_ref().clone(),
                AppInstallationId::parse("installation-purge").expect("installation id"),
                9,
                entries,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppRetentionError::InvalidContract(_))
        ));
    }

    #[test]
    fn purge_inventory_rejects_oversized_input_before_canonical_sorting() {
        let auth = authenticated_scope();
        let mut entries = inventory_entries();
        let duplicate = entries[0].clone();
        entries.push(duplicate);
        assert!(matches!(
            AppPurgeInventorySnapshot::from_storage_governance(
                auth.scope_binding_ref().clone(),
                AppInstallationId::parse("installation-purge").expect("installation id"),
                9,
                entries,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppRetentionError::InvalidContract(_))
        ));
    }

    #[test]
    fn purge_receipt_rejects_oversized_outcome_input_before_sorting() {
        let preview = preview();
        let approval = approval(&preview);
        let mut outcomes = outcomes_with(None);
        let duplicate = outcomes[0].clone();
        outcomes.push(duplicate);
        assert_eq!(
            build_purge_receipt(
                reference("receipt:oversized"),
                &preview,
                &approval,
                &authenticated_scope(),
                &retained_installation(),
                None,
                outcomes,
                now(),
                &AppContractLimits::default(),
            ),
            Err(AppRetentionError::IncompleteOutcomeInventory)
        );
    }

    #[test]
    fn purge_preview_cannot_be_approved_from_another_scope() {
        let preview = preview();
        let other_scope = AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: reference("principal:owner"),
                workspace: reference("workspace:other"),
            },
            AppScopeBindingRef::parse("scope_owner_other").expect("scope binding"),
            reference("actor:owner"),
            reference("session:purge"),
            revision(3),
            now() - Duration::minutes(5),
            now() + Duration::minutes(30),
        )
        .expect("other authenticated scope");
        assert_eq!(
            AppPurgeApproval::from_reviewed_preview(
                &other_scope,
                reference("approval:cross-scope"),
                &preview,
                now(),
                now() + Duration::minutes(5),
                &AppContractLimits::default(),
            ),
            Err(AppRetentionError::PreviewScopeMismatch)
        );
    }

    #[test]
    fn provider_history_can_be_unknown_but_never_claimed_deleted() {
        assert_eq!(
            AppPurgeTargetOutcome::from_verified_local_settlement(
                AppPurgeTargetKind::ProviderSideContinuations,
                AppPurgeTargetStatus::Deleted,
                1,
                128,
                digest("outcome"),
                None,
                None,
            ),
            Err(AppRetentionError::ProviderDeletionOverclaim)
        );
        assert!(AppPurgeTargetOutcome::from_verified_local_settlement(
            AppPurgeTargetKind::ProviderSideContinuations,
            AppPurgeTargetStatus::ProviderRetentionUnknown,
            1,
            128,
            digest("outcome"),
            None,
            None,
        )
        .is_ok());
    }

    #[test]
    fn installation_purge_cannot_claim_shared_database_crypto_erasure() {
        assert_eq!(
            AppPurgeTargetOutcome::from_verified_local_settlement(
                AppPurgeTargetKind::AppendOnlyRecordRevisions,
                AppPurgeTargetStatus::CryptographicallyErased,
                1,
                128,
                digest("shared-db-overclaim"),
                None,
                None,
            ),
            Err(AppRetentionError::SharedDatabaseCryptoErasureOverclaim)
        );
        assert!(AppPurgeTargetOutcome::from_verified_local_settlement(
            AppPurgeTargetKind::AppendOnlyRecordRevisions,
            AppPurgeTargetStatus::Deleted,
            1,
            128,
            digest("shared-db-deleted"),
            None,
            None,
        )
        .is_ok());
    }

    #[test]
    fn partial_cleanup_is_incomplete_and_cannot_commit_purged_state() {
        let preview = preview();
        let receipt = build_purge_receipt(
            reference("receipt:purge"),
            &preview,
            &approval(&preview),
            &authenticated_scope(),
            &retained_installation(),
            None,
            outcomes_with(Some((
                AppPurgeTargetKind::AppendOnlyRecordRevisions,
                AppPurgeTargetStatus::Failed,
            ))),
            now(),
            &AppContractLimits::default(),
        )
        .expect("exact incomplete receipt");
        assert_eq!(receipt.completion, AppPurgeCompletion::Incomplete);
        assert!(!receipt.permits_terminal_installation_transition());
    }

    #[test]
    fn disclosed_external_or_shared_retention_is_not_reported_fully_erased() {
        let preview = preview();
        let receipt = build_purge_receipt(
            reference("receipt:purge"),
            &preview,
            &approval(&preview),
            &authenticated_scope(),
            &retained_installation(),
            None,
            outcomes_with(Some((
                AppPurgeTargetKind::PackageAndCacheBytes,
                AppPurgeTargetStatus::RetainedShared,
            ))),
            now(),
            &AppContractLimits::default(),
        )
        .expect("settled receipt");
        assert_eq!(
            receipt.completion,
            AppPurgeCompletion::CompletedWithDisclosedRetention
        );
        assert!(receipt.permits_terminal_installation_transition());
        let purged = transition_installation_to_purged(
            &retained_installation(),
            &receipt,
            &authenticated_scope(),
            now(),
        )
        .expect("complete exact receipt permits the terminal transition");
        assert_eq!(purged.status, AppInstallationStatus::Purged);
        assert_eq!(purged.generation, 10);
    }

    #[test]
    fn retained_canonical_rows_block_terminal_purge_even_when_disclosed() {
        let preview = preview();
        let receipt = build_purge_receipt(
            reference("receipt:purge"),
            &preview,
            &approval(&preview),
            &authenticated_scope(),
            &retained_installation(),
            None,
            outcomes_with(Some((
                AppPurgeTargetKind::ActiveRows,
                AppPurgeTargetStatus::RetainedByPolicy,
            ))),
            now(),
            &AppContractLimits::default(),
        )
        .expect("disclosed retention receipt");
        assert_eq!(
            receipt.completion,
            AppPurgeCompletion::CompletedWithDisclosedRetention
        );
        assert!(!receipt.permits_terminal_installation_transition());
    }

    #[test]
    fn exact_purge_replay_returns_the_original_receipt() {
        let preview = preview();
        let approval = approval(&preview);
        let original = build_purge_receipt(
            reference("receipt:original"),
            &preview,
            &approval,
            &authenticated_scope(),
            &retained_installation(),
            None,
            outcomes_with(None),
            now(),
            &AppContractLimits::default(),
        )
        .expect("original receipt");
        let replay = build_purge_receipt(
            reference("receipt:ignored"),
            &preview,
            &approval,
            &authenticated_scope(),
            &retained_installation(),
            Some(&original),
            Vec::new(),
            now(),
            &AppContractLimits::default(),
        )
        .expect("idempotent replay");
        assert_eq!(replay.receipt_ref, original.receipt_ref);

        let mut already_purged = retained_installation();
        already_purged.lifecycle = transition_installation_to_purged(
            &already_purged,
            &original,
            &authenticated_scope(),
            now(),
        )
        .expect("terminal transition");
        already_purged.purged_at = Some(now());
        already_purged.updated_at = now();
        let replay_after_transition = build_purge_receipt(
            reference("receipt:ignored-after-transition"),
            &preview,
            &approval,
            &authenticated_scope(),
            &already_purged,
            Some(&original),
            Vec::new(),
            now(),
            &AppContractLimits::default(),
        )
        .expect("receipt replay remains idempotent after terminal transition");
        assert_eq!(replay_after_transition.receipt_ref, original.receipt_ref);
    }

    #[test]
    fn trusted_purge_inputs_are_not_deserializable_from_transport() {
        static_assertions::assert_not_impl_any!(
            AppPurgeInventorySnapshot: serde::de::DeserializeOwned
        );
        static_assertions::assert_not_impl_any!(AppPurgePreview: serde::de::DeserializeOwned);
        static_assertions::assert_not_impl_any!(AppPurgeApproval: serde::de::DeserializeOwned);
        static_assertions::assert_not_impl_any!(
            AppPurgeTargetOutcome: serde::de::DeserializeOwned
        );
    }

    #[test]
    fn retention_receipt_rejects_counter_overflow_and_unsettled_counts() {
        let receipt = AppRetentionRunReceipt {
            receipt_ref: reference("receipt:retention"),
            scope_binding_ref: authenticated_scope().scope_binding_ref().clone(),
            installation_id: AppInstallationId::parse("install_1").unwrap(),
            policy_revision: revision(4),
            policy_digest: retention_policy().policy_digest,
            examined_items: 2,
            examined_bytes: 2,
            deleted_items: 1,
            deleted_bytes: 1,
            cryptographically_erased_items: 0,
            cryptographically_erased_bytes: 0,
            retained_items: 0,
            retained_bytes: 0,
            failed_items: 0,
            failed_bytes: 0,
            started_at: now() - Duration::minutes(1),
            completed_at: now(),
        };
        assert!(receipt
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let mut crypto_erasure_overclaim = receipt.clone();
        crypto_erasure_overclaim.examined_items = 1;
        crypto_erasure_overclaim.examined_bytes = 1;
        crypto_erasure_overclaim.deleted_items = 0;
        crypto_erasure_overclaim.deleted_bytes = 0;
        crypto_erasure_overclaim.cryptographically_erased_items = 1;
        crypto_erasure_overclaim.cryptographically_erased_bytes = 1;
        assert!(crypto_erasure_overclaim
            .validate_app_contract(&AppContractLimits::default())
            .is_err());

        let mut overflow = receipt;
        overflow.examined_items = u64::MAX;
        overflow.deleted_items = u64::MAX;
        overflow.retained_items = 1;
        assert!(overflow
            .validate_app_contract(&AppContractLimits::default())
            .is_err());
    }
}

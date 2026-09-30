//! Whole-installation purge coordinator.
//!
//! Record forget has a separate owner. This module closes the retained-install
//! terminal path: it refuses to preview until memory and retrieval retractions
//! are durably acknowledged, inventories the exact scoped rows, and commits the
//! destructive deletion, replayable receipt, and lifecycle CAS in one registry
//! transaction. Shared packages, canonical Artifact evidence and
//! audit tombstones are disclosed as retained instead of being overclaimed as
//! erased.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, OptionalExtension, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    lifecycle::AppInstallationStatus,
    models::{
        AppContractError, AppContractLimits, AppDataClassification, AppDigest, AppInstallationId,
        AppReference, ValidateAppContract,
    },
    records::{AppInstallation, AppScope},
    registry::{AppRegistryError, AppRegistryService},
    registry_lifecycle::{load_installation, update_installation_cas},
    retention::{
        build_purge_receipt, decode_stored_purge_receipt, transition_installation_to_purged,
        AppPurgeApproval, AppPurgeInventoryEntry, AppPurgeInventorySnapshot, AppPurgePreview,
        AppPurgeReceipt, AppPurgeSelection, AppPurgeTargetKind, AppPurgeTargetOutcome,
        AppPurgeTargetStatus, AppRetentionError,
    },
};

const PURGE_APPROVAL_LIFETIME_SECONDS: i64 = 30;
const PURGE_PREVIEW_RETENTION_SECONDS: i64 = 5 * 60;
const MAX_PENDING_PURGE_PREVIEWS_PER_INSTALLATION: i64 = 8;

#[derive(Serialize)]
#[serde(deny_unknown_fields)]
struct StoredWholeInstallationPurgePreview<'a> {
    kind: &'static str,
    preview: &'a AppPurgePreview,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppWholeInstallationPurgeCommitRequest {
    pub preview_ref: AppReference,
    pub preview_digest: AppDigest,
    pub installation_generation: u64,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub idempotency_key: AppDigest,
}

impl AppWholeInstallationPurgeCommitRequest {
    fn validate(&self, now: &DateTime<Utc>) -> Result<(), AppInstallationPurgeError> {
        if self.installation_generation == 0
            || self.expires_at <= self.observed_at
            || *now < self.observed_at
            || *now >= self.expires_at
        {
            return Err(AppInstallationPurgeError::InvalidCommitRequest);
        }
        Ok(())
    }
}

impl ValidateAppContract for AppWholeInstallationPurgeCommitRequest {
    fn validate_app_contract(&self, _limits: &AppContractLimits) -> Result<(), AppContractError> {
        if self.installation_generation == 0 || self.expires_at <= self.observed_at {
            return Err(AppContractError::invalid(
                "purge_commit",
                "requires a positive generation and a valid preview window",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct AppInstallationPurgeService {
    registry: AppRegistryService,
}

impl AppInstallationPurgeService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self { registry }
    }

    pub async fn preview_whole_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        preview_ref: AppReference,
        observed_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<AppPurgePreview, AppInstallationPurgeError> {
        let installation_id = installation_id.clone();
        let authenticated_for_preview = authenticated.clone();
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        self.registry
            .execute_scoped_typed_write(authenticated, &observed_at, move |connection, scope| {
                let boundary_now = Utc::now();
                authenticated_for_preview
                    .ensure_live_at(&boundary_now)
                    .map_err(AppRegistryError::from)?;
                if boundary_now >= expires_at {
                    return Err(AppInstallationPurgeError::InvalidCommitRequest);
                }
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let installation = load_installation(&transaction, &installation_id)?;
                ensure_retained_installation(&installation, scope)?;
                ensure_destination_retractions_settled(&transaction, &installation_id)?;
                ensure_lifecycle_delivery_idle(&transaction, &installation_id)?;
                let inventory = whole_installation_inventory(
                    &transaction,
                    scope_binding_ref,
                    &installation,
                    observed_at,
                )?;
                let preview = AppPurgePreview::from_inventory(
                    preview_ref,
                    AppPurgeSelection::WholeInstallation,
                    &inventory,
                    expires_at,
                    &AppContractLimits::default(),
                )?;
                persist_whole_installation_preview(
                    &transaction,
                    &installation_id,
                    &preview,
                    &observed_at,
                )?;
                transaction.commit()?;
                Ok(preview)
            })
            .await
    }

    pub async fn commit_whole_installation(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        request: AppWholeInstallationPurgeCommitRequest,
        now: DateTime<Utc>,
    ) -> Result<AppPurgeReceipt, AppInstallationPurgeError> {
        let installation_id = installation_id.clone();
        let authenticated_for_commit = authenticated.clone();
        let scope_binding_ref = authenticated.scope_binding_ref().clone();
        let receipt = self
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
                let commit_now = Utc::now();
                authenticated_for_commit
                    .ensure_live_at(&commit_now)
                    .map_err(AppRegistryError::from)?;
                connection.pragma_update(None, "secure_delete", "ON")?;
                let transaction =
                    connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
                let current = load_installation(&transaction, &installation_id)?;
                ensure_installation_scope(&current, scope)?;

                let approval_ref =
                    derived_reference("app-purge-approval", &request.idempotency_key)?;
                if let Some(receipt) = load_completed_receipt(&transaction, &approval_ref)? {
                    ensure_exact_replay(&receipt, &current, &authenticated_for_commit, &request)?;
                    transaction.rollback()?;
                    return Ok(receipt);
                }

                request.validate(&commit_now)?;

                ensure_retained_installation(&current, scope)?;
                if current.lifecycle.generation != request.installation_generation {
                    return Err(AppInstallationPurgeError::GenerationConflict {
                        expected: request.installation_generation,
                        actual: current.lifecycle.generation,
                    });
                }
                ensure_destination_retractions_settled(&transaction, &installation_id)?;
                ensure_lifecycle_delivery_idle(&transaction, &installation_id)?;

                let inventory = whole_installation_inventory(
                    &transaction,
                    scope_binding_ref,
                    &current,
                    request.observed_at,
                )?;
                let preview = AppPurgePreview::from_inventory(
                    request.preview_ref.clone(),
                    AppPurgeSelection::WholeInstallation,
                    &inventory,
                    request.expires_at,
                    &AppContractLimits::default(),
                )?;
                if preview.preview_digest != request.preview_digest {
                    return Err(AppInstallationPurgeError::InventoryChanged);
                }
                let receipt_ref = derived_reference("app-purge-receipt", &preview.preview_digest)?;
                require_persisted_preview(&transaction, &receipt_ref, &preview)?;
                let approval_expires_at = commit_now
                    .checked_add_signed(chrono::Duration::seconds(PURGE_APPROVAL_LIFETIME_SECONDS))
                    .ok_or(AppInstallationPurgeError::InvalidCommitRequest)?;
                let approval = AppPurgeApproval::from_reviewed_preview(
                    &authenticated_for_commit,
                    approval_ref.clone(),
                    &preview,
                    commit_now,
                    approval_expires_at,
                    &AppContractLimits::default(),
                )?;

                let deleted = delete_installation_owned_rows(
                    &transaction,
                    &installation_id,
                    receipt_ref.as_str(),
                )?;
                verify_deleted_inventory(&preview, &deleted)?;
                let outcomes = purge_outcomes(&preview)?;
                let receipt = build_purge_receipt(
                    receipt_ref.clone(),
                    &preview,
                    &approval,
                    &authenticated_for_commit,
                    &current,
                    None,
                    outcomes,
                    commit_now,
                    &AppContractLimits::default(),
                )?;
                let mut purged = current.clone();
                purged.lifecycle = transition_installation_to_purged(
                    &current,
                    &receipt,
                    &authenticated_for_commit,
                    commit_now,
                )?;
                purged.grant_revision = None;
                purged.active_schema_revision = None;
                purged.active_surface_revision = None;
                purged.purged_at = Some(commit_now);
                purged.updated_at = commit_now;
                purged.validate_app_contract(&AppContractLimits::default())?;
                update_installation_cas(&transaction, &current, &purged)?;

                let updated_preview = transaction.execute(
                    "UPDATE app_purge_receipts
                            SET approval_ref=?2, state='completed', record_json=?3,
                                committed_at=?4
                          WHERE receipt_ref=?1 AND installation_id=?5
                            AND state='pending_checkpoint'",
                    params![
                        receipt_ref.as_str(),
                        approval_ref.as_str(),
                        serde_json::to_vec(&receipt)?,
                        commit_now.to_rfc3339(),
                        installation_id.as_str(),
                    ],
                )?;
                if updated_preview != 1 {
                    return Err(AppInstallationPurgeError::PurgeReplayCollision);
                }
                transaction.commit()?;
                Ok(receipt)
            })
            .await?;
        self.registry.hide_computed_capability_scope(authenticated);
        self.registry
            .tombstone_app_memory_index_projection_for_scope(
                authenticated,
                &format!(
                    "installation-purge:{}:{}:{}",
                    receipt.installation_id().as_str(),
                    receipt.installation_generation(),
                    receipt.receipt_ref().as_str(),
                ),
            )
            .await
            .map_err(|error| AppInstallationPurgeError::IndexProjection(error.to_string()))?;
        Ok(receipt)
    }

    /// Read a completed whole-installation purge receipt by the exact commit
    /// identity retained by the caller. This remains a scoped registry read;
    /// it neither reconstructs a receipt from CLI files nor opens registry
    /// storage outside the server owner.
    pub async fn completed_whole_installation_receipt(
        &self,
        authenticated: &AuthenticatedAppScope,
        idempotency_key: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<Option<AppPurgeReceipt>, AppInstallationPurgeError> {
        let approval_ref = derived_reference("app-purge-approval", idempotency_key)?;
        let expected_scope_binding_ref = authenticated.scope_binding_ref().clone();
        let receipt = self
            .registry
            .execute_scoped_typed_read(authenticated, &now, move |connection, _scope| {
                let stored: Option<(String, Vec<u8>)> = connection
                    .query_row(
                        "SELECT state, record_json FROM app_purge_receipts WHERE approval_ref=?1",
                        params![approval_ref.as_str()],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .optional()?;
                let Some((state, bytes)) = stored else {
                    return Ok(None);
                };
                if state != "completed" {
                    return Ok(None);
                }
                let receipt = decode_stored_purge_receipt(&bytes, &AppContractLimits::default())?;
                if receipt.approval_ref() != &approval_ref
                    || receipt.scope_binding_ref() != &expected_scope_binding_ref
                {
                    return Err(AppInstallationPurgeError::PurgeReplayCollision);
                }
                Ok(Some(receipt))
            })
            .await?;
        Ok(receipt.flatten())
    }
}

fn ensure_retained_installation(
    installation: &AppInstallation,
    scope: &AppScope,
) -> Result<(), AppInstallationPurgeError> {
    ensure_installation_scope(installation, scope)?;
    if installation.lifecycle.status != AppInstallationStatus::UninstalledRetained
        || installation.purged_at.is_some()
    {
        return Err(AppInstallationPurgeError::InstallationNotRetained);
    }
    Ok(())
}

fn ensure_installation_scope(
    installation: &AppInstallation,
    scope: &AppScope,
) -> Result<(), AppInstallationPurgeError> {
    installation.validate_app_contract(&AppContractLimits::default())?;
    if &installation.scope != scope {
        return Err(AppInstallationPurgeError::ScopeMismatch);
    }
    Ok(())
}

fn ensure_destination_retractions_settled(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
) -> Result<(), AppInstallationPurgeError> {
    let memory_unsettled: i64 = transaction.query_row(
        "SELECT
             (SELECT COUNT(*) FROM app_memory_contribution_heads
               WHERE installation_id=?1 AND lifecycle_state != 'settled') +
             (SELECT COUNT(*) FROM app_memory_contribution_outbox
               WHERE installation_id=?1 AND delivery_state != 'delivered') +
             (SELECT COUNT(*) FROM app_memory_invalidation_outbox
               WHERE installation_id=?1 AND delivery_state != 'delivered')",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    let retrieval_unsettled: i64 = transaction.query_row(
        "SELECT
             (SELECT COUNT(*) FROM app_retrieval_projection_heads
               WHERE installation_id=?1 AND lifecycle_state != 'settled') +
             (SELECT COUNT(*) FROM app_retrieval_delivery_outbox
               WHERE installation_id=?1)",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    if memory_unsettled != 0 || retrieval_unsettled != 0 {
        return Err(AppInstallationPurgeError::DestinationRetractionsPending {
            memory: nonnegative_u64(memory_unsettled)?,
            retrieval: nonnegative_u64(retrieval_unsettled)?,
        });
    }
    Ok(())
}

fn ensure_lifecycle_delivery_idle(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
) -> Result<(), AppInstallationPurgeError> {
    let pending: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_lifecycle_outbox
          WHERE installation_id=?1 AND delivery_state != 'delivered'",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    if pending != 0 {
        return Err(AppInstallationPurgeError::LifecycleDeliveryPending(
            nonnegative_u64(pending)?,
        ));
    }
    Ok(())
}

fn persist_whole_installation_preview(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    preview: &AppPurgePreview,
    observed_at: &DateTime<Utc>,
) -> Result<(), AppInstallationPurgeError> {
    let expiry_cutoff = observed_at
        .checked_sub_signed(chrono::Duration::seconds(PURGE_PREVIEW_RETENTION_SECONDS))
        .ok_or(AppInstallationPurgeError::InvalidCommitRequest)?;
    transaction.execute(
        "DELETE FROM app_purge_receipts
          WHERE installation_id=?1 AND state='pending_checkpoint'
            AND approval_ref LIKE 'app-purge-preview:%' AND started_at <= ?2",
        params![
            installation_id.as_str(),
            expiry_cutoff.to_rfc3339_opts(SecondsFormat::Micros, true)
        ],
    )?;
    let pending: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_purge_receipts
          WHERE installation_id=?1 AND state='pending_checkpoint'
            AND approval_ref LIKE 'app-purge-preview:%'",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    if pending >= MAX_PENDING_PURGE_PREVIEWS_PER_INSTALLATION {
        return Err(AppInstallationPurgeError::PreviewQuotaExceeded);
    }
    let receipt_ref = derived_reference("app-purge-receipt", &preview.preview_digest)?;
    let preview_approval_ref = derived_reference("app-purge-preview", &preview.preview_digest)?;
    let selection_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(
        AppPurgeSelection::WholeInstallation,
    )?)?;
    let stored = serde_json::to_vec(&StoredWholeInstallationPurgePreview {
        kind: "whole_installation_preview_v1",
        preview,
    })?;
    transaction.execute(
        "INSERT INTO app_purge_receipts (
             receipt_ref, installation_id, approval_ref, preview_digest,
             selection_digest, state, record_json, started_at, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending_checkpoint', ?6, ?7, NULL)",
        params![
            receipt_ref.as_str(),
            installation_id.as_str(),
            preview_approval_ref.as_str(),
            preview.preview_digest.as_str(),
            selection_digest.as_str(),
            stored,
            observed_at.to_rfc3339_opts(SecondsFormat::Micros, true),
        ],
    )?;
    Ok(())
}

fn require_persisted_preview(
    transaction: &Transaction<'_>,
    receipt_ref: &AppReference,
    preview: &AppPurgePreview,
) -> Result<(), AppInstallationPurgeError> {
    let expected_approval_ref = derived_reference("app-purge-preview", &preview.preview_digest)?;
    let stored: Option<(String, String, String, Vec<u8>)> = transaction
        .query_row(
            "SELECT approval_ref, preview_digest, state, record_json
               FROM app_purge_receipts WHERE receipt_ref=?1",
            params![receipt_ref.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    let expected_bytes = serde_json::to_vec(&StoredWholeInstallationPurgePreview {
        kind: "whole_installation_preview_v1",
        preview,
    })?;
    match stored {
        Some((approval_ref, preview_digest, state, bytes))
            if approval_ref == expected_approval_ref.as_str()
                && preview_digest == preview.preview_digest.as_str()
                && state == "pending_checkpoint"
                && bytes == expected_bytes =>
        {
            Ok(())
        },
        _ => Err(AppInstallationPurgeError::UnissuedPreview),
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct InventoryMetric {
    items: u64,
    bytes: u64,
}

impl InventoryMetric {
    fn checked_add(self, other: Self) -> Result<Self, AppInstallationPurgeError> {
        Ok(Self {
            items: self
                .items
                .checked_add(other.items)
                .ok_or(AppInstallationPurgeError::CounterOverflow)?,
            bytes: self
                .bytes
                .checked_add(other.bytes)
                .ok_or(AppInstallationPurgeError::CounterOverflow)?,
        })
    }
}

fn whole_installation_inventory(
    transaction: &Transaction<'_>,
    scope_binding_ref: super::models::AppScopeBindingRef,
    installation: &AppInstallation,
    observed_at: DateTime<Utc>,
) -> Result<AppPurgeInventorySnapshot, AppInstallationPurgeError> {
    let id = installation.installation_id.as_str();
    let mut metrics = BTreeMap::new();
    metrics.insert(
        AppPurgeTargetKind::ActiveRows,
        metric(
            transaction,
            "SELECT COUNT(*), 0 FROM app_record_heads WHERE installation_id=?1",
            id,
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::AppendOnlyRecordRevisions,
        metric(
            transaction,
            "SELECT COUNT(*), COALESCE(SUM( \
                 length(payload_json) + length(handling_policy_json) + length(provenance_json) \
             ),0) FROM app_record_revisions WHERE installation_id=?1",
            id,
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::DatabaseWalAndTemp,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::ScalarAndSearchIndexes,
        sum_metrics(
            transaction,
            id,
            &[
                "SELECT COUNT(*), \
                 COALESCE(SUM(length(field_path)+length(COALESCE(text_value,''))+COALESCE(length(order_key_asc),0)+COALESCE(length(order_key_desc),0)),0) FROM \
                 app_scalar_indexes WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(field_path)+length(search_text)),0) FROM \
                 app_text_search WHERE installation_id=?1",
            ],
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::PackageAndCacheBytes,
        metric(
            transaction,
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)+length(dependency_lock_json)),0) \
             FROM app_package_revisions WHERE package_revision_ref=?1",
            installation.package_revision_ref.as_str(),
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::RetainedAttachments,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::ExportArchives,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::ArtifactV2References,
        sum_metrics(
            transaction,
            id,
            &[
                "SELECT COUNT(*), COALESCE(SUM(length(identity_json)),0) FROM app_resource_trees \
                 WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(identity_json)+length(final_state_json)),0) \
                 FROM app_resource_retired_trees WHERE installation_id=?1",
            ],
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::MemoryCandidatesAndPromotions,
        memory_inventory(transaction, id)?,
    );
    metrics.insert(
        AppPurgeTargetKind::Analytics,
        sum_metrics(
            transaction,
            id,
            &[
                "SELECT COUNT(*), 0 FROM app_resource_periods WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(projection_json)),0) FROM \
                 app_resource_usage_projection WHERE installation_id=?1",
            ],
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::DebugAndPromptCaptures,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::EvaluationArtifacts,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::ProviderSideContinuations,
        InventoryMetric::default(),
    );
    metrics.insert(
        AppPurgeTargetKind::RoutesAndPublishedSurfaces,
        sum_metrics(
            transaction,
            id,
            &[
                "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_surface_bindings \
                 WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(compiled_set_digest)),0) FROM \
                 app_surface_generations WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(binding_json)+length(envelope_json)),0) \
                 FROM app_surface_generation_members WHERE installation_id=?1",
            ],
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::SchedulesAndOutbox,
        schedule_inventory(transaction, id)?,
    );
    metrics.insert(
        AppPurgeTargetKind::DisclosureSessions,
        metric(
            transaction,
            "SELECT COUNT(*), COALESCE(SUM(bytes),0) FROM (
                SELECT length(evidence_json)+length(snapshot_json) AS bytes FROM app_query_cursors WHERE installation_id=?1
                UNION ALL SELECT length(evidence_json)+length(boundary_json) FROM app_keyset_cursors WHERE installation_id=?1
                UNION ALL SELECT length(record_json) FROM app_data_cleanup_jobs WHERE installation_id=?1
                UNION ALL SELECT length(c.record_id)+24 FROM app_data_cleanup_candidates c JOIN app_data_cleanup_jobs j ON j.job_ref=c.job_ref WHERE j.installation_id=?1)",
            id,
        )?,
    );
    metrics.insert(
        AppPurgeTargetKind::DirectoryAndSearchProjections,
        sum_metrics(
            transaction,
            id,
            &[
                "SELECT COUNT(*), 0 FROM app_directory_state WHERE installation_id=?1",
                "SELECT COUNT(*), COALESCE(SUM(length(target_kind)+length(target_id)),0) FROM \
                 app_directory_pins WHERE installation_id=?1",
            ],
        )?,
    );

    let entries = AppPurgeTargetKind::ALL
        .into_iter()
        .map(|target| {
            let metric = metrics.get(&target).copied().unwrap_or_default();
            let maximum_classification = if metric.items != 0
                || metric.bytes != 0
                || matches!(
                    target,
                    AppPurgeTargetKind::DatabaseWalAndTemp
                        | AppPurgeTargetKind::PackageAndCacheBytes
                        | AppPurgeTargetKind::RetainedAttachments
                        | AppPurgeTargetKind::ExportArchives
                        | AppPurgeTargetKind::ArtifactV2References
                        | AppPurgeTargetKind::MemoryCandidatesAndPromotions
                        | AppPurgeTargetKind::Analytics
                        | AppPurgeTargetKind::DebugAndPromptCaptures
                        | AppPurgeTargetKind::EvaluationArtifacts
                        | AppPurgeTargetKind::ProviderSideContinuations
                ) {
                AppDataClassification::Secret
            } else {
                AppDataClassification::Ordinary
            };
            let inventory_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                "target": target,
                "item_count": metric.items,
                "byte_count": metric.bytes,
                "maximum_classification": maximum_classification,
            }))?;
            Ok(AppPurgeInventoryEntry {
                target,
                item_count: metric.items,
                byte_count: metric.bytes,
                maximum_classification,
                inventory_digest,
            })
        })
        .collect::<Result<Vec<_>, AppInstallationPurgeError>>()?;
    Ok(AppPurgeInventorySnapshot::from_storage_governance(
        scope_binding_ref,
        installation.installation_id.clone(),
        installation.lifecycle.generation,
        entries,
        observed_at,
        &AppContractLimits::default(),
    )?)
}

fn memory_inventory(
    transaction: &Transaction<'_>,
    installation_id: &str,
) -> Result<InventoryMetric, AppInstallationPurgeError> {
    sum_metrics(
        transaction,
        installation_id,
        &[
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_memory_candidates \
             WHERE candidate_id IN (SELECT candidate_id FROM app_memory_candidate_sources WHERE \
             installation_id=?1)",
            "SELECT COUNT(*), COALESCE(SUM(length(entity_name)+length(record_id)),0) FROM \
             app_memory_candidate_sources WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(proposal_json)),0) FROM \
             app_memory_contribution_outbox WHERE installation_id=?1",
            "SELECT COUNT(*), \
             COALESCE(SUM(length(invalidation_json)+length(source_proposal_json)),0) FROM \
             app_memory_invalidation_outbox WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(COALESCE(proposal_json,''))),0) FROM \
             app_memory_contribution_heads WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(dedupe_key)),0) FROM \
             app_memory_contribution_terminal WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(dedupe_key)),0) FROM \
             app_memory_invalidation_terminal WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(event_ledger_json)),0) FROM \
             app_contribution_frequency_buckets WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(contribution_port_id)),0) FROM \
             app_contribution_frequency_compaction_heads WHERE installation_id=?1",
            "SELECT COUNT(*), \
             COALESCE(SUM(length(proposal_json)+length(COALESCE(invalidation_json,''))),0) FROM \
             app_retrieval_delivery_outbox WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(COALESCE(proposal_json,''))),0) FROM \
             app_retrieval_projection_heads WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(COALESCE(payload_json,''))),0) FROM \
             app_retrieval_delivery_terminal WHERE installation_id=?1",
        ],
    )
}

fn schedule_inventory(
    transaction: &Transaction<'_>,
    installation_id: &str,
) -> Result<InventoryMetric, AppInstallationPurgeError> {
    sum_metrics(
        transaction,
        installation_id,
        &[
            "SELECT COUNT(*), COALESCE(SUM(length(event_ref)+length(projection_json)),0) FROM \
             app_event_ingress_receipts WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(event_ref)+length(projection_digest)),0) FROM \
             app_event_ingress_tombstones WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(event_ref)+length(projection_json)),0) FROM \
             app_event_behavior_fires WHERE installation_id=?1",
            "SELECT COUNT(*), 0 FROM app_event_behavior_periods WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(payload_json)),0) FROM \
             app_owner_notification_outbox WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(effect_ref)+length(payload_digest)),0) FROM \
             app_owner_notification_tombstones WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(candidate_ref)+length(reason_code)),0) FROM \
             app_terminal_compaction_quarantine WHERE installation_id=?1",
            "SELECT COUNT(*), \
             COALESCE(SUM(length(behavior_digest)+length(COALESCE(invocation_json,''))),0) FROM \
             app_behavior_heads WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(payload_json)),0) FROM app_entity_outbox WHERE \
             installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_mutation_receipts \
             WHERE installation_id=?1",
            "SELECT COUNT(*), 0 FROM app_installation_sequences WHERE installation_id=?1",
            "SELECT COUNT(*), 0 FROM app_dataset_generations WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(payload_json)),0) FROM app_lifecycle_outbox \
             WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_grant_revisions WHERE \
             installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_schema_revisions \
             WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM \
             app_installation_approvals WHERE attempt_id IN (SELECT attempt_id FROM \
             app_lifecycle_attempts WHERE installation_id=?1)",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_lifecycle_attempts \
             WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_migration_runs WHERE \
             installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_data_import_receipts \
             WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_purge_receipts WHERE \
             installation_id=?1 AND approval_ref NOT LIKE 'app-purge-preview:%'",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_retention_policies \
             WHERE installation_id=?1",
            "SELECT COUNT(*), COALESCE(SUM(length(record_json)),0) FROM app_retention_runs WHERE \
             installation_id=?1",
            "SELECT COUNT(*), 0 FROM app_storage_usage WHERE installation_id=?1",
        ],
    )
}

fn metric(
    transaction: &Transaction<'_>,
    sql: &str,
    identity: &str,
) -> Result<InventoryMetric, AppInstallationPurgeError> {
    let (items, bytes): (i64, i64) =
        transaction.query_row(sql, params![identity], |row| Ok((row.get(0)?, row.get(1)?)))?;
    Ok(InventoryMetric {
        items: nonnegative_u64(items)?,
        bytes: nonnegative_u64(bytes)?,
    })
}

fn sum_metrics(
    transaction: &Transaction<'_>,
    identity: &str,
    queries: &[&str],
) -> Result<InventoryMetric, AppInstallationPurgeError> {
    queries
        .iter()
        .try_fold(InventoryMetric::default(), |total, sql| {
            total.checked_add(metric(transaction, sql, identity)?)
        })
}

fn delete_installation_owned_rows(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    retained_receipt_ref: &str,
) -> Result<BTreeMap<AppPurgeTargetKind, u64>, AppInstallationPurgeError> {
    let id = installation_id.as_str();
    let mut deleted = BTreeMap::new();
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::ScalarAndSearchIndexes,
        &[
            "DELETE FROM app_scalar_indexes WHERE installation_id=?1",
            "DELETE FROM app_text_search WHERE installation_id=?1",
        ],
        &mut deleted,
    )?;
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::ActiveRows,
        &["DELETE FROM app_record_heads WHERE installation_id=?1"],
        &mut deleted,
    )?;
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::AppendOnlyRecordRevisions,
        &["DELETE FROM app_record_revisions WHERE installation_id=?1"],
        &mut deleted,
    )?;
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::DisclosureSessions,
        &["DELETE FROM app_query_cursors WHERE installation_id=?1", "DELETE FROM app_keyset_cursors WHERE installation_id=?1",
          "DELETE FROM app_data_cleanup_candidates WHERE job_ref IN (SELECT job_ref FROM app_data_cleanup_jobs WHERE installation_id=?1)",
          "DELETE FROM app_data_cleanup_jobs WHERE installation_id=?1"],
        &mut deleted,
    )?;
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::DirectoryAndSearchProjections,
        &[
            "DELETE FROM app_directory_pins WHERE installation_id=?1",
            "DELETE FROM app_directory_state WHERE installation_id=?1",
        ],
        &mut deleted,
    )?;
    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::RoutesAndPublishedSurfaces,
        &[
            "DELETE FROM app_surface_generation_members WHERE installation_id=?1",
            "DELETE FROM app_surface_generations WHERE installation_id=?1",
            "DELETE FROM app_surface_bindings WHERE installation_id=?1",
        ],
        &mut deleted,
    )?;

    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::MemoryCandidatesAndPromotions,
        &[
            "DELETE FROM app_memory_read_grant_heads WHERE installation_id=?1",
            "DELETE FROM app_memory_candidate_sources WHERE installation_id=?1",
            "DELETE FROM app_memory_contribution_sources WHERE installation_id=?1",
            "DELETE FROM app_memory_invalidation_outbox WHERE installation_id=?1",
            "DELETE FROM app_memory_contribution_outbox WHERE installation_id=?1",
            "DELETE FROM app_memory_contribution_heads WHERE installation_id=?1",
            "DELETE FROM app_memory_contribution_terminal WHERE installation_id=?1",
            "DELETE FROM app_memory_invalidation_terminal WHERE installation_id=?1",
            "DELETE FROM app_contribution_frequency_buckets WHERE installation_id=?1",
            "DELETE FROM app_contribution_frequency_compaction_heads WHERE installation_id=?1",
            "DELETE FROM app_retrieval_delivery_outbox WHERE installation_id=?1",
            "DELETE FROM app_retrieval_projection_heads WHERE installation_id=?1",
            "DELETE FROM app_retrieval_delivery_terminal WHERE installation_id=?1",
        ],
        &mut deleted,
    )?;

    delete_target(
        transaction,
        id,
        AppPurgeTargetKind::SchedulesAndOutbox,
        &[
            "DELETE FROM app_event_behavior_fires WHERE installation_id=?1",
            "DELETE FROM app_event_ingress_receipts WHERE installation_id=?1",
            "DELETE FROM app_event_ingress_tombstones WHERE installation_id=?1",
            "DELETE FROM app_event_behavior_periods WHERE installation_id=?1",
            "DELETE FROM app_owner_notification_outbox WHERE installation_id=?1",
            "DELETE FROM app_owner_notification_tombstones WHERE installation_id=?1",
            "DELETE FROM app_terminal_compaction_quarantine WHERE installation_id=?1",
            "DELETE FROM app_behavior_heads WHERE installation_id=?1",
            "DELETE FROM app_entity_outbox WHERE installation_id=?1",
            "DELETE FROM app_mutation_receipts WHERE installation_id=?1",
            "DELETE FROM app_installation_sequences WHERE installation_id=?1",
            "DELETE FROM app_dataset_generations WHERE installation_id=?1",
            "DELETE FROM app_lifecycle_outbox WHERE installation_id=?1",
            "DELETE FROM app_installation_approvals WHERE attempt_id IN (SELECT attempt_id FROM \
             app_lifecycle_attempts WHERE installation_id=?1)",
            "DELETE FROM app_lifecycle_attempts WHERE installation_id=?1",
            "DELETE FROM app_grant_revisions WHERE installation_id=?1",
            "DELETE FROM app_schema_revisions WHERE installation_id=?1",
            "DELETE FROM app_migration_runs WHERE installation_id=?1",
            "DELETE FROM app_data_import_receipts WHERE installation_id=?1",
            "DELETE FROM app_retention_runs WHERE installation_id=?1",
            "DELETE FROM app_retention_policies WHERE installation_id=?1",
            "DELETE FROM app_storage_usage WHERE installation_id=?1",
        ],
        &mut deleted,
    )?;
    let old_purge_rows = transaction.execute(
        "DELETE FROM app_purge_receipts
          WHERE installation_id=?1 AND receipt_ref != ?2
            AND approval_ref NOT LIKE 'app-purge-preview:%'",
        params![id, retained_receipt_ref],
    )?;
    add_deleted(
        &mut deleted,
        AppPurgeTargetKind::SchedulesAndOutbox,
        old_purge_rows,
    )?;
    transaction.execute(
        "DELETE FROM app_purge_receipts
          WHERE installation_id=?1 AND receipt_ref != ?2
            AND approval_ref LIKE 'app-purge-preview:%'",
        params![id, retained_receipt_ref],
    )?;
    Ok(deleted)
}

fn delete_target(
    transaction: &Transaction<'_>,
    installation_id: &str,
    target: AppPurgeTargetKind,
    statements: &[&str],
    deleted: &mut BTreeMap<AppPurgeTargetKind, u64>,
) -> Result<(), AppInstallationPurgeError> {
    for sql in statements {
        let count = transaction.execute(sql, params![installation_id])?;
        add_deleted(deleted, target, count)?;
    }
    Ok(())
}

fn add_deleted(
    deleted: &mut BTreeMap<AppPurgeTargetKind, u64>,
    target: AppPurgeTargetKind,
    count: usize,
) -> Result<(), AppInstallationPurgeError> {
    let count = u64::try_from(count).map_err(|_| AppInstallationPurgeError::CounterOverflow)?;
    let next = deleted
        .get(&target)
        .copied()
        .unwrap_or(0)
        .checked_add(count)
        .ok_or(AppInstallationPurgeError::CounterOverflow)?;
    deleted.insert(target, next);
    Ok(())
}

fn verify_deleted_inventory(
    preview: &AppPurgePreview,
    deleted: &BTreeMap<AppPurgeTargetKind, u64>,
) -> Result<(), AppInstallationPurgeError> {
    for target in [
        AppPurgeTargetKind::ActiveRows,
        AppPurgeTargetKind::AppendOnlyRecordRevisions,
        AppPurgeTargetKind::ScalarAndSearchIndexes,
        AppPurgeTargetKind::RoutesAndPublishedSurfaces,
        AppPurgeTargetKind::SchedulesAndOutbox,
        AppPurgeTargetKind::DisclosureSessions,
        AppPurgeTargetKind::DirectoryAndSearchProjections,
    ] {
        let inventory = preview
            .inventory_entries
            .iter()
            .find(|entry| entry.target == target)
            .ok_or(AppInstallationPurgeError::InventoryChanged)?;
        if deleted.get(&target).copied().unwrap_or(0) != inventory.item_count {
            return Err(AppInstallationPurgeError::InventoryChanged);
        }
    }
    Ok(())
}

fn purge_outcomes(
    preview: &AppPurgePreview,
) -> Result<Vec<AppPurgeTargetOutcome>, AppInstallationPurgeError> {
    preview
        .inventory_entries
        .iter()
        .map(|entry| {
            let (status, evidence) =
                purge_status(entry.target, entry.item_count, entry.byte_count)?;
            let outcome_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                "target": entry.target,
                "status": status,
                "affected_items": entry.item_count,
                "affected_bytes": entry.byte_count,
                "policy_or_ownership_ref": evidence.as_ref(),
            }))?;
            Ok(AppPurgeTargetOutcome::from_verified_local_settlement(
                entry.target,
                status,
                entry.item_count,
                entry.byte_count,
                outcome_digest,
                evidence,
                None,
            )?)
        })
        .collect()
}

fn purge_status(
    target: AppPurgeTargetKind,
    _items: u64,
    _bytes: u64,
) -> Result<(AppPurgeTargetStatus, Option<AppReference>), AppInstallationPurgeError> {
    if target == AppPurgeTargetKind::ProviderSideContinuations {
        return Ok((AppPurgeTargetStatus::ProviderRetentionUnknown, None));
    }
    // SQLite's WAL and temporary files are shared by every installation in the
    // scoped registry. `secure_delete` clears freed database cells, but this
    // coordinator cannot honestly attribute or erase shared WAL frames. Keep
    // that ownership visible even when no installation-specific rows can be
    // inventoried for the target.
    if target == AppPurgeTargetKind::DatabaseWalAndTemp {
        return Ok((
            AppPurgeTargetStatus::RetainedShared,
            Some(AppReference::parse("retention:shared-registry-wal")?),
        ));
    }
    if target == AppPurgeTargetKind::SchedulesAndOutbox {
        // Registry-owned schedules/outboxes were physically deleted and are
        // still counted above. A delivered notification may also have an
        // installation-tagged pending/history copy in the separately owned
        // UserRequest shards, however, so this mixed target must not claim
        // complete deletion until that owner participates in purge.
        return Ok((
            AppPurgeTargetStatus::RetainedShared,
            Some(AppReference::parse(
                "retention:user-request-notification-owner",
            )?),
        ));
    }
    if matches!(
        target,
        AppPurgeTargetKind::ActiveRows
            | AppPurgeTargetKind::AppendOnlyRecordRevisions
            | AppPurgeTargetKind::ScalarAndSearchIndexes
            | AppPurgeTargetKind::RoutesAndPublishedSurfaces
            | AppPurgeTargetKind::DisclosureSessions
            | AppPurgeTargetKind::DirectoryAndSearchProjections
    ) {
        // The rows were physically deleted under secure_delete, while the
        // SQLCipher key remains live for sibling installations in this scope.
        // Persist that ownership fact in the outcome digest/receipt so no
        // downstream surface can reinterpret deletion as crypto-erasure.
        return Ok((
            AppPurgeTargetStatus::Deleted,
            Some(AppReference::parse(
                "encryption:scope-database-key-retained-v1",
            )?),
        ));
    }
    let retained = match target {
        AppPurgeTargetKind::PackageAndCacheBytes => Some((
            AppPurgeTargetStatus::RetainedShared,
            "retention:shared-package-content",
        )),
        AppPurgeTargetKind::RetainedAttachments => Some((
            AppPurgeTargetStatus::RetainedShared,
            "retention:external-attachment-owner",
        )),
        AppPurgeTargetKind::ExportArchives => Some((
            AppPurgeTargetStatus::RetainedByPolicy,
            "retention:external-export-owner",
        )),
        AppPurgeTargetKind::ArtifactV2References => Some((
            AppPurgeTargetStatus::RetainedShared,
            "retention:artifact-v2-owner",
        )),
        AppPurgeTargetKind::MemoryCandidatesAndPromotions => Some((
            AppPurgeTargetStatus::RetainedByPolicy,
            "retention:memory-audit-tombstones",
        )),
        AppPurgeTargetKind::Analytics => Some((
            AppPurgeTargetStatus::RetainedByPolicy,
            "retention:resource-audit-ledger",
        )),
        AppPurgeTargetKind::DebugAndPromptCaptures | AppPurgeTargetKind::EvaluationArtifacts => {
            Some((
                AppPurgeTargetStatus::RetainedByPolicy,
                "retention:security-audit-policy",
            ))
        },
        _ => None,
    };
    match retained {
        Some((status, evidence)) => Ok((status, Some(AppReference::parse(evidence)?))),
        None => Ok((AppPurgeTargetStatus::Deleted, None)),
    }
}

fn load_completed_receipt(
    transaction: &Transaction<'_>,
    approval_ref: &AppReference,
) -> Result<Option<AppPurgeReceipt>, AppInstallationPurgeError> {
    let stored: Option<(String, Vec<u8>)> = transaction
        .query_row(
            "SELECT state, record_json FROM app_purge_receipts WHERE approval_ref=?1",
            params![approval_ref.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match stored {
        None => Ok(None),
        Some((state, bytes)) if state == "completed" => Ok(Some(decode_stored_purge_receipt(
            &bytes,
            &AppContractLimits::default(),
        )?)),
        Some(_) => Err(AppInstallationPurgeError::PurgeReplayCollision),
    }
}

fn ensure_exact_replay(
    receipt: &AppPurgeReceipt,
    installation: &AppInstallation,
    authenticated: &AuthenticatedAppScope,
    request: &AppWholeInstallationPurgeCommitRequest,
) -> Result<(), AppInstallationPurgeError> {
    ensure_installation_scope(installation, authenticated.scope())?;
    if receipt.installation_id() != &installation.installation_id
        || receipt.installation_generation() != request.installation_generation
        || receipt.preview_digest() != &request.preview_digest
        || receipt.scope_binding_ref() != authenticated.scope_binding_ref()
        || installation.lifecycle.status != AppInstallationStatus::Purged
        || installation.lifecycle.generation != request.installation_generation.saturating_add(1)
        || installation.purged_at.is_none()
    {
        return Err(AppInstallationPurgeError::PurgeReplayCollision);
    }
    Ok(())
}

fn derived_reference(
    domain: &str,
    digest: &AppDigest,
) -> Result<AppReference, AppInstallationPurgeError> {
    let suffix = digest
        .as_str()
        .strip_prefix("blake3:")
        .unwrap_or(digest.as_str());
    Ok(AppReference::parse(format!("{domain}:{suffix}"))?)
}

fn nonnegative_u64(value: i64) -> Result<u64, AppInstallationPurgeError> {
    u64::try_from(value).map_err(|_| AppInstallationPurgeError::CorruptAccounting)
}

#[derive(Debug, Error)]
pub enum AppInstallationPurgeError {
    #[error("app purge registry failed: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("app purge SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("app purge JSON evidence failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid app purge contract: {0}")]
    Contract(#[from] AppContractError),
    #[error("app purge retention protocol failed: {0}")]
    Retention(#[from] AppRetentionError),
    #[error("whole-installation purge requires an uninstalled-retained installation")]
    InstallationNotRetained,
    #[error("app purge scope does not match the installation")]
    ScopeMismatch,
    #[error("invalid or expired whole-installation purge commit request")]
    InvalidCommitRequest,
    #[error("app purge generation changed: expected {expected}, actual {actual}")]
    GenerationConflict { expected: u64, actual: u64 },
    #[error("memory/retrieval retractions are pending (memory={memory}, retrieval={retrieval})")]
    DestinationRetractionsPending { memory: u64, retrieval: u64 },
    #[error("{0} lifecycle projection events remain undelivered")]
    LifecycleDeliveryPending(u64),
    #[error("the exact purge inventory changed; request a fresh preview")]
    InventoryChanged,
    #[error("the purge confirmation does not reference an exact server-issued preview")]
    UnissuedPreview,
    #[error("too many unexpired purge previews exist for this installation")]
    PreviewQuotaExceeded,
    #[error("the purge idempotency identity collided with different durable evidence")]
    PurgeReplayCollision,
    #[error("purge accounting exceeded its bounded integer range")]
    CounterOverflow,
    #[error("stored purge accounting is negative or corrupt")]
    CorruptAccounting,
    #[error("app-memory index tombstone failed after purge commit: {0}")]
    IndexProjection(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::{
        apps::{
            entity_store::tests::{compiled_schema, seed_enabled_installation, seed_records},
            models::{decode_app_contract, AppRevision, AppScopeBindingRef},
            records::AppScope,
            registry::{encode_bounded_json, enum_json_label, tests::canonical_tempdir},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    async fn seeded_purge() -> (
        tempfile::TempDir,
        AppRegistryService,
        AppInstallationPurgeService,
        AuthenticatedAppScope,
    ) {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (schema_digest, schema) = compiled_schema();
        seed_enabled_installation(
            &registry,
            schema_digest,
            schema,
            AppInstallationStatus::UninstalledRetained,
        )
        .await;
        seed_records(&registry).await;
        let authentication_now = Utc::now();
        let authenticated = AuthenticatedAppScope::from_verified_session(
            AppScope {
                principal: AppReference::parse("anonymous").unwrap(),
                workspace: AppReference::parse("default").unwrap(),
            },
            AppScopeBindingRef::parse("scope_1").unwrap(),
            AppReference::parse("actor:owner").unwrap(),
            AppReference::parse("session:purge-test").unwrap(),
            AppRevision::new(1).unwrap(),
            authentication_now - chrono::Duration::hours(1),
            authentication_now + chrono::Duration::hours(1),
        )
        .unwrap();
        // The shared entity-store seed predates the byte-exact lifecycle CAS:
        // normalize its denormalized status and canonical record bytes before
        // this test exercises the production purge transition.
        registry
            .execute_scoped_test_write(&authenticated, &authentication_now, |connection, _| {
                let stored: Vec<u8> = connection.query_row(
                    "SELECT record_json FROM app_installations WHERE installation_id='install_1'",
                    [],
                    |row| row.get(0),
                )?;
                let installation: AppInstallation =
                    decode_app_contract(&stored, &AppContractLimits::default())?;
                connection.execute(
                    "UPDATE app_installations
                        SET lifecycle_status=?1, record_json=?2
                      WHERE installation_id='install_1'",
                    params![
                        enum_json_label(&installation.lifecycle.status)?,
                        encode_bounded_json(&installation, &AppContractLimits::default())?,
                    ],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        let service = AppInstallationPurgeService::new(registry.clone());
        (temporary, registry, service, authenticated)
    }

    fn request(preview: &AppPurgePreview) -> AppWholeInstallationPurgeCommitRequest {
        AppWholeInstallationPurgeCommitRequest {
            preview_ref: preview.preview_ref.clone(),
            preview_digest: preview.preview_digest.clone(),
            installation_generation: preview.installation_generation,
            observed_at: preview.observed_at.to_owned(),
            expires_at: preview.expires_at.to_owned(),
            idempotency_key: AppDigest::blake3(b"whole-installation-purge-1"),
        }
    }

    #[tokio::test]
    async fn issued_whole_purge_is_atomic_and_replays_after_preview_expiry() {
        let (_temporary, registry, service, authenticated) = seeded_purge().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let observed_at = Utc::now() - chrono::Duration::seconds(1);
        let expires_at = observed_at + chrono::Duration::minutes(5);
        let preview = service
            .preview_whole_installation(
                &authenticated,
                &installation_id,
                AppReference::parse("purge-preview:test-1").unwrap(),
                observed_at,
                expires_at,
            )
            .await
            .unwrap();
        assert_eq!(
            preview.inventory_entries.len(),
            AppPurgeTargetKind::ALL.len()
        );
        let request = request(&preview);
        let idempotency_key = request.idempotency_key.clone();
        let receipt = service
            .commit_whole_installation(
                &authenticated,
                &installation_id,
                request.clone(),
                observed_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        let replay = service
            .commit_whole_installation(
                &authenticated,
                &installation_id,
                request,
                expires_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        assert_eq!(replay.receipt_ref(), receipt.receipt_ref());
        let recovered = service
            .completed_whole_installation_receipt(
                &authenticated,
                &idempotency_key,
                expires_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap()
            .expect("completed receipt is recoverable by the retained key");
        assert_eq!(recovered.receipt_ref(), receipt.receipt_ref());
        assert!(service
            .completed_whole_installation_receipt(
                &authenticated,
                &AppDigest::blake3(b"different-purge"),
                expires_at + chrono::Duration::seconds(2),
            )
            .await
            .unwrap()
            .is_none());

        let stored = registry
            .execute_scoped_read(
                &authenticated,
                &(observed_at + chrono::Duration::seconds(2)),
                |connection, _| {
                    let status: String = connection.query_row(
                        "SELECT lifecycle_status FROM app_installations WHERE \
                         installation_id='install_1'",
                        [],
                        |row| row.get(0),
                    )?;
                    let rows: i64 = connection.query_row(
                        "SELECT (SELECT COUNT(*) FROM app_record_heads WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_record_revisions WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_scalar_indexes WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_lifecycle_outbox WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_grant_revisions WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_schema_revisions WHERE \
                         installation_id='install_1') +
                            (SELECT COUNT(*) FROM app_lifecycle_attempts WHERE \
                         installation_id='install_1')",
                        [],
                        |row| row.get(0),
                    )?;
                    let receipts: i64 = connection.query_row(
                        "SELECT COUNT(*) FROM app_purge_receipts
                      WHERE installation_id='install_1' AND state='completed'",
                        [],
                        |row| row.get(0),
                    )?;
                    Ok((status, rows, receipts))
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored, ("purged".to_owned(), 0, 1));
    }

    #[tokio::test]
    async fn item4_terminal_rows_are_inventoried_deleted_and_shared_cursor_is_retained() {
        let (_temporary, registry, service, authenticated) = seeded_purge().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let observed_at = Utc::now();
        let baseline = service
            .preview_whole_installation(
                &authenticated,
                &installation_id,
                AppReference::parse("purge-preview:item4-baseline").unwrap(),
                observed_at,
                observed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap();
        let baseline_schedules = baseline
            .inventory_entries
            .iter()
            .find(|entry| entry.target == AppPurgeTargetKind::SchedulesAndOutbox)
            .unwrap()
            .item_count;

        let seed_at = observed_at;
        let seed_write_at = seed_at.clone();
        registry
            .execute_scoped_test_write(&authenticated, &seed_at, move |connection, _| {
                let timestamp = seed_write_at.to_rfc3339_opts(SecondsFormat::Micros, true);
                connection.execute(
                    "INSERT INTO app_event_ingress_tombstones (
                         installation_id, event_ref, event_kind, projection_digest,
                         accepted_fanout, receipt_created_at, compacted_at
                     ) VALUES ('install_1', 'event:item4',
                               'installation_execution_terminal_v1', ?1, 0, ?2, ?2)",
                    params![AppDigest::blake3(b"event-projection").as_str(), &timestamp],
                )?;
                connection.execute(
                    "INSERT INTO app_owner_notification_tombstones (
                         correlation_id, installation_id, installation_generation,
                         workflow_id, port_id, reviewed_request_digest, period_seconds,
                         effect_ref, payload_digest, severity, expires_at, created_at,
                         compacted_at
                     ) VALUES ('notification:item4', 'install_1', 1, 'daily_digest',
                               'owner_briefing', ?1, 3600, 'effect:item4', ?2,
                               'info', ?3, ?4, ?4)",
                    params![
                        AppDigest::blake3(b"notification-review").as_str(),
                        AppDigest::blake3(b"notification-payload").as_str(),
                        (seed_write_at + chrono::Duration::hours(1))
                            .to_rfc3339_opts(SecondsFormat::Micros, true),
                        &timestamp,
                    ],
                )?;
                connection.execute(
                    "INSERT INTO app_terminal_compaction_quarantine (
                         candidate_kind, installation_id, candidate_ref,
                         reason_code, quarantined_at
                     ) VALUES ('event_ingress', 'install_1', 'event:item4',
                               'integrity_verification_failed', ?1)",
                    params![&timestamp],
                )?;
                connection.execute(
                    "INSERT OR IGNORE INTO app_terminal_compaction_cursors (
                         candidate_kind, forward_pages_since_revisit, updated_at
                     ) VALUES ('event_ingress', 0, ?1)",
                    params![&timestamp],
                )?;
                Ok(())
            })
            .await
            .unwrap();

        let preview = service
            .preview_whole_installation(
                &authenticated,
                &installation_id,
                AppReference::parse("purge-preview:item4-terminal").unwrap(),
                seed_at,
                seed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap();
        let schedule_inventory = preview
            .inventory_entries
            .iter()
            .find(|entry| entry.target == AppPurgeTargetKind::SchedulesAndOutbox)
            .unwrap();
        assert_eq!(schedule_inventory.item_count, baseline_schedules + 3);

        let receipt = service
            .commit_whole_installation(
                &authenticated,
                &installation_id,
                request(&preview),
                seed_at + chrono::Duration::seconds(1),
            )
            .await
            .unwrap();
        let retained_schedule_outcome = serde_json::to_value(&receipt).unwrap()["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|outcome| outcome["target"] == "schedules_and_outbox")
            .unwrap()
            .clone();
        assert_eq!(retained_schedule_outcome["status"], "retained_shared");
        assert_eq!(
            retained_schedule_outcome["policy_or_ownership_ref"],
            "retention:user-request-notification-owner"
        );

        let remaining = registry
            .execute_scoped_read(
                &authenticated,
                &(seed_at + chrono::Duration::seconds(2)),
                |connection, _| {
                    let installation_rows: i64 = connection.query_row(
                        "SELECT
                             (SELECT COUNT(*) FROM app_event_ingress_tombstones
                               WHERE installation_id='install_1') +
                             (SELECT COUNT(*) FROM app_owner_notification_tombstones
                               WHERE installation_id='install_1') +
                             (SELECT COUNT(*) FROM app_terminal_compaction_quarantine
                               WHERE installation_id='install_1')",
                        [],
                        |row| row.get(0),
                    )?;
                    let shared_cursor: i64 = connection.query_row(
                        "SELECT COUNT(*) FROM app_terminal_compaction_cursors
                          WHERE candidate_kind='event_ingress'",
                        [],
                        |row| row.get(0),
                    )?;
                    Ok((installation_rows, shared_cursor))
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining, (0, 1));
    }

    #[tokio::test]
    async fn deleting_the_server_preview_proves_a_forged_or_lost_confirmation_unusable() {
        let (_temporary, registry, service, authenticated) = seeded_purge().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let observed_at = Utc::now();
        let preview = service
            .preview_whole_installation(
                &authenticated,
                &installation_id,
                AppReference::parse("purge-preview:test-2").unwrap(),
                observed_at,
                observed_at + chrono::Duration::minutes(5),
            )
            .await
            .unwrap();
        let preview_digest = preview.preview_digest.clone();
        registry
            .execute_scoped_test_write(
                &authenticated,
                &(observed_at + chrono::Duration::seconds(1)),
                move |connection, _| {
                    connection.execute(
                        "DELETE FROM app_purge_receipts WHERE preview_digest=?1",
                        params![preview_digest.as_str()],
                    )?;
                    Ok(())
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            service
                .commit_whole_installation(
                    &authenticated,
                    &installation_id,
                    request(&preview),
                    observed_at + chrono::Duration::seconds(2),
                )
                .await,
            Err(AppInstallationPurgeError::UnissuedPreview)
        ));
    }

    #[test]
    fn retained_targets_are_never_reported_as_erased() {
        for target in [
            AppPurgeTargetKind::DatabaseWalAndTemp,
            AppPurgeTargetKind::PackageAndCacheBytes,
            AppPurgeTargetKind::RetainedAttachments,
            AppPurgeTargetKind::ExportArchives,
            AppPurgeTargetKind::ArtifactV2References,
            AppPurgeTargetKind::SchedulesAndOutbox,
            AppPurgeTargetKind::MemoryCandidatesAndPromotions,
            AppPurgeTargetKind::Analytics,
            AppPurgeTargetKind::DebugAndPromptCaptures,
            AppPurgeTargetKind::EvaluationArtifacts,
        ] {
            let (status, evidence) = purge_status(target, 0, 0).unwrap();
            assert!(matches!(
                status,
                AppPurgeTargetStatus::RetainedShared | AppPurgeTargetStatus::RetainedByPolicy
            ));
            assert!(evidence.is_some());
        }
        assert_eq!(
            purge_status(AppPurgeTargetKind::ProviderSideContinuations, 0, 0)
                .unwrap()
                .0,
            AppPurgeTargetStatus::ProviderRetentionUnknown
        );
    }
}

//! Route-free Phase-2D record forget and retention owner.
//!
//! Record erasure plans relation effects before approval, repeats the exact
//! plan inside the commit transaction, uses SQLite secure-delete plus bounded
//! WAL checkpoints, and leaves an exact replayable receipt. Revision retention
//! operates only on non-head history and reports logical payload bytes and WAL
//! bytes separately through one canonical receipt.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    contribution::AppContributionError,
    entity_mutation::{
        advance_dataset_generation, apply_delete_policies, ensure_working_record, next_revision,
        reserve_change_sequences, to_sql_u64, validate_denied_reference_cycles,
        validate_reference_targets, write_record_revision, AppEntityMutationError, WorkingRecord,
    },
    entity_store::{resolve_data_owner_schema, ActiveAppEntitySchema, AppEntityStoreService},
    memory_store::{settle_candidates_for_committed_records, AppMemoryStoreError},
    models::{
        decode_app_contract, decode_bounded_json_value, AppContractError, AppContractLimits,
        AppDataClassification, AppDigest, AppInstallationId, AppName, AppRecordId, AppReference,
        AppRevision, ValidateAppContract,
    },
    records::{AppInstallation, AppRecordActorKind, AppRecordProvenance, AppScope},
    registry::{AppRegistryError, AppRegistryService},
    retention::{
        build_purge_receipt, decode_stored_purge_receipt, AppPurgeApproval, AppPurgeInventoryEntry,
        AppPurgeInventorySnapshot, AppPurgePreview, AppPurgeReceipt, AppPurgeSelection,
        AppPurgeTargetKind, AppPurgeTargetOutcome, AppPurgeTargetStatus, AppRetentionError,
        AppRetentionPolicy, AppRetentionRunReceipt,
    },
};

mod cleanup;
pub use cleanup::{
    AppDataCleanupEntity, AppDataCleanupJob, AppDataCleanupRequest, AppDataCleanupStatus,
};

#[derive(Debug, Clone)]
pub struct AppEntityRetentionService {
    store: AppEntityStoreService,
}

impl AppEntityRetentionService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            store: AppEntityStoreService::new(registry),
        }
    }

    /// Plan a hard per-record forget. The plan includes cascade and nullify
    /// effects and refuses restrict edges before any approval can be issued.
    #[allow(clippy::too_many_arguments)]
    pub async fn preview_forget_records(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        entity: AppName,
        record_ids: Vec<AppRecordId>,
        preview_ref: AppReference,
        observed_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<AppPurgePreview, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        self.store
            .registry
            .execute_scoped_write(
                authenticated_scope,
                &observed_at,
                move |connection, scope| {
                    Ok(preview_forget_blocking(
                        connection,
                        scope,
                        scope_binding_ref,
                        &installation_id,
                        entity,
                        record_ids,
                        preview_ref,
                        observed_at,
                        expires_at,
                    ))
                },
            )
            .await?
    }

    pub fn approve_forget(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        approval_ref: AppReference,
        preview: &AppPurgePreview,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<AppPurgeApproval, AppEntityRetentionStoreError> {
        Ok(AppPurgeApproval::from_reviewed_preview(
            authenticated_scope,
            approval_ref,
            preview,
            issued_at,
            expires_at,
            &AppContractLimits::default(),
        )?)
    }

    pub async fn commit_forget_records(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        preview: AppPurgePreview,
        approval: AppPurgeApproval,
        now: DateTime<Utc>,
    ) -> Result<AppPurgeReceipt, AppEntityRetentionStoreError> {
        let authenticated = authenticated_scope.clone();
        let receipt = self
            .store
            .registry
            .execute_scoped_write(authenticated_scope, &now, move |connection, scope| {
                Ok(commit_forget_blocking(
                    connection,
                    scope,
                    &authenticated,
                    preview,
                    approval,
                    now,
                ))
            })
            .await??;
        self.store
            .registry
            .tombstone_app_memory_index_projection_for_scope(
                authenticated_scope,
                &format!(
                    "record-forget:{}:{}:{}",
                    receipt.installation_id().as_str(),
                    receipt.installation_generation(),
                    receipt.selection_digest().as_str(),
                ),
            )
            .await?;
        Ok(receipt)
    }

    pub async fn activate_retention_policy(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        policy: AppRetentionPolicy,
        activated_at: DateTime<Utc>,
    ) -> Result<(), AppEntityRetentionStoreError> {
        policy.validate_app_contract(&AppContractLimits::default())?;
        let installation_id = installation_id.clone();
        self.store
            .registry
            .execute_scoped_write(
                authenticated_scope,
                &activated_at,
                move |connection, scope| {
                    Ok(activate_retention_policy_blocking(
                        connection,
                        scope,
                        &installation_id,
                        policy,
                        activated_at,
                    ))
                },
            )
            .await?
    }

    pub async fn run_retention(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        receipt_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppRetentionRunReceipt, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        self.store
            .registry
            .execute_scoped_background_write(authenticated_scope, &now, move |connection, scope| {
                Ok(run_retention_blocking(
                    connection,
                    scope,
                    scope_binding_ref,
                    &installation_id,
                    receipt_ref,
                    now,
                ))
            })
            .await?
    }
}

#[derive(Debug)]
struct ForgetPlan {
    working: BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    forgotten: BTreeSet<(AppName, AppRecordId)>,
    outbox_sequences: Vec<i64>,
    mutation_receipt_ids: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingForgetProgress {
    receipt_ref: AppReference,
    installation_id: AppInstallationId,
    approval_ref: AppReference,
    preview_digest: AppDigest,
    selection_digest: AppDigest,
    committed_at: DateTime<Utc>,
}

#[allow(clippy::too_many_arguments)]
fn preview_forget_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    scope_binding_ref: super::models::AppScopeBindingRef,
    installation_id: &AppInstallationId,
    entity: AppName,
    record_ids: Vec<AppRecordId>,
    preview_ref: AppReference,
    observed_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
) -> Result<AppPurgePreview, AppEntityRetentionStoreError> {
    ensure_clean_wal_boundary(connection)?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    let transaction = connection.transaction()?;
    let active = resolve_data_owner_schema(&transaction, scope, installation_id)?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    let plan = plan_forget(&transaction, &active, &entity, &record_ids)?;
    let entries = inventory_for_forget(&transaction, &active, &plan)?;
    let inventory = AppPurgeInventorySnapshot::from_storage_governance(
        scope_binding_ref,
        installation_id.clone(),
        active.installation_generation(),
        entries,
        observed_at,
        &AppContractLimits::default(),
    )?;
    let preview = AppPurgePreview::from_inventory(
        preview_ref,
        AppPurgeSelection::Records {
            entity_name: entity,
            record_ids,
        },
        &inventory,
        expires_at,
        &AppContractLimits::default(),
    )?;
    transaction.rollback()?;
    Ok(preview)
}

fn commit_forget_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    preview: AppPurgePreview,
    approval: AppPurgeApproval,
    now: DateTime<Utc>,
) -> Result<AppPurgeReceipt, AppEntityRetentionStoreError> {
    preview.validate_app_contract(&AppContractLimits::default())?;
    let (entity, record_ids) = match &preview.selection {
        AppPurgeSelection::Records {
            entity_name,
            record_ids,
        } => (entity_name.clone(), record_ids.clone()),
        AppPurgeSelection::WholeInstallation => {
            return Err(AppEntityRetentionStoreError::WholeInstallationOwnedByPhase6)
        },
    };
    ensure_clean_wal_boundary(connection)?;
    connection.pragma_update(None, "secure_delete", "ON")?;
    let receipt_ref = purge_receipt_ref(&preview.preview_digest)?;
    let existing = load_purge_row(connection, approval.approval_ref())?;
    if let Some(PurgeRow::Completed(receipt)) = existing {
        let installation = load_installation(connection, scope, &preview.installation_id)?;
        return build_purge_receipt(
            receipt_ref,
            &preview,
            &approval,
            authenticated_scope,
            &installation,
            Some(&receipt),
            Vec::new(),
            now,
            &AppContractLimits::default(),
        )
        .map_err(AppEntityRetentionStoreError::from);
    }
    if let Some(PurgeRow::Pending(progress)) = existing {
        let selection_digest =
            AppDigest::blake3_canonical_json(&serde_json::to_value(&preview.selection)?)?;
        if progress.receipt_ref != receipt_ref
            || progress.installation_id != preview.installation_id
            || progress.approval_ref != *approval.approval_ref()
            || progress.preview_digest != preview.preview_digest
            || progress.selection_digest != selection_digest
        {
            return Err(AppEntityRetentionStoreError::PurgeReplayCollision);
        }
        approval.ensure_matches_preview(
            authenticated_scope,
            &preview,
            &progress.committed_at,
            &AppContractLimits::default(),
        )?;
        ensure_clean_wal_boundary(connection)?;
        return finalize_forget_receipt(
            connection,
            scope,
            authenticated_scope,
            &preview,
            &approval,
            receipt_ref,
            progress.committed_at,
        );
    }

    approval.ensure_matches_preview(
        authenticated_scope,
        &preview,
        &now,
        &AppContractLimits::default(),
    )?;

    let transaction = connection.transaction()?;
    let active = resolve_data_owner_schema(&transaction, scope, &preview.installation_id)?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    if active.installation_generation() != preview.installation_generation {
        return Err(AppEntityRetentionStoreError::GenerationConflict);
    }
    let plan = plan_forget(&transaction, &active, &entity, &record_ids)?;
    let entries = inventory_for_forget(&transaction, &active, &plan)?;
    if entries != preview.inventory_entries {
        return Err(AppEntityRetentionStoreError::InventoryChanged);
    }
    apply_forget_plan(
        &transaction,
        &active,
        authenticated_scope,
        &plan,
        &receipt_ref,
        now,
    )?;
    let selection_digest =
        AppDigest::blake3_canonical_json(&serde_json::to_value(&preview.selection)?)?;
    let progress = PendingForgetProgress {
        receipt_ref: receipt_ref.clone(),
        installation_id: preview.installation_id.clone(),
        approval_ref: approval.approval_ref().clone(),
        preview_digest: preview.preview_digest.clone(),
        selection_digest: selection_digest.clone(),
        committed_at: now.clone(),
    };
    transaction.execute(
        "INSERT INTO app_purge_receipts (
             receipt_ref, installation_id, approval_ref, preview_digest,
             selection_digest, state, record_json, started_at, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending_checkpoint', ?6, ?7, NULL)",
        params![
            receipt_ref.as_str(),
            preview.installation_id.as_str(),
            approval.approval_ref().as_str(),
            preview.preview_digest.as_str(),
            selection_digest.as_str(),
            serde_json::to_vec(&progress)?,
            now.to_rfc3339(),
        ],
    )?;
    transaction.commit()?;
    ensure_clean_wal_boundary(connection)?;
    finalize_forget_receipt(
        connection,
        scope,
        authenticated_scope,
        &preview,
        &approval,
        receipt_ref,
        now,
    )
}

/// The shared erasure primitive for exact record forget and owner-approved
/// age cleanup. Data, lineage invalidations and rebuild notification commit together.
fn apply_forget_plan(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    authenticated_scope: &AuthenticatedAppScope,
    plan: &ForgetPlan,
    receipt_ref: &AppReference,
    now: DateTime<Utc>,
) -> Result<(), AppEntityRetentionStoreError> {
    let scope = authenticated_scope.scope();
    let generation =
        advance_dataset_generation(transaction, active.installation_id(), now.clone())?;
    let survivors = plan
        .working
        .iter()
        .filter(|(_, record)| !record.deleted)
        .map(|(key, record)| (key.clone(), record.clone()))
        .collect::<BTreeMap<_, _>>();
    for record in survivors.values() {
        validate_reference_targets(transaction, &active, record, &survivors)?;
    }
    validate_denied_reference_cycles(transaction, &active, &survivors)?;

    // Source invalidations and physical deletion share one SQLite commit. A
    // prompt/retrieval reader therefore sees either the still-live source or
    // the durable forget high-water, never a deleted record with no revocation
    // evidence. Include cascade-deleted records, not only the explicit roots.
    for (entity_name, record_id) in &plan.forgotten {
        let record = plan
            .working
            .get(&(entity_name.clone(), record_id.clone()))
            .ok_or(AppEntityRetentionStoreError::RecordNotFound)?;
        let source_event_revision = next_revision(record.prior_revision)?.get();
        super::memory_contribution_outbox::append_memory_record_forget_invalidations_in_transaction(
            transaction,
            scope,
            authenticated_scope.scope_binding_ref().as_str(),
            active.installation_id().as_str(),
            entity_name.as_str(),
            record_id.as_str(),
            source_event_revision,
            &now,
        )?;
        super::retrieval_contribution_outbox::append_retrieval_record_forget_invalidations_in_transaction(
            transaction,
            scope,
            authenticated_scope.scope_binding_ref().as_str(),
            active.installation_id().as_str(),
            entity_name.as_str(),
            record_id.as_str(),
            source_event_revision,
            &now,
        )?;
    }

    delete_forget_rows(transaction, active.installation_id(), &plan)?;
    let forgotten = plan.forgotten.iter().cloned().collect::<Vec<_>>();
    settle_candidates_for_committed_records(
        transaction,
        scope,
        active.installation_id(),
        &forgotten,
        now,
    )?;
    let sequence_count = survivors.len().max(1);
    let first_sequence =
        reserve_change_sequences(transaction, active.installation_id(), sequence_count)?;
    let mut sequence = first_sequence;
    let provenance = AppRecordProvenance {
        actor_kind: AppRecordActorKind::System,
        actor_id: AppReference::parse("system:app-record-forget")?,
        execution_id: None,
        output_revision: None,
        mutation_receipt_id: Some(receipt_ref.clone()),
        source_artifact_refs: Vec::new(),
        citation_refs: Vec::new(),
    };
    for record in survivors.values() {
        let revision = next_revision(record.prior_revision)?;
        write_record_revision(
            transaction,
            &active,
            record,
            revision,
            generation,
            sequence,
            &provenance,
            now.clone(),
        )?;
        sequence = sequence
            .checked_add(1)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    }
    let last_sequence = if survivors.is_empty() {
        first_sequence
    } else {
        sequence
            .checked_sub(1)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?
    };
    reconcile_storage_usage(transaction, active.installation_id(), now.clone())?;
    append_rebuild_outbox(
        transaction,
        active.installation_id(),
        &receipt_ref,
        generation,
        first_sequence,
        last_sequence,
        now.clone(),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn finalize_forget_receipt(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    preview: &AppPurgePreview,
    approval: &AppPurgeApproval,
    receipt_ref: AppReference,
    committed_at: DateTime<Utc>,
) -> Result<AppPurgeReceipt, AppEntityRetentionStoreError> {
    let installation = load_installation(connection, scope, &preview.installation_id)?;
    let outcomes = preview
        .inventory_entries
        .iter()
        .map(|entry| {
            let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                "target": entry.target,
                "status": AppPurgeTargetStatus::Deleted,
                "affected_items": entry.item_count,
                "affected_bytes": entry.byte_count,
            }))?;
            AppPurgeTargetOutcome::from_verified_local_settlement(
                entry.target,
                AppPurgeTargetStatus::Deleted,
                entry.item_count,
                entry.byte_count,
                digest,
                None,
                None,
            )
            .map_err(AppEntityRetentionStoreError::from)
        })
        .collect::<Result<Vec<_>, AppEntityRetentionStoreError>>()?;
    let receipt = build_purge_receipt(
        receipt_ref.clone(),
        preview,
        approval,
        authenticated_scope,
        &installation,
        None,
        outcomes,
        committed_at.clone(),
        &AppContractLimits::default(),
    )?;
    let transaction = connection.transaction()?;
    let updated = transaction.execute(
        "UPDATE app_purge_receipts
            SET state = 'completed', record_json = ?2, committed_at = ?3
          WHERE receipt_ref = ?1 AND state = 'pending_checkpoint'",
        params![
            receipt_ref.as_str(),
            serde_json::to_vec(&receipt)?,
            committed_at.to_rfc3339(),
        ],
    )?;
    if updated != 1 {
        return Err(AppEntityRetentionStoreError::PurgeReplayCollision);
    }
    transaction.commit()?;
    Ok(receipt)
}

fn plan_forget(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    entity: &AppName,
    record_ids: &[AppRecordId],
) -> Result<ForgetPlan, AppEntityRetentionStoreError> {
    if record_ids.is_empty()
        || record_ids.len() > AppContractLimits::default().max_collection_items()
        || record_ids.iter().collect::<BTreeSet<_>>().len() != record_ids.len()
    {
        return Err(AppEntityRetentionStoreError::InvalidSelection);
    }
    active
        .runtime_contract(entity)
        .ok_or_else(|| AppEntityRetentionStoreError::UnknownEntity(entity.to_string()))?;
    let mut working = BTreeMap::new();
    let mut initial = Vec::with_capacity(record_ids.len());
    for record_id in record_ids {
        ensure_working_record(
            transaction,
            active.installation_id(),
            &mut working,
            entity,
            record_id,
        )?;
        let record = working
            .get_mut(&(entity.clone(), record_id.clone()))
            .ok_or(AppEntityRetentionStoreError::RecordNotFound)?;
        if record.deleted {
            return Err(AppEntityRetentionStoreError::RecordAlreadyDeleted);
        }
        record.deleted = true;
        initial.push((entity.clone(), record_id.clone()));
    }
    apply_delete_policies(transaction, active, &mut working, initial)?;
    let forgotten = working
        .iter()
        .filter(|(_, record)| record.deleted)
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    let outbox_sequences = matching_json_rows(
        transaction,
        "SELECT CAST(sequence AS TEXT), payload_json FROM app_entity_outbox
         WHERE installation_id = ?1 ORDER BY sequence",
        active.installation_id(),
        &forgotten,
    )?
    .into_iter()
    .map(|(id, _)| {
        id.parse::<i64>()
            .map_err(|_| AppEntityRetentionStoreError::CorruptAuxiliaryRow)
    })
    .collect::<Result<Vec<_>, _>>()?;
    let mutation_receipt_ids = matching_json_rows(
        transaction,
        "SELECT receipt_id, record_json FROM app_mutation_receipts
         WHERE installation_id = ?1 ORDER BY receipt_id",
        active.installation_id(),
        &forgotten,
    )?
    .into_iter()
    .map(|(id, _)| id)
    .collect();
    Ok(ForgetPlan {
        working,
        forgotten,
        outbox_sequences,
        mutation_receipt_ids,
    })
}

fn inventory_for_forget(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    plan: &ForgetPlan,
) -> Result<Vec<AppPurgeInventoryEntry>, AppEntityRetentionStoreError> {
    let mut metrics = BTreeMap::<AppPurgeTargetKind, (u64, u64, AppDataClassification)>::new();
    metrics.insert(
        AppPurgeTargetKind::ActiveRows,
        (
            u64::try_from(plan.forgotten.len())
                .map_err(|_| AppEntityRetentionStoreError::CounterOverflow)?,
            0,
            maximum_plan_classification(plan),
        ),
    );
    let purge_keys = plan.working.keys().cloned().collect::<Vec<_>>();
    let (revision_items, revision_bytes) =
        sum_record_revisions(transaction, active.installation_id(), &purge_keys)?;
    metrics.insert(
        AppPurgeTargetKind::AppendOnlyRecordRevisions,
        (
            revision_items,
            revision_bytes,
            maximum_plan_classification(plan),
        ),
    );
    let (index_items, index_bytes) =
        sum_indexes(transaction, active.installation_id(), &purge_keys)?;
    metrics.insert(
        AppPurgeTargetKind::ScalarAndSearchIndexes,
        (index_items, index_bytes, maximum_plan_classification(plan)),
    );
    let cursor: (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(bytes), 0) FROM (
            SELECT length(evidence_json)+length(snapshot_json) AS bytes FROM app_query_cursors WHERE installation_id=?1
            UNION ALL SELECT length(evidence_json)+length(boundary_json) FROM app_keyset_cursors WHERE installation_id=?1)",
        params![active.installation_id().as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    metrics.insert(
        AppPurgeTargetKind::DirectoryAndSearchProjections,
        (
            nonnegative_u64(cursor.0)?,
            nonnegative_u64(cursor.1)?,
            AppDataClassification::Secret,
        ),
    );
    let outbox_bytes = sum_selected_row_bytes(
        transaction,
        "app_entity_outbox",
        "sequence",
        "payload_json",
        &plan
            .outbox_sequences
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    )?;
    let receipt_bytes = sum_selected_row_bytes(
        transaction,
        "app_mutation_receipts",
        "receipt_id",
        "record_json",
        &plan.mutation_receipt_ids,
    )?;
    metrics.insert(
        AppPurgeTargetKind::SchedulesAndOutbox,
        (
            u64::try_from(plan.outbox_sequences.len() + plan.mutation_receipt_ids.len())
                .map_err(|_| AppEntityRetentionStoreError::CounterOverflow)?,
            outbox_bytes
                .checked_add(receipt_bytes)
                .ok_or(AppEntityRetentionStoreError::CounterOverflow)?,
            // Auxiliary payloads can aggregate records above the selected
            // record's floor, so absence of a stored row label must narrow to
            // the most protective classification rather than guess.
            AppDataClassification::Secret,
        ),
    );

    AppPurgeTargetKind::ALL
        .into_iter()
        .map(|target| {
            let (item_count, byte_count, maximum_classification) = metrics
                .get(&target)
                .copied()
                .unwrap_or((0, 0, AppDataClassification::Ordinary));
            let inventory_digest = AppDigest::blake3_canonical_json(&serde_json::json!({
                "target": target,
                "item_count": item_count,
                "byte_count": byte_count,
                "maximum_classification": maximum_classification,
            }))?;
            Ok(AppPurgeInventoryEntry {
                target,
                item_count,
                byte_count,
                maximum_classification,
                inventory_digest,
            })
        })
        .collect()
}

fn delete_forget_rows(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    plan: &ForgetPlan,
) -> Result<(), AppEntityRetentionStoreError> {
    for (entity, record_id) in plan.working.keys() {
        transaction.execute(
            "DELETE FROM app_scalar_indexes
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
        )?;
        transaction.execute(
            "DELETE FROM app_text_search
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
        )?;
        transaction.execute(
            "DELETE FROM app_record_heads
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
        )?;
        transaction.execute(
            "DELETE FROM app_record_revisions
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
        )?;
    }
    for sequence in &plan.outbox_sequences {
        transaction.execute(
            "DELETE FROM app_entity_outbox WHERE sequence = ?1 AND installation_id = ?2",
            params![sequence, installation_id.as_str()],
        )?;
    }
    for receipt_id in &plan.mutation_receipt_ids {
        transaction.execute(
            "DELETE FROM app_mutation_receipts WHERE receipt_id = ?1 AND installation_id = ?2",
            params![receipt_id, installation_id.as_str()],
        )?;
    }
    transaction.execute(
        "DELETE FROM app_keyset_cursors WHERE installation_id = ?1",
        params![installation_id.as_str()],
    )?;
    transaction.execute(
        "DELETE FROM app_query_cursors WHERE installation_id = ?1",
        params![installation_id.as_str()],
    )?;
    Ok(())
}

fn append_rebuild_outbox(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    receipt_ref: &AppReference,
    dataset_generation: u64,
    first_sequence: u64,
    last_sequence: u64,
    now: DateTime<Utc>,
) -> Result<(), AppEntityRetentionStoreError> {
    #[derive(Serialize)]
    struct RebuildProjection<'a> {
        installation_id: &'a AppInstallationId,
        receipt_ref: &'a AppReference,
        dataset_generation: u64,
        rebuild_all: bool,
    }
    let event_id = AppReference::parse(format!("app-forget-rebuild:{receipt_ref}"))?;
    let payload = serde_json::to_vec(&RebuildProjection {
        installation_id,
        receipt_ref,
        dataset_generation,
        rebuild_all: true,
    })?;
    transaction.execute(
        "INSERT INTO app_entity_outbox (
             event_id, installation_id, first_change_seq, last_change_seq,
             payload_json, delivery_state, available_at, lease_token,
             lease_expires_at, created_at, delivered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL, NULL, ?6, NULL)",
        params![
            event_id.as_str(),
            installation_id.as_str(),
            to_sql_u64(first_sequence)?,
            to_sql_u64(last_sequence)?,
            payload,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn reconcile_storage_usage(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
) -> Result<(), AppEntityRetentionStoreError> {
    let record_count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM app_record_heads
         WHERE installation_id = ?1 AND deleted_at IS NULL",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    let (revision_count, payload_bytes): (i64, i64) = transaction.query_row(
        "SELECT COUNT(*), COALESCE(SUM(length(payload_json)), 0)
         FROM app_record_revisions WHERE installation_id = ?1",
        params![installation_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    transaction.execute(
        "INSERT INTO app_storage_usage (
             installation_id, record_count, revision_count, payload_bytes,
             attachment_bytes, updated_at
         ) VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(installation_id) DO UPDATE SET
             record_count = excluded.record_count,
             revision_count = excluded.revision_count,
             payload_bytes = excluded.payload_bytes,
             updated_at = excluded.updated_at",
        params![
            installation_id.as_str(),
            record_count,
            revision_count,
            payload_bytes,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn activate_retention_policy_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
    policy: AppRetentionPolicy,
    activated_at: DateTime<Utc>,
) -> Result<(), AppEntityRetentionStoreError> {
    let transaction = connection.transaction()?;
    resolve_data_owner_schema(&transaction, scope, installation_id)?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    let policy_revision = to_sql_revision(policy.policy_revision)?;
    let existing = transaction
        .query_row(
            "SELECT policy_digest, record_json FROM app_retention_policies
             WHERE installation_id = ?1 AND policy_revision = ?2",
            params![installation_id.as_str(), policy_revision],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
        )
        .optional()?;
    let bytes = serde_json::to_vec(&policy)?;
    if let Some((digest, stored)) = existing {
        if digest == policy.policy_digest.as_str() && stored == bytes {
            transaction.commit()?;
            return Ok(());
        }
        return Err(AppEntityRetentionStoreError::RetentionPolicyCollision);
    }
    let maximum: Option<i64> = transaction.query_row(
        "SELECT MAX(policy_revision) FROM app_retention_policies WHERE installation_id = ?1",
        params![installation_id.as_str()],
        |row| row.get(0),
    )?;
    if maximum.is_some_and(|maximum| maximum >= policy_revision) {
        return Err(AppEntityRetentionStoreError::RetentionPolicyCollision);
    }
    transaction.execute(
        "INSERT INTO app_retention_policies (
             installation_id, policy_revision, policy_digest, record_json, activated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            installation_id.as_str(),
            policy_revision,
            policy.policy_digest.as_str(),
            bytes,
            activated_at.to_rfc3339(),
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionProgress {
    receipt_ref: AppReference,
    scope_binding_ref: super::models::AppScopeBindingRef,
    policy_revision: AppRevision,
    policy_digest: AppDigest,
    examined_revision_items: u64,
    examined_revision_bytes: u64,
    deleted_revision_items: u64,
    deleted_revision_bytes: u64,
    retained_revision_items: u64,
    retained_revision_bytes: u64,
    started_at: DateTime<Utc>,
}

fn run_retention_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    scope_binding_ref: super::models::AppScopeBindingRef,
    installation_id: &AppInstallationId,
    receipt_ref: AppReference,
    now: DateTime<Utc>,
) -> Result<AppRetentionRunReceipt, AppEntityRetentionStoreError> {
    connection.pragma_update(None, "secure_delete", "ON")?;
    if let Some((
        stored_installation_id,
        stored_policy_revision,
        stored_policy_digest,
        state,
        bytes,
    )) = connection
        .query_row(
            "SELECT installation_id, policy_revision, policy_digest, state, record_json
             FROM app_retention_runs WHERE receipt_ref = ?1",
            params![receipt_ref.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?
    {
        if stored_installation_id != installation_id.as_str() {
            return Err(AppEntityRetentionStoreError::RetentionRunCollision);
        }
        if state == "completed" {
            let receipt: AppRetentionRunReceipt =
                decode_app_contract(&bytes, &AppContractLimits::default())?;
            if receipt.receipt_ref != receipt_ref
                || receipt.scope_binding_ref != scope_binding_ref
                || receipt.installation_id != *installation_id
                || to_sql_revision(receipt.policy_revision)? != stored_policy_revision
                || receipt.policy_digest.as_str() != stored_policy_digest.as_str()
            {
                return Err(AppEntityRetentionStoreError::RetentionRunCollision);
            }
            return Ok(receipt);
        }
        if state != "pending_checkpoint" {
            return Err(AppEntityRetentionStoreError::RetentionRunCollision);
        }
        let progress: RetentionProgress = decode_internal_json(&bytes)?;
        if progress.receipt_ref != receipt_ref
            || progress.scope_binding_ref != scope_binding_ref
            || to_sql_revision(progress.policy_revision)? != stored_policy_revision
            || progress.policy_digest.as_str() != stored_policy_digest.as_str()
        {
            return Err(AppEntityRetentionStoreError::RetentionRunCollision);
        }
        return finalize_retention_run(connection, installation_id, progress, now);
    }
    resolve_data_owner_schema(connection, scope, installation_id)?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    let policy = load_latest_retention_policy(connection, installation_id)?;
    // Establish a clean boundary so the receipt accounts only for WAL frames
    // generated by this retention transaction. The pending journal row is
    // committed with the pruning and makes checkpoint recovery idempotent.
    ensure_clean_wal_boundary(connection)?;
    let transaction = connection.transaction()?;
    let (examined_items, examined_bytes) =
        historical_revision_usage(&transaction, installation_id)?;
    let cutoff = now
        .checked_sub_signed(Duration::seconds(
            i64::try_from(policy.record_revisions.ceiling.max_age_seconds).unwrap_or(i64::MAX),
        ))
        .unwrap_or(DateTime::<Utc>::MIN_UTC);
    let (mut deleted_items, mut deleted_bytes) = prune_count_and_age(
        &transaction,
        installation_id,
        policy.record_revisions.max_revisions_per_record,
        cutoff,
    )?;
    let (byte_items, byte_bytes) = prune_to_revision_byte_ceiling(
        &transaction,
        installation_id,
        policy.record_revisions.ceiling.max_bytes,
    )?;
    deleted_items = deleted_items
        .checked_add(byte_items)
        .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    deleted_bytes = deleted_bytes
        .checked_add(byte_bytes)
        .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    let (retained_items, retained_bytes) =
        historical_revision_usage(&transaction, installation_id)?;
    if deleted_items.checked_add(retained_items) != Some(examined_items)
        || deleted_bytes.checked_add(retained_bytes) != Some(examined_bytes)
    {
        return Err(AppEntityRetentionStoreError::RetentionAccountingMismatch);
    }
    reconcile_storage_usage(&transaction, installation_id, now.clone())?;
    let progress = RetentionProgress {
        receipt_ref: receipt_ref.clone(),
        scope_binding_ref,
        policy_revision: policy.policy_revision,
        policy_digest: policy.policy_digest.clone(),
        examined_revision_items: examined_items,
        examined_revision_bytes: examined_bytes,
        deleted_revision_items: deleted_items,
        deleted_revision_bytes: deleted_bytes,
        retained_revision_items: retained_items,
        retained_revision_bytes: retained_bytes,
        started_at: now.clone(),
    };
    transaction.execute(
        "INSERT INTO app_retention_runs (
             receipt_ref, installation_id, policy_revision, policy_digest,
             state, record_json, started_at, completed_at
         ) VALUES (?1, ?2, ?3, ?4, 'pending_checkpoint', ?5, ?6, NULL)",
        params![
            receipt_ref.as_str(),
            installation_id.as_str(),
            to_sql_revision(policy.policy_revision)?,
            policy.policy_digest.as_str(),
            serde_json::to_vec(&progress)?,
            now.to_rfc3339(),
        ],
    )?;
    transaction.commit()?;
    finalize_retention_run(connection, installation_id, progress, now)
}

fn finalize_retention_run(
    connection: &mut Connection,
    installation_id: &AppInstallationId,
    progress: RetentionProgress,
    completed_at: DateTime<Utc>,
) -> Result<AppRetentionRunReceipt, AppEntityRetentionStoreError> {
    let wal_examined_bytes = wal_logical_bytes(connection, "PASSIVE")?;
    ensure_clean_wal_boundary(connection)?;
    let wal_examined_item = u64::from(wal_examined_bytes > 0);
    let receipt = AppRetentionRunReceipt {
        receipt_ref: progress.receipt_ref.clone(),
        scope_binding_ref: progress.scope_binding_ref,
        installation_id: installation_id.clone(),
        policy_revision: progress.policy_revision,
        policy_digest: progress.policy_digest,
        examined_items: progress
            .examined_revision_items
            .checked_add(wal_examined_item)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?,
        examined_bytes: progress
            .examined_revision_bytes
            .checked_add(wal_examined_bytes)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?,
        deleted_items: progress
            .deleted_revision_items
            .checked_add(wal_examined_item)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?,
        deleted_bytes: progress
            .deleted_revision_bytes
            .checked_add(wal_examined_bytes)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?,
        cryptographically_erased_items: 0,
        cryptographically_erased_bytes: 0,
        retained_items: progress.retained_revision_items,
        retained_bytes: progress.retained_revision_bytes,
        failed_items: 0,
        failed_bytes: 0,
        started_at: progress.started_at,
        completed_at,
    };
    receipt.validate_app_contract(&AppContractLimits::default())?;
    let transaction = connection.transaction()?;
    let updated = transaction.execute(
        "UPDATE app_retention_runs
            SET state = 'completed', record_json = ?3, completed_at = ?4
          WHERE receipt_ref = ?1 AND installation_id = ?2
            AND state = 'pending_checkpoint'",
        params![
            receipt.receipt_ref.as_str(),
            installation_id.as_str(),
            serde_json::to_vec(&receipt)?,
            receipt.completed_at.to_rfc3339(),
        ],
    )?;
    if updated != 1 {
        return Err(AppEntityRetentionStoreError::RetentionRunCollision);
    }
    transaction.commit()?;
    Ok(receipt)
}

fn load_latest_retention_policy(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<AppRetentionPolicy, AppEntityRetentionStoreError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_retention_policies
             WHERE installation_id = ?1 ORDER BY policy_revision DESC LIMIT 1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityRetentionStoreError::MissingRetentionPolicy)?;
    Ok(decode_app_contract(&bytes, &AppContractLimits::default())?)
}

fn historical_revision_usage(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<(u64, u64), AppEntityRetentionStoreError> {
    let values: (i64, i64) = connection.query_row(
        "SELECT COUNT(*), COALESCE(SUM(
             length(r.payload_json) + length(r.handling_policy_json) + length(r.provenance_json)
         ), 0)
         FROM app_record_revisions r
         LEFT JOIN app_record_heads h
           ON h.installation_id = r.installation_id
          AND h.entity_name = r.entity_name
          AND h.record_id = r.record_id
          AND h.record_revision = r.record_revision
         WHERE r.installation_id = ?1 AND h.record_id IS NULL",
        params![installation_id.as_str()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    Ok((nonnegative_u64(values.0)?, nonnegative_u64(values.1)?))
}

fn prune_count_and_age(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    max_revisions_per_record: u32,
    cutoff: DateTime<Utc>,
) -> Result<(u64, u64), AppEntityRetentionStoreError> {
    let mut statement = transaction.prepare(
        "WITH ranked AS (
             SELECT r.rowid AS revision_rowid,
                    ROW_NUMBER() OVER (
                        PARTITION BY r.entity_name, r.record_id
                        ORDER BY r.record_revision DESC
                    ) AS revision_rank,
                    r.updated_at,
                    EXISTS (
                        SELECT 1 FROM app_record_heads h
                         WHERE h.installation_id = r.installation_id
                           AND h.entity_name = r.entity_name
                           AND h.record_id = r.record_id
                           AND h.record_revision = r.record_revision
                    ) AS is_head
             FROM app_record_revisions r
             WHERE r.installation_id = ?1
         )
         DELETE FROM app_record_revisions
          WHERE rowid IN (
              SELECT ranked.revision_rowid FROM ranked
              WHERE ranked.is_head = 0
                AND (ranked.revision_rank > ?2 OR ranked.updated_at < ?3)
          )
         RETURNING length(payload_json) + length(handling_policy_json) + length(provenance_json)",
    )?;
    let rows = statement.query_map(
        params![
            installation_id.as_str(),
            i64::from(max_revisions_per_record),
            cutoff.to_rfc3339(),
        ],
        |row| row.get::<_, i64>(0),
    )?;
    sum_returned_sizes(rows)
}

fn prune_to_revision_byte_ceiling(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    ceiling: u64,
) -> Result<(u64, u64), AppEntityRetentionStoreError> {
    let (_, current_bytes) = historical_revision_usage(transaction, installation_id)?;
    let excess = current_bytes.saturating_sub(ceiling);
    if excess == 0 {
        return Ok((0, 0));
    }
    let excess =
        i64::try_from(excess).map_err(|_| AppEntityRetentionStoreError::CounterOverflow)?;
    let mut statement = transaction.prepare(
        "WITH candidates AS (
             SELECT r.rowid AS revision_rowid,
                    length(r.payload_json) + length(r.handling_policy_json) + \
         length(r.provenance_json) AS byte_len,
                    SUM(length(r.payload_json) + length(r.handling_policy_json) + \
         length(r.provenance_json))
                      OVER (ORDER BY r.updated_at ASC, r.entity_name ASC,
                                     r.record_id ASC, r.record_revision ASC) AS running_bytes
             FROM app_record_revisions r
             LEFT JOIN app_record_heads h
               ON h.installation_id = r.installation_id
              AND h.entity_name = r.entity_name
              AND h.record_id = r.record_id
              AND h.record_revision = r.record_revision
             WHERE r.installation_id = ?1 AND h.record_id IS NULL
         )
         DELETE FROM app_record_revisions
          WHERE rowid IN (
              SELECT revision_rowid FROM candidates
               WHERE running_bytes - byte_len < ?2
          )
         RETURNING length(payload_json) + length(handling_policy_json) + length(provenance_json)",
    )?;
    let rows = statement.query_map(params![installation_id.as_str(), excess], |row| {
        row.get::<_, i64>(0)
    })?;
    sum_returned_sizes(rows)
}

enum PurgeRow {
    Pending(PendingForgetProgress),
    Completed(AppPurgeReceipt),
}

fn load_purge_row(
    connection: &Connection,
    approval_ref: &AppReference,
) -> Result<Option<PurgeRow>, AppEntityRetentionStoreError> {
    let row = connection
        .query_row(
            "SELECT receipt_ref, installation_id, preview_digest, selection_digest,
                    state, record_json
             FROM app_purge_receipts WHERE approval_ref = ?1",
            params![approval_ref.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                ))
            },
        )
        .optional()?;
    row.map(
        |(
            stored_receipt_ref,
            stored_installation_id,
            stored_preview_digest,
            stored_selection_digest,
            state,
            bytes,
        )| match state.as_str() {
            "pending_checkpoint" => {
                let progress: PendingForgetProgress = decode_internal_json(&bytes)?;
                if stored_receipt_ref != progress.receipt_ref.as_str()
                    || stored_installation_id != progress.installation_id.as_str()
                    || stored_preview_digest != progress.preview_digest.as_str()
                    || stored_selection_digest != progress.selection_digest.as_str()
                    || progress.approval_ref != *approval_ref
                {
                    return Err(AppEntityRetentionStoreError::CorruptPurgeEvidence);
                }
                Ok(PurgeRow::Pending(progress))
            },
            "completed" => {
                let receipt = decode_stored_purge_receipt(&bytes, &AppContractLimits::default())?;
                if stored_receipt_ref != receipt.receipt_ref().as_str()
                    || stored_installation_id != receipt.installation_id().as_str()
                    || stored_preview_digest != receipt.preview_digest().as_str()
                    || stored_selection_digest != receipt.selection_digest().as_str()
                    || receipt.approval_ref() != approval_ref
                {
                    return Err(AppEntityRetentionStoreError::CorruptPurgeEvidence);
                }
                Ok(PurgeRow::Completed(receipt))
            },
            _ => Err(AppEntityRetentionStoreError::CorruptPurgeEvidence),
        },
    )
    .transpose()
}

fn load_installation(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<AppInstallation, AppEntityRetentionStoreError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_installations WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    let installation: AppInstallation = decode_app_contract(&bytes, &AppContractLimits::default())?;
    if installation.scope != *scope || installation.installation_id != *installation_id {
        return Err(AppEntityRetentionStoreError::ScopeMismatch);
    }
    Ok(installation)
}

fn matching_json_rows(
    connection: &Connection,
    sql: &'static str,
    installation_id: &AppInstallationId,
    records: &BTreeSet<(AppName, AppRecordId)>,
) -> Result<Vec<(String, usize)>, AppEntityRetentionStoreError> {
    let mut statement = connection.prepare(sql)?;
    let rows = statement.query_map(params![installation_id.as_str()], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
    })?;
    let mut matches = Vec::new();
    for row in rows {
        // Decode one bounded receipt at a time. Total historical receipt count
        // must not prevent an owner from erasing old records.
        let (id, bytes) = row?;
        let value = decode_bounded_json_value(&bytes, &AppContractLimits::default())?;
        if json_contains_any_record_identity(&value, records) {
            matches.push((id, bytes.len()));
        }
    }
    Ok(matches)
}

fn json_contains_any_record_identity(
    value: &Value,
    records: &BTreeSet<(AppName, AppRecordId)>,
) -> bool {
    let mut stack = vec![value];
    while let Some(value) = stack.pop() {
        match value {
            Value::Array(values) => stack.extend(values.iter()),
            Value::Object(values) => {
                let identity = values
                    .get("entity")
                    .and_then(Value::as_str)
                    .zip(values.get("record_id").and_then(Value::as_str))
                    .and_then(|(entity, record_id)| {
                        Some((
                            AppName::parse(entity.to_owned()).ok()?,
                            AppRecordId::parse(record_id.to_owned()).ok()?,
                        ))
                    });
                if identity.is_some_and(|identity| records.contains(&identity)) {
                    return true;
                }
                stack.extend(values.values());
            },
            _ => {},
        }
    }
    false
}

fn sum_record_revisions(
    connection: &Connection,
    installation_id: &AppInstallationId,
    keys: &[(AppName, AppRecordId)],
) -> Result<(u64, u64), AppEntityRetentionStoreError> {
    let mut items = 0u64;
    let mut bytes = 0u64;
    for (entity, record_id) in keys {
        let values: (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(
                 length(payload_json) + length(handling_policy_json) + length(provenance_json)
             ), 0)
             FROM app_record_revisions
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        items = items
            .checked_add(nonnegative_u64(values.0)?)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
        bytes = bytes
            .checked_add(nonnegative_u64(values.1)?)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    }
    Ok((items, bytes))
}

fn sum_indexes(
    connection: &Connection,
    installation_id: &AppInstallationId,
    keys: &[(AppName, AppRecordId)],
) -> Result<(u64, u64), AppEntityRetentionStoreError> {
    let mut items = 0u64;
    let mut bytes = 0u64;
    for (entity, record_id) in keys {
        let scalar: (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(
                 length(COALESCE(text_value, '')) + COALESCE(length(order_key_asc),0) + COALESCE(length(order_key_desc),0) + CASE WHEN integer_value IS NULL THEN 0 ELSE 8 \
             END
             ), 0)
             FROM app_scalar_indexes
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let text: (i64, i64) = connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(search_text)), 0)
             FROM app_text_search
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let scalar_items = nonnegative_u64(scalar.0)?;
        let text_items = nonnegative_u64(text.0)?;
        let scalar_bytes = nonnegative_u64(scalar.1)?;
        let text_bytes = nonnegative_u64(text.1)?;
        items = items
            .checked_add(scalar_items)
            .and_then(|value| value.checked_add(text_items))
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
        bytes = bytes
            .checked_add(scalar_bytes)
            .and_then(|value| value.checked_add(text_bytes))
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    }
    Ok((items, bytes))
}

fn sum_selected_row_bytes(
    connection: &Connection,
    table: &'static str,
    id_column: &'static str,
    bytes_column: &'static str,
    ids: &[String],
) -> Result<u64, AppEntityRetentionStoreError> {
    let sql = match (table, id_column, bytes_column) {
        ("app_entity_outbox", "sequence", "payload_json") => {
            "SELECT length(payload_json) FROM app_entity_outbox WHERE sequence = ?1"
        },
        ("app_mutation_receipts", "receipt_id", "record_json") => {
            "SELECT length(record_json) FROM app_mutation_receipts WHERE receipt_id = ?1"
        },
        _ => return Err(AppEntityRetentionStoreError::CorruptAuxiliaryRow),
    };
    let mut total = 0u64;
    for id in ids {
        let bytes: i64 = connection.query_row(sql, params![id], |row| row.get(0))?;
        total = total
            .checked_add(nonnegative_u64(bytes)?)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    }
    Ok(total)
}

fn maximum_plan_classification(plan: &ForgetPlan) -> AppDataClassification {
    plan.working
        .values()
        .map(|record| record.handling_policy.classification_floor)
        .max()
        .unwrap_or(AppDataClassification::Ordinary)
}

fn ensure_clean_wal_boundary(connection: &Connection) -> Result<(), AppEntityRetentionStoreError> {
    let remaining = wal_logical_bytes(connection, "TRUNCATE")?;
    if remaining != 0 {
        return Err(AppEntityRetentionStoreError::WalCheckpointBusy);
    }
    Ok(())
}

fn wal_logical_bytes(
    connection: &Connection,
    mode: &'static str,
) -> Result<u64, AppEntityRetentionStoreError> {
    let sql = match mode {
        "PASSIVE" => "PRAGMA wal_checkpoint(PASSIVE)",
        "TRUNCATE" => "PRAGMA wal_checkpoint(TRUNCATE)",
        _ => return Err(AppEntityRetentionStoreError::WalCheckpointBusy),
    };
    let (busy, log_frames, _checkpointed): (i64, i64, i64) =
        connection.query_row(sql, [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    if busy != 0 || log_frames < 0 {
        return Err(AppEntityRetentionStoreError::WalCheckpointBusy);
    }
    if log_frames == 0 {
        return Ok(0);
    }
    let page_size: i64 = connection.query_row("PRAGMA page_size", [], |row| row.get(0))?;
    let frame_bytes = nonnegative_u64(page_size)?
        .checked_add(24)
        .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    nonnegative_u64(log_frames)?
        .checked_mul(frame_bytes)
        .and_then(|bytes| bytes.checked_add(32))
        .ok_or(AppEntityRetentionStoreError::CounterOverflow)
}

fn purge_receipt_ref(digest: &AppDigest) -> Result<AppReference, AppEntityRetentionStoreError> {
    AppReference::parse(format!(
        "app-purge-receipt:{}",
        digest
            .as_str()
            .strip_prefix("blake3:")
            .unwrap_or(digest.as_str())
    ))
    .map_err(AppEntityRetentionStoreError::from)
}

fn decode_internal_json<T: for<'de> Deserialize<'de>>(
    bytes: &[u8],
) -> Result<T, AppEntityRetentionStoreError> {
    let value = decode_bounded_json_value(bytes, &AppContractLimits::default())?;
    serde_json::from_value(value).map_err(AppEntityRetentionStoreError::from)
}

fn sum_returned_sizes<I>(rows: I) -> Result<(u64, u64), AppEntityRetentionStoreError>
where
    I: Iterator<Item = rusqlite::Result<i64>>,
{
    let mut items = 0u64;
    let mut bytes = 0u64;
    for row in rows {
        items = items
            .checked_add(1)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
        bytes = bytes
            .checked_add(nonnegative_u64(row?)?)
            .ok_or(AppEntityRetentionStoreError::CounterOverflow)?;
    }
    Ok((items, bytes))
}

fn nonnegative_u64(value: i64) -> Result<u64, AppEntityRetentionStoreError> {
    u64::try_from(value).map_err(|_| AppEntityRetentionStoreError::CorruptAccounting)
}

fn to_sql_revision(value: AppRevision) -> Result<i64, AppEntityRetentionStoreError> {
    i64::try_from(value.get()).map_err(|_| AppEntityRetentionStoreError::CounterOverflow)
}

#[derive(Debug, Error)]
pub enum AppEntityRetentionStoreError {
    #[error("app retention registry failed: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("app retention SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid app retention contract: {0}")]
    Contract(#[from] AppContractError),
    #[error("app retention protocol failed: {0}")]
    Retention(#[from] AppRetentionError),
    #[error("app entity mutation failed: {0}")]
    Mutation(#[from] AppEntityMutationError),
    #[error("app-memory settlement failed: {0}")]
    Memory(#[from] AppMemoryStoreError),
    #[error("app contribution invalidation failed: {0}")]
    Contribution(#[from] AppContributionError),
    #[error("app entity store failed: {0}")]
    Store(#[from] super::entity_store::AppEntityStoreError),
    #[error("failed to encode app retention evidence: {0}")]
    Json(#[from] serde_json::Error),
    #[error("app installation does not exist in the authenticated scope")]
    MissingInstallation,
    #[error("app installation belongs to another scope")]
    ScopeMismatch,
    #[error("whole-installation purge remains owned by the Phase-6 multi-store coordinator")]
    WholeInstallationOwnedByPhase6,
    #[error("record-forget selection is empty, duplicated or too large")]
    InvalidSelection,
    #[error("record-forget targets unknown entity `{0}`")]
    UnknownEntity(String),
    #[error("record-forget target does not exist")]
    RecordNotFound,
    #[error("record-forget target is already deleted")]
    RecordAlreadyDeleted,
    #[error("record-forget installation generation changed")]
    GenerationConflict,
    #[error("record-forget inventory changed after approval")]
    InventoryChanged,
    #[error("record-forget replay collided with different durable evidence")]
    PurgeReplayCollision,
    #[error("stored purge evidence is corrupt")]
    CorruptPurgeEvidence,
    #[error("auxiliary app row is corrupt")]
    CorruptAuxiliaryRow,
    #[error("record-forget auxiliary receipt scan exceeds its fixed ceiling")]
    AuxiliaryScanLimit,
    #[error("retention policy revision collides or moves backwards")]
    RetentionPolicyCollision,
    #[error("app has no active retention policy")]
    MissingRetentionPolicy,
    #[error("retention run identity collided")]
    RetentionRunCollision,
    #[error("retention counters do not settle the examined history")]
    RetentionAccountingMismatch,
    #[error("SQLite WAL checkpoint is busy or incomplete")]
    WalCheckpointBusy,
    #[error("stored retention accounting is corrupt")]
    CorruptAccounting,
    #[error("app retention counter overflow")]
    CounterOverflow,
    #[error("app data cleanup was not found in this workspace")]
    CleanupNotFound,
    #[error("app data cleanup is not in the required state")]
    CleanupInvalidState,
    #[error("app data cleanup preview expired; create a fresh preview")]
    CleanupPreviewExpired,
    #[error("app data cleanup confirmation does not match the preview")]
    CleanupConfirmationMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::{
        apps::{
            entity_store::tests::{compiled_schema, seed_enabled_installation, seed_records},
            lifecycle::AppInstallationStatus,
            registry::tests::{authenticated_scope, canonical_tempdir, time},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    async fn seeded_retention() -> (
        tempfile::TempDir,
        AppRegistryService,
        AppEntityRetentionService,
        AuthenticatedAppScope,
    ) {
        let temporary = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(temporary.path()));
        let (schema_digest, schema) = compiled_schema();
        seed_enabled_installation(
            &registry,
            schema_digest,
            schema,
            AppInstallationStatus::Enabled,
        )
        .await;
        seed_records(&registry).await;
        let service = AppEntityRetentionService::new(registry.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        (temporary, registry, service, authenticated)
    }

    #[tokio::test]
    async fn record_forget_is_approved_exactly_hard_deletes_history_and_replays() {
        let (_temporary, registry, service, authenticated) = seeded_retention().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let preview = service
            .preview_forget_records(
                &authenticated,
                &installation_id,
                AppName::parse("item").unwrap(),
                vec![AppRecordId::parse("record_b").unwrap()],
                AppReference::parse("preview:forget-record-b").unwrap(),
                time(4),
                time(15),
            )
            .await
            .unwrap();
        let approval = service
            .approve_forget(
                &authenticated,
                AppReference::parse("approval:forget-record-b").unwrap(),
                &preview,
                time(5),
                time(20),
            )
            .unwrap();
        let receipt = service
            .commit_forget_records(&authenticated, preview.clone(), approval.clone(), time(6))
            .await
            .unwrap();
        let replay = service
            .commit_forget_records(&authenticated, preview, approval, time(7))
            .await
            .unwrap();
        assert_eq!(replay.receipt_ref(), receipt.receipt_ref());
        assert_eq!(replay.committed_at(), receipt.committed_at());

        let persisted = registry
            .execute_scoped_read(&authenticated, &time(8), |connection, _| {
                let heads = connection.query_row(
                    "SELECT COUNT(*) FROM app_record_heads
                     WHERE installation_id = 'install_1' AND record_id = 'record_b'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let revisions = connection.query_row(
                    "SELECT COUNT(*) FROM app_record_revisions
                     WHERE installation_id = 'install_1' AND record_id = 'record_b'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let outbox_payloads = connection
                    .prepare(
                        "SELECT payload_json FROM app_entity_outbox
                         WHERE installation_id = 'install_1'",
                    )?
                    .query_map([], |row| row.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                let receipt_evidence = connection.query_row(
                    "SELECT state, record_json FROM app_purge_receipts
                     WHERE approval_ref = 'approval:forget-record-b'",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
                )?;
                Ok((heads, revisions, outbox_payloads, receipt_evidence))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!((persisted.0, persisted.1), (0, 0));
        assert_eq!(persisted.3 .0, "completed");
        assert!(
            !String::from_utf8_lossy(&persisted.3 .1).contains("record_b"),
            "durable forget evidence must retain only the reviewed selection digest, not raw \
             record IDs"
        );
        assert!(persisted
            .2
            .iter()
            .all(|payload| { !String::from_utf8_lossy(payload).contains("record_b") }));
    }

    #[test]
    fn auxiliary_forget_matching_requires_typed_record_identity() {
        let selected = BTreeSet::from([(
            AppName::parse("item").unwrap(),
            AppRecordId::parse("record_a").unwrap(),
        )]);

        assert!(!json_contains_any_record_identity(
            &serde_json::json!({"title": "record_a"}),
            &selected,
        ));
        assert!(!json_contains_any_record_identity(
            &serde_json::json!({
                "committed_record_revisions": [{
                    "entity": "another_entity",
                    "record_id": "record_a"
                }]
            }),
            &selected,
        ));
        assert!(json_contains_any_record_identity(
            &serde_json::json!({
                "committed_record_revisions": [{
                    "entity": "item",
                    "record_id": "record_a"
                }]
            }),
            &selected,
        ));
    }

    #[tokio::test]
    async fn restrict_edges_block_forget_before_approval_or_write() {
        let (_temporary, _registry, service, authenticated) = seeded_retention().await;
        let error = service
            .preview_forget_records(
                &authenticated,
                &AppInstallationId::parse("install_1").unwrap(),
                AppName::parse("item").unwrap(),
                vec![AppRecordId::parse("record_a").unwrap()],
                AppReference::parse("preview:restricted-record-a").unwrap(),
                time(4),
                time(15),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppEntityRetentionStoreError::Mutation(
                AppEntityMutationError::ReferencedRecordDeleteDenied
            )
        ));
    }

    #[test]
    fn revision_pruning_never_deletes_the_current_head() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE app_record_revisions (
                     installation_id TEXT NOT NULL,
                     entity_name TEXT NOT NULL,
                     record_id TEXT NOT NULL,
                     record_revision INTEGER NOT NULL,
                     payload_json BLOB NOT NULL,
                     handling_policy_json BLOB NOT NULL,
                     provenance_json BLOB NOT NULL,
                     updated_at TEXT NOT NULL
                 );
                 CREATE TABLE app_record_heads (
                     installation_id TEXT NOT NULL,
                     entity_name TEXT NOT NULL,
                     record_id TEXT NOT NULL,
                     record_revision INTEGER NOT NULL
                 );
                 INSERT INTO app_record_revisions VALUES
                     ('install_1', 'item', 'record_a', 1, '{}', '{}', '{}', \
                 '2026-08-15T00:00:01Z'),
                     ('install_1', 'item', 'record_a', 2, '{}', '{}', '{}', \
                 '2026-08-15T00:00:02Z'),
                     ('install_1', 'item', 'record_a', 3, '{}', '{}', '{}', \
                 '2026-08-15T00:00:03Z');
                 INSERT INTO app_record_heads VALUES ('install_1', 'item', 'record_a', 3);",
            )
            .unwrap();
        let transaction = connection.transaction().unwrap();
        let deleted = prune_count_and_age(
            &transaction,
            &AppInstallationId::parse("install_1").unwrap(),
            1,
            DateTime::<Utc>::MIN_UTC,
        )
        .unwrap();
        assert_eq!(deleted, (2, 12));
        let remaining = transaction
            .query_row(
                "SELECT group_concat(record_revision, ',') FROM app_record_revisions",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap();
        assert_eq!(remaining, "3");
    }
}

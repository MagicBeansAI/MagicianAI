//! Transactional Phase-2C mutation owner for the generic app entity store.
//!
//! This module owns no connection pool or path resolution. It executes only on
//! the registry's serialized bounded blocking lane and commits record
//! revisions, heads, typed indexes, usage, receipts, sequences and one local
//! outbox row in a single SQLite transaction.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    boundary::{AppBoundaryError, AppStoreAuthorityFence},
    entity_index::{normalized_search_text, project_scalar_index, AppEntityIndexError},
    entity_store::{
        resolve_active_schema, ActiveAppEntitySchema, AppEntityStoreError, AppEntityStoreService,
    },
    manifest::{AppReferenceCyclePolicy, AppReferenceDeletePolicy},
    memory_store::{settle_candidates_for_committed_records, AppMemoryStoreError},
    models::{
        AppContractError, AppContractLimits, AppDigest, AppFieldPath, AppInstallationId,
        AppMutationCommand, AppMutationOperation, AppName, AppRecordId, AppReference, AppRevision,
        ValidateAppContract,
    },
    records::{
        validate_policy, AppChangeSequenceRange, AppCommittedRecordRevision, AppDataHandlingPolicy,
        AppMutationOrigin, AppMutationReceipt, AppRecordActorKind, AppRecordProvenance,
        AppRecordRevision, AppScope,
    },
    registry::AppRegistryError,
    schema_compiler::{AppEntityRuntimeContract, AppSchemaCompilerError},
};

/// Conservative physical record-write bound owned by the transaction planner.
/// Deletes may expand through reference policies; all other operations name
/// one record or two relation endpoints. The transaction itself enforces the
/// same final working-set ceiling before any record revisions are written.
pub(crate) fn mutation_record_write_upper_bound(command: &AppMutationCommand) -> u64 {
    let ceiling = AppContractLimits::default().max_collection_items();
    if command
        .operations
        .iter()
        .any(|operation| matches!(operation, AppMutationOperation::Delete { .. }))
    {
        return ceiling as u64;
    }
    command
        .operations
        .iter()
        .map(|operation| match operation {
            AppMutationOperation::CreateRelation { .. }
            | AppMutationOperation::DeleteRelation { .. } => 2usize,
            AppMutationOperation::Create { .. }
            | AppMutationOperation::Update { .. }
            | AppMutationOperation::Restore { .. } => 1,
            AppMutationOperation::Delete { .. } => ceiling,
        })
        .sum::<usize>()
        .min(ceiling) as u64
}

impl AppEntityStoreService {
    /// Apply one all-or-nothing mutation command. The caller supplies only the
    /// move-only fence; its trusted origin and server-derived logical mutation
    /// key are consumed inside the same current-authority check.
    pub async fn mutate(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        authority: AppStoreAuthorityFence,
        command: AppMutationCommand,
        now: DateTime<Utc>,
    ) -> Result<AppMutationReceipt, AppEntityMutationError> {
        let scope_binding_ref = authenticated_scope.scope_binding_ref().clone();
        let authentication_revision = authenticated_scope.authentication_revision();
        let result = self
            .registry
            .execute_scoped_write(authenticated_scope, &now, move |connection, scope| {
                Ok(mutate_blocking(
                    connection,
                    scope,
                    &scope_binding_ref,
                    authentication_revision,
                    authority,
                    command,
                    now,
                ))
            })
            .await?;
        let receipt = result?;
        self.registry
            .tombstone_app_memory_index_projection_for_scope(
                authenticated_scope,
                &format!(
                    "entity-mutation:{}:{}:{}",
                    receipt.installation_id.as_str(),
                    receipt.receipt_id.as_str(),
                    receipt.batch_digest.as_str(),
                ),
            )
            .await?;
        Ok(receipt)
    }

    /// Read and fully verify the immutable receipt for one exact mutation
    /// identity without opening a write transaction or replaying the mutation.
    /// This is the only safe evidence source for crash recovery after the
    /// entity transaction may already have committed.
    pub async fn verified_mutation_receipt(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: AppInstallationId,
        mutation_key: AppDigest,
        batch_digest: AppDigest,
        origin: AppMutationOrigin,
        now: DateTime<Utc>,
    ) -> Result<Option<AppMutationReceipt>, AppEntityMutationError> {
        let result = self
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, _scope| {
                Ok(load_replay(
                    connection,
                    &installation_id,
                    &mutation_key,
                    &batch_digest,
                    &origin,
                ))
            })
            .await?;
        match result {
            None => Ok(None),
            Some(receipt) => receipt,
        }
    }
}

#[derive(Debug, Clone)]
pub struct WorkingRecord {
    pub entity: AppName,
    pub record_id: AppRecordId,
    pub prior_revision: Option<AppRevision>,
    pub created_at: DateTime<Utc>,
    pub payload: Value,
    pub handling_policy: AppDataHandlingPolicy,
    pub was_deleted: bool,
    pub deleted: bool,
}

fn mutate_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    scope_binding_ref: &super::models::AppScopeBindingRef,
    authentication_revision: AppRevision,
    authority: AppStoreAuthorityFence,
    command: AppMutationCommand,
    now: DateTime<Utc>,
) -> Result<AppMutationReceipt, AppEntityMutationError> {
    command.validate_app_contract(&AppContractLimits::default())?;
    let transaction = connection.transaction()?;
    let installation_id = authority.installation_id().clone();
    let active = resolve_active_schema(&transaction, scope, &installation_id)
        .map_err(AppEntityMutationError::Store)?
        .ok_or(AppEntityMutationError::MissingInstallation)?;
    let consumed = authority.consume_for_active_mutation(
        &command,
        scope_binding_ref,
        authentication_revision,
        active.installation_id(),
        active.installation_generation(),
        active.package_revision_ref(),
        active.grant_revision(),
        active.schema_revision(),
        active.active_surface_revision(),
    )?;
    let (origin, mutation_key) = consumed.into_parts();
    if command.expected_schema_revision != active.schema_revision() {
        return Err(AppEntityMutationError::SchemaRevisionConflict);
    }
    let batch_digest = AppDigest::blake3_canonical_json(&serde_json::to_value(&command)?)?;
    if let Some(receipt) = load_replay(
        &transaction,
        active.installation_id(),
        &mutation_key,
        &batch_digest,
        &origin,
    )? {
        transaction.commit()?;
        return Ok(receipt);
    }

    let expected = command
        .expected_record_revisions
        .iter()
        .map(|record| {
            (
                (record.entity.clone(), record.record_id.clone()),
                record.revision,
            )
        })
        .collect::<BTreeMap<_, _>>();
    let mut working = BTreeMap::<(AppName, AppRecordId), WorkingRecord>::new();
    let mut created_ids = BTreeSet::new();
    let mut explicitly_deleted = Vec::new();

    for operation in &command.operations {
        match operation {
            AppMutationOperation::Create {
                entity,
                temporary_id,
                record_id: chosen_record_id,
                payload,
            } => {
                let runtime = active
                    .runtime_contract(entity)
                    .ok_or_else(|| AppEntityMutationError::UnknownEntity(entity.to_string()))?;
                runtime.validate_payload(payload)?;
                let record_id = match chosen_record_id {
                    // A caller may name the row it creates, but not inside the
                    // store's own namespace: an id spelled `rec_...` could be
                    // aimed at one this batch is about to mint, turning a
                    // create into a collision with a record the caller never
                    // saw. The two namespaces stay disjoint.
                    Some(chosen) => {
                        if chosen.as_str().starts_with(HOST_MINTED_RECORD_ID_PREFIX) {
                            return Err(AppEntityMutationError::ReservedRecordId);
                        }
                        chosen.clone()
                    },
                    None => deterministic_record_id(&mutation_key, temporary_id)?,
                };
                if !created_ids.insert(record_id.clone())
                    || record_exists(&transaction, active.installation_id(), entity, &record_id)?
                {
                    return Err(AppEntityMutationError::RecordAlreadyExists);
                }
                working.insert(
                    (entity.clone(), record_id.clone()),
                    WorkingRecord {
                        entity: entity.clone(),
                        record_id,
                        prior_revision: None,
                        created_at: now.clone(),
                        payload: payload.clone(),
                        handling_policy: active.schema().canonical_data_handling_policy.clone(),
                        was_deleted: false,
                        deleted: false,
                    },
                );
            },
            AppMutationOperation::Update {
                entity,
                record_id,
                patch,
            } => {
                let runtime = active
                    .runtime_contract(entity)
                    .ok_or_else(|| AppEntityMutationError::UnknownEntity(entity.to_string()))?;
                ensure_working_record(
                    &transaction,
                    active.installation_id(),
                    &mut working,
                    entity,
                    record_id,
                )?;
                let mut record = working
                    .remove(&(entity.clone(), record_id.clone()))
                    .ok_or(AppEntityMutationError::RecordNotFound)?;
                assert_expected_revision(&expected, entity, record_id, record.prior_revision)?;
                if record.deleted {
                    return Err(AppEntityMutationError::RecordDeleted);
                }
                let patch = patch
                    .as_object()
                    .ok_or(AppEntityMutationError::InvalidPatch)?;
                let object = record
                    .payload
                    .as_object_mut()
                    .ok_or(AppEntityMutationError::CorruptRecord)?;
                for (field, value) in patch {
                    object.insert(field.clone(), value.clone());
                }
                runtime.validate_payload(&record.payload)?;
                working.insert((entity.clone(), record_id.clone()), record);
            },
            AppMutationOperation::Delete { entity, record_id } => {
                ensure_working_record(
                    &transaction,
                    active.installation_id(),
                    &mut working,
                    entity,
                    record_id,
                )?;
                let mut record = working
                    .remove(&(entity.clone(), record_id.clone()))
                    .ok_or(AppEntityMutationError::RecordNotFound)?;
                assert_expected_revision(&expected, entity, record_id, record.prior_revision)?;
                if record.deleted {
                    return Err(AppEntityMutationError::RecordDeleted);
                }
                record.deleted = true;
                working.insert((entity.clone(), record_id.clone()), record);
                explicitly_deleted.push((entity.clone(), record_id.clone()));
            },
            AppMutationOperation::Restore { entity, record_id } => {
                ensure_working_record(
                    &transaction,
                    active.installation_id(),
                    &mut working,
                    entity,
                    record_id,
                )?;
                let mut record = working
                    .remove(&(entity.clone(), record_id.clone()))
                    .ok_or(AppEntityMutationError::RecordNotFound)?;
                assert_expected_revision(&expected, entity, record_id, record.prior_revision)?;
                if !record.deleted {
                    return Err(AppEntityMutationError::RecordNotDeleted);
                }
                active
                    .runtime_contract(entity)
                    .ok_or_else(|| AppEntityMutationError::UnknownEntity(entity.to_string()))?
                    .validate_payload(&record.payload)?;
                record.deleted = false;
                working.insert((entity.clone(), record_id.clone()), record);
            },
            AppMutationOperation::CreateRelation {
                relation,
                from_record_id,
                to_record_id,
                expected_from_revision,
                expected_to_revision,
            } => apply_relation_mutation(
                &transaction,
                &active,
                &mut working,
                relation,
                from_record_id,
                to_record_id,
                *expected_from_revision,
                *expected_to_revision,
                RelationMutationKind::Create,
            )?,
            AppMutationOperation::DeleteRelation {
                relation,
                from_record_id,
                to_record_id,
                expected_from_revision,
                expected_to_revision,
            } => apply_relation_mutation(
                &transaction,
                &active,
                &mut working,
                relation,
                from_record_id,
                to_record_id,
                *expected_from_revision,
                *expected_to_revision,
                RelationMutationKind::Delete,
            )?,
        }
    }

    apply_delete_policies(&transaction, &active, &mut working, explicitly_deleted)?;
    if working.len() > AppContractLimits::default().max_collection_items() {
        return Err(AppEntityMutationError::MutationExpansionLimit);
    }

    for record in working.values() {
        if !record.deleted {
            validate_reference_targets(&transaction, &active, record, &working)?;
        }
    }
    validate_denied_reference_cycles(&transaction, &active, &working)?;

    let receipt_id = receipt_id(&mutation_key)?;
    let provenance = provenance_for(&origin, &receipt_id);
    let dataset_generation =
        advance_dataset_generation(&transaction, active.installation_id(), now.clone())?;
    let storage_usage = project_storage_usage(&transaction, &active, &working)?;
    let first_change_seq =
        reserve_change_sequences(&transaction, active.installation_id(), working.len())?;
    let mut next_change_seq = first_change_seq;
    let mut committed = Vec::with_capacity(working.len());
    for record in working.values() {
        let revision = next_revision(record.prior_revision)?;
        write_record_revision(
            &transaction,
            &active,
            record,
            revision,
            dataset_generation,
            next_change_seq,
            &provenance,
            now.clone(),
        )?;
        committed.push(AppCommittedRecordRevision {
            entity: record.entity.clone(),
            record_id: record.record_id.clone(),
            revision,
        });
        next_change_seq = next_change_seq
            .checked_add(1)
            .ok_or(AppEntityMutationError::SequenceExhausted)?;
    }
    let last_change_seq = next_change_seq
        .checked_sub(1)
        .ok_or(AppEntityMutationError::SequenceExhausted)?;
    persist_storage_usage(
        &transaction,
        active.installation_id(),
        storage_usage,
        now.clone(),
    )?;

    let receipt = AppMutationReceipt {
        receipt_id: receipt_id.clone(),
        installation_id: active.installation_id().clone(),
        origin,
        mutation_key: mutation_key.clone(),
        batch_digest: batch_digest.clone(),
        committed_record_revisions: committed,
        change_seq_range: AppChangeSequenceRange {
            first: first_change_seq,
            last: last_change_seq,
        },
        committed_at: now.clone(),
    };
    receipt.validate_app_contract(&AppContractLimits::default())?;
    let receipt_json = serde_json::to_vec(&receipt)?;
    transaction.execute(
        "INSERT INTO app_mutation_receipts (
             receipt_id, installation_id, idempotency_key, mutation_digest,
             first_change_seq, last_change_seq, record_json, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            receipt.receipt_id.as_str(),
            receipt.installation_id.as_str(),
            receipt.mutation_key.as_str(),
            receipt.batch_digest.as_str(),
            to_sql_u64(first_change_seq)?,
            to_sql_u64(last_change_seq)?,
            receipt_json,
            now.to_rfc3339(),
        ],
    )?;
    append_outbox(&transaction, &receipt, now)?;
    let touched = working
        .values()
        .filter(|record| record.prior_revision.is_some())
        .map(|record| (record.entity.clone(), record.record_id.clone()))
        .collect::<Vec<_>>();
    if !touched.is_empty() {
        settle_candidates_for_committed_records(
            &transaction,
            scope,
            active.installation_id(),
            &touched,
            now,
        )?;
    }
    transaction.commit()?;
    Ok(receipt)
}

fn load_replay(
    connection: &Connection,
    installation_id: &AppInstallationId,
    mutation_key: &AppDigest,
    batch_digest: &AppDigest,
    origin: &AppMutationOrigin,
) -> Result<Option<AppMutationReceipt>, AppEntityMutationError> {
    let stored = connection
        .query_row(
            "SELECT receipt_id, mutation_digest, first_change_seq, last_change_seq, record_json
             FROM app_mutation_receipts
             WHERE installation_id = ?1 AND idempotency_key = ?2",
            params![installation_id.as_str(), mutation_key.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                ))
            },
        )
        .optional()?;
    let Some((stored_receipt_id, stored_digest, stored_first, stored_last, bytes)) = stored else {
        return Ok(None);
    };
    if AppDigest::parse(stored_digest)? != *batch_digest {
        return Err(AppEntityMutationError::IdempotencyConflict);
    }
    let receipt: AppMutationReceipt = decode_bounded_contract(&bytes)?;
    if receipt.installation_id != *installation_id
        || receipt.mutation_key != *mutation_key
        || receipt.batch_digest != *batch_digest
        || receipt.origin != *origin
        || receipt.receipt_id != AppReference::parse(stored_receipt_id)?
        || receipt.receipt_id != receipt_id(mutation_key)?
        || to_sql_u64(receipt.change_seq_range.first)? != stored_first
        || to_sql_u64(receipt.change_seq_range.last)? != stored_last
        || receipt
            .change_seq_range
            .last
            .checked_sub(receipt.change_seq_range.first)
            .and_then(|span| span.checked_add(1))
            != u64::try_from(receipt.committed_record_revisions.len()).ok()
    {
        return Err(AppEntityMutationError::CorruptReceipt);
    }
    Ok(Some(receipt))
}

/// Prefix reserved for record ids the store mints itself. A caller-chosen id
/// may never begin with it; see the `Create` variant's `record_id`.
pub(crate) const HOST_MINTED_RECORD_ID_PREFIX: &str = "rec_";

fn deterministic_record_id(
    mutation_key: &AppDigest,
    temporary_id: &AppName,
) -> Result<AppRecordId, AppEntityMutationError> {
    let digest = AppDigest::blake3_canonical_json(&serde_json::json!({
        "mutation_key": mutation_key,
        "temporary_id": temporary_id,
    }))?;
    let hex = digest
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppEntityMutationError::CorruptMutationKey)?;
    Ok(AppRecordId::parse(format!(
        "{HOST_MINTED_RECORD_ID_PREFIX}{}",
        &hex[..32]
    ))?)
}

fn receipt_id(mutation_key: &AppDigest) -> Result<AppReference, AppEntityMutationError> {
    let hex = mutation_key
        .as_str()
        .strip_prefix("blake3:")
        .ok_or(AppEntityMutationError::CorruptMutationKey)?;
    Ok(AppReference::parse(format!("app-receipt:{hex}"))?)
}

fn provenance_for(origin: &AppMutationOrigin, receipt_id: &AppReference) -> AppRecordProvenance {
    let (actor_kind, actor_id, execution_id, output_revision, source_artifact_refs) = match origin {
        AppMutationOrigin::OwnerApi { session_ref, .. } => (
            AppRecordActorKind::User,
            session_ref.clone(),
            None,
            None,
            Vec::new(),
        ),
        AppMutationOrigin::Workflow {
            execution_id,
            output_revision,
            source_artifact_refs,
        } => (
            AppRecordActorKind::Workflow,
            execution_id.clone(),
            Some(execution_id.clone()),
            Some(*output_revision),
            source_artifact_refs.clone(),
        ),
        AppMutationOrigin::Surface {
            surface_session_id, ..
        } => (
            AppRecordActorKind::Surface,
            surface_session_id.clone(),
            None,
            None,
            Vec::new(),
        ),
        AppMutationOrigin::Migration {
            migration_run_id, ..
        } => (
            AppRecordActorKind::Migration,
            migration_run_id.clone(),
            None,
            None,
            Vec::new(),
        ),
    };
    AppRecordProvenance {
        actor_kind,
        actor_id,
        execution_id,
        output_revision,
        mutation_receipt_id: Some(receipt_id.clone()),
        source_artifact_refs,
        citation_refs: Vec::new(),
    }
}

fn record_exists(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    entity: &AppName,
    record_id: &AppRecordId,
) -> Result<bool, AppEntityMutationError> {
    Ok(transaction
        .query_row(
            "SELECT 1 FROM app_record_heads
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn load_existing_for_mutation(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    entity: &AppName,
    record_id: &AppRecordId,
) -> Result<WorkingRecord, AppEntityMutationError> {
    load_existing_optional(transaction, installation_id, entity, record_id)?
        .ok_or(AppEntityMutationError::RecordNotFound)
}

fn load_existing_optional(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    entity: &AppName,
    record_id: &AppRecordId,
) -> Result<Option<WorkingRecord>, AppEntityMutationError> {
    let row = transaction
        .query_row(
            "SELECT h.record_revision, h.deleted_at,
                    r.payload_digest, r.payload_json,
                    r.handling_policy_digest, r.handling_policy_json, r.created_at
             FROM app_record_heads h
             JOIN app_record_revisions r
               ON r.installation_id = h.installation_id
              AND r.entity_name = h.entity_name
              AND r.record_id = h.record_id
              AND r.record_revision = h.record_revision
             WHERE h.installation_id = ?1 AND h.entity_name = ?2 AND h.record_id = ?3",
            params![
                installation_id.as_str(),
                entity.as_str(),
                record_id.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Vec<u8>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Vec<u8>>(5)?,
                    row.get::<_, String>(6)?,
                ))
            },
        )
        .optional()?;
    let Some(row) = row else {
        return Ok(None);
    };
    let revision =
        AppRevision::new(u64::try_from(row.0).map_err(|_| AppEntityMutationError::CorruptRecord)?)?;
    let payload = super::models::decode_bounded_json_value(&row.3, &AppContractLimits::default())?;
    if AppDigest::blake3_canonical_json(&payload)? != AppDigest::parse(row.2)? {
        return Err(AppEntityMutationError::CorruptRecord);
    }
    let policy_value =
        super::models::decode_bounded_json_value(&row.5, &AppContractLimits::default())?;
    let handling_policy: AppDataHandlingPolicy = serde_json::from_value(policy_value)?;
    validate_policy(&handling_policy, &AppContractLimits::default())?;
    if AppDigest::blake3_canonical_json(&serde_json::to_value(&handling_policy)?)?
        != AppDigest::parse(row.4)?
    {
        return Err(AppEntityMutationError::CorruptRecord);
    }
    let created_at = DateTime::parse_from_rfc3339(&row.6)
        .map_err(|_| AppEntityMutationError::CorruptRecord)?
        .with_timezone(&Utc);
    Ok(Some(WorkingRecord {
        entity: entity.clone(),
        record_id: record_id.clone(),
        prior_revision: Some(revision),
        created_at,
        payload,
        handling_policy,
        was_deleted: row.1.is_some(),
        deleted: row.1.is_some(),
    }))
}

fn assert_expected_revision(
    expected: &BTreeMap<(AppName, AppRecordId), AppRevision>,
    entity: &AppName,
    record_id: &AppRecordId,
    actual: Option<AppRevision>,
) -> Result<(), AppEntityMutationError> {
    if expected.get(&(entity.clone(), record_id.clone())).copied() != actual {
        return Err(AppEntityMutationError::RecordRevisionConflict);
    }
    Ok(())
}

pub fn validate_reference_targets(
    transaction: &Transaction<'_>,
    active: &super::entity_store::ActiveAppEntitySchema,
    record: &WorkingRecord,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
) -> Result<(), AppEntityMutationError> {
    let runtime = active
        .runtime_contract(&record.entity)
        .ok_or_else(|| AppEntityMutationError::UnknownEntity(record.entity.to_string()))?;
    for (field, contract) in runtime.fields() {
        let Some(target_entity) = contract.reference_entity() else {
            continue;
        };
        let Some(target_id) = record.payload.get(field.as_str()).and_then(Value::as_str) else {
            continue;
        };
        let target_id = AppRecordId::parse(target_id)?;
        let target_key = (target_entity.clone(), target_id.clone());
        let available = if let Some(target) = working.get(&target_key) {
            !target.deleted
        } else {
            transaction
                .query_row(
                    "SELECT deleted_at FROM app_record_heads
                     WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
                    params![
                        active.installation_id().as_str(),
                        target_entity.as_str(),
                        target_id.as_str(),
                    ],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .is_some_and(|deleted| deleted.is_none())
        };
        if !available {
            return Err(AppEntityMutationError::ReferenceTargetUnavailable {
                field: field.to_string(),
            });
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
enum RelationMutationKind {
    Create,
    Delete,
}

#[allow(clippy::too_many_arguments)]
fn apply_relation_mutation(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &mut BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    relation: &AppName,
    from_record_id: &AppRecordId,
    to_record_id: &AppRecordId,
    expected_from_revision: AppRevision,
    expected_to_revision: AppRevision,
    kind: RelationMutationKind,
) -> Result<(), AppEntityMutationError> {
    let relation_path = AppFieldPath::parse(relation.as_str())?;
    let mut candidates = Vec::new();
    for (source_entity, runtime) in active.runtime_contracts() {
        let Some(field) = runtime.field(&relation_path) else {
            continue;
        };
        let Some(target_entity) = field.reference_entity() else {
            continue;
        };
        let source_key = (source_entity.clone(), from_record_id.clone());
        if working.contains_key(&source_key)
            || record_exists(
                transaction,
                active.installation_id(),
                source_entity,
                from_record_id,
            )?
        {
            candidates.push((source_entity.clone(), target_entity.clone()));
        }
    }
    let (source_entity, target_entity) = match candidates.as_slice() {
        [] => {
            return Err(AppEntityMutationError::UnknownRelation(
                relation.to_string(),
            ))
        },
        [candidate] => candidate.clone(),
        _ => {
            return Err(AppEntityMutationError::AmbiguousRelation(
                relation.to_string(),
            ))
        },
    };
    let source_key = (source_entity.clone(), from_record_id.clone());
    ensure_working_record(
        transaction,
        active.installation_id(),
        working,
        &source_entity,
        from_record_id,
    )?;
    let target_key = (target_entity.clone(), to_record_id.clone());
    let target = match working.get(&target_key) {
        Some(record) => record.clone(),
        None => load_existing_for_mutation(
            transaction,
            active.installation_id(),
            &target_entity,
            to_record_id,
        )?,
    };
    if target.deleted {
        return Err(AppEntityMutationError::RelationEndpointDeleted);
    }
    if target.prior_revision != Some(expected_to_revision) {
        return Err(AppEntityMutationError::RelationRevisionConflict);
    }

    let runtime = active
        .runtime_contract(&source_entity)
        .ok_or_else(|| AppEntityMutationError::UnknownEntity(source_entity.to_string()))?;
    let field = runtime
        .field(&relation_path)
        .ok_or_else(|| AppEntityMutationError::UnknownRelation(relation.to_string()))?;
    let source = working
        .get_mut(&source_key)
        .ok_or(AppEntityMutationError::RecordNotFound)?;
    if source.deleted {
        return Err(AppEntityMutationError::RelationEndpointDeleted);
    }
    if source.prior_revision != Some(expected_from_revision) {
        return Err(AppEntityMutationError::RelationRevisionConflict);
    }
    let object = source
        .payload
        .as_object_mut()
        .ok_or(AppEntityMutationError::CorruptRecord)?;
    let current = object
        .get(relation.as_str())
        .filter(|value| !value.is_null())
        .and_then(Value::as_str);
    match kind {
        RelationMutationKind::Create => {
            if current.is_some() {
                return Err(AppEntityMutationError::RelationAlreadyExists);
            }
            object.insert(
                relation.to_string(),
                Value::String(to_record_id.to_string()),
            );
        },
        RelationMutationKind::Delete => {
            if current != Some(to_record_id.as_str()) {
                return Err(AppEntityMutationError::RelationNotFound);
            }
            if field.nullable() {
                object.insert(relation.to_string(), Value::Null);
            } else {
                object.remove(relation.as_str());
            }
        },
    }
    runtime.validate_payload(&source.payload)?;
    Ok(())
}

pub fn ensure_working_record(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    working: &mut BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    entity: &AppName,
    record_id: &AppRecordId,
) -> Result<(), AppEntityMutationError> {
    let key = (entity.clone(), record_id.clone());
    if !working.contains_key(&key) {
        let record = load_existing_for_mutation(transaction, installation_id, entity, record_id)?;
        working.insert(key, record);
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct IncomingReference {
    source_entity: AppName,
    field: AppFieldPath,
    source_id: AppRecordId,
    delete_policy: AppReferenceDeletePolicy,
}

pub fn apply_delete_policies(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &mut BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    initial: Vec<(AppName, AppRecordId)>,
) -> Result<(), AppEntityMutationError> {
    let limits = AppContractLimits::default();
    let mut queue = VecDeque::from(initial);
    let mut visited = BTreeSet::new();
    let mut deferred_restrict = Vec::new();
    let mut inspected_edges = 0usize;
    while let Some((target_entity, target_id)) = queue.pop_front() {
        if !visited.insert((target_entity.clone(), target_id.clone())) {
            continue;
        }
        let incoming =
            incoming_references(transaction, active, working, &target_entity, &target_id)?;
        inspected_edges = inspected_edges
            .checked_add(incoming.len())
            .ok_or(AppEntityMutationError::MutationExpansionLimit)?;
        if inspected_edges > limits.max_collection_items() {
            return Err(AppEntityMutationError::MutationExpansionLimit);
        }
        for incoming in incoming {
            let source_key = (incoming.source_entity.clone(), incoming.source_id.clone());
            ensure_working_record(
                transaction,
                active.installation_id(),
                working,
                &incoming.source_entity,
                &incoming.source_id,
            )?;
            let source = working
                .get_mut(&source_key)
                .ok_or(AppEntityMutationError::RecordNotFound)?;
            if source.deleted || !reference_points_to(&source.payload, &incoming.field, &target_id)
            {
                continue;
            }
            match incoming.delete_policy {
                AppReferenceDeletePolicy::Restrict => {
                    deferred_restrict.push((incoming, target_id.clone()));
                },
                AppReferenceDeletePolicy::Nullify => {
                    source
                        .payload
                        .as_object_mut()
                        .ok_or(AppEntityMutationError::CorruptRecord)?
                        .insert(incoming.field.to_string(), Value::Null);
                    active
                        .runtime_contract(&incoming.source_entity)
                        .ok_or_else(|| {
                            AppEntityMutationError::UnknownEntity(
                                incoming.source_entity.to_string(),
                            )
                        })?
                        .validate_payload(&source.payload)?;
                },
                AppReferenceDeletePolicy::Cascade => {
                    source.deleted = true;
                    queue.push_back(source_key);
                },
            }
            if working.len() > limits.max_collection_items() {
                return Err(AppEntityMutationError::MutationExpansionLimit);
            }
        }
    }
    for (incoming, target_id) in deferred_restrict {
        let IncomingReference {
            source_entity,
            field,
            source_id,
            ..
        } = incoming;
        let source_key = (source_entity, source_id);
        if working.get(&source_key).is_some_and(|source| {
            !source.deleted && reference_points_to(&source.payload, &field, &target_id)
        }) {
            return Err(AppEntityMutationError::ReferencedRecordDeleteDenied);
        }
    }
    Ok(())
}

fn incoming_references(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    target_entity: &AppName,
    target_id: &AppRecordId,
) -> Result<Vec<IncomingReference>, AppEntityMutationError> {
    let limits = AppContractLimits::default();
    let mut references = BTreeMap::<(AppName, AppFieldPath, AppRecordId), IncomingReference>::new();
    let mut statement = transaction.prepare(
        "SELECT i.entity_name, i.field_path, i.record_id
         FROM app_scalar_indexes i
         JOIN app_record_heads h
           ON h.installation_id = i.installation_id
          AND h.entity_name = i.entity_name
          AND h.record_id = i.record_id
          AND h.record_revision = i.record_revision
         WHERE i.installation_id = ?1 AND i.value_kind = 'reference'
           AND i.text_value = ?2 AND h.deleted_at IS NULL
         ORDER BY i.entity_name, i.field_path, i.record_id
         LIMIT ?3",
    )?;
    let limit = i64::try_from(limits.max_collection_items() + 1)
        .map_err(|_| AppEntityMutationError::IncomingReferenceLimit)?;
    let rows = statement.query_map(
        params![active.installation_id().as_str(), target_id.as_str(), limit],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        },
    )?;
    for row in rows {
        if references.len() >= limits.max_collection_items() {
            return Err(AppEntityMutationError::IncomingReferenceLimit);
        }
        let (source_entity, field, source_id) = row?;
        let source_entity = AppName::parse(source_entity)?;
        let field = super::models::AppFieldPath::parse(field)?;
        let source_id = AppRecordId::parse(source_id)?;
        let Some(contract) = active
            .runtime_contract(&source_entity)
            .and_then(|runtime| runtime.field(&field))
        else {
            return Err(AppEntityMutationError::CorruptReferenceIndex);
        };
        if contract.reference_entity() != Some(target_entity) {
            continue;
        }
        if let Some(source) = working.get(&(source_entity.clone(), source_id.clone())) {
            if source.deleted || !reference_points_to(&source.payload, &field, target_id) {
                continue;
            }
        }
        let delete_policy = contract
            .reference_delete_policy()
            .ok_or(AppEntityMutationError::CorruptReferenceIndex)?;
        references.insert(
            (source_entity.clone(), field.clone(), source_id.clone()),
            IncomingReference {
                source_entity,
                field,
                source_id,
                delete_policy,
            },
        );
    }
    drop(statement);

    for record in working.values().filter(|record| !record.deleted) {
        let runtime = active
            .runtime_contract(&record.entity)
            .ok_or_else(|| AppEntityMutationError::UnknownEntity(record.entity.to_string()))?;
        for (field, contract) in runtime.fields() {
            if contract.reference_entity() != Some(target_entity)
                || !reference_points_to(&record.payload, field, target_id)
            {
                continue;
            }
            if references.len() >= limits.max_collection_items() {
                return Err(AppEntityMutationError::IncomingReferenceLimit);
            }
            references.insert(
                (
                    record.entity.clone(),
                    field.clone(),
                    record.record_id.clone(),
                ),
                IncomingReference {
                    source_entity: record.entity.clone(),
                    field: field.clone(),
                    source_id: record.record_id.clone(),
                    delete_policy: contract
                        .reference_delete_policy()
                        .ok_or(AppEntityMutationError::CorruptReferenceIndex)?,
                },
            );
        }
    }
    Ok(references.into_values().collect())
}

fn reference_points_to(payload: &Value, field: &AppFieldPath, target: &AppRecordId) -> bool {
    payload
        .get(field.as_str())
        .and_then(Value::as_str)
        .is_some_and(|value| value == target.as_str())
}

pub fn validate_denied_reference_cycles(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
) -> Result<(), AppEntityMutationError> {
    let limits = AppContractLimits::default();
    let mut cache = BTreeMap::<(AppName, AppRecordId), Option<WorkingRecord>>::new();
    let mut node_budget = limits.max_collection_items();
    let mut step_budget = limits
        .max_collection_items()
        .checked_mul(limits.max_collection_items())
        .ok_or(AppEntityMutationError::CycleCheckLimit)?;
    for record in working.values().filter(|record| !record.deleted) {
        let runtime = active
            .runtime_contract(&record.entity)
            .ok_or_else(|| AppEntityMutationError::UnknownEntity(record.entity.to_string()))?;
        for (field, contract) in runtime.fields() {
            if contract.reference_cycle_policy() != Some(AppReferenceCyclePolicy::Deny) {
                continue;
            }
            let (Some(target_entity), Some(target_id)) = (
                contract.reference_entity(),
                record.payload.get(field.as_str()).and_then(Value::as_str),
            ) else {
                continue;
            };
            let target_id = AppRecordId::parse(target_id)?;
            ensure_no_reference_path(
                transaction,
                active,
                working,
                &mut cache,
                &mut node_budget,
                &mut step_budget,
                (target_entity.clone(), target_id),
                (record.entity.clone(), record.record_id.clone()),
            )?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn ensure_no_reference_path(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    cache: &mut BTreeMap<(AppName, AppRecordId), Option<WorkingRecord>>,
    node_budget: &mut usize,
    step_budget: &mut usize,
    start: (AppName, AppRecordId),
    forbidden: (AppName, AppRecordId),
) -> Result<(), AppEntityMutationError> {
    let mut queue = VecDeque::from([start]);
    let mut visited = BTreeSet::new();
    while let Some(key) = queue.pop_front() {
        if *step_budget == 0 {
            return Err(AppEntityMutationError::CycleCheckLimit);
        }
        *step_budget -= 1;
        if key == forbidden {
            return Err(AppEntityMutationError::ReferenceCycleDenied);
        }
        if !visited.insert(key.clone()) {
            continue;
        }
        let Some(record) = graph_record(transaction, active, working, cache, node_budget, &key)?
        else {
            return Err(AppEntityMutationError::CorruptReferenceGraph);
        };
        let runtime = active
            .runtime_contract(&record.entity)
            .ok_or_else(|| AppEntityMutationError::UnknownEntity(record.entity.to_string()))?;
        runtime.validate_payload(&record.payload)?;
        for (field, contract) in runtime.fields() {
            let (Some(target_entity), Some(target_id)) = (
                contract.reference_entity(),
                record.payload.get(field.as_str()).and_then(Value::as_str),
            ) else {
                continue;
            };
            queue.push_back((target_entity.clone(), AppRecordId::parse(target_id)?));
        }
    }
    Ok(())
}

fn graph_record(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
    cache: &mut BTreeMap<(AppName, AppRecordId), Option<WorkingRecord>>,
    node_budget: &mut usize,
    key: &(AppName, AppRecordId),
) -> Result<Option<WorkingRecord>, AppEntityMutationError> {
    if let Some(record) = working.get(key) {
        return Ok((!record.deleted).then(|| record.clone()));
    }
    if let Some(record) = cache.get(key) {
        return Ok(record.clone());
    }
    if *node_budget == 0 {
        return Err(AppEntityMutationError::CycleCheckLimit);
    }
    *node_budget -= 1;
    let record = load_existing_optional(transaction, active.installation_id(), &key.0, &key.1)?
        .filter(|record| !record.deleted);
    cache.insert(key.clone(), record.clone());
    Ok(record)
}

pub fn advance_dataset_generation(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    now: DateTime<Utc>,
) -> Result<u64, AppEntityMutationError> {
    let stored = transaction
        .query_row(
            "SELECT current_generation FROM app_dataset_generations
             WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let had_generation_row = stored.is_some();
    let current = match stored {
        Some(value) => {
            let value = u64::try_from(value)
                .map_err(|_| AppEntityMutationError::CorruptDatasetGeneration)?;
            if value == 0 {
                return Err(AppEntityMutationError::CorruptDatasetGeneration);
            }
            value
        },
        None => {
            let maximum: Option<i64> = transaction.query_row(
                "SELECT MAX(dataset_generation) FROM app_record_heads
                 WHERE installation_id = ?1",
                params![installation_id.as_str()],
                |row| row.get(0),
            )?;
            maximum
                .map(u64::try_from)
                .transpose()
                .map_err(|_| AppEntityMutationError::CorruptDatasetGeneration)?
                .unwrap_or(0)
        },
    };
    let generation = if had_generation_row || current > 0 {
        current
            .checked_add(1)
            .ok_or(AppEntityMutationError::SequenceExhausted)?
    } else {
        1
    };
    transaction.execute(
        "INSERT INTO app_dataset_generations (installation_id, current_generation, updated_at)
         VALUES (?1, ?2, ?3)
         ON CONFLICT(installation_id) DO UPDATE SET
             current_generation = excluded.current_generation,
             updated_at = excluded.updated_at",
        params![
            installation_id.as_str(),
            to_sql_u64(generation)?,
            now.to_rfc3339(),
        ],
    )?;
    Ok(generation)
}

pub fn reserve_change_sequences(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    count: usize,
) -> Result<u64, AppEntityMutationError> {
    if count == 0 {
        return Err(AppEntityMutationError::EmptyCommit);
    }
    let stored = transaction
        .query_row(
            "SELECT next_change_seq FROM app_installation_sequences
             WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let first = if let Some(stored) = stored {
        u64::try_from(stored).map_err(|_| AppEntityMutationError::SequenceExhausted)?
    } else {
        let maximum: Option<i64> = transaction.query_row(
            "SELECT MAX(change_seq) FROM app_record_heads WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )?;
        let maximum = maximum
            .map(u64::try_from)
            .transpose()
            .map_err(|_| AppEntityMutationError::SequenceExhausted)?
            .unwrap_or(0);
        let first = maximum
            .checked_add(1)
            .ok_or(AppEntityMutationError::SequenceExhausted)?;
        transaction.execute(
            "INSERT INTO app_installation_sequences (installation_id, next_change_seq)
             VALUES (?1, ?2)",
            params![installation_id.as_str(), to_sql_u64(first)?],
        )?;
        first
    };
    if first == 0 {
        return Err(AppEntityMutationError::SequenceExhausted);
    }
    let next = first
        .checked_add(u64::try_from(count).map_err(|_| AppEntityMutationError::SequenceExhausted)?)
        .ok_or(AppEntityMutationError::SequenceExhausted)?;
    transaction.execute(
        "UPDATE app_installation_sequences SET next_change_seq = ?2 WHERE installation_id = ?1",
        params![installation_id.as_str(), to_sql_u64(next)?],
    )?;
    Ok(first)
}

#[allow(clippy::too_many_arguments)]
pub fn write_record_revision(
    transaction: &Transaction<'_>,
    active: &super::entity_store::ActiveAppEntitySchema,
    record: &WorkingRecord,
    revision: AppRevision,
    dataset_generation: u64,
    change_seq: u64,
    provenance: &AppRecordProvenance,
    now: DateTime<Utc>,
) -> Result<(), AppEntityMutationError> {
    let runtime = active
        .runtime_contract(&record.entity)
        .ok_or_else(|| AppEntityMutationError::UnknownEntity(record.entity.to_string()))?;
    runtime.validate_payload(&record.payload)?;
    validate_policy(&record.handling_policy, &AppContractLimits::default())?;
    let deleted_at = record.deleted.then_some(now.clone());
    let contract = AppRecordRevision {
        installation_id: active.installation_id().clone(),
        entity_name: record.entity.clone(),
        record_id: record.record_id.clone(),
        record_revision: revision,
        dataset_generation,
        schema_revision: active.schema_revision(),
        payload: record.payload.clone(),
        handling_override: Some(record.handling_policy.clone()),
        created_at: record.created_at.clone(),
        updated_at: now.clone(),
        deleted_at: deleted_at.clone(),
        provenance: provenance.clone(),
    };
    contract.validate_app_contract(&AppContractLimits::default())?;
    let payload_digest = AppDigest::blake3_canonical_json(&record.payload)?;
    let payload_json = serde_json::to_vec(&record.payload)?;
    let policy_value = serde_json::to_value(&record.handling_policy)?;
    let policy_digest = AppDigest::blake3_canonical_json(&policy_value)?;
    let policy_json = serde_json::to_vec(&record.handling_policy)?;
    let provenance_json = serde_json::to_vec(provenance)?;
    transaction.execute(
        "INSERT INTO app_record_revisions (
             installation_id, entity_name, record_id, record_revision,
             dataset_generation, schema_revision, payload_digest, payload_json,
             handling_policy_digest, handling_policy_json, provenance_json,
             created_at, updated_at, deleted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            active.installation_id().as_str(),
            record.entity.as_str(),
            record.record_id.as_str(),
            to_sql_u64(revision.get())?,
            to_sql_u64(dataset_generation)?,
            to_sql_u64(active.schema_revision().get())?,
            payload_digest.as_str(),
            payload_json,
            policy_digest.as_str(),
            policy_json,
            provenance_json,
            record.created_at.to_rfc3339(),
            now.to_rfc3339(),
            deleted_at.as_ref().map(DateTime::to_rfc3339),
        ],
    )?;
    transaction.execute(
        "INSERT INTO app_record_heads (
             installation_id, entity_name, record_id, record_revision,
             dataset_generation, schema_revision, change_seq, deleted_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(installation_id, entity_name, record_id) DO UPDATE SET
             record_revision = excluded.record_revision,
             dataset_generation = excluded.dataset_generation,
             schema_revision = excluded.schema_revision,
             change_seq = excluded.change_seq,
             deleted_at = excluded.deleted_at",
        params![
            active.installation_id().as_str(),
            record.entity.as_str(),
            record.record_id.as_str(),
            to_sql_u64(revision.get())?,
            to_sql_u64(dataset_generation)?,
            to_sql_u64(active.schema_revision().get())?,
            to_sql_u64(change_seq)?,
            deleted_at.as_ref().map(DateTime::to_rfc3339),
        ],
    )?;
    replace_indexes(
        transaction,
        active.installation_id(),
        runtime,
        record,
        revision,
    )?;
    Ok(())
}

fn replace_indexes(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    runtime: &AppEntityRuntimeContract,
    record: &WorkingRecord,
    revision: AppRevision,
) -> Result<(), AppEntityMutationError> {
    transaction.execute(
        "DELETE FROM app_scalar_indexes
         WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
        params![
            installation_id.as_str(),
            record.entity.as_str(),
            record.record_id.as_str()
        ],
    )?;
    transaction.execute(
        "DELETE FROM app_text_search
         WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3",
        params![
            installation_id.as_str(),
            record.entity.as_str(),
            record.record_id.as_str()
        ],
    )?;
    if record.deleted {
        return Ok(());
    }
    for (field, contract) in runtime.fields() {
        let Some(value) = record.payload.get(field.as_str()) else {
            continue;
        };
        if contract.indexed() {
            let index = project_scalar_index(contract.kind(), value)?;
            transaction.execute(
                "INSERT INTO app_scalar_indexes (
                     installation_id, entity_name, field_path, record_id,
                     record_revision, value_kind, text_value, integer_value, real_value, order_key_asc, order_key_desc
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, ?9, ?10)",
                params![
                    installation_id.as_str(),
                    record.entity.as_str(),
                    field.as_str(),
                    record.record_id.as_str(),
                    to_sql_u64(revision.get())?,
                    index.value_kind,
                    index.text_value,
                    index.integer_value,
                    super::indexed_snapshot::order_key(index.value_kind, index.text_value.as_deref(), index.integer_value, false)?,
                    super::indexed_snapshot::order_key(index.value_kind, index.text_value.as_deref(), index.integer_value, true)?,
                ],
            )?;
        }
        if contract.text_search() {
            if let Some(search_text) = normalized_search_text(contract.kind(), value)? {
                transaction.execute(
                    "INSERT INTO app_text_search (
                         installation_id, entity_name, field_path, record_id,
                         record_revision, search_text
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        installation_id.as_str(),
                        record.entity.as_str(),
                        field.as_str(),
                        record.record_id.as_str(),
                        to_sql_u64(revision.get())?,
                        search_text,
                    ],
                )?;
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub struct StorageUsageProjection {
    record_count: i64,
    revision_count: i64,
    payload_bytes: i64,
    attachment_bytes: i64,
}

pub fn project_storage_usage(
    transaction: &Transaction<'_>,
    active: &ActiveAppEntitySchema,
    working: &BTreeMap<(AppName, AppRecordId), WorkingRecord>,
) -> Result<StorageUsageProjection, AppEntityMutationError> {
    let stored = transaction
        .query_row(
            "SELECT record_count, revision_count, payload_bytes, attachment_bytes
             FROM app_storage_usage WHERE installation_id = ?1",
            params![active.installation_id().as_str()],
            |row| {
                Ok(StorageUsageProjection {
                    record_count: row.get(0)?,
                    revision_count: row.get(1)?,
                    payload_bytes: row.get(2)?,
                    attachment_bytes: row.get(3)?,
                })
            },
        )
        .optional()?;
    let baseline = if let Some(stored) = stored {
        stored
    } else {
        let record_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM app_record_heads
             WHERE installation_id = ?1 AND deleted_at IS NULL",
            params![active.installation_id().as_str()],
            |row| row.get(0),
        )?;
        let (revision_count, payload_bytes): (i64, i64) = transaction.query_row(
            "SELECT COUNT(*), COALESCE(SUM(length(payload_json)), 0)
             FROM app_record_revisions WHERE installation_id = ?1",
            params![active.installation_id().as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        StorageUsageProjection {
            record_count,
            revision_count,
            payload_bytes,
            attachment_bytes: 0,
        }
    };
    if baseline.record_count < 0
        || baseline.revision_count < 0
        || baseline.payload_bytes < 0
        || baseline.attachment_bytes < 0
    {
        return Err(AppEntityMutationError::CorruptStorageUsage);
    }
    let mut record_delta = 0_i64;
    let mut payload_delta = 0_i64;
    for record in working.values() {
        let live_record_delta = if record.prior_revision.is_none() {
            if record.deleted {
                0
            } else {
                1
            }
        } else {
            match (record.was_deleted, record.deleted) {
                (false, true) => -1,
                (true, false) => 1,
                _ => 0,
            }
        };
        record_delta = record_delta
            .checked_add(live_record_delta)
            .ok_or(AppEntityMutationError::StorageUsageOverflow)?;
        let payload_len = i64::try_from(serde_json::to_vec(&record.payload)?.len())
            .map_err(|_| AppEntityMutationError::StorageUsageOverflow)?;
        payload_delta = payload_delta
            .checked_add(payload_len)
            .ok_or(AppEntityMutationError::StorageUsageOverflow)?;
    }
    let projected = StorageUsageProjection {
        record_count: baseline
            .record_count
            .checked_add(record_delta)
            .ok_or(AppEntityMutationError::StorageUsageOverflow)?,
        revision_count: baseline
            .revision_count
            .checked_add(
                i64::try_from(working.len())
                    .map_err(|_| AppEntityMutationError::StorageUsageOverflow)?,
            )
            .ok_or(AppEntityMutationError::StorageUsageOverflow)?,
        payload_bytes: baseline
            .payload_bytes
            .checked_add(payload_delta)
            .ok_or(AppEntityMutationError::StorageUsageOverflow)?,
        attachment_bytes: baseline.attachment_bytes,
    };
    if projected.record_count < 0 {
        return Err(AppEntityMutationError::CorruptStorageUsage);
    }
    let ceiling = &active.grant().granted_resource_ceiling;
    if u64::try_from(projected.record_count)
        .ok()
        .is_none_or(|value| value > ceiling.max_records)
        || u64::try_from(projected.payload_bytes)
            .ok()
            .is_none_or(|value| value > ceiling.max_payload_bytes)
        || u64::try_from(projected.attachment_bytes)
            .ok()
            .is_none_or(|value| value > ceiling.max_attachment_bytes)
    {
        return Err(AppEntityMutationError::StorageCeilingExceeded);
    }
    Ok(projected)
}

pub fn persist_storage_usage(
    transaction: &Transaction<'_>,
    installation_id: &AppInstallationId,
    usage: StorageUsageProjection,
    now: DateTime<Utc>,
) -> Result<(), AppEntityMutationError> {
    transaction.execute(
        "INSERT INTO app_storage_usage (
             installation_id, record_count, revision_count, payload_bytes,
             attachment_bytes, updated_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(installation_id) DO UPDATE SET
             record_count = excluded.record_count,
             revision_count = excluded.revision_count,
             payload_bytes = excluded.payload_bytes,
             attachment_bytes = excluded.attachment_bytes,
             updated_at = excluded.updated_at",
        params![
            installation_id.as_str(),
            usage.record_count,
            usage.revision_count,
            usage.payload_bytes,
            usage.attachment_bytes,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn append_outbox(
    transaction: &Transaction<'_>,
    receipt: &AppMutationReceipt,
    now: DateTime<Utc>,
) -> Result<(), AppEntityMutationError> {
    #[derive(Serialize)]
    struct EntityChange<'a> {
        receipt_id: &'a AppReference,
        installation_id: &'a AppInstallationId,
        committed_record_revisions: &'a [AppCommittedRecordRevision],
        change_seq_range: &'a AppChangeSequenceRange,
    }
    let payload = serde_json::to_vec(&EntityChange {
        receipt_id: &receipt.receipt_id,
        installation_id: &receipt.installation_id,
        committed_record_revisions: &receipt.committed_record_revisions,
        change_seq_range: &receipt.change_seq_range,
    })?;
    let event_id = AppReference::parse(format!("app-entity-change:{}", receipt.receipt_id))?;
    transaction.execute(
        "INSERT INTO app_entity_outbox (
             event_id, installation_id, first_change_seq, last_change_seq,
             payload_json, delivery_state, available_at, lease_token,
             lease_expires_at, created_at, delivered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL, NULL, ?6, NULL)",
        params![
            event_id.as_str(),
            receipt.installation_id.as_str(),
            to_sql_u64(receipt.change_seq_range.first)?,
            to_sql_u64(receipt.change_seq_range.last)?,
            payload,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

pub fn next_revision(current: Option<AppRevision>) -> Result<AppRevision, AppEntityMutationError> {
    AppRevision::new(
        current
            .map(AppRevision::get)
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(AppEntityMutationError::RevisionExhausted)?,
    )
    .map_err(AppEntityMutationError::from)
}

pub fn to_sql_u64(value: u64) -> Result<i64, AppEntityMutationError> {
    i64::try_from(value).map_err(|_| AppEntityMutationError::SqlIntegerOverflow)
}

fn decode_bounded_contract<T>(bytes: &[u8]) -> Result<T, AppEntityMutationError>
where
    T: serde::de::DeserializeOwned + ValidateAppContract,
{
    let value = super::models::decode_bounded_json_value(bytes, &AppContractLimits::default())?;
    let contract: T = serde_json::from_value(value)?;
    contract.validate_app_contract(&AppContractLimits::default())?;
    Ok(contract)
}

#[derive(Debug, Error)]
pub enum AppEntityMutationError {
    #[error("app entity mutation registry failed: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("app entity mutation active-store resolution failed: {0}")]
    Store(#[source] AppEntityStoreError),
    #[error("app entity mutation SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("app entity mutation contract failed: {0}")]
    Contract(#[from] AppContractError),
    #[error("app entity mutation schema failed: {0}")]
    Schema(#[from] AppSchemaCompilerError),
    #[error("app entity mutation authority failed: {0}")]
    Boundary(#[from] AppBoundaryError),
    #[error("app entity mutation index projection failed")]
    Index,
    #[error("app entity mutation encoding failed: {0}")]
    Encoding(#[from] serde_json::Error),
    #[error("app-memory settlement failed: {0}")]
    Memory(#[from] AppMemoryStoreError),
    #[error("app installation does not exist")]
    MissingInstallation,
    #[error("app mutation expected schema revision is stale")]
    SchemaRevisionConflict,
    #[error("app mutation targets unknown entity `{0}`")]
    UnknownEntity(String),
    #[error("app mutation record does not exist")]
    RecordNotFound,
    #[error("app mutation deterministic record identity already exists")]
    RecordAlreadyExists,
    #[error("app mutation cannot choose a record id in the host-minted namespace")]
    ReservedRecordId,
    #[error("app mutation expected record revision is stale")]
    RecordRevisionConflict,
    #[error("app mutation cannot update or delete an already deleted record")]
    RecordDeleted,
    #[error("app mutation can restore only a deleted record")]
    RecordNotDeleted,
    #[error("app mutation patch is not a JSON object")]
    InvalidPatch,
    #[error("app mutation targets unknown relation `{0}`")]
    UnknownRelation(String),
    #[error("app relation `{0}` is ambiguous for the supplied source record")]
    AmbiguousRelation(String),
    #[error("app relation endpoint is deleted")]
    RelationEndpointDeleted,
    #[error("app relation endpoint revision is stale")]
    RelationRevisionConflict,
    #[error("app relation already exists")]
    RelationAlreadyExists,
    #[error("app relation does not exist")]
    RelationNotFound,
    #[error("app mutation record or its persisted digests are corrupt")]
    CorruptRecord,
    #[error("app mutation receipt is corrupt")]
    CorruptReceipt,
    #[error("app mutation logical idempotency key was reused with different bytes")]
    IdempotencyConflict,
    #[error("app mutation references unavailable target through `{field}`")]
    ReferenceTargetUnavailable { field: String },
    #[error("app record is still referenced and its delete policy denies removal")]
    ReferencedRecordDeleteDenied,
    #[error("app incoming-reference scan exceeded its bounded ceiling")]
    IncomingReferenceLimit,
    #[error("app mutation's bounded implicit relation expansion was exceeded")]
    MutationExpansionLimit,
    #[error("app reference index is inconsistent with the compiled schema")]
    CorruptReferenceIndex,
    #[error("app persisted reference graph contains an unavailable target")]
    CorruptReferenceGraph,
    #[error("app reference cycle is denied by the compiled schema")]
    ReferenceCycleDenied,
    #[error("app reference cycle validation exceeded its bounded ceiling")]
    CycleCheckLimit,
    #[error("app mutation exceeds its granted storage ceiling")]
    StorageCeilingExceeded,
    #[error("app storage usage projection is corrupt")]
    CorruptStorageUsage,
    #[error("app storage usage projection overflowed")]
    StorageUsageOverflow,
    #[error("app mutation produced no committed record")]
    EmptyCommit,
    #[error("app mutation revision counter is exhausted")]
    RevisionExhausted,
    #[error("app mutation change sequence is exhausted")]
    SequenceExhausted,
    #[error("app mutation value exceeds SQLite's signed integer range")]
    SqlIntegerOverflow,
    #[error("app dataset generation is corrupt")]
    CorruptDatasetGeneration,
    #[error("server-derived app mutation key is corrupt")]
    CorruptMutationKey,
}

impl From<AppEntityIndexError> for AppEntityMutationError {
    fn from(_: AppEntityIndexError) -> Self {
        Self::Index
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        apps::{
            boundary::AppStoreAuthorityFence,
            entity_store::tests::{
                compiled_schema, package_revision_ref, seed_enabled_installation, seed_records,
            },
            lifecycle::AppInstallationStatus,
            models::{
                AppExpectedRecordRevision, AppMutationAtomicity, AppProtocolVersion,
                AppQueryRequest,
            },
            registry::{
                tests::{authenticated_scope, canonical_tempdir, time},
                AppRegistryService,
            },
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    fn surface_origin(id: &str) -> AppMutationOrigin {
        AppMutationOrigin::Surface {
            surface_session_id: AppReference::parse("surface:test").unwrap(),
            client_mutation_id: AppReference::parse(id).unwrap(),
        }
    }

    #[test]
    fn terminal_record_reservation_tracks_writes_and_cascade_expansion() {
        let mut command = update_command(
            "mutation:bound",
            &[
                ("record_a", 1, serde_json::json!({"title":"a"})),
                ("record_b", 1, serde_json::json!({"title":"b"})),
            ],
        );
        assert_eq!(mutation_record_write_upper_bound(&command), 2);
        command.operations.push(AppMutationOperation::Delete {
            entity: AppName::parse("item").unwrap(),
            record_id: AppRecordId::parse("record_c").unwrap(),
        });
        assert_eq!(
            mutation_record_write_upper_bound(&command),
            AppContractLimits::default().max_collection_items() as u64
        );
    }

    fn fence(
        command: &AppMutationCommand,
        origin: AppMutationOrigin,
        authenticated: &AuthenticatedAppScope,
    ) -> AppStoreAuthorityFence {
        AppStoreAuthorityFence::for_mutation_test(
            command,
            origin,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            AppInstallationId::parse("install_1").unwrap(),
            2,
            package_revision_ref(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        )
    }

    fn update_command(idempotency_key: &str, records: &[(&str, u64, Value)]) -> AppMutationCommand {
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse(idempotency_key).unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: records
                .iter()
                .map(|(record_id, _, patch)| AppMutationOperation::Update {
                    entity: AppName::parse("item").unwrap(),
                    record_id: AppRecordId::parse(*record_id).unwrap(),
                    patch: patch.clone(),
                })
                .collect(),
            expected_record_revisions: records
                .iter()
                .map(|(record_id, revision, _)| AppExpectedRecordRevision {
                    entity: AppName::parse("item").unwrap(),
                    record_id: AppRecordId::parse(*record_id).unwrap(),
                    revision: AppRevision::new(*revision).unwrap(),
                })
                .collect(),
        }
    }

    fn relation_command(
        idempotency_key: &str,
        relation: &str,
        from_record_id: &AppRecordId,
        to_record_id: &AppRecordId,
        from_revision: u64,
        to_revision: u64,
        create: bool,
    ) -> AppMutationCommand {
        let operation = if create {
            AppMutationOperation::CreateRelation {
                relation: AppName::parse(relation).unwrap(),
                from_record_id: from_record_id.clone(),
                to_record_id: to_record_id.clone(),
                expected_from_revision: AppRevision::new(from_revision).unwrap(),
                expected_to_revision: AppRevision::new(to_revision).unwrap(),
            }
        } else {
            AppMutationOperation::DeleteRelation {
                relation: AppName::parse(relation).unwrap(),
                from_record_id: from_record_id.clone(),
                to_record_id: to_record_id.clone(),
                expected_from_revision: AppRevision::new(from_revision).unwrap(),
                expected_to_revision: AppRevision::new(to_revision).unwrap(),
            }
        };
        AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse(idempotency_key).unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![operation],
            expected_record_revisions: Vec::new(),
        }
    }

    async fn seeded_store() -> (
        tempfile::TempDir,
        AppRegistryService,
        AppEntityStoreService,
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
        let store = AppEntityStoreService::new(registry.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        (temporary, registry, store, authenticated)
    }

    /// A package that owns a singleton has to be able to name the row it will
    /// later address by name. Behavior input selectors resolve their source
    /// record by id, and a host-minted `rec_<hex>` is unknowable to whoever
    /// wrote the manifest, so without a caller-chosen id no declared behavior
    /// can ever find its input.
    #[tokio::test]
    async fn a_create_may_name_its_record_but_never_inside_the_host_minted_namespace() {
        let (_temporary, _registry, store, authenticated) = seeded_store().await;

        let named = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:named-create").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("item").unwrap(),
                temporary_id: AppName::parse("the-singleton").unwrap(),
                record_id: Some(AppRecordId::parse("singleton").unwrap()),
                payload: serde_json::json!({"title": "Only one of me", "status": "open"}),
            }],
            expected_record_revisions: Vec::new(),
        };
        let origin = surface_origin("client:named-create");
        let receipt = store
            .mutate(
                &authenticated,
                fence(&named, origin.clone(), &authenticated),
                named.clone(),
                time(4),
            )
            .await
            .unwrap();
        // The whole point: the committed id is the one the caller asked for, so
        // a manifest can name it.
        assert!(
            receipt
                .committed_record_revisions
                .iter()
                .any(|committed| committed.record_id.as_str() == "singleton"),
            "a named create must commit under the name it chose, got {:?}",
            receipt.committed_record_revisions
        );

        // Replay stays exact: naming a row does not weaken idempotency.
        let replay = store
            .mutate(
                &authenticated,
                fence(&named, origin.clone(), &authenticated),
                named,
                time(5),
            )
            .await
            .unwrap();
        assert_eq!(replay, receipt);

        // A second, different command may not seize a name already taken. This
        // is the check that keeps "name your row" from becoming "overwrite any
        // row you can guess".
        let seize = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:seize-name").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("item").unwrap(),
                temporary_id: AppName::parse("impostor").unwrap(),
                record_id: Some(AppRecordId::parse("singleton").unwrap()),
                payload: serde_json::json!({"title": "Mine now", "status": "open"}),
            }],
            expected_record_revisions: Vec::new(),
        };
        let error = store
            .mutate(
                &authenticated,
                fence(&seize, surface_origin("client:seize-name"), &authenticated),
                seize,
                time(6),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppEntityMutationError::RecordAlreadyExists));

        // And the store's own namespace stays the store's, so a caller cannot
        // aim a create at an id the store is about to mint.
        let forged = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:forged-name").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Create {
                entity: AppName::parse("item").unwrap(),
                temporary_id: AppName::parse("forger").unwrap(),
                record_id: Some(AppRecordId::parse("rec_0123456789abcdef").unwrap()),
                payload: serde_json::json!({"title": "Looks host-minted", "status": "open"}),
            }],
            expected_record_revisions: Vec::new(),
        };
        let error = store
            .mutate(
                &authenticated,
                fence(
                    &forged,
                    surface_origin("client:forged-name"),
                    &authenticated,
                ),
                forged,
                time(7),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppEntityMutationError::ReservedRecordId));
    }

    #[tokio::test]
    async fn mutation_commits_revision_indexes_receipt_sequence_outbox_and_exact_replay() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let command = update_command(
            "mutation:update-a",
            &[("record_a", 1, serde_json::json!({"title": "Updated"}))],
        );
        let origin = surface_origin("client:update-a");
        let receipt = store
            .mutate(
                &authenticated,
                fence(&command, origin.clone(), &authenticated),
                command.clone(),
                time(4),
            )
            .await
            .unwrap();
        let replay = store
            .mutate(
                &authenticated,
                fence(&command, origin.clone(), &authenticated),
                command,
                time(5),
            )
            .await
            .unwrap();
        assert_eq!(replay, receipt);

        let conflicting = update_command(
            "mutation:update-a",
            &[("record_a", 1, serde_json::json!({"title": "Different"}))],
        );
        let error = store
            .mutate(
                &authenticated,
                fence(&conflicting, origin, &authenticated),
                conflicting,
                time(6),
            )
            .await
            .unwrap_err();
        assert!(matches!(error, AppEntityMutationError::IdempotencyConflict));

        let query = AppQueryRequest {
            pagination: Default::default(),
            protocol_version: AppProtocolVersion::V1,
            source_installation_id: AppInstallationId::parse("install_1").unwrap(),
            entity: AppName::parse("item").unwrap(),
            select: vec![AppFieldPath::parse("title").unwrap()],
            predicate: None,
            order: Vec::new(),
            cursor: None,
            limit: 10,
            relation_expansions: Vec::new(),
            purpose: AppName::parse("mutation-regression").unwrap(),
        };
        let query_fence = AppStoreAuthorityFence::for_store_test(
            &query,
            authenticated.scope_binding_ref().clone(),
            authenticated.authentication_revision(),
            2,
            package_revision_ref(),
            AppRevision::new(1).unwrap(),
            AppRevision::new(1).unwrap(),
        );
        let page = store
            .query(&authenticated, query_fence, query, time(7))
            .await
            .unwrap();
        assert_eq!(page.envelope.value.len(), 2);

        let persisted = registry
            .execute_scoped_read(&authenticated, &time(8), |connection, _| {
                let revision = connection.query_row(
                    "SELECT record_revision FROM app_record_heads
                     WHERE installation_id = 'install_1' AND entity_name = 'item'
                       AND record_id = 'record_a'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let title = connection.query_row(
                    "SELECT text_value FROM app_scalar_indexes
                     WHERE installation_id = 'install_1' AND entity_name = 'item'
                       AND record_id = 'record_a' AND field_path = 'title'",
                    [],
                    |row| row.get::<_, String>(0),
                )?;
                let generation = connection.query_row(
                    "SELECT current_generation FROM app_dataset_generations
                     WHERE installation_id = 'install_1'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let receipt_count = connection.query_row(
                    "SELECT COUNT(*) FROM app_mutation_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let outbox_count =
                    connection.query_row("SELECT COUNT(*) FROM app_entity_outbox", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
                Ok((revision, title, generation, receipt_count, outbox_count))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted, (2, "Updated".to_owned(), 2, 1, 1));
    }

    #[tokio::test]
    async fn stale_member_rolls_back_the_complete_mutation_batch() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let command = update_command(
            "mutation:stale-batch",
            &[
                (
                    "record_a",
                    1,
                    serde_json::json!({"title": "Must roll back"}),
                ),
                ("record_b", 2, serde_json::json!({"title": "Stale"})),
            ],
        );
        let error = store
            .mutate(
                &authenticated,
                fence(
                    &command,
                    surface_origin("client:stale-batch"),
                    &authenticated,
                ),
                command,
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppEntityMutationError::RecordRevisionConflict
        ));
        let persisted = registry
            .execute_scoped_read(&authenticated, &time(5), |connection, _| {
                let revision = connection.query_row(
                    "SELECT record_revision FROM app_record_heads
                     WHERE installation_id = 'install_1' AND entity_name = 'item'
                       AND record_id = 'record_a'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let receipts = connection.query_row(
                    "SELECT COUNT(*) FROM app_mutation_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let outbox =
                    connection.query_row("SELECT COUNT(*) FROM app_entity_outbox", [], |row| {
                        row.get::<_, i64>(0)
                    })?;
                Ok((revision, receipts, outbox))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(persisted, (1, 0, 0));
    }

    #[tokio::test]
    async fn relation_mutation_uses_endpoint_revisions_and_allows_declared_bounded_cycles() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:relation").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::CreateRelation {
                relation: AppName::parse("parent").unwrap(),
                from_record_id: AppRecordId::parse("record_a").unwrap(),
                to_record_id: AppRecordId::parse("record_b").unwrap(),
                expected_from_revision: AppRevision::new(1).unwrap(),
                expected_to_revision: AppRevision::new(1).unwrap(),
            }],
            expected_record_revisions: Vec::new(),
        };
        store
            .mutate(
                &authenticated,
                fence(&command, surface_origin("client:relation"), &authenticated),
                command,
                time(4),
            )
            .await
            .unwrap();
        let relation = registry
            .execute_scoped_read(&authenticated, &time(5), |connection, _| {
                connection
                    .query_row(
                        "SELECT text_value FROM app_scalar_indexes
                         WHERE installation_id = 'install_1' AND entity_name = 'item'
                           AND record_id = 'record_a' AND field_path = 'parent'",
                        [],
                        |row| row.get::<_, String>(0),
                    )
                    .map_err(AppRegistryError::from)
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(relation, "record_b");
    }

    #[tokio::test]
    async fn nullify_and_cascade_delete_policies_commit_in_the_same_receipt() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let item_a = AppRecordId::parse("record_a").unwrap();
        let item_b = AppRecordId::parse("record_b").unwrap();
        let detach = relation_command(
            "mutation:detach-restrict-edge",
            "parent",
            &item_b,
            &item_a,
            1,
            1,
            false,
        );
        store
            .mutate(
                &authenticated,
                fence(
                    &detach,
                    surface_origin("client:detach-restrict-edge"),
                    &authenticated,
                ),
                detach,
                time(4),
            )
            .await
            .unwrap();

        let create_dependants = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:create-dependants").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![
                AppMutationOperation::Create {
                    entity: AppName::parse("note").unwrap(),
                    temporary_id: AppName::parse("note-one").unwrap(),
                    record_id: None,
                    payload: serde_json::json!({"body": "Keep me", "item": "record_a"}),
                },
                AppMutationOperation::Create {
                    entity: AppName::parse("branch").unwrap(),
                    temporary_id: AppName::parse("branch-one").unwrap(),
                    record_id: None,
                    payload: serde_json::json!({"label": "Delete me", "item": "record_a"}),
                },
            ],
            expected_record_revisions: Vec::new(),
        };
        let created = store
            .mutate(
                &authenticated,
                fence(
                    &create_dependants,
                    surface_origin("client:create-dependants"),
                    &authenticated,
                ),
                create_dependants,
                time(5),
            )
            .await
            .unwrap();
        let note_id = created
            .committed_record_revisions
            .iter()
            .find(|record| record.entity.as_str() == "note")
            .unwrap()
            .record_id
            .clone();
        let branch_id = created
            .committed_record_revisions
            .iter()
            .find(|record| record.entity.as_str() == "branch")
            .unwrap()
            .record_id
            .clone();

        let delete = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:delete-policy-owner").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Delete {
                entity: AppName::parse("item").unwrap(),
                record_id: item_a,
            }],
            expected_record_revisions: vec![AppExpectedRecordRevision {
                entity: AppName::parse("item").unwrap(),
                record_id: AppRecordId::parse("record_a").unwrap(),
                revision: AppRevision::new(1).unwrap(),
            }],
        };
        let receipt = store
            .mutate(
                &authenticated,
                fence(
                    &delete,
                    surface_origin("client:delete-policy-owner"),
                    &authenticated,
                ),
                delete,
                time(6),
            )
            .await
            .unwrap();
        assert_eq!(receipt.committed_record_revisions.len(), 3);

        let note_id_for_read = note_id.clone();
        let branch_id_for_read = branch_id.clone();
        let state = registry
            .execute_scoped_read(&authenticated, &time(7), move |connection, _| {
                let note_payload = connection.query_row(
                    "SELECT r.payload_json FROM app_record_heads h
                     JOIN app_record_revisions r
                       ON r.installation_id = h.installation_id
                      AND r.entity_name = h.entity_name
                      AND r.record_id = h.record_id
                      AND r.record_revision = h.record_revision
                     WHERE h.installation_id = 'install_1' AND h.entity_name = 'note'
                       AND h.record_id = ?1",
                    params![note_id_for_read.as_str()],
                    |row| row.get::<_, Vec<u8>>(0),
                )?;
                let branch_deleted = connection.query_row(
                    "SELECT deleted_at FROM app_record_heads
                     WHERE installation_id = 'install_1' AND entity_name = 'branch'
                       AND record_id = ?1",
                    params![branch_id_for_read.as_str()],
                    |row| row.get::<_, Option<String>>(0),
                )?;
                Ok((note_payload, branch_deleted))
            })
            .await
            .unwrap()
            .unwrap();
        let note_payload: Value = serde_json::from_slice(&state.0).unwrap();
        assert!(note_payload["item"].is_null());
        assert!(state.1.is_some());
    }

    #[tokio::test]
    async fn denied_reference_cycle_rolls_back_relation_revision_and_receipt() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let create_nodes = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:create-nodes").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![
                AppMutationOperation::Create {
                    entity: AppName::parse("node").unwrap(),
                    temporary_id: AppName::parse("node-a").unwrap(),
                    record_id: None,
                    payload: serde_json::json!({"label": "A"}),
                },
                AppMutationOperation::Create {
                    entity: AppName::parse("node").unwrap(),
                    temporary_id: AppName::parse("node-b").unwrap(),
                    record_id: None,
                    payload: serde_json::json!({"label": "B"}),
                },
            ],
            expected_record_revisions: Vec::new(),
        };
        let created = store
            .mutate(
                &authenticated,
                fence(
                    &create_nodes,
                    surface_origin("client:create-nodes"),
                    &authenticated,
                ),
                create_nodes,
                time(4),
            )
            .await
            .unwrap();
        let node_a = created.committed_record_revisions[0].record_id.clone();
        let node_b = created.committed_record_revisions[1].record_id.clone();
        let first_edge =
            relation_command("mutation:node-edge-a", "next", &node_a, &node_b, 1, 1, true);
        store
            .mutate(
                &authenticated,
                fence(
                    &first_edge,
                    surface_origin("client:node-edge-a"),
                    &authenticated,
                ),
                first_edge,
                time(5),
            )
            .await
            .unwrap();
        let closing_edge =
            relation_command("mutation:node-edge-b", "next", &node_b, &node_a, 1, 2, true);
        let error = store
            .mutate(
                &authenticated,
                fence(
                    &closing_edge,
                    surface_origin("client:node-edge-b"),
                    &authenticated,
                ),
                closing_edge,
                time(6),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppEntityMutationError::ReferenceCycleDenied
        ));

        let node_b_for_read = node_b.clone();
        let state = registry
            .execute_scoped_read(&authenticated, &time(7), move |connection, _| {
                let (revision, payload): (i64, Vec<u8>) = connection.query_row(
                    "SELECT h.record_revision, r.payload_json FROM app_record_heads h
                     JOIN app_record_revisions r
                       ON r.installation_id = h.installation_id
                      AND r.entity_name = h.entity_name
                      AND r.record_id = h.record_id
                      AND r.record_revision = h.record_revision
                     WHERE h.installation_id = 'install_1' AND h.entity_name = 'node'
                       AND h.record_id = ?1",
                    params![node_b_for_read.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let receipt_count = connection.query_row(
                    "SELECT COUNT(*) FROM app_mutation_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((revision, payload, receipt_count))
            })
            .await
            .unwrap()
            .unwrap();
        let payload: Value = serde_json::from_slice(&state.1).unwrap();
        assert_eq!(state.0, 1);
        assert!(payload.get("next").is_none());
        assert_eq!(state.2, 2);
    }

    #[tokio::test]
    async fn restrict_delete_policy_rejects_without_durable_side_effects() {
        let (_temporary, registry, store, authenticated) = seeded_store().await;
        let command = AppMutationCommand {
            protocol_version: AppProtocolVersion::V1,
            idempotency_key: AppReference::parse("mutation:delete-referenced").unwrap(),
            atomicity: AppMutationAtomicity::AllOrNothing,
            expected_schema_revision: AppRevision::new(1).unwrap(),
            operations: vec![AppMutationOperation::Delete {
                entity: AppName::parse("item").unwrap(),
                record_id: AppRecordId::parse("record_a").unwrap(),
            }],
            expected_record_revisions: vec![AppExpectedRecordRevision {
                entity: AppName::parse("item").unwrap(),
                record_id: AppRecordId::parse("record_a").unwrap(),
                revision: AppRevision::new(1).unwrap(),
            }],
        };
        let error = store
            .mutate(
                &authenticated,
                fence(
                    &command,
                    surface_origin("client:delete-referenced"),
                    &authenticated,
                ),
                command,
                time(4),
            )
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            AppEntityMutationError::ReferencedRecordDeleteDenied
        ));
        let durable_counts = registry
            .execute_scoped_read(&authenticated, &time(5), |connection, _| {
                let live = connection.query_row(
                    "SELECT COUNT(*) FROM app_record_heads
                     WHERE installation_id = 'install_1' AND deleted_at IS NULL",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let receipts = connection.query_row(
                    "SELECT COUNT(*) FROM app_mutation_receipts",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                Ok((live, receipts))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(durable_counts, (2, 0));
    }
}

//! Route-free Phase-2D data portability over the canonical app entity store.
//!
//! Export intentionally needs user-scope authentication rather than live app
//! authority so disabled, quarantined and retained installations cannot hold a
//! person's data hostage. Import remains bound to an enabled destination, a
//! server-produced preview and a non-deserializable reviewed approval. Source
//! scope, record, actor and execution identities never cross the archive seam.

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use serde_json::Value;
use thiserror::Error;

use super::{
    authority::AuthenticatedAppScope,
    entity_mutation::{
        advance_dataset_generation, next_revision, persist_storage_usage, project_storage_usage,
        reserve_change_sequences, to_sql_u64, validate_denied_reference_cycles,
        validate_reference_targets, write_record_revision, AppEntityMutationError, WorkingRecord,
    },
    entity_store::{
        resolve_active_schema, ActiveAppEntitySchema, AppEntityStoreError, AppEntityStoreService,
    },
    lifecycle::AppInstallationStatus,
    models::{
        decode_app_contract, decode_bounded_json_value, validate_json_value, AppContractError,
        AppContractLimits, AppDigest, AppInstallationId, AppName, AppRecordId, AppReference,
        AppRevision, ValidateAppContract,
    },
    portability::{
        resolve_import_replay, AppApprovedDataImport, AppDataArchiveManifest,
        AppDataImportCompatibility, AppDataImportPreview, AppDataImportPreviewStatus,
        AppDataImportReceipt, AppDataImportRecordDecision, AppExportSourceState,
        AppImportReplayDecision, AppMissingImportAttachment, AppPortabilityError, AppPortableAlias,
        AppPortableAliasKind, AppPortableProvenance, AppPortableRecordRevision,
        AppStoredDataImportReceipt, CurrentAppImportDestination,
        APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_BYTES, APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_NODES,
    },
    records::{
        validate_policy, AppDataHandlingPolicy, AppInstallation, AppPackageRevision,
        AppRecordProvenance, AppRecordRevision, AppSchemaRevision, AppScope,
    },
    registry::{AppRegistryError, AppRegistryService},
    schema_compiler::{
        runtime_contracts_from_revision, source_entity_schema_digest_from_revision,
        AppEntityRuntimeContract, AppSchemaCompilerError,
    },
};

const PORTABLE_RECORD_TOKEN_PREFIX: &str = "portable-record-";
const MAX_EXPORT_RECORD_METADATA_BYTES: usize = 16 * 1_024 * 1_024;

#[derive(Debug, Clone)]
pub struct AppDataPortabilityService {
    store: AppEntityStoreService,
}

impl AppDataPortabilityService {
    pub fn new(registry: AppRegistryService) -> Self {
        Self {
            store: AppEntityStoreService::new(registry),
        }
    }

    /// Export current live personal data without exporting installation scope,
    /// grants, credentials, schedules, memory or provider state.
    pub async fn export_data(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<AppDataArchiveManifest, AppEntityPortabilityError> {
        let installation_id = installation_id.clone();
        self.store
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(export_data_blocking(connection, scope, &installation_id))
            })
            .await?
            .ok_or(AppEntityPortabilityError::MissingScopedStore)?
    }

    /// Build a destination-local exact-compatibility preview. Every prospective
    /// identity is derived from destination + archive + alias; a collision is
    /// surfaced for review rather than merged heuristically.
    pub async fn preview_import(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        destination_installation_id: &AppInstallationId,
        source: &AppDataArchiveManifest,
        now: DateTime<Utc>,
    ) -> Result<AppDataImportPreview, AppEntityPortabilityError> {
        source.validate_app_contract(&AppContractLimits::default())?;
        let destination_installation_id = destination_installation_id.clone();
        let source = source.clone();
        let authenticated = authenticated_scope.clone();
        self.store
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, scope| {
                Ok(preview_import_blocking(
                    connection,
                    scope,
                    &authenticated,
                    &destination_installation_id,
                    &source,
                    now,
                ))
            })
            .await?
            .ok_or(AppEntityPortabilityError::MissingScopedStore)?
    }

    pub fn approve_import(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        approval_ref: AppReference,
        preview: &AppDataImportPreview,
        issued_at: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<AppApprovedDataImport, AppEntityPortabilityError> {
        Ok(AppApprovedDataImport::from_reviewed_preview(
            authenticated_scope,
            approval_ref,
            preview,
            issued_at,
            expires_at,
            &AppContractLimits::default(),
        )?)
    }

    /// Read a previously committed receipt for transport-level response-loss
    /// recovery. The receipt is scoped evidence only: it cannot recreate an
    /// approval or authorize a new import, and every identity is checked
    /// against the authenticated destination before it is returned.
    pub async fn committed_import_receipt(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        destination_installation_id: &AppInstallationId,
        preview_digest: &AppDigest,
        now: DateTime<Utc>,
    ) -> Result<Option<AppDataImportReceipt>, AppEntityPortabilityError> {
        let authenticated = authenticated_scope.clone();
        let destination_installation_id = destination_installation_id.clone();
        let preview_digest = preview_digest.clone();
        self.store
            .registry
            .execute_scoped_read(authenticated_scope, &now, move |connection, _scope| {
                Ok(committed_import_receipt_blocking(
                    connection,
                    &authenticated,
                    &destination_installation_id,
                    &preview_digest,
                ))
            })
            .await?
            .ok_or(AppEntityPortabilityError::MissingScopedStore)?
    }

    /// Commit only the exact server preview under its single reviewed approval.
    /// Import receipts and projection events share the record transaction.
    pub async fn commit_import(
        &self,
        authenticated_scope: &AuthenticatedAppScope,
        source: AppDataArchiveManifest,
        preview: AppDataImportPreview,
        approval: AppApprovedDataImport,
        now: DateTime<Utc>,
    ) -> Result<AppDataImportReceipt, AppEntityPortabilityError> {
        let authenticated = authenticated_scope.clone();
        let result = self
            .store
            .registry
            .execute_scoped_write(authenticated_scope, &now, move |connection, scope| {
                Ok(commit_import_blocking(
                    connection,
                    scope,
                    &authenticated,
                    source,
                    preview,
                    approval,
                    now,
                ))
            })
            .await?;
        result
    }
}

#[derive(Debug)]
struct ExportSource {
    installation: AppInstallation,
    package: AppPackageRevision,
    schema: AppSchemaRevision,
    runtime: Arc<BTreeMap<AppName, AppEntityRuntimeContract>>,
    source_state: AppExportSourceState,
}

fn export_data_blocking(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<AppDataArchiveManifest, AppEntityPortabilityError> {
    let source = resolve_export_source(connection, scope, installation_id)?;
    let records = load_live_export_records(connection, &source)?;
    if records.is_empty() {
        return Err(AppEntityPortabilityError::EmptyDataSet);
    }

    let aliases = records
        .iter()
        .enumerate()
        .map(|(index, record)| {
            Ok((
                (record.entity_name.clone(), record.record_id.clone()),
                portable_alias(AppPortableAliasKind::Record, index)?,
            ))
        })
        .collect::<Result<BTreeMap<_, _>, AppEntityPortabilityError>>()?;
    let mut actor_aliases = BTreeMap::<AppReference, AppPortableAlias>::new();
    let mut execution_aliases = BTreeMap::<AppReference, AppPortableAlias>::new();
    let mut portable = Vec::with_capacity(records.len());
    for mut record in records {
        let runtime = source.runtime.get(&record.entity_name).ok_or_else(|| {
            AppEntityPortabilityError::UnknownEntity(record.entity_name.to_string())
        })?;
        rewrite_references_for_export(&mut record.payload, runtime, &aliases)?;
        runtime.validate_payload(&record.payload)?;
        let record_alias = aliases
            .get(&(record.entity_name.clone(), record.record_id.clone()))
            .copied()
            .ok_or(AppEntityPortabilityError::CorruptRecord)?;
        let actor_alias = alias_for_reference(
            &mut actor_aliases,
            &record.provenance.actor_id,
            AppPortableAliasKind::Actor,
        )?;
        let execution_alias = record
            .provenance
            .execution_id
            .as_ref()
            .map(|reference| {
                alias_for_reference(
                    &mut execution_aliases,
                    reference,
                    AppPortableAliasKind::Execution,
                )
            })
            .transpose()?;
        let payload_digest = AppDigest::blake3_canonical_json(&record.payload)?;
        let classification = record
            .handling_override
            .as_ref()
            .map(|policy| policy.classification_floor)
            .unwrap_or(
                source
                    .schema
                    .canonical_data_handling_policy
                    .classification_floor,
            );
        portable.push(AppPortableRecordRevision {
            record_alias,
            entity_name: record.entity_name,
            record_revision: record.record_revision,
            schema_revision: record.schema_revision,
            payload: record.payload,
            payload_digest,
            classification,
            created_at: record.created_at,
            updated_at: record.updated_at,
            deleted_at: None,
            provenance: AppPortableProvenance {
                actor_kind: record.provenance.actor_kind,
                actor_alias: Some(actor_alias),
                execution_alias,
                source_aliases: Vec::new(),
            },
        });
    }
    AppDataArchiveManifest::from_trusted_export_projection(
        source.package.package_id,
        source.package.content_digest,
        source.package.entity_schema_digest,
        source.source_state,
        portable,
        Vec::new(),
        &AppContractLimits::default(),
    )
    .map_err(AppEntityPortabilityError::from)
}

fn resolve_export_source(
    connection: &Connection,
    scope: &AppScope,
    installation_id: &AppInstallationId,
) -> Result<ExportSource, AppEntityPortabilityError> {
    let installation_bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_installations
             WHERE installation_id = ?1 AND principal = ?2 AND workspace = ?3",
            params![
                installation_id.as_str(),
                scope.principal.as_str(),
                scope.workspace.as_str(),
            ],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityPortabilityError::MissingInstallation)?;
    let installation: AppInstallation =
        decode_app_contract(&installation_bytes, &AppContractLimits::default())?;
    if installation.scope != *scope || installation.installation_id != *installation_id {
        return Err(AppEntityPortabilityError::ScopeMismatch);
    }
    let source_state = export_source_state(&installation)?;
    let package_bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
            params![installation.package_revision_ref.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityPortabilityError::MissingPackage)?;
    let package: AppPackageRevision =
        decode_app_contract(&package_bytes, &AppContractLimits::default())?;
    let schema_revision = installation
        .active_schema_revision
        .ok_or(AppEntityPortabilityError::MissingSchema)?;
    let schema_bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_schema_revisions
             WHERE installation_id = ?1 AND revision = ?2",
            params![installation_id.as_str(), to_sql_revision(schema_revision)?],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityPortabilityError::MissingSchema)?;
    let schema: AppSchemaRevision =
        decode_app_contract(&schema_bytes, &AppContractLimits::default())?;
    if schema.installation_id != *installation_id
        || schema.package_revision_ref != installation.package_revision_ref
        || source_entity_schema_digest_from_revision(&schema)? != package.entity_schema_digest
    {
        return Err(AppEntityPortabilityError::CorruptSchemaBinding);
    }
    let runtime = runtime_contracts_from_revision(&schema)?;
    Ok(ExportSource {
        installation,
        package,
        schema,
        runtime,
        source_state,
    })
}

fn export_source_state(
    installation: &AppInstallation,
) -> Result<AppExportSourceState, AppEntityPortabilityError> {
    match installation.lifecycle.status {
        AppInstallationStatus::Enabled => Ok(AppExportSourceState::Enabled),
        AppInstallationStatus::Disabled => Ok(AppExportSourceState::Disabled),
        AppInstallationStatus::Quarantined => Ok(AppExportSourceState::Quarantined),
        AppInstallationStatus::UninstalledRetained => Ok(AppExportSourceState::UninstalledRetained),
        AppInstallationStatus::UpdatePending => match installation.lifecycle.update_return_status {
            Some(super::lifecycle::AppStableOperationalStatus::Enabled) => {
                Ok(AppExportSourceState::Enabled)
            },
            Some(super::lifecycle::AppStableOperationalStatus::Disabled) => {
                Ok(AppExportSourceState::Disabled)
            },
            None => Err(AppEntityPortabilityError::UnsupportedSourceState),
        },
        AppInstallationStatus::Purged => Ok(AppExportSourceState::Purged),
        AppInstallationStatus::ReadyForReview => {
            Err(AppEntityPortabilityError::UnsupportedSourceState)
        },
    }
}

fn load_live_export_records(
    connection: &Connection,
    source: &ExportSource,
) -> Result<Vec<AppRecordRevision>, AppEntityPortabilityError> {
    let limit = super::portability::APP_MAX_DATA_ARCHIVE_RECORDS
        .checked_add(1)
        .ok_or(AppEntityPortabilityError::ExportLimit)?;
    let mut statement = connection.prepare(
        "SELECT r.entity_name, r.record_id, r.record_revision,
                r.dataset_generation, r.schema_revision, r.payload_digest,
                r.payload_json, r.handling_policy_digest,
                r.handling_policy_json, r.provenance_json,
                r.created_at, r.updated_at, r.deleted_at
         FROM app_record_heads h
         JOIN app_record_revisions r
           ON r.installation_id = h.installation_id
          AND r.entity_name = h.entity_name
          AND r.record_id = h.record_id
          AND r.record_revision = h.record_revision
         WHERE h.installation_id = ?1 AND h.deleted_at IS NULL
         ORDER BY r.entity_name ASC, r.record_id ASC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![
            source.installation.installation_id.as_str(),
            i64::try_from(limit).map_err(|_| AppEntityPortabilityError::ExportLimit)?
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Vec<u8>>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, Vec<u8>>(8)?,
                row.get::<_, Vec<u8>>(9)?,
                row.get::<_, String>(10)?,
                row.get::<_, String>(11)?,
                row.get::<_, Option<String>>(12)?,
            ))
        },
    )?;
    let mut records = Vec::new();
    let mut aggregate_payload_bytes = 0usize;
    let mut aggregate_payload_nodes = 0usize;
    let mut aggregate_metadata_bytes = 0usize;
    for row in rows {
        let (
            entity,
            record_id,
            record_revision,
            dataset_generation,
            schema_revision,
            stored_payload_digest,
            payload_bytes,
            stored_policy_digest,
            policy_bytes,
            provenance_bytes,
            created_at,
            updated_at,
            deleted_at,
        ) = row?;
        aggregate_payload_bytes = aggregate_payload_bytes
            .checked_add(payload_bytes.len())
            .ok_or(AppEntityPortabilityError::ExportLimit)?;
        aggregate_metadata_bytes = aggregate_metadata_bytes
            .checked_add(policy_bytes.len())
            .and_then(|bytes| bytes.checked_add(provenance_bytes.len()))
            .ok_or(AppEntityPortabilityError::ExportLimit)?;
        if aggregate_payload_bytes > APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_BYTES
            || aggregate_metadata_bytes > MAX_EXPORT_RECORD_METADATA_BYTES
        {
            return Err(AppEntityPortabilityError::ExportLimit);
        }
        let payload = decode_bounded_json_value(&payload_bytes, &AppContractLimits::default())?;
        let (payload_nodes, _) = validate_json_value(&payload, &AppContractLimits::default())?;
        aggregate_payload_nodes = aggregate_payload_nodes
            .checked_add(payload_nodes)
            .ok_or(AppEntityPortabilityError::ExportLimit)?;
        if aggregate_payload_nodes > APP_MAX_DATA_ARCHIVE_RECORD_PAYLOAD_NODES {
            return Err(AppEntityPortabilityError::ExportLimit);
        }
        if AppDigest::blake3_canonical_json(&payload)? != AppDigest::parse(stored_payload_digest)? {
            return Err(AppEntityPortabilityError::CorruptRecord);
        }
        let policy_value = decode_bounded_json_value(&policy_bytes, &AppContractLimits::default())?;
        let policy: AppDataHandlingPolicy = serde_json::from_value(policy_value)?;
        validate_policy(&policy, &AppContractLimits::default())?;
        if AppDigest::blake3_canonical_json(&serde_json::to_value(&policy)?)?
            != AppDigest::parse(stored_policy_digest)?
        {
            return Err(AppEntityPortabilityError::CorruptRecord);
        }
        let provenance_value =
            decode_bounded_json_value(&provenance_bytes, &AppContractLimits::default())?;
        let provenance: AppRecordProvenance = serde_json::from_value(provenance_value)?;
        let record = AppRecordRevision {
            installation_id: source.installation.installation_id.clone(),
            entity_name: AppName::parse(entity)?,
            record_id: AppRecordId::parse(record_id)?,
            record_revision: revision_from_sql(record_revision)?,
            dataset_generation: positive_u64(dataset_generation)?,
            schema_revision: revision_from_sql(schema_revision)?,
            payload,
            handling_override: Some(policy),
            created_at: parse_timestamp(&created_at)?,
            updated_at: parse_timestamp(&updated_at)?,
            deleted_at: deleted_at.as_deref().map(parse_timestamp).transpose()?,
            provenance,
        };
        record.validate_app_contract(&AppContractLimits::default())?;
        source
            .runtime
            .get(&record.entity_name)
            .ok_or_else(|| {
                AppEntityPortabilityError::UnknownEntity(record.entity_name.to_string())
            })?
            .validate_payload(&record.payload)?;
        records.push(record);
    }
    if records.len() >= limit {
        return Err(AppEntityPortabilityError::ExportLimit);
    }
    Ok(records)
}

fn preview_import_blocking(
    connection: &Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    installation_id: &AppInstallationId,
    source: &AppDataArchiveManifest,
    now: DateTime<Utc>,
) -> Result<AppDataImportPreview, AppEntityPortabilityError> {
    let active = resolve_active_schema(connection, scope, installation_id)
        .map_err(AppEntityPortabilityError::Store)?
        .ok_or(AppEntityPortabilityError::MissingInstallation)?;
    let destination =
        current_import_destination(connection, authenticated_scope, &active, now.clone())?;
    let package: AppPackageRevision = load_package(connection, active.package_revision_ref())?;
    let compatibility = if package.package_id == source.package_id
        && package.content_digest == source.package_content_digest
        && package.entity_schema_digest == source.entity_schema_digest
    {
        AppDataImportCompatibility::Exact {
            package_content_digest: package.content_digest.clone(),
            entity_schema_digest: package.entity_schema_digest.clone(),
        }
    } else {
        AppDataImportCompatibility::Incompatible {
            reason_ref: AppReference::parse("app-import:incompatible-package-or-schema")?,
        }
    };
    let mut decisions = Vec::with_capacity(source.records.len());
    for source_record in &source.records {
        let record_id = imported_record_id(
            installation_id,
            &source.logical_payload_digest,
            source_record,
        )?;
        let exists: bool = connection.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM app_record_heads
                 WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3
             )",
            params![
                installation_id.as_str(),
                source_record.entity_name.as_str(),
                record_id.as_str()
            ],
            |row| row.get(0),
        )?;
        decisions.push(if exists {
            AppDataImportRecordDecision::Conflict {
                record_alias: source_record.record_alias,
                existing_local_record_id: record_id,
            }
        } else {
            AppDataImportRecordDecision::Create {
                record_alias: source_record.record_alias,
                new_local_record_id: record_id,
            }
        });
    }
    let missing_attachments = source
        .attachments
        .iter()
        .map(|attachment| AppMissingImportAttachment {
            attachment_alias: attachment.attachment_alias,
            expected_content_digest: attachment.content_digest.clone(),
        })
        .collect();
    let import_batch_key = import_batch_key(
        installation_id,
        active.installation_generation(),
        &source.logical_payload_digest,
    )?;
    AppDataImportPreview::from_trusted_import_planner(
        import_batch_key,
        source,
        &destination,
        authenticated_scope,
        now,
        compatibility,
        decisions,
        missing_attachments,
        &AppContractLimits::default(),
    )
    .map_err(AppEntityPortabilityError::from)
}

#[allow(clippy::too_many_arguments)]
fn commit_import_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated_scope: &AuthenticatedAppScope,
    source: AppDataArchiveManifest,
    preview: AppDataImportPreview,
    approval: AppApprovedDataImport,
    now: DateTime<Utc>,
) -> Result<AppDataImportReceipt, AppEntityPortabilityError> {
    source.validate_app_contract(&AppContractLimits::default())?;
    preview.validate_app_contract(&AppContractLimits::default())?;
    approval.ensure_matches_preview(&preview)?;
    if source.logical_payload_digest != preview.source_archive_digest
        || source.package_content_digest != preview.source_package_content_digest
        || source.entity_schema_digest != preview.source_schema_digest
        || usize::try_from(preview.source_record_count).ok() != Some(source.records.len())
        || preview.status == AppDataImportPreviewStatus::Blocked
    {
        return Err(AppEntityPortabilityError::PreviewSourceMismatch);
    }
    let transaction = connection.transaction()?;
    let active = resolve_active_schema(&transaction, scope, &preview.destination_installation_id)
        .map_err(AppEntityPortabilityError::Store)?
        .ok_or(AppEntityPortabilityError::MissingInstallation)?;
    if active.installation_generation() != preview.destination_installation_generation {
        return Err(AppEntityPortabilityError::DestinationGenerationConflict);
    }
    let destination =
        current_import_destination(&transaction, authenticated_scope, &active, now.clone())?;
    let existing = load_stored_import_receipt(
        &transaction,
        &destination,
        authenticated_scope,
        &preview,
        now.clone(),
    )?;
    match resolve_import_replay(
        &approval,
        &source.logical_payload_digest,
        &destination,
        existing.as_ref(),
        authenticated_scope,
        now.clone(),
    )? {
        AppImportReplayDecision::Replay(receipt) => {
            let receipt = receipt.clone();
            transaction.commit()?;
            return Ok(receipt);
        },
        AppImportReplayDecision::New => {},
    }

    let receipt_ref = import_receipt_ref(&preview.import_batch_key)?;
    let source_by_alias = source
        .records
        .iter()
        .map(|record| (record.record_alias, record))
        .collect::<HashMap<_, _>>();
    let local_ids = preview
        .record_decisions
        .iter()
        .filter_map(|decision| match decision {
            AppDataImportRecordDecision::Create {
                record_alias,
                new_local_record_id,
            } => Some((*record_alias, new_local_record_id.clone())),
            AppDataImportRecordDecision::Merge { .. }
            | AppDataImportRecordDecision::Conflict { .. }
            | AppDataImportRecordDecision::RejectedSensitiveFields { .. } => None,
        })
        .collect::<HashMap<_, _>>();
    let mut working = BTreeMap::new();
    let mut provenance_by_record = BTreeMap::new();
    let mut skipped_count = 0u32;
    for decision in &preview.record_decisions {
        match decision {
            AppDataImportRecordDecision::Create {
                record_alias,
                new_local_record_id,
            } => {
                let source_record = source_by_alias
                    .get(record_alias)
                    .copied()
                    .ok_or(AppEntityPortabilityError::IncompletePreview)?;
                let expected_id = imported_record_id(
                    active.installation_id(),
                    &source.logical_payload_digest,
                    source_record,
                )?;
                if &expected_id != new_local_record_id
                    || record_exists(
                        &transaction,
                        active.installation_id(),
                        &source_record.entity_name,
                        new_local_record_id,
                    )?
                {
                    return Err(AppEntityPortabilityError::ImportIdentityConflict);
                }
                if source_record.created_at > now || source_record.updated_at > now {
                    return Err(AppEntityPortabilityError::FutureSourceTimestamp);
                }
                let runtime = active
                    .runtime_contract(&source_record.entity_name)
                    .ok_or_else(|| {
                        AppEntityPortabilityError::UnknownEntity(
                            source_record.entity_name.to_string(),
                        )
                    })?;
                let mut payload = source_record.payload.clone();
                rewrite_references_for_import(&mut payload, runtime, &source_by_alias, &local_ids)?;
                runtime.validate_payload(&payload)?;
                let mut policy = active.schema().canonical_data_handling_policy.clone();
                policy.classification_floor = policy
                    .classification_floor
                    .max(source_record.classification);
                validate_policy(&policy, &AppContractLimits::default())?;
                let key = (
                    source_record.entity_name.clone(),
                    new_local_record_id.clone(),
                );
                working.insert(
                    key.clone(),
                    WorkingRecord {
                        entity: source_record.entity_name.clone(),
                        record_id: new_local_record_id.clone(),
                        prior_revision: None,
                        created_at: source_record.created_at.clone(),
                        payload,
                        handling_policy: policy,
                        was_deleted: false,
                        deleted: false,
                    },
                );
                provenance_by_record.insert(
                    key,
                    imported_provenance(
                        source_record,
                        active.installation_id(),
                        &source.logical_payload_digest,
                        &local_ids,
                        &receipt_ref,
                    )?,
                );
            },
            AppDataImportRecordDecision::Conflict { .. }
            | AppDataImportRecordDecision::RejectedSensitiveFields { .. } => {
                skipped_count = skipped_count
                    .checked_add(1)
                    .ok_or(AppEntityPortabilityError::CounterOverflow)?;
            },
            AppDataImportRecordDecision::Merge { .. } => {
                return Err(AppEntityPortabilityError::ReviewedMergeExecutorUnavailable)
            },
        }
    }
    for record in working.values() {
        validate_reference_targets(&transaction, &active, record, &working)?;
    }
    validate_denied_reference_cycles(&transaction, &active, &working)?;

    let mut committed = Vec::with_capacity(working.len());
    let sequence_range = if working.is_empty() {
        None
    } else {
        let generation =
            advance_dataset_generation(&transaction, active.installation_id(), now.clone())?;
        let usage = project_storage_usage(&transaction, &active, &working)?;
        let first =
            reserve_change_sequences(&transaction, active.installation_id(), working.len())?;
        let mut sequence = first;
        for (key, record) in &working {
            let revision = next_revision(None)?;
            let provenance = provenance_by_record
                .get(key)
                .ok_or(AppEntityPortabilityError::IncompletePreview)?;
            write_record_revision(
                &transaction,
                &active,
                record,
                revision,
                generation,
                sequence,
                provenance,
                now.clone(),
            )?;
            committed.push(ImportedRecordCommit {
                entity: record.entity.clone(),
                record_id: record.record_id.clone(),
                revision,
            });
            sequence = sequence
                .checked_add(1)
                .ok_or(AppEntityPortabilityError::CounterOverflow)?;
        }
        persist_storage_usage(&transaction, active.installation_id(), usage, now.clone())?;
        Some((
            first,
            sequence
                .checked_sub(1)
                .ok_or(AppEntityPortabilityError::CounterOverflow)?,
        ))
    };
    let created_count =
        u32::try_from(working.len()).map_err(|_| AppEntityPortabilityError::CounterOverflow)?;
    let receipt = approval.build_committed_receipt(
        &preview,
        receipt_ref,
        created_count,
        0,
        skipped_count,
        now.clone(),
        &AppContractLimits::default(),
    )?;
    persist_import_receipt(&transaction, &receipt)?;
    if let Some((first, last)) = sequence_range {
        append_import_outbox(&transaction, &receipt, &committed, first, last, now)?;
    }
    transaction.commit()?;
    Ok(receipt)
}

fn current_import_destination(
    connection: &Connection,
    authenticated_scope: &AuthenticatedAppScope,
    active: &ActiveAppEntitySchema,
    now: DateTime<Utc>,
) -> Result<CurrentAppImportDestination, AppEntityPortabilityError> {
    let store_revision = current_store_revision(connection, active.installation_id())?;
    let package = load_package(connection, active.package_revision_ref())?;
    CurrentAppImportDestination::from_trusted_import_planner(
        authenticated_scope,
        store_revision,
        active.installation_id().clone(),
        active.installation_generation(),
        package.content_digest,
        package.entity_schema_digest,
        now,
    )
    .map_err(AppEntityPortabilityError::from)
}

fn current_store_revision(
    connection: &Connection,
    installation_id: &AppInstallationId,
) -> Result<AppRevision, AppEntityPortabilityError> {
    let generation: Option<i64> = connection
        .query_row(
            "SELECT current_generation FROM app_dataset_generations
             WHERE installation_id = ?1",
            params![installation_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    AppRevision::new(generation.map(positive_u64).transpose()?.unwrap_or(1))
        .map_err(AppEntityPortabilityError::from)
}

fn load_package(
    connection: &Connection,
    package_revision_ref: &AppReference,
) -> Result<AppPackageRevision, AppEntityPortabilityError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_package_revisions WHERE package_revision_ref = ?1",
            params![package_revision_ref.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityPortabilityError::MissingPackage)?;
    Ok(decode_app_contract(&bytes, &AppContractLimits::default())?)
}

fn load_stored_import_receipt(
    transaction: &rusqlite::Transaction<'_>,
    destination: &CurrentAppImportDestination,
    authenticated_scope: &AuthenticatedAppScope,
    preview: &AppDataImportPreview,
    now: DateTime<Utc>,
) -> Result<Option<AppStoredDataImportReceipt>, AppEntityPortabilityError> {
    let bytes = transaction
        .query_row(
            "SELECT record_json FROM app_data_import_receipts
             WHERE installation_id = ?1 AND import_batch_key = ?2",
            params![
                preview.destination_installation_id.as_str(),
                preview.import_batch_key.as_str(),
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    bytes
        .map(|bytes| {
            let receipt: AppDataImportReceipt =
                decode_app_contract(&bytes, &AppContractLimits::default())?;
            AppStoredDataImportReceipt::from_app_store(
                receipt,
                current_store_revision(transaction, &preview.destination_installation_id)?,
                destination,
                authenticated_scope,
                now,
                &AppContractLimits::default(),
            )
            .map_err(AppEntityPortabilityError::from)
        })
        .transpose()
}

fn committed_import_receipt_blocking(
    connection: &Connection,
    authenticated_scope: &AuthenticatedAppScope,
    destination_installation_id: &AppInstallationId,
    preview_digest: &AppDigest,
) -> Result<Option<AppDataImportReceipt>, AppEntityPortabilityError> {
    let bytes = connection
        .query_row(
            "SELECT record_json FROM app_data_import_receipts
             WHERE installation_id = ?1 AND preview_digest = ?2",
            params![
                destination_installation_id.as_str(),
                preview_digest.as_str()
            ],
            |row| row.get::<_, Vec<u8>>(0),
        )
        .optional()?;
    bytes
        .map(|bytes| {
            let receipt: AppDataImportReceipt =
                decode_app_contract(&bytes, &AppContractLimits::default())?;
            if receipt.destination_installation_id != *destination_installation_id
                || receipt.preview_digest != *preview_digest
                || receipt.scope_binding_ref != *authenticated_scope.scope_binding_ref()
            {
                return Err(AppEntityPortabilityError::CorruptRecord);
            }
            Ok(receipt)
        })
        .transpose()
}

fn persist_import_receipt(
    transaction: &rusqlite::Transaction<'_>,
    receipt: &AppDataImportReceipt,
) -> Result<(), AppEntityPortabilityError> {
    let bytes = serde_json::to_vec(receipt)?;
    transaction.execute(
        "INSERT INTO app_data_import_receipts (
             receipt_ref, installation_id, import_batch_key,
             source_archive_digest, preview_digest, record_json, committed_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            receipt.receipt_ref.as_str(),
            receipt.destination_installation_id.as_str(),
            receipt.import_batch_key.as_str(),
            receipt.source_archive_digest.as_str(),
            receipt.preview_digest.as_str(),
            bytes,
            receipt.committed_at.to_rfc3339(),
        ],
    )?;
    Ok(())
}

#[derive(Debug, Serialize)]
struct ImportedRecordCommit {
    entity: AppName,
    record_id: AppRecordId,
    revision: AppRevision,
}

fn append_import_outbox(
    transaction: &rusqlite::Transaction<'_>,
    receipt: &AppDataImportReceipt,
    committed: &[ImportedRecordCommit],
    first: u64,
    last: u64,
    now: DateTime<Utc>,
) -> Result<(), AppEntityPortabilityError> {
    #[derive(Serialize)]
    struct ImportProjection<'a> {
        receipt_ref: &'a AppReference,
        installation_id: &'a AppInstallationId,
        source_archive_digest: &'a AppDigest,
        records: &'a [ImportedRecordCommit],
    }
    let payload = serde_json::to_vec(&ImportProjection {
        receipt_ref: &receipt.receipt_ref,
        installation_id: &receipt.destination_installation_id,
        source_archive_digest: &receipt.source_archive_digest,
        records: committed,
    })?;
    let event_id = AppReference::parse(format!("app-import-change:{}", receipt.receipt_ref))?;
    transaction.execute(
        "INSERT INTO app_entity_outbox (
             event_id, installation_id, first_change_seq, last_change_seq,
             payload_json, delivery_state, available_at, lease_token,
             lease_expires_at, created_at, delivered_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'pending', ?6, NULL, NULL, ?6, NULL)",
        params![
            event_id.as_str(),
            receipt.destination_installation_id.as_str(),
            to_sql_u64(first)?,
            to_sql_u64(last)?,
            payload,
            now.to_rfc3339(),
        ],
    )?;
    Ok(())
}

fn rewrite_references_for_export(
    payload: &mut Value,
    runtime: &AppEntityRuntimeContract,
    aliases: &BTreeMap<(AppName, AppRecordId), AppPortableAlias>,
) -> Result<(), AppEntityPortabilityError> {
    let object = payload
        .as_object_mut()
        .ok_or(AppEntityPortabilityError::CorruptRecord)?;
    for (field, contract) in runtime.fields() {
        let Some(target_entity) = contract.reference_entity() else {
            continue;
        };
        let Some(value) = object.get_mut(field.as_str()) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let target_id = value
            .as_str()
            .ok_or(AppEntityPortabilityError::CorruptReference)?;
        let alias = aliases
            .get(&(target_entity.clone(), AppRecordId::parse(target_id)?))
            .ok_or(AppEntityPortabilityError::CorruptReference)?;
        *value = Value::String(portable_record_token(*alias)?);
    }
    Ok(())
}

fn rewrite_references_for_import(
    payload: &mut Value,
    runtime: &AppEntityRuntimeContract,
    source_by_alias: &HashMap<AppPortableAlias, &AppPortableRecordRevision>,
    local_ids: &HashMap<AppPortableAlias, AppRecordId>,
) -> Result<(), AppEntityPortabilityError> {
    let object = payload
        .as_object_mut()
        .ok_or(AppEntityPortabilityError::CorruptRecord)?;
    for (field, contract) in runtime.fields() {
        let Some(target_entity) = contract.reference_entity() else {
            continue;
        };
        let Some(value) = object.get_mut(field.as_str()) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        let alias = parse_portable_record_token(
            value
                .as_str()
                .ok_or(AppEntityPortabilityError::CorruptReference)?,
        )?;
        let source = source_by_alias
            .get(&alias)
            .copied()
            .ok_or(AppEntityPortabilityError::CorruptReference)?;
        if &source.entity_name != target_entity {
            return Err(AppEntityPortabilityError::CorruptReference);
        }
        let local = local_ids
            .get(&alias)
            .ok_or(AppEntityPortabilityError::CorruptReference)?;
        *value = Value::String(local.to_string());
    }
    Ok(())
}

fn imported_provenance(
    source: &AppPortableRecordRevision,
    installation_id: &AppInstallationId,
    archive_digest: &AppDigest,
    local_ids: &HashMap<AppPortableAlias, AppRecordId>,
    receipt_ref: &AppReference,
) -> Result<AppRecordProvenance, AppEntityPortabilityError> {
    let actor_id = match source.provenance.actor_alias {
        Some(alias) => local_reference("import-actor", installation_id, archive_digest, alias)?,
        None => local_reference(
            "import-actor",
            installation_id,
            archive_digest,
            AppPortableAlias::new(AppPortableAliasKind::Actor, 1)?,
        )?,
    };
    let execution_id = source
        .provenance
        .execution_alias
        .map(|alias| local_reference("import-execution", installation_id, archive_digest, alias))
        .transpose()?;
    let mut source_artifact_refs = Vec::new();
    let mut citation_refs = Vec::new();
    for alias in &source.provenance.source_aliases {
        match alias.kind {
            AppPortableAliasKind::Record => {
                let record_id = local_ids
                    .get(alias)
                    .ok_or(AppEntityPortabilityError::CorruptReference)?;
                citation_refs.push(AppReference::parse(format!(
                    "app-import-record:{record_id}"
                ))?);
            },
            AppPortableAliasKind::Attachment => {
                source_artifact_refs.push(local_reference(
                    "app-import-missing-attachment",
                    installation_id,
                    archive_digest,
                    *alias,
                )?);
            },
            AppPortableAliasKind::Actor | AppPortableAliasKind::Execution => {
                return Err(AppEntityPortabilityError::CorruptReference)
            },
        }
    }
    Ok(AppRecordProvenance {
        actor_kind: source.provenance.actor_kind,
        actor_id,
        execution_id,
        output_revision: None,
        mutation_receipt_id: Some(receipt_ref.clone()),
        source_artifact_refs,
        citation_refs,
    })
}

fn alias_for_reference(
    aliases: &mut BTreeMap<AppReference, AppPortableAlias>,
    reference: &AppReference,
    kind: AppPortableAliasKind,
) -> Result<AppPortableAlias, AppEntityPortabilityError> {
    if let Some(alias) = aliases.get(reference) {
        return Ok(*alias);
    }
    let alias = portable_alias(kind, aliases.len())?;
    aliases.insert(reference.clone(), alias);
    Ok(alias)
}

fn portable_alias(
    kind: AppPortableAliasKind,
    zero_based_index: usize,
) -> Result<AppPortableAlias, AppEntityPortabilityError> {
    let ordinal = zero_based_index
        .checked_add(1)
        .and_then(|value| u32::try_from(value).ok())
        .ok_or(AppEntityPortabilityError::ExportLimit)?;
    AppPortableAlias::new(kind, ordinal).map_err(AppEntityPortabilityError::from)
}

fn portable_record_token(alias: AppPortableAlias) -> Result<String, AppEntityPortabilityError> {
    if alias.kind != AppPortableAliasKind::Record {
        return Err(AppEntityPortabilityError::CorruptReference);
    }
    Ok(format!(
        "{PORTABLE_RECORD_TOKEN_PREFIX}{:08}",
        alias.ordinal.get()
    ))
}

fn parse_portable_record_token(value: &str) -> Result<AppPortableAlias, AppEntityPortabilityError> {
    let digits = value
        .strip_prefix(PORTABLE_RECORD_TOKEN_PREFIX)
        .filter(|digits| digits.len() == 8 && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .ok_or(AppEntityPortabilityError::CorruptReference)?;
    let ordinal = digits
        .parse::<u32>()
        .map_err(|_| AppEntityPortabilityError::CorruptReference)?;
    AppPortableAlias::new(AppPortableAliasKind::Record, ordinal)
        .map_err(AppEntityPortabilityError::from)
}

fn imported_record_id(
    installation_id: &AppInstallationId,
    archive_digest: &AppDigest,
    source: &AppPortableRecordRevision,
) -> Result<AppRecordId, AppEntityPortabilityError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        installation_id: &'a AppInstallationId,
        archive_digest: &'a AppDigest,
        record_alias: AppPortableAlias,
        entity: &'a AppName,
    }
    let digest = AppDigest::blake3_canonical_json(&serde_json::to_value(Identity {
        installation_id,
        archive_digest,
        record_alias: source.record_alias,
        entity: &source.entity_name,
    })?)?;
    AppRecordId::parse(format!(
        "import-{}",
        digest
            .as_str()
            .strip_prefix("blake3:")
            .unwrap_or(digest.as_str())
    ))
    .map_err(AppEntityPortabilityError::from)
}

fn import_batch_key(
    installation_id: &AppInstallationId,
    installation_generation: u64,
    source_archive_digest: &AppDigest,
) -> Result<AppDigest, AppEntityPortabilityError> {
    #[derive(Serialize)]
    struct Batch<'a> {
        installation_id: &'a AppInstallationId,
        installation_generation: u64,
        source_archive_digest: &'a AppDigest,
    }
    AppDigest::blake3_canonical_json(&serde_json::to_value(Batch {
        installation_id,
        installation_generation,
        source_archive_digest,
    })?)
    .map_err(AppEntityPortabilityError::from)
}

fn import_receipt_ref(
    import_batch_key: &AppDigest,
) -> Result<AppReference, AppEntityPortabilityError> {
    AppReference::parse(format!(
        "app-import-receipt:{}",
        import_batch_key
            .as_str()
            .strip_prefix("blake3:")
            .unwrap_or(import_batch_key.as_str())
    ))
    .map_err(AppEntityPortabilityError::from)
}

fn local_reference(
    prefix: &str,
    installation_id: &AppInstallationId,
    archive_digest: &AppDigest,
    alias: AppPortableAlias,
) -> Result<AppReference, AppEntityPortabilityError> {
    #[derive(Serialize)]
    struct Identity<'a> {
        installation_id: &'a AppInstallationId,
        archive_digest: &'a AppDigest,
        alias: AppPortableAlias,
    }
    let digest = AppDigest::blake3_canonical_json(&serde_json::to_value(Identity {
        installation_id,
        archive_digest,
        alias,
    })?)?;
    AppReference::parse(format!(
        "{prefix}:{}",
        digest
            .as_str()
            .strip_prefix("blake3:")
            .unwrap_or(digest.as_str())
    ))
    .map_err(AppEntityPortabilityError::from)
}

fn record_exists(
    connection: &Connection,
    installation_id: &AppInstallationId,
    entity: &AppName,
    record_id: &AppRecordId,
) -> Result<bool, AppEntityPortabilityError> {
    Ok(connection.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM app_record_heads
             WHERE installation_id = ?1 AND entity_name = ?2 AND record_id = ?3
         )",
        params![
            installation_id.as_str(),
            entity.as_str(),
            record_id.as_str()
        ],
        |row| row.get(0),
    )?)
}

fn revision_from_sql(value: i64) -> Result<AppRevision, AppEntityPortabilityError> {
    AppRevision::new(positive_u64(value)?).map_err(AppEntityPortabilityError::from)
}

fn positive_u64(value: i64) -> Result<u64, AppEntityPortabilityError> {
    let value = u64::try_from(value).map_err(|_| AppEntityPortabilityError::CorruptRecord)?;
    if value == 0 {
        return Err(AppEntityPortabilityError::CorruptRecord);
    }
    Ok(value)
}

fn to_sql_revision(value: AppRevision) -> Result<i64, AppEntityPortabilityError> {
    i64::try_from(value.get()).map_err(|_| AppEntityPortabilityError::CorruptRecord)
}

fn parse_timestamp(value: &str) -> Result<DateTime<Utc>, AppEntityPortabilityError> {
    DateTime::parse_from_rfc3339(value)
        .map(|value| value.with_timezone(&Utc))
        .map_err(|_| AppEntityPortabilityError::CorruptRecord)
}

#[derive(Debug, Error)]
pub enum AppEntityPortabilityError {
    #[error("app portability registry failed: {0}")]
    Registry(#[from] AppRegistryError),
    #[error("app portability active-store resolution failed: {0}")]
    Store(#[source] AppEntityStoreError),
    #[error("app portability SQLite operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("invalid app portability contract: {0}")]
    Contract(#[from] AppContractError),
    #[error("app portability contract failed: {0}")]
    Portability(#[from] AppPortabilityError),
    #[error("app schema compilation failed: {0}")]
    Schema(#[from] AppSchemaCompilerError),
    #[error("app entity mutation failed: {0}")]
    Mutation(#[from] AppEntityMutationError),
    #[error("failed to encode app portability evidence: {0}")]
    Json(#[from] serde_json::Error),
    #[error("scoped app store does not exist")]
    MissingScopedStore,
    #[error("app installation does not exist in the authenticated scope")]
    MissingInstallation,
    #[error("app package revision is missing")]
    MissingPackage,
    #[error("app schema revision is missing")]
    MissingSchema,
    #[error("app installation belongs to another scope")]
    ScopeMismatch,
    #[error("app schema/package binding is corrupt")]
    CorruptSchemaBinding,
    #[error("app export source lifecycle is not data-exportable")]
    UnsupportedSourceState,
    #[error("app data export contains no live records")]
    EmptyDataSet,
    #[error("app data export exceeds the fixed record ceiling")]
    ExportLimit,
    #[error("stored app record is corrupt")]
    CorruptRecord,
    #[error("stored or portable app reference is corrupt")]
    CorruptReference,
    #[error("unknown app entity `{0}`")]
    UnknownEntity(String),
    #[error("import preview does not bind the supplied source archive")]
    PreviewSourceMismatch,
    #[error("import destination installation generation changed")]
    DestinationGenerationConflict,
    #[error("import preview is incomplete")]
    IncompletePreview,
    #[error("destination-local import identity collided before commit")]
    ImportIdentityConflict,
    #[error("reviewed merge executor is not available for this preview")]
    ReviewedMergeExecutorUnavailable,
    #[error("portable record timestamp is later than the import commit")]
    FutureSourceTimestamp,
    #[error("app portability counter overflow")]
    CounterOverflow,
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

    fn assert_json_values_do_not_contain(root: &Value, forbidden: &str) {
        let mut pending = vec![root];
        while let Some(value) = pending.pop() {
            match value {
                Value::String(value) => assert!(
                    !value.contains(forbidden),
                    "leaked source identity: {forbidden}"
                ),
                Value::Array(values) => pending.extend(values),
                Value::Object(values) => pending.extend(values.values()),
                Value::Null | Value::Bool(_) | Value::Number(_) => {},
            }
        }
    }

    async fn seeded_portability() -> (
        tempfile::TempDir,
        AppRegistryService,
        AppDataPortabilityService,
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
        let service = AppDataPortabilityService::new(registry.clone());
        let authenticated = authenticated_scope("anonymous", "default");
        (temporary, registry, service, authenticated)
    }

    #[tokio::test]
    async fn export_aliases_source_identity_and_import_mints_local_records_once() {
        let (_temporary, registry, service, authenticated) = seeded_portability().await;
        let installation_id = AppInstallationId::parse("install_1").unwrap();
        let archive = service
            .export_data(&authenticated, &installation_id, time(4))
            .await
            .unwrap();
        assert_eq!(archive.records.len(), 2);
        let encoded = serde_json::to_value(&archive).unwrap();
        for forbidden in [
            "anonymous",
            "default",
            "install_1",
            "record_a",
            "record_b",
            "actor:owner",
        ] {
            assert_json_values_do_not_contain(&encoded, forbidden);
        }
        assert!(archive.records.iter().any(|record| {
            record.payload.get("parent").and_then(Value::as_str) == Some("portable-record-00000001")
        }));

        let preview = service
            .preview_import(&authenticated, &installation_id, &archive, time(5))
            .await
            .unwrap();
        assert_eq!(preview.status, AppDataImportPreviewStatus::Ready);
        assert!(preview
            .record_decisions
            .iter()
            .all(|decision| { matches!(decision, AppDataImportRecordDecision::Create { .. }) }));
        let approval = service
            .approve_import(
                &authenticated,
                AppReference::parse("approval:import-reading-list").unwrap(),
                &preview,
                time(6),
                time(20),
            )
            .unwrap();
        let receipt = service
            .commit_import(
                &authenticated,
                archive.clone(),
                preview.clone(),
                approval.clone(),
                time(7),
            )
            .await
            .unwrap();
        assert_eq!(receipt.created_count, 2);
        assert_eq!(receipt.skipped_count, 0);
        let replay = service
            .commit_import(&authenticated, archive, preview, approval, time(8))
            .await
            .unwrap();
        assert_eq!(replay, receipt);
        assert_eq!(
            service
                .committed_import_receipt(
                    &authenticated,
                    &installation_id,
                    &receipt.preview_digest,
                    time(9),
                )
                .await
                .unwrap(),
            Some(receipt.clone())
        );

        let imported = registry
            .execute_scoped_read(&authenticated, &time(10), |connection, _| {
                let count = connection.query_row(
                    "SELECT COUNT(*) FROM app_record_heads
                     WHERE installation_id = 'install_1' AND record_id LIKE 'import-%'",
                    [],
                    |row| row.get::<_, i64>(0),
                )?;
                let parent = connection.query_row(
                    "SELECT json_extract(r.payload_json, '$.parent')
                     FROM app_record_heads h
                     JOIN app_record_revisions r
                       ON r.installation_id = h.installation_id
                      AND r.entity_name = h.entity_name
                      AND r.record_id = h.record_id
                      AND r.record_revision = h.record_revision
                     WHERE h.installation_id = 'install_1'
                       AND h.record_id LIKE 'import-%'
                       AND json_extract(r.payload_json, '$.title') = 'Beta'",
                    [],
                    |row| row.get::<_, String>(0),
                )?;
                Ok((count, parent))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(imported.0, 2);
        assert!(imported.1.starts_with("import-"));
        assert_ne!(imported.1, "record_a");
    }

    #[test]
    fn portable_reference_tokens_are_exact_and_reject_lookalikes() {
        let alias = AppPortableAlias::new(AppPortableAliasKind::Record, 42).unwrap();
        let token = portable_record_token(alias).unwrap();
        assert_eq!(token, "portable-record-00000042");
        assert_eq!(parse_portable_record_token(&token).unwrap(), alias);
        for invalid in [
            "portable-record-42",
            "portable-record-00000000",
            "portable-record-0000004x",
            "other-00000042",
        ] {
            assert!(parse_portable_record_token(invalid).is_err());
        }
    }
}

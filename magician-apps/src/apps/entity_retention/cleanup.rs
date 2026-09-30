//! Owner-approved age cleanup over a frozen, disk-backed identity selection.
//! Each advance commits one bounded batch and its progress together. The app
//! bridge has no access to this maintenance authority; there is no model work.
use super::super::{
    entity_store::{dataset_indexes_are_complete, timestamp_order_key},
    models::{AppFieldPath, AppScopeBindingRef},
    query_semantics::AppQueryScalarKind,
};
use super::*;

const BATCH_RECORDS: usize = 64;
const PREVIEW_MINUTES: i64 = 15;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppDataCleanupRequest {
    pub entity: AppName,
    pub timestamp_field: AppFieldPath,
    pub before: DateTime<Utc>,
}

impl ValidateAppContract for AppDataCleanupRequest {
    fn validate_app_contract(&self, _: &AppContractLimits) -> Result<(), AppContractError> {
        AppName::parse(self.entity.as_str())?;
        AppFieldPath::parse(self.timestamp_field.as_str())?;
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AppDataCleanupStatus {
    Preview,
    Running,
    Paused,
    Checkpointing,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppDataCleanupJob {
    pub job_ref: AppReference,
    pub installation_id: AppInstallationId,
    pub selection: AppDataCleanupRequest,
    pub status: AppDataCleanupStatus,
    pub preview_digest: AppDigest,
    pub matching_records: u64,
    pub matching_payload_bytes: u64,
    pub deleted_records: u64,
    pub kept_changed_records: u64,
    pub kept_referenced_records: u64,
    pub remaining_records: u64,
    pub observed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Serialize)]
pub struct AppDataCleanupEntity {
    pub entity: AppName,
    pub timestamp_fields: Vec<AppFieldPath>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredJob {
    report: AppDataCleanupJob,
    scope_binding_ref: AppScopeBindingRef,
    actor_ref: AppReference,
    installation_generation: u64,
    schema_revision: AppRevision,
    package_revision_ref: AppReference,
    approved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    cancel_requested: bool,
    batch_sequence: u64,
}

impl AppEntityRetentionService {
    pub async fn cleanup_entities(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Vec<AppDataCleanupEntity>, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        self.store
            .registry
            .execute_scoped_read(authenticated, &now, move |connection, scope| {
                Ok(
                    (|| -> Result<Vec<AppDataCleanupEntity>, AppEntityRetentionStoreError> {
                        let active =
                            resolve_data_owner_schema(connection, scope, &installation_id)?
                                .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
                        Ok(active
                            .runtime_contracts()
                            .filter_map(|(entity, runtime)| {
                                let timestamp_fields = runtime
                                    .fields()
                                    .filter(|(_, field)| {
                                        field.kind() == AppQueryScalarKind::Timestamp
                                            && field.indexed()
                                    })
                                    .map(|(field, _)| field.clone())
                                    .collect::<Vec<_>>();
                                (!timestamp_fields.is_empty()).then(|| AppDataCleanupEntity {
                                    entity: entity.clone(),
                                    timestamp_fields,
                                })
                            })
                            .collect())
                    })(),
                )
            })
            .await?
            .ok_or(AppEntityRetentionStoreError::MissingInstallation)?
    }

    pub async fn preview_age_cleanup(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        selection: AppDataCleanupRequest,
        now: DateTime<Utc>,
    ) -> Result<AppDataCleanupJob, AppEntityRetentionStoreError> {
        selection.validate_app_contract(&AppContractLimits::default())?;
        if selection.before > now {
            return Err(AppContractError::invalid("before", "must not be in the future").into());
        }
        let installation_id = installation_id.clone();
        let authenticated_copy = authenticated.clone();
        self.store.registry.execute_scoped_typed_write(authenticated, &now, move |connection, scope| {
            let transaction = connection.transaction()?;
            let active = resolve_data_owner_schema(&transaction, scope, &installation_id)?
                .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
            let field = active.runtime_contract(&selection.entity)
                .and_then(|runtime| runtime.field(&selection.timestamp_field))
                .ok_or_else(|| AppContractError::invalid("timestamp_field", "choose an indexed timestamp field"))?;
            if field.kind() != AppQueryScalarKind::Timestamp || !field.indexed() {
                return Err(AppContractError::invalid("timestamp_field", "choose an indexed timestamp field").into());
            }
            if !dataset_indexes_are_complete(&transaction, &installation_id)? {
                return Err(super::super::entity_store::AppEntityStoreError::KeysetIndexRequired.into());
            }
            // A replacement preview releases abandoned identity selections.
            // Expired previews from other owners are also safe to discard;
            // approved jobs and user records are never affected here.
            let previews = transaction.prepare("SELECT record_json FROM app_data_cleanup_jobs
                WHERE installation_id=?1 AND json_extract(CAST(record_json AS TEXT),'$.report.status')='preview'")?
                .query_map([installation_id.as_str()], |row| row.get::<_,Vec<u8>>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for bytes in previews {
                let mut previous: StoredJob = serde_json::from_slice(&bytes)?;
                if previous.report.expires_at <= now || previous.actor_ref == *authenticated_copy.actor_ref() {
                    previous.report.status = AppDataCleanupStatus::Cancelled;
                    previous.report.updated_at = now;
                    transaction.execute("DELETE FROM app_data_cleanup_candidates WHERE job_ref=?1",
                        [previous.report.job_ref.as_str()])?;
                    save_job(&transaction, &previous)?;
                }
            }
            let job_ref = AppReference::parse(format!("app-cleanup:{}", uuid::Uuid::new_v4().simple()))?;
            let mut stored = StoredJob {
                report: AppDataCleanupJob {
                    job_ref, installation_id: installation_id.clone(), selection,
                    status: AppDataCleanupStatus::Preview, preview_digest: AppDigest::blake3(b"pending"),
                    matching_records: 0, matching_payload_bytes: 0, deleted_records: 0,
                    kept_changed_records: 0, kept_referenced_records: 0, remaining_records: 0,
                    observed_at: now, expires_at: now + Duration::minutes(PREVIEW_MINUTES), updated_at: now,
                },
                scope_binding_ref: authenticated_copy.scope_binding_ref().clone(),
                actor_ref: authenticated_copy.actor_ref().clone(),
                installation_generation: active.installation_generation(), schema_revision: active.schema_revision(),
                package_revision_ref: active.package_revision_ref().clone(), approved_at: None, cancel_requested: false, batch_sequence: 0,
            };
            save_job(&transaction, &stored)?;
            // SQLite freezes only identities/revisions and counts; message bodies
            // are neither loaded into the service nor copied into this selection.
            transaction.execute(
                "INSERT INTO app_data_cleanup_candidates(job_ref,record_id,record_revision,dataset_generation,payload_bytes)
                 SELECT ?1,h.record_id,h.record_revision,h.dataset_generation,length(r.payload_json)
                 FROM app_scalar_indexes s INDEXED BY app_scalar_order_asc_idx
                 JOIN app_record_heads h ON h.installation_id=s.installation_id AND h.entity_name=s.entity_name
                    AND h.record_id=s.record_id AND h.record_revision=s.record_revision
                 JOIN app_record_revisions r ON r.installation_id=h.installation_id AND r.entity_name=h.entity_name
                    AND r.record_id=h.record_id AND r.record_revision=h.record_revision
                 WHERE s.installation_id=?2 AND s.entity_name=?3 AND s.field_path=?4
                   AND s.order_key_asc<?5 AND s.value_kind='timestamp' AND h.deleted_at IS NULL",
                params![stored.report.job_ref.as_str(), installation_id.as_str(), stored.report.selection.entity.as_str(),
                    stored.report.selection.timestamp_field.as_str(), timestamp_order_key(stored.report.selection.before)?],
            )?;
            let (count, bytes): (u64,u64) = transaction.query_row(
                "SELECT COUNT(*),COALESCE(SUM(payload_bytes),0) FROM app_data_cleanup_candidates WHERE job_ref=?1",
                [stored.report.job_ref.as_str()], |row| Ok((row.get(0)?,row.get(1)?)))?;
            stored.report.matching_records = count;
            stored.report.matching_payload_bytes = bytes;
            stored.report.remaining_records = count;
            stored.report.preview_digest = selection_digest(&transaction, &stored)?;
            save_job(&transaction, &stored)?;
            transaction.commit()?;
            Ok(stored.report)
        }).await
    }

    pub async fn latest_age_cleanup(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        now: DateTime<Utc>,
    ) -> Result<Option<AppDataCleanupJob>, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        let authenticated_copy = authenticated.clone();
        self.store
            .registry
            .execute_scoped_read(authenticated, &now, move |connection, _| {
                let bytes: Option<Vec<u8>> = connection
                    .query_row(
                        "SELECT record_json FROM app_data_cleanup_jobs WHERE installation_id=?1
                 AND json_extract(CAST(record_json AS TEXT),'$.actor_ref')=?2
                 ORDER BY CASE json_extract(CAST(record_json AS TEXT),'$.report.status')
                    WHEN 'running' THEN 0 WHEN 'paused' THEN 0 WHEN 'checkpointing' THEN 0
                    WHEN 'preview' THEN 1 ELSE 2 END,
                    updated_at DESC,job_ref DESC LIMIT 1",
                        params![
                            installation_id.as_str(),
                            authenticated_copy.actor_ref().as_str()
                        ],
                        |row| row.get(0),
                    )
                    .optional()?;
                Ok(bytes
                    .map(|bytes| {
                        let stored: StoredJob = serde_json::from_slice(&bytes)?;
                        ensure_job_scope(&stored, &authenticated_copy, &installation_id)?;
                        Ok::<_, AppEntityRetentionStoreError>(stored.report)
                    })
                    .transpose())
            })
            .await?
            .unwrap_or(Ok(None))
    }

    /// Confirmation authorizes the exact frozen selection, not future matching
    /// rows. Changed/deleted/recreated records are retained by revision checks.
    pub async fn control_age_cleanup(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        job_ref: AppReference,
        operation: String,
        preview_digest: Option<AppDigest>,
        now: DateTime<Utc>,
    ) -> Result<AppDataCleanupJob, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        let authenticated_copy = authenticated.clone();
        self.store
            .registry
            .execute_scoped_typed_write(authenticated, &now, move |connection, _| {
                connection.pragma_update(None, "secure_delete", "ON")?;
                let transaction = connection.transaction()?;
                let mut stored = load_job(
                    &transaction,
                    &authenticated_copy,
                    &installation_id,
                    &job_ref,
                )?;
                match operation.as_str() {
                    "confirm" => {
                        if preview_digest.as_ref() != Some(&stored.report.preview_digest) {
                            return Err(AppEntityRetentionStoreError::CleanupConfirmationMismatch);
                        }
                        if stored.approved_at.is_none() {
                            if stored.report.status != AppDataCleanupStatus::Preview {
                                return Err(AppEntityRetentionStoreError::CleanupInvalidState);
                            }
                            if now >= stored.report.expires_at {
                                return Err(AppEntityRetentionStoreError::CleanupPreviewExpired);
                            }
                            if selection_digest(&transaction, &stored)?
                                != stored.report.preview_digest
                            {
                                return Err(
                                    AppEntityRetentionStoreError::CleanupConfirmationMismatch,
                                );
                            }
                            stored.approved_at = Some(now);
                            stored.report.status = AppDataCleanupStatus::Running;
                        }
                    },
                    "pause" if stored.report.status == AppDataCleanupStatus::Running => {
                        stored.report.status = AppDataCleanupStatus::Paused
                    },
                    "resume"
                        if stored.report.status == AppDataCleanupStatus::Paused
                            && stored.approved_at.is_some() =>
                    {
                        stored.report.status = AppDataCleanupStatus::Running
                    },
                    "cancel"
                        if matches!(
                            stored.report.status,
                            AppDataCleanupStatus::Preview
                                | AppDataCleanupStatus::Paused
                                | AppDataCleanupStatus::Running
                        ) =>
                    {
                        stored.cancel_requested = true;
                        stored.report.status = if stored.report.deleted_records > 0 {
                            AppDataCleanupStatus::Checkpointing
                        } else {
                            AppDataCleanupStatus::Cancelled
                        };
                        transaction.execute(
                            "DELETE FROM app_data_cleanup_candidates WHERE job_ref=?1",
                            [job_ref.as_str()],
                        )?;
                    },
                    _ => return Err(AppEntityRetentionStoreError::CleanupInvalidState),
                }
                stored.report.updated_at = now;
                save_job(&transaction, &stored)?;
                transaction.commit()?;
                Ok(stored.report)
            })
            .await
    }

    pub async fn advance_age_cleanup(
        &self,
        authenticated: &AuthenticatedAppScope,
        installation_id: &AppInstallationId,
        job_ref: AppReference,
        now: DateTime<Utc>,
    ) -> Result<AppDataCleanupJob, AppEntityRetentionStoreError> {
        let installation_id = installation_id.clone();
        let authenticated_copy = authenticated.clone();
        let report = self
            .store
            .registry
            .execute_scoped_background_write(authenticated, &now, move |connection, scope| {
                Ok(advance_blocking(
                    connection,
                    scope,
                    &authenticated_copy,
                    &installation_id,
                    &job_ref,
                    now,
                ))
            })
            .await??;
        if report.deleted_records > 0 {
            self.store
                .registry
                .tombstone_app_memory_index_projection_for_scope(
                    authenticated,
                    &format!("age-cleanup:{}:{}", report.job_ref, report.deleted_records),
                )
                .await?;
        }
        Ok(report)
    }
}

fn ensure_job_scope(
    stored: &StoredJob,
    authenticated: &AuthenticatedAppScope,
    installation: &AppInstallationId,
) -> Result<(), AppEntityRetentionStoreError> {
    if stored.report.installation_id != *installation
        || stored.scope_binding_ref != *authenticated.scope_binding_ref()
        || stored.actor_ref != *authenticated.actor_ref()
    {
        return Err(AppEntityRetentionStoreError::ScopeMismatch);
    }
    Ok(())
}

fn load_job(
    connection: &Connection,
    authenticated: &AuthenticatedAppScope,
    installation: &AppInstallationId,
    job: &AppReference,
) -> Result<StoredJob, AppEntityRetentionStoreError> {
    let bytes: Vec<u8> = connection
        .query_row(
            "SELECT record_json FROM app_data_cleanup_jobs WHERE job_ref=?1 AND installation_id=?2",
            params![job.as_str(), installation.as_str()],
            |row| row.get(0),
        )
        .optional()?
        .ok_or(AppEntityRetentionStoreError::CleanupNotFound)?;
    let stored: StoredJob = serde_json::from_slice(&bytes)?;
    if stored.report.job_ref != *job {
        return Err(AppEntityRetentionStoreError::CorruptPurgeEvidence);
    }
    ensure_job_scope(&stored, authenticated, installation)?;
    Ok(stored)
}

fn save_job(
    connection: &Connection,
    stored: &StoredJob,
) -> Result<(), AppEntityRetentionStoreError> {
    connection.execute("INSERT INTO app_data_cleanup_jobs VALUES (?1,?2,?3,?4)
        ON CONFLICT(job_ref) DO UPDATE SET record_json=excluded.record_json,updated_at=excluded.updated_at",
        params![stored.report.job_ref.as_str(), stored.report.installation_id.as_str(),
            serde_json::to_vec(stored)?, stored.report.updated_at.to_rfc3339()])?;
    Ok(())
}

fn selection_digest(
    connection: &Connection,
    stored: &StoredJob,
) -> Result<AppDigest, AppEntityRetentionStoreError> {
    let mut digest = blake3::Hasher::new();
    digest.update(&serde_json::to_vec(&serde_json::json!({
        "job": stored.report.job_ref, "installation": stored.report.installation_id,
        "scope": stored.scope_binding_ref, "actor": stored.actor_ref, "generation": stored.installation_generation,
        "schema": stored.schema_revision, "package": stored.package_revision_ref, "selection": stored.report.selection,
        "observed_at": stored.report.observed_at, "expires_at": stored.report.expires_at,
    }))?);
    let mut statement = connection.prepare(
        "SELECT record_id,record_revision,dataset_generation,payload_bytes
        FROM app_data_cleanup_candidates WHERE job_ref=?1 ORDER BY record_id",
    )?;
    let rows = statement.query_map([stored.report.job_ref.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })?;
    for row in rows {
        digest.update(&serde_json::to_vec(&row?)?);
        digest.update(b"\n");
    }
    Ok(AppDigest::parse(format!(
        "blake3:{}",
        digest.finalize().to_hex()
    ))?)
}

fn candidate_matches(
    connection: &Connection,
    stored: &StoredJob,
    id: &AppRecordId,
) -> Result<bool, AppEntityRetentionStoreError> {
    Ok(connection.query_row("SELECT EXISTS(SELECT 1 FROM app_data_cleanup_candidates c
        JOIN app_record_heads h ON h.installation_id=?1 AND h.entity_name=?2 AND h.record_id=c.record_id
        WHERE c.job_ref=?3 AND c.record_id=?4 AND c.state='pending' AND h.deleted_at IS NULL
          AND h.record_revision=c.record_revision AND h.dataset_generation=c.dataset_generation)",
        params![stored.report.installation_id.as_str(),stored.report.selection.entity.as_str(),stored.report.job_ref.as_str(),id.as_str()],
        |row| row.get(0))?)
}

fn allowed_plan(
    connection: &Connection,
    stored: &StoredJob,
    plan: &ForgetPlan,
) -> Result<bool, AppEntityRetentionStoreError> {
    if plan.forgotten.len() > BATCH_RECORDS {
        return Ok(false);
    }
    for ((entity, id), record) in &plan.working {
        if *entity != stored.report.selection.entity
            || !record.deleted
            || !candidate_matches(connection, stored, id)?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn advance_blocking(
    connection: &mut Connection,
    scope: &AppScope,
    authenticated: &AuthenticatedAppScope,
    installation: &AppInstallationId,
    job: &AppReference,
    now: DateTime<Utc>,
) -> Result<AppDataCleanupJob, AppEntityRetentionStoreError> {
    let mut stored = load_job(connection, authenticated, installation, job)?;
    if matches!(
        stored.report.status,
        AppDataCleanupStatus::Completed | AppDataCleanupStatus::Cancelled
    ) {
        return Ok(stored.report);
    }
    if stored.approved_at.is_none()
        || !matches!(
            stored.report.status,
            AppDataCleanupStatus::Running | AppDataCleanupStatus::Checkpointing
        )
    {
        return Err(AppEntityRetentionStoreError::CleanupInvalidState);
    }
    if stored.report.status == AppDataCleanupStatus::Checkpointing {
        connection.pragma_update(None, "secure_delete", "ON")?;
        let transaction = connection.transaction()?;
        transaction.execute(
            "DELETE FROM app_data_cleanup_candidates WHERE job_ref=?1",
            [job.as_str()],
        )?;
        transaction.commit()?;
        // Checkpoint erased payloads and temporary selection identities before
        // recording a terminal outcome. A busy checkpoint remains retryable.
        ensure_clean_wal_boundary(connection)?;
        let transaction = connection.transaction()?;
        stored.report.status = if stored.cancel_requested {
            AppDataCleanupStatus::Cancelled
        } else {
            AppDataCleanupStatus::Completed
        };
        stored.report.updated_at = now;
        save_job(&transaction, &stored)?;
        transaction.commit()?;
        return Ok(stored.report);
    }
    connection.pragma_update(None, "secure_delete", "ON")?;
    let transaction = connection.transaction()?;
    let active = resolve_data_owner_schema(&transaction, scope, installation)?
        .ok_or(AppEntityRetentionStoreError::MissingInstallation)?;
    if active.installation_generation() != stored.installation_generation
        || active.schema_revision() != stored.schema_revision
        || active.package_revision_ref() != &stored.package_revision_ref
    {
        return Err(AppEntityRetentionStoreError::GenerationConflict);
    }
    let candidates = transaction
        .prepare(
            "SELECT record_id FROM app_data_cleanup_candidates
        WHERE job_ref=?1 AND state='pending' ORDER BY record_id LIMIT ?2",
        )?
        .query_map(params![job.as_str(), BATCH_RECORDS as i64], |row| {
            row.get::<_, String>(0)
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut current = Vec::new();
    for id in candidates {
        let id = AppRecordId::parse(id)?;
        if candidate_matches(&transaction, &stored, &id)? {
            current.push(id);
        } else {
            transaction.execute("UPDATE app_data_cleanup_candidates SET state='changed' WHERE job_ref=?1 AND record_id=?2",
                params![job.as_str(),id.as_str()])?;
            stored.report.kept_changed_records += 1;
        }
    }
    if !current.is_empty() {
        let first = current[0].clone();
        let plan = match plan_forget(
            &transaction,
            &active,
            &stored.report.selection.entity,
            &current,
        ) {
            Ok(plan) if allowed_plan(&transaction, &stored, &plan)? => Some(plan),
            Ok(_)
            | Err(AppEntityRetentionStoreError::Mutation(
                AppEntityMutationError::ReferencedRecordDeleteDenied
                | AppEntityMutationError::IncomingReferenceLimit,
            )) => {
                match plan_forget(
                    &transaction,
                    &active,
                    &stored.report.selection.entity,
                    &[first.clone()],
                ) {
                    Ok(plan) if allowed_plan(&transaction, &stored, &plan)? => Some(plan),
                    Ok(_)
                    | Err(AppEntityRetentionStoreError::Mutation(
                        AppEntityMutationError::ReferencedRecordDeleteDenied
                        | AppEntityMutationError::IncomingReferenceLimit,
                    )) => None,
                    Err(error) => return Err(error),
                }
            },
            Err(error) => return Err(error),
        };
        if let Some(plan) = plan {
            stored.batch_sequence += 1;
            let receipt = AppReference::parse(format!("{}:batch:{}", job, stored.batch_sequence))?;
            apply_forget_plan(&transaction, &active, authenticated, &plan, &receipt, now)?;
            for (_, id) in &plan.forgotten {
                let changed = transaction.execute(
                    "UPDATE app_data_cleanup_candidates SET state='deleted'
                    WHERE job_ref=?1 AND record_id=?2 AND state='pending'",
                    params![job.as_str(), id.as_str()],
                )?;
                stored.report.deleted_records += changed as u64;
            }
        } else {
            transaction.execute("UPDATE app_data_cleanup_candidates SET state='referenced' WHERE job_ref=?1 AND record_id=?2",
                params![job.as_str(),first.as_str()])?;
            stored.report.kept_referenced_records += 1;
        }
    }
    stored.report.remaining_records = transaction.query_row(
        "SELECT COUNT(*) FROM app_data_cleanup_candidates WHERE job_ref=?1 AND state='pending'",
        [job.as_str()],
        |row| row.get(0),
    )?;
    if stored.report.remaining_records == 0 {
        stored.report.status = AppDataCleanupStatus::Checkpointing;
    }
    stored.report.updated_at = now;
    save_job(&transaction, &stored)?;
    transaction.commit()?;
    Ok(stored.report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use magician::magician_v2::{
        apps::{
            entity_adapter::AppEntityAdapterService,
            entity_store::tests::{manifest, policy, seed_enabled_installation},
            lifecycle::AppInstallationStatus,
            manifest::AppManifestField,
            models::{
                AppExpectedRecordRevision, AppMutationAtomicity, AppMutationCommand,
                AppMutationOperation, AppProtocolVersion,
            },
            records::AppSchemaCompatibility,
            registry::tests::{authenticated_scope, canonical_tempdir, time},
            schema_compiler::{canonical_entity_schema_digest, compile_app_schema},
        },
        artifact_v2::workspace::ArtifactV2Workspace,
    };

    fn installation() -> AppInstallationId {
        AppInstallationId::parse("install_1").unwrap()
    }
    fn id(n: usize) -> AppRecordId {
        AppRecordId::parse(format!("record_{n:05}")).unwrap()
    }
    fn selection() -> AppDataCleanupRequest {
        AppDataCleanupRequest {
            entity: AppName::parse("item").unwrap(),
            timestamp_field: AppFieldPath::parse("created_at").unwrap(),
            before: time(3),
        }
    }
    fn create(n: usize, entity: &str, payload: Value) -> AppMutationOperation {
        AppMutationOperation::Create {
            entity: AppName::parse(entity).unwrap(),
            record_id: Some(id(n)),
            temporary_id: AppName::parse(format!("new_{n}")).unwrap(),
            payload,
        }
    }
    async fn mutate(
        registry: &AppRegistryService,
        scope: &AuthenticatedAppScope,
        key: &str,
        operations: Vec<AppMutationOperation>,
        expected: Vec<AppExpectedRecordRevision>,
        at: DateTime<Utc>,
    ) {
        AppEntityAdapterService::new(registry.clone())
            .owner_mutate(
                scope,
                &installation(),
                AppMutationCommand {
                    protocol_version: AppProtocolVersion::V1,
                    idempotency_key: AppReference::parse(key).unwrap(),
                    atomicity: AppMutationAtomicity::AllOrNothing,
                    expected_schema_revision: AppRevision::new(1).unwrap(),
                    operations,
                    expected_record_revisions: expected,
                },
                at,
            )
            .await
            .unwrap();
    }
    async fn setup(
        count: usize,
    ) -> (
        tempfile::TempDir,
        AppRegistryService,
        AuthenticatedAppScope,
        AppEntityRetentionService,
    ) {
        let root = canonical_tempdir();
        let registry = AppRegistryService::new(ArtifactV2Workspace::new(root.path()));
        let mut declaration = manifest();
        declaration
            .app
            .entities
            .get_mut(&AppName::parse("item").unwrap())
            .unwrap()
            .fields
            .insert(
                AppName::parse("created_at").unwrap(),
                AppManifestField::Timestamp {
                    required: true,
                    nullable: false,
                    data_policy: None,
                },
            );
        declaration
            .app
            .views
            .get_mut(&AppName::parse("items").unwrap())
            .unwrap()
            .columns
            .push(AppName::parse("created_at").unwrap());
        let digest = canonical_entity_schema_digest(&declaration).unwrap();
        let schema = compile_app_schema(
            &declaration,
            installation(),
            AppReference::parse("package:cleanup-fixture").unwrap(),
            AppRevision::new(1).unwrap(),
            policy(),
            AppSchemaCompatibility::Initial,
            None,
            &digest,
            time(1),
        )
        .unwrap()
        .into_revision();
        seed_enabled_installation(&registry, digest, schema, AppInstallationStatus::Enabled).await;
        let original = authenticated_scope("anonymous", "default");
        let scope = AuthenticatedAppScope::from_verified_session(
            original.scope().clone(),
            original.scope_binding_ref().clone(),
            original.actor_ref().clone(),
            original.session_ref().clone(),
            original.authentication_revision(),
            time(0),
            time(0) + Duration::days(2),
        )
        .unwrap();
        for start in (0..count).step_by(64) {
            let ops=(start..(start+64).min(count)).map(|n| create(n,"item",serde_json::json!({"title":"Old record","status":"open","created_at":time(1)}))).collect();
            mutate(
                &registry,
                &scope,
                &format!("mutation:cleanup-seed-{start}"),
                ops,
                vec![],
                time(4),
            )
            .await;
        }
        let service = AppEntityRetentionService::new(registry.clone());
        (root, registry, scope, service)
    }

    #[test]
    fn age_cleanup_auxiliary_history_is_streamed_past_the_old_receipt_limit() {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE auxiliary(installation_id TEXT,id TEXT,record_json BLOB)")
            .unwrap();
        let transaction = connection.transaction().unwrap();
        for n in 0..5000 {
            let record = if n == 4999 { id(0) } else { id(999) };
            transaction
                .execute(
                    "INSERT INTO auxiliary VALUES ('install_1',?1,?2)",
                    params![
                        format!("receipt_{n:05}"),
                        serde_json::to_vec(
                            &serde_json::json!({"entity":"item","record_id":record})
                        )
                        .unwrap()
                    ],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
        let records = BTreeSet::from([(AppName::parse("item").unwrap(), id(0))]);
        let matches = matching_json_rows(
            &connection,
            "SELECT id,record_json FROM auxiliary WHERE installation_id=?1 ORDER BY id",
            &installation(),
            &records,
        )
        .unwrap();
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "receipt_04999");
    }

    #[tokio::test]
    async fn age_cleanup_requires_explicit_bound_confirmation_and_supports_more_than_10000_records()
    {
        let (_root, registry, scope, service) = setup(1).await;
        registry.execute_scoped_write(&scope,&time(5),|connection,_| {
            let tx=connection.transaction()?;
            // Populate a scale fixture through the actual registry schema,
            // copying valid bounded payloads without 10,000 HTTP mutations.
            for table in ["app_record_revisions","app_record_heads","app_scalar_indexes"] {
                let columns=tx.prepare(&format!("PRAGMA table_info({table})"))?.query_map([],|row| row.get::<_,String>(1))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let projection=columns.iter().map(|column| if column=="record_id" {"printf('record_%05d', n)".to_string()} else {format!("base.{column}")}).collect::<Vec<_>>().join(",");
                tx.execute_batch(&format!("WITH RECURSIVE numbers(n) AS (SELECT 1 UNION ALL SELECT n+1 FROM numbers WHERE n<10000)
                    INSERT INTO {table}({}) SELECT {projection} FROM {table} base,numbers
                    WHERE base.installation_id='install_1' AND base.entity_name='item' AND base.record_id='record_00000'",columns.join(",")))?;
            }
            tx.commit()?;Ok(())
        }).await.unwrap();
        let preview = service
            .preview_age_cleanup(&scope, &installation(), selection(), time(6))
            .await
            .unwrap();
        assert_eq!(preview.matching_records, 10_001);
        assert!(matches!(
            service
                .advance_age_cleanup(&scope, &installation(), preview.job_ref.clone(), time(7))
                .await,
            Err(AppEntityRetentionStoreError::CleanupInvalidState)
        ));
        assert!(matches!(
            service
                .control_age_cleanup(
                    &scope,
                    &installation(),
                    preview.job_ref.clone(),
                    "confirm".into(),
                    Some(AppDigest::blake3(b"different")),
                    time(7)
                )
                .await,
            Err(AppEntityRetentionStoreError::CleanupConfirmationMismatch)
        ));
        assert!(service
            .control_age_cleanup(
                &authenticated_scope("another-owner", "default"),
                &installation(),
                preview.job_ref.clone(),
                "confirm".into(),
                Some(preview.preview_digest.clone()),
                time(7)
            )
            .await
            .is_err());
        let other_actor = AuthenticatedAppScope::from_verified_session(
            scope.scope().clone(),
            scope.scope_binding_ref().clone(),
            AppReference::parse("actor:other-owner").unwrap(),
            scope.session_ref().clone(),
            scope.authentication_revision(),
            time(0),
            time(0) + Duration::days(2),
        )
        .unwrap();
        assert!(matches!(
            service
                .control_age_cleanup(
                    &other_actor,
                    &installation(),
                    preview.job_ref.clone(),
                    "confirm".into(),
                    Some(preview.preview_digest.clone()),
                    time(7)
                )
                .await,
            Err(AppEntityRetentionStoreError::ScopeMismatch)
        ));
        assert!(service
            .latest_age_cleanup(&other_actor, &installation(), time(7))
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            service
                .control_age_cleanup(
                    &scope,
                    &installation(),
                    preview.job_ref.clone(),
                    "confirm".into(),
                    Some(preview.preview_digest),
                    time(7) + Duration::minutes(16)
                )
                .await,
            Err(AppEntityRetentionStoreError::CleanupPreviewExpired)
        ));
        let count: i64 = registry
            .execute_scoped_read(&scope, &time(8), |connection, _| {
                Ok(
                    connection.query_row("SELECT COUNT(*) FROM app_record_heads", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 10_001);
        let next = service
            .preview_age_cleanup(
                &scope,
                &installation(),
                selection(),
                time(8) + Duration::minutes(16),
            )
            .await
            .unwrap();
        assert_eq!(next.matching_records, 10_001);
        let old_job = preview.job_ref;
        let candidates: i64 = registry
            .execute_scoped_read(
                &scope,
                &(time(9) + Duration::minutes(16)),
                move |connection, _| {
                    Ok(connection.query_row(
                        "SELECT COUNT(*) FROM app_data_cleanup_candidates WHERE job_ref=?1",
                        [old_job.as_str()],
                        |row| row.get(0),
                    )?)
                },
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            candidates, 0,
            "expired preview identities must not accumulate"
        );
    }

    #[tokio::test]
    async fn age_cleanup_resumes_batches_erases_history_and_keeps_new_changed_and_referenced_records(
    ) {
        let (_root, registry, scope, service) = setup(130).await;
        let preview = service
            .preview_age_cleanup(&scope, &installation(), selection(), time(5))
            .await
            .unwrap();
        mutate(&registry,&scope,"mutation:cleanup-concurrent-edit",vec![AppMutationOperation::Update {
            entity:AppName::parse("item").unwrap(),record_id:id(0),patch:serde_json::json!({"title":"Changed after preview"}),
        },create(999,"item",serde_json::json!({"title":"Backdated new record","status":"open","created_at":time(1)})),
          create(998,"note",serde_json::json!({"body":"Retained note","item":id(1)}))],
          vec![AppExpectedRecordRevision{entity:AppName::parse("item").unwrap(),record_id:id(0),revision:AppRevision::new(1).unwrap()}],time(6)).await;
        let mut job = service
            .control_age_cleanup(
                &scope,
                &installation(),
                preview.job_ref.clone(),
                "confirm".into(),
                Some(preview.preview_digest),
                time(7),
            )
            .await
            .unwrap();
        job = service
            .advance_age_cleanup(&scope, &installation(), job.job_ref.clone(), time(8))
            .await
            .unwrap();
        assert_eq!(job.kept_changed_records, 1);
        assert_eq!(job.kept_referenced_records, 1);
        job = service
            .advance_age_cleanup(&scope, &installation(), job.job_ref.clone(), time(9))
            .await
            .unwrap();
        assert_eq!(job.deleted_records, 64);
        assert_eq!(job.remaining_records, 64);
        job = service
            .control_age_cleanup(
                &scope,
                &installation(),
                job.job_ref,
                "pause".into(),
                None,
                time(10),
            )
            .await
            .unwrap();
        assert!(service
            .advance_age_cleanup(&scope, &installation(), job.job_ref.clone(), time(11))
            .await
            .is_err());
        let reopened = AppEntityRetentionService::new(registry.clone());
        assert_eq!(
            reopened
                .latest_age_cleanup(&scope, &installation(), time(11))
                .await
                .unwrap()
                .unwrap()
                .deleted_records,
            64
        );
        job = reopened
            .control_age_cleanup(
                &scope,
                &installation(),
                job.job_ref,
                "resume".into(),
                None,
                time(12),
            )
            .await
            .unwrap();
        for second in 13..18 {
            job = reopened
                .advance_age_cleanup(&scope, &installation(), job.job_ref.clone(), time(second))
                .await
                .unwrap();
            if job.status == AppDataCleanupStatus::Completed {
                break;
            }
        }
        assert_eq!(job.status, AppDataCleanupStatus::Completed);
        assert_eq!(job.deleted_records, 128);
        assert_eq!(job.remaining_records, 0);
        let replay = reopened
            .advance_age_cleanup(&scope, &installation(), job.job_ref.clone(), time(19))
            .await
            .unwrap();
        assert_eq!(replay.deleted_records, 128);
        let (heads, old_history, retained_note, selection_rows): (i64, i64, i64, i64) = registry
            .execute_scoped_read(&scope, &time(20), |connection, _| {
                Ok((
                    connection.query_row("SELECT COUNT(*) FROM app_record_heads", [], |row| {
                        row.get(0)
                    })?,
                    connection.query_row(
                        "SELECT COUNT(*) FROM app_record_revisions WHERE record_id='record_00002'",
                        [],
                        |row| row.get(0),
                    )?,
                    connection.query_row(
                        "SELECT COUNT(*) FROM app_record_heads WHERE entity_name='note'",
                        [],
                        |row| row.get(0),
                    )?,
                    connection.query_row(
                        "SELECT COUNT(*) FROM app_data_cleanup_candidates",
                        [],
                        |row| row.get(0),
                    )?,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (heads, old_history, retained_note, selection_rows),
            (4, 0, 1, 0)
        );
    }

    #[tokio::test]
    async fn age_cleanup_migrates_v35_in_a_fresh_process_without_losing_records() {
        let (root, registry, scope, _service) = setup(1).await;
        registry
            .execute_scoped_write(&scope, &time(5), |connection, _| {
                let transaction = connection.transaction()?;
                transaction.execute_batch(
                    "DROP TABLE app_data_cleanup_candidates;
                DROP TABLE app_data_cleanup_jobs;
                DROP TABLE app_recurring_task_heads;
                DROP TABLE app_recurring_occurrence_locators;
                DROP TABLE app_behavior_execution_state; PRAGMA user_version=35;",
                )?;
                transaction.commit()?;
                Ok(())
            })
            .await
            .unwrap();
        // A separate process has no warm connection or schema cache. Opening
        // through the actual owner must migrate the existing encrypted store.
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "apps::entity_retention::cleanup::tests::age_cleanup_migration_reopen_child",
                "--test-threads=1",
            ])
            .env("MAGICIAN_AGE_CLEANUP_MIGRATION_ROOT", root.path())
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "migration child failed: {} {}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        let (version, records): (i64, i64) = registry
            .execute_scoped_read(&scope, &time(6), |connection, _| {
                Ok((
                    connection.pragma_query_value(None, "user_version", |row| row.get(0))?,
                    connection.query_row("SELECT COUNT(*) FROM app_record_heads", [], |row| {
                        row.get(0)
                    })?,
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!((version, records), (38, 1));
    }

    #[tokio::test]
    #[ignore = "launched by the migration parent with a private fixture root"]
    async fn age_cleanup_migration_reopen_child() {
        let root = std::env::var_os("MAGICIAN_AGE_CLEANUP_MIGRATION_ROOT")
            .expect("run the migration parent test");
        let registry =
            AppRegistryService::new(ArtifactV2Workspace::new(std::path::PathBuf::from(root)));
        let scope = authenticated_scope("anonymous", "default");
        let service = AppEntityRetentionService::new(registry);
        let preview = service
            .preview_age_cleanup(&scope, &installation(), selection(), time(5))
            .await
            .unwrap();
        assert_eq!(preview.matching_records, 1);
        assert_eq!(preview.deleted_records, 0);
    }

    #[tokio::test]
    async fn age_cleanup_stop_checkpoints_deleted_records_and_keeps_the_remainder() {
        let (_root, registry, scope, service) = setup(70).await;
        let preview = service
            .preview_age_cleanup(&scope, &installation(), selection(), time(5))
            .await
            .unwrap();
        let mut job = service
            .control_age_cleanup(
                &scope,
                &installation(),
                preview.job_ref,
                "confirm".into(),
                Some(preview.preview_digest),
                time(6),
            )
            .await
            .unwrap();
        job = service
            .advance_age_cleanup(&scope, &installation(), job.job_ref, time(7))
            .await
            .unwrap();
        assert_eq!(job.deleted_records, 64);
        job = service
            .control_age_cleanup(
                &scope,
                &installation(),
                job.job_ref,
                "cancel".into(),
                None,
                time(8),
            )
            .await
            .unwrap();
        assert_eq!(job.status, AppDataCleanupStatus::Checkpointing);
        assert_eq!(job.remaining_records, 6);
        let reopened = AppEntityRetentionService::new(registry.clone());
        job = reopened
            .advance_age_cleanup(&scope, &installation(), job.job_ref, time(9))
            .await
            .unwrap();
        assert_eq!(job.status, AppDataCleanupStatus::Cancelled);
        let replay = reopened
            .advance_age_cleanup(&scope, &installation(), job.job_ref, time(10))
            .await
            .unwrap();
        assert_eq!((replay.deleted_records, replay.remaining_records), (64, 6));
        let count: i64 = registry
            .execute_scoped_read(&scope, &time(11), |connection, _| {
                Ok(
                    connection.query_row("SELECT COUNT(*) FROM app_record_heads", [], |row| {
                        row.get(0)
                    })?,
                )
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 6);
    }

    #[tokio::test]
    async fn age_cleanup_rejects_non_timestamp_fields_and_installation_changes() {
        let (_root, registry, scope, service) = setup(1).await;
        let mut invalid = selection();
        invalid.timestamp_field = AppFieldPath::parse("status").unwrap();
        assert!(service
            .preview_age_cleanup(&scope, &installation(), invalid, time(5))
            .await
            .is_err());
        let preview = service
            .preview_age_cleanup(&scope, &installation(), selection(), time(5))
            .await
            .unwrap();
        service
            .control_age_cleanup(
                &scope,
                &installation(),
                preview.job_ref.clone(),
                "confirm".into(),
                Some(preview.preview_digest),
                time(6),
            )
            .await
            .unwrap();
        registry.execute_scoped_write(&scope,&time(7),|connection,_| {
            let bytes:Vec<u8>=connection.query_row("SELECT record_json FROM app_installations WHERE installation_id='install_1'",[],|row|row.get(0))?;
            let mut row:Value=serde_json::from_slice(&bytes).unwrap();row["lifecycle"]["generation"]=serde_json::json!(3);
            connection.execute("UPDATE app_installations SET lifecycle_generation=3,record_json=?1 WHERE installation_id='install_1'",[serde_json::to_vec(&row).unwrap()])?;Ok(())
        }).await.unwrap();
        assert!(matches!(
            service
                .advance_age_cleanup(&scope, &installation(), preview.job_ref, time(8))
                .await,
            Err(AppEntityRetentionStoreError::GenerationConflict)
        ));
    }
}

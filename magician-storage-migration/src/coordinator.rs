use std::path::Path;

use chrono::Utc;
use magician_storage::{
    legal_transition, MigrationCounts, MigrationDigests, MigrationPhase, StorageCatalogId,
    StorageError, StorageMigrationRecord, StorageScope,
};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::handler::{
    ExportBlob, OwnerInventory, OwnerMigrationHandler, OwnerMigrationRegistry, VerifyReport,
};
use crate::ledger::{sanitize_json, FileLedger, LedgerEntry, PhaseEvidence};

#[derive(Debug, Clone)]
pub struct PlanRequest {
    pub store_id: StorageCatalogId,
    pub scope: StorageScope,
    pub source_profile: String,
    pub target_profile: String,
}

#[derive(Debug, Clone, Copy)]
pub struct CrashSpec {
    pub phase: MigrationPhase,
    pub before_complete: bool,
}

#[derive(Debug, Clone)]
pub struct ResumeStatus {
    pub record: StorageMigrationRecord,
    pub fence_generation: u64,
    pub needs_retry: bool,
}

pub struct MigrationCoordinator {
    ledger: FileLedger,
    registry: OwnerMigrationRegistry,
    crash: Option<CrashSpec>,
}

impl MigrationCoordinator {
    pub async fn open(
        root: impl AsRef<Path>,
        registry: OwnerMigrationRegistry,
    ) -> Result<Self, StorageError> {
        Ok(Self {
            ledger: FileLedger::open(root).await?,
            registry,
            crash: None,
        })
    }

    pub fn with_crash(mut self, spec: CrashSpec) -> Self {
        self.crash = Some(spec);
        self
    }

    pub fn registry(&self) -> &OwnerMigrationRegistry {
        &self.registry
    }

    pub async fn inventory(
        &self,
        owner_id: &str,
        scope: &StorageScope,
    ) -> Result<OwnerInventory, StorageError> {
        self.registry.get(owner_id)?.inventory(scope).await
    }

    pub async fn status(&self, migration_id: Uuid) -> Result<LedgerEntry, StorageError> {
        self.ledger.get(migration_id).await
    }

    pub async fn find_open(
        &self,
        request: &PlanRequest,
    ) -> Result<Option<LedgerEntry>, StorageError> {
        self.ledger
            .find_open(
                request.store_id.as_str(),
                &request.scope,
                &request.source_profile,
                &request.target_profile,
            )
            .await
    }

    pub async fn plan(&self, request: PlanRequest) -> Result<StorageMigrationRecord, StorageError> {
        let handler = self.registry.get(request.store_id.as_str())?;
        let watermark = handler.watermark(&request.scope).await?;
        if let Some(existing) = self
            .ledger
            .find_open(
                request.store_id.as_str(),
                &request.scope,
                &request.source_profile,
                &request.target_profile,
            )
            .await?
        {
            if existing.record.source_generation != watermark.generation {
                return Err(wrong_generation(
                    &existing.record.source_generation,
                    &watermark.generation,
                ));
            }
            return Ok(existing.record);
        }
        let now = Utc::now();
        let record = StorageMigrationRecord {
            migration_id: Uuid::new_v4(),
            run_id: Uuid::new_v4(),
            store_id: request.store_id,
            scope: request.scope.clone(),
            source_profile: request.source_profile,
            target_profile: request.target_profile,
            source_generation: watermark.generation.clone(),
            phase: MigrationPhase::Planned,
            counts: MigrationCounts {
                records: 0,
                bytes: 0,
            },
            digests: MigrationDigests {
                payload: String::new(),
            },
            started_at: now,
            updated_at: now,
            cutover_fencing_generation: None,
        };
        let inventory = handler.inventory(&request.scope).await?;
        let mut entry = LedgerEntry {
            record,
            fence_generation: 0,
            evidence: Default::default(),
            export_digest: None,
        };
        attach_evidence(
            &mut entry,
            MigrationPhase::Planned,
            json!({
                "record_count": inventory.record_count,
                "watermark": inventory.watermark,
                "layout": inventory.layout,
            }),
            true,
        );
        self.persist(&entry, MigrationPhase::Planned, true).await?;
        Ok(entry.record)
    }

    pub async fn export(&self, migration_id: Uuid) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::Exporting) {
            return Ok(entry.record);
        }
        if entry.record.phase == MigrationPhase::Planned {
            self.enter_phase(&mut entry, MigrationPhase::Exporting)
                .await?;
        }
        Self::require_phase(&entry, MigrationPhase::Exporting)?;
        if entry.phase_complete(MigrationPhase::Exporting) {
            return Ok(entry.record);
        }
        let handler = self.handler(&entry)?;
        let blob = handler.export(&entry.record.scope).await?;
        self.ledger
            .put_export(entry.record.migration_id, &blob.bytes)
            .await?;
        entry.record.counts = MigrationCounts {
            records: blob.records,
            bytes: blob.bytes.len() as u64,
        };
        entry.record.digests.payload = blob.digest.clone();
        entry.export_digest = Some(blob.digest.clone());
        attach_evidence(
            &mut entry,
            MigrationPhase::Exporting,
            json!({ "records": blob.records, "digest": blob.digest }),
            true,
        );
        self.persist(&entry, MigrationPhase::Exporting, true)
            .await?;
        Ok(entry.record)
    }

    pub async fn import(&self, migration_id: Uuid) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::Imported) {
            return Ok(entry.record);
        }
        Self::require_complete(&entry, MigrationPhase::Exporting)?;
        if entry.record.phase == MigrationPhase::Exporting {
            self.enter_phase(&mut entry, MigrationPhase::Imported)
                .await?;
        }
        Self::require_phase(&entry, MigrationPhase::Imported)?;
        if entry.phase_complete(MigrationPhase::Imported) {
            return Ok(entry.record);
        }
        let blob = self.load_blob(&entry).await?;
        let receipt = self
            .handler(&entry)?
            .import(&entry.record.scope, &blob)
            .await?;
        attach_evidence(
            &mut entry,
            MigrationPhase::Imported,
            json!({ "records": receipt.records, "digest": receipt.digest }),
            true,
        );
        self.persist(&entry, MigrationPhase::Imported, true).await?;
        Ok(entry.record)
    }

    pub async fn checkpoint(
        &self,
        migration_id: Uuid,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if entry.record.phase.is_terminal() {
            return Err(StorageError::Conflict {
                expected: Some("in_progress".into()),
                actual: Some(entry.record.phase.as_str().into()),
            });
        }
        let mark = self
            .handler(&entry)?
            .checkpoint(&entry.record.scope)
            .await?;
        let phase = entry.record.phase;
        let payload = json!({ "watermark": mark.watermark, "phase": phase.as_str() });
        let complete = entry.phase_complete(phase);
        attach_evidence(&mut entry, phase, payload, complete);
        self.ledger.put(&entry).await?;
        Ok(entry.record)
    }

    pub async fn semantic_verify(
        &self,
        migration_id: Uuid,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::Verified) {
            return Ok(entry.record);
        }
        Self::require_complete(&entry, MigrationPhase::Imported)?;
        if entry.record.phase == MigrationPhase::Imported {
            self.enter_phase(&mut entry, MigrationPhase::Verified)
                .await?;
        }
        Self::require_phase(&entry, MigrationPhase::Verified)?;
        if entry.phase_complete(MigrationPhase::Verified) {
            return Ok(entry.record);
        }
        let blob = self.load_blob(&entry).await?;
        let report = self
            .handler(&entry)?
            .semantic_verify(&entry.record.scope, &blob)
            .await?;
        if !report.passed() {
            return Err(StorageError::Integrity {
                expected: "verify_pass".into(),
                actual: format!("{report:?}"),
            });
        }
        attach_evidence(
            &mut entry,
            MigrationPhase::Verified,
            verify_json(&report),
            true,
        );
        self.persist(&entry, MigrationPhase::Verified, true).await?;
        Ok(entry.record)
    }

    pub async fn prepare_cutover(
        &self,
        migration_id: Uuid,
        fence: u64,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::CutoverPending) {
            return Ok(entry.record);
        }
        Self::require_fence(&entry, fence)?;
        Self::require_complete(&entry, MigrationPhase::Verified)?;
        if entry.record.phase == MigrationPhase::Verified {
            self.enter_phase(&mut entry, MigrationPhase::CutoverPending)
                .await?;
        }
        Self::require_phase(&entry, MigrationPhase::CutoverPending)?;
        if entry.phase_complete(MigrationPhase::CutoverPending) {
            return Ok(entry.record);
        }
        self.handler(&entry)?
            .prepare_cutover(&entry.record.scope)
            .await?;
        let next_fence = entry.fence_generation.saturating_add(1);
        entry.record.cutover_fencing_generation = Some(next_fence);
        attach_evidence(
            &mut entry,
            MigrationPhase::CutoverPending,
            json!({ "cutover_fencing_generation": next_fence }),
            true,
        );
        self.persist(&entry, MigrationPhase::CutoverPending, true)
            .await?;
        Ok(entry.record)
    }

    pub async fn cutover(
        &self,
        migration_id: Uuid,
        fence: u64,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::Cutover) {
            return Ok(entry.record);
        }
        let expected = entry
            .record
            .cutover_fencing_generation
            .ok_or_else(|| StorageError::invalid_key("cutover fence missing"))?;
        if fence != expected {
            return Err(StorageError::LeaseLost {
                resource: entry.record.migration_id.to_string(),
                generation: expected,
            });
        }
        Self::require_complete(&entry, MigrationPhase::CutoverPending)?;
        if entry.record.phase == MigrationPhase::CutoverPending {
            self.enter_phase(&mut entry, MigrationPhase::Cutover)
                .await?;
        }
        Self::require_phase(&entry, MigrationPhase::Cutover)?;
        if entry.phase_complete(MigrationPhase::Cutover) {
            return Ok(entry.record);
        }
        self.handler(&entry)?.cutover(&entry.record.scope).await?;
        entry.fence_generation = expected;
        attach_evidence(
            &mut entry,
            MigrationPhase::Cutover,
            json!({ "canonical": "remote", "fence": expected }),
            true,
        );
        self.persist(&entry, MigrationPhase::Cutover, true).await?;
        Ok(entry.record)
    }

    pub async fn settle(
        &self,
        migration_id: Uuid,
        fence: u64,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        if already_complete_forward(&entry, MigrationPhase::Settled) {
            return Ok(entry.record);
        }
        Self::require_fence(&entry, fence)?;
        let from = entry.record.phase;
        if !matches!(from, MigrationPhase::Cutover | MigrationPhase::RolledBack) {
            return Err(StorageError::Conflict {
                expected: Some("cutover_or_rolled_back".into()),
                actual: Some(from.as_str().into()),
            });
        }
        if from != MigrationPhase::Settled {
            self.enter_phase(&mut entry, MigrationPhase::Settled)
                .await?;
        }
        if entry.phase_complete(MigrationPhase::Settled) {
            return Ok(entry.record);
        }
        attach_evidence(
            &mut entry,
            MigrationPhase::Settled,
            json!({ "legacy_source": "retained" }),
            true,
        );
        self.persist(&entry, MigrationPhase::Settled, true).await?;
        Ok(entry.record)
    }

    pub async fn rollback(
        &self,
        migration_id: Uuid,
        fence: u64,
    ) -> Result<StorageMigrationRecord, StorageError> {
        let mut entry = self.load_matching(migration_id).await?;
        Self::require_fence(&entry, fence)?;
        if !legal_transition(Some(entry.record.phase), MigrationPhase::RollbackPending)
            && entry.record.phase != MigrationPhase::RollbackPending
            && entry.record.phase != MigrationPhase::RolledBack
        {
            return Err(StorageError::Conflict {
                expected: Some("rollback_eligible".into()),
                actual: Some(entry.record.phase.as_str().into()),
            });
        }
        // Invalidate after the transition. enter_phase requires the current
        // forward phase (Cutover) to still be complete; clearing it first
        // fails closed as "missing evidence for cutover" and blocks rollback.
        if entry.record.phase != MigrationPhase::RollbackPending
            && entry.record.phase != MigrationPhase::RolledBack
        {
            self.enter_phase(&mut entry, MigrationPhase::RollbackPending)
                .await?;
            invalidate_forward_evidence(&mut entry);
        }
        if !entry.phase_complete(MigrationPhase::RollbackPending) {
            let fence_now = entry.fence_generation;
            attach_evidence(
                &mut entry,
                MigrationPhase::RollbackPending,
                json!({ "fence": fence_now }),
                true,
            );
            self.persist(&entry, MigrationPhase::RollbackPending, true)
                .await?;
        }
        if entry.record.phase == MigrationPhase::RollbackPending {
            self.enter_phase(&mut entry, MigrationPhase::RolledBack)
                .await?;
        }
        if entry.phase_complete(MigrationPhase::RolledBack) {
            return Ok(entry.record);
        }
        self.handler(&entry)?.rollback(&entry.record.scope).await?;
        let next_fence = entry.fence_generation.saturating_add(1);
        entry.fence_generation = next_fence;
        entry.record.cutover_fencing_generation = Some(next_fence);
        attach_evidence(
            &mut entry,
            MigrationPhase::RolledBack,
            json!({ "fence": next_fence, "canonical": "local" }),
            true,
        );
        self.persist(&entry, MigrationPhase::RolledBack, true)
            .await?;
        Ok(entry.record)
    }

    pub async fn resume(&self, migration_id: Uuid) -> Result<ResumeStatus, StorageError> {
        let entry = self.load_matching(migration_id).await?;
        Ok(ResumeStatus {
            record: entry.record.clone(),
            fence_generation: entry.fence_generation,
            needs_retry: !entry.phase_complete(entry.record.phase),
        })
    }

    async fn load_matching(&self, migration_id: Uuid) -> Result<LedgerEntry, StorageError> {
        let entry = self.ledger.get(migration_id).await?;
        if entry.record.phase.is_terminal() {
            return Ok(entry);
        }
        let current = self.handler(&entry)?.watermark(&entry.record.scope).await?;
        if current.generation != entry.record.source_generation {
            return Err(wrong_generation(
                &entry.record.source_generation,
                &current.generation,
            ));
        }
        Ok(entry)
    }

    async fn enter_phase(
        &self,
        entry: &mut LedgerEntry,
        to: MigrationPhase,
    ) -> Result<(), StorageError> {
        if entry.record.phase == to {
            return Ok(());
        }
        if !legal_transition(Some(entry.record.phase), to) {
            return Err(StorageError::Conflict {
                expected: Some(to.as_str().into()),
                actual: Some(entry.record.phase.as_str().into()),
            });
        }
        Self::require_complete(entry, entry.record.phase)?;
        entry.record.phase = to;
        entry.record.run_id = Uuid::new_v4();
        entry.record.updated_at = Utc::now();
        attach_evidence(entry, to, json!({}), false);
        self.persist(entry, to, false).await
    }

    async fn persist(
        &self,
        entry: &LedgerEntry,
        phase: MigrationPhase,
        complete: bool,
    ) -> Result<(), StorageError> {
        self.ledger.put(entry).await?;
        if let Some(spec) = self.crash {
            if spec.phase == phase && spec.before_complete != complete {
                return Err(StorageError::Unavailable { retry_after: None });
            }
        }
        Ok(())
    }

    fn handler(
        &self,
        entry: &LedgerEntry,
    ) -> Result<std::sync::Arc<dyn OwnerMigrationHandler>, StorageError> {
        self.registry.get(entry.record.store_id.as_str())
    }

    async fn load_blob(&self, entry: &LedgerEntry) -> Result<ExportBlob, StorageError> {
        let bytes = self.ledger.get_export(entry.record.migration_id).await?;
        let digest = entry
            .export_digest
            .clone()
            .ok_or_else(|| StorageError::invalid_key("export digest missing"))?;
        Ok(ExportBlob {
            records: entry.record.counts.records,
            digest,
            bytes,
        })
    }

    fn require_phase(entry: &LedgerEntry, phase: MigrationPhase) -> Result<(), StorageError> {
        if entry.record.phase == phase {
            Ok(())
        } else {
            Err(StorageError::Conflict {
                expected: Some(phase.as_str().into()),
                actual: Some(entry.record.phase.as_str().into()),
            })
        }
    }

    fn require_complete(entry: &LedgerEntry, phase: MigrationPhase) -> Result<(), StorageError> {
        if entry.phase_complete(phase) {
            Ok(())
        } else {
            Err(StorageError::invalid_key(format!(
                "missing evidence for {phase}"
            )))
        }
    }

    fn require_fence(entry: &LedgerEntry, fence: u64) -> Result<(), StorageError> {
        if fence == entry.fence_generation {
            Ok(())
        } else {
            Err(StorageError::LeaseLost {
                resource: entry.record.migration_id.to_string(),
                generation: entry.fence_generation,
            })
        }
    }
}

fn already_complete_forward(entry: &LedgerEntry, phase: MigrationPhase) -> bool {
    !matches!(
        entry.record.phase,
        MigrationPhase::RollbackPending | MigrationPhase::RolledBack
    ) && !entry.phase_complete(MigrationPhase::RolledBack)
        && entry.phase_complete(phase)
}

fn invalidate_forward_evidence(entry: &mut LedgerEntry) {
    for phase in [
        MigrationPhase::Exporting,
        MigrationPhase::Imported,
        MigrationPhase::Verified,
        MigrationPhase::CutoverPending,
        MigrationPhase::Cutover,
    ] {
        if let Some(evidence) = entry.evidence.get_mut(phase.as_str()) {
            evidence.complete = false;
        }
    }
}

fn attach_evidence(entry: &mut LedgerEntry, phase: MigrationPhase, payload: Value, complete: bool) {
    entry.record.updated_at = Utc::now();
    entry.evidence.insert(
        phase.as_str().to_string(),
        PhaseEvidence {
            complete,
            payload: sanitize_json(payload),
        },
    );
}

fn verify_json(report: &VerifyReport) -> Value {
    json!({
        "counts_match": report.counts_match,
        "identifiers_match": report.identifiers_match,
        "digest_match": report.digest_match,
        "semantic_match": report.semantic_match,
    })
}

fn wrong_generation(expected: &str, actual: &str) -> StorageError {
    StorageError::Conflict {
        expected: Some(expected.to_string()),
        actual: Some(actual.to_string()),
    }
}

//! Migration handler for the sent-index owner. Not invoked at default startup.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use magician_storage::{ScopeId, SourceWatermark, StorageCatalogId, StorageError, StorageScope};
use magician_storage_migration::{
    Checkpoint, ExportBlob, ImportReceipt, OwnerInventory, OwnerMigrationHandler, RollbackReceipt,
    SourceGuard, VerifyReport,
};

use super::guard::{sent_index_source_guard, SENT_INDEX_OWNER_ID};
use super::sent_index::{identifier_from_parts, SentIndexRecord};
use super::store::SentMessageStore;
use crate::magician_v2::delivery::DeliveryScope;

enum Canonical {
    Local,
    Remote,
}

pub struct SentIndexMigrationHandler {
    local: Arc<dyn SentMessageStore>,
    remote: Arc<dyn SentMessageStore>,
    canonical: Mutex<Canonical>,
    local_frozen: Mutex<bool>,
    guard: SourceGuard,
}

impl SentIndexMigrationHandler {
    pub fn new(local: Arc<dyn SentMessageStore>, remote: Arc<dyn SentMessageStore>) -> Self {
        Self {
            local,
            remote,
            canonical: Mutex::new(Canonical::Local),
            local_frozen: Mutex::new(false),
            guard: sent_index_source_guard(),
        }
    }

    fn delivery_scope(scope: &StorageScope) -> Result<DeliveryScope, StorageError> {
        match scope {
            StorageScope::Tenant(ScopeId {
                principal,
                workspace,
            }) => Ok(DeliveryScope::new(principal.as_str(), workspace.as_str())),
            StorageScope::System => Err(StorageError::invalid_key("sent-index is tenant scoped")),
        }
    }

    fn digest(records: &[SentIndexRecord]) -> String {
        let bytes = serde_json::to_vec(records).unwrap_or_default();
        blake3::hash(&bytes).to_hex().to_string()
    }
}

#[async_trait]
impl OwnerMigrationHandler for SentIndexMigrationHandler {
    fn owner_id(&self) -> StorageCatalogId {
        StorageCatalogId::parse(SENT_INDEX_OWNER_ID).expect("catalog id")
    }

    fn source_guard(&self) -> &SourceGuard {
        &self.guard
    }

    async fn inventory(&self, scope: &StorageScope) -> Result<OwnerInventory, StorageError> {
        let delivery = Self::delivery_scope(scope)?;
        let records = self
            .local
            .list_scope(&delivery)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(OwnerInventory {
            owner_id: self.owner_id(),
            scope: scope.clone(),
            record_count: records.len() as u64,
            watermark: Self::digest(&records),
            layout: "scopes/{principal}/{workspace}/delivery/index/sent/*.jsonl".into(),
        })
    }

    async fn watermark(&self, scope: &StorageScope) -> Result<SourceWatermark, StorageError> {
        Ok(SourceWatermark {
            generation: self.inventory(scope).await?.watermark,
        })
    }

    async fn export(&self, scope: &StorageScope) -> Result<ExportBlob, StorageError> {
        if *self.local_frozen.lock().expect("freeze lock") {
            return Err(StorageError::PermissionDenied);
        }
        let delivery = Self::delivery_scope(scope)?;
        let bytes = self
            .local
            .export_scope(&delivery)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let records: Vec<SentIndexRecord> = serde_json::from_slice(&bytes).unwrap_or_default();
        Ok(ExportBlob {
            digest: Self::digest(&records),
            records: records.len() as u64,
            bytes,
        })
    }

    async fn import(
        &self,
        scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<ImportReceipt, StorageError> {
        let delivery = Self::delivery_scope(scope)?;
        self.remote
            .import_scope(&delivery, &blob.bytes)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(ImportReceipt {
            records: blob.records,
            digest: blob.digest.clone(),
        })
    }

    async fn checkpoint(&self, scope: &StorageScope) -> Result<Checkpoint, StorageError> {
        Ok(Checkpoint {
            watermark: self.watermark(scope).await?.generation,
        })
    }

    async fn semantic_verify(
        &self,
        scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<VerifyReport, StorageError> {
        let delivery = Self::delivery_scope(scope)?;
        let exported = super::sent_index::fold_scope_records(
            serde_json::from_slice(&blob.bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?,
        );
        let remote = super::sent_index::fold_scope_records(
            self.remote
                .list_scope(&delivery)
                .map_err(|err| StorageError::backend(err.to_string()))?,
        );
        let mut identifiers = std::collections::BTreeSet::new();
        for row in exported.iter().chain(remote.iter()) {
            identifiers.insert((
                row.provider.clone(),
                row.id_kind.clone(),
                row.id_value.clone(),
            ));
        }
        let mut semantic = exported.len() == remote.len();
        for (provider, id_kind, id_value) in identifiers {
            let id = identifier_from_parts(&id_kind, &id_value)
                .map_err(|err| StorageError::backend(err.to_string()))?;
            let found = self
                .remote
                .lookup(&delivery, &provider, &id)
                .map_err(|err| StorageError::backend(err.to_string()))?;
            let expected = super::sent_index::binding_from_records(
                exported
                    .iter()
                    .filter(|row| {
                        row.provider == provider
                            && row.id_kind == id_kind
                            && row.id_value == id_value
                    })
                    .cloned()
                    .collect(),
            );
            if found != expected {
                semantic = false;
            }
        }
        Ok(VerifyReport {
            counts_match: exported.len() == remote.len(),
            identifiers_match: exported
                .iter()
                .map(|r| {
                    (
                        &r.provider,
                        &r.id_kind,
                        &r.id_value,
                        &r.act_ref,
                        &r.audience,
                        r.sent_at,
                    )
                })
                .eq(remote.iter().map(|r| {
                    (
                        &r.provider,
                        &r.id_kind,
                        &r.id_value,
                        &r.act_ref,
                        &r.audience,
                        r.sent_at,
                    )
                })),
            digest_match: Self::digest(&exported) == Self::digest(&remote),
            semantic_match: semantic,
        })
    }

    async fn rollback(&self, _scope: &StorageScope) -> Result<RollbackReceipt, StorageError> {
        *self.local_frozen.lock().expect("freeze lock") = false;
        *self.canonical.lock().expect("canonical lock") = Canonical::Local;
        Ok(RollbackReceipt {
            fencing_generation: 1,
        })
    }

    async fn prepare_cutover(&self, _scope: &StorageScope) -> Result<(), StorageError> {
        *self.local_frozen.lock().expect("freeze lock") = true;
        Ok(())
    }

    async fn cutover(&self, _scope: &StorageScope) -> Result<(), StorageError> {
        *self.canonical.lock().expect("canonical lock") = Canonical::Remote;
        Ok(())
    }
}

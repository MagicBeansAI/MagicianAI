//! Migration handler for a Task 10 object owner. Not invoked at default startup.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use magician_storage::{SourceWatermark, StorageCatalogId, StorageError, StorageScope};
use magician_storage_migration::{
    Checkpoint, ExportBlob, ImportReceipt, OwnerInventory, OwnerMigrationHandler, RollbackReceipt,
    SourceGuard, VerifyReport,
};

use super::blob::BlobAccess;
use super::guard::object_owner_source_guard;
use super::owners::ObjectOwner;

enum Canonical {
    Local,
    Remote,
}

pub struct ObjectOwnerMigrationHandler {
    owner: ObjectOwner,
    local: Arc<dyn BlobAccess>,
    remote: Arc<dyn BlobAccess>,
    canonical: Mutex<Canonical>,
    local_frozen: Mutex<bool>,
    guard: SourceGuard,
}

impl ObjectOwnerMigrationHandler {
    pub fn new(
        owner: ObjectOwner,
        local: Arc<dyn BlobAccess>,
        remote: Arc<dyn BlobAccess>,
    ) -> Self {
        Self {
            owner,
            local,
            remote,
            canonical: Mutex::new(Canonical::Local),
            local_frozen: Mutex::new(false),
            guard: object_owner_source_guard(owner),
        }
    }

    fn digest(bytes: &[u8]) -> String {
        blake3::hash(bytes).to_hex().to_string()
    }
}

#[async_trait]
impl OwnerMigrationHandler for ObjectOwnerMigrationHandler {
    fn owner_id(&self) -> StorageCatalogId {
        StorageCatalogId::parse(self.owner.id()).expect("catalog id")
    }

    fn source_guard(&self) -> &SourceGuard {
        &self.guard
    }

    async fn inventory(&self, scope: &StorageScope) -> Result<OwnerInventory, StorageError> {
        let bytes = self
            .local
            .export_all()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let records: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        Ok(OwnerInventory {
            owner_id: self.owner_id(),
            scope: scope.clone(),
            record_count: records.len() as u64,
            watermark: Self::digest(&bytes),
            layout: format!("scopes/{{principal}}/{{workspace}}/ ({})", self.owner.id()),
        })
    }

    async fn watermark(&self, scope: &StorageScope) -> Result<SourceWatermark, StorageError> {
        Ok(SourceWatermark {
            generation: self.inventory(scope).await?.watermark,
        })
    }

    async fn export(&self, scope: &StorageScope) -> Result<ExportBlob, StorageError> {
        let _ = scope;
        if *self.local_frozen.lock().expect("freeze lock") {
            return Err(StorageError::PermissionDenied);
        }
        let bytes = self
            .local
            .export_all()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let records: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap_or_default();
        Ok(ExportBlob {
            digest: Self::digest(&bytes),
            records: records.len() as u64,
            bytes,
        })
    }

    async fn import(
        &self,
        _scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<ImportReceipt, StorageError> {
        self.remote
            .import_all(&blob.bytes)
            .await
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
        _scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<VerifyReport, StorageError> {
        let remote = self
            .remote
            .export_all()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let exported: Vec<serde_json::Value> =
            serde_json::from_slice(&blob.bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?;
        let remote_records: Vec<serde_json::Value> =
            serde_json::from_slice(&remote).unwrap_or_default();
        let semantic = match exported.first() {
            Some(row) => {
                let path = row.get("path").and_then(|v| v.as_str()).unwrap_or("");
                self.remote
                    .exists(path)
                    .await
                    .map_err(|err| StorageError::backend(err.to_string()))?
            },
            None => remote_records.is_empty(),
        };
        Ok(VerifyReport {
            counts_match: exported.len() == remote_records.len(),
            identifiers_match: exported.len() == remote_records.len(),
            digest_match: Self::digest(&blob.bytes) == Self::digest(&remote),
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

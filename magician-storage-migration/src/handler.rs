use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use magician_storage::{SourceWatermark, StorageCatalogId, StorageError, StorageScope};

use crate::source_guard::SourceGuard;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnerInventory {
    pub owner_id: StorageCatalogId,
    pub scope: StorageScope,
    pub record_count: u64,
    pub watermark: String,
    pub layout: String,
}

pub struct ExportBlob {
    pub bytes: Vec<u8>,
    pub digest: String,
    pub records: u64,
}

impl fmt::Debug for ExportBlob {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportBlob")
            .field("digest", &self.digest)
            .field("records", &self.records)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportReceipt {
    pub records: u64,
    pub digest: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Checkpoint {
    pub watermark: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub counts_match: bool,
    pub identifiers_match: bool,
    pub digest_match: bool,
    pub semantic_match: bool,
}

impl VerifyReport {
    pub fn passed(&self) -> bool {
        self.counts_match && self.identifiers_match && self.digest_match && self.semantic_match
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RollbackReceipt {
    pub fencing_generation: u64,
}

/// Typed per-owner migration contract. Product models stay in owner crates.
#[async_trait]
pub trait OwnerMigrationHandler: Send + Sync {
    fn owner_id(&self) -> StorageCatalogId;
    fn source_guard(&self) -> &SourceGuard;

    async fn inventory(&self, scope: &StorageScope) -> Result<OwnerInventory, StorageError>;
    async fn watermark(&self, scope: &StorageScope) -> Result<SourceWatermark, StorageError>;
    async fn export(&self, scope: &StorageScope) -> Result<ExportBlob, StorageError>;
    async fn import(
        &self,
        scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<ImportReceipt, StorageError>;
    async fn checkpoint(&self, scope: &StorageScope) -> Result<Checkpoint, StorageError>;
    async fn semantic_verify(
        &self,
        scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<VerifyReport, StorageError>;
    async fn rollback(&self, scope: &StorageScope) -> Result<RollbackReceipt, StorageError>;
    async fn prepare_cutover(&self, scope: &StorageScope) -> Result<(), StorageError>;
    async fn cutover(&self, scope: &StorageScope) -> Result<(), StorageError>;
}

#[derive(Default)]
pub struct OwnerMigrationRegistry {
    handlers: BTreeMap<String, Arc<dyn OwnerMigrationHandler>>,
}

impl OwnerMigrationRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(
        &mut self,
        handler: Arc<dyn OwnerMigrationHandler>,
    ) -> Result<(), StorageError> {
        let id = handler.owner_id();
        if self.handlers.contains_key(id.as_str()) {
            return Err(StorageError::Conflict {
                expected: None,
                actual: Some(id.as_str().to_string()),
            });
        }
        self.handlers.insert(id.as_str().to_string(), handler);
        Ok(())
    }

    pub fn get(&self, owner_id: &str) -> Result<Arc<dyn OwnerMigrationHandler>, StorageError> {
        self.handlers
            .get(owner_id)
            .cloned()
            .ok_or_else(|| StorageError::invalid_key("unknown owner"))
    }

    pub fn list(&self) -> Vec<StorageCatalogId> {
        self.handlers
            .values()
            .map(|handler| handler.owner_id())
            .collect()
    }
}

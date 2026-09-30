use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use magician_storage::{
    RepositoryScenarioFactory, ScopeId, SourceWatermark, StorageCatalogId, StorageError,
    StorageScope,
};
use serde::{Deserialize, Serialize};

use crate::handler::{
    Checkpoint, ExportBlob, ImportReceipt, OwnerInventory, OwnerMigrationHandler, RollbackReceipt,
    VerifyReport,
};
use crate::source_guard::SourceGuard;

pub const SYNTHETIC_OWNER_ID: &str = "synthetic_owner";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyntheticItem {
    pub principal: String,
    pub workspace: String,
    pub id: String,
    pub revision: u64,
    pub payload: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Canonical {
    Local,
    Remote,
}

type ItemMap = BTreeMap<(String, String, String), SyntheticItem>;

#[derive(Default)]
struct Store {
    items: ItemMap,
    quarantine: ItemMap,
    frozen: bool,
}

impl Store {
    fn scope_key(scope: &StorageScope) -> Result<(String, String), StorageError> {
        match scope {
            StorageScope::Tenant(ScopeId {
                principal,
                workspace,
            }) => Ok((
                principal.as_str().to_string(),
                workspace.as_str().to_string(),
            )),
            StorageScope::System => Err(StorageError::invalid_key(
                "synthetic owner is tenant scoped",
            )),
        }
    }

    fn list(&self, scope: &StorageScope) -> Result<Vec<SyntheticItem>, StorageError> {
        let (principal, workspace) = Self::scope_key(scope)?;
        let mut items: Vec<_> = self
            .items
            .values()
            .filter(|item| item.principal == principal && item.workspace == workspace)
            .cloned()
            .collect();
        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(items)
    }

    fn put(&mut self, item: SyntheticItem) -> Result<(), StorageError> {
        if self.frozen {
            return Err(StorageError::PermissionDenied);
        }
        self.items.insert(
            (
                item.principal.clone(),
                item.workspace.clone(),
                item.id.clone(),
            ),
            item,
        );
        Ok(())
    }

    fn replace_scope(
        &mut self,
        scope: &StorageScope,
        items: Vec<SyntheticItem>,
    ) -> Result<(), StorageError> {
        let (principal, workspace) = Self::scope_key(scope)?;
        self.items
            .retain(|(p, w, _), _| p != &principal || w != &workspace);
        for item in items {
            if item.principal != principal || item.workspace != workspace {
                return Err(StorageError::invalid_key("export scope mismatch"));
            }
            self.put(item)?;
        }
        Ok(())
    }

    fn quarantine_scope(&mut self, scope: &StorageScope) -> Result<(), StorageError> {
        let (principal, workspace) = Self::scope_key(scope)?;
        let keys: Vec<_> = self
            .items
            .keys()
            .filter(|(p, w, _)| p == &principal && w == &workspace)
            .cloned()
            .collect();
        for key in keys {
            if let Some(item) = self.items.remove(&key) {
                self.quarantine.insert(key, item);
            }
        }
        Ok(())
    }
}

pub struct InMemoryRepository {
    store: Mutex<Store>,
}

impl InMemoryRepository {
    fn new() -> Self {
        Self {
            store: Mutex::new(Store::default()),
        }
    }

    pub fn upsert(&self, item: SyntheticItem) -> Result<(), StorageError> {
        lock(&self.store)?.put(item)
    }

    pub fn get(
        &self,
        scope: &StorageScope,
        id: &str,
    ) -> Result<Option<SyntheticItem>, StorageError> {
        let (principal, workspace) = Store::scope_key(scope)?;
        Ok(lock(&self.store)?
            .items
            .get(&(principal, workspace, id.to_string()))
            .cloned())
    }

    pub fn list(&self, scope: &StorageScope) -> Result<Vec<SyntheticItem>, StorageError> {
        lock(&self.store)?.list(scope)
    }

    pub fn freeze(&self, frozen: bool) -> Result<(), StorageError> {
        lock(&self.store)?.frozen = frozen;
        Ok(())
    }

    pub fn create_or_get(&self, item: SyntheticItem) -> Result<SyntheticItem, StorageError> {
        let mut store = lock(&self.store)?;
        let key = (
            item.principal.clone(),
            item.workspace.clone(),
            item.id.clone(),
        );
        if let Some(existing) = store.items.get(&key) {
            return Ok(existing.clone());
        }
        store.put(item.clone())?;
        Ok(item)
    }

    pub fn compare_and_update(
        &self,
        scope: &StorageScope,
        id: &str,
        expected: u64,
        payload: &str,
    ) -> Result<SyntheticItem, StorageError> {
        let (principal, workspace) = Store::scope_key(scope)?;
        let mut store = lock(&self.store)?;
        let key = (principal, workspace, id.to_string());
        match store.items.get_mut(&key) {
            Some(item) if item.revision == expected => {
                item.payload = payload.to_string();
                item.revision += 1;
                Ok(item.clone())
            },
            Some(item) => Err(StorageError::Conflict {
                expected: Some(expected.to_string()),
                actual: Some(item.revision.to_string()),
            }),
            None => Err(StorageError::NotFound),
        }
    }

    pub fn export_scope(&self, scope: &StorageScope) -> Result<Vec<u8>, StorageError> {
        let items = self.list(scope)?;
        serde_json::to_vec(&items).map_err(|err| StorageError::backend(err.to_string()))
    }

    pub fn import_scope(&self, scope: &StorageScope, bytes: &[u8]) -> Result<(), StorageError> {
        let items: Vec<SyntheticItem> =
            serde_json::from_slice(bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?;
        lock(&self.store)?.replace_scope(scope, items)
    }
}

pub struct SyntheticOwner {
    id: StorageCatalogId,
    local: Arc<InMemoryRepository>,
    remote: Arc<InMemoryRepository>,
    canonical: Mutex<Canonical>,
    fence: Mutex<u64>,
    guard: SourceGuard,
    pub export_calls: AtomicU64,
}

impl SyntheticOwner {
    pub fn new() -> Result<Self, StorageError> {
        let id = StorageCatalogId::parse(SYNTHETIC_OWNER_ID)?;
        Ok(Self {
            local: Arc::new(InMemoryRepository::new()),
            remote: Arc::new(InMemoryRepository::new()),
            canonical: Mutex::new(Canonical::Local),
            fence: Mutex::new(0),
            guard: SourceGuard::enabled(id.clone(), ["synthetic.rs"]),
            id,
            export_calls: AtomicU64::new(0),
        })
    }

    pub fn seed(&self, scope: &StorageScope, id: &str, payload: &str) -> Result<(), StorageError> {
        let (principal, workspace) = Store::scope_key(scope)?;
        self.local.upsert(SyntheticItem {
            principal,
            workspace,
            id: id.to_string(),
            revision: 1,
            payload: payload.to_string(),
        })
    }

    pub fn local(&self) -> Arc<InMemoryRepository> {
        Arc::clone(&self.local)
    }

    pub fn remote(&self) -> Arc<InMemoryRepository> {
        Arc::clone(&self.remote)
    }

    pub fn scenario_factory(
        &self,
    ) -> RepositoryScenarioFactory<Arc<InMemoryRepository>, Arc<InMemoryRepository>> {
        RepositoryScenarioFactory::new(
            self.id.as_str(),
            Arc::clone(&self.local),
            Arc::clone(&self.remote),
        )
    }

    pub fn source_guard(&self) -> &SourceGuard {
        &self.guard
    }

    fn digest(items: &[SyntheticItem]) -> String {
        let encoded = serde_json::to_vec(items).unwrap_or_default();
        blake3::hash(&encoded).to_hex().to_string()
    }
}

#[async_trait]
impl OwnerMigrationHandler for SyntheticOwner {
    fn owner_id(&self) -> StorageCatalogId {
        self.id.clone()
    }

    fn source_guard(&self) -> &SourceGuard {
        &self.guard
    }

    async fn inventory(&self, scope: &StorageScope) -> Result<OwnerInventory, StorageError> {
        let items = self.local.list(scope)?;
        Ok(OwnerInventory {
            owner_id: self.id.clone(),
            scope: scope.clone(),
            record_count: items.len() as u64,
            watermark: Self::digest(&items),
            layout: "memory:synthetic_items".into(),
        })
    }

    async fn watermark(&self, scope: &StorageScope) -> Result<SourceWatermark, StorageError> {
        let items = self.local.list(scope)?;
        Ok(SourceWatermark {
            generation: Self::digest(&items),
        })
    }

    async fn export(&self, scope: &StorageScope) -> Result<ExportBlob, StorageError> {
        self.export_calls.fetch_add(1, Ordering::SeqCst);
        let items = self.local.list(scope)?;
        let bytes =
            serde_json::to_vec(&items).map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(ExportBlob {
            digest: Self::digest(&items),
            records: items.len() as u64,
            bytes,
        })
    }

    async fn import(
        &self,
        scope: &StorageScope,
        blob: &ExportBlob,
    ) -> Result<ImportReceipt, StorageError> {
        let items: Vec<SyntheticItem> =
            serde_json::from_slice(&blob.bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?;
        lock(&self.remote.store)?.replace_scope(scope, items)?;
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
        let exported: Vec<SyntheticItem> =
            serde_json::from_slice(&blob.bytes).map_err(|err| StorageError::Corrupt {
                detail: err.to_string(),
            })?;
        let remote = self.remote.list(scope)?;
        let semantic = match exported.first() {
            Some(item) => self
                .remote
                .get(scope, &item.id)?
                .map(|got| got.payload == item.payload)
                .unwrap_or(false),
            None => remote.is_empty(),
        };
        Ok(VerifyReport {
            counts_match: exported.len() == remote.len(),
            identifiers_match: exported
                .iter()
                .map(|i| &i.id)
                .eq(remote.iter().map(|i| &i.id)),
            digest_match: Self::digest(&exported) == Self::digest(&remote),
            semantic_match: semantic,
        })
    }

    async fn rollback(&self, scope: &StorageScope) -> Result<RollbackReceipt, StorageError> {
        self.local.freeze(false)?;
        lock(&self.remote.store)?.quarantine_scope(scope)?;
        self.remote.freeze(true)?;
        *lock(&self.canonical)? = Canonical::Local;
        let mut fence = lock(&self.fence)?;
        *fence = fence.saturating_add(1);
        Ok(RollbackReceipt {
            fencing_generation: *fence,
        })
    }

    async fn prepare_cutover(&self, _scope: &StorageScope) -> Result<(), StorageError> {
        self.local.freeze(true)
    }

    async fn cutover(&self, _scope: &StorageScope) -> Result<(), StorageError> {
        *lock(&self.canonical)? = Canonical::Remote;
        Ok(())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> Result<std::sync::MutexGuard<'_, T>, StorageError> {
    mutex
        .lock()
        .map_err(|_| StorageError::backend("poisoned synthetic store"))
}

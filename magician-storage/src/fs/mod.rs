//! Legacy-compatible local filesystem adapters.
//!
//! Object versions and conditional writes use per-object sidecar metadata
//! (Task 3A). These adapters do not relocate existing bytes.

mod dataset;
mod index;
mod lease;
mod object;
mod paths;
mod scratch;
mod secret;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::StorageError;
use crate::health::StorageHealthRegistry;
use crate::lease::OwnerId;
use crate::profile::ResolvedStorageProfile;
use crate::runtime::StorageRuntime;
use crate::scope_lease::ScopeLeaseManager;

pub use dataset::LocalDatasetStore;
pub use index::LocalIndexStore;
pub use lease::LocalLeaseStore;
pub use object::LocalObjectStore;
pub use scratch::LocalScratchStore;
pub use secret::LocalSecretStore;

#[derive(Clone, Debug)]
pub struct LocalLayout {
    pub root: PathBuf,
    pub objects: PathBuf,
    pub datasets: PathBuf,
    pub indexes: PathBuf,
    pub leases: PathBuf,
    pub scratch: PathBuf,
    pub secrets: PathBuf,
}

impl LocalLayout {
    pub fn new(root: impl AsRef<Path>) -> Self {
        let root = root.as_ref().to_path_buf();
        Self {
            objects: root.join("objects"),
            datasets: root.join("datasets"),
            indexes: root.join("indexes"),
            leases: root.join("leases"),
            scratch: root.join("scratch"),
            secrets: root.join("secrets"),
            root,
        }
    }

    pub fn ensure(&self) -> Result<(), StorageError> {
        for dir in [
            &self.root,
            &self.objects,
            &self.datasets,
            &self.indexes,
            &self.leases,
            &self.scratch,
            &self.secrets,
        ] {
            std::fs::create_dir_all(dir).map_err(|err| StorageError::backend(err.to_string()))?;
        }
        Ok(())
    }
}

/// Factory for the Task 3 local adapter set. Callers pass the current runtime
/// root; nothing is moved or rewritten.
pub struct LocalStorage {
    pub layout: LocalLayout,
    pub objects: Arc<LocalObjectStore>,
    pub datasets: Arc<LocalDatasetStore>,
    pub indexes: Arc<LocalIndexStore>,
    pub leases: Arc<LocalLeaseStore>,
    pub scratch: Arc<LocalScratchStore>,
    pub secrets: Arc<LocalSecretStore>,
}

impl LocalStorage {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        Self::open_with_scratch_quota(root, 64 * 1024 * 1024)
    }

    pub fn open_with_scratch_quota(
        root: impl AsRef<Path>,
        max_bytes: u64,
    ) -> Result<Self, StorageError> {
        let layout = LocalLayout::new(root);
        layout.ensure()?;
        let objects = Arc::new(LocalObjectStore::new(layout.objects.clone()));
        let datasets = Arc::new(LocalDatasetStore::new(layout.datasets.clone()));
        let indexes = Arc::new(LocalIndexStore::new(layout.indexes.clone()));
        let leases = Arc::new(LocalLeaseStore::new(layout.leases.clone()));
        let secrets = Arc::new(LocalSecretStore::new(layout.secrets.clone()));
        let scratch = Arc::new(LocalScratchStore::new(
            layout.scratch.clone(),
            objects.clone(),
            max_bytes,
        ));
        scratch.scavenge()?;
        Ok(Self {
            layout,
            objects,
            datasets,
            indexes,
            leases,
            scratch,
            secrets,
        })
    }

    pub fn runtime(self, profile: ResolvedStorageProfile, owner: OwnerId) -> StorageRuntime {
        let root = self.layout.root.clone();
        let runtime = StorageRuntime {
            profile,
            root,
            objects: self.objects,
            datasets: self.datasets,
            indexes: self.indexes,
            leases: self.leases.clone(),
            scratch: self.scratch,
            secrets: self.secrets,
            scope_leases: ScopeLeaseManager::new(self.leases, owner),
            health: Arc::new(StorageHealthRegistry::new()),
        };
        runtime.record_startup_health();
        runtime
    }
}

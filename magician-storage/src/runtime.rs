use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde::Serialize;

use crate::dataset::DatasetStore;
use crate::error::StorageError;
use crate::fs::LocalStorage;
use crate::health::{HealthStatus, StorageHealth, StorageHealthRegistry};
use crate::index::IndexStore;
use crate::lease::{LeaseStore, OwnerId};
use crate::object::ObjectStore;
use crate::profile::{BootstrapSource, ProfileKind, ResolvedStorageProfile};
use crate::scope_lease::ScopeLeaseManager;
use crate::scratch::ScratchStore;
use crate::secret::SecretStore;

/// Directory under a workspace root that holds typed local adapters.
pub const LOCAL_ADAPTER_DIR: &str = ".magician-storage";

static INSTALLED: OnceLock<Arc<StorageRuntime>> = OnceLock::new();

/// Process-wide composition root. Constructed at startup and immutable until restart.
///
/// Production construction is `magician-bin` via `open_local`. Libraries must
/// not open a second backend from `$HOME` / `MAGICIAN_ROOT_DIR`. Task 21
/// (`scripts/check_typed_storage_boundaries.py`) ratchets new constructors.
pub struct StorageRuntime {
    pub profile: ResolvedStorageProfile,
    pub root: PathBuf,
    pub objects: Arc<dyn ObjectStore>,
    pub datasets: Arc<dyn DatasetStore>,
    pub indexes: Arc<dyn IndexStore>,
    pub leases: Arc<dyn LeaseStore>,
    pub scratch: Arc<dyn ScratchStore>,
    pub secrets: Arc<dyn SecretStore>,
    pub scope_leases: ScopeLeaseManager,
    pub health: Arc<StorageHealthRegistry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StorageOperatorHealth {
    pub profile: String,
    pub source: String,
    pub root: String,
    pub capabilities: Vec<StorageHealth>,
    pub scopes: Vec<ScopeLeaseHealth>,
    pub lease_lost: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ScopeLeaseHealth {
    pub principal: String,
    pub workspace: String,
    pub generation: u64,
}

impl StorageRuntime {
    /// Open the local adapter set for an embedded/silverbullet profile.
    /// Remote object/dataset adapters live in `magician-storage-s3` and are
    /// not selected by this constructor.
    pub fn open_local(
        root: impl AsRef<Path>,
        profile: ResolvedStorageProfile,
        owner: OwnerId,
    ) -> Result<Self, StorageError> {
        match profile.kind {
            ProfileKind::RemoteDurable => Err(StorageError::UnsupportedCapability),
            ProfileKind::LocalEmbedded | ProfileKind::LocalSilverbulletSpace => {
                let max_bytes = profile
                    .document
                    .scratch
                    .max_bytes
                    .unwrap_or(64 * 1024 * 1024);
                Ok(LocalStorage::open_with_scratch_quota(root, max_bytes)?.runtime(profile, owner))
            },
        }
    }

    pub fn adapter_root_for_workspace(workspace_root: impl AsRef<Path>) -> PathBuf {
        workspace_root.as_ref().join(LOCAL_ADAPTER_DIR)
    }

    /// Install the process-wide runtime. First writer wins. Magician-bin is
    /// the only production caller. Libraries use [`current`] / [`require`].
    pub fn install(runtime: Arc<Self>) {
        let _ = INSTALLED.set(runtime);
    }

    /// Typed capabilities for this process, if magician-bin has installed them.
    pub fn current() -> Option<Arc<Self>> {
        INSTALLED.get().cloned()
    }

    /// Fail closed when a library needs durable storage and no runtime exists.
    pub fn require() -> Result<Arc<Self>, StorageError> {
        Self::current().ok_or(StorageError::Unavailable { retry_after: None })
    }

    pub fn operator_health(&self) -> StorageOperatorHealth {
        let profile = match self.profile.kind {
            ProfileKind::LocalEmbedded => "local_embedded",
            ProfileKind::LocalSilverbulletSpace => "local_silverbullet_space",
            ProfileKind::RemoteDurable => "remote_durable",
        };
        let source = match &self.profile.source {
            BootstrapSource::MissingDefault => "missing_default",
            BootstrapSource::File(_) => "file",
        };
        StorageOperatorHealth {
            profile: profile.into(),
            source: source.into(),
            root: self.root.display().to_string(),
            capabilities: self.health.snapshot(),
            scopes: self
                .scope_leases
                .held_scopes()
                .into_iter()
                .map(|(scope, generation)| ScopeLeaseHealth {
                    principal: scope.principal.as_str().to_string(),
                    workspace: scope.workspace.as_str().to_string(),
                    generation,
                })
                .collect(),
            lease_lost: self.scope_leases.lease_lost(),
        }
    }

    pub(crate) fn record_startup_health(&self) {
        let detail = match self.profile.kind {
            ProfileKind::LocalEmbedded => "local filesystem",
            ProfileKind::LocalSilverbulletSpace => "local silverbullet space",
            ProfileKind::RemoteDurable => "remote durable",
        };
        for capability in [
            "object_store",
            "dataset_store",
            "index_store",
            "lease_store",
            "scratch_store",
            "secret_store",
        ] {
            self.health.record(StorageHealth {
                capability: capability.into(),
                status: HealthStatus::Ok,
                safe_detail: detail.into(),
            });
        }
        self.health.record(StorageHealth {
            capability: "profile".into(),
            status: HealthStatus::Ok,
            safe_detail: format!(
                "{} via {}",
                match self.profile.kind {
                    ProfileKind::LocalEmbedded => "local_embedded",
                    ProfileKind::LocalSilverbulletSpace => "local_silverbullet_space",
                    ProfileKind::RemoteDurable => "remote_durable",
                },
                match self.profile.source {
                    BootstrapSource::MissingDefault => "missing_default",
                    BootstrapSource::File(_) => "file",
                }
            ),
        });
    }
}

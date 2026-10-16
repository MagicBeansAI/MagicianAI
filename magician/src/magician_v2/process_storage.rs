//! Process-wide typed storage handle.
//!
//! Magician-bin installs [`magician_storage::StorageRuntime`] after
//! `open_local`. Satellite crates must use this module instead of resolving
//! `MAGICIAN_ROOT_DIR` themselves.

use std::path::PathBuf;
use std::sync::Arc;

use magician_storage::{StorageError, StorageRuntime};

use crate::magician_v2::artifact_v2::workspace::{default_storage_base_path, ArtifactV2Workspace};

/// Typed capabilities for this process, if the composition root installed them.
pub fn current() -> Option<Arc<StorageRuntime>> {
    StorageRuntime::current()
}

/// Fail closed when a library needs durable storage and no runtime exists.
pub fn require() -> Result<Arc<StorageRuntime>, StorageError> {
    StorageRuntime::require()
}

/// Resolved workspace root: installed composition root, else env/fallback.
pub fn runtime_root() -> PathBuf {
    default_storage_base_path()
}

/// Local compatibility workspace on the process root.
pub fn workspace() -> ArtifactV2Workspace {
    ArtifactV2Workspace::new(runtime_root())
}

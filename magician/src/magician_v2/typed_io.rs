//! Live workspace I/O dispatcher onto cataloged owner kits.
//!
//! `ArtifactV2Workspace::write_path` / `read_path` classify the locator and
//! persist through the typed local adapter (`BlobAccess`, `WorkAccess`, …).
//! Unclassified paths stay on the file provider (scratch, ephemeral, notes).
//! Layouts do not change. Remote adapters stay unselected.

use std::path::Path;

use crate::magician_v2::agent_owners::{self, AgentAccess};
use crate::magician_v2::artifact_v2::service::ArtifactV2Error;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat_owners::{self, ChatAccess};
use crate::magician_v2::database_owners::{self, DatabaseAccess};
use crate::magician_v2::dataset_owners::{self, DatasetAccess};
use crate::magician_v2::object_owners::{self, BlobAccess};
use crate::magician_v2::subprocess_owners::{self, SubprocessAccess};
use crate::magician_v2::system_owners::{self, SystemAccess};
use crate::magician_v2::work_owners::{self, WorkAccess};

fn runtime_err(err: anyhow::Error) -> ArtifactV2Error {
    for cause in err.chain() {
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            if io.kind() == std::io::ErrorKind::NotFound {
                return ArtifactV2Error::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    err.to_string(),
                ));
            }
        }
    }
    ArtifactV2Error::Runtime(err.to_string())
}

pub fn reject_lease_lost_anyhow() -> anyhow::Result<()> {
    reject_if_lease_lost().map_err(|err| anyhow::anyhow!("{err}"))
}

pub fn reject_if_lease_lost() -> Result<(), ArtifactV2Error> {
    if magician_storage::StorageRuntime::current()
        .is_some_and(|runtime| runtime.scope_leases.lease_lost())
    {
        return Err(ArtifactV2Error::Runtime(
            "storage scope lease lost; canonical writers fail closed".into(),
        ));
    }
    Ok(())
}

/// Canonical event logs keep the specialized JSONL + independent commit-marker
/// protocol on `WorkspaceFileProvider`. Whole-file persist would skip the
/// fsync/rename uncertainty path those writers require.
pub(crate) fn specialized_jsonl_commit_authority(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(|name| name.to_str()),
        Some("events.jsonl" | "events.jsonl.commit")
    )
}

pub fn is_classified(workspace: &ArtifactV2Workspace, path: &Path) -> bool {
    object_owners::store_for_any_owner(workspace, path).is_some()
        || dataset_owners::store_for_any_owner(workspace, path).is_some()
        || work_owners::store_for_any_owner(workspace, path).is_some()
        || chat_owners::store_for_any_owner(workspace, path).is_some()
        || agent_owners::store_for_any_owner(workspace, path).is_some()
        || system_owners::store_for_any_owner(workspace, path).is_some()
        || database_owners::store_for_any_owner(workspace, path).is_some()
        || subprocess_owners::store_for_any_owner(workspace, path).is_some()
}

pub async fn persist(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<bool, ArtifactV2Error> {
    reject_if_lease_lost()?;
    if specialized_jsonl_commit_authority(path) {
        return Ok(false);
    }
    if object_owners::store_for_any_owner(workspace, path).is_some() {
        object_owners::persist_object_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if dataset_owners::store_for_any_owner(workspace, path).is_some() {
        dataset_owners::persist_dataset_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if work_owners::store_for_any_owner(workspace, path).is_some() {
        work_owners::persist_work_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if chat_owners::store_for_any_owner(workspace, path).is_some() {
        chat_owners::persist_chat_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if agent_owners::store_for_any_owner(workspace, path).is_some() {
        agent_owners::persist_agent_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if system_owners::store_for_any_owner(workspace, path).is_some() {
        system_owners::persist_system_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if database_owners::store_for_any_owner(workspace, path).is_some() {
        database_owners::persist_database_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    if subprocess_owners::store_for_any_owner(workspace, path).is_some() {
        subprocess_owners::persist_subprocess_file(workspace, path, bytes)
            .await
            .map_err(runtime_err)?;
        return Ok(true);
    }
    Ok(false)
}

pub fn persist_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<bool, ArtifactV2Error> {
    reject_if_lease_lost()?;
    if specialized_jsonl_commit_authority(path) {
        return Ok(false);
    }
    if object_owners::store_for_any_owner(workspace, path).is_some() {
        object_owners::persist_object_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if dataset_owners::store_for_any_owner(workspace, path).is_some() {
        dataset_owners::persist_dataset_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if work_owners::store_for_any_owner(workspace, path).is_some() {
        work_owners::persist_work_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if chat_owners::store_for_any_owner(workspace, path).is_some() {
        chat_owners::persist_chat_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if agent_owners::store_for_any_owner(workspace, path).is_some() {
        agent_owners::persist_agent_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if system_owners::store_for_any_owner(workspace, path).is_some() {
        system_owners::persist_system_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if database_owners::store_for_any_owner(workspace, path).is_some() {
        database_owners::persist_database_file_sync(workspace, path, bytes).map_err(runtime_err)?;
        return Ok(true);
    }
    if subprocess_owners::store_for_any_owner(workspace, path).is_some() {
        subprocess_owners::persist_subprocess_file_sync(workspace, path, bytes)
            .map_err(runtime_err)?;
        return Ok(true);
    }
    Ok(false)
}

pub async fn read(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Result<Option<Vec<u8>>, ArtifactV2Error> {
    if let Some((store, rel)) = object_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = dataset_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = work_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = chat_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = agent_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = system_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = database_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    if let Some((store, rel)) = subprocess_owners::store_for_any_owner(workspace, path) {
        return store.get(&rel).await.map(Some).map_err(runtime_err);
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::magician_v2::object_owners::ObjectOwner;

    #[tokio::test]
    async fn classified_object_write_goes_through_blob_access() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(dir.path());
        let path = workspace
            .scope_root("alice", "home")
            .join(ObjectOwner::TaskOutputs.sample_rel());
        assert!(is_classified(&workspace, &path));
        workspace.write_path(&path, b"typed-bytes").await.unwrap();
        assert_eq!(workspace.read_path(&path).await.unwrap(), b"typed-bytes");
    }

    #[tokio::test]
    async fn unclassified_scratch_stays_on_file_provider() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(dir.path());
        let path = workspace
            .scope_root("alice", "home")
            .join("scratch/tmp.txt");
        assert!(!is_classified(&workspace, &path));
        workspace.write_path(&path, b"scratch").await.unwrap();
        assert_eq!(workspace.read_path(&path).await.unwrap(), b"scratch");
    }

    #[tokio::test]
    async fn execution_event_commit_marker_stays_on_file_provider() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = ArtifactV2Workspace::new(dir.path());
        let path =
            workspace.execution_events_commit_path("anonymous", "default", "task-1", "exec-1");
        assert!(is_classified(&workspace, &path));
        assert!(!persist(&workspace, &path, b"12").await.unwrap());
        workspace.write_atomic_path(&path, b"12").await.unwrap();
        assert_eq!(workspace.read_prefix_path(&path, 16).await.unwrap(), b"12");
    }
}

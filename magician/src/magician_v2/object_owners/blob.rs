//! Local directory adapter. Same locators and paths as the current layout.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::owners::ObjectOwner;
use crate::magician_v2::artifact_v2::io::{write_bytes_durably, write_bytes_durably_sync};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedBlob {
    pub path: String,
    pub b64: String,
    #[serde(default)]
    pub digest: String,
}

pub fn blob_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[async_trait]
pub trait BlobAccess: Send + Sync {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, rel: &str) -> Result<Vec<u8>>;
    async fn exists(&self, rel: &str) -> Result<bool>;
    async fn delete(&self, rel: &str) -> Result<()>;
    async fn list(&self) -> Result<Vec<String>>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub struct LocalTreeStore {
    root: PathBuf,
    owner: ObjectOwner,
    workspace: Option<ArtifactV2Workspace>,
}

pub fn open_local_object_owner(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: ObjectOwner,
) -> LocalTreeStore {
    LocalTreeStore {
        root: workspace.scope_root(principal, workspace_name),
        owner,
        workspace: Some(workspace.clone()),
    }
}

/// Resolve a scoped file to the Task 10 factory when it belongs to `owner`.
pub fn store_for_existing_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    owner: ObjectOwner,
) -> Option<(LocalTreeStore, String)> {
    let rel_to_scopes = path.strip_prefix(workspace.scopes_root()).ok()?;
    let mut components = rel_to_scopes.components();
    let principal = components.next()?.as_os_str().to_str()?.to_string();
    let workspace_name = components.next()?.as_os_str().to_str()?.to_string();
    let rest = rel_to_scopes
        .strip_prefix(Path::new(&principal).join(&workspace_name))
        .ok()?;
    let rel = rest.to_string_lossy().replace('\\', "/");
    if !owner.allows(&rel) {
        return None;
    }
    Some((
        open_local_object_owner(workspace, &principal, &workspace_name, owner),
        rel,
    ))
}

pub fn store_for_any_owner(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<(LocalTreeStore, String)> {
    ObjectOwner::ALL
        .into_iter()
        .find_map(|owner| store_for_existing_path(workspace, path, owner))
}

/// Production byte writer for classified Task 10 object paths.
pub async fn persist_object_file(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 10 object owners"))?;
    store.put(&rel, bytes).await?;
    Ok(path.to_path_buf())
}

pub fn persist_object_file_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 10 object owners"))?;
    store.put_sync(&rel, bytes)?;
    Ok(path.to_path_buf())
}

pub async fn read_object_file(workspace: &ArtifactV2Workspace, path: &Path) -> Result<Vec<u8>> {
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 10 object owners"))?;
    store.get(&rel).await
}

/// Production writer for execution recordings. Live screencast is not stored.
pub async fn persist_execution_recording(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    task_id: Option<&str>,
    execution_id: &str,
    file_name: &str,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let safe_name = Path::new(file_name)
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != "." && *name != "..")
        .ok_or_else(|| anyhow::anyhow!("invalid recording file name"))?;
    let path = workspace
        .runtime_execution_recordings_dir(principal, workspace_name, task_id, execution_id)
        .join(safe_name);
    let (store, rel) = store_for_existing_path(workspace, &path, ObjectOwner::ExecutionRecordings)
        .ok_or_else(|| anyhow::anyhow!("recording path is outside execution_recordings"))?;
    store.put(&rel, bytes).await?;
    Ok(path)
}

impl LocalTreeStore {
    pub fn for_scope_root(root: impl Into<PathBuf>, owner: ObjectOwner) -> Self {
        Self {
            root: root.into(),
            owner,
            workspace: None,
        }
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        if Path::new(rel).is_absolute() || !self.owner.allows(rel) {
            bail!("{} rejects locator {rel}", self.owner.id());
        }
        let dest = self.root.join(rel);
        if dest.is_absolute() && !dest.starts_with(&self.root) {
            bail!("{} escaped owner root", self.owner.id());
        }
        Ok(dest)
    }

    fn put_sync(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
        let path = self.resolve(rel)?;
        write_bytes_durably_sync(&path, bytes)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    fn walk_files(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        for dir in self.owner.walk_roots(&self.root) {
            collect_files(&self.root, &dir, &mut out)?;
        }
        out.sort();
        out.dedup();
        Ok(out
            .into_iter()
            .filter(|rel| self.owner.allows(rel))
            .collect())
    }
}

fn collect_files(scope_root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if let Ok(meta) = std::fs::symlink_metadata(&path) {
            if meta.file_type().is_symlink() {
                continue;
            }
        }
        if path.is_dir() {
            collect_files(scope_root, &path, out)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(scope_root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

#[async_trait]
impl BlobAccess for LocalTreeStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
        let path = self.resolve(rel)?;
        write_bytes_durably(&path, bytes)
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    async fn get(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.resolve(rel)?;
        if let Some(workspace) = &self.workspace {
            return workspace
                .read_path_raw(&path)
                .await
                .with_context(|| format!("read {}", path.display()));
        }
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    async fn exists(&self, rel: &str) -> Result<bool> {
        Ok(self.resolve(rel)?.is_file())
    }

    async fn delete(&self, rel: &str) -> Result<()> {
        let path = self.resolve(rel)?;
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    async fn list(&self) -> Result<Vec<String>> {
        self.walk_files()
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let mut records = Vec::new();
        for rel in self.walk_files()? {
            let bytes = self.get(&rel).await?;
            records.push(ExportedBlob {
                path: rel,
                digest: blob_digest(&bytes),
                b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedBlob> = serde_json::from_slice(bytes)?;
        for record in records {
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                record.b64.as_bytes(),
            )?;
            let actual = blob_digest(&raw);
            if record.digest.is_empty() || record.digest != actual {
                anyhow::bail!("import digest mismatch for {}", record.path);
            }
            self.put(&record.path, &raw).await?;
        }
        Ok(())
    }
}

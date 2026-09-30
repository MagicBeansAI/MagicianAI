//! Local work-spine adapter. Current task/execution/plan/pause/list-index paths stay canonical.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::owners::{walk_files, WorkOwner};
use crate::magician_v2::artifact_v2::io::{write_bytes_durably, write_bytes_durably_sync};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedWork {
    pub path: String,
    pub b64: String,
    #[serde(default)]
    pub digest: String,
}

#[async_trait]
pub trait WorkAccess: Send + Sync {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, rel: &str) -> Result<Vec<u8>>;
    async fn exists(&self, rel: &str) -> Result<bool>;
    async fn list_selected(&self) -> Result<Vec<String>>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub struct LocalWorkStore {
    root: PathBuf,
    owner: WorkOwner,
}

pub fn open_local_work_owner(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: WorkOwner,
) -> LocalWorkStore {
    LocalWorkStore {
        root: owner.root(workspace, principal, workspace_name),
        owner,
    }
}

impl LocalWorkStore {
    pub fn for_scope_root(root: impl Into<PathBuf>, owner: WorkOwner) -> Self {
        Self {
            root: root.into(),
            owner,
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
        let path = self.resolve(rel)?;
        write_bytes_durably_sync(&path, bytes)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

/// Resolve a scoped file to the Task 12 factory when it belongs to `owner`.
pub fn store_for_existing_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    owner: WorkOwner,
) -> Option<(LocalWorkStore, String)> {
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
        open_local_work_owner(workspace, &principal, &workspace_name, owner),
        rel,
    ))
}

pub fn store_for_any_owner(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<(LocalWorkStore, String)> {
    WorkOwner::ALL
        .into_iter()
        .find_map(|owner| store_for_existing_path(workspace, path, owner))
}

/// Production byte writer for classified Task 12 paths. Unclassified paths
/// stay on their specialized local adapters (task journal, pause envelope,
/// list-index SQLite).
pub async fn persist_work_file(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 12 work-spine owners"))?;
    store.put(&rel, bytes).await?;
    Ok(path.to_path_buf())
}

pub fn persist_work_file_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 12 work-spine owners"))?;
    store.put_sync(&rel, bytes)?;
    Ok(path.to_path_buf())
}

pub fn work_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[async_trait]
impl WorkAccess for LocalWorkStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(rel)?;
        write_bytes_durably(&path, bytes)
            .await
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }

    async fn get(&self, rel: &str) -> Result<Vec<u8>> {
        let path = self.resolve(rel)?;
        std::fs::read(&path).with_context(|| format!("read {}", path.display()))
    }

    async fn exists(&self, rel: &str) -> Result<bool> {
        Ok(self.resolve(rel)?.is_file())
    }

    async fn list_selected(&self) -> Result<Vec<String>> {
        Ok(walk_files(&self.root, self.owner))
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let mut records = Vec::new();
        for rel in walk_files(&self.root, self.owner) {
            let bytes = self.get(&rel).await?;
            records.push(ExportedWork {
                path: rel,
                digest: work_digest(&bytes),
                b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedWork> = serde_json::from_slice(bytes)?;
        for record in records {
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                record.b64.as_bytes(),
            )?;
            if record.digest.is_empty() || record.digest != work_digest(&raw) {
                anyhow::bail!("import digest mismatch for {}", record.path);
            }
            self.put(&record.path, &raw).await?;
        }
        Ok(())
    }
}

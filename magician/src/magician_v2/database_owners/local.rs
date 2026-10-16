//! Local SQLite/DuckDB adapter. Current database files stay canonical.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::owners::{walk_files, DatabaseOwner};
use crate::magician_v2::artifact_v2::io::write_bytes_durably;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedDatabase {
    pub path: String,
    pub b64: String,
    #[serde(default)]
    pub digest: String,
}

#[async_trait]
pub trait DatabaseAccess: Send + Sync {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, rel: &str) -> Result<Vec<u8>>;
    async fn exists(&self, rel: &str) -> Result<bool>;
    async fn list_selected(&self) -> Result<Vec<String>>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub struct LocalDatabaseStore {
    root: PathBuf,
    owner: DatabaseOwner,
}

pub fn open_local_database_owner(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: DatabaseOwner,
) -> LocalDatabaseStore {
    LocalDatabaseStore {
        root: owner.root(workspace, principal, workspace_name),
        owner,
    }
}

/// Canonical on-disk path for a Task 15 database file. Live
/// `Connection::open` stays on the specialized local store.
pub fn database_file_path(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: DatabaseOwner,
) -> PathBuf {
    owner
        .root(workspace, principal, workspace_name)
        .join(owner.sample_rel())
}

pub fn host_database_path(base_root: &Path, owner: DatabaseOwner) -> PathBuf {
    base_root.join(owner.sample_rel())
}

impl LocalDatabaseStore {
    pub fn for_scope_root(root: impl Into<PathBuf>, owner: DatabaseOwner) -> Self {
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
}

pub fn store_for_existing_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    owner: DatabaseOwner,
) -> Option<(LocalDatabaseStore, String)> {
    if owner.is_host() {
        let rel = path
            .strip_prefix(workspace.base_root())
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        if !owner.allows(&rel) {
            return None;
        }
        return Some((
            open_local_database_owner(workspace, "system", "system", owner),
            rel,
        ));
    }
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
        open_local_database_owner(workspace, &principal, &workspace_name, owner),
        rel,
    ))
}

pub fn store_for_any_owner(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<(LocalDatabaseStore, String)> {
    DatabaseOwner::ALL
        .into_iter()
        .find_map(|owner| store_for_existing_path(workspace, path, owner))
}

/// Closed-file snapshot writer for migration tests. Live engines keep
/// `Connection::open` / DuckDB.
pub async fn persist_database_file(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 15 database owners"))?;
    refuse_live_engine_sidecar(&rel)?;
    store.put(&rel, bytes).await?;
    Ok(path.to_path_buf())
}

pub fn persist_database_file_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 15 database owners"))?;
    refuse_live_engine_sidecar(&rel)?;
    let dest = store.resolve(&rel)?;
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&dest, bytes)
        .with_context(|| format!("write {}", dest.display()))?;
    Ok(path.to_path_buf())
}

pub fn database_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

fn refuse_live_engine_sidecar(rel: &str) -> Result<()> {
    if super::owners::is_live_engine_sidecar(rel) {
        anyhow::bail!("refuses opaque WAL/SHM write; checkpoint the engine first");
    }
    Ok(())
}

#[async_trait]
impl DatabaseAccess for LocalDatabaseStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
        refuse_live_engine_sidecar(rel)?;
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
        let files = walk_files(&self.root, self.owner);
        if files
            .iter()
            .any(|rel| super::owners::is_live_engine_sidecar(rel))
        {
            anyhow::bail!(
                "{} refuses live WAL/SHM export; checkpoint the engine first",
                self.owner.id()
            );
        }
        let mut records = Vec::new();
        for rel in files {
            let bytes = self.get(&rel).await?;
            records.push(ExportedDatabase {
                path: rel,
                digest: database_digest(&bytes),
                b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedDatabase> = serde_json::from_slice(bytes)?;
        for record in records {
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                record.b64.as_bytes(),
            )?;
            if record.digest.is_empty() || record.digest != database_digest(&raw) {
                anyhow::bail!("import digest mismatch for {}", record.path);
            }
            self.put(&record.path, &raw).await?;
        }
        Ok(())
    }
}

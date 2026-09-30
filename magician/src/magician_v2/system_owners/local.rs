//! Local system/device/secret adapter. Current layouts stay canonical.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::owners::{walk_files, SystemOwner};
use crate::magician_v2::artifact_v2::io::{write_bytes_durably, write_bytes_durably_sync};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedSystem {
    pub path: String,
    pub b64: String,
    #[serde(default)]
    pub digest: String,
}

#[async_trait]
pub trait SystemAccess: Send + Sync {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, rel: &str) -> Result<Vec<u8>>;
    async fn exists(&self, rel: &str) -> Result<bool>;
    async fn list_selected(&self) -> Result<Vec<String>>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub struct LocalSystemStore {
    root: PathBuf,
    owner: SystemOwner,
}

pub fn open_local_system_owner(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: SystemOwner,
) -> LocalSystemStore {
    LocalSystemStore {
        root: owner.root(workspace, principal, workspace_name),
        owner,
    }
}

pub fn system_file_path(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    owner: SystemOwner,
) -> PathBuf {
    owner
        .root(workspace, principal, workspace_name)
        .join(owner.sample_rel())
}

pub fn host_system_path(base_root: &Path, owner: SystemOwner) -> PathBuf {
    base_root.join(owner.sample_rel())
}

impl LocalSystemStore {
    pub fn for_scope_root(root: impl Into<PathBuf>, owner: SystemOwner) -> Self {
        Self {
            root: root.into(),
            owner,
        }
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        if !self.owner.allows(rel) {
            bail!("{} rejects locator {rel}", self.owner.id());
        }
        Ok(self.root.join(rel))
    }

    fn put_sync(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        let path = self.resolve(rel)?;
        write_bytes_durably_sync(&path, bytes)
            .with_context(|| format!("write {}", path.display()))?;
        Ok(())
    }
}

pub fn store_for_existing_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    owner: SystemOwner,
) -> Option<(LocalSystemStore, String)> {
    if owner.is_host() || owner.is_device() {
        let rel = path
            .strip_prefix(workspace.base_root())
            .ok()?
            .to_string_lossy()
            .replace('\\', "/");
        if !owner.allows(&rel) {
            return None;
        }
        return Some((
            open_local_system_owner(workspace, "system", "system", owner),
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
        open_local_system_owner(workspace, &principal, &workspace_name, owner),
        rel,
    ))
}

pub fn store_for_any_owner(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<(LocalSystemStore, String)> {
    SystemOwner::ALL
        .into_iter()
        .find_map(|owner| store_for_existing_path(workspace, path, owner))
}

pub async fn persist_system_file(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 16 system owners"))?;
    store.put(&rel, bytes).await?;
    Ok(path.to_path_buf())
}

pub fn persist_system_file_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 16 system owners"))?;
    store.put_sync(&rel, bytes)?;
    Ok(path.to_path_buf())
}

pub fn system_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[async_trait]
impl SystemAccess for LocalSystemStore {
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
            if self.owner == super::owners::SystemOwner::SecretVault
                && !rel.ends_with("secret_audit.jsonl")
            {
                continue;
            }
            let bytes = self.get(&rel).await?;
            records.push(ExportedSystem {
                path: rel,
                digest: system_digest(&bytes),
                b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedSystem> = serde_json::from_slice(bytes)?;
        for record in records {
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                record.b64.as_bytes(),
            )?;
            if record.digest.is_empty() || record.digest != system_digest(&raw) {
                anyhow::bail!("import digest mismatch for {}", record.path);
            }
            if self.owner == super::owners::SystemOwner::SecretVault
                && !record.path.ends_with("secret_audit.jsonl")
            {
                anyhow::bail!("secret vault bytes are not importable");
            }
            self.put(&record.path, &raw).await?;
        }
        Ok(())
    }
}

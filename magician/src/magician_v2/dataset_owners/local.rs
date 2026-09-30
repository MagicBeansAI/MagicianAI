//! Local Parquet adapter. Current `dt=*` paths stay canonical.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::family::DatasetFamily;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportedPart {
    pub path: String,
    pub b64: String,
    #[serde(default)]
    pub digest: String,
}

#[async_trait]
pub trait DatasetAccess: Send + Sync {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()>;
    async fn get(&self, rel: &str) -> Result<Vec<u8>>;
    async fn exists(&self, rel: &str) -> Result<bool>;
    async fn list_selected(&self) -> Result<Vec<String>>;
    async fn export_all(&self) -> Result<Vec<u8>>;
    async fn import_all(&self, bytes: &[u8]) -> Result<()>;
}

#[derive(Clone)]
pub struct LocalFamilyStore {
    root: PathBuf,
    family: DatasetFamily,
}

pub fn open_local_dataset_family(
    workspace: &ArtifactV2Workspace,
    principal: &str,
    workspace_name: &str,
    family: DatasetFamily,
) -> LocalFamilyStore {
    LocalFamilyStore {
        root: family.root(workspace, principal, workspace_name),
        family,
    }
}

pub fn store_for_existing_path(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    family: DatasetFamily,
) -> Option<(LocalFamilyStore, String)> {
    let rel_to_scopes = path.strip_prefix(workspace.scopes_root()).ok()?;
    let mut components = rel_to_scopes.components();
    let principal = components.next()?.as_os_str().to_str()?.to_string();
    let workspace_name = components.next()?.as_os_str().to_str()?.to_string();
    let store = open_local_dataset_family(workspace, &principal, &workspace_name, family);
    let rel = path.strip_prefix(&store.root).ok()?;
    let rel = rel.to_string_lossy().replace('\\', "/");
    if !family.allows(&rel) {
        return None;
    }
    Some((store, rel))
}

pub fn store_for_any_owner(
    workspace: &ArtifactV2Workspace,
    path: &Path,
) -> Option<(LocalFamilyStore, String)> {
    DatasetFamily::ALL
        .into_iter()
        .find_map(|family| store_for_existing_path(workspace, path, family))
}

pub async fn persist_dataset_file(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 11 dataset families"))?;
    store.put(&rel, bytes).await?;
    Ok(path.to_path_buf())
}

pub fn persist_dataset_file_sync(
    workspace: &ArtifactV2Workspace,
    path: &Path,
    bytes: &[u8],
) -> Result<PathBuf> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let (store, rel) = store_for_any_owner(workspace, path)
        .ok_or_else(|| anyhow::anyhow!("path is outside Task 11 dataset families"))?;
    let dest = store.resolve(&rel)?;
    if dest.is_absolute() && !dest.starts_with(&store.root) {
        bail!("dataset locator escaped family root");
    }
    crate::magician_v2::artifact_v2::io::write_bytes_durably_sync(&dest, bytes)
        .with_context(|| format!("write {}", dest.display()))?;
    Ok(path.to_path_buf())
}

/// After DuckDB `COPY TO` writes a parquet file, republish through DatasetAccess.
/// When the locator already is the family path, COPY/rename is the publish.
/// Analytics temp/unclassified parquet (no `scopes/` ancestor, or not a Task 11
/// family) keeps the COPY/rename as the publish — fail-open here would mark
/// healthy partitions incomplete.
pub fn publish_written_parquet(path: &Path) -> Result<()> {
    crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
    let Some(workspace) = workspace_from_scoped_path(path) else {
        return Ok(());
    };
    let Some((store, rel)) = store_for_any_owner(&workspace, path) else {
        return Ok(());
    };
    let dest = store.resolve(&rel)?;
    if dest == path {
        return Ok(());
    }
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    persist_dataset_file_sync(&workspace, path, &bytes)?;
    Ok(())
}

#[cfg(test)]
pub(crate) fn workspace_from_scoped_path_for_test(path: &Path) -> Option<ArtifactV2Workspace> {
    workspace_from_scoped_path(path)
}

fn workspace_from_scoped_path(path: &Path) -> Option<ArtifactV2Workspace> {
    let mut found = None;
    for ancestor in path.ancestors() {
        let Some(name) = ancestor.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if name == "scopes" {
            found = ancestor.parent().map(ArtifactV2Workspace::new);
        }
    }
    found
}

impl LocalFamilyStore {
    pub fn for_root(root: impl Into<PathBuf>, family: DatasetFamily) -> Self {
        Self {
            root: root.into(),
            family,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, rel: &str) -> Result<PathBuf> {
        if Path::new(rel).is_absolute() || !self.family.allows(rel) {
            bail!("{} rejects locator {rel}", self.family.id());
        }
        let dest = self.root.join(rel);
        if dest.is_absolute() && !dest.starts_with(&self.root) {
            bail!("{} escaped family root", self.family.id());
        }
        Ok(dest)
    }

    fn walk_parquet(&self) -> Result<Vec<String>> {
        let mut out = Vec::new();
        collect_parquet(&self.root, &self.root, &mut out)?;
        out.sort();
        out.dedup();
        Ok(out
            .into_iter()
            .filter(|rel| self.family.allows(rel))
            .collect())
    }
}

fn collect_parquet(family_root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
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
            collect_parquet(family_root, &path, out)?;
        } else if path.is_file() {
            if let Ok(rel) = path.strip_prefix(family_root) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    Ok(())
}

pub fn part_digest(bytes: &[u8]) -> String {
    blake3::hash(bytes).to_hex().to_string()
}

#[async_trait]
impl DatasetAccess for LocalFamilyStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
        let path = self.resolve(rel)?;
        crate::magician_v2::artifact_v2::io::write_bytes_durably(&path, bytes)
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
        self.walk_parquet()
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let mut records = Vec::new();
        for rel in self.walk_parquet()? {
            let bytes = self.get(&rel).await?;
            records.push(ExportedPart {
                path: rel,
                digest: part_digest(&bytes),
                b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedPart> = serde_json::from_slice(bytes)?;
        for record in records {
            let raw = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                record.b64.as_bytes(),
            )?;
            if record.digest.is_empty() || record.digest != part_digest(&raw) {
                anyhow::bail!("import digest mismatch for {}", record.path);
            }
            self.put(&record.path, &raw).await?;
        }
        Ok(())
    }
}

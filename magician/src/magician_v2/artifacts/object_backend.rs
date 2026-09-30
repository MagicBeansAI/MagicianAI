//! ObjectStore-backed durable artifacts. Explicit test/remote profile only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use futures_util::StreamExt;
use magician_storage::{
    bytes_body, DeleteCondition, ObjectStore, PutCondition, PutObjectRequest, StorageError,
    StorageKey, StoragePrefix,
};

use super::durable_store::{
    split_frontmatter, DurableArtifactAccess, DurableArtifactEntry, DurableFrontmatter,
    ExportedDurable,
};

const NAMESPACE: &str = "durable";

#[derive(Clone)]
pub struct ObjectDurableStore {
    objects: Arc<dyn ObjectStore>,
    principal: String,
    workspace: String,
    scratch: PathBuf,
}

impl ObjectDurableStore {
    pub fn new(
        objects: Arc<dyn ObjectStore>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        scratch: impl Into<PathBuf>,
    ) -> Self {
        Self {
            objects,
            principal: principal.into(),
            workspace: workspace.into(),
            scratch: scratch.into(),
        }
    }

    fn key(&self, namespace: &str, name: &str) -> Result<StorageKey> {
        durable_object_key(&self.principal, &self.workspace, namespace, name)
            .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn prefix(&self) -> StoragePrefix {
        StoragePrefix {
            encoded: format!("t/{}/{}/{}/", self.principal, self.workspace, NAMESPACE),
        }
    }

    async fn get_bytes(&self, key: &StorageKey) -> Result<Vec<u8>> {
        let read = self.objects.get(key, None).await.map_err(obj_err)?;
        collect_body(read.body).await
    }

    fn materialize_path(&self, namespace: &str, name: &str) -> Result<PathBuf> {
        if namespace.contains("..")
            || name.contains("..")
            || Path::new(namespace).is_absolute()
            || Path::new(name).is_absolute()
        {
            anyhow::bail!("durable materialize rejects traversal");
        }
        let path = self.scratch.join(namespace).join(name);
        if path.is_absolute() && !path.starts_with(&self.scratch) {
            anyhow::bail!("durable materialize escaped scratch");
        }
        Ok(path)
    }
}

fn durable_object_key(
    principal: &str,
    workspace: &str,
    namespace: &str,
    name: &str,
) -> Result<StorageKey, StorageError> {
    let hex = blake3::hash(format!("{namespace}\0{name}").as_bytes())
        .to_hex()
        .to_string();
    StorageKey::tenant(principal, workspace, NAMESPACE, &format!("d{hex}"))
}

async fn collect_body(mut body: magician_storage::ObjectBodyStream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.map_err(obj_err)?);
    }
    Ok(out)
}

fn obj_err(err: StorageError) -> anyhow::Error {
    anyhow::anyhow!("{err}")
}

fn encode_file(frontmatter: &DurableFrontmatter, content: &str) -> Result<Vec<u8>> {
    let yaml = serde_yaml::to_string(frontmatter)?;
    Ok(format!("---\n{yaml}---\n{content}").into_bytes())
}

#[async_trait]
impl DurableArtifactAccess for ObjectDurableStore {
    async fn write(
        &self,
        namespace: &str,
        name: &str,
        content: &str,
        frontmatter: DurableFrontmatter,
    ) -> Result<()> {
        let key = self.key(namespace, name)?;
        let bytes = encode_file(&frontmatter, content)?;
        self.objects
            .put(PutObjectRequest {
                key,
                body: bytes_body(bytes),
                content_type: frontmatter.content_type.clone(),
                condition: PutCondition::Overwrite,
            })
            .await
            .map_err(obj_err)?;
        Ok(())
    }

    async fn append(&self, namespace: &str, name: &str, content: &str) -> Result<()> {
        let (mut fm, mut body) = self.read(namespace, name).await?;
        fm.last_updated = chrono::Utc::now();
        body.push_str(content);
        self.write(namespace, name, &body, fm).await
    }

    async fn read(&self, namespace: &str, name: &str) -> Result<(DurableFrontmatter, String)> {
        let key = self.key(namespace, name)?;
        let bytes = self.get_bytes(&key).await?;
        let text = String::from_utf8(bytes).context("durable artifact is not utf-8")?;
        let (fm_str, body) =
            split_frontmatter(&text).context("No frontmatter found in artifact")?;
        let fm: DurableFrontmatter =
            serde_yaml::from_str(&fm_str).context("Invalid frontmatter YAML")?;
        let path = self.materialize_path(namespace, name)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, &text).await?;
        Ok((fm, body))
    }

    async fn list_entries(&self, namespace: Option<&str>) -> Result<Vec<DurableArtifactEntry>> {
        let listed = self
            .objects
            .list_diagnostic(&self.prefix(), None, 1000)
            .await
            .map_err(obj_err)?;
        let mut entries = Vec::new();
        for meta in listed {
            let bytes = match self.get_bytes(&meta.key).await {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let text = match String::from_utf8(bytes) {
                Ok(text) => text,
                Err(_) => continue,
            };
            let Some((fm_str, _)) = split_frontmatter(&text) else {
                continue;
            };
            let Ok(fm) = serde_yaml::from_str::<DurableFrontmatter>(&fm_str) else {
                continue;
            };
            if namespace.is_some_and(|ns| ns != fm.namespace) {
                continue;
            }
            let Ok(path) = self.materialize_path(&fm.namespace, &fm.name) else {
                continue;
            };
            entries.push(DurableArtifactEntry {
                namespace: fm.namespace.clone(),
                name: fm.name.clone(),
                path,
                frontmatter: Some(fm),
            });
        }
        Ok(entries)
    }

    async fn artifact_exists(&self, namespace: &str, name: &str) -> Result<bool> {
        let key = self.key(namespace, name)?;
        Ok(self.objects.head(&key).await.map_err(obj_err)?.is_some())
    }

    async fn delete(&self, namespace: &str, name: &str) -> Result<()> {
        let key = self.key(namespace, name)?;
        match self.objects.delete(&key, DeleteCondition::Existing).await {
            Ok(_) | Err(StorageError::NotFound) => Ok(()),
            Err(err) => Err(obj_err(err)),
        }
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let entries = self.list_entries(None).await?;
        let mut records = Vec::new();
        for entry in entries {
            let (frontmatter, body) = self.read(&entry.namespace, &entry.name).await?;
            records.push(ExportedDurable {
                namespace: entry.namespace,
                name: entry.name,
                frontmatter,
                body,
            });
        }
        Ok(serde_json::to_vec(&records)?)
    }

    async fn import_all(&self, bytes: &[u8]) -> Result<()> {
        let records: Vec<ExportedDurable> = serde_json::from_slice(bytes)?;
        for record in records {
            self.write(
                &record.namespace,
                &record.name,
                &record.body,
                record.frontmatter,
            )
            .await?;
        }
        Ok(())
    }
}

/// Local filesystem ObjectStore used as an explicit remote stand-in.
/// Does not construct S3 or touch default startup.
pub fn open_local_object_backend(root: impl AsRef<Path>) -> Result<Arc<dyn ObjectStore>> {
    let storage = magician_storage::LocalStorage::open(root.as_ref())
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(storage.objects as Arc<dyn ObjectStore>)
}

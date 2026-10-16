//! ObjectStore-backed database-file stand-in. Test/remote profile only. Not S3.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use futures_util::StreamExt;
use magician_storage::{
    bytes_body, ObjectStore, PutCondition, PutObjectRequest, StorageKey, StoragePrefix,
};

use super::local::{database_digest, DatabaseAccess, ExportedDatabase};
use super::owners::DatabaseOwner;

#[derive(Clone)]
pub struct RemoteDatabaseStore {
    objects: Arc<dyn ObjectStore>,
    principal: String,
    workspace: String,
    owner: DatabaseOwner,
    scratch: PathBuf,
}

impl RemoteDatabaseStore {
    pub fn new(
        objects: Arc<dyn ObjectStore>,
        principal: impl Into<String>,
        workspace: impl Into<String>,
        owner: DatabaseOwner,
        scratch: impl Into<PathBuf>,
    ) -> Self {
        Self {
            objects,
            principal: principal.into(),
            workspace: workspace.into(),
            owner,
            scratch: scratch.into(),
        }
    }

    fn key(&self, rel: &str) -> Result<StorageKey> {
        if !self.owner.allows(rel) {
            anyhow::bail!("{} rejects locator {rel}", self.owner.id());
        }
        let hex = blake3::hash(rel.as_bytes()).to_hex().to_string();
        StorageKey::tenant(
            &self.principal,
            &self.workspace,
            self.owner.id(),
            &format!("d{hex}"),
        )
        .map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn prefix(&self) -> StoragePrefix {
        StoragePrefix {
            encoded: format!(
                "t/{}/{}/{}/",
                self.principal,
                self.workspace,
                self.owner.id()
            ),
        }
    }

    fn envelope(rel: &str, bytes: &[u8]) -> Result<Vec<u8>> {
        Ok(serde_json::to_vec(&ExportedDatabase {
            path: rel.to_string(),
            digest: database_digest(bytes),
            b64: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes),
        })?)
    }

    fn parse_envelope(bytes: &[u8]) -> Result<(String, Vec<u8>)> {
        let record: ExportedDatabase = serde_json::from_slice(bytes)?;
        let raw = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            record.b64.as_bytes(),
        )?;
        if record.digest.is_empty() || record.digest != database_digest(&raw) {
            anyhow::bail!("database record digest mismatch");
        }
        Ok((record.path, raw))
    }
}

async fn collect_body(mut body: magician_storage::ObjectBodyStream) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    while let Some(chunk) = body.next().await {
        out.extend_from_slice(&chunk.map_err(|err| anyhow::anyhow!("{err}"))?);
    }
    Ok(out)
}

#[async_trait]
impl DatabaseAccess for RemoteDatabaseStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        crate::magician_v2::typed_io::reject_lease_lost_anyhow()?;
        if super::owners::is_live_engine_sidecar(rel) {
            anyhow::bail!("refuses opaque WAL/SHM write; checkpoint the engine first");
        }
        let key = self.key(rel)?;
        let body = Self::envelope(rel, bytes)?;
        self.objects
            .put(PutObjectRequest {
                key,
                body: bytes_body(body),
                content_type: Some("application/octet-stream".into()),
                condition: PutCondition::Overwrite,
            })
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        Ok(())
    }

    async fn get(&self, rel: &str) -> Result<Vec<u8>> {
        let key = self.key(rel)?;
        let read = self
            .objects
            .get(&key, None)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let bytes = collect_body(read.body).await?;
        let (path, raw) = Self::parse_envelope(&bytes)?;
        if path != rel {
            anyhow::bail!("database locator mismatch");
        }
        let scratch = self.scratch.join(rel);
        if scratch.is_absolute() && !scratch.starts_with(&self.scratch) {
            anyhow::bail!("scratch materialize escaped root");
        }
        if let Some(parent) = scratch.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&scratch, &raw).await?;
        Ok(raw)
    }

    async fn exists(&self, rel: &str) -> Result<bool> {
        let key = self.key(rel)?;
        Ok(self
            .objects
            .head(&key)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .is_some())
    }

    async fn list_selected(&self) -> Result<Vec<String>> {
        let listed = self
            .objects
            .list_diagnostic(&self.prefix(), None, 1000)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let mut names = Vec::new();
        for meta in listed {
            let read = match self.objects.get(&meta.key, None).await {
                Ok(read) => read,
                Err(_) => continue,
            };
            let bytes = match collect_body(read.body).await {
                Ok(bytes) => bytes,
                Err(_) => continue,
            };
            let Ok((path, _)) = Self::parse_envelope(&bytes) else {
                continue;
            };
            if self.owner.allows(&path) {
                names.push(path);
            }
        }
        names.sort();
        Ok(names)
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let files = self.list_selected().await?;
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

pub fn open_local_database_backend(root: impl AsRef<Path>) -> Result<Arc<dyn ObjectStore>> {
    let storage = magician_storage::LocalStorage::open(root.as_ref())
        .map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(storage.objects as Arc<dyn ObjectStore>)
}

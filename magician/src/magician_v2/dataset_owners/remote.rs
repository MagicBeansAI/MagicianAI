//! DatasetStore-backed family. Explicit test/remote profile only.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use async_trait::async_trait;
use magician_storage::dataset::ManifestVersion;
use magician_storage::fs::LocalDatasetStore;
use magician_storage::{
    bytes_body, ContentDigest, DatasetId, DatasetManifest, DatasetPartRef, DatasetStore,
    DigestAlgorithm, PartitionId, StageDatasetPart, StorageError,
};

use super::family::DatasetFamily;
use super::local::{part_digest, DatasetAccess, ExportedPart};

pub struct RemoteFamilyStore {
    datasets: Arc<dyn DatasetStore>,
    family: DatasetFamily,
    dataset_id: DatasetId,
    seen: Mutex<BTreeSet<String>>,
    scratch: PathBuf,
}

impl RemoteFamilyStore {
    pub fn new(
        datasets: Arc<dyn DatasetStore>,
        family: DatasetFamily,
        scratch: impl Into<PathBuf>,
    ) -> Result<Self> {
        Ok(Self {
            datasets,
            family,
            dataset_id: DatasetId::parse(family.id()).map_err(|err| anyhow::anyhow!("{err}"))?,
            seen: Mutex::new(BTreeSet::new()),
            scratch: scratch.into(),
        })
    }

    fn partition_id(rel: &str) -> Result<PartitionId> {
        let dir = rel
            .rsplit_once('/')
            .map(|(dir, _)| dir)
            .ok_or_else(|| anyhow::anyhow!("dataset locator missing partition"))?;
        let encoded = format!("p{}", blake3::hash(dir.as_bytes()).to_hex());
        PartitionId::parse(&encoded).map_err(|err| anyhow::anyhow!("{err}"))
    }

    fn file_name(rel: &str) -> Result<&str> {
        rel.rsplit_once('/')
            .map(|(_, name)| name)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| anyhow::anyhow!("dataset locator missing file name"))
    }
}

#[async_trait]
impl DatasetAccess for RemoteFamilyStore {
    async fn put(&self, rel: &str, bytes: &[u8]) -> Result<()> {
        if !self.family.allows(rel) {
            anyhow::bail!("{} rejects locator {rel}", self.family.id());
        }
        let partition = Self::partition_id(rel)?;
        let name = Self::file_name(rel)?.to_string();
        let dir = rel.rsplit_once('/').map(|(dir, _)| dir).unwrap();
        let digest_hex = part_digest(bytes);
        let part = DatasetPartRef {
            dataset: self.dataset_id.clone(),
            partition: partition.clone(),
            generation: format!("g{}", &digest_hex[..16]),
            name: name.clone(),
        };
        let digest = ContentDigest {
            algorithm: DigestAlgorithm::Blake3,
            hex: digest_hex,
        };
        match self
            .datasets
            .stage_part(StageDatasetPart {
                part: part.clone(),
                digest: digest.clone(),
                len: bytes.len() as u64,
                body: bytes_body(bytes.to_vec()),
            })
            .await
        {
            Ok(_) => {},
            Err(StorageError::Conflict { .. }) => {},
            Err(err) => return Err(anyhow::anyhow!("{err}")),
        }
        let current = self
            .datasets
            .read_manifest(&self.dataset_id, &partition)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let expected = current.as_ref().map(|manifest| manifest.version.clone());
        let mut parts = current.map(|manifest| manifest.parts).unwrap_or_default();
        parts.retain(|existing| existing.name != name);
        parts.push(part);
        let version = ManifestVersion::parse(&ulid::Ulid::new().to_string())
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        self.datasets
            .commit_manifest(
                DatasetManifest {
                    dataset: self.dataset_id.clone(),
                    partition,
                    version,
                    parts,
                    row_count: 1,
                    schema_identity: dir.to_string(),
                },
                expected,
            )
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        self.seen.lock().expect("seen").insert(rel.to_string());
        Ok(())
    }

    async fn get(&self, rel: &str) -> Result<Vec<u8>> {
        if !self.family.allows(rel) {
            anyhow::bail!("{} rejects locator {rel}", self.family.id());
        }
        let partition = Self::partition_id(rel)?;
        let name = Self::file_name(rel)?;
        let manifest = self
            .datasets
            .read_manifest(&self.dataset_id, &partition)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?
            .context("dataset partition manifest missing")?;
        let part = manifest
            .parts
            .iter()
            .find(|part| part.name == name)
            .cloned()
            .context("dataset part missing")?;
        let read = self
            .datasets
            .open_part(&part, None)
            .await
            .map_err(|err| anyhow::anyhow!("{err}"))?;
        let mut bytes = Vec::new();
        let mut body = read.body;
        use futures_util::StreamExt;
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk.map_err(|err| anyhow::anyhow!("{err}"))?);
        }
        if read.metadata.digest.hex != part_digest(&bytes) {
            anyhow::bail!("dataset part digest mismatch");
        }
        let scratch = self.scratch.join(rel);
        if scratch.is_absolute() && !scratch.starts_with(&self.scratch) {
            anyhow::bail!("scratch materialize escaped root");
        }
        if let Some(parent) = scratch.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&scratch, &bytes).await?;
        Ok(bytes)
    }

    async fn exists(&self, rel: &str) -> Result<bool> {
        Ok(self.seen.lock().expect("seen").contains(rel))
    }

    async fn list_selected(&self) -> Result<Vec<String>> {
        let mut names: Vec<String> = self.seen.lock().expect("seen").iter().cloned().collect();
        names.sort();
        Ok(names)
    }

    async fn export_all(&self) -> Result<Vec<u8>> {
        let mut records = Vec::new();
        for rel in self.list_selected().await? {
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

/// Hermetic DatasetStore used as an explicit remote stand-in. Not S3.
pub fn open_local_dataset_backend(root: impl AsRef<Path>) -> Arc<dyn DatasetStore> {
    Arc::new(LocalDatasetStore::new(root.as_ref().to_path_buf()))
}

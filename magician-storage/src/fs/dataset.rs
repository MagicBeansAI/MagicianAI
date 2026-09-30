use std::fs::{File, OpenOptions};
use std::path::PathBuf;

use async_trait::async_trait;
use fs4::FileExt;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::dataset::{
    DatasetId, DatasetManifest, DatasetPartReceipt, DatasetPartRef, DatasetStore,
    ManifestCommitReceipt, ManifestVersion, PartitionId, StageDatasetPart,
};
use crate::error::StorageError;
use crate::object::{bytes_body, ByteRange, ObjectRead, ObjectVersion};

use super::paths::{digest_of, join_encoded};

pub struct LocalDatasetStore {
    root: PathBuf,
}

impl LocalDatasetStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    /// Delete part generations that no current manifest references.
    pub fn collect_unreferenced_generations(&self) -> Result<crate::gc::GcReport, StorageError> {
        use std::collections::BTreeSet;
        let mut live: BTreeSet<(String, String, String)> = BTreeSet::new();
        let manifests = self.root.join("manifests");
        let mut manifest_files = Vec::new();
        collect_json_files(&manifests, &mut manifest_files)?;
        for path in &manifest_files {
            let bytes =
                std::fs::read(path).map_err(|err| StorageError::backend(err.to_string()))?;
            let manifest: DatasetManifest =
                serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
                    detail: format!("dataset manifest {}: {err}", path.display()),
                })?;
            for part in manifest.parts {
                live.insert((
                    part.dataset.as_str().to_string(),
                    part.partition.as_str().to_string(),
                    part.generation,
                ));
            }
        }
        let mut report = crate::gc::GcReport::default();
        let parts_root = self.root.join("parts");
        let mut generations = Vec::new();
        collect_generation_dirs(&parts_root, &mut generations)?;
        if generations.is_empty() {
            return Ok(report);
        }
        if manifest_files.is_empty() {
            return Err(StorageError::Corrupt {
                detail: "dataset GC found parts but no manifests".into(),
            });
        }
        for (dataset, partition, generation, dir) in generations {
            report.scanned += 1;
            if live.contains(&(dataset, partition, generation)) {
                report.retained_referenced += 1;
                continue;
            }
            match std::fs::remove_dir_all(&dir) {
                Ok(()) => report.collected += 1,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {},
                Err(err) => return Err(StorageError::backend(err.to_string())),
            }
        }
        Ok(report)
    }

    fn part_path(&self, part: &DatasetPartRef) -> Result<PathBuf, StorageError> {
        join_encoded(
            &self.root,
            &format!(
                "parts/{}/{}/{}/{}",
                part.dataset.as_str(),
                part.partition.as_str(),
                part.generation,
                part.name
            ),
        )
    }

    fn manifest_path(
        &self,
        dataset: &DatasetId,
        partition: &PartitionId,
    ) -> Result<PathBuf, StorageError> {
        join_encoded(
            &self.root,
            &format!("manifests/{}/{}.json", dataset.as_str(), partition.as_str()),
        )
    }
}

#[async_trait]
impl DatasetStore for LocalDatasetStore {
    async fn stage_part(
        &self,
        request: StageDatasetPart,
    ) -> Result<DatasetPartReceipt, StorageError> {
        crate::identifiers::LogicalObjectId::parse(&request.part.generation)
            .map_err(|_| StorageError::invalid_key("part identity"))?;
        crate::identifiers::LogicalObjectId::parse(&request.part.name)
            .map_err(|_| StorageError::invalid_key("part identity"))?;
        let path = self.part_path(&request.part)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4().simple()));
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let mut hasher = blake3::Hasher::new();
        let mut len = 0u64;
        let mut body = request.body;
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            len += chunk.len() as u64;
            file.write_all(&chunk)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        file.sync_all()
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        drop(file);
        let actual = hasher.finalize().to_hex().to_string();
        if actual != request.digest.hex || len != request.len {
            let _ = tokio::fs::remove_file(&tmp).await;
            return Err(StorageError::Integrity {
                expected: request.digest.hex,
                actual,
            });
        }
        match std::fs::hard_link(&tmp, &path) {
            Ok(()) => {
                if let Some(parent) = path.parent() {
                    let dir = std::fs::File::open(parent)
                        .map_err(|err| StorageError::backend(err.to_string()))?;
                    dir.sync_all()
                        .map_err(|err| StorageError::backend(err.to_string()))?;
                }
                let _ = tokio::fs::remove_file(&tmp).await;
            },
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = tokio::fs::remove_file(&tmp).await;
                let existing = tokio::fs::read(&path)
                    .await
                    .map_err(|read_err| StorageError::backend(read_err.to_string()))?;
                if blake3::hash(&existing).to_hex().as_str() == actual
                    && existing.len() as u64 == len
                {
                    if let Some(parent) = path.parent() {
                        let dir = std::fs::File::open(parent)
                            .map_err(|err| StorageError::backend(err.to_string()))?;
                        dir.sync_all()
                            .map_err(|err| StorageError::backend(err.to_string()))?;
                    }
                    return Ok(DatasetPartReceipt {
                        part: request.part,
                        digest: request.digest,
                    });
                }
                return Err(StorageError::Conflict {
                    expected: None,
                    actual: Some("part exists".into()),
                });
            },
            Err(err) => {
                let _ = tokio::fs::remove_file(&tmp).await;
                return Err(StorageError::backend(err.to_string()));
            },
        }
        Ok(DatasetPartReceipt {
            part: request.part,
            digest: request.digest,
        })
    }

    async fn read_manifest(
        &self,
        dataset: &DatasetId,
        partition: &PartitionId,
    ) -> Result<Option<DatasetManifest>, StorageError> {
        let path = self.manifest_path(dataset, partition)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|err| StorageError::Corrupt {
                        detail: err.to_string(),
                    })
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StorageError::backend(err.to_string())),
        }
    }

    async fn commit_manifest(
        &self,
        manifest: DatasetManifest,
        expected: Option<ManifestVersion>,
    ) -> Result<ManifestCommitReceipt, StorageError> {
        let path = self.manifest_path(&manifest.dataset, &manifest.partition)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let lock_path = path.with_extension("json.lock");
        let _lock = lock_path_exclusive(lock_path).await?;
        let current = self
            .read_manifest(&manifest.dataset, &manifest.partition)
            .await?;
        match (expected, current.as_ref().map(|m| &m.version)) {
            (None, Some(actual)) => {
                return Err(StorageError::Conflict {
                    expected: None,
                    actual: Some(actual.as_str().to_string()),
                });
            },
            (Some(expected), None) => {
                return Err(StorageError::Conflict {
                    expected: Some(expected.as_str().to_string()),
                    actual: None,
                });
            },
            (Some(expected), Some(actual)) if expected.as_str() != actual.as_str() => {
                return Err(StorageError::Conflict {
                    expected: Some(expected.as_str().to_string()),
                    actual: Some(actual.as_str().to_string()),
                });
            },
            _ => {},
        }
        for part in &manifest.parts {
            crate::identifiers::LogicalObjectId::parse(&part.generation)
                .map_err(|_| StorageError::invalid_key("part identity"))?;
            crate::identifiers::LogicalObjectId::parse(&part.name)
                .map_err(|_| StorageError::invalid_key("part identity"))?;
        }
        let bytes = serde_json::to_vec_pretty(&manifest)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let tmp = path.with_extension(format!("json.tmp.{}", uuid::Uuid::new_v4().simple()));
        tokio::fs::write(&tmp, &bytes)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        {
            let file = tokio::fs::File::open(&tmp)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
            file.sync_all()
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        if let Some(parent) = path.parent() {
            let dir = std::fs::File::open(parent)
                .map_err(|err| StorageError::backend(err.to_string()))?;
            dir.sync_all()
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        Ok(ManifestCommitReceipt {
            version: manifest.version,
        })
    }

    async fn open_part(
        &self,
        part: &DatasetPartRef,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError> {
        let path = self.part_path(part)?;
        let bytes = tokio::fs::read(&path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::backend(err.to_string())
            }
        })?;
        let digest = digest_of(&bytes);
        let slice = match range {
            None => bytes,
            Some(range) => {
                let start = range.start as usize;
                let end = range
                    .end_exclusive
                    .map(|end| end as usize)
                    .unwrap_or(bytes.len())
                    .min(bytes.len());
                bytes.get(start..end).unwrap_or(&[]).to_vec()
            },
        };
        let key = crate::identifiers::StorageKey::system("datasets", &part.name)?;
        Ok(ObjectRead {
            metadata: crate::object::ObjectMetadata {
                key,
                len: slice.len() as u64,
                digest,
                version: ObjectVersion::unversioned(),
            },
            body: bytes_body(bytes::Bytes::from(slice)),
        })
    }
}

fn collect_json_files(
    dir: &std::path::Path,
    out: &mut Vec<std::path::PathBuf>,
) -> Result<(), StorageError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(StorageError::backend(err.to_string())),
    };
    for entry in entries {
        let entry = entry.map_err(|err| StorageError::backend(err.to_string()))?;
        let path = entry.path();
        if path.is_dir() {
            collect_json_files(&path, out)?;
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
            out.push(path);
        }
    }
    Ok(())
}

fn collect_generation_dirs(
    parts_root: &std::path::Path,
    out: &mut Vec<(String, String, String, std::path::PathBuf)>,
) -> Result<(), StorageError> {
    let datasets = match std::fs::read_dir(parts_root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(StorageError::backend(err.to_string())),
    };
    for dataset in datasets {
        let dataset = dataset.map_err(|err| StorageError::backend(err.to_string()))?;
        if !dataset.path().is_dir() {
            continue;
        }
        let dataset_id = dataset.file_name().to_string_lossy().into_owned();
        let partitions = match std::fs::read_dir(dataset.path()) {
            Ok(entries) => entries,
            Err(_) => continue,
        };
        for partition in partitions.flatten() {
            if !partition.path().is_dir() {
                continue;
            }
            let partition_id = partition.file_name().to_string_lossy().into_owned();
            let generations = match std::fs::read_dir(partition.path()) {
                Ok(entries) => entries,
                Err(_) => continue,
            };
            for generation in generations.flatten() {
                if !generation.path().is_dir() {
                    continue;
                }
                out.push((
                    dataset_id.clone(),
                    partition_id.clone(),
                    generation.file_name().to_string_lossy().into_owned(),
                    generation.path(),
                ));
            }
        }
    }
    Ok(())
}

async fn lock_path_exclusive(lock_path: PathBuf) -> Result<File, StorageError> {
    tokio::task::spawn_blocking(move || {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        file.lock_exclusive()
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(file)
    })
    .await
    .map_err(|err| StorageError::backend(err.to_string()))?
}

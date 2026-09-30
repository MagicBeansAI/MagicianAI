use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartReceipt, DatasetPartRef, DatasetStore,
    ManifestCommitReceipt, ManifestVersion, PartitionId, StageDatasetPart,
};
use magician_storage::object::{
    bytes_body, ByteRange, ObjectMetadata, ObjectRead, ObjectStore, ObjectVersion, PutCondition,
    PutObjectRequest,
};
use magician_storage::StorageError;
use magician_storage::StorageKey;

use crate::object::S3ObjectStore;

pub struct S3DatasetStore {
    objects: Arc<S3ObjectStore>,
}

impl S3DatasetStore {
    pub fn new(objects: Arc<S3ObjectStore>) -> Self {
        Self { objects }
    }

    fn part_key(part: &DatasetPartRef) -> Result<StorageKey, StorageError> {
        StorageKey::system(
            "datasets",
            &format!(
                "parts.{}.{}.{}.{}",
                part.dataset.as_str(),
                part.partition.as_str(),
                part.generation,
                part.name
            ),
        )
    }

    fn manifest_key(
        dataset: &DatasetId,
        partition: &PartitionId,
    ) -> Result<StorageKey, StorageError> {
        StorageKey::system(
            "datasets",
            &format!("manifests.{}.{}.json", dataset.as_str(), partition.as_str()),
        )
    }
}

#[async_trait]
impl DatasetStore for S3DatasetStore {
    async fn stage_part(
        &self,
        request: StageDatasetPart,
    ) -> Result<DatasetPartReceipt, StorageError> {
        magician_storage::LogicalObjectId::parse(&request.part.generation)
            .map_err(|_| StorageError::invalid_key("part identity"))?;
        magician_storage::LogicalObjectId::parse(&request.part.name)
            .map_err(|_| StorageError::invalid_key("part identity"))?;
        let key = Self::part_key(&request.part)?;
        let mut hasher = blake3::Hasher::new();
        let mut len = 0u64;
        let mut body = request.body;
        let mut acc = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            hasher.update(&chunk);
            len += chunk.len() as u64;
            acc.extend_from_slice(&chunk);
        }
        let actual = hasher.finalize().to_hex().to_string();
        if actual != request.digest.hex || len != request.len {
            return Err(StorageError::Integrity {
                expected: request.digest.hex,
                actual,
            });
        }
        self.objects
            .put(PutObjectRequest {
                key,
                body: bytes_body(bytes::Bytes::from(acc)),
                content_type: None,
                condition: PutCondition::CreateOnly,
            })
            .await?;
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
        let key = Self::manifest_key(dataset, partition)?;
        match self.objects.get(&key, None).await {
            Ok(read) => {
                let mut bytes = Vec::new();
                let mut body = read.body;
                while let Some(chunk) = body.next().await {
                    bytes.extend_from_slice(&chunk?);
                }
                serde_json::from_slice(&bytes)
                    .map(Some)
                    .map_err(|err| StorageError::Corrupt {
                        detail: err.to_string(),
                    })
            },
            Err(StorageError::NotFound) => Ok(None),
            Err(err) => Err(err),
        }
    }

    async fn commit_manifest(
        &self,
        manifest: DatasetManifest,
        expected: Option<ManifestVersion>,
    ) -> Result<ManifestCommitReceipt, StorageError> {
        let key = Self::manifest_key(&manifest.dataset, &manifest.partition)?;
        let current_read = match self.objects.get(&key, None).await {
            Ok(read) => Some(read),
            Err(StorageError::NotFound) => None,
            Err(err) => return Err(err),
        };
        let (current_manifest, object_version) = match current_read {
            None => (None, None),
            Some(read) => {
                let object_version = read.metadata.version.clone();
                let mut bytes = Vec::new();
                let mut body = read.body;
                while let Some(chunk) = body.next().await {
                    bytes.extend_from_slice(&chunk?);
                }
                let parsed: DatasetManifest =
                    serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
                        detail: err.to_string(),
                    })?;
                let object_version = if object_version.as_str() == ObjectVersion::UNVERSIONED
                    || object_version.as_str().is_empty()
                {
                    None
                } else {
                    Some(object_version)
                };
                (Some(parsed), object_version)
            },
        };
        let condition = match (
            expected,
            current_manifest.as_ref().map(|m| &m.version),
            object_version,
        ) {
            (None, Some(actual), _) => {
                return Err(StorageError::Conflict {
                    expected: None,
                    actual: Some(actual.as_str().to_string()),
                });
            },
            (Some(expected), None, _) => {
                return Err(StorageError::Conflict {
                    expected: Some(expected.as_str().to_string()),
                    actual: None,
                });
            },
            (Some(expected), Some(actual), _) if expected.as_str() != actual.as_str() => {
                return Err(StorageError::Conflict {
                    expected: Some(expected.as_str().to_string()),
                    actual: Some(actual.as_str().to_string()),
                });
            },
            (None, None, _) => PutCondition::CreateOnly,
            (Some(_), Some(_), Some(version)) => PutCondition::ExpectedVersion(version),
            (Some(_), Some(_), None) => {
                return Err(StorageError::Corrupt {
                    detail: "manifest object version missing".into(),
                });
            },
        };
        let bytes =
            serde_json::to_vec(&manifest).map_err(|err| StorageError::backend(err.to_string()))?;
        self.objects
            .put(PutObjectRequest {
                key,
                body: bytes_body(bytes),
                content_type: Some("application/json".into()),
                condition,
            })
            .await?;
        Ok(ManifestCommitReceipt {
            version: manifest.version,
        })
    }

    async fn open_part(
        &self,
        part: &DatasetPartRef,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError> {
        let key = Self::part_key(part)?;
        let read = self.objects.get(&key, range).await?;
        Ok(ObjectRead {
            metadata: ObjectMetadata {
                key,
                len: read.metadata.len,
                digest: read.metadata.digest,
                version: read.metadata.version,
            },
            body: read.body,
        })
    }
}

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::identifiers::ContentDigest;
use crate::object::{ByteRange, ObjectBodyStream, ObjectRead, ObjectVersion};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DatasetId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct PartitionId(String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ManifestVersion(String);

impl DatasetId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        crate::identifiers::LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl PartitionId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        crate::identifiers::LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl ManifestVersion {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        ObjectVersion::new(raw).map(|v| Self(v.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for DatasetId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for PartitionId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl<'de> Deserialize<'de> for ManifestVersion {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetPartRef {
    pub dataset: DatasetId,
    pub partition: PartitionId,
    pub generation: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetManifest {
    pub dataset: DatasetId,
    pub partition: PartitionId,
    pub version: ManifestVersion,
    pub parts: Vec<DatasetPartRef>,
    pub row_count: u64,
    pub schema_identity: String,
}

pub struct StageDatasetPart {
    pub part: DatasetPartRef,
    pub digest: ContentDigest,
    pub len: u64,
    pub body: ObjectBodyStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatasetPartReceipt {
    pub part: DatasetPartRef,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestCommitReceipt {
    pub version: ManifestVersion,
}

#[async_trait]
pub trait DatasetStore: Send + Sync {
    async fn stage_part(
        &self,
        request: StageDatasetPart,
    ) -> Result<DatasetPartReceipt, StorageError>;
    async fn read_manifest(
        &self,
        dataset: &DatasetId,
        partition: &PartitionId,
    ) -> Result<Option<DatasetManifest>, StorageError>;
    async fn commit_manifest(
        &self,
        manifest: DatasetManifest,
        expected: Option<ManifestVersion>,
    ) -> Result<ManifestCommitReceipt, StorageError>;
    async fn open_part(
        &self,
        part: &DatasetPartRef,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError>;
}

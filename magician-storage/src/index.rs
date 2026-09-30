use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::identifiers::ScopeId;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct IndexId(String);

impl IndexId {
    pub fn parse(raw: &str) -> Result<Self, StorageError> {
        crate::identifiers::LogicalObjectId::parse(raw).map(|id| Self(id.as_str().to_string()))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for IndexId {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexQuery {
    pub text: String,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPage {
    pub hits: Vec<String>,
    pub watermark: Option<IndexWatermark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexMutationBatch {
    pub scope: ScopeId,
    pub index: IndexId,
    pub source_watermark: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexWatermark {
    pub source: String,
    pub position: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildIndexRequest {
    pub scope: ScopeId,
    pub index: IndexId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildReceipt {
    pub watermark: IndexWatermark,
}

#[async_trait]
pub trait IndexStore: Send + Sync {
    async fn query(
        &self,
        scope: &ScopeId,
        index: &IndexId,
        query: IndexQuery,
    ) -> Result<IndexPage, StorageError>;
    async fn apply(&self, batch: IndexMutationBatch) -> Result<IndexWatermark, StorageError>;
    async fn watermark(
        &self,
        scope: &ScopeId,
        index: &IndexId,
    ) -> Result<Option<IndexWatermark>, StorageError>;
    async fn rebuild(&self, request: RebuildIndexRequest) -> Result<RebuildReceipt, StorageError>;
}

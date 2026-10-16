use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::identifiers::ScopeId;
use crate::index::{
    IndexId, IndexMutationBatch, IndexPage, IndexQuery, IndexStore, IndexWatermark,
    RebuildIndexRequest, RebuildReceipt,
};

use super::paths::join_encoded;

#[derive(Serialize, Deserialize)]
struct StoredWatermark {
    source: String,
    position: String,
}

pub struct LocalIndexStore {
    root: PathBuf,
}

impl LocalIndexStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, scope: &ScopeId, index: &IndexId) -> Result<PathBuf, StorageError> {
        join_encoded(
            &self.root,
            &format!(
                "{}/{}/{}.watermark.json",
                scope.principal.as_str(),
                scope.workspace.as_str(),
                index.as_str()
            ),
        )
    }

    async fn write_watermark(
        &self,
        scope: &ScopeId,
        index: &IndexId,
        watermark: &IndexWatermark,
    ) -> Result<(), StorageError> {
        let path = self.path(scope, index)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let stored = StoredWatermark {
            source: watermark.source.clone(),
            position: watermark.position.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&stored)
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::fs::write(path, bytes)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))
    }
}

#[async_trait]
impl IndexStore for LocalIndexStore {
    async fn query(
        &self,
        scope: &ScopeId,
        index: &IndexId,
        query: IndexQuery,
    ) -> Result<IndexPage, StorageError> {
        let _ = query;
        Ok(IndexPage {
            hits: Vec::new(),
            watermark: self.watermark(scope, index).await?,
        })
    }

    async fn apply(&self, batch: IndexMutationBatch) -> Result<IndexWatermark, StorageError> {
        let watermark = IndexWatermark {
            source: batch.index.as_str().to_string(),
            position: batch.source_watermark,
        };
        self.write_watermark(&batch.scope, &batch.index, &watermark)
            .await?;
        Ok(watermark)
    }

    async fn watermark(
        &self,
        scope: &ScopeId,
        index: &IndexId,
    ) -> Result<Option<IndexWatermark>, StorageError> {
        let path = self.path(scope, index)?;
        match tokio::fs::read(&path).await {
            Ok(bytes) => {
                let stored: StoredWatermark =
                    serde_json::from_slice(&bytes).map_err(|err| StorageError::Corrupt {
                        detail: err.to_string(),
                    })?;
                Ok(Some(IndexWatermark {
                    source: stored.source,
                    position: stored.position,
                }))
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(StorageError::backend(err.to_string())),
        }
    }

    async fn rebuild(&self, request: RebuildIndexRequest) -> Result<RebuildReceipt, StorageError> {
        let watermark = IndexWatermark {
            source: request.index.as_str().to_string(),
            position: "rebuilt".into(),
        };
        self.write_watermark(&request.scope, &request.index, &watermark)
            .await?;
        Ok(RebuildReceipt { watermark })
    }
}

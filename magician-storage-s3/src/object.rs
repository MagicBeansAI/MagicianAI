use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt;

use magician_storage::identifiers::StorageKey;
use magician_storage::object::{
    ByteRange, DeleteCondition, DeleteReceipt, ObjectMetadata, ObjectRead, ObjectStore,
    PutObjectReceipt, PutObjectRequest, StoragePrefix,
};
use magician_storage::StorageError;

use crate::backend::{join_prefix, BlobStore};
use crate::memory::MemoryBlobStore;

pub struct S3ObjectStore {
    backend: Arc<dyn BlobStore>,
    prefix: String,
    max_object_bytes: u64,
    pub puts: std::sync::atomic::AtomicU64,
    pub gets: std::sync::atomic::AtomicU64,
}

impl S3ObjectStore {
    pub fn hermetic(prefix: impl Into<String>) -> Self {
        Self::new(Arc::new(MemoryBlobStore::new()), prefix, 64 * 1024 * 1024)
    }

    pub fn new(
        backend: Arc<dyn BlobStore>,
        prefix: impl Into<String>,
        max_object_bytes: u64,
    ) -> Self {
        Self {
            backend,
            prefix: prefix.into(),
            max_object_bytes,
            puts: std::sync::atomic::AtomicU64::new(0),
            gets: std::sync::atomic::AtomicU64::new(0),
        }
    }

    pub fn backend(&self) -> Arc<dyn BlobStore> {
        Arc::clone(&self.backend)
    }

    pub async fn cleanup_abandoned(&self, older_than: Duration) -> Result<u64, StorageError> {
        self.backend.cleanup_abandoned(older_than).await
    }

    fn blob_key(&self, key: &StorageKey) -> String {
        join_prefix(&self.prefix, &key.encode())
    }

    async fn collect_body(
        &self,
        mut body: magician_storage::object::ObjectBodyStream,
    ) -> Result<Bytes, StorageError> {
        let mut acc = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk?;
            acc.extend_from_slice(&chunk);
            if acc.len() as u64 > self.max_object_bytes {
                return Err(StorageError::CapacityExceeded);
            }
        }
        Ok(Bytes::from(acc))
    }
}

#[async_trait]
impl ObjectStore for S3ObjectStore {
    async fn head(&self, key: &StorageKey) -> Result<Option<ObjectMetadata>, StorageError> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(self
            .backend
            .head(&self.blob_key(key))
            .await?
            .map(|meta| ObjectMetadata {
                key: key.clone(),
                len: meta.len,
                digest: meta.digest,
                version: meta.version,
            }))
    }

    async fn get(
        &self,
        key: &StorageKey,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError> {
        self.gets.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (meta, bytes) = self.backend.get(&self.blob_key(key), range).await?;
        Ok(ObjectRead {
            metadata: ObjectMetadata {
                key: key.clone(),
                len: meta.len,
                digest: meta.digest,
                version: meta.version,
            },
            body: magician_storage::bytes_body(bytes),
        })
    }

    async fn put(&self, request: PutObjectRequest) -> Result<PutObjectReceipt, StorageError> {
        self.puts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let bytes = self.collect_body(request.body).await?;
        let meta = self
            .backend
            .put(&self.blob_key(&request.key), bytes, request.condition)
            .await?;
        Ok(PutObjectReceipt {
            metadata: ObjectMetadata {
                key: request.key,
                len: meta.len,
                digest: meta.digest,
                version: meta.version,
            },
            correlation: "s3-put".into(),
        })
    }

    async fn delete(
        &self,
        key: &StorageKey,
        condition: DeleteCondition,
    ) -> Result<DeleteReceipt, StorageError> {
        let tombstone = self.backend.delete(&self.blob_key(key), condition).await?;
        Ok(DeleteReceipt {
            key: key.clone(),
            tombstone_version: tombstone,
        })
    }

    async fn list_diagnostic(
        &self,
        prefix: &StoragePrefix,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<Vec<ObjectMetadata>, StorageError> {
        let blob_prefix = join_prefix(&self.prefix, &prefix.encoded);
        let listed = self
            .backend
            .list(&blob_prefix, cursor.as_deref(), limit.min(1000))
            .await?;
        let mut out = Vec::new();
        let strip = if self.prefix.is_empty() {
            0
        } else {
            self.prefix.trim_matches('/').len() + 1
        };
        for meta in listed {
            let encoded = meta.key.get(strip..).unwrap_or(&meta.key);
            if let Ok(key) = StorageKey::decode(encoded) {
                out.push(ObjectMetadata {
                    key,
                    len: meta.len,
                    digest: meta.digest,
                    version: meta.version,
                });
            }
        }
        Ok(out)
    }
}

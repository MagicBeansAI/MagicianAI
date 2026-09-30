use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;

use magician_storage::identifiers::ContentDigest;
use magician_storage::object::{ByteRange, DeleteCondition, ObjectVersion, PutCondition};
use magician_storage::StorageError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobMeta {
    pub key: String,
    pub len: u64,
    pub digest: ContentDigest,
    pub version: ObjectVersion,
}

#[async_trait]
pub trait BlobStore: Send + Sync {
    async fn put(
        &self,
        key: &str,
        bytes: Bytes,
        condition: PutCondition,
    ) -> Result<BlobMeta, StorageError>;
    async fn get(
        &self,
        key: &str,
        range: Option<ByteRange>,
    ) -> Result<(BlobMeta, Bytes), StorageError>;
    async fn head(&self, key: &str) -> Result<Option<BlobMeta>, StorageError>;
    async fn delete(
        &self,
        key: &str,
        condition: DeleteCondition,
    ) -> Result<ObjectVersion, StorageError>;
    async fn list(
        &self,
        prefix: &str,
        cursor: Option<&str>,
        limit: usize,
    ) -> Result<Vec<BlobMeta>, StorageError>;
    async fn create_multipart(&self, key: &str) -> Result<String, StorageError>;
    async fn upload_part(
        &self,
        upload_id: &str,
        part: i32,
        bytes: Bytes,
    ) -> Result<(), StorageError>;
    async fn complete_multipart(
        &self,
        upload_id: &str,
        condition: PutCondition,
    ) -> Result<BlobMeta, StorageError>;
    async fn abort_multipart(&self, upload_id: &str) -> Result<(), StorageError>;
    async fn cleanup_abandoned(&self, older_than: Duration) -> Result<u64, StorageError>;
}

pub fn digest_of(bytes: &[u8]) -> ContentDigest {
    magician_storage::ContentDigest {
        algorithm: magician_storage::DigestAlgorithm::Blake3,
        hex: blake3::hash(bytes).to_hex().to_string(),
    }
}

pub fn join_prefix(prefix: &str, encoded: &str) -> String {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        encoded.to_string()
    } else {
        format!("{prefix}/{encoded}")
    }
}

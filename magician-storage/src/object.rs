use std::pin::Pin;

use async_trait::async_trait;
use bytes::Bytes;
use futures_core::Stream;
use futures_util::stream;
use serde::{Deserialize, Serialize};

use crate::error::StorageError;
use crate::identifiers::{ContentDigest, StorageKey};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ObjectVersion(String);

impl ObjectVersion {
    pub const UNVERSIONED: &'static str = "unversioned";

    pub fn new(raw: impl Into<String>) -> Result<Self, StorageError> {
        let raw = raw.into();
        if raw.is_empty() || raw.len() > 256 {
            return Err(StorageError::invalid_key("object version"));
        }
        Ok(Self(raw))
    }

    pub fn unversioned() -> Self {
        Self(Self::UNVERSIONED.to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRange {
    pub start: u64,
    pub end_exclusive: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMetadata {
    pub key: StorageKey,
    pub len: u64,
    pub digest: ContentDigest,
    pub version: ObjectVersion,
}

pub type ObjectBodyStream = Pin<Box<dyn Stream<Item = Result<Bytes, StorageError>> + Send>>;

pub fn bytes_body(bytes: impl Into<Bytes>) -> ObjectBodyStream {
    let bytes = bytes.into();
    Box::pin(stream::once(async move { Ok(bytes) }))
}

pub struct ObjectRead {
    pub metadata: ObjectMetadata,
    pub body: ObjectBodyStream,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutCondition {
    /// Unconditional replace. The local adapter still assigns a new version.
    Overwrite,
    CreateOnly,
    ExpectedVersion(ObjectVersion),
}

pub struct PutObjectRequest {
    pub key: StorageKey,
    pub body: ObjectBodyStream,
    pub content_type: Option<String>,
    pub condition: PutCondition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PutObjectReceipt {
    pub metadata: ObjectMetadata,
    pub correlation: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeleteCondition {
    /// Remove if a live object is present.
    Existing,
    ExpectedVersion(ObjectVersion),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeleteReceipt {
    pub key: StorageKey,
    pub tombstone_version: ObjectVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoragePrefix {
    pub encoded: String,
}

#[async_trait]
pub trait ObjectStore: Send + Sync {
    async fn head(&self, key: &StorageKey) -> Result<Option<ObjectMetadata>, StorageError>;
    async fn get(
        &self,
        key: &StorageKey,
        range: Option<ByteRange>,
    ) -> Result<ObjectRead, StorageError>;
    async fn put(&self, request: PutObjectRequest) -> Result<PutObjectReceipt, StorageError>;
    async fn delete(
        &self,
        key: &StorageKey,
        condition: DeleteCondition,
    ) -> Result<DeleteReceipt, StorageError>;
    /// Diagnostic listing. Listing is never authority.
    async fn list_diagnostic(
        &self,
        prefix: &StoragePrefix,
        cursor: Option<String>,
        limit: usize,
    ) -> Result<Vec<ObjectMetadata>, StorageError>;
}

use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::StorageError;
use crate::identifiers::{ContentDigest, ScopeId};

#[derive(Debug, Clone)]
pub struct ScratchRequest {
    pub scope: ScopeId,
    pub purpose: String,
    pub max_bytes: u64,
}

#[derive(Debug, Clone)]
pub struct ScratchLease {
    pub root: PathBuf,
    pub max_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MaterializationPurpose {
    Read,
    ExecuteAdmission,
}

#[derive(Debug, Clone)]
pub struct ObjectRef {
    pub encoded_key: String,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone)]
pub struct MaterializedFile {
    pub path: PathBuf,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScratchCapacity {
    pub used_bytes: u64,
    pub max_bytes: u64,
}

#[async_trait]
pub trait ScratchStore: Send + Sync {
    async fn allocate(&self, request: ScratchRequest) -> Result<ScratchLease, StorageError>;
    async fn materialize_object(
        &self,
        object: &ObjectRef,
        purpose: MaterializationPurpose,
    ) -> Result<MaterializedFile, StorageError>;
    async fn capacity(&self) -> Result<ScratchCapacity, StorageError>;
}

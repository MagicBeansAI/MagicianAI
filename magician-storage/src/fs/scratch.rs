use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use tokio::io::AsyncWriteExt;

use crate::error::StorageError;
use crate::object::ObjectStore;
use crate::scratch::{
    MaterializationPurpose, MaterializedFile, ObjectRef, ScratchCapacity, ScratchLease,
    ScratchRequest, ScratchStore,
};

use super::object::LocalObjectStore;
use super::paths::digest_of;

pub struct LocalScratchStore {
    root: PathBuf,
    objects: Arc<LocalObjectStore>,
    max_bytes: u64,
}

impl LocalScratchStore {
    pub fn new(root: PathBuf, objects: Arc<LocalObjectStore>, max_bytes: u64) -> Self {
        Self {
            root,
            objects,
            max_bytes,
        }
    }

    pub fn scavenge(&self) -> Result<(), StorageError> {
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(StorageError::backend(err.to_string())),
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() && !path.join("lease.json").is_file() {
                let _ = std::fs::remove_dir_all(&path);
            }
        }
        Ok(())
    }

    fn used_bytes(&self) -> Result<u64, StorageError> {
        fn walk(path: &Path) -> std::io::Result<u64> {
            let mut total = 0;
            if path.is_file() {
                return Ok(path.metadata()?.len());
            }
            if path.is_dir() {
                for entry in std::fs::read_dir(path)? {
                    total += walk(&entry?.path())?;
                }
            }
            Ok(total)
        }
        walk(&self.root).map_err(|err| StorageError::backend(err.to_string()))
    }
}

#[async_trait]
impl ScratchStore for LocalScratchStore {
    async fn allocate(&self, request: ScratchRequest) -> Result<ScratchLease, StorageError> {
        if request.purpose.contains('/') || request.purpose.contains("..") {
            return Err(StorageError::invalid_key("scratch purpose"));
        }
        let used = self.used_bytes()?;
        if used.saturating_add(request.max_bytes) > self.max_bytes {
            return Err(StorageError::CapacityExceeded);
        }
        let dir = self.root.join(format!(
            "{}-{}-{}",
            request.scope.principal.as_str(),
            request.scope.workspace.as_str(),
            uuid::Uuid::new_v4().simple()
        ));
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        tokio::fs::write(dir.join("lease.json"), b"{\"active\":true}")
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        Ok(ScratchLease {
            root: dir,
            max_bytes: request.max_bytes,
        })
    }

    async fn materialize_object(
        &self,
        object: &ObjectRef,
        purpose: MaterializationPurpose,
    ) -> Result<MaterializedFile, StorageError> {
        let key = crate::identifiers::StorageKey::decode(&object.encoded_key)?;
        let read = self.objects.get(&key, None).await?;
        let mut bytes = Vec::new();
        let mut body = read.body;
        while let Some(chunk) = body.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        let digest = digest_of(&bytes);
        if digest.hex != object.digest.hex {
            return Err(StorageError::Integrity {
                expected: object.digest.hex.clone(),
                actual: digest.hex,
            });
        }
        let dest = self.root.join("materialized").join(format!(
            "{}.bin",
            blake3::hash(object.encoded_key.as_bytes()).to_hex()
        ));
        if !dest.starts_with(&self.root) {
            return Err(StorageError::invalid_key("materialize escape"));
        }
        if let Some(parent) = dest.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        let mut file = tokio::fs::File::create(&dest)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        file.write_all(&bytes)
            .await
            .map_err(|err| StorageError::backend(err.to_string()))?;
        let _ = purpose;
        Ok(MaterializedFile { path: dest, digest })
    }

    async fn capacity(&self) -> Result<ScratchCapacity, StorageError> {
        Ok(ScratchCapacity {
            used_bytes: self.used_bytes()?,
            max_bytes: self.max_bytes,
        })
    }
}

use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::StorageError;
use crate::secret::{SecretMetadata, SecretPurpose, SecretRef, SecretStore, SecretValue};

use super::paths::join_encoded;

pub struct LocalSecretStore {
    root: PathBuf,
}

impl LocalSecretStore {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path(&self, reference: &SecretRef) -> Result<PathBuf, StorageError> {
        join_encoded(&self.root, reference.as_str())
    }

    pub async fn put(&self, reference: &SecretRef, value: &[u8]) -> Result<(), StorageError> {
        let path = self.path(reference)?;
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        #[cfg(unix)]
        {
            use tokio::io::AsyncWriteExt;
            let tmp = path.with_extension(format!("tmp.{}", uuid::Uuid::new_v4().simple()));
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
            file.write_all(value)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
            file.sync_all()
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
            drop(file);
            tokio::fs::rename(&tmp, &path)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
            if let Some(parent) = path.parent() {
                let dir = std::fs::File::open(parent)
                    .map_err(|err| StorageError::backend(err.to_string()))?;
                dir.sync_all()
                    .map_err(|err| StorageError::backend(err.to_string()))?;
            }
        }
        #[cfg(not(unix))]
        {
            tokio::fs::write(&path, value)
                .await
                .map_err(|err| StorageError::backend(err.to_string()))?;
        }
        Ok(())
    }
}

#[async_trait]
impl SecretStore for LocalSecretStore {
    async fn resolve(
        &self,
        reference: &SecretRef,
        purpose: SecretPurpose,
    ) -> Result<SecretValue, StorageError> {
        let _ = purpose;
        let path = self.path(reference)?;
        let bytes = tokio::fs::read(&path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::backend(err.to_string())
            }
        })?;
        Ok(SecretValue::new(bytes))
    }

    async fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, StorageError> {
        let path = self.path(reference)?;
        let meta = tokio::fs::metadata(&path).await.map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                StorageError::NotFound
            } else {
                StorageError::backend(err.to_string())
            }
        })?;
        Ok(SecretMetadata {
            reference: reference.clone(),
            revision: meta.len().to_string(),
        })
    }
}

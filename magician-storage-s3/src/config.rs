use std::sync::Arc;
use std::time::Duration;

use magician_storage::profile::{ProfileKind, ResolvedStorageProfile};
use magician_storage::secret::{SecretPurpose, SecretStore, SecretValue};
use magician_storage::StorageError;

use crate::backend::BlobStore;
use crate::dataset::S3DatasetStore;
use crate::http::S3HttpBlobStore;
use crate::memory::MemoryBlobStore;
use crate::object::S3ObjectStore;

#[derive(Clone, Debug)]
pub struct RemoteOpenOptions {
    pub allow_http: bool,
    pub timeout: Duration,
    pub max_object_bytes: u64,
}

impl Default for RemoteOpenOptions {
    fn default() -> Self {
        Self {
            allow_http: false,
            timeout: Duration::from_secs(30),
            max_object_bytes: 64 * 1024 * 1024,
        }
    }
}

pub struct RemoteStores {
    pub objects: Arc<S3ObjectStore>,
    pub datasets: Arc<S3DatasetStore>,
}

impl RemoteStores {
    /// Hermetic in-memory S3-semantic backend for tests. Not a network service.
    pub fn hermetic() -> Self {
        let backend: Arc<dyn BlobStore> = Arc::new(MemoryBlobStore::new());
        let objects = Arc::new(S3ObjectStore::new(
            Arc::clone(&backend),
            "production/objects",
            64 * 1024 * 1024,
        ));
        let dataset_objects = Arc::new(S3ObjectStore::new(
            backend,
            "production/datasets",
            64 * 1024 * 1024,
        ));
        Self {
            objects,
            datasets: Arc::new(S3DatasetStore::new(dataset_objects)),
        }
    }
}

/// Construct S3 adapters only from an explicit remote-durable profile.
/// Default Magician startup must not call this.
pub async fn open_from_profile(
    profile: &ResolvedStorageProfile,
    secrets: &dyn SecretStore,
    options: RemoteOpenOptions,
) -> Result<RemoteStores, StorageError> {
    if profile.kind != ProfileKind::RemoteDurable {
        return Err(StorageError::UnsupportedCapability);
    }
    if profile.document.objects.encryption.as_deref() != Some("required") {
        return Err(StorageError::UnsupportedCapability);
    }
    let endpoint = env_or(
        profile.document.objects.endpoint.as_deref(),
        profile.document.objects.endpoint_env.as_deref(),
    )?;
    let region = env_or(None, profile.document.objects.region_env.as_deref())
        .unwrap_or_else(|_| "us-east-1".into());
    let bucket = profile
        .document
        .objects
        .bucket
        .clone()
        .ok_or_else(|| StorageError::invalid_key("objects.bucket"))?;
    let object_prefix = profile.document.objects.prefix.clone().unwrap_or_default();
    let dataset_prefix = profile.document.datasets.prefix.clone().unwrap_or_default();
    let cred_ref = profile
        .document
        .objects
        .credentials_ref
        .as_deref()
        .ok_or_else(|| StorageError::invalid_key("objects.credentials_ref"))?;
    let secret = secrets
        .resolve(
            &magician_storage::SecretRef::parse(cred_ref)?,
            SecretPurpose::ObjectStore,
        )
        .await?;
    let (access, secret_key) = parse_access_secret(&secret)?;
    let http = S3HttpBlobStore::new(crate::http::S3HttpSettings {
        endpoint,
        bucket,
        region,
        access_key: access,
        secret_key,
        path_style: true,
        sse_required: true,
        allow_http: options.allow_http,
        timeout: options.timeout,
    })?;
    let backend: Arc<dyn BlobStore> = Arc::new(http);
    let objects = Arc::new(S3ObjectStore::new(
        Arc::clone(&backend),
        object_prefix,
        options.max_object_bytes,
    ));
    let datasets = Arc::new(S3DatasetStore::new(Arc::new(S3ObjectStore::new(
        backend,
        dataset_prefix,
        options.max_object_bytes,
    ))));
    Ok(RemoteStores { objects, datasets })
}

fn env_or(inline: Option<&str>, env_name: Option<&str>) -> Result<String, StorageError> {
    if let Some(value) = inline {
        return Ok(value.to_string());
    }
    let name = env_name.ok_or_else(|| StorageError::invalid_key("missing endpoint"))?;
    std::env::var(name).map_err(|_| StorageError::invalid_key(name))
}

fn parse_access_secret(secret: &SecretValue) -> Result<(String, String), StorageError> {
    let raw = std::str::from_utf8(secret.expose())
        .map_err(|_| StorageError::invalid_key("credential utf8"))?;
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) {
        let access = value
            .get("access_key_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| StorageError::invalid_key("access_key_id"))?;
        let secret_key = value
            .get("secret_access_key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| StorageError::invalid_key("secret_access_key"))?;
        return Ok((access.to_string(), secret_key.to_string()));
    }
    let (access, secret_key) = raw
        .split_once(':')
        .ok_or_else(|| StorageError::invalid_key("credential shape"))?;
    Ok((access.to_string(), secret_key.to_string()))
}

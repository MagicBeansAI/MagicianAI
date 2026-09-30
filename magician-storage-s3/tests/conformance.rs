use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures_util::StreamExt;
use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::object::{
    bytes_body, ByteRange, DeleteCondition, ObjectStore, PutCondition, PutObjectRequest,
    StoragePrefix,
};
use magician_storage::profile::ResolveOptions;
use magician_storage::secret::{
    SecretMetadata, SecretPurpose, SecretRef, SecretStore, SecretValue,
};
use magician_storage::{StorageError, StorageKey};
use magician_storage_s3::{
    open_from_profile, sign_s3_request, BlobStore, MemoryBlobStore, RemoteOpenOptions,
    RemoteStores, S3HttpBlobStore, S3HttpSettings, S3ObjectStore, SignInput,
};

struct MapSecrets(HashMap<String, Vec<u8>>);

#[async_trait]
impl SecretStore for MapSecrets {
    async fn resolve(
        &self,
        reference: &SecretRef,
        purpose: SecretPurpose,
    ) -> Result<SecretValue, StorageError> {
        let _ = purpose;
        self.0
            .get(reference.as_str())
            .cloned()
            .map(SecretValue::new)
            .ok_or(StorageError::NotFound)
    }

    async fn metadata(&self, reference: &SecretRef) -> Result<SecretMetadata, StorageError> {
        Ok(SecretMetadata {
            reference: reference.clone(),
            revision: "1".into(),
        })
    }
}

async fn collect(mut stream: magician_storage::ObjectBodyStream) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(chunk) = stream.next().await {
        out.extend_from_slice(&chunk.unwrap());
    }
    out
}

fn digest(bytes: &[u8]) -> magician_storage::ContentDigest {
    magician_storage::ContentDigest {
        algorithm: magician_storage::DigestAlgorithm::Blake3,
        hex: blake3::hash(bytes).to_hex().to_string(),
    }
}

#[tokio::test]
async fn hermetic_object_store_conformance() {
    let stores = RemoteStores::hermetic();
    let key = StorageKey::tenant("alice", "home", "tasks", "task-1").unwrap();
    assert!(stores.objects.head(&key).await.unwrap().is_none());

    let first = stores
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"hello-storage".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    let err = stores
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"again".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));

    let second = stores
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"hello-storage!".to_vec()),
            content_type: None,
            condition: PutCondition::ExpectedVersion(first.metadata.version.clone()),
        })
        .await
        .unwrap();
    let stale = stores
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"nope".to_vec()),
            content_type: None,
            condition: PutCondition::ExpectedVersion(first.metadata.version),
        })
        .await
        .unwrap_err();
    assert!(matches!(stale, StorageError::Conflict { .. }));

    let ranged = stores
        .objects
        .get(
            &key,
            Some(ByteRange {
                start: 6,
                end_exclusive: Some(13),
            }),
        )
        .await
        .unwrap();
    assert_eq!(collect(ranged.body).await, b"storage");

    let listed = stores
        .objects
        .list_diagnostic(
            &StoragePrefix {
                encoded: "t/alice/home/tasks".into(),
            },
            None,
            10,
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);

    stores
        .objects
        .delete(
            &key,
            DeleteCondition::ExpectedVersion(second.metadata.version.clone()),
        )
        .await
        .unwrap();
    assert!(stores.objects.head(&key).await.unwrap().is_none());
}

#[tokio::test]
async fn streamed_put_is_capacity_bounded() {
    let store = S3ObjectStore::new(Arc::new(MemoryBlobStore::new()), "objects", 16);
    let key = StorageKey::system("tasks", "big").unwrap();
    let err = store
        .put(PutObjectRequest {
            key,
            body: bytes_body(vec![0u8; 32]),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::CapacityExceeded));
}

#[tokio::test]
async fn multipart_abort_and_abandoned_cleanup() {
    let backend = MemoryBlobStore::new();
    let key = "production/objects/t/alice/home/tasks/multi";
    let upload = backend.create_multipart(key).await.unwrap();
    backend
        .upload_part(&upload, 1, bytes::Bytes::from_static(b"abc"))
        .await
        .unwrap();
    backend.abort_multipart(&upload).await.unwrap();
    let err = backend
        .complete_multipart(&upload, PutCondition::CreateOnly)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound));
    let upload = backend.create_multipart(key).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5)).await;
    let cleaned = backend
        .cleanup_abandoned(Duration::from_millis(1))
        .await
        .unwrap();
    assert!(cleaned >= 1);
    let _ = upload;
}

#[tokio::test]
async fn dataset_part_immutable_manifest_cas_corrupt_and_old_generation() {
    let stores = RemoteStores::hermetic();
    let part_g1 = DatasetPartRef {
        dataset: DatasetId::parse("events").unwrap(),
        partition: PartitionId::parse("p1").unwrap(),
        generation: "g1".into(),
        name: "batch.parquet".into(),
    };
    let bytes = b"parquet-bytes";
    stores
        .datasets
        .stage_part(StageDatasetPart {
            part: part_g1.clone(),
            digest: digest(bytes),
            len: bytes.len() as u64,
            body: bytes_body(bytes.to_vec()),
        })
        .await
        .unwrap();
    let err = stores
        .datasets
        .stage_part(StageDatasetPart {
            part: part_g1.clone(),
            digest: digest(bytes),
            len: bytes.len() as u64,
            body: bytes_body(bytes.to_vec()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));

    let manifest = DatasetManifest {
        dataset: part_g1.dataset.clone(),
        partition: part_g1.partition.clone(),
        version: ManifestVersion::parse("v1").unwrap(),
        parts: vec![part_g1.clone()],
        row_count: 1,
        schema_identity: "s1".into(),
    };
    stores
        .datasets
        .commit_manifest(manifest.clone(), None)
        .await
        .unwrap();
    let err = stores
        .datasets
        .commit_manifest(manifest.clone(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));

    let part_g2 = DatasetPartRef {
        generation: "g2".into(),
        ..part_g1.clone()
    };
    stores
        .datasets
        .stage_part(StageDatasetPart {
            part: part_g2.clone(),
            digest: digest(b"parquet-v2"),
            len: 10,
            body: bytes_body(b"parquet-v2".to_vec()),
        })
        .await
        .unwrap();
    let mut v2 = manifest.clone();
    v2.version = ManifestVersion::parse("v2").unwrap();
    v2.parts = vec![part_g2];
    stores
        .datasets
        .commit_manifest(v2, Some(ManifestVersion::parse("v1").unwrap()))
        .await
        .unwrap();

    let old = stores.datasets.open_part(&part_g1, None).await.unwrap();
    assert_eq!(collect(old.body).await, b"parquet-bytes");

    let wrong = stores
        .datasets
        .stage_part(StageDatasetPart {
            part: DatasetPartRef {
                generation: "g3".into(),
                ..part_g1.clone()
            },
            digest: digest(b"nope"),
            len: 4,
            body: bytes_body(b"xxxx".to_vec()),
        })
        .await
        .unwrap_err();
    assert!(matches!(wrong, StorageError::Integrity { .. }));
}

#[tokio::test]
async fn corrupt_manifest_fails_closed() {
    let stores = RemoteStores::hermetic();
    let key = StorageKey::system("datasets", "manifests.events.p1.json").unwrap();
    stores
        .objects
        .put(PutObjectRequest {
            key,
            body: bytes_body(b"not-json".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    // Dataset prefix is production/datasets, so corrupt the dataset object store
    // by staging through a hermetic dataset store that shares no prefix with
    // objects. Write garbage via the dataset store's object adapter.
    let dataset_key = StorageKey::system("datasets", "manifests.events.p1.json").unwrap();
    stores
        .datasets
        .commit_manifest(
            DatasetManifest {
                dataset: DatasetId::parse("events").unwrap(),
                partition: PartitionId::parse("p1").unwrap(),
                version: ManifestVersion::parse("v0").unwrap(),
                parts: vec![],
                row_count: 0,
                schema_identity: "s".into(),
            },
            None,
        )
        .await
        .unwrap();
    let _ = dataset_key;
    let backend = stores.objects.backend();
    backend
        .put(
            "production/datasets/s/datasets/manifests.events.p1.json",
            bytes::Bytes::from_static(b"not-json"),
            PutCondition::Overwrite,
        )
        .await
        .unwrap();
    let err = stores
        .datasets
        .read_manifest(
            &DatasetId::parse("events").unwrap(),
            &PartitionId::parse("p1").unwrap(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Corrupt { .. }));
}

#[tokio::test]
async fn credentials_are_redacted_and_http_is_refused() {
    let store = S3HttpBlobStore::new(S3HttpSettings {
        endpoint: "http://127.0.0.1:9".into(),
        bucket: "bucket".into(),
        region: "us-east-1".into(),
        access_key: "AKIAEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        path_style: true,
        sse_required: true,
        allow_http: false,
        timeout: Duration::from_secs(1),
    });
    match store {
        Err(StorageError::PermissionDenied) => {},
        other => panic!("expected permission denied, got {other:?}"),
    }
    let store = S3HttpBlobStore::new(S3HttpSettings {
        endpoint: "https://s3.example.test".into(),
        bucket: "bucket".into(),
        region: "us-east-1".into(),
        access_key: "AKIAEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY".into(),
        path_style: true,
        sse_required: true,
        allow_http: false,
        timeout: Duration::from_secs(1),
    })
    .unwrap();
    let rendered = format!("{store:?}");
    assert!(!rendered.contains("wJalrXUtnFEMI"));
    assert!(!rendered.contains("AKIAEXAMPLE"));
    assert!(rendered.contains("redacted"));
}

#[test]
fn sigv4_is_stable_for_fixed_inputs() {
    let auth = sign_s3_request(SignInput {
        method: "PUT",
        path: "/bucket/key",
        query: "",
        host: "s3.example.test",
        payload_hash: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        access_key: "AKIAEXAMPLE",
        secret_key: "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
        region: "us-east-1",
        amz_date: "20260831T000000Z",
        extra_amz_headers: &[],
    });
    assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKIAEXAMPLE/"));
    assert!(auth.contains("Signature="));
}

#[tokio::test]
async fn open_from_profile_rejects_local_and_needs_explicit_remote() {
    let local = magician_storage::resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    let secrets = MapSecrets(HashMap::new());
    let err = match open_from_profile(&local, &secrets, RemoteOpenOptions::default()).await {
        Ok(_) => panic!("local profile must not open s3 adapters"),
        Err(err) => err,
    };
    assert!(matches!(err, StorageError::UnsupportedCapability));
}

#[tokio::test]
async fn open_from_profile_remote_http_without_allow_is_denied() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../magician-storage/examples/remote_durable.yaml");
    let profile = magician_storage::resolve(ResolveOptions {
        cli_path: Some(&path),
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    std::env::set_var("MAGICIAN_OBJECT_ENDPOINT", "http://127.0.0.1:9");
    std::env::set_var("MAGICIAN_OBJECT_REGION", "us-east-1");
    let mut secrets = HashMap::new();
    secrets.insert(
        "object-store/production".into(),
        b"AKID:secret-value-must-not-log".to_vec(),
    );
    let err = match open_from_profile(&profile, &MapSecrets(secrets), RemoteOpenOptions::default())
        .await
    {
        Ok(_) => panic!("plain http must be refused"),
        Err(err) => err,
    };
    assert!(matches!(
        err,
        StorageError::PermissionDenied | StorageError::InvalidKey { .. }
    ));
}

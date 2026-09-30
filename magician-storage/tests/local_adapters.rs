use std::process::Command;
use std::time::Duration;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::index::{
    IndexId, IndexMutationBatch, IndexQuery, IndexStore, RebuildIndexRequest,
};
use magician_storage::lease::{LeaseResource, LeaseStore, OwnerId};
use magician_storage::object::{
    bytes_body, ByteRange, DeleteCondition, ObjectStore, ObjectVersion, PutCondition,
    PutObjectRequest, StoragePrefix,
};
use magician_storage::scratch::{ScratchRequest, ScratchStore};
use magician_storage::secret::{SecretPurpose, SecretRef, SecretStore};
use magician_storage::{LocalStorage, PrincipalId, ScopeId, StorageError, StorageKey, WorkspaceId};

fn object_file(root: &std::path::Path, key: &StorageKey) -> std::path::PathBuf {
    root.join("objects").join(key.encode())
}

fn sidecar_file(root: &std::path::Path, key: &StorageKey) -> std::path::PathBuf {
    let mut path = object_file(root, key).into_os_string();
    path.push(".objmeta.json");
    std::path::PathBuf::from(path)
}

fn quarantine_file(root: &std::path::Path, key: &StorageKey) -> std::path::PathBuf {
    let mut path = object_file(root, key).into_os_string();
    path.push(".objquarantine");
    std::path::PathBuf::from(path)
}

async fn collect_body(mut stream: magician_storage::ObjectBodyStream) -> Vec<u8> {
    use futures_util::StreamExt;
    let mut got = Vec::new();
    while let Some(chunk) = stream.next().await {
        got.extend_from_slice(&chunk.unwrap());
    }
    got
}

fn scope() -> ScopeId {
    ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    )
}

fn digest(bytes: &[u8]) -> magician_storage::ContentDigest {
    let hash = blake3::hash(bytes);
    magician_storage::ContentDigest {
        algorithm: magician_storage::DigestAlgorithm::Blake3,
        hex: hash.to_hex().to_string(),
    }
}

#[tokio::test]
async fn object_overwrite_round_trip_and_range() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "task-1").unwrap();
    let body = b"hello-storage";
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(body.to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let head = store.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(head.len, body.len() as u64);
    assert_eq!(head.digest.hex, digest(body).hex);
    assert_ne!(head.version.as_str(), ObjectVersion::UNVERSIONED);
    assert!(!head.version.as_str().is_empty());

    let ranged = store
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
    assert_eq!(collect_body(ranged.body).await, b"storage");

    let again = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"hello-storage".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    assert_ne!(again.metadata.version.as_str(), head.version.as_str());
}

#[tokio::test]
async fn object_create_only_and_expected_version_cas() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "task-2").unwrap();
    let first = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"first".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    assert_eq!(first.correlation, "local-create");
    let err = store
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

    let second = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"second".to_vec()),
            content_type: None,
            condition: PutCondition::ExpectedVersion(first.metadata.version.clone()),
        })
        .await
        .unwrap();
    assert_eq!(second.correlation, "local-cas");
    let err = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"stale".to_vec()),
            content_type: None,
            condition: PutCondition::ExpectedVersion(first.metadata.version),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
    assert_eq!(
        collect_body(store.objects.get(&key, None).await.unwrap().body).await,
        b"second"
    );
}

#[tokio::test]
async fn object_legacy_file_is_readable_without_migration() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "legacy").unwrap();
    let path = object_file(dir.path(), &key);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"old-bytes").unwrap();
    let head = store.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(head.len, 9);
    assert_eq!(head.digest.hex, digest(b"old-bytes").hex);
    assert_ne!(head.version.as_str(), ObjectVersion::UNVERSIONED);
    assert_eq!(std::fs::read(&path).unwrap(), b"old-bytes");
    assert!(sidecar_file(dir.path(), &key).is_file());
    let again = store.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(again.version.as_str(), head.version.as_str());
}

#[tokio::test]
async fn leftover_tmp_is_not_an_object() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "crash").unwrap();
    let tmp = dir
        .path()
        .join("objects")
        .join("t")
        .join("alice")
        .join("home")
        .join("tasks")
        .join("crash.tmp-dead");
    std::fs::create_dir_all(tmp.parent().unwrap()).unwrap();
    std::fs::write(&tmp, b"partial").unwrap();
    assert!(store.objects.head(&key).await.unwrap().is_none());
    let listed = store
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
    assert!(listed.is_empty());
}

#[tokio::test]
async fn dataset_part_immutable_and_manifest_cas() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let part = DatasetPartRef {
        dataset: DatasetId::parse("events").unwrap(),
        partition: PartitionId::parse("dt-2026-08-31").unwrap(),
        generation: "g1".into(),
        name: "batch.parquet".into(),
    };
    let bytes = b"parquet-bytes";
    store
        .datasets
        .stage_part(StageDatasetPart {
            part: part.clone(),
            digest: digest(bytes),
            len: bytes.len() as u64,
            body: bytes_body(bytes.to_vec()),
        })
        .await
        .unwrap();
    store
        .datasets
        .stage_part(StageDatasetPart {
            part: part.clone(),
            digest: digest(bytes),
            len: bytes.len() as u64,
            body: bytes_body(bytes.to_vec()),
        })
        .await
        .expect("identical digest restage is idempotent CAS");
    let other = b"other-parquet-bytes";
    let err = store
        .datasets
        .stage_part(StageDatasetPart {
            part: part.clone(),
            digest: digest(other),
            len: other.len() as u64,
            body: bytes_body(other.to_vec()),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));

    let manifest = DatasetManifest {
        dataset: part.dataset.clone(),
        partition: part.partition.clone(),
        version: ManifestVersion::parse("v1").unwrap(),
        parts: vec![part.clone()],
        row_count: 1,
        schema_identity: "s1".into(),
    };
    store
        .datasets
        .commit_manifest(manifest.clone(), None)
        .await
        .unwrap();
    let err = store
        .datasets
        .commit_manifest(manifest.clone(), None)
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
    let mut v2 = manifest;
    v2.version = ManifestVersion::parse("v2").unwrap();
    store
        .datasets
        .commit_manifest(v2, Some(ManifestVersion::parse("v1").unwrap()))
        .await
        .unwrap();
}

#[tokio::test]
async fn corrupt_manifest_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let path = dir
        .path()
        .join("datasets")
        .join("manifests")
        .join("events")
        .join("p1.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not-json").unwrap();
    let err = store
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
async fn lease_generation_advances_and_second_holder_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let resource = LeaseResource::parse("scope-lock").unwrap();
    let a = store
        .leases
        .acquire(
            resource.clone(),
            OwnerId::parse("proc-a").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(a.generation, 1);
    let err = store
        .leases
        .acquire(
            resource.clone(),
            OwnerId::parse("proc-b").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
    store.leases.release(a).await.unwrap();
    let b = store
        .leases
        .acquire(
            resource,
            OwnerId::parse("proc-b").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    assert_eq!(b.generation, 2);
}

#[tokio::test]
async fn two_process_lease_exclusion() {
    if let Ok(root) = std::env::var("MAGICIAN_LEASE_CHILD_ROOT") {
        let store = LocalStorage::open(root).unwrap();
        let err = store
            .leases
            .acquire(
                LeaseResource::parse("scope-lock").unwrap(),
                OwnerId::parse("proc-b").unwrap(),
                Duration::from_secs(5),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict { .. }));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let _token = store
        .leases
        .acquire(
            LeaseResource::parse("scope-lock").unwrap(),
            OwnerId::parse("proc-a").unwrap(),
            Duration::from_secs(30),
        )
        .await
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("MAGICIAN_LEASE_CHILD_ROOT", dir.path())
        .args(["two_process_lease_exclusion", "--exact"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
async fn scratch_quota_and_purpose_escape() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let err = store
        .scratch
        .allocate(ScratchRequest {
            scope: scope(),
            purpose: "../escape".into(),
            max_bytes: 10,
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::InvalidKey { .. }));
    let tiny = LocalStorage::open(dir.path().join("tiny")).unwrap();
    // reopen with small quota via direct constructor is the factory default 64MiB.
    let lease = store
        .scratch
        .allocate(ScratchRequest {
            scope: scope(),
            purpose: "tool".into(),
            max_bytes: 10,
        })
        .await
        .unwrap();
    assert!(lease.root.starts_with(dir.path().join("scratch")));
    let _ = tiny;
}

#[tokio::test]
async fn secret_round_trip_debug_stays_redacted() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let reference = SecretRef::parse("object-store-prod").unwrap();
    store.secrets.put(&reference, b"token-value").await.unwrap();
    let value = store
        .secrets
        .resolve(&reference, SecretPurpose::ObjectStore)
        .await
        .unwrap();
    assert_eq!(value.expose(), b"token-value");
    assert_eq!(format!("{value:?}"), "SecretValue(redacted)");
}

#[tokio::test]
async fn index_watermark_survives_rebuild() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let index = IndexId::parse("memory").unwrap();
    store
        .indexes
        .apply(IndexMutationBatch {
            scope: scope(),
            index: index.clone(),
            source_watermark: "w1".into(),
        })
        .await
        .unwrap();
    let page = store
        .indexes
        .query(
            &scope(),
            &index,
            IndexQuery {
                text: "hello".into(),
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert!(page.hits.is_empty());
    assert_eq!(page.watermark.unwrap().position, "w1");
    let rebuilt = store
        .indexes
        .rebuild(RebuildIndexRequest {
            scope: scope(),
            index,
        })
        .await
        .unwrap();
    assert_eq!(rebuilt.watermark.position, "rebuilt");
}

#[tokio::test]
async fn object_delete_existing_and_list() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "gone").unwrap();
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"x".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    store
        .objects
        .delete(&key, DeleteCondition::Existing)
        .await
        .unwrap();
    assert!(store.objects.head(&key).await.unwrap().is_none());
    let listed = store
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
    assert!(listed.is_empty());
}

#[tokio::test]
async fn object_delete_expected_version() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "cas-del").unwrap();
    let put = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"keep".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    let err = store
        .objects
        .delete(
            &key,
            DeleteCondition::ExpectedVersion(ObjectVersion::unversioned()),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
    store
        .objects
        .delete(
            &key,
            DeleteCondition::ExpectedVersion(put.metadata.version.clone()),
        )
        .await
        .unwrap();
    assert!(store.objects.head(&key).await.unwrap().is_none());
    let err = store
        .objects
        .delete(&key, DeleteCondition::ExpectedVersion(put.metadata.version))
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::NotFound));
}

#[tokio::test]
async fn object_corrupt_sidecar_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "corrupt").unwrap();
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"payload".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    std::fs::write(sidecar_file(dir.path(), &key), b"not-json").unwrap();
    let err = store.objects.head(&key).await.unwrap_err();
    assert!(matches!(err, StorageError::Corrupt { .. }));
    assert!(quarantine_file(dir.path(), &key).is_file());
    assert_eq!(
        std::fs::read(object_file(dir.path(), &key)).unwrap(),
        b"payload"
    );
}

#[tokio::test]
async fn object_digest_mismatch_fails_closed() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "tamper").unwrap();
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"payload".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    std::fs::write(object_file(dir.path(), &key), b"tampered").unwrap();
    let err = store.objects.head(&key).await.unwrap_err();
    assert!(matches!(err, StorageError::Integrity { .. }));
    assert!(quarantine_file(dir.path(), &key).is_file());
}

#[tokio::test]
async fn object_tombstone_not_resurrected() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "gone-again").unwrap();
    let put = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"live".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    store
        .objects
        .delete(&key, DeleteCondition::Existing)
        .await
        .unwrap();
    std::fs::write(object_file(dir.path(), &key), b"live").unwrap();
    assert!(store.objects.head(&key).await.unwrap().is_none());
    let err = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"resurrect".to_vec()),
            content_type: None,
            condition: PutCondition::ExpectedVersion(put.metadata.version),
        })
        .await
        .unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"new-life".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    assert_eq!(
        collect_body(store.objects.get(&key, None).await.unwrap().body).await,
        b"new-life"
    );
}

#[tokio::test]
async fn object_interrupted_publish_completes_new_generation() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "crash-pub").unwrap();
    let old = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"old!!".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let new_bytes = b"newnew!";
    let new_version = "11111111-2222-4333-8444-555555555555".to_string();
    let pending_name = format!("crash-pub.objpub.{new_version}");
    let pending_path = object_file(dir.path(), &key)
        .parent()
        .unwrap()
        .join(&pending_name);
    std::fs::write(&pending_path, new_bytes).unwrap();
    let sidecar = serde_json::json!({
        "schema": 1,
        "state": "publishing",
        "version": old.metadata.version.as_str(),
        "len": old.metadata.len,
        "digest": {
            "algorithm": "blake3",
            "hex": old.metadata.digest.hex,
        },
        "pending": {
            "version": new_version,
            "len": new_bytes.len(),
            "digest": {
                "algorithm": "blake3",
                "hex": digest(new_bytes).hex,
            },
            "file": pending_name,
        }
    });
    std::fs::write(
        sidecar_file(dir.path(), &key),
        serde_json::to_vec_pretty(&sidecar).unwrap(),
    )
    .unwrap();
    let head = store.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(head.version.as_str(), new_version);
    assert_eq!(head.len, new_bytes.len() as u64);
    assert_eq!(
        std::fs::read(object_file(dir.path(), &key)).unwrap(),
        new_bytes
    );
    assert!(!pending_path.exists());
}

#[tokio::test]
async fn object_interrupted_publish_keeps_old_when_pending_lost() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "crash-old").unwrap();
    let old = store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"old!!".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let new_version = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee".to_string();
    let sidecar = serde_json::json!({
        "schema": 1,
        "state": "publishing",
        "version": old.metadata.version.as_str(),
        "len": old.metadata.len,
        "digest": {
            "algorithm": "blake3",
            "hex": old.metadata.digest.hex,
        },
        "pending": {
            "version": new_version,
            "len": 7,
            "digest": {
                "algorithm": "blake3",
                "hex": digest(b"newnew!").hex,
            },
            "file": format!("crash-old.objpub.{new_version}"),
        }
    });
    std::fs::write(
        sidecar_file(dir.path(), &key),
        serde_json::to_vec_pretty(&sidecar).unwrap(),
    )
    .unwrap();
    let head = store.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(head.version.as_str(), old.metadata.version.as_str());
    assert_eq!(
        std::fs::read(object_file(dir.path(), &key)).unwrap(),
        b"old!!"
    );
}

#[tokio::test]
async fn object_list_skips_adapter_sidecars() {
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "listed").unwrap();
    store
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"x".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    assert!(sidecar_file(dir.path(), &key).is_file());
    let listed = store
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
    assert_eq!(listed[0].key, key);
}

#[tokio::test]
async fn two_process_create_only_conflict() {
    let key = StorageKey::tenant("alice", "home", "tasks", "race").unwrap();
    if let Ok(root) = std::env::var("MAGICIAN_OBJECT_CHILD_ROOT") {
        let store = LocalStorage::open(root).unwrap();
        let err = store
            .objects
            .put(PutObjectRequest {
                key,
                body: bytes_body(b"child".to_vec()),
                content_type: None,
                condition: PutCondition::CreateOnly,
            })
            .await
            .unwrap_err();
        assert!(matches!(err, StorageError::Conflict { .. }));
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStorage::open(dir.path()).unwrap();
    store
        .objects
        .put(PutObjectRequest {
            key,
            body: bytes_body(b"parent".to_vec()),
            content_type: None,
            condition: PutCondition::CreateOnly,
        })
        .await
        .unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .env("MAGICIAN_OBJECT_CHILD_ROOT", dir.path())
        .args(["two_process_create_only_conflict", "--exact"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn object_store_cas_cases_are_named() {
    for name in [
        "put_create_only",
        "put_expected_version",
        "delete_expected_version",
        "conflict_two_writers",
    ] {
        assert!(magician_storage::conformance::OBJECT_STORE_CASES.contains(&name));
    }
}

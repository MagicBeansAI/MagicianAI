use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::fs::LocalStorage;
use magician_storage::object::{
    bytes_body, DeleteCondition, ObjectStore, PutCondition, PutObjectRequest,
};
use magician_storage::{GcPolicy, ObjectReference, StorageKey};

#[tokio::test]
async fn delayed_gc_keeps_referenced_and_replacement_objects() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "payload").unwrap();
    let put = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"old".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    storage
        .objects
        .delete(&key, DeleteCondition::Existing)
        .await
        .unwrap();
    assert!(storage.objects.head(&key).await.unwrap().is_none());
    let retained = storage
        .objects
        .retained_generation(&key)
        .unwrap()
        .expect("deleted bytes stay in the restore window");
    assert_eq!(retained.0.as_str(), put.metadata.version.as_str());
    assert_eq!(retained.1, b"old");

    let referenced = [ObjectReference {
        key: key.clone(),
        version: put.metadata.version.clone(),
    }];
    let kept = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &referenced, i64::MAX)
        .unwrap();
    assert_eq!(kept.retained_referenced, 1);
    assert_eq!(kept.collected, 0);
    assert!(storage.objects.retained_generation(&key).unwrap().is_some());

    let replacement = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"new".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    assert_ne!(
        replacement.metadata.version.as_str(),
        put.metadata.version.as_str()
    );
    let _second = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"newer".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let retained_after_live_puts = storage
        .objects
        .retained_generation(&key)
        .unwrap()
        .expect("retain metadata stays on live replacements");
    assert_eq!(
        retained_after_live_puts.0.as_str(),
        put.metadata.version.as_str()
    );
    assert_eq!(retained_after_live_puts.1, b"old");
    let still_referenced = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &referenced, i64::MAX)
        .unwrap();
    assert_eq!(still_referenced.retained_referenced, 1);
    assert_eq!(still_referenced.collected, 0);
    let unreferenced = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &[], i64::MAX)
        .unwrap();
    assert_eq!(unreferenced.collected, 1);
    assert_eq!(
        storage
            .objects
            .get(&key, None)
            .await
            .unwrap()
            .metadata
            .version,
        _second.metadata.version
    );
}

#[tokio::test]
async fn delayed_gc_collects_unreferenced_retain_after_window() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "gone").unwrap();
    storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"x".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    storage
        .objects
        .delete(&key, DeleteCondition::Existing)
        .await
        .unwrap();
    let fresh = storage
        .objects
        .collect_unreferenced(
            GcPolicy {
                tombstone_retain: std::time::Duration::from_secs(60),
            },
            &[],
            1,
        )
        .unwrap();
    assert_eq!(fresh.collected, 0);
    let collected = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &[], i64::MAX)
        .unwrap();
    assert_eq!(collected.collected, 1);
    assert!(storage.objects.retained_generation(&key).unwrap().is_none());
}

#[tokio::test]
async fn dataset_gc_drops_unreferenced_generations_only() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let dataset = DatasetId::parse("events").unwrap();
    let partition = PartitionId::parse("dt-1").unwrap();
    let live = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g1".into(),
        name: "part.bin".into(),
    };
    let stale = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g0".into(),
        name: "part.bin".into(),
    };
    for part in [&live, &stale] {
        storage
            .datasets
            .stage_part(StageDatasetPart {
                part: part.clone(),
                digest: magician_storage::ContentDigest {
                    algorithm: magician_storage::DigestAlgorithm::Blake3,
                    hex: blake3::hash(b"row").to_hex().to_string(),
                },
                len: 3,
                body: bytes_body(b"row".to_vec()),
            })
            .await
            .unwrap();
    }
    storage
        .datasets
        .commit_manifest(
            DatasetManifest {
                dataset: dataset.clone(),
                partition: partition.clone(),
                version: ManifestVersion::parse("v1").unwrap(),
                parts: vec![live.clone()],
                row_count: 1,
                schema_identity: "s".into(),
            },
            None,
        )
        .await
        .unwrap();
    let report = storage.datasets.collect_unreferenced_generations().unwrap();
    assert_eq!(report.collected, 1);
    assert_eq!(report.retained_referenced, 1);
    assert!(storage.datasets.open_part(&live, None).await.is_ok());
    assert!(storage.datasets.open_part(&stale, None).await.is_err());
}

use std::time::Instant;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::fs::LocalStorage;
use magician_storage::index::{IndexId, IndexMutationBatch, IndexStore, RebuildIndexRequest};
use magician_storage::object::{bytes_body, ObjectStore, PutCondition, PutObjectRequest};
use magician_storage::{
    assert_budget, assert_latency_budget, percentile_ms, BudgetLine, ContentDigest,
    DigestAlgorithm, PrincipalId, ScopeId, StorageKey, WorkspaceId,
};

#[tokio::test]
async fn local_adapters_meet_object_dataset_index_budgets() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let mut complete = Vec::new();
    let mut first = Vec::new();
    for _ in 0..8 {
        let started = Instant::now();
        storage
            .objects
            .put(PutObjectRequest {
                key: key.clone(),
                body: bytes_body(vec![b'a'; 64 * 1024]),
                content_type: None,
                condition: PutCondition::Overwrite,
            })
            .await
            .unwrap();
        complete.push(started.elapsed().as_nanos());
        let started = Instant::now();
        let _ = storage.objects.get(&key, None).await.unwrap();
        first.push(started.elapsed().as_nanos());
    }
    assert_latency_budget(
        BudgetLine::ObjectSmallCompleteLocalP95Ms,
        percentile_ms(&complete, 95),
    );
    assert_latency_budget(
        BudgetLine::ObjectSmallFirstByteP95Ms,
        percentile_ms(&first, 95),
    );

    let dataset = DatasetId::parse("events").unwrap();
    let partition = PartitionId::parse("p1").unwrap();
    let part = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g1".into(),
        name: "rows.bin".into(),
    };
    let started = Instant::now();
    storage
        .datasets
        .stage_part(StageDatasetPart {
            part: part.clone(),
            digest: ContentDigest {
                algorithm: DigestAlgorithm::Blake3,
                hex: blake3::hash(b"row").to_hex().to_string(),
            },
            len: 3,
            body: bytes_body(b"row".to_vec()),
        })
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::ParquetFlushP95Ms,
        started.elapsed().as_millis() as u64,
    );
    let started = Instant::now();
    storage
        .datasets
        .commit_manifest(
            DatasetManifest {
                dataset,
                partition,
                version: ManifestVersion::parse("v1").unwrap(),
                parts: vec![part.clone()],
                row_count: 1,
                schema_identity: "s".into(),
            },
            None,
        )
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::ManifestCommitP95Ms,
        started.elapsed().as_millis() as u64,
    );
    let started = Instant::now();
    storage.datasets.open_part(&part, None).await.unwrap();
    assert_latency_budget(
        BudgetLine::DatasetQueryP95Ms,
        started.elapsed().as_millis() as u64,
    );

    let scope = ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    );
    storage
        .indexes
        .apply(IndexMutationBatch {
            scope: scope.clone(),
            index: IndexId::parse("memory").unwrap(),
            source_watermark: "src".into(),
        })
        .await
        .unwrap();
    let started = Instant::now();
    storage
        .indexes
        .rebuild(RebuildIndexRequest {
            scope,
            index: IndexId::parse("memory").unwrap(),
        })
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::IndexRebuildP95Ms,
        started.elapsed().as_millis() as u64,
    );
    assert_budget(BudgetLine::ScratchQuotaBytes, 64 * 1024 * 1024);
    assert_budget(BudgetLine::LeaseTtlSecs, 24 * 60 * 60);
}

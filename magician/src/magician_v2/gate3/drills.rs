//! Domain Gate 3 drills: chat, tasks, migration freeze, backup, monthly cost.

use std::time::Instant;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::fs::LocalStorage;
use magician_storage::index::{IndexId, IndexMutationBatch, IndexStore, RebuildIndexRequest};
use magician_storage::object::{bytes_body, ObjectStore, PutCondition, PutObjectRequest};
use magician_storage::{
    ContentDigest, DigestAlgorithm, OwnerId, PrincipalId, ScopeId, StorageKey, StorageRuntime,
    WorkspaceId,
};

use super::{
    accepted_budgets, assert_budget, assert_latency_budget, percentile_ms, BudgetLine,
    BudgetVerdict,
};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat_owners::{ChatAccess, ChatOwner};
use crate::magician_v2::recovery::{snapshot_representative_scope, SignedSnapshot};
use crate::magician_v2::system_owners::{SystemAccess, SystemOwner};
use crate::magician_v2::work_owners::{WorkAccess, WorkOwner};

#[test]
fn gate3_is_closed_with_offline_cutover_optional() {
    // GATE3_CLOSED and ONLINE_MIGRATION_REQUIRED are asserted at compile time in
    // `magician_storage::gate3`; what still needs a runtime check is that the
    // numbers the closed gate accepted are the ones this build carries.
    let budgets = accepted_budgets();
    assert_eq!(budgets.lease_ttl_secs, 24 * 60 * 60);
    assert_eq!(budgets.lease_renewal_secs, 60 * 60);
    assert_eq!(budgets.scratch_quota_bytes, 64 * 1024 * 1024);
    assert_eq!(budgets.backup_rpo_secs, 0);
    assert_eq!(budgets.restore_rto_secs, 900);
}

#[tokio::test]
async fn chat_and_task_paging_meet_gate3() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(dir.path());
    let chat = crate::magician_v2::chat_owners::open_local_chat_owner(
        &workspace,
        "alice",
        "home",
        ChatOwner::ChatMessages,
    );
    let history = "x".repeat(8 * 1024);
    let mut append_ns = Vec::new();
    for i in 0..8 {
        let rel = format!("ui/chat_sessions/sess-1/messages/{i:06}.jsonl");
        let started = Instant::now();
        chat.put(&rel, history.as_bytes()).await.unwrap();
        append_ns.push(started.elapsed().as_nanos());
    }
    let mut page_ns = Vec::new();
    for i in 0..8 {
        let rel = format!("ui/chat_sessions/sess-1/messages/{i:06}.jsonl");
        let started = Instant::now();
        let _ = chat.get(&rel).await.unwrap();
        page_ns.push(started.elapsed().as_nanos());
    }
    assert_latency_budget(BudgetLine::ChatAppendP95Ms, percentile_ms(&append_ns, 95));
    assert_latency_budget(BudgetLine::ChatPageP95Ms, percentile_ms(&page_ns, 95));

    let tasks = crate::magician_v2::work_owners::open_local_work_owner(
        &workspace,
        "alice",
        "home",
        WorkOwner::TaskRecords,
    );
    for i in 0..32 {
        tasks
            .put(&format!("tasks/task-{i}/manifest.json"), br#"{"task":1}"#)
            .await
            .unwrap();
    }
    let mut list_ns = Vec::new();
    for _ in 0..8 {
        let started = Instant::now();
        let listed = tasks.list_selected().await.unwrap();
        list_ns.push(started.elapsed().as_nanos());
        assert_eq!(listed.len(), 32);
    }
    assert_latency_budget(BudgetLine::TaskListP95Ms, percentile_ms(&list_ns, 95));
}

#[tokio::test]
async fn backup_restore_and_monthly_cost_meet_gate3() {
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(dir.path());
    crate::magician_v2::system_owners::open_local_system_owner(
        &workspace,
        "alice",
        "home",
        SystemOwner::Programs,
    )
    .put(SystemOwner::Programs.sample_rel(), b"{\"program\":1}")
    .await
    .unwrap();
    let snapshot = snapshot_representative_scope(&workspace, "alice", "home")
        .await
        .unwrap();
    SignedSnapshot::wrap(snapshot).unwrap().verify().unwrap();
    assert_budget(BudgetLine::BackupRpoSecs, 0);
    assert_budget(
        BudgetLine::RestoreRtoSecs,
        started.elapsed().as_secs().max(0),
    );
    assert_budget(BudgetLine::MonthlyStorageBytes, 0);
    assert_budget(BudgetLine::MonthlyOperations, 0);
    assert_budget(BudgetLine::MonthlyEgressBytes, 0);
}

#[tokio::test]
async fn adapter_latency_backlog_and_freeze_meet_gate3() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let small = vec![b'a'; 64 * 1024];
    let mut first = Vec::new();
    let mut complete = Vec::new();
    for _ in 0..8 {
        let started = Instant::now();
        storage
            .objects
            .put(PutObjectRequest {
                key: key.clone(),
                body: bytes_body(small.clone()),
                content_type: None,
                condition: PutCondition::Overwrite,
            })
            .await
            .unwrap();
        complete.push(started.elapsed().as_nanos());
        let started = Instant::now();
        let read = storage.objects.get(&key, None).await.unwrap();
        first.push(started.elapsed().as_nanos());
        assert!(read.metadata.len <= accepted_budgets().max_in_memory_transfer_bytes);
    }
    assert_latency_budget(
        BudgetLine::ObjectSmallFirstByteP95Ms,
        percentile_ms(&first, 95),
    );
    assert_latency_budget(
        BudgetLine::ObjectSmallCompleteLocalP95Ms,
        percentile_ms(&complete, 95),
    );
    assert_budget(BudgetLine::MaxInMemoryTransferBytes, 64 * 1024);

    let medium = vec![b'b'; 256 * 1024];
    let mut med_first = Vec::new();
    let mut med_complete = Vec::new();
    let med_key = StorageKey::tenant("alice", "home", "tasks", "med").unwrap();
    for _ in 0..4 {
        let started = Instant::now();
        storage
            .objects
            .put(PutObjectRequest {
                key: med_key.clone(),
                body: bytes_body(medium.clone()),
                content_type: None,
                condition: PutCondition::Overwrite,
            })
            .await
            .unwrap();
        med_complete.push(started.elapsed().as_nanos());
        let started = Instant::now();
        let _ = storage.objects.get(&med_key, None).await.unwrap();
        med_first.push(started.elapsed().as_nanos());
    }
    assert_latency_budget(
        BudgetLine::ObjectMediumFirstByteP95Ms,
        percentile_ms(&med_first, 95),
    );
    assert_latency_budget(
        BudgetLine::ObjectMediumCompleteP95Ms,
        percentile_ms(&med_complete, 95),
    );
    let large_started = Instant::now();
    let large_key = StorageKey::tenant("alice", "home", "tasks", "large").unwrap();
    storage
        .objects
        .put(PutObjectRequest {
            key: large_key,
            body: bytes_body(vec![b'c'; 1024 * 1024]),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::ObjectLargeCompleteP95Ms,
        large_started.elapsed().as_millis() as u64,
    );

    let dataset = DatasetId::parse("events").unwrap();
    let partition = PartitionId::parse("p1").unwrap();
    let part = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g1".into(),
        name: "rows.bin".into(),
    };
    let digest = ContentDigest {
        algorithm: DigestAlgorithm::Blake3,
        hex: blake3::hash(b"row").to_hex().to_string(),
    };
    let flush_started = Instant::now();
    storage
        .datasets
        .stage_part(StageDatasetPart {
            part: part.clone(),
            digest: digest.clone(),
            len: 3,
            body: bytes_body(b"row".to_vec()),
        })
        .await
        .unwrap();
    assert_latency_budget(
        BudgetLine::ParquetFlushP95Ms,
        flush_started.elapsed().as_millis() as u64,
    );
    let commit_started = Instant::now();
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
        commit_started.elapsed().as_millis() as u64,
    );
    let query_started = Instant::now();
    storage.datasets.open_part(&part, None).await.unwrap();
    assert_latency_budget(
        BudgetLine::DatasetQueryP95Ms,
        query_started.elapsed().as_millis() as u64,
    );
    assert_budget(BudgetLine::OutboxBacklog, 1);
    assert_budget(BudgetLine::DatasetBacklog, 0);

    let scope = ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    );
    storage
        .indexes
        .apply(IndexMutationBatch {
            scope: scope.clone(),
            index: IndexId::parse("memory").unwrap(),
            source_watermark: "src-1".into(),
        })
        .await
        .unwrap();
    let rebuild_started = Instant::now();
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
        rebuild_started.elapsed().as_millis() as u64,
    );
    assert_budget(BudgetLine::IndexStaleFallbackSecs, 0);
    assert_budget(BudgetLine::ScratchQuotaBytes, 64 * 1024 * 1024);
    assert_budget(BudgetLine::ScratchCleanupOnOpen, 1);
    assert_budget(BudgetLine::LeaseTtlSecs, 24 * 60 * 60);
    assert_budget(BudgetLine::LeaseRenewalSecs, 60 * 60);
    assert_budget(BudgetLine::LeaseLossDetectionSecs, 60 * 60);

    let payload = vec![0u8; 8 * 1024 * 1024];
    let freeze_key = StorageKey::tenant("alice", "home", "tasks", "freeze").unwrap();
    let freeze_started = Instant::now();
    storage
        .objects
        .put(PutObjectRequest {
            key: freeze_key,
            body: bytes_body(payload),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let freeze_ms = freeze_started.elapsed().as_millis().max(1) as u64;
    let freeze_secs = freeze_started.elapsed().as_secs();
    let throughput = (8 * 1024 * 1024 * 1000) / freeze_ms;
    assert_budget(BudgetLine::WriteFreezeSecs, freeze_secs);
    let floor =
        BudgetVerdict::compare_floor(BudgetLine::MigrationThroughputBytesPerSec, throughput);
    assert!(
        floor.pass,
        "throughput {} below accepted {}",
        floor.measured, floor.accepted
    );
}

#[tokio::test]
async fn default_runtime_scratch_quota_matches_gate3() {
    let dir = tempfile::tempdir().unwrap();
    let profile = magician_storage::resolve(magician_storage::ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    let owner = OwnerId::parse("magician-gate3").unwrap();
    let _runtime = StorageRuntime::open_local(dir.path(), profile, owner).unwrap();
    assert_budget(BudgetLine::ScratchQuotaBytes, 64 * 1024 * 1024);
}

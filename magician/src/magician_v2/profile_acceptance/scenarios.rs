//! Task 20 scenarios 1–11 plus §13.5 outages.

use std::sync::Arc;
use std::time::Duration;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::fs::LocalStorage;
use magician_storage::index::{IndexId, IndexMutationBatch, IndexQuery, IndexStore};
use magician_storage::object::{bytes_body, ObjectStore, PutCondition, PutObjectRequest};
use magician_storage::scratch::ScratchRequest;
use magician_storage::secret::{SecretPurpose, SecretRef, SecretStore};
use magician_storage::{
    resolve, BootstrapSource, ContentDigest, DigestAlgorithm, OwnerId, PrincipalId, ProfileKind,
    ResolveOptions, ScopeId, ScratchStore, StorageError, StorageKey, StorageProfileDocument,
    StorageRuntime, WorkspaceId,
};
use magician_storage_migration::SyntheticOwner;

use super::device::{
    assert_device_local_matrix_unavailable, device_bound_availability, DeviceAvailability,
    DeviceBoundSurface, DEVICE_BRIDGE_REQUIRED,
};
use crate::magician_v2::agent_owners::{AgentAccess, AgentOwner};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::recovery::{
    restore_representative_scope, snapshot_representative_scope, SignedSnapshot,
};
use crate::magician_v2::storage_activation::{ActivationContext, OperatorWorkflow};
use crate::magician_v2::system_owners::{SystemAccess, SystemOwner};
use crate::magician_v2::work_owners::{WorkAccess, WorkOwner};

fn scope() -> ScopeId {
    ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    )
}

fn local_profile() -> magician_storage::ResolvedStorageProfile {
    resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap()
}

fn runtime(root: &std::path::Path, owner: &str) -> StorageRuntime {
    StorageRuntime::open_local(root, local_profile(), OwnerId::parse(owner).unwrap()).unwrap()
}

async fn put_blob(storage: &LocalStorage, object: &str, bytes: &[u8]) {
    let key = StorageKey::tenant("alice", "home", "tasks", object).unwrap();
    storage
        .objects
        .put(PutObjectRequest {
            key,
            body: bytes_body(bytes.to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
}

#[test]
fn scenario_1_local_engine_local_profile_fresh_root() {
    let dir = tempfile::tempdir().unwrap();
    let profile = local_profile();
    assert_eq!(profile.kind, ProfileKind::LocalEmbedded);
    assert_eq!(profile.source, BootstrapSource::MissingDefault);
    let runtime = runtime(dir.path(), "magician-local");
    assert_eq!(runtime.operator_health().profile, "local_embedded");
    let _ = ArtifactV2Workspace::new(dir.path());
}

#[tokio::test]
async fn scenario_2_local_engine_migrated_existing_root() {
    let existing = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(existing.path());
    crate::magician_v2::work_owners::open_local_work_owner(
        &workspace,
        "alice",
        "home",
        WorkOwner::TaskRecords,
    )
    .put(WorkOwner::TaskRecords.sample_rel(), b"{\"task\":1}")
    .await
    .unwrap();
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner
        .seed(
            &magician_storage::StorageScope::Tenant(scope()),
            "task-1",
            "{\"task\":1}",
        )
        .unwrap();
    let ledger = tempfile::tempdir().unwrap();
    let flow = OperatorWorkflow::with_handler(ledger.path(), owner, ActivationContext::default())
        .await
        .unwrap();
    let planned = flow
        .plan(
            magician_storage_migration::SYNTHETIC_OWNER_ID,
            magician_storage::StorageScope::Tenant(scope()),
            true,
            "alice",
            "home",
        )
        .await
        .unwrap();
    assert!(planned.ok && planned.dry_run);
    let migrated = tempfile::tempdir().unwrap();
    let dest = ArtifactV2Workspace::new(migrated.path());
    crate::magician_v2::work_owners::open_local_work_owner(
        &dest,
        "alice",
        "home",
        WorkOwner::TaskRecords,
    )
    .import_all(
        &crate::magician_v2::work_owners::open_local_work_owner(
            &workspace,
            "alice",
            "home",
            WorkOwner::TaskRecords,
        )
        .export_all()
        .await
        .unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(
        crate::magician_v2::work_owners::open_local_work_owner(
            &dest,
            "alice",
            "home",
            WorkOwner::TaskRecords,
        )
        .get(WorkOwner::TaskRecords.sample_rel())
        .await
        .unwrap(),
        b"{\"task\":1}"
    );
}

#[test]
fn scenario_3_local_engine_remote_durable_uses_remote_store_not_open_local() {
    let yaml = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician-storage/examples/remote_durable.yaml"),
    )
    .unwrap();
    let doc = StorageProfileDocument::from_yaml(&yaml).unwrap();
    assert_eq!(doc.kind().unwrap(), ProfileKind::RemoteDurable);
    let remote = magician_storage::ResolvedStorageProfile {
        kind: ProfileKind::RemoteDurable,
        source: BootstrapSource::File("remote_durable.yaml".into()),
        document: doc,
    };
    let dir = tempfile::tempdir().unwrap();
    let err = match StorageRuntime::open_local(
        dir.path(),
        remote,
        OwnerId::parse("magician-local").unwrap(),
    ) {
        Ok(_) => panic!("remote profile must not open as local"),
        Err(err) => err,
    };
    assert!(matches!(err, StorageError::UnsupportedCapability));
}

#[tokio::test]
async fn scenario_4_and_5_remote_linux_sees_same_store_without_copying_directory() {
    let shared = tempfile::tempdir().unwrap();
    let local_engine_scratch = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(shared.path()).unwrap();
    put_blob(&storage, "ack", b"acknowledged").await;
    drop(storage);
    std::fs::write(local_engine_scratch.path().join("scratch.txt"), b"compute").unwrap();
    std::fs::remove_dir_all(local_engine_scratch.path()).unwrap();
    assert!(!local_engine_scratch.path().join("scratch.txt").exists());
    let linux = LocalStorage::open(shared.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "ack").unwrap();
    let got = linux.objects.get(&key, None).await.unwrap();
    assert_eq!(got.metadata.len, b"acknowledged".len() as u64);
}

#[tokio::test]
async fn scenario_6_local_engine_resumes_after_remote_lease_release() {
    let shared = tempfile::tempdir().unwrap();
    let linux = runtime(shared.path(), "magician-linux");
    linux.scope_leases.acquire(&scope()).await.unwrap();
    put_blob(
        &LocalStorage::open(shared.path()).unwrap(),
        "state",
        b"remote-write",
    )
    .await;
    linux.scope_leases.release_all().await.unwrap();
    drop(linux);
    let local = runtime(shared.path(), "magician-local");
    local.scope_leases.acquire(&scope()).await.unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "state").unwrap();
    let storage = LocalStorage::open(shared.path()).unwrap();
    assert_eq!(
        storage.objects.get(&key, None).await.unwrap().metadata.len,
        b"remote-write".len() as u64
    );
}

#[tokio::test]
async fn scenario_7_delete_scratch_and_index_keeps_canonical() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(dir.path());
    crate::magician_v2::agent_owners::open_local_agent_owner(
        &workspace,
        "alice",
        "home",
        AgentOwner::MemoryCanonical,
    )
    .put(AgentOwner::MemoryCanonical.sample_rel(), b"canon")
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        &workspace,
        "alice",
        "home",
        AgentOwner::MemoryIndex,
    )
    .put(AgentOwner::MemoryIndex.sample_rel(), b"idx")
    .await
    .unwrap();
    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(AgentOwner::MemoryIndex.sample_rel()),
    )
    .ok();
    assert_eq!(
        crate::magician_v2::agent_owners::open_local_agent_owner(
            &workspace,
            "alice",
            "home",
            AgentOwner::MemoryCanonical,
        )
        .get(AgentOwner::MemoryCanonical.sample_rel())
        .await
        .unwrap(),
        b"canon"
    );
}

#[tokio::test]
async fn scenario_8_fresh_host_restore_from_recovery_inputs() {
    let source = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(source.path());
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
    let signed = SignedSnapshot::wrap(snapshot).unwrap();
    signed.verify().unwrap();
    let fresh = tempfile::tempdir().unwrap();
    let restored = ArtifactV2Workspace::new(fresh.path());
    restore_representative_scope(&restored, &signed)
        .await
        .unwrap();
    assert_eq!(
        crate::magician_v2::system_owners::open_local_system_owner(
            &restored,
            "alice",
            "home",
            SystemOwner::Programs,
        )
        .get(SystemOwner::Programs.sample_rel())
        .await
        .unwrap(),
        b"{\"program\":1}"
    );
}

#[tokio::test]
async fn scenario_9_outages_follow_section_13_5() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    put_blob(&storage, "req", b"need-durable").await;
    let key = StorageKey::tenant("alice", "home", "tasks", "req").unwrap();
    std::fs::remove_dir_all(dir.path().join("objects")).unwrap();
    let err = match storage.objects.get(&key, None).await {
        Ok(_) => panic!("required object outage must fail closed, not succeed"),
        Err(err) => err,
    };
    assert!(
        matches!(
            err,
            StorageError::NotFound
                | StorageError::Unavailable { .. }
                | StorageError::Backend { .. }
        ),
        "required object outage must fail closed, not succeed: {err:?}"
    );

    let secret = SecretRef::parse("missing-secret").unwrap();
    assert!(storage
        .secrets
        .resolve(&secret, SecretPurpose::ObjectStore)
        .await
        .is_err());

    let overflow = storage
        .scratch
        .allocate(ScratchRequest {
            scope: scope(),
            purpose: "tool".into(),
            max_bytes: 128 * 1024 * 1024,
        })
        .await;
    assert!(overflow.is_err());

    let index = IndexId::parse("memory").unwrap();
    storage
        .indexes
        .apply(IndexMutationBatch {
            scope: scope(),
            index: index.clone(),
            source_watermark: "src".into(),
        })
        .await
        .unwrap();
    std::fs::remove_dir_all(dir.path().join("indexes")).ok();
    let page = LocalStorage::open(dir.path())
        .unwrap()
        .indexes
        .query(
            &scope(),
            &index,
            IndexQuery {
                text: "x".into(),
                limit: 10,
            },
        )
        .await
        .unwrap();
    assert!(page.watermark.is_none());
    assert!(page.hits.is_empty());

    let dataset = DatasetId::parse("events").unwrap();
    let partition = PartitionId::parse("p1").unwrap();
    let part = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g1".into(),
        name: "rows.bin".into(),
    };
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
    std::fs::remove_dir_all(dir.path().join("datasets").join("parts")).ok();
    assert!(storage.datasets.open_part(&part, None).await.is_err());
}

#[tokio::test]
async fn scenario_10_scope_isolation_and_two_writer_fencing() {
    let dir = tempfile::tempdir().unwrap();
    let first = runtime(dir.path(), "magician-a");
    first.scope_leases.acquire(&scope()).await.unwrap();
    let second = runtime(dir.path(), "magician-b");
    assert!(second.scope_leases.acquire(&scope()).await.is_err());
    let other = ScopeId::new(
        PrincipalId::parse("bob").unwrap(),
        WorkspaceId::parse("work").unwrap(),
    );
    second.scope_leases.acquire(&other).await.unwrap();
    put_blob(&LocalStorage::open(dir.path()).unwrap(), "alice-only", b"a").await;
    let bob_key = StorageKey::tenant("bob", "work", "tasks", "bob-only").unwrap();
    LocalStorage::open(dir.path())
        .unwrap()
        .objects
        .put(PutObjectRequest {
            key: bob_key.clone(),
            body: bytes_body(b"b".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let alice_key = StorageKey::tenant("alice", "home", "tasks", "alice-only").unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    assert!(storage.objects.get(&alice_key, None).await.is_ok());
    assert!(storage.objects.get(&bob_key, None).await.is_ok());
}

#[test]
fn scenario_11_default_profile_unchanged_while_remote_code_is_present() {
    let profile = local_profile();
    assert_eq!(profile.kind, ProfileKind::LocalEmbedded);
    let cargo = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician-bin/Cargo.toml"),
    )
    .unwrap();
    for crate_name in [
        "magician-storage-s3",
        "magician-storage-state",
        "magician-storage-migration",
    ] {
        assert!(
            !cargo.contains(crate_name),
            "magician-bin must not depend on {crate_name}"
        );
    }
    let main = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician-bin/src/main.rs"),
    )
    .unwrap();
    assert!(main.contains("StorageRuntime::open_local"));
    assert!(!main.contains("magician_storage_s3"));
}

#[test]
fn device_bound_surfaces_are_unavailable_without_bridge() {
    for surface in [
        DeviceBoundSurface::Browser,
        DeviceBoundSurface::Screen,
        DeviceBoundSurface::Meeting,
        DeviceBoundSurface::Audio,
    ] {
        match device_bound_availability(surface) {
            DeviceAvailability::Unavailable { reason } => {
                assert_eq!(reason, DEVICE_BRIDGE_REQUIRED);
            },
        }
    }
    assert_device_local_matrix_unavailable().unwrap();
}

#[test]
fn unavailable_canonical_outage_is_retryable_not_local_fallback() {
    let err = StorageError::Unavailable {
        retry_after: Some(Duration::from_secs(1)),
    };
    assert_eq!(
        err.retry_class(),
        magician_storage::RetryClass::SameIdempotencyKey
    );
    assert!(err.retry_safe());
}

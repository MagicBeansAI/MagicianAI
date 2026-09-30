//! Section 16.3 restore drills compared against Gate 2 targets.

use std::time::Instant;

use magician_storage::dataset::{
    DatasetId, DatasetManifest, DatasetPartRef, DatasetStore, ManifestVersion, PartitionId,
    StageDatasetPart,
};
use magician_storage::fs::LocalStorage;
use magician_storage::index::{IndexId, IndexMutationBatch, IndexStore, RebuildIndexRequest};
use magician_storage::object::{
    bytes_body, DeleteCondition, ObjectStore, PutCondition, PutObjectRequest,
};
use magician_storage::{
    ContentDigest, DigestAlgorithm, GcPolicy, ObjectReference, PrincipalId, ScopeId, SecretPurpose,
    SecretRef, SecretStore, StorageKey, WorkspaceId,
};

use super::gate2::{DrillVerdict, RecoveryClass};
use super::integrity::{referenced_object, RecoveryManifest, ReferencedObject};
use super::snapshot::{
    restore_representative_scope, snapshot_representative_scope, SignedSnapshot,
};
use crate::magician_v2::agent_owners::{AgentAccess, AgentOwner};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat_owners::{ChatAccess, ChatOwner};
use crate::magician_v2::dataset_owners::{DatasetAccess, DatasetFamily};
use crate::magician_v2::object_owners::{BlobAccess, ObjectOwner};
use crate::magician_v2::system_owners::{SystemAccess, SystemOwner};
use crate::magician_v2::work_owners::{WorkAccess, WorkOwner};

async fn seed_scope(workspace: &ArtifactV2Workspace, principal: &str, name: &str) {
    crate::magician_v2::system_owners::open_local_system_owner(
        workspace,
        principal,
        name,
        SystemOwner::Programs,
    )
    .put(SystemOwner::Programs.sample_rel(), b"{\"program\":1}")
    .await
    .unwrap();
    crate::magician_v2::work_owners::open_local_work_owner(
        workspace,
        principal,
        name,
        WorkOwner::TaskRecords,
    )
    .put(
        WorkOwner::TaskRecords.sample_rel(),
        b"{\"task\":1,\"schedule\":\"daily\"}",
    )
    .await
    .unwrap();
    crate::magician_v2::chat_owners::open_local_chat_owner(
        workspace,
        principal,
        name,
        ChatOwner::ChatSessions,
    )
    .put(
        ChatOwner::ChatSessions.sample_rel(),
        b"{\"session\":\"s1\"}",
    )
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::MemoryCanonical,
    )
    .put(
        AgentOwner::MemoryCanonical.sample_rel(),
        b"{\"memory\":\"core\"}",
    )
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::AgentRuntime,
    )
    .put(
        AgentOwner::AgentRuntime.sample_rel(),
        b"{\"scheduler\":\"on\"}",
    )
    .await
    .unwrap();
    crate::magician_v2::system_owners::open_local_system_owner(
        workspace,
        principal,
        name,
        SystemOwner::DevicePairing,
    )
    .put(
        SystemOwner::DevicePairing.sample_rel(),
        b"{\"device\":\"a\"}",
    )
    .await
    .unwrap();
    let secrets = crate::magician_v2::system_owners::open_local_system_owner(
        workspace,
        principal,
        name,
        SystemOwner::SecretVault,
    );
    secrets
        .put("secrets/secret_audit.jsonl", b"{\"id\":\"k\"}\n")
        .await
        .unwrap();
    secrets
        .put("secrets/mcp_oauth.vault", b"CLEARTEXT")
        .await
        .unwrap();
    crate::magician_v2::object_owners::open_local_object_owner(
        workspace,
        principal,
        name,
        ObjectOwner::AppAttachments,
    )
    .put(ObjectOwner::AppAttachments.sample_rel(), b"note")
    .await
    .unwrap();
    crate::magician_v2::dataset_owners::open_local_dataset_family(
        workspace,
        principal,
        name,
        DatasetFamily::Events,
    )
    .put(DatasetFamily::Events.sample_rel(), b"parquet")
    .await
    .unwrap();
}

fn assert_gate2(elapsed_secs: u64) {
    for class in [
        RecoveryClass::CanonicalState,
        RecoveryClass::RequiredObjects,
        RecoveryClass::ObservabilityDatasets,
        RecoveryClass::SecretsConfiguration,
    ] {
        let verdict = DrillVerdict::compare(class, 0, elapsed_secs);
        assert!(verdict.pass, "{class:?} missed Gate 2: {verdict:?}");
    }
}

#[tokio::test]
async fn local_embedded_backup_restore_meets_gate2() {
    let started = Instant::now();
    let source = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(source.path());
    seed_scope(&workspace, "alice", "home").await;
    let snapshot = snapshot_representative_scope(&workspace, "alice", "home")
        .await
        .unwrap();
    let signed = SignedSnapshot::wrap(snapshot).unwrap();
    signed.verify().unwrap();
    let evidence = signed.snapshot.evidence(&signed.digest);
    assert!(evidence.secret_bytes_excluded);
    assert!(evidence.owners.contains(&"task_records".into()));
    assert!(evidence.owners.contains(&"chat_sessions".into()));
    assert!(evidence.owners.contains(&"memory_canonical".into()));
    assert!(evidence.owners.contains(&"agent_runtime".into()));
    let dump = serde_json::to_string(&signed).unwrap();
    assert!(!dump.contains("CLEARTEXT"));

    let fresh = tempfile::tempdir().unwrap();
    let restored = ArtifactV2Workspace::new(fresh.path());
    restore_representative_scope(&restored, &signed)
        .await
        .unwrap();

    let programs = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::Programs,
    );
    assert_eq!(
        programs
            .get(SystemOwner::Programs.sample_rel())
            .await
            .unwrap(),
        b"{\"program\":1}"
    );
    let tasks = crate::magician_v2::work_owners::open_local_work_owner(
        &restored,
        "alice",
        "home",
        WorkOwner::TaskRecords,
    );
    assert!(String::from_utf8_lossy(
        &tasks
            .get(WorkOwner::TaskRecords.sample_rel())
            .await
            .unwrap()
    )
    .contains("schedule"));
    let chat = crate::magician_v2::chat_owners::open_local_chat_owner(
        &restored,
        "alice",
        "home",
        ChatOwner::ChatSessions,
    );
    assert_eq!(
        chat.get(ChatOwner::ChatSessions.sample_rel())
            .await
            .unwrap(),
        b"{\"session\":\"s1\"}"
    );
    let memory = crate::magician_v2::agent_owners::open_local_agent_owner(
        &restored,
        "alice",
        "home",
        AgentOwner::MemoryCanonical,
    );
    assert_eq!(
        memory
            .get(AgentOwner::MemoryCanonical.sample_rel())
            .await
            .unwrap(),
        b"{\"memory\":\"core\"}"
    );
    let schedules = crate::magician_v2::agent_owners::open_local_agent_owner(
        &restored,
        "alice",
        "home",
        AgentOwner::AgentRuntime,
    );
    assert!(String::from_utf8_lossy(
        &schedules
            .get(AgentOwner::AgentRuntime.sample_rel())
            .await
            .unwrap()
    )
    .contains("scheduler"));
    let pairing = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::DevicePairing,
    );
    assert_eq!(
        pairing
            .get(SystemOwner::DevicePairing.sample_rel())
            .await
            .unwrap(),
        b"{\"device\":\"a\"}"
    );
    let attachments = crate::magician_v2::object_owners::open_local_object_owner(
        &restored,
        "alice",
        "home",
        ObjectOwner::AppAttachments,
    );
    assert_eq!(
        attachments
            .get(ObjectOwner::AppAttachments.sample_rel())
            .await
            .unwrap(),
        b"note"
    );
    let events = crate::magician_v2::dataset_owners::open_local_dataset_family(
        &restored,
        "alice",
        "home",
        DatasetFamily::Events,
    );
    assert!(events
        .exists(DatasetFamily::Events.sample_rel())
        .await
        .unwrap());
    let secrets = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::SecretVault,
    );
    assert!(secrets.exists("secrets/secret_audit.jsonl").await.unwrap());
    assert!(!secrets.exists("secrets/mcp_oauth.vault").await.unwrap());
    assert!(!fresh.path().join("scratch").exists());
    assert!(!fresh.path().join("indexes").exists());
    assert_gate2(started.elapsed().as_secs());
}

#[tokio::test]
async fn fresh_host_rebuilds_index_and_validates_references() {
    let source = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(source.path());
    seed_scope(&workspace, "alice", "home").await;
    let signed = SignedSnapshot::wrap(
        snapshot_representative_scope(&workspace, "alice", "home")
            .await
            .unwrap(),
    )
    .unwrap();

    let started = Instant::now();
    let fresh = tempfile::tempdir().unwrap();
    let restored = ArtifactV2Workspace::new(fresh.path());
    restore_representative_scope(&restored, &signed)
        .await
        .unwrap();

    let adapter = LocalStorage::open(fresh.path().join(".magician-storage")).unwrap();
    let scope = ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    );
    let index = IndexId::parse("memory").unwrap();
    let memory = crate::magician_v2::agent_owners::open_local_agent_owner(
        &restored,
        "alice",
        "home",
        AgentOwner::MemoryCanonical,
    );
    let memory_bytes = memory.export_all().await.unwrap();
    let source_watermark = format!("memory:{}", blake3::hash(&memory_bytes).to_hex());
    adapter
        .indexes
        .apply(IndexMutationBatch {
            scope: scope.clone(),
            index: index.clone(),
            source_watermark: source_watermark.clone(),
        })
        .await
        .unwrap();
    let rebuilt = adapter
        .indexes
        .rebuild(RebuildIndexRequest {
            scope: scope.clone(),
            index: index.clone(),
        })
        .await
        .unwrap();
    assert_eq!(rebuilt.watermark.position, "rebuilt");
    assert_eq!(rebuilt.watermark.source, index.as_str());
    let scratch = fresh.path().join(".magician-storage").join("scratch");
    assert!(!scratch.exists() || std::fs::read_dir(&scratch).unwrap().next().is_none());

    let events = crate::magician_v2::dataset_owners::open_local_dataset_family(
        &restored,
        "alice",
        "home",
        DatasetFamily::Events,
    );
    let present_parts = events.list_selected().await.unwrap();
    let pairing = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::DevicePairing,
    );
    let present_devices = pairing.list_selected().await.unwrap();
    let secrets = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::SecretVault,
    );
    let present_secrets = secrets.list_selected().await.unwrap();
    let attachments = crate::magician_v2::object_owners::open_local_object_owner(
        &restored,
        "alice",
        "home",
        ObjectOwner::AppAttachments,
    );
    let mut expected_objects = Vec::new();
    let mut present_objects = Vec::new();
    for owner in &signed.snapshot.owners {
        if owner.catalog_id != "app_attachments" {
            continue;
        }
        let records: Vec<crate::magician_v2::object_owners::ExportedBlob> =
            serde_json::from_slice(&owner.bytes).unwrap();
        for record in records {
            expected_objects.push(ReferencedObject {
                key: record.path.clone(),
                version: "restored".into(),
                digest: record.digest.clone(),
            });
            let raw = attachments.get(&record.path).await.unwrap();
            present_objects.push(ReferencedObject {
                key: record.path,
                version: "restored".into(),
                digest: crate::magician_v2::object_owners::blob_digest(&raw),
            });
        }
    }
    let expected = RecoveryManifest {
        objects: expected_objects,
        dataset_parts: vec![DatasetFamily::Events.sample_rel().to_string()],
        index_watermarks: vec![rebuilt.watermark.source],
        device_records: vec![SystemOwner::DevicePairing.sample_rel().to_string()],
        secret_refs: vec!["secrets/secret_audit.jsonl".into()],
    };
    let report = expected.walk(
        &present_objects,
        &present_parts,
        &present_devices,
        &present_secrets,
    );
    assert!(
        report.missing.is_empty(),
        "fresh-host restore missing {:?}",
        report.missing
    );
    assert_gate2(started.elapsed().as_secs());
}

#[tokio::test]
async fn deleted_object_stays_restorable_until_unreferenced_gc() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let put = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"payload".to_vec()),
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
    let retained = storage.objects.retained_generation(&key).unwrap().unwrap();
    assert_eq!(retained.1, b"payload");
    let referenced = [ObjectReference {
        key: key.clone(),
        version: put.metadata.version.clone(),
    }];
    let kept = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &referenced, i64::MAX)
        .unwrap();
    assert_eq!(kept.collected, 0);
    let collected = storage
        .objects
        .collect_unreferenced(GcPolicy::immediate(), &[], i64::MAX)
        .unwrap();
    assert_eq!(collected.collected, 1);
}

#[tokio::test]
async fn remote_database_pitr_validates_referenced_object_generations() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let put = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"payload".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let db_path = dir.path().join("remote.sqlite");
    {
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "CREATE TABLE refs (key TEXT NOT NULL, version TEXT NOT NULL, digest TEXT NOT NULL)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO refs(key, version, digest) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                key.encode(),
                put.metadata.version.as_str(),
                put.metadata.digest.hex.as_str()
            ],
        )
        .unwrap();
    }
    let pitr = std::fs::read(&db_path).unwrap();
    storage
        .objects
        .delete(&key, DeleteCondition::Existing)
        .await
        .unwrap();

    let restored_db = dir.path().join("restored.sqlite");
    std::fs::write(&restored_db, pitr).unwrap();
    let conn = rusqlite::Connection::open(&restored_db).unwrap();
    let (ref_key, ref_version, ref_digest): (String, String, String) = conn
        .query_row("SELECT key, version, digest FROM refs", [], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap();
    let retained = storage.objects.retained_generation(&key).unwrap().unwrap();
    assert_eq!(retained.0.as_str(), ref_version);
    assert_eq!(blake3::hash(&retained.1).to_hex().to_string(), ref_digest);
    let expected = referenced_object(&key, &put.metadata.version, &put.metadata.digest.hex);
    let present = ReferencedObject {
        key: ref_key,
        version: retained.0.as_str().to_string(),
        digest: blake3::hash(&retained.1).to_hex().to_string(),
    };
    let manifest = RecoveryManifest {
        objects: vec![expected],
        dataset_parts: vec![],
        index_watermarks: vec![],
        device_records: vec![],
        secret_refs: vec![],
    };
    let report = manifest.walk(&[present], &[], &[], &[]);
    assert!(report.missing.is_empty());
    let kept = storage
        .objects
        .collect_unreferenced(
            GcPolicy::immediate(),
            &[ObjectReference {
                key: key.clone(),
                version: put.metadata.version.clone(),
            }],
            i64::MAX,
        )
        .unwrap();
    assert_eq!(kept.collected, 0);
}

#[tokio::test]
async fn remote_logical_export_round_trips_without_secret_bytes() {
    let source = tempfile::tempdir().unwrap();
    let remote = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(source.path());
    seed_scope(&workspace, "alice", "home").await;
    let snapshot = snapshot_representative_scope(&workspace, "alice", "home")
        .await
        .unwrap();
    let signed = SignedSnapshot::wrap(snapshot).unwrap();
    let restored = ArtifactV2Workspace::new(remote.path());
    restore_representative_scope(&restored, &signed)
        .await
        .unwrap();
    let secrets = crate::magician_v2::system_owners::open_local_system_owner(
        &restored,
        "alice",
        "home",
        SystemOwner::SecretVault,
    );
    let dump = secrets.export_all().await.unwrap();
    assert!(!String::from_utf8_lossy(&dump).contains("CLEARTEXT"));
}

#[tokio::test]
async fn rotating_storage_credentials_does_not_rewrite_product_data() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let key = StorageKey::tenant("alice", "home", "tasks", "blob").unwrap();
    let put = storage
        .objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"payload".to_vec()),
            content_type: None,
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    let creds = SecretRef::parse("object-store-creds").unwrap();
    storage
        .secrets
        .put(&creds, b"old-credential")
        .await
        .unwrap();
    storage
        .secrets
        .put(&creds, b"new-credential-rotated")
        .await
        .unwrap();
    let after = storage.objects.head(&key).await.unwrap().unwrap();
    assert_eq!(after.digest, put.metadata.digest);
    assert_eq!(after.version, put.metadata.version);
    let resolved = storage
        .secrets
        .resolve(&creds, SecretPurpose::ObjectStore)
        .await
        .unwrap();
    assert_eq!(resolved.expose(), b"new-credential-rotated");
}

#[tokio::test]
async fn dataset_generation_gc_keeps_manifest_parts() {
    let dir = tempfile::tempdir().unwrap();
    let storage = LocalStorage::open(dir.path()).unwrap();
    let dataset = DatasetId::parse("events").unwrap();
    let partition = PartitionId::parse("p1").unwrap();
    let live = DatasetPartRef {
        dataset: dataset.clone(),
        partition: partition.clone(),
        generation: "g1".into(),
        name: "rows.bin".into(),
    };
    let digest = ContentDigest {
        algorithm: DigestAlgorithm::Blake3,
        hex: blake3::hash(b"abc").to_hex().to_string(),
    };
    storage
        .datasets
        .stage_part(StageDatasetPart {
            part: live.clone(),
            digest: digest.clone(),
            len: 3,
            body: bytes_body(b"abc".to_vec()),
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
                parts: vec![live.clone()],
                row_count: 1,
                schema_identity: "s".into(),
            },
            None,
        )
        .await
        .unwrap();
    let report = storage.datasets.collect_unreferenced_generations().unwrap();
    assert_eq!(report.retained_referenced, 1);
    assert_eq!(report.collected, 0);
}

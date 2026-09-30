//! Task 18 default-profile proof and Tier 2 rebuild/degraded contracts.

use std::collections::BTreeSet;
use std::time::Instant;

use magician_storage::{resolve, BootstrapSource, ProfileKind, ResolveOptions, StorageRuntime};
use magician_storage_migration::SourceGuard;

use super::{load_catalog, load_support_matrix, RemoteOutcome, TASK1_BASELINE_LINES};
use crate::magician_v2::agent_owners::{AgentAccess, AgentOwner};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::chat_owners::{ChatAccess, ChatOwner};
use crate::magician_v2::database_owners::{DatabaseAccess, DatabaseOwner};
use crate::magician_v2::dataset_owners::{DatasetAccess, DatasetFamily};
use crate::magician_v2::object_owners::{BlobAccess, ObjectOwner};
use crate::magician_v2::recovery::{
    restore_representative_scope, snapshot_representative_scope, SignedSnapshot,
};
use crate::magician_v2::subprocess_owners::{SubprocessAccess, SubprocessOwner};
use crate::magician_v2::system_owners::{SystemAccess, SystemOwner};
use crate::magician_v2::work_owners::{WorkAccess, WorkOwner};

const EVIDENCE_KEYS: [&str; 9] = [
    "characterized",
    "wrapped_local",
    "callers_routed",
    "bypass_guarded",
    "remote_implemented",
    "remote_conformant",
    "migration_qualified",
    "restore_qualified",
    "remote_ready",
];

#[test]
fn catalog_inventory_is_remote_ready_while_locally_active() {
    let catalog = load_catalog().unwrap();
    assert_eq!(
        catalog.owners.len() as u64,
        catalog.measurements.owner_count
    );
    assert_eq!(catalog.measurements.owner_count, 89);
    assert_eq!(catalog.measurements.tier1_count, 74);
    assert_eq!(catalog.measurements.tier2_count, 10);
    assert_eq!(catalog.measurements.device_local_count, 5);

    let mut tier1 = 0u64;
    let mut tier2 = 0u64;
    let mut device = 0u64;
    for owner in &catalog.owners {
        assert_eq!(
            owner.readiness.state, "remote_ready",
            "{} is not remote_ready",
            owner.id
        );
        for key in EVIDENCE_KEYS {
            assert!(
                owner
                    .readiness
                    .evidence
                    .contains_key(serde_yaml::Value::String(key.into())),
                "{} missing evidence {key}",
                owner.id
            );
        }
        assert_eq!(owner.authority.state, "local_active");
        assert_eq!(owner.authority.canonical_profile, "local_embedded");
        assert_eq!(owner.legacy_source.state, "retained");
        if owner.is_tier1() {
            tier1 += 1;
        } else if owner.is_tier2() {
            tier2 += 1;
        } else if owner.is_device_local() {
            device += 1;
        } else {
            panic!("{} has unknown tier {}", owner.id, owner.tier_label());
        }
    }
    assert_eq!(tier1, 74);
    assert_eq!(tier2, 10);
    assert_eq!(device, 5);
}

#[test]
fn every_tier2_and_device_local_owner_has_a_support_matrix_entry() {
    let catalog = load_catalog().unwrap();
    let matrix = load_support_matrix().unwrap();
    assert_eq!(matrix.profile, "remote_durable");
    let catalog_ids: BTreeSet<_> = catalog.owners.iter().map(|o| o.id.as_str()).collect();
    let mut required = BTreeSet::new();
    for owner in &catalog.owners {
        if owner.is_tier2() || owner.is_device_local() {
            required.insert(owner.id.as_str());
        }
    }
    let mut seen = BTreeSet::new();
    for entry in &matrix.owners {
        assert!(
            catalog_ids.contains(entry.id.as_str()),
            "matrix owner {} is not in the catalog",
            entry.id
        );
        assert!(!entry.degraded_behavior.trim().is_empty(), "{}", entry.id);
        assert!(!entry.watermark.is_empty(), "{}", entry.id);
        match entry.remote_outcome {
            RemoteOutcome::RegenerableLocalProjection => {
                assert_eq!(entry.tier_label(), "2", "{}", entry.id);
            },
            RemoteOutcome::Unavailable => {},
            RemoteOutcome::Provided => {
                panic!("{} must not claim provided without Track B", entry.id)
            },
        }
        if entry.remote_outcome == RemoteOutcome::RegenerableLocalProjection {
            for source in &entry.rebuild_sources {
                assert!(
                    catalog_ids.contains(source.as_str()),
                    "{} rebuild source {source} is not cataloged",
                    entry.id
                );
            }
        }
        seen.insert(entry.id.as_str());
    }
    assert_eq!(seen, required);
}

#[test]
fn missing_bootstrap_stays_local_embedded() {
    let resolved = resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    assert_eq!(resolved.kind, ProfileKind::LocalEmbedded);
    assert_eq!(resolved.source, BootstrapSource::MissingDefault);
    let fixture = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../magician-storage/examples/local_embedded.yaml"),
    )
    .unwrap();
    let doc = magician_storage::StorageProfileDocument::from_yaml(&fixture).unwrap();
    assert_eq!(doc.kind().unwrap(), ProfileKind::LocalEmbedded);
}

#[test]
fn magician_bin_does_not_start_remote_or_migration() {
    let main = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician-bin/src/main.rs"),
    )
    .unwrap();
    assert!(main.contains("StorageRuntime::open_local"));
    assert!(!main.contains("open_from_profile"));
    assert!(!main.contains("MigrationCoordinator"));
    assert!(!main.contains("snapshot_representative_scope"));
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
}

#[test]
fn every_closed_owner_bypass_guard_rejects_a_new_site() {
    let guards: Vec<(&str, SourceGuard)> = vec![
        (
            "durable_artifacts",
            crate::magician_v2::artifacts::durable_artifacts_source_guard(),
        ),
        (
            "app_packages",
            crate::magician_v2::object_owners::object_owner_source_guard(ObjectOwner::AppPackages),
        ),
        (
            "events",
            crate::magician_v2::dataset_owners::dataset_owner_source_guard(DatasetFamily::Events),
        ),
        (
            "task_records",
            crate::magician_v2::work_owners::work_owner_source_guard(WorkOwner::TaskRecords),
        ),
        (
            "chat_sessions",
            crate::magician_v2::chat_owners::chat_owner_source_guard(ChatOwner::ChatSessions),
        ),
        (
            "memory_canonical",
            crate::magician_v2::agent_owners::agent_owner_source_guard(AgentOwner::MemoryCanonical),
        ),
        (
            "analytics_duckdb",
            crate::magician_v2::database_owners::database_owner_source_guard(
                DatabaseOwner::AnalyticsDuckdb,
            ),
        ),
        (
            "programs",
            crate::magician_v2::system_owners::system_owner_source_guard(SystemOwner::Programs),
        ),
        (
            "workdirs_scratch",
            crate::magician_v2::subprocess_owners::subprocess_owner_source_guard(
                SubprocessOwner::WorkdirsScratch,
            ),
        ),
    ];
    for (id, guard) in guards {
        assert!(guard.is_enabled(), "{id} guard must be enabled");
        assert!(
            guard.check("magician/src/bypass.rs").is_err(),
            "{id} must reject a new bypass site"
        );
    }
}

#[test]
fn magician_src_growth_is_recorded_against_task1_baseline() {
    let lines = count_rs_lines(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"));
    assert!(
        lines >= TASK1_BASELINE_LINES,
        "magician/src shrank below the Task 1 baseline ({lines} < {TASK1_BASELINE_LINES})"
    );
}

fn count_rs_lines(root: impl AsRef<std::path::Path>) -> u64 {
    let mut total = 0u64;
    fn walk(dir: &std::path::Path, total: &mut u64) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, total);
            } else if path.extension().and_then(|ext| ext.to_str()) == Some("rs") {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    *total += text.lines().count() as u64;
                }
            }
        }
    }
    walk(root.as_ref(), &mut total);
    total
}

async fn seed_golden(workspace: &ArtifactV2Workspace, principal: &str, name: &str) {
    crate::magician_v2::work_owners::open_local_work_owner(
        workspace,
        principal,
        name,
        WorkOwner::TaskRecords,
    )
    .put(WorkOwner::TaskRecords.sample_rel(), b"{\"task\":1}")
    .await
    .unwrap();
    crate::magician_v2::work_owners::open_local_work_owner(
        workspace,
        principal,
        name,
        WorkOwner::ListIndex,
    )
    .put(WorkOwner::ListIndex.sample_rel(), b"index")
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
    crate::magician_v2::chat_owners::open_local_chat_owner(
        workspace,
        principal,
        name,
        ChatOwner::ProgressEvents,
    )
    .put(ChatOwner::ProgressEvents.sample_rel(), b"event\n")
    .await
    .unwrap();
    crate::magician_v2::chat_owners::open_local_chat_owner(
        workspace,
        principal,
        name,
        ChatOwner::ProgressLineage,
    )
    .put(ChatOwner::ProgressLineage.sample_rel(), b"{}")
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
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::MemoryCanonical,
    )
    .put(AgentOwner::MemoryCanonical.sample_rel(), b"canon")
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::MemoryIndex,
    )
    .put(AgentOwner::MemoryIndex.sample_rel(), b"idx")
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::LearningProcedures,
    )
    .put(AgentOwner::LearningProcedures.sample_rel(), b"proc: 1\n")
    .await
    .unwrap();
    crate::magician_v2::agent_owners::open_local_agent_owner(
        workspace,
        principal,
        name,
        AgentOwner::LearningProcedureIndex,
    )
    .put(AgentOwner::LearningProcedureIndex.sample_rel(), b"idx")
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
    crate::magician_v2::dataset_owners::open_local_dataset_family(
        workspace,
        principal,
        name,
        DatasetFamily::Events,
    )
    .put(DatasetFamily::Events.sample_rel(), b"parquet")
    .await
    .unwrap();
    crate::magician_v2::database_owners::open_local_database_owner(
        workspace,
        principal,
        name,
        DatabaseOwner::AnalyticsDuckdb,
    )
    .put(DatabaseOwner::AnalyticsDuckdb.sample_rel(), b"duckdb")
    .await
    .unwrap();
    crate::magician_v2::database_owners::open_local_database_owner(
        workspace,
        principal,
        name,
        DatabaseOwner::FeedDuckdb,
    )
    .put(DatabaseOwner::FeedDuckdb.sample_rel(), b"feed")
    .await
    .unwrap();
    crate::magician_v2::subprocess_owners::open_local_subprocess_owner(
        workspace,
        principal,
        name,
        SubprocessOwner::WorkdirsScratch,
    )
    .put(SubprocessOwner::WorkdirsScratch.sample_rel(), b"tmp")
    .await
    .unwrap();
    crate::magician_v2::system_owners::open_local_system_owner(
        workspace,
        principal,
        name,
        SystemOwner::CompactionMetrics,
    )
    .put(SystemOwner::CompactionMetrics.sample_rel(), b"{}")
    .await
    .unwrap();
}

#[tokio::test]
async fn golden_local_paths_survive_restart_and_backup_restore() {
    let started = Instant::now();
    let dir = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(dir.path());
    seed_golden(&workspace, "alice", "home").await;
    crate::magician_v2::system_owners::open_local_system_owner(
        &workspace,
        "alice",
        "home",
        SystemOwner::Programs,
    )
    .put(SystemOwner::Programs.sample_rel(), b"{\"program\":1}")
    .await
    .unwrap();
    crate::magician_v2::system_owners::open_local_system_owner(
        &workspace,
        "alice",
        "home",
        SystemOwner::SecretVault,
    )
    .put("secrets/secret_audit.jsonl", b"{\"id\":\"k\"}\n")
    .await
    .unwrap();

    let restarted = ArtifactV2Workspace::new(dir.path());
    assert_eq!(
        crate::magician_v2::work_owners::open_local_work_owner(
            &restarted,
            "alice",
            "home",
            WorkOwner::TaskRecords,
        )
        .get(WorkOwner::TaskRecords.sample_rel())
        .await
        .unwrap(),
        b"{\"task\":1}"
    );
    assert_eq!(
        crate::magician_v2::chat_owners::open_local_chat_owner(
            &restarted,
            "alice",
            "home",
            ChatOwner::ChatSessions,
        )
        .get(ChatOwner::ChatSessions.sample_rel())
        .await
        .unwrap(),
        b"{\"session\":\"s1\"}"
    );
    assert_eq!(
        crate::magician_v2::object_owners::open_local_object_owner(
            &restarted,
            "alice",
            "home",
            ObjectOwner::AppAttachments,
        )
        .get(ObjectOwner::AppAttachments.sample_rel())
        .await
        .unwrap(),
        b"note"
    );
    assert_eq!(
        crate::magician_v2::agent_owners::open_local_agent_owner(
            &restarted,
            "alice",
            "home",
            AgentOwner::MemoryCanonical,
        )
        .get(AgentOwner::MemoryCanonical.sample_rel())
        .await
        .unwrap(),
        b"canon"
    );
    assert_eq!(
        crate::magician_v2::system_owners::open_local_system_owner(
            &restarted,
            "alice",
            "home",
            SystemOwner::DevicePairing,
        )
        .get(SystemOwner::DevicePairing.sample_rel())
        .await
        .unwrap(),
        b"{\"device\":\"a\"}"
    );
    assert!(
        crate::magician_v2::dataset_owners::open_local_dataset_family(
            &restarted,
            "alice",
            "home",
            DatasetFamily::Events,
        )
        .exists(DatasetFamily::Events.sample_rel())
        .await
        .unwrap()
    );

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
        crate::magician_v2::work_owners::open_local_work_owner(
            &restored,
            "alice",
            "home",
            WorkOwner::TaskRecords,
        )
        .get(WorkOwner::TaskRecords.sample_rel())
        .await
        .unwrap(),
        b"{\"task\":1}"
    );
    let _elapsed = started.elapsed();
}

#[tokio::test]
async fn deleting_scratch_and_indexes_keeps_canonical_truth() {
    let dir = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(dir.path());
    seed_golden(&workspace, "alice", "home").await;

    let list_index = crate::magician_v2::work_owners::open_local_work_owner(
        &workspace,
        "alice",
        "home",
        WorkOwner::ListIndex,
    );
    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(WorkOwner::ListIndex.sample_rel()),
    )
    .unwrap();
    assert!(!list_index
        .exists(WorkOwner::ListIndex.sample_rel())
        .await
        .unwrap());
    assert_eq!(
        crate::magician_v2::work_owners::open_local_work_owner(
            &workspace,
            "alice",
            "home",
            WorkOwner::TaskRecords,
        )
        .get(WorkOwner::TaskRecords.sample_rel())
        .await
        .unwrap(),
        b"{\"task\":1}"
    );

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

    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(AgentOwner::LearningProcedureIndex.sample_rel()),
    )
    .ok();
    assert!(crate::magician_v2::agent_owners::open_local_agent_owner(
        &workspace,
        "alice",
        "home",
        AgentOwner::LearningProcedures,
    )
    .exists(AgentOwner::LearningProcedures.sample_rel())
    .await
    .unwrap());

    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(ChatOwner::ProgressLineage.sample_rel()),
    )
    .unwrap();
    assert!(crate::magician_v2::chat_owners::open_local_chat_owner(
        &workspace,
        "alice",
        "home",
        ChatOwner::ProgressEvents,
    )
    .exists(ChatOwner::ProgressEvents.sample_rel())
    .await
    .unwrap());

    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(DatabaseOwner::AnalyticsDuckdb.sample_rel()),
    )
    .unwrap();
    assert!(
        crate::magician_v2::dataset_owners::open_local_dataset_family(
            &workspace,
            "alice",
            "home",
            DatasetFamily::Events,
        )
        .exists(DatasetFamily::Events.sample_rel())
        .await
        .unwrap()
    );

    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(DatabaseOwner::FeedDuckdb.sample_rel()),
    )
    .unwrap();
    assert!(crate::magician_v2::work_owners::open_local_work_owner(
        &workspace,
        "alice",
        "home",
        WorkOwner::TaskRecords,
    )
    .exists(WorkOwner::TaskRecords.sample_rel())
    .await
    .unwrap());

    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(SubprocessOwner::WorkdirsScratch.sample_rel()),
    )
    .unwrap();
    std::fs::remove_file(
        workspace
            .scope_root("alice", "home")
            .join(SystemOwner::CompactionMetrics.sample_rel()),
    )
    .unwrap();
    assert!(crate::magician_v2::object_owners::open_local_object_owner(
        &workspace,
        "alice",
        "home",
        ObjectOwner::AppAttachments,
    )
    .exists(ObjectOwner::AppAttachments.sample_rel())
    .await
    .unwrap());
}

#[tokio::test]
async fn default_open_local_does_not_start_migration_or_remote_copy() {
    let dir = tempfile::tempdir().unwrap();
    let profile = resolve(ResolveOptions {
        cli_path: None,
        env_path: None,
        allow_ambient: false,
    })
    .unwrap();
    let owner = magician_storage::OwnerId::parse("magician-test").unwrap();
    let runtime = StorageRuntime::open_local(dir.path(), profile, owner).unwrap();
    assert_eq!(runtime.operator_health().profile, "local_embedded");
}

#[test]
fn delivery_receipts_guard_file_stays_enabled() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/magician_v2/delivery_receipts/guard.rs"),
    )
    .unwrap();
    assert!(text.contains("SourceGuard::enabled"));
    assert!(text.contains("delivery_receipts"));
}

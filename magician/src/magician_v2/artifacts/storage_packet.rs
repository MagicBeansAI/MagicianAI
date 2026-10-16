//! Shared local/remote durable-artifact scenarios, migration, and restore.

use std::sync::Arc;

use magician_storage::{MigrationPhase, PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};

use super::durable_store::{
    open_local_durable_artifacts, DurableArtifactAccess, DurableArtifactStore, DurableFrontmatter,
};
use super::migration::DurableArtifactsMigrationHandler;
use super::object_backend::{open_local_object_backend, ObjectDurableStore};
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;

fn fm(ns: &str, name: &str) -> DurableFrontmatter {
    DurableFrontmatter {
        namespace: ns.to_string(),
        name: name.to_string(),
        created_by: "test".to_string(),
        last_updated_by: "test".to_string(),
        last_updated: chrono::Utc::now(),
        content_type: Some("text/markdown".to_string()),
        source_execution_id: None,
        source_task_id: None,
        source_workflow_instance_id: None,
        source_run_id: None,
        source_cycle_id: None,
        source_agent_id: None,
        producer_stage: None,
    }
}

async fn run_scenarios(store: &dyn DurableArtifactAccess) {
    let large = "x".repeat(64 * 1024);
    store
        .write(
            "billing",
            "payments.md",
            &large,
            fm("billing", "payments.md"),
        )
        .await
        .unwrap();
    store
        .write(
            "billing",
            "payments.md",
            &large,
            fm("billing", "payments.md"),
        )
        .await
        .unwrap();
    store
        .append("billing", "payments.md", "\ntail\n")
        .await
        .unwrap();
    let (read_fm, body) = store.read("billing", "payments.md").await.unwrap();
    assert_eq!(read_fm.namespace, "billing");
    assert_eq!(read_fm.name, "payments.md");
    assert!(body.contains("tail"));
    assert!(body.len() > 64 * 1024);
    assert!(store
        .artifact_exists("billing", "payments.md")
        .await
        .unwrap());
    let listed = store.list_entries(Some("billing")).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].namespace, "billing");
    assert_eq!(listed[0].name, "payments.md");
    store.delete("billing", "payments.md").await.unwrap();
    assert!(!store
        .artifact_exists("billing", "payments.md")
        .await
        .unwrap());
}

#[tokio::test]
async fn local_and_object_backends_share_scenarios() {
    let local_dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let local = DurableArtifactStore::new(local_dir.path().join("artifacts")).unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let remote = ObjectDurableStore::new(objects, "alice", "home", scratch.path());
    run_scenarios(&local).await;
    run_scenarios(&remote).await;
}

#[tokio::test]
async fn default_layout_stays_under_durable_artifacts() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(tmp.path());
    let store = open_local_durable_artifacts(&workspace, "alice", "home").unwrap();
    store
        .write("ns", "a.md", "body", fm("ns", "a.md"))
        .await
        .unwrap();
    assert!(store.exists("ns", "a.md"));
    let expected = workspace
        .durable_artifacts_root("alice", "home")
        .join("ns")
        .join("a.md");
    assert!(expected.is_file());
}

#[tokio::test]
async fn traversal_and_tamper_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let store = DurableArtifactStore::new(tmp.path().join("artifacts")).unwrap();
    assert!(store
        .write("../x", "a.md", "nope", fm("x", "a.md"))
        .await
        .is_err());
    store
        .write("ns", "a.md", "body", fm("ns", "a.md"))
        .await
        .unwrap();
    let path = store.resolve_path_safe("ns", "a.md").unwrap();
    std::fs::write(&path, "not-frontmatter").unwrap();
    assert!(store.read("ns", "a.md").await.is_err());
}

#[tokio::test]
async fn object_backend_isolates_scopes_and_keeps_public_locators() {
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let alice = ObjectDurableStore::new(
        objects.clone(),
        "alice",
        "home",
        scratch.path().join("alice"),
    );
    let bob = ObjectDurableStore::new(objects, "bob", "home", scratch.path().join("bob"));
    alice
        .write("ns", "a.md", "secret", fm("ns", "a.md"))
        .await
        .unwrap();
    assert!(!bob.artifact_exists("ns", "a.md").await.unwrap());
    let listed = alice.list_entries(None).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].namespace, "ns");
    assert_eq!(listed[0].name, "a.md");
}

#[tokio::test]
async fn migration_resume_rollback_and_fresh_host_restore() {
    let local_dir = tempfile::tempdir().unwrap();
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let ledger = tempfile::tempdir().unwrap();
    let local = Arc::new(DurableArtifactStore::new(local_dir.path().join("artifacts")).unwrap());
    local
        .write("ns", "a.md", "payload", fm("ns", "a.md"))
        .await
        .unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let remote = Arc::new(ObjectDurableStore::new(
        objects,
        "alice",
        "home",
        scratch.path(),
    ));
    let handler = Arc::new(DurableArtifactsMigrationHandler::new(
        local.clone(),
        remote.clone(),
    ));
    let mut registry = OwnerMigrationRegistry::new();
    let boxed: Arc<dyn OwnerMigrationHandler> = handler;
    registry.register(boxed).unwrap();
    let coord = MigrationCoordinator::open(ledger.path(), registry)
        .await
        .unwrap();
    let scope = StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    ));
    let planned = coord
        .plan(PlanRequest {
            store_id: magician_storage::StorageCatalogId::parse("durable_artifacts").unwrap(),
            scope: scope.clone(),
            source_profile: "local_embedded".into(),
            target_profile: "remote_durable".into(),
        })
        .await
        .unwrap();
    coord.export(planned.migration_id).await.unwrap();
    coord.import(planned.migration_id).await.unwrap();
    coord.semantic_verify(planned.migration_id).await.unwrap();
    coord
        .prepare_cutover(planned.migration_id, 0)
        .await
        .unwrap();
    let fence = coord
        .status(planned.migration_id)
        .await
        .unwrap()
        .record
        .cutover_fencing_generation
        .unwrap();
    coord.cutover(planned.migration_id, fence).await.unwrap();
    assert!(remote.artifact_exists("ns", "a.md").await.unwrap());
    let rolled = coord.rollback(planned.migration_id, fence).await.unwrap();
    assert_eq!(rolled.phase, MigrationPhase::RolledBack);
    coord
        .settle(
            planned.migration_id,
            rolled.cutover_fencing_generation.unwrap(),
        )
        .await
        .unwrap();

    let restore_dir = tempfile::tempdir().unwrap();
    let restore_scratch = tempfile::tempdir().unwrap();
    let restore_objects = open_local_object_backend(restore_dir.path()).unwrap();
    let restored =
        ObjectDurableStore::new(restore_objects, "alice", "home", restore_scratch.path());
    let dump = local.export_all().await.unwrap();
    restored.import_all(&dump).await.unwrap();
    let (_, body) = restored.read("ns", "a.md").await.unwrap();
    assert_eq!(body, "payload");
}

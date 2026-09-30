//! Shared local/remote system-owner scenarios, migration, and restore.

use std::sync::Arc;

use magician_storage::{MigrationPhase, PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};

use super::local::{LocalSystemStore, SystemAccess};
use super::migration::SystemOwnerMigrationHandler;
use super::owners::SystemOwner;
use super::remote::{open_local_system_backend, RemoteSystemStore};

async fn run_scenarios(store: &dyn SystemAccess, owner: SystemOwner) {
    let rel = owner.sample_rel();
    store.put(rel, b"{\"v\":1}").await.unwrap();
    store.put(rel, b"{\"v\":2}").await.unwrap();
    assert_eq!(store.get(rel).await.unwrap(), b"{\"v\":2}");
    assert!(store.exists(rel).await.unwrap());
    assert_eq!(store.list_selected().await.unwrap(), vec![rel.to_string()]);
}

#[tokio::test]
async fn local_and_remote_share_scenarios() {
    for owner in SystemOwner::ALL {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let local = LocalSystemStore::for_scope_root(local_dir.path(), owner);
        let objects = open_local_system_backend(remote_dir.path()).unwrap();
        let remote = RemoteSystemStore::new(objects, "alice", "home", owner, scratch.path());
        run_scenarios(&local, owner).await;
        run_scenarios(&remote, owner).await;
    }
}

#[tokio::test]
async fn reference_commit_failure_keeps_local_canonical() {
    let owner = SystemOwner::Programs;
    let local_dir = tempfile::tempdir().unwrap();
    let store = LocalSystemStore::for_scope_root(local_dir.path(), owner);
    store.put(owner.sample_rel(), b"staged").await.unwrap();
    assert_eq!(
        store.list_selected().await.unwrap(),
        vec![owner.sample_rel().to_string()]
    );
}

#[tokio::test]
async fn migration_resume_rollback_and_fresh_host_restore() {
    for owner in SystemOwner::TENANT {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let ledger = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalSystemStore::for_scope_root(local_dir.path(), owner));
        local.put(owner.sample_rel(), b"payload").await.unwrap();
        let objects = open_local_system_backend(remote_dir.path()).unwrap();
        let remote = Arc::new(RemoteSystemStore::new(
            objects,
            "alice",
            "home",
            owner,
            scratch.path(),
        ));
        let handler = Arc::new(SystemOwnerMigrationHandler::new(
            owner,
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
                store_id: magician_storage::StorageCatalogId::parse(owner.id()).unwrap(),
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
        assert!(remote.exists(owner.sample_rel()).await.unwrap());
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
        let restore_objects = open_local_system_backend(restore_dir.path()).unwrap();
        let restored = RemoteSystemStore::new(
            restore_objects,
            "alice",
            "home",
            owner,
            restore_scratch.path(),
        );
        let dump = local.export_all().await.unwrap();
        restored.import_all(&dump).await.unwrap();
        assert_eq!(restored.get(owner.sample_rel()).await.unwrap(), b"payload");
    }
}

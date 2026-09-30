//! Shared local/remote Parquet scenarios, migration, restore, and glob parity.

use std::sync::Arc;

use magician_storage::{MigrationPhase, PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};

use super::family::DatasetFamily;
use super::local::{DatasetAccess, LocalFamilyStore};
use super::migration::DatasetFamilyMigrationHandler;
use super::remote::{open_local_dataset_backend, RemoteFamilyStore};

async fn run_scenarios(store: &dyn DatasetAccess, family: DatasetFamily) {
    let rel = family.sample_rel();
    let large = vec![b'P'; 64 * 1024];
    store.put(rel, &large).await.unwrap();
    store.put(rel, &large).await.unwrap();
    let body = store.get(rel).await.unwrap();
    assert_eq!(body.len(), 64 * 1024);
    assert!(store.exists(rel).await.unwrap());
    let listed = store.list_selected().await.unwrap();
    assert_eq!(listed, vec![rel.to_string()]);
}

#[tokio::test]
async fn local_and_dataset_backends_share_scenarios() {
    for family in DatasetFamily::ALL {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let local = LocalFamilyStore::for_root(local_dir.path(), family);
        let datasets = open_local_dataset_backend(remote_dir.path());
        let remote = RemoteFamilyStore::new(datasets, family, scratch.path()).unwrap();
        run_scenarios(&local, family).await;
        run_scenarios(&remote, family).await;
    }
}

#[tokio::test]
async fn dataset_backend_isolates_families_and_keeps_public_locators() {
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let datasets = open_local_dataset_backend(remote_dir.path());
    let events = RemoteFamilyStore::new(
        datasets.clone(),
        DatasetFamily::Events,
        scratch.path().join("events"),
    )
    .unwrap();
    let memory = RemoteFamilyStore::new(
        datasets,
        DatasetFamily::MemoryEvents,
        scratch.path().join("memory"),
    )
    .unwrap();
    events
        .put(DatasetFamily::Events.sample_rel(), b"ev")
        .await
        .unwrap();
    assert!(!memory
        .exists(DatasetFamily::MemoryEvents.sample_rel())
        .await
        .unwrap());
    let listed = events.list_selected().await.unwrap();
    assert_eq!(listed, vec![DatasetFamily::Events.sample_rel().to_string()]);
}

#[tokio::test]
async fn reference_commit_failure_keeps_local_canonical() {
    let family = DatasetFamily::LlmCalls;
    let local_dir = tempfile::tempdir().unwrap();
    let store = LocalFamilyStore::for_root(local_dir.path(), family);
    assert!(store.list_selected().await.unwrap().is_empty());
    store.put(family.sample_rel(), b"staged").await.unwrap();
    assert_eq!(
        store.list_selected().await.unwrap(),
        vec![family.sample_rel().to_string()]
    );
}

#[tokio::test]
async fn migration_resume_rollback_and_fresh_host_restore() {
    for family in DatasetFamily::ALL {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let ledger = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalFamilyStore::for_root(local_dir.path(), family));
        local.put(family.sample_rel(), b"payload").await.unwrap();
        let datasets = open_local_dataset_backend(remote_dir.path());
        let remote = Arc::new(RemoteFamilyStore::new(datasets, family, scratch.path()).unwrap());
        let handler = Arc::new(DatasetFamilyMigrationHandler::new(
            family,
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
                store_id: magician_storage::StorageCatalogId::parse(family.id()).unwrap(),
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
        assert!(remote.exists(family.sample_rel()).await.unwrap());
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
        let restore_datasets = open_local_dataset_backend(restore_dir.path());
        let restored =
            RemoteFamilyStore::new(restore_datasets, family, restore_scratch.path()).unwrap();
        let dump = local.export_all().await.unwrap();
        restored.import_all(&dump).await.unwrap();
        assert_eq!(restored.get(family.sample_rel()).await.unwrap(), b"payload");
    }
}

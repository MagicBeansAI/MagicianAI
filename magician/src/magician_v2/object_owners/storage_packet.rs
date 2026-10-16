//! Shared local/remote scenarios, migration, restore, and locator isolation.

use std::sync::Arc;

use magician_storage::{
    bytes_body, MigrationPhase, PrincipalId, PutCondition, PutObjectRequest, ScopeId,
    StoragePrefix, StorageScope, WorkspaceId,
};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};

use super::blob::{BlobAccess, LocalTreeStore};
use super::migration::ObjectOwnerMigrationHandler;
use super::object_backend::{open_local_object_backend, ObjectTreeStore};
use super::owners::ObjectOwner;

async fn run_scenarios(store: &dyn BlobAccess, owner: ObjectOwner) {
    let rel = owner.sample_rel();
    let large = vec![b'x'; 64 * 1024];
    store.put(rel, &large).await.unwrap();
    store.put(rel, &large).await.unwrap();
    let body = store.get(rel).await.unwrap();
    assert_eq!(body.len(), 64 * 1024);
    assert!(store.exists(rel).await.unwrap());
    let listed = store.list().await.unwrap();
    assert_eq!(listed, vec![rel.to_string()]);
    store.delete(rel).await.unwrap();
    assert!(!store.exists(rel).await.unwrap());
}

#[tokio::test]
async fn local_and_object_backends_share_scenarios() {
    for owner in ObjectOwner::ALL {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let local = LocalTreeStore::for_scope_root(local_dir.path(), owner);
        let objects = open_local_object_backend(remote_dir.path()).unwrap();
        let remote = ObjectTreeStore::new(objects, "alice", "home", owner, scratch.path());
        run_scenarios(&local, owner).await;
        run_scenarios(&remote, owner).await;
    }
}

#[tokio::test]
async fn object_backend_rejects_tampered_digest() {
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let owner = ObjectOwner::ExecutionDownloads;
    let store = ObjectTreeStore::new(objects.clone(), "alice", "home", owner, scratch.path());
    store.put(owner.sample_rel(), b"payload").await.unwrap();
    let listed = objects
        .list_diagnostic(
            &StoragePrefix {
                encoded: format!("t/alice/home/{}/", owner.id()),
            },
            None,
            10,
        )
        .await
        .unwrap();
    let key = listed[0].key.clone();
    objects
        .put(PutObjectRequest {
            key: key.clone(),
            body: bytes_body(b"{\"path\":\"x\",\"b64\":\"YQ==\",\"digest\":\"00\"}".to_vec()),
            content_type: Some("application/json".into()),
            condition: PutCondition::Overwrite,
        })
        .await
        .unwrap();
    assert!(store.get(owner.sample_rel()).await.is_err());
}

#[tokio::test]
async fn object_backend_orphan_delete_drops_locator() {
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let owner = ObjectOwner::ExecutionRecordings;
    let store = ObjectTreeStore::new(objects, "alice", "home", owner, scratch.path());
    store.put(owner.sample_rel(), b"clip").await.unwrap();
    store.delete(owner.sample_rel()).await.unwrap();
    assert!(!store.exists(owner.sample_rel()).await.unwrap());
    assert!(store.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn cancelled_put_leaves_no_locator() {
    let owner = ObjectOwner::ExecutionDownloads;
    let local_dir = tempfile::tempdir().unwrap();
    let store = LocalTreeStore::for_scope_root(local_dir.path(), owner);
    let cancel = tokio_util::sync::CancellationToken::new();
    cancel.cancel();
    tokio::select! {
        _ = cancel.cancelled() => {}
        _ = store.put(owner.sample_rel(), b"nope") => panic!("cancelled put should not run"),
    }
    assert!(!store.exists(owner.sample_rel()).await.unwrap());
}

#[tokio::test]
async fn unpublished_part_is_not_listed() {
    let owner = ObjectOwner::AppPackages;
    let local_dir = tempfile::tempdir().unwrap();
    let store = LocalTreeStore::for_scope_root(local_dir.path(), owner);
    assert!(store.list().await.unwrap().is_empty());
    store.put(owner.sample_rel(), b"staged").await.unwrap();
    assert_eq!(
        store.list().await.unwrap(),
        vec![owner.sample_rel().to_string()]
    );
}

#[tokio::test]
async fn object_backend_isolates_scopes_and_keeps_public_locators() {
    let remote_dir = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let objects = open_local_object_backend(remote_dir.path()).unwrap();
    let owner = ObjectOwner::AppPackages;
    let alice = ObjectTreeStore::new(
        objects.clone(),
        "alice",
        "home",
        owner,
        scratch.path().join("alice"),
    );
    let bob = ObjectTreeStore::new(objects, "bob", "home", owner, scratch.path().join("bob"));
    alice.put(owner.sample_rel(), b"secret").await.unwrap();
    assert!(!bob.exists(owner.sample_rel()).await.unwrap());
    let listed = alice.list().await.unwrap();
    assert_eq!(listed, vec![owner.sample_rel().to_string()]);
}

#[tokio::test]
async fn migration_resume_rollback_and_fresh_host_restore() {
    for owner in ObjectOwner::ALL {
        let local_dir = tempfile::tempdir().unwrap();
        let remote_dir = tempfile::tempdir().unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let ledger = tempfile::tempdir().unwrap();
        let local = Arc::new(LocalTreeStore::for_scope_root(local_dir.path(), owner));
        local.put(owner.sample_rel(), b"payload").await.unwrap();
        let objects = open_local_object_backend(remote_dir.path()).unwrap();
        let remote = Arc::new(ObjectTreeStore::new(
            objects,
            "alice",
            "home",
            owner,
            scratch.path(),
        ));
        let handler = Arc::new(ObjectOwnerMigrationHandler::new(
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
        let restore_objects = open_local_object_backend(restore_dir.path()).unwrap();
        let restored = ObjectTreeStore::new(
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

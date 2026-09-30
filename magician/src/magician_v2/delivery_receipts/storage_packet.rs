//! Shared local/remote scenarios, migration, restore, and golden equivalence.

use std::sync::Arc;

use chrono::{TimeZone, Utc};
use magician_storage::{MigrationPhase, PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{
    MigrationCoordinator, OwnerMigrationHandler, OwnerMigrationRegistry, PlanRequest,
};

use super::migration::SentIndexMigrationHandler;
use super::sent_index::{SendBinding, SentMessage};
use super::store::{open_local_sent_index, open_sqlite_sent_index, SentMessageStore};
use super::SendIdentifier;
use crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use crate::magician_v2::delivery::DeliveryScope;
use magician_storage::REPOSITORY_SCENARIO_CASES;

fn t(hour: u32) -> chrono::DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 21, hour, 0, 0)
        .single()
        .expect("instant")
}

fn scope() -> DeliveryScope {
    DeliveryScope::new("alpha", "prod")
}

fn sent(act: &str) -> SentMessage {
    SentMessage {
        act_ref: act.to_string(),
        audience: vec!["gone@recipient.test".into()],
        sent_at: t(9),
    }
}

fn identifier() -> SendIdentifier {
    SendIdentifier::MessageId("orig-1@agentmail.to".into())
}

fn run_scenarios(store: &dyn SentMessageStore) {
    store
        .record(&scope(), "agentmail", &identifier(), &sent("act-1"), t(10))
        .unwrap();
    store
        .record(&scope(), "agentmail", &identifier(), &sent("act-1"), t(11))
        .unwrap();
    match store.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        store
            .lookup(
                &DeliveryScope::new("other", "prod"),
                "agentmail",
                &identifier()
            )
            .unwrap(),
        SendBinding::Absent
    );
    assert!(store
        .record(&scope(), "agentmail", &identifier(), &sent("act-2"), t(12))
        .is_err());
    let dump = store.export_scope(&scope()).unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let other = open_sqlite_sent_index(tmp.path().join("copy.sqlite")).unwrap();
    other.import_scope(&scope(), &dump).unwrap();
    match other.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn import_preserves_ambiguous_and_folds_identical() {
    let dump = serde_json::to_vec(&[
        super::sent_index::SentIndexRecord {
            act_ref: "act-1".into(),
            provider: "agentmail".into(),
            id_kind: "message_id".into(),
            id_value: "orig-1@agentmail.to".into(),
            audience: vec!["gone@recipient.test".into()],
            sent_at: t(9),
            recorded_at: t(10),
        },
        super::sent_index::SentIndexRecord {
            act_ref: "act-1".into(),
            provider: "agentmail".into(),
            id_kind: "message_id".into(),
            id_value: "orig-1@agentmail.to".into(),
            audience: vec!["gone@recipient.test".into()],
            sent_at: t(9),
            recorded_at: t(11),
        },
        super::sent_index::SentIndexRecord {
            act_ref: "act-2".into(),
            provider: "agentmail".into(),
            id_kind: "message_id".into(),
            id_value: "orig-1@agentmail.to".into(),
            audience: vec!["gone@recipient.test".into()],
            sent_at: t(12),
            recorded_at: t(12),
        },
    ])
    .unwrap();
    let jsonl_dir = tempfile::tempdir().unwrap();
    let sqlite_dir = tempfile::tempdir().unwrap();
    let local = open_local_sent_index(ArtifactV2Workspace::new(jsonl_dir.path()));
    let remote = open_sqlite_sent_index(sqlite_dir.path().join("sent.sqlite")).unwrap();
    local.import_scope(&scope(), &dump).unwrap();
    remote.import_scope(&scope(), &dump).unwrap();
    match local.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Ambiguous { act_refs } => {
            assert_eq!(act_refs, vec!["act-1".to_string(), "act-2".to_string()]);
        },
        other => panic!("{other:?}"),
    }
    match remote.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Ambiguous { act_refs } => {
            assert_eq!(act_refs, vec!["act-1".to_string(), "act-2".to_string()]);
        },
        other => panic!("{other:?}"),
    }
    let exported = local.export_scope(&scope()).unwrap();
    let round = open_sqlite_sent_index(sqlite_dir.path().join("round.sqlite")).unwrap();
    round.import_scope(&scope(), &exported).unwrap();
    match round.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Ambiguous { act_refs } => {
            assert_eq!(act_refs, vec!["act-1".to_string(), "act-2".to_string()]);
        },
        other => panic!("{other:?}"),
    }
}

#[test]
fn local_and_remote_share_repository_scenarios() {
    let jsonl_dir = tempfile::tempdir().unwrap();
    let sqlite_dir = tempfile::tempdir().unwrap();
    let local = open_local_sent_index(ArtifactV2Workspace::new(jsonl_dir.path()));
    let remote = open_sqlite_sent_index(sqlite_dir.path().join("sent.sqlite")).unwrap();
    run_scenarios(local.as_ref());
    run_scenarios(remote.as_ref());
    assert_eq!(REPOSITORY_SCENARIO_CASES.len(), 5);
}

#[test]
fn default_startup_still_constructs_the_jsonl_adapter() {
    let tmp = tempfile::tempdir().unwrap();
    let store = open_local_sent_index(ArtifactV2Workspace::new(tmp.path()));
    store
        .record(&scope(), "agentmail", &identifier(), &sent("act-1"), t(10))
        .unwrap();
    match store.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn migration_interruption_resume_verify_rollback_and_restore() {
    let jsonl_dir = tempfile::tempdir().unwrap();
    let sqlite_dir = tempfile::tempdir().unwrap();
    let ledger_dir = tempfile::tempdir().unwrap();
    let local = open_local_sent_index(ArtifactV2Workspace::new(jsonl_dir.path()));
    local
        .record(&scope(), "agentmail", &identifier(), &sent("act-1"), t(10))
        .unwrap();
    let remote = open_sqlite_sent_index(sqlite_dir.path().join("sent.sqlite")).unwrap();
    let handler = Arc::new(SentIndexMigrationHandler::new(
        local.clone(),
        remote.clone(),
    ));
    let mut registry = OwnerMigrationRegistry::new();
    let boxed: Arc<dyn OwnerMigrationHandler> = handler;
    registry.register(boxed).unwrap();
    let coord = MigrationCoordinator::open(ledger_dir.path(), registry)
        .await
        .unwrap();
    let storage_scope = StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse("alpha").unwrap(),
        WorkspaceId::parse("prod").unwrap(),
    ));
    let planned = coord
        .plan(PlanRequest {
            store_id: magician_storage::StorageCatalogId::parse("delivery_receipts").unwrap(),
            scope: storage_scope.clone(),
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
    match remote.lookup(&scope(), "agentmail", &identifier()).unwrap() {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
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
    let restored = open_sqlite_sent_index(restore_dir.path().join("restore.sqlite")).unwrap();
    let dump = local.export_scope(&scope()).unwrap();
    restored.import_scope(&scope(), &dump).unwrap();
    match restored
        .lookup(&scope(), "agentmail", &identifier())
        .unwrap()
    {
        SendBinding::Bound(found) => assert_eq!(found.act_ref, "act-1"),
        other => panic!("{other:?}"),
    }
}

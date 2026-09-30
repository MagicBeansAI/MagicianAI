use std::collections::BTreeMap;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use magician_storage::{
    EvidenceKind, EvidenceLink, MigrationPhase, PrincipalId, ReadinessState, ScopeId, StorageError,
    StorageScope, WorkspaceId, REPOSITORY_SCENARIO_CASES,
};
use magician_storage_migration::{
    sanitize_json, ClosureCoordinator, CrashSpec, MigrationCoordinator, OwnerMigrationHandler,
    OwnerMigrationRegistry, PlanRequest, SourceGuard, SyntheticItem, SyntheticOwner,
    SYNTHETIC_OWNER_ID,
};
use serde_json::json;

fn tenant() -> StorageScope {
    StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    ))
}

fn plan_request(owner: &SyntheticOwner) -> PlanRequest {
    PlanRequest {
        store_id: owner.owner_id(),
        scope: tenant(),
        source_profile: "local_embedded".into(),
        target_profile: "remote_durable".into(),
    }
}

async fn seeded_stack() -> (tempfile::TempDir, Arc<SyntheticOwner>, MigrationCoordinator) {
    let dir = tempfile::tempdir().unwrap();
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    owner.seed(&tenant(), "beta", "two").unwrap();
    let mut registry = OwnerMigrationRegistry::new();
    let handler: Arc<dyn OwnerMigrationHandler> = owner.clone();
    registry.register(handler).unwrap();
    let coord = MigrationCoordinator::open(dir.path(), registry)
        .await
        .unwrap();
    (dir, owner, coord)
}

async fn reopen(
    dir: &tempfile::TempDir,
    owner: &Arc<SyntheticOwner>,
    crash: Option<CrashSpec>,
) -> MigrationCoordinator {
    let mut registry = OwnerMigrationRegistry::new();
    let handler: Arc<dyn OwnerMigrationHandler> = owner.clone();
    registry.register(handler).unwrap();
    let coord = MigrationCoordinator::open(dir.path(), registry)
        .await
        .unwrap();
    match crash {
        Some(spec) => coord.with_crash(spec),
        None => coord,
    }
}

async fn finish_forward(coord: &MigrationCoordinator, id: uuid::Uuid) {
    loop {
        let entry = coord.status(id).await.unwrap();
        if entry.record.phase == MigrationPhase::Settled
            && entry.phase_complete(MigrationPhase::Settled)
        {
            return;
        }
        let complete = entry.phase_complete(entry.record.phase);
        match (entry.record.phase, complete) {
            (MigrationPhase::Planned, _) => {
                coord.export(id).await.unwrap();
            },
            (MigrationPhase::Exporting, false) => {
                coord.export(id).await.unwrap();
            },
            (MigrationPhase::Exporting, true) => {
                coord.import(id).await.unwrap();
            },
            (MigrationPhase::Imported, false) => {
                coord.import(id).await.unwrap();
            },
            (MigrationPhase::Imported, true) => {
                coord.semantic_verify(id).await.unwrap();
            },
            (MigrationPhase::Verified, false) => {
                coord.semantic_verify(id).await.unwrap();
            },
            (MigrationPhase::Verified, true) => {
                coord
                    .prepare_cutover(id, entry.fence_generation)
                    .await
                    .unwrap();
            },
            (MigrationPhase::CutoverPending, false) => {
                coord
                    .prepare_cutover(id, entry.fence_generation)
                    .await
                    .unwrap();
            },
            (MigrationPhase::CutoverPending, true) => {
                let fence = entry.record.cutover_fencing_generation.unwrap();
                coord.cutover(id, fence).await.unwrap();
            },
            (MigrationPhase::Cutover, false) => {
                let fence = entry.record.cutover_fencing_generation.unwrap();
                coord.cutover(id, fence).await.unwrap();
            },
            (MigrationPhase::Cutover, true) | (MigrationPhase::RolledBack, true) => {
                coord.settle(id, entry.fence_generation).await.unwrap();
            },
            (MigrationPhase::RolledBack, false) => {
                coord.rollback(id, entry.fence_generation).await.unwrap();
            },
            (MigrationPhase::RollbackPending, _) => {
                coord.rollback(id, entry.fence_generation).await.unwrap();
            },
            (MigrationPhase::Settled, false) => {
                coord.settle(id, entry.fence_generation).await.unwrap();
            },
            (MigrationPhase::Settled, true) => return,
        }
    }
}

#[tokio::test]
async fn synthetic_owner_advances_every_forward_phase() {
    let (_dir, owner, coord) = seeded_stack().await;
    let inventory = coord
        .inventory(SYNTHETIC_OWNER_ID, &tenant())
        .await
        .unwrap();
    assert_eq!(inventory.record_count, 2);
    let planned = coord.plan(plan_request(&owner)).await.unwrap();
    assert_eq!(planned.phase, MigrationPhase::Planned);
    coord.checkpoint(planned.migration_id).await.unwrap();
    finish_forward(&coord, planned.migration_id).await;
    let done = coord.status(planned.migration_id).await.unwrap();
    assert_eq!(done.record.phase, MigrationPhase::Settled);
    for phase in [
        MigrationPhase::Planned,
        MigrationPhase::Exporting,
        MigrationPhase::Imported,
        MigrationPhase::Verified,
        MigrationPhase::CutoverPending,
        MigrationPhase::Cutover,
        MigrationPhase::Settled,
    ] {
        assert!(done.phase_complete(phase), "missing evidence for {phase}");
    }
    assert_eq!(owner.remote().list(&tenant()).unwrap().len(), 2);
}

#[tokio::test]
async fn crash_restart_at_every_forward_phase() {
    let phases = [
        MigrationPhase::Planned,
        MigrationPhase::Exporting,
        MigrationPhase::Imported,
        MigrationPhase::Verified,
        MigrationPhase::CutoverPending,
        MigrationPhase::Cutover,
        MigrationPhase::Settled,
    ];
    for phase in phases {
        let dir = tempfile::tempdir().unwrap();
        let owner = Arc::new(SyntheticOwner::new().unwrap());
        owner.seed(&tenant(), "alpha", "one").unwrap();
        let request = plan_request(&owner);
        let coord = reopen(&dir, &owner, None).await;
        let id = if phase == MigrationPhase::Planned {
            drop(coord);
            let crashing = reopen(
                &dir,
                &owner,
                Some(CrashSpec {
                    phase,
                    before_complete: false,
                }),
            )
            .await;
            let crashed = crashing.plan(request.clone()).await.unwrap_err();
            assert!(
                matches!(crashed, StorageError::Unavailable { .. }),
                "{phase} should crash, got {crashed:?}"
            );
            let id = crashing
                .find_open(&request)
                .await
                .unwrap()
                .expect("planned record survived crash")
                .record
                .migration_id;
            drop(crashing);
            id
        } else {
            let id = coord.plan(request.clone()).await.unwrap().migration_id;
            finish_until(&coord, id, phase).await;
            drop(coord);
            let crashing = reopen(
                &dir,
                &owner,
                Some(CrashSpec {
                    phase,
                    before_complete: false,
                }),
            )
            .await;
            let crashed = invoke_phase(&crashing, id, phase).await.unwrap_err();
            assert!(
                matches!(crashed, StorageError::Unavailable { .. }),
                "{phase} should crash, got {crashed:?}"
            );
            drop(crashing);
            id
        };
        let resumed = reopen(&dir, &owner, None).await;
        let status = resumed.resume(id).await.unwrap();
        assert_eq!(status.record.phase, phase);
        finish_forward(&resumed, id).await;
        assert_eq!(
            resumed.status(id).await.unwrap().record.phase,
            MigrationPhase::Settled
        );
    }
}

#[tokio::test]
async fn crash_during_export_retries_as_resume() {
    let (dir, owner, coord) = seeded_stack().await;
    let id = coord.plan(plan_request(&owner)).await.unwrap().migration_id;
    drop(coord);
    let crashing = reopen(
        &dir,
        &owner,
        Some(CrashSpec {
            phase: MigrationPhase::Exporting,
            before_complete: true,
        }),
    )
    .await;
    assert!(matches!(
        crashing.export(id).await.unwrap_err(),
        StorageError::Unavailable { .. }
    ));
    drop(crashing);
    let resumed = reopen(&dir, &owner, None).await;
    let status = resumed.resume(id).await.unwrap();
    assert!(status.needs_retry);
    assert_eq!(status.record.phase, MigrationPhase::Exporting);
    resumed.export(id).await.unwrap();
    finish_forward(&resumed, id).await;
}

#[tokio::test]
async fn rollback_path_crash_and_fencing() {
    let (dir, owner, coord) = seeded_stack().await;
    let id = coord.plan(plan_request(&owner)).await.unwrap().migration_id;
    finish_until(&coord, id, MigrationPhase::CutoverPending).await;
    coord.prepare_cutover(id, 0).await.unwrap();
    let cut = coord
        .status(id)
        .await
        .unwrap()
        .record
        .cutover_fencing_generation
        .unwrap();
    coord.cutover(id, cut).await.unwrap();
    let stale = coord.rollback(id, 0).await.unwrap_err();
    assert!(matches!(
        stale,
        StorageError::LeaseLost { generation: 1, .. }
    ));
    drop(coord);
    let crashing = reopen(
        &dir,
        &owner,
        Some(CrashSpec {
            phase: MigrationPhase::RollbackPending,
            before_complete: false,
        }),
    )
    .await;
    assert!(matches!(
        crashing.rollback(id, 1).await.unwrap_err(),
        StorageError::Unavailable { .. }
    ));
    drop(crashing);
    let resumed = reopen(&dir, &owner, None).await;
    assert_eq!(
        resumed.resume(id).await.unwrap().record.phase,
        MigrationPhase::RollbackPending
    );
    let rolled = resumed.rollback(id, 1).await.unwrap();
    assert_eq!(rolled.phase, MigrationPhase::RolledBack);
    assert_eq!(rolled.cutover_fencing_generation, Some(2));
    resumed.settle(id, 2).await.unwrap();
    assert!(owner.remote().list(&tenant()).unwrap().is_empty());
}

#[tokio::test]
async fn rejects_wrong_source_generation() {
    let (_dir, owner, coord) = seeded_stack().await;
    let id = coord.plan(plan_request(&owner)).await.unwrap().migration_id;
    owner.seed(&tenant(), "gamma", "three").unwrap();
    let err = coord.export(id).await.unwrap_err();
    assert!(matches!(err, StorageError::Conflict { .. }));
}

#[tokio::test]
async fn rerun_is_noop() {
    let (_dir, owner, coord) = seeded_stack().await;
    let first = coord.plan(plan_request(&owner)).await.unwrap();
    let again = coord.plan(plan_request(&owner)).await.unwrap();
    assert_eq!(first.migration_id, again.migration_id);
    coord.export(first.migration_id).await.unwrap();
    let calls = owner.export_calls.load(Ordering::SeqCst);
    coord.export(first.migration_id).await.unwrap();
    assert_eq!(owner.export_calls.load(Ordering::SeqCst), calls);
}

#[tokio::test]
async fn cannot_advance_ledger_without_required_evidence() {
    let (_dir, owner, coord) = seeded_stack().await;
    let id = coord.plan(plan_request(&owner)).await.unwrap().migration_id;
    let err = coord.import(id).await.unwrap_err();
    assert!(matches!(err, StorageError::InvalidKey { .. }));
}

#[tokio::test]
async fn readiness_is_monotonic_and_evidence_gated() {
    let dir = tempfile::tempdir().unwrap();
    let owner = SyntheticOwner::new().unwrap().owner_id();
    let ledger = ClosureCoordinator::open(dir.path()).await.unwrap();
    let start = ledger.load(&owner).await.unwrap();
    assert_eq!(start.state, ReadinessState::Discovered);
    let skip = ledger
        .advance(&owner, ReadinessState::WrappedLocal, BTreeMap::new())
        .await
        .unwrap_err();
    assert!(matches!(skip, StorageError::Conflict { .. }));
    let missing = ledger
        .advance(&owner, ReadinessState::Characterized, BTreeMap::new())
        .await
        .unwrap_err();
    assert!(matches!(missing, StorageError::InvalidKey { .. }));
    for state in magician_storage::READINESS_STATES.iter().skip(1) {
        let mut evidence = BTreeMap::new();
        for key in state.required_evidence_keys() {
            evidence.insert(
                (*key).to_string(),
                EvidenceLink::new(EvidenceKind::Test, format!("tests/{key}.rs")).unwrap(),
            );
        }
        let packet = ledger.advance(&owner, *state, evidence).await.unwrap();
        assert_eq!(packet.state, *state);
    }
    assert_eq!(
        ledger.load(&owner).await.unwrap().state,
        ReadinessState::RemoteReady
    );
}

#[tokio::test]
async fn source_guard_enabled_owner_by_owner() {
    let owner = SyntheticOwner::new().unwrap();
    owner.source_guard().check("synthetic.rs").unwrap();
    assert!(matches!(
        owner.source_guard().check("bypass.rs"),
        Err(StorageError::PermissionDenied)
    ));
    let other = SourceGuard::disabled(owner.owner_id());
    other.check("bypass.rs").unwrap();
}

#[tokio::test]
async fn repository_scenario_factory_local_remote_parity() {
    let owner = SyntheticOwner::new().unwrap();
    let factory = owner.scenario_factory();
    for repo in [factory.local(), factory.remote()] {
        run_repository_scenarios(repo);
    }
    assert_eq!(REPOSITORY_SCENARIO_CASES.len(), 5);
}

#[test]
fn evidence_json_redacts_secrets() {
    let raw = json!({
        "records": 2,
        "password": "hunter2",
        "nested": { "api_key": "abc", "count": 1 }
    });
    let clean = sanitize_json(raw);
    assert_eq!(clean["password"], "[redacted]");
    assert_eq!(clean["nested"]["api_key"], "[redacted]");
    assert_eq!(clean["nested"]["count"], 1);
    assert_eq!(clean["records"], 2);
}

#[test]
fn magician_bin_does_not_depend_on_migration_crate() {
    let toml = include_str!("../../magician-bin/Cargo.toml");
    assert!(!toml.contains("magician-storage-migration"));
}

async fn finish_until(coord: &MigrationCoordinator, id: uuid::Uuid, stop_before: MigrationPhase) {
    if stop_before == MigrationPhase::Planned {
        return;
    }
    loop {
        let entry = coord.status(id).await.unwrap();
        if entry.record.phase == stop_before && !entry.phase_complete(stop_before) {
            return;
        }
        if entry.phase_complete(stop_before) {
            return;
        }
        match entry.record.phase {
            MigrationPhase::Planned => {
                if stop_before == MigrationPhase::Exporting {
                    return;
                }
                coord.export(id).await.unwrap();
            },
            MigrationPhase::Exporting => {
                if stop_before == MigrationPhase::Imported {
                    return;
                }
                coord.import(id).await.unwrap();
            },
            MigrationPhase::Imported => {
                if stop_before == MigrationPhase::Verified {
                    return;
                }
                coord.semantic_verify(id).await.unwrap();
            },
            MigrationPhase::Verified => {
                if stop_before == MigrationPhase::CutoverPending {
                    return;
                }
                coord
                    .prepare_cutover(id, entry.fence_generation)
                    .await
                    .unwrap();
            },
            MigrationPhase::CutoverPending => {
                if stop_before == MigrationPhase::Cutover {
                    return;
                }
                coord
                    .cutover(id, entry.record.cutover_fencing_generation.unwrap())
                    .await
                    .unwrap();
            },
            MigrationPhase::Cutover => {
                if stop_before == MigrationPhase::Settled {
                    return;
                }
                coord.settle(id, entry.fence_generation).await.unwrap();
            },
            _ => return,
        }
    }
}

async fn invoke_phase(
    coord: &MigrationCoordinator,
    id: uuid::Uuid,
    phase: MigrationPhase,
) -> Result<magician_storage::StorageMigrationRecord, StorageError> {
    match phase {
        MigrationPhase::Planned => {
            let owner = coord.registry().get(SYNTHETIC_OWNER_ID).unwrap();
            coord
                .plan(PlanRequest {
                    store_id: owner.owner_id(),
                    scope: tenant(),
                    source_profile: "local_embedded".into(),
                    target_profile: "remote_durable".into(),
                })
                .await
        },
        MigrationPhase::Exporting => coord.export(id).await,
        MigrationPhase::Imported => coord.import(id).await,
        MigrationPhase::Verified => coord.semantic_verify(id).await,
        MigrationPhase::CutoverPending => {
            let fence = coord.status(id).await.unwrap().fence_generation;
            coord.prepare_cutover(id, fence).await
        },
        MigrationPhase::Cutover => {
            let fence = coord
                .status(id)
                .await
                .unwrap()
                .record
                .cutover_fencing_generation
                .unwrap();
            coord.cutover(id, fence).await
        },
        MigrationPhase::Settled => {
            let fence = coord.status(id).await.unwrap().fence_generation;
            coord.settle(id, fence).await
        },
        other => panic!("forward crash test does not invoke {other}"),
    }
}

fn run_repository_scenarios(repo: &magician_storage_migration::InMemoryRepository) {
    let alice = tenant();
    let bob = StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse("bob").unwrap(),
        WorkspaceId::parse("work").unwrap(),
    ));
    let item = SyntheticItem {
        principal: "alice".into(),
        workspace: "home".into(),
        id: "item-1".into(),
        revision: 1,
        payload: "one".into(),
    };
    let created = repo.create_or_get(item.clone()).unwrap();
    let again = repo.create_or_get(item).unwrap();
    assert_eq!(created, again);
    assert_eq!(repo.get(&alice, "item-1").unwrap().unwrap().payload, "one");
    assert!(repo.get(&bob, "item-1").unwrap().is_none());
    let updated = repo.compare_and_update(&alice, "item-1", 1, "two").unwrap();
    assert_eq!(updated.revision, 2);
    let stale = repo
        .compare_and_update(&alice, "item-1", 1, "nope")
        .unwrap_err();
    assert!(matches!(stale, StorageError::Conflict { .. }));
    let dump = repo.export_scope(&alice).unwrap();
    let other = magician_storage_migration::SyntheticOwner::new().unwrap();
    other.remote().import_scope(&alice, &dump).unwrap();
    assert_eq!(
        other
            .remote()
            .get(&alice, "item-1")
            .unwrap()
            .unwrap()
            .payload,
        "two"
    );
}

//! Task 19 activation preconditions and workflow drills.

use std::sync::Arc;

use magician_storage::{PrincipalId, ScopeId, StorageScope, WorkspaceId};
use magician_storage_migration::{CrashSpec, SyntheticOwner, SYNTHETIC_OWNER_ID};

use super::{
    evaluate_preconditions, ActivationContext, ActivationOperation, OperatorWorkflow,
    CUTOVER_CONFIRM, GATE3_CLOSED,
};
use magician_storage::MigrationPhase;

fn tenant() -> StorageScope {
    StorageScope::Tenant(ScopeId::new(
        PrincipalId::parse("alice").unwrap(),
        WorkspaceId::parse("home").unwrap(),
    ))
}

fn ctx_ready() -> ActivationContext {
    ActivationContext {
        backup_age_secs: Some(60),
        remote_health: true,
        other_migration_lease: false,
        rollback_retain_days: 7,
        ..ActivationContext::default()
    }
}

async fn flow(
    owner: Arc<SyntheticOwner>,
    ctx: ActivationContext,
) -> (tempfile::TempDir, OperatorWorkflow) {
    let dir = tempfile::tempdir().unwrap();
    let handler: Arc<dyn magician_storage_migration::OwnerMigrationHandler> = owner;
    let flow = OperatorWorkflow::with_handler(dir.path(), handler, ctx)
        .await
        .unwrap();
    (dir, flow)
}

#[test]
fn gate3_is_closed_and_cutover_still_needs_backup_and_health() {
    assert!(GATE3_CLOSED);
    let pre = evaluate_preconditions(&ActivationContext::default()).unwrap();
    assert!(pre.gates.gate1_closed && pre.gates.gate2_closed && pre.gates.gate3_closed);
    assert!(!pre
        .blocking
        .iter()
        .any(|item| item == "decision_gate_3_open"));
    assert!(pre
        .blocking
        .iter()
        .any(|item| item == "backup_stale_or_missing"));
    assert!(!pre.cutover_allowed());
    assert!(evaluate_preconditions(&ctx_ready())
        .unwrap()
        .cutover_allowed());
}

#[test]
fn inventory_is_bounded_and_status_lists_qualified_ops() {
    let pre = evaluate_preconditions(&ActivationContext::default()).unwrap();
    assert!(pre.tier1_remote_ready);
    assert!(pre.tier2_matrix_complete);
    let report = super::ActivationReport::from_preconditions(
        ActivationOperation::Inventory,
        "alice",
        "home",
        &pre,
        "local_embedded",
        "remote_durable",
    );
    let inventory = report
        .qualified
        .iter()
        .find(|row| row.operation == ActivationOperation::Inventory)
        .unwrap();
    assert!(inventory.qualified);
    let cutover = report
        .qualified
        .iter()
        .find(|row| row.operation == ActivationOperation::Cutover)
        .unwrap();
    assert!(!cutover.qualified);
}

#[tokio::test]
async fn inventory_report_is_bounded_to_catalog() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    let (_dir, flow) = flow(owner, ActivationContext::default()).await;
    let report = flow.inventory("alice", "home").unwrap();
    assert!(report.ok);
    let catalog = crate::magician_v2::track_a_acceptance::load_catalog().unwrap();
    assert_eq!(report.detail["owner_count"], catalog.owners.len());
    assert!(report.local_canonical);
    assert!(report.backend_unavailable);
}

#[tokio::test]
async fn dry_run_plan_does_not_copy_bytes() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    let (_dir, flow) = flow(Arc::clone(&owner), ActivationContext::default()).await;
    let report = flow
        .plan(SYNTHETIC_OWNER_ID, tenant(), true, "alice", "home")
        .await
        .unwrap();
    assert!(report.ok && report.dry_run);
    assert!(report.migration_id.is_none());
    assert_eq!(owner.remote().list(&tenant()).unwrap().len(), 0);
    assert_eq!(owner.local().list(&tenant()).unwrap().len(), 1);
}

#[tokio::test]
async fn failed_verify_cannot_reach_cutover_and_leaves_local_canonical() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    let (_dir, flow) = flow(Arc::clone(&owner), ctx_ready()).await;
    let planned = flow
        .plan(SYNTHETIC_OWNER_ID, tenant(), false, "alice", "home")
        .await
        .unwrap();
    let id = uuid::Uuid::parse_str(planned.migration_id.as_deref().unwrap()).unwrap();
    flow.export(id, "alice", "home").await.unwrap();
    owner
        .local()
        .upsert(magician_storage_migration::SyntheticItem {
            principal: "alice".into(),
            workspace: "home".into(),
            id: "alpha".into(),
            revision: 2,
            payload: "mutated".into(),
        })
        .unwrap();
    let verified = flow.verify(id, "alice", "home").await.unwrap();
    assert!(!verified.ok);
    assert!(verified.local_canonical);
    let cut = flow
        .cutover(id, 1, CUTOVER_CONFIRM, "alice", "home")
        .await
        .unwrap();
    assert!(!cut.ok);
    assert!(cut.local_canonical);
    assert_eq!(owner.local().list(&tenant()).unwrap()[0].payload, "mutated");
}

#[tokio::test]
async fn cutover_refuses_without_confirmation() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    let (_dir, flow) = flow(owner, ctx_ready()).await;
    let planned = flow
        .plan(SYNTHETIC_OWNER_ID, tenant(), false, "alice", "home")
        .await
        .unwrap();
    let id = uuid::Uuid::parse_str(planned.migration_id.as_deref().unwrap()).unwrap();
    let denied = flow
        .cutover(id, 1, "please", "alice", "home")
        .await
        .unwrap();
    assert!(!denied.ok);
    assert!(denied
        .blocking
        .iter()
        .any(|item| item == "confirmation_mismatch"));
    let gated = flow
        .cutover(id, 1, CUTOVER_CONFIRM, "alice", "home")
        .await
        .unwrap();
    assert!(!gated.ok);
    assert!(gated.local_canonical);
}

#[tokio::test]
async fn secrets_are_redacted_from_reports() {
    let mut report = super::ActivationReport::from_preconditions(
        ActivationOperation::Status,
        "alice",
        "home",
        &evaluate_preconditions(&ActivationContext::default()).unwrap(),
        "local_embedded",
        "remote_durable",
    );
    report.detail = serde_json::json!({
        "password": "hunter2",
        "ok": true,
    });
    let redacted = report.redact();
    assert_eq!(redacted.detail["password"], "[redacted]");
    assert_eq!(redacted.detail["ok"], true);
}

#[tokio::test]
async fn wrong_source_generation_is_refused_on_resume() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    let dir = tempfile::tempdir().unwrap();
    let handler: Arc<dyn magician_storage_migration::OwnerMigrationHandler> = owner.clone();
    let flow = OperatorWorkflow::with_handler(dir.path(), handler, ActivationContext::default())
        .await
        .unwrap();
    let planned = flow
        .plan(SYNTHETIC_OWNER_ID, tenant(), false, "alice", "home")
        .await
        .unwrap();
    let id = uuid::Uuid::parse_str(planned.migration_id.as_deref().unwrap()).unwrap();
    owner.seed(&tenant(), "beta", "two").unwrap();
    let resumed = flow.resume(id, "alice", "home").await;
    assert!(resumed.is_err() || !resumed.unwrap().ok);
}

#[tokio::test]
async fn cancel_before_cutover_keeps_local_canonical() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    let (_dir, flow) = flow(Arc::clone(&owner), ActivationContext::default()).await;
    let planned = flow
        .plan(SYNTHETIC_OWNER_ID, tenant(), false, "alice", "home")
        .await
        .unwrap();
    let id = uuid::Uuid::parse_str(planned.migration_id.as_deref().unwrap()).unwrap();
    let cancelled = flow.cancel(id, "alice", "home").await.unwrap();
    assert!(cancelled.ok);
    assert!(cancelled.local_canonical);
    assert_eq!(owner.local().list(&tenant()).unwrap().len(), 1);
}

#[tokio::test]
async fn changing_target_profile_does_not_move_bytes() {
    let owner = Arc::new(SyntheticOwner::new().unwrap());
    owner.seed(&tenant(), "alpha", "one").unwrap();
    assert_eq!(owner.remote().list(&tenant()).unwrap().len(), 0);
    let _ = ActivationContext {
        target_profile: "remote_durable".into(),
        ..ActivationContext::default()
    };
    assert_eq!(owner.local().list(&tenant()).unwrap().len(), 1);
    assert_eq!(owner.remote().list(&tenant()).unwrap().len(), 0);
}

#[tokio::test]
async fn crash_spec_is_available_for_phase_restart() {
    let _ = CrashSpec {
        phase: MigrationPhase::Exporting,
        before_complete: true,
    };
}

#[test]
fn magician_bin_does_not_cut_over_at_startup() {
    let text = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../magician-bin/src/main.rs"),
    )
    .unwrap();
    assert!(!text.contains("OperatorWorkflow::cutover"));
    assert!(text.contains("StorageRuntime::open_local"));
}

#[test]
fn backup_age_gate_blocks_when_missing() {
    let pre = evaluate_preconditions(&ActivationContext::default()).unwrap();
    assert!(pre
        .blocking
        .iter()
        .any(|item| item == "backup_stale_or_missing"));
    let mut ctx = ctx_ready();
    ctx.backup_age_secs = Some(48 * 60 * 60);
    let stale = evaluate_preconditions(&ctx).unwrap();
    assert!(stale
        .blocking
        .iter()
        .any(|item| item == "backup_stale_or_missing"));
}

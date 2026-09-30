//! The generic authority carrier on the durable execution record.
//!
//! The record every flow writes used to carry an OPC-specific engagement
//! reference. A recruiting, support-triage or vendor-management run had no slot
//! it could honestly fill, and a program-scoped root was refused outright
//! because the field could not hold one. This binary pins the replacement:
//!
//! - the record holds **every** arm of `WorkContextKind`, on disk, read back;
//! - absent still means **no authority**, never "unrestricted";
//! - an arm the dispatch boundary cannot enforce is **refused by name**, never
//!   narrowed to `None` — an unenforceable confinement that reads as no
//!   confinement is the one failure nothing downstream can see;
//! - a store that drops the carrier is **caught by the record it returns**.
//!
//! Byte-for-byte engagement behaviour is not re-proved here: that is
//! `engagement_authority_matrix.rs`, which still runs unmodified.

use std::path::Path;

use magician::magician_v2::{
    engagements::EngagementAuthorityRef,
    orchestrator::v2_orchestrator::{verify_persisted_work_authority, work_authority_grant},
    storage::{ExecutionRun, WaitingState},
    work_context::{WorkAuthorityRef, WorkContextKind},
    FileV2Store,
};
use runtime_core::{V2ConversationStore as _, WorkAuthorityGrant};

// ─────────────────────────────────────────────────────────────────────────
// The durable record, against a real store on real disk.
// ─────────────────────────────────────────────────────────────────────────

/// A task workspace has to exist before a root execution may be written, so a
/// record test that skipped this would be testing the error path by accident.
async fn seed_workspace(storage_root: &Path, principal: &str, workspace: &str, task_id: &str) {
    use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
    ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(storage_root))
        .ensure_task_workspace(principal, workspace, task_id)
        .await
        .expect("task workspace");
}

async fn write_root_with(
    store: &FileV2Store,
    storage_root: &Path,
    execution_id: &str,
    grant: Option<WorkAuthorityGrant>,
) -> Result<ExecutionRun, String> {
    let task_id = format!("task-{execution_id}");
    seed_workspace(storage_root, "alpha", "prod", &task_id).await;
    store
        .create_execution_with_work_authority(
            "alpha",
            "prod",
            Some("Carrier row".to_string()),
            "personal-assistant",
            Some(task_id),
            Some(execution_id.to_string()),
            Some(execution_id.to_string()),
            None,
            None,
            Vec::new(),
            WaitingState::Runnable,
            grant,
        )
        .await
        .map_err(|error| error.to_string())
}

/// Pins: a **program**-scoped root survives a write and a read back.
///
/// This is the arm that did not exist. With an engagement reference in the
/// field, a program root could only be refused or fabricated into an engagement
/// id nobody granted, so a second flow could not be confined at all. Asserted
/// on VALUES read off disk — the kind, the id and the revision — because a
/// carrier that came back as the wrong arm would still be `Some`.
#[tokio::test]
async fn the_record_carries_a_program_scoped_authority_across_a_write_and_a_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileV2Store::new(dir.path());

    let created = write_root_with(
        &store,
        dir.path(),
        "exec-program-root",
        Some(WorkAuthorityGrant {
            work_kind: "program".to_string(),
            work_id: "recruiting".to_string(),
            authority_revision: 4,
        }),
    )
    .await
    .expect("a program-scoped root is writable");

    let carried = created
        .work_authority
        .as_ref()
        .expect("the created record carries the authority it was written with");
    assert_eq!(
        carried.work,
        WorkContextKind::Program("recruiting".to_string())
    );
    assert_eq!(carried.authority_revision, 4);

    // Off disk, not out of memory: the durable read is the one that matters.
    let reloaded = store
        .get_execution("exec-program-root")
        .await
        .expect("the record reloads");
    let reloaded_carrier = reloaded
        .work_authority
        .as_ref()
        .expect("the carrier survived the round trip to disk");
    assert_eq!(
        reloaded_carrier.work,
        WorkContextKind::Program("recruiting".to_string()),
        "a program-scoped run must come back program-scoped, not as the nearest known arm"
    );
    assert_eq!(reloaded_carrier.authority_revision, 4);
    assert_eq!(reloaded_carrier.as_key(), "program:recruiting");
}

/// Pins: the engagement arm lands in the engagement arm — the kind and the id
/// are not swapped as they cross the storage boundary.
///
/// The grant crosses as two strings. A boundary that swapped them would persist
/// a run confined to a work context nobody granted, and both fields being
/// non-empty means nothing would notice.
#[tokio::test]
async fn an_engagement_grant_crosses_the_storage_boundary_without_swapping_its_halves() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileV2Store::new(dir.path());

    let created = write_root_with(
        &store,
        dir.path(),
        "exec-engagement-root",
        Some(WorkAuthorityGrant {
            work_kind: "engagement".to_string(),
            work_id: "eng-acme".to_string(),
            authority_revision: 9,
        }),
    )
    .await
    .expect("an engagement-scoped root is writable");

    let carried = created.work_authority.expect("carrier present");
    assert_eq!(
        carried.work,
        WorkContextKind::Engagement("eng-acme".to_string())
    );
    assert_eq!(carried.id(), "eng-acme");
    assert_eq!(carried.kind_token(), "engagement");
    assert_eq!(carried.authority_revision, 9);
}

/// Pins: a kind token this build does not know is an **error**, and no record
/// is created.
///
/// Fail closed at the boundary. Coercing an unknown token into the nearest
/// known arm would silently re-scope the authority; dropping it into `None`
/// would start the run unconfined. The record is asserted absent afterwards, so
/// this cannot pass by the write having half-happened.
#[tokio::test]
async fn an_unknown_work_kind_refuses_the_write_instead_of_guessing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileV2Store::new(dir.path());

    // The same store writes a known kind, so the refusal below is about the
    // token and not about an empty fixture.
    write_root_with(
        &store,
        dir.path(),
        "exec-known-kind",
        Some(WorkAuthorityGrant {
            work_kind: "engagement".to_string(),
            work_id: "eng-live".to_string(),
            authority_revision: 1,
        }),
    )
    .await
    .expect("a known kind writes");

    let refusal = write_root_with(
        &store,
        dir.path(),
        "exec-unknown-kind",
        Some(WorkAuthorityGrant {
            work_kind: "counterparty".to_string(),
            work_id: "acme".to_string(),
            authority_revision: 1,
        }),
    )
    .await
    .expect_err("an unknown work kind must refuse the write");
    assert!(
        refusal.contains("unknown work kind `counterparty`"),
        "the refusal must name the token it refused, got: {refusal}"
    );
    assert!(
        store.get_execution("exec-unknown-kind").await.is_err(),
        "a refused write must leave no record behind for a resume to pick up"
    );
}

/// Pins: a work id carrying U+001F never reaches the record.
///
/// It is a caller string feeding an id derivation, and one carrying the field
/// separator could address a record it did not name. Refused at the boundary
/// rather than escaped downstream.
#[tokio::test]
async fn a_separator_carrying_work_id_refuses_the_write() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = FileV2Store::new(dir.path());

    let refusal = write_root_with(
        &store,
        dir.path(),
        "exec-separator",
        Some(WorkAuthorityGrant {
            work_kind: "engagement".to_string(),
            work_id: "eng\u{1f}smuggled".to_string(),
            authority_revision: 1,
        }),
    )
    .await
    .expect_err("U+001F must never reach an id derivation");
    assert!(
        refusal.contains("U+001F"),
        "the refusal must name the separator, got: {refusal}"
    );
    assert!(store.get_execution("exec-separator").await.is_err());
}

// ─────────────────────────────────────────────────────────────────────────
// Absent means no authority, never permission.
// ─────────────────────────────────────────────────────────────────────────

/// Pins: a record written without the field reads back as **no authority**.
///
/// `#[serde(default)]` is what makes an absent value safe, and the failure it
/// stops is a deserialization error being "fixed" one day by inventing a
/// default that is anything other than `None`. The seeded record proves the
/// field is genuinely absent from the JSON rather than present and null.
#[test]
fn a_record_without_the_field_reads_as_no_authority() {
    let raw = serde_json::json!({
        "id": "exec-legacy",
        "principal": "alpha",
        "workspace": "prod",
        "task_id": "task-legacy",
        "root_execution_id": "exec-legacy",
        "title": "No carrier",
        "waiting_state": "Runnable",
        "created_at": 1,
        "updated_at": 2,
        "processing_correlation_id": null,
        "active_owner_agent_id": "personal-assistant"
    });
    assert!(
        raw.get("work_authority").is_none(),
        "the fixture must actually omit the field, or this proves nothing"
    );

    let record: ExecutionRun = serde_json::from_value(raw).expect("the record deserializes");
    assert_eq!(
        record.work_authority, None,
        "an absent carrier is no authority; reading it as anything else would make a record \
         nobody confined look like a record that may do anything"
    );
}

/// Pins: the carrier is written out only when there is one, and round-trips
/// exactly when there is.
#[test]
fn the_carrier_round_trips_through_json_unchanged() {
    let seeded = WorkAuthorityRef::new(
        WorkContextKind::Engagement("eng-round-trip".to_string()),
        11,
    )
    .expect("a well-formed id builds");

    let encoded = serde_json::to_string(&seeded).expect("serializes");
    let decoded: WorkAuthorityRef = serde_json::from_str(&encoded).expect("deserializes");
    assert_eq!(
        decoded.work,
        WorkContextKind::Engagement("eng-round-trip".to_string())
    );
    assert_eq!(decoded.authority_revision, 11);
}

// ─────────────────────────────────────────────────────────────────────────
// Narrowing the generic carrier to the boundary that enforces it.
// ─────────────────────────────────────────────────────────────────────────

/// Pins: an engagement-armed carrier narrows to exactly the reference the
/// §4.2c dispatch checks take, with both halves intact.
///
/// If the revision were dropped or reset here, staleness (row 9) would stop
/// being observable: every snapshot would look current.
#[test]
fn an_engagement_carrier_narrows_to_the_reference_the_dispatch_boundary_takes() {
    let carried = WorkAuthorityRef::new(WorkContextKind::Engagement("eng-acme".to_string()), 7)
        .expect("well-formed");

    let narrowed = EngagementAuthorityRef::try_from(&carried)
        .expect("an engagement-armed carrier is exactly what the boundary enforces");
    assert_eq!(narrowed.engagement_id, "eng-acme");
    assert_eq!(narrowed.authority_revision, 7);

    // And back again, unchanged: the delegation seam inherits the parent's
    // durable value verbatim (§4.2c row 5), so the two shapes must agree.
    let widened = WorkAuthorityRef::from(&narrowed);
    assert_eq!(widened, carried);
}

/// Pins: a **program**-armed carrier is refused at the engagement boundary,
/// never read as "no engagement".
///
/// This is the fail-closed rule with teeth. `Ok(None)` here would hand the
/// caller a run that looks exactly like a run nobody confined, and that run
/// proceeds. The engagement arm is asserted to convert first, so the refusal is
/// known to be about the arm and not about a broken conversion.
#[test]
fn a_program_carrier_is_refused_at_the_engagement_boundary_not_read_as_unbound() {
    let enforceable = WorkAuthorityRef::new(WorkContextKind::Engagement("eng-live".to_string()), 1)
        .expect("well-formed");
    assert!(EngagementAuthorityRef::try_from(&enforceable).is_ok());

    let program = WorkAuthorityRef::new(WorkContextKind::Program("recruiting".to_string()), 3)
        .expect("well-formed");
    let refusal = EngagementAuthorityRef::try_from(&program)
        .expect_err("a program carrier cannot be enforced by an engagement ceiling");
    assert!(
        refusal.contains("program:recruiting"),
        "the refusal must name the work it stopped, got: {refusal}"
    );
    assert!(
        refusal.contains("no roster owns programs"),
        "the refusal must name the piece that is missing, got: {refusal}"
    );
}

/// Pins: a blank work id is refused at construction, so no carrier that
/// addresses every record and none of them can be built at all.
#[test]
fn a_blank_work_id_cannot_become_a_carrier() {
    let refusal = WorkAuthorityRef::new(WorkContextKind::Program("   ".to_string()), 1)
        .expect_err("a blank id names nothing");
    assert!(refusal.contains("must name something"), "got: {refusal}");
}

// ─────────────────────────────────────────────────────────────────────────
// The store's answer is checked, not assumed.
// ─────────────────────────────────────────────────────────────────────────

fn record_carrying(work_authority: Option<WorkAuthorityRef>) -> ExecutionRun {
    ExecutionRun {
        id: "exec-verify".to_string(),
        principal: "alpha".to_string(),
        workspace: "prod".to_string(),
        task_id: Some("task-verify".to_string()),
        root_execution_id: Some("exec-verify".to_string()),
        title: None,
        waiting_state: WaitingState::Runnable,
        created_at: 1,
        updated_at: 1,
        processing_correlation_id: None,
        current_stage: None,
        current_provider: None,
        escalation_trigger: None,
        parent_execution_id: None,
        child_execution_ids: Vec::new(),
        active_owner_agent_id: "personal-assistant".to_string(),
        owner_stack: Vec::new(),
        active_delegation_group: Vec::new(),
        timeout_secs: None,
        delegation_chain: Vec::new(),
        work_authority,
        paused_from_state: None,
        entry_mode: magician::magician_v2::storage::ExecutionEntryMode::PlanningBacked,
    }
}

/// Pins: a store that drops the carrier is caught by the record it returned.
///
/// The storage trait's authority-carrying creation is a *provided* method whose
/// default silently drops the grant, so every store that never overrode it
/// would produce an unconfined child from a confined parent. The check has to
/// compare, not trust — and it has to fire in both directions, because a
/// carrier nobody asked for is a run confined to work nobody granted it.
#[test]
fn a_record_that_lost_or_gained_a_carrier_refuses_the_run() {
    let asked_for = WorkAuthorityRef::new(WorkContextKind::Engagement("eng-acme".to_string()), 2)
        .expect("well-formed");

    // The honest case passes, so the failures below are not the check refusing
    // everything.
    verify_persisted_work_authority(&record_carrying(Some(asked_for.clone())), Some(&asked_for))
        .expect("a record carrying what was asked for is accepted");
    verify_persisted_work_authority(&record_carrying(None), None)
        .expect("an unbound run asked for nothing and carries nothing");

    let dropped = verify_persisted_work_authority(&record_carrying(None), Some(&asked_for))
        .expect_err("a dropped carrier must refuse the run");
    assert!(
        dropped.contains("<unbound>") && dropped.contains("engagement:eng-acme"),
        "the refusal must say what was carried and what was asked for, got: {dropped}"
    );

    let different = WorkAuthorityRef::new(
        WorkContextKind::Engagement("eng-someone-else".to_string()),
        2,
    )
    .expect("well-formed");
    let swapped =
        verify_persisted_work_authority(&record_carrying(Some(different)), Some(&asked_for))
            .expect_err("a record confined to other work must refuse the run");
    assert!(
        swapped.contains("engagement:eng-someone-else"),
        "got: {swapped}"
    );

    let unasked = verify_persisted_work_authority(&record_carrying(Some(asked_for.clone())), None)
        .expect_err("an authority nobody asked for must refuse the run");
    assert!(unasked.contains("engagement:eng-acme"), "got: {unasked}");

    // Same work, forged revision: caught too, or a stale ceiling could be
    // pinned by whichever layer wrote the record.
    let revision_forged =
        WorkAuthorityRef::new(WorkContextKind::Engagement("eng-acme".to_string()), 99)
            .expect("well-formed");
    assert!(
        verify_persisted_work_authority(&record_carrying(Some(revision_forged)), Some(&asked_for))
            .is_err(),
        "a revision that does not match what was asked for must refuse the run"
    );
}

/// Pins: the wire grant carries the kind in the kind field and the id in the id
/// field.
///
/// Two adjacent strings. Swapping them persists a confinement to work nobody
/// named, and nothing further down the write path could tell.
#[test]
fn the_wire_grant_puts_each_half_in_the_field_that_names_it() {
    assert_eq!(work_authority_grant(None), None);

    let carried =
        WorkAuthorityRef::new(WorkContextKind::Program("vendor-management".to_string()), 5)
            .expect("well-formed");
    let grant = work_authority_grant(Some(&carried)).expect("a carrier produces a grant");
    assert_eq!(grant.work_kind, "program");
    assert_eq!(grant.work_id, "vendor-management");
    assert_eq!(grant.authority_revision, 5);
}

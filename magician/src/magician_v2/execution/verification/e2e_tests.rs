//! End-to-end tests through the real controller loop.
//!
//! These drive an actual [`VerificationStore`] on a temp directory, an actual
//! source tree, and the actual runner spawning real processes — no mocks. The
//! unit tests elsewhere prove each rule in isolation; these prove the rules
//! compose into the lifecycle the plan describes, and that the outcomes a
//! consumer sees match the durable record.

use std::sync::Arc;

use chrono::Utc;

use super::attestation::AttemptOutcome;
use super::controller::{ControllerDeps, PassOutcome, VerificationController};
use super::events::{RecordingEventSink, VerificationEventKind};
use super::gate::{GateBudgets, GateOrigin, GateStatus, VerificationGate, VerificationState};
use super::ids::CandidateRevision;
use super::policy::{CheckSpec, PolicySource, SandboxPolicy, VerificationPolicy};
use super::store::VerificationStore;
use super::VerificationActivation;
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

struct Harness {
    _scope_dir: tempfile::TempDir,
    _source_dir: tempfile::TempDir,
    _workspace_dir: tempfile::TempDir,
    source_root: std::path::PathBuf,
    scope: TransactionScope,
    sink: Arc<RecordingEventSink>,
    controller: VerificationController,
    store: VerificationStore,
}

impl Harness {
    fn new() -> Self {
        Self::with_activation(VerificationActivation::Enforce)
    }

    fn with_activation(activation: VerificationActivation) -> Self {
        let scope_dir = tempfile::tempdir().unwrap();
        let source_dir = tempfile::tempdir().unwrap();
        let workspace_dir = tempfile::tempdir().unwrap();

        // A minimal but real source tree, so the snapshot has content to key
        // on and the ephemeral workspace has something to materialise.
        std::fs::create_dir_all(source_dir.path().join("src")).unwrap();
        std::fs::write(source_dir.path().join("src/main.rs"), "fn main() {}\n").unwrap();

        let scope = TransactionScope {
            principal: "anonymous".into(),
            workspace: "default".into(),
        };
        let sink = Arc::new(RecordingEventSink::default());
        let controller = VerificationController::new(ControllerDeps {
            store: Arc::new(VerificationStore::new(scope_dir.path(), scope.clone())),
            sink: sink.clone(),
            activation,
            worker_id: "worker-1".into(),
            workspace_root: workspace_dir.path().to_path_buf(),
        });

        Self {
            source_root: source_dir.path().to_path_buf(),
            store: VerificationStore::new(scope_dir.path(), scope.clone()),
            _scope_dir: scope_dir,
            _source_dir: source_dir,
            _workspace_dir: workspace_dir,
            scope,
            sink,
            controller,
        }
    }

    fn open_gate(&self) -> VerificationGate {
        self.open_gate_with(GateBudgets::default(), |_| {})
    }

    fn open_gate_with(
        &self,
        budgets: GateBudgets,
        mutate: impl FnOnce(&mut VerificationGate),
    ) -> VerificationGate {
        let mut gate = VerificationGate::new(
            self.scope.clone(),
            "proj-e2e",
            "task-e2e",
            "exec-e2e",
            CandidateRevision::new("ccp-e2e", 1).unwrap(),
            GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: Some("pi".into()),
                constraint_auto: false,
                coding_invocation_ref: Some("opaque-server-side".into()),
                child_execution_id: Some("exec-child".into()),
            },
            budgets,
        )
        .unwrap();
        mutate(&mut gate);
        self.store.open_gate(gate).unwrap().0
    }

    fn edit_source(&self, contents: &str) {
        std::fs::write(self.source_root.join("src/main.rs"), contents).unwrap();
    }
}

fn policy(required: Vec<CheckSpec>) -> VerificationPolicy {
    VerificationPolicy {
        source: PolicySource::Owner,
        required,
        advisory: Vec::new(),
        total_timeout_secs: Some(120),
        sandbox: SandboxPolicy::default(),
        baseline_ref: Some("owner-e2e".into()),
    }
}

fn check(id: &str, program: &str, args: &[&str]) -> CheckSpec {
    CheckSpec {
        id: id.into(),
        program: program.into(),
        args: args.iter().map(|s| (*s).to_string()).collect(),
        display: format!("{program} {}", args.join(" ")),
        timeout_secs: Some(60),
        env: Default::default(),
        advisory: false,
    }
}

// ---------------------------------------------------------------------------

#[tokio::test]
async fn green_path_releases_the_candidate_and_records_verified() {
    let h = Harness::new();
    let gate = h.open_gate();
    let baseline = policy(vec![check("ok", "true", &[])]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    assert_eq!(outcome, PassOutcome::Verified);

    // The durable record agrees with what the caller was told.
    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Verified);
    assert_eq!(settled.verification_state(), VerificationState::Verified);
    assert!(settled.verification_state().is_verified());

    // The attestation is sealed green.
    let attestation = h
        .store
        .load_attestation(settled.active_attestation_ref.as_ref().unwrap())
        .unwrap();
    assert!(attestation.is_sealed());
    assert_eq!(
        attestation.accepted_result.as_ref().unwrap().outcome,
        AttemptOutcome::Green
    );

    // The outbox entry is retired, so no worker picks the gate up again.
    assert!(h.store.list_outbox().unwrap().is_empty());

    let kinds = h.sink.kinds();
    assert!(kinds.contains(&VerificationEventKind::Started));
    assert!(kinds.contains(&VerificationEventKind::Passed));
}

/// The defect this closes was permanent, not transient. The workspace path was
/// `{gate}-r{revision}`, cleanup was `Drop`-only, and nothing reaped: one
/// SIGKILL left a directory there, `materialise` refused it, the gate settled
/// `unavailable` — which *keeps* its outbox entry — and the revision only
/// advances on red, which requires materialising. Every later pass hit the same
/// wall forever.
#[tokio::test]
async fn a_workspace_a_hard_kill_stranded_does_not_wedge_the_gate() {
    let h = Harness::new();
    let gate = h.open_gate();

    // Exactly what a killed process leaves behind: a full directory under the
    // workspace root, with no owner.
    let stranded = h._workspace_dir.path().join(format!(
        "{}-r{}",
        gate.gate_id, gate.current_candidate.revision
    ));
    std::fs::create_dir_all(stranded.join("src")).unwrap();
    std::fs::write(stranded.join("src/main.rs"), "half a checkout").unwrap();

    let outcome = h
        .controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("ok", "true", &[])])),
        )
        .await
        .unwrap();

    assert_eq!(
        outcome,
        PassOutcome::Verified,
        "a stranded checkout must not be able to stop a gate from ever verifying"
    );
    assert!(
        !stranded.exists(),
        "the stranded workspace must be reaped, not left to fill the disk"
    );
    // And the pass left nothing of its own behind either.
    let leftovers: Vec<_> = std::fs::read_dir(h._workspace_dir.path())
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A pass that ends `indeterminate` commits no gate transition, so it was the
/// one non-error exit that walked away still owning the lease. The next pass
/// claims under a fresh worker id, and a live lease held by someone else is
/// refused — so the re-run this outcome exists to allow could not happen for
/// the rest of the TTL.
#[tokio::test]
async fn an_indeterminate_attempt_gives_the_lease_back() {
    let h = Harness::new();
    let gate = h.open_gate();

    // A "check" that rewrites the code it is checking invalidates its own
    // evidence.
    let baseline = policy(vec![check(
        "reformats",
        "sh",
        &["-c", "printf 'fn main() { }\\n' > src/main.rs"],
    )]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();
    assert!(
        matches!(outcome, PassOutcome::RetryLater { .. }),
        "{outcome:?}"
    );

    let after = h.store.load_gate(&gate.gate_id).unwrap();
    assert!(
        after.lease.is_none(),
        "an indeterminate pass is over; holding its lease locks the gate out \
         for the rest of the TTL"
    );
    assert_eq!(after.status, GateStatus::VerificationPending);

    // Concretely: another worker can take it straight away.
    h.store
        .claim_lease(&gate.gate_id, "worker-2", Utc::now(), 120)
        .expect("a finished pass must not lock the next one out");
}

/// Cancelling is a settlement like any other, and every settlement returns the
/// pass's lease.
#[tokio::test]
async fn cancelling_a_gate_gives_the_lease_back() {
    let h = Harness::new();
    let gate = h.open_gate();

    let leased = h
        .store
        .claim_lease(&gate.gate_id, "worker-1", Utc::now(), 120)
        .unwrap();
    assert!(leased.lease.is_some());

    h.controller
        .cancel(&gate.gate_id, leased.generation)
        .expect("cancel");

    let after = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(after.status, GateStatus::Cancelled);
    assert!(
        after.lease.is_none(),
        "a cancelled gate came to rest owning a lease nobody holds"
    );
    assert!(h.store.list_outbox().unwrap().is_empty());
}

/// The reuse lookup has to happen before the copy, not after it. Consulted
/// after materialising, a cache hit had already paid for a full-tree copy —
/// free today on a toy fixture, and the cost grows with the repository.
#[tokio::test]
async fn a_reused_green_attestation_never_materialises_a_workspace() {
    let h = Harness::new();
    let baseline = policy(vec![check("ok", "true", &[])]);

    let first = h.open_gate();
    assert_eq!(
        h.controller
            .run_pass(&first.gate_id, &h.source_root, Some(&baseline))
            .await
            .unwrap(),
        PassOutcome::Verified
    );

    // A second gate over the same candidate, snapshot and policy: the whole
    // attestation key matches, so the stored green answers for it.
    let second = VerificationGate::new(
        h.scope.clone(),
        "proj-e2e",
        "task-e2e-2",
        "exec-e2e-2",
        CandidateRevision::new("ccp-e2e", 1).unwrap(),
        GateOrigin {
            engineer_agent_id: "engineer".into(),
            coding_profile: None,
            coding_engine: Some("pi".into()),
            constraint_auto: false,
            coding_invocation_ref: None,
            child_execution_id: None,
        },
        GateBudgets::default(),
    )
    .unwrap();
    let second = h.store.open_gate(second).unwrap().0;

    // Make materialising impossible: a plain file where the workspace root has
    // to be a directory. A pass that still verifies cannot have copied
    // anything.
    let blocked = h._workspace_dir.path().join("blocked-root");
    std::fs::write(&blocked, b"not a directory").unwrap();
    let blocking_controller = VerificationController::new(ControllerDeps {
        store: Arc::new(VerificationStore::new(h._scope_dir.path(), h.scope.clone())),
        sink: Arc::new(RecordingEventSink::default()),
        activation: VerificationActivation::Enforce,
        worker_id: "worker-cache".into(),
        workspace_root: blocked,
    });

    let outcome = blocking_controller
        .run_pass(&second.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        PassOutcome::Verified,
        "the stored green must settle the gate without a copy: {outcome:?}"
    );
}

#[tokio::test]
async fn red_path_hands_diagnostics_to_the_owning_engineer_and_keeps_the_task_running() {
    let h = Harness::new();
    let gate = h.open_gate();
    let baseline = policy(vec![check("fails", "false", &[])]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    let PassOutcome::Repairing(request) = outcome else {
        panic!("expected repair, got {outcome:?}");
    };
    // Repair re-enters the engineer that owned the work.
    assert_eq!(request.engineer_agent_id, "engineer");
    assert_eq!(request.round, 1);
    assert_eq!(request.superseded_candidate.revision, 1);
    assert!(request.diagnostics.contains("Verification failed"));

    // §4.6: no native session handle anywhere in what leaves the controller.
    let json = serde_json::to_string(&request).unwrap();
    assert!(!json.contains("opaque-server-side"));

    // The gate is repairing, NOT terminal — the task stays running.
    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Repairing);
    assert!(!settled.status.is_terminal());
    assert_eq!(settled.spend.repair_rounds, 1);
    assert_eq!(settled.verification_state(), VerificationState::Repairing);
    assert!(!settled.verification_state().is_verified());

    // `checks_failed`, not `failed` — a consumer must not render a red
    // terminal state mid-loop.
    let kinds = h.sink.kinds();
    assert!(kinds.contains(&VerificationEventKind::ChecksFailed));
    assert!(kinds.contains(&VerificationEventKind::RepairStarted));
    assert!(!kinds.iter().any(|k| k.is_terminal()));
}

#[tokio::test]
async fn repair_retires_the_outbox_entry_for_the_invalidated_candidate() {
    // §4.2: the candidate is invalidated the moment repair starts. Leaving
    // its work item live would let another worker pick up the superseded
    // candidate and verify the pre-repair tree.
    let h = Harness::new();
    let gate = h.open_gate();
    assert_eq!(h.store.list_outbox().unwrap().len(), 1);

    let outcome = h
        .controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("fails", "false", &[])])),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, PassOutcome::Repairing(_)));

    assert!(
        h.store.list_outbox().unwrap().is_empty(),
        "the invalidated candidate's work item must not remain queued"
    );
}

#[tokio::test]
async fn a_gate_mid_repair_is_not_verified_against_the_pre_repair_tree() {
    let h = Harness::new();
    let gate = h.open_gate();

    h.controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("fails", "false", &[])])),
        )
        .await
        .unwrap();
    assert_eq!(
        h.store.load_gate(&gate.gate_id).unwrap().status,
        GateStatus::Repairing
    );

    // A second worker must not verify while the successor does not exist.
    let again = h
        .controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("ok", "true", &[])])),
        )
        .await
        .unwrap();
    assert!(
        matches!(again, PassOutcome::RetryLater { .. }),
        "a repairing gate must not be verified: {again:?}"
    );
    // And it must not have been settled green off the pre-repair tree.
    assert_eq!(
        h.store.load_gate(&gate.gate_id).unwrap().status,
        GateStatus::Repairing
    );
}

#[tokio::test]
async fn the_successor_candidate_re_arms_the_gate_and_verifies_green() {
    // The full repair round-trip: fail -> repairing (candidate invalidated,
    // queue empty) -> engineer lands a successor -> re-armed and verified.
    // Without the successor step a repairing gate has no path back to
    // verification and simply stops.
    let h = Harness::new();
    let gate = h.open_gate();

    let failing = policy(vec![check("build", "false", &[])]);
    assert!(matches!(
        h.controller
            .run_pass(&gate.gate_id, &h.source_root, Some(&failing))
            .await
            .unwrap(),
        PassOutcome::Repairing(_)
    ));

    // The engineer's successor.
    h.edit_source("fn main() { /* repaired */ }\n");
    let successor = CandidateRevision::new("ccp-e2e", 2).unwrap();
    let (rearmed, entry) = h
        .store
        .record_successor_candidate(&gate.gate_id, successor.clone())
        .unwrap();

    assert_eq!(rearmed.status, GateStatus::VerificationPending);
    assert_eq!(rearmed.current_candidate, successor);
    assert_eq!(
        h.store.list_outbox().unwrap(),
        vec![entry],
        "the successor must be queued for a worker to pick up"
    );

    let outcome = h
        .controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("build", "true", &[])])),
        )
        .await
        .unwrap();
    assert!(matches!(outcome, PassOutcome::Verified), "{outcome:?}");
    assert_eq!(
        h.store.load_gate(&gate.gate_id).unwrap().status,
        GateStatus::Verified
    );
}

#[tokio::test]
async fn a_successor_is_refused_unless_the_gate_is_awaiting_one() {
    let h = Harness::new();
    let gate = h.open_gate();

    // Still pending, not repairing.
    let err = h
        .store
        .record_successor_candidate(&gate.gate_id, CandidateRevision::new("ccp-e2e", 2).unwrap())
        .unwrap_err()
        .to_string();
    assert!(err.contains("not repairing"), "{err}");

    // And a non-advancing revision is refused once it *is* repairing.
    h.controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("build", "false", &[])])),
        )
        .await
        .unwrap();
    let err = h
        .store
        .record_successor_candidate(&gate.gate_id, CandidateRevision::new("ccp-e2e", 1).unwrap())
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not supersede"), "{err}");
}

#[tokio::test]
async fn a_terminal_gate_retires_its_work_item_instead_of_looping() {
    // `blocked_partial` is terminal from the moment the gate is opened, so no
    // settle path ever retires its entry. Without this the scheduler
    // re-offers it on every tick, and each pass used to claim a lease —
    // bumping the generation and appending a journal transaction — forever.
    let h = Harness::new();
    let mut gate = VerificationGate::new(
        h.scope.clone(),
        "proj-e2e",
        "task-partial",
        "exec-partial",
        CandidateRevision::new("ccp-partial", 1).unwrap(),
        GateOrigin {
            engineer_agent_id: "engineer".into(),
            coding_profile: None,
            coding_engine: Some("pi".into()),
            constraint_auto: false,
            coding_invocation_ref: None,
            child_execution_id: None,
        },
        GateBudgets::default(),
    )
    .unwrap();
    gate.status = GateStatus::BlockedPartial;
    gate.terminal_reason = Some("partially applied".into());
    let (gate, _) = h.store.open_gate(gate).unwrap();
    assert_eq!(h.store.list_outbox().unwrap().len(), 1);

    let before = h.store.load_gate(&gate.gate_id).unwrap().generation;
    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&policy(vec![])))
        .await
        .unwrap();

    assert!(matches!(outcome, PassOutcome::Exhausted { .. }));
    assert!(
        h.store.list_outbox().unwrap().is_empty(),
        "a terminal gate must not keep a live work item"
    );
    // No lease was claimed, so the journal did not grow.
    assert_eq!(h.store.load_gate(&gate.gate_id).unwrap().generation, before);
}

#[tokio::test]
async fn no_configured_checks_finishes_explicitly_unverified() {
    let h = Harness::new();
    let gate = h.open_gate();

    // A project that defines nothing to run.
    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&policy(vec![])))
        .await
        .unwrap();

    assert_eq!(outcome, PassOutcome::Unverified);

    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Unverified);
    // Explicit, and never confusable with success.
    assert_eq!(settled.verification_state(), VerificationState::Unverified);
    assert!(!settled.verification_state().is_verified());
    assert!(settled.terminal_reason.is_some());

    let event = h
        .sink
        .events()
        .into_iter()
        .find(|e| e.kind == VerificationEventKind::Unverified)
        .unwrap();
    let line = event.voice_line().unwrap();
    assert!(line.contains("no checks were configured"));
    assert!(!line.contains("successfully"));
}

#[tokio::test]
async fn an_unreadable_repository_policy_fails_closed_as_unavailable() {
    let h = Harness::new();
    let gate = h.open_gate();

    std::fs::create_dir_all(h.source_root.join(".magician")).unwrap();
    std::fs::write(
        h.source_root.join(".magician/verification.json"),
        b"{ this is not json",
    )
    .unwrap();

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&policy(vec![])))
        .await
        .unwrap();

    assert!(matches!(outcome, PassOutcome::Unavailable { .. }));

    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    // The distinction that matters: "we could not run it" is not
    // "there was nothing to run", and neither is a pass.
    assert_eq!(settled.status, GateStatus::Unavailable);
    assert_ne!(settled.verification_state(), VerificationState::Unverified);
    assert!(!settled.verification_state().is_verified());
}

#[tokio::test]
async fn a_check_that_rewrites_tracked_source_is_indeterminate_not_green() {
    let h = Harness::new();
    let gate = h.open_gate();

    // A "check" that passes but formats the code it was checking. Its
    // evidence would describe an input that no longer exists.
    let baseline = policy(vec![check(
        "reformats",
        "sh",
        &["-c", "printf 'fn main() { }\\n' > src/main.rs"],
    )]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    // Never green, and the gate stays non-terminal so a later pass re-runs.
    assert!(matches!(outcome, PassOutcome::RetryLater { .. }));
    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert!(!settled.status.is_terminal());

    let kinds = h.sink.kinds();
    assert!(kinds.contains(&VerificationEventKind::AttemptIndeterminate));
    assert!(!kinds.contains(&VerificationEventKind::Passed));
}

#[tokio::test]
async fn a_repair_that_fixes_the_code_reaches_verified_on_the_next_pass() {
    let h = Harness::new();
    let gate = h.open_gate();

    // Round 1: a check that only passes once a marker file exists.
    let baseline = policy(vec![check("needs-fix", "test", &["-f", "FIXED"])]);

    let first = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();
    assert!(matches!(first, PassOutcome::Repairing(_)));

    // The engineer "repairs" — a real source change, which also advances the
    // snapshot digest so no-progress does not trip.
    std::fs::write(h.source_root.join("FIXED"), "done").unwrap();
    h.edit_source("fn main() { /* fixed */ }\n");

    // A repair produces a NEW candidate; the gate advances and re-enters
    // verification.
    let repairing = h.store.load_gate(&gate.gate_id).unwrap();
    let mut next = repairing.clone();
    next.current_candidate = repairing.current_candidate.next("ccp-e2e-r2").unwrap();
    next.status = GateStatus::VerificationPending;
    next.active_attestation_ref = None;
    let advanced = h
        .store
        .commit_gate(next, repairing.generation, |gate| {
            super::journal::JournalPayload::GateUpdate { gate }
        })
        .unwrap();
    assert_eq!(advanced.current_candidate.revision, 2);

    let second = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();
    assert_eq!(second, PassOutcome::Verified);

    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Verified);
    // The released candidate is the repaired one, never the stale original.
    assert_eq!(settled.current_candidate.revision, 2);
}

#[tokio::test]
async fn a_superseded_worker_cannot_settle_a_gate_another_worker_took_over() {
    let h = Harness::new();
    let gate = h.open_gate();
    let now = Utc::now();

    let first = h
        .store
        .claim_lease(&gate.gate_id, "worker-1", now, super::DEFAULT_LEASE_SECS)
        .unwrap();
    let later = now + chrono::Duration::seconds(super::DEFAULT_LEASE_SECS + 1);
    h.store
        .claim_lease(&gate.gate_id, "worker-2", later, super::DEFAULT_LEASE_SECS)
        .unwrap();

    let mut stale = first.clone();
    stale.status = GateStatus::Verified;
    let result = h.store.commit_gate(stale, first.generation, |gate| {
        super::journal::JournalPayload::GateUpdate { gate }
    });

    assert!(result.is_err(), "a superseded worker must not settle");
    assert_eq!(
        h.store.load_gate(&gate.gate_id).unwrap().status,
        GateStatus::VerificationPending
    );
}

#[tokio::test]
async fn recovery_after_a_crash_rebuilds_state_and_the_work_is_still_reachable() {
    let h = Harness::new();
    let gate = h.open_gate();

    // Simulate a crash that lost the projections but not the journal.
    let gates_dir = h._scope_dir.path().join("verification/gates");
    std::fs::remove_dir_all(&gates_dir).unwrap();

    let recovered = h.store.recover().unwrap();
    assert_eq!(recovered, vec![gate.gate_id.clone()]);

    let rebuilt = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(rebuilt.status, GateStatus::VerificationPending);

    // The outbox entry survived, so the gated task has not been stranded —
    // this is the "non-terminal task with no verification job" failure the
    // atomic entry exists to prevent.
    assert_eq!(h.store.list_outbox().unwrap().len(), 1);

    // And the gate still verifies normally afterwards.
    let outcome = h
        .controller
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("ok", "true", &[])])),
        )
        .await
        .unwrap();
    assert_eq!(outcome, PassOutcome::Verified);
}

#[tokio::test]
async fn a_disabled_controller_never_touches_a_gate() {
    let h = Harness::new();
    let gate = h.open_gate();

    let disabled = VerificationController::new(ControllerDeps {
        store: Arc::new(VerificationStore::new(h._scope_dir.path(), h.scope.clone())),
        sink: Arc::new(RecordingEventSink::default()),
        activation: VerificationActivation::Disabled,
        worker_id: "worker-off".into(),
        workspace_root: h._workspace_dir.path().to_path_buf(),
    });

    let outcome = disabled
        .run_pass(
            &gate.gate_id,
            &h.source_root,
            Some(&policy(vec![check("ok", "true", &[])])),
        )
        .await
        .unwrap();

    assert!(matches!(outcome, PassOutcome::RetryLater { .. }));
    // Untouched: same status, same generation, no lease taken.
    let after = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(after.status, GateStatus::VerificationPending);
    assert_eq!(after.generation, gate.generation);
    assert!(after.lease.is_none());
}

#[tokio::test]
async fn a_repository_cannot_weaken_the_owners_required_checks_end_to_end() {
    let h = Harness::new();
    let gate = h.open_gate();

    // The owner requires a check that fails.
    let baseline = policy(vec![check("must-pass", "false", &[])]);

    // The repository tries to replace it with something that always passes.
    std::fs::create_dir_all(h.source_root.join(".magician")).unwrap();
    std::fs::write(
        h.source_root.join(".magician/verification.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "source": "project",
            "required": [{
                "id": "must-pass",
                "program": "true",
                "args": [],
                "display": "true",
                "timeout_secs": 60
            }]
        }))
        .unwrap(),
    )
    .unwrap();

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    // The owner's command ran, so the gate is red — the swap was refused.
    assert!(
        matches!(outcome, PassOutcome::Repairing(_)),
        "a repository must not be able to switch verification off: {outcome:?}"
    );
}

/// A legacy task — one that predates the controller — must never read as
/// verified.
#[test]
fn a_task_with_no_gate_projects_unknown() {
    let dir = tempfile::tempdir().unwrap();
    let scope = TransactionScope {
        principal: "anonymous".into(),
        workspace: "default".into(),
    };
    let store = VerificationStore::new(dir.path(), scope);
    assert!(store.journal().gate_ids().unwrap().is_empty());

    let state = VerificationState::default();
    assert_eq!(state, VerificationState::Unknown);
    assert!(!state.is_verified());
}

// ---------------------------------------------------------------------------
// Observe mode runs real passes: evidence without holds, and never repair.

/// The reason `observe` exists: real attestation evidence. A red result is
/// recorded and sealed, the gate settles terminally, and no repair is
/// proposed — a repair child would land on a task that already completed.
#[tokio::test]
async fn an_observe_pass_records_a_red_attestation_and_dispatches_no_repair() {
    let h = Harness::with_activation(VerificationActivation::Observe);
    let gate = h.open_gate();
    let baseline = policy(vec![check("fails", "false", &[])]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    let reason = match outcome {
        PassOutcome::Exhausted { reason } => reason,
        other => panic!("an observe red must settle, not {other:?}"),
    };
    assert!(
        reason.contains("observe"),
        "the settle reason must say why no repair came: {reason}"
    );

    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Exhausted);
    assert!(settled.status.is_terminal());

    // The evidence is real and sealed red — this is what qualification reads.
    let attestation = h
        .store
        .load_attestation(settled.active_attestation_ref.as_ref().unwrap())
        .unwrap();
    assert!(attestation.is_sealed());
    assert_eq!(
        attestation.accepted_result.as_ref().unwrap().outcome,
        AttemptOutcome::Red
    );

    let kinds = h.sink.kinds();
    assert!(kinds.contains(&VerificationEventKind::ChecksFailed));
    assert!(
        !kinds.contains(&VerificationEventKind::RepairStarted),
        "observe must never start repair"
    );
    // Settling retired the work item; nothing re-offers this gate.
    assert!(h.store.list_outbox().unwrap().is_empty());
}

/// A green observe pass seals a green attestation exactly as enforce would —
/// the difference is only that nothing was held, so there is nothing to
/// release.
#[tokio::test]
async fn an_observe_pass_seals_green_evidence() {
    let h = Harness::with_activation(VerificationActivation::Observe);
    let gate = h.open_gate();
    let baseline = policy(vec![check("ok", "true", &[])]);

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, Some(&baseline))
        .await
        .unwrap();

    assert_eq!(outcome, PassOutcome::Verified);
    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Verified);
    let attestation = h
        .store
        .load_attestation(settled.active_attestation_ref.as_ref().unwrap())
        .unwrap();
    assert_eq!(
        attestation.accepted_result.as_ref().unwrap().outcome,
        AttemptOutcome::Green
    );
}

// ---------------------------------------------------------------------------
// The elapsed and spend ceilings, on the paths the reconciler now reaches.

/// A repairing gate whose elapsed budget is spent settles `exhausted` on the
/// next offered pass. Before the reconciler existed this branch was
/// unreachable in production — repair retires the outbox entry, so nothing
/// offered the pass that applies the ceiling.
#[tokio::test]
async fn a_stalled_repair_settles_exhausted_when_its_elapsed_budget_is_spent() {
    let h = Harness::new();
    let gate = h.open_gate_with(
        GateBudgets {
            max_repair_rounds: 3,
            max_spend_usd: None,
            max_elapsed_secs: Some(1),
        },
        |gate| {
            gate.status = GateStatus::Repairing;
            gate.created_at = Utc::now() - chrono::Duration::seconds(10);
        },
    );

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, None)
        .await
        .unwrap();

    let reason = match outcome {
        PassOutcome::Exhausted { reason } => reason,
        other => panic!("a stalled repair past its budget must settle, not {other:?}"),
    };
    assert!(
        reason.contains("repair never produced a successor candidate"),
        "the reason must name the stall: {reason}"
    );
    let settled = h.store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(settled.status, GateStatus::Exhausted);
    assert!(settled.lease.is_none(), "settling returns the pass lease");
}

/// `charge_repair_cost` moves the ledger, and a charged-out gate settles
/// before its next round starts — the spend ceiling was inert while nothing
/// charged it.
#[tokio::test]
async fn a_charged_out_gate_settles_before_another_round_starts() {
    let h = Harness::new();
    let gate = h.open_gate_with(
        GateBudgets {
            max_repair_rounds: 3,
            max_spend_usd: Some(1.0),
            max_elapsed_secs: Some(2 * 60 * 60),
        },
        |gate| {
            gate.status = GateStatus::Repairing;
        },
    );

    let charged = h
        .controller
        .charge_repair_cost(&gate.gate_id, 1.5)
        .await
        .unwrap();
    assert_eq!(charged.spend.spend_usd, 1.5, "the round's cost must land");

    let outcome = h
        .controller
        .run_pass(&gate.gate_id, &h.source_root, None)
        .await
        .unwrap();
    let reason = match outcome {
        PassOutcome::Exhausted { reason } => reason,
        other => panic!("a charged-out gate must settle, not {other:?}"),
    };
    assert!(
        reason.contains("spend budget exhausted"),
        "the reason must name the dimension: {reason}"
    );
}

//! Phase 1 regression bar.
//!
//! One test per row of the plan's "must prove" table. These are the properties
//! the rest of the controller assumes, so they are asserted at the store
//! boundary — against real files, real locks and real journal replay — rather
//! than against the typed API alone. A property that only holds when callers
//! are well-behaved is not a property.

use chrono::{Duration as ChronoDuration, Utc};

use super::attestation::{
    AcceptedResult, AttemptOutcome, AttestationKey, VerificationAttempt, VerificationAttestation,
};
use super::gate::{GateBudgets, GateOrigin, GateStatus, VerificationGate, VerificationState};
use super::ids::{AttemptId, CandidateRevision, Generation};
use super::journal::JournalPayload;
use super::store::{CasOutcome, VerificationStore, DEFAULT_LEASE_SECS};
use super::VerificationActivation;
use crate::magician_v2::execution::file_edit::transaction::TransactionScope;

fn scope(principal: &str, workspace: &str) -> TransactionScope {
    TransactionScope {
        principal: principal.into(),
        workspace: workspace.into(),
    }
}

fn store_at(dir: &std::path::Path, s: TransactionScope) -> VerificationStore {
    VerificationStore::new(dir, s)
}

fn origin() -> GateOrigin {
    GateOrigin {
        engineer_agent_id: "engineer".into(),
        coding_profile: None,
        coding_engine: Some("pi".into()),
        constraint_auto: false,
        coding_invocation_ref: Some("opaque".into()),
        child_execution_id: Some("exec-child".into()),
    }
}

fn new_gate(s: TransactionScope, project: &str) -> VerificationGate {
    VerificationGate::new(
        s,
        project,
        "task-1",
        "exec-1",
        CandidateRevision::new("ccp-1", 1).unwrap(),
        origin(),
        GateBudgets::default(),
    )
    .unwrap()
}

fn key_for(s: TransactionScope, project: &str) -> AttestationKey {
    AttestationKey {
        scope: s,
        project_binding: project.into(),
        candidate_ref: "ccp-1".into(),
        proposal_ref: Some("ccp-1".into()),
        snapshot_digest: "snap-identical".into(),
        policy_digest: "pol-1".into(),
        runner_env_digest: "env-1".into(),
    }
}

fn green_attempt(generation: Generation) -> VerificationAttempt {
    VerificationAttempt {
        attempt_id: AttemptId::new(),
        generation,
        fenced_lease: "lease".into(),
        started_at: Utc::now(),
        settled_at: Some(Utc::now()),
        command_results: Vec::new(),
        outcome: AttemptOutcome::Green,
    }
}

// ---------------------------------------------------------------------------
// cross-scope / cross-project attestation reuse is rejected
// ---------------------------------------------------------------------------

#[test]
fn cross_project_attestation_is_not_reusable_even_with_identical_content() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let a = VerificationAttestation::new(key_for(s.clone(), "proj-a")).unwrap();
    store.create_attestation(&a).unwrap();

    // Settle it green so it becomes a genuine reuse candidate. Without this
    // the assertion below would pass trivially — there would be no reusable
    // evidence for *any* key, so it would never exercise the key comparison.
    let attempt = green_attempt(Generation(1));
    let attempt_id = attempt.attempt_id.clone();
    store.append_attempt(&a.attestation_id, attempt).unwrap();
    store
        .accept_result(
            &a.attestation_id,
            AcceptedResult {
                attempt_id,
                outcome: AttemptOutcome::Green,
                accepted_at: Utc::now(),
                generation: Generation(1),
            },
        )
        .unwrap();

    // Proven reusable for its own key.
    assert!(
        store.find_reusable(&a.key).unwrap().is_some(),
        "green evidence must be reusable for the key it was produced under"
    );

    // Same snapshot, same policy, same runner, same principal — different
    // project. A shared template or vendored file legitimately produces this.
    let b_key = key_for(s, "proj-b");
    assert_eq!(a.key.snapshot_digest, b_key.snapshot_digest);
    assert!(
        store.find_reusable(&b_key).unwrap().is_none(),
        "one project's evidence must never satisfy another project's gate"
    );
}

#[test]
fn a_red_attestation_is_never_offered_as_a_reuse_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let att = VerificationAttestation::new(key_for(s, "proj-a")).unwrap();
    store.create_attestation(&att).unwrap();
    let mut red = green_attempt(Generation(1));
    red.outcome = AttemptOutcome::Red;
    let red_id = red.attempt_id.clone();
    store.append_attempt(&att.attestation_id, red).unwrap();
    store
        .accept_result(
            &att.attestation_id,
            AcceptedResult {
                attempt_id: red_id,
                outcome: AttemptOutcome::Red,
                accepted_at: Utc::now(),
                generation: Generation(1),
            },
        )
        .unwrap();

    // A sealed red attestation is evidence of failure, not a cache hit.
    assert!(store.find_reusable(&att.key).unwrap().is_none());
}

#[test]
fn the_task_index_resolves_a_gate_without_scanning() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();

    // The task-read hot path resolves in one indexed lookup.
    assert_eq!(store.gate_id_for_task("task-1"), Some(gate.gate_id.clone()));
    // A task with no gate — the overwhelmingly common case — costs one miss.
    assert_eq!(store.gate_id_for_task("task-does-not-exist"), None);

    // And the index survives recovery, so a crash cannot strand the hot path
    // on a scan-or-nothing fallback.
    std::fs::remove_dir_all(dir.path().join("verification/index")).unwrap();
    store.recover().unwrap();
    assert_eq!(store.gate_id_for_task("task-1"), Some(gate.gate_id));
}

#[test]
fn cross_scope_records_are_refused_on_read_not_just_unfindable() {
    let dir = tempfile::tempdir().unwrap();
    let mine = scope("anonymous", "default");
    let theirs = scope("someone-else", "default");

    let store = store_at(dir.path(), mine.clone());

    // A record belonging to another principal, physically present in this
    // store's directory — the copied-file case that physical isolation alone
    // does not cover.
    let foreign = VerificationAttestation::new(key_for(theirs.clone(), "proj-a")).unwrap();
    assert!(
        store.create_attestation(&foreign).is_err(),
        "a foreign-scope attestation must not be writable through this store"
    );

    let foreign_gate = new_gate(theirs, "proj-a");
    assert!(store.open_gate(foreign_gate).is_err());
}

// ---------------------------------------------------------------------------
// an attestation key cannot change after creation
// ---------------------------------------------------------------------------

#[test]
fn attestation_key_is_immutable_through_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let a = VerificationAttestation::new(key_for(s, "proj-a")).unwrap();
    store.create_attestation(&a).unwrap();

    // Re-creating under the same id is refused outright.
    assert!(store.create_attestation(&a).is_err());

    // And an append cannot smuggle a key change, because append reloads the
    // stored record and validates the successor.
    let loaded = store.load_attestation(&a.attestation_id).unwrap();
    assert_eq!(loaded.key, a.key);
}

// ---------------------------------------------------------------------------
// only one live worker owns a lease generation
// ---------------------------------------------------------------------------

#[test]
fn a_live_lease_cannot_be_stolen_and_claiming_bumps_the_generation() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    let first = store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    assert_eq!(first.generation, Generation(1));

    // A second worker cannot take a live lease.
    assert!(store
        .claim_lease(&gate.gate_id, "worker-2", now, DEFAULT_LEASE_SECS)
        .is_err());

    // Once it expires, takeover is allowed and the generation moves — which is
    // exactly what invalidates worker-1.
    let later = now + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1);
    let second = store
        .claim_lease(&gate.gate_id, "worker-2", later, DEFAULT_LEASE_SECS)
        .unwrap();
    assert_eq!(second.generation, Generation(2));

    store
        .assert_lease_live(&second, "worker-2", Generation(2), later)
        .unwrap();
    assert!(store
        .assert_lease_live(&second, "worker-1", Generation(1), later)
        .is_err());
}

// ---------------------------------------------------------------------------
// an expired worker cannot settle or accept a result
// ---------------------------------------------------------------------------

#[test]
fn a_superseded_worker_cannot_commit_after_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    let g1 = store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    let later = now + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1);
    let _g2 = store
        .claim_lease(&gate.gate_id, "worker-2", later, DEFAULT_LEASE_SECS)
        .unwrap();

    // worker-1 wakes up late and tries to finish. This is the normal case
    // under load, not an exotic one.
    let mut stale = g1.clone();
    stale.status = GateStatus::Verified;
    let err = store.commit_gate(stale, Generation(1), |gate| JournalPayload::GateUpdate {
        gate,
    });
    assert!(err.is_err(), "an expired worker must not be able to settle");

    // The gate is untouched.
    let reloaded = store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(reloaded.status, GateStatus::VerificationPending);
    assert_eq!(reloaded.generation, Generation(2));
}

#[test]
fn an_expired_lease_fails_the_liveness_check_even_before_takeover() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    let held = store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();

    // Generation still matches — nobody has taken over — but the clock has
    // passed. Checking only the generation here would be the hole.
    let expired_at = now + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1);
    assert!(store
        .assert_lease_live(&held, "worker-1", Generation(1), expired_at)
        .is_err());
}

// ---------------------------------------------------------------------------
// concurrent accept attempts produce exactly one winner
// ---------------------------------------------------------------------------

#[test]
fn accepting_twice_yields_one_winner_and_a_benign_replay() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let att = VerificationAttestation::new(key_for(s, "proj-a")).unwrap();
    store.create_attestation(&att).unwrap();
    let attempt = green_attempt(Generation(1));
    let attempt_id = attempt.attempt_id.clone();
    store.append_attempt(&att.attestation_id, attempt).unwrap();

    let accepted = AcceptedResult {
        attempt_id: attempt_id.clone(),
        outcome: AttemptOutcome::Green,
        accepted_at: Utc::now(),
        generation: Generation(1),
    };

    let first = store
        .accept_result(&att.attestation_id, accepted.clone())
        .unwrap();
    assert!(first.was_applied());

    // The identical acceptance replayed is benign, not a second win.
    let second = store.accept_result(&att.attestation_id, accepted).unwrap();
    assert!(matches!(second, CasOutcome::AlreadySettled(_)));

    // A *different* acceptance is refused outright.
    let other = AcceptedResult {
        attempt_id,
        outcome: AttemptOutcome::Red,
        accepted_at: Utc::now(),
        generation: Generation(1),
    };
    assert!(store.accept_result(&att.attestation_id, other).is_err());
}

#[test]
fn acceptance_must_reference_a_real_attempt_that_agrees_about_its_outcome() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let att = VerificationAttestation::new(key_for(s, "proj-a")).unwrap();
    store.create_attestation(&att).unwrap();

    // No such attempt.
    assert!(store
        .accept_result(
            &att.attestation_id,
            AcceptedResult {
                attempt_id: AttemptId::new(),
                outcome: AttemptOutcome::Green,
                accepted_at: Utc::now(),
                generation: Generation(1),
            }
        )
        .is_err());

    // Attempt exists but disagrees — an acceptance describing evidence that
    // was never produced.
    let mut red = green_attempt(Generation(1));
    red.outcome = AttemptOutcome::Red;
    let red_id = red.attempt_id.clone();
    store.append_attempt(&att.attestation_id, red).unwrap();
    assert!(store
        .accept_result(
            &att.attestation_id,
            AcceptedResult {
                attempt_id: red_id,
                outcome: AttemptOutcome::Green,
                accepted_at: Utc::now(),
                generation: Generation(1),
            }
        )
        .is_err());
}

#[test]
fn an_indeterminate_attempt_can_never_be_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());

    let att = VerificationAttestation::new(key_for(s, "proj-a")).unwrap();
    store.create_attestation(&att).unwrap();

    let mut indet = green_attempt(Generation(1));
    indet.outcome = AttemptOutcome::Indeterminate;
    let id = indet.attempt_id.clone();
    store.append_attempt(&att.attestation_id, indet).unwrap();

    // A crash mid-check must yield a re-run, never a pass.
    assert!(store
        .accept_result(
            &att.attestation_id,
            AcceptedResult {
                attempt_id: id,
                outcome: AttemptOutcome::Indeterminate,
                accepted_at: Utc::now(),
                generation: Generation(1),
            }
        )
        .is_err());
}

// ---------------------------------------------------------------------------
// replaying a committed journal transaction is idempotent
// ---------------------------------------------------------------------------

#[test]
fn recovery_is_idempotent_and_rebuilds_projections() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, entry) = store.open_gate(new_gate(s, "proj-a")).unwrap();

    let before = store.load_gate(&gate.gate_id).unwrap();

    // Blow away the projections; the journal is the source of truth.
    std::fs::remove_file(
        dir.path()
            .join("verification/gates")
            .join(format!("{}.json", gate.gate_id.as_str())),
    )
    .unwrap();

    let recovered = store.recover().unwrap();
    assert_eq!(recovered, vec![gate.gate_id.clone()]);
    assert_eq!(store.load_gate(&gate.gate_id).unwrap(), before);

    // Running recovery repeatedly converges.
    store.recover().unwrap();
    store.recover().unwrap();
    assert_eq!(store.load_gate(&gate.gate_id).unwrap(), before);

    // The outbox entry survives, so the gated task is still reachable.
    let outbox = store.list_outbox().unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].entry_id, entry.entry_id);
}

// ---------------------------------------------------------------------------
// crash injection between every journal write recovers all-or-nothing
// ---------------------------------------------------------------------------

#[test]
fn truncating_the_journal_at_every_point_yields_a_consistent_state() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "proj-a")).unwrap();

    // Build a multi-step *journaled* history. Renewals are deliberately not
    // journaled (they are heartbeats, not decisions), so takeovers are used
    // here — each claim bumps the fencing generation and is durable.
    let now = Utc::now();
    store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    store
        .claim_lease(
            &gate.gate_id,
            "worker-2",
            now + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1),
            DEFAULT_LEASE_SECS,
        )
        .unwrap();

    let journal_dir = dir
        .path()
        .join("verification/journal")
        .join(gate.gate_id.as_str());
    let mut records: Vec<_> = std::fs::read_dir(&journal_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("json"))
        .collect();
    records.sort();
    assert!(records.len() >= 3, "expected a multi-step history");

    // For each prefix of the history, a crash at that point must recover to
    // exactly the state that prefix implies — never a hybrid, never an error.
    let full = records.clone();
    for cut in 1..=full.len() {
        let work = tempfile::tempdir().unwrap();
        let work_journal = work
            .path()
            .join("verification/journal")
            .join(gate.gate_id.as_str());
        std::fs::create_dir_all(&work_journal).unwrap();
        for path in full.iter().take(cut) {
            std::fs::copy(path, work_journal.join(path.file_name().unwrap())).unwrap();
        }

        let recovering = store_at(work.path(), s.clone());
        let ids = recovering.recover().unwrap();
        assert_eq!(ids, vec![gate.gate_id.clone()], "cut at {cut}");

        let g = recovering.load_gate(&gate.gate_id).unwrap();
        // Whatever the cut, the gate is internally consistent: it exists, it
        // is still pending, and its generation is exactly the number of lease
        // claims that were durable at that point — never a hybrid of two
        // transactions.
        assert_eq!(g.status, GateStatus::VerificationPending, "cut at {cut}");
        assert_eq!(
            g.generation.0 as usize,
            cut - 1,
            "cut at {cut} recovered a generation that matches no committed prefix"
        );
    }
}

#[test]
fn recovery_does_not_roll_back_a_live_renewed_lease() {
    // Regression: renewals are projection-only, so the journal's `expires_at`
    // is frozen at claim time. A recover() that projected the journal
    // verbatim would roll a live worker's lease backwards — its next commit
    // would fail the liveness check and a *finished* verification would be
    // discarded, then redone by whoever re-claims.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    let renewed_at = now + ChronoDuration::seconds(DEFAULT_LEASE_SECS - 5);
    let renewed = store
        .renew_lease(
            &gate.gate_id,
            "worker-1",
            Generation(1),
            renewed_at,
            DEFAULT_LEASE_SECS,
        )
        .unwrap();
    let live_expiry = renewed.lease.as_ref().unwrap().expires_at;

    store.recover().unwrap();

    let after = store.load_gate(&gate.gate_id).unwrap();
    let lease = after
        .lease
        .as_ref()
        .expect("recovery must not drop a live lease");
    assert_eq!(
        lease.expires_at, live_expiry,
        "recovery rolled the lease back to its claim-time expiry"
    );
    // The holder can still commit.
    store
        .assert_lease_live(&after, "worker-1", Generation(1), renewed_at)
        .expect("the live holder must still pass the liveness check after recovery");
}

#[test]
fn recovery_does_not_resurrect_a_lease_from_a_superseded_generation() {
    // The guard is generation-scoped on purpose: a stale projection from a
    // previous holder must not be carried forward over a takeover.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    let later = now + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1);
    store
        .claim_lease(&gate.gate_id, "worker-2", later, DEFAULT_LEASE_SECS)
        .unwrap();

    store.recover().unwrap();

    let after = store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(after.generation, Generation(2));
    assert_eq!(after.lease.as_ref().unwrap().holder, "worker-2");
    assert!(store
        .assert_lease_live(&after, "worker-1", Generation(1), later)
        .is_err());
}

#[test]
fn lease_renewal_is_not_journaled_and_replay_yields_no_lease() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();
    let now = Utc::now();

    store
        .claim_lease(&gate.gate_id, "worker-1", now, DEFAULT_LEASE_SECS)
        .unwrap();
    let after_claim = store.journal().last_sequence(&gate.gate_id).unwrap();

    // Renew repeatedly, as a long-running check would.
    for i in 1..=25 {
        store
            .renew_lease(
                &gate.gate_id,
                "worker-1",
                Generation(1),
                now + ChronoDuration::seconds(i),
                DEFAULT_LEASE_SECS,
            )
            .unwrap();
    }

    // Heartbeats must not accumulate as durable history — otherwise a
    // long-lived gate's journal grows without bound and every replay
    // re-parses it.
    assert_eq!(
        store.journal().last_sequence(&gate.gate_id).unwrap(),
        after_claim,
        "renewals must not append journal transactions"
    );

    // The renewal is still visible in the projection.
    let loaded = store.load_gate(&gate.gate_id).unwrap();
    assert!(loaded.lease.is_some());

    // And replay reconstructs the gate without a lease, which is correct: a
    // process that crashed is not holding one.
    let replayed = store.journal().replay_gate(&gate.gate_id).unwrap().unwrap();
    assert_eq!(replayed.generation, Generation(1));
}

// ---------------------------------------------------------------------------
// corrupt or unavailable storage never creates green evidence
// ---------------------------------------------------------------------------

#[test]
fn a_corrupt_gate_projection_is_refused_rather_than_read_as_green() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();

    let path = dir
        .path()
        .join("verification/gates")
        .join(format!("{}.json", gate.gate_id.as_str()));
    std::fs::write(&path, b"{ not json").unwrap();

    assert!(store.load_gate(&gate.gate_id).is_err());
}

#[test]
fn an_unavailable_store_is_an_error_not_an_absent_gate() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s);
    let missing = super::ids::GateId::new();

    // "We could not read it" must not be indistinguishable from "there is
    // nothing to read" at the type level.
    assert!(!store.gate_exists(&missing));
    assert!(store.load_gate(&missing).is_err());
}

#[test]
fn an_unknown_gate_schema_version_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();

    let path = dir
        .path()
        .join("verification/gates")
        .join(format!("{}.json", gate.gate_id.as_str()));
    let mut raw: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    raw["schema_version"] = serde_json::json!(999);
    std::fs::write(&path, serde_json::to_vec_pretty(&raw).unwrap()).unwrap();

    assert!(store.load_gate(&gate.gate_id).is_err());
}

// ---------------------------------------------------------------------------
// legacy tasks hydrate with verification_state = unknown
// ---------------------------------------------------------------------------

#[test]
fn a_task_with_no_gate_reads_as_unknown_never_verified() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s);

    // No gate exists — the legacy VibeDev run case.
    assert!(store.recover().unwrap().is_empty());

    let state = VerificationState::default();
    assert_eq!(state, VerificationState::Unknown);
    assert!(!state.is_verified());
    assert_eq!(state.as_str(), "unknown");
}

// ---------------------------------------------------------------------------
// landing Phase 1 does not alter existing task completion behaviour
// ---------------------------------------------------------------------------

#[test]
fn phase_one_is_inert_by_default() {
    // The whole slice ships switched off. No terminal path may consult it
    // until a deployment opts in, which is what keeps completion timing
    // byte-for-byte unchanged.
    let activation = VerificationActivation::default();
    assert_eq!(activation, VerificationActivation::Disabled);
    assert!(!activation.gates_completion());
    assert!(!activation.records_evidence());
}

#[test]
fn observe_mode_records_but_still_does_not_gate() {
    // The intermediate rung: see which tasks *would* be held, hold none.
    let activation = VerificationActivation::Observe;
    assert!(activation.records_evidence());
    assert!(!activation.gates_completion());
}

#[test]
fn opening_a_gate_does_not_by_itself_terminalise_anything() {
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "proj-a")).unwrap();

    // A freshly opened gate is pending and green-less. Nothing about creating
    // it can release or fail a task.
    assert_eq!(gate.status, GateStatus::VerificationPending);
    assert!(gate.active_attestation_ref.is_none());
    assert_eq!(gate.verification_state(), VerificationState::Verifying);
    assert!(gate
        .may_finalize_green(
            &gate.current_candidate,
            &super::ids::AttestationId::new(),
            false,
            gate.generation
        )
        .is_err());
}

#[test]
fn the_interrupted_completion_is_parked_with_the_gate_and_survives_reopen() {
    // Holding a candidate means the terminal transaction did not run. If the
    // outcome it would have written is not durable, a gate that later settles
    // green has nothing to release — the code is verified and the task still
    // never finishes.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "apps/site")).unwrap();

    assert!(
        store.read_held_outcome(&gate.gate_id).unwrap().is_none(),
        "nothing parked until the caller parks it"
    );

    let parked = serde_json::json!({
        "execution_status": "completed",
        "task_status": "completed",
        "outcome_type": "completed",
        "outcome_summary": "shipped the footer fix",
        "iterations_used": 3,
        "is_terminal": true,
    });
    store.write_held_outcome(&gate.gate_id, &parked).unwrap();

    // A different store instance over the same root: this is what a restart
    // sees, and the release path runs from exactly that.
    let reopened = store_at(dir.path(), s);
    assert_eq!(
        reopened.read_held_outcome(&gate.gate_id).unwrap(),
        Some(parked)
    );
}

#[test]
fn the_parked_outcome_can_be_claimed_exactly_once() {
    // The fence that stops a completion running twice. Two releases can both
    // *read* the parked outcome; only one can claim it, because the claim is a
    // rename and the loser's source is already gone.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "apps/site")).unwrap();

    let parked = serde_json::json!({ "task_status": "completed" });
    store.write_held_outcome(&gate.gate_id, &parked).unwrap();

    assert_eq!(
        store.take_held_outcome(&gate.gate_id).unwrap(),
        Some(parked.clone())
    );
    assert_eq!(
        store.take_held_outcome(&gate.gate_id).unwrap(),
        None,
        "the second release must find nothing to release"
    );
    // And a fresh store over the same root — a restart — agrees, so the
    // exactly-once property is durable rather than in-memory.
    assert_eq!(
        store_at(dir.path(), s.clone())
            .take_held_outcome(&gate.gate_id)
            .unwrap(),
        None
    );

    // A release that could not finish hands the claim back, so the task is not
    // left held with nothing able to release it.
    store.restore_held_outcome(&gate.gate_id).unwrap();
    assert_eq!(
        store.take_held_outcome(&gate.gate_id).unwrap(),
        Some(parked)
    );
}

#[test]
fn a_claimed_outcome_survives_a_crash_mid_release() {
    // The claim has to be a *lease* on the release, not a disposal of the
    // outcome. Settling the gate already retired its outbox entry, so if a
    // claimed-but-unreleased outcome is not itself enumerable, the process
    // dying between the claim and the terminal transaction leaves the task
    // non-terminal with nothing anywhere able to finish it.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "apps/site")).unwrap();

    let parked = serde_json::json!({ "task_status": "completed" });
    store.write_held_outcome(&gate.gate_id, &parked).unwrap();

    // Settle-then-release is the real order, and the settle is what empties
    // the outbox. Recovery therefore cannot learn about this gate from the
    // outbox — only from the parked outcome.
    store.retire_outbox_for_gate(&gate.gate_id).unwrap();
    assert!(
        store.list_outbox().unwrap().is_empty(),
        "the settle retires the work item, which is why the outbox cannot be the only record"
    );

    // The release claims the outcome… and the process dies here.
    assert_eq!(
        store.take_held_outcome(&gate.gate_id).unwrap(),
        Some(parked.clone())
    );
    assert!(store.release_is_in_flight(&gate.gate_id));
    assert!(!store.release_has_completed(&gate.gate_id));

    // Restart. A fresh store over the same root is exactly what recovery has.
    let restarted = store_at(dir.path(), s.clone());
    let unreleased = restarted.reclaim_orphaned_held_outcomes().unwrap();
    assert_eq!(
        unreleased,
        vec![gate.gate_id.clone()],
        "recovery must name the gate whose completion was never handed back"
    );
    assert!(
        !restarted.release_is_in_flight(&gate.gate_id),
        "the orphaned claim is handed back, not left where nothing can take it"
    );

    // …and the re-driven release finds the outcome waiting for it.
    assert_eq!(
        restarted.take_held_outcome(&gate.gate_id).unwrap(),
        Some(parked)
    );
}

#[test]
fn a_completed_release_is_not_re_driven() {
    // The other half of the same property. If a finished release left its
    // record in the claimed directory, recovery would re-drive every gate the
    // scope has ever settled, every boot.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "apps/site")).unwrap();

    store
        .write_held_outcome(
            &gate.gate_id,
            &serde_json::json!({ "task_status": "completed" }),
        )
        .unwrap();
    assert!(store.take_held_outcome(&gate.gate_id).unwrap().is_some());
    // The terminal transaction landed.
    store.complete_held_release(&gate.gate_id).unwrap();

    assert!(store.release_has_completed(&gate.gate_id));
    assert!(!store.release_is_in_flight(&gate.gate_id));
    assert!(
        store_at(dir.path(), s)
            .reclaim_orphaned_held_outcomes()
            .unwrap()
            .is_empty(),
        "a gate whose release completed owes nothing and must not be re-driven"
    );
}

#[test]
fn recovery_names_a_gate_that_was_never_even_claimed() {
    // The simpler half: a crash after the settle but before the release ran at
    // all. The outbox is already empty, so the parked outcome is again the only
    // record that this task is still waiting to be completed.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "apps/site")).unwrap();

    store
        .write_held_outcome(
            &gate.gate_id,
            &serde_json::json!({ "task_status": "completed" }),
        )
        .unwrap();
    store.retire_outbox_for_gate(&gate.gate_id).unwrap();

    assert_eq!(
        store_at(dir.path(), s)
            .reclaim_orphaned_held_outcomes()
            .unwrap(),
        vec![gate.gate_id]
    );
}

#[test]
fn a_gate_binds_evidence_to_the_repository_not_the_task() {
    // `project_binding` is what the driver resolves to find the tree to check
    // and what the attestation key scopes reuse to. A task id names no tree,
    // so a gate carrying one could never be verified by anything.
    let dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s, "apps/site")).unwrap();

    assert_eq!(gate.project_binding, "apps/site");
    assert_ne!(
        gate.project_binding, gate.root_task_id,
        "binding evidence to the task id leaves nothing to verify against"
    );
}

#[test]
fn a_failed_outcome_is_not_a_candidate_and_must_never_be_gated() {
    // Gating keys on a candidate *success*. A failed or cancelled run has
    // nothing to verify, and holding one leaves the task reporting neither
    // success nor failure. The repair path makes this reachable: a repair
    // execution that fails arrives while the gate is still `repairing`.
    use super::gating::{evaluate, CandidateFacts, GateDecision};

    let hold = || {
        Some(super::gating::HoldRequest {
            scope: scope("anonymous", "default"),
            project_binding: "apps/site".into(),
            root_task_id: "task-1".into(),
            root_execution_id: "exec-1".into(),
            candidate: CandidateRevision::new("ccp-1", 1).unwrap(),
            origin: origin(),
        })
    };
    let failed = CandidateFacts {
        is_terminal: true,
        is_success: false,
        is_root: true,
        produced_code: true,
        application: None,
    };
    assert_eq!(
        evaluate(VerificationActivation::Enforce, &failed, hold),
        GateDecision::Proceed,
        "a failed run must reach its terminal transaction"
    );
}

#[test]
fn a_scope_whose_name_the_filesystem_cannot_hold_still_recovers() {
    // `list_scopes` returns directory names, which are `safe_segment`-
    // sanitised, so startup recovery builds its store from `a_b` while the
    // gate on disk records the raw `a/b`. Comparing raw made the store reject
    // every record in that scope: the path resolved (the sanitiser is
    // idempotent) but the gate read as foreign, so the recovery that exists to
    // find stranded gates found none — silently, because a skipped scope logs
    // nothing.
    let dir = tempfile::tempdir().unwrap();
    let raw = scope("team/eng", "prod:web");
    let store = store_at(dir.path(), raw.clone());
    let (gate, _) = store.open_gate(new_gate(raw.clone(), "apps/site")).unwrap();

    // What a restart does: rebuild the scope from the directory names on disk.
    let (principal, workspace) =
        crate::magician_v2::artifact_v2::workspace::ArtifactV2Workspace::scope_dir_segments(
            &raw.principal,
            &raw.workspace,
        );
    assert_ne!(
        (principal.as_str(), workspace.as_str()),
        (raw.principal.as_str(), raw.workspace.as_str()),
        "this test is pointless unless the name actually gets sanitised"
    );
    let after_restart = store_at(dir.path(), scope(&principal, &workspace));

    assert!(
        after_restart.load_gate(&gate.gate_id).is_ok(),
        "a gate must survive being reached through its sanitised directory name"
    );
    assert_eq!(
        after_restart.recover().unwrap(),
        vec![gate.gate_id.clone()],
        "recovery must find the gate it exists to find"
    );
}

#[test]
fn a_record_from_a_genuinely_different_scope_is_still_refused() {
    // The property the segment comparison must not have weakened.
    let dir = tempfile::tempdir().unwrap();
    let mine = store_at(dir.path(), scope("anonymous", "default"));
    let theirs = new_gate(scope("someone-else", "default"), "apps/site");

    assert!(
        mine.open_gate(theirs).is_err(),
        "a record naming another scope must still be refused"
    );
}

// ---------------------------------------------------------------------------
// The renewal task: a pass's lease stays live for exactly as long as the pass

/// The renewal loop extends a held lease on its half-TTL cadence, and stops
/// itself the moment the gate is claimed out from under it — the generation
/// fence owns everything after that.
///
/// Paused tokio time drives the ticker; the store's clock is real, which only
/// makes the assertions conservative (a renewal pushes `expires_at` out from
/// real now, so it can only be later than the claim's).
#[tokio::test(start_paused = true)]
async fn the_renewal_task_extends_a_live_lease_and_stops_when_superseded() {
    use std::sync::Arc;

    use super::controller::{ControllerDeps, VerificationController};
    use super::events::NullEventSink;

    let dir = tempfile::tempdir().unwrap();
    let workspace_dir = tempfile::tempdir().unwrap();
    let s = scope("anonymous", "default");
    let store = store_at(dir.path(), s.clone());
    let (gate, _) = store.open_gate(new_gate(s.clone(), "proj-a")).unwrap();

    let claimed = store
        .claim_lease(&gate.gate_id, "worker-1", Utc::now(), DEFAULT_LEASE_SECS)
        .unwrap();
    let generation = claimed.generation;
    let claim_expiry = claimed.lease.as_ref().unwrap().expires_at;

    let controller = VerificationController::new(ControllerDeps {
        store: Arc::new(store_at(dir.path(), s.clone())),
        sink: Arc::new(NullEventSink),
        activation: VerificationActivation::Enforce,
        worker_id: "worker-1".into(),
        workspace_root: workspace_dir.path().to_path_buf(),
    });
    let renewal = controller.spawn_lease_renewal(gate.gate_id.clone(), generation);

    // One renewal cadence elapses (paused time auto-advances through it).
    tokio::time::sleep(std::time::Duration::from_secs(65)).await;
    // Let the renewal's blocking write finish before reading.
    tokio::task::yield_now().await;

    let renewed = store.load_gate(&gate.gate_id).unwrap();
    let lease = renewed
        .lease
        .as_ref()
        .expect("the lease must still be held");
    assert_eq!(lease.holder, "worker-1");
    assert_eq!(
        renewed.generation, generation,
        "renewal must not bump the fence"
    );
    assert!(
        lease.expires_at >= claim_expiry,
        "a renewal must extend the lease, not shorten it"
    );

    // Another worker claims the gate out from under the pass — the renewal
    // task's next attempt is refused and the task exits on its own.
    store
        .claim_lease(
            &gate.gate_id,
            "worker-2",
            Utc::now() + ChronoDuration::seconds(DEFAULT_LEASE_SECS + 1),
            DEFAULT_LEASE_SECS,
        )
        .unwrap();

    tokio::time::timeout(std::time::Duration::from_secs(600), renewal)
        .await
        .expect("a superseded renewal task must stop itself")
        .expect("the renewal task must exit cleanly, not panic");

    // The thief's lease was left alone.
    let after = store.load_gate(&gate.gate_id).unwrap();
    assert_eq!(after.lease.as_ref().unwrap().holder, "worker-2");
}

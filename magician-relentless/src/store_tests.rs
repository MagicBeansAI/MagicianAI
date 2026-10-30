use std::{collections::BTreeMap, sync::Arc};

use chrono::{Duration, Utc};
use uuid::Uuid;

use super::{budget::*, contract::*, review::*, store::*, tests::*};

struct Fixture {
    temp: tempfile::TempDir,
    store: Arc<RunStore>,
    id: Uuid,
    now: chrono::DateTime<Utc>,
}

impl Fixture {
    fn new(proposal: DefinitionOfDone) -> Self {
        Self::with_policy(proposal, None, Default::default())
    }

    fn with_policy(
        mut proposal: DefinitionOfDone,
        routing_policy: Option<crate::routing::RoutingPolicy>,
        handoff_kind: crate::routing::HandoffKind,
    ) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let store = Arc::new(RunStore::new(
            temp.path(),
            RunScope {
                principal: "owner".into(),
                workspace: "work".into(),
            },
        ));
        let id = Uuid::new_v4();
        let now = Utc::now();
        proposal.sources.clear();
        let source_hash = store.put_blob(id, b"immutable owner source").unwrap();
        proposal.sources.push(SourcePin {
            id: "owner-note".into(),
            content_hash: source_hash.clone(),
        });
        store
            .admit(
                id,
                proposal.try_into().unwrap(),
                Admission {
                    owner_principal: "owner".into(),
                    source_id: "owner-approved-contract".into(),
                    source_hash: hash("admitted source"),
                    task_id: "task".into(),
                    routing_policy,
                    handoff_kind,
                    not_after: None,
                },
                now,
            )
            .unwrap();
        store.claim(id, "worker".into(), now).unwrap();
        Self {
            temp,
            store,
            id,
            now,
        }
    }

    fn commit(&self, event: RunEvent) -> anyhow::Result<RunState> {
        self.store.commit(self.id, 1, event, self.now)
    }

    fn start(&self) {
        self.commit(RunEvent::AttemptStarted {
            actor: intent("actor", 50, OperationClass::Actor),
            review: Allowance {
                id: "review-headroom".into(),
                input_hash: hash("review-headroom"),
                maximum_microusd: 40,
                remaining_microusd: 40,
                closed: false,
            },
        })
        .unwrap();
    }

    fn seal(&self) -> ArtifactRef {
        let artifact = self
            .store
            .prepare_artifact(
                self.id,
                &BTreeMap::from([("answer.md".into(), b"sealed candidate".to_vec())]),
            )
            .unwrap();
        self.commit(RunEvent::ArtifactSealed {
            artifact: artifact.clone(),
        })
        .unwrap();
        artifact
    }
}

#[test]
fn evidence_dispatch_proof_survives_unknown_reservation_without_granting_dispatch() {
    let f = Fixture::new(proposal());
    f.start();
    let artifact = f.seal();
    f.commit(RunEvent::OperationSettled {
        operation_id: "actor".into(),
        actual_cost_microusd: 20,
        receipt_hash: hash("actor receipt"),
    })
    .unwrap();
    let operation = intent("critic-proof", 0, OperationClass::Critic);
    let critic_id = f.store.load(f.id).unwrap().contract.definition().critics[0]
        .id
        .clone();
    f.commit(RunEvent::ReviewStarted {
        critic_id,
        operation: operation.clone(),
    })
    .unwrap();
    let preparation = hash("check recipe");
    assert!(f
        .store
        .begin_evidence_preparation(
            f.id,
            "worker",
            &operation,
            &artifact.hash,
            &preparation,
            f.now
        )
        .unwrap());
    f.commit(RunEvent::OperationUnknown {
        operation_id: operation.id.clone(),
    })
    .unwrap();
    assert!(f
        .store
        .evidence_preparation_started(f.id, &artifact.hash, &preparation)
        .unwrap());
    assert!(f
        .store
        .begin_evidence_preparation(
            f.id,
            "worker",
            &operation,
            &artifact.hash,
            &preparation,
            f.now
        )
        .is_err());
    assert!(!f
        .store
        .evidence_preparation_started(f.id, &artifact.hash, &hash("other recipe"))
        .unwrap());
}

#[test]
fn independent_route_and_capture_replay_without_a_second_review_or_handoff() {
    use crate::routing::{HandoffKind, ReviewRecommendation, ReviewRoute, RoutingPolicy};
    let mut p = proposal();
    p.criteria.retain(|c| c.id != "grounded");
    for pin in &mut p.critics {
        pin.criteria.retain(|c| c != "grounded");
    }
    let policy = RoutingPolicy {
        version_hash: hash("owner capture policy"),
        reviewer: p.critics.iter().find_map(|p| p.model.clone()).unwrap(),
        min_confidence: 0.8,
        owner_review_required: false,
        allow_automatic_internal: false,
        allow_automatic_capture: true,
    };
    let f = Fixture::with_policy(p, Some(policy.clone()), HandoffKind::Capture);
    f.start();
    let artifact = f.seal();
    f.commit(RunEvent::OperationSettled {
        operation_id: "actor".into(),
        actual_cost_microusd: 20,
        receipt_hash: hash("actor receipt"),
    })
    .unwrap();
    let c = f.store.load(f.id).unwrap().contract;
    let ctx = ReviewContext {
        artifact_hash: artifact.hash.clone(),
        source_epoch: 0,
        artifact_texts: vec!["sealed candidate".into()],
        sources: BTreeMap::from([("owner-note".into(), b"immutable owner source".to_vec())]),
        evidence_ids: artifact.evidence_ids.clone(),
    };
    for (i, mut verdict) in verdicts(&c, &ctx, 0.9, true).into_iter().enumerate() {
        verdict.evidence_ids = artifact.evidence_ids.iter().cloned().collect();
        let operation_id = format!("critic-{i}");
        if c.definition().critics[i].model.as_ref() == Some(&policy.reviewer) {
            verdict.review_recommendation = Some(ReviewRecommendation {
                route: ReviewRoute::AutomaticCapture,
                confidence: 0.9,
            });
        }
        f.commit(RunEvent::ReviewStarted {
            critic_id: verdict.critic_id.clone(),
            operation: intent(&operation_id, 1, OperationClass::Critic),
        })
        .unwrap();
        f.commit(RunEvent::CriticCompleted {
            critic_id: verdict.critic_id.clone(),
            operation_id: operation_id.clone(),
            verdict,
            receipt: OperationReceipt {
                input_hash: hash(&operation_id),
                actual_cost_microusd: Some(1),
                receipt_hash: hash(&format!("receipt-{i}")),
            },
        })
        .unwrap();
    }
    f.commit(RunEvent::AttemptDecided).unwrap();
    let routed = f.commit(RunEvent::ReviewRouted).unwrap();
    assert_eq!(routed.routing.unwrap().route, ReviewRoute::AutomaticCapture);
    assert!(f
        .store
        .commit(
            f.id,
            1,
            RunEvent::OwnerReviewed {
                artifact_hash: artifact.hash.clone(),
                approved: true
            },
            f.now
        )
        .is_err());
    f.commit(RunEvent::HandoffRecorded).unwrap();
    assert!(f.commit(RunEvent::HandoffRecorded).is_err());
    let replayed = RunStore::new(f.temp.path(), routed.scope)
        .load(f.id)
        .unwrap();
    assert_eq!(
        replayed.handoff.unwrap(),
        crate::routing::Handoff {
            artifact_hash: artifact.hash,
            kind: HandoffKind::Capture
        }
    );
    assert_eq!(replayed.owner_approved, None);
    assert_eq!(replayed.attempts.len(), 1);
}

fn intent(id: &str, maximum: u64, class: OperationClass) -> OperationIntent {
    OperationIntent {
        id: id.into(),
        input_hash: hash(id),
        class,
        maximum_cost_microusd: maximum,
    }
}

fn check_only() -> DefinitionOfDone {
    let mut p = proposal();
    p.criteria.retain(|c| c.kind == CriterionKind::Check);
    p.critics.retain(|c| c.model.is_none());
    p.score_at_least = None;
    p
}

#[test]
fn durable_adapter_result_recovers_once_and_cancel_prevents_a_new_dispatch() {
    let f = Fixture::new(check_only());
    f.start();
    let op = intent("actor", 50, OperationClass::Actor);
    assert!(matches!(
        f.store.lookup_adapter_job(f.id, &op).unwrap(),
        AdapterJobLookup::NotStarted
    ));
    assert!(f
        .store
        .begin_adapter_job(f.id, "stale-worker", &op, f.now)
        .is_err());
    f.store
        .begin_adapter_job(f.id, "worker", &op, f.now)
        .unwrap();
    assert!(f
        .store
        .begin_adapter_job(f.id, "worker", &op, f.now)
        .is_err());
    assert!(matches!(
        f.store.lookup_adapter_job(f.id, &op).unwrap(),
        AdapterJobLookup::InFlight
    ));
    f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).unwrap();
    assert!(f
        .store
        .begin_adapter_job(f.id, "worker", &op, f.now)
        .is_err());
    f.store
        .finish_adapter_job(f.id, &op, b"exact completed response")
        .unwrap();
    assert!(f
        .store
        .finish_adapter_job(f.id, &op, b"different response")
        .is_err());
    let restarted = RunStore::new(f.temp.path(), f.store.load(f.id).unwrap().scope);
    match restarted.lookup_adapter_job(f.id, &op).unwrap() {
        AdapterJobLookup::Completed(bytes) => assert_eq!(bytes, b"exact completed response"),
        _ => panic!("response not recovered"),
    }
    assert_eq!(restarted.load(f.id).unwrap().status, RunStatus::Cancelled);
}

fn accepted_check_fixture() -> (Fixture, ArtifactRef, RunState) {
    checked_fixture(check_only(), true, Some(20))
}

fn checked_fixture(
    p: DefinitionOfDone,
    feasible: bool,
    cost: Option<u64>,
) -> (Fixture, ArtifactRef, RunState) {
    let f = Fixture::new(p);
    f.start();
    let artifact = f.seal();
    if let Some(cost) = cost {
        f.commit(RunEvent::OperationSettled {
            operation_id: "actor".into(),
            actual_cost_microusd: cost,
            receipt_hash: hash("actor-receipt"),
        })
        .unwrap();
    } else {
        f.commit(RunEvent::OperationUnknown {
            operation_id: "actor".into(),
        })
        .unwrap();
    }
    f.commit(RunEvent::ReviewStarted {
        critic_id: "check".into(),
        operation: intent("check-1", 0, OperationClass::Critic),
    })
    .unwrap();
    let c = f.store.load(f.id).unwrap().contract;
    let ctx = ReviewContext {
        artifact_hash: artifact.hash.clone(),
        source_epoch: 0,
        artifact_texts: vec!["sealed candidate".into()],
        sources: BTreeMap::from([("owner-note".into(), b"immutable owner source".to_vec())]),
        evidence_ids: artifact
            .evidence_ids
            .iter()
            .cloned()
            .chain(["owner-note".into()])
            .collect(),
    };
    let mut verdict = verdicts(&c, &ctx, 0.0, true).remove(0);
    if !feasible {
        for score in &mut verdict.scores {
            score.outcome = Outcome::Fail;
            score.score = None;
        }
    }
    verdict.evidence_ids = vec!["artifact:answer.md".into()];
    f.commit(RunEvent::VerdictRecorded {
        critic_id: "check".into(),
        operation_id: "check-1".into(),
        verdict,
    })
    .unwrap();
    f.commit(RunEvent::OperationSettled {
        operation_id: "check-1".into(),
        actual_cost_microusd: 0,
        receipt_hash: hash("check-receipt"),
    })
    .unwrap();
    let committed = f.commit(RunEvent::AttemptDecided).unwrap();
    (f, artifact, committed)
}

fn budgeted_adapter_fixture(maximum_operations: u32) -> (Fixture, OperationIntent) {
    let f = Fixture::new(check_only());
    f.start();
    let parent = intent("actor", 50, OperationClass::Actor);
    f.store
        .begin_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            hash("frozen harness configuration"),
            maximum_operations,
            f.now,
        )
        .unwrap();
    (f, parent)
}

#[test]
fn composite_children_cannot_spend_parent_or_critic_headroom_twice() {
    let (f, parent) = budgeted_adapter_fixture(3);
    let first = intent("physical-1", 30, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, first.clone(), f.now)
        .unwrap();
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("physical-2", 21, OperationClass::Actor),
            f.now
        )
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child(f.id, "worker", &parent, first, f.now)
        .is_err());
    let budget = f.store.adapter_budget(f.id, &parent).unwrap();
    assert_eq!(budget.ledger.operations().len(), 1);
    assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 30);
    // Child accounting is inside the original 50 + 40 reservation.
    assert_eq!(
        f.store
            .load(f.id)
            .unwrap()
            .budget
            .spend()
            .unwrap()
            .reserved_microusd,
        90
    );
}

#[test]
fn composite_namespaces_keep_model_and_tool_counts_separate_under_one_budget() {
    let (f, parent) = budgeted_adapter_fixture(4);
    for (id, namespace, maximum) in [
        ("model:1", "model:", 1),
        ("tool:a:1", "tool:a:", 2),
        ("tool:a:2", "tool:a:", 2),
    ] {
        let child = intent(id, 10, OperationClass::Actor);
        f.store
            .reserve_adapter_child_bounded(
                f.id,
                "worker",
                &parent,
                child.clone(),
                Some((namespace, maximum)),
                f.now,
            )
            .unwrap();
        f.store
            .start_adapter_child(f.id, "worker", &parent, &child, f.now)
            .unwrap();
        f.store
            .settle_adapter_child(f.id, &parent, &child, Some(5), hash(id))
            .unwrap();
    }
    assert!(f
        .store
        .reserve_adapter_child_bounded(
            f.id,
            "worker",
            &parent,
            intent("model:2", 10, OperationClass::Actor),
            Some(("model:", 1)),
            f.now
        )
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child_bounded(
            f.id,
            "worker",
            &parent,
            intent("tool:a:3", 10, OperationClass::Actor),
            Some(("tool:a:", 2)),
            f.now
        )
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child_bounded(
            f.id,
            "worker",
            &parent,
            intent("foreign:1", 10, OperationClass::Actor),
            Some(("tool:a:", 2)),
            f.now
        )
        .is_err());
    let budget = f.store.adapter_budget(f.id, &parent).unwrap();
    assert_eq!(budget.ledger.operations().len(), 3);
    assert_eq!(budget.actual_cost_microusd().unwrap(), Some(15));
}

#[test]
fn composite_pre_io_release_preserves_call_count_and_never_permits_a_resend() {
    let (f, parent) = budgeted_adapter_fixture(1);
    let child = intent("physical-1", 30, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
        .unwrap();
    assert!(f
        .store
        .settle_adapter_child(f.id, &parent, &child, Some(1), hash("usage"))
        .is_err());
    f.store
        .release_adapter_child_pre_io(f.id, &parent, &child, hash("no physical start"))
        .unwrap();
    assert_eq!(
        f.store
            .adapter_budget(f.id, &parent)
            .unwrap()
            .actual_cost_microusd()
            .unwrap(),
        Some(0)
    );
    assert!(f
        .store
        .reserve_adapter_child(f.id, "worker", &parent, child, f.now)
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("physical-2", 0, OperationClass::Actor),
            f.now
        )
        .is_err());
}

#[test]
fn composite_unknown_usage_survives_restart_without_refund_or_duplicate_start() {
    let (f, parent) = budgeted_adapter_fixture(3);
    let child = intent("physical-1", 30, OperationClass::Actor);
    let pending = intent("physical-2", 10, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
        .unwrap();
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, pending.clone(), f.now)
        .unwrap();
    let deadline = f
        .store
        .start_adapter_child(f.id, "worker", &parent, &child, f.now)
        .unwrap();
    assert_eq!(deadline, f.now + Duration::seconds(LEASE_SECS));
    assert!(f
        .store
        .start_adapter_child(f.id, "worker", &parent, &child, f.now)
        .is_err());
    f.store
        .settle_adapter_child(f.id, &parent, &child, None, hash("unknown usage"))
        .unwrap();
    let restarted = RunStore::new(f.temp.path(), f.store.load(f.id).unwrap().scope);
    let budget = restarted.adapter_budget(f.id, &parent).unwrap();
    assert_eq!(budget.actual_cost_microusd().unwrap(), None);
    assert_eq!(budget.ledger.spend().unwrap().reserved_microusd, 40);
    assert!(restarted
        .release_adapter_child_pre_io(f.id, &parent, &child, hash("refund"))
        .is_err());
    assert!(restarted
        .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
        .is_err());
    assert!(restarted
        .start_adapter_child(f.id, "worker", &parent, &pending, f.now)
        .is_err());
    assert!(restarted
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("physical-3", 10, OperationClass::Actor),
            f.now
        )
        .is_err());
    // Affordable headroom cannot bypass uncertainty, but an exact late receipt
    // reconciles it without erasing either reservation or the original start.
    restarted
        .settle_adapter_child(f.id, &parent, &child, Some(7), hash("late usage"))
        .unwrap();
    restarted
        .start_adapter_child(f.id, "worker", &parent, &pending, f.now)
        .unwrap();
    restarted
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("physical-3", 21, OperationClass::Actor),
            f.now,
        )
        .unwrap();
    let reconciled = restarted.adapter_budget(f.id, &parent).unwrap();
    assert_eq!(reconciled.ledger.spend().unwrap().settled_microusd, 7);
    assert_eq!(reconciled.ledger.spend().unwrap().reserved_microusd, 31);
}

#[test]
fn composite_expired_or_cancelled_owner_cannot_start_but_late_usage_is_recorded() {
    let (f, parent) = budgeted_adapter_fixture(3);
    let started = intent("physical-1", 30, OperationClass::Actor);
    let pending = intent("physical-2", 20, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, started.clone(), f.now)
        .unwrap();
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, pending.clone(), f.now)
        .unwrap();
    f.store
        .start_adapter_child(f.id, "worker", &parent, &started, f.now)
        .unwrap();
    assert!(f
        .store
        .start_adapter_child(
            f.id,
            "worker",
            &parent,
            &pending,
            f.now + Duration::seconds(LEASE_SECS)
        )
        .is_err());
    let state = f.store.load(f.id).unwrap();
    f.store
        .commit(f.id, state.generation + 1, RunEvent::Cancelled, f.now)
        .unwrap();
    assert!(f
        .store
        .start_adapter_child(f.id, "worker", &parent, &pending, f.now)
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("physical-3", 0, OperationClass::Actor),
            f.now
        )
        .is_err());
    f.store
        .settle_adapter_child(f.id, &parent, &started, Some(7), hash("late usage"))
        .unwrap();
    f.store
        .release_adapter_child_pre_io(f.id, &parent, &pending, hash("pending never started"))
        .unwrap();
    let receipt = OperationReceipt {
        input_hash: parent.input_hash.clone(),
        actual_cost_microusd: Some(7),
        receipt_hash: hash("aggregate"),
    };
    assert!(f
        .store
        .finish_adapter_job(f.id, &parent, b"result without aggregate receipt")
        .is_err());
    let mut wrong = receipt.clone();
    wrong.actual_cost_microusd = Some(0);
    assert!(f
        .store
        .finish_budgeted_adapter_job(f.id, &parent, b"result", &wrong)
        .is_err());
    f.store
        .finish_budgeted_adapter_job(f.id, &parent, b"result", &receipt)
        .unwrap();
    assert_eq!(f.store.load(f.id).unwrap().status, RunStatus::Cancelled);
    assert!(f
        .store
        .settle_adapter_child(f.id, &parent, &started, Some(7), hash("late usage"))
        .is_err());
}

#[test]
fn composite_zero_cost_uncertainty_cannot_be_reported_as_known_zero() {
    let (f, parent) = budgeted_adapter_fixture(1);
    let child = intent("physical-free", 0, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
        .unwrap();
    f.store
        .start_adapter_child(f.id, "worker", &parent, &child, f.now)
        .unwrap();
    f.store
        .settle_adapter_child(f.id, &parent, &child, None, hash("unknown usage"))
        .unwrap();
    assert_eq!(
        f.store
            .adapter_budget(f.id, &parent)
            .unwrap()
            .actual_cost_microusd()
            .unwrap(),
        None
    );
    let receipt = OperationReceipt {
        input_hash: parent.input_hash.clone(),
        actual_cost_microusd: Some(0),
        receipt_hash: hash("aggregate"),
    };
    assert!(f
        .store
        .finish_budgeted_adapter_job(f.id, &parent, b"result", &receipt)
        .is_err());
}

#[test]
fn composite_lease_takeover_fences_old_owner_and_keeps_existing_child_reservations() {
    let (f, parent) = budgeted_adapter_fixture(2);
    let child = intent("physical-1", 30, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
        .unwrap();
    let now = f.now + Duration::seconds(LEASE_SECS + 1);
    f.store.claim(f.id, "replacement".into(), now).unwrap();
    assert!(f
        .store
        .start_adapter_child(f.id, "worker", &parent, &child, now)
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child(f.id, "replacement", &parent, child.clone(), now)
        .is_err());
    assert_eq!(
        f.store
            .adapter_budget(f.id, &parent)
            .unwrap()
            .ledger
            .spend()
            .unwrap()
            .reserved_microusd,
        30
    );
    f.store
        .release_adapter_child_pre_io(f.id, &parent, &child, hash("durable start absent"))
        .unwrap();
    f.store
        .reserve_adapter_child(
            f.id,
            "replacement",
            &parent,
            intent("physical-2", 50, OperationClass::Actor),
            now,
        )
        .unwrap();
}

#[test]
fn cancellation_fences_a_pending_accepted_handoff_after_recovery() {
    let (f, artifact, state) = accepted_check_fixture();
    f.commit(RunEvent::ReviewRouted).unwrap();
    let restarted = RunStore::new(f.temp.path(), state.scope);
    let cancelled = restarted
        .commit(f.id, 2, RunEvent::Cancelled, f.now)
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert!(cancelled.handoff.is_none());
    assert_eq!(
        cancelled.retained.champion.unwrap().artifact_hash,
        artifact.hash
    );
    assert!(restarted
        .commit(f.id, 1, RunEvent::HandoffRecorded, f.now)
        .is_err());
    assert!(restarted
        .commit(
            f.id,
            2,
            RunEvent::OwnerReviewed {
                artifact_hash: artifact.hash,
                approved: true
            },
            f.now
        )
        .is_err());
}

#[test]
fn owner_decline_is_final_and_cannot_be_replaced_by_cancellation() {
    let (f, artifact, _) = accepted_check_fixture();
    f.commit(RunEvent::ReviewRouted).unwrap();
    f.store
        .commit(
            f.id,
            1,
            RunEvent::OwnerReviewed {
                artifact_hash: artifact.hash,
                approved: false,
            },
            f.now,
        )
        .unwrap();
    assert!(f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).is_err());
    assert!(f.commit(RunEvent::HandoffRecorded).is_err());
}

#[test]
fn restart_replays_exact_artifact_review_spend_and_decision() {
    let (f, artifact, committed) = accepted_check_fixture();
    assert_eq!(committed.status, RunStatus::Accepted);
    assert!(f.commit(RunEvent::HandoffRecorded).is_err());
    let routed = f.commit(RunEvent::ReviewRouted).unwrap();
    assert_eq!(
        routed.routing.unwrap().route,
        crate::routing::ReviewRoute::HumanReview
    );
    assert!(f
        .store
        .commit(
            f.id,
            1,
            RunEvent::OwnerReviewed {
                artifact_hash: hash("edited draft"),
                approved: true
            },
            f.now
        )
        .is_err());
    f.store
        .commit(
            f.id,
            1,
            RunEvent::OwnerReviewed {
                artifact_hash: artifact.hash.clone(),
                approved: true,
            },
            f.now,
        )
        .unwrap();
    f.commit(RunEvent::HandoffRecorded).unwrap();
    assert!(f.commit(RunEvent::HandoffRecorded).is_err());
    let restarted = RunStore::new(f.temp.path(), committed.scope.clone())
        .load(f.id)
        .unwrap();
    assert_eq!(
        restarted.retained.champion.unwrap().artifact_hash,
        artifact.hash
    );
    assert_eq!(restarted.budget.spend().unwrap().total().unwrap(), 20);
    assert_eq!(restarted.attempts.len(), 1);
    assert_eq!(restarted.owner_approved, Some(true));
    assert_eq!(restarted.handoff.unwrap().artifact_hash, artifact.hash);
    assert!(restarted.attempts[0].decided);
    assert_eq!(
        restarted.decisions[0].accepted_hash,
        committed.decisions[0].accepted_hash
    );
    assert!(f
        .commit(RunEvent::AttemptStarted {
            actor: intent("after-accept", 1, OperationClass::Actor),
            review: Allowance {
                id: "after-accept-review".into(),
                input_hash: hash("after-accept-review"),
                maximum_microusd: 0,
                remaining_microusd: 0,
                closed: false
            }
        })
        .is_err());
}

#[test]
fn expired_and_cancelled_controllers_cannot_commit() {
    let f = Fixture::new(proposal());
    f.start();
    assert!(f.store.claim(f.id, "other-worker".into(), f.now).is_err());
    let later = f.now + Duration::seconds(LEASE_SECS + 1);
    let successor = f.store.claim(f.id, "successor".into(), later).unwrap();
    assert_eq!(successor.generation, 2);
    assert!(f
        .store
        .commit(f.id, 1, RunEvent::LeaseRenewed, later)
        .is_err());
    let cancelled = f.store.commit(f.id, 3, RunEvent::Cancelled, later).unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.budget.spend().unwrap().reserved_microusd, 90);
    assert!(f
        .store
        .commit(
            f.id,
            2,
            RunEvent::OperationSettled {
                operation_id: "actor".into(),
                actual_cost_microusd: 10,
                receipt_hash: hash("receipt")
            },
            later
        )
        .is_err());
    let reconciler = f.store.claim(f.id, "reconciler".into(), later).unwrap();
    let reconciled = f
        .store
        .commit(
            f.id,
            reconciler.generation,
            RunEvent::OperationSettled {
                operation_id: "actor".into(),
                actual_cost_microusd: 10,
                receipt_hash: hash("receipt"),
            },
            later,
        )
        .unwrap();
    assert_eq!(reconciled.status, RunStatus::Cancelled);
    assert_eq!(reconciled.budget.spend().unwrap().settled_microusd, 10);
}

#[test]
fn terminal_reconciliation_releases_closed_headroom_and_preserves_resumable_funding() {
    for stop in ["cancelled", "deadline", "failed"] {
        let f = Fixture::new(proposal());
        f.start();
        assert!(
            f.commit(RunEvent::ReviewHeadroomReleased {
                allowance_id: "review-headroom".into(),
            })
            .is_err(),
            "active work still needs its admitted review headroom"
        );
        f.seal();
        let critic_id = f
            .store
            .load(f.id)
            .unwrap()
            .contract
            .definition()
            .critics
            .iter()
            .find(|critic| critic.model.is_some())
            .unwrap()
            .id
            .clone();
        f.commit(RunEvent::ReviewStarted {
            critic_id,
            operation: intent("uncertain-critic", 10, OperationClass::Critic),
        })
        .unwrap();
        f.commit(RunEvent::OperationUnknown {
            operation_id: "uncertain-critic".into(),
        })
        .unwrap();
        let stopped = match stop {
            "cancelled" => f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).unwrap(),
            "deadline" => {
                let at = f.store.load(f.id).unwrap().deadline_at;
                let claimed = f.store.claim(f.id, "deadline-worker".into(), at).unwrap();
                let stopped = f
                    .store
                    .commit(f.id, claimed.generation, RunEvent::DeadlineExpired, at)
                    .unwrap();
                f.store
                    .commit(f.id, claimed.generation, RunEvent::LeaseReleased, at)
                    .unwrap();
                stopped
            },
            _ => {
                let stopped = f
                    .commit(RunEvent::Failed {
                        reason: "synthetic failure".into(),
                    })
                    .unwrap();
                f.commit(RunEvent::LeaseReleased).unwrap();
                stopped
            },
        };
        let reconciled = f
            .store
            .reconcile_terminal_receipts(f.id, "billing-worker".into())
            .unwrap();
        assert_eq!(reconciled.status, stopped.status);
        assert_eq!(reconciled.attempts.len(), stopped.attempts.len());
        assert_eq!(reconciled.budget.operations(), stopped.budget.operations());
        assert_eq!(
            reconciled.budget.spend().unwrap().reserved_microusd,
            if stop == "deadline" { 90 } else { 60 }
        );
        assert_eq!(
            reconciled.budget.allowances()["review-headroom"].closed,
            stop != "deadline"
        );
        let before = serde_json::to_value(&reconciled).unwrap();
        let restarted = RunStore::new(f.temp.path(), reconciled.scope.clone());
        assert_eq!(
            serde_json::to_value(restarted.load(f.id).unwrap()).unwrap(),
            before
        );
        // Unknown operations may be rechecked, but never released or dispatched.
        let repeated = restarted
            .reconcile_terminal_receipts(f.id, "billing-worker".into())
            .unwrap();
        assert_eq!(repeated.budget.operations(), reconciled.budget.operations());
        assert_eq!(
            repeated.budget.spend().unwrap(),
            reconciled.budget.spend().unwrap()
        );
    }
}

#[test]
fn terminal_reconciliation_closes_headroom_after_all_operations_have_settled() {
    let f = Fixture::new(proposal());
    f.start();
    f.commit(RunEvent::OperationSettled {
        operation_id: "actor".into(),
        actual_cost_microusd: 7,
        receipt_hash: hash("actor receipt"),
    })
    .unwrap();
    f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).unwrap();
    let reconciled = f
        .store
        .reconcile_terminal_receipts(f.id, "billing-worker".into())
        .unwrap();
    assert_eq!(
        reconciled.budget.spend().unwrap(),
        Spend {
            settled_microusd: 7,
            reserved_microusd: 0
        }
    );
    let before = serde_json::to_value(&reconciled).unwrap();
    assert_eq!(
        serde_json::to_value(
            f.store
                .reconcile_terminal_receipts(f.id, "billing-worker".into())
                .unwrap()
        )
        .unwrap(),
        before
    );
}

#[test]
fn concurrent_children_share_one_atomic_cost_ceiling() {
    let mut p = proposal();
    p.budget.max_cost_microusd = 100;
    let f = Fixture::new(p);
    f.start();
    let mut workers = Vec::new();
    for n in 0..10 {
        let store = f.store.clone();
        let id = f.id;
        let now = f.now;
        workers.push(std::thread::spawn(move || {
            store
                .commit(
                    id,
                    1,
                    RunEvent::OperationReserved {
                        operation: intent(&format!("child-{n}"), 5, OperationClass::Read),
                    },
                    now,
                )
                .is_ok()
        }));
    }
    let admitted = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .filter(|ok| *ok)
        .count();
    assert_eq!(admitted, 2);
    assert_eq!(
        f.store
            .load(f.id)
            .unwrap()
            .budget
            .spend()
            .unwrap()
            .total()
            .unwrap(),
        100
    );
}

#[test]
fn damaged_catalog_record_does_not_hide_verified_runs_or_authorize_dispatch() {
    let f = Fixture::new(proposal());
    let broken = Uuid::new_v4();
    let journal = f
        .temp
        .path()
        .join("relentless/runs")
        .join(broken.to_string())
        .join("journal");
    std::fs::create_dir_all(&journal).unwrap();
    std::fs::write(journal.join("00000000000000000000.json"), b"{broken").unwrap();
    let rows = f.store.list_projected_results(|run| Ok(run.id)).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].1.as_ref().unwrap(), &f.id);
    assert_eq!(rows[1].0, broken);
    assert!(rows[1].1.is_err());
    assert!(
        f.store.list().is_err(),
        "authoritative scans must remain strict"
    );
    assert!(f.store.load(broken).is_err());
    assert!(f.store.claim(broken, "worker".into(), f.now).is_err());
}

#[test]
fn interrupted_temp_is_ignored_but_committed_corruption_and_gaps_refuse_recovery() {
    let f = Fixture::new(proposal());
    let journal = f
        .temp
        .path()
        .join("relentless/runs")
        .join(f.id.to_string())
        .join("journal");
    std::fs::write(journal.join(".interrupted.tmp"), b"{truncated").unwrap();
    assert_eq!(f.store.load(f.id).unwrap().generation, 1);
    std::fs::write(journal.join("00000000000000000002.json"), b"{corrupt").unwrap();
    assert!(f.store.load(f.id).is_err());
    let g = Fixture::new(proposal());
    let journal = g
        .temp
        .path()
        .join("relentless/runs")
        .join(g.id.to_string())
        .join("journal");
    std::fs::write(journal.join("00000000000000000003.json"), b"{}").unwrap();
    assert!(g.store.load(g.id).is_err());
}

#[test]
fn write_failure_does_not_commit_dispatch_intent() {
    let f = Fixture::new(proposal());
    let next = f
        .temp
        .path()
        .join("relentless/runs")
        .join(f.id.to_string())
        .join("journal/00000000000000000002.json");
    std::fs::create_dir(&next).unwrap();
    assert!(f
        .commit(RunEvent::OperationReserved {
            operation: intent("must-not-dispatch", 1, OperationClass::Actor)
        })
        .is_err());
    // A conflicting journal pathname itself is a storage failure. After the
    // injected conflict is removed, no intent or cost reservation exists.
    std::fs::remove_dir(next).unwrap();
    assert!(f.store.load(f.id).unwrap().budget.operations().is_empty());
}

#[test]
fn source_and_artifact_bytes_are_pinned_and_detect_corruption() {
    let f = Fixture::new(proposal());
    f.start();
    let artifact = f.seal();
    let state = f.store.load(f.id).unwrap();
    let source = &state.contract.definition().sources[0];
    assert_eq!(
        f.store.read_blob(f.id, &source.content_hash).unwrap(),
        b"immutable owner source"
    );
    let file_hash = artifact.files.get("answer.md").unwrap();
    let object = f
        .temp
        .path()
        .join("relentless/runs")
        .join(f.id.to_string())
        .join("objects")
        .join(file_hash);
    std::fs::write(object, b"actor changed sealed output").unwrap();
    assert!(f.store.load(f.id).is_err());
    assert!(f
        .store
        .prepare_artifact(f.id, &BTreeMap::from([("../escape".into(), vec![1])]))
        .is_err());
}

#[test]
fn deadlines_include_downtime_and_waiting_owner() {
    let mut p = proposal();
    p.budget.max_wall_clock_secs = 10;
    let f = Fixture::new(p);
    f.commit(RunEvent::Blocked {
        reason: "missing source".into(),
        attention_id: "attention-1".into(),
    })
    .unwrap();
    let after_deadline = f.now + Duration::seconds(11);
    assert!(f
        .store
        .commit(
            f.id,
            1,
            RunEvent::OwnerResumed {
                attention_id: "attention-1".into()
            },
            after_deadline
        )
        .is_err());
    let stopped = f
        .store
        .commit(f.id, 1, RunEvent::DeadlineExpired, after_deadline)
        .unwrap();
    assert_eq!(stopped.status, RunStatus::BudgetExhausted);
    assert!(stopped.retained.artifact().is_none());
}

fn approve_reset(
    f: &Fixture,
    reset: crate::budget_round::BudgetReset,
    now: chrono::DateTime<Utc>,
) -> RunState {
    let state = f.store.load(f.id).unwrap();
    f.store
        .commit(
            f.id,
            state.generation + 1,
            RunEvent::BudgetResetApproved {
                id: Uuid::new_v4(),
                approved_by: "owner".into(),
                contract_hash: state.contract.hash().unwrap(),
                reset,
            },
            now,
        )
        .unwrap()
}

#[test]
fn budget_reset_time_preserves_paid_checkpoint_funding_and_other_allowances() {
    use crate::budget_round::BudgetReset;
    let (f, parent, checkpoint) = parked_adapter_fixture();
    let before = f.store.load(f.id).unwrap();
    let expired = before.deadline_at + Duration::seconds(1);
    let claimed = f
        .store
        .claim(f.id, "deadline-worker".into(), expired)
        .unwrap();
    let stopped = f
        .store
        .commit(f.id, claimed.generation, RunEvent::DeadlineExpired, expired)
        .unwrap();
    assert_eq!(
        stopped.exhausted_limits(expired).unwrap(),
        crate::budget_round::ExhaustedLimits {
            time: true,
            ..Default::default()
        }
    );
    f.store
        .commit(f.id, claimed.generation, RunEvent::LeaseReleased, expired)
        .unwrap();
    let reconciled = f
        .store
        .reconcile_terminal_receipts(f.id, "billing-worker".into())
        .unwrap();
    assert_eq!(
        reconciled.budget.spend().unwrap(),
        before.budget.spend().unwrap()
    );
    assert_eq!(
        reconciled.budget.operations()[&parent.id].settlement,
        Settlement::Reserved
    );
    assert!(!reconciled.budget.allowances()["review-headroom"].closed);
    let resumed = approve_reset(
        &f,
        BudgetReset {
            wall_clock_secs: Some(86400),
            ..Default::default()
        },
        expired,
    );
    assert_eq!(resumed.status, RunStatus::WaitingOwner);
    assert_eq!(resumed.deadline_at, expired + Duration::seconds(86400));
    assert_eq!(resumed.contract, before.contract);
    assert_eq!(
        resumed.effective_budget().unwrap().max_cost_microusd,
        before.effective_budget().unwrap().max_cost_microusd
    );
    assert_eq!(
        resumed.budget.spend().unwrap(),
        before.budget.spend().unwrap()
    );
    assert_eq!(resumed.attempts[0].checkpoint.as_ref(), Some(&checkpoint));
    assert_eq!(resumed.attempts[0].actor_operation_id, parent.id);
    assert_eq!(resumed.attempts.len(), 1);
    assert!(!resumed.budget.allowances()["review-headroom"].closed);
    let restarted = RunStore::new(f.temp.path(), resumed.scope.clone())
        .load(f.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(&restarted).unwrap(),
        serde_json::to_value(&resumed).unwrap()
    );
    let event = RunEvent::BudgetResetApproved {
        id: resumed.budget_resets[0].id,
        approved_by: "owner".into(),
        contract_hash: resumed.contract.hash().unwrap(),
        reset: resumed.budget_resets[0].reset.clone(),
    };
    let replay = f
        .store
        .commit(f.id, 1, event, expired + Duration::hours(2))
        .unwrap();
    assert_eq!(replay.deadline_at, resumed.deadline_at);
    assert_eq!(replay.budget_resets.len(), 1);
    assert_eq!(
        resumed.budget_usage(expired + Duration::hours(1)).unwrap()["cumulative"]
            ["time_allowance_used_secs"],
        before.contract.definition().budget.max_wall_clock_secs + 3600
    );
}

#[test]
fn completed_work_clock_usage_does_not_grow_with_later_report_reads() {
    let (_, _, accepted) = accepted_check_fixture();
    let original = accepted.budget_usage(accepted.started_at).unwrap();
    let later = accepted
        .budget_usage(accepted.started_at + Duration::days(30))
        .unwrap();
    assert_eq!(original, later);
    assert_eq!(accepted.finished_at, Some(accepted.started_at));
}

#[test]
fn budget_reset_attempts_continues_same_run_with_cumulative_numbering() {
    use crate::budget_round::BudgetReset;
    let mut p = check_only();
    p.budget.max_attempts = 1;
    let (f, artifact, stopped) = checked_fixture(p, false, Some(20));
    assert_eq!(stopped.status, RunStatus::BudgetExhausted);
    f.store
        .commit(f.id, 1, RunEvent::LeaseReleased, f.now)
        .unwrap();
    let resumed = approve_reset(
        &f,
        BudgetReset {
            attempts: Some(25),
            ..Default::default()
        },
        f.now,
    );
    assert_eq!(resumed.status, RunStatus::Admitted);
    assert_eq!(resumed.effective_budget().unwrap().max_attempts, 26);
    assert_eq!(resumed.deadline_at, stopped.deadline_at);
    assert_eq!(
        resumed.budget.spend().unwrap(),
        stopped.budget.spend().unwrap()
    );
    assert_eq!(resumed.retained, stopped.retained);
    assert_eq!(resumed.attempts[0].artifact.as_ref(), Some(&artifact));
    let claimed = f.store.claim(f.id, "next-worker".into(), f.now).unwrap();
    let next = f
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::AttemptStarted {
                actor: intent("actor-round-2", 50, OperationClass::Actor),
                review: Allowance {
                    id: "review-round-2".into(),
                    input_hash: hash("review-round-2"),
                    maximum_microusd: 40,
                    remaining_microusd: 40,
                    closed: false,
                },
            },
            f.now,
        )
        .unwrap();
    assert_eq!(next.attempts[1].number, 2);
    assert_eq!(next.budget.spend().unwrap().settled_microusd, 20);
}

#[test]
fn budget_reset_late_receipts_do_not_refill_new_cost_window() {
    use crate::budget_round::BudgetReset;
    let mut p = check_only();
    p.budget.max_cost_microusd = 90;
    let (f, _, state) = checked_fixture(p, false, None);
    assert_eq!(state.status, RunStatus::Admitted);
    f.commit(RunEvent::BudgetUnavailable {
        required_microusd: 50,
    })
    .unwrap();
    f.commit(RunEvent::LeaseReleased).unwrap();
    let grant = approve_reset(
        &f,
        BudgetReset {
            cost_microusd: Some(100),
            ..Default::default()
        },
        f.now,
    );
    assert_eq!(grant.effective_budget().unwrap().max_cost_microusd, 150);
    let claimed = f.store.claim(f.id, "next-worker".into(), f.now).unwrap();
    let settled = f
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::OperationSettled {
                operation_id: "actor".into(),
                actual_cost_microusd: 20,
                receipt_hash: hash("late-proof"),
            },
            f.now,
        )
        .unwrap();
    assert_eq!(settled.effective_budget().unwrap().max_cost_microusd, 120);
    assert_eq!(
        settled.budget_usage(f.now).unwrap()["cost"]["remaining_microusd"],
        100
    );
    let next = f
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::AttemptStarted {
                actor: intent("new-actor", 50, OperationClass::Actor),
                review: Allowance {
                    id: "new-review".into(),
                    input_hash: hash("new-review"),
                    maximum_microusd: 40,
                    remaining_microusd: 40,
                    closed: false,
                },
            },
            f.now,
        )
        .unwrap();
    assert_eq!(
        next.budget_usage(f.now).unwrap()["cost"]["remaining_microusd"],
        10
    );
    assert_eq!(next.budget.spend().unwrap().settled_microusd, 20);
}

#[test]
fn budget_reset_rejects_unreached_wrong_owner_stale_and_insufficient_approval() {
    use crate::budget_round::BudgetReset;
    let f = Fixture::new(proposal());
    f.commit(RunEvent::BudgetUnavailable {
        required_microusd: 5_000_001,
    })
    .unwrap();
    let stopped = f.commit(RunEvent::LeaseReleased).unwrap();
    for (owner, generation, hash, reset) in [
        (
            "intruder",
            2,
            stopped.contract.hash().unwrap(),
            BudgetReset {
                cost_microusd: Some(100_000_000),
                ..Default::default()
            },
        ),
        (
            "owner",
            3,
            stopped.contract.hash().unwrap(),
            BudgetReset {
                cost_microusd: Some(100_000_000),
                ..Default::default()
            },
        ),
        (
            "owner",
            2,
            hash("wrong-contract"),
            BudgetReset {
                cost_microusd: Some(100_000_000),
                ..Default::default()
            },
        ),
        (
            "owner",
            2,
            stopped.contract.hash().unwrap(),
            BudgetReset {
                wall_clock_secs: Some(86400),
                ..Default::default()
            },
        ),
        (
            "owner",
            2,
            stopped.contract.hash().unwrap(),
            BudgetReset {
                cost_microusd: Some(100),
                ..Default::default()
            },
        ),
    ] {
        assert!(f
            .store
            .commit(
                f.id,
                generation,
                RunEvent::BudgetResetApproved {
                    id: Uuid::new_v4(),
                    approved_by: owner.into(),
                    contract_hash: hash,
                    reset
                },
                f.now
            )
            .is_err());
    }
    assert!(f.store.load(f.id).unwrap().budget_resets.is_empty());
}

#[test]
fn unlimited_review_preserves_reservations_restart_and_owner_cancellation() {
    let mut p = proposal();
    p.budget = Budget::default();
    let f = Fixture::new(p);
    f.start();
    let state = f.store.load(f.id).unwrap();
    assert_eq!(state.budget.spend().unwrap().reserved_microusd, 90);
    assert_eq!(state.exhausted_limits(f.now).unwrap(), Default::default());
    assert!(state.budget_usage(f.now).unwrap()["attempts"]["remaining"].is_null());
    assert!(state.budget_usage(f.now).unwrap()["time"]["remaining_secs"].is_null());
    let restored = RunStore::new(f.temp.path(), state.scope);
    assert_eq!(
        restored.load(f.id).unwrap().deadline_at,
        chrono::DateTime::<Utc>::MAX_UTC
    );
    f.commit(RunEvent::LeaseReleased).unwrap();
    let cancelled = restored
        .commit(f.id, 2, RunEvent::Cancelled, f.now)
        .unwrap();
    assert_eq!(cancelled.status, RunStatus::Cancelled);
    assert_eq!(cancelled.budget.spend().unwrap().reserved_microusd, 90);
}

#[test]
fn owner_can_remove_an_exhausted_cost_cap_without_resetting_usage() {
    use crate::budget_round::BudgetReset;
    let f = Fixture::new(proposal());
    f.commit(RunEvent::BudgetUnavailable {
        required_microusd: 5_000_001,
    })
    .unwrap();
    let stopped = f.commit(RunEvent::LeaseReleased).unwrap();
    let resumed = f
        .store
        .commit(
            f.id,
            2,
            RunEvent::BudgetResetApproved {
                id: Uuid::new_v4(),
                approved_by: "owner".into(),
                contract_hash: stopped.contract.hash().unwrap(),
                reset: BudgetReset {
                    cost_microusd: Some(0),
                    ..Default::default()
                },
            },
            f.now,
        )
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Admitted);
    assert_eq!(resumed.effective_budget().unwrap().max_cost_microusd, 0);
    assert_eq!(
        resumed.effective_budget().unwrap().max_attempts,
        stopped.effective_budget().unwrap().max_attempts
    );
    assert_eq!(
        resumed.budget.spend().unwrap(),
        stopped.budget.spend().unwrap()
    );
    assert!(!resumed.exhausted_limits(f.now).unwrap().cost);
}

#[test]
fn owner_answer_resumes_same_unfinished_attempt() {
    let f = Fixture::new(proposal());
    f.start();
    f.commit(RunEvent::Blocked {
        reason: "missing input".into(),
        attention_id: "attention-1".into(),
    })
    .unwrap();
    let resumed = f
        .commit(RunEvent::OwnerResumed {
            attention_id: "attention-1".into(),
        })
        .unwrap();
    assert_eq!(resumed.status, RunStatus::Attempting);
    assert_eq!(resumed.attempts.len(), 1);
    assert!(f
        .commit(RunEvent::AttemptStarted {
            actor: intent("reset-attempt", 1, OperationClass::Actor),
            review: Allowance {
                id: "reset-review".into(),
                input_hash: hash("reset-review"),
                maximum_microusd: 0,
                remaining_microusd: 0,
                closed: false
            }
        })
        .is_err());
}

fn wait_for_owner_input(f: &Fixture) {
    f.commit(RunEvent::Blocked {
        reason: "a material source fact is missing".into(),
        attention_id: "attention-1".into(),
    })
    .unwrap();
}

fn owner_input(f: &Fixture, bytes: &[u8]) -> RunEvent {
    RunEvent::OwnerInputRecorded {
        attention_id: "attention-1".into(),
        response_hash: f.store.put_blob(f.id, bytes).unwrap(),
    }
}

#[test]
fn owner_input_pins_a_new_epoch_and_replays_once_without_resetting_the_attempt() {
    let f = Fixture::new(proposal());
    f.start();
    wait_for_owner_input(&f);
    let before = f.store.load(f.id).unwrap();
    let event = owner_input(&f, b"Question: missing fact? Answer: owner-supplied fact.");
    let resumed = f.commit(event.clone()).unwrap();
    assert_eq!(resumed.source_epoch, 1);
    assert_eq!(resumed.contract, before.contract);
    assert_eq!(resumed.deadline_at, before.deadline_at);
    assert_eq!(resumed.generation, before.generation);
    assert_eq!(resumed.attempts.len(), 1);
    assert_eq!(resumed.attempts[0].actor_operation_id, "actor");
    assert_eq!(resumed.status, RunStatus::Attempting);
    assert!(resumed.attention_id.is_none() && resumed.reason.is_none());
    assert_eq!(
        serde_json::to_value(&resumed.budget).unwrap(),
        serde_json::to_value(&before.budget).unwrap()
    );
    let evaluation = resumed.evaluation_contract().unwrap();
    assert_ne!(evaluation.hash().unwrap(), before.contract.hash().unwrap());
    let mut enriched = evaluation.definition().clone();
    let answer_pin = enriched.sources.pop().unwrap();
    assert_eq!(&enriched, before.contract.definition());
    assert_eq!(answer_pin.content_hash, resumed.owner_inputs["attention-1"]);
    assert_eq!(
        f.store.read_blob(f.id, &answer_pin.content_hash).unwrap(),
        b"Question: missing fact? Answer: owner-supplied fact."
    );

    let journal = f
        .temp
        .path()
        .join("relentless/runs")
        .join(f.id.to_string())
        .join("journal");
    let count = std::fs::read_dir(&journal).unwrap().count();
    let recovered = RunStore::new(f.temp.path(), resumed.scope.clone());
    assert_eq!(
        serde_json::to_value(recovered.load(f.id).unwrap()).unwrap(),
        serde_json::to_value(&resumed).unwrap()
    );
    let duplicate = recovered.commit(f.id, 1, event, f.now).unwrap();
    assert_eq!(std::fs::read_dir(journal).unwrap().count(), count);
    assert_eq!(duplicate.source_epoch, 1);
    assert!(f
        .commit(owner_input(&f, b"a conflicting response"))
        .is_err());
    assert_eq!(
        f.store.load(f.id).unwrap().owner_inputs,
        resumed.owner_inputs
    );
}

#[test]
fn owner_input_refuses_wrong_requests_unbound_bytes_and_unfinished_operation_mismatch() {
    for case in [
        "wrong-request",
        "missing-blob",
        "foreign-blob",
        "non-utf8",
        "unknown",
        "settled",
        "sealed",
    ] {
        let f = Fixture::new(proposal());
        f.start();
        match case {
            "unknown" => {
                f.commit(RunEvent::OperationUnknown {
                    operation_id: "actor".into(),
                })
                .unwrap();
            },
            "settled" => {
                f.commit(RunEvent::OperationSettled {
                    operation_id: "actor".into(),
                    actual_cost_microusd: 1,
                    receipt_hash: hash("settled receipt"),
                })
                .unwrap();
            },
            "sealed" => {
                f.seal();
            },
            _ => {},
        }
        wait_for_owner_input(&f);
        let mut event = owner_input(&f, b"owner fact");
        if let RunEvent::OwnerInputRecorded {
            attention_id,
            response_hash,
        } = &mut event
        {
            match case {
                "wrong-request" => *attention_id = "another-request".into(),
                "missing-blob" => *response_hash = hash("not stored"),
                "foreign-blob" => {
                    *response_hash = f
                        .store
                        .put_blob(Uuid::new_v4(), b"foreign run fact")
                        .unwrap()
                },
                "non-utf8" => *response_hash = f.store.put_blob(f.id, &[0xff]).unwrap(),
                _ => {},
            }
        }
        let before = serde_json::to_value(f.store.load(f.id).unwrap()).unwrap();
        assert!(f.commit(event).is_err(), "case {case}");
        assert_eq!(
            serde_json::to_value(f.store.load(f.id).unwrap()).unwrap(),
            before,
            "case {case}"
        );
    }

    for case in ["cancelled", "deadline", "wrong-generation"] {
        let f = Fixture::new(proposal());
        f.start();
        wait_for_owner_input(&f);
        let event = owner_input(&f, b"owner fact");
        if case == "cancelled" {
            f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).unwrap();
        }
        let before = f.store.load(f.id).unwrap();
        let now = if case == "deadline" {
            before.deadline_at
        } else {
            f.now
        };
        let generation = if case == "wrong-generation" {
            2
        } else {
            before.generation
        };
        assert!(
            f.store.commit(f.id, generation, event, now).is_err(),
            "case {case}"
        );
        assert_eq!(
            serde_json::to_value(f.store.load(f.id).unwrap()).unwrap(),
            serde_json::to_value(before).unwrap()
        );
    }
}

#[test]
fn owner_input_is_grounding_evidence_and_old_epoch_reviews_cannot_pass() {
    use crate::routing::{HandoffKind, ReviewRecommendation, ReviewRoute, RoutingPolicy};
    let definition = proposal();
    let policy = RoutingPolicy {
        version_hash: hash("context capture policy"),
        reviewer: definition.critics[0].model.clone().unwrap(),
        min_confidence: 0.8,
        owner_review_required: false,
        allow_automatic_internal: false,
        allow_automatic_capture: true,
    };
    let f = Fixture::with_policy(definition, Some(policy), HandoffKind::Capture);
    f.start();
    wait_for_owner_input(&f);
    let enriched = f
        .commit(owner_input(
            &f,
            b"Question: missing fact? Answer: owner-supplied fact.",
        ))
        .unwrap();
    let artifact = f.seal();
    f.commit(RunEvent::OperationSettled {
        operation_id: "actor".into(),
        actual_cost_microusd: 20,
        receipt_hash: hash("actor receipt"),
    })
    .unwrap();
    let evaluation = enriched.evaluation_contract().unwrap();
    let answer_id = evaluation.definition().sources.last().unwrap().id.clone();
    let ctx = ReviewContext {
        artifact_hash: artifact.hash.clone(),
        source_epoch: 1,
        artifact_texts: vec!["sealed candidate".into()],
        sources: evaluation
            .definition()
            .sources
            .iter()
            .map(|pin| {
                (
                    pin.id.clone(),
                    f.store.read_blob(f.id, &pin.content_hash).unwrap(),
                )
            })
            .collect(),
        evidence_ids: artifact
            .evidence_ids
            .iter()
            .cloned()
            .chain(
                evaluation
                    .definition()
                    .sources
                    .iter()
                    .map(|pin| pin.id.clone()),
            )
            .collect(),
    };
    let mut panel = verdicts(&evaluation, &ctx, 0.9, true);
    panel[0].review_recommendation = Some(ReviewRecommendation {
        route: ReviewRoute::AutomaticCapture,
        confidence: 0.9,
    });
    for verdict in &mut panel {
        verdict.evidence_ids = ctx.evidence_ids.iter().cloned().collect();
        for coverage in verdict
            .scores
            .iter_mut()
            .filter_map(|score| score.coverage.as_mut())
        {
            for claim in &mut coverage.claims {
                claim.sources = vec![ClaimSource {
                    source_id: answer_id.clone(),
                    quote: "owner-supplied fact".into(),
                }];
            }
        }
    }
    let current = aggregate(&evaluation, &ctx, &panel).unwrap();
    assert!(current.complete && current.feasible);
    let mut stale = panel.clone();
    for verdict in &mut stale {
        verdict.contract_hash = enriched.contract.hash().unwrap();
        verdict.source_epoch = 0;
    }
    let rejected = aggregate(&evaluation, &ctx, &stale).unwrap();
    assert!(!rejected.complete && !rejected.feasible);
    assert!(validate_verdict(
        &evaluation.definition().critics[0],
        &panel[0],
        &enriched.contract,
        &enriched.contract.hash().unwrap(),
        &ctx
    )
    .is_err());
    let mut forged = ctx;
    forged
        .sources
        .insert(answer_id, b"different supplied fact".to_vec());
    assert!(validate_verdict(
        &evaluation.definition().critics[0],
        &panel[0],
        &evaluation,
        &evaluation.hash().unwrap(),
        &forged
    )
    .is_err());
    for (i, verdict) in panel.into_iter().enumerate() {
        let id = format!("input-review-{i}");
        f.commit(RunEvent::ReviewStarted {
            critic_id: verdict.critic_id.clone(),
            operation: intent(&id, 1, OperationClass::Critic),
        })
        .unwrap();
        f.commit(RunEvent::CriticCompleted {
            critic_id: verdict.critic_id.clone(),
            operation_id: id.clone(),
            verdict,
            receipt: OperationReceipt {
                input_hash: hash(&id),
                actual_cost_microusd: Some(1),
                receipt_hash: hash(&format!("input-receipt-{i}")),
            },
        })
        .unwrap();
    }
    let accepted = f.commit(RunEvent::AttemptDecided).unwrap();
    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.contract, enriched.contract);
    assert_eq!(accepted.retained.champion.as_ref().unwrap().source_epoch, 1);
    assert_eq!(
        accepted.retained.champion.unwrap().contract_hash,
        evaluation.hash().unwrap()
    );
    let routed = f.commit(RunEvent::ReviewRouted).unwrap();
    assert_eq!(routed.routing.unwrap().route, ReviewRoute::AutomaticCapture);
    f.commit(RunEvent::HandoffRecorded).unwrap();
}

#[test]
fn owner_input_keeps_previous_draft_bytes_as_reference_without_reusing_their_review() {
    let f = Fixture::new(check_only());
    f.start();
    let artifact = f.seal();
    f.commit(RunEvent::OperationSettled {
        operation_id: "actor".into(),
        actual_cost_microusd: 20,
        receipt_hash: hash("actor receipt"),
    })
    .unwrap();
    let before = f.store.load(f.id).unwrap();
    let mut ctx = context("previous draft");
    ctx.artifact_hash = artifact.hash.clone();
    ctx.evidence_ids = artifact.evidence_ids.clone();
    let mut failed = verdicts(&before.contract, &ctx, 0.0, false).remove(0);
    failed.evidence_ids = artifact.evidence_ids.iter().cloned().collect();
    failed.scores[0].outcome = Outcome::Fail;
    f.commit(RunEvent::ReviewStarted {
        critic_id: "check".into(),
        operation: intent("failed-check", 0, OperationClass::Critic),
    })
    .unwrap();
    f.commit(RunEvent::CriticCompleted {
        critic_id: "check".into(),
        operation_id: "failed-check".into(),
        verdict: failed,
        receipt: OperationReceipt {
            input_hash: hash("failed-check"),
            actual_cost_microusd: Some(0),
            receipt_hash: hash("failed-check receipt"),
        },
    })
    .unwrap();
    let decided = f.commit(RunEvent::AttemptDecided).unwrap();
    assert_eq!(decided.status, RunStatus::Admitted);
    assert_eq!(
        decided.retained.best_draft.as_ref().unwrap().artifact_hash,
        artifact.hash
    );
    f.commit(RunEvent::AttemptStarted {
        actor: intent("actor-2", 50, OperationClass::Actor),
        review: Allowance {
            id: "review-2".into(),
            input_hash: hash("review-2"),
            maximum_microusd: 40,
            remaining_microusd: 40,
            closed: false,
        },
    })
    .unwrap();
    wait_for_owner_input(&f);
    let resumed = f.commit(owner_input(&f, b"owner-supplied fact")).unwrap();
    assert!(resumed.retained.artifact().is_none());
    assert_eq!(resumed.retained.plateau_count, 0);
    assert_eq!(
        resumed.reference_for_actor().unwrap().artifact_hash,
        artifact.hash
    );
    assert_eq!(resumed.reference_for_actor().unwrap().source_epoch, 0);
    assert_eq!(resumed.attempts.len(), 2);
    assert_eq!(resumed.budget.spend().unwrap().settled_microusd, 20);
    assert_eq!(
        f.store
            .read_blob(f.id, &artifact.files["answer.md"])
            .unwrap(),
        b"sealed candidate"
    );
    let recovered = RunStore::new(f.temp.path(), resumed.scope.clone())
        .load(f.id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(recovered).unwrap(),
        serde_json::to_value(resumed).unwrap()
    );
}

fn parked_adapter_fixture() -> (Fixture, OperationIntent, ActorCheckpoint) {
    let (f, parent) = budgeted_adapter_fixture(2);
    let checkpoint = f
        .store
        .checkpoint_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            "attention-1".into(),
            b"opaque harness checkpoint",
            f.now,
        )
        .unwrap();
    f.commit(RunEvent::ActorCheckpointed {
        checkpoint: checkpoint.clone(),
        reason: "missing material fact".into(),
    })
    .unwrap();
    (f, parent, checkpoint)
}

#[test]
fn adapter_checkpoint_resumes_with_the_same_reservation_and_only_pre_io_replay() {
    let (f, parent) = budgeted_adapter_fixture(2);
    let first = intent("physical-1", 30, OperationClass::Actor);
    f.store
        .reserve_adapter_child(f.id, "worker", &parent, first.clone(), f.now)
        .unwrap();
    f.store
        .start_adapter_child(f.id, "worker", &parent, &first, f.now)
        .unwrap();
    f.store
        .settle_adapter_child(f.id, &parent, &first, Some(7), hash("first receipt"))
        .unwrap();
    let budget_before =
        serde_json::to_value(f.store.adapter_budget(f.id, &parent).unwrap()).unwrap();
    let checkpoint = f
        .store
        .checkpoint_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            "attention-1".into(),
            b"exact native pause bytes",
            f.now,
        )
        .unwrap();
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("before-answer", 1, OperationClass::Actor),
            f.now
        )
        .is_err());
    f.commit(RunEvent::ActorCheckpointed {
        checkpoint: checkpoint.clone(),
        reason: "missing material fact".into(),
    })
    .unwrap();
    assert_eq!(
        f.store.adapter_checkpoint(f.id, &parent).unwrap(),
        Some(checkpoint.clone())
    );
    assert!(matches!(
        f.store.lookup_adapter_job(f.id, &parent).unwrap(),
        AdapterJobLookup::InFlight
    ));
    assert!(f
        .store
        .resume_budgeted_adapter_job(f.id, "worker", &parent, &checkpoint, f.now)
        .is_err());
    assert!(f
        .store
        .finish_budgeted_adapter_job(
            f.id,
            &parent,
            b"premature result",
            &OperationReceipt {
                input_hash: parent.input_hash.clone(),
                actual_cost_microusd: Some(7),
                receipt_hash: hash("premature receipt")
            }
        )
        .is_err());
    f.commit(RunEvent::LeaseReleased).unwrap();
    let answered = f
        .commit(owner_input(&f, b"the material source fact"))
        .unwrap();
    assert_eq!(answered.attempts.len(), 1);
    assert_eq!(
        answered.budget.operations()[&parent.id].settlement,
        Settlement::Reserved
    );
    f.store.claim(f.id, "resumed-worker".into(), f.now).unwrap();
    let budget = f
        .store
        .resume_budgeted_adapter_job(f.id, "resumed-worker", &parent, &checkpoint, f.now)
        .unwrap();
    assert_eq!(serde_json::to_value(budget).unwrap(), budget_before);
    assert!(f
        .store
        .resume_budgeted_adapter_job(f.id, "resumed-worker", &parent, &checkpoint, f.now)
        .is_err());
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("stale-worker", 1, OperationClass::Actor),
            f.now
        )
        .is_err());

    // The claimed worker disappeared before any physical call. A new lease
    // can prove that from the unchanged checkpoint ledger and resume once.
    f.store
        .commit(f.id, 2, RunEvent::LeaseReleased, f.now)
        .unwrap();
    let restarted = RunStore::new(f.temp.path(), answered.scope);
    restarted
        .claim(f.id, "recovered-worker".into(), f.now)
        .unwrap();
    let budget = restarted
        .resume_budgeted_adapter_job(f.id, "recovered-worker", &parent, &checkpoint, f.now)
        .unwrap();
    assert_eq!(budget.maximum_operations, 2);
    assert_eq!(budget.ledger.spend().unwrap().settled_microusd, 7);
    assert_eq!(budget.physical_starts.len(), 1);
    let second = intent("physical-2", 20, OperationClass::Actor);
    restarted
        .reserve_adapter_child(f.id, "recovered-worker", &parent, second.clone(), f.now)
        .unwrap();
    restarted
        .start_adapter_child(f.id, "recovered-worker", &parent, &second, f.now)
        .unwrap();
    restarted
        .settle_adapter_child(f.id, &parent, &second, Some(8), hash("second receipt"))
        .unwrap();
    assert!(restarted
        .reserve_adapter_child(
            f.id,
            "recovered-worker",
            &parent,
            intent("physical-3", 0, OperationClass::Actor),
            f.now
        )
        .is_err());
    restarted
        .commit(f.id, 3, RunEvent::LeaseReleased, f.now)
        .unwrap();
    restarted.claim(f.id, "later-worker".into(), f.now).unwrap();
    assert!(
        restarted
            .resume_budgeted_adapter_job(f.id, "later-worker", &parent, &checkpoint, f.now)
            .is_err(),
        "a later physical call invalidates pre-I/O replay proof"
    );
    let state = restarted.load(f.id).unwrap();
    assert_eq!(state.attempts.len(), 1);
    assert_eq!(state.budget.spend().unwrap().reserved_microusd, 90);
    assert_eq!(
        restarted
            .adapter_budget(f.id, &parent)
            .unwrap()
            .ledger
            .spend()
            .unwrap()
            .settled_microusd,
        15
    );
}

#[test]
fn adapter_checkpoint_refuses_unresolved_calls_and_unbound_journal_references() {
    for mode in ["reserved", "started", "unknown"] {
        let (f, parent) = budgeted_adapter_fixture(2);
        let child = intent("zero-cost-physical", 0, OperationClass::Actor);
        f.store
            .reserve_adapter_child(f.id, "worker", &parent, child.clone(), f.now)
            .unwrap();
        if mode != "reserved" {
            f.store
                .start_adapter_child(f.id, "worker", &parent, &child, f.now)
                .unwrap();
        }
        if mode == "unknown" {
            f.store
                .settle_adapter_child(f.id, &parent, &child, None, hash("unknown receipt"))
                .unwrap();
        }
        assert!(
            f.store
                .checkpoint_budgeted_adapter_job(
                    f.id,
                    "worker",
                    &parent,
                    "attention-1".into(),
                    b"pause without a known call receipt",
                    f.now
                )
                .is_err(),
            "mode {mode}"
        );
        assert!(f.store.adapter_checkpoint(f.id, &parent).unwrap().is_none());
    }

    let (f, parent) = budgeted_adapter_fixture(2);
    let checkpoint = f
        .store
        .checkpoint_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            "attention-1".into(),
            b"parked pause",
            f.now,
        )
        .unwrap();
    for field in ["bytes", "budget", "epoch", "question"] {
        let mut forged = checkpoint.clone();
        match field {
            "bytes" => forged.content_hash = f.store.put_blob(f.id, b"different pause").unwrap(),
            "budget" => forged.budget_hash = hash("different budget"),
            "epoch" => forged.source_epoch += 1,
            "question" => forged.attention_id = "different-question".into(),
            _ => unreachable!(),
        }
        assert!(
            f.commit(RunEvent::ActorCheckpointed {
                checkpoint: forged,
                reason: "missing fact".into()
            })
            .is_err(),
            "field {field}"
        );
    }
    f.commit(RunEvent::ActorCheckpointed {
        checkpoint: checkpoint.clone(),
        reason: "missing fact".into(),
    })
    .unwrap();
    f.commit(RunEvent::OwnerResumed {
        attention_id: "attention-1".into(),
    })
    .unwrap();
    assert!(
        f.store
            .resume_budgeted_adapter_job(f.id, "worker", &parent, &checkpoint, f.now)
            .is_err(),
        "status-only resume cannot manufacture answer evidence"
    );
    assert!(f
        .store
        .reserve_adapter_child(
            f.id,
            "worker",
            &parent,
            intent("status-only-call", 1, OperationClass::Actor),
            f.now
        )
        .is_err());
}

#[test]
fn adapter_checkpoint_continuation_refuses_stopped_expired_and_changed_authority() {
    for mode in [
        "cancelled",
        "expired",
        "wrong-holder",
        "wrong-parent",
        "wrong-checkpoint",
    ] {
        let (f, parent, checkpoint) = parked_adapter_fixture();
        f.commit(owner_input(&f, b"the required source fact"))
            .unwrap();
        if mode == "cancelled" {
            f.store.commit(f.id, 2, RunEvent::Cancelled, f.now).unwrap();
        }
        let before = serde_json::to_value(f.store.adapter_budget(f.id, &parent).unwrap()).unwrap();
        let mut changed_parent = parent.clone();
        let mut changed_checkpoint = checkpoint.clone();
        if mode == "wrong-parent" {
            changed_parent.input_hash = hash("different actor input");
        }
        if mode == "wrong-checkpoint" {
            changed_checkpoint.attention_id = "another-question".into();
        }
        let now = if mode == "expired" {
            f.now + Duration::seconds(LEASE_SECS)
        } else {
            f.now
        };
        let holder = if mode == "wrong-holder" {
            "another-worker"
        } else {
            "worker"
        };
        assert!(
            f.store
                .resume_budgeted_adapter_job(
                    f.id,
                    holder,
                    &changed_parent,
                    &changed_checkpoint,
                    now
                )
                .is_err(),
            "mode {mode}"
        );
        assert_eq!(
            serde_json::to_value(f.store.adapter_budget(f.id, &parent).unwrap()).unwrap(),
            before
        );
    }
}

#[test]
fn adapter_workspace_loss_after_checkpoint_staging_cannot_create_fresh_work() {
    let (f, parent) = budgeted_adapter_fixture(2);
    let work = f
        .store
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .unwrap();
    std::fs::write(work.join("draft.md"), b"paid work before question").unwrap();
    f.store
        .checkpoint_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            "attention-1".into(),
            b"exact persisted actor state",
            f.now,
        )
        .unwrap();
    let state = f.store.load(f.id).unwrap();
    assert!(
        state.attempts.last().unwrap().checkpoint.is_none(),
        "simulate interruption before the controller commits ActorCheckpointed"
    );
    // This directory belongs only to the test TempDir. Reconstruct the store
    // after losing the staged checkpoint's work files, never runtime data.
    std::fs::remove_dir_all(&work).unwrap();
    let restarted = RunStore::new(f.temp.path(), state.scope);
    assert!(restarted
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .is_err());
    assert!(
        !work.exists(),
        "a staged adapter checkpoint also forbids fresh work"
    );
}

#[test]
fn adapter_workspace_survives_answer_restart_and_refuses_missing_checkpoint_files() {
    let (f, parent) = budgeted_adapter_fixture(2);
    let work = f
        .store
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .unwrap();
    std::fs::write(work.join("draft.md"), b"in-progress candidate bytes").unwrap();
    let checkpoint = f
        .store
        .checkpoint_budgeted_adapter_job(
            f.id,
            "worker",
            &parent,
            "attention-1".into(),
            b"exact persisted actor state",
            f.now,
        )
        .unwrap();
    f.commit(RunEvent::ActorCheckpointed {
        checkpoint,
        reason: "material missing fact".into(),
    })
    .unwrap();
    f.commit(RunEvent::LeaseReleased).unwrap();
    let answered = f.commit(owner_input(&f, b"owner fact")).unwrap();
    let restarted = RunStore::new(f.temp.path(), answered.scope);
    restarted
        .claim(f.id, "replacement-worker".into(), f.now)
        .unwrap();
    let resumed = restarted
        .prepare_adapter_workspace(f.id, "replacement-worker", &parent, f.now)
        .unwrap();
    assert_eq!(resumed, work);
    assert_eq!(
        std::fs::read(resumed.join("draft.md")).unwrap(),
        b"in-progress candidate bytes"
    );
    assert!(restarted
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .is_err());
    assert!(restarted
        .prepare_adapter_workspace(
            f.id,
            "replacement-worker",
            &intent("different-actor", 50, OperationClass::Actor),
            f.now
        )
        .is_err());
    // Only this test-created TempDir workspace is removed. Simulate lost
    // checkpoint files without adding a hand-rolled durable write path.
    std::fs::remove_dir_all(&work).unwrap();
    assert!(restarted
        .prepare_adapter_workspace(f.id, "replacement-worker", &parent, f.now)
        .is_err());
    assert!(
        !work.exists(),
        "a missing checkpoint work area cannot be recreated as a fresh one"
    );
}

#[cfg(unix)]
#[test]
fn adapter_workspace_refuses_a_symlink_instead_of_adopting_an_outside_directory() {
    let (f, parent) = budgeted_adapter_fixture(2);
    let work = f
        .store
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    // The fixture-created work directory is empty; replace only that entry
    // to simulate an attacker substituting a symlink after checkpoint setup.
    std::fs::remove_dir(&work).unwrap();
    std::os::unix::fs::symlink(outside.path(), &work).unwrap();
    assert!(f
        .store
        .prepare_adapter_workspace(f.id, "worker", &parent, f.now)
        .is_err());
    assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
}

#[test]
fn critic_restart_requires_exact_unused_intent_and_preserves_original_reservation() {
    let f = Fixture::new(proposal());
    f.start();
    f.seal();
    let critic_id = "critic-0".to_string();
    let old = intent("critic-pre-io", 5, OperationClass::Critic);
    f.commit(RunEvent::ReviewStarted {
        critic_id: critic_id.clone(),
        operation: old.clone(),
    })
    .unwrap();
    let replacement = OperationIntent {
        id: "critic-restarted".into(),
        ..old.clone()
    };
    let event = |replacement: OperationIntent| RunEvent::ReviewRestarted {
        critic_id: critic_id.clone(),
        previous_operation_id: old.id.clone(),
        replacement,
    };
    assert!(f.commit(event(replacement.clone())).is_err());
    f.commit(RunEvent::OperationUnknown {
        operation_id: old.id.clone(),
    })
    .unwrap();
    assert!(f.commit(event(replacement.clone())).is_err());
    f.commit(RunEvent::OperationNotPerformed {
        operation_id: old.id.clone(),
        evidence_hash: hash("no critic I/O"),
    })
    .unwrap();
    for fault in 0..4 {
        let mut changed = replacement.clone();
        match fault {
            0 => changed.id = old.id.clone(),
            1 => changed.input_hash = hash("different draft"),
            2 => changed.maximum_cost_microusd += 1,
            3 => changed.class = OperationClass::Actor,
            _ => unreachable!(),
        }
        assert!(f.commit(event(changed)).is_err());
    }
    let state = f.commit(event(replacement.clone())).unwrap();
    assert_eq!(state.budget.spend().unwrap().total().unwrap(), 90);
    assert_eq!(
        state.budget.allowances()["review-headroom"].remaining_microusd,
        35
    );
    assert_eq!(
        state.attempts[0].critic_operations[&critic_id],
        vec![old.id.clone(), replacement.id.clone()]
    );
    assert!(f
        .commit(event(OperationIntent {
            id: "another-restart".into(),
            ..replacement
        }))
        .is_err());
    assert_eq!(
        f.store.load(f.id).unwrap().budget.spend().unwrap(),
        state.budget.spend().unwrap()
    );
}

#[test]
fn critic_restart_cannot_exceed_the_original_run_cost_ceiling() {
    let mut p = proposal();
    p.budget.max_cost_microusd = 90;
    let f = Fixture::new(p);
    f.start();
    f.seal();
    let old = intent("critic-pre-io", 5, OperationClass::Critic);
    f.commit(RunEvent::ReviewStarted {
        critic_id: "critic-0".into(),
        operation: old.clone(),
    })
    .unwrap();
    f.commit(RunEvent::OperationNotPerformed {
        operation_id: old.id.clone(),
        evidence_hash: hash("no critic I/O"),
    })
    .unwrap();
    f.commit(RunEvent::OperationReserved {
        operation: intent("other-admitted-cost", 5, OperationClass::Reconciliation),
    })
    .unwrap();
    assert!(f
        .commit(RunEvent::ReviewRestarted {
            critic_id: "critic-0".into(),
            previous_operation_id: old.id.clone(),
            replacement: OperationIntent {
                id: "critic-restarted".into(),
                ..old
            },
        })
        .is_err());
    let state = f.store.load(f.id).unwrap();
    assert_eq!(state.budget.spend().unwrap().total().unwrap(), 90);
    assert_eq!(state.attempts[0].critic_operations["critic-0"].len(), 1);
}

#[test]
fn failed_actor_closes_unused_reviews_and_records_a_bounded_retry() {
    let f = Fixture::new(check_only());
    f.start();
    let state = f
        .commit(RunEvent::ActorFailed {
            reason: "declared file is missing".into(),
            receipt: OperationReceipt {
                input_hash: hash("actor"),
                actual_cost_microusd: Some(17),
                receipt_hash: hash("failed actor receipt"),
            },
        })
        .unwrap();
    assert_eq!(state.status, RunStatus::Admitted);
    assert!(state.attempts[0].decided);
    assert_eq!(
        state.attempts[0].failure.as_deref(),
        Some("declared file is missing")
    );
    assert!(state.attempts[0].artifact.is_none());
    assert_eq!(state.budget.spend().unwrap().settled_microusd, 17);
    assert_eq!(state.budget.spend().unwrap().reserved_microusd, 0);
    let state = f
        .commit(RunEvent::AttemptStarted {
            actor: intent("actor-repair", 50, OperationClass::Actor),
            review: Allowance {
                id: "repair-review".into(),
                input_hash: hash("repair-review"),
                maximum_microusd: 40,
                remaining_microusd: 40,
                closed: false,
            },
        })
        .unwrap();
    assert_eq!(state.attempts.len(), 2);
    assert_eq!(state.status, RunStatus::Attempting);
    assert!(state.retained.artifact().is_none());
}

#[test]
fn failed_actor_with_unknown_usage_pauses_and_retains_its_ceiling() {
    let f = Fixture::new(check_only());
    f.start();
    let state = f
        .commit(RunEvent::ActorFailed {
            reason: "invalid declared bytes".into(),
            receipt: OperationReceipt {
                input_hash: hash("actor"),
                actual_cost_microusd: None,
                receipt_hash: hash("unknown failure receipt"),
            },
        })
        .unwrap();
    assert_eq!(state.status, RunStatus::WaitingOwner);
    assert_eq!(state.budget.spend().unwrap().reserved_microusd, 50);
    assert!(state.attention_id.is_some());
}

#[test]
fn public_read_reservations_replay_without_resend_or_budget_reset() {
    let mut proposal = check_only();
    proposal.collected_source_limits = Some(CollectedSourceLimits {
        maximum_sources: 2,
        maximum_bytes: 128,
    });
    let f = Fixture::new(proposal);
    f.start();
    let policy = hash("public-read policy");
    let first = hash("request one");
    let second = hash("request two");
    assert!(matches!(
        f.store
            .claim_collected_read(f.id, "worker", &policy, &first, 2, 64, f.now)
            .unwrap(),
        CollectedReadClaim::Reserved
    ));
    assert!(matches!(
        f.store
            .claim_collected_read(f.id, "worker", &policy, &first, 2, 64, f.now)
            .unwrap(),
        CollectedReadClaim::Unknown
    ));
    f.store
        .finish_collected_read(
            f.id,
            &policy,
            &first,
            b"sealed response",
            Some(b"fetched fact"),
        )
        .unwrap();
    assert!(f
        .store
        .finish_collected_read(
            f.id,
            &policy,
            &first,
            b"changed response",
            Some(b"fetched fact")
        )
        .is_err());
    assert!(matches!(
        f.store
            .claim_collected_read(f.id, "worker", &policy, &second, 2, 64, f.now)
            .unwrap(),
        CollectedReadClaim::Reserved
    ));
    assert!(f
        .store
        .claim_collected_read(
            f.id,
            "worker",
            &policy,
            &hash("third request"),
            2,
            64,
            f.now
        )
        .is_err());
    let recovered = RunStore::new(
        f.temp.path(),
        RunScope {
            principal: "owner".into(),
            workspace: "work".into(),
        },
    );
    match recovered
        .claim_collected_read(f.id, "worker", &policy, &first, 2, 64, f.now)
        .unwrap()
    {
        CollectedReadClaim::Completed(bytes) => assert_eq!(bytes, b"sealed response"),
        _ => panic!("completed public read was not reused"),
    }
    assert!(matches!(
        recovered
            .claim_collected_read(f.id, "worker", &policy, &second, 2, 64, f.now)
            .unwrap(),
        CollectedReadClaim::Unknown
    ));
    let collected = recovered.collected_read_sources(f.id, &policy).unwrap();
    let files = BTreeMap::from([("answer.md".into(), b"candidate".to_vec())]);
    let bare = recovered.prepare_artifact(f.id, &files).unwrap();
    let artifact = recovered
        .prepare_artifact_with_sources(f.id, &files, &collected)
        .unwrap();
    assert_ne!(bare.hash, artifact.hash);
    assert_eq!(artifact.collected_sources.len(), 1);
    f.commit(RunEvent::ArtifactSealed { artifact }).unwrap();
    assert!(
        recovered
            .claim_collected_read(f.id, "worker", &policy, &first, 2, 64, f.now)
            .is_err(),
        "reviewing cannot start actor reads"
    );
}

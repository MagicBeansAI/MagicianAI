use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Mutex,
};

use super::*;
use crate::{
    review::ReviewStatus,
    store::{Admission, RunScope},
    tests::{hash, proposal, verdicts},
};

struct Actor {
    calls: AtomicUsize,
    reconcile_calls: AtomicUsize,
    requests: Mutex<Vec<ActorRequest>>,
    repair: bool,
    hang: bool,
    recover: bool,
    not_performed: bool,
    oversize_once: bool,
    cancelled: Arc<AtomicBool>,
    started: tokio::sync::Notify,
}

impl Actor {
    fn new(repair: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            reconcile_calls: AtomicUsize::new(0),
            requests: Mutex::new(vec![]),
            repair,
            hang: false,
            recover: false,
            not_performed: false,
            oversize_once: false,
            cancelled: Arc::new(AtomicBool::new(false)),
            started: tokio::sync::Notify::new(),
        }
    }
    fn candidate(&self, operation: &OperationIntent, model: ModelPin, fixed: bool) -> ActorOutput {
        ActorOutput::Candidate {
            files: BTreeMap::from([(
                "draft.md".into(),
                if fixed {
                    b"supported draft".to_vec()
                } else {
                    b"unsupported claim".to_vec()
                },
            )]),
            model,
            receipt: receipt(operation, 10),
        }
    }
}

struct CancelGuard(Arc<AtomicBool>);
impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

fn receipt(operation: &OperationIntent, actual: u64) -> OperationReceipt {
    OperationReceipt {
        input_hash: operation.input_hash.clone(),
        actual_cost_microusd: Some(actual),
        receipt_hash: hash(&format!("receipt:{}:{actual}", operation.id)),
    }
}

#[async_trait]
impl ActorAdapter for Actor {
    fn plan(&self, request: &ActorRequest) -> Result<AttemptPlan> {
        Ok(AttemptPlan {
            model: request.contract.definition().actor.clone(),
            actor_maximum_microusd: 20,
            maximum_artifact_bytes: 1024,
            critic_maximums_microusd: request
                .contract
                .definition()
                .critics
                .iter()
                .map(|p| (p.id.clone(), if p.model.is_some() { 5 } else { 0 }))
                .collect(),
        })
    }
    async fn run(
        &self,
        request: &ActorRequest,
        _plan: &AttemptPlan,
        operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<ActorOutput> {
        let _guard = CancelGuard(self.cancelled.clone());
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.requests.lock().unwrap().push(request.clone());
        self.started.notify_one();
        if self.hang {
            std::future::pending::<()>().await;
        }
        if self.oversize_once && request.attempt_number == 1 {
            return Ok(ActorOutput::Candidate {
                files: BTreeMap::from([("draft.md".into(), vec![b'x'; 1025])]),
                model: request.contract.definition().actor.clone(),
                receipt: receipt(operation, 10),
            });
        }
        Ok(self.candidate(
            operation,
            request.contract.definition().actor.clone(),
            !self.repair || request.attempt_number > 1,
        ))
    }
    async fn reconcile(
        &self,
        operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<Recovery<ActorOutput>> {
        self.reconcile_calls.fetch_add(1, Ordering::SeqCst);
        if self.not_performed {
            Ok(Recovery::NotPerformed {
                evidence_hash: hash("verified job absence"),
            })
        } else if self.recover {
            Ok(Recovery::Completed(self.candidate(
                operation,
                proposal().actor,
                true,
            )))
        } else {
            Ok(Recovery::Unknown {
                reason: "remote result has no lookup".into(),
            })
        }
    }
}

struct Critic {
    calls: Mutex<BTreeMap<String, usize>>,
    timeout_once: bool,
    low_confidence: bool,
    disagreement: bool,
    malformed_quote_once: bool,
    pre_io_absence: bool,
}

#[async_trait]
impl CriticAdapter for Critic {
    async fn reconcile(
        &self,
        _: &OperationIntent,
        _: CancellationToken,
    ) -> Result<Recovery<CriticOutput>> {
        Ok(if self.pre_io_absence {
            Recovery::NotPerformed {
                evidence_hash: hash("verified critic job absence"),
            }
        } else {
            Recovery::Unknown {
                reason: "critic outcome unknown".into(),
            }
        })
    }
    async fn run(
        &self,
        request: &CriticRequest,
        operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<CriticOutput> {
        let count = {
            let mut calls = self.calls.lock().unwrap();
            let count = calls.entry(request.critic.id.clone()).or_default();
            *count += 1;
            *count
        };
        let ctx = ReviewContext {
            artifact_hash: request.artifact_hash.clone(),
            source_epoch: request.source_epoch,
            evidence_ids: request.evidence_ids.clone(),
            artifact_texts: request
                .artifact_files
                .values()
                .filter_map(|bytes| std::str::from_utf8(bytes).ok().map(str::to_owned))
                .collect(),
            sources: request.sources.clone(),
        };
        let grounded = request.artifact_files["draft.md"] == b"supported draft"
            || (self.disagreement && request.critic.id == "critic-0");
        let mut verdict = verdicts(
            &request.contract,
            &ctx,
            if grounded { 0.9 } else { 0.95 },
            grounded,
        )
        .into_iter()
        .find(|v| v.critic_id == request.critic.id)
        .unwrap();
        verdict.evidence_ids = request.evidence_ids.iter().cloned().collect();
        if self.timeout_once && request.critic.id == "critic-0" && count == 1 {
            verdict.status = ReviewStatus::TimedOut;
        }
        if self.malformed_quote_once && request.critic.id == "critic-0" {
            if count == 1 {
                assert!(request.retry_feedback.is_none());
            } else {
                let feedback = request
                    .retry_feedback
                    .as_ref()
                    .expect("retry needs validation feedback");
                assert!(feedback.validation_error.contains("artifact quote"));
                assert_eq!(
                    feedback.previous_verdict.artifact_hash,
                    request.artifact_hash
                );
                assert_eq!(feedback.previous_verdict.critic_id, request.critic.id);
                assert_eq!(
                    feedback
                        .previous_verdict
                        .scores
                        .iter()
                        .find_map(|score| score.coverage.as_ref())
                        .unwrap()
                        .claims[0]
                        .artifact_quote,
                    "not an exact sealed quote"
                );
            }
        }
        // Repeating the same request cannot repair this malformed review.
        if self.malformed_quote_once
            && request.critic.id == "critic-0"
            && request.retry_feedback.is_none()
        {
            verdict
                .scores
                .iter_mut()
                .find_map(|score| score.coverage.as_mut())
                .unwrap()
                .claims[0]
                .artifact_quote = "not an exact sealed quote".into();
        }
        if self.low_confidence && request.critic.id == "critic-0" {
            verdict.scores[0].confidence = Some(0.7);
        }
        Ok(CriticOutput {
            verdict,
            receipt: receipt(
                operation,
                if request.critic.model.is_some() { 3 } else { 0 },
            ),
        })
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    controller: Controller,
    actor: Arc<Actor>,
    critic: Arc<Critic>,
    id: Uuid,
}

impl Fixture {
    fn new(actor: Actor, critic: Critic, max_cost: u64, seconds: u64) -> Self {
        Self::with_policy(actor, critic, max_cost, seconds, Default::default())
    }
    fn with_policy(
        actor: Actor,
        critic: Critic,
        max_cost: u64,
        seconds: u64,
        policy: crate::contract::UnknownReviewPolicy,
    ) -> Self {
        Self::with_retries(actor, critic, max_cost, seconds, policy, 1)
    }
    fn with_retries(
        actor: Actor,
        critic: Critic,
        max_cost: u64,
        seconds: u64,
        policy: crate::contract::UnknownReviewPolicy,
        retries: u32,
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
        let mut p = proposal();
        p.review.unknown_review = policy;
        p.review.max_retries_per_critic = retries;
        p.sources.clear();
        p.sources.push(crate::contract::SourcePin {
            id: "owner-note".into(),
            content_hash: store.put_blob(id, b"owner source bytes").unwrap(),
        });
        p.budget.max_cost_microusd = max_cost;
        p.budget.max_wall_clock_secs = seconds;
        store
            .admit(
                id,
                p.try_into().unwrap(),
                Admission {
                    routing_policy: None,
                    handoff_kind: Default::default(),
                    owner_principal: "owner".into(),
                    source_id: "approved-program".into(),
                    source_hash: hash("program"),
                    task_id: "task-1".into(),
                    not_after: None,
                },
                Utc::now(),
            )
            .unwrap();
        let actor = Arc::new(actor);
        let critic = Arc::new(critic);
        let controller = Controller {
            store,
            actor: actor.clone(),
            critic: critic.clone(),
            worker_id: "worker-1".into(),
        };
        Self {
            _temp: temp,
            controller,
            actor,
            critic,
            id,
        }
    }
    async fn run(&self) -> RunState {
        self.controller
            .run_pass(self.id, CancellationToken::new())
            .await
            .unwrap()
    }
}

fn critic() -> Critic {
    Critic {
        calls: Mutex::new(BTreeMap::new()),
        timeout_once: false,
        low_confidence: false,
        disagreement: false,
        malformed_quote_once: false,
        pre_io_absence: false,
    }
}

struct PausingActor {
    store: Arc<RunStore>,
    id: Uuid,
    initial_calls: AtomicUsize,
    resume_calls: AtomicUsize,
    resumed_requests: Mutex<Vec<ActorRequest>>,
}

impl PausingActor {
    fn holder(&self) -> String {
        self.store.load(self.id).unwrap().lease.unwrap().holder
    }

    fn physical_call(&self, parent: &OperationIntent, id: &str, actual: u64) {
        let child = OperationIntent {
            id: id.into(),
            input_hash: hash(id),
            class: OperationClass::Actor,
            maximum_cost_microusd: 10,
        };
        let now = Utc::now();
        self.store
            .reserve_adapter_child(self.id, &self.holder(), parent, child.clone(), now)
            .unwrap();
        self.store
            .start_adapter_child(self.id, &self.holder(), parent, &child, now)
            .unwrap();
        self.store
            .settle_adapter_child(
                self.id,
                parent,
                &child,
                Some(actual),
                hash(&format!("physical-receipt-{id}")),
            )
            .unwrap();
    }
}

#[async_trait]
impl ActorAdapter for PausingActor {
    fn plan(&self, request: &ActorRequest) -> Result<AttemptPlan> {
        let mut plan = Actor::new(false).plan(request)?;
        let review_headroom = plan.critic_maximums_microusd.values().sum::<u64>()
            * (u64::from(request.contract.definition().review.max_retries_per_critic) + 1);
        plan.actor_maximum_microusd = plan
            .actor_maximum_microusd
            .min(
                request
                    .remaining_cost_microusd
                    .saturating_sub(review_headroom),
            )
            .max(10);
        Ok(plan)
    }

    async fn run(
        &self,
        _request: &ActorRequest,
        _plan: &AttemptPlan,
        operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<ActorOutput> {
        self.initial_calls.fetch_add(1, Ordering::SeqCst);
        self.store.begin_budgeted_adapter_job(
            self.id,
            &self.holder(),
            operation,
            hash("scripted actor recipe"),
            2,
            Utc::now(),
        )?;
        self.physical_call(operation, "before-question", 7);
        let checkpoint = self.store.checkpoint_budgeted_adapter_job(
            self.id,
            &self.holder(),
            operation,
            "material-fact-request".into(),
            b"persisted actor context",
            Utc::now(),
        )?;
        Ok(ActorOutput::Paused {
            reason: "a material owner fact is missing".into(),
            checkpoint,
        })
    }

    async fn reconcile(
        &self,
        operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<Recovery<ActorOutput>> {
        if let super::super::store::AdapterJobLookup::Completed(bytes) =
            self.store.lookup_adapter_job(self.id, operation)?
        {
            return Ok(Recovery::Completed(serde_json::from_slice(&bytes)?));
        }
        let checkpoint = self
            .store
            .adapter_checkpoint(self.id, operation)?
            .context("missing actor checkpoint")?;
        Ok(Recovery::Completed(ActorOutput::Paused {
            reason: "a material owner fact is missing".into(),
            checkpoint,
        }))
    }

    async fn resume(
        &self,
        request: &ActorRequest,
        _plan: &AttemptPlan,
        operation: &OperationIntent,
        checkpoint: &ActorCheckpoint,
        _cancel: CancellationToken,
    ) -> Result<ActorOutput> {
        self.resume_calls.fetch_add(1, Ordering::SeqCst);
        self.resumed_requests.lock().unwrap().push(request.clone());
        self.store.resume_budgeted_adapter_job(
            self.id,
            &self.holder(),
            operation,
            checkpoint,
            Utc::now(),
        )?;
        self.physical_call(operation, "after-answer", 3);
        let output = Actor::new(false).candidate(
            operation,
            request.contract.definition().actor.clone(),
            true,
        );
        self.store.finish_budgeted_adapter_job(
            self.id,
            operation,
            &serde_json::to_vec(&output)?,
            &receipt(operation, 10),
        )?;
        Ok(output)
    }
}

struct AnswerContextCritic {
    inner: Critic,
    requests: Mutex<Vec<CriticRequest>>,
}

#[async_trait]
impl CriticAdapter for AnswerContextCritic {
    async fn run(
        &self,
        request: &CriticRequest,
        operation: &OperationIntent,
        cancel: CancellationToken,
    ) -> Result<CriticOutput> {
        self.requests.lock().unwrap().push(request.clone());
        self.inner.run(request, operation, cancel).await
    }
}

#[tokio::test]
async fn controller_resumes_answered_checkpoint_on_the_same_attempt_and_enriches_critics() {
    // Reserve the complete owner allowance: the resumed plan must use its
    // frozen funding, not the now-zero unreserved run dollars.
    let mut f = Fixture::new(Actor::new(false), critic(), 50, 60);
    let actor = Arc::new(PausingActor {
        store: f.controller.store.clone(),
        id: f.id,
        initial_calls: AtomicUsize::new(0),
        resume_calls: AtomicUsize::new(0),
        resumed_requests: Mutex::new(vec![]),
    });
    let critics = Arc::new(AnswerContextCritic {
        inner: critic(),
        requests: Mutex::new(vec![]),
    });
    f.controller.actor = actor.clone();
    f.controller.critic = critics.clone();
    let waiting = f
        .controller
        .run_pass(f.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(waiting.status, RunStatus::WaitingOwner);
    assert!(f.controller.store.load(f.id).unwrap().lease.is_none());
    let parent = waiting.budget.operations()[&waiting.attempts[0].actor_operation_id]
        .intent
        .clone();
    assert_eq!(
        waiting.budget.operations()[&parent.id].settlement,
        crate::budget::Settlement::Reserved
    );
    assert_eq!(
        f.controller
            .store
            .adapter_budget(f.id, &parent)
            .unwrap()
            .ledger
            .spend()
            .unwrap()
            .settled_microusd,
        7
    );
    assert!(critics.requests.lock().unwrap().is_empty());

    // A legacy resume supplies no fact. Recovery parks the same question and
    // makes no model call, preserving the paid-call ledger and original ID.
    f.controller
        .store
        .commit(
            f.id,
            waiting.generation,
            RunEvent::OwnerResumed {
                attention_id: waiting.attention_id.clone().unwrap(),
            },
            Utc::now(),
        )
        .unwrap();
    let still_waiting = f
        .controller
        .run_pass(f.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(still_waiting.status, RunStatus::WaitingOwner);
    assert_eq!(still_waiting.attention_id, waiting.attention_id);
    assert_eq!(actor.resume_calls.load(Ordering::SeqCst), 0);
    let answer = b"Question: required fact? Answer: owner-supplied fact.";
    let response_hash = f.controller.store.put_blob(f.id, answer).unwrap();
    f.controller
        .store
        .commit(
            f.id,
            still_waiting.generation,
            RunEvent::OwnerInputRecorded {
                attention_id: still_waiting.attention_id.unwrap(),
                response_hash,
            },
            Utc::now(),
        )
        .unwrap();

    // Reconstruct the controller/store before continuing the authenticated
    // answer. Adapter reconciliation reads the persisted checkpoint.
    let controller = Controller {
        store: Arc::new(RunStore::new(f._temp.path(), waiting.scope.clone())),
        actor: actor.clone(),
        critic: critics.clone(),
        worker_id: "replacement-worker".into(),
    };
    let accepted = controller
        .run_pass(f.id, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(accepted.status, RunStatus::Accepted);
    assert_eq!(accepted.attempts.len(), 1);
    assert_eq!(accepted.attempts[0].actor_operation_id, parent.id);
    assert_eq!(accepted.deadline_at, waiting.deadline_at);
    assert_eq!(accepted.contract, waiting.contract);
    assert_eq!(actor.initial_calls.load(Ordering::SeqCst), 1);
    assert_eq!(actor.resume_calls.load(Ordering::SeqCst), 1);
    let requests = actor.resumed_requests.lock().unwrap();
    assert_eq!(requests[0].attempt_number, 1);
    assert_eq!(requests[0].remaining_cost_microusd, 50);
    assert_eq!(requests[0].source_epoch, 1);
    assert!(requests[0].sources.values().any(|bytes| bytes == answer));
    let requests = critics.requests.lock().unwrap();
    assert_eq!(requests.len(), 4);
    assert!(requests.iter().all(|request| request.source_epoch == 1
        && request.sources.values().any(|bytes| bytes == answer)));
    assert_eq!(
        accepted.retained.champion.as_ref().unwrap().contract_hash,
        accepted.evaluation_contract().unwrap().hash().unwrap()
    );
    assert_eq!(accepted.budget.spend().unwrap().settled_microusd, 19);
    assert_eq!(accepted.budget.spend().unwrap().reserved_microusd, 0);
    assert_eq!(
        controller
            .store
            .adapter_budget(f.id, &parent)
            .unwrap()
            .ledger
            .operations()
            .len(),
        2
    );
}

#[tokio::test]
async fn malformed_evidence_retries_only_that_critic_on_the_same_sealed_draft() {
    let mut c = critic();
    c.malformed_quote_once = true;
    let f = Fixture::new(Actor::new(false), c, 1000, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 1);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
    let calls = f.critic.calls.lock().unwrap();
    assert_eq!(calls["critic-0"], 2);
    assert_eq!(calls["critic-1"], 1);
    assert_eq!(calls["critic-2"], 1);
    assert_eq!(result.budget.spend().unwrap().settled_microusd, 22);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 0);
    let recovered = f.controller.store.load(f.id).unwrap();
    assert_eq!(recovered.decisions, result.decisions);
    assert_eq!(
        recovered.budget.spend().unwrap(),
        result.budget.spend().unwrap()
    );
}

#[tokio::test]
async fn completed_disagreement_panel_retires_unused_headroom_and_repairs_within_budget() {
    let mut c = critic();
    c.disagreement = true;
    // First settled attempt costs 19; a second full reservation costs 50.
    // This ceiling permits the next attempt only after unused review retry
    // headroom has been closed; no settled or uncertain cost is refunded.
    let f = Fixture::with_policy(
        Actor::new(true),
        c,
        69,
        60,
        crate::contract::UnknownReviewPolicy::ReviseSupportedDefects,
    );
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(
        result.decisions[0].kind,
        crate::decision::DecisionKind::RepairAndRetry
    );
    assert!(result.decisions[0].retained.artifact().is_none());
    assert_eq!(result.decisions[0].retained.plateau_count, 0);
    assert!(!result.attempts[0].review.as_ref().unwrap().complete);
    assert!(
        result.attempts[0]
            .review
            .as_ref()
            .unwrap()
            .disagreement_repair_eligible
    );
    assert_eq!(result.budget.spend().unwrap().settled_microusd, 38);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 0);
    let requests = f.actor.requests.lock().unwrap();
    assert!(requests[1].retained_files.is_empty());
    assert_eq!(
        requests[1].feedback["decisions"][0]["kind"],
        "repair_and_retry"
    );
    let recovered = f.controller.store.load(f.id).unwrap();
    assert_eq!(recovered.decisions, result.decisions);
    assert_eq!(recovered.attempts[0].review, result.attempts[0].review);
    assert_eq!(
        recovered.budget.spend().unwrap(),
        result.budget.spend().unwrap()
    );
}

#[tokio::test]
async fn generic_controller_repairs_unsupported_draft_with_ledger_feedback() {
    let f = Fixture::new(Actor::new(true), critic(), 1000, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(result.budget.spend().unwrap().total().unwrap(), 38);
    let requests = f.actor.requests.lock().unwrap();
    assert!(requests[0].retained_files.is_empty());
    assert_eq!(requests[1].retained_files["draft.md"], b"unsupported claim");
    assert!(!requests[1].feedback["attempts"][0]["verdicts"]
        .as_object()
        .unwrap()
        .is_empty());
    let recovered = f.controller.store.load(f.id).unwrap();
    assert_eq!(
        recovered.decisions.last().unwrap().accepted_hash,
        result.decisions.last().unwrap().accepted_hash
    );
}

#[tokio::test]
async fn infrastructure_review_retries_same_artifact_without_an_actor_attempt() {
    let mut c = critic();
    c.timeout_once = true;
    let f = Fixture::new(Actor::new(false), c, 1000, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 1);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.critic.calls.lock().unwrap()["critic-0"], 2);
    assert_eq!(result.attempts[0].critic_operations["critic-0"].len(), 2);
}

#[tokio::test]
async fn low_confidence_parks_once_and_never_rejection_shops() {
    let mut c = critic();
    c.low_confidence = true;
    let f = Fixture::new(Actor::new(false), c, 1000, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::WaitingOwner);
    assert!(result.attention_id.is_some());
    assert_eq!(f.critic.calls.lock().unwrap()["critic-0"], 1);
    assert_eq!(result.retained.plateau_count, 0);
    assert!(result.retained.artifact().is_none());
    assert!(!result.attempts[0].decided);
}

#[tokio::test]
async fn insufficient_actor_and_review_headroom_stops_before_dispatch() {
    let f = Fixture::new(Actor::new(false), critic(), 49, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::BudgetExhausted);
    assert_eq!(result.attempts.len(), 0);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 0);
    assert!(f.critic.calls.lock().unwrap().is_empty());
    assert_eq!(result.budget.spend().unwrap().total().unwrap(), 0);
}

#[tokio::test]
async fn zero_cost_and_time_caps_admit_review_and_keep_usage_accounting() {
    let f = Fixture::new(Actor::new(true), critic(), 0, 0);
    let result = tokio::time::timeout(std::time::Duration::from_secs(10), f.run())
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 2);
    assert_eq!(result.deadline_at, chrono::DateTime::<Utc>::MAX_UTC);
    assert_eq!(result.budget.spend().unwrap().total().unwrap(), 38);
    let restored = f.controller.store.load(f.id).unwrap();
    assert_eq!(restored.effective_budget().unwrap().max_cost_microusd, 0);
    assert!(restored.budget_usage(Utc::now()).unwrap()["cost"]["remaining_microusd"].is_null());
}

#[tokio::test]
async fn unlimited_deadline_wait_has_no_overflowing_timer_and_stays_cancellable() {
    let cancel = CancellationToken::new();
    cancel.cancel();
    tokio::select! {
        _ = wait_for_deadline(chrono::DateTime::<Utc>::MAX_UTC) => panic!("unlimited deadline expired"),
        _ = cancel.cancelled() => {},
    }
    wait_for_deadline(Utc::now() - chrono::Duration::seconds(1)).await;
}

#[tokio::test]
async fn deadline_cancels_actor_and_preserves_unsettled_spend() {
    let mut actor = Actor::new(false);
    actor.hang = true;
    let f = Fixture::new(actor, critic(), 1000, 1);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::BudgetExhausted);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
    assert!(f.actor.cancelled.load(Ordering::SeqCst));
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 50);
    assert!(result.retained.artifact().is_none());
    assert!(f.critic.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn recovered_actor_result_is_reused_without_a_second_paid_dispatch() {
    let mut actor = Actor::new(false);
    actor.recover = true;
    let f = Fixture::new(actor, critic(), 1000, 60);
    let claimed = f
        .controller
        .store
        .claim(f.id, "crashed-worker".into(), Utc::now())
        .unwrap();
    let admitted = f.controller.prepare_attempt(&claimed).await.unwrap();
    let operation_id = admitted.attempts[0].actor_operation_id.clone();
    f.controller
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::LeaseReleased,
            Utc::now(),
        )
        .unwrap();
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 1);
    assert_eq!(result.attempts[0].actor_operation_id, operation_id);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.actor.reconcile_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn oversized_completed_draft_settles_and_repairs_without_owner_input() {
    let mut actor = Actor::new(false);
    actor.oversize_once = true;
    let f = Fixture::new(actor, critic(), 1000, 60);
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 2);
    assert!(result.attempts[0].decided);
    assert!(result.attempts[0].artifact.is_none());
    assert!(result.attempts[0]
        .failure
        .as_ref()
        .unwrap()
        .contains("byte ceiling"));
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 2);
    assert!(f.actor.requests.lock().unwrap()[1].feedback["attempts"][0]["failure"].is_string());
    assert_eq!(result.budget.spend().unwrap().settled_microusd, 29);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 0);
    assert_eq!(f.critic.calls.lock().unwrap()["critic-0"], 1);
    assert_eq!(
        f.controller.store.load(f.id).unwrap().attempts[0].failure,
        result.attempts[0].failure
    );
}

#[tokio::test]
async fn oversized_draft_preserves_model_identity_and_unknown_usage() {
    let f = Fixture::new(Actor::new(false), critic(), 1000, 60);
    let claimed = f
        .controller
        .store
        .claim(f.id, "worker-1".into(), Utc::now())
        .unwrap();
    let state = f.controller.prepare_attempt(&claimed).await.unwrap();
    let envelope = f.controller.envelope(&state).await.unwrap();
    let operation = &state.budget.operations()[&state.attempts[0].actor_operation_id].intent;
    let files = BTreeMap::from([("draft.md".into(), vec![b'x'; 1025])]);
    let mut changed_model = envelope.plan.model.clone();
    changed_model.configuration_hash = hash("unapproved model config");
    assert!(f
        .controller
        .seal_candidate(
            &state,
            &envelope,
            files.clone(),
            BTreeMap::new(),
            changed_model,
            receipt(operation, 10)
        )
        .await
        .is_err());
    assert_eq!(
        f.controller.store.load(f.id).unwrap().status,
        RunStatus::Attempting
    );
    let mut unknown = receipt(operation, 10);
    unknown.actual_cost_microusd = None;
    let result = f
        .controller
        .seal_candidate(
            &state,
            &envelope,
            files,
            BTreeMap::new(),
            envelope.plan.model.clone(),
            unknown,
        )
        .await
        .unwrap();
    assert_eq!(result.status, RunStatus::WaitingOwner);
    assert!(result.attempts[0].artifact.is_none());
    assert_eq!(result.budget.spend().unwrap().settled_microusd, 0);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 20);
    assert!(f.critic.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn undispatched_critic_recovers_with_zero_paid_retries_and_exact_headroom() {
    for proof_already_recorded in [false, true] {
        let mut c = critic();
        c.pre_io_absence = !proof_already_recorded;
        let f = Fixture::with_retries(Actor::new(false), c, 35, 60, Default::default(), 0);
        let claimed = f
            .controller
            .store
            .claim(f.id, "crashed-worker".into(), Utc::now())
            .unwrap();
        let state = f.controller.prepare_attempt(&claimed).await.unwrap();
        let state = f
            .controller
            .actor_step(&state, true, CancellationToken::new())
            .await
            .unwrap();
        let request = f
            .controller
            .critic_request(&state, state.contract.definition().critics[0].clone())
            .await
            .unwrap();
        let old = OperationIntent {
            id: "critic-pre-io".into(),
            input_hash: f
                .controller
                .store
                .put_blob(f.id, &serde_json::to_vec(&request).unwrap())
                .unwrap(),
            class: OperationClass::Critic,
            maximum_cost_microusd: 5,
        };
        f.controller
            .store
            .commit(
                f.id,
                state.generation,
                RunEvent::ReviewStarted {
                    critic_id: request.critic.id.clone(),
                    operation: old.clone(),
                },
                Utc::now(),
            )
            .unwrap();
        if proof_already_recorded {
            f.controller
                .store
                .commit(
                    f.id,
                    state.generation,
                    RunEvent::OperationNotPerformed {
                        operation_id: old.id.clone(),
                        evidence_hash: hash("verified critic job absence"),
                    },
                    Utc::now(),
                )
                .unwrap();
        }
        f.controller
            .store
            .commit(f.id, state.generation, RunEvent::LeaseReleased, Utc::now())
            .unwrap();
        let result = f.run().await;
        assert_eq!(result.status, RunStatus::Accepted);
        assert_eq!(result.attempts.len(), 1);
        assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
        assert_eq!(f.critic.calls.lock().unwrap()[&request.critic.id], 1);
        let ids = &result.attempts[0].critic_operations[&request.critic.id];
        assert_eq!(ids.len(), 2);
        assert_eq!(ids[0], old.id);
        assert!(matches!(
            result.budget.operations()[&old.id].settlement,
            crate::budget::Settlement::NotPerformed { .. }
        ));
        assert_eq!(
            result.budget.operations()[&ids[1]].intent.input_hash,
            old.input_hash
        );
        assert_eq!(result.budget.spend().unwrap().settled_microusd, 19);
        assert_eq!(result.budget.spend().unwrap().reserved_microusd, 0);
        assert_eq!(
            f.controller.store.load(f.id).unwrap().decisions,
            result.decisions
        );
    }
}

#[tokio::test]
async fn proven_pre_io_interruption_readmits_the_same_attempt_with_a_new_operation() {
    let mut actor = Actor::new(false);
    actor.not_performed = true;
    let f = Fixture::new(actor, critic(), 1000, 60);
    let claimed = f
        .controller
        .store
        .claim(f.id, "crashed-worker".into(), Utc::now())
        .unwrap();
    let admitted = f.controller.prepare_attempt(&claimed).await.unwrap();
    let prior = admitted.attempts[0].actor_operation_id.clone();
    f.controller
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::LeaseReleased,
            Utc::now(),
        )
        .unwrap();
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 1);
    assert_ne!(result.attempts[0].actor_operation_id, prior);
    let old = &result.budget.operations()[&prior];
    let new = &result.budget.operations()[&result.attempts[0].actor_operation_id];
    assert!(matches!(
        old.settlement,
        crate::budget::Settlement::NotPerformed { .. }
    ));
    assert_eq!(old.intent.input_hash, new.intent.input_hash);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
    assert_eq!(f.actor.requests.lock().unwrap()[0].attempt_number, 1);
}

#[tokio::test]
async fn journaled_actor_non_dispatch_proof_survives_recovery_lookup_unavailability() {
    let f = Fixture::new(Actor::new(false), critic(), 50, 60);
    let claimed = f
        .controller
        .store
        .claim(f.id, "crashed-worker".into(), Utc::now())
        .unwrap();
    let admitted = f.controller.prepare_attempt(&claimed).await.unwrap();
    let prior = admitted.attempts[0].actor_operation_id.clone();
    f.controller
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::OperationNotPerformed {
                operation_id: prior.clone(),
                evidence_hash: hash("durable actor dispatch absence"),
            },
            Utc::now(),
        )
        .unwrap();
    f.controller
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::LeaseReleased,
            Utc::now(),
        )
        .unwrap();

    // The recovered adapter's lookup returns Unknown. The journal already
    // contains the authoritative proof that permits this exact replacement.
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::Accepted);
    assert_eq!(result.attempts.len(), 1);
    assert_eq!(f.actor.reconcile_calls.load(Ordering::SeqCst), 0);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 1);
    let old = &result.budget.operations()[&prior];
    let new = &result.budget.operations()[&result.attempts[0].actor_operation_id];
    assert_ne!(old.intent.id, new.intent.id);
    assert_eq!(old.intent.input_hash, new.intent.input_hash);
    assert_eq!(
        old.intent.maximum_cost_microusd,
        new.intent.maximum_cost_microusd
    );
    assert!(matches!(old.settlement, Settlement::NotPerformed { .. }));
    assert_eq!(result.budget.spend().unwrap().settled_microusd, 19);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 0);
    assert_eq!(
        f.controller.store.load(f.id).unwrap().decisions,
        result.decisions
    );
}

#[tokio::test]
async fn unknown_remote_actor_is_parked_without_blind_resend() {
    let f = Fixture::new(Actor::new(false), critic(), 1000, 60);
    let claimed = f
        .controller
        .store
        .claim(f.id, "crashed-worker".into(), Utc::now())
        .unwrap();
    f.controller.prepare_attempt(&claimed).await.unwrap();
    f.controller
        .store
        .commit(
            f.id,
            claimed.generation,
            RunEvent::LeaseReleased,
            Utc::now(),
        )
        .unwrap();
    let result = f.run().await;
    assert_eq!(result.status, RunStatus::WaitingOwner);
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 50);
    assert_eq!(f.actor.calls.load(Ordering::SeqCst), 0);
    assert_eq!(result.attempts.len(), 1);
}

#[tokio::test]
async fn owner_cancellation_fences_controller_and_stops_child_work() {
    let mut actor = Actor::new(false);
    actor.hang = true;
    let f = Fixture::new(actor, critic(), 1000, 60);
    let cancel = CancellationToken::new();
    let run = f.controller.run_pass(f.id, cancel.clone());
    let cancellation = async {
        f.actor.started.notified().await;
        let state = f.controller.store.load(f.id).unwrap();
        f.controller
            .store
            .commit(f.id, state.generation + 1, RunEvent::Cancelled, Utc::now())
            .unwrap();
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(run, cancellation);
    assert_eq!(result.unwrap().status, RunStatus::Cancelled);
    assert!(f.actor.cancelled.load(Ordering::SeqCst));
    assert!(f
        .controller
        .store
        .load(f.id)
        .unwrap()
        .retained
        .artifact()
        .is_none());
}

struct SupersededActor {
    store: Arc<RunStore>,
    id: Uuid,
    fail_on_quiesce: bool,
    parent_cancel: CancellationToken,
}

#[async_trait]
impl ActorAdapter for SupersededActor {
    fn plan(&self, request: &ActorRequest) -> Result<AttemptPlan> {
        Actor::new(false).plan(request)
    }

    async fn run(
        &self,
        _request: &ActorRequest,
        _plan: &AttemptPlan,
        _operation: &OperationIntent,
        _cancel: CancellationToken,
    ) -> Result<ActorOutput> {
        let lease = self.store.load(self.id)?.lease.unwrap();
        // Advance the injected store clock instead of waiting for a real
        // lease timeout. The successor owns a new generation before the old
        // adapter returns or drains.
        self.store.claim(
            self.id,
            "successor".into(),
            lease.expires_at + chrono::Duration::milliseconds(1),
        )?;
        if self.fail_on_quiesce {
            self.parent_cancel.cancel();
            std::future::pending().await
        } else {
            anyhow::bail!("superseded actor failed")
        }
    }

    async fn quiesce(&self) -> Result<()> {
        anyhow::ensure!(!self.fail_on_quiesce, "superseded adapter failed to drain");
        Ok(())
    }
}

async fn assert_superseded_cleanup_preserves_successor(fail_on_quiesce: bool) {
    let mut f = Fixture::new(Actor::new(false), critic(), 1000, 120);
    let cancel = CancellationToken::new();
    f.controller.actor = Arc::new(SupersededActor {
        store: f.controller.store.clone(),
        id: f.id,
        fail_on_quiesce,
        parent_cancel: cancel.clone(),
    });
    let result = f.controller.run_pass(f.id, cancel).await.unwrap();
    assert_eq!(result.generation, 2);
    assert_eq!(result.status, RunStatus::Attempting);
    assert!(result.attention_id.is_none());
    assert_eq!(result.lease.as_ref().unwrap().holder, "successor");
    // The actor reservation and admitted review headroom both survive.
    assert_eq!(result.budget.spend().unwrap().reserved_microusd, 50);
    assert!(f.critic.calls.lock().unwrap().is_empty());
    let persisted = f.controller.store.load(f.id).unwrap();
    assert_eq!(persisted.status, RunStatus::Attempting);
    assert_eq!(persisted.lease.as_ref().unwrap().holder, "successor");
}

#[tokio::test]
async fn superseded_actor_failure_cannot_block_successor() {
    assert_superseded_cleanup_preserves_successor(false).await;
}

#[tokio::test]
async fn superseded_quiescence_failure_cannot_block_successor() {
    assert_superseded_cleanup_preserves_successor(true).await;
}

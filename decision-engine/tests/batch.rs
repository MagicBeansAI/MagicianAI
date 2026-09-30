//! End-to-end classification dispatch with controllable provider latency.
use async_trait::async_trait;
use decision_engine::Engine;
use decision_engine_contract::{batch::*, telemetry::DecisionCallStatus, *};
use magician_decision::{config::DecisionConfig, *};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

struct Delayed {
    active: AtomicUsize,
    peak: AtomicUsize,
    calls: AtomicUsize,
}
struct Active<'a>(&'a AtomicUsize);
impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
#[async_trait]
impl StructuredDecisionModel for Delayed {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("memory", "delayed")
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let n = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(n, Ordering::SeqCst);
        let _active = Active(&self.active);
        let delay = request.state.as_json()["delay"].as_u64().unwrap_or(5);
        tokio::time::sleep(Duration::from_millis(delay)).await;
        Ok(DecisionResponse {
            model: self.identity(),
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            answers: BTreeMap::from([(QuestionId::new("applicable"), Answer::Noul { noul: 0.99 })]),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 0,
            },
        })
    }
}
fn setup(concurrency: usize) -> (Engine, Arc<Delayed>) {
    setup_with(concurrency, |_| {})
}
fn setup_with(
    concurrency: usize,
    change: impl FnOnce(&mut DecisionConfig),
) -> (Engine, Arc<Delayed>) {
    let model = Arc::new(Delayed {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    });
    let pack = Pack {
        id: "test".into(),
        version: "1.0.0".into(),
        description: None,
        questions: vec![Question::Noul(NoulQuestion {
            id: QuestionId::new("applicable"),
            instructions: Instruction::Text("applies".into()),
            criteria: None,
        })],
    };
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  test:\n    pack: test\n    shadow: {enabled: true}\n",
    )
    .unwrap();
    let limits = &mut config.operations.get_mut("test").unwrap().classification;
    limits.item_concurrency = concurrency;
    limits.chunk_size = 3;
    change(&mut config);
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                "test",
                pack,
                vec![BoundModel {
                    name: "delayed".into(),
                    model: model.clone(),
                    admission: Default::default(),
                    thresholds: Some(if config.operations["test"].thresholds.is_empty() {
                        BTreeMap::from([("applicable".into(), 0.9)])
                    } else {
                        config.operations["test"].thresholds.clone()
                    }),
                }],
            )
            .build(),
    );
    (Engine::with_runtimes(config, None, Some(runtime)), model)
}
fn request(engine: &Engine, delays: &[u64]) -> DecideRequest {
    DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: "test".into(),
        state: DecisionState::from_text(""),
        choice_candidates: BTreeMap::new(),
        locality: Locality::Cloud,
        batch: BatchRequest {
            mode: Default::default(),
            reference_version: "ref1".into(),
            request_id: "r1".into(),
            expected_policy_revision: engine.operations().policy_revision,
            projection_version: "p1".into(),
            execution_budget_ms: 1000,
            context: None,
            items: delays
                .iter()
                .enumerate()
                .map(|(i, delay)| DecisionItem {
                    item_id: i.to_string(),
                    state: DecisionState::from_json(serde_json::json!({"delay":delay})),
                    choice_candidates: BTreeMap::new(),
                })
                .collect(),
        },
    }
}
#[tokio::test]
async fn partial_deadline_retains_fast_items_and_cancelled_receipts() {
    let (engine, model) = setup(2);
    let mut request = request(&engine, &[500, 5, 500, 5]);
    request.batch.execution_budget_ms = 60;
    let reply = engine.decide(request.clone()).await;
    assert!(reply.batch.matches(&request.batch));
    assert_eq!(
        reply
            .batch
            .items
            .iter()
            .map(|i| i.status)
            .collect::<Vec<_>>(),
        vec![
            ItemStatus::Cancelled,
            ItemStatus::Answered,
            ItemStatus::Cancelled,
            ItemStatus::NotStarted
        ]
    );
    assert_eq!(model.active.load(Ordering::SeqCst), 0);
    assert_eq!(reply.model_calls.len(), 3);
    assert_eq!(
        reply
            .model_calls
            .iter()
            .filter(|c| c.status == DecisionCallStatus::Cancelled)
            .count(),
        2
    );
    assert!(reply
        .model_calls
        .iter()
        .all(|c| c.batch_id.as_deref() == Some("r1") && c.item_ids.len() == 1));
}
#[tokio::test]
async fn all_windows_are_processed_and_concurrent_batches_share_limits() {
    let (engine, model) = setup(2);
    let request = request(&engine, &[5; 8]);
    let (a, b) = tokio::join!(engine.decide(request.clone()), engine.decide(request));
    assert!(a
        .batch
        .items
        .iter()
        .chain(&b.batch.items)
        .all(|i| i.status == ItemStatus::Answered));
    assert_eq!(model.calls.load(Ordering::SeqCst), 16);
    assert_eq!(model.peak.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn admission_rejects_whole_batch_before_dispatch_but_isolates_oversized_item() {
    let (engine, model) = setup(2);
    let mut oversized = request(&engine, &[5; 65]);
    assert_eq!(
        engine.decide(oversized.clone()).await.status,
        DecideStatus::Failed
    );
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
    oversized.batch.items.truncate(2);
    oversized.batch.items[0].state = DecisionState::from_text("x".repeat(3000));
    let result = engine.decide(oversized).await;
    assert_eq!(result.batch.items[0].status, ItemStatus::Failed);
    assert_eq!(result.batch.items[1].status, ItemStatus::Answered);
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn stale_policy_and_v4_host_requests_never_dispatch() {
    let (engine, model) = setup(2);
    let mut stale = request(&engine, &[5]);
    stale.batch.expected_policy_revision = "stale".into();
    let result = engine.decide(stale.clone()).await;
    assert!(!result.batch.matches(&stale.batch));
    let mut old = request(&engine, &[5]);
    old.contract_version = 4;
    assert_eq!(engine.decide(old).await.status, DecideStatus::Unbound);
    assert_eq!(model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn operator_gate_is_explicit_and_still_requires_confidence() {
    for (enabled, threshold, expected) in
        [(true, 0.9, true), (true, 1.0, false), (false, 0.9, false)]
    {
        let (engine, _) = setup_with(2, |config| {
            let op = config.operations.get_mut("test").unwrap();
            op.gate.enabled = true;
            op.allow_unqualified_gate = enabled;
            op.thresholds.insert("applicable".into(), threshold);
        });
        let reply = engine.decide(request(&engine, &[5])).await;
        let answer = reply.batch.items[0].eligible_answers.get("applicable");
        assert_eq!(answer.is_some(), expected);
        if let Some(origin) = answer {
            assert!(origin.starts_with("operator-enabled:"));
        }
    }
}

#[tokio::test]
async fn gate_switch_without_exact_review_is_never_eligible() {
    use decision_engine_contract::classification::ClassificationQualification;
    use magician_decision::config::fingerprint;
    let (engine, _) = setup_with(2, |config| {
        config.operations.get_mut("test").unwrap().gate.enabled = true;
    });
    assert!(engine.decide(request(&engine, &[5])).await.batch.items[0]
        .eligible_answers
        .is_empty());
    let (engine, _) = setup_with(2, |config| {
        let behavior = config.behavior_fingerprint(&config.operations["test"]);
        let op = config.operations.get_mut("test").unwrap();
        op.gate.enabled = true;
        op.qualifications.push(ClassificationQualification {
            question: "applicable".into(),
            output: "true".into(),
            model: "delayed".into(),
            provider: "decision:memory".into(),
            pack_version: "1.0.0".into(),
            threshold_fingerprint: fingerprint(&BTreeMap::from([("applicable", 0.9)])),
            projection_version: "p1".into(),
            reference_version: "ref1".into(),
            evidence_id: "fixture-evidence".into(),
            human_review_ref: "fixture-review-only".into(),
            behavior_fingerprint: behavior,
            score_range: None,
            consumer_mapping_version: None,
        });
    });
    let req = request(&engine, &[5]);
    assert_eq!(
        engine.decide(req.clone()).await.batch.items[0].eligible_answers["applicable"],
        "fixture-evidence"
    );
    let mut changed = req.clone();
    changed.batch.reference_version = "changed".into();
    assert!(engine.decide(changed).await.batch.items[0]
        .eligible_answers
        .is_empty());
    let mut changed = req;
    changed.batch.projection_version = "changed".into();
    assert!(engine.decide(changed).await.batch.items[0]
        .eligible_answers
        .is_empty());
}

#[tokio::test]
async fn memory_decision_model_limit_is_shared_across_operations_and_resizes_without_losing_work() {
    let model = Arc::new(Delayed {
        active: AtomicUsize::new(0),
        peak: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
    });
    let admission = Arc::new(magician_decision::admission::ModelAdmission::new(2));
    let pack = Pack {
        id: "test".into(),
        version: "1.0.0".into(),
        description: None,
        questions: vec![Question::Noul(NoulQuestion {
            id: QuestionId::new("applicable"),
            instructions: Instruction::Text("applies".into()),
            criteria: None,
        })],
    };
    let mut builder = DecisionRuntimeBuilder::new();
    for op in ["test", "second"] {
        builder = builder.bind_route(
            op,
            pack.clone(),
            vec![BoundModel {
                name: "delayed".into(),
                model: model.clone(),
                admission: admission.clone(),
                thresholds: Some(BTreeMap::from([("applicable".into(), 0.9)])),
            }],
        );
    }
    let config:DecisionConfig=serde_yaml::from_str("enabled: true\noperations:\n  test:\n    pack: test\n    shadow: {enabled: true}\n    gate: {enabled: true}\n  second:\n    pack: test\n    shadow: {enabled: true}\n    gate: {enabled: true}\n").unwrap();
    let engine = Engine::with_runtimes(config, None, Some(Arc::new(builder.build())));
    let mut a = request(&engine, &[30; 13]);
    a.batch.mode = ClassificationMode::Gate;
    let mut b = a.clone();
    b.operation = "second".into();
    b.batch.request_id = "second-request".into();
    let resize = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(model.active.load(Ordering::SeqCst), 2);
        admission.set_limit(1);
    };
    let (a, b, ()) = tokio::join!(engine.decide(a), engine.decide(b), resize);
    assert!(a
        .batch
        .items
        .iter()
        .chain(&b.batch.items)
        .all(|i| i.status == ItemStatus::Answered));
    assert_eq!(model.calls.load(Ordering::SeqCst), 26);
    assert_eq!(model.peak.load(Ordering::SeqCst), 2);
    assert_eq!(model.active.load(Ordering::SeqCst), 0);
    assert_eq!(a.model_calls.len() + b.model_calls.len(), 26);
    // Acquiring an operation slot is not a physical start when the shared
    // model remains busy with another operation (or the tool action rail).
    let held = admission.enter().await.unwrap();
    let mut queued = request(&engine, &[5]);
    queued.batch.execution_budget_ms = 40;
    let queued = engine.decide(queued).await;
    assert_eq!(queued.batch.items[0].status, ItemStatus::NotStarted);
    assert!(queued.model_calls.is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 26);
    drop(held);
}

#[tokio::test]
async fn memory_decision_expired_queue_does_not_start_provider_work() {
    let (engine, model) = setup(1);
    let mut a = request(&engine, &[300]);
    a.batch.request_id = "running".into();
    let mut b = request(&engine, &[5]);
    b.batch.execution_budget_ms = 40;
    b.batch.request_id = "queued".into();
    let queued = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        engine.decide(b).await
    };
    let (a, b) = tokio::join!(engine.decide(a), queued);
    assert_eq!(a.batch.items[0].status, ItemStatus::Answered);
    assert_eq!(b.batch.items[0].status, ItemStatus::NotStarted);
    assert!(b.model_calls.is_empty());
    assert_eq!(model.calls.load(Ordering::SeqCst), 1);
}

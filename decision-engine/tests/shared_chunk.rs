use decision_engine::Engine;
use decision_engine_contract::{batch::*, *};
use magician_decision::{
    adapters::MemoryDecisionModel,
    config::{BatchStrategy, DecisionConfig},
    *,
};
use std::{collections::BTreeMap, sync::Arc};

struct ChunkRejectModel;
#[async_trait::async_trait]
impl StructuredDecisionModel for ChunkRejectModel {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("memory", "recover")
    }
    fn capabilities(&self) -> ModelCapabilities {
        ModelCapabilities::default()
    }
    async fn evaluate(&self, request: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        if request.questions.len() > 2 {
            return Err(DecisionError::DispatchFull);
        }
        Ok(DecisionResponse {
            model: self.identity(),
            pack_id: request.pack_id,
            pack_version: request.pack_version,
            answers: request
                .questions
                .iter()
                .map(|q| (q.id().clone(), Answer::Noul { noul: 0.99 }))
                .collect(),
            usage: Usage {
                input_tokens: 10,
                output_tokens: 2,
            },
        })
    }
}

#[tokio::test]
async fn shared_context_is_sent_once_and_demultiplexed_with_one_physical_receipt() {
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  memory_applicability:\n    pack: memory_applicability\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    batch_strategy: shared_chunk\n",
    ).unwrap();
    let op = config.operations.get_mut("memory_applicability").unwrap();
    assert_eq!(op.batch_strategy, BatchStrategy::SharedChunk);
    op.classification.chunk_size = 2;
    let model = Arc::new(
        MemoryDecisionModel::new("memory", "chunked")
            .with_answer("c0q0", Answer::Noul { noul: 0.99 })
            .with_answer("c0q1", Answer::Noul { noul: 0.99 })
            .with_answer("c1q0", Answer::Noul { noul: 0.99 })
            .with_answer("c1q1", Answer::Noul { noul: 0.99 }),
    );
    let pack = PackStore::new(None)
        .load("memory_applicability", "1.0.0")
        .unwrap();
    let route = vec![
        BoundModel {
            name: "chunked".into(),
            model: model.clone(),
            admission: Default::default(),
            thresholds: Some(BTreeMap::from([
                ("applicable".into(), 0.9),
                ("load_bearing".into(), 0.9),
            ])),
        },
        BoundModel {
            name: "small-backup".into(),
            model: Arc::new(
                MemoryDecisionModel::new("memory", "small-backup").with_capabilities(
                    ModelCapabilities {
                        max_questions: Some(2),
                        ..Default::default()
                    },
                ),
            ),
            admission: Default::default(),
            thresholds: Some(BTreeMap::from([
                ("applicable".into(), 0.9),
                ("load_bearing".into(), 0.9),
            ])),
        },
    ];
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route("memory_applicability", pack, route)
            .build(),
    );
    let engine = Engine::with_runtimes(config, None, Some(runtime));
    let discovered = engine.operations();
    let policy = &discovered
        .operations
        .iter()
        .find(|p| p.name == "memory_applicability")
        .unwrap()
        .classification;
    assert_eq!(policy.batch_strategy, BatchStrategy::SharedChunk);
    assert_eq!(
        policy.shared_chunk_transform_version,
        Some(magician_decision::config::SHARED_CHUNK_TRANSFORM_VERSION)
    );
    let request = DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: "memory_applicability".into(),
        state: DecisionState::from_text(""),
        choice_candidates: Default::default(),
        locality: Locality::Cloud,
        batch: BatchRequest {
            mode: ClassificationMode::Gate,
            reference_version: "ref".into(),
            request_id: "batch".into(),
            expected_policy_revision: engine.operations().policy_revision,
            projection_version: "projection".into(),
            execution_budget_ms: 1000,
            context: Some(DecisionState::from_json(
                serde_json::json!({"goal":"one shared goal"}),
            )),
            items: vec!["a", "b"]
                .into_iter()
                .map(|id| DecisionItem {
                    item_id: id.into(),
                    state: DecisionState::from_json(serde_json::json!({"memory":id})),
                    choice_candidates: Default::default(),
                })
                .collect(),
        },
    };
    let response = engine.decide(request.clone()).await;
    assert!(response.batch.matches(&request.batch));
    assert_eq!(response.model_calls.len(), 1);
    assert_eq!(response.model_calls[0].item_ids, vec!["a", "b"]);
    for item in &response.batch.items {
        assert_eq!(item.status, ItemStatus::Answered);
        assert_eq!(item.eligible_answers.len(), 2);
        assert!(item
            .response
            .as_ref()
            .unwrap()
            .answers
            .contains_key(&QuestionId::new("applicable")));
    }
    let physical = model.last_request().unwrap();
    assert_eq!(physical.questions.len(), 4);
    assert_eq!(
        physical.state.as_json()["context"]["goal"],
        "one shared goal"
    );
    assert_eq!(
        physical.state.as_json()["items"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test]
async fn a_chunk_failure_retains_its_receipt_and_recovers_only_unresolved_items() {
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  memory_applicability:\n    pack: memory_applicability\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    batch_strategy: shared_chunk\n",
    ).unwrap();
    config
        .operations
        .get_mut("memory_applicability")
        .unwrap()
        .classification
        .chunk_size = 2;
    // A temporary shared-request admission failure has a paid receipt but
    // does not cool the model. Each canonical item gets one recovery attempt.
    let model = Arc::new(ChunkRejectModel);
    let pack = PackStore::new(None)
        .load("memory_applicability", "1.0.0")
        .unwrap();
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                "memory_applicability",
                pack,
                vec![BoundModel {
                    name: "recover".into(),
                    model,
                    admission: Default::default(),
                    thresholds: Some(BTreeMap::from([
                        ("applicable".into(), 0.9),
                        ("load_bearing".into(), 0.9),
                    ])),
                }],
            )
            .build(),
    );
    let engine = Engine::with_runtimes(config, None, Some(runtime));
    let request = DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: "memory_applicability".into(),
        state: DecisionState::from_text(""),
        choice_candidates: Default::default(),
        locality: Locality::Cloud,
        batch: BatchRequest {
            mode: ClassificationMode::Gate,
            reference_version: "ref".into(),
            request_id: "recovery".into(),
            expected_policy_revision: engine.operations().policy_revision,
            projection_version: "projection".into(),
            execution_budget_ms: 1000,
            context: Some(DecisionState::from_text("shared")),
            items: vec!["a", "b"]
                .into_iter()
                .map(|id| DecisionItem {
                    item_id: id.into(),
                    state: DecisionState::from_text(id),
                    choice_candidates: Default::default(),
                })
                .collect(),
        },
    };
    let result = engine.decide(request).await;
    assert_eq!(
        result.model_calls.len(),
        3,
        "one failed chunk and two paid recoveries: {result:?}"
    );
    assert_eq!(result.model_calls[0].item_ids, vec!["a", "b"]);
    assert_ne!(
        result.model_calls[0].status,
        decision_engine_contract::telemetry::DecisionCallStatus::Succeeded
    );
    assert!(result
        .batch
        .items
        .iter()
        .all(|item| item.status == ItemStatus::Answered && item.eligible_answers.len() == 2));
    let ids: std::collections::BTreeSet<_> = result
        .model_calls
        .iter()
        .map(|call| &call.call_id)
        .collect();
    assert_eq!(
        ids.len(),
        result.model_calls.len(),
        "physical attempts have distinct identities"
    );
}

#[tokio::test]
async fn low_confidence_in_one_item_recovers_without_overwriting_a_confident_neighbor() {
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  memory_applicability:\n    pack: memory_applicability\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    batch_strategy: shared_chunk\n",
    ).unwrap();
    config
        .operations
        .get_mut("memory_applicability")
        .unwrap()
        .classification
        .chunk_size = 2;
    let model = Arc::new(
        MemoryDecisionModel::new("memory", "partial")
            .with_answer("c0q0", Answer::Noul { noul: 0.99 })
            .with_answer("c0q1", Answer::Noul { noul: 0.99 })
            .with_answer("c1q0", Answer::Noul { noul: 0.55 })
            .with_answer("c1q1", Answer::Noul { noul: 0.55 })
            .with_answer("applicable", Answer::Noul { noul: 0.01 })
            .with_answer("load_bearing", Answer::Noul { noul: 0.01 }),
    );
    let pack = PackStore::new(None)
        .load("memory_applicability", "1.0.0")
        .unwrap();
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                "memory_applicability",
                pack,
                vec![BoundModel {
                    name: "partial".into(),
                    model,
                    admission: Default::default(),
                    thresholds: Some(BTreeMap::from([
                        ("applicable".into(), 0.9),
                        ("load_bearing".into(), 0.9),
                    ])),
                }],
            )
            .build(),
    );
    let engine = Engine::with_runtimes(config, None, Some(runtime));
    let result = engine
        .decide(DecideRequest {
            contract_version: CONTRACT_VERSION,
            operation: "memory_applicability".into(),
            state: DecisionState::from_text(""),
            choice_candidates: Default::default(),
            locality: Locality::Cloud,
            batch: BatchRequest {
                mode: ClassificationMode::Gate,
                reference_version: "ref".into(),
                request_id: "partial".into(),
                expected_policy_revision: engine.operations().policy_revision,
                projection_version: "projection".into(),
                execution_budget_ms: 1000,
                context: Some(DecisionState::from_text("shared")),
                items: vec!["a", "b"]
                    .into_iter()
                    .map(|id| DecisionItem {
                        item_id: id.into(),
                        state: DecisionState::from_text(id),
                        choice_candidates: Default::default(),
                    })
                    .collect(),
            },
        })
        .await;
    assert_eq!(result.model_calls.len(), 2);
    assert_eq!(result.model_calls[0].item_ids, vec!["a", "b"]);
    assert_eq!(result.model_calls[1].item_ids, vec!["b"]);
    assert_eq!(
        result.batch.items[0].response.as_ref().unwrap().answers[&QuestionId::new("applicable")]
            .noul_value(),
        Some(0.99)
    );
    assert_eq!(
        result.batch.items[1].response.as_ref().unwrap().answers[&QuestionId::new("applicable")]
            .noul_value(),
        Some(0.01)
    );
    assert!(result
        .batch
        .items
        .iter()
        .all(|item| item.eligible_answers.len() == 2));
}

#[tokio::test]
async fn oversized_item_is_rejected_before_any_shared_model_call() {
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  memory_applicability:\n    pack: memory_applicability\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    batch_strategy: shared_chunk\n",
    ).unwrap();
    config
        .operations
        .get_mut("memory_applicability")
        .unwrap()
        .classification
        .chunk_size = 3;
    let model = Arc::new(
        MemoryDecisionModel::new("memory", "bounded")
            .with_answer("c0q0", Answer::Noul { noul: 0.99 })
            .with_answer("c0q1", Answer::Noul { noul: 0.99 })
            .with_answer("applicable", Answer::Noul { noul: 0.99 })
            .with_answer("load_bearing", Answer::Noul { noul: 0.99 }),
    );
    let pack = PackStore::new(None)
        .load("memory_applicability", "1.0.0")
        .unwrap();
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                "memory_applicability",
                pack,
                vec![BoundModel {
                    name: "bounded".into(),
                    model,
                    admission: Default::default(),
                    thresholds: Some(BTreeMap::from([
                        ("applicable".into(), 0.9),
                        ("load_bearing".into(), 0.9),
                    ])),
                }],
            )
            .build(),
    );
    let engine = Engine::with_runtimes(config, None, Some(runtime));
    let result = engine
        .decide(DecideRequest {
            contract_version: CONTRACT_VERSION,
            operation: "memory_applicability".into(),
            state: DecisionState::from_text(""),
            choice_candidates: Default::default(),
            locality: Locality::Cloud,
            batch: BatchRequest {
                mode: ClassificationMode::Gate,
                reference_version: "ref".into(),
                request_id: "oversized".into(),
                expected_policy_revision: engine.operations().policy_revision,
                projection_version: "projection".into(),
                execution_budget_ms: 1000,
                context: None,
                items: vec!["a".into(), "x".repeat(4000), "b".into()]
                    .into_iter()
                    .enumerate()
                    .map(|(i, body): (usize, String)| DecisionItem {
                        item_id: i.to_string(),
                        state: DecisionState::from_text(body),
                        choice_candidates: Default::default(),
                    })
                    .collect(),
            },
        })
        .await;
    assert_eq!(result.batch.items[1].status, ItemStatus::Failed);
    assert_eq!(result.batch.items[0].status, ItemStatus::Answered);
    assert_eq!(result.batch.items[2].status, ItemStatus::Answered);
    assert_eq!(result.model_calls.len(), 2);
    assert!(result
        .model_calls
        .iter()
        .all(|call| !call.item_ids.contains(&"1".to_string())));
}

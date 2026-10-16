use decision_engine::Engine;
use decision_engine_contract::{batch::*, *};
use magician_decision::{adapters::MemoryDecisionModel, config::DecisionConfig, *};
use std::{collections::BTreeMap, sync::Arc};

#[tokio::test]
async fn batch_fallback_qualifies_local_identity_and_meters_both_calls() {
    let config: DecisionConfig = serde_yaml::from_str("enabled: true\noperations:\n  memory_applicability:\n    pack: memory_applicability\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n").unwrap();
    let pack = PackStore::from_roots(vec![])
        .load("memory_applicability", "1.0.0")
        .unwrap();
    let route = [
        ("typesafe", "primary", 0.6, 0.7),
        ("kev-mlx", "local", 0.99, 0.9),
    ]
    .into_iter()
    .map(|(adapter, name, noul, threshold)| BoundModel {
        admission: Default::default(),
        name: name.into(),
        model: Arc::new(
            MemoryDecisionModel::new(adapter, name)
                .with_answer("applicable", Answer::Noul { noul })
                .with_answer("load_bearing", Answer::Noul { noul }),
        ),
        thresholds: Some(BTreeMap::from([
            ("applicable".into(), threshold),
            ("load_bearing".into(), threshold),
        ])),
    })
    .collect();
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route("memory_applicability", pack, route)
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
            request_id: "review".into(),
            reference_version: "review".into(),
            projection_version: "review".into(),
            expected_policy_revision: engine.operations().policy_revision,
            execution_budget_ms: 500,
            context: None,
            items: vec![DecisionItem {
                item_id: "one".into(),
                state: DecisionState::from_text("evidence"),
                choice_candidates: Default::default(),
            }],
        },
    };
    let reply = engine.decide(request).await;
    let item = &reply.batch.items[0];
    assert_eq!(item.status, ItemStatus::Answered);
    assert_eq!(item.response.as_ref().unwrap().model.model, "local");
    assert_eq!(item.eligible_answers.len(), 2);
    assert_eq!(item.thresholds.as_ref().unwrap()["applicable"], 0.9);
    assert_eq!(reply.model_calls.len(), 2);
    assert!(!reply.model_calls[0].local);
    assert!(reply.model_calls[1].local);
    assert!(reply
        .model_calls
        .iter()
        .all(|c| c.item_ids == ["one"] && c.batch_id.as_deref() == Some("review")));
}

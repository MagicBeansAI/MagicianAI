use decision_engine::Engine;
use decision_engine_contract::{batch::*, classification::ClassificationQualification, *};
use magician_decision::{
    adapters::MemoryDecisionModel,
    config::{fingerprint, DecisionConfig},
    *,
};
use std::{collections::BTreeMap, sync::Arc};

fn choice(value: &str) -> Answer {
    let id = OptionId::new(value);
    Answer::Choice {
        choice: id.clone(),
        probabilities: BTreeMap::from([(id, 1.0)]),
        confidence: 0.99,
    }
}

fn engine(value: &str, qualified: bool) -> Engine {
    let mut config: DecisionConfig = serde_yaml::from_str(
        "enabled: true\noperations:\n  memory_conflict_review:\n    pack: memory_conflict_review\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    restricted_outputs: {resolution: [replace_existing, keep_existing]}\n",
    ).unwrap();
    if qualified {
        // The behavior identity excludes qualifications themselves.
        let behavior = config.behavior_fingerprint(&config.operations["memory_conflict_review"]);
        let op = config.operations.get_mut("memory_conflict_review").unwrap();
        op.qualifications.push(ClassificationQualification {
            question: "resolution".into(),
            output: value.into(),
            model: "review".into(),
            provider: "decision:memory".into(),
            pack_version: "1.0.0".into(),
            threshold_fingerprint: fingerprint(&BTreeMap::from([(
                String::from("resolution"),
                0.9,
            )])),
            projection_version: "projection".into(),
            reference_version: "reference".into(),
            evidence_id: "reviewed-fixture".into(),
            human_review_ref: "test".into(),
            behavior_fingerprint: behavior,
            score_range: None,
            consumer_mapping_version: None,
        });
    }
    let pack = PackStore::new(None)
        .load("memory_conflict_review", "1.0.0")
        .unwrap();
    let model = Arc::new(
        MemoryDecisionModel::new("memory", "review").with_answer("resolution", choice(value)),
    );
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                "memory_conflict_review",
                pack,
                vec![BoundModel {
                    name: "review".into(),
                    model,
                    admission: Default::default(),
                    thresholds: Some(BTreeMap::from([("resolution".into(), 0.9)])),
                }],
            )
            .build(),
    );
    Engine::with_runtimes(config, None, Some(runtime))
}

async fn result(value: &str, qualified: bool) -> DecisionItemResult {
    let engine = engine(value, qualified);
    let request = DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: "memory_conflict_review".into(),
        state: DecisionState::from_text(""),
        choice_candidates: Default::default(),
        locality: Locality::Cloud,
        batch: BatchRequest {
            mode: ClassificationMode::Gate,
            reference_version: "reference".into(),
            request_id: "batch".into(),
            expected_policy_revision: engine.operations().policy_revision,
            projection_version: "projection".into(),
            execution_budget_ms: 1000,
            context: None,
            items: vec![DecisionItem {
                item_id: "item".into(),
                state: DecisionState::from_text("evidence"),
                choice_candidates: Default::default(),
            }],
        },
    };
    engine.decide(request).await.batch.items.remove(0)
}

#[tokio::test]
async fn both_supersession_directions_require_reviewed_qualification() {
    for value in ["replace_existing", "keep_existing"] {
        let unqualified = result(value, false).await;
        assert!(unqualified.response.is_some()); // observable, not authorized
        assert!(unqualified.eligible_answers.is_empty(), "{value}");
        let qualified = result(value, true).await;
        assert_eq!(qualified.eligible_answers["resolution"], "reviewed-fixture");
    }
    assert!(
        result("keep_both", false).await.eligible_answers["resolution"]
            .starts_with("operator-enabled:")
    );
}

async fn classified(
    operation: &str,
    restricted: &str,
    answers: &[(&str, Answer)],
    qualification: Option<(&str, &str, Option<[f64; 2]>, bool)>,
) -> DecisionItemResult {
    let config_text = format!(
        "enabled: true\noperations:\n  {operation}:\n    pack: {operation}\n    gate: {{enabled: true}}\n    allow_unqualified_gate: true\n    restricted_outputs: {restricted}\n"
    );
    let mut config: DecisionConfig = serde_yaml::from_str(&config_text).unwrap();
    if let Some((question, output, range, stale)) = qualification {
        let behavior = config.behavior_fingerprint(&config.operations[operation]);
        config
            .operations
            .get_mut(operation)
            .unwrap()
            .qualifications
            .push(ClassificationQualification {
                question: question.into(),
                output: output.into(),
                model: "review".into(),
                provider: "decision:memory".into(),
                pack_version: "1.0.0".into(),
                threshold_fingerprint: fingerprint(
                    &answers
                        .iter()
                        .map(|(id, _)| (String::from(*id), 0.9_f64))
                        .collect::<BTreeMap<String, f64>>(),
                ),
                projection_version: "projection".into(),
                reference_version: "reference".into(),
                evidence_id: "reviewed-fixture".into(),
                human_review_ref: "test".into(),
                behavior_fingerprint: if stale { "stale".into() } else { behavior },
                score_range: range,
                consumer_mapping_version: range.map(|_| "projection".into()),
            });
    }
    let pack = PackStore::new(None).load(operation, "1.0.0").unwrap();
    let mut model = MemoryDecisionModel::new("memory", "review");
    let mut thresholds = BTreeMap::new();
    for (id, answer) in answers {
        model = model.with_answer(*id, answer.clone());
        thresholds.insert((*id).into(), 0.9);
    }
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                operation,
                pack,
                vec![BoundModel {
                    name: "review".into(),
                    model: Arc::new(model),
                    admission: Default::default(),
                    thresholds: Some(thresholds),
                }],
            )
            .build(),
    );
    let engine = Engine::with_runtimes(config, None, Some(runtime));
    let request = DecideRequest {
        contract_version: CONTRACT_VERSION,
        operation: operation.into(),
        state: DecisionState::from_text(""),
        choice_candidates: Default::default(),
        locality: Locality::Cloud,
        batch: BatchRequest {
            mode: ClassificationMode::Gate,
            reference_version: "reference".into(),
            request_id: "batch".into(),
            expected_policy_revision: engine.operations().policy_revision,
            projection_version: "projection".into(),
            execution_budget_ms: 1000,
            context: None,
            items: vec![DecisionItem {
                item_id: "item".into(),
                state: DecisionState::from_text("evidence"),
                choice_candidates: Default::default(),
            }],
        },
    };
    engine.decide(request).await.batch.items.remove(0)
}

#[tokio::test]
async fn restricted_noul_requires_exact_current_review_for_true_and_false() {
    for (value, output) in [(0.99, "true"), (0.01, "false")] {
        let answers = [
            ("kind", choice("fact")),
            ("relation", choice("coexist")),
            ("incoming_coverage", choice("full")),
            ("same_subject_and_aspect", Answer::Noul { noul: value }),
            ("same_context", Answer::Noul { noul: 0.99 }),
            ("explicit_correction", Answer::Noul { noul: 0.99 }),
        ];
        let restricted = "{same_subject_and_aspect: [\"true\", \"false\"]}";
        let unqualified = classified("memory_lifecycle_relation", restricted, &answers, None).await;
        assert!(unqualified.response.is_some());
        assert!(!unqualified
            .eligible_answers
            .contains_key("same_subject_and_aspect"));
        assert!(unqualified.eligible_answers.contains_key("relation"));
        let stale = classified(
            "memory_lifecycle_relation",
            restricted,
            &answers,
            Some(("same_subject_and_aspect", output, None, true)),
        )
        .await;
        assert!(!stale
            .eligible_answers
            .contains_key("same_subject_and_aspect"));
        let qualified = classified(
            "memory_lifecycle_relation",
            restricted,
            &answers,
            Some(("same_subject_and_aspect", output, None, false)),
        )
        .await;
        assert_eq!(
            qualified.eligible_answers["same_subject_and_aspect"],
            "reviewed-fixture"
        );
    }
}

#[tokio::test]
async fn restricted_score_requires_matching_reviewed_range() {
    let answers = [
        ("classification", choice("high_signal")),
        ("priority", choice("high")),
        (
            "score",
            Answer::Score {
                score: 4.0,
                probabilities: vec![0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                confidence: 0.99,
            },
        ),
    ];
    let restricted = "{score: [score]}";
    let unqualified = classified("memory_episode_quality", restricted, &answers, None).await;
    assert!(unqualified.response.is_some());
    assert!(!unqualified.eligible_answers.contains_key("score"));
    assert!(unqualified.eligible_answers.contains_key("classification"));
    let outside = classified(
        "memory_episode_quality",
        restricted,
        &answers,
        Some(("score", "score", Some([0.0, 3.0]), false)),
    )
    .await;
    assert!(!outside.eligible_answers.contains_key("score"));
    let inside = classified(
        "memory_episode_quality",
        restricted,
        &answers,
        Some(("score", "score", Some([4.0, 5.0]), false)),
    )
    .await;
    assert_eq!(inside.eligible_answers["score"], "reviewed-fixture");
}

#[tokio::test]
async fn restricted_backup_answer_uses_its_own_reviewed_model_identity() {
    for reviewed_model in ["primary", "backup"] {
        let mut config: DecisionConfig = serde_yaml::from_str(
            "enabled: true\noperations:\n  memory_conflict_review:\n    pack: memory_conflict_review\n    gate: {enabled: true}\n    allow_unqualified_gate: true\n    restricted_outputs: {resolution: [replace_existing]}\n",
        ).unwrap();
        let behavior = config.behavior_fingerprint(&config.operations["memory_conflict_review"]);
        config
            .operations
            .get_mut("memory_conflict_review")
            .unwrap()
            .qualifications
            .push(ClassificationQualification {
                question: "resolution".into(),
                output: "replace_existing".into(),
                model: reviewed_model.into(),
                provider: "decision:memory".into(),
                pack_version: "1.0.0".into(),
                threshold_fingerprint: fingerprint(&BTreeMap::from([(
                    "resolution".to_owned(),
                    0.9,
                )])),
                projection_version: "projection".into(),
                reference_version: "reference".into(),
                evidence_id: "reviewed-backup".into(),
                human_review_ref: "test".into(),
                behavior_fingerprint: behavior,
                score_range: None,
                consumer_mapping_version: None,
            });
        let pack = PackStore::new(None)
            .load("memory_conflict_review", "1.0.0")
            .unwrap();
        let uncertain = Answer::Choice {
            choice: OptionId::new("replace_existing"),
            probabilities: BTreeMap::from([
                (OptionId::new("replace_existing"), 0.6),
                (OptionId::new("keep_both"), 0.4),
            ]),
            confidence: 0.6,
        };
        let route = vec![
            BoundModel {
                name: "primary".into(),
                model: Arc::new(
                    MemoryDecisionModel::new("memory", "primary")
                        .with_answer("resolution", uncertain),
                ),
                admission: Default::default(),
                thresholds: Some(BTreeMap::from([("resolution".into(), 0.9)])),
            },
            BoundModel {
                name: "backup".into(),
                model: Arc::new(
                    MemoryDecisionModel::new("memory", "backup")
                        .with_answer("resolution", choice("replace_existing")),
                ),
                admission: Default::default(),
                thresholds: Some(BTreeMap::from([("resolution".into(), 0.9)])),
            },
        ];
        let runtime = Arc::new(
            DecisionRuntimeBuilder::new()
                .bind_route("memory_conflict_review", pack, route)
                .build(),
        );
        let engine = Engine::with_runtimes(config, None, Some(runtime));
        let request = DecideRequest {
            contract_version: CONTRACT_VERSION,
            operation: "memory_conflict_review".into(),
            state: DecisionState::from_text(""),
            choice_candidates: Default::default(),
            locality: Locality::Cloud,
            batch: BatchRequest {
                mode: ClassificationMode::Gate,
                reference_version: "reference".into(),
                request_id: "fallback".into(),
                expected_policy_revision: engine.operations().policy_revision,
                projection_version: "projection".into(),
                execution_budget_ms: 1000,
                context: None,
                items: vec![DecisionItem {
                    item_id: "item".into(),
                    state: DecisionState::from_text("evidence"),
                    choice_candidates: Default::default(),
                }],
            },
        };
        let response = engine.decide(request).await;
        let item = &response.batch.items[0];
        assert_eq!(item.response.as_ref().unwrap().model.model, "backup");
        assert_eq!(response.model_calls.len(), 2);
        assert_eq!(
            item.eligible_answers.contains_key("resolution"),
            reviewed_model == "backup"
        );
    }
}

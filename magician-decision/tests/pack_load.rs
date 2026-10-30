//! Pack loading, candidate injection, validation, and runtime binding —
//! the whole offline path a host exercises before any network adapter
//! exists.

use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::primitives::{OptionId, Question, QuestionId};
use magician_decision::request::{set_choice_candidates, Answer, DecisionState};
use magician_decision::{DecisionError, DecisionRuntimeBuilder, PackStore};
use std::collections::BTreeMap;
use std::path::Path;

fn fixture_store() -> PackStore {
    PackStore::from_roots(vec![
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/decision_packs")
    ])
}

#[test]
fn loads_fixture_pack_and_builds_request() {
    let store = fixture_store();
    let pack = store.load("test_judge", "1.0.0").expect("pack loads");
    assert_eq!(pack.id, "test_judge");
    assert_eq!(pack.version, "1.0.0");
    assert_eq!(pack.questions.len(), 3);

    let request = pack.to_request("test_judge", DecisionState::from_text("state"));
    assert_eq!(request.operation, "test_judge");
    assert_eq!(request.pack_version, "1.0.0");
    assert_eq!(request.questions.len(), 3);
}

#[test]
fn missing_pack_is_pack_missing_not_a_panic() {
    let store = fixture_store();
    let err = store.load("absent", "9.9.9").expect_err("must fail");
    assert!(matches!(err, DecisionError::PackMissing(_)));
}

#[test]
fn injects_dynamic_choice_candidates() {
    let store = fixture_store();
    let pack = store.load("test_judge", "1.0.0").expect("pack loads");
    let mut request = pack.to_request("test_judge", DecisionState::from_text("state"));

    let candidates = vec![
        (OptionId::new("@e12"), "button 'Search'".to_string()),
        (OptionId::new("@e54"), "link 'Flights'".to_string()),
    ];
    set_choice_candidates(&mut request, &QuestionId::new("next_element"), &candidates)
        .expect("injection works");

    match request.question(&QuestionId::new("next_element")) {
        Some(Question::Choice(choice)) => {
            let ids: Vec<&str> = choice.criteria.keys().map(|o| o.as_str()).collect();
            assert_eq!(ids, vec!["@e12", "@e54"]);
            let criteria = choice
                .criteria
                .get(&OptionId::new("@e12"))
                .expect("candidate present");
            assert!(criteria.what().contains("Search"));
            // The label alone: the instruction rides `instructions` once,
            // not once per option.
            assert_eq!(criteria.what(), "button 'Search'");
        },
        other => panic!("next_element should stay a Choice, got {other:?}"),
    }
}
#[tokio::test]
async fn runtime_unbound_is_off_and_bound_evaluates() {
    let store = fixture_store();
    let pack = store.load("test_judge", "1.0.0").expect("pack loads");
    let model = MemoryDecisionModel::new("memory", "scripted")
        .with_answer("page_settled", Answer::Noul { noul: 0.97 })
        .with_answer(
            "next_element",
            Answer::Choice {
                choice: OptionId::new("@e12"),
                probabilities: BTreeMap::from([(OptionId::new("@e12"), 0.91)]),
                confidence: 0.9,
            },
        )
        .with_answer(
            "next_verb",
            Answer::Choice {
                choice: OptionId::new("click"),
                probabilities: BTreeMap::from([(OptionId::new("click"), 0.95)]),
                confidence: 0.93,
            },
        );

    let idle = DecisionRuntimeBuilder::new().build();
    assert!(!idle.operation_bound("test_judge"));
    let err = idle
        .evaluate("test_judge", DecisionState::from_text("state"))
        .await
        .expect_err("unbound is an error");
    assert!(matches!(err, DecisionError::OperationUnbound(_)));

    let runtime = DecisionRuntimeBuilder::new()
        .bind("test_judge", pack, std::sync::Arc::new(model))
        .build();
    assert!(runtime.operation_bound("test_judge"));
    // The dynamic-candidate path: build, inject, dispatch. Evaluating the
    // pack without injecting candidates must fail validation (the Choice
    // has no declared options) — that is the contract, not an inconvenience.
    let mut request = runtime
        .build_request("test_judge", DecisionState::from_text("state"))
        .expect("request builds");
    let raw_err = runtime
        .evaluate_request(request.clone())
        .await
        .expect_err("uninjected dynamic Choice must be refused");
    assert!(matches!(raw_err, DecisionError::InvalidResponse(_)));
    set_choice_candidates(
        &mut request,
        &QuestionId::new("next_element"),
        &[
            (OptionId::new("@e12"), "button 'Search'".to_string()),
            (OptionId::new("@e54"), "link 'Flights'".to_string()),
        ],
    )
    .expect("injection works");
    let response = runtime.evaluate_request(request).await.expect("evaluates");
    assert_eq!(response.model.model, "scripted");
    assert_eq!(response.answers.len(), 3);
}

#[tokio::test]
async fn runtime_rejects_undeclared_option_even_when_adapter_returns_it() {
    let store = fixture_store();
    let pack = store.load("test_judge", "1.0.0").expect("pack loads");
    // Scripted with an option the request never declared (the injection
    // declares a different candidate): the runtime must refuse, not coerce.
    let model = MemoryDecisionModel::new("memory", "scripted")
        .with_answer("page_settled", Answer::Noul { noul: 0.9 })
        .with_answer(
            "next_element",
            Answer::Choice {
                choice: OptionId::new("@e999-not-declared"),
                probabilities: BTreeMap::from([(OptionId::new("@e999-not-declared"), 1.0)]),
                confidence: 0.99,
            },
        )
        .with_answer(
            "next_verb",
            Answer::Choice {
                choice: OptionId::new("click"),
                probabilities: BTreeMap::from([(OptionId::new("click"), 1.0)]),
                confidence: 0.99,
            },
        );
    let runtime = DecisionRuntimeBuilder::new()
        .bind("test_judge", pack, std::sync::Arc::new(model))
        .build();
    let mut request = runtime
        .build_request("test_judge", DecisionState::from_text("state"))
        .unwrap();
    set_choice_candidates(
        &mut request,
        &QuestionId::new("next_element"),
        &[(OptionId::new("@e12"), "Allowed button".into())],
    )
    .unwrap();
    let err = runtime
        .evaluate_request(request)
        .await
        .expect_err("must refuse");
    assert!(matches!(err, DecisionError::UnknownOption { .. }));
}

#[test]
fn channel_classify_pack_ships_with_the_closed_product_vocabulary() {
    let pack = PackStore::from_roots(vec![])
        .load("channel_classify", "1.0.0")
        .unwrap();
    assert_eq!(pack.questions.len(), 13);
    for (id, options) in [
        ("lane", vec!["needs_reply", "follow_up", "fyi", "no_action"]),
        (
            "follow_up_kind",
            vec![
                "needs_reply",
                "owner_owes",
                "other_owes",
                "waiting_on",
                "check_back",
                "schedule",
                "none",
            ],
        ),
        (
            "action_owner",
            vec!["owner", "counterparty", "agent", "unknown"],
        ),
        ("urgency", vec!["low", "normal", "high"]),
    ] {
        let value = serde_json::to_value(
            pack.questions
                .iter()
                .find(|q| q.id().as_str() == id)
                .unwrap(),
        )
        .unwrap();
        let actual = value["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, options.into_iter().collect(), "{id}");
    }
    assert!(pack
        .questions
        .iter()
        .any(|q| q.id().as_str() == "repeat_of_recently_handled"));
}

#[test]
fn channel_classify_seed_matches_the_owner_approved_rollout() {
    let config: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    config.validate().unwrap();
    let op = &config.operations["channel_classify"];
    assert!(!op.shadow.enabled);
    assert!(op.gate.enabled && op.allow_unqualified_gate);
    assert_eq!(op.pack_version, "1.1.0");
    assert_eq!(op.thresholds_by_model["jev"].len(), 5);
    for question in [
        "lane",
        "follow_up_kind",
        "action_owner",
        "urgency",
        "repeat_of_recently_handled",
    ] {
        assert_eq!(op.thresholds_by_model["jev"][question], 0.7);
    }
    assert_eq!(op.observation.gate_sample_rate, 0.1);
    assert_eq!(op.classification.max_items, 4);
    assert!(op.sees_body && op.allow_remote_when_local);
}

#[test]
fn channel_classify_revised_pack_preserves_vocabulary_and_has_distinct_identity() {
    let store = PackStore::from_roots(vec![]);
    let old = store.load("channel_classify", "1.0.0").unwrap();
    let new = store.load("channel_classify", "1.1.0").unwrap();
    assert_ne!(old, new);
    for (before, after) in old.questions.iter().zip(&new.questions) {
        let before = serde_json::to_value(before).unwrap();
        let after = serde_json::to_value(after).unwrap();
        assert_eq!(before["id"], after["id"]);
        assert_eq!(before["kind"], after["kind"]);
        assert_eq!(
            before["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>(),
            after["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .collect::<Vec<_>>()
        );
    }
}

#[test]
fn memory_pack_is_available_without_repository_or_runtime_data_roots() {
    let pack = PackStore::from_roots(vec![])
        .load("memory_applicability", "1.0.0")
        .unwrap();
    assert_eq!(pack.questions.len(), 2);
    assert!(pack
        .questions
        .iter()
        .any(|q| q.id().as_str() == "applicable"));
    assert!(PackStore::from_roots(vec![])
        .load("memory_applicability", "2.0.0")
        .is_err());
}

#[test]
fn memory_operation_packs_are_versioned_and_embedded() {
    let store = PackStore::from_roots(vec![]);
    for (id, questions) in [
        ("memory_utility_review", 1),
        ("memory_episode_quality", 3),
        ("memory_conflict_review", 1),
        ("evidence_promote", 2),
        ("memory_lifecycle_relation", 6),
        ("procedure_feedback", 2),
        ("memory_connection_gate", 2),
    ] {
        let pack = store.load(id, "1.0.0").unwrap();
        assert_eq!(pack.id, id);
        assert_eq!(pack.questions.len(), questions);
        assert!(store.load(id, "2.0.0").is_err());
    }
}

#[test]
fn memory_owner_recovery_packs_ship_without_runtime_files_and_preserve_answer_schema() {
    let store = PackStore::from_roots(vec![]);
    for id in [
        "evidence_promote",
        "memory_connection_gate",
        "memory_lifecycle_relation",
    ] {
        let old = store.load(id, "1.0.0").unwrap();
        let new = store.load(id, "1.1.0").unwrap();
        assert_ne!(old.questions, new.questions);
        assert_eq!(
            old.questions
                .iter()
                .map(|q| q.id().as_str())
                .collect::<Vec<_>>(),
            new.questions
                .iter()
                .map(|q| q.id().as_str())
                .collect::<Vec<_>>()
        );
        new.validate().unwrap();
    }
}

#[test]
fn planning_parameter_priority_pack_is_embedded() {
    let pack = PackStore::from_roots(vec![])
        .load("plan_parameter_priority", "1.0.0")
        .unwrap();
    let ids: Vec<_> = pack
        .questions
        .iter()
        .map(|q| q.id().as_str().to_owned())
        .collect();
    assert_eq!(ids, ["priority", "ask_timing", "discovery_timing"]);
}

#[test]
fn planning_query_intent_pack_is_embedded() {
    let pack = PackStore::from_roots(vec![])
        .load("query_intent", "1.0.0")
        .unwrap();
    let ids: Vec<_> = pack
        .questions
        .iter()
        .map(|q| q.id().as_str().to_owned())
        .collect();
    assert_eq!(ids, ["intent", "task_clarity", "complexity"]);
}

#[test]
fn planning_parameter_priority_1_1_asks_priority_only() {
    let pack = PackStore::from_roots(vec![])
        .load("plan_parameter_priority", "1.1.0")
        .unwrap();
    let ids: Vec<_> = pack
        .questions
        .iter()
        .map(|q| q.id().as_str().to_owned())
        .collect();
    assert_eq!(ids, ["priority"]);
}

/// Planning-decisions Phase D: each pack is embedded with the heads and
/// option ids its host adapter maps (`ask_loop/answer_interpretation_decisions.rs`,
/// `execution/agentic/progress_review_decisions.rs`,
/// `api_mining/recipe_match_decisions.rs`).
#[test]
fn planning_phase_d_packs_are_embedded_with_host_option_ids() {
    let store = PackStore::from_roots(vec![]);
    type QuestionOptions<'a> = (&'a str, &'a [&'a str]);
    let expected: [(&str, &[QuestionOptions<'_>]); 3] = [
        (
            "answer_interpretation",
            &[
                (
                    "answer_type",
                    &[
                        "ambiguous",
                        "clarification",
                        "correction",
                        "direct_answer",
                        "multi_value",
                        "rejection",
                    ],
                ),
                ("requires_replan", &["keep_plan", "replan"]),
            ],
        ),
        (
            "progress_review",
            &[(
                "decision",
                &["continue", "converge", "escalate", "redirect"],
            )],
        ),
        // Candidates are injected per item by the host.
        ("recipe_match", &[("recipe", &[])]),
    ];
    for (id, questions) in expected {
        let pack = store.load(id, "1.0.0").unwrap();
        pack.validate().unwrap();
        assert_eq!(pack.questions.len(), questions.len(), "{id}");
        for (question, (qid, options)) in pack.questions.iter().zip(questions) {
            assert_eq!(question.id().as_str(), *qid, "{id}");
            let value = serde_json::to_value(question).unwrap();
            assert_eq!(value["kind"], "choice", "{id}/{qid}");
            let actual = value["criteria"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            assert_eq!(actual, options.iter().copied().collect(), "{id}/{qid}");
        }
    }
}

#[test]
fn planning_phase_d_seed_matches_the_owner_approved_rollout() {
    // Owner decision 2026-10-02: gate answer_interpretation (requires_replan
    // raised to 0.85) and recipe_match at 0.8 without a recorded
    // qualification; progress_review stays shadow-only.
    let config: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    config.validate().unwrap();
    for (name, gated, thresholds) in [
        (
            "answer_interpretation",
            true,
            &[("answer_type", 0.8), ("requires_replan", 0.85)][..],
        ),
        ("progress_review", false, &[("decision", 0.8)][..]),
        ("recipe_match", true, &[("recipe", 0.8)][..]),
    ] {
        let op = &config.operations[name];
        assert_eq!(op.pack, name);
        assert_eq!(op.pack_version, "1.0.0");
        assert!(op.shadow.enabled, "{name} keeps shadow records");
        assert_eq!(op.gate.enabled, gated, "{name} gate");
        assert_eq!(op.allow_unqualified_gate, gated, "{name} unqualified gate");
        for model in ["jev", "kev-4b"] {
            for (head, value) in thresholds {
                assert_eq!(
                    op.thresholds_by_model[model][*head], *value,
                    "{name}/{model}/{head}"
                );
            }
        }
    }
}

/// The chat-reply claim check (`artifact_v2/claim_support_decisions.rs`)
/// maps these option ids, and runs only gated: it has no incumbent to shadow.
#[test]
fn claim_support_pack_and_route_match_the_host_adapter() {
    let pack = PackStore::from_roots(vec![])
        .load("claim_support", "1.0.0")
        .unwrap();
    pack.validate().unwrap();
    let options: Vec<(String, Vec<String>)> = pack
        .questions
        .iter()
        .map(|question| {
            let value = serde_json::to_value(question).unwrap();
            assert_eq!(value["kind"], "choice");
            (
                question.id().as_str().to_owned(),
                value["criteria"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
            )
        })
        .collect();
    assert_eq!(
        options,
        [
            (
                "verdict".to_owned(),
                vec![
                    "absent".to_owned(),
                    "contradicted".to_owned(),
                    "not_a_claim".to_owned(),
                    "supported".to_owned(),
                ],
            ),
            (
                "core".to_owned(),
                vec!["core".to_owned(), "supporting".to_owned()],
            ),
        ]
    );
    // Owner decision 2026-10-04: gated without a recorded qualification.
    let config: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    config.validate().unwrap();
    let op = &config.operations["claim_support"];
    assert_eq!(
        (op.pack.as_str(), op.pack_version.as_str()),
        ("claim_support", "1.0.0")
    );
    assert!(op.gate.enabled && op.allow_unqualified_gate);
    for model in ["jev", "kev-4b"] {
        assert_eq!(op.thresholds_by_model[model]["verdict"], 0.85, "{model}");
    }
}

#[test]
fn result_delivery_pack_is_separate_from_factual_claim_support_and_bounded() {
    let pack = PackStore::from_roots(vec![])
        .load("result_delivery", "1.0.0")
        .unwrap();
    pack.validate().unwrap();
    assert_eq!(pack.questions.len(), 1);
    let question = serde_json::to_value(&pack.questions[0]).unwrap();
    assert_eq!(question["id"], "verdict");
    assert_eq!(question["kind"], "choice");
    let choices: std::collections::BTreeSet<_> = question["criteria"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        choices,
        ["absent", "contradicted", "not_a_claim", "supported"]
            .into_iter()
            .collect()
    );
    let config: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    config.validate().unwrap();
    let op = &config.operations["result_delivery"];
    assert_eq!(op.pack, "result_delivery");
    assert_eq!(op.pack_version, "1.0.0");
    assert_eq!(op.classification.max_items, 1);
    assert_eq!(op.classification.max_item_bytes, 16_384);
    assert_eq!(op.classification.max_request_bytes, 32_768);
    assert!(op.gate.enabled && op.allow_unqualified_gate);
    assert_eq!(config.operations["claim_support"].pack_version, "1.0.0");
}

/// Preference alignment P3: the pack's three heads carry exactly the
/// `alignment_decision.v2` enum values (unchanged from v1) the host validator accepts
/// (`magician_v2/preference_alignment/contract.rs`). An option renamed here
/// without the contract would be an answer the host can never map.
#[test]
fn action_alignment_pack_is_embedded_with_the_contract_vocabulary() {
    let store = PackStore::from_roots(vec![]);
    let pack = store.load("action_alignment", "1.0.0").unwrap();
    pack.validate().unwrap();
    assert_eq!(pack.id, "action_alignment");
    let expected: [(&str, &[&str]); 3] = [
        (
            "assessment",
            &["acknowledged_exception", "aligned", "conflict", "unclear"],
        ),
        ("intervention", &["clarify", "confirm", "none", "nudge"]),
        ("continuation", &["cancel", "proceed", "wait"]),
    ];
    assert_eq!(pack.questions.len(), expected.len());
    for (question, (qid, options)) in pack.questions.iter().zip(expected) {
        assert_eq!(question.id().as_str(), qid);
        let value = serde_json::to_value(question).unwrap();
        assert_eq!(value["kind"], "choice", "{qid}");
        let actual = value["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, options.iter().copied().collect(), "{qid}");
    }
    assert!(store.load("action_alignment", "2.0.0").is_err());
}

/// 1.1.0 asks per offered intention: the item is one intention, the context
/// the act and the owner's turns. The host composes the decision from these
/// answers, so their vocabulary is the contract it maps.
#[test]
fn action_alignment_per_intention_pack_is_embedded() {
    let store = PackStore::from_roots(vec![]);
    let pack = store.load("action_alignment", "1.1.0").unwrap();
    pack.validate().unwrap();
    let expected: [(&str, &str, &[&str]); 5] = [
        (
            "relation",
            "choice",
            &[
                "acknowledged_exception",
                "conflict",
                "consistent",
                "not_applicable",
                "unclear",
            ],
        ),
        ("owner_cancels", "noul", &["is_false", "is_true"]),
        ("owner_specified", "noul", &["is_false", "is_true"]),
        ("firm_limit", "noul", &["is_false", "is_true"]),
        ("same_subject", "noul", &["is_false", "is_true"]),
    ];
    assert_eq!(pack.questions.len(), expected.len());
    for (question, (qid, kind, options)) in pack.questions.iter().zip(expected) {
        assert_eq!(question.id().as_str(), qid);
        let value = serde_json::to_value(question).unwrap();
        assert_eq!(value["kind"], kind, "{qid}");
        let actual = value["criteria"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, options.iter().copied().collect(), "{qid}");
    }
}

/// Shadow only: the engine records its answer beside the generative
/// reference and has no gate authority until this exact operation is
/// qualified. No thresholds or qualifications are inherited from another
/// operation.
#[test]
fn action_alignment_seed_gates_on_pack_1_3_0_with_every_answer_eligible() {
    let config: magician_decision::config::DecisionConfig =
        serde_yaml::from_str(include_str!("../../decision-engine.yaml")).unwrap();
    config.validate().unwrap();
    let op = &config.operations["action_alignment"];
    assert_eq!(op.pack, "action_alignment");
    assert_eq!(op.pack_version, "1.3.0");
    assert!(op.gate.enabled);
    assert!(
        op.allow_unqualified_gate,
        "owner-approved switch, 2026-10-11"
    );
    assert!(op.qualifications.is_empty());
    assert!(op.restricted_outputs.is_empty());
    // The host composer judges uncertainty: every answer must be eligible.
    let pack = PackStore::from_roots(vec![])
        .load("action_alignment", "1.3.0")
        .unwrap();
    let thresholds = &op.thresholds_by_model["jev"];
    for question in &pack.questions {
        let threshold = thresholds[question.id().as_str()];
        let floor = if question.id().as_str() == "relation" {
            0.0
        } else {
            0.5
        };
        assert!(
            threshold <= floor,
            "jev: {} must be at most {floor}",
            question.id().as_str()
        );
    }
    // Jev only: Kev-4B fails this task, so no route falls back to it.
    let routing = op
        .routing
        .as_ref()
        .expect("action_alignment pins its route");
    for route in [&routing.local, &routing.cloud] {
        assert_eq!(route.names(), vec!["jev".to_string()]);
    }
}

/// Pack 1.3.0: 1.2.0 plus `owner_decides` (the owner's turn makes the one
/// decision a rule reserves for them; owner ruling 2026-10-11).
#[test]
fn action_alignment_pack_1_3_0_adds_owner_decides() {
    let pack = PackStore::from_roots(vec![])
        .load("action_alignment", "1.3.0")
        .unwrap();
    pack.validate().unwrap();
    let ids: Vec<&str> = pack.questions.iter().map(|q| q.id().as_str()).collect();
    assert_eq!(
        ids,
        [
            "relation",
            "owner_cancels",
            "owner_specified",
            "firm_limit",
            "same_subject",
            "muted",
            "owner_decides"
        ]
    );
}

/// Pack 1.2.0: 1.1.0 plus `muted`, with the other offered statements in
/// context.
#[test]
fn action_alignment_pack_1_2_0_adds_muting() {
    let pack = PackStore::from_roots(vec![])
        .load("action_alignment", "1.2.0")
        .unwrap();
    pack.validate().unwrap();
    let ids: Vec<&str> = pack.questions.iter().map(|q| q.id().as_str()).collect();
    assert_eq!(
        ids,
        [
            "relation",
            "owner_cancels",
            "owner_specified",
            "firm_limit",
            "same_subject",
            "muted"
        ]
    );
}

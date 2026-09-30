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

use decision_engine_contract::{batch::*, request::DecisionState, DecideRequest};

fn request() -> BatchRequest {
    BatchRequest {
        mode: Default::default(),
        reference_version: "ref1".into(),
        request_id: "request-1".into(),
        expected_policy_revision: "policy-1".into(),
        projection_version: "projection-1".into(),
        execution_budget_ms: 750,
        context: None,
        items: ["a", "b"]
            .into_iter()
            .map(|id| DecisionItem {
                item_id: id.into(),
                state: DecisionState::from_text("untrusted content"),
                choice_candidates: Default::default(),
            })
            .collect(),
    }
}
fn response(request: &BatchRequest) -> BatchResponse {
    BatchResponse {
        request_id: request.request_id.clone(),
        engine_instance: "boot-1".into(),
        policy_revision: request.expected_policy_revision.clone(),
        items: request
            .items
            .iter()
            .map(|item| DecisionItemResult {
                eligible_answers: Default::default(),
                item_id: item.item_id.clone(),
                status: ItemStatus::NotStarted,
                response: None,
                thresholds: None,
                error: None,
                latency_ms: 0,
            })
            .collect(),
        model_health: Default::default(),
    }
}
#[test]
fn invalid_membership_and_stale_policy_cannot_be_applied() {
    let request = request();
    let valid = response(&request);
    assert!(valid.matches(&request));
    let mut bad = valid.clone();
    bad.items.reverse();
    assert!(!bad.matches(&request));
    let mut bad = valid.clone();
    bad.items.pop();
    assert!(!bad.matches(&request));
    let mut bad = valid.clone();
    bad.items[1].item_id = "a".into();
    assert!(!bad.matches(&request));
    let mut bad = valid.clone();
    bad.request_id = "other".into();
    assert!(!bad.matches(&request));
    let mut bad = valid.clone();
    bad.policy_revision = "other".into();
    assert!(!bad.matches(&request));
    let mut bad = valid.clone();
    bad.engine_instance.clear();
    assert!(!bad.matches(&request));
    let mut bad = valid;
    bad.items[0].status = ItemStatus::Answered;
    assert!(!bad.matches(&request));
}
#[test]
fn duplicate_or_unbounded_request_ids_are_rejected() {
    let mut request = request();
    request.items[1].item_id = "a".into();
    assert!(request.validate_identity().is_err());
    request.items[1].item_id = "b".repeat(257);
    assert!(request.validate_identity().is_err());
}
#[test]
fn old_shape_parses_without_claiming_live_v3_compatibility() {
    let request: DecideRequest = serde_json::from_value(serde_json::json!({
        "contract_version": 3, "operation": "old", "state": "text"
    }))
    .unwrap();
    assert!(request.batch.items.is_empty());
    assert_ne!(
        request.contract_version,
        decision_engine_contract::CONTRACT_VERSION
    );
}
#[test]
fn flat_batch_round_trips_without_text_in_identities() {
    let request = DecideRequest {
        contract_version: decision_engine_contract::CONTRACT_VERSION,
        operation: "memory_applicability".into(),
        state: DecisionState::from_text(""),
        batch: request(),
        choice_candidates: Default::default(),
        locality: Default::default(),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert!(json.get("batch").is_none());
    assert_eq!(json["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        serde_json::from_value::<DecideRequest>(json).unwrap(),
        request
    );
}

#[test]
fn score_outside_rubric_cannot_reach_qualification() {
    use decision_engine_contract::{identity::ModelIdentity, primitives::*, request::*};
    let request = DecisionRequest {
        operation: "quality".into(),
        pack_id: "quality".into(),
        pack_version: "1".into(),
        state: DecisionState::from_text("evidence"),
        questions: vec![Question::Score(ScoreQuestion {
            id: QuestionId::new("quality"),
            instructions: Instruction::Text("quality".into()),
            levels: vec![Criteria::Str("low".into()), Criteria::Str("high".into())],
        })],
    };
    for score in [-0.1, 1.1, f64::NAN, f64::INFINITY] {
        let response = DecisionResponse {
            model: ModelIdentity::new("test", "test"),
            pack_id: "quality".into(),
            pack_version: "1".into(),
            answers: std::collections::BTreeMap::from([(
                QuestionId::new("quality"),
                Answer::Score {
                    score,
                    confidence: 0.99,
                    probabilities: vec![0.1, 0.9],
                },
            )]),
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
        };
        assert!(validate_answers(&request, &response).is_err());
    }
}

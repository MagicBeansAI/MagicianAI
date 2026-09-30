//! Tier routing by fit (plan Part IV, milestone E3): the first model on an
//! operation's route that fits the request and is healthy answers; misfits
//! and failures escalate; thresholds belong to the model that answered.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::config::DecisionConfig;
use magician_decision::engine::{build_runtime_with, route_names};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::{Answer, DecisionRequest, DecisionResponse, DecisionState};
use magician_decision::runtime::{estimate_request_tokens, misfit_reason};
use magician_decision::{
    BoundModel, DecisionError, DecisionRuntime, DecisionRuntimeBuilder, ModelCapabilities,
    ModelIdentity, Pack, PackStore, StructuredDecisionModel,
};

const OP: &str = "memory_applicability";

fn noul(id: &str) -> Question {
    Question::Noul(NoulQuestion {
        id: QuestionId::new(id),
        instructions: Instruction::Text("`memory` is relevant to `request`".to_string()),
        criteria: None,
    })
}

fn choice(id: &str, options: usize) -> Question {
    Question::Choice(ChoiceQuestion {
        id: QuestionId::new(id),
        instructions: Instruction::Text("Which category".to_string()),
        criteria: (0..options)
            .map(|i| {
                (
                    OptionId::new(format!("c{i}")),
                    Criteria::Str(format!("category {i}")),
                )
            })
            .collect(),
    })
}

fn score(id: &str) -> Question {
    Question::Score(ScoreQuestion {
        id: QuestionId::new(id),
        instructions: Instruction::Text("How important".to_string()),
        levels: vec![Criteria::Str("low".into()), Criteria::Str("high".into())],
    })
}

fn pack(questions: Vec<Question>) -> Pack {
    Pack {
        id: OP.to_string(),
        version: "1.0.0".to_string(),
        description: None,
        questions,
    }
}

fn request(questions: Vec<Question>, state_chars: usize) -> DecisionRequest {
    DecisionRequest {
        operation: OP.to_string(),
        pack_id: OP.to_string(),
        pack_version: "1.0.0".to_string(),
        state: DecisionState::from_text("x".repeat(state_chars)),
        questions,
    }
}

fn small_caps() -> ModelCapabilities {
    ModelCapabilities {
        max_state_tokens: Some(512),
        max_choice_options: Some(77),
        supports_score: false,
        remote: false,
        ..ModelCapabilities::default()
    }
}

fn answering(model: &str, caps: ModelCapabilities) -> MemoryDecisionModel {
    MemoryDecisionModel::new("systemone", model)
        .with_capabilities(caps)
        .with_answer("applicable", Answer::Noul { noul: 0.93 })
        .with_answer(
            "importance",
            Answer::Score {
                score: 1.0,
                probabilities: vec![0.1, 0.9],
                confidence: 0.9,
            },
        )
        .with_answer(
            "category",
            Answer::Choice {
                choice: OptionId::new("c1"),
                probabilities: BTreeMap::new(),
                confidence: 0.8,
            },
        )
}

fn entry(name: &str, model: impl StructuredDecisionModel + 'static) -> BoundModel {
    BoundModel {
        admission: Default::default(),
        name: name.to_string(),
        model: Arc::new(model),
        thresholds: None,
    }
}

fn runtime(questions: Vec<Question>, route: Vec<BoundModel>) -> DecisionRuntime {
    DecisionRuntimeBuilder::new()
        .bind_route(OP, pack(questions), route)
        .build()
}

#[tokio::test]
async fn decision_telemetry_preserves_local_usage_when_validation_rejects_answer() {
    let model = MemoryDecisionModel::new("kev-mlx", "kev-4b").with_answer(
        "category",
        Answer::Choice {
            choice: OptionId::new("undeclared"),
            probabilities: BTreeMap::new(),
            confidence: 0.9,
        },
    );
    let runtime = runtime(vec![choice("category", 2)], vec![entry("local", model)]);
    let (result, calls) = magician_decision::telemetry::capture(
        runtime.evaluate(OP, DecisionState::from_text("fixture")),
    )
    .await;
    assert!(matches!(result, Err(DecisionError::UnknownOption { .. })));
    assert_eq!(calls.len(), 1);
    let id: ulid::Ulid = calls[0].call_id.parse().expect("time-indexed call id");
    assert!((id.timestamp_ms() as i64 - calls[0].started_at_ms).abs() < 1000);
    assert_eq!(calls[0].input_tokens, Some(64));
    assert_eq!(calls[0].output_tokens, Some(0));
    assert!(calls[0].local);
    assert_eq!(calls[0].provider, "decision:kev-mlx");
    assert_eq!(
        calls[0].status,
        decision_engine_contract::telemetry::DecisionCallStatus::Failed
    );
}

/// A model whose transport is down; counts the calls it received.
struct Down {
    calls: Arc<AtomicUsize>,
    caps: ModelCapabilities,
}

#[async_trait]
impl StructuredDecisionModel for Down {
    fn identity(&self) -> ModelIdentity {
        ModelIdentity::new("systemone", "down")
    }
    fn capabilities(&self) -> ModelCapabilities {
        self.caps.clone()
    }
    async fn evaluate(&self, _: DecisionRequest) -> Result<DecisionResponse, DecisionError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(DecisionError::Transport("connection refused".to_string()))
    }
}

#[tokio::test]
async fn the_small_model_answers_a_request_that_fits_it() {
    let questions = vec![noul("applicable")];
    let rt = runtime(
        questions.clone(),
        vec![
            entry("laya", answering("laya", small_caps())),
            entry("jev", answering("jev-1.13.0", ModelCapabilities::default())),
        ],
    );
    let response = rt
        .evaluate_request(request(questions, 200))
        .await
        .expect("answers");
    assert_eq!(response.model.model, "laya");
}

#[tokio::test]
async fn too_many_options_a_score_question_or_a_long_state_escalate() {
    let cases = vec![
        (vec![choice("category", 120)], 200),
        (vec![score("importance")], 200),
        (vec![noul("applicable")], 4_000),
    ];
    for (questions, state_chars) in cases {
        let rt = runtime(
            questions.clone(),
            vec![
                entry("laya", answering("laya", small_caps())),
                entry("jev", answering("jev-1.13.0", ModelCapabilities::default())),
            ],
        );
        let response = rt
            .evaluate_request(request(questions, state_chars))
            .await
            .expect("the large model answers");
        assert_eq!(response.model.model, "jev-1.13.0");
    }
}

#[tokio::test]
async fn a_failed_model_escalates_and_then_cools_down() {
    let calls = Arc::new(AtomicUsize::new(0));
    let questions = vec![noul("applicable")];
    let rt = runtime(
        questions.clone(),
        vec![
            entry(
                "laya",
                Down {
                    calls: Arc::clone(&calls),
                    caps: small_caps(),
                },
            ),
            entry("jev", answering("jev-1.13.0", ModelCapabilities::default())),
        ],
    );
    for index in 0..3 {
        let (response, receipts) = magician_decision::telemetry::capture(
            rt.evaluate_request(request(questions.clone(), 200)),
        )
        .await;
        assert_eq!(
            response.expect("the next model answers").model.model,
            "jev-1.13.0"
        );
        assert_eq!(
            receipts.len(),
            if index == 0 { 2 } else { 1 },
            "cooldown skips are not calls"
        );
        assert_eq!(
            receipts.last().unwrap().status,
            decision_engine_contract::telemetry::DecisionCallStatus::Succeeded
        );
        if index == 0 {
            assert_eq!(
                receipts[0].status,
                decision_engine_contract::telemetry::DecisionCallStatus::Failed
            );
            assert_eq!(receipts[0].input_tokens, None);
        }
    }
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "skipped while cooling down"
    );
    let health = rt.health_for(OP);
    assert!(health.iter().any(
        |h| h.model == "laya" && h.issue.as_deref() == Some("structured_provider_unavailable")
    ));
    assert!(health.iter().any(|h| h.model == "jev" && h.issue.is_none()));
}

#[tokio::test]
async fn nothing_fitting_is_no_fitting_model_with_reasons() {
    let questions = vec![score("importance")];
    let rt = runtime(
        questions.clone(),
        vec![
            entry("laya", answering("laya", small_caps())),
            entry("laya-2", answering("laya-2", small_caps())),
        ],
    );
    match rt.evaluate_request(request(questions, 200)).await {
        Err(DecisionError::NoFittingModel { operation, skipped }) => {
            assert_eq!(operation, OP);
            assert_eq!(skipped.len(), 2);
            assert!(skipped[0].contains("Score"), "{skipped:?}");
        },
        other => panic!("expected NoFittingModel, got {other:?}"),
    }
}

#[tokio::test]
async fn a_pinned_single_model_rejects_oversized_context_and_choices() {
    // Explicit selection cannot authorize an adapter to truncate evidence.
    let questions = vec![choice("category", 120)];
    let rt = runtime(
        questions.clone(),
        vec![entry("laya", answering("laya", small_caps()))],
    );
    let error = rt
        .evaluate_request(request(questions, 4_000))
        .await
        .expect_err("oversized pin must escalate");
    assert!(matches!(error, DecisionError::NoFittingModel { .. }));
    let error = rt
        .evaluate_request(request(vec![noul("applicable")], 4_000))
        .await
        .expect_err("oversized context must escalate even with few options");
    assert!(matches!(error, DecisionError::NoFittingModel { .. }));
}

#[test]
fn fit_checks_name_the_limit_that_failed() {
    let caps = small_caps();
    assert!(misfit_reason(&request(vec![noul("applicable")], 200), &caps).is_none());
    let reason = misfit_reason(&request(vec![choice("category", 78)], 10), &caps).expect("misfit");
    assert!(reason.contains("78 options"), "{reason}");
    let long = request(vec![noul("applicable")], 2_000);
    assert!(estimate_request_tokens(&long) > 512);
    assert!(misfit_reason(&long, &caps)
        .expect("misfit")
        .contains("tokens"));
    // Undeclared limits never misfit.
    assert!(misfit_reason(&long, &ModelCapabilities::default()).is_none());
}

fn tiered_config() -> DecisionConfig {
    serde_yaml::from_str(
        "enabled: true
models:
  laya:
    adapter: systemone
    model: laya-typed-decisions
    endpoint: http://127.0.0.1:8090/v1/systemone
    capabilities:
      max_state_tokens: 512
      max_choice_options: 77
      supports_score: false
  kev-4b:
    adapter: systemone
    model: kev-4b
    endpoint: http://127.0.0.1:8091/v1/systemone
  jev:
    adapter: typesafe
    model: jev-1.13.0
tiers:
  small: [laya]
  large: [kev-4b, jev]
operations:
  tool_action_judge:
    tier: small
    pack: tool_action_judge
    pack_version: '1.0.0'
    sees_body: true
    thresholds_by_model:
      jev:
        evidence_sufficient: 0.9
      laya:
        evidence_sufficient: 0.95
",
    )
    .expect("parses")
}

fn jev_key(name: &str) -> Option<String> {
    (name == "TYPESAFE_API_KEY").then(|| "k".to_string())
}

#[test]
fn a_small_tier_route_escalates_into_large_and_a_pin_wins() {
    let config = tiered_config();
    let op = &config.operations["tool_action_judge"];
    assert_eq!(route_names(&config, op), vec!["laya", "kev-4b", "jev"]);
    let mut pinned = op.clone();
    pinned.model = "jev".to_string();
    assert_eq!(route_names(&config, &pinned), vec!["jev"]);
    let mut large = op.clone();
    large.tier = Some(magician_decision::config::DecisionTier::Large);
    assert_eq!(route_names(&config, &large), vec!["kev-4b", "jev"]);
}

#[test]
fn local_mode_drops_the_remote_model_from_a_body_seeing_route() {
    let config = tiered_config();
    let store = PackStore::new(None);
    let local = build_runtime_with(&config, true, &store, &jev_key).expect("binds");
    let names: Vec<_> = local
        .bound_operation("tool_action_judge")
        .expect("bound")
        .route
        .iter()
        .map(|entry| entry.name.clone())
        .collect();
    assert_eq!(names, vec!["laya", "kev-4b"]);
    let cloud = build_runtime_with(&config, false, &store, &jev_key).expect("binds");
    assert_eq!(
        cloud
            .bound_operation("tool_action_judge")
            .expect("bound")
            .route
            .len(),
        3
    );
}

#[test]
fn thresholds_belong_to_the_model_that_answered() {
    let config = tiered_config();
    let rt = build_runtime_with(&config, false, &PackStore::new(None), &jev_key).expect("binds");
    let laya = rt
        .thresholds_for(
            "tool_action_judge",
            &ModelIdentity::new("systemone", "laya-typed-decisions"),
        )
        .expect("laya owns a set");
    assert_eq!(laya.get("evidence_sufficient"), Some(&0.95));
    let jev = rt
        .thresholds_for(
            "tool_action_judge",
            &ModelIdentity::new("typesafe", "jev-1.13.0"),
        )
        .expect("jev owns a set");
    assert_eq!(jev.get("evidence_sufficient"), Some(&0.9));
    // Kev has none of its own: it may answer, it may not gate.
    assert!(rt
        .thresholds_for(
            "tool_action_judge",
            &ModelIdentity::new("systemone", "kev-4b")
        )
        .is_none());
}

#[test]
fn a_pinned_operation_keeps_its_plain_thresholds() {
    let mut config = tiered_config();
    let op = config.operations.get_mut("tool_action_judge").expect("op");
    op.tier = None;
    op.model = "jev".to_string();
    op.thresholds_by_model.clear();
    op.thresholds
        .insert("evidence_sufficient".to_string(), 0.85);
    let rt = build_runtime_with(&config, false, &PackStore::new(None), &jev_key).expect("binds");
    let jev = rt
        .thresholds_for(
            "tool_action_judge",
            &ModelIdentity::new("typesafe", "jev-1.13.0"),
        )
        .expect("the pin owns `thresholds`");
    assert_eq!(jev.get("evidence_sufficient"), Some(&0.85));
}

/// The engine's settings carry no host keys: where the host reaches the
/// engine is the host's own config, so these are refused here.
#[test]
fn host_transport_keys_are_not_engine_settings() {
    for yaml in [
        "enabled: true\ntransport: service\n",
        "enabled: true\nservice:\n  timeout_ms: 1500\n",
    ] {
        assert!(
            serde_yaml::from_str::<DecisionConfig>(yaml).is_err(),
            "{yaml}"
        );
    }
}

#[tokio::test]
async fn pinned_model_outage_is_cooled_down_without_duplicate_receipts() {
    let calls = Arc::new(AtomicUsize::new(0));
    let questions = vec![noul("applicable")];
    let rt = runtime(
        questions.clone(),
        vec![entry(
            "only",
            Down {
                calls: calls.clone(),
                caps: small_caps(),
            },
        )],
    );
    for index in 0..3 {
        let (result, receipts) = magician_decision::telemetry::capture(
            rt.evaluate_request(request(questions.clone(), 100)),
        )
        .await;
        assert!(result.is_err());
        assert_eq!(receipts.len(), usize::from(index == 0));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

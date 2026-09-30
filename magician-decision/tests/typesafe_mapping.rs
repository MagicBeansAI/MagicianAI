//! Frozen-fixture HTTP contract tests for the typesafe adapter: request
//! wire shape, auth header, response mapping, unknown-option refusal, and
//! the 429 → retry-after → success path. Mirrors the magicllm
//! `provider_http.rs` pattern (mock server + `with_client` + assertions on
//! recorded requests).

use magician_decision::adapters::typesafe::{TypesafeConfig, TypesafeDecisionModel};
use magician_decision::primitives::{
    ChoiceQuestion, Criteria, Instruction, NoulQuestion, OptionId, Question, QuestionId,
    ScoreQuestion,
};
use magician_decision::request::{DecisionRequest, DecisionState};
use magician_decision::{DecisionError, StructuredDecisionModel};
use std::collections::BTreeMap;
use std::time::Duration;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn sample_request() -> DecisionRequest {
    let mut criteria = BTreeMap::new();
    criteria.insert(
        OptionId::new("@e12"),
        Criteria::Str("button 'Search'".to_string()),
    );
    criteria.insert(
        OptionId::new("@e54"),
        Criteria::Str("link 'Flights'".to_string()),
    );
    DecisionRequest {
        operation: "tool_action_judge".to_string(),
        pack_id: "tool_action_judge".to_string(),
        pack_version: "1.0.0".to_string(),
        state: DecisionState::from_json(serde_json::json!({
            "goal": "Open the flights page",
            "flat_page": "text snapshot"
        })),
        questions: vec![
            Question::Choice(ChoiceQuestion {
                id: QuestionId::new("next_element"),
                instructions: Instruction::Text("Which element advances the goal".to_string()),
                criteria,
            }),
            Question::Score(ScoreQuestion {
                id: QuestionId::new("urgency"),
                instructions: Instruction::Text("How urgent".to_string()),
                levels: vec![
                    Criteria::Str("low".to_string()),
                    Criteria::Str("normal".to_string()),
                    Criteria::Str("high".to_string()),
                ],
            }),
            Question::Noul(NoulQuestion {
                id: QuestionId::new("evidence_sufficient"),
                instructions: Instruction::Text("The page finished loading".to_string()),
                criteria: None,
            }),
        ],
    }
}

fn ok_response_body() -> serde_json::Value {
    serde_json::json!({
        "model": "jev-latest",
        "answers": {
            "next_element": {
                "type": "choice",
                "choice": "@e12",
                "probabilities": { "@e12": 0.91, "@e54": 0.09 },
                "confidence": 0.9
            },
            "urgency": {
                "type": "score",
                "score": 1.0,
                "legend": { "0": "low", "1": "normal", "2": "high" },
                "probabilities": { "0": 0.1, "1": 0.8, "2": 0.1 },
                "confidence": 0.8
            },
            "evidence_sufficient": { "type": "noul", "noul": 0.97 }
        },
        "usage": { "input_tokens": 312, "output_tokens": 48 }
    })
}

fn model_against(server: &MockServer) -> TypesafeDecisionModel {
    let config = TypesafeConfig::new(
        format!("{}/v1/systemone", server.uri()),
        "test-key",
        "jev-latest",
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("client");
    TypesafeDecisionModel::with_client(config, client)
}

#[tokio::test]
async fn maps_request_and_response_and_sends_bearer_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .and(header("authorization", "Bearer test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
        .mount(&server)
        .await;

    let response = model_against(&server)
        .evaluate(sample_request())
        .await
        .expect("evaluates");

    assert_eq!(response.model.model, "jev-latest");
    assert_eq!(response.usage.input_tokens, 312);
    let element = response
        .answer(&QuestionId::new("next_element"))
        .expect("answer");
    assert_eq!(element.choice_option().map(|o| o.as_str()), Some("@e12"));
    assert_eq!(element.choice_confidence(), Some(0.9));
    match response
        .answer(&QuestionId::new("evidence_sufficient"))
        .expect("answer")
    {
        magician_decision::Answer::Noul { noul } => assert!((*noul - 0.97).abs() < 1e-9),
        other => panic!("expected noul, got {other:?}"),
    }

    // Wire shape: one request, model + state + the three question dialects.
    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).expect("json");
    assert_eq!(body["model"], "jev-latest");
    assert_eq!(body["state"]["goal"], "Open the flights page");
    assert_eq!(body["questions"]["next_element"]["type"], "choice");
    assert_eq!(
        body["questions"]["next_element"]["criteria"]["@e12"],
        "button 'Search'"
    );
    assert_eq!(body["questions"]["urgency"]["type"], "score");
    assert_eq!(
        body["questions"]["urgency"]["criteria"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    assert_eq!(body["questions"]["evidence_sufficient"]["type"], "noul");
}

#[tokio::test]
async fn undeclared_option_is_refused_not_coerced() {
    let server = MockServer::start().await;
    let mut body = ok_response_body();
    body["answers"]["next_element"]["choice"] = "@e999-not-declared".into();
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;

    let err = model_against(&server)
        .evaluate(sample_request())
        .await
        .expect_err("must refuse");
    assert!(matches!(err, DecisionError::UnknownOption { .. }));
}

#[tokio::test]
async fn rate_limited_retries_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(serde_json::json!({})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
        .mount(&server)
        .await;

    let model = model_against(&server);
    let (response, calls) =
        magician_decision::telemetry::capture(model.evaluate(sample_request())).await;
    let response = response.expect("succeeds after retry");
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].retry_group_id, calls[1].retry_group_id);
    assert_ne!(calls[0].call_id, calls[1].call_id);
    assert_eq!(calls[0].attempt, 1);
    assert_eq!(calls[1].attempt, 2);
    assert_eq!(calls[0].input_tokens, None);
    assert_eq!(calls[1].input_tokens, Some(312));
    assert_eq!(calls[1].cache_read_tokens, None);
    assert_eq!(
        calls[1].provider, "decision:systemone:typesafe",
        "custom endpoints cannot inherit TypeSafe billing"
    );
    assert_eq!(response.answers.len(), 3);
    assert_eq!(server.received_requests().await.expect("recorded").len(), 2);
}

#[tokio::test]
async fn decision_telemetry_keeps_billed_invalid_answers_and_exact_model() {
    let server = MockServer::start().await;
    let mut body = ok_response_body();
    body["model"] = "jev-1.13.0".into();
    body["answers"]["next_element"]["choice"] = "unoffered".into();
    body["usage"]["cache_read_tokens"] = 100.into();
    body["usage"]["cache_creation_tokens"] = 12.into();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_json(body))
        .mount(&server)
        .await;
    let model = model_against(&server);
    let (response, calls) =
        magician_decision::telemetry::capture(model.evaluate(sample_request())).await;
    assert!(response.is_err());
    assert_eq!(calls.len(), 1);
    let call = &calls[0];
    assert_eq!(call.model, "jev-1.13.0");
    assert_eq!(call.requested_model, "jev-latest");
    assert_eq!(call.input_tokens, Some(312));
    assert_eq!(call.cache_read_tokens, Some(100));
    assert_eq!(call.cache_write_tokens, Some(12));
    assert_eq!(
        call.status,
        decision_engine_contract::telemetry::DecisionCallStatus::Failed
    );
    assert_eq!(call.error_class.as_deref(), Some("invalid_response"));
    let json = serde_json::to_string(call).unwrap();
    assert!(!json.contains("test-key") && !json.contains("flights") && !json.contains("unoffered"));
}

#[tokio::test]
async fn decision_telemetry_missing_usage_and_cancelled_requests_are_not_free_zero_tokens() {
    let server = MockServer::start().await;
    let mut body = ok_response_body();
    body.as_object_mut().unwrap().remove("usage");
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(body)
                .set_delay(Duration::from_millis(60)),
        )
        .mount(&server)
        .await;
    let model = model_against(&server);
    let (first, calls) =
        magician_decision::telemetry::capture(model.evaluate(sample_request())).await;
    assert!(first.is_ok());
    assert_eq!(calls[0].input_tokens, None);
    assert_eq!(calls[0].output_tokens, None);
    let (cancelled, calls) = magician_decision::telemetry::capture(tokio::time::timeout(
        Duration::from_millis(10),
        model.evaluate(sample_request()),
    ))
    .await;
    assert!(cancelled.is_err());
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].status,
        decision_engine_contract::telemetry::DecisionCallStatus::Cancelled
    );
    assert!(calls[0].latency_ms >= 9);
    assert_eq!(calls[0].input_tokens, None);
}

#[tokio::test]
async fn quota_exhaustion_on_429_is_credit_failure_without_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429).set_body_json(serde_json::json!({
            "error": {"code": "insufficient_quota", "message": "Account credit exhausted"}
        })))
        .mount(&server)
        .await;
    let error = model_against(&server)
        .evaluate(sample_request())
        .await
        .expect_err("quota must not be retried as temporary throttling");
    assert_eq!(error.health_reason(), Some("structured_provider_credit"));
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn persistent_rate_limit_surfaces_as_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let err = model_against(&server)
        .evaluate(sample_request())
        .await
        .expect_err("must fail");
    assert!(matches!(err, DecisionError::RateLimited { .. }));
}

#[tokio::test]
async fn a_keyless_self_hosted_model_sends_no_auth_and_labels_its_answers() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/systemone"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_response_body()))
        .mount(&server)
        .await;
    let config = magician_decision::adapters::SystemOneConfig::keyless(
        format!("{}/v1/systemone", server.uri()),
        "jev-latest",
    );
    let model = magician_decision::adapters::SystemOneDecisionModel::with_client(
        config,
        reqwest::Client::new(),
    );
    // The mock server listens on 127.0.0.1: a loopback endpoint is local.
    assert!(!model.capabilities().remote);
    assert!(!model.capabilities().calibrated);

    let response = model.evaluate(sample_request()).await.expect("evaluates");
    assert_eq!(response.model.adapter, "systemone");

    let requests = server.received_requests().await.expect("recorded");
    assert_eq!(requests.len(), 1);
    assert!(
        !requests[0]
            .headers
            .iter()
            .any(|(name, _)| name.as_str().eq_ignore_ascii_case("authorization")),
        "a keyless profile must not send a bearer header"
    );
}

#[test]
fn only_loopback_endpoints_count_as_local() {
    use magician_decision::adapters::systemone::is_loopback_endpoint;
    assert!(is_loopback_endpoint("http://127.0.0.1:8090/v1/systemone"));
    assert!(is_loopback_endpoint("http://localhost:8090/v1/systemone"));
    assert!(is_loopback_endpoint("http://[::1]:8090/v1/systemone"));
    assert!(!is_loopback_endpoint(
        "https://api.typesafe.ai/v1/systemone"
    ));
    assert!(!is_loopback_endpoint(
        "http://192.168.1.20:8090/v1/systemone"
    ));
    assert!(!is_loopback_endpoint(
        "http://localhost.example.com/v1/systemone"
    ));
    assert!(!is_loopback_endpoint("not a url"));
}

use super::action_binding::{ActionBinding, ActionContext, ActionParamBinding};
use super::capability::ApiCapability;
use super::capability_store::CapabilityStore;
use super::origin_policy::{OriginPolicyStore, OriginReplayMode};
use super::replay::ApiRunner;
use super::router::{ApiRouter, RouteDecision};
use super::types::SessionContext;
use super::workflow::{
    AuthRequirements, DataFlow, InferenceMethod, ParamSource, ReplayStats, WorkflowConfidence,
    WorkflowGraph, WorkflowMaturity, WorkflowStep,
};
use super::workflow_replay::step_executor::StepExecutor;
use super::workflow_replay::{ReplayInputs, WorkflowReplayEngine};
use crate::config::ApiMiningConfig;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::sync::Mutex;
use wiremock::matchers::{method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn enabled_config() -> ApiMiningConfig {
    ApiMiningConfig {
        enable_trace_capture: true,
        enable_mining: true,
        enable_replay: true,
        enable_xhr_validation: true,
        ..ApiMiningConfig::default()
    }
}

fn candidate_get_capability(origin: &str, name: &str, url_template: String) -> ApiCapability {
    let mut capability = ApiCapability::new(
        name.to_string(),
        origin.to_string(),
        "GET".to_string(),
        url_template,
    );
    capability.add_sample("sample-2".to_string());
    capability.add_sample("sample-3".to_string());
    capability
}

fn search_action_binding() -> ActionBinding {
    ActionBinding {
        action_type: "Click".to_string(),
        action_signature: "selector=button[data-fixture-search]|button=Left|count=1|iframe="
            .to_string(),
        semantic_signature: Some("click:button:data-fixture-search".to_string()),
        page_origin: Some("https://fixture.local".to_string()),
        page_path_template: Some("/search".to_string()),
        param_bindings: vec![ActionParamBinding {
            action_param: "query".to_string(),
            capability_param: "query".to_string(),
        }],
        default_params: HashMap::new(),
        sample_count: 2,
        last_seen_at: Some(1_780_000_000_000),
    }
}

fn search_action_context(query: &str) -> ActionContext {
    ActionContext {
        action_type: "Click".to_string(),
        action_signature: "selector=button[data-fixture-search]|button=Left|count=1|iframe="
            .to_string(),
        semantic_signature: Some("click:button:data-fixture-search".to_string()),
        page_origin: Some("https://fixture.local".to_string()),
        page_path_template: Some("/search".to_string()),
        param_values: HashMap::from([("query".to_string(), query.to_string())]),
        user_values: vec![query.to_string()],
    }
}

#[tokio::test]
async fn local_fixture_second_run_routes_and_replays_direct_api() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "americano"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "id": "drink-1", "name": "Iced Americano" }
            ]
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let origin = server.uri();
    OriginPolicyStore::open(temp.path())
        .set_replay_mode(&origin, OriginReplayMode::ReplayReads)
        .unwrap();

    let mut router = ApiRouter::with_base_path(&enabled_config(), temp.path());
    let mut capability = candidate_get_capability(
        &origin,
        "fixture_search",
        format!("{origin}/api/search?q={{query}}"),
    );
    capability.record_action_binding(search_action_binding());
    let capability_id = capability.id.clone();
    router
        .registry_mut()
        .unwrap()
        .register(&capability)
        .unwrap();

    let decision = router.route_action_context(
        &search_action_context("americano"),
        &SessionContext::default(),
    );
    let RouteDecision::Replay {
        capability_id: routed_id,
        request,
        request_params,
        ..
    } = decision
    else {
        panic!("expected second-run action binding to route to direct API replay");
    };
    assert_eq!(routed_id, capability_id);
    assert_eq!(request.method, "GET");
    assert_eq!(request.url, format!("{origin}/api/search?q=americano"));
    assert_eq!(
        request_params.get("query").map(String::as_str),
        Some("americano")
    );

    let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
    let replay = runner
        .replay_with_reqwest(
            &origin,
            &capability_id,
            &HashMap::from([("query".to_string(), "americano".to_string())]),
            &SessionContext::default(),
            None,
            None,
        )
        .await
        .unwrap();

    assert!(replay.success);
    assert_eq!(replay.status, 200);
    assert!(replay
        .response_body
        .as_deref()
        .unwrap_or_default()
        .contains("Iced Americano"));
}

#[tokio::test]
async fn local_fixture_replay_failure_surfaces_browser_fallback_signal() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "fail"))
        .respond_with(ResponseTemplate::new(503).set_body_json(json!({
            "error": "fixture unavailable"
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let origin = server.uri();
    let mut capability = candidate_get_capability(
        &origin,
        "fixture_search_failure",
        format!("{origin}/api/search?q={{query}}"),
    );
    capability.record_action_binding(search_action_binding());
    let capability_id = capability.id.clone();
    super::registry::CapabilityRegistry::with_base_path(temp.path())
        .unwrap()
        .register(&capability)
        .unwrap();

    let mut runner = ApiRunner::with_base_path(temp.path()).unwrap();
    let replay = runner
        .replay_with_reqwest(
            &origin,
            &capability_id,
            &HashMap::from([("query".to_string(), "fail".to_string())]),
            &SessionContext::default(),
            None,
            None,
        )
        .await
        .unwrap();

    assert!(!replay.success);
    assert_eq!(replay.status, 503);
    assert!(
        replay
            .verification
            .as_ref()
            .is_some_and(|verification| !verification.passed),
        "executor fallback depends on failed replay verification being explicit"
    );
}

#[tokio::test]
async fn local_fixture_two_step_workflow_replays_search_then_detail() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/api/search"))
        .and(query_param("q", "americano"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "results": [
                { "id": "drink-1", "name": "Iced Americano" }
            ]
        })))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/detail/drink-1"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "drink-1",
            "price": 129
        })))
        .mount(&server)
        .await;

    let temp = TempDir::new().unwrap();
    let origin = server.uri();
    let origin_key = CapabilityStore::origin_to_key(&origin);
    let search = candidate_get_capability(
        &origin,
        "fixture_search",
        format!("{origin}/api/search?q={{query}}"),
    );
    let detail = candidate_get_capability(
        &origin,
        "fixture_detail",
        format!("{origin}/api/detail/{{id}}"),
    );
    let search_id = search.id.clone();
    let detail_id = detail.id.clone();
    let mut registry = super::registry::CapabilityRegistry::with_base_path(temp.path()).unwrap();
    registry.register(&search).unwrap();
    registry.register(&detail).unwrap();

    let mut workflow = WorkflowGraph {
        id: "fixture_search_detail".to_string(),
        origin_key: origin.clone(),
        name: "Fixture search/detail".to_string(),
        steps: vec![
            WorkflowStep {
                id: "search".to_string(),
                step_index: 0,
                capability_id: Some(search_id),
                param_sources: HashMap::from([(
                    "query".to_string(),
                    ParamSource::UserInput {
                        input_key: "query".to_string(),
                    },
                )]),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            },
            WorkflowStep {
                id: "detail".to_string(),
                step_index: 1,
                capability_id: Some(detail_id),
                param_sources: HashMap::from([(
                    "id".to_string(),
                    ParamSource::DataFlow {
                        data_flow_id: "search_to_detail".to_string(),
                    },
                )]),
                skip_if: None,
                browser_only: false,
                browser_fallback: None,
            },
        ],
        data_flows: vec![DataFlow {
            id: "search_to_detail".to_string(),
            source_step: "search".to_string(),
            source_path: "$.results[0].id".to_string(),
            target_step: "detail".to_string(),
            target_param: "id".to_string(),
            inference_method: InferenceMethod::AutoMatch,
            confidence: 0.95,
        }],
        auth_requirements: AuthRequirements::default(),
        confidence: WorkflowConfidence {
            workflow_level: WorkflowMaturity::Candidate,
            step_confidences: HashMap::new(),
        },
        compiled_from_sequence_ids: vec!["fixture-seq".to_string()],
        last_compiled_at_ms: 1_780_000_000_000,
        last_replayed_at_ms: None,
        replay_stats: ReplayStats::default(),
    };

    let runner = Arc::new(Mutex::new(ApiRunner::with_base_path(temp.path()).unwrap()));
    let executor = Arc::new(StepExecutor::new(runner));
    let engine = WorkflowReplayEngine::new(executor);
    let result = engine
        .replay(
            &mut workflow,
            &ReplayInputs {
                user_inputs: HashMap::from([("query".to_string(), "americano".to_string())]),
                timeout_ms: Some(5_000),
            },
            HashMap::new(),
            &SessionContext::default(),
        )
        .await;

    assert_eq!(
        origin_key,
        CapabilityStore::origin_to_key(&workflow.origin_key)
    );
    assert!(
        result.success,
        "workflow replay failed: {:?}",
        result.failure
    );
    assert_eq!(result.steps.len(), 2);
    let expected_search_url = format!("{origin}/api/search?q=americano");
    assert_eq!(
        result.steps[0].replay_url.as_deref(),
        Some(expected_search_url.as_str())
    );
    assert_eq!(
        result.steps[1].request_params.get("id").map(String::as_str),
        Some("drink-1")
    );
    assert_eq!(result.steps[1].http_status, Some(200));
}

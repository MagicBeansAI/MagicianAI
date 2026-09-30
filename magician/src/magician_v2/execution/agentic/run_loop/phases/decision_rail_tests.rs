//! Integration checks against the real host catalog, service and lowering path.
use super::*;
use decision_engine_contract::action::{ActionCandidate, ActionPlan};
use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::config::{DecisionConfig, DecisionOperationConfig};
use magician_decision::{
    Answer, BoundModel, DecisionRuntimeBuilder, OptionId, PackStore,
};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

struct PlannerFixtureModel;

#[async_trait::async_trait]
impl magician_decision::StructuredDecisionModel for PlannerFixtureModel {
    fn identity(&self) -> magician_decision::ModelIdentity {
        magician_decision::ModelIdentity::new("memory", "scripted")
    }

    fn capabilities(&self) -> magician_decision::ModelCapabilities {
        Default::default()
    }

    async fn evaluate(
        &self,
        request: magician_decision::DecisionRequest,
    ) -> Result<magician_decision::DecisionResponse, magician_decision::DecisionError> {
        // The host catalog also contains grounded built-in calls before a
        // planner proposes anything. Never invent plan:0 in that first Choice:
        // an undeclared answer correctly puts even a pinned model on cooldown.
        let choice = if request.questions.iter().any(|question| {
            matches!(question, magician_decision::Question::Choice(choice)
                if choice.criteria.contains_key(&OptionId::new("plan:0")))
        }) {
            "plan:0"
        } else {
            "need_planner"
        };
        MemoryDecisionModel::new("memory", "scripted")
            .with_answer(
                "next_action",
                Answer::Choice {
                    choice: OptionId::new(choice),
                    confidence: 0.99,
                    probabilities: BTreeMap::from([(OptionId::new(choice), 0.99)]),
                },
            )
            .with_answer("evidence_sufficient", Answer::Noul { noul: 0.99 })
            .with_answer("action_applicable", Answer::Noul { noul: 0.99 })
            .evaluate(request)
            .await
    }
}

fn engine() -> decision_engine::Engine {
    let model = Arc::new(PlannerFixtureModel);
    let runtime = Arc::new(
        DecisionRuntimeBuilder::new()
            .bind_route(
                ACTION_OPERATION,
                PackStore::new(None)
                    .load(ACTION_OPERATION, "1.0.0")
                    .unwrap(),
                vec![BoundModel {
                    admission: Default::default(),
                    name: "scripted".into(),
                    model,
                    thresholds: Some(BTreeMap::new()),
                }],
            )
            .build(),
    );
    let mut operation = DecisionOperationConfig::default();
    operation.gate.enabled = true;
    operation.model = "scripted".into();
    operation.pack = ACTION_OPERATION.into();
    let mut config = DecisionConfig::default();
    config.enabled = true;
    config.operations.insert(ACTION_OPERATION.into(), operation);
    decision_engine::Engine::with_runtimes(config, Some(Arc::clone(&runtime)), Some(runtime))
}

struct Service {
    client: decision_engine_contract::client::EngineClient,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    socket: std::path::PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}
async fn service() -> Service {
    let socket = std::path::PathBuf::from(format!(
        "/tmp/rail-host-{}.sock",
        uuid::Uuid::new_v4().simple()
    ));
    let path = socket.clone();
    let task = tokio::spawn(async move { decision_engine::serve(Arc::new(engine()), &path).await });
    let client =
        decision_engine_contract::client::EngineClient::new(&socket, Duration::from_secs(5));
    for _ in 0..100 {
        if client.health().await.is_ok() {
            return Service {
                client,
                task,
                socket,
            };
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("decision service did not start");
}

fn fixture_context() -> AgenticContext {
    let mut ctx = AgenticContext::new("Read the linked records", "Three records read");
    ctx.execution_id = Some(format!("decision-rail-test-{}", uuid::Uuid::new_v4()));
    ctx.merged_agent_tools = vec![runtime_core::ToolInfo {
        name: "rail_fixture".into(),
        description: "Read a record by its key".into(),
        category: "custom".into(),
        categories: vec!["custom".into()],
        parameters: vec![runtime_core::ParameterDefinition {
            name: "key".into(),
            param_type: "integer".into(),
            required: true,
            description: "Record key".into(),
            validation_rules: vec![],
            default_value: None,
            enum_values: None,
            schema: json!({"type":"integer"}),
        }],
        enhanced_description: None,
        keywords: vec![],
        use_cases: vec![],
        composition_category: None,
        providing_agent_id: None,
    }];
    ctx
}
fn fixture_plan() -> ActionPlan {
    ActionPlan {
        scope: None,
        steps: (1..=3)
            .map(|key| ActionCandidate {
                id: format!("step-{key}"),
                call: ToolCall {
                    tool: "rail_fixture".into(),
                    arguments: json!({"key":key}),
                },
                bindings: vec![],
                reason: "Read the next requested record".into(),
            })
            .collect(),
    }
}
fn executors(
    plan: &ActionPlan,
) -> (
    ActionExecutors,
    Arc<crate::magician_v2::execution::multi_llm_agent_adapter::TestNativeResponseQueue>,
) {
    use crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse;
    let response = ExecutionNativeResponse::unadmitted(vec![ExecutionToolCall {
        id: "proposal".into(),
        name: decision_engine_contract::action::PLANNER_TOOL.into(),
        arguments: json!({"steps":plan.steps}),
    }]);
    executors_with_response(response)
}

fn executors_with_response(
    response: crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse,
) -> (
    ActionExecutors,
    Arc<crate::magician_v2::execution::multi_llm_agent_adapter::TestNativeResponseQueue>,
) {
    use crate::magician_v2::execution::multi_llm_agent_adapter::{
        MultiLlmAgentAdapter, TestNativeResponseQueue,
    };
    use crate::magician_v2::prompts::{JsonPromptStorage, PromptManager};
    let queue = Arc::new(TestNativeResponseQueue::new(vec![response]));
    let executors = ActionExecutors::new(
        Arc::new(crate::magician_v2::test_utils::ConfigurableMockLlm::with_response("{}")),
        Arc::new(PromptManager::new(Arc::new(
            JsonPromptStorage::with_default_config().unwrap(),
        ))),
    )
    .with_native_adapter(Arc::new(MultiLlmAgentAdapter::new_with_test_native_queue(
        Arc::clone(&queue),
    )));
    (executors, queue)
}

#[actix_rt::test]
async fn decision_rail_native_planner_and_structured_continuations_use_real_host_lowering() {
    let service = service().await;
    let ctx = fixture_context();
    let (executors, queue) = executors(&fixture_plan());
    let mut protective = LoopProtectiveState::default();
    for iteration in 1..=3 {
        let output = decide_with_client(
            service.client.clone(),
            &ctx,
            &executors,
            &EnvironmentState::Uninitialized,
            &ExecutionHistory::new(),
            &mut protective,
            &[],
            None,
            iteration,
        )
        .await
        .unwrap()
        .unwrap();
        let crate::magician_v2::execution::agentic::decision::Decision::Execute {
            candidates, ..
        } = output.decision_result.unwrap()
        else {
            panic!("expected a work call")
        };
        let ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } = &candidates.candidates[0].action
        else {
            panic!("expected the ordinary pack dispatch")
        };
        assert_eq!(capability_name, "rail_fixture");
        assert_eq!(resolved_params["key"], json!(iteration));
    }
    assert_eq!(
        queue.prompts().len(),
        1,
        "structured continuations must not call the planner"
    );
    assert!(protective.decision_rail_plan.is_none());
}

#[actix_rt::test]
async fn decision_rail_native_json_text_still_requires_engine_admission() {
    use crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse;
    let service = service().await;
    for (tool, allowed) in [("rail_fixture", true), ("unauthorized_work_tool", false)] {
        let mut plan = fixture_plan();
        plan.steps[0].call.tool = tool.into();
        let mut response = ExecutionNativeResponse::unadmitted(Vec::new());
        response.text = Some(serde_json::to_string(&plan).unwrap());
        let (executors, _) = executors_with_response(response);
        let output = decide_with_client(
            service.client.clone(),
            &fixture_context(),
            &executors,
            &EnvironmentState::Uninitialized,
            &ExecutionHistory::new(),
            &mut LoopProtectiveState::default(),
            &[],
            None,
            1,
        )
        .await;
        if allowed {
            let output = output.unwrap().unwrap();
            assert!(matches!(
                output.decision_result.unwrap(),
                crate::magician_v2::execution::agentic::decision::Decision::Execute { .. }
            ));
        } else {
            assert!(
                output.is_err(),
                "text proposals cannot bypass the authorized catalog"
            );
        }
    }
}

#[actix_rt::test]
async fn decision_rail_every_selected_harness_can_use_structured_selection_without_spawning() {
    let service = service().await;
    for name in [
        "magician",
        "pi",
        "claude_code",
        "codex",
        "codex_app_server",
        "grok",
        "agy",
    ] {
        let mut ctx = fixture_context();
        ctx.harness_engine = Some(name.into());
        let (executors, queue) = executors(&fixture_plan());
        let mut protective = LoopProtectiveState {
            decision_rail_plan: Some(fixture_plan()),
            ..Default::default()
        };
        let output = decide_with_client(
            service.client.clone(),
            &ctx,
            &executors,
            &EnvironmentState::Uninitialized,
            &ExecutionHistory::new(),
            &mut protective,
            &[],
            None,
            1,
        )
        .await
        .unwrap()
        .unwrap();
        assert!(output.decision_result.is_ok(), "engine {name}");
        assert!(
            queue.prompts().is_empty(),
            "engine {name} should not need a planner"
        );
        assert_eq!(protective.decision_rail_consecutive_steps, 1);
    }
}

#[actix_rt::test]
async fn decision_rail_cancellation_cannot_return_a_dispatchable_call() {
    let service = service().await;
    let ctx = fixture_context();
    let (executors, queue) = executors(&fixture_plan());
    let cancel = CancellationToken::new();
    cancel.cancel();
    let mut protective = LoopProtectiveState::default();
    let outcome = decide_with_client(
        service.client.clone(),
        &ctx,
        &executors,
        &EnvironmentState::Uninitialized,
        &ExecutionHistory::new(),
        &mut protective,
        &[],
        Some(&cancel),
        1,
    )
    .await;
    assert!(outcome.is_err());
    assert!(queue.prompts().is_empty());
}

#[actix_rt::test]
async fn decision_rail_shipped_browser_cua_and_android_schemas_need_no_tool_changes() {
    use tool_runtime_core::{
        action_overrides::compile_typed_action_overrides,
        manifest_parser::parse_skill_runtime_package,
        manifest_validation::validate_skill_runtime_contract,
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let mut cases = Vec::new();
    for (skill, action, args) in [
        ("browser", "click", json!({"args":["@e2"]})),
        ("browser", "snapshot", json!({"args":["-i"]})),
        (
            "macos-ui-automation",
            "call",
            json!({"action_name":"get_window_state","args_json":"{\"app\":\"Calculator\"}"}),
        ),
    ] {
        let text =
            std::fs::read_to_string(root.join("skillshub").join(skill).join("SKILL.md")).unwrap();
        let package = parse_skill_runtime_package(&text).unwrap().unwrap();
        let catalog = compile_typed_action_overrides(
            skill,
            validate_skill_runtime_contract(&package.contract).unwrap(),
            package.actions.as_ref().unwrap(),
        )
        .unwrap();
        let definition = &catalog.actions[action].definition;
        cases.push((
            definition.name.clone(),
            definition.description.clone(),
            definition.input_schema.clone(),
            args,
        ));
    }
    let android: crate::magician_v2::execution::capability::CapabilityPackDefinition =
        serde_yaml::from_str(
            &std::fs::read_to_string(
                root.join("magician/src/magician_v2/execution/embedded_pack_defs/android_act.yaml"),
            )
            .unwrap(),
        )
        .unwrap();
    let properties: serde_json::Map<String, Value> = android
        .parameters
        .iter()
        .map(|p| {
            (
                p.name.clone(),
                crate::magician_v2::execution::capability::derive_param_schema_for_emission(p),
            )
        })
        .collect();
    let required: Vec<_> = android
        .parameters
        .iter()
        .filter(|p| p.required)
        .map(|p| &p.name)
        .collect();
    cases.push((
        android.name.clone(),
        android.description.clone().unwrap_or_default(),
        json!({"type":"object","properties":properties,"required":required}),
        json!({"action":"tap","x":120,"y":200}),
    ));
    let engine = engine();
    for (name, description, parameters, arguments) in cases {
        let call = ToolCall {
            tool: name.clone(),
            arguments,
        };
        let req = ActionRequest {
            contract_version: CONTRACT_VERSION,
            snapshot: format!("schema-{name}"),
            locality: decision_engine_contract::wire::Locality::Cloud,
            context: ActionContext {
                goal: "Perform the next authorized test-fixture action".into(),
                ..Default::default()
            },
            tools: vec![ActionTool {
                name,
                description,
                parameters,
            }],
            plan: ActionPlan {
                scope: None,
                steps: vec![ActionCandidate {
                    id: "one".into(),
                    call: call.clone(),
                    bindings: vec![],
                    reason: "Fixture plan".into(),
                }],
            },
            phase: ActionPhase::Select,
            consecutive_gated_steps: 0,
        };
        let reply = engine.action(req).await;
        let ActionVerdict::Execute { call: selected, .. } = reply.verdict else {
            panic!("shipped schema failed: {reply:?}")
        };
        assert_eq!(selected, call);
        assert!(matches!(
            lower_native_tool_call(&ExecutionToolCall {
                id: "one".into(),
                name: selected.tool,
                arguments: selected.arguments
            }),
            NativeDecisionOutcome::Valid(_)
        ));
    }
}

#[actix_rt::test]
async fn decision_rail_charges_structured_usage_before_returning_a_call() {
    use crate::magician_v2::execution::agentic::types::{
        execution_token_budget_snapshot, with_execution_token_meter, ExecutionTokenBudgetError,
    };
    let service = service().await;
    let ctx = fixture_context();
    let (executors, queue) = executors(&fixture_plan());
    with_execution_token_meter(0, 1, async {
        let mut protective = LoopProtectiveState {
            decision_rail_plan: Some(fixture_plan()),
            ..Default::default()
        };
        let result = decide_with_client(
            service.client.clone(),
            &ctx,
            &executors,
            &EnvironmentState::Uninitialized,
            &ExecutionHistory::new(),
            &mut protective,
            &[],
            None,
            1,
        )
        .await;
        let error = match result {
            Err(error) => error,
            Ok(_) => panic!("over-budget call was admitted"),
        };
        assert!(
            error.downcast_ref::<ExecutionTokenBudgetError>().is_some(),
            "{error:#}"
        );
        assert!(execution_token_budget_snapshot().unwrap().0 > 1);
        assert!(queue.prompts().is_empty());
        assert_eq!(
            protective.decision_rail_plan.as_ref().unwrap().steps.len(),
            3
        );
    })
    .await;
}

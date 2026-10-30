//! Integration checks against the real host catalog, service and lowering path.
use super::*;
use decision_engine_contract::action::{ActionCandidate, ActionPlan};
use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::config::{DecisionConfig, DecisionOperationConfig};
use magician_decision::{Answer, BoundModel, DecisionRuntimeBuilder, OptionId, PackStore};
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
        name: crate::magician_v2::execution::agentic::native_catalog::SUBMIT_PLAN_TOOL.into(),
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

/// A plan the model submitted (`submit_plan`) is sticky: every step runs
/// through the real host lowering with no model asked, the planner included.
#[actix_rt::test]
async fn decision_rail_sticky_plan_runs_every_step_without_a_model() {
    let service = service().await;
    let ctx = fixture_context();
    let (executors, queue) = executors(&fixture_plan());
    let mut protective = LoopProtectiveState {
        decision_rail_plan: Some(fixture_plan()),
        ..Default::default()
    };
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
    assert!(queue.prompts().is_empty(), "a sticky plan asks no model");
    assert!(protective.decision_rail_plan.is_none());
}

/// Magician's own model proposes through its normal turn: with no grounded
/// step, the rail asks no planner and hands back a check, and every work
/// call the model then makes is validated by the engine before it runs.
#[actix_rt::test]
async fn decision_rail_magicians_own_model_decides_and_the_engine_checks_its_calls() {
    use crate::magician_v2::execution::agentic::decision::Decision;
    let service = service().await;
    let ctx = fixture_context();
    let (executors, queue) = executors(&fixture_plan());
    let mut protective = LoopProtectiveState::default();
    let (output, check) = decide_with_client_checked(
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
    .unwrap();
    assert!(output.is_none(), "the agent's model decides this step");
    assert!(queue.prompts().is_empty(), "no planner turn");
    let check = check.expect("the engine checks the model's calls");

    let decision = |tool: &str| {
        let NativeDecisionOutcome::Valid(envelope) = lower_native_tool_call(&ExecutionToolCall {
            id: "model".into(),
            name: tool.into(),
            arguments: json!({"key": 1}),
        }) else {
            panic!("lowers")
        };
        envelope.decision
    };
    let allowed = check
        .validate(&ctx, decision("rail_fixture"), &mut protective, None)
        .await
        .expect("an authorized call passes");
    assert!(matches!(allowed, Decision::Execute { .. }));
    let rejected = check
        .validate(
            &ctx,
            decision("unauthorized_work_tool"),
            &mut protective,
            None,
        )
        .await
        .expect_err("a call outside the catalog never runs");
    assert!(
        rejected
            .to_string()
            .contains("rejected `unauthorized_work_tool`"),
        "{rejected}"
    );
}

/// A step the catalog does not authorize, or a steer, drops the sticky plan;
/// the agent's model then decides.
#[actix_rt::test]
async fn decision_rail_drops_a_sticky_plan_on_an_unauthorized_step_or_a_steer() {
    let service = service().await;
    let mut unauthorized = fixture_plan();
    unauthorized.steps[0].call.tool = "unauthorized_work_tool".into();
    for (plan, steer) in [
        (unauthorized, Vec::new()),
        (
            fixture_plan(),
            vec!["Stop and summarise instead".to_string()],
        ),
    ] {
        let (executors, queue) = executors(&fixture_plan());
        let mut protective = LoopProtectiveState {
            decision_rail_plan: Some(plan),
            ..Default::default()
        };
        let output = decide_with_client(
            service.client.clone(),
            &fixture_context(),
            &executors,
            &EnvironmentState::Uninitialized,
            &ExecutionHistory::new(),
            &mut protective,
            &steer,
            None,
            1,
        )
        .await
        .unwrap();
        assert!(output.is_none(), "the agent's model decides afresh");
        assert!(queue.prompts().is_empty(), "no planner turn");
        assert!(
            protective.decision_rail_plan.is_none(),
            "the plan is dropped"
        );
    }
}

/// A plan a harness just submitted was made knowing the failure before it, so
/// that failure does not drop it; an older sticky plan is dropped by it.
#[actix_rt::test]
async fn decision_rail_a_freshly_submitted_plan_survives_the_failure_before_it() {
    use crate::magician_v2::execution::agentic::types::{
        ActionOutcomeCategory, ActionResultRecord, IterationRecord,
    };
    let service = service().await;
    let mut history = ExecutionHistory::new();
    history.iterations.push(IterationRecord {
        iteration: 1,
        timestamp: chrono::Utc::now(),
        state_before: EnvironmentState::Uninitialized,
        action:
            crate::magician_v2::execution::agentic::executor::browser_observation_history_action(),
        result: ActionResultRecord {
            success: false,
            output: Some("The plan you submitted did not start".into()),
            error: Some("rejected".into()),
            duration_ms: 0,
            outcome_category: Some(ActionOutcomeCategory::Failed),
            api_replay_used: None,
            api_replay_time_ms: None,
            browser_fallback_reason: None,
            tool_result_projection: None,
        },
        state_after: EnvironmentState::Uninitialized,
        verification: None,
        llm_reasoning: None,
    });
    for (fresh, runs) in [(true, true), (false, false)] {
        let (executors, queue) = executors(&fixture_plan());
        let mut protective = LoopProtectiveState {
            decision_rail_plan: Some(fixture_plan()),
            decision_rail_plan_fresh: fresh,
            ..Default::default()
        };
        let output = decide_with_client(
            service.client.clone(),
            &fixture_context(),
            &executors,
            &EnvironmentState::Uninitialized,
            &history,
            &mut protective,
            &[],
            None,
            2,
        )
        .await
        .unwrap();
        assert_eq!(output.is_some(), runs, "fresh {fresh}");
        assert!(
            !protective.decision_rail_plan_fresh,
            "the mark is used once"
        );
        assert!(queue.prompts().is_empty());
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
        // A sticky plan step is resolved, not judged by Jev.
        assert_eq!(protective.decision_rail_consecutive_steps, 0);
        assert_eq!(
            protective.decision_rail_plan.as_ref().unwrap().steps.len(),
            2
        );
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
        let mut protective = LoopProtectiveState::default();
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
    })
    .await;
}

/// On the rail, a run whose every micro-goal is resolved takes the same
/// definition-of-done turn the native path does: the run's own model, tools
/// forbidden. The same micro-goal set is not answered twice. New operator steer
/// reopens the normal decision path instead (covered separately below).
#[actix_rt::test]
async fn decision_rail_definition_of_done_takes_one_final_no_tools_turn() {
    use crate::magician_v2::execution::agentic::decision::Decision;
    use crate::magician_v2::execution::agentic::native_types::ExecutionNativeResponse;
    let service = service().await;
    let mut ctx = fixture_context();
    ctx.task_state = Some(
        json!({
            "task_id": "task-rail-dod",
            "micro_goals": [
                {"id": "read", "status": "completed", "description": "Read the three records."},
            ],
        })
        .to_string(),
    );
    let answer = "FINAL_ANSWER_MARKER: the three records, summarised.";
    let mut response = ExecutionNativeResponse::unadmitted(Vec::new());
    response.text = Some(answer.to_string());
    response.finish_reason = Some("stop".to_string());
    let (executors, queue) = executors_with_response(response);
    let mut protective = LoopProtectiveState::default();

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
    .expect("the definition-of-done turn decides");
    let Ok(Decision::Completed { evidence, .. }) = output.decision_result else {
        panic!("expected Completed, got {:?}", output.decision_result);
    };
    assert_eq!(evidence.as_deref(), Some(answer));
    let prompts = queue.prompts();
    assert_eq!(prompts.len(), 1, "one final turn");
    assert!(prompts[0].contains("FINAL ANSWER (tools are disabled for this turn)"));
    assert_eq!(queue.tool_choices(), vec![Some(json!({"type": "none"}))]);
    assert_eq!(protective.definition_of_done_turns, 1);

    // The same micro-goals are not answered again: the model is not asked
    // for another final turn.
    let _ = decide_with_client(
        service.client.clone(),
        &ctx,
        &executors,
        &EnvironmentState::Uninitialized,
        &ExecutionHistory::new(),
        &mut protective,
        &[],
        None,
        2,
    )
    .await;
    assert_eq!(protective.definition_of_done_turns, 1);
    assert!(
        queue
            .prompts()
            .iter()
            .filter(|prompt| prompt.contains("FINAL ANSWER (tools are disabled"))
            .count()
            <= 1
    );
}

#[actix_rt::test]
async fn decision_rail_definition_of_done_keeps_tools_for_new_operator_steer() {
    use crate::magician_v2::execution::agentic::decision::Decision;
    let service = service().await;
    let mut ctx = fixture_context();
    ctx.task_state = Some(
        json!({
            "task_id": "task-rail-dod",
            "micro_goals": [
                {"id": "read", "status": "completed", "description": "Read the three records."},
            ],
        })
        .to_string(),
    );
    let (executors, queue) = executors(&fixture_plan());
    let mut protective = LoopProtectiveState::default();
    let (output, check) = decide_with_client_checked(
        service.client.clone(),
        &ctx,
        &executors,
        &EnvironmentState::Uninitialized,
        &ExecutionHistory::new(),
        &mut protective,
        &["STEER_MARKER: read the updated records".to_string()],
        None,
        1,
    )
    .await
    .unwrap();
    assert!(
        output.is_none(),
        "the agent's normal turn decides the new work"
    );
    assert_eq!(protective.definition_of_done_turns, 0);
    assert!(queue.prompts().is_empty(), "no premature final-answer turn");
    let check = check.expect("the engine checks the model's subsequent calls");
    assert!(check
        .request
        .context
        .planner_context
        .contains("STEER_MARKER"));
    assert!(!check
        .request
        .context
        .planner_context
        .contains("FINAL ANSWER (tools are disabled for this turn)"));
    let NativeDecisionOutcome::Valid(envelope) = lower_native_tool_call(&ExecutionToolCall {
        id: "model".into(),
        name: "rail_fixture".into(),
        arguments: json!({"key": 1}),
    }) else {
        panic!("the normal tool call lowers")
    };
    let allowed = check
        .validate(&ctx, envelope.decision, &mut protective, None)
        .await
        .expect("the new work retains its authorized tools");
    assert!(matches!(allowed, Decision::Execute { .. }));
}

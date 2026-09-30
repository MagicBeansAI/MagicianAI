use super::super::models::StoredToolCall;
use super::*;
use magician_decision::adapters::MemoryDecisionModel;
use magician_decision::config::{DecisionConfig, DecisionOperationConfig};
use magician_decision::{Answer, BoundModel, DecisionRuntimeBuilder, OptionId, PackStore};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
fn engine() -> decision_engine::Engine {
    let model = Arc::new(
        MemoryDecisionModel::new("memory", "scripted")
            .with_answer(
                "next_action",
                Answer::Choice {
                    choice: OptionId::new("plan:0"),
                    confidence: 0.99,
                    probabilities: BTreeMap::from([(OptionId::new("plan:0"), 0.99)]),
                },
            )
            .with_answer("evidence_sufficient", Answer::Noul { noul: 0.99 })
            .with_answer("action_applicable", Answer::Noul { noul: 0.99 }),
    );
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

pub(crate) struct Service {
    pub(crate) client: decision_engine_contract::client::EngineClient,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
    socket: std::path::PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.socket);
    }
}
pub(crate) async fn service() -> Service {
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

fn tools() -> Vec<LLMToolSpec> {
    vec![LLMToolSpec {
        name: "fixture_read".into(),
        description: "Read the requested record".into(),
        parameters: json!({"type":"object","properties":{"key":{"type":"integer"}},"required":["key"],"additionalProperties":false}),
    }]
}
fn proposed_response() -> ChatLlmResponse {
    let mut response = selected_response(ToolCall {
        tool: "fixture_read".into(),
        arguments: json!({"key":1}),
    });
    response.tool_calls.push(LLMToolCall {
        id: "second".into(),
        name: "fixture_read".into(),
        arguments: json!({"key":2}),
    });
    response.provider_state = Some(
        super::super::models::AssistantProviderState::OpenaiResponses {
            response_id: "original-two-call-response".into(),
            tool_protocol_repair_checkpoint: false,
        },
    );
    response
}
fn completed(response: &ChatLlmResponse, value: Value) -> Vec<ChatLlmTranscriptEntry> {
    vec![
        ChatLlmTranscriptEntry::AssistantTurn {
            text: None,
            tool_calls: response
                .tool_calls
                .iter()
                .map(|call| StoredToolCall {
                    id: call.id.clone(),
                    name: call.name.clone(),
                    arguments: call.arguments.clone(),
                })
                .collect(),
            provider_state: None,
        },
        ChatLlmTranscriptEntry::ToolResult {
            tool_call_id: response.tool_calls[0].id.clone(),
            tool_name: Some("fixture_read".into()),
            content: value.to_string(),
        },
    ]
}

#[actix_rt::test]
async fn chat_decision_rail_validates_native_batch_and_jev_selects_its_continuation() {
    let service = service().await;
    let cancel = CancellationToken::new();
    let mut rail = ChatDecisionRail::default();
    assert!(matches!(
        rail.prepare_with_client(
            service.client.clone(),
            "Read records 1 and 2",
            "",
            &[],
            &tools(),
            PlannerMode::ChatNative,
            &cancel
        )
        .await
        .unwrap(),
        ChatDecision::Planner { .. }
    ));
    let mut response = proposed_response();
    let first_id = response.tool_calls[0].id.clone();
    rail.resolve_native(&mut response, &cancel).await.unwrap();
    assert_eq!(response.tool_calls.len(), 1);
    assert_eq!(response.tool_calls[0].id, first_id);
    assert!(response.provider_state.is_none());
    assert_eq!(rail.plan.steps.len(), 1);
    let history = completed(&response, json!({"status":"ok","record":1}));
    let next = rail
        .prepare_with_client(
            service.client.clone(),
            "Read records 1 and 2",
            "",
            &history,
            &tools(),
            PlannerMode::ChatNative,
            &cancel,
        )
        .await
        .unwrap();
    let ChatDecision::Execute(next) = next else {
        panic!("structured continuation expected")
    };
    assert_eq!(next.arguments, json!({"key":2}));
    assert_eq!(rail.consecutive, 1);
    assert!(rail.plan.steps.is_empty());
    assert!(selected_response(next).telemetry.is_none());
}

#[actix_rt::test]
async fn chat_decision_rail_rejects_unknown_native_call_before_dispatch() {
    let service = service().await;
    let cancel = CancellationToken::new();
    let mut rail = ChatDecisionRail::default();
    rail.prepare_with_client(
        service.client.clone(),
        "Read",
        "",
        &[],
        &tools(),
        PlannerMode::ChatNative,
        &cancel,
    )
    .await
    .unwrap();
    let mut response = selected_response(ToolCall {
        tool: "unauthorized_mutation".into(),
        arguments: json!({}),
    });
    assert!(rail.resolve_native(&mut response, &cancel).await.is_err());
    assert!(rail.plan.steps.is_empty());
}

#[actix_rt::test]
async fn chat_decision_rail_cancellation_and_socket_failure_never_authorize_calls() {
    let service = service().await;
    let cancel = CancellationToken::new();
    let mut rail = ChatDecisionRail::default();
    rail.prepare_with_client(
        service.client.clone(),
        "Read",
        "",
        &[],
        &tools(),
        PlannerMode::ChatNative,
        &cancel,
    )
    .await
    .unwrap();
    cancel.cancel();
    assert!(rail
        .resolve_native(&mut proposed_response(), &cancel)
        .await
        .is_err());
    let missing = EngineClient::new(
        format!("/tmp/missing-{}.sock", uuid::Uuid::new_v4()),
        Duration::from_millis(100),
    );
    assert!(rail
        .prepare_with_client(
            missing,
            "Read",
            "",
            &[],
            &tools(),
            PlannerMode::ChatNative,
            &CancellationToken::new()
        )
        .await
        .is_err());
    assert!(rail.pending.is_none());
}

#[actix_rt::test]
async fn chat_decision_rail_failed_results_and_changed_catalog_force_replanning() {
    for failure in [true, false] {
        let service = service().await;
        let cancel = CancellationToken::new();
        let mut rail = ChatDecisionRail::default();
        rail.prepare_with_client(
            service.client.clone(),
            "Read",
            "",
            &[],
            &tools(),
            PlannerMode::ChatNative,
            &cancel,
        )
        .await
        .unwrap();
        let mut response = proposed_response();
        rail.resolve_native(&mut response, &cancel).await.unwrap();
        let history = completed(
            &response,
            if failure {
                json!({"status":"error","reason":"record vanished"})
            } else {
                json!({"status":"ok"})
            },
        );
        let allowed = if failure { tools() } else { Vec::new() };
        let decision = rail
            .prepare_with_client(
                service.client.clone(),
                "Read",
                "",
                &history,
                &allowed,
                PlannerMode::ChatNative,
                &cancel,
            )
            .await;
        assert!(!matches!(decision, Ok(ChatDecision::Execute(_))));
    }
}

#[test]
fn chat_decision_rail_evidence_is_bounded_and_tracks_failed_calls() {
    let response = proposed_response();
    let history = completed(&response, json!({"status":"error","reason":"failed"}));
    let evidence = evidence(&history);
    assert_eq!(evidence[0].succeeded, Some(false));
    assert_eq!(
        evidence[0].call.as_ref().unwrap().arguments,
        json!({"key":1})
    );
    let huge = json!({"content":"a".repeat(80 * 1024)});
    assert!(bounded_value(huge).get("evidence_unavailable").is_some());
}

#[test]
fn chat_decision_rail_projected_empty_success_and_pending_are_distinct() {
    use crate::magician_v2::tool_result_projection::*;
    let response = proposed_response();
    let call = &response.tool_calls[0];
    for (status, expected) in [
        (ToolOutcomeStatus::Succeeded, Some(true)),
        (ToolOutcomeStatus::Pending, None),
        (ToolOutcomeStatus::Failed, Some(false)),
    ] {
        let raw = RawResultDescriptor {
            content_ref: ScopedResultRef { result_ref: "result_ref_opaque".into(), cursor: None },
            content_hash: "raw-hash".into(), media_type: "application/json".into(),
            size_bytes: 28, retention_class: ResultRetentionClass::ChatSession,
        };
        let projection = ToolResultProjector::default().project(ToolResultProjectionRequest {
            identity: ToolResultIdentity {
                tool_name: call.name.clone(), tool_call_id: call.id.clone(), execution_id: None,
                task_id: None, scope_digest: "scope".into(), authority_revision: "revision".into(),
            },
            outcome: ToolOutcome::with_status(status), raw_result: &json!({"kind":"text","content":""}),
            display: DisplayResultProjection::referenced(&raw), raw, spoken_hint: None,
            contract_id: None, budget: ProjectionBudget::default(),
        }).unwrap();
        let mut history = completed(&response, json!({}));
        *history.last_mut().unwrap() = ChatLlmTranscriptEntry::ToolResultProjected {
            tool_call_id: call.id.clone(), tool_name: Some(call.name.clone()), projection,
        };
        assert_eq!(evidence(&history).last().unwrap().succeeded, expected);
    }
}

#[actix_rt::test]
async fn chat_decision_rail_final_prose_never_becomes_a_tool_call() {
    let service = service().await;
    let cancel = CancellationToken::new();
    let mut rail = ChatDecisionRail::default();
    rail.prepare_with_client(
        service.client.clone(),
        "Hello",
        "",
        &[],
        &tools(),
        PlannerMode::ChatNative,
        &cancel,
    )
    .await
    .unwrap();
    let mut response = proposed_response();
    response.tool_calls.clear();
    response.content = Some("Hello!".into());
    rail.resolve_native(&mut response, &cancel).await.unwrap();
    assert_eq!(response.content.as_deref(), Some("Hello!"));
    assert!(rail.pending.is_none());
    assert!(response.tool_calls.is_empty());
}

#[test]
fn chat_decision_rail_reused_provider_call_ids_do_not_mix_evidence() {
    let mut first = selected_response(ToolCall {
        tool: "fixture_read".into(),
        arguments: json!({"key":1}),
    });
    first.tool_calls[0].id = "call-0".into();
    let mut second = first.clone();
    second.tool_calls[0].arguments = json!({"key":2});
    let mut history = completed(&first, json!({"status":"ok","record":1}));
    history.extend(completed(&second, json!({"status":"ok","record":2})));
    let evidence = evidence(&history);
    assert_ne!(evidence[0].id, evidence[1].id);
    assert_eq!(evidence[0].call.as_ref().unwrap().arguments["key"], 1);
    assert_eq!(evidence[1].call.as_ref().unwrap().arguments["key"], 2);
}

#[test]
fn chat_decision_rail_keeps_followup_context_without_provider_state() {
    let history = vec![
        ChatLlmTranscriptEntry::AssistantTurn {
            text: Some("Read record 7?".into()),
            tool_calls: Vec::new(),
            provider_state: Some(
                super::super::models::AssistantProviderState::OpenaiResponses {
                    response_id: "private-provider-state".into(),
                    tool_protocol_repair_checkpoint: false,
                },
            ),
        },
        ChatLlmTranscriptEntry::UserText { text: "Yes".into() },
    ];
    let state = context("Yes", "", &history, PlannerMode::ChatNative);
    assert!(state.observation.to_string().contains("Read record 7?"));
    assert!(!state
        .observation
        .to_string()
        .contains("private-provider-state"));
    assert!(state.planner_context.is_empty());
}

#[actix_rt::test]
async fn chat_decision_rail_preserves_native_provider_state_for_an_unchanged_single_call() {
    let service = service().await;
    let cancel = CancellationToken::new();
    let mut rail = ChatDecisionRail::default();
    rail.prepare_with_client(
        service.client.clone(),
        "Read",
        "",
        &[],
        &tools(),
        PlannerMode::ChatNative,
        &cancel,
    )
    .await
    .unwrap();
    let mut response = proposed_response();
    response.tool_calls.pop();
    let original = response.provider_state.clone();
    rail.resolve_native(&mut response, &cancel).await.unwrap();
    assert_eq!(response.provider_state, original);
}

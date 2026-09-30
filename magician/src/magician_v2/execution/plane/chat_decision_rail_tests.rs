use super::*;
use crate::magician_v2::agents::{
    AgentInvocationContext, FeatureMode, InvocationSourceKind, InvocationSurface,
};
use crate::magician_v2::execution::agentic::AgenticContext;
use decision_engine_contract::action::{ActionCandidate, ToolCall};
use serde_json::json;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};
use tokio_util::sync::CancellationToken;

struct ScriptedPlanner {
    rounds: AtomicUsize,
    engine: String,
    fail_final: bool,
    unauthorized: bool,
}
#[async_trait::async_trait]
impl ChatPlanner for ScriptedPlanner {
    async fn propose(
        &self,
        ctx: &AgenticContext,
        system: &str,
        _prompt: &str,
        _images: Vec<Value>,
        _seconds: u64,
        _cancel: &CancellationToken,
    ) -> Result<(Option<ActionPlan>, HarnessTurnSettled)> {
        assert_eq!(ctx.harness_engine.as_deref(), Some(self.engine.as_str()));
        assert!(system.contains("proposals"));
        let round = self.rounds.fetch_add(1, Ordering::SeqCst);
        if round > 0 && self.fail_final {
            return Err(anyhow!("planner unavailable"));
        }
        let plan = (round == 0).then(|| ActionPlan {
            scope: None,
            steps: (1..=2)
                .map(|key| ActionCandidate {
                    id: format!("step-{key}"),
                    call: ToolCall {
                        tool: if self.unauthorized {
                            "not_granted"
                        } else {
                            "fixture_read"
                        }
                        .into(),
                        arguments: json!({"key":key}),
                    },
                    bindings: Vec::new(),
                    reason: "Read requested record".into(),
                })
                .collect(),
        });
        Ok((
            plan,
            HarnessTurnSettled {
                assistant_text: "Read both records.".into(),
                stop_reason: HarnessStopReason::Settled,
                usage: Some(HarnessUsage {
                    input_tokens: 10,
                    output_tokens: 5,
                    cached_input_tokens: 2,
                    ..Default::default()
                }),
                native_session_id: None,
            },
        ))
    }
}

async fn workflow(
    engine: &str,
    fail_final: bool,
    unauthorized: bool,
    cancel_after_first: bool,
    timeout_during_tool: bool,
) -> (ChatHarnessTurnOutcome, usize, usize) {
    let service = crate::magician_v2::chat::decision_rail::tests::service().await;
    let invocation = AgentInvocationContext {
        principal: "owner".into(),
        workspace: "home".into(),
        source_agent_id: None,
        target_agent_id: "personal-assistant".into(),
        surface: InvocationSurface::Chat,
        feature_mode: FeatureMode::None,
        source_kind: InvocationSourceKind::Direct,
        chat_session_id: Some(uuid::Uuid::new_v4().to_string()),
        chat_turn_id: Some("test-turn".into()),
    };
    let cancel = CancellationToken::new();
    let dispatches = Arc::new(AtomicUsize::new(0));
    let count = dispatches.clone();
    let bridge_cancel = cancel.clone();
    let bridge: super::super::mouth_bridge::ChatMouthBridge =
        Arc::new(move |_id, name, args, _cancel| {
            assert_eq!(name, "fixture_read");
            count.fetch_add(1, Ordering::SeqCst);
            if cancel_after_first {
                bridge_cancel.cancel();
            }
            Box::pin(async move {
                if timeout_during_tool {
                    _cancel.cancelled().await;
                }
                json!({"status":"ok","record":args["key"]})
            })
        });
    let spec = LLMToolSpec {
        name: "fixture_read".into(),
        description: "Read requested record".into(),
        parameters: json!({"type":"object","properties":{"key":{"type":"integer"}},"required":["key"],"additionalProperties":false}),
    };
    let mut ctx = AgenticContext::default();
    ctx.principal = Some(invocation.principal.clone());
    ctx.workspace = Some(invocation.workspace.clone());
    ctx.agent_id = Some(invocation.target_agent_id.clone());
    ctx.invocation_context_override = Some(invocation.clone());
    ctx.harness_engine = Some(engine.into());
    let mut grant = PlaneGrant::for_conversation(
        ctx,
        invocation.chat_session_id.clone().unwrap(),
        cancel.clone(),
    )
    .with_bridged_tools(
        BTreeMap::from([(spec.name.clone(), spec.clone())]),
        bridge.clone(),
    )
    .with_turn_tool_budget(8, 0);
    grant.allowed_tools = vec![spec.name.clone()];
    let postures = HashMap::new();
    let request = ChatHarnessTurnRequest {
        invocation: &invocation,
        system_prompt: "Read only the two requested records.",
        history: &[],
        user_text: "Read records 1 and 2",
        cancel,
        disclosure_guarded: false,
        token_sink: None,
        approval_rules: &[],
        policy_snapshot: None,
        plane_postures: &postures,
        tool_index: None,
        trust_level: None,
        trust_enforcer: None,
        trust_policies_path: None,
        mouth_tool_specs: std::slice::from_ref(&spec),
        mouth_bridge: Some(bridge),
        choice: None,
        pi_profile: None,
        pi_images: Vec::new(),
    };
    let planner = ScriptedPlanner {
        rounds: AtomicUsize::new(0),
        engine: engine.into(),
        fail_final,
        unauthorized,
    };
    let snapshot = ChatHarnessSnapshot {
        turn_max_seconds: if timeout_during_tool { 1 } else { 60 },
        ..Default::default()
    };
    let outcome =
        run_with_planner(&request, &snapshot, grant, service.client.clone(), &planner).await;
    if timeout_during_tool {
        assert!(
            !request.cancel.is_cancelled(),
            "turn deadline must not cancel its parent conversation"
        );
    }
    (
        outcome,
        dispatches.load(Ordering::SeqCst),
        planner.rounds.load(Ordering::SeqCst),
    )
}

#[actix_rt::test]
async fn chat_decision_rail_foreign_loop_preserves_every_engine_and_dispatches_once() {
    for engine in [
        "pi",
        "claude_code",
        "codex",
        "codex_app_server",
        "grok",
        "agy",
    ] {
        let (outcome, calls, rounds) = workflow(engine, false, false, false, false).await;
        assert_eq!(
            outcome.settled.stop_reason,
            HarnessStopReason::Settled,
            "{engine}: {}",
            outcome.settled.assistant_text
        );
        assert_eq!(calls, 2, "{engine}");
        assert_eq!(
            rounds, 2,
            "initial proposal + final answer; middle step is structured"
        );
        assert_eq!(outcome.tool_calls.len(), 2);
        assert_eq!(outcome.settled.usage.unwrap().input_tokens, 20);
        assert!(outcome.settled.native_session_id.is_none());
    }
}

#[actix_rt::test]
async fn chat_decision_rail_foreign_failure_preserves_work_without_replay() {
    let (outcome, calls, _) = workflow("pi", true, false, false, false).await;
    assert_eq!(outcome.settled.stop_reason, HarnessStopReason::Refused);
    assert_eq!(calls, 2);
    assert_eq!(outcome.tool_calls.len(), 2);
    assert!(
        outcome.settled.usage.is_none(),
        "failed planner usage is unknown"
    );
}

#[actix_rt::test]
async fn chat_decision_rail_foreign_cancel_and_bad_proposals_cannot_dispatch_more_work() {
    let (outcome, calls, _) = workflow("codex", false, true, false, false).await;
    assert_eq!(outcome.settled.stop_reason, HarnessStopReason::Refused);
    assert_eq!(calls, 0);
    assert!(outcome.tool_calls.is_empty());
    let (outcome, calls, _) = workflow("claude_code", false, false, true, false).await;
    assert_eq!(outcome.settled.stop_reason, HarnessStopReason::Cancelled);
    assert_eq!(calls, 1);
    assert_eq!(outcome.tool_calls.len(), 1);
}

#[actix_rt::test]
async fn chat_decision_rail_deadline_cancels_in_flight_work_and_keeps_its_ledger() {
    let (outcome, calls, _) = workflow("pi", false, false, false, true).await;
    assert_eq!(
        outcome.settled.stop_reason,
        HarnessStopReason::TurnBudgetSpent
    );
    assert_eq!(calls, 1);
    assert_eq!(outcome.tool_calls.len(), 1);
}

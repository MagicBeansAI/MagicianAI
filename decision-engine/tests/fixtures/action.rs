use decision_engine::Engine;
use decision_engine_contract::action::*;
use decision_engine_contract::wire::{DecideRequest, DecideStatus, Locality, CONTRACT_VERSION};
use magician_decision::config::DecisionConfig;
use magician_decision::request::DecisionState;
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn settings(model: Value) -> DecisionConfig {
    serde_json::from_value(json!({"enabled":true,"models":{"tested":model},
        "operations":{"tool_action_judge":{"model":"tested","pack":"tool_action_judge",
            "pack_version":"1.0.0","sees_body":true,"gate":{"enabled":true},
            "thresholds":{"next_action_confidence":0.9,"evidence_sufficient":0.9,"action_applicable":0.9}}}})).unwrap()
}

pub fn request(tool: &str, locality: Locality) -> ActionRequest {
    ActionRequest {
        contract_version: CONTRACT_VERSION,
        snapshot: format!("fixture:{tool}:fresh"),
        locality,
        context: ActionContext {
            goal: "Click the Continue button to advance to the next page.".into(),
            observation: json!({"current_page":"Welcome","elements":[{"target":"@e7","role":"button","text":"Continue"}]}),
            evidence: vec![ActionEvidence {
                id: "snapshot-1".into(),
                value: json!({"target":"@e7","text":"Continue","role":"button","enabled":true}),
                call: None,
                succeeded: Some(true),
            }],
            ..Default::default()
        },
        tools: vec![ActionTool {
            name: tool.into(),
            description: "Click the target element from the current snapshot.".into(),
            parameters: json!({"type":"object","properties":{"target":{"type":"string"}},"required":["target"],"additionalProperties":false}),
        }],
        plan: ActionPlan::default(),
        phase: ActionPhase::Select,
        consecutive_gated_steps: 0,
    }
}

#[allow(dead_code)]
pub async fn verify_local(model: Value, adapter: &str) {
    let engine = Engine::from_config(settings(model));
    let first = engine
        .action(request("example__click", Locality::Local))
        .await;
    assert_eq!(
        first.model.as_ref().expect("local model answered").adapter,
        adapter,
        "{first:?}"
    );
    assert!(first.usage.as_ref().unwrap().input_tokens > 0);
    assert!(matches!(
        first.verdict,
        ActionVerdict::Execute { .. } | ActionVerdict::NeedPlanner { .. }
    ));
    let typed = DecideRequest {
        batch: Default::default(),
        contract_version: CONTRACT_VERSION,
        operation: ACTION_OPERATION.into(),
        locality: Locality::Local,
        state: DecisionState::from_json(
            json!({"goal":"Read record 42.","selected_call":{"tool":"lookup","arguments":{"key":42}},"evidence":[{"key":42}]}),
        ),
        choice_candidates: BTreeMap::from([(
            "next_action".into(),
            vec![
                ("read".into(), "Read record 42".into()),
                ("need_planner".into(), "Ask the planner".into()),
            ],
        )]),
    };
    let a = engine.decide(typed.clone()).await;
    let b = engine.decide(typed).await;
    assert_eq!(a.status, DecideStatus::Answered, "{a:?}");
    assert_eq!(a.response.as_ref().unwrap().answers.len(), 3);
    assert_eq!(
        a.response.as_ref().unwrap().answers,
        b.response.unwrap().answers
    );
    eprintln!(
        "[{adapter}] shared action: {} ms, {}; typed {} ms",
        first.latency_ms, first.reason, a.latency_ms
    );
}

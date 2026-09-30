//! Tests for the remaining PlanGraph lowering compatibility path.
//!
//! Browser PlanGraph steps deliberately lower to the generic browser pack now;
//! primitive browser work belongs to the browser inner loop and pinned
//! `agent-browser` CLI, not to deterministic Magicutor direct-action
//! construction.

use serde_json::json;

use crate::magician_v2::{
    execution::{
        actions::ExecutableAction,
        lowering::{lower_plan_to_executable_steps, topological_sort},
    },
    strategy::plan::{PlanGraph, PlanStep},
};

fn make_step(id: &str, tool: &str, task: &str, parameters: serde_json::Value) -> PlanStep {
    let parameters = parameters
        .as_object()
        .map(|obj| {
            obj.iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()
        })
        .unwrap_or_default();

    PlanStep {
        id: id.to_string(),
        task: task.to_string(),
        tool: Some(tool.to_string()),
        parameters,
        expected_outputs: vec![],
        depends_on: vec![],
        timeout_override_secs: None,
        confidence: 1.0,
        metadata: Default::default(),
        ..Default::default()
    }
}

fn make_plan(steps: Vec<PlanStep>) -> PlanGraph {
    PlanGraph {
        steps,
        edges: vec![],
        planning_metadata: Default::default(),
        unresolved_inputs: vec![],
        response_contract: Default::default(),
        ..Default::default()
    }
}

#[test]
fn browser_plan_step_lowers_to_primitive_pack() {
    let plan = make_plan(vec![make_step(
        "open-page",
        "browser",
        "Open the dashboard and inspect the first card",
        json!({
            "action": "navigate",
            "url": "https://example.com",
            "connection_mode": "headed"
        }),
    )]);

    let steps = lower_plan_to_executable_steps(&plan).expect("lowering should succeed");
    assert_eq!(steps.len(), 1);

    match steps[0].inner_action() {
        ExecutableAction::Pack {
            capability_name,
            resolved_params,
            ..
        } => {
            assert_eq!(capability_name, "browser");
            assert_eq!(resolved_params["connection_mode"], "headed");
            assert_eq!(resolved_params["action"], "navigate");
            assert_eq!(resolved_params["url"], "https://example.com");
            assert!(resolved_params["intent"]
                .as_str()
                .unwrap()
                .contains("Open the dashboard"));
        },
        other => panic!("expected browser pack action, got {other:?}"),
    }
}

#[test]
fn topological_sort_preserves_dependency_order() {
    let plan = make_plan(vec![
        make_step("b", "shell", "second", json!({"command": "echo b"})),
        {
            let mut step = make_step("a", "shell", "first", json!({"command": "echo a"}));
            step.depends_on = vec![];
            step
        },
        {
            let mut step = make_step("c", "shell", "third", json!({"command": "echo c"}));
            step.depends_on = vec!["a".to_string(), "b".to_string()];
            step
        },
    ]);

    let order = topological_sort(&plan).expect("sort should succeed");
    let ordered_ids: Vec<_> = order
        .into_iter()
        .map(|idx| plan.steps[idx].id.as_str())
        .collect();
    assert_eq!(ordered_ids.last().copied(), Some("c"));
}

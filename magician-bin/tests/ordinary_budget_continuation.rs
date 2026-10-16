//! Real inbox publication and production Attention projection regressions.
use magician::magician_v2::artifact_v2::{
    models::{CanonicalEvent, EventRefIds, ExecutionTreeNode, TaskRecord},
    service::build_attention_summaries,
    workspace::ArtifactV2Workspace,
};
use magician::magician_v2::execution::agentic::run_loop::steer_inbox as inbox;
use serde_json::json;
use std::collections::HashMap;

#[tokio::test]
async fn long_run_budget_pause_retains_accepted_input_for_the_owner_successor() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
    inbox::activate_control_generation(&workspace, "owner", "default", "exec", "controls")
        .await
        .unwrap();
    inbox::enqueue(
        &workspace,
        "owner",
        "default",
        "exec",
        "controls",
        "Keep the local result concise",
    )
    .await
    .unwrap();
    inbox::admit_resumable_budget_pause(
        &workspace, "owner", "default", "exec", "controls", "exec", 20,
    )
    .await
    .unwrap();
    assert!(
        inbox::enqueue(&workspace, "owner", "default", "exec", "controls", "Too late")
            .await
            .is_err()
    );
    assert!(
        inbox::claim(&workspace, "owner", "default", "exec", "controls", "exec-r20", 1)
            .await
            .is_err()
    );
    inbox::rebind_terminal_fence_to_successor(&workspace, "owner", "default", "exec", "exec-r20")
        .await
        .unwrap();
    inbox::reopen_after_nonterminal_boundary(&workspace, "owner", "default", "exec", "exec-r20", 1)
        .await
        .unwrap();
    let claimed = inbox::claim(
        &workspace, "owner", "default", "exec", "controls", "exec-r20", 1,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(claimed.messages, ["Keep the local result concise"]);
    inbox::acknowledge(
        &workspace,
        "owner",
        "default",
        "exec",
        "exec-r20",
        &claimed.receipt,
    )
    .await
    .unwrap();
    assert!(
        inbox::claim(&workspace, "owner", "default", "exec", "controls", "exec-r20", 2)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn long_run_budget_pause_cannot_reopen_a_cancelled_control_epoch() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = ArtifactV2Workspace::new(temp.path().join("workspace"));
    inbox::activate_control_generation(&workspace, "owner", "default", "exec", "controls")
        .await
        .unwrap();
    inbox::supersede_generation_for_terminal(
        &workspace, "owner", "default", "exec", "controls", "exec", 20,
    )
    .await
    .unwrap();
    assert!(inbox::admit_resumable_budget_pause(
        &workspace, "owner", "default", "exec", "controls", "exec", 20
    )
    .await
    .is_err());
    assert!(inbox::rebind_terminal_fence_to_successor(
        &workspace, "owner", "default", "exec", "exec-r20"
    )
    .await
    .is_err());
}

fn event(
    task: &TaskRecord,
    seq: u64,
    event_type: &str,
    payload: serde_json::Value,
) -> CanonicalEvent {
    CanonicalEvent {
        event_id: format!("evt-{seq}"),
        seq,
        timestamp: format!("2026-10-13T00:00:{seq:02}Z"),
        principal: "anonymous".into(),
        workspace: "default".into(),
        task_id: task.manifest.task_id.clone(),
        execution_id: "exec-budget".into(),
        plan_id: None,
        step_id: None,
        event_type: event_type.into(),
        ref_ids: EventRefIds::default(),
        payload,
    }
}

#[test]
fn long_run_cold_attention_preserves_the_budget_dimension_and_exact_ask_id() {
    let task: TaskRecord =
        serde_json::from_str(include_str!("fixtures/ordinary-budget-task.json")).unwrap();
    let node: ExecutionTreeNode = serde_json::from_value(json!({
        "execution_id":"exec-budget", "agent_id":"agent", "relationship_type":"root",
        "status":"max_iterations_reached", "active_child_execution_ids":[], "child_execution_ids":[],
        "ready_child_output_ids":[], "waiting_for_children":false,
        "started_at":"2026-10-13T00:00:00Z", "updated_at":"2026-10-13T00:00:00Z",
        "delegation_summary":{"execution_id":"exec-budget", "task_id":task.manifest.task_id,
            "execution_status":"max_iterations_reached", "waiting_for_children":false,
            "active_child_execution_ids":[], "ready_child_output_ids":[], "result_ready_count":0,
            "waiting_count":0, "blocked_count":0, "terminal_without_output_count":0, "delegations":[]}
    })).unwrap();
    let legacy = event(
        &task,
        1,
        "max_iterations_reached",
        json!({"pause_state_id":"exec-budget:plan:step"}),
    );
    let canonical = event(
        &task,
        2,
        "hitl.requested",
        json!({
            "source":"agentic", "correlation_id":"exec-budget:plan:step~exact-generation",
            "input_type":"confirmation", "prompt":"Time budget (120s/120s) reached. Continue or cancel?",
            "input_schema":{"pause_kind":"budget", "used":"120s", "limit":"120s"}
        }),
    );
    let mut events = HashMap::from([("exec-budget".into(), vec![legacy.clone(), canonical])]);
    let projection = build_attention_summaries(&task, &[node.clone()], &events, None, None);
    assert_eq!(projection.len(), 1);
    assert_eq!(projection[0].source.as_deref(), Some("agentic"));
    assert_eq!(
        projection[0].pause_state_id.as_deref(),
        Some("exec-budget:plan:step~exact-generation")
    );
    assert_eq!(
        projection[0].input_schema.as_ref().unwrap()["pause_kind"],
        "budget"
    );
    assert!(projection[0]
        .detail
        .as_ref()
        .unwrap()
        .starts_with("Time budget"));
    events.get_mut("exec-budget").unwrap().push(event(
        &task,
        3,
        "hitl.resolved",
        json!({"correlation_id":"exec-budget:plan:step~exact-generation"}),
    ));
    assert!(build_attention_summaries(&task, &[node.clone()], &events, None, None).is_empty());
    events.get_mut("exec-budget").unwrap().push(event(
        &task,
        4,
        "max_iterations_reached",
        json!({"pause_state_id":"exec-budget:plan:later-step"}),
    ));
    let subsequent = build_attention_summaries(&task, &[node.clone()], &events, None, None);
    assert_eq!(subsequent.len(), 1);
    assert_eq!(subsequent[0].attention_kind, "max_iterations_reached");
    // Genuine historical iteration stops keep their existing fallback.
    events.insert("exec-budget".into(), vec![legacy]);
    let old = build_attention_summaries(&task, &[node], &events, None, None);
    assert_eq!(old[0].attention_kind, "max_iterations_reached");
}

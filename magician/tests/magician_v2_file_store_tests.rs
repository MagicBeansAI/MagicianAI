use std::{path::Path, sync::Arc};

use chrono::Utc;
use magician::magician_v2::{
    artifact_v2::workspace::ArtifactV2Workspace,
    state_tracker::{BudgetState, ConfidenceScore, StageContext, StateBundle, WorkflowState},
    storage::{PaginationParams, WaitingState},
    FileV2Store, TurnDirection, V2SlotStatus,
};
use serde_json::json;
use tempfile::tempdir;
use uuid::Uuid;

// Import trait to bring methods into scope
use runtime_core::V2ConversationStore as _;

fn test_schema() -> serde_json::Value {
    json!({
        "type": "object",
        "properties": {
            "value": {"type": "string"}
        }
    })
}

async fn create_task_backed_root_execution(
    store: &FileV2Store,
    storage_root: &Path,
    principal: &str,
    workspace: &str,
    title: Option<String>,
    active_owner_agent_id: &str,
) -> Result<magician::magician_v2::storage::ExecutionRun, Box<dyn std::error::Error>> {
    let execution_id = format!("exec-{}", Uuid::new_v4());
    let task_id = format!("task-{}", Uuid::new_v4());
    ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(storage_root))
        .ensure_task_workspace(principal, workspace, &task_id)
        .await?;
    Ok(store
        .create_execution_with_options(
            principal,
            workspace,
            title,
            active_owner_agent_id,
            Some(task_id),
            Some(execution_id.clone()),
            Some(execution_id),
            None,
            None,
            Vec::new(),
            WaitingState::Planning,
        )
        .await?)
}

#[tokio::test]
async fn file_store_persists_conversation() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FileV2Store::new(dir.path());

    let execution = create_task_backed_root_execution(
        &store,
        dir.path(),
        "alice",
        "workspace",
        Some("Test Execution".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    let turn = store
        .add_turn(
            &execution.id,
            TurnDirection::Inbound,
            "hello world".to_string(),
            None,
        )
        .await?;

    let slot = store
        .create_slot(
            &execution.id,
            "hostname".to_string(),
            test_schema(),
            true,
            Some(turn.id.clone()),
        )
        .await?;

    store
        .update_slot_answer(&execution.id, &slot.id, json!("db.internal"))
        .await?;

    let slots = store.get_slots(&execution.id).await?;
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].status, V2SlotStatus::Answered);
    assert_eq!(slots[0].answer.as_ref().unwrap(), &json!("db.internal"));

    let summaries = store
        .list_executions(
            &execution.principal,
            &execution.workspace,
            PaginationParams::default(),
        )
        .await?;
    assert_eq!(summaries.pagination.total, 1);
    assert_eq!(summaries.items[0].title.as_deref(), Some("Test Execution"));

    let artifact_workspace =
        ArtifactV2Workspace::new(ArtifactV2Workspace::resolve_scoped_root(dir.path()));
    let persisted_path = artifact_workspace
        .runtime_execution_dir(
            &execution.principal,
            &execution.workspace,
            execution.task_id.as_deref(),
            &execution.id,
        )
        .join("execution.json");
    assert!(persisted_path.exists());
    let persisted_json = tokio::fs::read_to_string(&persisted_path).await?;
    assert!(persisted_json.contains("\"execution\""));
    assert!(persisted_json.contains("\"execution_id\""));
    assert!(!persisted_json.contains("\"thread\":"));
    assert!(!persisted_json.contains("\"thread_id\":"));

    Ok(())
}

#[tokio::test]
async fn file_store_handles_parallel_updates() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = Arc::new(FileV2Store::new(dir.path()));

    let execution = create_task_backed_root_execution(
        &store,
        dir.path(),
        "bob",
        "workspace",
        Some("Parallel".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    let slot = store
        .create_slot(
            &execution.id,
            "api_key".to_string(),
            test_schema(),
            true,
            None,
        )
        .await?;

    let execution_id_answer = execution.id.clone();
    let slot_id_answer = slot.id.clone();
    let store_for_answer = Arc::clone(&store);

    let execution_id_status = execution.id.clone();
    let slot_id_status = slot.id.clone();
    let store_for_status = Arc::clone(&store);

    let answer_task = tokio::spawn(async move {
        store_for_answer
            .update_slot_answer(&execution_id_answer, &slot_id_answer, json!("secret"))
            .await
    });

    let status_task = tokio::spawn(async move {
        store_for_status
            .update_slot_status(
                &execution_id_status,
                &slot_id_status,
                V2SlotStatus::Answered,
            )
            .await
    });

    answer_task.await??;
    status_task.await??;

    let slots = store.get_slots(&execution.id).await?;
    assert_eq!(slots.len(), 1);
    assert_eq!(slots[0].status, V2SlotStatus::Answered);
    assert_eq!(slots[0].answer.as_ref().unwrap(), &json!("secret"));

    Ok(())
}

#[tokio::test]
async fn file_store_tracks_state_snapshots() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FileV2Store::new(dir.path());

    let execution = create_task_backed_root_execution(
        &store,
        dir.path(),
        "carol",
        "workspace",
        Some("Stateful".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    let bundle = StateBundle {
        state_id: Uuid::new_v4().to_string(),
        workflow_id: execution.id.clone(),
        current_state: WorkflowState::Observe,
        llm_reasoning: Some("Bootstrapping plan".to_string()),
        observations: Vec::new(),
        slot_deltas: Vec::new(),
        confidence: ConfidenceScore {
            overall: 0.35,
            ..ConfidenceScore::default()
        },
        budget: BudgetState {
            remaining: 5.0,
            initial: 5.0,
            ..BudgetState::default()
        },
        stage_context: StageContext::PlanningBootstrap,
        completed_stages: Vec::new(),
        failed_stage: None,
        created_at: Utc::now(),
    };

    store.append_state(&execution.id, bundle.clone()).await?;

    let latest = store.get_latest_state(&execution.id).await?;
    assert!(latest.is_some());
    assert_eq!(latest.unwrap().current_state, WorkflowState::Observe);

    let states = store.get_states(&execution.id).await?;
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].state_id, bundle.state_id);

    Ok(())
}

#[tokio::test]
async fn file_store_preserves_pause_provenance() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FileV2Store::new(dir.path());

    let execution = create_task_backed_root_execution(
        &store,
        dir.path(),
        "dave",
        "workspace",
        Some("Pause Provenance".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    store
        .update_execution_status(&execution.id, WaitingState::WaitingChildren)
        .await?;
    store
        .update_execution_status(&execution.id, WaitingState::Paused)
        .await?;

    let paused = store.get_execution(&execution.id).await?;
    assert_eq!(paused.waiting_state, WaitingState::Paused);
    assert_eq!(
        paused.paused_from_state,
        Some(WaitingState::WaitingChildren)
    );

    store
        .update_execution_status(&execution.id, WaitingState::WaitingChildren)
        .await?;

    let resumed = store.get_execution(&execution.id).await?;
    assert_eq!(resumed.waiting_state, WaitingState::WaitingChildren);
    assert_eq!(resumed.paused_from_state, None);

    Ok(())
}

#[tokio::test]
async fn file_store_creates_child_executions_through_store_contract(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FileV2Store::new(dir.path());

    let parent = create_task_backed_root_execution(
        &store,
        dir.path(),
        "erin",
        "workspace",
        Some("Parent".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    let child = store
        .create_execution_with_options(
            "erin",
            "workspace",
            Some("[Delegation] Child".to_string()),
            "specialist-agent",
            parent.task_id.clone(),
            parent.root_execution_id.clone(),
            Some("child-exec-1".to_string()),
            Some(parent.id.clone()),
            Some(300),
            vec![magician::magician_v2::chat::DEFAULT_AGENT_ID.to_string()],
            WaitingState::Planning,
        )
        .await?;
    store.add_child_execution_id(&parent.id, &child.id).await?;

    let persisted_child = store.get_execution(&child.id).await?;
    assert_eq!(
        persisted_child.parent_execution_id.as_deref(),
        Some(parent.id.as_str())
    );
    assert_eq!(persisted_child.active_owner_agent_id, "specialist-agent");

    let persisted_parent = store.get_execution(&parent.id).await?;
    assert_eq!(persisted_parent.child_execution_ids, vec![child.id.clone()]);

    let index = store.rebuild_execution_index().await?;
    assert!(index.entries.contains_key(&parent.id));
    assert!(index.entries.contains_key(&child.id));

    Ok(())
}

#[tokio::test]
async fn file_store_settles_parent_status_and_active_group_together(
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let store = FileV2Store::new(dir.path());
    let parent = create_task_backed_root_execution(
        &store,
        dir.path(),
        "frank",
        "workspace",
        Some("Explicit parent".to_string()),
        magician::magician_v2::chat::DEFAULT_AGENT_ID,
    )
    .await?;

    store
        .update_execution_status(&parent.id, WaitingState::WaitingChildren)
        .await?;
    store
        .replace_active_delegation_group(&parent.id, &["child-1".to_string()])
        .await?;
    store
        .settle_delegation_parent(&parent.id, WaitingState::Completed)
        .await?;

    let settled = store.get_execution(&parent.id).await?;
    assert_eq!(settled.waiting_state, WaitingState::Completed);
    assert!(settled.active_delegation_group.is_empty());
    assert!(settled.paused_from_state.is_none());
    Ok(())
}

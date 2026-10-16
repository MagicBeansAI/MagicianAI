use std::sync::Arc;

use magician::magician_v2::{
    orchestrator::v2_orchestrator::ProcessingMetadata,
    state_tracker::{StateBundle, StateTracker, TransitionContext, WorkflowState},
    storage::{
        ExecutionRun, ExecutionSummary, StrategyAttempt, TurnDirection, V2ConversationStore,
        V2Slot, V2SlotStatus, V2StorageError, V2Turn, WaitingState,
    },
    AnalysisMetadata, UnifiedQueryAnalysis,
};
use runtime_core::{
    PaginatedResult, PaginationParams, V2ConversationStore as CoreV2ConversationStore,
};

#[derive(Clone, Default)]
struct InMemoryStore {
    inner: Arc<tokio::sync::Mutex<std::collections::HashMap<String, Vec<StateBundle>>>>,
}

#[async_trait::async_trait]
impl CoreV2ConversationStore for InMemoryStore {
    type Error = V2StorageError;
    type Execution = ExecutionRun;
    type ExecutionSummary = ExecutionSummary;
    type ExecutionStatus = WaitingState;
    type Turn = V2Turn;
    type TurnDirection = TurnDirection;
    type Slot = V2Slot;
    type SlotStatus = V2SlotStatus;
    type StrategyAttempt = StrategyAttempt;
    type ProcessingMetadata = ProcessingMetadata;
    type UnifiedAnalysis = UnifiedQueryAnalysis;
    type AnalysisMetadata = AnalysisMetadata;
    type StateBundle = StateBundle;
    type RecommendedQuestion =
        magician::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion;
    type PendingClarification =
        magician::magician_v2::orchestrator::v2_orchestrator::PendingClarification;
    type ClarificationSession = magician::magician_v2::ask_loop::ClarificationSession;
    type ClarificationHistoryEntry =
        magician::magician_v2::storage::models::ClarificationHistoryEntry;
    async fn create_execution_with_options(
        &self,
        _principal: &str,
        _workspace: &str,
        _title: Option<String>,
        _active_owner_agent_id: &str,
        _task_id: Option<String>,
        _root_execution_id: Option<String>,
        _execution_id: Option<String>,
        _parent_execution_id: Option<String>,
        _timeout_secs: Option<u64>,
        _delegation_chain: Vec<String>,
        _waiting_state: Self::ExecutionStatus,
    ) -> Result<Self::Execution, Self::Error> {
        unimplemented!()
    }

    // Does not persist a work authority: the create this delegates to is
    // `unimplemented!()`, so this panics before any store state is reached and
    // a grant has nothing to be written onto — unchanged from the old default.
    async fn create_execution_with_work_authority(
        &self,
        principal: &str,
        workspace: &str,
        title: Option<String>,
        active_owner_agent_id: &str,
        task_id: Option<String>,
        root_execution_id: Option<String>,
        execution_id: Option<String>,
        parent_execution_id: Option<String>,
        timeout_secs: Option<u64>,
        delegation_chain: Vec<String>,
        waiting_state: Self::ExecutionStatus,
        _work_authority: Option<runtime_core::WorkAuthorityGrant>,
    ) -> Result<Self::Execution, Self::Error> {
        CoreV2ConversationStore::create_execution_with_options(
            self,
            principal,
            workspace,
            title,
            active_owner_agent_id,
            task_id,
            root_execution_id,
            execution_id,
            parent_execution_id,
            timeout_secs,
            delegation_chain,
            waiting_state,
        )
        .await
    }

    async fn update_execution_entry_mode(
        &self,
        _execution_id: &str,
        _entry_mode: String,
    ) -> Result<(), Self::Error> {
        Err(V2StorageError::Storage(
            "update_execution_entry_mode unsupported in tests".to_string(),
        ))
    }

    async fn get_execution(&self, _execution_id: &str) -> Result<Self::Execution, Self::Error> {
        unimplemented!()
    }

    async fn add_turn(
        &self,
        _execution_id: &str,
        _direction: Self::TurnDirection,
        _text: String,
        _in_reply_to_slot_id: Option<String>,
    ) -> Result<Self::Turn, Self::Error> {
        unimplemented!()
    }

    async fn store_analysis(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _analysis: Self::UnifiedAnalysis,
        _metadata: Self::AnalysisMetadata,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn store_strategy_attempts(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _attempts: Vec<Self::StrategyAttempt>,
        _processing_metadata: Self::ProcessingMetadata,
        _recommended_questions: Option<Vec<Self::RecommendedQuestion>>,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn get_turn(
        &self,
        _execution_id: &str,
        _turn_id: &str,
    ) -> Result<Self::Turn, Self::Error> {
        unimplemented!()
    }

    async fn get_turns(&self, _execution_id: &str) -> Result<Vec<Self::Turn>, Self::Error> {
        unimplemented!()
    }

    async fn get_latest_turn_with_analysis(
        &self,
        _execution_id: &str,
    ) -> Result<Option<Self::Turn>, Self::Error> {
        unimplemented!()
    }

    async fn list_executions(
        &self,
        _principal: &str,
        _workspace: &str,
        _pagination: PaginationParams,
    ) -> Result<PaginatedResult<Self::ExecutionSummary>, Self::Error> {
        unimplemented!()
    }

    async fn delete_execution(&self, _execution_id: &str) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn get_turns_paginated(
        &self,
        _execution_id: &str,
        _pagination: PaginationParams,
        _direction_filter: Option<Self::TurnDirection>,
    ) -> Result<PaginatedResult<Self::Turn>, Self::Error> {
        unimplemented!()
    }

    async fn update_execution_status(
        &self,
        _execution_id: &str,
        _status: Self::ExecutionStatus,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn compare_exchange_execution_status(
        &self,
        _execution_id: &str,
        _expected: Self::ExecutionStatus,
        _status: Self::ExecutionStatus,
    ) -> Result<bool, Self::Error> {
        unimplemented!()
    }

    async fn compare_exchange_execution_status_at(
        &self,
        _execution_id: &str,
        _expected: Self::ExecutionStatus,
        _expected_status_revision: u64,
        _status: Self::ExecutionStatus,
    ) -> Result<bool, Self::Error> {
        unimplemented!()
    }

    async fn get_execution_status_revision(&self, _execution_id: &str) -> Result<u64, Self::Error> {
        unimplemented!()
    }

    async fn bind_execution_scope(
        &self,
        _execution_id: &str,
        _task_id: Option<String>,
        _root_execution_id: Option<String>,
    ) -> Result<(), Self::Error> {
        Err(V2StorageError::Storage(
            "execution scope binding not supported in tests".to_string(),
        ))
    }

    async fn add_child_execution_id(
        &self,
        _parent_execution_id: &str,
        _child_execution_id: &str,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn update_execution_owner_snapshot(
        &self,
        _execution_id: &str,
        _active_owner_agent_id: &str,
        _owner_stack: &[String],
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn replace_active_delegation_group(
        &self,
        _execution_id: &str,
        _active_delegation_group: &[String],
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn get_execution_status(
        &self,
        _execution_id: &str,
    ) -> Result<Self::ExecutionStatus, Self::Error> {
        unimplemented!()
    }

    async fn update_processing_correlation_id(
        &self,
        _execution_id: &str,
        _correlation_id: Option<String>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn create_slot(
        &self,
        _execution_id: &str,
        _name: String,
        _schema_json: serde_json::Value,
        _required: bool,
        _asked_turn_id: Option<String>,
    ) -> Result<Self::Slot, Self::Error> {
        unimplemented!()
    }

    async fn update_slot_answer(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _answer: serde_json::Value,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn update_slot_status(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _status: Self::SlotStatus,
    ) -> Result<(), Self::Error> {
        unimplemented!()
    }

    async fn get_slots(&self, _execution_id: &str) -> Result<Vec<Self::Slot>, Self::Error> {
        unimplemented!()
    }

    async fn get_pending_slots(&self, _execution_id: &str) -> Result<Vec<Self::Slot>, Self::Error> {
        unimplemented!()
    }

    async fn get_slot(
        &self,
        _execution_id: &str,
        _slot_id: &str,
    ) -> Result<Self::Slot, Self::Error> {
        unimplemented!()
    }

    async fn append_state(
        &self,
        execution_id: &str,
        state: Self::StateBundle,
    ) -> Result<(), Self::Error> {
        let mut map = self.inner.lock().await;
        map.entry(execution_id.to_string()).or_default().push(state);
        Ok(())
    }

    async fn get_states(&self, execution_id: &str) -> Result<Vec<Self::StateBundle>, Self::Error> {
        let map = self.inner.lock().await;
        Ok(map.get(execution_id).cloned().unwrap_or_default())
    }

    async fn get_latest_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<Self::StateBundle>, Self::Error> {
        let map = self.inner.lock().await;
        Ok(map.get(execution_id).and_then(|entries| {
            entries
                .iter()
                .cloned()
                .max_by(|a, b| a.created_at.cmp(&b.created_at))
        }))
    }

    async fn update_execution_processing_stage(
        &self,
        _execution_id: &str,
        _stage: Option<String>,
        _provider: Option<String>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn update_turn_enriched_query(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _enriched_query: Option<String>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn load_clarification_session(
        &self,
        _execution_id: &str,
    ) -> Result<Option<Self::ClarificationSession>, Self::Error> {
        Ok(None)
    }

    async fn store_clarification_session(
        &self,
        _execution_id: &str,
        _session: Self::ClarificationSession,
    ) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn delete_clarification_session(&self, _execution_id: &str) -> Result<(), Self::Error> {
        Ok(())
    }

    async fn get_clarification_history(
        &self,
        _execution_id: &str,
    ) -> Result<Vec<Self::ClarificationHistoryEntry>, Self::Error> {
        Ok(vec![])
    }

    async fn append_clarification_history(
        &self,
        _execution_id: &str,
        _entries: Vec<Self::ClarificationHistoryEntry>,
    ) -> Result<(), Self::Error> {
        Ok(())
    }
}

#[tokio::test]
async fn state_tracker_records_and_lists_states() {
    let store = Arc::new(InMemoryStore::default()) as Arc<dyn V2ConversationStore>;
    let tracker = StateTracker::new(store);

    let workflow_id = "wf-123";

    tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            TransitionContext {
                confidence_overall: Some(0.3),
                budget_remaining: Some(10.0),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    tracker
        .transition(
            workflow_id,
            WorkflowState::Hypothesize,
            WorkflowState::Act,
            TransitionContext {
                confidence_overall: Some(0.6),
                budget_spend: Some((2.0, "LLM tokens".to_string())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let states = tracker.list_states(workflow_id).await.unwrap();
    assert_eq!(states.len(), 2);
    assert_eq!(states[0].current_state, WorkflowState::Hypothesize);
    assert_eq!(states[1].current_state, WorkflowState::Act);

    // Confidence history should have an entry per transition
    assert_eq!(states[1].confidence.history.len(), 2);
    assert_eq!(states[1].confidence.overall, 0.6);
    let slope = states[1]
        .confidence
        .slope
        .expect("confidence slope captured for second transition");
    assert!(slope.is_sign_positive(), "slope should reflect improvement");

    // Budget reflects spend and remaining amount
    assert_eq!(states[1].budget.remaining, 8.0);
    assert_eq!(states[1].budget.spent.len(), 1);
    assert_eq!(states[1].budget.spent[0].amount, 2.0);
    assert_eq!(states[1].budget.spent[0].reason, "LLM tokens");
}

// NOTE: test_transition_persists_atomic_plan_and_budget_updates was removed because
// atomic_plan is no longer stored in StateBundle - it's stored only in turn storage.
// Plan is fetched via AskLoopApi::get_plan_for_execution().

#[tokio::test]
async fn transition_persists_budget_updates() {
    let store = Arc::new(InMemoryStore::default()) as Arc<dyn V2ConversationStore>;
    let tracker = StateTracker::new(store);
    let workflow_id = "wf-budget";

    tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            TransitionContext {
                confidence_overall: Some(0.65),
                budget_remaining: Some(12.0),
                budget_spend: Some((0.75, "budget test spend".to_string())),
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let latest = tracker.latest_state(workflow_id).await.unwrap().unwrap();
    assert!(
        (latest.confidence.overall - 0.65).abs() < f64::EPSILON,
        "confidence should reflect transition context"
    );

    assert!(
        (latest.budget.remaining - 11.25).abs() < f64::EPSILON,
        "budget remaining should account for spend"
    );
    assert_eq!(latest.budget.initial, 12.0);
    assert_eq!(latest.budget.spent.len(), 1);
    assert!(
        latest.budget.spent[0].reason.contains("budget test spend"),
        "budget spend should record rationale"
    );
}

#[tokio::test]
async fn state_tracker_latest_state_returns_most_recent() {
    let store = Arc::new(InMemoryStore::default()) as Arc<dyn V2ConversationStore>;
    let tracker = StateTracker::new(store);

    let workflow_id = "wf-456";

    tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            TransitionContext::default(),
        )
        .await
        .unwrap();
    tracker
        .transition(
            workflow_id,
            WorkflowState::Hypothesize,
            WorkflowState::Act,
            TransitionContext::default(),
        )
        .await
        .unwrap();

    let latest = tracker.latest_state(workflow_id).await.unwrap().unwrap();
    assert_eq!(latest.current_state, WorkflowState::Act);
}

#[tokio::test]
async fn transition_rejects_invalid_sequence() {
    let store = Arc::new(InMemoryStore::default()) as Arc<dyn V2ConversationStore>;
    let tracker = StateTracker::new(store);
    let workflow_id = "wf-789";

    tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            TransitionContext::default(),
        )
        .await
        .unwrap();

    let err = tracker
        .transition(
            workflow_id,
            WorkflowState::Hypothesize,
            WorkflowState::Observe,
            TransitionContext::default(),
        )
        .await
        .expect_err("should reject invalid transition");

    assert!(err.to_string().contains("Invalid state transition"));
}

#[tokio::test]
async fn transition_detects_state_mismatch() {
    let store = Arc::new(InMemoryStore::default()) as Arc<dyn V2ConversationStore>;
    let tracker = StateTracker::new(store);
    let workflow_id = "wf-999";

    tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            TransitionContext::default(),
        )
        .await
        .unwrap();

    let err = tracker
        .transition(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Act,
            TransitionContext::default(),
        )
        .await
        .expect_err("mismatched state should fail");

    assert!(err.to_string().contains("State mismatch"));
}

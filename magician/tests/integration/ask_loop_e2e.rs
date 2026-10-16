use std::{
    collections::HashMap,
    sync::{Arc, Weak},
};

use async_trait::async_trait;
use magician::magician_v2::{
    ask_loop::{
        clarifier::{
            BlockerType, ClarifierLibrary, ClarifierQuestion, DeterministicClarifier,
            WorkflowContext,
        },
        ledger::BudgetLedger,
        pause::{
            InMemoryQueueRepository, PauseReason, PauseResumeManager, QueueRepository, WaitingQueue,
        },
        session::{
            ClarificationSession, ClarificationSessionState, ClarificationSessionStoreError,
            SessionQuestionStatus,
        },
        session_manager::SessionManager,
        BudgetConfig, BudgetPolicy, Channel, ClarificationHistory, ResumeListener,
        ResumeNotification, ResumePreparation, ResumeTriggerService, TaskComplexity,
    },
    confidence::ConfidenceService,
    orchestrator::v2_orchestrator::ProcessingMetadata,
    slot_graph::SlotRecord,
    state_tracker::{
        BudgetState, ConfidenceScore, StageContext, StateBundle, StateTracker, TransitionContext,
        WorkflowState,
    },
    storage::{
        ExecutionRun, ExecutionSummary, PaginatedResult, PaginationParams, StrategyAttempt,
        TurnDirection, V2Slot, V2SlotStatus, V2StorageError, V2Turn, WaitingState,
    },
    AnalysisMetadata, UnifiedQueryAnalysis,
};
use runtime_core::V2ConversationStore as CoreV2ConversationStore;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
struct InMemoryStore {
    states: Arc<Mutex<HashMap<String, Vec<StateBundle>>>>,
}

#[async_trait]
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
        _waiting_state: WaitingState,
    ) -> Result<ExecutionRun, V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    // Does not persist a work authority: this stub refuses every create, so no
    // record ever exists for a grant to land on. Delegating hands back exactly
    // the refusal the plain variant already returns, so no test changes outcome.
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
        waiting_state: WaitingState,
        _work_authority: Option<runtime_core::WorkAuthorityGrant>,
    ) -> Result<ExecutionRun, V2StorageError> {
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
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn get_execution(&self, _execution_id: &str) -> Result<ExecutionRun, V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn add_turn(
        &self,
        _execution_id: &str,
        _direction: TurnDirection,
        _text: String,
        _in_reply_to_slot_id: Option<String>,
    ) -> Result<V2Turn, V2StorageError> {
        Err(V2StorageError::Storage(
            "turn operations not supported in tests".to_string(),
        ))
    }

    async fn store_analysis(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _analysis: UnifiedQueryAnalysis,
        _metadata: AnalysisMetadata,
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "analysis storage not supported in tests".to_string(),
        ))
    }

    async fn store_strategy_attempts(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _attempts: Vec<StrategyAttempt>,
        _processing_metadata: ProcessingMetadata,
        _recommended_questions: Option<Vec<Self::RecommendedQuestion>>,
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "strategy attempts not supported in tests".to_string(),
        ))
    }

    async fn get_turn(
        &self,
        _execution_id: &str,
        _turn_id: &str,
    ) -> Result<V2Turn, V2StorageError> {
        Err(V2StorageError::Storage(
            "turn operations not supported in tests".to_string(),
        ))
    }

    async fn get_turns(&self, _execution_id: &str) -> Result<Vec<V2Turn>, V2StorageError> {
        Ok(Vec::new())
    }

    async fn get_latest_turn_with_analysis(
        &self,
        _execution_id: &str,
    ) -> Result<Option<V2Turn>, V2StorageError> {
        Ok(None)
    }

    async fn list_executions(
        &self,
        _principal: &str,
        _workspace: &str,
        _pagination: PaginationParams,
    ) -> Result<PaginatedResult<ExecutionSummary>, V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn delete_execution(&self, _execution_id: &str) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn get_turns_paginated(
        &self,
        _execution_id: &str,
        _pagination: PaginationParams,
        _direction_filter: Option<TurnDirection>,
    ) -> Result<PaginatedResult<V2Turn>, V2StorageError> {
        Err(V2StorageError::Storage(
            "turn operations not supported in tests".to_string(),
        ))
    }

    async fn update_execution_status(
        &self,
        execution_id: &str,
        _status: WaitingState,
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn compare_exchange_execution_status(
        &self,
        execution_id: &str,
        _expected: WaitingState,
        _status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn compare_exchange_execution_status_at(
        &self,
        execution_id: &str,
        _expected: WaitingState,
        _expected_status_revision: u64,
        _status: WaitingState,
    ) -> Result<bool, V2StorageError> {
        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn get_execution_status_revision(
        &self,
        execution_id: &str,
    ) -> Result<u64, V2StorageError> {
        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn bind_execution_scope(
        &self,
        _execution_id: &str,
        _task_id: Option<String>,
        _root_execution_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn add_child_execution_id(
        &self,
        _parent_execution_id: &str,
        _child_execution_id: &str,
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn update_execution_owner_snapshot(
        &self,
        _execution_id: &str,
        _active_owner_agent_id: &str,
        _owner_stack: &[String],
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn replace_active_delegation_group(
        &self,
        _execution_id: &str,
        _active_delegation_group: &[String],
    ) -> Result<(), V2StorageError> {
        Err(V2StorageError::Storage(
            "execution operations not supported in tests".to_string(),
        ))
    }

    async fn get_execution_status(
        &self,
        execution_id: &str,
    ) -> Result<WaitingState, V2StorageError> {
        Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
    }

    async fn update_processing_correlation_id(
        &self,
        _execution_id: &str,
        _correlation_id: Option<String>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn create_slot(
        &self,
        _execution_id: &str,
        _name: String,
        _schema_json: serde_json::Value,
        _required: bool,
        _asked_turn_id: Option<String>,
    ) -> Result<V2Slot, V2StorageError> {
        Err(V2StorageError::Storage(
            "slot operations not supported in tests".to_string(),
        ))
    }

    async fn update_slot_answer(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _answer: serde_json::Value,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn update_slot_status(
        &self,
        _execution_id: &str,
        _slot_id: &str,
        _status: V2SlotStatus,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn get_slots(&self, _execution_id: &str) -> Result<Vec<V2Slot>, V2StorageError> {
        Ok(Vec::new())
    }

    async fn get_pending_slots(&self, _execution_id: &str) -> Result<Vec<V2Slot>, V2StorageError> {
        Ok(Vec::new())
    }

    async fn get_slot(
        &self,
        _execution_id: &str,
        _slot_id: &str,
    ) -> Result<V2Slot, V2StorageError> {
        Err(V2StorageError::Storage(
            "slot operations not supported in tests".to_string(),
        ))
    }

    async fn append_state(
        &self,
        execution_id: &str,
        state: StateBundle,
    ) -> Result<(), V2StorageError> {
        let mut map = self.states.lock().await;
        map.entry(execution_id.to_string()).or_default().push(state);
        Ok(())
    }

    async fn get_states(&self, execution_id: &str) -> Result<Vec<StateBundle>, V2StorageError> {
        let map = self.states.lock().await;
        Ok(map.get(execution_id).cloned().unwrap_or_default())
    }

    async fn get_latest_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<StateBundle>, V2StorageError> {
        let map = self.states.lock().await;
        Ok(map
            .get(execution_id)
            .and_then(|entries| entries.iter().cloned().max_by_key(|state| state.created_at)))
    }

    async fn update_execution_processing_stage(
        &self,
        _execution_id: &str,
        _stage: Option<String>,
        _provider: Option<String>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn update_turn_enriched_query(
        &self,
        _execution_id: &str,
        _turn_id: &str,
        _enriched_query: Option<String>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn load_clarification_session(
        &self,
        _execution_id: &str,
    ) -> Result<Option<Self::ClarificationSession>, V2StorageError> {
        Ok(None)
    }

    async fn store_clarification_session(
        &self,
        _execution_id: &str,
        _session: Self::ClarificationSession,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn delete_clarification_session(
        &self,
        _execution_id: &str,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }

    async fn get_clarification_history(
        &self,
        _execution_id: &str,
    ) -> Result<Vec<Self::ClarificationHistoryEntry>, V2StorageError> {
        Ok(vec![])
    }

    async fn append_clarification_history(
        &self,
        _execution_id: &str,
        _entries: Vec<Self::ClarificationHistoryEntry>,
    ) -> Result<(), V2StorageError> {
        Ok(())
    }
}

#[tokio::test]
async fn in_memory_store_update_execution_status_rejects_missing_execution() {
    let store = InMemoryStore::default();
    let err = store
        .update_execution_status("missing-execution", WaitingState::Paused)
        .await
        .expect_err("missing executions should not accept status writes");
    assert!(matches!(
        err,
        V2StorageError::ExecutionNotFound(id) if id == "missing-execution"
    ));
}

// Mock SessionManager for tests
#[derive(Clone, Default)]
struct MockSessionManager {
    sessions: Arc<Mutex<HashMap<String, ClarificationSession>>>,
}

#[async_trait]
impl SessionManager for MockSessionManager {
    async fn load(
        &self,
        workflow_id: &str,
    ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError> {
        let sessions = self.sessions.lock().await;
        Ok(sessions.get(workflow_id).cloned())
    }

    async fn save(
        &self,
        session: ClarificationSession,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        sessions.insert(session.workflow_id.clone(), session);
        Ok(())
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        sessions.remove(workflow_id);
        Ok(())
    }

    async fn append_question(
        &self,
        workflow_id: &str,
        question: ClarifierQuestion,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        let entry = sessions
            .entry(workflow_id.to_string())
            .or_insert_with(|| ClarificationSession::new(workflow_id));
        entry.enqueue_question(question);
        Ok(())
    }

    async fn mark_question_answered(
        &self,
        workflow_id: &str,
        question_id: &str,
        slots: Vec<SlotRecord>,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(workflow_id) {
            session.update_question_status(question_id, SessionQuestionStatus::Answered);
            for slot in slots {
                session.record_slot_update(question_id, slot);
            }
        }
        Ok(())
    }

    async fn cancel_questions(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<(), ClarificationSessionStoreError> {
        if question_ids.is_empty() {
            return Ok(());
        }
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(workflow_id) {
            for id in question_ids {
                session.update_question_status(id, SessionQuestionStatus::Cancelled);
            }
        }
        Ok(())
    }

    async fn mark_questions_handed_off(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<usize, ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(workflow_id) {
            Ok(session.mark_questions_handed_off(question_ids))
        } else {
            Ok(0)
        }
    }

    async fn ensure_round_limit(
        &self,
        _workflow_id: &str,
        _limit: usize,
    ) -> Result<(), ClarificationSessionStoreError> {
        Ok(())
    }

    async fn mark_state(
        &self,
        workflow_id: &str,
        state: ClarificationSessionState,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut sessions = self.sessions.lock().await;
        if let Some(session) = sessions.get_mut(workflow_id) {
            match state {
                ClarificationSessionState::CollectingAnswers => session.mark_collecting(),
                ClarificationSessionState::ReadyToPlan => session.mark_ready_to_plan(),
                ClarificationSessionState::Planning => session.mark_planning(),
            }
        }
        Ok(())
    }

    async fn update_batch_progress(
        &self,
        _workflow_id: &str,
        _batch_id: &str,
        _answered: usize,
        _total: usize,
    ) -> Result<(), ClarificationSessionStoreError> {
        Ok(())
    }
}

struct AskLoopFixture {
    tracker: Arc<StateTracker>,
    ledger: Arc<BudgetLedger>,
    pause_manager: Arc<PauseResumeManager>,
    clarifier: Arc<ClarifierLibrary>,
    resume_service: Arc<ResumeTriggerService>,
    session_manager: Arc<MockSessionManager>,
}

impl AskLoopFixture {
    fn new(config: BudgetConfig) -> Self {
        let store = Arc::new(InMemoryStore::default());
        let confidence_service = Arc::new(ConfidenceService::default());
        let tracker = Arc::new(StateTracker::with_confidence_service(
            store,
            confidence_service.clone(),
        ));
        let policy = Arc::new(BudgetPolicy::with_confidence_service(
            config,
            confidence_service.clone(),
        ));
        let ledger = Arc::new(BudgetLedger::new(tracker.clone(), policy));
        let queue_repo: Arc<dyn QueueRepository> = Arc::new(InMemoryQueueRepository::default());
        let waiting_queue = Arc::new(WaitingQueue::new(queue_repo));
        let pause_manager = Arc::new(PauseResumeManager::new(tracker.clone(), waiting_queue));
        let clarifier = Arc::new(ClarifierLibrary::with_default_templates(Arc::new(
            DeterministicClarifier,
        )));
        let clarification_history = Arc::new(ClarificationHistory::default());
        let session_manager = Arc::new(MockSessionManager::default());
        let resume_service = Arc::new(ResumeTriggerService::new(
            pause_manager.clone(),
            clarifier.clone(),
            Some(ledger.clone()),
            None,
            clarification_history,
        ));
        resume_service.set_resume_listener(Some(Arc::new(RecordingListener {
            notifications: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            failures: Arc::new(tokio::sync::Mutex::new(Vec::new())),
            prepared: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            resume_service: Arc::downgrade(&resume_service),
        })));

        // Set the session manager in the resume service
        resume_service.set_session_manager(session_manager.clone() as Arc<dyn SessionManager>);

        Self {
            tracker,
            ledger,
            pause_manager,
            clarifier,
            resume_service,
            session_manager,
        }
    }

    async fn push_confidence(
        &self,
        workflow_id: &str,
        from: WorkflowState,
        to: WorkflowState,
        confidence: f64,
        budget_remaining: Option<f64>,
    ) -> WorkflowState {
        let mut context = TransitionContext {
            confidence_overall: Some(confidence),
            ..Default::default()
        };
        if let Some(remaining) = budget_remaining {
            context.budget_remaining = Some(remaining);
        }
        self.tracker
            .transition(workflow_id, from, to, context)
            .await
            .expect("state transition");
        to
    }

    async fn seed_confidence_history(
        &self,
        workflow_id: &str,
        mut state: WorkflowState,
        values: &[f64],
    ) {
        for value in values {
            let next = match state {
                WorkflowState::Observe => WorkflowState::Hypothesize,
                WorkflowState::Hypothesize => WorkflowState::Act,
                WorkflowState::Act => WorkflowState::Verify,
                WorkflowState::Verify => WorkflowState::Observe,
                WorkflowState::Clarify => WorkflowState::Pause,
                WorkflowState::Pause => WorkflowState::Hypothesize,
                WorkflowState::Complete | WorkflowState::Failed => WorkflowState::Observe,
            };
            state = self
                .push_confidence(workflow_id, state, next, *value, None)
                .await;
            // Add a small delay to ensure distinct timestamps for slope calculation
            tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        }
    }
}

fn default_budget_config() -> BudgetConfig {
    BudgetConfig::default()
}

fn custom_channel_config() -> BudgetConfig {
    let mut penalty_factor_channel = HashMap::new();
    penalty_factor_channel.insert(Channel::InApp, 1.2);
    penalty_factor_channel.insert(Channel::PushNotification, 0.8);
    penalty_factor_channel.insert(Channel::Email, 1.1);

    let mut base_config = BudgetConfig::default();
    base_config.initial_budget = 10.0;
    base_config.confidence_slope_window = 5;
    base_config.slope_threshold = -0.02;
    base_config.penalty_factor_recency = 1.5;
    base_config.penalty_factor_channel = penalty_factor_channel;
    base_config.replenish_threshold = 0.15;
    base_config.replenish_amount = 0.4;
    base_config
}

#[tokio::test]
async fn budget_exhaustion_flow_pauses_and_resumes() {
    let mut config = default_budget_config();
    config.initial_budget = 4.0;
    let fixture = AskLoopFixture::new(config);

    let workflow_id = "wf-budget-exhaust";
    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Simple)
        .await
        .expect("ledger init");

    let mut current_state = WorkflowState::Observe;
    current_state = fixture
        .push_confidence(
            workflow_id,
            current_state,
            WorkflowState::Hypothesize,
            0.7,
            Some(4.0),
        )
        .await;

    fixture
        .ledger
        .spend(workflow_id, 2.0, "first action".to_string())
        .await
        .expect("first spend");
    let budget = fixture
        .ledger
        .get_current(workflow_id)
        .await
        .expect("budget");
    let _ = fixture
        .push_confidence(
            workflow_id,
            current_state,
            WorkflowState::Act,
            0.5,
            Some(budget.remaining),
        )
        .await;

    let remaining = budget.remaining;
    fixture
        .ledger
        .spend(workflow_id, remaining, "second action".to_string())
        .await
        .expect("second spend");
    let exhausted_budget = fixture
        .ledger
        .get_current(workflow_id)
        .await
        .expect("budget after spend");
    assert!(
        exhausted_budget.remaining <= f64::EPSILON,
        "budget should be depleted"
    );

    let question = fixture
        .clarifier
        .generate_question(
            BlockerType::LowConfidenceSlot,
            &WorkflowContext::empty(workflow_id),
        )
        .await
        .expect("generate question");

    // Add question to session before pausing
    fixture
        .session_manager
        .append_question(workflow_id, question.clone())
        .await
        .expect("append question to session");

    let paused = fixture
        .pause_manager
        .pause(workflow_id, PauseReason::WaitingOnUser(question.id.clone()))
        .await
        .expect("pause workflow");
    match paused.reason {
        PauseReason::WaitingOnUser(ref id) => assert_eq!(id, &question.id),
        _ => panic!("expected pause reason WaitingOnUser"),
    }

    let paused_entries = fixture
        .pause_manager
        .get_paused_workflows()
        .await
        .expect("list paused workflows");
    assert_eq!(paused_entries.len(), 1);

    let slots = fixture
        .resume_service
        .on_clarification_received(&question.id, "Budget restored", None, None)
        .await
        .expect("resume on clarifier");
    assert_eq!(slots.len(), 1, "clarifier should produce a slot");

    let remaining_paused = fixture
        .pause_manager
        .get_paused_workflows()
        .await
        .expect("paused workflows after resume");
    assert!(
        remaining_paused.is_empty(),
        "workflow should be removed from pause queue"
    );

    let latest_state = fixture
        .tracker
        .latest_state(workflow_id)
        .await
        .expect("latest state")
        .expect("state exists");
    assert!(
        latest_state.confidence.overall >= 0.6,
        "confidence should increase after clarification"
    );
    assert!(
        !latest_state.confidence.slot_records.is_empty(),
        "slot snapshots should be persisted after clarification"
    );
    assert!(
        latest_state.confidence.summary.is_some(),
        "confidence summary should be populated after clarification"
    );
}

#[tokio::test]
async fn confidence_slope_triggers_clarifier_and_budget_spend() {
    let mut config = default_budget_config();
    config.slope_threshold = -0.01;
    let fixture = AskLoopFixture::new(config);

    let workflow_id = "wf-slope-trigger";
    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Moderate)
        .await
        .expect("ledger init");

    let mut state = WorkflowState::Observe;
    state = fixture
        .push_confidence(
            workflow_id,
            state,
            WorkflowState::Hypothesize,
            0.8,
            Some(10.0),
        )
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    state = fixture
        .push_confidence(workflow_id, state, WorkflowState::Act, 0.65, Some(9.0))
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    state = fixture
        .push_confidence(workflow_id, state, WorkflowState::Verify, 0.5, Some(8.0))
        .await;
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
    fixture
        .push_confidence(workflow_id, state, WorkflowState::Observe, 0.35, Some(8.0))
        .await;

    let decision = fixture
        .ledger
        .evaluate_ask_decision(workflow_id)
        .await
        .expect("ask decision");
    assert!(
        decision.should_ask,
        "negative slope should trigger an ask decision"
    );
    assert!(
        decision.reason.contains("slope"),
        "decision rationale should reference slope"
    );

    let previous_budget = fixture
        .ledger
        .get_current(workflow_id)
        .await
        .expect("budget before spend")
        .remaining;
    let post_spend = fixture
        .ledger
        .spend(
            workflow_id,
            decision.estimated_cost,
            "clarifier escalation".to_string(),
        )
        .await
        .expect("spend for clarifier");
    assert!(
        post_spend.remaining < previous_budget,
        "clarifier spend should reduce remaining budget"
    );
}

struct RecordingListener {
    notifications: Arc<tokio::sync::Mutex<Vec<ResumeNotification>>>,
    failures: Arc<tokio::sync::Mutex<Vec<(String, String)>>>,
    prepared: Arc<tokio::sync::Mutex<HashMap<String, ResumePreparation>>>,
    resume_service: Weak<ResumeTriggerService>,
}

#[async_trait]
impl ResumeListener for RecordingListener {
    async fn prepare_resume(
        &self,
        preparation: ResumePreparation,
    ) -> Result<Option<String>, String> {
        let recovery_id = format!(
            "recording_resume_{}",
            preparation.notification.resumed_state.state_id
        );
        self.prepared
            .lock()
            .await
            .insert(recovery_id.clone(), preparation);
        Ok(Some(recovery_id))
    }

    async fn commit_prepared_resume(
        &self,
        preparation: &ResumePreparation,
        recovery_id: &str,
    ) -> Result<(), String> {
        let prepared = self
            .prepared
            .lock()
            .await
            .remove(recovery_id)
            .ok_or_else(|| format!("prepared resume missing: {recovery_id}"))?;
        if prepared.notification.workflow_id != preparation.notification.workflow_id
            || prepared.notification.resumed_state.state_id
                != preparation.notification.resumed_state.state_id
        {
            return Err(format!("prepared resume changed: {recovery_id}"));
        }
        let resume_service = self
            .resume_service
            .upgrade()
            .ok_or_else(|| "test resume service unavailable".to_string())?;
        resume_service
            .commit_prepared_resume_mutation(&prepared.mutation)
            .await
            .map_err(|error| error.to_string())?;
        self.on_resume(prepared.notification).await
    }

    async fn on_resume(&self, notification: ResumeNotification) -> Result<(), String> {
        let mut guard = self.notifications.lock().await;
        guard.push(notification);
        Ok(())
    }

    async fn on_resume_failed(&self, workflow_id: &str, error: &str) {
        let mut guard = self.failures.lock().await;
        guard.push((workflow_id.to_string(), error.to_string()));
    }
}

#[tokio::test]
async fn resume_listener_receives_stage_and_blocker() {
    let fixture = AskLoopFixture::new(default_budget_config());
    let notifications = Arc::new(tokio::sync::Mutex::new(Vec::new()));
    let failures = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let listener = Arc::new(RecordingListener {
        notifications: Arc::clone(&notifications),
        failures: Arc::clone(&failures),
        prepared: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
        resume_service: Arc::downgrade(&fixture.resume_service),
    });
    fixture
        .resume_service
        .set_resume_listener(Some(listener as Arc<dyn ResumeListener>));

    let workflow_id = "wf-listener";
    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Moderate)
        .await
        .expect("ledger init");

    let now = chrono::Utc::now();
    let state = StateBundle {
        state_id: uuid::Uuid::new_v4().to_string(),
        workflow_id: workflow_id.to_string(),
        current_state: WorkflowState::Act,
        llm_reasoning: None,
        observations: Vec::new(),
        slot_deltas: Vec::new(),
        confidence: ConfidenceScore {
            overall: 0.4,
            ..ConfidenceScore::default()
        },
        budget: BudgetState {
            remaining: 5.0,
            initial: 5.0,
            spent: Vec::new(),
        },
        stage_context: StageContext::ExecutionCycle,
        completed_stages: Vec::new(),
        failed_stage: None,
        created_at: now,
    };
    fixture
        .tracker
        .record_state(state)
        .await
        .expect("seed state");

    let context = WorkflowContext {
        workflow_id: workflow_id.to_string(),
        stage_context: StageContext::ExecutionCycle,
        recent_observations: Vec::new(),
        slot_graph: Vec::new(),
        confidence_scores: HashMap::new(),
        question_hint: None,
    };

    let question = fixture
        .clarifier
        .generate_question(BlockerType::LowConfidenceSlot, &context)
        .await
        .expect("clarifier question");

    // Add question to session before pausing
    fixture
        .session_manager
        .append_question(workflow_id, question.clone())
        .await
        .expect("append question to session");

    fixture
        .pause_manager
        .pause(workflow_id, PauseReason::WaitingOnUser(question.id.clone()))
        .await
        .expect("pause workflow");

    fixture
        .resume_service
        .on_clarification_received(&question.id, "clarified answer", None, None)
        .await
        .expect("resume flow");

    let recorded = notifications.lock().await;
    assert_eq!(recorded.len(), 1, "expected a single resume notification");
    let notification = &recorded[0];
    assert_eq!(notification.workflow_id, workflow_id);
    assert_eq!(notification.stage, StageContext::ExecutionCycle);
    assert_eq!(
        notification.blocker_type,
        Some(BlockerType::LowConfidenceSlot)
    );
    assert!(
        !notification.slots.is_empty(),
        "resume notification should include slot updates"
    );

    let failure_records = failures.lock().await;
    assert!(
        failure_records.is_empty(),
        "no resume failures should be recorded"
    );
}

/// Tests that planning stages respect confidence thresholds while execution stages
/// bypass confidence checks (execution clarifications are about missing data, not confidence).
#[tokio::test]
async fn planning_vs_execution_budget_thresholds_diverge() {
    let fixture = AskLoopFixture::new(default_budget_config());
    let workflow_id = "wf-stage-thresholds";

    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Moderate)
        .await
        .expect("ledger init");

    let _ = fixture
        .push_confidence(
            workflow_id,
            WorkflowState::Observe,
            WorkflowState::Hypothesize,
            0.5,
            Some(6.0),
        )
        .await;

    fixture
        .ledger
        .annotate_stage(workflow_id, StageContext::PlanningBootstrap)
        .await
        .expect("annotate planning stage");

    let decision_bootstrap = fixture
        .ledger
        .evaluate_ask_decision_for_stage(workflow_id, StageContext::PlanningBootstrap)
        .await
        .expect("planning bootstrap decision");
    assert!(
        decision_bootstrap.should_ask,
        "planning bootstrap should trigger ask when confidence 0.50 is below stage floor (0.92)"
    );

    fixture
        .ledger
        .annotate_stage(workflow_id, StageContext::ExecutionCycle)
        .await
        .expect("annotate execution stage");

    let decision_execution = fixture
        .ledger
        .evaluate_ask_decision_for_stage(workflow_id, StageContext::ExecutionCycle)
        .await
        .expect("execution stage decision");
    // Execution stage bypasses confidence checks - clarifications are about missing data
    // needed to proceed, not about confidence levels. With budget available, should_ask is true.
    assert!(
        decision_execution.should_ask,
        "execution cycle should ask when budget available (bypasses confidence checks for data-driven clarifications)"
    );
    assert!(
        decision_execution
            .reason
            .contains("confidence checks bypassed"),
        "reason should indicate confidence checks were bypassed for execution stage, got: {}",
        decision_execution.reason
    );
}

#[tokio::test]
async fn auto_replenish_budget_on_confidence_gain() {
    let fixture = AskLoopFixture::new(default_budget_config());
    let workflow_id = "wf-auto-replenish";

    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Moderate)
        .await
        .expect("ledger init");

    let state = WorkflowState::Observe;
    let _ = fixture
        .push_confidence(
            workflow_id,
            state,
            WorkflowState::Hypothesize,
            0.5,
            Some(10.0),
        )
        .await;

    fixture
        .ledger
        .spend(workflow_id, 4.0, "investigation".to_string())
        .await
        .expect("spend budget");
    let budget_before = fixture
        .ledger
        .get_current(workflow_id)
        .await
        .expect("budget before clarification");

    let question = fixture
        .clarifier
        .generate_question(
            BlockerType::LowConfidenceSlot,
            &WorkflowContext::empty(workflow_id),
        )
        .await
        .expect("generate question");

    // Add question to session before pausing
    fixture
        .session_manager
        .append_question(workflow_id, question.clone())
        .await
        .expect("append question to session");

    fixture
        .pause_manager
        .pause(workflow_id, PauseReason::WaitingOnUser(question.id.clone()))
        .await
        .expect("pause workflow");

    fixture
        .resume_service
        .on_clarification_received(&question.id, "All clear", None, None)
        .await
        .expect("resume after clarification");

    let budget_after = fixture
        .ledger
        .get_current(workflow_id)
        .await
        .expect("budget after clarification");
    assert!(
        budget_after.remaining > budget_before.remaining,
        "confidence gain should replenish budget"
    );
}

#[tokio::test]
async fn clarifier_channel_selection_reflects_urgency() {
    let fixture = AskLoopFixture::new(custom_channel_config());
    let workflow_id = "wf-channel-selection";
    fixture
        .ledger
        .initialize(workflow_id, TaskComplexity::Complex)
        .await
        .expect("ledger init");

    fixture
        .seed_confidence_history(workflow_id, WorkflowState::Observe, &[0.8, 0.6, 0.4, 0.25])
        .await;

    let decision_high = fixture
        .ledger
        .evaluate_ask_decision(workflow_id)
        .await
        .expect("high urgency decision");
    assert!(
        decision_high.should_ask,
        "steep confidence decline should trigger clarifier"
    );
    assert_eq!(
        decision_high.recommended_channel,
        Channel::PushNotification,
        "high urgency path should favour push channel"
    );

    // Add positive history to reduce urgency and ensure in-app is chosen.
    // Use values that plateau at low confidence (below 0.45) to trigger low_confidence ask
    // Use 5+ identical values to ensure stable plateau detection
    fixture
        .seed_confidence_history(
            workflow_id,
            WorkflowState::Observe,
            &[0.30, 0.30, 0.30, 0.30, 0.30, 0.30],
        )
        .await;
    let decision_low = fixture
        .ledger
        .evaluate_ask_decision(workflow_id)
        .await
        .expect("low urgency decision");
    assert!(
        decision_low.should_ask,
        "history still allows an ask decision, decision={:?}",
        decision_low
    );
    // Channel is chosen based on cost, not urgency. PushNotification is cheapest (0.8 penalty)
    assert_eq!(
        decision_low.recommended_channel,
        Channel::PushNotification,
        "cheapest channel (PushNotification) is chosen"
    );
    // Verify urgency is lower than the steep decline case
    assert!(
        decision_low.urgency < decision_high.urgency,
        "plateaued confidence should have lower urgency than steep decline"
    );
}

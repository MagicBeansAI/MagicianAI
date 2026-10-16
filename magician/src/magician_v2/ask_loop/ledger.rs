use std::sync::Arc;

use chrono::{DateTime, TimeZone, Utc};
use thiserror::Error;
use uuid::Uuid;

use super::budget::{AskDecision, BudgetPolicy};
use crate::magician_v2::state_tracker::{
    BudgetSpend, BudgetState, ConfidenceScore, StageContext, StateBundle, StateTracker,
    TransitionContext, WorkflowState,
};

/// Service responsible for applying budget changes and evaluating ask
/// eligibility.
pub struct BudgetLedger {
    state_tracker: Arc<StateTracker>,
    policy: Arc<BudgetPolicy>,
}

impl BudgetLedger {
    pub fn new(state_tracker: Arc<StateTracker>, policy: Arc<BudgetPolicy>) -> Self {
        Self {
            state_tracker,
            policy,
        }
    }

    pub fn policy(&self) -> &BudgetPolicy {
        &self.policy
    }

    /// Ensure the latest snapshot for a workflow reflects the supplied stage.
    ///
    /// If no snapshot exists yet, this seeds an initial bundle so stage-aware
    /// thresholds can be applied before any LLM calls are scheduled.
    pub async fn annotate_stage(
        &self,
        workflow_id: &str,
        stage: StageContext,
    ) -> Result<(), BudgetLedgerError> {
        if stage == StageContext::Unknown {
            return Ok(());
        }

        match self.state_tracker.latest_state(workflow_id).await? {
            Some(current) if current.stage_context == stage => Ok(()),
            Some(current) => {
                self.state_tracker
                    .transition(
                        workflow_id,
                        current.current_state,
                        current.current_state,
                        TransitionContext {
                            stage_context: Some(stage),
                            ..TransitionContext::default()
                        },
                    )
                    .await?;
                Ok(())
            },
            None => {
                let mut bundle = self.empty_snapshot(workflow_id);
                bundle.stage_context = stage;
                self.persist_state(bundle).await?;
                Ok(())
            },
        }
    }

    /// Initialise the budget state for a workflow. If already initialised, this
    /// is a no-op.
    pub async fn initialize(
        &self,
        workflow_id: &str,
        complexity: super::budget::TaskComplexity,
    ) -> Result<BudgetState, BudgetLedgerError> {
        if let Some(state) = self.state_tracker.latest_state(workflow_id).await? {
            if state.budget.initial > 0.0 {
                return Ok(state.budget);
            }
            return self
                .persist_budget(workflow_id, state, |budget, _| {
                    let initial = self.policy.initialize_budget(complexity);
                    budget.initial = initial;
                    budget.remaining = initial;
                    budget.spent.clear();
                    Ok(())
                })
                .await;
        }

        let mut bundle = self.empty_snapshot(workflow_id);
        let initial = self.policy.initialize_budget(complexity);
        bundle.budget.initial = initial;
        bundle.budget.remaining = initial;
        self.persist_state(bundle)
            .await
            .map(|snapshot| snapshot.budget)
    }

    /// Spend budget for the given reason, ensuring the ledger never goes
    /// negative.
    pub async fn spend(
        &self,
        workflow_id: &str,
        amount: f64,
        reason: String,
    ) -> Result<BudgetState, BudgetLedgerError> {
        if amount <= 0.0 {
            return Err(BudgetLedgerError::InvalidAmount(amount));
        }

        let state = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| BudgetLedgerError::UnknownWorkflow(workflow_id.to_string()))?;

        self.persist_budget(workflow_id, state, |budget, snapshot| {
            if budget.initial == 0.0 {
                budget.initial = self
                    .policy
                    .initialize_budget(super::budget::TaskComplexity::Moderate);
                if budget.remaining == 0.0 {
                    budget.remaining = budget.initial;
                }
            }

            if amount > budget.remaining + f64::EPSILON {
                return Err(BudgetLedgerError::InsufficientBudget {
                    requested: amount,
                    remaining: budget.remaining,
                });
            }

            budget.remaining = (budget.remaining - amount).max(0.0);
            budget.spent.push(BudgetSpend {
                amount,
                reason: reason.clone(),
                stage: snapshot.stage_context,
                timestamp: snapshot.created_at,
            });
            Ok(())
        })
        .await
    }

    /// Replenish budget, capping at the initial allocation.
    pub async fn replenish(
        &self,
        workflow_id: &str,
        amount: f64,
    ) -> Result<BudgetState, BudgetLedgerError> {
        if amount <= 0.0 {
            return Err(BudgetLedgerError::InvalidAmount(amount));
        }

        let state = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| BudgetLedgerError::UnknownWorkflow(workflow_id.to_string()))?;

        self.persist_budget(workflow_id, state, |budget, _snapshot| {
            let cap = if budget.initial > 0.0 {
                budget.initial
            } else {
                self.policy.config().initial_budget
            };
            budget.remaining = (budget.remaining + amount).min(cap);
            Ok(())
        })
        .await
    }

    /// Fetch the current budget state.
    pub async fn get_current(&self, workflow_id: &str) -> Result<BudgetState, BudgetLedgerError> {
        self.state_tracker
            .latest_state(workflow_id)
            .await?
            .map(|state| state.budget)
            .ok_or_else(|| BudgetLedgerError::UnknownWorkflow(workflow_id.to_string()))
    }

    /// Evaluate whether we should ask the user, using the policy and the latest
    /// state.
    pub async fn evaluate_ask_decision(
        &self,
        workflow_id: &str,
    ) -> Result<AskDecision, BudgetLedgerError> {
        let state = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| BudgetLedgerError::UnknownWorkflow(workflow_id.to_string()))?;

        let history = convert_confidence_history(&state.confidence);
        let last_ask = last_ask_timestamp(&state.budget);

        Ok(self.policy.should_ask(
            state.stage_context,
            state.budget.remaining,
            &history,
            last_ask,
        ))
    }

    /// Evaluate whether an ask should be triggered, forcing the evaluation to
    /// use the supplied stage context. This is used by planning strategies so
    /// stage-aware thresholds apply even before the tracker has recorded a
    /// dedicated snapshot for the new phase.
    pub async fn evaluate_ask_decision_for_stage(
        &self,
        workflow_id: &str,
        stage: StageContext,
    ) -> Result<AskDecision, BudgetLedgerError> {
        if stage != StageContext::Unknown {
            self.annotate_stage(workflow_id, stage).await?;
        }

        let state = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| BudgetLedgerError::UnknownWorkflow(workflow_id.to_string()))?;

        let history = convert_confidence_history(&state.confidence);
        let last_ask = last_ask_timestamp(&state.budget);
        let stage_context = if stage == StageContext::Unknown {
            state.stage_context
        } else {
            stage
        };

        Ok(self
            .policy
            .should_ask(stage_context, state.budget.remaining, &history, last_ask))
    }

    async fn persist_budget<F>(
        &self,
        _workflow_id: &str,
        state: StateBundle,
        mut mutate: F,
    ) -> Result<BudgetState, BudgetLedgerError>
    where
        F: FnMut(&mut BudgetState, &StateBundle) -> Result<(), BudgetLedgerError>,
    {
        let mut snapshot = self.clone_snapshot(state);
        let created_at = snapshot.created_at;
        let snapshot_clone = snapshot.clone();
        mutate(&mut snapshot.budget, &snapshot_clone)?;
        if let Some(last) = snapshot.budget.spent.last_mut() {
            last.timestamp = created_at;
            if matches!(last.stage, StageContext::Unknown) {
                last.stage = snapshot.stage_context;
            }
        }
        self.persist_state(snapshot).await.map(|state| state.budget)
    }

    fn clone_snapshot(&self, mut state: StateBundle) -> StateBundle {
        state.state_id = Uuid::new_v4().to_string();
        state.created_at = Utc::now();
        state.slot_deltas.clear();
        state.llm_reasoning = None;
        // NOTE: atomic_plan is no longer in StateBundle - stored in turn storage only
        state
    }

    fn empty_snapshot(&self, workflow_id: &str) -> StateBundle {
        StateBundle {
            state_id: Uuid::new_v4().to_string(),
            workflow_id: workflow_id.to_string(),
            current_state: WorkflowState::Observe,
            llm_reasoning: None,
            observations: Vec::new(),
            slot_deltas: Vec::new(),
            confidence: ConfidenceScore::default(),
            budget: BudgetState::default(),
            stage_context: StageContext::default(),
            completed_stages: Vec::new(),
            failed_stage: None,
            created_at: Utc::now(),
        }
    }

    async fn persist_state(&self, snapshot: StateBundle) -> Result<StateBundle, BudgetLedgerError> {
        self.state_tracker.record_state(snapshot.clone()).await?;
        Ok(snapshot)
    }
}

/// Ledger errors for budget operations.
#[derive(Debug, Error)]
pub enum BudgetLedgerError {
    #[error("workflow '{0}' has no state history")]
    UnknownWorkflow(String),
    #[error("invalid amount: {0}")]
    InvalidAmount(f64),
    #[error("insufficient budget: requested {requested:.2} but {remaining:.2} remaining")]
    InsufficientBudget { requested: f64, remaining: f64 },
    #[error("storage error: {0}")]
    Storage(#[from] crate::magician_v2::storage::V2StorageError),
}

fn convert_confidence_history(confidence: &ConfidenceScore) -> Vec<(DateTime<Utc>, f64)> {
    confidence
        .history
        .iter()
        .filter_map(|(ts, value)| {
            Utc.timestamp_millis_opt(*ts)
                .single()
                .map(|dt| (dt, *value))
        })
        .collect()
}

fn last_ask_timestamp(budget: &BudgetState) -> Option<DateTime<Utc>> {
    budget
        .spent
        .iter()
        .rev()
        .find(|spend| spend.reason.to_ascii_lowercase().contains("ask"))
        .map(|spend| spend.timestamp)
    // Note: Removed .or_else() fallback to fix sequential penalty being applied
    // to first questions when budget was spent on query analysis/strategy selection
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{ask_loop::BudgetConfig, ConfidenceService};
    use std::collections::HashMap;
    use tokio::runtime::Runtime;

    fn test_state_tracker() -> (Arc<StateTracker>, Arc<ConfidenceService>) {
        use crate::magician_v2::storage::V2StorageError;
        use async_trait::async_trait;
        use runtime_core::V2ConversationStore as CoreV2ConversationStore;
        use tokio::sync::Mutex;

        #[derive(Default)]
        struct InMemoryStore {
            states: Mutex<HashMap<String, Vec<StateBundle>>>,
        }

        #[async_trait]
        impl CoreV2ConversationStore for InMemoryStore {
            type Error = V2StorageError;
            type Execution = crate::magician_v2::storage::ExecutionRun;
            type ExecutionSummary = crate::magician_v2::storage::ExecutionSummary;
            type ExecutionStatus = crate::magician_v2::storage::WaitingState;
            type Turn = crate::magician_v2::storage::V2Turn;
            type TurnDirection = crate::magician_v2::storage::TurnDirection;
            type Slot = crate::magician_v2::storage::V2Slot;
            type SlotStatus = crate::magician_v2::storage::V2SlotStatus;
            type StrategyAttempt = crate::magician_v2::storage::StrategyAttempt;
            type ProcessingMetadata =
                crate::magician_v2::orchestrator::v2_orchestrator::ProcessingMetadata;
            type UnifiedAnalysis = crate::magician_v2::UnifiedQueryAnalysis;
            type AnalysisMetadata = crate::magician_v2::AnalysisMetadata;
            type StateBundle = StateBundle;
            type RecommendedQuestion =
                crate::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion;
            type PendingClarification =
                crate::magician_v2::orchestrator::v2_orchestrator::PendingClarification;
            type ClarificationSession = crate::magician_v2::ask_loop::ClarificationSession;
            type ClarificationHistoryEntry =
                crate::magician_v2::storage::models::ClarificationHistoryEntry;
            async fn create_execution_with_options(
                &self,
                _principal: &str,
                _workspace: &str,
                _title: Option<String>,
                _active_owner_agent_id: &str,
                _task_id: Option<String>,
                _root_execution_id: Option<String>,
                _thread_id: Option<String>,
                _parent_thread_id: Option<String>,
                _timeout_secs: Option<u64>,
                _delegation_chain: Vec<String>,
                _waiting_state: crate::magician_v2::storage::WaitingState,
            ) -> Result<crate::magician_v2::storage::ExecutionRun, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
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
                thread_id: Option<String>,
                parent_thread_id: Option<String>,
                timeout_secs: Option<u64>,
                delegation_chain: Vec<String>,
                waiting_state: crate::magician_v2::storage::WaitingState,
                _work_authority: Option<runtime_core::WorkAuthorityGrant>,
            ) -> Result<crate::magician_v2::storage::ExecutionRun, V2StorageError> {
                CoreV2ConversationStore::create_execution_with_options(
                    self,
                    principal,
                    workspace,
                    title,
                    active_owner_agent_id,
                    task_id,
                    root_execution_id,
                    thread_id,
                    parent_thread_id,
                    timeout_secs,
                    delegation_chain,
                    waiting_state,
                )
                .await
            }

            async fn get_execution(
                &self,
                _thread_id: &str,
            ) -> Result<crate::magician_v2::storage::ExecutionRun, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn update_execution_entry_mode(
                &self,
                _execution_id: &str,
                _entry_mode: String,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn add_turn(
                &self,
                _thread_id: &str,
                _direction: crate::magician_v2::storage::TurnDirection,
                _text: String,
                _in_reply_to_slot_id: Option<String>,
            ) -> Result<crate::magician_v2::storage::V2Turn, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn store_analysis(
                &self,
                _thread_id: &str,
                _turn_id: &str,
                _analysis: crate::magician_v2::UnifiedQueryAnalysis,
                _metadata: crate::magician_v2::AnalysisMetadata,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn store_strategy_attempts(
                &self,
                _thread_id: &str,
                _turn_id: &str,
                _attempts: Vec<crate::magician_v2::storage::StrategyAttempt>,
                _processing_metadata: crate::magician_v2::orchestrator::v2_orchestrator::ProcessingMetadata,
                _recommended_questions: Option<
                    Vec<crate::magician_v2::orchestrator::v2_orchestrator::RecommendedQuestion>,
                >,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_turn(
                &self,
                _thread_id: &str,
                _turn_id: &str,
            ) -> Result<crate::magician_v2::storage::V2Turn, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_turns(
                &self,
                _thread_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Turn>, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_latest_turn_with_analysis(
                &self,
                _thread_id: &str,
            ) -> Result<Option<crate::magician_v2::storage::V2Turn>, V2StorageError> {
                Ok(None)
            }

            async fn list_executions(
                &self,
                _principal: &str,
                _workspace: &str,
                _pagination: crate::magician_v2::storage::PaginationParams,
            ) -> Result<
                crate::magician_v2::storage::PaginatedResult<
                    crate::magician_v2::storage::ExecutionSummary,
                >,
                V2StorageError,
            > {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn delete_execution(&self, _thread_id: &str) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_turns_paginated(
                &self,
                _thread_id: &str,
                _pagination: crate::magician_v2::storage::PaginationParams,
                _direction_filter: Option<crate::magician_v2::storage::TurnDirection>,
            ) -> Result<
                crate::magician_v2::storage::PaginatedResult<crate::magician_v2::storage::V2Turn>,
                V2StorageError,
            > {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn update_execution_status(
                &self,
                _thread_id: &str,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn compare_exchange_execution_status(
                &self,
                _thread_id: &str,
                _expected: crate::magician_v2::storage::WaitingState,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<bool, V2StorageError> {
                Ok(false)
            }

            async fn compare_exchange_execution_status_at(
                &self,
                _thread_id: &str,
                _expected: crate::magician_v2::storage::WaitingState,
                _expected_status_revision: u64,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<bool, V2StorageError> {
                Ok(false)
            }

            async fn get_execution_status_revision(
                &self,
                _thread_id: &str,
            ) -> Result<u64, V2StorageError> {
                Ok(0)
            }

            async fn bind_execution_scope(
                &self,
                _thread_id: &str,
                _task_id: Option<String>,
                _root_execution_id: Option<String>,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn add_child_execution_id(
                &self,
                _parent_thread_id: &str,
                _child_thread_id: &str,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn update_execution_owner_snapshot(
                &self,
                _thread_id: &str,
                _active_owner_agent_id: &str,
                _owner_stack: &[String],
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn replace_active_delegation_group(
                &self,
                _thread_id: &str,
                _active_delegation_group: &[String],
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_execution_status(
                &self,
                execution_id: &str,
            ) -> Result<crate::magician_v2::storage::WaitingState, V2StorageError> {
                Err(V2StorageError::ExecutionNotFound(execution_id.to_string()))
            }

            async fn update_processing_correlation_id(
                &self,
                _thread_id: &str,
                _correlation_id: Option<String>,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn create_slot(
                &self,
                _thread_id: &str,
                _name: String,
                _schema_json: serde_json::Value,
                _required: bool,
                _asked_turn_id: Option<String>,
            ) -> Result<crate::magician_v2::storage::V2Slot, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn update_slot_answer(
                &self,
                _thread_id: &str,
                _slot_id: &str,
                _answer: serde_json::Value,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn update_slot_status(
                &self,
                _thread_id: &str,
                _slot_id: &str,
                _status: crate::magician_v2::storage::V2SlotStatus,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn get_slots(
                &self,
                _thread_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Slot>, V2StorageError> {
                Ok(Vec::new())
            }

            async fn get_pending_slots(
                &self,
                _thread_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Slot>, V2StorageError> {
                Ok(Vec::new())
            }

            async fn get_slot(
                &self,
                _thread_id: &str,
                _slot_id: &str,
            ) -> Result<crate::magician_v2::storage::V2Slot, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn append_state(
                &self,
                thread_id: &str,
                state: StateBundle,
            ) -> Result<(), V2StorageError> {
                let mut map = self.states.lock().await;
                map.entry(thread_id.to_string()).or_default().push(state);
                Ok(())
            }

            async fn get_states(
                &self,
                thread_id: &str,
            ) -> Result<Vec<StateBundle>, V2StorageError> {
                let map = self.states.lock().await;
                Ok(map.get(thread_id).cloned().unwrap_or_default())
            }

            async fn get_latest_state(
                &self,
                thread_id: &str,
            ) -> Result<Option<StateBundle>, V2StorageError> {
                let map = self.states.lock().await;
                Ok(map
                    .get(thread_id)
                    .and_then(|states| states.iter().cloned().max_by_key(|s| s.created_at)))
            }

            async fn update_execution_processing_stage(
                &self,
                _thread_id: &str,
                _stage: Option<String>,
                _provider: Option<String>,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn update_turn_enriched_query(
                &self,
                _thread_id: &str,
                _turn_id: &str,
                _enriched_query: Option<String>,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn load_clarification_session(
                &self,
                _thread_id: &str,
            ) -> Result<Option<Self::ClarificationSession>, V2StorageError> {
                Ok(None)
            }

            async fn store_clarification_session(
                &self,
                _thread_id: &str,
                _session: Self::ClarificationSession,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn delete_clarification_session(
                &self,
                _thread_id: &str,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn get_clarification_history(
                &self,
                _thread_id: &str,
            ) -> Result<
                Vec<crate::magician_v2::storage::models::ClarificationHistoryEntry>,
                V2StorageError,
            > {
                Ok(vec![])
            }

            async fn append_clarification_history(
                &self,
                _thread_id: &str,
                _entries: Vec<crate::magician_v2::storage::models::ClarificationHistoryEntry>,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }
        }

        let store = Arc::new(InMemoryStore::default());
        let confidence_service = Arc::new(ConfidenceService::default());
        let tracker = Arc::new(StateTracker::with_confidence_service(
            store,
            confidence_service.clone(),
        ));
        (tracker, confidence_service)
    }

    #[test]
    fn ledger_initialises_budget() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tracker, confidence_service) = test_state_tracker();
            let policy = Arc::new(BudgetPolicy::with_confidence_service(
                BudgetConfig::default(),
                confidence_service.clone(),
            ));
            let ledger = BudgetLedger::new(tracker.clone(), policy.clone());

            let workflow = "wf-initialise";
            ledger
                .initialize(workflow, super::super::budget::TaskComplexity::Simple)
                .await
                .unwrap();

            let state = tracker.latest_state(workflow).await.unwrap().unwrap();
            assert!(state.budget.initial > 0.0);
            assert_eq!(state.budget.remaining, state.budget.initial);
        });
    }

    #[test]
    fn ledger_prevents_negative_budget() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tracker, confidence_service) = test_state_tracker();
            let policy = Arc::new(BudgetPolicy::with_confidence_service(
                BudgetConfig::default(),
                confidence_service.clone(),
            ));
            let ledger = BudgetLedger::new(tracker.clone(), policy.clone());

            let workflow = "wf-budget";
            let state = ledger
                .initialize(workflow, super::super::budget::TaskComplexity::Moderate)
                .await
                .unwrap();
            let overspend = state.initial + 1.0;
            let err = ledger
                .spend(workflow, overspend, "ask".to_string())
                .await
                .unwrap_err();
            assert!(matches!(err, BudgetLedgerError::InsufficientBudget { .. }));
        });
    }

    #[test]
    fn ledger_records_spend() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let (tracker, confidence_service) = test_state_tracker();
            let policy = Arc::new(BudgetPolicy::with_confidence_service(
                BudgetConfig::default(),
                confidence_service.clone(),
            ));
            let ledger = BudgetLedger::new(tracker.clone(), policy.clone());

            let workflow = "wf-spend";
            ledger
                .initialize(workflow, super::super::budget::TaskComplexity::Moderate)
                .await
                .unwrap();
            ledger
                .spend(workflow, 1.5, "clarifier ask".to_string())
                .await
                .unwrap();

            let state = tracker.latest_state(workflow).await.unwrap().unwrap();
            assert_eq!(state.budget.spent.len(), 1);
        });
    }
}

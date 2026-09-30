use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::info;
use uuid::Uuid;

use crate::magician_v2::{
    slot_graph::SlotRecord,
    state_tracker::{DeltaOperation, SlotDelta, StateBundle, StateTracker, WorkflowState},
};

/// High-level manager handling workflow pauses and resumes.
pub struct PauseResumeManager {
    state_tracker: Arc<StateTracker>,
    waiting_queue: Arc<WaitingQueue>,
}

impl PauseResumeManager {
    pub fn new(state_tracker: Arc<StateTracker>, waiting_queue: Arc<WaitingQueue>) -> Self {
        Self {
            state_tracker,
            waiting_queue,
        }
    }

    pub fn state_tracker(&self) -> Arc<StateTracker> {
        Arc::clone(&self.state_tracker)
    }

    /// Pause a workflow for the given reason.
    pub async fn pause(
        &self,
        workflow_id: &str,
        reason: PauseReason,
    ) -> Result<PausedWorkflow, PauseError> {
        let latest = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| PauseError::StateUnavailable(workflow_id.to_string()))?;

        let pending_questions = match &reason {
            PauseReason::WaitingOnUser(question_id) => vec![question_id.clone()],
            _ => Vec::new(),
        };

        let paused = PausedWorkflow {
            workflow_id: workflow_id.to_string(),
            paused_at: Utc::now(),
            reason: reason.clone(),
            checkpoint_id: latest.state_id.clone(),
            pending_questions,
        };

        self.waiting_queue.enqueue(paused.clone()).await?;
        Ok(paused)
    }

    /// Pause a workflow for multiple questions (batch mode).
    ///
    /// This is used when enqueueing a batch of questions where all question IDs
    /// need to be tracked in the pause manager so answers can be matched.
    pub async fn pause_for_batch(
        &self,
        workflow_id: &str,
        question_ids: Vec<String>,
    ) -> Result<PausedWorkflow, PauseError> {
        info!(
            "[MAGICIAN-PAUSE-BATCH] Pausing workflow {} for batch of {} questions: {:?}",
            workflow_id,
            question_ids.len(),
            question_ids
        );

        let latest = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| PauseError::StateUnavailable(workflow_id.to_string()))?;

        // Use the first question ID as the reason, but track all questions
        let reason = if let Some(first_id) = question_ids.first() {
            PauseReason::WaitingOnUser(first_id.clone())
        } else {
            return Err(PauseError::Queue(
                "No questions provided for batch pause".to_string(),
            ));
        };

        let paused = PausedWorkflow {
            workflow_id: workflow_id.to_string(),
            paused_at: Utc::now(),
            reason,
            checkpoint_id: latest.state_id.clone(),
            pending_questions: question_ids.clone(),
        };

        self.waiting_queue.enqueue(paused.clone()).await?;

        info!(
            "[MAGICIAN-PAUSE-BATCH] Successfully paused workflow {} with pending_questions: {:?}",
            workflow_id, paused.pending_questions
        );

        Ok(paused)
    }

    /// Resume a workflow with optional slot updates and confidence adjustments.
    pub async fn resume(
        &self,
        workflow_id: &str,
        resume_context: ResumeContext,
    ) -> Result<StateBundle, PauseError> {
        let prepared = self.prepare_resume(workflow_id, resume_context).await?;
        self.commit_prepared_resume(&prepared).await
    }

    /// Build the exact state/queue mutation without changing state. Artifact
    /// planning seals this value before an answer or pause is consumed.
    pub async fn prepare_resume(
        &self,
        workflow_id: &str,
        resume_context: ResumeContext,
    ) -> Result<PreparedPauseResume, PauseError> {
        let ResumeContext {
            user_provided_slots,
            updated_confidence,
            handled_question,
        } = resume_context;

        let paused = self
            .waiting_queue
            .find_by_workflow(workflow_id)
            .await?
            .ok_or_else(|| PauseError::NotPaused(workflow_id.to_string()))?;

        let latest = self
            .state_tracker
            .latest_state(workflow_id)
            .await?
            .ok_or_else(|| PauseError::StateUnavailable(workflow_id.to_string()))?;

        let source_state_id = latest.state_id.clone();
        let mut bundle = latest;
        bundle.state_id = Uuid::new_v4().to_string();
        bundle.created_at = Utc::now();

        // Clear any clarify gating reason once we resume so Mission Control
        // no longer surfaces a stale pending clarification banner.
        bundle.llm_reasoning = None;

        if bundle.current_state == WorkflowState::Pause {
            bundle.current_state = WorkflowState::Observe;
        }

        bundle.slot_deltas = user_provided_slots
            .iter()
            .map(|slot| SlotDelta {
                slot_id: slot.id.clone(),
                operation: DeltaOperation::Update,
                previous_value: None,
                new_value: slot.value.clone(),
                stage: bundle.stage_context,
            })
            .collect();

        let mut summary = None;
        if !user_provided_slots.is_empty() {
            summary = self
                .state_tracker
                .integrate_slot_records(&mut bundle.confidence, &user_provided_slots);
        }

        if let Some(confidence) = updated_confidence {
            bundle.confidence.overall = confidence;
            if let Some(existing) = bundle.confidence.summary.as_mut() {
                existing.overall = confidence;
            }
        }

        if summary.is_some() || updated_confidence.is_some() {
            bundle.confidence.history.push((
                bundle.created_at.timestamp_millis(),
                bundle.confidence.overall,
            ));
            self.state_tracker
                .recalculate_confidence_slope(&mut bundle.confidence);
        }

        let mut remaining_questions = paused.pending_questions.clone();

        // ENHANCED LOGGING: Track pending_questions before removal
        let initial_count = remaining_questions.len();
        info!(
            "[MAGICIAN-PAUSE-MANAGER] 📋 PENDING QUESTIONS BEFORE REMOVAL for workflow {}\n\
             - Initial pending count: {}\n\
             - Initial pending IDs: {:?}\n\
             - Handled question ID: {:?}",
            paused.workflow_id, initial_count, remaining_questions, handled_question
        );

        if let Some(question_id) = handled_question {
            remaining_questions.retain(|q| q != &question_id);

            // ENHANCED LOGGING: Track pending_questions after removal
            let removed_count = initial_count - remaining_questions.len();
            info!(
                "[MAGICIAN-PAUSE-MANAGER] ✅ PENDING QUESTIONS AFTER REMOVAL for workflow {}\n\
                 - Questions removed: {}\n\
                 - Remaining pending count: {}\n\
                 - Remaining pending IDs: {:?}",
                paused.workflow_id,
                removed_count,
                remaining_questions.len(),
                remaining_questions
            );
        }

        Ok(PreparedPauseResume {
            schema_version: 1,
            workflow_id: workflow_id.to_owned(),
            source_state_id,
            paused,
            remaining_questions,
            resumed_state: bundle,
        })
    }

    /// Apply a sealed resume preparation idempotently. The state id is fixed by
    /// the preparation, so replay after a crash cannot mint a second resume.
    pub async fn commit_prepared_resume(
        &self,
        prepared: &PreparedPauseResume,
    ) -> Result<StateBundle, PauseError> {
        if prepared.schema_version != 1
            || prepared.workflow_id != prepared.paused.workflow_id
            || prepared.workflow_id != prepared.resumed_state.workflow_id
        {
            return Err(PauseError::InvalidPreparation(prepared.workflow_id.clone()));
        }

        // Once the durable preparation exists, losing the volatile pause entry
        // is harmless: replay owns the exact state and replacement queue value.
        let _ = self.waiting_queue.remove(&prepared.workflow_id).await?;
        let states = self
            .state_tracker
            .list_states(&prepared.workflow_id)
            .await?;
        if !states
            .iter()
            .any(|state| state.state_id == prepared.resumed_state.state_id)
        {
            let latest = states
                .iter()
                .max_by(|left, right| left.created_at.cmp(&right.created_at))
                .ok_or_else(|| PauseError::StateUnavailable(prepared.workflow_id.clone()))?;
            if latest.state_id != prepared.source_state_id {
                return Err(PauseError::PreparationConflict(
                    prepared.workflow_id.clone(),
                ));
            }
            self.state_tracker
                .record_state(prepared.resumed_state.clone())
                .await?;
        }

        if !prepared.remaining_questions.is_empty() {
            info!(
                "[MAGICIAN-PAUSE-MANAGER] 🔄 RE-QUEUING workflow {} with {} remaining questions",
                prepared.workflow_id,
                prepared.remaining_questions.len()
            );
            self.waiting_queue
                .enqueue(PausedWorkflow {
                    pending_questions: prepared.remaining_questions.clone(),
                    ..prepared.paused.clone()
                })
                .await?;
        } else {
            info!(
                "[MAGICIAN-PAUSE-MANAGER] ✅ ALL QUESTIONS ANSWERED for workflow {}, not re-queuing",
                prepared.workflow_id
            );
        }

        Ok(prepared.resumed_state.clone())
    }

    /// List all paused workflows.
    pub async fn get_paused_workflows(&self) -> Result<Vec<PausedWorkflow>, PauseError> {
        self.waiting_queue.list().await
    }

    /// Find a paused workflow that references the given question identifier.
    pub async fn find_by_question(
        &self,
        question_id: &str,
    ) -> Result<Option<PausedWorkflow>, PauseError> {
        self.waiting_queue.find_by_question(question_id).await
    }
}

/// Queue abstraction for storing paused workflows.
pub struct WaitingQueue {
    repository: Arc<dyn QueueRepository>,
}

impl WaitingQueue {
    pub fn new(repository: Arc<dyn QueueRepository>) -> Self {
        Self { repository }
    }

    pub async fn enqueue(&self, workflow: PausedWorkflow) -> Result<(), PauseError> {
        self.repository.save(workflow).await
    }

    pub async fn remove(&self, workflow_id: &str) -> Result<Option<PausedWorkflow>, PauseError> {
        self.repository.remove(workflow_id).await
    }

    pub async fn list(&self) -> Result<Vec<PausedWorkflow>, PauseError> {
        self.repository.list().await
    }

    pub async fn find_by_question(
        &self,
        question_id: &str,
    ) -> Result<Option<PausedWorkflow>, PauseError> {
        self.repository.find_by_question(question_id).await
    }

    pub async fn find_by_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<PausedWorkflow>, PauseError> {
        self.repository.find_by_workflow(workflow_id).await
    }
}

/// Repository trait for storing paused workflow metadata.
#[async_trait]
pub trait QueueRepository: Send + Sync {
    async fn save(&self, workflow: PausedWorkflow) -> Result<(), PauseError>;
    async fn remove(&self, workflow_id: &str) -> Result<Option<PausedWorkflow>, PauseError>;
    async fn list(&self) -> Result<Vec<PausedWorkflow>, PauseError>;

    async fn find_by_question(
        &self,
        question_id: &str,
    ) -> Result<Option<PausedWorkflow>, PauseError> {
        let workflows = self.list().await?;

        info!(
            "[MAGICIAN-PAUSE-FIND] Looking for question '{}' in {} paused workflows",
            question_id,
            workflows.len()
        );

        for wf in &workflows {
            info!(
                "[MAGICIAN-PAUSE-FIND] Workflow {} has {} pending questions: {:?}",
                wf.workflow_id,
                wf.pending_questions.len(),
                wf.pending_questions
            );
        }

        let result = workflows
            .into_iter()
            .find(|wf| wf.pending_questions.iter().any(|q| q == question_id));

        if result.is_some() {
            info!(
                "[MAGICIAN-PAUSE-FIND] Found workflow for question '{}'",
                question_id
            );
        } else {
            info!(
                "[MAGICIAN-PAUSE-FIND] No workflow found for question '{}'",
                question_id
            );
        }

        Ok(result)
    }

    async fn find_by_workflow(
        &self,
        workflow_id: &str,
    ) -> Result<Option<PausedWorkflow>, PauseError> {
        let workflows = self.list().await?;
        let mut matching = workflows
            .into_iter()
            .filter(|workflow| workflow.workflow_id == workflow_id);
        let Some(mut merged) = matching.next() else {
            return Ok(None);
        };
        for workflow in matching {
            for question in workflow.pending_questions {
                if !merged.pending_questions.contains(&question) {
                    merged.pending_questions.push(question);
                }
            }
        }
        Ok(Some(merged))
    }
}

/// In-memory queue repository used for testing and local development.
#[derive(Default)]
pub struct InMemoryQueueRepository {
    inner: DashMap<String, PausedWorkflow>,
}

#[async_trait]
impl QueueRepository for InMemoryQueueRepository {
    async fn save(&self, workflow: PausedWorkflow) -> Result<(), PauseError> {
        // Key by checkpoint_id (unique per pause) so that two consecutive Pause
        // routing decisions for the same workflow_id don't overwrite each other.
        self.inner.insert(workflow.checkpoint_id.clone(), workflow);
        Ok(())
    }

    async fn remove(&self, workflow_id: &str) -> Result<Option<PausedWorkflow>, PauseError> {
        // Idempotently remove ALL entries whose value has the matching
        // workflow_id — not just the first (#22). Because entries are keyed by
        // checkpoint_id, a single workflow can accumulate multiple entries
        // (e.g. a pipeline-suspension pause plus the real question-batch pause
        // minted at a later state_id). Removing only the first left a duplicate
        // lingering and made removal non-deterministic. We merge the removed
        // entries' pending_questions so no answerable question is silently
        // dropped, and return the merged workflow (or None if none matched).
        let keys: Vec<String> = self
            .inner
            .iter()
            .filter(|e| e.value().workflow_id == workflow_id)
            .map(|e| e.key().clone())
            .collect();

        let mut merged: Option<PausedWorkflow> = None;
        for key in keys {
            if let Some((_, wf)) = self.inner.remove(&key) {
                match merged.as_mut() {
                    None => merged = Some(wf),
                    Some(acc) => {
                        for q in wf.pending_questions {
                            if !acc.pending_questions.contains(&q) {
                                acc.pending_questions.push(q);
                            }
                        }
                    },
                }
            }
        }
        Ok(merged)
    }

    async fn list(&self) -> Result<Vec<PausedWorkflow>, PauseError> {
        Ok(self
            .inner
            .iter()
            .map(|entry| entry.value().clone())
            .collect())
    }
}

/// Serialized representation of a paused workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PausedWorkflow {
    pub workflow_id: String,
    pub paused_at: chrono::DateTime<Utc>,
    pub reason: PauseReason,
    pub checkpoint_id: String,
    pub pending_questions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PauseReason {
    WaitingOnUser(String),
    BudgetExhausted,
    ManualPause,
    /// A pipeline stage suspended awaiting user input. The user-facing question
    /// batch is registered separately (via `pause_for_batch`) by the dispatch
    /// path, so this reason MUST NOT contribute a `pending_questions` id — doing
    /// so registered a phantom queue entry keyed by a throwaway checkpoint uuid
    /// distinct from the real answerable question ids (#22).
    PipelineSuspended,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeContext {
    pub user_provided_slots: Vec<SlotRecord>,
    pub updated_confidence: Option<f64>,
    pub handled_question: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedPauseResume {
    pub schema_version: u8,
    pub workflow_id: String,
    pub source_state_id: String,
    pub paused: PausedWorkflow,
    pub remaining_questions: Vec<String>,
    pub resumed_state: StateBundle,
}

/// Determines the type of resume operation to perform after receiving clarification answers.
///
/// This enum controls whether we perform a lightweight slot update, partial replanning,
/// or full replanning based on batch completion status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResumeMode {
    /// Just update slot values without replanning
    ///
    /// Used when: Individual answers within incomplete batch
    /// Cost: Minimal (slot updates only)
    /// Behavior: Updates confidence, no LLM replanning
    LightSlotUpdate,

    /// Replan with current batch answers (batch incomplete)
    ///
    /// Used when: Partial batch progress warrants replanning
    /// Cost: Medium (replanning with partial context)
    /// Behavior: Incremental plan refinement
    PartialReplan,

    /// Full replan after batch completion
    ///
    /// Used when: All questions in batch answered
    /// Cost: High (full replanning cycle)
    /// Behavior: Complete plan regeneration with all answers
    FullReplan,
    // NOTE: StateRecovery variant removed - agentic execution handles state recovery
    // via observe-decide-execute loop. See execution/agentic/ module.
}

#[derive(Debug, Error)]
pub enum PauseError {
    #[error("workflow '{0}' is not currently paused")]
    NotPaused(String),
    #[error("no state available for workflow '{0}'")]
    StateUnavailable(String),
    #[error("storage error: {0}")]
    Storage(#[from] crate::magician_v2::storage::V2StorageError),
    #[error("queue error: {0}")]
    Queue(String),
    #[error("invalid sealed resume preparation for workflow '{0}'")]
    InvalidPreparation(String),
    #[error("workflow '{0}' advanced beyond its sealed resume preparation")]
    PreparationConflict(String),
}

impl From<std::io::Error> for PauseError {
    fn from(err: std::io::Error) -> Self {
        PauseError::Queue(err.to_string())
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType},
        state_tracker::{BudgetState, ConfidenceScore},
        StageContext,
    };
    use runtime_core::V2ConversationStore as CoreV2ConversationStore;
    use std::collections::HashMap;
    use tokio::runtime::Runtime;

    fn test_state_tracker() -> Arc<StateTracker> {
        use crate::magician_v2::storage::V2StorageError;
        use async_trait::async_trait;
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
                _execution_id: Option<String>,
                _parent_execution_id: Option<String>,
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
                execution_id: Option<String>,
                parent_execution_id: Option<String>,
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
                    execution_id,
                    parent_execution_id,
                    timeout_secs,
                    delegation_chain,
                    waiting_state,
                )
                .await
            }

            async fn get_execution(
                &self,
                _execution_id: &str,
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
                _execution_id: &str,
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
                _execution_id: &str,
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
                _execution_id: &str,
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
                _execution_id: &str,
                _turn_id: &str,
            ) -> Result<crate::magician_v2::storage::V2Turn, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_turns(
                &self,
                _execution_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Turn>, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_latest_turn_with_analysis(
                &self,
                _execution_id: &str,
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

            async fn delete_execution(&self, _execution_id: &str) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn get_turns_paginated(
                &self,
                _execution_id: &str,
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
                _execution_id: &str,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn compare_exchange_execution_status(
                &self,
                _execution_id: &str,
                _expected: crate::magician_v2::storage::WaitingState,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<bool, V2StorageError> {
                Ok(false)
            }

            async fn compare_exchange_execution_status_at(
                &self,
                _execution_id: &str,
                _expected: crate::magician_v2::storage::WaitingState,
                _expected_status_revision: u64,
                _status: crate::magician_v2::storage::WaitingState,
            ) -> Result<bool, V2StorageError> {
                Ok(false)
            }

            async fn get_execution_status_revision(
                &self,
                _execution_id: &str,
            ) -> Result<u64, V2StorageError> {
                Ok(0)
            }

            async fn bind_execution_scope(
                &self,
                _execution_id: &str,
                _task_id: Option<String>,
                _root_execution_id: Option<String>,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn add_child_execution_id(
                &self,
                _parent_execution_id: &str,
                _child_execution_id: &str,
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn update_execution_owner_snapshot(
                &self,
                _execution_id: &str,
                _active_owner_agent_id: &str,
                _owner_stack: &[String],
            ) -> Result<(), V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
                ))
            }

            async fn replace_active_delegation_group(
                &self,
                _execution_id: &str,
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
            ) -> Result<crate::magician_v2::storage::V2Slot, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
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
                _status: crate::magician_v2::storage::V2SlotStatus,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }

            async fn get_slots(
                &self,
                _execution_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Slot>, V2StorageError> {
                Ok(Vec::new())
            }

            async fn get_pending_slots(
                &self,
                _execution_id: &str,
            ) -> Result<Vec<crate::magician_v2::storage::V2Slot>, V2StorageError> {
                Ok(Vec::new())
            }

            async fn get_slot(
                &self,
                _execution_id: &str,
                _slot_id: &str,
            ) -> Result<crate::magician_v2::storage::V2Slot, V2StorageError> {
                Err(V2StorageError::Storage(
                    "not required for tests".to_string(),
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

            async fn get_states(
                &self,
                execution_id: &str,
            ) -> Result<Vec<StateBundle>, V2StorageError> {
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
                    .and_then(|states| states.iter().cloned().max_by_key(|s| s.created_at)))
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
            ) -> Result<
                Vec<crate::magician_v2::storage::models::ClarificationHistoryEntry>,
                V2StorageError,
            > {
                Ok(vec![])
            }

            async fn append_clarification_history(
                &self,
                _execution_id: &str,
                _entries: Vec<crate::magician_v2::storage::models::ClarificationHistoryEntry>,
            ) -> Result<(), V2StorageError> {
                Ok(())
            }
        }

        Arc::new(StateTracker::new(Arc::new(InMemoryStore::default())))
    }

    #[test]
    fn pause_and_resume_workflow() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tracker = test_state_tracker();
            let repository =
                Arc::new(InMemoryQueueRepository::default()) as Arc<dyn QueueRepository>;
            let queue = Arc::new(WaitingQueue::new(repository));
            let manager = PauseResumeManager::new(tracker.clone(), queue.clone());

            let workflow_id = "wf-pause";

            let bundle = StateBundle {
                state_id: Uuid::new_v4().to_string(),
                workflow_id: workflow_id.to_string(),
                current_state: WorkflowState::Observe,
                llm_reasoning: None,
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence: ConfidenceScore::default(),
                budget: BudgetState::default(),
                stage_context: StageContext::PlanningBootstrap,
                completed_stages: Vec::new(),
                failed_stage: None,
                created_at: Utc::now(),
            };
            tracker.record_state(bundle).await.unwrap();

            let reason = PauseReason::WaitingOnUser("q1".to_string());
            manager.pause(workflow_id, reason.clone()).await.unwrap();

            assert_eq!(manager.get_paused_workflows().await.unwrap().len(), 1);

            let resume_context = ResumeContext {
                user_provided_slots: Vec::new(),
                updated_confidence: Some(0.6),
                handled_question: Some("q1".to_string()),
            };

            manager.resume(workflow_id, resume_context).await.unwrap();
            assert!(manager.get_paused_workflows().await.unwrap().is_empty());
        });
    }

    #[test]
    fn resume_updates_confidence_from_slots() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tracker = test_state_tracker();
            let repository =
                Arc::new(InMemoryQueueRepository::default()) as Arc<dyn QueueRepository>;
            let queue = Arc::new(WaitingQueue::new(repository));
            let manager = PauseResumeManager::new(tracker.clone(), queue.clone());

            let workflow_id = "wf-slot-confidence";
            let initial_bundle = StateBundle {
                state_id: Uuid::new_v4().to_string(),
                workflow_id: workflow_id.to_string(),
                current_state: WorkflowState::Observe,
                llm_reasoning: None,
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence: ConfidenceScore::default(),
                budget: BudgetState::default(),
                stage_context: StageContext::PlanningBootstrap,
                completed_stages: Vec::new(),
                failed_stage: None,
                created_at: Utc::now(),
            };
            tracker.record_state(initial_bundle).await.unwrap();

            let pause_reason = PauseReason::WaitingOnUser("slot-q".to_string());
            manager.pause(workflow_id, pause_reason).await.unwrap();

            let now = Utc::now();
            let slot = SlotRecord {
                id: "entity::name".to_string(),
                slot_type: SlotType::Entity,
                value: serde_json::json!("Acme Corp"),
                confidence: 0.5,
                provenance: vec![ProvenanceRecord {
                    source: ProvenanceSource::UserReply,
                    timestamp: now,
                }],
                evidence_links: Vec::new(),
                created_at: now,
                updated_at: now,
            };

            let resume_bundle = manager
                .resume(
                    workflow_id,
                    ResumeContext {
                        user_provided_slots: vec![slot.clone()],
                        updated_confidence: None,
                        handled_question: Some("slot-q".to_string()),
                    },
                )
                .await
                .expect("resume should succeed");

            let expected_confidence = tracker
                .confidence_service()
                .calculate_slot_confidence(&slot);
            let slot_confidence = resume_bundle
                .confidence
                .per_slot
                .get(&slot.id)
                .copied()
                .expect("slot confidence recorded");
            assert!(
                (slot_confidence - expected_confidence).abs() < 1e-6,
                "expected per-slot confidence {:.4} to equal computed {:.4}",
                slot_confidence,
                expected_confidence
            );

            let summary = resume_bundle
                .confidence
                .summary
                .as_ref()
                .expect("confidence summary populated");
            assert_eq!(
                summary.overall, resume_bundle.confidence.overall,
                "summary overall should match stored overall"
            );
            assert!(
                resume_bundle
                    .confidence
                    .history
                    .iter()
                    .any(|(_, value)| (value - resume_bundle.confidence.overall).abs() < 1e-6),
                "history should include updated overall confidence"
            );
            assert!(
                resume_bundle.confidence.slot_records.contains_key(&slot.id),
                "slot snapshot should be stored"
            );
        });
    }

    #[test]
    fn sealed_resume_preparation_is_non_mutating_and_commit_is_idempotent() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let tracker = test_state_tracker();
            let repository =
                Arc::new(InMemoryQueueRepository::default()) as Arc<dyn QueueRepository>;
            let queue = Arc::new(WaitingQueue::new(repository));
            let manager = PauseResumeManager::new(tracker.clone(), queue);
            let workflow_id = "planexec-prepared-resume";
            let source = StateBundle {
                state_id: "source-state".to_owned(),
                workflow_id: workflow_id.to_owned(),
                current_state: WorkflowState::Pause,
                llm_reasoning: Some("waiting".to_owned()),
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence: ConfidenceScore::default(),
                budget: BudgetState::default(),
                stage_context: StageContext::PlanningBootstrap,
                completed_stages: Vec::new(),
                failed_stage: None,
                created_at: Utc::now(),
            };
            tracker.record_state(source.clone()).await.unwrap();
            manager
                .pause_for_batch(workflow_id, vec!["q1".to_owned(), "q2".to_owned()])
                .await
                .unwrap();

            let prepared = manager
                .prepare_resume(
                    workflow_id,
                    ResumeContext {
                        user_provided_slots: Vec::new(),
                        updated_confidence: Some(0.8),
                        handled_question: Some("q1".to_owned()),
                    },
                )
                .await
                .unwrap();
            assert_eq!(
                tracker
                    .latest_state(workflow_id)
                    .await
                    .unwrap()
                    .unwrap()
                    .state_id,
                source.state_id,
                "prepare must not append state"
            );
            assert!(manager.find_by_question("q1").await.unwrap().is_some());

            manager.commit_prepared_resume(&prepared).await.unwrap();
            manager.commit_prepared_resume(&prepared).await.unwrap();
            let matching = tracker
                .list_states(workflow_id)
                .await
                .unwrap()
                .into_iter()
                .filter(|state| state.state_id == prepared.resumed_state.state_id)
                .count();
            assert_eq!(matching, 1, "replay must not append a second state");
            assert!(manager.find_by_question("q1").await.unwrap().is_none());
            assert!(manager.find_by_question("q2").await.unwrap().is_some());
        });
    }

    /// C-13 / #22: Two consecutive suspensions for the same workflow_id are both
    /// stored (keyed by distinct checkpoint_ids), but `remove()` is idempotent —
    /// it removes ALL entries for the workflow in one call and merges their
    /// pending_questions, so no duplicate can linger and removal is
    /// deterministic (previously it removed only the first match).
    #[test]
    fn in_memory_queue_remove_is_idempotent_over_duplicate_suspensions() {
        let rt = Runtime::new().unwrap();
        rt.block_on(async {
            let repo = InMemoryQueueRepository::default();
            let workflow_id = "wf-dup-test";

            let wf1 = PausedWorkflow {
                workflow_id: workflow_id.to_string(),
                paused_at: chrono::Utc::now(),
                reason: PauseReason::WaitingOnUser("first pause".to_string()),
                checkpoint_id: "chk-1".to_string(),
                pending_questions: vec!["q1".to_string()],
            };
            let wf2 = PausedWorkflow {
                workflow_id: workflow_id.to_string(),
                paused_at: chrono::Utc::now(),
                reason: PauseReason::WaitingOnUser("second pause".to_string()),
                checkpoint_id: "chk-2".to_string(),
                pending_questions: vec!["q2".to_string()],
            };

            repo.save(wf1).await.unwrap();
            repo.save(wf2).await.unwrap();

            // Both suspensions must be present until removal.
            let listed = repo.list().await.unwrap();
            assert_eq!(listed.len(), 2, "both suspensions should be stored");

            // remove() removes ALL matching entries in one call and merges their
            // pending questions.
            let removed = repo
                .remove(workflow_id)
                .await
                .unwrap()
                .expect("remove should return the merged matching entry");
            assert!(
                removed.pending_questions.contains(&"q1".to_string())
                    && removed.pending_questions.contains(&"q2".to_string()),
                "merged entry should carry both questions"
            );

            let listed_after = repo.list().await.unwrap();
            assert_eq!(
                listed_after.len(),
                0,
                "no suspension should remain after idempotent remove"
            );

            // A second remove for the same workflow is a benign no-op.
            assert!(
                repo.remove(workflow_id).await.unwrap().is_none(),
                "second remove should return None"
            );
        });
    }
}

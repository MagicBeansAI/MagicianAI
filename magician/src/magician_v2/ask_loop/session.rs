use std::{cmp::Ordering, collections::VecDeque, sync::Arc};

use crate::magician_v2::ask_loop::batch_tracker::BatchMetadata;
use crate::magician_v2::ask_loop::budget::Channel;
use crate::magician_v2::ask_loop::clarifier::{BlockerType, ClarifierQuestion};
use crate::magician_v2::slot_graph::SlotRecord;
use crate::magician_v2::state_tracker::StageContext;
use crate::magician_v2::storage::{V2ConversationStore, V2StorageError};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// High-level lifecycle for a clarification session.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum ClarificationSessionState {
    #[default]
    CollectingAnswers,
    ReadyToPlan,
    Planning,
}

impl ClarificationSessionState {
    pub fn as_str(&self) -> &'static str {
        match self {
            ClarificationSessionState::CollectingAnswers => "collecting_answers",
            ClarificationSessionState::ReadyToPlan => "ready_to_plan",
            ClarificationSessionState::Planning => "planning",
        }
    }
}

/// Lightweight representation of a clarification question tracked within a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionQuestion {
    pub id: String,
    pub source_slot_id: Option<String>,
    pub blocker_type: BlockerType,
    pub stage: StageContext,
    pub question_text: String,
    pub context_snippets: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<super::clarifier::QuestionOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_confidence: Option<f32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_slots: Vec<String>,
    pub status: SessionQuestionStatus,
    pub created_at: DateTime<Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<DateTime<Utc>>,
}

impl From<&ClarifierQuestion> for SessionQuestion {
    fn from(question: &ClarifierQuestion) -> Self {
        Self {
            id: question.id.clone(),
            source_slot_id: question.source_slot_id.clone(),
            blocker_type: question.blocker_type,
            stage: question.stage,
            question_text: question.question_text.clone(),
            context_snippets: question.context_snippets.clone(),
            options: question.options.clone(),
            slot_confidence: question.slot_confidence,
            related_slots: question.related_slots.clone(),
            status: SessionQuestionStatus::Queued,
            created_at: question.created_at,
            answered_at: None,
        }
    }
}

impl From<&SessionQuestion> for ClarifierQuestion {
    fn from(question: &SessionQuestion) -> Self {
        ClarifierQuestion {
            id: question.id.clone(),
            blocker_type: question.blocker_type,
            stage: question.stage,
            source_slot_id: question.source_slot_id.clone(),
            question_text: question.question_text.clone(),
            context_snippets: question.context_snippets.clone(),
            urgency: 0.5,
            channel: Channel::InApp,
            created_at: question.created_at,
            options: question.options.clone(),
            batch_id: None,
            batch_total: None,
            slot_confidence: question.slot_confidence,
            related_slots: question.related_slots.clone(),
        }
    }
}

/// Lifecycle for an individual session question.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum SessionQuestionStatus {
    #[default]
    Queued,
    WaitingOnUser,
    Answered,
    Cancelled,
    /// Question has been handed off to the agentic executor.
    /// The agentic loop is now responsible for asking this question during execution.
    HandedOff,
}

/// Snapshot of batch progress for the currently active batch, if any.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBatchProgress {
    pub batch_id: String,
    pub total: usize,
    pub answered: usize,
}

/// Minimal metadata snapshot for a tracked batch.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionBatchMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iteration: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avg_priority: Option<f64>,
}

impl From<BatchMetadata> for SessionBatchMetadata {
    fn from(metadata: BatchMetadata) -> Self {
        SessionBatchMetadata {
            source: Some(metadata.source),
            iteration: metadata.iteration,
            avg_priority: metadata.avg_priority,
        }
    }
}

/// Metadata for the batch currently managed by the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionBatch {
    pub batch_id: String,
    pub question_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SessionBatchMetadata>,
    pub progress: SessionBatchProgress,
}

/// Record describing a slot update gathered during the session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionSlotUpdate {
    pub slot: SlotRecord,
    pub question_id: String,
    pub received_at: DateTime<Utc>,
}

/// Aggregate clarification session state for a workflow.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClarificationSession {
    pub workflow_id: String,
    pub state: ClarificationSessionState,
    pub pending_questions: VecDeque<SessionQuestion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_batch: Option<SessionBatch>,
    pub slot_updates: Vec<SessionSlotUpdate>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_question_asked_at: Option<DateTime<Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_updated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub metadata: serde_json::Value,
    #[serde(with = "chrono::serde::ts_milliseconds", default = "now_timestamp")]
    pub created_at: DateTime<Utc>,
    #[serde(default)]
    pub round_count: usize,
    #[serde(default)]
    pub total_questions_asked: usize,
    #[serde(default = "default_round_limit")]
    pub max_rounds: usize,
}

const MAX_TOTAL_CLARIFICATION_QUESTIONS: usize = 50;
const DEFAULT_MAX_CLARIFICATION_ROUNDS: usize = 10;
const CLARIFICATION_TIMEOUT_SECS: i64 = 600;

fn now_timestamp() -> DateTime<Utc> {
    Utc::now()
}

fn default_round_limit() -> usize {
    DEFAULT_MAX_CLARIFICATION_ROUNDS
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuardrailBreach {
    TimedOut,
    QuestionLimit,
    RoundLimit,
}

impl ClarificationSession {
    pub fn new(workflow_id: impl Into<String>) -> Self {
        Self {
            workflow_id: workflow_id.into(),
            state: ClarificationSessionState::CollectingAnswers,
            pending_questions: VecDeque::new(),
            active_batch: None,
            slot_updates: Vec::new(),
            last_question_asked_at: None,
            last_updated_at: Some(Utc::now()),
            metadata: serde_json::Value::Null,
            created_at: Utc::now(),
            round_count: 0,
            total_questions_asked: 0,
            max_rounds: DEFAULT_MAX_CLARIFICATION_ROUNDS,
        }
    }

    pub fn set_round_limit(&mut self, limit: usize) {
        self.max_rounds = limit.max(1);
    }

    pub fn round_limit(&self) -> usize {
        self.max_rounds
    }

    /// Returns the maximum total number of questions allowed across all rounds.
    pub fn question_limit(&self) -> usize {
        MAX_TOTAL_CLARIFICATION_QUESTIONS
    }

    /// Register a clarification question with the session.
    pub fn enqueue_question(&mut self, question: ClarifierQuestion) {
        self.pending_questions
            .push_back(SessionQuestion::from(&question));
        self.last_question_asked_at = Some(question.created_at);
        self.last_updated_at = Some(Utc::now());
        self.total_questions_asked = self.total_questions_asked.saturating_add(1);
        self.reprioritize_questions();
    }

    /// Configure metadata for the active batch tied to this session.
    pub fn set_active_batch(
        &mut self,
        batch_id: String,
        question_ids: Vec<String>,
        metadata: Option<SessionBatchMetadata>,
    ) {
        let total = question_ids.len();
        self.active_batch = Some(SessionBatch {
            batch_id: batch_id.clone(),
            question_ids,
            metadata,
            progress: SessionBatchProgress {
                batch_id,
                total,
                answered: 0,
            },
        });
        self.last_updated_at = Some(Utc::now());
    }

    /// Promote the next queued question (if any) to WaitingOnUser.
    pub fn promote_next_question(&mut self) -> Option<String> {
        if let Some(question) = self
            .pending_questions
            .iter_mut()
            .find(|q| q.status == SessionQuestionStatus::Queued)
        {
            question.status = SessionQuestionStatus::WaitingOnUser;
            question.answered_at = None;
            let now = Utc::now();
            self.last_question_asked_at = Some(now);
            self.last_updated_at = Some(now);
            return Some(question.id.clone());
        }
        None
    }

    /// Update an existing question status.
    pub fn update_question_status(&mut self, question_id: &str, status: SessionQuestionStatus) {
        if let Some(question) = self
            .pending_questions
            .iter_mut()
            .find(|q| q.id == question_id)
        {
            question.status = status;
            if status == SessionQuestionStatus::Answered {
                question.answered_at = Some(Utc::now());
            }
            self.last_updated_at = Some(Utc::now());
            self.reprioritize_questions();
        }
    }

    /// Mark questions as handed off to the agentic executor.
    ///
    /// When a step starts execution, its unresolved inputs are converted to
    /// pending inputs for the agentic loop. The corresponding clarification
    /// questions should be marked as "handed off" - they're no longer waiting
    /// for the planning-phase clarification system but are now the responsibility
    /// of the agentic executor.
    ///
    /// Returns the number of questions marked as handed off.
    pub fn mark_questions_handed_off(&mut self, question_ids: &[String]) -> usize {
        let mut count = 0;
        for question_id in question_ids {
            if let Some(question) = self
                .pending_questions
                .iter_mut()
                .find(|q| &q.id == question_id)
            {
                // Only hand off questions that are still pending (Queued or WaitingOnUser)
                if matches!(
                    question.status,
                    SessionQuestionStatus::Queued | SessionQuestionStatus::WaitingOnUser
                ) {
                    question.status = SessionQuestionStatus::HandedOff;
                    count += 1;
                }
            }
        }
        if count > 0 {
            self.last_updated_at = Some(Utc::now());
        }
        count
    }

    /// Push a slot update collected from the ask loop.
    pub fn record_slot_update(&mut self, question_id: &str, slot: SlotRecord) {
        self.slot_updates.push(SessionSlotUpdate {
            slot,
            question_id: question_id.to_string(),
            received_at: Utc::now(),
        });
        self.last_updated_at = Some(Utc::now());
    }

    /// Update the active batch progress with current answered count.
    /// This keeps the session's batch metadata in sync with the batch tracker.
    pub fn update_batch_progress(&mut self, batch_id: &str, answered: usize, total: usize) {
        if let Some(active_batch) = &mut self.active_batch {
            if active_batch.batch_id == batch_id {
                active_batch.progress.answered = answered;
                active_batch.progress.total = total;
                self.last_updated_at = Some(Utc::now());
            }
        }
    }

    /// Helper to mark the session as ready for planning.
    pub fn mark_ready_to_plan(&mut self) {
        self.state = ClarificationSessionState::ReadyToPlan;
        self.last_updated_at = Some(Utc::now());
    }

    /// Helper to mark the session as actively planning.
    pub fn mark_planning(&mut self) {
        self.state = ClarificationSessionState::Planning;
        self.last_updated_at = Some(Utc::now());
    }

    /// Reset back to collecting answers (e.g., after replanning reopens the loop).
    pub fn mark_collecting(&mut self) {
        if self.state != ClarificationSessionState::CollectingAnswers || self.round_count == 0 {
            self.round_count = self.round_count.saturating_add(1);
        }
        self.state = ClarificationSessionState::CollectingAnswers;
        self.pending_questions.retain(|q| {
            matches!(
                q.status,
                SessionQuestionStatus::Queued | SessionQuestionStatus::WaitingOnUser
            )
        });
        // Always clear slot updates when resetting to collecting state
        self.slot_updates.clear();
        self.last_updated_at = Some(Utc::now());
        self.reprioritize_questions();
    }

    /// Determine whether a new clarification round should be blocked.
    ///
    /// We block once the configured round limit has been reached **and** the
    /// session is no longer actively collecting answers (i.e. we're between
    /// rounds and would otherwise start a fresh batch).
    pub fn should_block_new_round(&self) -> bool {
        self.round_count >= self.max_rounds
            && self.state != ClarificationSessionState::CollectingAnswers
    }

    pub fn guardrail_status(&self, now: DateTime<Utc>) -> Option<GuardrailBreach> {
        if self.total_questions_asked > MAX_TOTAL_CLARIFICATION_QUESTIONS {
            return Some(GuardrailBreach::QuestionLimit);
        }

        if self.should_block_new_round() {
            return Some(GuardrailBreach::RoundLimit);
        }

        // #23: time out stale outstanding questions. Previously this only
        // considered `WaitingOnUser` questions, so a phantom/never-surfaced
        // `Queued` question that pins the plan in `CollectingAnswers` forever
        // never tripped the timeout even once the sweep started calling this.
        // Both statuses represent an unanswered question the plan is blocked on.
        if self.pending_questions.iter().any(|q| {
            matches!(
                q.status,
                SessionQuestionStatus::WaitingOnUser | SessionQuestionStatus::Queued
            )
        }) {
            if let Some(last) = self.last_question_asked_at {
                if now.signed_duration_since(last).num_seconds() > CLARIFICATION_TIMEOUT_SECS {
                    return Some(GuardrailBreach::TimedOut);
                }
            }
        }

        None
    }

    pub fn outstanding_question_ids(&self) -> Vec<String> {
        self.pending_questions
            .iter()
            .filter(|q| {
                matches!(
                    q.status,
                    SessionQuestionStatus::Queued | SessionQuestionStatus::WaitingOnUser
                )
            })
            .map(|q| q.id.clone())
            .collect()
    }

    fn reprioritize_questions(&mut self) {
        let mut queued: Vec<_> = self
            .pending_questions
            .iter()
            .filter(|q| q.status == SessionQuestionStatus::Queued)
            .cloned()
            .collect();
        queued.sort_by(|a, b| {
            slot_priority(a)
                .partial_cmp(&slot_priority(b))
                .unwrap_or(Ordering::Equal)
                .then_with(|| a.created_at.cmp(&b.created_at))
        });
        let mut iter = queued.into_iter();
        for question in self.pending_questions.iter_mut() {
            if question.status == SessionQuestionStatus::Queued {
                if let Some(next) = iter.next() {
                    *question = next;
                }
            }
        }
    }
}

fn slot_priority(question: &SessionQuestion) -> f32 {
    question.slot_confidence.unwrap_or(1.0)
}

/// Persistence abstraction for clarification sessions.
#[async_trait]
pub trait ClarificationSessionStore: Send + Sync {
    async fn load_session(
        &self,
        workflow_id: &str,
    ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError>;

    async fn save_session(
        &self,
        session: ClarificationSession,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn delete_session(&self, workflow_id: &str)
        -> Result<(), ClarificationSessionStoreError>;
}

#[derive(Debug, Error)]
pub enum ClarificationSessionStoreError {
    #[error("storage error: {0}")]
    Storage(#[from] V2StorageError),
    #[error("serialization error: {0}")]
    Serialization(String),
    #[error("other error: {0}")]
    Other(String),
}

#[async_trait]
impl ClarificationSessionStore for Arc<dyn V2ConversationStore> {
    async fn load_session(
        &self,
        workflow_id: &str,
    ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError> {
        self.load_clarification_session(workflow_id)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }

    async fn save_session(
        &self,
        session: ClarificationSession,
    ) -> Result<(), ClarificationSessionStoreError> {
        let workflow_id = session.workflow_id.clone();
        self.store_clarification_session(&workflow_id, session)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }

    async fn delete_session(
        &self,
        workflow_id: &str,
    ) -> Result<(), ClarificationSessionStoreError> {
        self.delete_clarification_session(workflow_id)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord};
    use chrono::Duration;
    use serde_json::json;

    #[test]
    fn new_session_has_defaults() {
        let session = ClarificationSession::new("wf-123");
        assert_eq!(session.workflow_id, "wf-123");
        assert_eq!(session.state, ClarificationSessionState::CollectingAnswers);
        assert!(session.pending_questions.is_empty());
        assert!(session.active_batch.is_none());
        assert!(session.slot_updates.is_empty());
        assert_eq!(session.round_count, 0);
        assert_eq!(session.total_questions_asked, 0);
    }

    #[test]
    fn enqueue_question_tracks_metadata() {
        let question = ClarifierQuestion {
            id: "q1".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some("wf::slot".into()),
            question_text: "Need more info?".into(),
            context_snippets: vec!["snippet".into()],
            urgency: 0.5,
            channel: crate::magician_v2::ask_loop::budget::Channel::InApp,
            created_at: Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        let mut session = ClarificationSession::new("wf-123");
        session.metadata = json!({"iteration": 1});
        session.enqueue_question(question);
        assert_eq!(session.pending_questions.len(), 1);
        let stored = session.pending_questions.front().unwrap();
        assert_eq!(stored.id, "q1");
        assert_eq!(stored.source_slot_id.as_deref(), Some("wf::slot"));
        assert_eq!(stored.status, SessionQuestionStatus::Queued);
        assert_eq!(session.total_questions_asked, 1);
    }

    #[test]
    fn promote_next_question_advances_queue() {
        let mut session = ClarificationSession::new("wf-queue");
        let now = Utc::now();
        let q1 = ClarifierQuestion {
            id: "q1".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            question_text: "First?".into(),
            context_snippets: vec![],
            urgency: 0.4,
            channel: Channel::InApp,
            created_at: now,
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        let mut q2 = q1.clone();
        q2.id = "q2".into();
        q2.question_text = "Second?".into();

        session.enqueue_question(q1);
        session.enqueue_question(q2);
        session.update_question_status("q1", SessionQuestionStatus::WaitingOnUser);

        session.update_question_status("q1", SessionQuestionStatus::Answered);
        let promoted = session.promote_next_question();

        assert_eq!(promoted.as_deref(), Some("q2"));
        let second = session
            .pending_questions
            .iter()
            .find(|q| q.id == "q2")
            .unwrap();
        assert_eq!(second.status, SessionQuestionStatus::WaitingOnUser);
    }

    #[test]
    fn guardrail_triggers_on_question_cap() {
        let mut session = ClarificationSession::new("wf-guardrail");
        for idx in 0..MAX_TOTAL_CLARIFICATION_QUESTIONS {
            let question = ClarifierQuestion {
                id: format!("q{}", idx),
                blocker_type: BlockerType::LowConfidenceSlot,
                stage: StageContext::PlanningBootstrap,
                source_slot_id: None,
                question_text: "Need more info?".into(),
                context_snippets: vec![],
                urgency: 0.3,
                channel: Channel::InApp,
                created_at: Utc::now(),
                options: None,
                batch_id: None,
                batch_total: None,
                ..ClarifierQuestion::default()
            };
            session.enqueue_question(question);
        }
        assert!(
            session.guardrail_status(Utc::now()).is_none(),
            "cap is inclusive until exceeded"
        );

        let extra = ClarifierQuestion {
            id: "overflow".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            question_text: "Overflow?".into(),
            context_snippets: vec![],
            urgency: 0.3,
            channel: Channel::InApp,
            created_at: Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        session.enqueue_question(extra.clone());
        assert_eq!(
            session.guardrail_status(Utc::now()),
            Some(GuardrailBreach::QuestionLimit)
        );
    }

    #[test]
    fn round_limit_allows_finishing_current_round() {
        let mut session = ClarificationSession::new("wf-round");
        session.max_rounds = 1;
        session.round_count = 1;
        session.state = ClarificationSessionState::CollectingAnswers;
        let question = ClarifierQuestion {
            id: "q-final".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            question_text: "still pending".into(),
            context_snippets: vec![],
            urgency: 0.2,
            channel: Channel::InApp,
            created_at: Utc::now(),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        session.enqueue_question(question);
        assert!(session.guardrail_status(Utc::now()).is_none());
    }

    #[test]
    fn round_limit_blocks_new_round_when_not_collecting() {
        let mut session = ClarificationSession::new("wf-round");
        session.max_rounds = 1;
        session.round_count = 1;
        session.state = ClarificationSessionState::ReadyToPlan;
        assert_eq!(
            session.guardrail_status(Utc::now()),
            Some(GuardrailBreach::RoundLimit)
        );
    }

    #[test]
    fn guardrail_triggers_on_timeout() {
        let mut session = ClarificationSession::new("wf-timeout");
        let q = ClarifierQuestion {
            id: "q-time".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: None,
            question_text: "Timeout?".into(),
            context_snippets: vec![],
            urgency: 0.2,
            channel: Channel::InApp,
            created_at: Utc::now() - Duration::seconds(CLARIFICATION_TIMEOUT_SECS + 5),
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        session.enqueue_question(q.clone());
        session.update_question_status("q-time", SessionQuestionStatus::WaitingOnUser);
        session.last_question_asked_at =
            Some(Utc::now() - Duration::seconds(CLARIFICATION_TIMEOUT_SECS + 5));

        assert_eq!(
            session.guardrail_status(Utc::now()),
            Some(GuardrailBreach::TimedOut)
        );
    }

    #[test]
    fn record_slot_update_appends_entry() {
        let mut session = ClarificationSession::new("wf-123");
        let slot = SlotRecord {
            id: "wf::slot".into(),
            slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
            value: json!("value"),
            confidence: 0.8,
            provenance: Vec::new(),
            evidence_links: Vec::new(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };
        session.record_slot_update("q1", slot.clone());
        assert_eq!(session.slot_updates.len(), 1);
        assert_eq!(session.slot_updates[0].slot.id, slot.id);
        assert_eq!(session.slot_updates[0].question_id, "q1");
    }

    #[test]
    fn mark_collecting_prunes_answered_questions_and_clears_slots() {
        let now = Utc::now();
        let answered = ClarifierQuestion {
            id: "q-answered".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some("wf::slot_a".into()),
            question_text: "Answered?".into(),
            context_snippets: vec![],
            urgency: 0.5,
            channel: crate::magician_v2::ask_loop::budget::Channel::InApp,
            created_at: now,
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };
        let waiting = ClarifierQuestion {
            id: "q-waiting".into(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some("wf::slot_b".into()),
            question_text: "Still open?".into(),
            context_snippets: vec![],
            urgency: 0.5,
            channel: crate::magician_v2::ask_loop::budget::Channel::InApp,
            created_at: now,
            options: None,
            batch_id: None,
            batch_total: None,
            ..ClarifierQuestion::default()
        };

        let mut session = ClarificationSession::new("wf-456");
        session.enqueue_question(answered);
        session.enqueue_question(waiting);
        session.update_question_status("q-answered", SessionQuestionStatus::Answered);
        session.record_slot_update(
            "q-answered",
            SlotRecord {
                id: "wf::slot_a".into(),
                slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
                value: json!("foo"),
                confidence: 0.9,
                provenance: vec![ProvenanceRecord {
                    source: ProvenanceSource::UserReply,
                    timestamp: now,
                }],
                evidence_links: Vec::new(),
                created_at: now,
                updated_at: now,
            },
        );

        session.mark_collecting();

        assert_eq!(
            session.pending_questions.len(),
            1,
            "only unanswered questions should remain"
        );
        assert_eq!(
            session.pending_questions.front().unwrap().id,
            "q-waiting",
            "waiting question should be preserved"
        );
        assert!(
            session.slot_updates.is_empty(),
            "slot updates should be cleared when resetting to collecting"
        );
    }
}

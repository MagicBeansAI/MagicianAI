use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use tracing::{debug, info, warn};

use super::session::{ClarificationSession, ClarificationSessionState, SessionQuestionStatus};
use super::ClarifierQuestion;
use crate::magician_v2::{
    ask_loop::ClarificationSessionStoreError, realtime_events::RuntimeTransportBroadcaster,
    slot_graph::SlotRecord, storage::V2ConversationStore,
};

/// High-level behaviour for clarification session management.
#[async_trait]
pub trait SessionManager: Send + Sync {
    async fn load(
        &self,
        workflow_id: &str,
    ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError>;

    async fn save(
        &self,
        session: ClarificationSession,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn delete(&self, workflow_id: &str) -> Result<(), ClarificationSessionStoreError>;

    async fn append_question(
        &self,
        workflow_id: &str,
        question: ClarifierQuestion,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn mark_question_answered(
        &self,
        workflow_id: &str,
        question_id: &str,
        slots: Vec<SlotRecord>,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn cancel_questions(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn mark_state(
        &self,
        workflow_id: &str,
        state: ClarificationSessionState,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn update_batch_progress(
        &self,
        workflow_id: &str,
        batch_id: &str,
        answered: usize,
        total: usize,
    ) -> Result<(), ClarificationSessionStoreError>;

    async fn ensure_round_limit(
        &self,
        workflow_id: &str,
        limit: usize,
    ) -> Result<(), ClarificationSessionStoreError>;

    /// Mark questions as handed off to the agentic executor.
    ///
    /// When execution starts for a step, its planning-phase clarifications
    /// are converted to PendingInput format for the agentic loop. The
    /// corresponding questions should be marked as "handed off" to indicate
    /// they're no longer waiting for the clarification system.
    async fn mark_questions_handed_off(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<usize, ClarificationSessionStoreError>;
}

/// Default session manager backed by the conversation store.
pub struct ClarificationSessionManager {
    store: Arc<dyn V2ConversationStore>,
    broadcaster: Arc<RuntimeTransportBroadcaster>,
}

impl ClarificationSessionManager {
    pub fn new(
        store: Arc<dyn V2ConversationStore>,
        broadcaster: Arc<RuntimeTransportBroadcaster>,
    ) -> Self {
        Self { store, broadcaster }
    }

    async fn load_or_new(
        &self,
        workflow_id: &str,
    ) -> Result<ClarificationSession, ClarificationSessionStoreError> {
        if let Some(session) = self
            .store
            .load_clarification_session(workflow_id)
            .await
            .map_err(ClarificationSessionStoreError::from)?
        {
            return Ok(session);
        }

        let mut session = ClarificationSession::new(workflow_id);
        session.last_updated_at = Some(Utc::now());
        Ok(session)
    }
}

#[async_trait]
impl SessionManager for ClarificationSessionManager {
    async fn load(
        &self,
        workflow_id: &str,
    ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError> {
        self.store
            .load_clarification_session(workflow_id)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }

    async fn save(
        &self,
        session: ClarificationSession,
    ) -> Result<(), ClarificationSessionStoreError> {
        let workflow_id = session.workflow_id.clone();
        debug!(
            workflow = %session.workflow_id,
            pending = session.pending_questions.len(),
            "saving clarification session"
        );
        self.store
            .store_clarification_session(&workflow_id, session)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }

    async fn delete(&self, workflow_id: &str) -> Result<(), ClarificationSessionStoreError> {
        self.store
            .delete_clarification_session(workflow_id)
            .await
            .map_err(ClarificationSessionStoreError::from)
    }

    async fn append_question(
        &self,
        workflow_id: &str,
        question: ClarifierQuestion,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut session = self.load_or_new(workflow_id).await?;
        if session.should_block_new_round() {
            warn!(
                workflow = %workflow_id,
                round_count = session.round_count,
                max_rounds = session.max_rounds,
                "blocking new clarification round because round limit reached"
            );
            return Err(ClarificationSessionStoreError::Other(format!(
                "round limit reached ({}/{}). cannot enqueue more clarification questions",
                session.round_count, session.max_rounds
            )));
        }
        let question_id = question.id.clone();
        if let Some(existing) = session
            .pending_questions
            .iter_mut()
            .find(|pending| pending.id == question_id)
        {
            info!(
                workflow = %session.workflow_id,
                question_id = %existing.id,
                status = ?existing.status,
                "session manager updating existing pending question metadata"
            );
            let was_answered = existing.status == SessionQuestionStatus::Answered;
            existing.blocker_type = question.blocker_type;
            existing.stage = question.stage;
            existing.question_text = question.question_text.clone();
            existing.context_snippets = question.context_snippets.clone();
            existing.source_slot_id = question.source_slot_id.clone();
            existing.created_at = question.created_at;
            if was_answered {
                debug!(
                    workflow = %session.workflow_id,
                    question_id = %existing.id,
                    "reopening previously answered question"
                );
            }
            existing.status = SessionQuestionStatus::WaitingOnUser;
            existing.answered_at = None;
        } else {
            let next_position = session.pending_questions.len() + 1;
            info!(
                workflow = %session.workflow_id,
                question_id = %question_id,
                pending_total = next_position,
                "session manager enqueueing new clarification question"
            );
            session.enqueue_question(question.clone());
            session.update_question_status(&question_id, SessionQuestionStatus::WaitingOnUser);
        }

        session.last_question_asked_at = Some(question.created_at);
        session.last_updated_at = Some(Utc::now());
        self.save(session).await
    }

    async fn mark_question_answered(
        &self,
        workflow_id: &str,
        question_id: &str,
        slots: Vec<SlotRecord>,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut session = self.load_or_new(workflow_id).await?;
        if session.pending_questions.iter().any(|question| {
            question.id == question_id && question.status == SessionQuestionStatus::Answered
        }) {
            // Prepared resume recovery may replay after the session write but
            // before the receipt phase promotion. Do not append duplicate slot
            // updates or promote the queue twice.
            return Ok(());
        }
        if session
            .pending_questions
            .iter()
            .all(|q| q.id != question_id)
        {
            warn!(
                workflow = %workflow_id,
                question_id = %question_id,
                "session manager could not find question to mark answered"
            );
            let slot_ids: Vec<String> = slots.iter().map(|slot| slot.id.clone()).collect();
            self.broadcaster.observability_alert(
                workflow_id,
                "clarification_question_not_found",
                json!({
                    "question_id": question_id,
                    "slot_count": slots.len(),
                    "slot_ids": slot_ids,
                    "pending_count": session.pending_questions.len()
                }),
            );
        }

        session.update_question_status(question_id, SessionQuestionStatus::Answered);
        for slot in slots {
            session.record_slot_update(question_id, slot);
        }
        if let Some(next_id) = session.promote_next_question() {
            debug!(
                workflow = %workflow_id,
                question_id = %next_id,
                "promoted next queued clarification to WaitingOnUser"
            );
        }
        session.last_updated_at = Some(Utc::now());
        self.save(session).await
    }

    async fn cancel_questions(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<(), ClarificationSessionStoreError> {
        if question_ids.is_empty() {
            return Ok(());
        }

        let mut session = self.load_or_new(workflow_id).await?;
        for question_id in question_ids {
            session.update_question_status(question_id, SessionQuestionStatus::Cancelled);
        }
        session.last_updated_at = Some(Utc::now());
        self.save(session).await
    }

    async fn mark_state(
        &self,
        workflow_id: &str,
        state: ClarificationSessionState,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut session = self.load_or_new(workflow_id).await?;
        match state {
            ClarificationSessionState::CollectingAnswers => session.mark_collecting(),
            ClarificationSessionState::ReadyToPlan => session.mark_ready_to_plan(),
            ClarificationSessionState::Planning => session.mark_planning(),
        }
        self.save(session).await
    }

    async fn update_batch_progress(
        &self,
        workflow_id: &str,
        batch_id: &str,
        answered: usize,
        total: usize,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut session = self.load_or_new(workflow_id).await?;
        session.update_batch_progress(batch_id, answered, total);
        self.save(session).await
    }

    async fn ensure_round_limit(
        &self,
        workflow_id: &str,
        limit: usize,
    ) -> Result<(), ClarificationSessionStoreError> {
        let mut session = self.load_or_new(workflow_id).await?;
        if session.round_limit() != limit {
            info!(
                workflow = %workflow_id,
                previous_limit = session.round_limit(),
                new_limit = limit,
                "updating clarification round limit"
            );
            session.set_round_limit(limit);
            self.save(session).await
        } else {
            Ok(())
        }
    }

    async fn mark_questions_handed_off(
        &self,
        workflow_id: &str,
        question_ids: &[String],
    ) -> Result<usize, ClarificationSessionStoreError> {
        if question_ids.is_empty() {
            return Ok(0);
        }

        let mut session = self.load_or_new(workflow_id).await?;
        let count = session.mark_questions_handed_off(question_ids);

        if count > 0 {
            info!(
                workflow = %workflow_id,
                count = count,
                question_ids = ?question_ids,
                "Marked {} questions as handed off to agentic executor",
                count
            );
            self.save(session).await?;
        }

        Ok(count)
    }
}

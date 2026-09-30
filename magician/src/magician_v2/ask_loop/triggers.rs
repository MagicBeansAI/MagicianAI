use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokio::{pin, sync::Mutex};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, info_span, warn};

use super::{
    answer_interpreter::AnswerType,
    batch_tracker::QuestionBatchTracker,
    budget::Channel,
    clarifier::{BlockerType, ClarifierLibrary, ClarifierQuestion, ClarifierResponsePayload},
    history::ClarificationHistory,
    ledger::{BudgetLedger, BudgetLedgerError},
    metrics::{ClarificationMetrics, SessionSnapshotStats},
    pause::{
        PauseError, PauseReason, PauseResumeManager, PausedWorkflow, PreparedPauseResume,
        ResumeContext, ResumeMode,
    },
    plan_confidence::{
        ConfidenceTrigger, PlanConfidenceSnapshot, PlanConfidenceTracker, SlotConfidenceDelta,
    },
    session::{
        ClarificationSession, ClarificationSessionState, GuardrailBreach, SessionQuestion,
        SessionQuestionStatus,
    },
    session_manager::SessionManager,
};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext, OperationLlmTelemetryScope,
    },
    execution::PromptIdentityContext,
    realtime_events::{ConfidenceSlotDelta, RuntimeTransportBroadcaster},
    slot_graph::{rewriter::QuestionRewriter, SlotRecord},
    state_tracker::{StageContext, StateBundle},
    storage::V2StorageError,
};

/// Notification emitted when a workflow successfully resumes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeNotification {
    pub workflow_id: String,
    pub question_id: Option<String>,
    pub stage: StageContext,
    pub blocker_type: Option<BlockerType>,
    pub slots: Vec<SlotRecord>,
    pub resumed_state: StateBundle,
    /// The resume mode determined for this clarification
    pub resume_mode: ResumeMode,
    /// Queue metrics at the time the workflow resumed
    pub answered_count: usize,
    pub pending_count: usize,
}

/// Exact AskLoop mutations sealed before an Artifact planning continuation
/// consumes an answer, pause entry, or guardrail session state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PreparedResumeMutation {
    pub pause_resume: PreparedPauseResume,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answered_question_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub answer_slots: Vec<SlotRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cancelled_question_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_state_after: Option<ClarificationSessionState>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumePreparation {
    pub schema_version: u8,
    pub notification: ResumeNotification,
    pub mutation: PreparedResumeMutation,
}

#[derive(Debug, Clone, Copy, Default)]
struct SessionQueueSummary {
    outstanding: usize,
    answered: usize,
}

#[derive(Debug, Clone)]
struct SessionTelemetryScope {
    principal: String,
    workspace: String,
    task_id: Option<String>,
}

#[async_trait]
pub trait ResumeListener: Send + Sync {
    /// Persist an exact restart authority before AskLoop applies the mutation.
    /// `None` preserves the legacy non-Artifact path.
    async fn prepare_resume(
        &self,
        _preparation: ResumePreparation,
    ) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Atomically claim a prepared authority, apply the idempotent AskLoop
    /// mutation under that renewable lease, and promote the exact receipt.
    async fn commit_prepared_resume(
        &self,
        _preparation: &ResumePreparation,
        _recovery_id: &str,
    ) -> Result<(), String> {
        Ok(())
    }

    async fn on_resume(&self, notification: ResumeNotification) -> Result<(), String>;

    async fn on_resume_failed(&self, _workflow_id: &str, _error: &str) {}
}

/// Service orchestrating resume attempts when clarifier responses arrive.
pub struct ResumeTriggerService {
    pause_manager: Arc<PauseResumeManager>,
    clarifier: Arc<ClarifierLibrary>,
    ledger: Option<Arc<BudgetLedger>>,
    event_emitter: Arc<RwLock<Option<Arc<RuntimeTransportBroadcaster>>>>,
    history: Arc<ClarificationHistory>,
    resume_listener: Arc<RwLock<Option<Arc<dyn ResumeListener>>>>,
    session_manager: Arc<RwLock<Option<Arc<dyn SessionManager>>>>,
    batch_tracker: Arc<RwLock<Option<Arc<QuestionBatchTracker>>>>,
    /// Phase 6: Plan confidence tracker for recording confidence evolution
    confidence_tracker: Arc<RwLock<Option<Arc<PlanConfidenceTracker>>>>,
    /// Query rewriter for generating enriched queries after batch completion (Issue #3 fix)
    query_rewriter: Arc<RwLock<Option<Arc<QuestionRewriter>>>>,
    /// Metrics collector for dashboards
    metrics_collector: Arc<RwLock<Option<Arc<ClarificationMetrics>>>>,
    /// Per-workflow rewrite locks to serialize enriched-query updates
    rewrite_locks: DashMap<String, Arc<Mutex<()>>>,
    /// Per-workflow cancellation tokens to guard against overlapping rewrites
    rewrite_tokens: DashMap<String, CancellationToken>,
    /// Serialize prepare -> mutation -> commit inside one process. The Artifact
    /// task write fence provides the corresponding cross-process exclusion.
    resume_transaction_locks: DashMap<String, Arc<Mutex<()>>>,
}

impl ResumeTriggerService {
    pub fn new(
        pause_manager: Arc<PauseResumeManager>,
        clarifier: Arc<ClarifierLibrary>,
        ledger: Option<Arc<BudgetLedger>>,
        event_emitter: Option<Arc<RuntimeTransportBroadcaster>>,
        history: Arc<ClarificationHistory>,
    ) -> Self {
        Self {
            pause_manager,
            clarifier,
            ledger,
            event_emitter: Arc::new(RwLock::new(event_emitter)),
            history,
            resume_listener: Arc::new(RwLock::new(None)),
            session_manager: Arc::new(RwLock::new(None)),
            batch_tracker: Arc::new(RwLock::new(None)),
            confidence_tracker: Arc::new(RwLock::new(None)),
            query_rewriter: Arc::new(RwLock::new(None)),
            rewrite_locks: DashMap::new(),
            rewrite_tokens: DashMap::new(),
            resume_transaction_locks: DashMap::new(),
            metrics_collector: Arc::new(RwLock::new(None)),
        }
    }

    /// Wire the clarification session manager used to track queue state.
    pub fn set_session_manager(&self, session_manager: Arc<dyn SessionManager>) {
        if let Ok(mut guard) = self.session_manager.write() {
            *guard = Some(session_manager);
        }
    }

    /// Set the batch tracker for smart resume mode determination.
    pub fn set_batch_tracker(&self, batch_tracker: Arc<QuestionBatchTracker>) {
        if let Ok(mut guard) = self.batch_tracker.write() {
            *guard = Some(batch_tracker);
        }
    }

    /// Phase 6: Set the confidence tracker for recording confidence evolution.
    pub fn set_confidence_tracker(&self, confidence_tracker: Arc<PlanConfidenceTracker>) {
        if let Ok(mut guard) = self.confidence_tracker.write() {
            *guard = Some(confidence_tracker);
        }
    }

    /// Set the query rewriter for batch completion enrichment (Issue #3 fix).
    pub fn set_query_rewriter(&self, query_rewriter: Arc<QuestionRewriter>) {
        if let Ok(mut guard) = self.query_rewriter.write() {
            *guard = Some(query_rewriter);
        }
    }

    fn rewrite_lock(&self, workflow_id: &str) -> Arc<Mutex<()>> {
        self.rewrite_locks
            .entry(workflow_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    fn resume_transaction_lock(&self, workflow_id: &str) -> Arc<Mutex<()>> {
        self.resume_transaction_locks
            .entry(workflow_id.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Idempotently finish the AskLoop half of a sealed continuation. This is
    /// public for startup recovery through AskLoopApi; callers must provide the
    /// integrity-verified preparation loaded from the Artifact task plan.
    pub async fn commit_prepared_resume_mutation(
        &self,
        mutation: &PreparedResumeMutation,
    ) -> Result<StateBundle, TriggerError> {
        let workflow_id = mutation.pause_resume.workflow_id.clone();
        let lock = self.resume_transaction_lock(&workflow_id);
        let _guard = lock.lock().await;
        if let Some(manager) = self
            .session_manager
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
        {
            if let Some(question_id) = mutation.answered_question_id.as_deref() {
                manager
                    .mark_question_answered(
                        &workflow_id,
                        question_id,
                        mutation.answer_slots.clone(),
                    )
                    .await
                    .map_err(|error| TriggerError::InvalidResume(error.to_string()))?;
            }
            if !mutation.cancelled_question_ids.is_empty() {
                manager
                    .cancel_questions(&workflow_id, &mutation.cancelled_question_ids)
                    .await
                    .map_err(|error| TriggerError::InvalidResume(error.to_string()))?;
            }
            if let Some(state) = mutation.session_state_after {
                manager
                    .mark_state(&workflow_id, state)
                    .await
                    .map_err(|error| TriggerError::InvalidResume(error.to_string()))?;
            }
        } else if mutation.answered_question_id.is_some()
            || !mutation.cancelled_question_ids.is_empty()
            || mutation.session_state_after.is_some()
        {
            return Err(TriggerError::InvalidResume(format!(
                "resume_session_manager_unavailable:{workflow_id}"
            )));
        }
        self.pause_manager
            .commit_prepared_resume(&mutation.pause_resume)
            .await
            .map_err(TriggerError::from)
    }

    /// Determine the appropriate resume mode based on batch completion status.
    ///
    /// # Logic
    /// - If question is part of a batch:
    ///   - Batch complete → FullReplan (trigger full replanning)
    ///   - Batch ≥50% complete → PartialReplan (incremental replanning)
    ///   - Batch <50% complete → LightSlotUpdate (just update slots)
    /// - If question is NOT part of a batch → FullReplan (single question, replan immediately)
    fn determine_resume_mode(
        &self,
        workflow_id: &str,
        question_id: &str,
        interpretation: Option<&crate::magician_v2::ask_loop::clarifier::AnswerInterpretation>,
        session_summary: Option<SessionQueueSummary>,
    ) -> ResumeMode {
        // Phase 5: Check if answer interpretation requires immediate replanning
        // Corrections, rejections, clarifications with new requirements should trigger replanning
        if let Some(interp) = interpretation {
            if interp.requires_replan {
                info!(
                    "[DISCOVERY-REPLAN] Answer type {:?} requires replanning, using PartialReplan",
                    interp.answer_type
                );
                return ResumeMode::PartialReplan;
            }
        }

        // Get batch tracker if available
        let batch_tracker = self
            .batch_tracker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone));

        if let Some(batch_tracker) = batch_tracker {
            if let Some(batch_id) = batch_tracker.find_batch_for_question(question_id) {
                // Check if batch is complete
                if batch_tracker.is_batch_complete(&batch_id) {
                    info!(
                        "[BATCH-RESUME] Batch {} complete, using FullReplan",
                        batch_id
                    );
                    return ResumeMode::FullReplan;
                }

                // Check progress for partial replan decision
                // Requires BOTH >= 50% progress AND at least 2 questions answered
                // This ensures: 1/2 (50%) → Light, but 2/4 (50%) → Partial
                if let Some((answered, total)) = batch_tracker.get_progress(&batch_id) {
                    let progress = answered as f64 / total as f64;
                    // Require >= 50% progress AND at least 2 answers
                    if progress >= 0.5 && answered >= 2 {
                        info!(
                            "[BATCH-RESUME] Batch {} at {:.0}% ({}/{}), using PartialReplan",
                            batch_id,
                            progress * 100.0,
                            answered,
                            total
                        );
                        return ResumeMode::PartialReplan;
                    }
                }

                tracing::debug!(
                    "[BATCH-RESUME] Batch {} incomplete (<=50%), using LightSlotUpdate",
                    batch_id
                );
                return ResumeMode::LightSlotUpdate;
            }
        }

        if let Some(summary) = session_summary {
            if summary.outstanding > 0 {
                tracing::info!(
                    "[SESSION-RESUME] Workflow {} still has {} outstanding clarification question(s); using LightSlotUpdate",
                    workflow_id,
                    summary.outstanding
                );
                return ResumeMode::LightSlotUpdate;
            }

            if summary.answered > 0 {
                tracing::info!(
                    "[SESSION-RESUME] Workflow {} queue clear after {} answered question(s); using PartialReplan",
                    workflow_id,
                    summary.answered
                );
                return ResumeMode::PartialReplan;
            }
        }

        tracing::debug!(
            "[SESSION-RESUME] Workflow {} defaulting to FullReplan for question {}",
            workflow_id,
            question_id
        );
        ResumeMode::FullReplan
    }

    /// Predict the mode before mutating the in-memory batch tracker. This is
    /// used solely to seal an exact pre-mutation Artifact continuation.
    fn determine_resume_mode_after_answer(
        &self,
        _workflow_id: &str,
        question_id: &str,
        interpretation: Option<&crate::magician_v2::ask_loop::clarifier::AnswerInterpretation>,
        _session_summary: Option<SessionQueueSummary>,
    ) -> ResumeMode {
        if interpretation.is_some_and(|value| value.requires_replan) {
            return ResumeMode::PartialReplan;
        }
        let batch_tracker = self
            .batch_tracker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone));
        let Some(batch_tracker) = batch_tracker else {
            return ResumeMode::FullReplan;
        };
        let Some(batch_id) = batch_tracker.find_batch_for_question(question_id) else {
            return ResumeMode::FullReplan;
        };
        let Some((answered, total)) = batch_tracker.get_progress(&batch_id) else {
            return ResumeMode::LightSlotUpdate;
        };
        if total == 0 {
            return ResumeMode::FullReplan;
        }
        let answered_after = answered.saturating_add(1).min(total);
        if answered_after == total {
            return ResumeMode::FullReplan;
        }
        if answered_after >= 2 && (answered_after as f64 / total as f64) >= 0.5 {
            return ResumeMode::PartialReplan;
        }
        ResumeMode::LightSlotUpdate
    }

    async fn session_queue_summary(
        &self,
        workflow_id: &str,
    ) -> Result<Option<SessionQueueSummary>, TriggerError> {
        let manager = self
            .session_manager
            .read()
            .map_err(|_| {
                TriggerError::RecoveryUnavailable(
                    "clarification session manager lock is poisoned".to_owned(),
                )
            })?
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| {
                TriggerError::RecoveryUnavailable(
                    "clarification session manager is unavailable".to_owned(),
                )
            })?;

        match manager.load(workflow_id).await {
            Ok(Some(session)) => {
                let mut summary = SessionQueueSummary::default();
                for question in session.pending_questions.iter() {
                    match question.status {
                        SessionQuestionStatus::WaitingOnUser | SessionQuestionStatus::Queued => {
                            summary.outstanding += 1;
                        },
                        SessionQuestionStatus::Answered => summary.answered += 1,
                        SessionQuestionStatus::Cancelled | SessionQuestionStatus::HandedOff => {},
                    }
                }
                Ok(Some(summary))
            },
            Ok(None) => Ok(None),
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-ASK] Failed to load clarification session for {}: {}",
                    workflow_id, err
                );
                Err(TriggerError::RecoveryUnavailable(format!(
                    "clarification session summary read failed for {workflow_id}: {err}"
                )))
            },
        }
    }

    pub fn set_metrics_collector(&self, metrics: Arc<ClarificationMetrics>) {
        if let Ok(mut guard) = self.metrics_collector.write() {
            *guard = Some(metrics);
        }
    }

    async fn record_session_answer(
        &self,
        workflow_id: &str,
        question_id: &str,
        slots: &[SlotRecord],
    ) {
        let manager = self
            .session_manager
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone));

        if let Some(manager) = manager {
            match manager
                .mark_question_answered(workflow_id, question_id, slots.to_vec())
                .await
            {
                Ok(()) => {
                    if let Ok(Some(session)) = manager.load(workflow_id).await {
                        self.emit_session_snapshot(workflow_id, &session);
                    }
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-V2-ASK] Session manager failed to record answer for {} ({}): {}",
                        workflow_id, question_id, err
                    );
                },
            }
        }
    }

    /// Update the event emitter used for broadcasting resume outcomes.
    pub fn set_event_emitter(&self, emitter: Option<Arc<RuntimeTransportBroadcaster>>) {
        if let Ok(mut guard) = self.event_emitter.write() {
            *guard = emitter;
        }
    }

    pub fn emit_session_snapshot(&self, workflow_id: &str, session: &ClarificationSession) {
        let Some(emitter) = self.event_broadcaster() else {
            return;
        };

        let mut waiting_on_user = 0usize;
        let mut queued = 0usize;
        let mut answered = 0usize;
        let mut cancelled = 0usize;
        let mut handed_off = 0usize;
        let mut pending_ids = Vec::new();

        for question in session.pending_questions.iter() {
            match question.status {
                SessionQuestionStatus::WaitingOnUser => {
                    waiting_on_user += 1;
                    pending_ids.push(question.id.clone());
                },
                SessionQuestionStatus::Queued => {
                    queued += 1;
                    pending_ids.push(question.id.clone());
                },
                SessionQuestionStatus::Answered => answered += 1,
                SessionQuestionStatus::Cancelled => cancelled += 1,
                SessionQuestionStatus::HandedOff => handed_off += 1,
            }
        }
        let _ = handed_off; // Used for internal tracking

        let active_batch = session.active_batch.as_ref().map(|batch| {
            crate::magician_v2::realtime_events::ClarificationBatchSnapshot {
                batch_id: batch.batch_id.clone(),
                total: batch.progress.total,
                answered: batch.progress.answered,
            }
        });

        let last_question_asked_at = session
            .last_question_asked_at
            .map(|ts| ts.timestamp_millis());

        emitter.clarification_session_snapshot(
            workflow_id,
            session.state.as_str(),
            session.pending_questions.len(),
            waiting_on_user,
            queued,
            answered,
            cancelled,
            pending_ids,
            active_batch,
            last_question_asked_at,
        );

        if let Some(metrics) = self.metrics_collector() {
            let stats = SessionSnapshotStats {
                state: session.state,
                total_questions: session.total_questions_asked as u64,
                waiting_on_user,
                queued,
            };
            let snapshot = metrics.record_session_snapshot(workflow_id, &stats);
            emitter.clarification_metrics_snapshot(snapshot);
        }
    }

    /// Set an optional listener to be notified when resumes succeed or fail.
    pub fn set_resume_listener(&self, listener: Option<Arc<dyn ResumeListener>>) {
        if let Ok(mut guard) = self.resume_listener.write() {
            *guard = listener;
        }
    }

    /// Rebuild a `PausedWorkflow` directly from the persisted clarification
    /// session when the in-memory pause queue has no entry for `question_id`.
    ///
    /// Returns `Some` only when the caller supplied a `workflow_id` whose
    /// persisted session still lists the question as outstanding
    /// (`WaitingOnUser`/`Queued`). Returns `None` when there is no session, the
    /// question is absent, or it has already been answered/cancelled — those
    /// cases are surfaced by the caller as `AlreadyResolved`, not a hard error.
    async fn rehydrate_paused_from_session(
        &self,
        question_id: &str,
        caller_workflow_id: Option<&str>,
    ) -> Result<Option<PausedWorkflow>, TriggerError> {
        let Some(workflow_id) = caller_workflow_id else {
            return Ok(None);
        };

        let manager = self
            .session_manager
            .read()
            .map_err(|_| {
                TriggerError::RecoveryUnavailable(
                    "clarification session manager lock is poisoned".to_owned(),
                )
            })?
            .as_ref()
            .map(Arc::clone)
            .ok_or_else(|| {
                TriggerError::RecoveryUnavailable(
                    "clarification session manager is unavailable".to_owned(),
                )
            })?;

        let session = match manager.load(workflow_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return Ok(None),
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-ASK] Fallback session load failed for {}: {}",
                    workflow_id, err
                );
                return Err(TriggerError::RecoveryUnavailable(format!(
                    "clarification session read failed for {workflow_id}: {err}"
                )));
            },
        };

        let is_outstanding = session.pending_questions.iter().any(|q| {
            q.id == question_id
                && matches!(
                    q.status,
                    SessionQuestionStatus::WaitingOnUser | SessionQuestionStatus::Queued
                )
        });
        if !is_outstanding {
            return Ok(None);
        }

        // The queue entry is gone (restart / eviction) but the session proves the
        // question is genuinely still waiting. Re-enqueue the pause entry keyed
        // off the latest persisted state so the downstream `resume()` — which
        // removes-by-workflow_id from the queue — finds it and proceeds as if the
        // entry had survived. `pause()` requires a persisted latest_state; if
        // that state or its storage is unavailable, fail closed so the caller
        // retries instead of acknowledging the pending answer as resolved.
        match self
            .pause_manager
            .pause(
                workflow_id,
                PauseReason::WaitingOnUser(question_id.to_string()),
            )
            .await
        {
            Ok(paused) => Ok(Some(paused)),
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-ASK] Failed to re-enqueue paused workflow {} for question '{}' \
                     from session fallback: {}",
                    workflow_id, question_id, err
                );
                Err(TriggerError::Pause(err))
            },
        }
    }

    /// Handle a clarifier response; resume workflow if requirements satisfied.
    ///
    /// `caller_workflow_id` is the workflow the HTTP caller already knows
    /// (`web_api.rs` passes the task id → `submit_clarification`). It is used as
    /// a durable fallback when the in-memory pause queue has no entry for the
    /// question — the queue is not persisted, so a post-restart / uncached
    /// answer can arrive with an empty queue even though the persisted
    /// clarification session still tracks the question (#20). It also lets us
    /// tell an already-resolved question from a genuinely-unknown one (#6/#21).
    pub async fn on_clarification_received(
        &self,
        question_id: &str,
        user_response: &str,
        prompt_identity: Option<&PromptIdentityContext>,
        caller_workflow_id: Option<&str>,
    ) -> Result<Vec<SlotRecord>, TriggerError> {
        let paused = match self.pause_manager.find_by_question(question_id).await? {
            Some(workflow) => workflow,
            None => {
                warn!(
                    "[MAGICIAN-V2-ASK] No live pause-queue entry for question '{}'; \
                     attempting persisted-session fallback",
                    question_id
                );
                // P1.1-#6/#20/#21: the in-memory pause queue is not persisted and
                // `resume()` drops the entry once the last question is answered, so
                // `find_by_question` returns `None` for double-submit, post-restart,
                // and resolved-elsewhere answers as well as truly-unknown ids.
                // Hard-erroring every `None` (the prior behavior) surfaced benign
                // already-resolved dismissals as a stuck 5xx attention card. Instead:
                //   1. Try the caller's workflow_id to rehydrate a still-waiting
                //      question straight from the persisted session (survives restart
                //      without depending on `restore_execution_questions`).
                //   2. Otherwise treat the missing entry as already-resolved (a
                //      dismissable soft-success), not a hard failure. A never-existed
                //      id is indistinguishable from an answered one once the session's
                //      pending_questions have been cleared, so we err toward the benign
                //      path and reserve hard errors for questions that ARE still queued
                //      but genuinely cannot resolve (handled below once we have a
                //      `paused` entry).
                match self
                    .rehydrate_paused_from_session(question_id, caller_workflow_id)
                    .await?
                {
                    Some(workflow) => {
                        info!(
                            "[MAGICIAN-V2-ASK] Rehydrated paused workflow {} for question '{}' \
                             from persisted session (queue miss)",
                            workflow.workflow_id, question_id
                        );
                        workflow
                    },
                    None => {
                        info!(
                            "[MAGICIAN-V2-ASK] Question '{}' is not pending in any queue or \
                             persisted session; treating as already-resolved",
                            question_id
                        );
                        return Err(TriggerError::AlreadyResolved(format!(
                            "question {} is not pending",
                            question_id
                        )));
                    },
                }
            },
        };

        let workflow_id = paused.workflow_id.clone();
        info!(
            "[MAGICIAN-V2-ASK] Clarifier response for workflow {} (question {})",
            workflow_id, question_id
        );

        let state_tracker = self.pause_manager.state_tracker();

        let session_manager = self
            .session_manager
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone));

        let (session_question, session_prompt_identity, session_telemetry_scope) =
            if let Some(manager) = session_manager.as_ref() {
                match manager.load(&workflow_id).await {
                    Ok(Some(session)) => (
                        session
                            .pending_questions
                            .iter()
                            .find(|q| q.id == question_id)
                            .cloned(),
                        prompt_identity_from_session_metadata(&session.metadata),
                        telemetry_scope_from_session_metadata(&session.metadata),
                    ),
                    Ok(None) => (None, None, None),
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-ASK] Failed to load session for {}: {}",
                            workflow_id, err
                        );
                        return Err(TriggerError::RecoveryUnavailable(format!(
                            "clarification session read failed for {workflow_id}: {err}"
                        )));
                    },
                }
            } else {
                (None, None, None)
            };

        let session_question = if let Some(question) = session_question {
            question
        } else {
            warn!(
                "[MAGICIAN-V2-ASK] Session question {} not found for workflow {}",
                question_id, workflow_id
            );
            return Err(TriggerError::InvalidResume(format!(
                "question {} not found in session",
                question_id
            )));
        };

        let mut clarifier_question: ClarifierQuestion = ClarifierQuestion::from(&session_question);
        // The answer is about to go through the clarifier LLM and onto the
        // durable plan: a secret never does (P3). The marker tells the planner
        // the value will be collected at execution time instead.
        let withheld_response;
        let user_response = {
            withheld_response =
                crate::magician_v2::secrets::classify::clarification_answer_for_record(
                    &session_question.question_text,
                    session_question
                        .options
                        .as_ref()
                        .is_some_and(|options| !options.is_empty()),
                    user_response.to_string(),
                );
            withheld_response.as_str()
        };

        let enriched_query = state_tracker
            .load_enriched_query(&workflow_id)
            .await
            .map_err(|error| {
                TriggerError::RecoveryUnavailable(format!(
                    "enriched query read failed for {workflow_id}: {error}"
                ))
            })?;

        if let Some(ref enriched) = enriched_query {
            inject_context_snippet(&mut clarifier_question, "Clarified plan", enriched);
        }

        let answer_telemetry_scope = session_telemetry_scope.as_ref().map(|scope| {
            OperationLlmTelemetryScope::new(&scope.principal, &scope.workspace).with_attribution(
                OperationLlmCallAttribution {
                    execution_id: Some(workflow_id.clone()),
                    task_id: scope.task_id.clone(),
                    ..OperationLlmCallAttribution::default()
                },
            )
        });

        let parsed = match self
            .clarifier
            .parse_response_with_identity_and_telemetry(
                &workflow_id,
                question_id,
                &clarifier_question,
                user_response,
                enriched_query.as_deref(),
                prompt_identity.or(session_prompt_identity.as_ref()),
                answer_telemetry_scope.as_ref(),
            )
            .await
        {
            Ok(payload) => payload,
            Err(err) => return Err(err.into()),
        };

        if parsed.slots.is_empty() {
            self.emit_resume_failed(
                &workflow_id,
                Some(question_id),
                "clarifier response did not include any slot updates",
            );
            self.notify_resume_failed(
                &workflow_id,
                "clarifier response missing slot updates".to_string(),
            )
            .await;
            return Err(TriggerError::InvalidResume(
                "clarifier response missing slot updates".to_string(),
            ));
        }

        if parsed.workflow_id != workflow_id {
            warn!(
                "[MAGICIAN-V2-ASK] Workflow mismatch for question {}: paused workflow {} but response recorded {}",
                question_id, workflow_id, parsed.workflow_id
            );
        }

        let slots = parsed.slots.clone();
        let previous_state = state_tracker.latest_state(&workflow_id).await?;
        let previous_confidence = previous_state
            .as_ref()
            .map(|state| state.confidence.overall)
            .unwrap_or(0.0);
        let pre_resume_summary = self.session_queue_summary(&workflow_id).await?;
        let predicted_summary = pre_resume_summary.map(|summary| SessionQueueSummary {
            outstanding: summary.outstanding.saturating_sub(1),
            answered: summary.answered.saturating_add(1),
        });
        let predicted_resume_mode = self.determine_resume_mode_after_answer(
            &workflow_id,
            question_id,
            parsed.interpretation.as_ref(),
            predicted_summary,
        );
        let prepared_pause = self
            .pause_manager
            .prepare_resume(
                &workflow_id,
                ResumeContext {
                    user_provided_slots: slots.clone(),
                    updated_confidence: None,
                    handled_question: Some(question_id.to_string()),
                },
            )
            .await?;
        let (predicted_answered_count, predicted_pending_count) = predicted_summary
            .map(|summary| (summary.answered, summary.outstanding))
            .unwrap_or((0, 0));
        let preparation = ResumePreparation {
            schema_version: 1,
            notification: ResumeNotification {
                workflow_id: workflow_id.clone(),
                question_id: Some(question_id.to_string()),
                stage: parsed.stage,
                blocker_type: Some(parsed.blocker_type.clone()),
                slots: slots.clone(),
                resumed_state: prepared_pause.resumed_state.clone(),
                resume_mode: predicted_resume_mode,
                answered_count: predicted_answered_count,
                pending_count: predicted_pending_count,
            },
            mutation: PreparedResumeMutation {
                pause_resume: prepared_pause.clone(),
                answered_question_id: Some(question_id.to_string()),
                answer_slots: slots.clone(),
                cancelled_question_ids: Vec::new(),
                session_state_after: None,
            },
        };
        let prepared_recovery_id = self.prepare_listener(preparation.clone()).await?;
        let resume_state = if let Some(recovery_id) = prepared_recovery_id.as_deref() {
            self.commit_listener(&preparation, recovery_id).await?;
            preparation.notification.resumed_state.clone()
        } else {
            if let Some(manager) = session_manager.as_ref() {
                if let Err(err) = manager
                    .mark_question_answered(&workflow_id, question_id, slots.clone())
                    .await
                {
                    warn!(
                        "[MAGICIAN-V2-ASK] Session manager failed to record answer for {} ({}): {}",
                        workflow_id, question_id, err
                    );
                }
            }
            self.pause_manager
                .commit_prepared_resume(&prepared_pause)
                .await?
        };
        self.emit_response_received(&workflow_id, question_id);

        // Mark answer in batch tracker if question is part of a batch
        // Also check if batch is complete before proceeding with resume (Phase 2 logic)
        // Capture batch_id and completion status for enrichment logic later (Issue #3)
        let (batch_id_opt, batch_complete_opt) = if let Some(batch_tracker) = self
            .batch_tracker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
        {
            if let Some(batch_id) = batch_tracker.find_batch_for_question(question_id) {
                batch_tracker.mark_answered(&batch_id, question_id);

                // Gap #9 fix: Store slots immediately after parsing
                batch_tracker.store_question_slots(&batch_id, question_id, parsed.slots.clone());

                let answered = batch_tracker.answered_count(&batch_id);
                let total = batch_tracker.total_count(&batch_id);

                info!(
                    "[BATCH-RESUME] Marked question {} as answered in batch {} ({}/{})",
                    question_id, batch_id, answered, total
                );

                // Phase 3: Sync batch progress to session
                if let Some(manager) = self
                    .session_manager
                    .read()
                    .ok()
                    .and_then(|guard| guard.as_ref().map(Arc::clone))
                {
                    if let Err(err) = manager
                        .update_batch_progress(&workflow_id, &batch_id, answered, total)
                        .await
                    {
                        warn!(
                            "[BATCH-RESUME] Failed to update batch progress in session for {} (batch {}): {}",
                            workflow_id, batch_id, err
                        );
                    }
                }

                // Phase 2: Check batch completion (but always proceed to resume for state persistence)
                let is_correction = parsed
                    .interpretation
                    .as_ref()
                    .map(|interp| interp.requires_replan)
                    .unwrap_or(false);

                let batch_complete = batch_tracker.is_batch_complete(&batch_id);

                if !batch_complete && !is_correction {
                    // Batch incomplete and not a correction - will still call resume() for state persistence
                    // but workflow execution should be deferred
                    info!(
                        "[BATCH-RESUME] Batch {} incomplete ({}/{}), will persist state but defer workflow execution",
                        batch_id, answered, total
                    );
                } else if batch_complete {
                    info!(
                        "[BATCH-RESUME] Batch {} complete, proceeding with full resume and query enrichment",
                        batch_id
                    );
                } else {
                    info!(
                        "[BATCH-RESUME] Correction detected, proceeding with immediate resume despite incomplete batch"
                    );
                }

                (Some(batch_id), Some(batch_complete))
            } else {
                (None, None)
            }
        } else {
            (None, None)
        };

        // Phase 5: Determine resume mode based on batch completion AND answer interpretation.
        let session_summary = self.session_queue_summary(&workflow_id).await?;
        let resume_mode = self.determine_resume_mode(
            &workflow_id,
            question_id,
            parsed.interpretation.as_ref(),
            session_summary,
        );
        info!(
            "[BATCH-RESUME] Resume mode for question {}: {:?}",
            question_id, resume_mode
        );

        // Query enrichment: run for both full and partial replans so each answered question
        // immediately improves the clarified task/enriched query.
        let should_rewrite = matches!(
            resume_mode,
            ResumeMode::FullReplan | ResumeMode::PartialReplan
        );

        if should_rewrite {
            if let Some(query_rewriter) = self
                .query_rewriter
                .read()
                .ok()
                .and_then(|guard| guard.as_ref().map(Arc::clone))
            {
                let mut rewrite_slots: Vec<SlotRecord> = Vec::new();
                let mut rewrite_source: Option<String> = None;

                if let Some(batch_id) = &batch_id_opt {
                    if let Some(batch_tracker) = self
                        .batch_tracker
                        .read()
                        .ok()
                        .and_then(|guard| guard.as_ref().map(Arc::clone))
                    {
                        rewrite_slots = batch_tracker.collect_batch_slots(batch_id);
                        rewrite_source = Some(batch_id.clone());
                    }
                }

                if rewrite_slots.is_empty() {
                    rewrite_slots = slots.clone();
                }

                if !rewrite_slots.is_empty() {
                    let lock = self.rewrite_lock(&workflow_id);
                    let _rewrite_guard = lock.lock().await;

                    let customer_query = match state_tracker
                        .conversation_store()
                        .get_turns(&workflow_id)
                        .await
                    {
                        Ok(turns) => turns
                            .into_iter()
                            .find(|turn| {
                                matches!(
                                    turn.direction,
                                    crate::magician_v2::storage::TurnDirection::Inbound
                                )
                            })
                            .map(|turn| turn.text)
                            .unwrap_or_else(|| {
                                warn!(
                                    "[BATCH-ENRICHMENT] No inbound turns found for workflow {}, using workflow_id as fallback",
                                    workflow_id
                                );
                                workflow_id.clone()
                            }),
                        Err(err) => {
                            warn!(
                                "[BATCH-ENRICHMENT] Failed to get turns for workflow {}: {:?}, using workflow_id as fallback",
                                workflow_id, err
                            );
                            workflow_id.clone()
                        },
                    };

                    let planning_query = match state_tracker
                        .get_query_for_rewrite(&workflow_id)
                        .await
                    {
                        Ok(query) if !query.is_empty() => query,
                        Ok(_) => customer_query.clone(),
                        Err(err) => {
                            warn!(
                                "[MAGICIAN-BATCH-ENRICHMENT] Failed to load query for rewrite {}: {:?}",
                                workflow_id, err
                            );
                            customer_query.clone()
                        },
                    };

                    let rewrite_label = if matches!(batch_complete_opt, Some(true)) {
                        "final"
                    } else if rewrite_slots.len() == 1 && rewrite_source.is_none() {
                        "single-question"
                    } else {
                        "partial"
                    };

                    info!(
                        "[MAGICIAN-BATCH-ENRICHMENT] 🔄 {} rewrite for workflow {}\n\
                         - Source batch: {:?}\n\
                         - Slot count: {}\n\
                         - Slot IDs: {:?}\n\
                         - Customer query: '{}'\n\
                         - Current query for rewrite: '{}'",
                        rewrite_label,
                        workflow_id,
                        rewrite_source,
                        rewrite_slots.len(),
                        rewrite_slots.iter().map(|s| &s.id).collect::<Vec<_>>(),
                        customer_query,
                        planning_query
                    );

                    let token = CancellationToken::new();
                    if let Some(previous) = self
                        .rewrite_tokens
                        .insert(workflow_id.clone(), token.clone())
                    {
                        previous.cancel();
                    }

                    let rewrite_telemetry = session_telemetry_scope.as_ref().and_then(|scope| {
                        self.event_broadcaster().map(|broadcaster| {
                            OperationLlmTelemetryContext::new(
                                broadcaster,
                                &scope.principal,
                                &scope.workspace,
                                "clarification",
                            )
                        })
                    });
                    let rewrite_future = query_rewriter.rewrite_for_planner_with_telemetry(
                        &customer_query,
                        &planning_query,
                        &rewrite_slots,
                        &[],
                        rewrite_telemetry.as_ref(),
                        OperationLlmCallAttribution {
                            execution_id: Some(workflow_id.clone()),
                            task_id: session_telemetry_scope
                                .as_ref()
                                .and_then(|scope| scope.task_id.clone()),
                            ..Default::default()
                        },
                    );
                    pin!(rewrite_future);

                    tokio::select! {
                        result = &mut rewrite_future => {
                            match result {
                                Ok(clarified_task) => {
                                    info!(
                                        "[MAGICIAN-BATCH-ENRICHMENT] ✅ {} rewrite succeeded\n\
                                         - Enriched query: '{}'\n\
                                         - Length: {} chars\n\
                                         - Planning input: '{}'",
                                        rewrite_label,
                                        clarified_task.clarified_task,
                                        clarified_task.clarified_task.len(),
                                        planning_query
                                    );

                                    if let Ok(turn_id) =
                                        state_tracker.get_current_turn_id(&workflow_id).await
                                    {
                                        if let Err(err) = state_tracker
                                            .store_enriched_query(
                                                &workflow_id,
                                                &turn_id,
                                                clarified_task.clarified_task.clone(),
                                            )
                                            .await
                                        {
                                            warn!(
                                                "[MAGICIAN-BATCH-ENRICHMENT] ❌ Failed to persist enriched query for {} (turn {}): {}",
                                                workflow_id, turn_id, err
                                            );
                                        } else {
                                            debug!(
                                                "[MAGICIAN-BATCH-ENRICHMENT] Stored enriched query for {} (turn {})",
                                                workflow_id, turn_id
                                            );
                                        }
                                    } else {
                                        warn!(
                                            "[MAGICIAN-BATCH-ENRICHMENT] ❌ Unable to determine turn id for {} while storing enriched query",
                                            workflow_id
                                        );
                                    }
                                },
                                Err(err) => {
                                    warn!(
                                        "[MAGICIAN-BATCH-ENRICHMENT] ❌ {} rewrite failed for workflow {}: {:?}",
                                        rewrite_label,
                                        workflow_id,
                                        err
                                    );
                                    self.emit_observability_alert(
                                        &workflow_id,
                                        "batch_rewrite_failed",
                                        json!({
                                            "rewrite_label": rewrite_label,
                                            "error": err.to_string(),
                                            "slot_count": rewrite_slots.len()
                                        }),
                                    );
                                },
                            }
                        }
                        _ = token.cancelled() => {
                            info!(
                                "[MAGICIAN-BATCH-ENRICHMENT] Cancelled rewrite for workflow {} (superseded request)",
                                workflow_id
                            );
                        }
                    }
                    self.rewrite_tokens.remove(&workflow_id);
                }
            }
        }

        self.history.resolve(&workflow_id, parsed.stage, &slots);

        // Phase 6: Record confidence snapshot after user answer
        let ready_to_plan = self.record_confidence_snapshot_after_answer(
            &workflow_id,
            question_id,
            &parsed,
            &slots,
            &resume_state,
            &session_question,
        );

        if ready_to_plan {
            if let Some(manager) = self
                .session_manager
                .read()
                .ok()
                .and_then(|guard| guard.as_ref().map(Arc::clone))
            {
                if let Err(err) = manager
                    .mark_state(&workflow_id, ClarificationSessionState::ReadyToPlan)
                    .await
                {
                    warn!(
                        "[MAGICIAN-V2-ASK] Failed to mark session ready_to_plan for {}: {}",
                        workflow_id, err
                    );
                }
            }
        }

        if let Some(ref ledger) = self.ledger {
            if let Some(amount) = ledger
                .policy()
                .should_replenish(previous_confidence, resume_state.confidence.overall)
            {
                ledger.replenish(&workflow_id, amount).await?;
            }
        }

        let post_resume_summary = self.session_queue_summary(&workflow_id).await?;
        let (answered_count, pending_count) = post_resume_summary
            .map(|s| (s.answered, s.outstanding))
            .unwrap_or((0, 0));

        self.emit_resumed(
            &workflow_id,
            Some(question_id),
            resume_mode,
            post_resume_summary,
        );
        if prepared_recovery_id.is_none() {
            self.notify_listener(ResumeNotification {
                workflow_id,
                question_id: Some(question_id.to_string()),
                stage: parsed.stage,
                blocker_type: Some(parsed.blocker_type),
                slots: slots.clone(),
                resumed_state: resume_state.clone(),
                resume_mode,
                answered_count,
                pending_count,
            })
            .await?;
        }
        Ok(slots)
    }

    /// Resume a workflow manually without clarifier input.
    pub async fn on_manual_resume(
        &self,
        workflow_id: &str,
        options: ManualResumeOptions,
    ) -> Result<(), TriggerError> {
        info!(
            "[MAGICIAN-V2-ASK] Manual resume requested for workflow {}",
            workflow_id
        );

        let ManualResumeOptions {
            slots,
            updated_confidence,
            handled_question,
        } = options;

        if slots.is_empty() && updated_confidence.is_none() {
            self.emit_resume_failed(
                workflow_id,
                handled_question.as_deref(),
                "manual resume requires slot updates or confidence adjustment",
            );
            return Err(TriggerError::InvalidResume(
                "manual resume requires slot updates or confidence adjustment".to_string(),
            ));
        }

        if handled_question.is_some() && slots.is_empty() {
            self.emit_resume_failed(
                workflow_id,
                handled_question.as_deref(),
                "handled question provided without slot updates",
            );
            return Err(TriggerError::InvalidResume(
                "handled question requires at least one slot update".to_string(),
            ));
        }

        let slot_updates = slots.clone();
        let handled_question_clone = handled_question.clone();
        let prepared_pause = self
            .pause_manager
            .prepare_resume(
                workflow_id,
                ResumeContext {
                    user_provided_slots: slots,
                    updated_confidence,
                    handled_question: handled_question.clone(),
                },
            )
            .await?;
        let pre_summary = self.session_queue_summary(workflow_id).await?;
        let predicted_summary = if handled_question.is_some() {
            pre_summary.map(|summary| SessionQueueSummary {
                outstanding: summary.outstanding.saturating_sub(1),
                answered: summary.answered.saturating_add(1),
            })
        } else {
            pre_summary
        };
        let predicted_mode = handled_question
            .as_deref()
            .map(|question_id| {
                self.determine_resume_mode_after_answer(
                    workflow_id,
                    question_id,
                    None,
                    predicted_summary,
                )
            })
            .unwrap_or(ResumeMode::FullReplan);
        let (predicted_answered, predicted_pending) = predicted_summary
            .map(|summary| (summary.answered, summary.outstanding))
            .unwrap_or((0, 0));
        let preparation = ResumePreparation {
            schema_version: 1,
            notification: ResumeNotification {
                workflow_id: workflow_id.to_string(),
                question_id: handled_question.clone(),
                stage: prepared_pause.resumed_state.stage_context,
                blocker_type: None,
                slots: slot_updates.clone(),
                resumed_state: prepared_pause.resumed_state.clone(),
                resume_mode: predicted_mode,
                answered_count: predicted_answered,
                pending_count: predicted_pending,
            },
            mutation: PreparedResumeMutation {
                pause_resume: prepared_pause.clone(),
                answered_question_id: handled_question.clone(),
                answer_slots: slot_updates.clone(),
                cancelled_question_ids: Vec::new(),
                session_state_after: (predicted_pending == 0)
                    .then_some(ClarificationSessionState::Planning),
            },
        };
        let prepared_recovery_id = self.prepare_listener(preparation.clone()).await?;
        let prepared_state = if let Some(recovery_id) = prepared_recovery_id.as_deref() {
            self.commit_listener(&preparation, recovery_id).await?;
            preparation.notification.resumed_state.clone()
        } else {
            self.pause_manager
                .commit_prepared_resume(&prepared_pause)
                .await?
        };
        if let Some(question_id) = handled_question.as_deref() {
            self.emit_response_received(workflow_id, question_id);
        }
        match Ok::<StateBundle, PauseError>(prepared_state) {
            Ok(state) => {
                if !slot_updates.is_empty() {
                    self.history
                        .resolve(workflow_id, state.stage_context, &slot_updates);
                }
                if prepared_recovery_id.is_none() {
                    if let Some(ref q_id) = handled_question {
                        self.record_session_answer(workflow_id, q_id, &slot_updates)
                            .await;
                    }
                }

                // Mark session state transitions like on_clarification_received does
                if prepared_recovery_id.is_none() {
                    if let Some(manager) = self
                        .session_manager
                        .read()
                        .ok()
                        .and_then(|guard| guard.as_ref().map(Arc::clone))
                    {
                        if let Err(err) = manager
                            .mark_state(workflow_id, ClarificationSessionState::ReadyToPlan)
                            .await
                        {
                            warn!(
                                "[MAGICIAN-V2-ASK] Failed to mark session ready_to_plan for {}: {}",
                                workflow_id, err
                            );
                        }
                        if let Err(err) = manager
                            .mark_state(workflow_id, ClarificationSessionState::Planning)
                            .await
                        {
                            warn!(
                                "[MAGICIAN-V2-ASK] Failed to mark session planning for {}: {}",
                                workflow_id, err
                            );
                        }
                    }
                }

                let post_summary = self.session_queue_summary(workflow_id).await?;

                // Determine resume mode - for manual resume, check if handling a specific question
                let resume_mode = if let Some(ref q_id) = handled_question {
                    // Manual resume doesn't have answer interpretation
                    self.determine_resume_mode(workflow_id, q_id, None, post_summary)
                } else {
                    // No specific question, default to full replan for manual resume
                    ResumeMode::FullReplan
                };

                let (answered_count, pending_count) = post_summary
                    .map(|s| (s.answered, s.outstanding))
                    .unwrap_or((0, 0));

                self.emit_resumed(
                    workflow_id,
                    handled_question.as_deref(),
                    resume_mode,
                    post_summary,
                );

                if prepared_recovery_id.is_none() {
                    self.notify_listener(ResumeNotification {
                        workflow_id: workflow_id.to_string(),
                        question_id: handled_question,
                        stage: state.stage_context,
                        blocker_type: None,
                        slots: slot_updates,
                        resumed_state: state,
                        resume_mode,
                        answered_count,
                        pending_count,
                    })
                    .await?;
                }
                Ok(())
            },
            Err(err) => {
                self.emit_resume_failed(
                    workflow_id,
                    handled_question_clone.as_deref(),
                    err.to_string(),
                );
                self.notify_resume_failed(workflow_id, err.to_string())
                    .await;
                Err(err.into())
            },
        }
    }

    // NOTE: on_recovery_resume() method removed - agentic execution handles state recovery
    // via observe-decide-execute loop. See execution/agentic/ module.

    pub fn emit_clarification_enqueued(
        &self,
        workflow_id: &str,
        question_id: &str,
        blocker_type: &BlockerType,
        channel: &Channel,
        stage: &StageContext,
        urgency: f64,
        // Phase H4.2 / H4.3 — optional task_id + question_text for the
        // canonical `HitlRequested` dual-emit. Both default to None so
        // existing callers compile; new callers should pass them so the
        // canonical event carries proper scope + prompt.
        task_id: Option<&str>,
        question_text: Option<&str>,
        source_slot_id: Option<&str>,
    ) {
        if let Some(emitter) = self.event_broadcaster() {
            let blocker = format!("{:?}", blocker_type);
            let delivery_channel = format!("{:?}", channel);
            let stage_label = format!("{:?}", stage);
            emitter.clarification_queued(
                workflow_id,
                question_id,
                blocker,
                delivery_channel,
                stage_label,
                urgency,
                task_id,
                question_text,
                source_slot_id,
            );
        }
    }

    fn emit_response_received(&self, workflow_id: &str, question_id: &str) {
        if let Some(emitter) = self.event_broadcaster() {
            emitter.clarification_response_received(workflow_id, question_id);
        }
    }

    fn emit_resume_failed(
        &self,
        workflow_id: &str,
        question_id: Option<&str>,
        error: impl Into<String>,
    ) {
        if let Some(emitter) = self.event_broadcaster() {
            emitter.workflow_resume_failed(
                workflow_id,
                question_id.map(|q| q.to_string()),
                error.into(),
            );
        }
    }

    fn emit_resumed(
        &self,
        workflow_id: &str,
        question_id: Option<&str>,
        resume_mode: ResumeMode,
        summary: Option<SessionQueueSummary>,
    ) {
        if let Some(emitter) = self.event_broadcaster() {
            let (answered_count, pending_count) = summary
                .map(|s| (s.answered, s.outstanding))
                .unwrap_or((0, 0));
            emitter.workflow_resumed(
                workflow_id,
                question_id.map(|q| q.to_string()),
                format!("{:?}", resume_mode),
                answered_count,
                pending_count,
            );
        }
    }

    fn event_broadcaster(&self) -> Option<Arc<RuntimeTransportBroadcaster>> {
        self.event_emitter
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
    }

    fn metrics_collector(&self) -> Option<Arc<ClarificationMetrics>> {
        self.metrics_collector
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
    }

    pub fn emit_observability_alert(
        &self,
        workflow_id: &str,
        alert_type: &str,
        details: serde_json::Value,
    ) {
        if let Some(emitter) = self.event_broadcaster() {
            emitter.observability_alert(workflow_id, alert_type.to_string(), details);
        }
    }

    fn listener(&self) -> Option<Arc<dyn ResumeListener>> {
        self.resume_listener
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
    }

    async fn prepare_listener(
        &self,
        preparation: ResumePreparation,
    ) -> Result<Option<String>, TriggerError> {
        // Every production AskLoop continuation mutates durable pause/session
        // state. It therefore requires a sealed recovery owner before that
        // mutation, whether the owner is embedded in a TaskPlan or stored in
        // the compact Runtime resume catalog.
        let requires_durable_owner = true;
        let Some(listener) = self.listener() else {
            if requires_durable_owner {
                return Err(TriggerError::InvalidResume(format!(
                    "durable_resume_listener_unavailable:{}",
                    preparation.notification.workflow_id
                )));
            }
            return Ok(None);
        };
        let recovery_id = listener
            .prepare_resume(preparation.clone())
            .await
            .map_err(TriggerError::InvalidResume)?;
        if requires_durable_owner && recovery_id.is_none() {
            return Err(TriggerError::InvalidResume(format!(
                "durable_resume_owner_unavailable:{}",
                preparation.notification.workflow_id
            )));
        }
        Ok(recovery_id)
    }

    async fn commit_listener(
        &self,
        preparation: &ResumePreparation,
        recovery_id: &str,
    ) -> Result<(), TriggerError> {
        let listener = self.listener().ok_or_else(|| {
            TriggerError::InvalidResume(format!(
                "task_planning_resume_listener_unavailable:{recovery_id}"
            ))
        })?;
        listener
            .commit_prepared_resume(preparation, recovery_id)
            .await
            .map_err(TriggerError::InvalidResume)
    }

    async fn notify_listener(&self, notification: ResumeNotification) -> Result<(), TriggerError> {
        if let Some(listener) = self.listener() {
            listener
                .on_resume(notification)
                .await
                .map_err(TriggerError::InvalidResume)?;
        }
        Ok(())
    }

    async fn notify_resume_failed(&self, workflow_id: &str, error: String) {
        if let Some(listener) = self.listener() {
            listener.on_resume_failed(workflow_id, &error).await;
        }
    }

    pub async fn resume_after_guardrail(
        &self,
        workflow_id: &str,
        breach: GuardrailBreach,
        cancelled_question_ids: Vec<String>,
    ) -> Result<StateBundle, TriggerError> {
        info!(
            "[ASK-LOOP][RESUME] Triggering resume after guardrail {:?} for workflow {}",
            breach, workflow_id
        );
        let mut prepared_pause = self
            .pause_manager
            .prepare_resume(
                workflow_id,
                ResumeContext {
                    user_provided_slots: Vec::new(),
                    updated_confidence: None,
                    handled_question: None,
                },
            )
            .await?;
        prepared_pause.remaining_questions.clear();
        let preparation = ResumePreparation {
            schema_version: 1,
            notification: ResumeNotification {
                workflow_id: workflow_id.to_string(),
                question_id: None,
                stage: prepared_pause.resumed_state.stage_context,
                blocker_type: None,
                slots: Vec::new(),
                resumed_state: prepared_pause.resumed_state.clone(),
                resume_mode: ResumeMode::FullReplan,
                answered_count: 0,
                pending_count: 0,
            },
            mutation: PreparedResumeMutation {
                pause_resume: prepared_pause.clone(),
                answered_question_id: None,
                answer_slots: Vec::new(),
                cancelled_question_ids,
                session_state_after: Some(ClarificationSessionState::ReadyToPlan),
            },
        };
        let prepared_recovery_id = self.prepare_listener(preparation.clone()).await?;
        let resume_state = if let Some(recovery_id) = prepared_recovery_id.as_deref() {
            self.commit_listener(&preparation, recovery_id).await?;
            preparation.notification.resumed_state.clone()
        } else {
            let state = self
                .commit_prepared_resume_mutation(&preparation.mutation)
                .await?;
            state
        };

        let message = format!("clarification guardrail triggered: {:?}", breach);
        let summary = self.session_queue_summary(workflow_id).await?;
        self.emit_resume_failed(workflow_id, None, &message);
        self.notify_resume_failed(workflow_id, message.clone())
            .await;
        self.emit_resumed(workflow_id, None, ResumeMode::FullReplan, summary);
        if prepared_recovery_id.is_none() {
            let (answered_count, pending_count) = summary
                .map(|value| (value.answered, value.outstanding))
                .unwrap_or((0, 0));
            self.notify_listener(ResumeNotification {
                workflow_id: workflow_id.to_string(),
                question_id: None,
                stage: resume_state.stage_context,
                blocker_type: None,
                slots: Vec::new(),
                resumed_state: resume_state.clone(),
                resume_mode: ResumeMode::FullReplan,
                answered_count,
                pending_count,
            })
            .await?;
        }

        info!(
            "[ASK-LOOP][RESUME] Resume notification sent for workflow {}",
            workflow_id
        );

        Ok(resume_state)
    }

    /// Phase 6: Record a confidence snapshot after a user provides an answer.
    ///
    /// Updates parameter confidence based on:
    /// - Which parameter was answered (from slots)
    /// - The interpretation confidence
    /// - The answer type (correction may lower confidence in related params)
    fn record_confidence_snapshot_after_answer(
        &self,
        workflow_id: &str,
        question_id: &str,
        parsed: &ClarifierResponsePayload,
        slots: &[SlotRecord],
        resume_state: &StateBundle,
        session_question: &SessionQuestion,
    ) -> bool {
        let span = info_span!(
            "clarification.record_confidence_snapshot",
            workflow_id = workflow_id,
            question_id = question_id,
            blocker = ?parsed.blocker_type,
            answer_type = tracing::field::debug(
                parsed
                    .interpretation
                    .as_ref()
                    .map(|interp| &interp.answer_type)
            )
        );
        let _enter = span.enter();
        let Some(tracker) = self
            .confidence_tracker
            .read()
            .ok()
            .and_then(|guard| guard.as_ref().map(Arc::clone))
        else {
            self.emit_observability_alert(
                workflow_id,
                "confidence_service_unavailable",
                json!({
                    "question_id": question_id,
                    "stage": parsed.stage,
                    "blocker": parsed.blocker_type
                }),
            );
            return false; // Confidence tracking not enabled
        };

        // Get previous snapshot for delta calculation
        let previous_snapshot = tracker.get_latest(workflow_id);
        let previous_confidence = previous_snapshot
            .as_ref()
            .map(|s| s.overall_confidence)
            .unwrap_or(resume_state.confidence.overall);

        // Build parameter confidence map based on updated slots
        let mut parameter_confidence = previous_snapshot
            .as_ref()
            .map(|s| s.parameter_confidence.clone())
            .unwrap_or_default();

        let mut slot_deltas = Vec::new();

        // Update confidence for parameters that were just answered
        for slot in slots {
            let slot_confidence = slot.confidence;
            parameter_confidence.insert(slot.id.clone(), slot_confidence);

            let previous_value = if session_question
                .source_slot_id
                .as_deref()
                .map(|id| id == slot.id)
                .unwrap_or(false)
            {
                session_question.slot_confidence.map(|c| c as f64)
            } else {
                previous_snapshot
                    .as_ref()
                    .and_then(|s| s.parameter_confidence.get(&slot.id).copied())
            };

            slot_deltas.push(SlotConfidenceDelta {
                slot_id: slot.id.clone(),
                previous: previous_value,
                updated: slot_confidence,
            });
        }

        // If this was a correction, potentially lower confidence in related parameters
        if let Some(ref interpretation) = parsed.interpretation {
            if interpretation.answer_type == AnswerType::Correction {
                // Lower confidence slightly for all other parameters (they might also need correction)
                for (param_name, conf) in parameter_confidence.iter_mut() {
                    if !slots.iter().any(|s| &s.id == param_name) {
                        *conf *= 0.95; // 5% reduction for uncertainty introduced by correction
                    }
                }
            }
        }

        // Determine trigger based on answer interpretation
        let trigger = if let Some(ref interpretation) = parsed.interpretation {
            match interpretation.answer_type {
                AnswerType::Correction => ConfidenceTrigger::Correction {
                    question_id: question_id.to_string(),
                },
                AnswerType::Clarification if interpretation.requires_replan => {
                    ConfidenceTrigger::Discovery {
                        question_id: question_id.to_string(),
                    }
                },
                _ => ConfidenceTrigger::UserAnswer {
                    question_id: question_id.to_string(),
                },
            }
        } else {
            ConfidenceTrigger::UserAnswer {
                question_id: question_id.to_string(),
            }
        };

        // Calculate confidence delta
        let current_confidence = resume_state.confidence.overall;
        let confidence_delta = Some(current_confidence - previous_confidence);

        // Get iteration number from history
        let iteration = tracker.get_history(workflow_id).len();

        // Count unresolved parameters
        // For now, we'll use a heuristic: parameters in confidence map that are below threshold
        let unresolved_count = parameter_confidence
            .values()
            .filter(|&&conf| conf < 0.7)
            .count();

        let trigger_label = format!("{:?}", trigger);
        let slot_delta_events = slot_deltas.clone();

        let snapshot = PlanConfidenceSnapshot {
            iteration,
            overall_confidence: current_confidence,
            parameter_confidence,
            trigger,
            confidence_delta,
            unresolved_count,
            timestamp: chrono::Utc::now(),
            notes: Some(format!(
                "After answering question {}, {} slot(s) updated",
                question_id,
                slots.len()
            )),
            slot_deltas,
        };

        tracker.record_snapshot(workflow_id, snapshot);

        if let Some(emitter) = self.event_broadcaster() {
            let slot_delta_events: Vec<ConfidenceSlotDelta> = slot_delta_events
                .iter()
                .map(|delta| ConfidenceSlotDelta {
                    slot_id: delta.slot_id.clone(),
                    previous: delta.previous,
                    updated: delta.updated,
                })
                .collect();
            emitter.clarification_confidence_snapshot(
                workflow_id,
                question_id,
                trigger_label,
                current_confidence,
                unresolved_count,
                slot_delta_events,
                session_question.created_at.timestamp_millis(),
                chrono::Utc::now().timestamp_millis(),
            );
        }

        tracker.is_sufficient(workflow_id)
    }
}

fn inject_context_snippet(question: &mut ClarifierQuestion, label: &str, value: &str) {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return;
    }

    if question
        .context_snippets
        .iter()
        .any(|existing| existing.starts_with(label))
    {
        return;
    }

    question
        .context_snippets
        .insert(0, format!("{}: {}", label, trimmed));
}

fn prompt_identity_from_session_metadata(
    metadata: &serde_json::Value,
) -> Option<PromptIdentityContext> {
    let object = metadata.as_object()?;

    if let Some(raw) = object.get("agent:prompt_identity") {
        if let Some(raw_json) = raw.as_str() {
            return serde_json::from_str::<PromptIdentityContext>(raw_json).ok();
        }
        return serde_json::from_value::<PromptIdentityContext>(raw.clone()).ok();
    }

    if let Some(raw) = object.get("prompt_identity") {
        return serde_json::from_value::<PromptIdentityContext>(raw.clone()).ok();
    }

    None
}

fn telemetry_scope_from_session_metadata(
    metadata: &serde_json::Value,
) -> Option<SessionTelemetryScope> {
    let object = metadata.as_object()?;
    let principal = object.get("telemetry:principal")?.as_str()?.trim();
    let workspace = object.get("telemetry:workspace")?.as_str()?.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    let task_id = object
        .get("telemetry:task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    Some(SessionTelemetryScope {
        principal: principal.to_string(),
        workspace: workspace.to_string(),
        task_id,
    })
}

#[derive(Debug, Error)]
pub enum TriggerError {
    #[error("pause error: {0}")]
    Pause(#[from] PauseError),
    #[error("clarifier error: {0}")]
    Clarifier(#[from] super::clarifier::ClarifierError),
    #[error("ledger error: {0}")]
    Ledger(#[from] BudgetLedgerError),
    #[error("storage error: {0}")]
    Storage(#[from] V2StorageError),
    #[error("resume recovery unavailable: {0}")]
    RecoveryUnavailable(String),
    #[error("invalid resume request: {0}")]
    InvalidResume(String),
    /// The question existed and was already answered/resolved (double-submit,
    /// resolved-elsewhere, post-restart replay). Distinct from `InvalidResume`,
    /// which is a genuine bad request. Callers should map this to a dismissable
    /// soft-success (HTTP 409 + `reason: "already_resolved"`) rather than a hard
    /// error, so an already-answered clarification card is dismissed instead of
    /// sticking with a 4xx/5xx error.
    #[error("clarification already resolved: {0}")]
    AlreadyResolved(String),
}

#[derive(Debug, Default)]
pub struct ManualResumeOptions {
    pub slots: Vec<SlotRecord>,
    pub updated_confidence: Option<f64>,
    pub handled_question: Option<String>,
}

// NOTE: RecoveryResumeOptions struct removed - agentic execution handles state recovery
// via observe-decide-execute loop. See execution/agentic/ module.

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::magician_v2::{
        ask_loop::ClarificationSessionStoreError,
        ask_loop::{
            clarifier::{AnswerInterpretation, ClarifierQuestion, DeterministicClarifier},
            pause::{InMemoryQueueRepository, QueueRepository, WaitingQueue},
            session::{ClarificationSession, ClarificationSessionState, SessionQuestionStatus},
            session_manager::SessionManager,
        },
        confidence::ConfidenceService,
        execution::{PromptAgentKind, PromptIdentityContext},
        state_tracker::StateTracker,
        storage::{file::FileV2Store, V2ConversationStore},
    };

    #[test]
    fn clarification_recovery_reads_fail_closed_before_receipt_preparation() {
        let source = include_str!("triggers.rs");
        let fallback = source
            .split("async fn rehydrate_paused_from_session")
            .nth(1)
            .and_then(|tail| tail.split("pub async fn on_clarification_received").next())
            .expect("persisted clarification fallback");
        assert!(fallback.contains("TriggerError::RecoveryUnavailable"));
        assert!(fallback.contains("clarification session manager is unavailable"));
        assert!(fallback.contains("Err(TriggerError::Pause(err))"));
        assert!(!fallback.contains("let Some(manager) = manager else"));

        let answer = source
            .split("pub async fn on_clarification_received")
            .nth(1)
            .and_then(|tail| tail.split("pub async fn on_manual_resume").next())
            .expect("clarification answer transaction");
        assert!(answer.contains("enriched query read failed"));
        assert!(answer.contains("session_queue_summary(&workflow_id).await?"));
        assert!(!answer
            .contains("load_enriched_query(&workflow_id)\n            .await\n            .ok()"));
    }
    use async_trait::async_trait;
    use tempfile::TempDir;

    #[test]
    fn task_plan_resume_delegates_prepared_mutation_to_the_durable_owner() {
        let source = include_str!("triggers.rs");
        for (start, end) in [
            (
                "pub async fn on_clarification_received",
                "/// Resume a workflow manually",
            ),
            (
                "pub async fn on_manual_resume",
                "// NOTE: on_recovery_resume",
            ),
            (
                "pub async fn resume_after_guardrail",
                "/// Phase 6: Record a confidence",
            ),
        ] {
            let window = source
                .split(start)
                .nth(1)
                .and_then(|tail| tail.split(end).next())
                .expect("resume source window");
            let prepare = window
                .find("prepare_listener(preparation.clone())")
                .expect("durable prepare listener");
            let commit = window
                .find("commit_listener(&preparation")
                .expect("combined durable mutation owner");
            assert!(prepare < commit);

            let durable_branch = window
                .split("if let Some(recovery_id) = prepared_recovery_id.as_deref()")
                .nth(1)
                .and_then(|tail| tail.split("} else {").next())
                .expect("durably owned resume branch");
            assert!(durable_branch.contains("commit_listener(&preparation"));
            assert!(
                !durable_branch.contains("commit_prepared_resume_mutation")
                    && !durable_branch.contains("commit_prepared_resume(&prepared_pause)")
                    && !durable_branch.contains("mark_question_answered"),
                "the synchronous path must not mutate AskLoop before the durable listener claims the prepared receipt"
            );
        }
        let listener = source
            .split("async fn prepare_listener")
            .nth(1)
            .and_then(|tail| tail.split("async fn notify_listener").next())
            .expect("listener preparation gate");
        assert!(listener.contains("task_planning_resume_listener_unavailable"));
        assert!(listener.contains("durable_resume_owner_unavailable"));
    }

    struct NoopSessionManager;

    #[async_trait]
    impl SessionManager for NoopSessionManager {
        async fn load(
            &self,
            _workflow_id: &str,
        ) -> Result<Option<ClarificationSession>, ClarificationSessionStoreError> {
            Ok(None)
        }

        async fn save(
            &self,
            _session: ClarificationSession,
        ) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }

        async fn delete(&self, _workflow_id: &str) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }

        async fn append_question(
            &self,
            _workflow_id: &str,
            _question: ClarifierQuestion,
        ) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }

        async fn mark_question_answered(
            &self,
            _workflow_id: &str,
            _question_id: &str,
            _slots: Vec<crate::magician_v2::slot_graph::SlotRecord>,
        ) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }

        async fn cancel_questions(
            &self,
            _workflow_id: &str,
            _question_ids: &[String],
        ) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }

        async fn mark_questions_handed_off(
            &self,
            _workflow_id: &str,
            _question_ids: &[String],
        ) -> Result<usize, ClarificationSessionStoreError> {
            Ok(0)
        }

        async fn mark_state(
            &self,
            _workflow_id: &str,
            _state: ClarificationSessionState,
        ) -> Result<(), ClarificationSessionStoreError> {
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

        async fn ensure_round_limit(
            &self,
            _workflow_id: &str,
            _limit: usize,
        ) -> Result<(), ClarificationSessionStoreError> {
            Ok(())
        }
    }

    fn build_service() -> (
        Arc<ResumeTriggerService>,
        Arc<QuestionBatchTracker>,
        TempDir,
    ) {
        let temp_dir = TempDir::new().expect("temp dir");
        let store = Arc::new(FileV2Store::new(temp_dir.path())) as Arc<dyn V2ConversationStore>;
        let confidence_service = Arc::new(ConfidenceService::default());
        let state_tracker = Arc::new(StateTracker::with_confidence_service(
            Arc::clone(&store),
            Arc::clone(&confidence_service),
        ));
        let repo: Arc<dyn QueueRepository> = Arc::new(InMemoryQueueRepository::default());
        let waiting_queue = Arc::new(WaitingQueue::new(repo));
        let pause_manager = Arc::new(PauseResumeManager::new(
            Arc::clone(&state_tracker),
            waiting_queue,
        ));
        let clarifier = Arc::new(ClarifierLibrary::with_default_templates(Arc::new(
            DeterministicClarifier,
        )));
        let history = Arc::new(ClarificationHistory::new());
        let resume_service = Arc::new(ResumeTriggerService::new(
            pause_manager,
            clarifier,
            None,
            None,
            history,
        ));
        let batch_tracker = Arc::new(QuestionBatchTracker::new());
        resume_service.set_batch_tracker(Arc::clone(&batch_tracker));
        resume_service.set_session_manager(Arc::new(NoopSessionManager));
        (resume_service, batch_tracker, temp_dir)
    }

    #[tokio::test]
    async fn persisted_clarification_recovery_fails_closed_without_session_manager() {
        let (service, _batch, _tmp) = build_service();
        *service
            .session_manager
            .write()
            .expect("session manager test lock") = None;

        let error = service
            .rehydrate_paused_from_session("question-1", Some("workflow-1"))
            .await
            .expect_err("missing durable session authority must stay retryable");

        assert!(matches!(
            error,
            TriggerError::RecoveryUnavailable(message)
                if message == "clarification session manager is unavailable"
        ));
    }

    #[test]
    fn resume_mode_respects_session_outstanding_queue() {
        let (service, _batch, _tmp) = build_service();

        let mode = service.determine_resume_mode(
            "wf-1",
            "q-1",
            None,
            Some(SessionQueueSummary {
                outstanding: 2,
                answered: 0,
            }),
        );

        assert_eq!(mode, ResumeMode::LightSlotUpdate);
    }

    #[test]
    fn resume_mode_escalates_when_queue_clears() {
        let (service, _batch, _tmp) = build_service();

        let mode = service.determine_resume_mode(
            "wf-1",
            "q-1",
            None,
            Some(SessionQueueSummary {
                outstanding: 0,
                answered: 3,
            }),
        );

        assert_eq!(mode, ResumeMode::PartialReplan);
    }

    #[test]
    fn resume_mode_prioritises_replan_interpretation() {
        let (service, _batch, _tmp) = build_service();

        let interpretation = AnswerInterpretation {
            answer_type: AnswerType::Correction,
            requires_replan: true,
            confidence: 0.9,
        };

        let mode = service.determine_resume_mode(
            "wf-1",
            "q-1",
            Some(&interpretation),
            Some(SessionQueueSummary {
                outstanding: 2,
                answered: 0,
            }),
        );

        assert_eq!(mode, ResumeMode::PartialReplan);
    }

    #[test]
    fn resume_mode_follows_batch_progress() {
        let (service, batch_tracker, _tmp) = build_service();

        batch_tracker.register_batch(
            "batch-1".to_string(),
            "wf-1".to_string(),
            vec![
                "q-1".to_string(),
                "q-2".to_string(),
                "q-3".to_string(),
                "q-4".to_string(),
            ],
            None,
        );

        batch_tracker.mark_answered("batch-1", "q-1");
        batch_tracker.mark_answered("batch-1", "q-2");
        let partial_mode = service.determine_resume_mode("wf-1", "q-2", None, None);
        assert_eq!(partial_mode, ResumeMode::PartialReplan);

        batch_tracker.mark_answered("batch-1", "q-3");
        batch_tracker.mark_answered("batch-1", "q-4");
        let full_mode = service.determine_resume_mode("wf-1", "q-4", None, None);
        assert_eq!(full_mode, ResumeMode::FullReplan);
    }

    #[test]
    fn resume_mode_stays_light_for_incomplete_batch() {
        let (service, batch_tracker, _tmp) = build_service();

        batch_tracker.register_batch(
            "batch-light".to_string(),
            "wf-1".to_string(),
            vec![
                "q-1".to_string(),
                "q-2".to_string(),
                "q-3".to_string(),
                "q-4".to_string(),
            ],
            None,
        );

        batch_tracker.mark_answered("batch-light", "q-1");
        let mode = service.determine_resume_mode("wf-1", "q-1", None, None);

        assert_eq!(
            mode,
            ResumeMode::LightSlotUpdate,
            "Batch below 50% completion should stay in LightSlotUpdate"
        );
    }

    #[test]
    fn resume_mode_defaults_to_full_replan_when_no_context() {
        let (service, _batch, _tmp) = build_service();

        let mode = service.determine_resume_mode("wf-1", "q-1", None, None);

        assert_eq!(mode, ResumeMode::FullReplan);
    }

    #[derive(Clone, Default)]
    struct TrackingSessionManager {
        sessions: Arc<tokio::sync::Mutex<HashMap<String, ClarificationSession>>>,
    }

    impl TrackingSessionManager {
        fn new() -> Self {
            Self {
                sessions: Arc::new(tokio::sync::Mutex::new(HashMap::new())),
            }
        }
    }

    #[async_trait::async_trait]
    impl SessionManager for TrackingSessionManager {
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
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            entry.update_question_status(question_id, SessionQuestionStatus::Answered);
            for slot in slots {
                entry.record_slot_update(question_id, slot);
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
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            for id in question_ids {
                entry.update_question_status(id, SessionQuestionStatus::Cancelled);
            }
            Ok(())
        }

        async fn mark_questions_handed_off(
            &self,
            workflow_id: &str,
            question_ids: &[String],
        ) -> Result<usize, ClarificationSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            Ok(entry.mark_questions_handed_off(question_ids))
        }

        async fn mark_state(
            &self,
            workflow_id: &str,
            state: ClarificationSessionState,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            match state {
                ClarificationSessionState::CollectingAnswers => entry.mark_collecting(),
                ClarificationSessionState::ReadyToPlan => entry.mark_ready_to_plan(),
                ClarificationSessionState::Planning => entry.mark_planning(),
            }
            Ok(())
        }

        async fn update_batch_progress(
            &self,
            workflow_id: &str,
            batch_id: &str,
            answered: usize,
            total: usize,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            entry.update_batch_progress(batch_id, answered, total);
            Ok(())
        }

        async fn ensure_round_limit(
            &self,
            workflow_id: &str,
            limit: usize,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            let entry = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            entry.set_round_limit(limit);
            Ok(())
        }
    }

    fn make_batch_question(id: &str, slot_id: &str, batch_id: &str) -> ClarifierQuestion {
        ClarifierQuestion {
            id: id.to_string(),
            blocker_type: BlockerType::LowConfidenceSlot,
            stage: StageContext::PlanningBootstrap,
            source_slot_id: Some(slot_id.to_string()),
            question_text: format!("Clarify {}", slot_id),
            context_snippets: vec![],
            urgency: 0.6,
            channel: Channel::InApp,
            created_at: chrono::Utc::now(),
            options: None,
            batch_id: Some(batch_id.to_string()),
            batch_total: Some(2),
            ..ClarifierQuestion::default()
        }
    }

    #[tokio::test]
    async fn full_batch_completion_promotes_full_replan() {
        let (service, batch_tracker, _tmp) = build_service();
        let tracking_manager = Arc::new(TrackingSessionManager::new());
        service.set_session_manager(tracking_manager.clone());

        let workflow_id = "wf-batch";
        let batch_id = "batch-1";
        let q1 = make_batch_question("q-1", "slot:alpha", batch_id);
        let q2 = make_batch_question("q-2", "slot:beta", batch_id);

        tracking_manager
            .append_question(workflow_id, q1.clone())
            .await
            .unwrap();
        tracking_manager
            .append_question(workflow_id, q2.clone())
            .await
            .unwrap();

        batch_tracker.register_batch(
            batch_id.to_string(),
            workflow_id.to_string(),
            vec![q1.id.clone(), q2.id.clone()],
            None,
        );

        // First answer: mark question answered, store slots, ensure resume mode stays light
        batch_tracker.mark_answered(batch_id, &q1.id);
        batch_tracker.store_question_slots(
            batch_id,
            &q1.id,
            vec![SlotRecord {
                id: "slot:alpha".into(),
                slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
                value: serde_json::json!("alpha"),
                confidence: 0.8,
                provenance: vec![],
                evidence_links: vec![],
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
        );

        service
            .record_session_answer(
                workflow_id,
                &q1.id,
                &[SlotRecord {
                    id: "slot:alpha".into(),
                    slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
                    value: serde_json::json!("alpha"),
                    confidence: 0.8,
                    provenance: vec![],
                    evidence_links: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                }],
            )
            .await;

        let summary_after_first = service
            .session_queue_summary(workflow_id)
            .await
            .expect("session summary");
        assert_eq!(
            summary_after_first.unwrap().outstanding,
            1,
            "one outstanding question should remain after first answer"
        );

        let mode_after_first = service.determine_resume_mode(
            workflow_id,
            &q1.id,
            None,
            service
                .session_queue_summary(workflow_id)
                .await
                .expect("session summary"),
        );
        assert_eq!(
            mode_after_first,
            ResumeMode::LightSlotUpdate,
            "incomplete batch should remain in light slot update mode"
        );

        // Second answer: completes batch and should promote to full replan
        batch_tracker.mark_answered(batch_id, &q2.id);
        batch_tracker.store_question_slots(
            batch_id,
            &q2.id,
            vec![SlotRecord {
                id: "slot:beta".into(),
                slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
                value: serde_json::json!("beta"),
                confidence: 0.85,
                provenance: vec![],
                evidence_links: vec![],
                created_at: chrono::Utc::now(),
                updated_at: chrono::Utc::now(),
            }],
        );

        service
            .record_session_answer(
                workflow_id,
                &q2.id,
                &[SlotRecord {
                    id: "slot:beta".into(),
                    slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
                    value: serde_json::json!("beta"),
                    confidence: 0.85,
                    provenance: vec![],
                    evidence_links: vec![],
                    created_at: chrono::Utc::now(),
                    updated_at: chrono::Utc::now(),
                }],
            )
            .await;

        let summary_after_second = service
            .session_queue_summary(workflow_id)
            .await
            .expect("session summary");
        assert_eq!(
            summary_after_second.unwrap().outstanding,
            0,
            "no outstanding questions should remain after batch completion"
        );
        assert!(
            batch_tracker.is_batch_complete(batch_id),
            "batch tracker should report completion after second answer"
        );

        let final_mode = service.determine_resume_mode(
            workflow_id,
            &q2.id,
            None,
            service
                .session_queue_summary(workflow_id)
                .await
                .expect("session summary"),
        );
        assert_eq!(
            final_mode,
            ResumeMode::FullReplan,
            "completed batch should escalate to full replan"
        );
    }

    #[test]
    fn prompt_identity_parser_supports_string_and_object_metadata() {
        let identity = PromptIdentityContext {
            agent_kind: Some(PromptAgentKind::User),
            base_persona: Some("Careful".to_string()),
            source_agent_id: Some("agent-parse-test".to_string()),
            source_agent_name: Some("Assistant".to_string()),
            source_agent_aliases: Vec::new(),
            source_agent_persona: None,
            autonomous_controls: None,
        };
        let raw = serde_json::to_string(&identity).expect("serialize identity");

        let string_metadata = serde_json::json!({
            "agent:prompt_identity": raw,
        });
        let parsed_from_string =
            prompt_identity_from_session_metadata(&string_metadata).expect("parse from string");
        assert_eq!(
            parsed_from_string.source_agent_id.as_deref(),
            Some("agent-parse-test")
        );

        let object_metadata = serde_json::json!({
            "prompt_identity": identity,
        });
        let parsed_from_object =
            prompt_identity_from_session_metadata(&object_metadata).expect("parse from object");
        assert_eq!(
            parsed_from_object.source_agent_id.as_deref(),
            Some("agent-parse-test")
        );
    }

    #[test]
    fn telemetry_scope_parser_requires_complete_scope_and_retains_task() {
        let metadata = serde_json::json!({
            "telemetry:principal": "principal-a",
            "telemetry:workspace": "workspace-a",
            "telemetry:task_id": "task-a",
        });
        let scope = telemetry_scope_from_session_metadata(&metadata).expect("telemetry scope");
        assert_eq!(scope.principal, "principal-a");
        assert_eq!(scope.workspace, "workspace-a");
        assert_eq!(scope.task_id.as_deref(), Some("task-a"));

        assert!(telemetry_scope_from_session_metadata(&serde_json::json!({
            "telemetry:principal": "principal-a"
        }))
        .is_none());
    }
}

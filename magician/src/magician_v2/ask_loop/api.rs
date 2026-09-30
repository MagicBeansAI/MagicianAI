use std::{collections::HashMap, sync::Arc};

use actix_web::{web, HttpResponse, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tracing::{debug, error, info, warn};

use super::{
    batch_tracker::QuestionBatchTracker,
    budget::{Channel, TaskComplexity},
    clarifier::{build_slot_question_hint, ClarifierLibrary, ClarifierQuestion, WorkflowContext},
    history::ClarificationHistory,
    ledger::{BudgetLedger, BudgetLedgerError},
    metrics::ClarificationMetrics,
    pause::{PauseReason, PauseResumeManager},
    plan_confidence::PlanConfidenceTracker,
    session::{
        ClarificationSession, ClarificationSessionState, ClarificationSessionStoreError,
        GuardrailBreach, SessionQuestion, SessionQuestionStatus,
    },
    triggers::{ManualResumeOptions, PreparedResumeMutation, ResumeListener, ResumeTriggerService},
};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{OperationLlmCallAttribution, OperationLlmTelemetryScope},
    ask_loop::SessionManager,
    elicitation::ElicitationManager,
    execution::PromptIdentityContext,
    realtime_events::RuntimeTransportBroadcaster,
    slot_graph::{
        rewriter::QuestionRewriter, ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType,
    },
    state_tracker::{AssetContent, AssetType, ObservationAsset, StageContext, StateTracker},
    storage::{V2ConversationStore, V2StorageError},
    strategy::PlanGraph,
};

/// High-level API surface for the Ask loop.
pub struct AskLoopApi {
    clarifier: Arc<ClarifierLibrary>,
    pause_manager: Arc<PauseResumeManager>,
    ledger: Arc<BudgetLedger>,
    state_tracker: Arc<StateTracker>,
    resume_service: Arc<ResumeTriggerService>,
    history: Arc<ClarificationHistory>,
    /// Batch tracker for grouping related questions and determining completion
    batch_tracker: Arc<QuestionBatchTracker>,
    /// Phase 6: Plan confidence tracker for iterative ask-plan-ask loop
    confidence_tracker: Arc<PlanConfidenceTracker>,
    /// Query rewriter for generating enriched queries after batch completion
    query_rewriter: Arc<QuestionRewriter>,
    /// Metrics collector feeding dashboards
    metrics: Arc<ClarificationMetrics>,
    /// Optional elicitation manager for progressive elicitation filtering
    elicitation_manager: Option<Arc<ElicitationManager>>,
    /// Session manager for clarification state persistence
    session_manager: Arc<dyn SessionManager>,
    /// Conversation store for cleaning up turn metadata
    conversation_store: Arc<dyn V2ConversationStore>,
}

impl AskLoopApi {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        clarifier: Arc<ClarifierLibrary>,
        pause_manager: Arc<PauseResumeManager>,
        ledger: Arc<BudgetLedger>,
        resume_service: Arc<ResumeTriggerService>,
        history: Arc<ClarificationHistory>,
        batch_tracker: Arc<QuestionBatchTracker>,
        confidence_tracker: Arc<PlanConfidenceTracker>,
        query_rewriter: Arc<QuestionRewriter>,
        metrics: Arc<ClarificationMetrics>,
        session_manager: Arc<dyn SessionManager>,
        conversation_store: Arc<dyn V2ConversationStore>,
    ) -> Self {
        resume_service.set_session_manager(Arc::clone(&session_manager));
        resume_service.set_metrics_collector(Arc::clone(&metrics));
        let state_tracker = pause_manager.state_tracker();
        Self {
            clarifier,
            pause_manager,
            ledger,
            state_tracker,
            resume_service,
            history,
            batch_tracker,
            confidence_tracker,
            query_rewriter,
            metrics,
            elicitation_manager: None,
            session_manager,
            conversation_store,
        }
    }

    /// Set the elicitation manager for progressive elicitation filtering
    pub fn with_elicitation_manager(mut self, manager: Arc<ElicitationManager>) -> Self {
        self.elicitation_manager = Some(manager);
        self
    }

    /// Attach a real-time event broadcaster so resume outcomes surface immediately.
    pub fn attach_event_broadcaster(&self, broadcaster: Arc<RuntimeTransportBroadcaster>) {
        self.resume_service.set_event_emitter(Some(broadcaster));
    }

    /// Remove any previously attached event broadcaster.
    pub fn detach_event_broadcaster(&self) {
        self.resume_service.set_event_emitter(None);
    }

    /// Attach a resume listener for orchestration integrations.
    pub fn set_resume_listener(&self, listener: Option<Arc<dyn ResumeListener>>) {
        self.resume_service.set_resume_listener(listener);
    }

    /// Expose the shared budget ledger for strategy-level integrations.
    pub fn budget_ledger(&self) -> Arc<BudgetLedger> {
        Arc::clone(&self.ledger)
    }

    /// Expose state tracker for controller integrations.
    pub fn state_tracker(&self) -> Arc<StateTracker> {
        Arc::clone(&self.state_tracker)
    }

    /// Expose clarifier library for question restoration.
    pub fn clarifier(&self) -> Arc<ClarifierLibrary> {
        Arc::clone(&self.clarifier)
    }

    /// Expose pause manager for workflow restoration.
    pub fn pause_manager(&self) -> Arc<PauseResumeManager> {
        Arc::clone(&self.pause_manager)
    }

    pub async fn commit_prepared_resume_mutation(
        &self,
        mutation: &PreparedResumeMutation,
    ) -> Result<crate::magician_v2::state_tracker::StateBundle, AskLoopError> {
        self.resume_service
            .commit_prepared_resume_mutation(mutation)
            .await
            .map_err(AskLoopError::from)
    }

    /// Expose session manager for orchestrator integrations.
    pub fn session_manager(&self) -> Arc<dyn SessionManager> {
        Arc::clone(&self.session_manager)
    }

    /// Expose batch tracker for batch completion tracking.
    pub fn batch_tracker(&self) -> Arc<QuestionBatchTracker> {
        Arc::clone(&self.batch_tracker)
    }

    /// Phase 6: Expose confidence tracker for plan confidence monitoring.
    pub fn confidence_tracker(&self) -> Arc<PlanConfidenceTracker> {
        Arc::clone(&self.confidence_tracker)
    }

    /// Return pending session questions including their persisted status and options.
    pub async fn get_pending_session_questions_for_execution(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<SessionQuestion>, AskLoopError> {
        let session = match self.session_manager.load(workflow_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return Ok(Vec::new()),
            Err(err) => {
                if err.to_string().contains("Execution not found") {
                    debug!(
                        workflow = %workflow_id,
                        "execution not found for clarification session, returning empty list"
                    );
                } else {
                    warn!(
                        workflow = %workflow_id,
                        "failed to load clarification session: {}, returning empty list",
                        err
                    );
                }
                return Ok(Vec::new());
            },
        };

        Ok(session
            .pending_questions
            .iter()
            .filter(|question| {
                matches!(
                    question.status,
                    SessionQuestionStatus::WaitingOnUser | SessionQuestionStatus::Queued
                )
            })
            .cloned()
            .collect())
    }

    /// Wire up the batch tracker to the resume service (enables batch-aware resume mode).
    pub fn wire_batch_tracker(&self) {
        self.resume_service
            .set_batch_tracker(Arc::clone(&self.batch_tracker));
    }

    /// Phase 6: Wire up the confidence tracker to the resume service (enables confidence tracking on answers).
    pub fn wire_confidence_tracker(&self) {
        self.resume_service
            .set_confidence_tracker(Arc::clone(&self.confidence_tracker));
    }

    /// Wire up the query rewriter to the resume service (enables query enrichment after batch completion).
    pub fn wire_query_rewriter(&self) {
        self.resume_service
            .set_query_rewriter(Arc::clone(&self.query_rewriter));
    }

    /// Check whether clarification sessions for the workflow have exceeded guardrails (timeouts, caps).
    /// Returns the breach that was handled (if any). When a breach occurs, pending questions are cancelled
    /// and the workflow is resumed in best-effort mode.
    pub async fn enforce_guardrails(
        &self,
        workflow_id: &str,
    ) -> Result<Option<GuardrailBreach>, AskLoopError> {
        let session = match self
            .session_manager
            .load(workflow_id)
            .await
            .map_err(Self::map_session_error)?
        {
            Some(session) => session,
            None => return Ok(None),
        };

        let now = Utc::now();
        let Some(breach) = session.guardrail_status(now) else {
            return Ok(None);
        };

        info!(
            "[ASK-LOOP][GUARDRAIL] Breach detected: {:?} | round={}/{} | total_questions={}/{} | workflow={}",
            breach,
            session.round_count,
            session.round_limit(),
            session.total_questions_asked,
            session.question_limit(),
            workflow_id
        );

        self.handle_guardrail_breach(workflow_id, session, breach)
            .await?;
        Ok(Some(breach))
    }

    /// Run one clarification-guardrail sweep over all currently-paused
    /// workflows, calling [`Self::enforce_guardrails`] on each so the existing
    /// 600s `TimedOut` breach path actually fires for a stale clarification
    /// batch (#23).
    ///
    /// `enforce_guardrails` was previously only invoked on active user paths
    /// (asking/answering), so a partially-answered batch with one undelivered
    /// question deferred forever (`LightSlotUpdate`) with nothing ever
    /// re-checking the timeout. This pass — driven by
    /// [`spawn_clarification_guardrail_sweep`] on a ~60s interval — closes that
    /// gap. Returns the number of workflows whose guardrails breached this pass.
    pub async fn sweep_clarification_guardrails(&self) -> usize {
        let paused = match self.pause_manager.get_paused_workflows().await {
            Ok(paused) => paused,
            Err(err) => {
                warn!(
                    "[ASK-LOOP][GUARDRAIL-SWEEP] failed to list paused workflows: {}",
                    err
                );
                return 0;
            },
        };

        // De-duplicate: a workflow may have multiple queue entries. A terminal
        // breach seals one exact resume and cancels the pending questions, so
        // it must not be prepared twice in one pass.
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut breached = 0usize;
        for entry in paused {
            if !seen.insert(entry.workflow_id.clone()) {
                continue;
            }
            match self.enforce_guardrails(&entry.workflow_id).await {
                Ok(Some(breach)) => {
                    breached += 1;
                    info!(
                        workflow = %entry.workflow_id,
                        ?breach,
                        "[ASK-LOOP][GUARDRAIL-SWEEP] guardrail breached during sweep"
                    );
                },
                Ok(None) => {},
                Err(err) => {
                    warn!(
                        workflow = %entry.workflow_id,
                        "[ASK-LOOP][GUARDRAIL-SWEEP] enforce_guardrails failed: {}",
                        err
                    );
                },
            }
        }
        breached
    }

    /// Clean up answered question from turn metadata (recommended_questions and pending_clarification).
    /// This prevents stale questions from appearing on UI refresh.
    async fn cleanup_answered_question_metadata(
        &self,
        workflow_id: &str,
        question_id: &str,
    ) -> Result<(), AskLoopError> {
        // Find the turn containing this question
        let turns = self
            .conversation_store
            .get_turns(workflow_id)
            .await
            .map_err(|e| AskLoopError::StateUnavailable(format!("Failed to load turns: {}", e)))?;

        debug!(
            "[ASK-LOOP] Cleanup: Found {} turns for workflow {}",
            turns.len(),
            workflow_id
        );

        // Find the most recent turn with recommended questions
        let turn_opt = turns.iter().rev().find(|t| {
            let has_questions = t
                .recommended_questions
                .as_ref()
                .is_some_and(|q| !q.is_empty());
            if has_questions {
                debug!(
                    "[ASK-LOOP] Cleanup: Turn {} has {} recommended questions",
                    t.id,
                    t.recommended_questions.as_ref().unwrap().len()
                );
            }
            has_questions
        });

        if let Some(turn) = turn_opt {
            debug!(
                "[ASK-LOOP] Cleanup: Processing turn {} for question {}",
                turn.id, question_id
            );
            let mut updated_turn = turn.clone();
            let mut updated_metadata = false;

            // Remove from recommended_questions
            let mut filtered = updated_turn
                .recommended_questions
                .clone()
                .unwrap_or_default();
            let before_len = filtered.len();
            filtered.retain(|question| question.id != question_id);
            let after_len = filtered.len();

            if before_len != after_len {
                updated_metadata = true;
                debug!(
                    "[ASK-LOOP] Cleanup: Removed question {} from recommended_questions ({} -> {})",
                    question_id, before_len, after_len
                );
                updated_turn.recommended_questions = if filtered.is_empty() {
                    None
                } else {
                    Some(filtered.clone())
                };
            } else {
                debug!(
                    "[ASK-LOOP] Cleanup: Question {} not found in recommended_questions (had {} questions)",
                    question_id, before_len
                );
            }

            // Phase 5: pending_clarification cleanup removed - now tracked in ClarificationSession

            // Save if anything changed
            if updated_metadata {
                debug!(
                    "[ASK-LOOP] Cleanup: Persisting updated turn {} after removing question {}",
                    updated_turn.id, question_id
                );
                if let Some(processing_metadata) = updated_turn.processing_metadata.clone() {
                    self.conversation_store
                        .store_strategy_attempts(
                            workflow_id,
                            &updated_turn.id,
                            updated_turn.strategy_attempts.clone(),
                            processing_metadata,
                            updated_turn.recommended_questions.clone(),
                        )
                        .await
                        .map_err(|e| {
                            AskLoopError::StateUnavailable(format!(
                                "Failed to persist clarification cleanup: {}",
                                e
                            ))
                        })?;
                } else {
                    warn!(
                        "[ASK-LOOP] Missing processing metadata while cleaning answered clarification for {} ({}); skipping persistence",
                        workflow_id, question_id
                    );
                }
            } else {
                debug!(
                    "[ASK-LOOP] Cleanup: No metadata changes needed for question {}",
                    question_id
                );
            }
        } else {
            debug!(
                "[ASK-LOOP] Cleanup: No turn found with recommended_questions for workflow {}",
                workflow_id
            );
        }

        Ok(())
    }

    fn map_session_error(err: ClarificationSessionStoreError) -> AskLoopError {
        match err {
            ClarificationSessionStoreError::Storage(storage) => AskLoopError::Storage(storage),
            other => AskLoopError::StateUnavailable(other.to_string()),
        }
    }

    async fn handle_guardrail_breach(
        &self,
        workflow_id: &str,
        session: ClarificationSession,
        breach: GuardrailBreach,
    ) -> Result<(), AskLoopError> {
        let pending_ids = session.outstanding_question_ids();
        let alert_type = match breach {
            GuardrailBreach::TimedOut => "clarification_timeout",
            GuardrailBreach::QuestionLimit => "clarification_question_cap",
            GuardrailBreach::RoundLimit => "clarification_round_cap",
        };
        let alert_details = json!({
            "pending_question_ids": pending_ids,
            "round_count": session.round_count,
            "max_rounds": session.round_limit(),
            "total_questions_asked": session.total_questions_asked,
            "question_cap": session.question_limit(),
            "state": session.state,
        });
        self.resume_service
            .emit_observability_alert(workflow_id, alert_type, alert_details);
        match breach {
            GuardrailBreach::TimedOut => self.metrics.record_guardrail_timeout(),
            GuardrailBreach::QuestionLimit => self.metrics.record_guardrail_question_cap(),
            GuardrailBreach::RoundLimit => self.metrics.record_guardrail_round_cap(),
        }

        match breach {
            GuardrailBreach::QuestionLimit | GuardrailBreach::TimedOut => {
                self.resume_service
                    .resume_after_guardrail(workflow_id, breach, pending_ids)
                    .await?;
            },
            GuardrailBreach::RoundLimit => {
                info!(
                    workflow = %workflow_id,
                    round_count = session.round_count,
                    max_rounds = session.max_rounds,
                    "round limit reached; allowing current questions to complete but blocking new rounds"
                );
            },
        }

        Ok(())
    }

    pub async fn ask_clarification(
        &self,
        workflow_id: &str,
        request: ClarifyRequest,
    ) -> Result<ClarifyResponse, AskLoopError> {
        let telemetry_principal = request
            .context
            .as_ref()
            .and_then(|context| context.get("principal"))
            .cloned();
        let telemetry_workspace = request
            .context
            .as_ref()
            .and_then(|context| context.get("workspace"))
            .cloned();
        let telemetry_task_id = request.task_id.clone();
        let telemetry_scope = telemetry_principal
            .as_ref()
            .zip(telemetry_workspace.as_ref())
            .map(|(principal, workspace)| {
                OperationLlmTelemetryScope::new(principal, workspace).with_attribution(
                    OperationLlmCallAttribution {
                        execution_id: Some(workflow_id.to_string()),
                        task_id: telemetry_task_id.clone(),
                        ..OperationLlmCallAttribution::default()
                    },
                )
            });
        let prompt_identity = request.prompt_identity.clone().or_else(|| {
            request
                .context
                .as_ref()
                .and_then(crate::magician_v2::prompt_identity::parse_prompt_identity_metadata)
        });

        let complexity = request.complexity.unwrap_or(TaskComplexity::Moderate);
        if let Some(breach) = self.enforce_guardrails(workflow_id).await? {
            return Err(AskLoopError::StateUnavailable(format!(
                "clarification guardrail {:?} is active",
                breach
            )));
        }
        let round_limit = round_limit_for_complexity(complexity);
        self.session_manager
            .ensure_round_limit(workflow_id, round_limit)
            .await
            .map_err(Self::map_session_error)?;
        let _ = self.ledger.initialize(workflow_id, complexity).await?;

        // Get ask decision - execution stage bypasses confidence checks in should_ask
        // (handled in BudgetPolicy), so we just check the decision directly
        let decision = self.ledger.evaluate_ask_decision(workflow_id).await?;

        if !decision.should_ask {
            return Err(AskLoopError::AskNotRecommended(decision.reason));
        }

        let state = self.state_tracker.latest_state(workflow_id).await?;
        let mut composed =
            self.compose_context(workflow_id, state.as_ref(), request.context.clone());
        let stage = composed.context.stage_context;

        // Apply progressive elicitation filtering (try to infer/discover before asking user)
        let filtered_unresolved = self
            .filter_with_progressive_elicitation(workflow_id, &composed.unresolved_slots)
            .await;

        // Find the first slot we haven't asked about yet
        // We check them one at a time to avoid adding all to history
        let mut new_unresolved = Vec::new();
        for slot in &filtered_unresolved {
            let result = self
                .history
                .filter_new_slots(workflow_id, stage, &[slot.clone()]);
            if !result.is_empty() {
                // This slot is new - we'll ask about it
                new_unresolved.push(slot.clone());
                break; // Only take the FIRST new slot
            }
        }

        if !new_unresolved.is_empty() {
            composed.context.slot_graph.extend(new_unresolved.clone());
        }

        if composed.context.question_hint.is_none() {
            if let Some(slot) = new_unresolved.first() {
                composed.context.question_hint = Some(build_slot_question_hint(slot));
            }
        }

        let mut question = match self
            .clarifier
            .generate_question_with_telemetry(
                request.blocker_type,
                &composed.context,
                telemetry_scope.as_ref(),
            )
            .await
        {
            Ok(question) => question,
            Err(err) => {
                // Clean up - mark as not asked since we didn't actually ask
                if !new_unresolved.is_empty() {
                    self.history.resolve(workflow_id, stage, &new_unresolved);
                }
                return Err(err.into());
            },
        };

        // Populate source_slot_id from the first unresolved slot if present
        if question.source_slot_id.is_none() {
            if let Some(slot) = new_unresolved.first() {
                question.source_slot_id = Some(slot.id.clone());
            }
        }

        self.ledger
            .spend(
                workflow_id,
                decision.estimated_cost,
                format!("clarifier ask via {}", decision.recommended_channel),
            )
            .await?;

        self.pause_manager
            .pause(workflow_id, PauseReason::WaitingOnUser(question.id.clone()))
            .await?;

        self.resume_service.emit_clarification_enqueued(
            workflow_id,
            &question.id,
            &question.blocker_type,
            &question.channel,
            &question.stage,
            question.urgency,
            request.task_id.as_deref(),
            Some(question.question_text.as_str()),
            question.source_slot_id.as_deref(),
        );

        let response = ClarifyResponse {
            question_id: question.id.clone(),
            question_text: question.question_text.clone(),
            context_snippets: question.context_snippets.clone(),
            urgency: question.urgency,
            channel: decision.recommended_channel,
            stage: question.stage,
            source_slot_id: question.source_slot_id.clone(),
            options: question.options.clone(),
            batch_id: question.batch_id.clone(),
            batch_total: question.batch_total,
            slot_confidence: question.slot_confidence,
            related_slots: question.related_slots.clone(),
        };

        self.session_manager
            .append_question(workflow_id, question.clone())
            .await
            .map_err(|err| {
                warn!(
                    workflow = %workflow_id,
                    question_id = %question.id,
                    "session manager failed to append question: {}",
                    err
                );
                Self::map_session_error(err)
            })?;

        if let Err(err) = self
            .session_manager
            .mark_state(workflow_id, ClarificationSessionState::CollectingAnswers)
            .await
        {
            warn!(
                workflow = %workflow_id,
                "session manager failed to mark state CollectingAnswers: {}",
                err
            );
        }

        self.persist_prompt_identity_metadata(workflow_id, prompt_identity.as_ref())
            .await;
        self.persist_telemetry_scope_metadata(
            workflow_id,
            telemetry_principal.as_deref(),
            telemetry_workspace.as_deref(),
            telemetry_task_id.as_deref(),
        )
        .await;

        self.emit_session_snapshot(workflow_id).await;

        Ok(response)
    }

    pub async fn submit_clarification(
        &self,
        workflow_id: &str,
        question_id: &str,
        submission: ClarificationSubmission,
    ) -> Result<ClarificationResult, AskLoopError> {
        let slots = self
            .resume_service
            .on_clarification_received(
                question_id,
                &submission.response_text,
                submission.prompt_identity.as_ref(),
                Some(workflow_id),
            )
            .await?;

        // Mark answered slots as resolved in the history
        // Get the stage from the latest state to resolve the correct history entry
        if !slots.is_empty() {
            if let Ok(Some(state)) = self.state_tracker.latest_state(workflow_id).await {
                self.history
                    .resolve(workflow_id, state.stage_context, &slots);
            }
        }

        // Phase 4: Clean up answered question from turn metadata immediately
        // This prevents stale questions from appearing on UI refresh
        // Note: Cleanup runs in background to avoid blocking the response
        if let Err(err) = self
            .cleanup_answered_question_metadata(workflow_id, question_id)
            .await
        {
            warn!(
                "[ASK-LOOP] Failed to cleanup answered question {} for {}: {}",
                question_id, workflow_id, err
            );
        }

        // Get latest state to check if workflow resumed
        // If state is not found, assume workflow is still paused (return false for workflow_resumed)
        let workflow_resumed = match self.state_tracker.latest_state(workflow_id).await {
            Ok(Some(state)) => {
                state.current_state != crate::magician_v2::state_tracker::WorkflowState::Pause
            },
            Ok(None) => {
                warn!(
                    "[ASK-LOOP] No state found for workflow {} after answer submission, assuming paused",
                    workflow_id
                );
                false
            },
            Err(err) => {
                warn!(
                    "[ASK-LOOP] Failed to get state for workflow {} after answer: {}, assuming paused",
                    workflow_id, err
                );
                false
            },
        };

        Ok(ClarificationResult {
            extracted_slots: slots,
            workflow_resumed,
        })
    }

    /// Enqueue a pre-registered batch question for clarification.
    ///
    /// This is used when questions are generated externally (e.g., from elicitation)
    /// Enqueue a batch of clarification questions that have already been curated
    /// and registered. Ensures the workflow is paused once for the entire batch
    /// and that every question is surfaced to the ask loop/UI.
    pub async fn enqueue_batch_questions(
        &self,
        workflow_id: &str,
        questions: Vec<ClarifierQuestion>,
        complexity: TaskComplexity,
    ) -> Result<(), AskLoopError> {
        self.enqueue_batch_questions_with_scope(
            workflow_id,
            questions,
            complexity,
            None,
            None,
            None,
        )
        .await
    }

    pub async fn enqueue_batch_questions_with_scope(
        &self,
        workflow_id: &str,
        questions: Vec<ClarifierQuestion>,
        complexity: TaskComplexity,
        principal: Option<&str>,
        workspace: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<(), AskLoopError> {
        if questions.is_empty() {
            return Err(AskLoopError::StateUnavailable(
                "attempted to enqueue empty clarification batch".to_string(),
            ));
        }

        if let Some(breach) = self.enforce_guardrails(workflow_id).await? {
            return Err(AskLoopError::StateUnavailable(format!(
                "clarification guardrail {:?} is active",
                breach
            )));
        }

        let question_ids: Vec<String> = questions.iter().map(|q| q.id.clone()).collect();

        let round_limit = round_limit_for_complexity(complexity);
        self.session_manager
            .ensure_round_limit(workflow_id, round_limit)
            .await
            .map_err(Self::map_session_error)?;

        // Initialize ledger if needed
        let _ = self.ledger.initialize(workflow_id, complexity).await?;

        // Evaluate cost for budget tracking (but don't block on decision)
        // Elicitation has already decided these questions are necessary
        let decision = self.ledger.evaluate_ask_decision(workflow_id).await?;
        for _ in &questions {
            self.ledger
                .spend(
                    workflow_id,
                    decision.estimated_cost,
                    format!(
                        "elicitation batch question via {}",
                        decision.recommended_channel
                    ),
                )
                .await?;
        }

        // Pause workflow for ALL batch questions (not just this one)
        // This ensures answers to all questions in the batch can be matched
        self.pause_manager
            .pause_for_batch(workflow_id, question_ids.clone())
            .await?;

        for question in questions.into_iter() {
            info!(
                "[ASK-LOOP] Enqueued batch clarification question for workflow {workflow_id}: id={} batch_id={:?} batch_total={:?}",
                question.id,
                question.batch_id,
                question.batch_total
            );

            // Phase H8.2 — canonical `HitlRequested` emit deliberately
            // suppressed here. `enqueue_batch_questions` is only called
            // from `v2_orchestrator::process_with_strategy`'s V3
            // planning-elicitation branches (lines 4712 and 5811 in
            // v2_orchestrator.rs), both gated behind V3 planning
            // suspension; V3 planning is itself only entered via
            // `artifact_v2/service.rs::refresh_task_plan_from_planning_execution`,
            // which always runs `apply_task_plan_record` afterward —
            // and that path emits the canonical envelope through
            // `emit_v3_planning_clarification_needed` with richer scope
            // (`task_id`, `agent_id`, `chain_id`). Double-emitting from
            // both sites produced duplicate `hitl.requested` rows in
            // events.jsonl for the same `correlation_id`. The
            // single-question `ask_clarification` path below keeps its
            // own emit because it isn't downstream of
            // `apply_task_plan_record`.

            self.session_manager
                .append_question(workflow_id, question.clone())
                .await
                .map_err(|err| {
                    warn!(
                        workflow = %workflow_id,
                        question_id = %question.id,
                        "session manager failed to append batch question: {}",
                        err
                    );
                    Self::map_session_error(err)
                })?;
        }

        if let Err(err) = self
            .session_manager
            .mark_state(workflow_id, ClarificationSessionState::CollectingAnswers)
            .await
        {
            warn!(
                workflow = %workflow_id,
                "session manager failed to mark state CollectingAnswers: {}",
                err
            );
        }

        self.persist_telemetry_scope_metadata(workflow_id, principal, workspace, task_id)
            .await;

        self.emit_session_snapshot(workflow_id).await;

        Ok(())
    }

    pub async fn get_pending_clarifications(&self) -> Result<Vec<ClarifierQuestion>, AskLoopError> {
        let paused_workflows = self
            .pause_manager
            .get_paused_workflows()
            .await
            .map_err(|err| AskLoopError::StateUnavailable(err.to_string()))?;

        let mut pending = Vec::new();

        for paused in paused_workflows {
            let session = match self.session_manager.load(&paused.workflow_id).await {
                Ok(Some(session)) => session,
                Ok(None) => continue,
                Err(err) => {
                    warn!(
                        "[ASK-LOOP] Failed to load session for {}: {}",
                        paused.workflow_id, err
                    );
                    continue;
                },
            };

            let mut lookup = HashMap::new();
            for question in session.pending_questions.iter() {
                lookup.insert(question.id.clone(), question);
            }

            for question_id in paused.pending_questions.iter() {
                if let Some(question) = lookup.get(question_id) {
                    pending.push(ClarifierQuestion::from(*question));
                }
            }
        }

        Ok(pending)
    }

    pub async fn get_pending_clarifications_for_execution(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<ClarifierQuestion>, AskLoopError> {
        // Load session - if not found (brand new execution), just return empty list
        // Session will be created when first question is enqueued
        let session = match self.session_manager.load(workflow_id).await {
            Ok(Some(session)) => session,
            Ok(None) => {
                debug!(
                    workflow = %workflow_id,
                    "no clarification session found (brand new execution), returning empty list"
                );
                return Ok(Vec::new());
            },
            Err(err) => {
                // Execution not found is expected for tasks that haven't started execution yet
                let level = if err.to_string().contains("Execution not found") {
                    "debug"
                } else {
                    "warn"
                };
                if level == "debug" {
                    debug!(
                        workflow = %workflow_id,
                        "execution not found for clarification session, returning empty list"
                    );
                } else {
                    warn!(
                        workflow = %workflow_id,
                        "failed to load clarification session: {}, returning empty list",
                        err
                    );
                }
                return Ok(Vec::new());
            },
        };

        let mut questions = Vec::new();
        for question in session.pending_questions.iter() {
            if matches!(
                question.status,
                SessionQuestionStatus::WaitingOnUser | SessionQuestionStatus::Queued
            ) {
                questions.push(ClarifierQuestion::from(question));
            }
        }

        Ok(questions)
    }

    /// Get the clarification history for an execution (answered questions)
    pub async fn get_clarification_history_for_execution(
        &self,
        workflow_id: &str,
    ) -> Result<Vec<crate::magician_v2::storage::models::ClarificationHistoryEntry>, AskLoopError>
    {
        match self
            .conversation_store
            .get_clarification_history(workflow_id)
            .await
        {
            Ok(history) => Ok(history),
            Err(e) if e.to_string().contains("Execution not found") => {
                debug!(
                    workflow = %workflow_id,
                    "execution not found, returning empty clarification history"
                );
                Ok(Vec::new())
            },
            Err(e) => Err(AskLoopError::StateUnavailable(format!(
                "Failed to load clarification history: {}",
                e
            ))),
        }
    }

    /// Get the plan for an execution from turn storage (strategy_attempts).
    ///
    /// This fetches the plan from the single source of truth - the turn storage
    /// where strategy attempts are persisted. The plan is stored in
    /// `turn.strategy_attempts[last].exploration_result.plan`.
    ///
    /// Returns `None` if:
    /// - No turns exist for the execution
    /// - No strategy attempts exist with a plan
    /// - The plan is empty (no steps)
    pub async fn get_plan_for_execution(
        &self,
        execution_id: &str,
    ) -> Result<Option<PlanGraph>, AskLoopError> {
        let turns = self
            .conversation_store
            .get_turns(execution_id)
            .await
            .map_err(|e| {
                AskLoopError::StateUnavailable(format!("Failed to load turns for plan: {}", e))
            })?;

        // Search turns in reverse order (most recent first) for a valid plan
        for turn in turns.iter().rev() {
            // Skip turns without strategy attempts
            if turn.strategy_attempts.is_empty() {
                continue;
            }

            // Look at the last (most recent) strategy attempt first
            // It's typically the successful one after escalation
            for attempt in turn.strategy_attempts.iter().rev() {
                if let Some(plan) = &attempt.exploration_result.plan {
                    // Verify plan has steps (not empty)
                    if !plan.steps.is_empty() {
                        debug!(
                            "[ASK-LOOP] Found plan in turn {} strategy attempt {} with {} steps",
                            turn.id,
                            attempt.attempt_number,
                            plan.steps.len()
                        );
                        return Ok(Some(plan.clone()));
                    }
                }
            }
        }

        debug!(
            "[ASK-LOOP] No valid plan found in any turn for execution {}",
            execution_id
        );
        Ok(None)
    }

    pub async fn manual_resume(
        &self,
        workflow_id: &str,
        request: ManualResumeRequest,
    ) -> Result<ManualResumeResponse, AskLoopError> {
        let slots = request
            .slots
            .unwrap_or_default()
            .into_iter()
            .map(|slot| slot.into_record())
            .collect::<Result<Vec<_>, AskLoopError>>()?;

        let options = ManualResumeOptions {
            slots,
            updated_confidence: request.updated_confidence,
            handled_question: request.handled_question.clone(),
        };

        self.resume_service
            .on_manual_resume(workflow_id, options)
            .await?;

        Ok(ManualResumeResponse {
            workflow_resumed: true,
        })
    }

    // NOTE: recovery_resume() method removed - agentic execution handles state recovery
    // via the observe-decide-execute loop. See execution/agentic/ module.

    fn compose_context(
        &self,
        workflow_id: &str,
        latest_state: Option<&crate::magician_v2::state_tracker::StateBundle>,
        supplemental: Option<HashMap<String, String>>,
    ) -> ComposeOutcome {
        let mut context = WorkflowContext::empty(workflow_id);
        let mut unresolved_records = Vec::new();

        if let Some(state) = latest_state {
            context.stage_context = state.stage_context;
            context.recent_observations = state.observations.clone();
            context.confidence_scores = state.confidence.per_slot.clone();

            if let Some(summary) = state.confidence.summary.as_ref() {
                let unresolved_ids = summary.unresolved_slots.clone();
                self.history
                    .prune_resolved(workflow_id, context.stage_context, &unresolved_ids);
                unresolved_records = unresolved_ids
                    .into_iter()
                    .filter_map(|id| state.confidence.slot_records.get(&id).cloned())
                    .collect();
            } else {
                self.history
                    .prune_resolved(workflow_id, context.stage_context, &[]);
            }
        } else {
            self.history
                .prune_resolved(workflow_id, context.stage_context, &[]);
        }

        if let Some(additional) = supplemental {
            for (key, value) in additional {
                let handled_as_text = match key.as_str() {
                    "user_query" => {
                        Self::push_text_observation(
                            &mut context,
                            "Original request",
                            value.clone(),
                            AssetType::UserMessage,
                        );
                        true
                    },
                    "clarified_task" => {
                        Self::push_text_observation(
                            &mut context,
                            "Clarified plan",
                            value.clone(),
                            AssetType::ToolOutput,
                        );
                        true
                    },
                    "rewrite_open_questions" => {
                        if context.question_hint.is_none() {
                            if let Some(first) =
                                value.split('|').map(str::trim).find(|s| !s.is_empty())
                            {
                                context.question_hint = Some(first.to_string());
                            }
                        }
                        Self::push_text_observation(
                            &mut context,
                            "Outstanding questions",
                            value.clone(),
                            AssetType::ToolOutput,
                        );
                        true
                    },
                    "rewrite_constraints" => {
                        Self::push_text_observation(
                            &mut context,
                            "Constraints",
                            value.clone(),
                            AssetType::ToolOutput,
                        );
                        true
                    },
                    "question_text" | "question_hint" => {
                        if value.trim().is_empty() {
                            context.question_hint = None;
                        } else {
                            context.question_hint = Some(value.trim().to_string());
                        }
                        true
                    },
                    "slot_summary" => {
                        Self::push_text_observation(
                            &mut context,
                            "Slot summary",
                            value.clone(),
                            AssetType::ToolOutput,
                        );
                        true
                    },
                    _ => false,
                };

                if handled_as_text {
                    continue;
                }

                Self::push_text_observation(
                    &mut context,
                    key.as_str(),
                    value.clone(),
                    AssetType::ToolOutput,
                );
            }
        }

        if context.question_hint.is_none() {
            if let Some(slot) = unresolved_records.first() {
                context.question_hint = Some(build_slot_question_hint(slot));
            }
        }

        ComposeOutcome {
            context,
            unresolved_slots: unresolved_records,
        }
    }

    /// Filter unresolved slots using progressive elicitation
    ///
    /// **Architectural Note:** Progressive elicitation filtering primarily occurs
    /// during the planning phase in `atomic_composition.rs` via LLM-based priority
    /// classification. This method serves as a just-in-time safety net for runtime
    /// elicitation if needed in future enhancements.
    ///
    /// **Current Behavior:** Returns all slots unchanged, as filtering has already
    /// occurred during plan generation. The elicitation_manager is available for
    /// future runtime filtering enhancements.
    ///
    /// **Primary Filtering Location:** See `atomic_composition.rs::classify_slot_priority_with_llm()`
    /// where LLM-based classification determines which parameters to ask, infer, or discover.
    ///
    /// Returns: Filtered list of slots (currently unfiltered - filtering happens during planning)
    async fn filter_with_progressive_elicitation(
        &self,
        workflow_id: &str,
        unresolved_slots: &[SlotRecord],
    ) -> Vec<SlotRecord> {
        // Infrastructure in place for future runtime filtering
        // Current filtering happens in atomic_composition.rs during planning

        if let Some(_manager) = self.elicitation_manager.as_ref() {
            tracing::debug!(
                "[MAGICIAN-V2-ASK-LOOP] Progressive elicitation available for workflow {}, {} unresolved slots",
                workflow_id,
                unresolved_slots.len()
            );
        }

        unresolved_slots.to_vec()
    }

    async fn emit_session_snapshot(&self, workflow_id: &str) {
        match self.session_manager.load(workflow_id).await {
            Ok(Some(session)) => {
                self.resume_service
                    .emit_session_snapshot(workflow_id, &session);
            },
            Ok(None) => {},
            Err(err) => {
                warn!(
                    workflow = %workflow_id,
                    "failed to load session while emitting snapshot: {}",
                    err
                );
            },
        }
    }

    async fn persist_prompt_identity_metadata(
        &self,
        workflow_id: &str,
        prompt_identity: Option<&PromptIdentityContext>,
    ) {
        let Some(prompt_identity) = prompt_identity else {
            return;
        };

        let raw_identity = match serde_json::to_string(prompt_identity) {
            Ok(raw) => raw,
            Err(err) => {
                warn!(
                    workflow = %workflow_id,
                    "failed to serialize prompt identity for clarification session: {}",
                    err
                );
                return;
            },
        };

        let mut session = match self.session_manager.load(workflow_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return,
            Err(err) => {
                warn!(
                    workflow = %workflow_id,
                    "failed to load clarification session for prompt identity persistence: {}",
                    err
                );
                return;
            },
        };

        let mut metadata = session.metadata.as_object().cloned().unwrap_or_default();
        metadata.insert(
            "agent:prompt_identity".to_string(),
            serde_json::Value::String(raw_identity),
        );
        session.metadata = serde_json::Value::Object(metadata);

        if let Err(err) = self.session_manager.save(session).await {
            warn!(
                workflow = %workflow_id,
                "failed to persist prompt identity metadata for clarification session: {}",
                err
            );
        }
    }

    async fn persist_telemetry_scope_metadata(
        &self,
        workflow_id: &str,
        principal: Option<&str>,
        workspace: Option<&str>,
        task_id: Option<&str>,
    ) {
        let principal = principal.map(str::trim).filter(|value| !value.is_empty());
        let workspace = workspace.map(str::trim).filter(|value| !value.is_empty());
        let task_id = task_id.map(str::trim).filter(|value| !value.is_empty());
        if principal.is_none() && workspace.is_none() && task_id.is_none() {
            return;
        }

        let mut session = match self.session_manager.load(workflow_id).await {
            Ok(Some(session)) => session,
            Ok(None) => return,
            Err(err) => {
                warn!(
                    workflow = %workflow_id,
                    "failed to load clarification session for telemetry scope persistence: {}",
                    err
                );
                return;
            },
        };

        let mut metadata = session.metadata.as_object().cloned().unwrap_or_default();
        if let Some(principal) = principal {
            metadata.insert(
                "telemetry:principal".to_string(),
                serde_json::Value::String(principal.to_string()),
            );
        }
        if let Some(workspace) = workspace {
            metadata.insert(
                "telemetry:workspace".to_string(),
                serde_json::Value::String(workspace.to_string()),
            );
        }
        if let Some(task_id) = task_id {
            metadata.insert(
                "telemetry:task_id".to_string(),
                serde_json::Value::String(task_id.to_string()),
            );
        }
        session.metadata = serde_json::Value::Object(metadata);

        if let Err(err) = self.session_manager.save(session).await {
            warn!(
                workflow = %workflow_id,
                "failed to persist telemetry scope for clarification session: {}",
                err
            );
        }
    }

    fn push_text_observation(
        context: &mut WorkflowContext,
        label: &str,
        value: String,
        asset_type: AssetType,
    ) {
        let normalized = normalize_observation_value(&value);
        if normalized.is_empty() {
            return;
        }

        let mut metadata = HashMap::new();
        metadata.insert("label".to_string(), label.to_string());
        metadata.insert(
            "source".to_string(),
            "orchestrator_supplemental".to_string(),
        );

        context.recent_observations.push(ObservationAsset {
            asset_id: format!("supplemental-{}-{}", label, Utc::now().timestamp_millis()),
            asset_type,
            content: AssetContent::Text(normalized),
            metadata,
        });
    }
}

fn normalize_observation_value(raw: &str) -> String {
    let collapsed = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let base = if collapsed.is_empty() {
        raw.trim().to_string()
    } else {
        collapsed
    };

    const MAX_LEN: usize = 400;
    if base.chars().count() > MAX_LEN {
        base.chars().take(MAX_LEN).collect::<String>() + "…"
    } else {
        base
    }
}

fn round_limit_for_complexity(complexity: TaskComplexity) -> usize {
    match complexity {
        TaskComplexity::Complex => 30,
        TaskComplexity::Moderate | TaskComplexity::Simple => 10,
    }
}

struct ComposeOutcome {
    context: WorkflowContext,
    unresolved_slots: Vec<SlotRecord>,
}

#[derive(Debug, Deserialize)]
pub struct ClarifyRequest {
    pub blocker_type: super::clarifier::BlockerType,
    pub context: Option<HashMap<String, String>>,
    pub complexity: Option<TaskComplexity>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_identity: Option<PromptIdentityContext>,

    /// Batch ID if this question is part of a batch
    pub batch_id: Option<String>,

    /// Total number of questions in the batch
    pub batch_total: Option<usize>,

    /// Phase H4.2 — task_id of the originating task. Passed through to
    /// the canonical `HitlRequested` event so frontend `pendingHitlStore`
    /// and `/events?event_type=HitlRequested` can scope clarifications to
    /// the right task. Optional because today not every caller has the
    /// task_id in scope; in V3 task_id == workflow_id so callers that
    /// can't supply it will see the canonical event with `task_id: None`
    /// (the legacy `execution_id` field still carries the workflow_id).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ClarifyResponse {
    pub question_id: String,
    pub question_text: String,
    pub context_snippets: Vec<String>,
    pub urgency: f64,
    pub channel: Channel,
    pub stage: StageContext,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_slot_id: Option<String>,

    /// Optional predefined choices for questions that require selection (e.g., tool selection)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<crate::magician_v2::ask_loop::clarifier::QuestionOption>>,

    /// Batch ID if this question is part of a batch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_id: Option<String>,

    /// Total number of questions in the batch
    #[serde(skip_serializing_if = "Option::is_none")]
    pub batch_total: Option<usize>,

    /// Optional confidence score for the source slot
    #[serde(skip_serializing_if = "Option::is_none")]
    pub slot_confidence: Option<f32>,

    /// Optional list of related slot identifiers
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub related_slots: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClarificationSubmission {
    pub response_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_identity: Option<PromptIdentityContext>,
}

#[derive(Debug, Serialize)]
pub struct ClarificationResult {
    pub extracted_slots: Vec<SlotRecord>,
    pub workflow_resumed: bool,
}

#[derive(Debug, Deserialize)]
pub struct ManualResumeRequest {
    pub slots: Option<Vec<ManualResumeSlot>>,
    pub updated_confidence: Option<f64>,
    pub handled_question: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ManualResumeSlot {
    pub slot_id: String,
    pub slot_type: SlotType,
    pub value: serde_json::Value,
    pub confidence: Option<f64>,
}

impl ManualResumeSlot {
    fn into_record(self) -> Result<SlotRecord, AskLoopError> {
        let now = Utc::now();
        Ok(SlotRecord {
            id: self.slot_id,
            slot_type: self.slot_type,
            value: self.value,
            confidence: self.confidence.unwrap_or(0.7).clamp(0.0, 1.0),
            provenance: vec![ProvenanceRecord {
                source: ProvenanceSource::UserReply,
                timestamp: now,
            }],
            evidence_links: Vec::new(),
            created_at: now,
            updated_at: now,
        })
    }
}

#[derive(Debug, Serialize)]
pub struct ManualResumeResponse {
    pub workflow_resumed: bool,
}

// NOTE: RecoveryResumeRequest and RecoveryResumeResponse removed - agentic execution
// handles state recovery via observe-decide-execute loop. See execution/agentic/ module.

#[derive(Debug, Error)]
pub enum AskLoopError {
    #[error("ask not recommended: {0}")]
    AskNotRecommended(String),
    #[error("budget error: {0}")]
    Budget(#[from] BudgetLedgerError),
    #[error("clarifier error: {0}")]
    Clarifier(#[from] super::clarifier::ClarifierError),
    #[error("pause error: {0}")]
    Pause(#[from] super::pause::PauseError),
    #[error("storage error: {0}")]
    Storage(#[from] V2StorageError),
    #[error("trigger error: {0}")]
    Trigger(#[from] super::triggers::TriggerError),
    #[error("state unavailable for workflow '{0}'")]
    StateUnavailable(String),
}

impl From<AskLoopError> for actix_web::Error {
    fn from(err: AskLoopError) -> Self {
        match err {
            AskLoopError::AskNotRecommended(reason) => {
                debug!("[MAGICIAN-V2-ASK] Ask not recommended: {}", reason);
                actix_web::error::ErrorConflict(reason)
            },
            AskLoopError::StateUnavailable(reason) => {
                debug!("[MAGICIAN-V2-ASK] State unavailable: {}", reason);
                actix_web::error::ErrorNotFound(reason)
            },
            other => {
                error!("[MAGICIAN-V2-ASK] API error: {:?}", other);
                actix_web::error::ErrorInternalServerError(other.to_string())
            },
        }
    }
}

/// Handler: POST /api/magician/v2/executions/{execution_id}/clarify/{question_id}/respond
///
/// Phase H8.1 — retired (returns 410 Gone). The fifth and final legacy
/// HITL resolve URL. The canonical
/// `POST /api/magician/v2/hitl/{correlation_id}/respond` with
/// `source: "clarification"` dispatches to the same underlying
/// `submit_clarification` API method, so the canonical path is intact.
/// Counter still records hits so we can identify remaining direct
/// callers.
pub async fn submit_clarification_handler(
    _api: web::Data<AskLoopApi>,
    _path: web::Path<(String, String)>,
    _req: web::Json<ClarificationSubmission>,
) -> Result<HttpResponse> {
    crate::magician_v2::hitl_deprecation_metrics::record_hit(
        "/api/magician/v2/executions/{execution_id}/clarify/{question_id}/respond",
    );
    Ok(HttpResponse::Gone().json(serde_json::json!({
        "error": "endpoint_retired",
        "message": "POST /api/magician/v2/hitl/{correlation_id}/respond with body { source: \"clarification\", value, channel, task_id, execution_id? }",
        "retired_in": "magician v0.6.506 (Phase H8.1)",
    })))
}

// NOTE: recovery_resume_handler removed - agentic execution handles state recovery
// via observe-decide-execute loop. See execution/agentic/ module.

/// Default interval for the clarification-guardrail sweep.
pub const CLARIFICATION_GUARDRAIL_SWEEP_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(60);

/// Spawn the background clarification-guardrail sweep (#23).
///
/// The existing 600s `TimedOut` guardrail only fired on active user paths, so a
/// partially-answered / stuck clarification batch could pin a plan in
/// `CollectingAnswers` forever. This spawns one lightweight `tokio` interval
/// task (mirroring `feed_stall_watchdog::spawn_watchdog`) that periodically
/// calls [`AskLoopApi::sweep_clarification_guardrails`] so the timeout actually
/// gets a chance to breach and drive `cancel_questions` → `ReadyToPlan` →
/// resume. Additive and fail-open: a sweep error only logs; nothing breaks.
///
/// Returns a `JoinHandle` intended to be held for the process lifetime.
pub fn spawn_clarification_guardrail_sweep(
    api: Arc<AskLoopApi>,
    interval: std::time::Duration,
    shutdown: tokio_util::sync::CancellationToken,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.tick().await; // skip the immediate first tick
        loop {
            tokio::select! {
                _ = shutdown.cancelled() => return,
                _ = tick.tick() => {},
            }
            let breached = api.sweep_clarification_guardrails().await;
            if breached > 0 {
                debug!(
                    "[ASK-LOOP][GUARDRAIL-SWEEP] pass complete: {} workflow(s) breached",
                    breached
                );
            }
        }
    })
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::Weak;
    use tokio::sync::Mutex;

    use crate::magician_v2::{
        ask_loop::{
            budget::{BudgetConfig, BudgetPolicy},
            clarifier::{ClarifierLibrary, DeterministicClarifier},
            pause::{InMemoryQueueRepository, PauseReason, QueueRepository, WaitingQueue},
            session_manager::SessionManager,
            BlockerType, ClarificationHistory, ClarificationSession,
            ClarificationSessionStoreError, ResumeNotification, ResumePreparation,
        },
        confidence::{ConfidenceService, ConfidenceSummary},
        execution::{PromptAgentKind, PromptIdentityContext},
        slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType},
        state_tracker::{BudgetState, ConfidenceScore, StageContext, StateBundle, WorkflowState},
        storage::V2StorageError,
    };
    use runtime_core::V2ConversationStore as CoreV2ConversationStore;

    struct TestHarness {
        pub api: AskLoopApi,
        pub history: Arc<ClarificationHistory>,
    }

    struct TestResumeOwner {
        service: Weak<ResumeTriggerService>,
    }

    #[async_trait]
    impl ResumeListener for TestResumeOwner {
        async fn prepare_resume(
            &self,
            preparation: ResumePreparation,
        ) -> Result<Option<String>, String> {
            Ok(Some(format!(
                "test_resume_{}",
                preparation.notification.resumed_state.state_id
            )))
        }

        async fn commit_prepared_resume(
            &self,
            preparation: &ResumePreparation,
            _recovery_id: &str,
        ) -> Result<(), String> {
            let service = self
                .service
                .upgrade()
                .ok_or_else(|| "test resume service unavailable".to_string())?;
            service
                .commit_prepared_resume_mutation(&preparation.mutation)
                .await
                .map(|_| ())
                .map_err(|error| error.to_string())
        }

        async fn on_resume(&self, _notification: ResumeNotification) -> Result<(), String> {
            Ok(())
        }
    }

    impl TestHarness {
        fn new() -> Self {
            Self::with_session_manager(Arc::new(RecordingSessionManager::new()))
        }

        fn with_session_manager(session_manager: Arc<dyn SessionManager>) -> Self {
            let history = Arc::new(ClarificationHistory::new());
            let clarifier = Arc::new(ClarifierLibrary::with_default_templates(Arc::new(
                DeterministicClarifier,
            )));

            let (tracker, confidence_service, conversation_store) = test_state_tracker();
            let policy = Arc::new(BudgetPolicy::with_confidence_service(
                BudgetConfig::default(),
                confidence_service,
            ));
            let ledger = Arc::new(BudgetLedger::new(tracker.clone(), policy));
            let queue_repo: Arc<dyn QueueRepository> = Arc::new(InMemoryQueueRepository::default());
            let waiting_queue = Arc::new(WaitingQueue::new(queue_repo));
            let pause_manager = Arc::new(PauseResumeManager::new(tracker.clone(), waiting_queue));
            let resume_service = Arc::new(ResumeTriggerService::new(
                pause_manager.clone(),
                clarifier.clone(),
                Some(ledger.clone()),
                None,
                history.clone(),
            ));
            resume_service.set_resume_listener(Some(Arc::new(TestResumeOwner {
                service: Arc::downgrade(&resume_service),
            })));
            let batch_tracker = Arc::new(QuestionBatchTracker::new());
            let confidence_tracker = Arc::new(PlanConfidenceTracker::with_defaults());
            let metrics = Arc::new(ClarificationMetrics::new());

            // Create a test query rewriter with deterministic model
            use crate::magician_v2::prompts::PromptManager;
            use crate::magician_v2::prompts::{json_storage::JsonStorageConfig, PromptStore};
            use crate::magician_v2::slot_graph::{
                adapters::DeterministicRewriteModel,
                rewriter::{QuestionRewriter, RewriteModel, RewriterConfig},
            };
            use crate::magician_v2::JsonPromptStorage;

            let data_dir = crate::magician_v2::prompts::json_storage::default_prompt_dir();
            let storage_config = JsonStorageConfig {
                storage_dir: data_dir,
                enable_cache: true,
                max_cache_entries: 50,
            };
            let prompt_storage: Arc<dyn PromptStore> = Arc::new(
                JsonPromptStorage::new(storage_config).expect("Failed to create prompt storage"),
            );
            let prompt_manager = Arc::new(PromptManager::new(Arc::clone(&prompt_storage)));
            let rewrite_model: Arc<dyn RewriteModel> = Arc::new(DeterministicRewriteModel);
            let query_rewriter = Arc::new(QuestionRewriter::new(
                rewrite_model,
                prompt_manager,
                RewriterConfig::default(),
            ));

            let api = AskLoopApi::new(
                clarifier,
                pause_manager,
                ledger,
                resume_service,
                history.clone(),
                batch_tracker,
                confidence_tracker,
                query_rewriter,
                metrics,
                session_manager,
                conversation_store,
            );

            Self { api, history }
        }

        fn state_with_unresolved(
            &self,
            workflow_id: &str,
            stage: StageContext,
            unresolved: &[&str],
        ) -> StateBundle {
            let mut confidence = ConfidenceScore::default();
            confidence.summary = Some(ConfidenceSummary {
                overall: 0.4,
                min_critical_slot: 0.0,
                unresolved_slots: unresolved.iter().map(|id| id.to_string()).collect(),
            });

            for slot_id in unresolved {
                let slot = Self::make_slot(slot_id);
                confidence
                    .slot_records
                    .insert(slot.id.clone(), slot.clone());
                confidence.per_slot.insert(slot.id.clone(), slot.confidence);
            }

            StateBundle {
                state_id: "state".to_string(),
                workflow_id: workflow_id.to_string(),
                current_state: WorkflowState::Clarify,
                llm_reasoning: None,
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence,
                budget: BudgetState::default(),
                stage_context: stage,
                completed_stages: Vec::new(),
                failed_stage: None,
                created_at: Utc::now(),
            }
        }

        fn make_slot(id: &str) -> SlotRecord {
            SlotRecord {
                id: id.to_string(),
                slot_type: SlotType::Modifier,
                value: serde_json::Value::String("pending".to_string()),
                confidence: 0.2,
                provenance: vec![ProvenanceRecord {
                    source: ProvenanceSource::MemoryLookup,
                    timestamp: Utc::now(),
                }],
                evidence_links: Vec::new(),
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }
        }
    }

    fn test_state_tracker() -> (
        Arc<StateTracker>,
        Arc<ConfidenceService>,
        Arc<dyn V2ConversationStore>,
    ) {
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
                Ok(Vec::new())
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
                Ok(())
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
                // This ask-loop fixture has no execution table; fail closed
                // instead of pretending an atomic transition occurred.
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
                let mut guard = self.states.lock().await;
                guard
                    .entry(execution_id.to_string())
                    .or_default()
                    .push(state);
                Ok(())
            }

            async fn get_states(
                &self,
                execution_id: &str,
            ) -> Result<Vec<StateBundle>, V2StorageError> {
                let guard = self.states.lock().await;
                Ok(guard.get(execution_id).cloned().unwrap_or_default())
            }

            async fn get_latest_state(
                &self,
                execution_id: &str,
            ) -> Result<Option<StateBundle>, V2StorageError> {
                let guard = self.states.lock().await;
                Ok(guard
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

        let store = Arc::new(InMemoryStore::default());
        let confidence_service = Arc::new(ConfidenceService::default());
        let tracker = Arc::new(StateTracker::with_confidence_service(
            store.clone(),
            confidence_service.clone(),
        ));
        (
            tracker,
            confidence_service,
            store as Arc<dyn V2ConversationStore>,
        )
    }

    #[derive(Default, Clone)]
    struct CallLog {
        append_calls: usize,
        append_question_ids: Vec<String>,
        mark_answered_calls: usize,
        mark_answered_ids: Vec<String>,
        mark_answered_slot_counts: Vec<usize>,
        mark_answered_slot_ids: Vec<Vec<String>>,
        recorded_states: Vec<ClarificationSessionState>,
        cancel_calls: usize,
        cancelled_question_ids: Vec<String>,
    }

    struct RecordingSessionManager {
        calls: Mutex<CallLog>,
        sessions: Mutex<HashMap<String, ClarificationSession>>,
    }

    impl RecordingSessionManager {
        fn new() -> Self {
            Self {
                calls: Mutex::new(CallLog::default()),
                sessions: Mutex::new(HashMap::new()),
            }
        }

        fn with_session(session: ClarificationSession) -> Self {
            let mut sessions = HashMap::new();
            sessions.insert(session.workflow_id.clone(), session);
            Self {
                calls: Mutex::new(CallLog::default()),
                sessions: Mutex::new(sessions),
            }
        }

        async fn snapshot(&self) -> CallLog {
            self.calls.lock().await.clone()
        }
    }

    #[async_trait]
    impl SessionManager for RecordingSessionManager {
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
            self.sessions
                .lock()
                .await
                .insert(session.workflow_id.clone(), session);
            Ok(())
        }

        async fn delete(&self, workflow_id: &str) -> Result<(), ClarificationSessionStoreError> {
            self.sessions.lock().await.remove(workflow_id);
            Ok(())
        }

        async fn append_question(
            &self,
            workflow_id: &str,
            question: ClarifierQuestion,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut calls = self.calls.lock().await;
            calls.append_calls += 1;
            calls.append_question_ids.push(question.id.clone());
            drop(calls); // Release lock before acquiring sessions lock

            // Actually add the question to the session (create session if it doesn't exist)
            let mut sessions = self.sessions.lock().await;
            use crate::magician_v2::ask_loop::session::{ClarificationSession, SessionQuestion};
            let session = sessions
                .entry(workflow_id.to_string())
                .or_insert_with(|| ClarificationSession::new(workflow_id));
            session
                .pending_questions
                .push_back(SessionQuestion::from(&question));
            Ok(())
        }

        async fn mark_question_answered(
            &self,
            _workflow_id: &str,
            question_id: &str,
            slots: Vec<SlotRecord>,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut calls = self.calls.lock().await;
            calls.mark_answered_calls += 1;
            calls.mark_answered_ids.push(question_id.to_string());
            calls.mark_answered_slot_counts.push(slots.len());
            calls
                .mark_answered_slot_ids
                .push(slots.into_iter().map(|slot| slot.id).collect());
            Ok(())
        }

        async fn mark_state(
            &self,
            workflow_id: &str,
            state: ClarificationSessionState,
        ) -> Result<(), ClarificationSessionStoreError> {
            {
                let mut sessions = self.sessions.lock().await;
                if let Some(existing) = sessions.get_mut(workflow_id) {
                    match state {
                        ClarificationSessionState::CollectingAnswers => existing.mark_collecting(),
                        ClarificationSessionState::ReadyToPlan => existing.mark_ready_to_plan(),
                        ClarificationSessionState::Planning => existing.mark_planning(),
                    }
                }
            }
            let mut calls = self.calls.lock().await;
            calls.recorded_states.push(state);
            Ok(())
        }

        async fn cancel_questions(
            &self,
            _workflow_id: &str,
            question_ids: &[String],
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut calls = self.calls.lock().await;
            calls.cancel_calls += 1;
            calls
                .cancelled_question_ids
                .extend(question_ids.iter().cloned());
            Ok(())
        }

        async fn mark_questions_handed_off(
            &self,
            _workflow_id: &str,
            question_ids: &[String],
        ) -> Result<usize, ClarificationSessionStoreError> {
            Ok(question_ids.len())
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
            workflow_id: &str,
            limit: usize,
        ) -> Result<(), ClarificationSessionStoreError> {
            let mut sessions = self.sessions.lock().await;
            if let Some(existing) = sessions.get_mut(workflow_id) {
                existing.set_round_limit(limit);
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn ask_clarification_updates_session_manager_when_enabled() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-session-enabled";
        let stage = StageContext::PlanningBootstrap;
        let state = harness.state_with_unresolved(workflow_id, stage, &["slot:beta"]);

        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        let request = ClarifyRequest {
            blocker_type: BlockerType::LowConfidenceSlot,
            context: None,
            complexity: None,
            prompt_identity: None,
            batch_id: None,
            batch_total: None,
            task_id: None,
        };

        let response = harness
            .api
            .ask_clarification(workflow_id, request)
            .await
            .expect("clarify succeeds");

        let calls = session_manager.snapshot().await;
        assert_eq!(calls.append_calls, 1, "question should be recorded once");
        assert_eq!(
            calls.append_question_ids,
            vec![response.question_id.clone()],
            "recorded question IDs should match the emitted question"
        );
        assert_eq!(
            calls.recorded_states,
            vec![ClarificationSessionState::CollectingAnswers],
            "collecting state should be recorded once"
        );
        assert_eq!(
            calls.mark_answered_calls, 0,
            "no answers should be recorded before submission"
        );
    }

    #[tokio::test]
    async fn ask_clarification_persists_prompt_identity_in_session_metadata() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-session-prompt-identity";
        let state = harness.state_with_unresolved(
            workflow_id,
            StageContext::PlanningBootstrap,
            &["slot:identity"],
        );
        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        let identity = PromptIdentityContext {
            agent_kind: Some(PromptAgentKind::User),
            base_persona: Some("Precise".to_string()),
            source_agent_id: Some("agent-api-test".to_string()),
            source_agent_name: Some("Assistant".to_string()),
            source_agent_aliases: Vec::new(),
            source_agent_persona: None,
            autonomous_controls: None,
        };
        let request = ClarifyRequest {
            blocker_type: BlockerType::LowConfidenceSlot,
            context: Some(HashMap::from([
                ("principal".to_string(), "principal-api-test".to_string()),
                ("workspace".to_string(), "workspace-api-test".to_string()),
            ])),
            complexity: None,
            prompt_identity: Some(identity),
            batch_id: None,
            batch_total: None,
            task_id: Some("task-api-test".to_string()),
        };

        harness
            .api
            .ask_clarification(workflow_id, request)
            .await
            .expect("clarification should succeed");

        let session = session_manager
            .load(workflow_id)
            .await
            .expect("session load")
            .expect("session exists");
        let raw_identity = session.metadata["agent:prompt_identity"]
            .as_str()
            .expect("identity metadata string");
        let parsed: PromptIdentityContext =
            serde_json::from_str(raw_identity).expect("parse persisted identity");
        assert_eq!(
            parsed.source_agent_id.as_deref(),
            Some("agent-api-test"),
            "source agent id should persist in session metadata"
        );
        assert_eq!(
            session.metadata["telemetry:principal"].as_str(),
            Some("principal-api-test")
        );
        assert_eq!(
            session.metadata["telemetry:workspace"].as_str(),
            Some("workspace-api-test")
        );
        assert_eq!(
            session.metadata["telemetry:task_id"].as_str(),
            Some("task-api-test")
        );
    }

    #[tokio::test]
    async fn submit_clarification_records_all_slots_with_session_manager() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-session-submit";
        let stage = StageContext::PlanningBootstrap;
        let unresolved_slot = "slot:delta";
        let state = harness.state_with_unresolved(workflow_id, stage, &[unresolved_slot]);

        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        let request = ClarifyRequest {
            blocker_type: BlockerType::LowConfidenceSlot,
            context: None,
            complexity: None,
            prompt_identity: None,
            batch_id: None,
            batch_total: None,
            task_id: None,
        };

        let response = harness
            .api
            .ask_clarification(workflow_id, request)
            .await
            .expect("clarify succeeds");

        let submission = ClarificationSubmission {
            response_text: "confirmed".into(),
            prompt_identity: None,
        };

        let result = harness
            .api
            .submit_clarification(workflow_id, &response.question_id, submission)
            .await
            .expect("submit succeeds");

        assert_eq!(
            result.extracted_slots.len(),
            1,
            "expected one slot extracted by deterministic clarifier"
        );
        assert_eq!(
            result.extracted_slots[0].id, unresolved_slot,
            "clarifier should preserve the original slot identifier"
        );

        let calls = session_manager.snapshot().await;
        assert_eq!(
            calls.mark_answered_calls, 1,
            "submit_clarification should record the answer exactly once"
        );
        assert_eq!(
            calls.mark_answered_ids,
            vec![response.question_id.clone()],
            "recorded question IDs should match the submitted question"
        );
        assert_eq!(
            calls.mark_answered_slot_counts,
            vec![1],
            "all extracted slots should be forwarded to the session manager"
        );
        assert_eq!(
            calls.mark_answered_slot_ids,
            vec![vec![unresolved_slot.to_string()]],
            "slot identifiers should match the originating slot"
        );
    }

    #[tokio::test]
    async fn answered_slot_is_not_reasked() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-no-duplicate";
        let stage = StageContext::PlanningBootstrap;
        let state = harness.state_with_unresolved(workflow_id, stage, &["slot:alpha", "slot:beta"]);

        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        let first_response = harness
            .api
            .ask_clarification(
                workflow_id,
                ClarifyRequest {
                    blocker_type: BlockerType::LowConfidenceSlot,
                    context: None,
                    complexity: None,
                    prompt_identity: None,
                    batch_id: None,
                    batch_total: None,
                    task_id: None,
                },
            )
            .await
            .expect("first clarification succeeds");
        let first_question_id = first_response.question_id.clone();
        assert_eq!(
            first_response.source_slot_id.as_deref(),
            Some("slot:alpha"),
            "first question should target the first unresolved slot"
        );

        let submission = ClarificationSubmission {
            response_text: "alpha-value".into(),
            prompt_identity: None,
        };
        harness
            .api
            .submit_clarification(workflow_id, &first_question_id, submission)
            .await
            .expect("submission succeeds");

        // Next clarification should target the remaining unresolved slot (slot:beta),
        // not repeat the already-answered question.
        let second_response = harness
            .api
            .ask_clarification(
                workflow_id,
                ClarifyRequest {
                    blocker_type: BlockerType::LowConfidenceSlot,
                    context: None,
                    complexity: None,
                    prompt_identity: None,
                    batch_id: None,
                    batch_total: None,
                    task_id: None,
                },
            )
            .await
            .expect("second clarification succeeds");

        assert_ne!(
            second_response.question_id, first_question_id,
            "second question should be different from the first"
        );
        assert_eq!(
            second_response.source_slot_id.as_deref(),
            Some("slot:beta"),
            "second question should target the remaining unresolved slot"
        );
    }

    fn blocked_round_session(workflow_id: &str) -> ClarificationSession {
        let mut session = ClarificationSession::new(workflow_id);
        session.round_count = session.round_limit();
        session.state = ClarificationSessionState::ReadyToPlan;
        session
    }

    #[tokio::test]
    async fn ask_clarification_rejects_when_round_guardrail_active() {
        let workflow_id = "wf-round-guard";
        let session_manager = Arc::new(RecordingSessionManager::with_session(
            blocked_round_session(workflow_id),
        ));
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let stage = StageContext::PlanningBootstrap;
        let state = harness.state_with_unresolved(workflow_id, stage, &["slot:guarded"]);
        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        let request = ClarifyRequest {
            blocker_type: BlockerType::LowConfidenceSlot,
            context: None,
            complexity: Some(TaskComplexity::Simple),
            prompt_identity: None,
            batch_id: None,
            batch_total: None,
            task_id: None,
        };

        let err = harness
            .api
            .ask_clarification(workflow_id, request)
            .await
            .expect_err("guardrail should block new questions");

        match err {
            AskLoopError::StateUnavailable(reason) => assert!(
                reason.contains("guardrail"),
                "expected guardrail reason, got {}",
                reason
            ),
            other => panic!("unexpected error variant: {:?}", other),
        }

        let calls = session_manager.snapshot().await;
        assert_eq!(
            calls.append_calls, 0,
            "question should not be appended when guardrail is active"
        );
    }

    #[tokio::test]
    async fn enqueue_batch_questions_respects_round_guardrail() {
        let workflow_id = "wf-batch-guard";
        let session_manager = Arc::new(RecordingSessionManager::with_session(
            blocked_round_session(workflow_id),
        ));
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let question = ClarifierQuestion {
            id: "q-guard-1".into(),
            question_text: "Need missing parameter".into(),
            created_at: Utc::now(),
            ..ClarifierQuestion::default()
        };

        let err = harness
            .api
            .enqueue_batch_questions(workflow_id, vec![question], TaskComplexity::Simple)
            .await
            .expect_err("guardrail should reject batch enqueue");

        match err {
            AskLoopError::StateUnavailable(reason) => assert!(
                reason.contains("guardrail"),
                "expected guardrail reason, got {}",
                reason
            ),
            other => panic!("unexpected error variant: {:?}", other),
        }

        let calls = session_manager.snapshot().await;
        assert_eq!(
            calls.append_calls, 0,
            "batch questions should not be appended when guardrail is active"
        );
    }

    #[tokio::test]
    async fn manual_resume_marks_session_ready_to_plan() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-manual-session";
        let question_id = "q-manual";
        let stage = StageContext::PlanningBootstrap;

        let state = harness.state_with_unresolved(workflow_id, stage, &["slot:gamma"]);
        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        harness
            .api
            .pause_manager()
            .pause(
                workflow_id,
                PauseReason::WaitingOnUser(question_id.to_string()),
            )
            .await
            .expect("workflow paused");

        let request = ManualResumeRequest {
            slots: Some(vec![ManualResumeSlot {
                slot_id: "slot:gamma".into(),
                slot_type: SlotType::Entity,
                value: serde_json::Value::String("manual-value".into()),
                confidence: Some(0.9),
            }]),
            updated_confidence: Some(0.9),
            handled_question: Some(question_id.to_string()),
        };

        harness
            .api
            .manual_resume(workflow_id, request)
            .await
            .expect("manual resume succeeds");

        let calls = session_manager.snapshot().await;
        assert_eq!(
            calls.mark_answered_calls, 1,
            "manual resume should record slot updates in the session"
        );
        assert_eq!(
            calls.mark_answered_ids,
            vec![question_id.to_string()],
            "manual resume should target the handled question"
        );
        assert_eq!(
            calls.mark_answered_slot_counts,
            vec![1],
            "manual resume should forward all provided slots"
        );
        assert_eq!(
            calls.mark_answered_slot_ids,
            vec![vec!["slot:gamma".to_string()]],
            "manual resume should preserve slot identifiers"
        );
        assert_eq!(
            calls.recorded_states,
            vec![ClarificationSessionState::Planning],
            "the durable resume owner should commit the final planning state atomically"
        );
    }

    #[tokio::test]
    async fn manual_resume_records_multiple_slots() {
        let session_manager = Arc::new(RecordingSessionManager::new());
        let harness = TestHarness::with_session_manager(session_manager.clone());

        let workflow_id = "wf-manual-multi-slot";
        let question_id = "q-manual-multi";
        let stage = StageContext::PlanningBootstrap;

        let state = harness.state_with_unresolved(workflow_id, stage, &["slot:alpha", "slot:beta"]);
        harness
            .api
            .state_tracker()
            .conversation_store()
            .append_state(workflow_id, state)
            .await
            .expect("state stored");

        harness
            .api
            .pause_manager()
            .pause(
                workflow_id,
                PauseReason::WaitingOnUser(question_id.to_string()),
            )
            .await
            .expect("workflow paused");

        let request = ManualResumeRequest {
            slots: Some(vec![
                ManualResumeSlot {
                    slot_id: "slot:alpha".into(),
                    slot_type: SlotType::Entity,
                    value: serde_json::Value::String("alpha-value".into()),
                    confidence: Some(0.82),
                },
                ManualResumeSlot {
                    slot_id: "slot:beta".into(),
                    slot_type: SlotType::Modifier,
                    value: serde_json::Value::String("beta-value".into()),
                    confidence: Some(0.78),
                },
            ]),
            updated_confidence: Some(0.85),
            handled_question: Some(question_id.to_string()),
        };

        harness
            .api
            .manual_resume(workflow_id, request)
            .await
            .expect("manual resume succeeds");

        let calls = session_manager.snapshot().await;
        assert_eq!(
            calls.mark_answered_calls, 1,
            "manual resume should record a single answer event"
        );
        assert_eq!(
            calls.mark_answered_ids,
            vec![question_id.to_string()],
            "manual resume should target the handled question"
        );
        assert_eq!(
            calls.mark_answered_slot_counts,
            vec![2],
            "all provided slots should be forwarded to the session manager"
        );
        assert_eq!(
            calls.mark_answered_slot_ids,
            vec![vec!["slot:alpha".to_string(), "slot:beta".to_string()]],
            "session manager should receive every slot identifier exactly once"
        );
        assert_eq!(
            calls.recorded_states,
            vec![ClarificationSessionState::Planning],
            "the durable resume owner should atomically advance the multi-slot session"
        );
    }

    #[tokio::test]
    async fn compose_context_only_returns_new_unresolved_slots() {
        let harness = TestHarness::new();
        let stage = StageContext::PlanningBootstrap;
        let mut state = harness.state_with_unresolved("wf-delta", stage, &["slot:a", "slot:b"]);

        let composed = harness.api.compose_context("wf-delta", Some(&state), None);
        let first = harness
            .history
            .filter_new_slots("wf-delta", stage, &composed.unresolved_slots);
        let mut ids: Vec<_> = first.iter().map(|slot| slot.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["slot:a", "slot:b"]);

        // Re-run without changing state: should produce no new slots.
        let composed_again = harness.api.compose_context("wf-delta", Some(&state), None);
        let second =
            harness
                .history
                .filter_new_slots("wf-delta", stage, &composed_again.unresolved_slots);
        assert!(second.is_empty(), "no new slots expected on second pass");

        // Simulate resolution of slot:a and introduction of slot:c
        if let Some(summary) = state.confidence.summary.as_mut() {
            summary.unresolved_slots = vec!["slot:b".to_string(), "slot:c".to_string()];
        }
        state
            .confidence
            .slot_records
            .insert("slot:c".to_string(), TestHarness::make_slot("slot:c"));
        state.confidence.per_slot.insert("slot:c".to_string(), 0.2);

        let composed_third = harness.api.compose_context("wf-delta", Some(&state), None);
        let third =
            harness
                .history
                .filter_new_slots("wf-delta", stage, &composed_third.unresolved_slots);
        let third_ids: Vec<_> = third.iter().map(|slot| slot.id.as_str()).collect();
        assert_eq!(
            third_ids,
            vec!["slot:c"],
            "only newly blocking slot should be returned"
        );
    }
}

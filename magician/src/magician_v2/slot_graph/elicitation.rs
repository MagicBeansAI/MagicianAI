use std::collections::{HashMap, HashSet};
use std::sync::{Arc, RwLock};

use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use blake3;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, info, warn};

use super::{
    enrichment::{EnrichmentPipeline, EnrichmentSummary},
    extraction::{ConversationContext, SlotExtractor},
    rewriter::{is_sentinel_question_text, ClarifiedOpenQuestion, ClarifiedTask, QuestionRewriter},
    types::{SlotRecord, SlotType},
};
use crate::magician_v2::{
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    ask_loop::{
        budget::Channel,
        clarifier::{
            build_slot_question_hint, BlockerType, ClarifierLibrary, ClarifierQuestion,
            WorkflowContext,
        },
    },
    confidence::{ConfidenceService, ConfidenceSummary},
    query_analysis::unified_analyzer::UnifiedQueryAnalysis,
    realtime_events::RuntimeTransportBroadcaster,
    state_tracker::{FailedStageInfo, StageCheckpoint, StageContext, StateTracker},
};

/// Repository abstraction for persisting slot graph records.
#[async_trait]
pub trait SlotGraphRepository: Send + Sync {
    async fn create_slot(&self, slot: &SlotRecord) -> Result<()>;
}

pub const STAGE_ELICITATION_SLOTS: &str = "elicitation.slots_prepared";
pub const STAGE_SLOT_PERSISTENCE: &str = "slot_graph.persistence";
pub const STAGE_ELICITATION_REWRITE: &str = "elicitation.question_rewrite";
pub const STAGE_ELICITATION_OUTCOME: &str = "elicitation.outcome";
pub const STAGE_ELICITATION_QUESTION_GENERATION: &str = "elicitation.question_generation";

#[derive(Debug, Clone, Default)]
pub struct ElicitationResumeState {
    pub stage_context: StageContext,
    pub completed_stages: Vec<StageCheckpoint>,
    pub failed_stage: Option<FailedStageInfo>,
}

/// Configuration tuning for the elicitation pipeline.
#[derive(Debug, Clone)]
pub struct ElicitationConfig {
    /// Minimum acceptable overall confidence before questions are triggered.
    pub confidence_gate: f64,
    /// Hard cap on clarification questions returned per request.
    pub max_questions: usize,
}

impl Default for ElicitationConfig {
    fn default() -> Self {
        Self {
            confidence_gate: 0.7,
            max_questions: 3,
        }
    }
}

/// Coordinated pipeline that extracts, enriches, persists, and rewrites user requests.
pub struct ElicitationService {
    extractor: Arc<RwLock<Arc<SlotExtractor>>>,
    enrichment: Arc<EnrichmentPipeline>,
    rewriter: Arc<RwLock<Arc<QuestionRewriter>>>,
    confidence_service: Arc<ConfidenceService>,
    slot_repo: Arc<dyn SlotGraphRepository>,
    clarifier: Option<Arc<ClarifierLibrary>>,
    state_tracker: Option<Arc<StateTracker>>,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    config: ElicitationConfig,
}

impl ElicitationService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        extractor: Arc<SlotExtractor>,
        enrichment: Arc<EnrichmentPipeline>,
        rewriter: Arc<QuestionRewriter>,
        confidence_service: Arc<ConfidenceService>,
        slot_repo: Arc<dyn SlotGraphRepository>,
        clarifier: Option<Arc<ClarifierLibrary>>,
        state_tracker: Option<Arc<StateTracker>>,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
        config: ElicitationConfig,
    ) -> Self {
        Self {
            extractor: Arc::new(RwLock::new(extractor)),
            enrichment,
            rewriter: Arc::new(RwLock::new(rewriter)),
            confidence_service,
            slot_repo,
            clarifier,
            state_tracker,
            event_broadcaster,
            config,
        }
    }

    /// Swap the extraction component at runtime to toggle between deterministic and live LLMs.
    pub fn set_extractor(&self, extractor: Arc<SlotExtractor>) {
        let mut guard = self
            .extractor
            .write()
            .expect("extractor lock poisoned during update");
        *guard = extractor;
    }

    /// Get access to the extractor's RwLock for reading
    pub fn extractor(&self) -> &Arc<RwLock<Arc<SlotExtractor>>> {
        &self.extractor
    }

    /// Swap the rewrite component at runtime (used to toggle between deterministic and live LLMs).
    pub fn set_rewriter(&self, rewriter: Arc<QuestionRewriter>) {
        let mut guard = self
            .rewriter
            .write()
            .expect("rewriter lock poisoned during update");
        *guard = rewriter;
    }

    /// Execute the full elicitation flow from free-form text to a planner-ready task.
    pub async fn elicit_and_rewrite(
        &self,
        workflow_id: &str,
        user_message: &str,
        query_analysis: Option<&UnifiedQueryAnalysis>,
        context: Option<ConversationContext>,
        stage_context_override: Option<StageContext>,
    ) -> Result<ElicitationResult> {
        self.elicit_and_rewrite_with_scope(
            workflow_id,
            user_message,
            query_analysis,
            context,
            stage_context_override,
            None,
            None,
            OperationLlmCallAttribution::default(),
        )
        .await
    }

    pub async fn elicit_and_rewrite_with_scope(
        &self,
        workflow_id: &str,
        user_message: &str,
        query_analysis: Option<&UnifiedQueryAnalysis>,
        context: Option<ConversationContext>,
        stage_context_override: Option<StageContext>,
        principal: Option<&str>,
        workspace: Option<&str>,
        attribution: OperationLlmCallAttribution,
    ) -> Result<ElicitationResult> {
        let llm_telemetry = match (principal, workspace, self.event_broadcaster.as_ref()) {
            (Some(principal), Some(workspace), Some(broadcaster)) => {
                Some(OperationLlmTelemetryContext::new(
                    Arc::clone(broadcaster),
                    principal,
                    workspace,
                    "elicitation",
                ))
            },
            _ => None,
        };
        info!(
            "[MAGICIAN-ELICITATION] === Starting elicit_and_rewrite ===\nWorkflow: {}\nInput Query: '{}'\nStage Override: {:?}",
            workflow_id, user_message, stage_context_override
        );

        if user_message.trim().is_empty() {
            return Err(anyhow!("user message cannot be empty"));
        }

        let resume_state = self
            .load_resume_state(workflow_id, stage_context_override)
            .await;

        // CHANGE 1: Use composite key (stage_name, stage_context) to prevent cross-context checkpoint reuse
        let completed_lookup: HashMap<(&str, StageContext), &StageCheckpoint> = resume_state
            .completed_stages
            .iter()
            .map(|cp| ((cp.stage_name.as_str(), cp.stage_context), cp))
            .collect();

        // CHANGE 2: Use composite key for outcome checkpoint lookup
        if let Some(outcome_checkpoint) =
            completed_lookup.get(&(STAGE_ELICITATION_OUTCOME, resume_state.stage_context))
        {
            info!(
                "[MAGICIAN-ELICITATION-CHECKPOINT] Using outcome checkpoint for context {:?}",
                resume_state.stage_context
            );
            return Self::parse_outcome_checkpoint(outcome_checkpoint);
        }

        let (slots, mut confidence_summary, enrichment_summary) =
            // CHANGE 3: Use composite key for slots checkpoint lookup
            if let Some(prepared_checkpoint) = completed_lookup.get(
                &(STAGE_ELICITATION_SLOTS, resume_state.stage_context)
            ) {
                info!(
                    "[MAGICIAN-ELICITATION-CHECKPOINT] Using slots checkpoint for context {:?}\n\
                     - Completed at: {}",
                    resume_state.stage_context,
                    prepared_checkpoint.completed_at
                );
                let (slots, summary, stored_enrichment) =
                    Self::parse_slots_checkpoint(prepared_checkpoint)?;
                (slots, summary, stored_enrichment)
            } else {
                // CHANGE 4: Log fresh extraction path
                info!(
                    "[MAGICIAN-ELICITATION-FRESH] No matching checkpoint, running fresh extraction\n\
                     - Resume context: {:?}\n\
                     - Query length: {} chars\n\
                     - Available checkpoints: {}",
                    resume_state.stage_context,
                    user_message.len(),
                    resume_state.completed_stages.len()
                );
                // Emit extraction started event
                if let Some(broadcaster) = &self.event_broadcaster {
                    broadcaster.slot_extraction_started(
                        workflow_id,
                        workflow_id, // Use workflow_id as correlation_id
                        user_message.len(),
                    );
                }

                let extractor = {
                    let guard = self
                        .extractor
                        .read()
                        .expect("extractor lock poisoned during read");
                    Arc::clone(&*guard)
                };

                match extractor
                    .extract_slots_with_scope(
                        user_message,
                        context.as_ref(),
                        Some(workflow_id),
                        Some(workflow_id),
                        principal,
                        workspace,
                        attribution.clone(),
                    )
                    .await
                {
                    Ok(provisional_slots) => {
                        let mut slots: Vec<SlotRecord> = provisional_slots
                            .into_iter()
                            .map(|slot| SlotRecord::from_provisional(workflow_id, slot))
                            .collect();

                        // Emit individual slot extracted events
                        if let Some(broadcaster) = &self.event_broadcaster {
                            for slot in &slots {
                                broadcaster.slot_extracted(
                                    workflow_id,
                                    workflow_id,
                                    slot.id.clone(),
                                    format!("{:?}", slot.slot_type),
                                    slot.confidence,
                                );
                            }
                        }

                        // Emit enrichment started event
                        if let Some(broadcaster) = &self.event_broadcaster {
                            broadcaster.slot_enrichment_started(
                                workflow_id,
                                workflow_id,
                                slots.len(),
                                self.enrichment.enricher_count(),
                            );
                        }

                        let enrichment_summary_value = self.enrichment.run(&mut slots).await;
                        if let Some(broadcaster) = &self.event_broadcaster {
                            for update in &enrichment_summary_value.confidence_updates {
                                broadcaster.slot_confidence_updated(
                                    workflow_id,
                                    workflow_id,
                                    update.slot_id.clone(),
                                    update.old_confidence,
                                    update.new_confidence,
                                    update.enricher.clone(),
                                );
                            }
                        }
                        let enrichment_summary = Some(enrichment_summary_value.clone());

                        // Emit enrichment completed event
                        if let Some(broadcaster) = &self.event_broadcaster {
                            broadcaster.slot_enrichment_completed(
                                workflow_id,
                                workflow_id,
                                slots.len(),
                                enrichment_summary_value.slots_changed,
                                enrichment_summary_value.invocations,
                                enrichment_summary_value.errors.len(),
                            );
                        }

                        if !enrichment_summary_value.errors.is_empty() {
                            warn!(
                                "[MAGICIAN-V2-ELICIT] enrichment completed with {} error(s)",
                                enrichment_summary_value.errors.len()
                            );
                        } else {
                            debug!(
                                "[MAGICIAN-V2-ELICIT] enrichment applied to {} slot(s)",
                                enrichment_summary_value.slots_changed
                            );
                        }

                        let confidence_summary =
                            self.confidence_service.summarize_confidence(&slots);
                        let payload = json!({
                            "slots": slots,
                            "confidence_summary": Self::summary_to_json(&confidence_summary),
                            "enrichment_summary": enrichment_summary.clone(),
                        });
                        self.append_stage_checkpoint(
                            workflow_id,
                            resume_state.stage_context,
                            STAGE_ELICITATION_SLOTS,
                            payload,
                        )
                        .await;

                        (slots, confidence_summary, enrichment_summary)
                    },
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-ELICIT] slot extraction failed for {} (context {:?}): {}",
                            workflow_id, resume_state.stage_context, err
                        );

                        let fallback = resume_state
                            .completed_stages
                            .iter()
                            .rev()
                            .find(|cp| cp.stage_name == STAGE_ELICITATION_SLOTS)
                            .and_then(|checkpoint| Self::parse_slots_checkpoint(checkpoint).ok());

                        if let Some((slots, summary, stored_enrichment)) = fallback {
                            info!(
                                "[MAGICIAN-V2-ELICIT] Falling back to last persisted slots checkpoint for {} ({} slot records)",
                                workflow_id,
                                slots.len()
                            );
                            (slots, summary, stored_enrichment)
                        } else {
                            let message = err.to_string();
                            self.record_stage_failure(
                                workflow_id,
                                resume_state.stage_context,
                                STAGE_ELICITATION_SLOTS,
                                message,
                            )
                            .await;
                            return Err(err.context("slot extraction failed"));
                        }
                    },
                }
            };

        // CHANGE 5: Use composite key for slot persistence check
        if !completed_lookup.contains_key(&(STAGE_SLOT_PERSISTENCE, resume_state.stage_context)) {
            if let Err(err) = self.persist_slots(&slots).await {
                let message = err.to_string();
                self.record_stage_failure(
                    workflow_id,
                    resume_state.stage_context,
                    STAGE_SLOT_PERSISTENCE,
                    message,
                )
                .await;
                return Err(err);
            }

            let payload = json!({
                "slot_ids": slots.iter().map(|slot| slot.id.clone()).collect::<Vec<_>>()
            });
            self.append_stage_checkpoint(
                workflow_id,
                resume_state.stage_context,
                STAGE_SLOT_PERSISTENCE,
                payload,
            )
            .await;
        }

        // CHANGE 6: Use composite key for rewrite checkpoint lookup
        let (mut clarified_task, from_checkpoint) = if let Some(rewrite_checkpoint) =
            completed_lookup.get(&(STAGE_ELICITATION_REWRITE, resume_state.stage_context))
        {
            info!(
                "[MAGICIAN-ELICITATION-CHECKPOINT] Using rewrite checkpoint for context {:?}",
                resume_state.stage_context
            );
            (Self::parse_rewrite_checkpoint(rewrite_checkpoint)?, true)
        } else {
            let rewriter = {
                let guard = self
                    .rewriter
                    .read()
                    .expect("rewriter lock poisoned during read");
                Arc::clone(&*guard)
            };

            info!(
                    "[MAGICIAN-ELICITATION] Calling query rewriter for planner\nInput Query: '{}'\nSlots: {} resolved\nUnresolved: {:?}",
                    user_message,
                    slots.len(),
                    confidence_summary.unresolved_slots
                );

            let mut clarified_task = match rewriter
                .rewrite_for_planner_with_telemetry(
                    user_message,
                    user_message,
                    &slots,
                    &confidence_summary.unresolved_slots,
                    llm_telemetry.as_ref(),
                    attribution.clone(),
                )
                .await
            {
                Ok(task) => {
                    info!(
                            "[MAGICIAN-ELICITATION] ✓ Query rewritten successfully\nOriginal: '{}'\nClarified: '{}'\nObjectives: {}, Constraints: {}, Confidence: {:.2}",
                            user_message,
                            task.clarified_task,
                            task.objectives.len(),
                            task.constraints.len(),
                            task.confidence
                        );
                    task
                },
                Err(err) => {
                    error!("[MAGICIAN-ELICITATION] ✗ Query rewriting failed: {}", err);
                    let message = err.to_string();
                    if let Some(broadcaster) = &self.event_broadcaster {
                        broadcaster.observability_alert(
                            workflow_id,
                            "rewrite_failed",
                            json!({
                                "stage_context": resume_state.stage_context.as_str(),
                                "error": message,
                                "unresolved_slots": confidence_summary.unresolved_slots,
                            }),
                        );
                    }
                    self.record_stage_failure(
                        workflow_id,
                        resume_state.stage_context,
                        STAGE_ELICITATION_REWRITE,
                        message,
                    )
                    .await;
                    return Err(err.context("question rewriting failed"));
                },
            };

            self.curate_open_questions(
                workflow_id,
                &mut clarified_task,
                llm_telemetry.as_ref(),
                attribution.clone(),
            )
            .await;

            let payload = json!({ "clarified_task": clarified_task.clone() });
            self.append_stage_checkpoint(
                workflow_id,
                resume_state.stage_context,
                STAGE_ELICITATION_REWRITE,
                payload,
            )
            .await;

            // Emit clarified task ready event
            if let Some(broadcaster) = &self.event_broadcaster {
                broadcaster.clarified_task_ready(
                    workflow_id,
                    workflow_id,
                    clarified_task.clarified_task.clone(),
                    clarified_task.objectives.len(),
                    clarified_task.constraints.len(),
                    clarified_task.confidence,
                );
            }

            (clarified_task, false)
        };

        if from_checkpoint {
            self.curate_open_questions(
                workflow_id,
                &mut clarified_task,
                llm_telemetry.as_ref(),
                attribution.clone(),
            )
            .await;
        }

        let has_open_questions = !clarified_task.open_questions.is_empty();

        let mut qa_triggered = false;
        if let Some(analysis) = query_analysis {
            let qa_confidence = analysis.extracted_entities.extraction_confidence as f64;
            if qa_confidence > 0.0 {
                confidence_summary.overall = confidence_summary.overall.min(qa_confidence);
            }

            // Trigger clarification based on confidence threshold only
            // Removed dangerous keyword matching on LLM reasoning text
            if qa_confidence < self.config.confidence_gate {
                qa_triggered = true;
            }
        }

        let needs_clarification = qa_triggered
            || confidence_summary.overall < self.config.confidence_gate
            || !confidence_summary.unresolved_slots.is_empty()
            || has_open_questions;

        if needs_clarification {
            let (slot_trigger_mappings, mut recommended_questions) = if has_open_questions {
                (
                    Vec::new(),
                    self.convert_open_questions_to_clarifier_questions(
                        workflow_id,
                        &clarified_task,
                        &slots,
                        &confidence_summary,
                    )
                    .await?,
                )
            } else {
                let plan = match self
                    .generate_clarification_questions(
                        workflow_id,
                        user_message,
                        &slots,
                        &confidence_summary,
                    )
                    .await
                {
                    Ok(plan) => plan,
                    Err(err) => {
                        let message = err.to_string();
                        self.record_stage_failure(
                            workflow_id,
                            resume_state.stage_context,
                            STAGE_ELICITATION_QUESTION_GENERATION,
                            message,
                        )
                        .await;
                        return Err(err);
                    },
                };

                (plan.slot_mappings, plan.questions)
            };

            if recommended_questions.is_empty() && qa_triggered {
                if let Some(analysis) = query_analysis {
                    recommended_questions.push(query_analysis_fallback_question(
                        workflow_id,
                        user_message,
                        analysis,
                    ));
                }
            }

            if recommended_questions.is_empty() {
                recommended_questions.push(generic_fallback_question(workflow_id, user_message));
            }

            recommended_questions.truncate(self.config.max_questions);

            let result = ElicitationResult {
                slot_graph: slots.clone(),
                clarified_task: clarified_task.clone(),
                confidence_summary: confidence_summary.clone(),
                needs_clarification: true,
                recommended_questions,
                enrichment_summary: enrichment_summary.clone(),
                slot_trigger_mappings,
                confidence_boost_results: HashMap::new(),
                llm_calls_used: 2,
            };

            self.record_outcome_checkpoint(workflow_id, resume_state.stage_context, &result)
                .await;

            return Ok(result);
        }

        let result = ElicitationResult {
            slot_graph: slots.clone(),
            clarified_task: clarified_task.clone(),
            confidence_summary: confidence_summary.clone(),
            needs_clarification: false,
            recommended_questions: Vec::new(),
            enrichment_summary: enrichment_summary.clone(),
            slot_trigger_mappings: Vec::new(),
            confidence_boost_results: HashMap::new(),
            llm_calls_used: 2, // Extraction + rewrite when no clarification needed
        };

        self.record_outcome_checkpoint(workflow_id, resume_state.stage_context, &result)
            .await;

        Ok(result)
    }

    async fn generate_clarification_questions(
        &self,
        workflow_id: &str,
        user_message: &str,
        slots: &[SlotRecord],
        summary: &ConfidenceSummary,
    ) -> Result<ClarificationPlan> {
        let mut by_id: HashMap<&str, &SlotRecord> = HashMap::new();
        for slot in slots {
            by_id.insert(slot.id.as_str(), slot);
        }

        let mut targeted_slots = Vec::new();
        for slot_id in &summary.unresolved_slots {
            if let Some(slot) = by_id.get(slot_id.as_str()) {
                targeted_slots.push(*slot);
            }
        }

        let mut questions = Vec::new();
        let mut slot_mappings: Vec<SlotTriggerMapping> = Vec::new();

        if let Some(clarifier) = &self.clarifier {
            let mut confidence_scores = HashMap::with_capacity(slots.len());
            for slot in slots {
                confidence_scores.insert(
                    slot.id.clone(),
                    self.confidence_service.calculate_slot_confidence(slot),
                );
            }

            let workflow_context = WorkflowContext {
                workflow_id: workflow_id.to_string(),
                stage_context: StageContext::PlanningBootstrap,
                recent_observations: Vec::new(),
                slot_graph: slots.to_vec(),
                confidence_scores,
                question_hint: None,
            };

            let mut seen_blockers = HashSet::new();
            for slot in targeted_slots.iter().take(self.config.max_questions) {
                let blocker = blocker_for_slot(slot);
                if !seen_blockers.insert(blocker) {
                    continue;
                }

                let mut slot_context = workflow_context.clone();
                slot_context.question_hint = Some(build_slot_question_hint(slot));

                match clarifier.generate_question(blocker, &slot_context).await {
                    Ok(question) => {
                        slot_mappings.push(SlotTriggerMapping {
                            question_id: question.id.clone(),
                            triggered_slots: vec![TriggeredSlot {
                                slot_id: slot.id.clone(),
                                slot_type: slot.slot_type.clone(),
                                confidence: slot.confidence,
                                triggered_at: slot.updated_at,
                            }],
                        });
                        questions.push(question);
                    },
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-ELICIT] failed to generate clarifier question: {}",
                            err
                        );
                    },
                }
            }
        }

        if questions.is_empty() {
            let fallback = fallback_question(workflow_id, user_message, summary, slots);
            let mut triggered_slots: Vec<TriggeredSlot> = targeted_slots
                .iter()
                .map(|slot| TriggeredSlot {
                    slot_id: slot.id.clone(),
                    slot_type: slot.slot_type.clone(),
                    confidence: slot.confidence,
                    triggered_at: slot.updated_at,
                })
                .collect();

            if triggered_slots.is_empty() {
                triggered_slots = summary
                    .unresolved_slots
                    .iter()
                    .filter_map(|slot_id| by_id.get(slot_id.as_str()))
                    .map(|slot| TriggeredSlot {
                        slot_id: slot.id.clone(),
                        slot_type: slot.slot_type.clone(),
                        confidence: slot.confidence,
                        triggered_at: slot.updated_at,
                    })
                    .collect();
            }

            slot_mappings.push(SlotTriggerMapping {
                question_id: fallback.id.clone(),
                triggered_slots,
            });

            questions.push(fallback);
        }

        Ok(ClarificationPlan {
            questions,
            slot_mappings,
        })
    }

    async fn convert_open_questions_to_clarifier_questions(
        &self,
        workflow_id: &str,
        clarified_task: &ClarifiedTask,
        slots: &[SlotRecord],
        summary: &ConfidenceSummary,
    ) -> Result<Vec<ClarifierQuestion>> {
        if clarified_task.open_questions.is_empty() {
            return Ok(Vec::new());
        }

        let max_questions = self.config.max_questions.max(1);
        let mut questions = Vec::new();

        for (idx, open_question) in clarified_task
            .open_questions
            .iter()
            .enumerate()
            .take(max_questions)
        {
            let question_text = open_question.question_text.trim();
            // Defense in depth (the normaliser already filters these): skip empty
            // and null-ish sentinel ("None"/"null"/…) question text so a
            // stringified-null never becomes a surfaced clarification.
            if question_text.is_empty() || is_sentinel_question_text(question_text) {
                continue;
            }

            if let Some(clarifier) = &self.clarifier {
                let mut confidence_scores = HashMap::with_capacity(slots.len());
                for slot in slots {
                    confidence_scores.insert(
                        slot.id.clone(),
                        self.confidence_service.calculate_slot_confidence(slot),
                    );
                }

                let workflow_context = WorkflowContext {
                    workflow_id: workflow_id.to_string(),
                    stage_context: StageContext::PlanningBootstrap,
                    recent_observations: Vec::new(),
                    slot_graph: slots.to_vec(),
                    confidence_scores,
                    question_hint: Some(question_text.to_string()),
                };

                match clarifier
                    .generate_question(BlockerType::LowConfidenceSlot, &workflow_context)
                    .await
                {
                    Ok(mut question) => {
                        question.question_text = question_text.to_string();
                        if question.context_snippets.is_empty()
                            && !clarified_task.clarified_task.is_empty()
                        {
                            question
                                .context_snippets
                                .push(format!("Clarified task: {}", clarified_task.clarified_task));
                        }

                        if let Some(slot_id) = open_question
                            .slot_id
                            .as_ref()
                            .or_else(|| summary.unresolved_slots.get(idx))
                        {
                            if let Some(slot) =
                                slots.iter().find(|candidate| &candidate.id == slot_id)
                            {
                                question.source_slot_id = Some(slot.id.clone());
                                question.context_snippets.push(format!(
                                    "Related slot {} ({:?}) confidence {:.2}",
                                    slot.id, slot.slot_type, slot.confidence
                                ));
                            }
                        }

                        apply_open_question_metadata(&mut question, open_question);
                        enrich_open_question(&mut question, workflow_id, idx, slots, summary);
                        append_rewrite_context(&mut question, clarified_task, idx);

                        question.id = stable_question_id(
                            workflow_id,
                            &question.stage,
                            question.source_slot_id.as_deref(),
                            &question.question_text,
                        );

                        questions.push(question);
                        continue;
                    },
                    Err(err) => {
                        warn!(
                            "[MAGICIAN-V2-ELICIT] Failed to generate clarifier question for open question: {}",
                            err
                        );
                    },
                }
            }

            let mut context_snippets = Vec::new();
            if !clarified_task.clarified_task.is_empty() {
                context_snippets.push(format!("Clarified task: {}", clarified_task.clarified_task));
            }

            if let Some(rationale) = clarified_task.constraints.first() {
                if !rationale.trim().is_empty() {
                    context_snippets.push(rationale.clone());
                }
            }

            if context_snippets.is_empty() {
                context_snippets.push("Additional detail required to proceed.".to_string());
            }

            let mut question = ClarifierQuestion {
                id: stable_question_id(
                    workflow_id,
                    &StageContext::PlanningBootstrap,
                    open_question
                        .slot_id
                        .as_deref()
                        .or_else(|| summary.unresolved_slots.get(idx).map(|s| s.as_str())),
                    question_text,
                ),
                blocker_type: BlockerType::LowConfidenceSlot,
                stage: StageContext::PlanningBootstrap,
                source_slot_id: open_question
                    .slot_id
                    .clone()
                    .or_else(|| summary.unresolved_slots.get(idx).cloned()),
                question_text: question_text.to_string(),
                context_snippets,
                urgency: 0.6,
                channel: Channel::InApp,
                created_at: Utc::now(),
                options: None,
                batch_id: None,
                batch_total: None,
                ..ClarifierQuestion::default()
            };

            apply_open_question_metadata(&mut question, open_question);
            enrich_open_question(&mut question, workflow_id, idx, slots, summary);
            append_rewrite_context(&mut question, clarified_task, idx);

            question.id = stable_question_id(
                workflow_id,
                &question.stage,
                question.source_slot_id.as_deref(),
                &question.question_text,
            );

            questions.push(question);
        }

        Ok(questions)
    }

    async fn curate_open_questions(
        &self,
        workflow_id: &str,
        clarified_task: &mut ClarifiedTask,
        llm_telemetry: Option<&OperationLlmTelemetryContext>,
        attribution: OperationLlmCallAttribution,
    ) {
        if clarified_task.open_questions.is_empty() {
            return;
        }

        let original_count = clarified_task.open_questions.len();
        let max_questions = self.config.max_questions.max(1);

        let rewriter = {
            let guard = self
                .rewriter
                .read()
                .expect("rewriter lock poisoned during read");
            Arc::clone(&*guard)
        };

        match rewriter
            .curate_clarification_questions_with_telemetry(
                clarified_task,
                max_questions,
                llm_telemetry,
                attribution,
            )
            .await
        {
            Ok(curated) if !curated.is_empty() => {
                let mut curated_questions = Vec::new();
                for (idx, entry) in curated.into_iter().enumerate() {
                    let trimmed = entry.question.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    let mut question = clarified_task
                        .open_questions
                        .get(idx)
                        .cloned()
                        .unwrap_or_else(|| ClarifiedOpenQuestion::from_text(trimmed));
                    question.question_text = trimmed.to_string();

                    if let Some(reason) = entry.reason.as_ref().and_then(|r| {
                        let cleaned = r.trim();
                        if cleaned.is_empty() {
                            None
                        } else {
                            Some(cleaned.to_string())
                        }
                    }) {
                        if !question.context.iter().any(|ctx| ctx == &reason) {
                            question.context.push(reason);
                        }
                    }

                    curated_questions.push(question);
                }

                if curated_questions.is_empty() {
                    debug!(
                        "[MAGICIAN-V2-ELICIT] Curator returned only empty strings; keeping original {} questions",
                        original_count
                    );
                    return;
                }

                info!(
                    "[MAGICIAN-V2-ELICIT] Curated {} clarification question(s) from {} candidate(s)",
                    curated_questions.len(),
                    original_count
                );
                clarified_task.open_questions = curated_questions;
            },
            Ok(_) => {
                debug!(
                    "[MAGICIAN-V2-ELICIT] Question curation returned no results; retaining original {} questions",
                    original_count
                );
            },
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-ELICIT] Question curation failed (using original {} questions): {}",
                    original_count,
                    err
                );
                if let Some(broadcaster) = &self.event_broadcaster {
                    broadcaster.observability_alert(
                        workflow_id,
                        "curation_failed",
                        json!({
                            "original_count": original_count,
                            "error": err.to_string()
                        }),
                    );
                }
            },
        }
    }

    async fn persist_slots(&self, slots: &[SlotRecord]) -> Result<()> {
        for slot in slots {
            self.slot_repo
                .create_slot(slot)
                .await
                .with_context(|| format!("failed to persist slot {}", slot.id))?;
        }
        Ok(())
    }

    async fn append_stage_checkpoint(
        &self,
        workflow_id: &str,
        stage_context: StageContext,
        stage_name: &str,
        outputs: Value,
    ) {
        if let Some(tracker) = &self.state_tracker {
            if let Err(err) = tracker
                .append_stage_checkpoint(workflow_id, stage_context, stage_name, outputs)
                .await
            {
                warn!(
                    "[MAGICIAN-V2-ELICIT] Failed to append stage checkpoint {} for {}: {}",
                    stage_name, workflow_id, err
                );
            }
        }
    }

    async fn record_stage_failure(
        &self,
        workflow_id: &str,
        stage_context: StageContext,
        stage_name: &str,
        error: String,
    ) {
        if let Some(tracker) = &self.state_tracker {
            if let Err(err) = tracker
                .record_stage_failure(workflow_id, stage_context, stage_name, error)
                .await
            {
                warn!(
                    "[MAGICIAN-V2-ELICIT] Failed to record stage failure {} for {}: {}",
                    stage_name, workflow_id, err
                );
            }
        }
    }

    async fn record_outcome_checkpoint(
        &self,
        workflow_id: &str,
        stage_context: StageContext,
        result: &ElicitationResult,
    ) {
        let payload = match serde_json::to_value(result) {
            Ok(serialized) => json!({ "result": serialized }),
            Err(err) => {
                warn!(
                    "[MAGICIAN-V2-ELICIT] Failed to serialize elicitation outcome for {}: {}",
                    workflow_id, err
                );
                return;
            },
        };
        self.append_stage_checkpoint(
            workflow_id,
            stage_context,
            STAGE_ELICITATION_OUTCOME,
            payload,
        )
        .await;
    }

    async fn load_resume_state(
        &self,
        workflow_id: &str,
        stage_context_override: Option<StageContext>,
    ) -> ElicitationResumeState {
        let default_context = stage_context_override.unwrap_or(StageContext::PlanningBootstrap);

        if let Some(tracker) = &self.state_tracker {
            match tracker.latest_state(workflow_id).await {
                Ok(Some(state)) => {
                    let mut stage_context = stage_context_override.unwrap_or({
                        if state.stage_context == StageContext::Unknown {
                            StageContext::PlanningBootstrap
                        } else {
                            state.stage_context
                        }
                    });

                    if let Some(override_ctx) = stage_context_override {
                        stage_context = override_ctx;
                    }

                    ElicitationResumeState {
                        stage_context,
                        completed_stages: state.completed_stages.clone(),
                        failed_stage: state.failed_stage.clone(),
                    }
                },
                Ok(None) => ElicitationResumeState {
                    stage_context: default_context,
                    ..Default::default()
                },
                Err(err) => {
                    warn!(
                        "[MAGICIAN-V2-ELICIT] Failed to load latest state for {}: {}",
                        workflow_id, err
                    );
                    ElicitationResumeState {
                        stage_context: default_context,
                        ..Default::default()
                    }
                },
            }
        } else {
            ElicitationResumeState {
                stage_context: default_context,
                ..Default::default()
            }
        }
    }

    fn summary_to_json(summary: &ConfidenceSummary) -> Value {
        json!({
            "overall": summary.overall,
            "min_critical_slot": summary.min_critical_slot,
            "unresolved_slots": summary.unresolved_slots,
        })
    }

    fn parse_slots_checkpoint(
        checkpoint: &StageCheckpoint,
    ) -> Result<(
        Vec<SlotRecord>,
        ConfidenceSummary,
        Option<EnrichmentSummary>,
    )> {
        let payload = checkpoint
            .outputs
            .as_object()
            .ok_or_else(|| anyhow!("slots checkpoint missing object payload"))?;

        let slots_value = payload
            .get("slots")
            .ok_or_else(|| anyhow!("slots checkpoint missing slot list"))?
            .clone();
        let slots: Vec<SlotRecord> =
            serde_json::from_value(slots_value).context("deserialize slots from checkpoint")?;

        let summary_value = payload
            .get("confidence_summary")
            .ok_or_else(|| anyhow!("slots checkpoint missing confidence summary"))?
            .clone();
        let summary_payload: ConfidenceSummaryRecord = serde_json::from_value(summary_value)
            .context("deserialize confidence summary from checkpoint")?;

        let summary = ConfidenceSummary {
            overall: summary_payload.overall,
            min_critical_slot: summary_payload.min_critical_slot,
            unresolved_slots: summary_payload.unresolved_slots,
        };

        let enrichment_summary = payload
            .get("enrichment_summary")
            .map(|value| serde_json::from_value(value.clone()))
            .transpose()
            .context("deserialize enrichment summary from checkpoint")?;

        Ok((slots, summary, enrichment_summary))
    }

    fn parse_rewrite_checkpoint(checkpoint: &StageCheckpoint) -> Result<ClarifiedTask> {
        let payload = checkpoint
            .outputs
            .as_object()
            .ok_or_else(|| anyhow!("rewrite checkpoint missing object payload"))?;
        let task_value = payload
            .get("clarified_task")
            .ok_or_else(|| anyhow!("rewrite checkpoint missing clarified task"))?
            .clone();
        let clarified_task: ClarifiedTask = serde_json::from_value(task_value)
            .context("deserialize clarified task from checkpoint")?;
        Ok(clarified_task)
    }

    fn parse_outcome_checkpoint(checkpoint: &StageCheckpoint) -> Result<ElicitationResult> {
        let payload = checkpoint
            .outputs
            .as_object()
            .ok_or_else(|| anyhow!("elicitation outcome checkpoint missing payload"))?;
        let result_value = payload
            .get("result")
            .ok_or_else(|| anyhow!("elicitation outcome checkpoint missing result field"))?
            .clone();
        let result: ElicitationResult = serde_json::from_value(result_value)
            .context("deserialize elicitation result from checkpoint")?;
        Ok(result)
    }
}

/// Result payload returned by the elicitation service.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationResult {
    pub slot_graph: Vec<SlotRecord>,
    pub clarified_task: ClarifiedTask,
    pub confidence_summary: ConfidenceSummary,
    pub needs_clarification: bool,
    pub recommended_questions: Vec<ClarifierQuestion>,
    #[serde(default)]
    pub enrichment_summary: Option<EnrichmentSummary>,
    #[serde(default)]
    pub slot_trigger_mappings: Vec<SlotTriggerMapping>,
    #[serde(default)]
    pub confidence_boost_results: HashMap<String, Vec<ConfidenceBoostResult>>,

    /// Number of LLM calls made during elicitation (extraction + rewrite)
    #[serde(default)]
    pub llm_calls_used: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfidenceSummaryRecord {
    overall: f64,
    min_critical_slot: f64,
    #[serde(default)]
    unresolved_slots: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotTriggerMapping {
    pub question_id: String,
    #[serde(default)]
    pub triggered_slots: Vec<TriggeredSlot>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggeredSlot {
    pub slot_id: String,
    pub slot_type: SlotType,
    pub confidence: f64,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub triggered_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceBoostResult {
    pub slot_id: String,
    pub old_confidence: f64,
    pub new_confidence: f64,
    pub boost_amount: f64,
    #[serde(with = "chrono::serde::ts_milliseconds")]
    pub resolved_at: chrono::DateTime<chrono::Utc>,
}

struct ClarificationPlan {
    questions: Vec<ClarifierQuestion>,
    slot_mappings: Vec<SlotTriggerMapping>,
}

fn query_analysis_fallback_question(
    workflow_id: &str,
    user_message: &str,
    analysis: &UnifiedQueryAnalysis,
) -> ClarifierQuestion {
    let reasoning = analysis
        .extracted_entities
        .extraction_reasoning
        .trim()
        .to_string();

    let question_text = if reasoning.is_empty() {
        format!(
            "I need a bit more detail to help with: \"{}\". What should I clarify first?",
            user_message
        )
    } else {
        format!(
            "I noticed some ambiguity: {}. Could you clarify your request \"{}\"?",
            reasoning, user_message
        )
    };

    let mut context_snippets = vec![format!("User request: {}", user_message)];
    if !analysis.extracted_entities.entities.is_empty() {
        let entity_summary: Vec<String> = analysis
            .extracted_entities
            .entities
            .iter()
            .take(3)
            .map(|(key, value)| format!("{} → {}", key, value))
            .collect();
        if !entity_summary.is_empty() {
            context_snippets.push(format!("Extracted entities: {}", entity_summary.join(", ")));
        }
    }

    ClarifierQuestion {
        id: stable_question_id(
            workflow_id,
            &StageContext::PlanningBootstrap,
            None,
            &question_text,
        ),
        blocker_type: BlockerType::LowConfidenceSlot,
        stage: StageContext::PlanningBootstrap,
        source_slot_id: None,
        question_text,
        context_snippets,
        urgency: 0.7,
        channel: Channel::InApp,
        created_at: Utc::now(),
        options: None,
        batch_id: None,
        batch_total: None,
        ..ClarifierQuestion::default()
    }
}

fn generic_fallback_question(workflow_id: &str, user_message: &str) -> ClarifierQuestion {
    ClarifierQuestion {
        id: stable_question_id(
            workflow_id,
            &StageContext::PlanningBootstrap,
            None,
            "Could you share a bit more detail so I can help?",
        ),
        blocker_type: BlockerType::LowConfidenceSlot,
        stage: StageContext::PlanningBootstrap,
        source_slot_id: None,
        question_text: "Could you share a bit more detail so I can help?".to_string(),
        context_snippets: vec![format!("User request: {}", user_message)],
        urgency: 0.6,
        channel: Channel::InApp,
        created_at: Utc::now(),
        options: None,
        batch_id: None,
        batch_total: None,
        ..ClarifierQuestion::default()
    }
}

fn blocker_for_slot(slot: &SlotRecord) -> BlockerType {
    match slot.slot_type {
        SlotType::Entity => BlockerType::MissingEntity,
        SlotType::Spatial => BlockerType::AmbiguousLocation,
        SlotType::Status => BlockerType::UncertainStatus,
        SlotType::Modifier => BlockerType::ConflictingInformation,
        SlotType::Action => BlockerType::UnclearIntent,
        SlotType::ToolSelection => BlockerType::ToolSelectionRequired,
        _ => BlockerType::LowConfidenceSlot,
    }
}

fn fallback_question(
    workflow_id: &str,
    user_message: &str,
    summary: &ConfidenceSummary,
    slots: &[SlotRecord],
) -> ClarifierQuestion {
    let unresolved = if summary.unresolved_slots.is_empty() {
        "I'm still missing some key details to move forward.".to_string()
    } else {
        format!(
            "I'm still missing the following details: {}.",
            summary.unresolved_slots.join(", ")
        )
    };

    let context_snippets = slots
        .iter()
        .map(|slot| format!("{:?}: {}", slot.slot_type, slot.id))
        .collect();

    ClarifierQuestion {
        id: stable_question_id(
            workflow_id,
            &StageContext::PlanningBootstrap,
            None,
            &format!(
                "You mentioned \"{user_message}\". {unresolved} Could you share more specifics so I can plan accurately?"
            ),
        ),
        blocker_type: BlockerType::LowConfidenceSlot,
        stage: StageContext::PlanningBootstrap,
        source_slot_id: None,
        question_text: format!(
            "You mentioned \"{user_message}\". {unresolved} Could you share more specifics so I can plan accurately?"
        ),
        context_snippets,
        urgency: 0.6,
        channel: Channel::InApp,
        created_at: Utc::now(),
        options: None, // Fallback questions use free-text response
        batch_id: None,    // Batch tracking will be set during batch generation in Phase 4
        batch_total: None,
        ..ClarifierQuestion::default()
    }
}

fn enrich_open_question(
    question: &mut ClarifierQuestion,
    workflow_id: &str,
    question_index: usize,
    slots: &[SlotRecord],
    summary: &ConfidenceSummary,
) {
    let fallback_slot = summary.unresolved_slots.get(question_index).cloned();
    let mut source_slot_id = question.source_slot_id.clone().or(fallback_slot);

    if source_slot_id.is_none() {
        source_slot_id = Some(format!("{}::{}", workflow_id, question.id));
    }

    let resolved_slot = source_slot_id
        .as_ref()
        .and_then(|slot_id| find_slot(slots, slot_id));

    if let Some(slot) = resolved_slot {
        question.blocker_type = blocker_for_slot(slot);
    } else {
        question.blocker_type = BlockerType::LowConfidenceSlot;
    }

    question.source_slot_id = source_slot_id;

    if let Some(slot) = resolved_slot {
        if question.slot_confidence.is_none() {
            question.slot_confidence = Some(slot.confidence as f32);
        }
        if !question
            .context_snippets
            .iter()
            .any(|snippet| snippet.contains(&slot.id))
        {
            question.context_snippets.push(format!(
                "Target slot {} ({:?}) current value {} (confidence {:.2})",
                slot.id, slot.slot_type, slot.value, slot.confidence
            ));
        }
    }
}

fn apply_open_question_metadata(
    question: &mut ClarifierQuestion,
    open_question: &ClarifiedOpenQuestion,
) {
    for ctx in &open_question.context {
        if let Some(snippet) = snippet_preview(ctx) {
            push_context_snippet(question, "Clarifier note", &snippet);
        }
    }

    if question.source_slot_id.is_none() {
        question.source_slot_id = open_question.slot_id.clone();
    }

    if question.slot_confidence.is_none() {
        question.slot_confidence = open_question.slot_confidence;
    }

    if question.related_slots.is_empty() && !open_question.related_slots.is_empty() {
        question.related_slots = open_question.related_slots.clone();
    }
}

fn append_rewrite_context(
    question: &mut ClarifierQuestion,
    clarified_task: &ClarifiedTask,
    current_index: usize,
) {
    if let Some(original) = snippet_preview(&clarified_task.original_message) {
        push_context_snippet(question, "Original request", &original);
    }

    if let Some(clarified) = snippet_preview(&clarified_task.clarified_task) {
        push_context_snippet(question, "Clarified plan", &clarified);
    }

    let constraints: Vec<String> = clarified_task
        .constraints
        .iter()
        .filter_map(|c| snippet_preview(c))
        .take(3)
        .collect();
    if !constraints.is_empty() {
        push_context_snippet(question, "Constraints", &constraints.join(" | "));
    }

    let remaining_questions: Vec<String> = clarified_task
        .open_questions
        .iter()
        .enumerate()
        .filter(|(idx, _)| *idx != current_index)
        .filter_map(|(_, q)| snippet_preview(&q.question_text))
        .take(3)
        .collect();
    if !remaining_questions.is_empty() {
        push_context_snippet(
            question,
            "Other outstanding questions",
            &remaining_questions.join(" | "),
        );
    }
}

fn find_slot<'a>(slots: &'a [SlotRecord], slot_id: &str) -> Option<&'a SlotRecord> {
    slots.iter().find(|slot| slot.id == slot_id)
}

fn push_context_snippet(question: &mut ClarifierQuestion, label: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let entry = format!("{}: {}", label, value);
    if question
        .context_snippets
        .iter()
        .any(|snippet| snippet.starts_with(label))
    {
        return;
    }
    question.context_snippets.push(entry);
}

fn snippet_preview(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }

    let collapsed = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    let target = if collapsed.is_empty() {
        trimmed
    } else {
        collapsed.as_str()
    };

    let limited: String = if target.chars().count() > 240 {
        target.chars().take(240).collect::<String>() + "…"
    } else {
        target.to_string()
    };
    Some(limited)
}

fn stable_question_id(
    workflow_id: &str,
    stage: &StageContext,
    source_slot_id: Option<&str>,
    question_text: &str,
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(workflow_id.as_bytes());
    hasher.update(b"::");
    hasher.update(stage.as_str().as_bytes());
    hasher.update(b"::");

    if let Some(slot) = source_slot_id {
        hasher.update(slot.as_bytes());
    } else {
        hasher.update(question_text.trim().to_lowercase().as_bytes());
    }

    let digest = hasher.finalize().to_hex();
    let suffix = &digest[..12];
    format!("rewrite-open-{}-{}", workflow_id, suffix)
}

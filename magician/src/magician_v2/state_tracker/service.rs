use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, TimeZone, Utc};
use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::magician_v2::{
    confidence::{ConfidenceService, ConfidenceSummary},
    slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord},
    storage::{TurnDirection, V2ConversationStore, V2StorageError},
};

use super::types::{AssetType, ObservationAsset, SlotDelta, STAGE_METADATA_KEY};
use super::{
    BudgetSpend, BudgetState, ConfidenceScore, FailedStageInfo, StageCheckpoint, StageContext,
    StateBundle, WorkflowState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageResumeAction {
    Skip,
    Rehydrate,
    Recompute,
}

#[derive(Debug, Clone)]
pub struct StageResumeDecision {
    pub stage_name: String,
    pub stage_context: StageContext,
    pub action: StageResumeAction,
    pub checkpoint: Option<StageCheckpoint>,
    pub checkpoint_fingerprint: Option<String>,
}

impl StageResumeDecision {
    pub fn reused(&self) -> bool {
        matches!(
            self.action,
            StageResumeAction::Skip | StageResumeAction::Rehydrate
        )
    }
}

#[derive(Debug, Clone, Default)]
pub struct StageResumePolicy {
    decisions: HashMap<String, StageResumeDecision>,
}

impl StageResumePolicy {
    pub fn empty() -> Self {
        Self {
            decisions: HashMap::new(),
        }
    }

    pub fn from_state(state: &StateBundle) -> Self {
        let mut decisions = HashMap::new();

        for checkpoint in &state.completed_stages {
            let fingerprint = fingerprint_value(&checkpoint.outputs);
            decisions.insert(
                checkpoint.stage_name.clone(),
                StageResumeDecision {
                    stage_name: checkpoint.stage_name.clone(),
                    stage_context: checkpoint.stage_context,
                    action: StageResumeAction::Skip,
                    checkpoint: Some(checkpoint.clone()),
                    checkpoint_fingerprint: fingerprint,
                },
            );
        }

        if let Some(failed) = &state.failed_stage {
            let fingerprint = state
                .completed_stages
                .iter()
                .rev()
                .find(|cp| cp.stage_name == failed.stage_name)
                .and_then(|checkpoint| fingerprint_value(&checkpoint.outputs));

            decisions.insert(
                failed.stage_name.clone(),
                StageResumeDecision {
                    stage_name: failed.stage_name.clone(),
                    stage_context: if failed.stage_context == StageContext::Unknown {
                        state.stage_context
                    } else {
                        failed.stage_context
                    },
                    action: StageResumeAction::Recompute,
                    checkpoint: state
                        .completed_stages
                        .iter()
                        .rev()
                        .find(|cp| cp.stage_name == failed.stage_name)
                        .cloned(),
                    checkpoint_fingerprint: fingerprint,
                },
            );
        }

        Self { decisions }
    }

    pub fn decision_for(&self, stage_name: &str) -> Option<&StageResumeDecision> {
        self.decisions.get(stage_name)
    }

    pub fn should_skip(&self, stage_name: &str) -> bool {
        self.decisions
            .get(stage_name)
            .map(|decision| decision.action == StageResumeAction::Skip)
            .unwrap_or(false)
    }

    pub fn reused_stages(&self) -> Vec<&StageResumeDecision> {
        self.decisions
            .values()
            .filter(|decision| decision.reused())
            .collect()
    }
}

fn fingerprint_value(value: &Value) -> Option<String> {
    if value.is_null() {
        return None;
    }

    match serde_json::to_vec(value) {
        Ok(bytes) => Some(blake3::hash(&bytes).to_hex().to_string()),
        Err(_) => None,
    }
}

#[derive(Debug, Clone)]
pub struct StageCheckpointRequest {
    pub stage_name: Option<String>,
    pub outputs: Option<Value>,
}

impl StageCheckpointRequest {
    pub fn new(stage_name: impl Into<String>) -> Self {
        Self {
            stage_name: Some(stage_name.into()),
            outputs: None,
        }
    }

    pub fn with_outputs(stage_name: impl Into<String>, outputs: Value) -> Self {
        Self {
            stage_name: Some(stage_name.into()),
            outputs: Some(outputs),
        }
    }
}

#[derive(Debug, Default, Clone)]
pub struct FailedStageUpdate {
    pub stage_name: Option<String>,
    pub error: Option<String>,
    pub retry_count: Option<u32>,
}

#[derive(Debug, Default, Clone)]
pub struct TransitionContext {
    pub llm_reasoning: Option<String>,
    pub observations: Vec<ObservationAsset>,
    pub slot_deltas: Vec<SlotDelta>,
    // NOTE: atomic_plan removed - plan is now stored ONLY in turn storage
    // (strategy_attempts[last].exploration_result.plan). Use AskLoopApi::get_plan_for_execution().
    pub resolved_slots: Vec<SlotRecord>,
    pub confidence_overall: Option<f64>,
    pub confidence_per_slot: Option<HashMap<String, f64>>,
    pub budget_remaining: Option<f64>,
    pub budget_spend: Option<(f64, String)>,
    pub stage_context: Option<StageContext>,
    pub stage_checkpoint: Option<StageCheckpointRequest>,
    pub failed_stage: Option<FailedStageUpdate>,
}

/// Lightweight state tracker facade built on top of the conversation store.
/// Planning flow appends immutable state snapshots that can be replayed later
/// when execution mode is introduced.
pub struct StateTracker {
    store: Arc<dyn V2ConversationStore>,
    confidence_service: Arc<ConfidenceService>,
    evidence_broker: EvidenceBroker,
}

impl StateTracker {
    pub fn new(store: Arc<dyn V2ConversationStore>) -> Self {
        Self::with_confidence_service(store, Arc::new(ConfidenceService::default()))
    }

    pub fn with_confidence_service(
        store: Arc<dyn V2ConversationStore>,
        confidence_service: Arc<ConfidenceService>,
    ) -> Self {
        Self {
            store,
            confidence_service,
            evidence_broker: EvidenceBroker,
        }
    }

    pub fn confidence_service(&self) -> Arc<ConfidenceService> {
        Arc::clone(&self.confidence_service)
    }

    /// Get access to the underlying conversation store for turn retrieval.
    /// Used by AskLoopApi to restore clarification questions from persisted turns.
    pub fn conversation_store(&self) -> Arc<dyn V2ConversationStore> {
        Arc::clone(&self.store)
    }

    /// Append or update a stage checkpoint for the given workflow.
    ///
    /// If no prior state exists, a new bundle is created to host the checkpoint.
    pub async fn append_stage_checkpoint(
        &self,
        workflow_id: &str,
        stage_context: StageContext,
        stage_name: &str,
        outputs: Value,
    ) -> Result<StateBundle, V2StorageError> {
        let request = StageCheckpointRequest::with_outputs(stage_name, outputs.clone());
        let latest = self.store.get_latest_state(workflow_id).await?;

        if let Some(state) = latest {
            self.transition(
                workflow_id,
                state.current_state,
                state.current_state,
                TransitionContext {
                    stage_context: Some(stage_context),
                    stage_checkpoint: Some(request),
                    ..TransitionContext::default()
                },
            )
            .await
        } else {
            let now = Utc::now();
            let bundle = StateBundle {
                state_id: Uuid::new_v4().to_string(),
                workflow_id: workflow_id.to_string(),
                current_state: WorkflowState::Hypothesize,
                llm_reasoning: None,
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence: ConfidenceScore::default(),
                budget: BudgetState::default(),
                stage_context,
                completed_stages: vec![StageCheckpoint {
                    stage_name: stage_name.to_string(),
                    stage_context,
                    completed_at: now,
                    outputs,
                }],
                failed_stage: None,
                created_at: now,
            };
            self.store.append_state(workflow_id, bundle.clone()).await?;
            Ok(bundle)
        }
    }

    /// Record a failed stage transition for later resume handling.
    pub async fn record_stage_failure(
        &self,
        workflow_id: &str,
        stage_context: StageContext,
        stage_name: &str,
        error: impl Into<String>,
    ) -> Result<StateBundle, V2StorageError> {
        let latest = self.store.get_latest_state(workflow_id).await?;
        let error_message = error.into();
        let failure_update = FailedStageUpdate {
            stage_name: Some(stage_name.to_string()),
            error: Some(error_message.clone()),
            retry_count: Some(0),
        };

        if let Some(state) = latest {
            self.transition(
                workflow_id,
                state.current_state,
                WorkflowState::Failed,
                TransitionContext {
                    stage_context: Some(stage_context),
                    failed_stage: Some(failure_update),
                    ..TransitionContext::default()
                },
            )
            .await
        } else {
            let now = Utc::now();
            let bundle = StateBundle {
                state_id: Uuid::new_v4().to_string(),
                workflow_id: workflow_id.to_string(),
                current_state: WorkflowState::Failed,
                llm_reasoning: None,
                observations: Vec::new(),
                slot_deltas: Vec::new(),
                confidence: ConfidenceScore::default(),
                budget: BudgetState::default(),
                stage_context,
                completed_stages: Vec::new(),
                failed_stage: Some(FailedStageInfo {
                    stage_name: stage_name.to_string(),
                    stage_context,
                    error: error_message,
                    retry_count: 0,
                    failed_at: now,
                }),
                created_at: now,
            };
            self.store.append_state(workflow_id, bundle.clone()).await?;
            Ok(bundle)
        }
    }

    /// Validate a state transition and persist the resulting snapshot.
    pub async fn transition(
        &self,
        workflow_id: &str,
        from: WorkflowState,
        to: WorkflowState,
        context: TransitionContext,
    ) -> Result<StateBundle, V2StorageError> {
        let latest = self.store.get_latest_state(workflow_id).await?;
        let mut context = context;

        if let Some(ref current) = latest {
            if current.current_state != from {
                return Err(V2StorageError::Storage(format!(
                    "State mismatch: expected {:?} but current state is {:?}",
                    from, current.current_state
                )));
            }
        }

        validate_transition(from, to)?;

        let now = Utc::now();
        let mut stage = context.stage_context.unwrap_or_else(|| {
            latest
                .as_ref()
                .map(|state| state.stage_context)
                .unwrap_or_default()
        });
        if stage == StageContext::Unknown {
            if let Some(previous) = latest.as_ref() {
                stage = previous.stage_context;
            }
        }
        context.stage_context = Some(stage);

        let broker_outcome =
            self.evidence_broker
                .normalize(latest.as_ref(), &mut context, stage, now);

        let mut confidence = latest
            .as_ref()
            .map(|state| state.confidence.clone())
            .unwrap_or_default();
        self.apply_confidence_update(&mut confidence, &context, now);

        if !broker_outcome.removed_slots.is_empty() {
            for slot_id in &broker_outcome.removed_slots {
                confidence.slot_records.remove(slot_id);
                confidence.per_slot.remove(slot_id);
            }
            if confidence.slot_records.is_empty() {
                confidence.summary = None;
            } else {
                self.integrate_slot_records(&mut confidence, &[]);
            }
        }

        // NOTE: atomic_plan is no longer stored in StateBundle.
        // Plan is stored ONLY in turn storage (strategy_attempts[last].exploration_result.plan).

        let mut budget = latest
            .as_ref()
            .map(|state| state.budget.clone())
            .unwrap_or_default();
        update_budget(&mut budget, &context, now, stage);

        let mut completed_stages = latest
            .as_ref()
            .map(|state| state.completed_stages.clone())
            .unwrap_or_default();
        let mut failed_stage = latest.as_ref().and_then(|state| state.failed_stage.clone());

        if to == WorkflowState::Failed && context.stage_checkpoint.is_none() {
            let stage_ctx = stage;
            let update = context.failed_stage.clone().unwrap_or_default();

            let stage_name = update
                .stage_name
                .or_else(|| {
                    context
                        .stage_checkpoint
                        .as_ref()
                        .and_then(|req| req.stage_name.clone())
                })
                .unwrap_or_else(|| stage_ctx.as_str().to_string());

            let error = update.error.unwrap_or_else(|| "stage failed".to_string());

            let retry_count = update.retry_count.unwrap_or_else(|| {
                failed_stage
                    .as_ref()
                    .filter(|info| info.stage_name == stage_name)
                    .map(|info| info.retry_count + 1)
                    .unwrap_or(0)
            });

            failed_stage = Some(FailedStageInfo {
                stage_name,
                stage_context: stage_ctx,
                error,
                retry_count,
                failed_at: now,
            });
        } else if let Some(checkpoint_req) = context.stage_checkpoint.as_ref() {
            let stage_ctx = stage;
            let mut stage_name = checkpoint_req
                .stage_name
                .clone()
                .unwrap_or_else(|| stage_ctx.as_str().to_string());
            if stage_name.is_empty() {
                stage_name = stage_ctx.as_str().to_string();
            }

            let outputs = checkpoint_req.outputs.clone().unwrap_or(Value::Null);

            completed_stages.retain(|cp| cp.stage_name != stage_name);
            completed_stages.push(StageCheckpoint {
                stage_name: stage_name.clone(),
                stage_context: stage_ctx,
                completed_at: now,
                outputs,
            });

            if failed_stage
                .as_ref()
                .map(|info| info.stage_name == stage_name || info.stage_context == stage_ctx)
                .unwrap_or(false)
            {
                failed_stage = None;
            }
        } else if failed_stage.is_some() && to != WorkflowState::Failed {
            failed_stage = None;
        }

        let bundle = StateBundle {
            state_id: Uuid::new_v4().to_string(),
            workflow_id: workflow_id.to_string(),
            current_state: to,
            llm_reasoning: context.llm_reasoning.clone(),
            observations: context.observations.clone(),
            slot_deltas: context.slot_deltas.clone(),
            confidence,
            budget,
            stage_context: stage,
            completed_stages,
            failed_stage,
            created_at: now,
        };

        self.store.append_state(workflow_id, bundle.clone()).await?;
        Ok(bundle)
    }

    /// Append a new state snapshot for the given workflow execution.
    pub async fn record_state(&self, mut bundle: StateBundle) -> Result<(), V2StorageError> {
        if bundle.workflow_id.is_empty() {
            return Err(V2StorageError::Storage(
                "State bundle missing workflow_id".to_string(),
            ));
        }

        if bundle.created_at.timestamp_millis() == 0 {
            bundle.created_at = Utc::now();
        }

        let workflow_id = bundle.workflow_id.clone();
        self.store.append_state(&workflow_id, bundle).await
    }

    /// Retrieve all stored snapshots for a workflow.
    pub async fn list_states(
        &self,
        execution_id: &str,
    ) -> Result<Vec<StateBundle>, V2StorageError> {
        self.store.get_states(execution_id).await
    }

    /// Retrieve the most recent state snapshot for an execution, if any.
    pub async fn latest_state(
        &self,
        execution_id: &str,
    ) -> Result<Option<StateBundle>, V2StorageError> {
        self.store.get_latest_state(execution_id).await
    }

    fn apply_confidence_update(
        &self,
        confidence: &mut ConfidenceScore,
        context: &TransitionContext,
        now: DateTime<Utc>,
    ) {
        if let Some(ref per_slot) = context.confidence_per_slot {
            confidence.per_slot = per_slot.clone();
        }

        let mut derived_overall = None;

        if !context.resolved_slots.is_empty() {
            derived_overall = self
                .integrate_slot_records(confidence, &context.resolved_slots)
                .map(|summary| summary.overall);
        } else if !confidence.slot_records.is_empty() && confidence.summary.is_none() {
            derived_overall = self
                .integrate_slot_records(confidence, &[])
                .map(|summary| summary.overall);
        } else if let Some(summary) = confidence.summary.as_ref() {
            derived_overall = Some(summary.overall);
        }

        let new_overall = context
            .confidence_overall
            .or(derived_overall)
            .unwrap_or(confidence.overall);

        confidence.overall = new_overall;
        confidence
            .history
            .push((now.timestamp_millis(), new_overall));
        confidence.slope = self.compute_confidence_slope(&confidence.history);
    }

    pub fn recalculate_confidence_slope(&self, confidence: &mut ConfidenceScore) {
        confidence.slope = self.compute_confidence_slope(&confidence.history);
    }

    fn compute_confidence_slope(&self, history: &[(i64, f64)]) -> Option<f64> {
        if history.len()
            < self
                .confidence_service
                .config()
                .min_samples_for_slope
                .max(2)
        {
            // Fast path: rely on service to enforce window only when enough samples exist.
            // If insufficient points, skip allocation altogether.
            return None;
        }

        let data: Vec<(DateTime<Utc>, f64)> = history
            .iter()
            .filter_map(|(ts, value)| {
                Utc.timestamp_millis_opt(*ts)
                    .single()
                    .map(|dt| (dt, *value))
            })
            .collect();

        if data.is_empty() {
            None
        } else {
            self.confidence_service.calculate_confidence_slope(&data)
        }
    }

    pub fn integrate_slot_records(
        &self,
        confidence: &mut ConfidenceScore,
        slots: &[SlotRecord],
    ) -> Option<ConfidenceSummary> {
        if slots.is_empty() && confidence.slot_records.is_empty() {
            confidence.summary = None;
            return None;
        }

        if !slots.is_empty() {
            for slot in slots {
                let mut snapshot = slot.clone();
                // For user-provided slots (UserReply provenance), use per_slot if already set
                // (explicit confidence values), otherwise calculate with provenance boost.
                // For other slots, use per_slot if available, otherwise calculate.
                let is_user_reply = slot.provenance.iter().any(|p| {
                    matches!(
                        p.source,
                        crate::magician_v2::slot_graph::ProvenanceSource::UserReply
                    )
                });

                let score = if is_user_reply {
                    // User answered - update confidence based on whether slot exists and matches per_slot
                    let existing_record = confidence.slot_records.get(&snapshot.id);
                    let per_slot_value = confidence.per_slot.get(&snapshot.id).copied();

                    match (existing_record, per_slot_value) {
                        (Some(existing), Some(per_slot_conf))
                            if (existing.confidence - per_slot_conf).abs() < 1e-6 =>
                        {
                            // Slot exists with same confidence as per_slot - this is stale, recalculate
                            let calculated =
                                self.confidence_service.calculate_slot_confidence(&snapshot);
                            confidence.per_slot.insert(snapshot.id.clone(), calculated);
                            calculated
                        },
                        (_, Some(per_slot_conf)) => {
                            // per_slot exists but differs from existing slot - use per_slot (explicit override)
                            per_slot_conf
                        },
                        _ => {
                            // No per_slot - calculate and store
                            let calculated =
                                self.confidence_service.calculate_slot_confidence(&snapshot);
                            confidence.per_slot.insert(snapshot.id.clone(), calculated);
                            calculated
                        },
                    }
                } else {
                    // Use confidence from per_slot if available, otherwise calculate
                    confidence
                        .per_slot
                        .get(&snapshot.id)
                        .copied()
                        .unwrap_or_else(|| {
                            self.confidence_service.calculate_slot_confidence(&snapshot)
                        })
                };
                snapshot.confidence = score;
                confidence
                    .slot_records
                    .insert(snapshot.id.clone(), snapshot);
            }
        }

        if confidence.slot_records.is_empty() {
            confidence.summary = None;
            return None;
        }

        // Update per_slot map with slot_records values (for slots not already in per_slot)
        for (id, record) in &confidence.slot_records {
            confidence
                .per_slot
                .entry(id.clone())
                .or_insert(record.confidence);
        }

        let all_slots: Vec<SlotRecord> = confidence.slot_records.values().cloned().collect();

        let summary = self.confidence_service.summarize_confidence(&all_slots);
        confidence.summary = Some(summary.clone());
        confidence.overall = summary.overall;
        Some(summary)
    }

    /// Store enriched query for a turn after batch completion (Gap #5 fix)
    ///
    /// Called after all batch answers are collected and query rewriting is complete.
    /// Stores the enriched query so the orchestrator can use it during resume instead
    /// of the stale original query.
    pub async fn store_enriched_query(
        &self,
        execution_id: &str,
        turn_id: &str,
        enriched_query: String,
    ) -> Result<(), V2StorageError> {
        // Load existing enriched query to detect overwrites
        let existing = self.load_enriched_query(execution_id).await.ok().flatten();

        if let Some(ref old_query) = existing {
            // ENHANCED LOGGING: Character count comparison and content change detection
            let old_len = old_query.len();
            let new_len = enriched_query.len();
            let len_diff = new_len as i64 - old_len as i64;
            let len_change_pct = if old_len > 0 {
                (len_diff as f64 / old_len as f64) * 100.0
            } else {
                0.0
            };

            if old_query == &enriched_query {
                info!(
                    "[MAGICIAN-QUERY-ENRICHMENT] ✅ QUERY UNCHANGED for execution {}, turn {}\n\
                     - Query length: {} chars\n\
                     - Query: '{}'",
                    execution_id, turn_id, new_len, enriched_query
                );
            } else {
                // Detect type of change (addition, replacement, etc.)
                let change_type = if old_query.is_empty() && !enriched_query.is_empty() {
                    "FIRST-TIME POPULATION"
                } else if enriched_query.contains(old_query) {
                    "INCREMENTAL ADDITION"
                } else if old_query.contains(&enriched_query) {
                    "CONTENT REDUCTION"
                } else {
                    "COMPLETE REPLACEMENT"
                };

                warn!(
                    "[MAGICIAN-QUERY-ENRICHMENT] ⚠️  OVERWRITING ENRICHED QUERY for execution {}, turn {}\n\
                     - Change type: {}\n\
                     - Old length: {} chars\n\
                     - New length: {} chars\n\
                     - Length diff: {:+} chars ({:+.1}%)\n\
                     - Old query: '{}'\n\
                     - New query: '{}'",
                    execution_id,
                    turn_id,
                    change_type,
                    old_len,
                    new_len,
                    len_diff,
                    len_change_pct,
                    old_query,
                    enriched_query
                );
            }
        } else {
            info!(
                "[MAGICIAN-QUERY-ENRICHMENT] ✅ STORING NEW ENRICHED QUERY for execution {}, turn {}\n\
                 - Query length: {} chars\n\
                 - Query: '{}'",
                execution_id, turn_id, enriched_query.len(), enriched_query
            );
        }

        // Call trait method directly - no downcast needed! (Issue #2 fix)
        self.store
            .update_turn_enriched_query(execution_id, turn_id, Some(enriched_query))
            .await
    }

    /// Get the ID of the current (latest) turn in an execution (Gap #10 fix)
    ///
    /// Used when storing enriched queries - we need to know which turn to update.
    pub async fn get_current_turn_id(&self, execution_id: &str) -> Result<String, V2StorageError> {
        let turns = self.store.get_turns(execution_id).await?;
        turns.last().map(|turn| turn.id.clone()).ok_or_else(|| {
            V2StorageError::TurnNotFound(format!("No turns found for execution {}", execution_id))
        })
    }

    /// Load enriched query for an execution from the latest turn (Part 3D fix)
    ///
    /// Called by orchestrator during resume to check if we have a query that
    /// incorporates all batch answers. If present, this should be used instead
    /// of the original query to avoid re-eliciting already-answered questions.
    pub async fn load_enriched_query(
        &self,
        execution_id: &str,
    ) -> Result<Option<String>, V2StorageError> {
        let turns = self.store.get_turns(execution_id).await?;
        let enriched = turns
            .iter()
            .rev()
            .find_map(|turn| turn.enriched_query.clone());

        if let Some(ref query) = enriched {
            info!(
                "[MAGICIAN-QUERY-ENRICHMENT] ✓ Loaded enriched query for execution {}\nQuery: '{}'",
                execution_id, query
            );
        } else {
            debug!(
                "[MAGICIAN-QUERY-ENRICHMENT] No enriched query found for execution {}",
                execution_id
            );
        }

        Ok(enriched)
    }

    /// Determine the current query to feed into the rewriter.
    ///
    /// Prefers the latest enriched query, falling back to the first inbound
    /// user turn if no enrichment has been recorded yet.
    pub async fn get_query_for_rewrite(
        &self,
        execution_id: &str,
    ) -> Result<String, V2StorageError> {
        if let Some(enriched) = self.load_enriched_query(execution_id).await? {
            return Ok(enriched);
        }

        let turns = self.store.get_turns(execution_id).await?;
        let original = turns
            .into_iter()
            .find(|turn| matches!(turn.direction, TurnDirection::Inbound))
            .map(|turn| turn.text)
            .unwrap_or_default();

        Ok(original)
    }
}

#[derive(Default)]
struct EvidenceBroker;

struct BrokerOutcome {
    removed_slots: Vec<String>,
}

#[derive(Default)]
struct ObservationSummary {
    per_slot: HashMap<String, Vec<ObservationEvidence>>,
    global: Vec<ObservationEvidence>,
}

#[derive(Clone)]
struct ObservationEvidence {
    link: String,
    source: ProvenanceSource,
}

impl EvidenceBroker {
    fn normalize(
        &self,
        latest: Option<&StateBundle>,
        context: &mut TransitionContext,
        stage: StageContext,
        timestamp: DateTime<Utc>,
    ) -> BrokerOutcome {
        let new_observations = std::mem::take(&mut context.observations);
        let (merged_observations, summary) =
            self.merge_observations(latest, new_observations, stage);
        context.observations = merged_observations;

        for delta in &mut context.slot_deltas {
            if delta.stage == StageContext::Unknown {
                delta.stage = stage;
            }
        }

        let provided_slots = std::mem::take(&mut context.resolved_slots);
        let (resolved_slots, removed_slots) = self.merge_slot_updates(
            latest,
            provided_slots,
            &context.slot_deltas,
            stage,
            timestamp,
            &summary,
            context.confidence_per_slot.as_ref(),
        );
        context.resolved_slots = resolved_slots;

        BrokerOutcome { removed_slots }
    }

    fn merge_observations(
        &self,
        latest: Option<&StateBundle>,
        mut new_observations: Vec<ObservationAsset>,
        stage: StageContext,
    ) -> (Vec<ObservationAsset>, ObservationSummary) {
        let mut merged = latest
            .map(|bundle| bundle.observations.clone())
            .unwrap_or_default();
        let mut by_id: HashMap<String, usize> = HashMap::new();
        for (index, observation) in merged.iter_mut().enumerate() {
            by_id.insert(observation.asset_id.clone(), index);
            observation
                .metadata
                .entry(STAGE_METADATA_KEY.to_string())
                .or_insert_with(|| stage.as_str().to_string());
        }

        let mut summary = ObservationSummary::default();
        let stage_label = stage.as_str().to_string();

        for mut observation in new_observations.drain(..) {
            observation
                .metadata
                .entry(STAGE_METADATA_KEY.to_string())
                .or_insert_with(|| stage_label.clone());

            if let Some(entry) = self.collect_evidence(&observation, stage) {
                summary.extend(entry);
            }

            if let Some(existing) = by_id.get(&observation.asset_id).copied() {
                merged[existing] = observation;
            } else {
                by_id.insert(observation.asset_id.clone(), merged.len());
                merged.push(observation);
            }
        }

        (merged, summary)
    }

    fn collect_evidence(
        &self,
        observation: &ObservationAsset,
        stage: StageContext,
    ) -> Option<ObservationEvidenceEntry> {
        let link = format!("obs:{}:{}", stage.as_str(), observation.asset_id);
        let source = Self::source_for_asset(&observation.asset_type);
        let targets = Self::extract_slot_targets(&observation.metadata);
        Some(ObservationEvidenceEntry {
            targets,
            evidence: ObservationEvidence { link, source },
        })
    }

    fn merge_slot_updates(
        &self,
        latest: Option<&StateBundle>,
        provided_slots: Vec<SlotRecord>,
        slot_deltas: &[SlotDelta],
        stage: StageContext,
        timestamp: DateTime<Utc>,
        summary: &ObservationSummary,
        confidence_per_slot: Option<&HashMap<String, f64>>,
    ) -> (Vec<SlotRecord>, Vec<String>) {
        let mut resolved_map: HashMap<String, SlotRecord> = HashMap::new();
        for mut slot in provided_slots {
            self.annotate_slot(&mut slot, stage, timestamp, summary, confidence_per_slot);
            resolved_map.insert(slot.id.clone(), slot);
        }

        let latest_map = latest.map(|bundle| bundle.confidence.slot_records.clone());
        let mut removed = Vec::new();

        for delta in slot_deltas {
            match delta.operation {
                super::DeltaOperation::Delete => {
                    removed.push(delta.slot_id.clone());
                    resolved_map.remove(&delta.slot_id);
                },
                super::DeltaOperation::Create | super::DeltaOperation::Update => {
                    let slot = resolved_map
                        .entry(delta.slot_id.clone())
                        .or_insert_with(|| {
                            latest_map
                                .as_ref()
                                .and_then(|map| map.get(&delta.slot_id).cloned())
                                .unwrap_or_else(|| SlotRecord {
                                    id: delta.slot_id.clone(),
                                    slot_type: crate::magician_v2::slot_graph::SlotType::Modifier,
                                    value: delta.new_value.clone(),
                                    confidence: 0.0,
                                    provenance: Vec::new(),
                                    evidence_links: Vec::new(),
                                    created_at: timestamp,
                                    updated_at: timestamp,
                                })
                        });
                    slot.value = delta.new_value.clone();
                    self.annotate_slot(slot, stage, timestamp, summary, confidence_per_slot);
                },
            }
        }

        let mut resolved: Vec<SlotRecord> = resolved_map.into_values().collect();
        resolved.sort_by(|a, b| a.id.cmp(&b.id));
        (resolved, removed)
    }

    fn annotate_slot(
        &self,
        slot: &mut SlotRecord,
        stage: StageContext,
        timestamp: DateTime<Utc>,
        summary: &ObservationSummary,
        confidence_per_slot: Option<&HashMap<String, f64>>,
    ) {
        if let Some(scores) = confidence_per_slot {
            if let Some(score) = scores.get(&slot.id) {
                slot.confidence = *score;
            }
        }

        Self::dedup_push(
            &mut slot.evidence_links,
            format!("stage:{}", stage.as_str()),
        );

        let mut sources_to_add: HashSet<ProvenanceSource> = HashSet::new();

        if let Some(entries) = summary.per_slot.get(&slot.id) {
            for evidence in entries {
                Self::dedup_push(&mut slot.evidence_links, evidence.link.clone());
                sources_to_add.insert(evidence.source);
            }
        } else if !summary.global.is_empty() {
            for evidence in &summary.global {
                Self::dedup_push(&mut slot.evidence_links, evidence.link.clone());
                sources_to_add.insert(evidence.source);
            }
        } else {
            sources_to_add.insert(Self::default_stage_source(stage));
        }

        let mut existing_sources: HashSet<ProvenanceSource> =
            slot.provenance.iter().map(|p| p.source).collect();

        for source in sources_to_add {
            if existing_sources.insert(source) {
                slot.provenance.push(ProvenanceRecord { source, timestamp });
            }
        }

        slot.touch();
    }

    fn dedup_push(list: &mut Vec<String>, value: String) {
        if !list.iter().any(|existing| existing == &value) {
            list.push(value);
        }
    }

    fn extract_slot_targets(metadata: &HashMap<String, String>) -> Vec<String> {
        if let Some(single) = metadata.get("slot_id") {
            return single
                .split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect();
        }

        if let Some(raw) = metadata.get("slot_ids") {
            if raw.trim().starts_with('[') {
                if let Ok(parsed) = serde_json::from_str::<Vec<String>>(raw) {
                    return parsed.into_iter().map(|id| id.trim().to_string()).collect();
                }
            }
            return raw
                .split(',')
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .collect();
        }

        Vec::new()
    }

    fn source_for_asset(asset_type: &AssetType) -> ProvenanceSource {
        match asset_type {
            AssetType::UserMessage => ProvenanceSource::UserReply,
            AssetType::Screenshot => ProvenanceSource::ScreenshotInference,
            AssetType::DomSummary | AssetType::Transcript | AssetType::ToolOutput => {
                ProvenanceSource::DeterministicCheck
            },
        }
    }

    fn default_stage_source(stage: StageContext) -> ProvenanceSource {
        match stage {
            StageContext::PlanningBootstrap
            | StageContext::PlanningIteration
            | StageContext::Unknown => ProvenanceSource::LlmPrimary,
            StageContext::ExecutionCycle => ProvenanceSource::DeterministicCheck,
            StageContext::FollowUp => ProvenanceSource::UserReply,
        }
    }
}

struct ObservationEvidenceEntry {
    targets: Vec<String>,
    evidence: ObservationEvidence,
}

impl ObservationSummary {
    fn extend(&mut self, entry: ObservationEvidenceEntry) {
        if entry.targets.is_empty() {
            self.global.push(entry.evidence);
            return;
        }

        for target in entry.targets {
            self.per_slot
                .entry(target)
                .or_default()
                .push(entry.evidence.clone());
        }
    }
}

fn validate_transition(from: WorkflowState, to: WorkflowState) -> Result<(), V2StorageError> {
    use WorkflowState::*;
    if from == to {
        return Ok(());
    }
    let valid = matches!(
        (from, to),
        (Observe, Hypothesize)
            | (Hypothesize, Act)
            | (Hypothesize, Clarify)
            | (Act, Verify)
            | (Verify, Observe)
            | (Verify, Complete)
            | (Clarify, Pause)
            | (Pause, Hypothesize)
            | (_, Failed)
    );

    if valid {
        Ok(())
    } else {
        Err(V2StorageError::Storage(format!(
            "Invalid state transition: {:?} -> {:?}",
            from, to
        )))
    }
}

fn update_budget(
    budget: &mut BudgetState,
    context: &TransitionContext,
    now: chrono::DateTime<Utc>,
    stage: StageContext,
) {
    if let Some(remaining) = context.budget_remaining {
        if budget.initial == 0.0 {
            budget.initial = remaining;
        }
        budget.remaining = remaining.max(0.0);
    }

    if let Some((amount, reason)) = context.budget_spend.clone() {
        if amount > 0.0 {
            budget.remaining = (budget.remaining - amount).max(0.0);
            budget.spent.push(BudgetSpend {
                amount,
                reason,
                stage,
                timestamp: now,
            });
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::{
        orchestrator::v2_orchestrator::{
            PendingClarification, ProcessingMetadata, RecommendedQuestion,
        },
        state_tracker::AssetContent,
        storage::{
            ExecutionRun, ExecutionSummary, PaginatedResult, PaginationParams, StrategyAttempt,
            TurnDirection, V2ConversationStore, V2Slot, V2SlotStatus, V2StorageError, V2Turn,
            WaitingState,
        },
        AnalysisMetadata, UnifiedQueryAnalysis,
    };
    use async_trait::async_trait;
    use runtime_core::V2ConversationStore as CoreV2ConversationStore;
    use serde_json::json;
    use std::{collections::HashMap, sync::Arc};
    use tokio::sync::Mutex;

    #[derive(Default)]
    struct StubStore {
        states: Mutex<HashMap<String, Vec<StateBundle>>>,
    }

    #[async_trait]
    impl CoreV2ConversationStore for StubStore {
        type Error = V2StorageError;
        type Execution = crate::magician_v2::storage::ExecutionRun;
        type ExecutionSummary = ExecutionSummary;
        type ExecutionStatus = WaitingState;
        type Turn = V2Turn;
        type TurnDirection = crate::magician_v2::storage::TurnDirection;
        type Slot = V2Slot;
        type SlotStatus = V2SlotStatus;
        type StrategyAttempt = StrategyAttempt;
        type ProcessingMetadata =
            crate::magician_v2::orchestrator::v2_orchestrator::ProcessingMetadata;
        type UnifiedAnalysis = UnifiedQueryAnalysis;
        type AnalysisMetadata = AnalysisMetadata;
        type StateBundle = StateBundle;
        type RecommendedQuestion = RecommendedQuestion;
        type PendingClarification = PendingClarification;
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
            _waiting_state: WaitingState,
        ) -> Result<ExecutionRun, V2StorageError> {
            Err(V2StorageError::Storage(
                "create_execution_with_options unsupported in tests".into(),
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

        async fn get_execution(&self, _execution_id: &str) -> Result<ExecutionRun, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_execution unsupported in tests".into(),
            ))
        }

        async fn update_execution_entry_mode(
            &self,
            _execution_id: &str,
            _entry_mode: String,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_execution_entry_mode unsupported in tests".into(),
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
                "add_turn unsupported in tests".into(),
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
                "store_analysis unsupported in tests".into(),
            ))
        }

        async fn store_strategy_attempts(
            &self,
            _execution_id: &str,
            _turn_id: &str,
            _attempts: Vec<StrategyAttempt>,
            _processing_metadata: ProcessingMetadata,
            _recommended_questions: Option<Vec<RecommendedQuestion>>,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "store_strategy_attempts unsupported in tests".into(),
            ))
        }

        async fn get_turn(
            &self,
            _execution_id: &str,
            _turn_id: &str,
        ) -> Result<V2Turn, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_turn unsupported in tests".into(),
            ))
        }

        async fn get_turns(&self, _execution_id: &str) -> Result<Vec<V2Turn>, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_turns unsupported in tests".into(),
            ))
        }

        async fn get_latest_turn_with_analysis(
            &self,
            _execution_id: &str,
        ) -> Result<Option<V2Turn>, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_latest_turn_with_analysis unsupported in tests".into(),
            ))
        }

        async fn list_executions(
            &self,
            _principal: &str,
            _workspace: &str,
            _pagination: PaginationParams,
        ) -> Result<PaginatedResult<ExecutionSummary>, V2StorageError> {
            Err(V2StorageError::Storage(
                "list_executions unsupported in tests".into(),
            ))
        }

        async fn delete_execution(&self, _execution_id: &str) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "delete_execution unsupported in tests".into(),
            ))
        }

        async fn get_turns_paginated(
            &self,
            _execution_id: &str,
            _pagination: PaginationParams,
            _direction_filter: Option<TurnDirection>,
        ) -> Result<PaginatedResult<V2Turn>, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_turns_paginated unsupported in tests".into(),
            ))
        }

        async fn update_execution_status(
            &self,
            _execution_id: &str,
            _status: WaitingState,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_execution_status unsupported in tests".into(),
            ))
        }

        async fn compare_exchange_execution_status(
            &self,
            _execution_id: &str,
            _expected: WaitingState,
            _status: WaitingState,
        ) -> Result<bool, V2StorageError> {
            Err(V2StorageError::Storage(
                "compare_exchange_execution_status unsupported in tests".into(),
            ))
        }

        async fn compare_exchange_execution_status_at(
            &self,
            _execution_id: &str,
            _expected: WaitingState,
            _expected_status_revision: u64,
            _status: WaitingState,
        ) -> Result<bool, V2StorageError> {
            Err(V2StorageError::Storage(
                "compare_exchange_execution_status_at unsupported in tests".into(),
            ))
        }

        async fn get_execution_status_revision(
            &self,
            _execution_id: &str,
        ) -> Result<u64, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_execution_status_revision unsupported in tests".into(),
            ))
        }

        async fn bind_execution_scope(
            &self,
            _execution_id: &str,
            _task_id: Option<String>,
            _root_execution_id: Option<String>,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "bind_execution_scope unsupported in tests".into(),
            ))
        }

        async fn add_child_execution_id(
            &self,
            _parent_execution_id: &str,
            _child_execution_id: &str,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "add_child_execution_id unsupported in tests".into(),
            ))
        }

        async fn update_execution_owner_snapshot(
            &self,
            _execution_id: &str,
            _active_owner_agent_id: &str,
            _owner_stack: &[String],
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_execution_owner_snapshot unsupported in tests".into(),
            ))
        }

        async fn replace_active_delegation_group(
            &self,
            _execution_id: &str,
            _active_delegation_group: &[String],
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "replace_active_delegation_group unsupported in tests".into(),
            ))
        }

        async fn get_execution_status(
            &self,
            _execution_id: &str,
        ) -> Result<WaitingState, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_execution_status unsupported in tests".into(),
            ))
        }

        async fn update_processing_correlation_id(
            &self,
            _execution_id: &str,
            _correlation_id: Option<String>,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_processing_correlation_id unsupported in tests".into(),
            ))
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
                "create_slot unsupported in tests".into(),
            ))
        }

        async fn update_slot_answer(
            &self,
            _execution_id: &str,
            _slot_id: &str,
            _answer: serde_json::Value,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_slot_answer unsupported in tests".into(),
            ))
        }

        async fn update_slot_status(
            &self,
            _execution_id: &str,
            _slot_id: &str,
            _status: V2SlotStatus,
        ) -> Result<(), V2StorageError> {
            Err(V2StorageError::Storage(
                "update_slot_status unsupported in tests".into(),
            ))
        }

        async fn get_slots(&self, _execution_id: &str) -> Result<Vec<V2Slot>, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_slots unsupported in tests".into(),
            ))
        }

        async fn get_pending_slots(
            &self,
            _execution_id: &str,
        ) -> Result<Vec<V2Slot>, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_pending_slots unsupported in tests".into(),
            ))
        }

        async fn get_slot(
            &self,
            _execution_id: &str,
            _slot_id: &str,
        ) -> Result<V2Slot, V2StorageError> {
            Err(V2StorageError::Storage(
                "get_slot unsupported in tests".into(),
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

        async fn get_states(&self, execution_id: &str) -> Result<Vec<StateBundle>, V2StorageError> {
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
                .and_then(|entries| entries.last().cloned()))
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

    fn tracker() -> StateTracker {
        let store: Arc<dyn V2ConversationStore> = Arc::new(StubStore::default());
        StateTracker::new(store)
    }

    fn sample_slot(id: &str) -> SlotRecord {
        let now = Utc::now();
        SlotRecord {
            id: id.to_string(),
            slot_type: crate::magician_v2::slot_graph::SlotType::Entity,
            value: json!({"name": "Launch Plan", "type": "project"}),
            confidence: 0.6,
            provenance: Vec::new(),
            evidence_links: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[tokio::test]
    async fn transition_enriches_slot_with_stage_evidence() {
        let tracker = tracker();
        let workflow_id = "wf-slot";
        let slot_id = format!("{workflow_id}::slot-1");
        let slot = sample_slot(&slot_id);

        let mut confidence = HashMap::new();
        confidence.insert(slot_id.clone(), 0.82);

        let mut metadata = HashMap::new();
        metadata.insert("slot_id".to_string(), slot_id.clone());

        let observation = ObservationAsset {
            asset_id: "obs-1".to_string(),
            asset_type: AssetType::UserMessage,
            content: AssetContent::Text("Launch is on Friday".to_string()),
            metadata,
        };

        let context = TransitionContext {
            observations: vec![observation],
            slot_deltas: vec![SlotDelta {
                slot_id: slot_id.clone(),
                operation: super::super::DeltaOperation::Update,
                previous_value: None,
                new_value: slot.value.clone(),
                stage: StageContext::Unknown,
            }],
            resolved_slots: vec![slot],
            confidence_per_slot: Some(confidence),
            stage_context: Some(StageContext::PlanningIteration),
            ..TransitionContext::default()
        };

        let bundle = tracker
            .transition(
                workflow_id,
                WorkflowState::Observe,
                WorkflowState::Hypothesize,
                context,
            )
            .await
            .expect("transition should succeed");

        let stage_label = StageContext::PlanningIteration.as_str().to_string();
        assert!(
            bundle
                .observations
                .iter()
                .any(|obs| obs.metadata.get(STAGE_METADATA_KEY) == Some(&stage_label)),
            "observation metadata should carry stage label"
        );

        let slot_record = bundle
            .confidence
            .slot_records
            .get(&slot_id)
            .expect("slot record exists");

        assert!(
            slot_record
                .evidence_links
                .iter()
                .any(|link| link.contains("obs:planning_iteration:obs-1")),
            "slot evidence links should reference observation"
        );

        assert!(
            slot_record
                .provenance
                .iter()
                .any(|prov| prov.source == ProvenanceSource::UserReply),
            "slot provenance should capture user reply source"
        );

        assert!(
            (slot_record.confidence - 0.82).abs() < 1e-6,
            "slot confidence should reflect supplied per-slot value"
        );

        assert!(
            bundle
                .slot_deltas
                .iter()
                .all(|delta| delta.stage == StageContext::PlanningIteration),
            "slot deltas should inherit stage context"
        );
    }

    #[tokio::test]
    async fn transition_removes_slot_on_delete_delta() {
        let tracker = tracker();
        let workflow_id = "wf-delete";
        let slot_id = format!("{workflow_id}::slot-1");
        let slot = sample_slot(&slot_id);

        let initial_context = TransitionContext {
            observations: Vec::new(),
            slot_deltas: vec![SlotDelta {
                slot_id: slot_id.clone(),
                operation: super::super::DeltaOperation::Create,
                previous_value: None,
                new_value: slot.value.clone(),
                stage: StageContext::PlanningBootstrap,
            }],
            resolved_slots: vec![slot],
            confidence_per_slot: None,
            stage_context: Some(StageContext::PlanningBootstrap),
            ..TransitionContext::default()
        };

        let first_bundle = tracker
            .transition(
                workflow_id,
                WorkflowState::Observe,
                WorkflowState::Hypothesize,
                initial_context,
            )
            .await
            .expect("initial transition succeeds");

        let removal_context = TransitionContext {
            observations: Vec::new(),
            slot_deltas: vec![SlotDelta {
                slot_id: slot_id.clone(),
                operation: super::super::DeltaOperation::Delete,
                previous_value: None,
                new_value: serde_json::Value::Null,
                stage: StageContext::PlanningIteration,
            }],
            resolved_slots: Vec::new(),
            confidence_per_slot: None,
            stage_context: Some(StageContext::PlanningIteration),
            ..TransitionContext::default()
        };

        let second_bundle = tracker
            .transition(
                workflow_id,
                first_bundle.current_state,
                first_bundle.current_state,
                removal_context,
            )
            .await
            .expect("removal transition succeeds");

        assert!(
            !second_bundle.confidence.slot_records.contains_key(&slot_id),
            "slot should be removed from confidence map"
        );

        assert_eq!(
            second_bundle.slot_deltas.first().map(|delta| delta.stage),
            Some(StageContext::PlanningIteration),
            "slot delta should be stage-tagged after removal"
        );
    }

    #[tokio::test]
    async fn transition_persists_stage_checkpoints() {
        let tracker = tracker();
        let workflow_id = "wf-checkpoint";

        let first = tracker
            .transition(
                workflow_id,
                WorkflowState::Observe,
                WorkflowState::Hypothesize,
                TransitionContext {
                    stage_context: Some(StageContext::PlanningBootstrap),
                    stage_checkpoint: Some(StageCheckpointRequest::with_outputs(
                        "planning.query_analysis",
                        json!({"status": "initial"}),
                    )),
                    ..TransitionContext::default()
                },
            )
            .await
            .expect("first transition succeeds");

        assert_eq!(first.completed_stages.len(), 1);
        let checkpoint = first
            .completed_stages
            .first()
            .expect("checkpoint present")
            .clone();
        assert_eq!(checkpoint.stage_name, "planning.query_analysis");
        assert_eq!(checkpoint.stage_context, StageContext::PlanningBootstrap);

        let second = tracker
            .transition(
                workflow_id,
                WorkflowState::Hypothesize,
                WorkflowState::Hypothesize,
                TransitionContext {
                    stage_context: Some(StageContext::PlanningBootstrap),
                    stage_checkpoint: Some(StageCheckpointRequest::with_outputs(
                        "planning.query_analysis",
                        json!({"status": "updated"}),
                    )),
                    ..TransitionContext::default()
                },
            )
            .await
            .expect("second transition succeeds");

        assert_eq!(second.completed_stages.len(), 1);
        assert_eq!(
            second.completed_stages[0].outputs,
            json!({"status": "updated"})
        );
        assert!(second.failed_stage.is_none());
    }

    #[tokio::test]
    async fn transition_records_failed_stage_with_retry() {
        let tracker = tracker();
        let workflow_id = "wf-failure";

        let first_failure = tracker
            .transition(
                workflow_id,
                WorkflowState::Observe,
                WorkflowState::Failed,
                TransitionContext {
                    stage_context: Some(StageContext::PlanningBootstrap),
                    failed_stage: Some(FailedStageUpdate {
                        stage_name: Some("planning.query_analysis".to_string()),
                        error: Some("timeout".to_string()),
                        retry_count: Some(0),
                    }),
                    ..TransitionContext::default()
                },
            )
            .await
            .expect("first failure transition succeeds");

        let failure_info = first_failure.failed_stage.expect("failed stage recorded");
        assert_eq!(failure_info.stage_name, "planning.query_analysis");
        assert_eq!(failure_info.retry_count, 0);
        assert_eq!(failure_info.error, "timeout");

        let second_failure = tracker
            .transition(
                workflow_id,
                WorkflowState::Failed,
                WorkflowState::Failed,
                TransitionContext {
                    stage_context: Some(StageContext::PlanningBootstrap),
                    failed_stage: Some(FailedStageUpdate {
                        stage_name: Some("planning.query_analysis".to_string()),
                        ..FailedStageUpdate::default()
                    }),
                    ..TransitionContext::default()
                },
            )
            .await
            .expect("second failure transition succeeds");

        let retry_info = second_failure
            .failed_stage
            .expect("failed stage persisted across retries");
        assert_eq!(retry_info.retry_count, 1);
        assert_eq!(retry_info.stage_name, "planning.query_analysis");
    }

    #[test]
    fn resume_policy_marks_completed_and_failed_stages() {
        let now = Utc::now();
        let completed_checkpoint = StageCheckpoint {
            stage_name: "execution.tool_runner".to_string(),
            stage_context: StageContext::ExecutionCycle,
            completed_at: now,
            outputs: json!({"tool": "bash", "artifact": "logs.tar.gz"}),
        };
        let state = StateBundle {
            state_id: "state-1".to_string(),
            workflow_id: "wf-policy".to_string(),
            current_state: WorkflowState::Failed,
            llm_reasoning: None,
            observations: Vec::new(),
            slot_deltas: Vec::new(),
            confidence: ConfidenceScore::default(),
            budget: BudgetState::default(),
            stage_context: StageContext::ExecutionCycle,
            completed_stages: vec![completed_checkpoint.clone()],
            failed_stage: Some(FailedStageInfo {
                stage_name: "execution.asset_reconcile".to_string(),
                stage_context: StageContext::ExecutionCycle,
                error: "network timeout".to_string(),
                retry_count: 2,
                failed_at: now,
            }),
            created_at: now,
        };

        let policy = StageResumePolicy::from_state(&state);

        let completed = policy
            .decision_for("execution.tool_runner")
            .expect("completed decision exists");
        assert_eq!(completed.action, StageResumeAction::Skip);
        assert!(completed.reused());
        assert!(
            completed.checkpoint_fingerprint.is_some(),
            "completed checkpoints should have fingerprints"
        );

        let failed = policy
            .decision_for("execution.asset_reconcile")
            .expect("failed decision exists");
        assert_eq!(failed.action, StageResumeAction::Recompute);
        assert!(
            failed.checkpoint.is_none(),
            "failure decision should not reuse checkpoint by default"
        );
    }
}

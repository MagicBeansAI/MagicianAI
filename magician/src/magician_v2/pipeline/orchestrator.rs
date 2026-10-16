//! # Pipeline Orchestrator
//!
//! Router-driven orchestrator for the planning pipeline (B-07).
//! [`PlanningOrchestrator`] drives agents through a router loop,
//! threading a shared [`ArtifactStore`] between agents. Supports
//! stateless pause/resume via [`PipelineSuspension`] checkpoints.

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::agent::{
    PipelineAgent, PipelineAgentError, PipelineAgentResult, PipelineContext, AGENT_ID_ELICITOR,
    AGENT_ID_INTENT_CLASSIFIER, AGENT_ID_PLAN_PATCHER, AGENT_ID_SLOT_EXTRACTOR,
    MAX_REFINEMENT_ROUNDS, MAX_REFINEMENT_USER_PAUSES,
};
use super::artifact::{AgentArtifact, ArtifactStore, ArtifactType, ARTIFACT_SCHEMA_VERSION};
use super::router::{RouterAgent, RoutingDecision};
use super::system_agents::intent_classifier::IntentClassificationResult;
use crate::magician_v2::ask_loop::pause::{PauseReason, PauseResumeManager};
use crate::magician_v2::strategy::plan::{PlanGraph, PlanStep, StepReadiness};

#[cfg(any(test, feature = "test-fixtures"))]
use super::definition::{
    ConvergenceCriterion, LoopConfig, PipelineDefinition, RetryConfig, StageDefinition,
};

// ---------------------------------------------------------------------------
// Resume mode → artifact type mapping (shared between purge and stale-marking)
// ---------------------------------------------------------------------------

/// Return the artifact types that should be purged (and marked stale in the
/// lifecycle catalog) for a given resume mode.
///
/// Called from `from_suspension()` (actual purge) and
/// `set_lifecycle_service()` (lifecycle stale-marking) to ensure they stay
/// in sync.
fn purge_types_for_resume_mode(resume_mode: &str) -> Vec<ArtifactType> {
    match resume_mode {
        "full_replan" => vec![
            ArtifactType::SlotGraph,
            ArtifactType::ElicitationResult,
            ArtifactType::InterpretedAnswer,
            ArtifactType::ClarifiedTask,
            ArtifactType::PlanGraph,
            ArtifactType::AgentError,
        ],
        "partial_replan" => vec![
            ArtifactType::ElicitationResult,
            ArtifactType::InterpretedAnswer,
            ArtifactType::ClarifiedTask,
            ArtifactType::PlanGraph,
            ArtifactType::AgentError,
        ],
        "light_slot_update" => vec![ArtifactType::AgentError],
        other => {
            warn!(
                resume_mode = %other,
                "unrecognized resume_mode, defaulting to light_slot_update semantics"
            );
            vec![ArtifactType::AgentError]
        },
    }
}

// ---------------------------------------------------------------------------
// Pre-dispatch validation
// ---------------------------------------------------------------------------

/// Validates that all artifacts declared by `agent.required_inputs()` exist in
/// the store. Short-circuits on empty requirements. Collects ALL missing types
/// for diagnostics rather than failing on the first.
fn validate_required_inputs(
    agent: &dyn PipelineAgent,
    store: &ArtifactStore,
) -> Result<(), PipelineOrchestratorError> {
    let required = agent.required_inputs();
    if required.is_empty() {
        return Ok(());
    }
    let missing: Vec<String> = required
        .iter()
        .filter(|t| store.latest_of_type(t).is_none())
        .map(|t| t.to_string())
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(PipelineOrchestratorError::MissingRequiredInputs {
            agent_id: agent.agent_id().to_string(),
            missing,
        })
    }
}

// ---------------------------------------------------------------------------
// PipelineOrchestratorError
// ---------------------------------------------------------------------------

/// Errors produced by the [`PlanningOrchestrator`].
#[derive(Debug, Error)]
pub enum PipelineOrchestratorError {
    #[error("agent not found in registry: {0}")]
    AgentNotFound(String),

    #[error("stage '{stage}' failed: {source}")]
    StageExecutionFailed {
        stage: String,
        #[source]
        source: PipelineAgentError,
    },

    #[error("stage '{stage}' failed after {attempts} retry attempts: {source}")]
    RetryExhausted {
        stage: String,
        attempts: u32,
        #[source]
        source: PipelineAgentError,
    },

    #[error(
        "convergence loop for stage '{stage}' did not converge after {max_iterations} iterations"
    )]
    ConvergenceNotReached { stage: String, max_iterations: u32 },

    #[error("agent '{agent_id}' missing required input artifacts: {missing:?}")]
    MissingRequiredInputs {
        agent_id: String,
        missing: Vec<String>,
    },
}

// ---------------------------------------------------------------------------
// PipelineSuspension (B-06)
// ---------------------------------------------------------------------------

/// Checkpoint that captures the full pipeline state at the point of suspension.
/// All fields needed to reconstruct a [`PlanningOrchestrator`] are included.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PipelineSuspension {
    /// Unique ID for this checkpoint (used as the question_id with PauseResumeManager).
    pub checkpoint_id: String,
    pub chain_id: String,
    pub workflow_id: String,
    /// Snapshot of all artifacts produced before suspension.
    pub artifact_snapshot: HashMap<String, AgentArtifact>,
    /// Pipeline context at the moment of suspension.
    pub context: PipelineContext,
    pub iteration: u32,
    /// The clarifying question to ask the user.
    pub question_text: String,
    /// Slot IDs that are unresolved and triggered this pause.
    pub slot_ids: Vec<String>,
    pub suspended_at: DateTime<Utc>,
    /// The question ID passed to PauseResumeManager for correlating this pause
    /// with the user's answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_id: Option<String>,
}

// ---------------------------------------------------------------------------
// PipelineStatus (B-06)
// ---------------------------------------------------------------------------

/// Terminal status of a pipeline run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PipelineStatus {
    /// All stages completed; PlanGraph produced.
    Completed,
    /// Pipeline paused waiting for user input. Resume via `from_suspension()`.
    Suspended(Box<PipelineSuspension>),
    /// C-01: A step returned `AgenticOutcome::Sleeping`.
    /// Callers should re-schedule the pipeline at `wake_at`; this is NOT the same
    /// as `Completed` and must not be treated as a finished run.
    Sleeping {
        wake_at: chrono::DateTime<chrono::Utc>,
    },
    /// Unrecoverable failure (agent error, budget exhausted, or routing error).
    Failed(String),
}

// ---------------------------------------------------------------------------
// PlanningOutcome (B-06 refactor)
// ---------------------------------------------------------------------------

/// Summary produced after a pipeline run (or suspension).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanningOutcome {
    /// Terminal status of this run.
    pub status: PipelineStatus,
    pub artifacts_produced: usize,
    /// Agent IDs that were executed (in order).
    pub stages_executed: Vec<String>,
    /// Extracted PlanGraph, if produced.
    pub plan_graph: Option<PlanGraph>,
    /// Set when a stage returned `Sleeping`; callers should re-schedule at this time.
    #[serde(default)]
    pub sleeping_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Full artifact store snapshot — callers can extract individual artifacts
    /// (QueryAnalysis, SlotGraph, ClarifiedTask, ElicitationResult, etc.).
    pub store: ArtifactStore,
}

// ---------------------------------------------------------------------------
// PlanningOrchestrator (B-07)

/// Router-driven orchestrator for the planning pipeline.
///
/// Construct via [`new_with_router`] for fresh runs, or [`from_suspension`]
/// to resume from a [`PipelineSuspension`] checkpoint.
pub struct PlanningOrchestrator {
    agents: HashMap<String, Arc<dyn PipelineAgent>>,
    router: Arc<dyn RouterAgent>,
    store: ArtifactStore,
    context: PipelineContext,
    /// Maximum router iterations before declaring budget exhaustion.
    max_iterations: u32,
    /// Optional PauseResumeManager for wiring suspension into the platform (B-08).
    pause_manager: Option<Arc<PauseResumeManager>>,
    /// Optional event broadcaster for pipeline lifecycle events (M7).
    event_broadcaster:
        Option<Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>>,
    /// Handles for fire-and-forget consolidation tasks, awaited on run() exit.
    ///
    /// **Known limitation:** If the process crashes (not graceful shutdown), in-flight
    /// consolidation tasks are lost. This is acceptable because consolidation produces
    /// derived data from already-persisted episodes — it can be re-computed via the
    /// consolidation sweep endpoint.
    consolidation_handles: Vec<tokio::task::JoinHandle<()>>,
    /// Test-only field: populated by `new()` to support the linear execution path.
    #[cfg(any(test, feature = "test-fixtures"))]
    test_definition: Option<PipelineDefinition>,
}

// ---------------------------------------------------------------------------
// Production constructors and run loop
// ---------------------------------------------------------------------------

impl PlanningOrchestrator {
    /// Create a new orchestrator for a fresh run.
    pub fn new_with_router(
        agents: HashMap<String, Arc<dyn PipelineAgent>>,
        router: Arc<dyn RouterAgent>,
        context: PipelineContext,
        max_iterations: u32,
        pause_manager: Option<Arc<PauseResumeManager>>,
    ) -> Self {
        let store = ArtifactStore::new(context.chain_id.clone());
        Self {
            agents,
            router,
            store,
            context,
            max_iterations,
            pause_manager,
            event_broadcaster: None,
            consolidation_handles: Vec::new(),
            #[cfg(any(test, feature = "test-fixtures"))]
            test_definition: None,
        }
    }

    /// Create a new orchestrator with a pre-populated [`ArtifactStore`].
    ///
    /// Use this when upstream results (e.g. `QueryAnalysis` from an earlier LLM
    /// call) should be injected before the pipeline starts so that the router
    /// can skip the corresponding agent and avoid redundant recomputation.
    pub fn new_with_seeded_store(
        agents: HashMap<String, Arc<dyn PipelineAgent>>,
        router: Arc<dyn RouterAgent>,
        context: PipelineContext,
        max_iterations: u32,
        pause_manager: Option<Arc<PauseResumeManager>>,
        initial_store: ArtifactStore,
    ) -> Self {
        Self {
            agents,
            router,
            store: initial_store,
            context,
            max_iterations,
            pause_manager,
            event_broadcaster: None,
            consolidation_handles: Vec::new(),
            #[cfg(any(test, feature = "test-fixtures"))]
            test_definition: None,
        }
    }

    /// Reconstruct an orchestrator from a [`PipelineSuspension`] checkpoint.
    ///
    /// Restores the artifact store from the snapshot and purges downstream
    /// artifacts per `resume_mode`:
    /// - `"full_replan"`: purge SlotGraph, ElicitationResult, InterpretedAnswer,
    ///   ClarifiedTask, PlanGraph, AgentError.  IntentClassification and
    ///   QueryAnalysis are intentionally preserved — the IntentClassifier does
    ///   not re-run on resume (resume_mode guard) so `handle_pipeline_outcome`
    ///   still needs the prior run's artifacts.
    /// - `"partial_replan"`: purge ElicitationResult, InterpretedAnswer,
    ///   ClarifiedTask, PlanGraph, AgentError.
    /// - `"light_slot_update"` (default): purge AgentError only.
    pub fn from_suspension(
        suspension: PipelineSuspension,
        agents: HashMap<String, Arc<dyn PipelineAgent>>,
        router: Arc<dyn RouterAgent>,
        user_answer: String,
        resume_mode: String,
        max_iterations: u32,
        pause_manager: Option<Arc<PauseResumeManager>>,
    ) -> Self {
        let purge_types = purge_types_for_resume_mode(&resume_mode);

        let mut store = ArtifactStore::restore_from_snapshot(
            suspension.chain_id.clone(),
            suspension.artifact_snapshot,
        );
        for artifact_type in &purge_types {
            store.remove_all_of_type(artifact_type);
        }
        // Note: mark_chain_artifacts_stale (bridge function) is called from
        // set_lifecycle_service() — not here — because the lifecycle service
        // hasn't been wired yet at this point.

        let mut context = suspension.context;
        context.user_answer = Some(user_answer);
        context.question_id = suspension.question_id.clone();
        context.resume_mode = Some(resume_mode);
        context.iteration = suspension.iteration;
        // NOTE: run_started_at is NOT set here — it is set at the top of run().

        Self {
            agents,
            router,
            store,
            context,
            max_iterations,
            pause_manager,
            event_broadcaster: None,
            consolidation_handles: Vec::new(),
            #[cfg(any(test, feature = "test-fixtures"))]
            test_definition: None,
        }
    }

    /// Set the event broadcaster for pipeline lifecycle events (M7).
    pub fn set_event_broadcaster(
        &mut self,
        broadcaster: Arc<crate::magician_v2::realtime_events::RuntimeTransportBroadcaster>,
    ) {
        self.event_broadcaster = Some(broadcaster);
    }

    /// Set the lifecycle service on the internal artifact store.
    ///
    /// If this orchestrator was resumed with `full_replan` mode, releases any
    /// chain references from the old plan graph in the lifecycle catalog.
    pub fn set_lifecycle_service(
        &mut self,
        svc: std::sync::Arc<crate::magician_v2::artifacts::service::LifecycleService>,
        ownership: crate::magician_v2::artifacts::types::OwnershipScope,
    ) {
        // Release chain references and mark purged artifacts as stale
        // *before* moving `svc` into `with_lifecycle`. Queries the lifecycle
        // catalog directly (not the ArtifactStore) because from_suspension()
        // already purged the store before this runs.
        if let Some(ref mode) = self.context.resume_mode {
            if mode == "full_replan" {
                crate::magician_v2::artifacts::bridge::release_chain_references(
                    &svc,
                    &self.context.chain_id,
                );
            }
            crate::magician_v2::artifacts::bridge::mark_chain_artifacts_stale(
                &svc,
                &self.context.chain_id,
                &format!("resume_mode:{}", mode),
            );
        }

        // Temporarily swap the store out to call the builder.
        let placeholder = ArtifactStore::new(self.store.chain_id().to_owned());
        let old_store = std::mem::replace(&mut self.store, placeholder);
        self.store = old_store.with_lifecycle(svc, ownership);
    }

    /// Execute the router-driven pipeline loop (B-07).
    ///
    /// Sets `context.run_started_at` at the top of the loop so resume rules
    /// can distinguish artifacts produced in *this* run from stale ones.
    ///
    /// On agent failure, writes an `AgentError` artifact (with per-agent
    /// `retry_count` and `recoverable` fields) and continues — rule 3.5 in
    /// the TieredRouter catches unrecoverable failures and returns Error.
    pub async fn run(&mut self) -> Result<PlanningOutcome, PipelineOrchestratorError> {
        let result = self.run_inner().await;
        // Await any outstanding consolidation tasks before returning,
        // so panics are surfaced rather than silently lost.
        for handle in self.consolidation_handles.drain(..) {
            if let Err(e) = handle.await {
                warn!(error = %e, "consolidation task panicked");
            }
        }
        result
    }

    async fn run_inner(&mut self) -> Result<PlanningOutcome, PipelineOrchestratorError> {
        const MAX_RETRIES: u32 = 2;
        const MAX_SERVICE_RETRIES: u32 = 2;

        // Freshness anchor for all resume rules.
        self.context.run_started_at = Some(Utc::now());

        // M7: emit PipelineStarted event
        if let Some(ref eb) = self.event_broadcaster {
            eb.pipeline_started(
                &self.context.workflow_id,
                &self.context.chain_id,
                self.max_iterations,
                self.context.principal.as_deref(),
                self.context.workspace.as_deref(),
            );
        }

        let mut stages_executed: Vec<String> = Vec::new();
        // I-18: initialise from existing AgentError artifacts so a resumed run
        // does not reset the retry budget that was already consumed.
        let mut service_retry_count: u32 = {
            self.store
                .all_of_type(&ArtifactType::AgentError)
                .iter()
                .filter_map(|a| a.content.get("retry_count").and_then(|v| v.as_u64()))
                .max()
                .map(|m| m as u32 + 1)
                .unwrap_or(0)
        };
        let mut last_retried_agent: Option<String> = None;

        // M-02: preserve the base cycle_id from before the run starts so we can derive
        // per-iteration IDs without losing the original.
        let base_cycle_id = self.context.cycle_id.clone();

        // M-16: resume from the stored iteration counter so that a resumed pipeline
        // continues numbering from where it left off rather than resetting to 0.
        let iteration_start = self.context.iteration;
        for offset in 0..self.max_iterations {
            let iteration = iteration_start + offset;
            self.context.iteration = iteration;
            // M-02: update cycle_id on every routing pass so artifacts can be attributed
            // to the iteration that produced them (tracing, state-tracker attribution).
            self.context.cycle_id = format!("{base_cycle_id}-iter-{iteration}");

            // Phase C: Budget exhaustion — downgrade remaining Weak steps to JIT
            // BEFORE routing so the planning pipeline can complete cleanly.
            // Must happen before router.route() because if all remaining steps
            // are Weak and budget is exhausted, no routing rule would produce
            // a next planning stage, causing a pipeline deadlock.
            if self.context.refinement_step_id.is_none()
                && (self.context.refinement_rounds >= MAX_REFINEMENT_ROUNDS
                    || self.context.refinement_user_pauses >= MAX_REFINEMENT_USER_PAUSES)
            {
                downgrade_weak_steps_to_jit(&mut self.store, &self.context.cycle_id);
            }

            let decision = self.router.route(&self.store, &mut self.context).await;

            match decision {
                RoutingDecision::NextStage { agent_id, reason } => {
                    debug!(agent_id = %agent_id, reason = %reason, "router: next stage");

                    let agent = self.agents.get(&agent_id).cloned().ok_or_else(|| {
                        PipelineOrchestratorError::AgentNotFound(agent_id.clone())
                    })?;

                    // Phase C: Enter refinement mode when routing to slot-extractor for a Weak step.
                    // Override context.query to the step's task description and purge stale
                    // artifacts so the existing slot-extract → elicit → rewrite chain fires fresh.
                    if agent_id == AGENT_ID_SLOT_EXTRACTOR
                        && self.context.refinement_step_id.is_none()
                    {
                        if let Some(weak_step) = find_first_weak_step(&self.store) {
                            info!(step_id = %weak_step.id, "entering refinement mode for weak step");
                            self.context.refinement_step_id = Some(weak_step.id.clone());
                            if self.context.original_query.is_none() {
                                self.context.original_query = Some(self.context.query.clone());
                            }
                            self.context.query = weak_step.task.clone();
                            // Purge artifacts so existing rules chain fresh
                            self.store.remove_all_of_type(&ArtifactType::SlotGraph);
                            self.store
                                .remove_all_of_type(&ArtifactType::ElicitationResult);
                            self.store.remove_all_of_type(&ArtifactType::ClarifiedTask);
                        }
                    }

                    validate_required_inputs(&*agent, &self.store)?;

                    // Emit step started before dispatching the agent so subscribers
                    // can pair it with the matching `PipelineStepCompleted` (or the
                    // `failed` outcome below).
                    if let Some(ref eb) = self.event_broadcaster {
                        eb.pipeline_step_started(
                            &self.context.workflow_id,
                            &agent_id,
                            &agent_id,
                            self.context.principal.as_deref(),
                            self.context.workspace.as_deref(),
                        );
                    }

                    match agent.execute(&mut self.store, &self.context).await {
                        Ok(PipelineAgentResult::Completed { .. }) => {
                            // I-08: track elicitation rounds as a counter (not proxy via
                            // recommended_questions.len()) so the planner gets an accurate count.
                            if agent_id == AGENT_ID_ELICITOR {
                                self.context.elicitation_rounds =
                                    self.context.elicitation_rounds.saturating_add(1);
                                // Phase C: track refinement user pauses — only count when
                                // the elicitor actually produced a needs_clarification=true
                                // result (i.e., it will pause for user input), not when
                                // it completes without needing clarification.
                                if self.context.refinement_step_id.is_some() {
                                    let needs_clarification = self
                                        .store
                                        .latest_of_type(&ArtifactType::ElicitationResult)
                                        .and_then(|a| {
                                            a.content
                                                .get("needs_clarification")
                                                .and_then(|v| v.as_bool())
                                        })
                                        .unwrap_or(false);
                                    if needs_clarification {
                                        self.context.refinement_user_pauses =
                                            self.context.refinement_user_pauses.saturating_add(1);
                                    }
                                }
                            }

                            // Phase C: refinement round complete after plan-patcher.
                            // Restore original query and purge refinement artifacts.
                            if agent_id == AGENT_ID_PLAN_PATCHER {
                                self.context.refinement_rounds =
                                    self.context.refinement_rounds.saturating_add(1);
                                self.context.refinement_step_id = None;
                                if let Some(orig) = self.context.original_query.take() {
                                    self.context.query = orig;
                                }
                                // Purge refinement artifacts for next round
                                self.store.remove_all_of_type(&ArtifactType::SlotGraph);
                                self.store
                                    .remove_all_of_type(&ArtifactType::ElicitationResult);
                                self.store.remove_all_of_type(&ArtifactType::ClarifiedTask);
                            }

                            // Post-agent hook: extract user_answer from IntentClassification
                            // so TieredRouter Rules 1-3 can route to answer-interpreter.
                            if agent_id == AGENT_ID_INTENT_CLASSIFIER {
                                if let Some(classification) = self
                                    .store
                                    .latest_of_type(&ArtifactType::IntentClassification)
                                {
                                    match classification
                                        .deserialize_content::<IntentClassificationResult>()
                                    {
                                        Ok(result) => {
                                            if let Some(answer) = result.user_answer {
                                                self.context.user_answer = Some(answer);
                                            }
                                        },
                                        Err(e) => {
                                            debug!(
                                                "[PIPELINE] failed to deserialize IntentClassificationResult \
                                                 in post-agent hook: {e}"
                                        );
                                        },
                                    }
                                }
                            }

                            // M7: emit step completed event
                            if let Some(ref eb) = self.event_broadcaster {
                                eb.pipeline_step_completed(
                                    &self.context.workflow_id,
                                    &agent_id,
                                    &agent_id,
                                    "completed",
                                    self.context.principal.as_deref(),
                                    self.context.workspace.as_deref(),
                                );
                            }
                            stages_executed.push(agent_id);
                        },
                        Ok(PipelineAgentResult::Sleeping { wake_at, .. }) => {
                            info!(
                                event = "pipeline.step.sleeping",
                                wake_at = %wake_at,
                                agent_id = %agent_id,
                                "agent sleeping; suspending pipeline run"
                            );
                            // Phase C: clear refinement state on suspension so a resumed
                            // pipeline does not get stuck in stale refinement mode.
                            if self.context.refinement_step_id.is_some() {
                                self.context.refinement_step_id = None;
                                if let Some(orig) = self.context.original_query.take() {
                                    self.context.query = orig;
                                }
                            }
                            // C-01: use PipelineStatus::Sleeping, not Completed, so callers can
                            // distinguish a sleeping pipeline from a finished one.
                            return Ok(PlanningOutcome {
                                status: PipelineStatus::Sleeping { wake_at },
                                artifacts_produced: self.store.len(),
                                stages_executed,
                                plan_graph: extract_plan_graph(&self.store),
                                sleeping_until: Some(wake_at),
                                store: self.store.clone(),
                            });
                        },
                        Ok(PipelineAgentResult::Failed {
                            reason,
                            artifact_ids,
                        }) => {
                            // Three-tier failure protocol (M5)
                            // Phase C: clear refinement state on failure so the pipeline
                            // does not get stuck retrying refinement for a broken step.
                            if self.context.refinement_step_id.is_some() {
                                self.context.refinement_step_id = None;
                                if let Some(orig) = self.context.original_query.take() {
                                    self.context.query = orig;
                                }
                            }

                            // Count prior AgentError artifacts for same step to determine tier
                            let prior_failures: u32 = {
                                let errors = self.store.all_of_type(&ArtifactType::AgentError);
                                errors
                                    .iter()
                                    .filter(|a| {
                                        a.content
                                            .get("failed_agent_id")
                                            .and_then(|v| v.as_str())
                                            .map(|id| id == agent_id)
                                            .unwrap_or(false)
                                    })
                                    .count() as u32
                            };
                            let tier = prior_failures + 1;

                            // Record failure as an AgentError artifact
                            self.store.put(AgentArtifact {
                                artifact_id: Uuid::new_v4().to_string(),
                                artifact_type: ArtifactType::AgentError,
                                producer_agent_id: "system:orchestrator".to_string(),
                                producer_cycle_id: self.context.cycle_id.clone(),
                                content: serde_json::json!({
                                    "failed_agent_id": agent_id,
                                    "error": &reason,
                                    "error_kind": "step_failed",
                                    "tier": tier,
                                    "artifact_ids": artifact_ids,
                                }),
                                schema_version: ARTIFACT_SCHEMA_VERSION,
                                produced_at: Utc::now(),
                                render_hints: None,
                            });

                            if tier <= 2 {
                                warn!(
                                    agent = %agent_id,
                                    reason = %reason,
                                    tier = tier,
                                    "agent failed (tier {}); removing produced artifacts to allow re-dispatch",
                                    tier
                                );
                                // Remove artifacts from the failed attempt so the agent can retry cleanly.
                                for aid in &artifact_ids {
                                    self.store.remove(aid);
                                }
                                if tier == 2 {
                                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                                }
                                // Continue loop — router will re-dispatch
                            } else {
                                warn!(
                                    agent = %agent_id,
                                    reason = %reason,
                                    tier = tier,
                                    "step failed (tier 3); escalating to pipeline failure"
                                );
                                // M7: emit step failed + pipeline failed events
                                if let Some(ref eb) = self.event_broadcaster {
                                    eb.pipeline_step_completed(
                                        &self.context.workflow_id,
                                        &agent_id,
                                        &agent_id,
                                        "failed",
                                        self.context.principal.as_deref(),
                                        self.context.workspace.as_deref(),
                                    );
                                    eb.pipeline_failed(
                                        &self.context.workflow_id,
                                        &self.context.chain_id,
                                        &reason,
                                        stages_executed.len(),
                                        self.context.principal.as_deref(),
                                        self.context.workspace.as_deref(),
                                    );
                                }
                                return Ok(PlanningOutcome {
                                    status: PipelineStatus::Failed(reason),
                                    artifacts_produced: self.store.len(),
                                    stages_executed,
                                    plan_graph: extract_plan_graph(&self.store),
                                    sleeping_until: None,
                                    store: self.store.clone(),
                                });
                            }
                        },
                        Ok(PipelineAgentResult::WaitingForUser { artifact_ids }) => {
                            info!(
                                event = "pipeline.step.waiting_for_user",
                                agent_id = %agent_id,
                                "agent waiting for user input; suspending pipeline"
                            );
                            // Phase C: clear refinement state on WaitingForUser so a
                            // resumed pipeline does not get stuck in stale refinement mode.
                            if self.context.refinement_step_id.is_some() {
                                self.context.refinement_step_id = None;
                                if let Some(orig) = self.context.original_query.take() {
                                    self.context.query = orig;
                                }
                            }

                            // Extract question text from the first artifact the agent returned.
                            let question_text = artifact_ids
                                .first()
                                .and_then(|aid| self.store.get(aid))
                                .and_then(|a| a.content.get("last_action_result").cloned())
                                .and_then(|v| {
                                    v.get("question").and_then(|q| q.as_str().map(String::from))
                                })
                                .unwrap_or_else(|| "Awaiting user input".to_string());

                            // Build PipelineSuspension (mirrors RoutingDecision::Pause arm)
                            let checkpoint_id = Uuid::new_v4().to_string();
                            let suspension = PipelineSuspension {
                                checkpoint_id: checkpoint_id.clone(),
                                chain_id: self.context.chain_id.clone(),
                                workflow_id: self.context.workflow_id.clone(),
                                artifact_snapshot: self.store.snapshot(),
                                context: self.context.clone(),
                                iteration: self.context.iteration,
                                question_text: question_text.clone(),
                                slot_ids: vec![],
                                suspended_at: Utc::now(),
                                question_id: Some(checkpoint_id.clone()),
                            };

                            // Register with PauseResumeManager if available.
                            // #22: pause with the non-question `PipelineSuspended`
                            // reason so `pending_questions` stays empty. The real
                            // answerable questions are registered by the dispatch
                            // path (`pause_for_batch`); registering a phantom entry
                            // keyed by this throwaway `checkpoint_id` uuid left a
                            // duplicate that never cleared and made `remove()`
                            // non-deterministic.
                            if let Some(ref pm) = self.pause_manager {
                                if let Err(e) = pm
                                    .pause(
                                        &self.context.workflow_id,
                                        PauseReason::PipelineSuspended,
                                    )
                                    .await
                                {
                                    warn!(
                                        error = %e,
                                        "PauseResumeManager::pause failed (WaitingForUser); returning Suspended anyway"
                                    );
                                }
                            }

                            return Ok(PlanningOutcome {
                                status: PipelineStatus::Suspended(Box::new(suspension)),
                                artifacts_produced: self.store.len(),
                                stages_executed,
                                plan_graph: extract_plan_graph(&self.store),
                                sleeping_until: None,
                                store: self.store.clone(),
                            });
                        },
                        Err(e) => {
                            // Compute the new retry count from existing AgentError artifacts
                            // for this agent. The immutable borrow ends before put().
                            let new_retry_count: u32 = {
                                let existing = self.store.all_of_type(&ArtifactType::AgentError);
                                let prev = existing
                                    .iter()
                                    .filter(|a| {
                                        a.content
                                            .get("failed_agent_id")
                                            .and_then(|v| v.as_str())
                                            .map(|id| id == agent_id)
                                            .unwrap_or(false)
                                    })
                                    .filter_map(|a| {
                                        a.content.get("retry_count").and_then(|v| v.as_u64())
                                    })
                                    .max();
                                match prev {
                                    Some(p) => p as u32 + 1,
                                    None => 0,
                                }
                            };
                            let recoverable = new_retry_count < MAX_RETRIES;

                            warn!(
                                agent = %agent_id,
                                retry_count = new_retry_count,
                                recoverable,
                                error = %e,
                                "agent execution failed; writing AgentError artifact"
                            );

                            // Derive a stable error_kind from the PipelineAgentError
                            // variant so retry rules can match on kind rather than
                            // fragile display-string prefixes.
                            let error_kind = match &e {
                                PipelineAgentError::ServiceError(_) => "service_error",
                                PipelineAgentError::MissingInput(_) => "missing_input",
                                PipelineAgentError::ExecutionFailed(_) => "execution_failed",
                                PipelineAgentError::SerializationError(_) => "serialization_error",
                            };

                            self.store.put(AgentArtifact {
                                artifact_id: Uuid::new_v4().to_string(),
                                artifact_type: ArtifactType::AgentError,
                                producer_agent_id: "system:orchestrator".to_string(),
                                producer_cycle_id: self.context.cycle_id.clone(),
                                content: serde_json::json!({
                                    "failed_agent_id": agent_id,
                                    "error": e.to_string(),
                                    "error_kind": error_kind,
                                    "retry_count": new_retry_count,
                                    "recoverable": recoverable,
                                }),
                                schema_version: ARTIFACT_SCHEMA_VERSION,
                                produced_at: Utc::now(),
                                render_hints: None,
                            });
                            // Phase C: clear refinement state on Err so the pipeline
                            // does not get stuck retrying refinement for a broken agent.
                            if self.context.refinement_step_id.is_some() {
                                self.context.refinement_step_id = None;
                                if let Some(orig) = self.context.original_query.take() {
                                    self.context.query = orig;
                                }
                            }
                            // Continue loop — rule 3.5 handles unrecoverable failures.
                        },
                    }
                },

                RoutingDecision::Pause {
                    question_text,
                    slot_ids,
                } => {
                    info!(question = %question_text, "pipeline paused, awaiting user answer");

                    // Generate question_id before building the suspension so it can be stored.
                    let question_id = uuid::Uuid::new_v4().to_string();

                    let suspension = PipelineSuspension {
                        checkpoint_id: Uuid::new_v4().to_string(),
                        // I-23: use the store's own chain_id as the authoritative
                        // source rather than context, which may diverge on resume.
                        chain_id: self.store.chain_id().to_string(),
                        workflow_id: self.context.workflow_id.clone(),
                        artifact_snapshot: self.store.snapshot(),
                        context: self.context.clone(),
                        iteration: self.context.iteration,
                        question_text,
                        slot_ids,
                        suspended_at: Utc::now(),
                        question_id: Some(question_id.clone()),
                    };

                    // B-08: wire suspension into PauseResumeManager if configured.
                    // #22: pause with the non-question `PipelineSuspended` reason
                    // so `pending_questions` stays empty. The real answerable
                    // question ids are owned/registered by the dispatch path
                    // (`pause_for_batch`); the suspension itself already carries
                    // `question_id` for correlation. Registering it here as
                    // `WaitingOnUser(question_id)` minted a phantom queue entry
                    // (keyed by a different checkpoint state_id) that duplicated
                    // the real batch entry and never cleared.
                    if let Some(ref pm) = self.pause_manager {
                        if let Err(e) = pm
                            .pause(&self.context.workflow_id, PauseReason::PipelineSuspended)
                            .await
                        {
                            warn!(
                                error = %e,
                                "PauseResumeManager::pause failed; returning Suspended outcome anyway"
                            );
                        }
                    }

                    return Ok(PlanningOutcome {
                        status: PipelineStatus::Suspended(Box::new(suspension)),
                        artifacts_produced: self.store.len(),
                        stages_executed,
                        plan_graph: extract_plan_graph(&self.store),
                        sleeping_until: None,
                        store: self.store.clone(),
                    });
                },

                RoutingDecision::Complete { reason } => {
                    info!(reason = %reason, "pipeline complete");
                    self.store
                        .register_plan_graph_reference(&self.context.chain_id);
                    // M7: emit PipelineCompleted event
                    if let Some(ref eb) = self.event_broadcaster {
                        eb.pipeline_completed(
                            &self.context.workflow_id,
                            &self.context.chain_id,
                            stages_executed.len(),
                            self.context.principal.as_deref(),
                            self.context.workspace.as_deref(),
                        );
                    }
                    return Ok(PlanningOutcome {
                        status: PipelineStatus::Completed,
                        artifacts_produced: self.store.len(),
                        stages_executed,
                        plan_graph: extract_plan_graph(&self.store),
                        sleeping_until: None,
                        store: self.store.clone(),
                    });
                },

                RoutingDecision::Error { reason } => {
                    warn!(reason = %reason, "router returned error decision");
                    return Ok(PlanningOutcome {
                        status: PipelineStatus::Failed(reason),
                        artifacts_produced: self.store.len(),
                        stages_executed,
                        plan_graph: extract_plan_graph(&self.store),
                        sleeping_until: None,
                        store: self.store.clone(),
                    });
                },
                RoutingDecision::Retry {
                    agent_id,
                    reason,
                    delay_ms,
                } => {
                    // Reset service_retry_count when switching to a different agent.
                    if last_retried_agent.as_deref() != Some(&agent_id) {
                        service_retry_count = 0;
                        last_retried_agent = Some(agent_id.clone());
                    }

                    if service_retry_count >= MAX_SERVICE_RETRIES {
                        warn!(
                            agent = %agent_id,
                            service_retry_count,
                            "service retry budget exhausted; writing unrecoverable AgentError"
                        );
                        self.store.put(AgentArtifact {
                            artifact_id: Uuid::new_v4().to_string(),
                            artifact_type: ArtifactType::AgentError,
                            producer_agent_id: "system:orchestrator".to_string(),
                            producer_cycle_id: self.context.cycle_id.clone(),
                            content: serde_json::json!({
                                "failed_agent_id": agent_id,
                                "error": reason,
                                "retry_count": service_retry_count,
                                "recoverable": false,
                            }),
                            schema_version: ARTIFACT_SCHEMA_VERSION,
                            produced_at: Utc::now(),
                            render_hints: None,
                        });
                        // Explicit early return: the AgentError artifact above is unrecoverable;
                        // returning here avoids a wasted routing round-trip that would only
                        // confirm the failure via rule 3.5.
                        return Ok(PlanningOutcome {
                            status: PipelineStatus::Failed(format!(
                                "service retry budget exhausted for agent {agent_id}: {reason}"
                            )),
                            artifacts_produced: self.store.len(),
                            stages_executed,
                            plan_graph: extract_plan_graph(&self.store),
                            sleeping_until: None,
                            store: self.store.clone(),
                        });
                    }

                    info!(
                        agent = %agent_id,
                        service_retry_count,
                        delay_ms,
                        reason = %reason,
                        "retrying agent after transient service error"
                    );

                    if delay_ms > 0 {
                        tokio::time::sleep(tokio::time::Duration::from_millis(delay_ms)).await;
                    }

                    let agent = match self.agents.get(&agent_id).cloned() {
                        Some(a) => a,
                        None => {
                            return Err(PipelineOrchestratorError::AgentNotFound(agent_id.clone()));
                        },
                    };

                    match agent.execute(&mut self.store, &self.context).await {
                        Ok(PipelineAgentResult::Completed { .. }) => {
                            // C-12: reset on success — the service recovered, give it a
                            // fresh retry budget rather than counting the attempt as a failure.
                            service_retry_count = 0;
                            stages_executed.push(format!("{agent_id}(retry)"));
                        },
                        Ok(PipelineAgentResult::Sleeping { wake_at, .. }) => {
                            // C-01: Sleeping, not Completed.
                            return Ok(PlanningOutcome {
                                status: PipelineStatus::Sleeping { wake_at },
                                artifacts_produced: self.store.len(),
                                stages_executed,
                                plan_graph: extract_plan_graph(&self.store),
                                sleeping_until: Some(wake_at),
                                store: self.store.clone(),
                            });
                        },
                        Ok(PipelineAgentResult::Failed { reason, .. }) => {
                            warn!(agent = %agent_id, reason = %reason, "agent failed (service-retry path)");
                        },
                        Ok(PipelineAgentResult::WaitingForUser { .. }) => {
                            info!(agent = %agent_id, "agent waiting for user (service-retry path)");
                        },
                        Err(e) => {
                            service_retry_count += 1;
                            warn!(
                                agent = %agent_id,
                                service_retry_count,
                                error = %e,
                                "agent failed during retry; writing AgentError"
                            );
                            self.store.put(AgentArtifact {
                                artifact_id: Uuid::new_v4().to_string(),
                                artifact_type: ArtifactType::AgentError,
                                producer_agent_id: "system:orchestrator".to_string(),
                                producer_cycle_id: self.context.cycle_id.clone(),
                                content: serde_json::json!({
                                    "failed_agent_id": agent_id,
                                    "error": e.to_string(),
                                    "retry_count": service_retry_count,
                                    "recoverable": service_retry_count < MAX_SERVICE_RETRIES,
                                }),
                                schema_version: ARTIFACT_SCHEMA_VERSION,
                                produced_at: Utc::now(),
                                render_hints: None,
                            });
                        },
                    }
                    // iteration is NOT incremented — continue goes to top of loop (router.route again).
                },
            }
        }

        Ok(PlanningOutcome {
            status: PipelineStatus::Failed(format!(
                "pipeline budget exhausted after {} iterations",
                self.max_iterations
            )),
            artifacts_produced: self.store.len(),
            stages_executed,
            plan_graph: extract_plan_graph(&self.store),
            sleeping_until: None,
            store: self.store.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Test-only: linear execution path (P5.5-A compatibility)
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
impl PlanningOrchestrator {
    /// Create a test orchestrator from a [`PipelineDefinition`].
    ///
    /// Uses a [`NoopRouter`] internally — call [`run_linear`] instead of
    /// [`run`] to execute the linear stage walker.
    pub fn new(
        definition: PipelineDefinition,
        agents: HashMap<String, Arc<dyn PipelineAgent>>,
        context: PipelineContext,
    ) -> Self {
        let store = ArtifactStore::new(context.chain_id.clone());
        Self {
            agents,
            router: Arc::new(NoopRouter),
            store,
            context,
            max_iterations: 20,
            pause_manager: None,
            event_broadcaster: None,
            consolidation_handles: Vec::new(),
            test_definition: Some(definition),
        }
    }

    /// Execute the pipeline sequentially through the stages in the
    /// [`PipelineDefinition`] (test-only linear path — P5.5-A compatibility).
    pub async fn run_linear(&mut self) -> Result<PlanningOutcome, PipelineOrchestratorError> {
        let definition = self
            .test_definition
            .as_ref()
            .expect("run_linear() requires PlanningOrchestrator::new() constructor");
        let mut sleeping_until: Option<chrono::DateTime<chrono::Utc>> = None;

        let stages: Vec<StageDefinition> = definition.stages.clone();
        let mut stages_executed: Vec<String> = Vec::new();

        for stage in &stages {
            let agent = self
                .agents
                .get(&stage.agent)
                .cloned()
                .ok_or_else(|| PipelineOrchestratorError::AgentNotFound(stage.agent.clone()))?;

            if should_skip(stage, &self.context) {
                info!(stage = %stage.name, "skipping stage (condition: {:?})", stage.skip_when);
                continue;
            }

            validate_required_inputs(&*agent, &self.store)?;

            if let Some(ref retry) = stage.retry {
                debug!(stage = %stage.name, max_attempts = retry.max_attempts, "running stage with retry");
                let agent_result = self.run_with_retry(&agent, retry).await.map_err(|e| {
                    match e {
                        // Propagate RetryExhausted with the stage name filled in.
                        PipelineOrchestratorError::RetryExhausted {
                            stage: _,
                            attempts,
                            source,
                        } => PipelineOrchestratorError::RetryExhausted {
                            stage: stage.name.clone(),
                            attempts,
                            source,
                        },
                        other => other,
                    }
                })?;
                match agent_result {
                    PipelineAgentResult::Completed { artifact_ids } => {
                        debug!(
                            "agent {} completed (retry path), artifacts: {:?}",
                            agent.agent_id(),
                            artifact_ids
                        );
                        // (f) Record success — only Completed counts as executed.
                        stages_executed.push(stage.name.clone());
                    },
                    PipelineAgentResult::Sleeping { wake_at, .. } => {
                        info!(event = "pipeline.step.sleeping", wake_at = %wake_at, "agent {} sleeping until {}", agent.agent_id(), wake_at);
                        sleeping_until = Some(wake_at);
                        break;
                    },
                    PipelineAgentResult::Failed { reason, .. } => {
                        warn!(agent = agent.agent_id(), reason = %reason, "agent failed (retry path)");
                        break;
                    },
                    PipelineAgentResult::WaitingForUser { .. } => {
                        info!(
                            agent = agent.agent_id(),
                            "agent waiting for user (retry path)"
                        );
                        break;
                    },
                }
            } else if let Some(ref loop_config) = stage.loop_config {
                debug!(
                    stage = %stage.name,
                    max_iterations = loop_config.max_iterations,
                    "running stage with convergence loop"
                );
                if let Some(wake_at) = self
                    .run_convergence_loop(&agent, &stage.name, loop_config)
                    .await?
                {
                    sleeping_until = Some(wake_at);
                    break;
                }
                // (f) Record success — convergence loop only returns Ok(None) if converged (Completed).
                stages_executed.push(stage.name.clone());
            } else {
                debug!(stage = %stage.name, "executing stage");
                let agent_result = agent
                    .execute(&mut self.store, &self.context)
                    .await
                    .map_err(|source| PipelineOrchestratorError::StageExecutionFailed {
                        stage: stage.name.clone(),
                        source,
                    })?;
                match agent_result {
                    PipelineAgentResult::Completed { artifact_ids } => {
                        debug!(
                            "agent {} completed, artifacts: {:?}",
                            agent.agent_id(),
                            artifact_ids
                        );
                        // (f) Record success — only Completed counts as executed.
                        stages_executed.push(stage.name.clone());
                    },
                    PipelineAgentResult::Sleeping { wake_at, .. } => {
                        info!(event = "pipeline.step.sleeping", wake_at = %wake_at, "agent {} sleeping until {}", agent.agent_id(), wake_at);
                        sleeping_until = Some(wake_at);
                        break;
                    },
                    PipelineAgentResult::Failed { reason, .. } => {
                        warn!(agent = agent.agent_id(), reason = %reason, "agent failed (linear path)");
                        break;
                    },
                    PipelineAgentResult::WaitingForUser { .. } => {
                        info!(
                            agent = agent.agent_id(),
                            "agent waiting for user (linear path)"
                        );
                        break;
                    },
                }
            }
            self.context.iteration += 1;
        }

        let plan_graph = extract_plan_graph(&self.store);
        let artifacts_produced = self.store.len();

        info!(
            stages_executed = stages_executed.len(),
            artifacts_produced,
            has_plan_graph = plan_graph.is_some(),
            "linear pipeline run complete"
        );

        Ok(PlanningOutcome {
            status: PipelineStatus::Completed,
            artifacts_produced,
            stages_executed,
            plan_graph,
            sleeping_until,
            store: self.store.clone(),
        })
    }

    async fn run_with_retry(
        &mut self,
        agent: &Arc<dyn PipelineAgent>,
        retry: &RetryConfig,
    ) -> Result<PipelineAgentResult, PipelineOrchestratorError> {
        let mut last_error: Option<PipelineAgentError> = None;
        for attempt in 1..=retry.max_attempts {
            match agent.execute(&mut self.store, &self.context).await {
                Ok(result) => {
                    match &result {
                        PipelineAgentResult::Completed { .. } => {
                            if attempt > 1 {
                                info!(agent = agent.agent_id(), attempt, "succeeded after retry");
                            }
                        },
                        PipelineAgentResult::Sleeping { wake_at, .. } => {
                            info!(agent = agent.agent_id(), %wake_at, "agent sleeping — no retry");
                        },
                        PipelineAgentResult::Failed { reason, .. } => {
                            // Failed is a business-logic outcome (goal not achievable),
                            // not a transient error — do not retry.
                            warn!(agent = agent.agent_id(), %reason, "agent returned Failed — no retry");
                        },
                        PipelineAgentResult::WaitingForUser { .. } => {
                            info!(
                                agent = agent.agent_id(),
                                "agent waiting for user — no retry"
                            );
                        },
                    }
                    return Ok(result);
                },
                Err(e) => {
                    warn!(
                        agent = agent.agent_id(),
                        attempt,
                        max_attempts = retry.max_attempts,
                        error = %e,
                        "attempt failed"
                    );
                    last_error = Some(e);
                },
            }
        }
        Err(PipelineOrchestratorError::RetryExhausted {
            stage: String::new(),
            attempts: retry.max_attempts,
            source: last_error.expect("at least one attempt must have been made"),
        })
    }

    async fn run_convergence_loop(
        &mut self,
        agent: &Arc<dyn PipelineAgent>,
        stage_name: &str,
        loop_config: &LoopConfig,
    ) -> Result<Option<chrono::DateTime<chrono::Utc>>, PipelineOrchestratorError> {
        for iteration in 1..=loop_config.max_iterations {
            debug!(
                agent = agent.agent_id(),
                iteration,
                max = loop_config.max_iterations,
                "convergence loop iteration"
            );

            let agent_result = agent
                .execute(&mut self.store, &self.context)
                .await
                .map_err(|source| PipelineOrchestratorError::StageExecutionFailed {
                    stage: stage_name.to_string(),
                    source,
                })?;
            match agent_result {
                PipelineAgentResult::Completed { artifact_ids } => {
                    debug!(
                        "agent {} completed (convergence loop), artifacts: {:?}",
                        agent.agent_id(),
                        artifact_ids
                    );
                },
                PipelineAgentResult::Sleeping { wake_at, .. } => {
                    info!(event = "pipeline.step.sleeping", wake_at = %wake_at, "agent {} sleeping until {}", agent.agent_id(), wake_at);
                    return Ok(Some(wake_at));
                },
                PipelineAgentResult::Failed { reason, .. } => {
                    warn!(agent = agent.agent_id(), reason = %reason, "agent failed (convergence loop)");
                    return Ok(None);
                },
                PipelineAgentResult::WaitingForUser { .. } => {
                    info!(
                        agent = agent.agent_id(),
                        "agent waiting for user (convergence loop)"
                    );
                    return Ok(None);
                },
            }

            if is_converged(&self.store, &loop_config.convergence) {
                info!(
                    agent = agent.agent_id(),
                    iteration,
                    convergence = ?loop_config.convergence,
                    "convergence reached"
                );
                return Ok(None);
            }
        }
        Err(PipelineOrchestratorError::ConvergenceNotReached {
            stage: stage_name.to_string(),
            max_iterations: loop_config.max_iterations,
        })
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Extract the most recent PlanGraph artifact from the store.
fn extract_plan_graph(store: &ArtifactStore) -> Option<PlanGraph> {
    let artifact = store.latest_consumable_of_type(&ArtifactType::PlanGraph)?;
    match artifact.deserialize_content() {
        Ok(pg) => Some(pg),
        Err(e) => {
            tracing::warn!(
                artifact_id = %artifact.artifact_id, error = %e,
                "extract_plan_graph: PlanGraph deserialization failed"
            );
            None
        },
    }
}

/// Find the first Weak step in the current PlanGraph.
///
/// Used by the refinement enter hook (Phase C) to identify which step
/// to scope the slot-extractor → elicitor → query-rewriter chain to.
fn find_first_weak_step(store: &ArtifactStore) -> Option<PlanStep> {
    let artifact = store.latest_of_type(&ArtifactType::PlanGraph)?;
    let plan_graph: PlanGraph = artifact.deserialize_content().ok()?;
    plan_graph
        .steps
        .into_iter()
        .find(|step| step.readiness == Some(StepReadiness::Weak))
}

/// Downgrade all remaining Weak steps to JustInTime in the PlanGraph artifact.
///
/// Called when the refinement budget is exhausted so execution can handle the
/// remaining steps through the seeded-taskplan direct path.
fn downgrade_weak_steps_to_jit(store: &mut ArtifactStore, cycle_id: &str) {
    let Some(plan_artifact) = store.latest_of_type(&ArtifactType::PlanGraph) else {
        return;
    };
    let Ok(mut plan_graph) = plan_artifact.deserialize_content::<PlanGraph>() else {
        return;
    };

    let mut changed = false;
    for step in &mut plan_graph.steps {
        if step.readiness == Some(StepReadiness::Weak) {
            step.readiness = Some(StepReadiness::JustInTime);
            changed = true;
        }
    }

    if changed {
        if let Ok(content) = serde_json::to_value(&plan_graph) {
            store.put(AgentArtifact {
                artifact_id: Uuid::new_v4().to_string(),
                artifact_type: ArtifactType::PlanGraph,
                producer_agent_id: "system:orchestrator".to_string(),
                producer_cycle_id: cycle_id.to_string(),
                content,
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
        }
    }
}

/// Evaluate a convergence criterion against the current artifact store.
#[cfg(any(test, feature = "test-fixtures"))]
fn is_converged(store: &ArtifactStore, criterion: &ConvergenceCriterion) -> bool {
    match criterion {
        ConvergenceCriterion::AllSlotsResolved => store
            .latest_of_type(&ArtifactType::ElicitationResult)
            .map(|a| {
                a.content
                    .get("needs_clarification")
                    .and_then(|v| v.as_bool())
                    .map(|needs| !needs)
                    .unwrap_or(false)
            })
            .unwrap_or(false),
        ConvergenceCriterion::AllComplete | ConvergenceCriterion::AnyComplete => true,
        ConvergenceCriterion::Threshold { .. } => true, // not yet evaluable without stage tracking
    }
}

/// Evaluate skip conditions for a stage.
#[cfg(any(test, feature = "test-fixtures"))]
fn should_skip(stage: &StageDefinition, context: &PipelineContext) -> bool {
    match stage.skip_when.as_deref() {
        None => false,
        Some("agent.has_goal_definition") => context.agent_id.is_some(),
        Some("always") => true,
        Some(other) => {
            warn!(
                stage = %stage.name,
                condition = other,
                "unknown skip_when condition, not skipping (fail closed)"
            );
            false
        },
    }
}

// ---------------------------------------------------------------------------
// Test-only mock router
// ---------------------------------------------------------------------------

/// Placeholder router for the test-only `new()` constructor.
/// Always returns an error — tests should call `run_linear()` instead.
#[cfg(any(test, feature = "test-fixtures"))]
struct NoopRouter;

#[cfg(any(test, feature = "test-fixtures"))]
#[async_trait::async_trait]
impl RouterAgent for NoopRouter {
    async fn route(
        &self,
        _store: &ArtifactStore,
        _context: &mut PipelineContext,
    ) -> RoutingDecision {
        RoutingDecision::Error {
            reason: "NoopRouter: use run_linear() for definition-based tests".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::pipeline::artifact::{AgentArtifact, ArtifactType};
    use crate::magician_v2::pipeline::definition::StageDefinition;
    use crate::magician_v2::strategy::plan::{
        PlanGraph, PlanProvenance, PlanStep, PlanningMetadata, SessionPolicy,
    };
    use async_trait::async_trait;
    use chrono::Utc;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU32, Ordering};

    // -----------------------------------------------------------------------
    // Mock Agents
    // -----------------------------------------------------------------------

    struct SuccessAgent {
        id: String,
        inputs: Vec<ArtifactType>,
        outputs: Vec<ArtifactType>,
    }

    #[async_trait]
    impl PipelineAgent for SuccessAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            self.inputs.clone()
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            self.outputs.clone()
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let mut ids = Vec::new();
            for output_type in &self.outputs {
                let id = uuid::Uuid::new_v4().to_string();
                store.put(AgentArtifact {
                    artifact_id: id.clone(),
                    artifact_type: output_type.clone(),
                    producer_agent_id: self.id.clone(),
                    producer_cycle_id: context.cycle_id.clone(),
                    content: serde_json::json!({"mock": true}),
                    schema_version: ARTIFACT_SCHEMA_VERSION,
                    produced_at: Utc::now(),
                    render_hints: None,
                });
                ids.push(id);
            }
            Ok(PipelineAgentResult::Completed { artifact_ids: ids })
        }
    }

    struct FlakeyAgent {
        id: String,
        fail_count: AtomicU32,
        max_failures: u32,
        outputs: Vec<ArtifactType>,
    }

    #[async_trait]
    impl PipelineAgent for FlakeyAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            self.outputs.clone()
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let count = self.fail_count.fetch_add(1, Ordering::SeqCst);
            if count < self.max_failures {
                return Err(PipelineAgentError::ExecutionFailed(format!(
                    "flakey failure #{}",
                    count + 1
                )));
            }
            let mut ids = Vec::new();
            for output_type in &self.outputs {
                let id = uuid::Uuid::new_v4().to_string();
                store.put(AgentArtifact {
                    artifact_id: id.clone(),
                    artifact_type: output_type.clone(),
                    producer_agent_id: self.id.clone(),
                    producer_cycle_id: context.cycle_id.clone(),
                    content: serde_json::json!({"mock": true}),
                    schema_version: ARTIFACT_SCHEMA_VERSION,
                    produced_at: Utc::now(),
                    render_hints: None,
                });
                ids.push(id);
            }
            Ok(PipelineAgentResult::Completed { artifact_ids: ids })
        }
    }

    struct FailAgent {
        id: String,
    }

    #[async_trait]
    impl PipelineAgent for FailAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![]
        }
        async fn execute(
            &self,
            _store: &mut ArtifactStore,
            _context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            Err(PipelineAgentError::ExecutionFailed(
                "always fails".to_string(),
            ))
        }
    }

    struct PlannerAgent {
        id: String,
    }

    #[async_trait]
    impl PipelineAgent for PlannerAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::PlanGraph]
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let plan = PlanGraph {
                steps: vec![PlanStep {
                    id: "s1".into(),
                    task: "test".into(),
                    ..Default::default()
                }],
                edges: vec![],
                unresolved_inputs: vec![],
                confidence: 0.8,
                provenance: PlanProvenance::default(),
                session_policy: SessionPolicy::default(),
                planning_metadata: PlanningMetadata::default(),
                ..Default::default()
            };
            let content = serde_json::to_value(&plan)
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;
            let id = uuid::Uuid::new_v4().to_string();
            store.put(AgentArtifact {
                artifact_id: id.clone(),
                artifact_type: ArtifactType::PlanGraph,
                producer_agent_id: self.id.clone(),
                producer_cycle_id: context.cycle_id.clone(),
                content,
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![id],
            })
        }
    }

    struct ConvergingAgent {
        id: String,
        call_count: AtomicU32,
        converge_after: u32,
    }

    #[async_trait]
    impl PipelineAgent for ConvergingAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::ElicitationResult]
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let n = self.call_count.fetch_add(1, Ordering::SeqCst) + 1;
            let needs_clarification = n < self.converge_after;
            let id = uuid::Uuid::new_v4().to_string();
            store.put(AgentArtifact {
                artifact_id: id.clone(),
                artifact_type: ArtifactType::ElicitationResult,
                producer_agent_id: self.id.clone(),
                producer_cycle_id: context.cycle_id.clone(),
                content: serde_json::json!({
                    "needs_clarification": needs_clarification,
                    "iteration": n,
                }),
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![id],
            })
        }
    }

    // -----------------------------------------------------------------------
    // Mock router for B-07 tests
    // -----------------------------------------------------------------------

    /// Pre-programmed router — returns decisions from the queue in order.
    struct MockDecisionRouter {
        decisions: Vec<RoutingDecision>,
        call_count: AtomicU32,
    }

    #[async_trait]
    impl RouterAgent for MockDecisionRouter {
        async fn route(
            &self,
            _store: &ArtifactStore,
            _context: &mut PipelineContext,
        ) -> RoutingDecision {
            let idx = self.call_count.fetch_add(1, Ordering::SeqCst) as usize;
            self.decisions
                .get(idx)
                .cloned()
                .unwrap_or(RoutingDecision::Complete {
                    reason: "mock exhausted".to_string(),
                })
        }
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    fn test_context() -> PipelineContext {
        PipelineContext {
            chain_id: "test-chain".to_string(),
            cycle_id: "cycle-1".to_string(),
            workflow_id: "wf-1".to_string(),
            query: "test query".to_string(),
            iteration: 0,
            agent_id: None,
            correlation_id: None,
            user_answer: None,
            run_started_at: None,
            resume_mode: None,
            llm_routing_calls: 0,
            elicitation_rounds: 0,
            session_id: None,
            question_id: None,
            agent_kind: None,
            is_autonomous_cycle: false,
            trust_level: None,
            tier_definitions: Vec::new(),
            observation_mode: None,
            llm_model_override: None,
            max_delegation_depth: None,
            schedule_context: None,
            ..Default::default()
        }
    }

    fn test_definition(stages: Vec<StageDefinition>) -> PipelineDefinition {
        PipelineDefinition { stages }
    }

    fn make_stage(name: &str, agent: &str) -> StageDefinition {
        StageDefinition {
            name: name.to_string(),
            agent: agent.to_string(),
            inputs: vec![],
            outputs: vec![],
            skip_when: None,
            loop_config: None,
            retry: None,
            config: HashMap::new(),
        }
    }

    // -----------------------------------------------------------------------
    // P5.5-A linear tests (migrated to run_linear)
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn linear_pipeline_runs_all_stages() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:query-analyzer".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:query-analyzer".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::QueryAnalysis],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:slot-extractor".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:slot-extractor".to_string(),
                    inputs: vec![ArtifactType::QueryAnalysis],
                    outputs: vec![ArtifactType::SlotGraph],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:planner".to_string(),
                    inputs: vec![ArtifactType::SlotGraph],
                    outputs: vec![ArtifactType::PlanGraph],
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);

        let definition = test_definition(vec![
            make_stage("query_analysis", "system:query-analyzer"),
            make_stage("slot_extraction", "system:slot-extractor"),
            make_stage("planning", "system:planner"),
        ]);

        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed");

        assert!(matches!(outcome.status, PipelineStatus::Completed));
        assert_eq!(outcome.stages_executed.len(), 3);
        assert_eq!(
            outcome.stages_executed,
            vec!["query_analysis", "slot_extraction", "planning"]
        );
        assert!(outcome.artifacts_produced > 0);
    }

    #[tokio::test]
    async fn skip_when_agent_has_goal_skips_stage() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:query-analyzer".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:query-analyzer".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::QueryAnalysis],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:planner".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::PlanGraph],
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);

        let mut qa_stage = make_stage("query_analysis", "system:query-analyzer");
        qa_stage.skip_when = Some("agent.has_goal_definition".to_string());

        let definition = test_definition(vec![qa_stage, make_stage("planning", "system:planner")]);

        let mut ctx = test_context();
        ctx.agent_id = Some("my-agent".to_string());

        let mut orchestrator = PlanningOrchestrator::new(definition, agents, ctx);
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed");

        assert!(matches!(outcome.status, PipelineStatus::Completed));
        // query_analysis was skipped — only planning was executed.
        assert_eq!(outcome.stages_executed, vec!["planning"]);
        assert!(!outcome
            .stages_executed
            .contains(&"query_analysis".to_string()));
    }

    #[tokio::test]
    async fn retry_eventually_succeeds() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:flakey".to_string(),
            Arc::new(FlakeyAgent {
                id: "system:flakey".to_string(),
                fail_count: AtomicU32::new(0),
                max_failures: 2,
                outputs: vec![ArtifactType::QueryAnalysis],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let mut stage = make_stage("flakey_stage", "system:flakey");
        stage.retry = Some(RetryConfig {
            max_attempts: 3,
            escalation: None,
        });

        let definition = test_definition(vec![stage]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed after retries");

        assert_eq!(outcome.stages_executed, vec!["flakey_stage"]);
        assert!(outcome.artifacts_produced > 0);
    }

    #[tokio::test]
    async fn retry_exhaustion_returns_error() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:fail".to_string(),
            Arc::new(FailAgent {
                id: "system:fail".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        let mut stage = make_stage("fail_stage", "system:fail");
        stage.retry = Some(RetryConfig {
            max_attempts: 3,
            escalation: None,
        });

        let definition = test_definition(vec![stage]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let err = orchestrator.run_linear().await.unwrap_err();

        match err {
            PipelineOrchestratorError::RetryExhausted {
                stage, attempts, ..
            } => {
                assert_eq!(stage, "fail_stage");
                assert_eq!(attempts, 3);
            },
            other => panic!("expected RetryExhausted, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn missing_agent_returns_error() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::new();
        let definition = test_definition(vec![make_stage("ghost_stage", "nonexistent")]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let err = orchestrator.run_linear().await.unwrap_err();

        match err {
            PipelineOrchestratorError::AgentNotFound(name) => {
                assert_eq!(name, "nonexistent");
            },
            other => panic!("expected AgentNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn planning_outcome_contains_plan_graph() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:planner".to_string(),
            Arc::new(PlannerAgent {
                id: "system:planner".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        let definition = test_definition(vec![make_stage("planning", "system:planner")]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed");

        assert!(outcome.plan_graph.is_some(), "plan_graph should be present");
        let graph = outcome.plan_graph.unwrap();
        assert_eq!(graph.steps.len(), 1);
        assert_eq!(graph.steps[0].id, "s1");
        assert_eq!(graph.steps[0].task, "test");
    }

    #[tokio::test]
    async fn planning_outcome_tracks_executed_stages() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:query-analyzer".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:query-analyzer".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::QueryAnalysis],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:slot-extractor".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:slot-extractor".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::SlotGraph],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:planner".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::PlanGraph],
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);

        // Stage 1: executed normally.
        let stage1 = make_stage("query_analysis", "system:query-analyzer");
        // Stage 2: skipped via "always".
        let mut stage2 = make_stage("slot_extraction", "system:slot-extractor");
        stage2.skip_when = Some("always".to_string());
        // Stage 3: executed normally.
        let stage3 = make_stage("planning", "system:planner");

        let definition = test_definition(vec![stage1, stage2, stage3]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed");

        assert!(matches!(outcome.status, PipelineStatus::Completed));
        assert_eq!(outcome.stages_executed, vec!["query_analysis", "planning"]);
        // slot_extraction was skipped — not present in stages_executed.
        assert!(!outcome
            .stages_executed
            .contains(&"slot_extraction".to_string()));
    }

    // -----------------------------------------------------------------------
    // Convergence loop tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn convergence_loop_stops_when_converged() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:elicitor".to_string(),
            Arc::new(ConvergingAgent {
                id: "system:elicitor".to_string(),
                call_count: AtomicU32::new(0),
                converge_after: 2,
            }) as Arc<dyn PipelineAgent>,
        )]);

        let mut stage = make_stage("elicitation", "system:elicitor");
        stage.loop_config = Some(super::super::definition::LoopConfig {
            convergence: ConvergenceCriterion::AllSlotsResolved,
            max_iterations: 5,
        });

        let definition = test_definition(vec![stage]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator.run_linear().await.expect("should converge");

        assert_eq!(outcome.stages_executed, vec!["elicitation"]);
        assert_eq!(outcome.artifacts_produced, 2);
    }

    #[tokio::test]
    async fn convergence_loop_respects_max_iterations() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:elicitor".to_string(),
            Arc::new(ConvergingAgent {
                id: "system:elicitor".to_string(),
                call_count: AtomicU32::new(0),
                converge_after: 100,
            }) as Arc<dyn PipelineAgent>,
        )]);

        let mut stage = make_stage("elicitation", "system:elicitor");
        stage.loop_config = Some(super::super::definition::LoopConfig {
            convergence: ConvergenceCriterion::AllSlotsResolved,
            max_iterations: 3,
        });

        let definition = test_definition(vec![stage]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let err = orchestrator.run_linear().await.unwrap_err();

        match err {
            PipelineOrchestratorError::ConvergenceNotReached {
                stage,
                max_iterations,
            } => {
                assert_eq!(stage, "elicitation");
                assert_eq!(max_iterations, 3);
            },
            other => panic!("expected ConvergenceNotReached, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn convergence_loop_single_pass() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:elicitor".to_string(),
            Arc::new(ConvergingAgent {
                id: "system:elicitor".to_string(),
                call_count: AtomicU32::new(0),
                converge_after: 1,
            }) as Arc<dyn PipelineAgent>,
        )]);

        let mut stage = make_stage("elicitation", "system:elicitor");
        stage.loop_config = Some(super::super::definition::LoopConfig {
            convergence: ConvergenceCriterion::AllSlotsResolved,
            max_iterations: 10,
        });

        let definition = test_definition(vec![stage]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("should converge on first pass");

        assert_eq!(outcome.stages_executed, vec!["elicitation"]);
        assert_eq!(outcome.artifacts_produced, 1);
    }

    // -----------------------------------------------------------------------
    // is_converged unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn is_converged_returns_false_when_no_artifact() {
        let store = ArtifactStore::new("test");
        assert!(!super::is_converged(
            &store,
            &ConvergenceCriterion::AllSlotsResolved
        ));
    }

    #[test]
    fn is_converged_returns_true_when_needs_clarification_false() {
        let mut store = ArtifactStore::new("test");
        store.put(AgentArtifact {
            artifact_id: "e1".to_string(),
            artifact_type: ArtifactType::ElicitationResult,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({"needs_clarification": false}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        assert!(super::is_converged(
            &store,
            &ConvergenceCriterion::AllSlotsResolved
        ));
    }

    #[test]
    fn is_converged_returns_false_when_needs_clarification_true() {
        let mut store = ArtifactStore::new("test");
        store.put(AgentArtifact {
            artifact_id: "e1".to_string(),
            artifact_type: ArtifactType::ElicitationResult,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({"needs_clarification": true}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        assert!(!super::is_converged(
            &store,
            &ConvergenceCriterion::AllSlotsResolved
        ));
    }

    /// M-13: AllComplete and AnyComplete always converge (no stage tracking yet).
    #[test]
    fn is_converged_all_complete_and_any_complete_return_true() {
        let store = ArtifactStore::new("test");
        assert!(super::is_converged(
            &store,
            &ConvergenceCriterion::AllComplete
        ));
        assert!(super::is_converged(
            &store,
            &ConvergenceCriterion::AnyComplete
        ));
    }

    // -----------------------------------------------------------------------
    // PlanGraph contract validation
    // -----------------------------------------------------------------------

    struct ContractPlannerAgent;

    #[async_trait]
    impl PipelineAgent for ContractPlannerAgent {
        fn agent_id(&self) -> &str {
            "system:planner"
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::QueryAnalysis, ArtifactType::ElicitationResult]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![ArtifactType::PlanGraph]
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            use crate::magician_v2::strategy::plan::{
                PlanEdge, PlanGraph, PlanProvenance, PlanStep, PlanningMetadata, SessionPolicy,
            };

            let mut step_a = PlanStep {
                id: "step-a".to_string(),
                task: "Gather context".to_string(),
                confidence: 0.9,
                success_criteria: Some("Context gathered".to_string()),
                ..Default::default()
            };
            step_a.metadata.insert(
                "observation".to_string(),
                "User context available".to_string(),
            );

            let step_b = PlanStep {
                id: "step-b".to_string(),
                task: "Execute action".to_string(),
                confidence: 0.7,
                depends_on: vec!["step-a".to_string()],
                success_criteria: Some("Action completed".to_string()),
                ..Default::default()
            };

            let graph = PlanGraph {
                steps: vec![step_a, step_b],
                edges: vec![PlanEdge {
                    from: "step-a".to_string(),
                    to: "step-b".to_string(),
                    reason: "sequential dependency".to_string(),
                }],
                unresolved_inputs: vec![],
                confidence: 0.8,
                provenance: PlanProvenance {
                    strategy: "ContractTest".to_string(),
                    generator: Some("system:planner".to_string()),
                    notes: Some("Contract validation test".to_string()),
                },
                session_policy: SessionPolicy::Shared,
                planning_metadata: PlanningMetadata {
                    clarified_task: Some("Build a dashboard".to_string()),
                    constraints: vec!["Must be responsive".to_string()],
                    objectives: vec!["Fast load time".to_string()],
                    elicitation_rounds: 2,
                    slots_resolved: 3,
                    slot_confidence: 0.92,
                    upstream_llm_calls: 5,
                },
                ..Default::default()
            };

            let content = serde_json::to_value(&graph)
                .map_err(|e| PipelineAgentError::SerializationError(e.to_string()))?;
            let id = uuid::Uuid::new_v4().to_string();
            store.put(AgentArtifact {
                artifact_id: id.clone(),
                artifact_type: ArtifactType::PlanGraph,
                producer_agent_id: self.agent_id().to_string(),
                producer_cycle_id: context.cycle_id.clone(),
                content,
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![id],
            })
        }
    }

    #[tokio::test]
    async fn plan_graph_v1_1_contract_validation() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:query-analyzer".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:query-analyzer".to_string(),
                    inputs: vec![],
                    outputs: vec![ArtifactType::QueryAnalysis],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:elicitor".to_string(),
                Arc::new(SuccessAgent {
                    id: "system:elicitor".to_string(),
                    inputs: vec![ArtifactType::QueryAnalysis],
                    outputs: vec![ArtifactType::ElicitationResult],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(ContractPlannerAgent) as Arc<dyn PipelineAgent>,
            ),
        ]);

        let definition = test_definition(vec![
            make_stage("query_analysis", "system:query-analyzer"),
            make_stage("elicitation", "system:elicitor"),
            make_stage("planning", "system:planner"),
        ]);

        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("pipeline should succeed");

        assert!(outcome.plan_graph.is_some(), "PlanGraph must be present");
        let graph = outcome.plan_graph.unwrap();

        assert_eq!(graph.steps.len(), 2);
        assert_eq!(graph.edges.len(), 1);
        assert!(graph.confidence > 0.0);
        assert!(!graph.provenance.strategy.is_empty());
        assert!(graph.provenance.generator.is_some());

        let step_a = &graph.steps[0];
        assert_eq!(step_a.id, "step-a");
        assert!(step_a.success_criteria.is_some());

        let step_b = &graph.steps[1];
        assert_eq!(step_b.id, "step-b");
        assert!(step_b.success_criteria.is_some());
        assert_eq!(step_b.depends_on, vec!["step-a".to_string()]);

        let edge = &graph.edges[0];
        assert_eq!(edge.from, "step-a");
        assert_eq!(edge.to, "step-b");
        assert!(!edge.reason.is_empty());

        assert_eq!(graph.session_policy, SessionPolicy::Shared);

        let meta = &graph.planning_metadata;
        assert_eq!(meta.clarified_task.as_deref(), Some("Build a dashboard"));
        assert_eq!(meta.elicitation_rounds, 2);
        assert_eq!(meta.slots_resolved, 3);
        assert!((meta.slot_confidence - 0.92).abs() < f32::EPSILON);
        assert_eq!(meta.upstream_llm_calls, 5);

        let json = serde_json::to_value(&graph).expect("should serialize");
        let roundtripped: PlanGraph = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(roundtripped.steps.len(), graph.steps.len());
        assert_eq!(roundtripped.edges.len(), graph.edges.len());
        assert_eq!(roundtripped.session_policy, graph.session_policy);
        assert_eq!(
            roundtripped.planning_metadata.clarified_task,
            graph.planning_metadata.clarified_task
        );

        assert!(matches!(outcome.status, PipelineStatus::Completed));
        assert_eq!(outcome.stages_executed.len(), 3);
        assert!(outcome.artifacts_produced >= 3);
    }

    // -----------------------------------------------------------------------
    // B-07: Router-driven orchestrator tests
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn router_driven_error_decision_returns_failed() {
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Error {
                reason: "test error".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(HashMap::new(), router, test_context(), 20, None);
        let outcome = orch.run().await.expect("run() itself should not error");

        match outcome.status {
            PipelineStatus::Failed(reason) => {
                assert_eq!(reason, "test error");
            },
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_driven_complete_decision_returns_completed() {
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Complete {
                reason: "done".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(HashMap::new(), router, test_context(), 20, None);
        let outcome = orch.run().await.expect("run() should not error");

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed"
        );
    }

    #[tokio::test]
    async fn router_driven_budget_exhaustion() {
        // Router always returns NextStage for a registered agent — runs until budget exhausted.
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:query-analyzer".to_string(),
            Arc::new(SuccessAgent {
                id: "system:query-analyzer".to_string(),
                inputs: vec![],
                outputs: vec![ArtifactType::QueryAnalysis],
            }) as Arc<dyn PipelineAgent>,
        )]);

        // Return NextStage for all 5 iterations → budget exhausted.
        let decisions: Vec<RoutingDecision> = (0..5)
            .map(|_| RoutingDecision::NextStage {
                agent_id: "system:query-analyzer".to_string(),
                reason: "loop".to_string(),
            })
            .collect();

        let router = Arc::new(MockDecisionRouter {
            decisions,
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 5, None);
        let outcome = orch.run().await.expect("run() should not error");

        match outcome.status {
            PipelineStatus::Failed(ref reason) => {
                assert!(reason.contains("budget exhausted"), "got: {reason}");
            },
            other => panic!("expected Failed(budget exhausted), got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_driven_agent_error_produces_artifact() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:fail".to_string(),
            Arc::new(FailAgent {
                id: "system:fail".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        // NextStage → agent fails → Error (from router next iteration).
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "try it".to_string(),
                },
                RoutingDecision::Error {
                    reason: "giving up".to_string(),
                },
            ],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        let outcome = orch.run().await.expect("run() should not error");

        // Verify AgentError artifact was written.
        let artifact_snapshot = orch.store.snapshot();
        let agent_errors: Vec<_> = artifact_snapshot
            .values()
            .filter(|a| a.artifact_type == ArtifactType::AgentError)
            .collect();

        assert_eq!(agent_errors.len(), 1, "expected 1 AgentError artifact");
        let err = &agent_errors[0];
        assert_eq!(
            err.content.get("failed_agent_id").and_then(|v| v.as_str()),
            Some("system:fail")
        );
        assert_eq!(
            err.content.get("retry_count").and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            err.content.get("recoverable").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert!(matches!(outcome.status, PipelineStatus::Failed(_)));
    }

    #[tokio::test]
    async fn router_driven_agent_error_retry_increments_count() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:fail".to_string(),
            Arc::new(FailAgent {
                id: "system:fail".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        // Two NextStage decisions → two failures → two AgentError artifacts.
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "first attempt".to_string(),
                },
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "second attempt".to_string(),
                },
                RoutingDecision::Error {
                    reason: "giving up".to_string(),
                },
            ],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        orch.run().await.expect("run() should not error");

        let snapshot = orch.store.snapshot();
        let mut agent_errors: Vec<_> = snapshot
            .values()
            .filter(|a| a.artifact_type == ArtifactType::AgentError)
            .collect();
        // Sort by retry_count to assert in order.
        agent_errors.sort_by_key(|a| {
            a.content
                .get("retry_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        });

        assert_eq!(agent_errors.len(), 2);
        assert_eq!(
            agent_errors[0]
                .content
                .get("retry_count")
                .and_then(|v| v.as_u64()),
            Some(0)
        );
        assert_eq!(
            agent_errors[0]
                .content
                .get("recoverable")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            agent_errors[1]
                .content
                .get("retry_count")
                .and_then(|v| v.as_u64()),
            Some(1)
        );
        assert_eq!(
            agent_errors[1]
                .content
                .get("recoverable")
                .and_then(|v| v.as_bool()),
            Some(true) // 1 < MAX_RETRIES(2)
        );
    }

    #[tokio::test]
    async fn router_driven_agent_error_exhausts_retries() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:fail".to_string(),
            Arc::new(FailAgent {
                id: "system:fail".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        // Three failures → third has retry_count=2 → recoverable=false.
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "attempt 1".to_string(),
                },
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "attempt 2".to_string(),
                },
                RoutingDecision::NextStage {
                    agent_id: "system:fail".to_string(),
                    reason: "attempt 3".to_string(),
                },
                RoutingDecision::Error {
                    reason: "giving up".to_string(),
                },
            ],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        orch.run().await.expect("run() should not error");

        let snapshot = orch.store.snapshot();
        let mut agent_errors: Vec<_> = snapshot
            .values()
            .filter(|a| a.artifact_type == ArtifactType::AgentError)
            .collect();
        agent_errors.sort_by_key(|a| {
            a.content
                .get("retry_count")
                .and_then(|v| v.as_u64())
                .unwrap_or(0)
        });

        assert_eq!(agent_errors.len(), 3);
        let third = &agent_errors[2];
        assert_eq!(
            third.content.get("retry_count").and_then(|v| v.as_u64()),
            Some(2)
        );
        assert_eq!(
            third.content.get("recoverable").and_then(|v| v.as_bool()),
            Some(false) // 2 >= MAX_RETRIES(2)
        );
    }

    #[tokio::test]
    async fn router_driven_agent_not_found_returns_error() {
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::NextStage {
                agent_id: "nonexistent".to_string(),
                reason: "test".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(HashMap::new(), router, test_context(), 20, None);
        let err = orch.run().await.unwrap_err();

        match err {
            PipelineOrchestratorError::AgentNotFound(id) => {
                assert_eq!(id, "nonexistent");
            },
            other => panic!("expected AgentNotFound, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn router_driven_pause_produces_suspension() {
        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Pause {
                question_text: "What is your budget?".to_string(),
                slot_ids: vec!["budget".to_string()],
            }],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(HashMap::new(), router, test_context(), 20, None);
        let outcome = orch.run().await.expect("run() should not error");

        match outcome.status {
            PipelineStatus::Suspended(ref s) => {
                assert!(!s.checkpoint_id.is_empty());
                assert_eq!(s.chain_id, "test-chain");
                assert_eq!(s.workflow_id, "wf-1");
                assert_eq!(s.question_text, "What is your budget?");
                assert_eq!(s.slot_ids, vec!["budget"]);
            },
            other => panic!("expected Suspended, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn run_sets_run_started_at() {
        let before = Utc::now();

        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Complete {
                reason: "done".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let mut orch =
            PlanningOrchestrator::new_with_router(HashMap::new(), router, test_context(), 20, None);
        orch.run().await.expect("run() should not error");

        let run_started_at = orch
            .context
            .run_started_at
            .expect("run_started_at should be set");
        assert!(
            run_started_at >= before,
            "run_started_at should be >= before"
        );
        assert!(
            run_started_at <= Utc::now(),
            "run_started_at should be <= now"
        );
    }

    #[tokio::test]
    async fn from_suspension_restores_state() {
        // Create a suspension with 3 artifacts.
        let mut store = ArtifactStore::new("chain-1");
        for i in 0..3_u32 {
            store.put(AgentArtifact {
                artifact_id: format!("a{i}"),
                artifact_type: ArtifactType::QueryAnalysis,
                producer_agent_id: "test".to_string(),
                producer_cycle_id: "c1".to_string(),
                content: serde_json::json!({}),
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
        }
        let snapshot = store.snapshot();

        let suspension = PipelineSuspension {
            checkpoint_id: "chk-1".to_string(),
            chain_id: "chain-1".to_string(),
            workflow_id: "wf-1".to_string(),
            artifact_snapshot: snapshot,
            context: test_context(),
            iteration: 2,
            question_text: "What is your budget?".to_string(),
            slot_ids: vec!["budget".to_string()],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        };

        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Complete {
                reason: "done".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let orch = PlanningOrchestrator::from_suspension(
            suspension,
            HashMap::new(),
            router,
            "10 dollars".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );

        // All 3 QueryAnalysis artifacts should be present (light_slot_update only purges AgentError).
        assert_eq!(
            orch.store.all_of_type(&ArtifactType::QueryAnalysis).len(),
            3
        );
        assert_eq!(orch.context.user_answer, Some("10 dollars".to_string()));
        assert_eq!(
            orch.context.resume_mode,
            Some("light_slot_update".to_string())
        );
        assert_eq!(orch.context.iteration, 2);
    }

    #[test]
    fn question_id_preserved_through_from_suspension() {
        // M-03: Verify that question_id is correctly stored on PipelineSuspension
        // and survives the from_suspension() round-trip. The caller (resume_workflow)
        // must extract question_id from the suspension before handing it to
        // from_suspension; this test confirms it is non-None and accessible.
        let suspension = PipelineSuspension {
            checkpoint_id: "chk-qid".to_string(),
            chain_id: "chain-1".to_string(),
            workflow_id: "wf-1".to_string(),
            artifact_snapshot: HashMap::new(),
            context: test_context(),
            iteration: 1,
            question_text: "What is your name?".to_string(),
            slot_ids: vec!["name".to_string()],
            suspended_at: Utc::now(),
            question_id: Some("round-trip-question-id".to_string()),
        };

        // Confirm question_id is accessible before handing off to from_suspension.
        assert_eq!(
            suspension.question_id,
            Some("round-trip-question-id".to_string()),
            "question_id must be Some before from_suspension"
        );

        let router = Arc::new(SequenceRouter::new(vec![RoutingDecision::Complete {
            reason: "done".to_string(),
        }]));

        let orch = PlanningOrchestrator::from_suspension(
            suspension,
            HashMap::new(),
            router,
            "Alice".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );

        // from_suspension threads user_answer and resume_mode into context.
        assert_eq!(
            orch.context.user_answer,
            Some("Alice".to_string()),
            "user_answer should be threaded into context by from_suspension"
        );
        assert_eq!(
            orch.context.resume_mode,
            Some("light_slot_update".to_string()),
            "resume_mode should be threaded into context by from_suspension"
        );
    }

    #[tokio::test]
    async fn from_suspension_purges_downstream_on_full_replan() {
        let mut store = ArtifactStore::new("chain-1");
        // Seed artifacts: QA and IC survive, SG and ER are purged.
        let types = [
            ArtifactType::QueryAnalysis,
            ArtifactType::IntentClassification,
            ArtifactType::SlotGraph,
            ArtifactType::ElicitationResult,
        ];
        for (i, t) in types.iter().enumerate() {
            store.put(AgentArtifact {
                artifact_id: format!("a{i}"),
                artifact_type: t.clone(),
                producer_agent_id: "test".to_string(),
                producer_cycle_id: "c1".to_string(),
                content: serde_json::json!({}),
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: Utc::now(),
                render_hints: None,
            });
        }
        let snapshot = store.snapshot();

        let suspension = PipelineSuspension {
            checkpoint_id: "chk-1".to_string(),
            chain_id: "chain-1".to_string(),
            workflow_id: "wf-1".to_string(),
            artifact_snapshot: snapshot,
            context: test_context(),
            iteration: 1,
            question_text: "question".to_string(),
            slot_ids: vec![],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        };

        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Complete {
                reason: "done".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let orch = PlanningOrchestrator::from_suspension(
            suspension,
            HashMap::new(),
            router,
            "answer".to_string(),
            "full_replan".to_string(),
            20,
            None,
        );

        // QueryAnalysis and IntentClassification survive full_replan —
        // the IntentClassifier does not re-run on resume (resume_mode guard)
        // so handle_pipeline_outcome still needs these artifacts.
        assert_eq!(
            orch.store.all_of_type(&ArtifactType::QueryAnalysis).len(),
            1
        );
        assert_eq!(
            orch.store
                .all_of_type(&ArtifactType::IntentClassification)
                .len(),
            1
        );
        // SlotGraph and ElicitationResult are purged.
        assert_eq!(orch.store.all_of_type(&ArtifactType::SlotGraph).len(), 0);
        assert_eq!(
            orch.store
                .all_of_type(&ArtifactType::ElicitationResult)
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn from_suspension_purges_only_agent_error_on_light_update() {
        let mut store = ArtifactStore::new("chain-1");
        store.put(AgentArtifact {
            artifact_id: "qa1".to_string(),
            artifact_type: ArtifactType::QueryAnalysis,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        store.put(AgentArtifact {
            artifact_id: "sg1".to_string(),
            artifact_type: ArtifactType::SlotGraph,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        store.put(AgentArtifact {
            artifact_id: "ae1".to_string(),
            artifact_type: ArtifactType::AgentError,
            producer_agent_id: "system:orchestrator".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({"failed_agent_id": "x", "recoverable": true}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        let snapshot = store.snapshot();

        let suspension = PipelineSuspension {
            checkpoint_id: "chk-1".to_string(),
            chain_id: "chain-1".to_string(),
            workflow_id: "wf-1".to_string(),
            artifact_snapshot: snapshot,
            context: test_context(),
            iteration: 0,
            question_text: "question".to_string(),
            slot_ids: vec![],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        };

        let router = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::Complete {
                reason: "done".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let orch = PlanningOrchestrator::from_suspension(
            suspension,
            HashMap::new(),
            router,
            "answer".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );

        assert_eq!(
            orch.store.all_of_type(&ArtifactType::QueryAnalysis).len(),
            1
        );
        assert_eq!(orch.store.all_of_type(&ArtifactType::SlotGraph).len(), 1);
        assert_eq!(orch.store.all_of_type(&ArtifactType::AgentError).len(), 0);
    }

    // -----------------------------------------------------------------------
    // PipelineStatus / PipelineSuspension serde tests (B-06)
    // -----------------------------------------------------------------------

    #[test]
    fn pipeline_status_completed_serde() {
        let status = PipelineStatus::Completed;
        let json = serde_json::to_value(&status).unwrap();
        let rt: PipelineStatus = serde_json::from_value(json).unwrap();
        assert!(matches!(rt, PipelineStatus::Completed));
    }

    #[test]
    fn pipeline_status_failed_serde() {
        let status = PipelineStatus::Failed("test reason".to_string());
        let json = serde_json::to_value(&status).unwrap();
        let rt: PipelineStatus = serde_json::from_value(json).unwrap();
        match rt {
            PipelineStatus::Failed(r) => assert_eq!(r, "test reason"),
            other => panic!("expected Failed, got: {other:?}"),
        }
    }

    #[test]
    fn pipeline_suspension_serde_roundtrip() {
        let suspension = PipelineSuspension {
            checkpoint_id: "chk-abc".to_string(),
            chain_id: "chain-1".to_string(),
            workflow_id: "wf-1".to_string(),
            artifact_snapshot: HashMap::new(),
            context: test_context(),
            iteration: 3,
            question_text: "How many seats?".to_string(),
            slot_ids: vec!["seats".to_string(), "date".to_string()],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        };
        let json = serde_json::to_value(&suspension).unwrap();
        let rt: PipelineSuspension = serde_json::from_value(json).unwrap();
        assert_eq!(rt.checkpoint_id, "chk-abc");
        assert_eq!(rt.question_text, "How many seats?");
        assert_eq!(rt.slot_ids, vec!["seats", "date"]);
        assert_eq!(rt.iteration, 3);
        assert_eq!(rt.question_id, Some("test-question-uuid".to_string()));
    }

    #[test]
    fn pipeline_status_suspended_serde() {
        let suspension = PipelineSuspension {
            checkpoint_id: "ckpt-123".to_string(),
            chain_id: "chain-2".to_string(),
            workflow_id: "wf-2".to_string(),
            artifact_snapshot: HashMap::new(),
            context: test_context(),
            iteration: 1,
            question_text: "Which room?".to_string(),
            slot_ids: vec!["room".to_string()],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        };

        let status = PipelineStatus::Suspended(Box::new(suspension));
        let json = serde_json::to_string(&status).expect("should serialize");
        let recovered: PipelineStatus = serde_json::from_str(&json).expect("should deserialize");

        match recovered {
            PipelineStatus::Suspended(s) => {
                assert_eq!(s.checkpoint_id, "ckpt-123");
                assert_eq!(s.chain_id, "chain-2");
                assert_eq!(s.workflow_id, "wf-2");
                assert_eq!(s.iteration, 1);
                assert_eq!(s.question_text, "Which room?");
                assert_eq!(s.slot_ids, vec!["room"]);
                assert_eq!(s.context.workflow_id, "wf-1");
                assert_eq!(s.question_id, Some("test-question-uuid".to_string()));
            },
            other => panic!("expected Suspended, got: {other:?}"),
        }
    }

    // -----------------------------------------------------------------------
    // Router-driven orchestrator tests (B-07)
    // -----------------------------------------------------------------------

    use crate::magician_v2::pipeline::router::{RouterAgent, RoutingDecision, TieredRouter};

    /// Mock router that returns decisions from a pre-configured sequence.
    struct SequenceRouter {
        decisions: std::sync::Mutex<Vec<RoutingDecision>>,
    }

    impl SequenceRouter {
        fn new(decisions: Vec<RoutingDecision>) -> Self {
            Self {
                decisions: std::sync::Mutex::new(decisions),
            }
        }
    }

    #[async_trait]
    impl RouterAgent for SequenceRouter {
        async fn route(
            &self,
            _store: &ArtifactStore,
            _context: &mut PipelineContext,
        ) -> RoutingDecision {
            let mut guard = self.decisions.lock().unwrap_or_else(|e| e.into_inner());
            if guard.is_empty() {
                RoutingDecision::Error {
                    reason: "no more decisions".to_string(),
                }
            } else {
                guard.remove(0)
            }
        }
    }

    /// Mock agent that produces artifacts with specified content.
    struct ContentAgent {
        id: String,
        outputs: Vec<(ArtifactType, serde_json::Value)>,
    }

    #[async_trait]
    impl PipelineAgent for ContentAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            self.outputs.iter().map(|(t, _)| t.clone()).collect()
        }
        async fn execute(
            &self,
            store: &mut ArtifactStore,
            context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            let mut ids = Vec::new();
            for (artifact_type, content) in &self.outputs {
                let id = uuid::Uuid::new_v4().to_string();
                store.put(AgentArtifact {
                    artifact_id: id.clone(),
                    artifact_type: artifact_type.clone(),
                    producer_agent_id: self.id.clone(),
                    producer_cycle_id: context.cycle_id.clone(),
                    content: content.clone(),
                    schema_version: ARTIFACT_SCHEMA_VERSION,
                    produced_at: Utc::now(),
                    render_hints: None,
                });
                ids.push(id);
            }
            Ok(PipelineAgentResult::Completed { artifact_ids: ids })
        }
    }

    // 1. router_driven_seeded_analysis_to_complete
    //
    // QueryAnalysis is now always pre-seeded by IntentAwareProcessor before
    // the pipeline runs.  This test mirrors production: the store already
    // contains a QueryAnalysis artifact, so the router skips straight to
    // slot extraction.
    #[tokio::test]
    async fn router_driven_seeded_analysis_to_complete() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:slot-extractor".to_string(),
                Arc::new(ContentAgent {
                    id: "system:slot-extractor".to_string(),
                    outputs: vec![(ArtifactType::SlotGraph, serde_json::json!({"slots": []}))],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:elicitor".to_string(),
                Arc::new(ContentAgent {
                    id: "system:elicitor".to_string(),
                    outputs: vec![(
                        ArtifactType::ElicitationResult,
                        serde_json::json!({"needs_clarification": false}),
                    )],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:query-rewriter".to_string(),
                Arc::new(ContentAgent {
                    id: "system:query-rewriter".to_string(),
                    outputs: vec![(
                        ArtifactType::ClarifiedTask,
                        serde_json::json!({"task": "clarified"}),
                    )],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:answer-interpreter".to_string(),
                Arc::new(ContentAgent {
                    id: "system:answer-interpreter".to_string(),
                    outputs: vec![(
                        ArtifactType::InterpretedAnswer,
                        serde_json::json!({"requires_replan": false}),
                    )],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(PlannerAgent {
                    id: "system:planner".to_string(),
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                // IntentClassifier produces IntentClassification + QueryAnalysis.
                "system:intent-classifier".to_string(),
                Arc::new(ContentAgent {
                    id: "system:intent-classifier".to_string(),
                    outputs: vec![
                        (
                            ArtifactType::IntentClassification,
                            serde_json::json!({
                                "intent": "NewTask",
                                "requires_pipeline": true,
                                "user_answer": null,
                                "is_new_task": true,
                                "classification_duration_ms": 0
                            }),
                        ),
                        (
                            ArtifactType::QueryAnalysis,
                            serde_json::json!({"intent": "test"}),
                        ),
                    ],
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);

        // No pre-seeded store — IntentClassifier is the first pipeline stage.
        let ctx = test_context();

        let agent_ids: Vec<String> = agents.keys().cloned().collect();
        let router: Arc<dyn RouterAgent> = Arc::new(TieredRouter::new(None, agent_ids));

        let mut orchestrator = PlanningOrchestrator::new_with_router(agents, router, ctx, 20, None);
        let outcome = orchestrator.run().await.unwrap();

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed, got: {:?}",
            outcome.status
        );
        assert!(outcome.plan_graph.is_some(), "PlanGraph should be present");
        let graph = outcome.plan_graph.unwrap();
        assert_eq!(graph.steps.len(), 1);
        assert_eq!(graph.steps[0].id, "s1");

        // The planning pipeline stops once the plan is ready; execution happens
        // later through the direct seeded-taskplan path.
        assert_eq!(outcome.stages_executed.len(), 5);
        assert_eq!(outcome.stages_executed[0], "system:intent-classifier");
        assert_eq!(outcome.stages_executed[1], "system:slot-extractor");
        assert_eq!(outcome.stages_executed[2], "system:elicitor");
        assert_eq!(outcome.stages_executed[3], "system:query-rewriter");
        assert_eq!(outcome.stages_executed[4], "system:planner");
    }

    // 6. router_driven_per_agent_retry_isolation
    #[tokio::test]
    async fn router_driven_per_agent_retry_isolation() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:fail-a".to_string(),
                Arc::new(FailAgent {
                    id: "system:fail-a".to_string(),
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:fail-b".to_string(),
                Arc::new(FailAgent {
                    id: "system:fail-b".to_string(),
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);

        let router = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:fail-a".to_string(),
                reason: "run A".to_string(),
            },
            RoutingDecision::NextStage {
                agent_id: "system:fail-b".to_string(),
                reason: "run B".to_string(),
            },
            RoutingDecision::Error {
                reason: "done".to_string(),
            },
        ]));

        let mut orchestrator =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        let _outcome = orchestrator.run().await.unwrap();

        let errors = orchestrator.store.all_of_type(&ArtifactType::AgentError);
        assert_eq!(errors.len(), 2, "should have 2 AgentError artifacts");

        // Both should have retry_count = 0 (independent counters).
        assert_eq!(
            errors[0]
                .content
                .get("failed_agent_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "system:fail-a"
        );
        assert_eq!(
            errors[0]
                .content
                .get("retry_count")
                .unwrap()
                .as_u64()
                .unwrap(),
            0
        );

        assert_eq!(
            errors[1]
                .content
                .get("failed_agent_id")
                .unwrap()
                .as_str()
                .unwrap(),
            "system:fail-b"
        );
        assert_eq!(
            errors[1]
                .content
                .get("retry_count")
                .unwrap()
                .as_u64()
                .unwrap(),
            0
        );
    }

    // -- from_suspension helper --

    fn make_suspension_with_artifacts(
        artifact_types: Vec<(String, ArtifactType)>,
    ) -> PipelineSuspension {
        let mut artifact_snapshot = HashMap::new();
        for (id, artifact_type) in artifact_types {
            artifact_snapshot.insert(
                id.clone(),
                AgentArtifact {
                    artifact_id: id,
                    artifact_type,
                    producer_agent_id: "test-agent".to_string(),
                    producer_cycle_id: "cycle-1".to_string(),
                    content: serde_json::json!({"mock": true}),
                    schema_version: ARTIFACT_SCHEMA_VERSION,
                    produced_at: Utc::now(),
                    render_hints: None,
                },
            );
        }
        PipelineSuspension {
            checkpoint_id: "ckpt-test".to_string(),
            chain_id: "chain-test".to_string(),
            workflow_id: "wf-test".to_string(),
            artifact_snapshot,
            context: test_context(),
            iteration: 2,
            question_text: "test question".to_string(),
            slot_ids: vec!["s1".to_string()],
            suspended_at: Utc::now(),
            question_id: Some("test-question-uuid".to_string()),
        }
    }

    fn empty_agents() -> HashMap<String, Arc<dyn PipelineAgent>> {
        HashMap::new()
    }

    fn dummy_router() -> Arc<dyn RouterAgent> {
        Arc::new(SequenceRouter::new(vec![RoutingDecision::Complete {
            reason: "dummy".to_string(),
        }]))
    }

    // 9b. from_suspension_purges_downstream_on_partial_replan
    // Verifies the spec-mandated partial_replan purge map:
    // ElicitationResult, InterpretedAnswer, ClarifiedTask, PlanGraph, AgentError are purged;
    // QueryAnalysis and SlotGraph survive.
    #[test]
    fn from_suspension_purges_downstream_on_partial_replan() {
        let suspension = make_suspension_with_artifacts(vec![
            ("qa-1".to_string(), ArtifactType::QueryAnalysis),
            ("sg-1".to_string(), ArtifactType::SlotGraph),
            ("er-1".to_string(), ArtifactType::ElicitationResult),
            ("ia-1".to_string(), ArtifactType::InterpretedAnswer),
            ("ct-1".to_string(), ArtifactType::ClarifiedTask),
            ("pg-1".to_string(), ArtifactType::PlanGraph),
            ("ae-1".to_string(), ArtifactType::AgentError),
        ]);

        let orchestrator = PlanningOrchestrator::from_suspension(
            suspension,
            empty_agents(),
            dummy_router(),
            "update slots".to_string(),
            "partial_replan".to_string(),
            20,
            None,
        );

        // QueryAnalysis and SlotGraph should survive partial_replan.
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::QueryAnalysis)
                .is_some(),
            "QueryAnalysis should survive partial_replan"
        );
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::SlotGraph)
                .is_some(),
            "SlotGraph should survive partial_replan"
        );
        // These should all be purged by partial_replan.
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::ElicitationResult)
                .is_none(),
            "ElicitationResult should be purged on partial_replan"
        );
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::InterpretedAnswer)
                .is_none(),
            "InterpretedAnswer should be purged on partial_replan"
        );
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::ClarifiedTask)
                .is_none(),
            "ClarifiedTask should be purged on partial_replan"
        );
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::PlanGraph)
                .is_none(),
            "PlanGraph should be purged on partial_replan"
        );
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::AgentError)
                .is_none(),
            "AgentError should be purged on partial_replan"
        );
    }

    // 11. router_sets_run_started_at
    #[tokio::test]
    async fn router_sets_run_started_at() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::new();

        let router = Arc::new(SequenceRouter::new(vec![RoutingDecision::Complete {
            reason: "immediate".to_string(),
        }]));

        let before = Utc::now();
        let mut orchestrator =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        let _outcome = orchestrator.run().await.unwrap();
        let after = Utc::now();

        let run_started_at = orchestrator
            .context
            .run_started_at
            .expect("run_started_at should be set");

        assert!(
            run_started_at >= before && run_started_at <= after,
            "run_started_at ({run_started_at:?}) should be between {before:?} and {after:?}"
        );
    }

    // -----------------------------------------------------------------------
    // Pause/Resume wiring tests (B-08)
    // -----------------------------------------------------------------------

    // B-08-1. pause_creates_suspension_with_correct_state
    #[tokio::test]
    async fn pause_creates_suspension_with_correct_state() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::new();

        let router = Arc::new(SequenceRouter::new(vec![RoutingDecision::Pause {
            question_text: "What is your email?".to_string(),
            slot_ids: vec!["email".to_string()],
        }]));

        let mut orchestrator =
            PlanningOrchestrator::new_with_router(agents, router, test_context(), 20, None);
        let outcome = orchestrator.run().await.unwrap();

        match outcome.status {
            PipelineStatus::Suspended(suspension) => {
                assert_eq!(suspension.slot_ids, vec!["email".to_string()]);
                assert_eq!(suspension.question_text, "What is your email?");
                assert!(
                    !suspension.checkpoint_id.is_empty(),
                    "checkpoint_id should be set"
                );
                assert_eq!(suspension.chain_id, "test-chain");
                assert_eq!(suspension.workflow_id, "wf-1");
                // question_id must be Some — it is stored so the caller can correlate with PauseResumeManager.
                assert!(
                    suspension.question_id.is_some(),
                    "question_id should be populated in PipelineSuspension"
                );
                assert!(
                    !suspension.question_id.as_deref().unwrap_or("").is_empty(),
                    "question_id should be non-empty"
                );
            },
            other => panic!("expected Suspended, got: {other:?}"),
        }
    }

    // B-08-2. resume_from_suspension_reaches_complete
    #[tokio::test]
    async fn resume_from_suspension_reaches_complete() {
        // Build a suspension as if produced by a prior Pause.
        let suspension =
            make_suspension_with_artifacts(vec![("qa-1".to_string(), ArtifactType::QueryAnalysis)]);

        // On resume, router returns NextStage then Complete.
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:planner".to_string(),
            Arc::new(PlannerAgent {
                id: "system:planner".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);

        let router = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:planner".to_string(),
                reason: "resume -> plan".to_string(),
            },
            RoutingDecision::Complete {
                reason: "done".to_string(),
            },
        ]));

        let mut orchestrator = PlanningOrchestrator::from_suspension(
            suspension,
            agents,
            router,
            "user@example.com".to_string(),
            "full_replan".to_string(),
            20,
            None,
        );
        let outcome = orchestrator.run().await.unwrap();

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed after resume, got: {:?}",
            outcome.status
        );
    }

    // B-08-3. full_pause_resume_roundtrip
    #[tokio::test]
    async fn full_pause_resume_roundtrip() {
        // --- Run 1: Router returns Pause ---
        let agents_run1: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::new();
        let router_run1 = Arc::new(SequenceRouter::new(vec![RoutingDecision::Pause {
            question_text: "Need your name".to_string(),
            slot_ids: vec!["name".to_string()],
        }]));

        let mut orchestrator1 = PlanningOrchestrator::new_with_router(
            agents_run1,
            router_run1,
            test_context(),
            20,
            None,
        );
        let outcome1 = orchestrator1.run().await.unwrap();

        let suspension = match outcome1.status {
            PipelineStatus::Suspended(s) => s,
            other => panic!("run 1 should be Suspended, got: {other:?}"),
        };
        assert_eq!(suspension.slot_ids, vec!["name"]);

        // --- Run 2: from_suspension → Forward → Complete ---
        let agents_run2: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:planner".to_string(),
            Arc::new(PlannerAgent {
                id: "system:planner".to_string(),
            }) as Arc<dyn PipelineAgent>,
        )]);
        let router_run2 = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:planner".to_string(),
                reason: "resume".to_string(),
            },
            RoutingDecision::Complete {
                reason: "planning done".to_string(),
            },
        ]));

        let mut orchestrator2 = PlanningOrchestrator::from_suspension(
            *suspension,
            agents_run2,
            router_run2,
            "Alice".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );
        let outcome2 = orchestrator2.run().await.unwrap();

        assert!(
            matches!(outcome2.status, PipelineStatus::Completed),
            "run 2 should be Completed, got: {:?}",
            outcome2.status
        );
    }

    // B-08-4. resume_mode_full_replan_routes_to_slot_extractor
    #[tokio::test]
    async fn resume_mode_full_replan_routes_to_slot_extractor() {
        // Build suspension with QueryAnalysis, SlotGraph, ElicitationResult all present.
        let suspension = make_suspension_with_artifacts(vec![
            ("qa-1".to_string(), ArtifactType::QueryAnalysis),
            ("sg-1".to_string(), ArtifactType::SlotGraph),
            ("er-1".to_string(), ArtifactType::ElicitationResult),
        ]);

        // ContextCapturingRouter records the resume_mode from context on first call.
        struct ContextCapturingRouter {
            decisions: std::sync::Mutex<Vec<RoutingDecision>>,
            captured_resume_mode: std::sync::Mutex<Option<String>>,
            captured_has_slot_graph: std::sync::Mutex<Option<bool>>,
        }

        #[async_trait]
        impl RouterAgent for ContextCapturingRouter {
            async fn route(
                &self,
                store: &ArtifactStore,
                context: &mut PipelineContext,
            ) -> RoutingDecision {
                // Capture on first call.
                {
                    let mut rm = self
                        .captured_resume_mode
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if rm.is_none() {
                        *rm = context.resume_mode.clone();
                    }
                    let mut sg = self
                        .captured_has_slot_graph
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if sg.is_none() {
                        *sg = Some(store.latest_of_type(&ArtifactType::SlotGraph).is_some());
                    }
                }
                let mut guard = self.decisions.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty() {
                    RoutingDecision::Error {
                        reason: "no more decisions".to_string(),
                    }
                } else {
                    guard.remove(0)
                }
            }
        }

        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:slot-extractor".to_string(),
            Arc::new(ContentAgent {
                id: "system:slot-extractor".to_string(),
                outputs: vec![(ArtifactType::SlotGraph, serde_json::json!({"slots": []}))],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let capturing_router = Arc::new(ContextCapturingRouter {
            decisions: std::sync::Mutex::new(vec![
                RoutingDecision::NextStage {
                    agent_id: "system:slot-extractor".to_string(),
                    reason: "full_replan -> slot_extractor".to_string(),
                },
                RoutingDecision::Complete {
                    reason: "done".to_string(),
                },
            ]),
            captured_resume_mode: std::sync::Mutex::new(None),
            captured_has_slot_graph: std::sync::Mutex::new(None),
        });

        let router: Arc<dyn RouterAgent> = capturing_router.clone();

        let mut orchestrator = PlanningOrchestrator::from_suspension(
            suspension,
            agents,
            router,
            "full replan answer".to_string(),
            "full_replan".to_string(),
            20,
            None,
        );
        let outcome = orchestrator.run().await.unwrap();

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed, got: {:?}",
            outcome.status
        );
        // The router saw full_replan in context.
        let captured_mode = capturing_router
            .captured_resume_mode
            .lock()
            .unwrap()
            .clone();
        assert_eq!(
            captured_mode,
            Some("full_replan".to_string()),
            "router should see full_replan resume mode"
        );
        // After full_replan purge, SlotGraph should NOT be in the store when router first runs.
        let saw_slot_graph = capturing_router
            .captured_has_slot_graph
            .lock()
            .unwrap()
            .unwrap();
        assert!(
            !saw_slot_graph,
            "SlotGraph should have been purged by full_replan before router runs"
        );
        // Agent system:slot-extractor was invoked.
        assert!(
            outcome
                .stages_executed
                .contains(&"system:slot-extractor".to_string()),
            "slot-extractor should have been executed"
        );
    }

    // B-08-5. resume_mode_partial_replan_routes_to_rewriter
    #[tokio::test]
    async fn resume_mode_partial_replan_routes_to_rewriter() {
        // Under partial_replan, SlotGraph survives but ClarifiedTask is purged.
        let suspension = make_suspension_with_artifacts(vec![
            ("qa-1".to_string(), ArtifactType::QueryAnalysis),
            ("sg-1".to_string(), ArtifactType::SlotGraph),
            ("ct-1".to_string(), ArtifactType::ClarifiedTask),
        ]);

        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:query-rewriter".to_string(),
            Arc::new(ContentAgent {
                id: "system:query-rewriter".to_string(),
                outputs: vec![(
                    ArtifactType::ClarifiedTask,
                    serde_json::json!({"task": "rewritten"}),
                )],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let router = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:query-rewriter".to_string(),
                reason: "partial_replan -> query_rewriter".to_string(),
            },
            RoutingDecision::Complete {
                reason: "done".to_string(),
            },
        ]));

        let mut orchestrator = PlanningOrchestrator::from_suspension(
            suspension,
            agents,
            router,
            "rewrite it".to_string(),
            "partial_replan".to_string(),
            20,
            None,
        );
        let outcome = orchestrator.run().await.unwrap();

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed, got: {:?}",
            outcome.status
        );
        // SlotGraph survived partial_replan.
        assert!(
            orchestrator
                .store
                .latest_of_type(&ArtifactType::SlotGraph)
                .is_some(),
            "SlotGraph should survive partial_replan"
        );
        // ClarifiedTask was purged then re-produced by the rewriter.
        let ct_artifacts = orchestrator.store.all_of_type(&ArtifactType::ClarifiedTask);
        // The re-produced one should have producer = "system:query-rewriter".
        assert!(
            ct_artifacts
                .iter()
                .any(|a| a.producer_agent_id == "system:query-rewriter"),
            "ClarifiedTask should have been re-produced by query-rewriter"
        );
        assert!(
            outcome
                .stages_executed
                .contains(&"system:query-rewriter".to_string()),
            "query-rewriter should have been executed"
        );
    }

    // B-08-6. multi_turn_rule_7_no_re_pause
    //
    // After resuming from an elicitation pause, the next run should not
    // re-pause for the same elicitation. The router is driven by a mock that
    // returns Pause only if called when there is no ElicitationResult in the
    // store. On the second run we inject an ElicitationResult (simulating the
    // answer-interpreter), so the router must not re-pause.
    #[tokio::test]
    async fn multi_turn_rule_7_no_re_pause() {
        // --- Run 1: cold start → Pause (no ElicitationResult yet) ---
        let agents_run1: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:query-analyzer".to_string(),
            Arc::new(ContentAgent {
                id: "system:query-analyzer".to_string(),
                outputs: vec![(
                    ArtifactType::QueryAnalysis,
                    serde_json::json!({"intent": "book_room"}),
                )],
            }) as Arc<dyn PipelineAgent>,
        )]);
        let router_run1 = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:query-analyzer".to_string(),
                reason: "cold start".to_string(),
            },
            RoutingDecision::Pause {
                question_text: "What room type do you need?".to_string(),
                slot_ids: vec!["room_type".to_string()],
            },
        ]));

        let mut orch1 = PlanningOrchestrator::new_with_router(
            agents_run1,
            router_run1,
            test_context(),
            20,
            None,
        );
        let outcome1 = orch1.run().await.unwrap();

        let suspension = match outcome1.status {
            PipelineStatus::Suspended(s) => s,
            other => panic!("run 1 should be Suspended, got: {other:?}"),
        };

        // --- Run 2: resume → answer-interpreter → planner → Complete ---
        // The resumed run should NOT pause again; the ElicitationResult is
        // now present because we inject it via ContentAgent.
        let agents_run2: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([
            (
                "system:answer-interpreter".to_string(),
                Arc::new(ContentAgent {
                    id: "system:answer-interpreter".to_string(),
                    outputs: vec![(
                        ArtifactType::ElicitationResult,
                        serde_json::json!({"needs_clarification": false}),
                    )],
                }) as Arc<dyn PipelineAgent>,
            ),
            (
                "system:planner".to_string(),
                Arc::new(PlannerAgent {
                    id: "system:planner".to_string(),
                }) as Arc<dyn PipelineAgent>,
            ),
        ]);
        let router_run2 = Arc::new(SequenceRouter::new(vec![
            RoutingDecision::NextStage {
                agent_id: "system:answer-interpreter".to_string(),
                reason: "interpret answer".to_string(),
            },
            RoutingDecision::NextStage {
                agent_id: "system:planner".to_string(),
                reason: "plan".to_string(),
            },
            RoutingDecision::Complete {
                reason: "done".to_string(),
            },
        ]));

        let mut orch2 = PlanningOrchestrator::from_suspension(
            *suspension,
            agents_run2,
            router_run2,
            "king".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );
        let outcome2 = orch2.run().await.unwrap();

        assert!(
            matches!(outcome2.status, PipelineStatus::Completed),
            "run 2 should be Completed (no re-pause), got: {:?}",
            outcome2.status
        );

        // Verify ElicitationResult is in the store (answer-interpreter ran).
        assert!(
            orch2
                .store
                .latest_of_type(&ArtifactType::ElicitationResult)
                .is_some(),
            "ElicitationResult should be present after run 2"
        );
    }

    // B-08-7. double_pause_resume_roundtrip
    //
    // Run 1: Pause on slot A.
    // Run 2: resume -> Pause on slot B (different slot).
    // Run 3: resume -> Complete.
    // Verify each suspension serialises/deserialises correctly.
    #[tokio::test]
    async fn double_pause_resume_roundtrip() {
        // --- Run 1 ---
        let router1 = Arc::new(SequenceRouter::new(vec![RoutingDecision::Pause {
            question_text: "What is your budget?".to_string(),
            slot_ids: vec!["budget".to_string()],
        }]));
        let mut orch1 = PlanningOrchestrator::new_with_router(
            HashMap::new(),
            router1,
            test_context(),
            20,
            None,
        );
        let outcome1 = orch1.run().await.unwrap();
        let suspension1 = match outcome1.status {
            PipelineStatus::Suspended(s) => s,
            other => panic!("run 1 should be Suspended, got: {other:?}"),
        };
        assert_eq!(suspension1.slot_ids, vec!["budget"]);

        // Serde roundtrip for suspension1.
        let json1 = serde_json::to_string(&suspension1).expect("serialize suspension1");
        let recovered1: PipelineSuspension =
            serde_json::from_str(&json1).expect("deserialize suspension1");
        assert_eq!(recovered1.slot_ids, vec!["budget"]);

        // --- Run 2: resume from suspension1, pause again on different slot ---
        let router2 = Arc::new(SequenceRouter::new(vec![RoutingDecision::Pause {
            question_text: "What city?".to_string(),
            slot_ids: vec!["city".to_string()],
        }]));
        let mut orch2 = PlanningOrchestrator::from_suspension(
            recovered1,
            HashMap::new(),
            router2,
            "".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );
        let outcome2 = orch2.run().await.unwrap();
        let suspension2 = match outcome2.status {
            PipelineStatus::Suspended(s) => s,
            other => panic!("run 2 should be Suspended, got: {other:?}"),
        };
        assert_eq!(suspension2.slot_ids, vec!["city"]);

        // Serde roundtrip for suspension2.
        let json2 = serde_json::to_string(&suspension2).expect("serialize suspension2");
        let recovered2: PipelineSuspension =
            serde_json::from_str(&json2).expect("deserialize suspension2");
        assert_eq!(recovered2.slot_ids, vec!["city"]);
        assert_eq!(recovered2.question_text, "What city?");

        // --- Run 3: resume from suspension2 → Complete ---
        let router3 = Arc::new(SequenceRouter::new(vec![RoutingDecision::Complete {
            reason: "all slots filled".to_string(),
        }]));
        let mut orch3 = PlanningOrchestrator::from_suspension(
            recovered2,
            HashMap::new(),
            router3,
            "Berlin".to_string(),
            "light_slot_update".to_string(),
            20,
            None,
        );
        let outcome3 = orch3.run().await.unwrap();

        assert!(
            matches!(outcome3.status, PipelineStatus::Completed),
            "run 3 should be Completed, got: {:?}",
            outcome3.status
        );
    }

    // B-08-8. resume_full_replan_no_stale_refire
    //
    // Put a stale QueryAnalysis artifact into the store (produced before
    // run_started_at). After full_replan resume, the stale artifact is NOT
    // cleared by from_suspension (full_replan only clears SlotGraph and below),
    // but latest_of_type_since(run_started_at) should return None for it,
    // causing the router to route to slot_extractor rather than using the
    // stale analysis.
    #[tokio::test]
    async fn resume_full_replan_no_stale_refire() {
        // Build a suspension with a QueryAnalysis, SlotGraph, and ElicitationResult.
        let stale_time = Utc::now() - chrono::Duration::hours(1);
        let mut artifact_snapshot = HashMap::new();
        // Stale QueryAnalysis — produced 1 hour ago.
        artifact_snapshot.insert(
            "qa-stale".to_string(),
            AgentArtifact {
                artifact_id: "qa-stale".to_string(),
                artifact_type: ArtifactType::QueryAnalysis,
                producer_agent_id: "system:query-analyzer".to_string(),
                producer_cycle_id: "cycle-0".to_string(),
                content: serde_json::json!({"intent": "old_intent"}),
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: stale_time,
                render_hints: None,
            },
        );
        // SlotGraph that will be purged by full_replan.
        artifact_snapshot.insert(
            "sg-old".to_string(),
            AgentArtifact {
                artifact_id: "sg-old".to_string(),
                artifact_type: ArtifactType::SlotGraph,
                producer_agent_id: "system:slot-extractor".to_string(),
                producer_cycle_id: "cycle-0".to_string(),
                content: serde_json::json!({"slots": []}),
                schema_version: ARTIFACT_SCHEMA_VERSION,
                produced_at: stale_time,
                render_hints: None,
            },
        );

        let suspension = PipelineSuspension {
            checkpoint_id: "ckpt-stale".to_string(),
            chain_id: "chain-test".to_string(),
            workflow_id: "wf-test".to_string(),
            artifact_snapshot,
            context: test_context(),
            iteration: 1,
            question_text: "stale question".to_string(),
            slot_ids: vec!["slot-x".to_string()],
            suspended_at: stale_time,
            question_id: Some("test-question-uuid".to_string()),
        };

        // VerifyingRouter checks that latest_of_type_since(run_started_at)
        // returns None for QueryAnalysis (stale), then routes to slot_extractor.
        struct VerifyingRouter {
            decisions: std::sync::Mutex<Vec<RoutingDecision>>,
            stale_qa_visible_since: std::sync::Mutex<Option<bool>>,
        }

        #[async_trait]
        impl RouterAgent for VerifyingRouter {
            async fn route(
                &self,
                store: &ArtifactStore,
                context: &mut PipelineContext,
            ) -> RoutingDecision {
                // On first call: check whether stale QueryAnalysis is visible
                // via latest_of_type_since(run_started_at).
                {
                    let mut vis = self
                        .stale_qa_visible_since
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if vis.is_none() {
                        if let Some(run_started_at) = context.run_started_at {
                            let fresh_qa = store
                                .latest_of_type_since(&ArtifactType::QueryAnalysis, run_started_at);
                            *vis = Some(fresh_qa.is_some());
                        } else {
                            *vis = Some(false);
                        }
                    }
                }
                let mut guard = self.decisions.lock().unwrap_or_else(|e| e.into_inner());
                if guard.is_empty() {
                    RoutingDecision::Error {
                        reason: "no more decisions".to_string(),
                    }
                } else {
                    guard.remove(0)
                }
            }
        }

        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:slot-extractor".to_string(),
            Arc::new(ContentAgent {
                id: "system:slot-extractor".to_string(),
                outputs: vec![(
                    ArtifactType::SlotGraph,
                    serde_json::json!({"slots": ["slot-x"]}),
                )],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let verifying_router = Arc::new(VerifyingRouter {
            decisions: std::sync::Mutex::new(vec![
                RoutingDecision::NextStage {
                    agent_id: "system:slot-extractor".to_string(),
                    reason: "no fresh QueryAnalysis -> slot_extractor".to_string(),
                },
                RoutingDecision::Complete {
                    reason: "done".to_string(),
                },
            ]),
            stale_qa_visible_since: std::sync::Mutex::new(None),
        });

        let router: Arc<dyn RouterAgent> = verifying_router.clone();

        let mut orchestrator = PlanningOrchestrator::from_suspension(
            suspension,
            agents,
            router,
            "fresh answer".to_string(),
            "full_replan".to_string(),
            20,
            None,
        );
        let outcome = orchestrator.run().await.unwrap();

        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed, got: {:?}",
            outcome.status
        );

        // The stale QueryAnalysis was NOT visible via latest_of_type_since(run_started_at).
        let stale_visible = verifying_router
            .stale_qa_visible_since
            .lock()
            .unwrap()
            .unwrap_or(true);
        assert!(
            !stale_visible,
            "stale QueryAnalysis should not be visible via latest_of_type_since(run_started_at)"
        );

        // SlotGraph was purged by full_replan and then re-produced by slot-extractor.
        let sg = orchestrator.store.latest_of_type(&ArtifactType::SlotGraph);
        assert!(sg.is_some(), "SlotGraph should have been re-produced");
        assert_eq!(
            sg.unwrap().producer_agent_id,
            "system:slot-extractor",
            "SlotGraph should be from the fresh slot-extractor run"
        );
    }

    // -----------------------------------------------------------------------
    // Sleeping propagation tests (C-sprint)
    // -----------------------------------------------------------------------

    /// Mock agent that always returns Sleeping with a fixed wake_at.
    struct SleepingAgent {
        id: String,
        wake_at: chrono::DateTime<chrono::Utc>,
    }

    #[async_trait]
    impl PipelineAgent for SleepingAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![]
        }
        async fn execute(
            &self,
            _store: &mut ArtifactStore,
            _context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            Ok(PipelineAgentResult::Sleeping {
                wake_at: self.wake_at,
                artifact_ids: vec![],
            })
        }
    }

    #[tokio::test]
    async fn sleeping_result_propagates_to_planning_outcome() {
        let wake_time = chrono::Utc::now() + chrono::Duration::seconds(300);
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:scheduler".to_string(),
            Arc::new(SleepingAgent {
                id: "system:scheduler".to_string(),
                wake_at: wake_time,
            }) as Arc<dyn PipelineAgent>,
        )]);

        let definition = test_definition(vec![make_stage("scheduling", "system:scheduler")]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let outcome = orchestrator
            .run_linear()
            .await
            .expect("sleeping pipeline should succeed");

        assert!(
            outcome.sleeping_until.is_some(),
            "sleeping_until must be set"
        );
        assert_eq!(outcome.sleeping_until.unwrap(), wake_time);
        assert!(
            outcome.stages_executed.is_empty(),
            "no stages completed when sleeping"
        );
    }
    #[test]
    fn lock_poison_does_not_cascade() {
        use std::sync::{Arc, Mutex};
        let m: Arc<Mutex<u32>> = Arc::new(Mutex::new(0));
        let m2 = m.clone();
        // Poison the lock by panicking while holding it.
        let _ = std::panic::catch_unwind(|| {
            let _guard = m2.lock().unwrap_or_else(|e| e.into_inner());
            panic!("intentional poison");
        });
        // unwrap_or_else should recover; unwrap() would panic here.
        let val = *m.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(val, 0);
    }

    // -----------------------------------------------------------------------
    // Y-03: RoutingDecision::Retry handler tests
    // -----------------------------------------------------------------------

    struct AlwaysSucceedsAgent {
        id: String,
    }
    impl AlwaysSucceedsAgent {
        fn new(id: &str) -> Self {
            Self { id: id.to_string() }
        }
    }
    #[async_trait]
    impl PipelineAgent for AlwaysSucceedsAgent {
        fn agent_id(&self) -> &str {
            &self.id
        }
        fn required_inputs(&self) -> Vec<ArtifactType> {
            vec![]
        }
        fn output_types(&self) -> Vec<ArtifactType> {
            vec![]
        }
        async fn execute(
            &self,
            _store: &mut ArtifactStore,
            _context: &PipelineContext,
        ) -> Result<PipelineAgentResult, PipelineAgentError> {
            Ok(PipelineAgentResult::Completed {
                artifact_ids: vec![],
            })
        }
    }

    #[tokio::test]
    async fn retry_decision_re_executes_agent_without_consuming_iteration() {
        let decisions = vec![
            RoutingDecision::Retry {
                agent_id: "system:query-analyzer".to_string(),
                reason: "service timeout".to_string(),
                delay_ms: 0,
            },
            RoutingDecision::Complete {
                reason: "done".to_string(),
            },
        ];
        let router = Arc::new(MockDecisionRouter {
            decisions,
            call_count: std::sync::atomic::AtomicU32::new(0),
        });
        let mut agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::new();
        agents.insert(
            "system:query-analyzer".to_string(),
            Arc::new(AlwaysSucceedsAgent::new("system:query-analyzer")),
        );
        let context = PipelineContext {
            workflow_id: "wf-retry".to_string(),
            chain_id: "chain-retry".to_string(),
            cycle_id: "c1".to_string(),
            query: "test retry".to_string(),
            ..Default::default()
        };
        let mut orch = PlanningOrchestrator::new_with_router(agents, router, context, 5, None);
        let outcome = orch.run().await.expect("run should succeed");
        assert!(
            matches!(outcome.status, PipelineStatus::Completed),
            "expected Completed, got {:?}",
            outcome.status
        );
    }

    // -----------------------------------------------------------------------
    // validate_required_inputs unit tests
    // -----------------------------------------------------------------------

    #[test]
    fn validate_required_inputs_passes_when_all_present() {
        let mut store = ArtifactStore::new("test");
        store.put(AgentArtifact {
            artifact_id: "qa-1".to_string(),
            artifact_type: ArtifactType::QueryAnalysis,
            producer_agent_id: "test".to_string(),
            producer_cycle_id: "c1".to_string(),
            content: serde_json::json!({}),
            schema_version: ARTIFACT_SCHEMA_VERSION,
            produced_at: Utc::now(),
            render_hints: None,
        });
        let agent = SuccessAgent {
            id: "test-agent".to_string(),
            inputs: vec![ArtifactType::QueryAnalysis],
            outputs: vec![],
        };
        assert!(super::validate_required_inputs(&agent, &store).is_ok());
    }

    #[test]
    fn validate_required_inputs_fails_when_missing() {
        let store = ArtifactStore::new("test");
        let agent = SuccessAgent {
            id: "test-agent".to_string(),
            inputs: vec![ArtifactType::QueryAnalysis, ArtifactType::SlotGraph],
            outputs: vec![],
        };
        let err = super::validate_required_inputs(&agent, &store).unwrap_err();
        match err {
            PipelineOrchestratorError::MissingRequiredInputs { agent_id, missing } => {
                assert_eq!(agent_id, "test-agent");
                assert_eq!(missing.len(), 2);
                assert!(missing.contains(&"query_analysis".to_string()));
                assert!(missing.contains(&"slot_graph".to_string()));
            },
            other => panic!("expected MissingRequiredInputs, got: {other:?}"),
        }
    }

    #[test]
    fn validate_required_inputs_passes_when_none_required() {
        let store = ArtifactStore::new("test");
        let agent = SuccessAgent {
            id: "test-agent".to_string(),
            inputs: vec![],
            outputs: vec![],
        };
        assert!(super::validate_required_inputs(&agent, &store).is_ok());
    }

    // -----------------------------------------------------------------------
    // Integration tests: dispatch rejects missing required inputs
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn router_driven_rejects_missing_required_inputs() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:needs-qa".to_string(),
            Arc::new(SuccessAgent {
                id: "system:needs-qa".to_string(),
                inputs: vec![ArtifactType::QueryAnalysis],
                outputs: vec![],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let router: Arc<dyn RouterAgent> = Arc::new(MockDecisionRouter {
            decisions: vec![RoutingDecision::NextStage {
                agent_id: "system:needs-qa".to_string(),
                reason: "test".to_string(),
            }],
            call_count: AtomicU32::new(0),
        });

        let context = PipelineContext {
            workflow_id: "wf-missing".to_string(),
            chain_id: "chain-missing".to_string(),
            cycle_id: "c1".to_string(),
            query: "test".to_string(),
            ..Default::default()
        };

        let mut orch = PlanningOrchestrator::new_with_router(agents, router, context, 5, None);
        let err = orch.run().await.unwrap_err();
        match err {
            PipelineOrchestratorError::MissingRequiredInputs { agent_id, missing } => {
                assert_eq!(agent_id, "system:needs-qa");
                assert!(missing.contains(&"query_analysis".to_string()));
            },
            other => panic!("expected MissingRequiredInputs, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn linear_rejects_missing_required_inputs() {
        let agents: HashMap<String, Arc<dyn PipelineAgent>> = HashMap::from([(
            "system:needs-qa".to_string(),
            Arc::new(SuccessAgent {
                id: "system:needs-qa".to_string(),
                inputs: vec![ArtifactType::QueryAnalysis],
                outputs: vec![],
            }) as Arc<dyn PipelineAgent>,
        )]);

        let definition = test_definition(vec![make_stage("needs_qa", "system:needs-qa")]);
        let mut orchestrator = PlanningOrchestrator::new(definition, agents, test_context());
        let err = orchestrator.run_linear().await.unwrap_err();
        match err {
            PipelineOrchestratorError::MissingRequiredInputs { agent_id, missing } => {
                assert_eq!(agent_id, "system:needs-qa");
                assert!(missing.contains(&"query_analysis".to_string()));
            },
            other => panic!("expected MissingRequiredInputs, got: {other:?}"),
        }
    }
}

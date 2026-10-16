mod decision;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::warn;

use crate::magician_v2::{
    agents::{resolve_focus_area_for_goal_id, AgentDefinitionStore, AgentMemoryService},
    analytics::operation_llm_telemetry::{
        OperationLlmCallAttribution, OperationLlmTelemetryContext,
    },
    artifact_v2::{memory::V3EpisodeRecord, workspace::ArtifactV2Workspace},
    prompts::{constants, PromptManager},
    query_analysis::operation_llm_router::{LLMOperation, OperationLlmRouter},
    realtime_events::RuntimeTransportBroadcaster,
};

use super::{
    log_capability_route_error, log_eval_route_error, log_harness_profile_route_error,
    log_memory_route_error, log_program_state_route_error, CreateLearningCandidateRequest,
    CreateLearningEventRequest, LearningCandidateState, LearningCandidateType,
    LearningCapabilityEvolutionBridge, LearningEvalBridge, LearningEventRef, LearningEvidenceRef,
    LearningHarnessProfileBridge, LearningMemoryBridge, LearningProcedureBridge,
    LearningProcedureFeedbackBridge, LearningProcedureFilters,
    LearningProcedureRunFeedbackJudgement, LearningProcedureRunFeedbackVerdict,
    LearningProcedureUsageContext, LearningProgramStateBridge, LearningRiskLevel, LearningScope,
    LearningStore,
};

const LEARNING_REFLECTION_OPERATION: &str = "learning_reflection";
const MAX_RELATED_EPISODES: usize = 6;
const MAX_EXISTING_CANDIDATES: usize = 25;
const MAX_EXISTING_PROCEDURES: usize = 25;
const MAX_OUTPUT_EXCERPT_CHARS: usize = 8_000;
const MIN_REUSABLE_WORKFLOW_EPISODES: usize = 2;
const MAX_WORKFLOW_ACTIONS: usize = 16;
const MAX_WORKFLOW_GROUPS: usize = 5;
const MAX_REFLECTION_OBJECT_FIELDS: usize = 32;
const MAX_REFLECTION_EPISODE_JSON_CHARS: usize = 10_000;
const MAX_REFLECTION_RELATED_JSON_CHARS: usize = 10_000;
const MAX_REFLECTION_CANDIDATES_JSON_CHARS: usize = 7_000;
const MAX_REFLECTION_PROCEDURES_JSON_CHARS: usize = 7_000;
const MAX_REFLECTION_EXTRA_CONTEXT_JSON_CHARS: usize = 12_000;
const MAX_REFLECTION_REQUEST_CHARS: usize = 96_000;

#[derive(Clone)]
pub struct LearningReflectionRuntime {
    workspace_layout: ArtifactV2Workspace,
    operation_llm_router: Option<Arc<OperationLlmRouter>>,
    prompt_manager: Arc<PromptManager>,
    event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
}

#[derive(Debug, Clone)]
pub struct LearningReflectionInput {
    pub boundary: String,
    pub episode: V3EpisodeRecord,
    pub extra_context: Value,
}

#[derive(Debug, Clone, Serialize)]
pub struct LearningReflectionRun {
    pub episode_id: String,
    pub boundary: String,
    pub event_id: Option<String>,
    pub candidate_ids: Vec<String>,
    pub no_op_reason: Option<String>,
    pub skipped_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ReflectionOutput {
    #[serde(default)]
    no_op_reason: Option<String>,
    #[serde(default)]
    procedure_feedback: Vec<ReflectedProcedureFeedback>,
    #[serde(default)]
    candidates: Vec<ReflectedCandidate>,
}

#[derive(Debug, Deserialize)]
struct ReflectedProcedureFeedback {
    procedure_id: String,
    verdict: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    confidence: Option<f64>,
    #[serde(default)]
    deprecation_recommended: bool,
}

#[derive(Debug, Deserialize)]
struct ReflectedCandidate {
    candidate_type: String,
    title: String,
    summary: String,
    #[serde(default)]
    rationale: String,
    #[serde(default)]
    proposed_change: Value,
    #[serde(default)]
    proposed_target: Option<String>,
    #[serde(default)]
    confidence: Option<f64>,
    risk_level: String,
    #[serde(default)]
    review_required: bool,
    #[serde(default)]
    review_reason: Option<String>,
    #[serde(default)]
    promotion_target: Option<String>,
    #[serde(default)]
    evidence: Vec<String>,
}

/// Harness program-state provenance for the agent that produced an episode.
///
/// Phase 3: only harness-enabled agents own program state, and any
/// program-state candidate must target the agent's real focus-area program
/// document rather than a hardcoded default.
enum HarnessProgramProvenance {
    /// The producing agent has no harness config — worker (non-harness) agents
    /// must not propose harness program-state.
    NotHarnessEnabled,
    /// The producing agent is harness-enabled. `program_relative_path` carries
    /// the resolved focus-area program document path when one is configured.
    HarnessEnabled {
        program_relative_path: Option<String>,
    },
}

impl LearningReflectionRuntime {
    pub fn new(
        workspace_layout: ArtifactV2Workspace,
        operation_llm_router: Option<Arc<OperationLlmRouter>>,
        prompt_manager: Arc<PromptManager>,
    ) -> Self {
        Self {
            workspace_layout,
            operation_llm_router,
            prompt_manager,
            event_broadcaster: None,
        }
    }

    pub fn with_event_broadcaster(
        mut self,
        event_broadcaster: Option<Arc<RuntimeTransportBroadcaster>>,
    ) -> Self {
        self.event_broadcaster = event_broadcaster;
        self
    }

    pub async fn reflect_episode(
        &self,
        input: LearningReflectionInput,
    ) -> Result<LearningReflectionRun> {
        let scope = scope_from_episode(&input.episode)?;
        let store = LearningStore::new(self.workspace_layout.clone());
        let episode_id = input.episode.episode_id.clone();
        let boundary = input.boundary.clone();
        if store.has_completed_reflection_for_episode(&scope, &episode_id, &boundary)? {
            return Ok(LearningReflectionRun {
                episode_id,
                boundary,
                event_id: None,
                candidate_ids: Vec::new(),
                no_op_reason: None,
                skipped_reason: Some("reflection_already_completed_for_episode".to_string()),
            });
        }

        let Some(router) = self.operation_llm_router.as_ref() else {
            let event = store.append_event(
                scope,
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_reflection_skipped".to_string(),
                    agent_id: Some(input.episode.agent_id.clone()),
                    task_id: input.episode.task_id.clone(),
                    execution_id: input.episode.execution_id.clone(),
                    chat_session_id: chat_session_id(&input.episode),
                    summary: "Learning reflection skipped because no LLM router is configured."
                        .to_string(),
                    evidence_refs: base_evidence_refs(&input.episode),
                    payload: json!({
                        "boundary": boundary.clone(),
                        "episode_id": episode_id.clone(),
                        "reason": "operation_llm_router_not_configured"
                    }),
                },
            )?;
            return Ok(LearningReflectionRun {
                episode_id,
                boundary,
                event_id: Some(event.id),
                candidate_ids: Vec::new(),
                no_op_reason: None,
                skipped_reason: Some("operation_llm_router_not_configured".to_string()),
            });
        };

        let related_episodes = self.related_episodes(&input.episode).await;
        let workflow_signal_context =
            build_workflow_signal_context(&input.episode, &related_episodes);
        let related_episode_summaries = related_episodes
            .iter()
            .map(episode_summary_value)
            .collect::<Vec<_>>();
        let existing_candidates = self.existing_candidate_summaries(&store, &scope)?;
        let existing_procedures = self.existing_procedure_summaries(&store, &scope)?;
        let procedure_feedback_bridge =
            LearningProcedureFeedbackBridge::new(self.workspace_layout.clone());
        let procedure_usage_context = match procedure_feedback_bridge
            .collect_usage_context_for_episode(&store, &scope, &input.episode)
        {
            Ok(context) => context,
            Err(error) => {
                warn!(
                    boundary = %boundary,
                    episode_id = %episode_id,
                    error = %error,
                    "Learning reflection could not collect procedure usage context"
                );
                LearningProcedureUsageContext::default()
            },
        };
        let extra_context = extra_context_with_learning_signals(
            &input.extra_context,
            workflow_signal_context,
            &procedure_usage_context,
        );
        let system_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::LEARNING_REFLECTION_SYSTEM,
                constants::versions::LEARNING_REFLECTION_SYSTEM,
                HashMap::new(),
            )
            .await?;
        let mut variables = HashMap::new();
        variables.insert("boundary".to_string(), boundary.clone());
        variables.insert(
            "episode_json".to_string(),
            // Reflection needs the durable outcome, actions, and memory
            // candidates—not the complete episode transport/provenance tree.
            // Reuse the curated episode packet so background learning cannot
            // reload an execution-sized context after answer readiness.
            bounded_reflection_json(
                episode_summary_value(&input.episode),
                MAX_REFLECTION_EPISODE_JSON_CHARS,
            )?,
        );
        variables.insert(
            "related_episodes_json".to_string(),
            bounded_reflection_json(
                serde_json::to_value(&related_episode_summaries)?,
                MAX_REFLECTION_RELATED_JSON_CHARS,
            )?,
        );
        variables.insert(
            "existing_candidates_json".to_string(),
            bounded_reflection_json(
                serde_json::to_value(&existing_candidates)?,
                MAX_REFLECTION_CANDIDATES_JSON_CHARS,
            )?,
        );
        variables.insert(
            "existing_procedures_json".to_string(),
            bounded_reflection_json(
                serde_json::to_value(&existing_procedures)?,
                MAX_REFLECTION_PROCEDURES_JSON_CHARS,
            )?,
        );
        variables.insert(
            "extra_context_json".to_string(),
            bounded_reflection_json(
                compact_value(&extra_context, 0),
                MAX_REFLECTION_EXTRA_CONTEXT_JSON_CHARS,
            )?,
        );
        let user_prompt = self
            .prompt_manager
            .get_rendered_prompt(
                constants::names::LEARNING_REFLECTION,
                constants::versions::LEARNING_REFLECTION,
                variables,
            )
            .await?;

        let tool_schema = reflection_tool_schema();
        validate_reflection_request_size(&system_prompt, &user_prompt, &tool_schema)?;
        let llm_started = std::time::Instant::now();
        let llm_scope = magicllm::LlmScope::new(scope.principal.clone(), scope.workspace.clone());
        let task_ref = input.episode.task_id.as_ref().map(|task_id| {
            let mut task_ref = magicllm::dispatch::TaskRef::task(task_id.clone())
                .with_agent(input.episode.agent_id.clone())
                .with_scope(llm_scope.principal.clone(), llm_scope.workspace.clone());
            if let Some(chat_session_id) = chat_session_id(&input.episode) {
                task_ref = task_ref.with_chat_session(chat_session_id);
            }
            if let Some(execution_id) = input.episode.execution_id.as_ref() {
                let root_execution_id = input
                    .episode
                    .root_execution_id
                    .clone()
                    .unwrap_or_else(|| execution_id.clone());
                task_ref = task_ref.with_execution(root_execution_id, execution_id.clone());
            }
            task_ref
        });
        let scoped_router = router
            .with_scope_context(Some(llm_scope))
            .with_task_context(task_ref);
        let reviewed = decision::review(
            self,
            &scoped_router,
            &system_prompt,
            &user_prompt,
            &tool_schema,
            &procedure_usage_context,
            &input.episode,
            &store,
            &scope,
            &related_episodes,
            &existing_candidates,
            &existing_procedures,
        )
        .await
        .context("learning reflection LLM call failed")?;
        let decision_guard = reviewed.guard.clone();
        let decision_origins = reviewed.origins;
        let response = reviewed.response;
        let parsed = reviewed.value;
        if let Some(broadcaster) = self.event_broadcaster.as_ref() {
            let telemetry = OperationLlmTelemetryContext::new(
                Arc::clone(broadcaster),
                scope.principal.clone(),
                scope.workspace.clone(),
                "learning_reflection",
            );
            let attribution = OperationLlmCallAttribution {
                execution_id: input.episode.execution_id.clone(),
                root_execution_id: input
                    .episode
                    .root_execution_id
                    .clone()
                    .or_else(|| input.episode.execution_id.clone()),
                task_id: input.episode.task_id.clone(),
                agent_id: Some(input.episode.agent_id.clone()),
                chat_session_id: chat_session_id(&input.episode),
                ..OperationLlmCallAttribution::default()
            };
            let latency_ms = llm_started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            match parsed.as_ref() {
                Ok(_) => telemetry.emit_validated_success(
                    LEARNING_REFLECTION_OPERATION,
                    &response,
                    latency_ms,
                    attribution,
                    "learning_reflection_json",
                ),
                Err(error) => telemetry.emit_validation_failure(
                    LEARNING_REFLECTION_OPERATION,
                    &response,
                    latency_ms,
                    attribution,
                    "learning_reflection_json",
                    &error.to_string(),
                ),
            }
        }
        let reflection = parsed.map_err(|error| {
            let _ = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_reflection_failed".to_string(),
                    agent_id: Some(input.episode.agent_id.clone()),
                    task_id: input.episode.task_id.clone(),
                    execution_id: input.episode.execution_id.clone(),
                    chat_session_id: chat_session_id(&input.episode),
                    summary: format!("Learning reflection output was not valid JSON: {error}"),
                    evidence_refs: base_evidence_refs(&input.episode),
                    payload: json!({
                        "boundary": boundary.clone(),
                        "episode_id": episode_id.clone(),
                        "error": error.to_string(),
                        "raw_excerpt": truncate_chars(response.content.as_str(), MAX_OUTPUT_EXCERPT_CHARS),
                    }),
                },
            );
            error
        })?;

        let no_op_reason = reflection.no_op_reason.clone();
        anyhow::ensure!(
            match &decision_guard {
                Some(g) => g.revalidate().await,
                None => true,
            },
            "procedure decision policy changed before application"
        );
        let procedure_feedback_judgements = reflection
            .procedure_feedback
            .iter()
            .filter_map(reflected_procedure_feedback_to_judgement)
            .collect::<Vec<_>>();
        let feedback_deprecation_targets =
            procedure_feedback_deprecation_targets(&procedure_feedback_judgements);
        let completed_event = store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_reflection_completed".to_string(),
                agent_id: Some(input.episode.agent_id.clone()),
                task_id: input.episode.task_id.clone(),
                execution_id: input.episode.execution_id.clone(),
                chat_session_id: chat_session_id(&input.episode),
                summary: format!(
                    "Learning reflection completed with {} candidate(s).",
                    reflection.candidates.len()
                ),
                evidence_refs: base_evidence_refs(&input.episode),
                payload: json!({
                    "boundary": boundary.clone(),
                    "episode_id": episode_id.clone(),
                    "candidate_count": reflection.candidates.len(),
                    "decision_origins": decision_origins,
                    "no_op_reason": no_op_reason.clone(),
                    "procedure_feedback": {
                        "used_procedure_count": procedure_usage_context.used_procedures.len(),
                        "judgement_count": procedure_feedback_judgements.len(),
                        "matched_event_ids": &procedure_usage_context.matched_event_ids,
                    },
                }),
            },
        )?;
        let event_ref = LearningEventRef {
            event_id: completed_event.id.clone(),
            event_type: completed_event.event_type.clone(),
        };

        // Phase 3: resolve harness program-state provenance once for this
        // agent/goal. Worker (non-harness) agents do not own harness program
        // state, and harness candidates must target the real focus-area program
        // document path, not a hardcoded default.
        let harness_program_provenance = self
            .resolve_harness_program_provenance(&scope, &input.episode)
            .await;

        let mut candidate_ids = Vec::new();
        for mut reflected in reflection.candidates {
            anyhow::ensure!(
                match &decision_guard {
                    Some(g) => g.revalidate().await,
                    None => true,
                },
                "procedure decision policy changed before candidate creation"
            );
            if reflected_candidate_duplicates_feedback_deprecation(
                &reflected,
                &feedback_deprecation_targets,
            ) {
                let _ = store.append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type:
                            "learning_procedure_duplicate_deprecation_candidate_skipped"
                                .to_string(),
                        agent_id: Some(input.episode.agent_id.clone()),
                        task_id: input.episode.task_id.clone(),
                        execution_id: input.episode.execution_id.clone(),
                        chat_session_id: chat_session_id(&input.episode),
                        summary: "Skipped model-emitted procedure deprecation candidate because top-level procedure feedback already requested reviewed deprecation."
                            .to_string(),
                        evidence_refs: base_evidence_refs(&input.episode),
                        payload: json!({
                            "boundary": boundary.clone(),
                            "episode_id": episode_id.clone(),
                            "candidate_title": reflected.title,
                            "candidate_type": reflected.candidate_type,
                            "feedback_deprecation_targets": &feedback_deprecation_targets,
                        }),
                    },
                );
                continue;
            }
            // Phase 3: gate + correct provenance for program-state candidates.
            if parse_candidate_type(&reflected.candidate_type)
                == LearningCandidateType::ProgramStateUpdate
            {
                match &harness_program_provenance {
                    HarnessProgramProvenance::NotHarnessEnabled => {
                        let _ = store.append_event(
                            scope.clone(),
                            CreateLearningEventRequest {
                                principal: None,
                                workspace: None,
                                event_type:
                                    "learning_program_state_candidate_skipped_non_harness"
                                        .to_string(),
                                agent_id: Some(input.episode.agent_id.clone()),
                                task_id: input.episode.task_id.clone(),
                                execution_id: input.episode.execution_id.clone(),
                                chat_session_id: chat_session_id(&input.episode),
                                summary: format!(
                                    "Skipped program-state candidate from non-harness agent `{}`; only harness-enabled agents own program state.",
                                    input.episode.agent_id
                                ),
                                evidence_refs: base_evidence_refs(&input.episode),
                                payload: json!({
                                    "boundary": boundary.clone(),
                                    "episode_id": episode_id.clone(),
                                    "candidate_title": reflected.title,
                                    "agent_id": input.episode.agent_id,
                                }),
                            },
                        );
                        continue;
                    },
                    HarnessProgramProvenance::HarnessEnabled {
                        program_relative_path,
                    } => {
                        if let Some(path) = program_relative_path.as_deref() {
                            set_program_state_candidate_path(&mut reflected.proposed_change, path);
                        }
                    },
                }
            }
            let Some(request) =
                reflected_candidate_to_request(&input.episode, &event_ref, reflected)
            else {
                continue;
            };
            let candidate = store.create_candidate(scope.clone(), request)?;
            candidate_ids.push(candidate.id.clone());
            let _ = store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_candidate_created".to_string(),
                    agent_id: candidate.source_agent_id.clone(),
                    task_id: candidate.source_task_id.clone(),
                    execution_id: candidate.source_execution_id.clone(),
                    chat_session_id: candidate.source_chat_session_id.clone(),
                    summary: format!(
                        "Learning candidate created: {} ({})",
                        candidate.title,
                        candidate.candidate_type.as_str()
                    ),
                    evidence_refs: candidate.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": candidate.id,
                        "candidate_type": candidate.candidate_type.as_str(),
                        "source_reflection_event_id": completed_event.id.clone(),
                    }),
                },
            );
            if candidate.candidate_type.is_procedure_candidate() {
                let bridge = LearningProcedureBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate) {
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: "learning_procedure_candidate_route_failed".to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning procedure candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            } else if candidate.candidate_type.is_memory_candidate() {
                let bridge = LearningMemoryBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate).await {
                    log_memory_route_error(&candidate.id, &error);
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: "learning_memory_candidate_route_failed".to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning memory candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            }
            if candidate.candidate_type.is_evaluation_candidate() {
                let bridge = LearningEvalBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate) {
                    log_eval_route_error(&candidate.id, &error);
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: "learning_eval_candidate_route_failed".to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning evaluation candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            }
            if candidate
                .candidate_type
                .is_skill_or_capability_evolution_candidate()
            {
                let bridge = LearningCapabilityEvolutionBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate) {
                    log_capability_route_error(&candidate.id, &error);
                    let event_type = if candidate.candidate_type.is_skill_or_workflow_candidate() {
                        "learning_skill_candidate_route_failed"
                    } else {
                        "learning_capability_candidate_route_failed"
                    };
                    let route_label = if candidate.candidate_type.is_skill_or_workflow_candidate() {
                        "skill/workflow"
                    } else {
                        "capability-evolution"
                    };
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: event_type.to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning {route_label} candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            }
            if candidate.candidate_type.is_harness_profile_candidate() {
                // Boundary D. Staged only: this bridge has no auto-apply arm
                // at all, so a reflection cannot change live harness guidance
                // however low-risk it rates itself.
                let bridge = LearningHarnessProfileBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate) {
                    log_harness_profile_route_error(&candidate.id, &error);
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: "learning_harness_profile_candidate_route_failed"
                                .to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning harness-profile candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            }
            if candidate.candidate_type == LearningCandidateType::ProgramStateUpdate {
                let bridge = LearningProgramStateBridge::new(self.workspace_layout.clone());
                if let Err(error) = bridge.route_candidate(&store, &scope, &candidate).await {
                    log_program_state_route_error(&candidate.id, &error);
                    let _ = store.append_event(
                        scope.clone(),
                        CreateLearningEventRequest {
                            principal: None,
                            workspace: None,
                            event_type: "learning_program_state_candidate_route_failed".to_string(),
                            agent_id: candidate.source_agent_id.clone(),
                            task_id: candidate.source_task_id.clone(),
                            execution_id: candidate.source_execution_id.clone(),
                            chat_session_id: candidate.source_chat_session_id.clone(),
                            summary: format!(
                                "Learning program-state candidate `{}` could not be routed: {}",
                                candidate.id, error
                            ),
                            evidence_refs: candidate.evidence_refs.clone(),
                            payload: json!({
                                "candidate_id": candidate.id,
                                "candidate_type": candidate.candidate_type.as_str(),
                                "error": error.to_string(),
                            }),
                        },
                    );
                }
            }
        }

        anyhow::ensure!(
            match &decision_guard {
                Some(g) => g.revalidate().await,
                None => true,
            },
            "procedure decision policy changed before feedback update"
        );
        if let Err(error) = procedure_feedback_bridge.record_post_run_feedback(
            &store,
            &scope,
            &input.episode,
            &procedure_usage_context,
            &procedure_feedback_judgements,
            Some(&event_ref),
            no_op_reason.as_deref(),
            &input.extra_context,
        ) {
            warn!(
                boundary = %boundary,
                episode_id = %episode_id,
                error = %error,
                "Learning reflection could not record procedure feedback"
            );
        }

        Ok(LearningReflectionRun {
            episode_id,
            boundary,
            event_id: Some(completed_event.id),
            candidate_ids,
            no_op_reason,
            skipped_reason: None,
        })
    }

    /// Resolve whether the agent that produced this episode is harness-enabled
    /// and, if so, the focus-area program document path its program-state
    /// candidates should target. Failure to load the definition is treated as
    /// non-harness (fail closed: do not emit harness program-state).
    async fn resolve_harness_program_provenance(
        &self,
        scope: &LearningScope,
        episode: &V3EpisodeRecord,
    ) -> HarnessProgramProvenance {
        let store = AgentDefinitionStore::with_workspace_layout(self.workspace_layout.clone())
            .for_scope(&scope.principal, &scope.workspace);
        let record = match store.get_definition(&episode.agent_id).await {
            Ok(Some(record)) => record,
            Ok(None) => return HarnessProgramProvenance::NotHarnessEnabled,
            Err(error) => {
                warn!(
                    agent_id = %episode.agent_id,
                    error = %error,
                    "Learning reflection could not load agent definition for program-state provenance"
                );
                return HarnessProgramProvenance::NotHarnessEnabled;
            },
        };
        if record.definition.harness.is_none() {
            return HarnessProgramProvenance::NotHarnessEnabled;
        }
        let program_relative_path =
            resolve_focus_area_for_goal_id(&record.definition, &episode.goal_key)
                .and_then(|focus_area| focus_area.program.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        HarnessProgramProvenance::HarnessEnabled {
            program_relative_path,
        }
    }

    async fn related_episodes(&self, episode: &V3EpisodeRecord) -> Vec<V3EpisodeRecord> {
        let Some(scope) = episode_scope_tuple(episode) else {
            return Vec::new();
        };
        let memory_service = AgentMemoryService::with_scoped_memory_scope(
            self.workspace_layout.memory_root(scope.0, scope.1),
            scope.0,
            scope.1,
        );
        let mut related = memory_service
            .load_native_episodes_for_goal(&episode.agent_id, &episode.goal_key)
            .await
            .unwrap_or_default();
        if related.len() <= 1 {
            related = memory_service
                .load_native_episodes(&episode.agent_id)
                .await
                .unwrap_or_default();
        }
        related.retain(|candidate| candidate.episode_id != episode.episode_id);
        related.sort_by(|left, right| right.completed_at.cmp(&left.completed_at));
        related.into_iter().take(MAX_RELATED_EPISODES).collect()
    }

    fn existing_candidate_summaries(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
    ) -> Result<Vec<Value>> {
        let candidates = store.list_candidates(
            scope,
            super::LearningCandidateFilters {
                state: None,
                candidate_type: None,
                source_agent_id: None,
                limit: Some(MAX_EXISTING_CANDIDATES),
            },
        )?;
        Ok(candidates
            .into_iter()
            .filter(|candidate| !candidate.state.is_terminal())
            .map(|candidate| {
                json!({
                    "id": candidate.id,
                    "candidate_type": candidate.candidate_type.as_str(),
                    "state": candidate.state.as_str(),
                    "title": candidate.title,
                    "summary": truncate_chars(&candidate.summary, 800),
                    "source_agent_id": candidate.source_agent_id,
                    "source_task_id": candidate.source_task_id,
                    "source_execution_id": candidate.source_execution_id,
                    "source_chat_session_id": candidate.source_chat_session_id,
                })
            })
            .collect())
    }

    fn existing_procedure_summaries(
        &self,
        store: &LearningStore,
        scope: &LearningScope,
    ) -> Result<Vec<Value>> {
        let procedures = store.list_procedures(
            scope,
            LearningProcedureFilters {
                status: None,
                owner_agent: None,
                limit: None,
            },
        )?;
        Ok(procedures
            .into_iter()
            .filter(|procedure| {
                matches!(
                    procedure.status,
                    super::LearningProcedureStatus::Draft | super::LearningProcedureStatus::Active
                )
            })
            .map(|procedure| {
                json!({
                    "id": procedure.id,
                    "status": procedure.status.as_str(),
                    "title": procedure.title,
                    "summary": truncate_chars(&procedure.summary, 800),
                    "owner_agent": procedure.owner_agent,
                    "activation": procedure.activation,
                    "workflow": procedure.workflow.into_iter().take(8).collect::<Vec<_>>(),
                    "workflow_signature": procedure.payload.get("workflow_signature").cloned(),
                    "source_candidate_id": procedure.source_candidate_id,
                    "source_task_ids": procedure.source_task_ids,
                    "source_chat_session_ids": procedure.source_chat_session_ids,
                    "success_count": procedure.success_count,
                    "failure_count": procedure.failure_count,
                })
            })
            .take(MAX_EXISTING_PROCEDURES)
            .collect())
    }
}

pub fn spawn_learning_reflection(
    runtime: LearningReflectionRuntime,
    input: LearningReflectionInput,
) {
    tokio::spawn(async move {
        let boundary = input.boundary.clone();
        let episode_id = input.episode.episode_id.clone();
        if let Err(error) = runtime.reflect_episode(input).await {
            warn!(
                boundary = %boundary,
                episode_id = %episode_id,
                error = %error,
                "Learning reflection failed"
            );
        }
    });
}

fn reflected_procedure_feedback_to_judgement(
    feedback: &ReflectedProcedureFeedback,
) -> Option<LearningProcedureRunFeedbackJudgement> {
    let procedure_id = feedback.procedure_id.trim();
    if procedure_id.is_empty() {
        return None;
    }
    Some(LearningProcedureRunFeedbackJudgement {
        procedure_id: procedure_id.to_string(),
        verdict: LearningProcedureRunFeedbackVerdict::parse(&feedback.verdict),
        rationale: truncate_chars(feedback.rationale.trim(), 1_000),
        confidence: feedback
            .confidence
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0)),
        deprecation_recommended: feedback.deprecation_recommended,
    })
}

fn procedure_feedback_deprecation_targets(
    judgements: &[LearningProcedureRunFeedbackJudgement],
) -> HashSet<String> {
    judgements
        .iter()
        .filter(|judgement| judgement.deprecation_recommended)
        .map(|judgement| normalize_procedure_reference(&judgement.procedure_id))
        .filter(|procedure_id| !procedure_id.is_empty())
        .collect()
}

fn reflected_candidate_duplicates_feedback_deprecation(
    reflected: &ReflectedCandidate,
    feedback_deprecation_targets: &HashSet<String>,
) -> bool {
    if feedback_deprecation_targets.is_empty()
        || parse_candidate_type(&reflected.candidate_type) != LearningCandidateType::MemoryProcedure
    {
        return false;
    }
    let Some(payload) = reflected_procedure_payload(&reflected.proposed_change) else {
        return false;
    };
    if !payload_requests_procedure_deprecation(payload) {
        return false;
    }
    reflected_procedure_target_ids(payload, reflected.proposed_target.as_deref())
        .into_iter()
        .any(|target| feedback_deprecation_targets.contains(&target))
}

fn reflected_procedure_payload(value: &Value) -> Option<&Value> {
    value
        .get("procedure")
        .or_else(|| {
            value
                .get("memory")
                .and_then(|memory| memory.get("procedure"))
        })
        .or_else(|| value.get("workflow_template"))
        .or_else(|| value.as_object().map(|_| value))
}

fn payload_requests_procedure_deprecation(payload: &Value) -> bool {
    for key in [
        "deprecation_recommended",
        "deprecate",
        "deprecated",
        "should_deprecate",
        "retire",
    ] {
        if payload.get(key).and_then(Value::as_bool).unwrap_or(false) {
            return true;
        }
    }
    for key in [
        "status",
        "action",
        "recommendation",
        "deprecation_reason",
        "retirement_reason",
    ] {
        if payload
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(procedure_deprecation_text)
        {
            return true;
        }
    }
    false
}

fn procedure_deprecation_text(value: &str) -> bool {
    let value = normalize_token(value);
    value.contains("deprecat") || value.contains("retire") || value.contains("stop_inject")
}

fn reflected_procedure_target_ids(payload: &Value, proposed_target: Option<&str>) -> Vec<String> {
    let mut ids = Vec::new();
    for key in [
        "existing_procedure_id",
        "procedure_id",
        "id",
        "target_procedure_id",
    ] {
        if let Some(value) = payload.get(key).and_then(Value::as_str) {
            push_normalized_procedure_reference(&mut ids, value);
        }
    }
    if let Some(value) = proposed_target {
        push_normalized_procedure_reference(&mut ids, value);
    }
    ids
}

fn push_normalized_procedure_reference(ids: &mut Vec<String>, value: &str) {
    let value = normalize_procedure_reference(value);
    if !value.is_empty() && !ids.iter().any(|existing| existing == &value) {
        ids.push(value);
    }
}

fn normalize_procedure_reference(value: &str) -> String {
    normalize_token(value).trim().to_string()
}

fn reflected_candidate_to_request(
    episode: &V3EpisodeRecord,
    reflection_event: &LearningEventRef,
    reflected: ReflectedCandidate,
) -> Option<CreateLearningCandidateRequest> {
    let title = reflected.title.trim();
    let summary = reflected.summary.trim();
    if title.is_empty() || summary.is_empty() {
        return None;
    }
    let candidate_type = parse_candidate_type(&reflected.candidate_type);
    let risk_level = parse_risk_level(&reflected.risk_level);
    let high_risk_review = matches!(
        &risk_level,
        LearningRiskLevel::High | LearningRiskLevel::Critical
    );
    let is_procedure_candidate = candidate_type.is_procedure_candidate();
    let review_required = reflected.review_required
        || high_risk_review
        || is_procedure_candidate
        || !matches!(
            &candidate_type,
            LearningCandidateType::MemoryFact | LearningCandidateType::MemoryPreference
        );
    let eligible_for_auto_promotion = matches!(
        &candidate_type,
        LearningCandidateType::MemoryFact | LearningCandidateType::MemoryPreference
    ) && !review_required
        && matches!(&risk_level, LearningRiskLevel::Low);
    let mut evidence_refs = base_evidence_refs(episode);
    evidence_refs.extend(reflected.evidence.into_iter().filter_map(|summary| {
        let summary = summary.trim();
        (!summary.is_empty()).then(|| LearningEvidenceRef {
            kind: "reflection_evidence".to_string(),
            id: None,
            path: None,
            uri: None,
            summary: Some(truncate_chars(summary, 1_000)),
        })
    }));

    Some(CreateLearningCandidateRequest {
        principal: None,
        workspace: None,
        candidate_type,
        state: LearningCandidateState::Proposed,
        title: truncate_chars(title, 220),
        summary: truncate_chars(summary, 2_000),
        rationale: truncate_chars(reflected.rationale.trim(), 2_000),
        proposed_change: if reflected.proposed_change.is_null() {
            json!({})
        } else {
            reflected.proposed_change
        },
        proposed_target: reflected.proposed_target,
        confidence: reflected
            .confidence
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0)),
        source_agent_id: Some(episode.agent_id.clone()),
        source_task_id: episode.task_id.clone(),
        source_execution_id: episode.execution_id.clone(),
        source_chat_session_id: chat_session_id(episode),
        event_refs: vec![reflection_event.clone()],
        evidence_refs,
        risk_level,
        review_required,
        review_reason: reflected.review_reason.or_else(|| {
            review_required.then(|| {
                if is_procedure_candidate {
                    "Learning reflection recorded reusable procedural knowledge as a draft procedure; review is required before activation."
                        .to_string()
                } else {
                    "Learning reflection recorded this candidate for review; it is not eligible for automatic Phase 3 memory promotion."
                        .to_string()
                }
            })
        }),
        review_policy: json!({
            "phase": if is_procedure_candidate { "phase_11_procedure_extraction" } else { "phase_3_memory_bridge" },
            "requires_review": review_required,
            "auto_apply": eligible_for_auto_promotion
        }),
        promotion_target: reflected.promotion_target,
        promotion_policy: json!({
            "phase": if is_procedure_candidate { "phase_11_procedure_extraction" } else { "phase_3_memory_bridge" },
            "eligible_for_auto_promotion": eligible_for_auto_promotion,
            "bridge": if is_procedure_candidate { "learning_procedure_bridge" } else { "learning_memory_bridge" },
            "reason": if is_procedure_candidate {
                "Reusable procedure candidates are recorded as draft procedures for review; activation remains explicit."
            } else {
                "Only low-risk explicit user memory requests/corrections can auto-promote; the memory bridge still validates scope, explicit flags, confidence, tier, and secret checks before writing."
            }
        }),
    })
}

fn parse_reflection_output(content: &str) -> Result<ReflectionOutput> {
    serde_json::from_str::<ReflectionOutput>(content)
        .or_else(|_| {
            extract_json_object(content)
                .ok_or_else(|| anyhow!("no JSON object found in reflection output"))
                .and_then(|json| {
                    serde_json::from_str::<ReflectionOutput>(json)
                        .context("failed to parse extracted reflection JSON")
                })
        })
        .map(|mut output| {
            output
                .candidates
                .retain(|candidate| !candidate.title.trim().is_empty());
            output
        })
}

fn extract_json_object(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    (end > start).then_some(&content[start..=end])
}

fn scope_from_episode(episode: &V3EpisodeRecord) -> Result<LearningScope> {
    let (principal, workspace) = episode_scope_tuple(episode)
        .ok_or_else(|| anyhow!("learning reflection requires scoped episode provenance"))?;
    Ok(LearningScope::new(principal, workspace))
}

fn episode_scope_tuple(episode: &V3EpisodeRecord) -> Option<(&str, &str)> {
    let principal = episode.principal.as_deref()?.trim();
    let workspace = episode.workspace.as_deref()?.trim();
    if principal.is_empty() || workspace.is_empty() {
        return None;
    }
    Some((principal, workspace))
}

fn chat_session_id(episode: &V3EpisodeRecord) -> Option<String> {
    episode
        .trigger_payload
        .as_ref()
        .and_then(|payload| payload.get("session_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn base_evidence_refs(episode: &V3EpisodeRecord) -> Vec<LearningEvidenceRef> {
    let mut refs = vec![LearningEvidenceRef {
        kind: "memory_episode".to_string(),
        id: Some(episode.episode_id.clone()),
        path: None,
        uri: None,
        summary: Some(truncate_chars(&episode.outcome_summary, 1_000)),
    }];
    if let Some(task_id) = &episode.task_id {
        refs.push(LearningEvidenceRef {
            kind: "task".to_string(),
            id: Some(task_id.clone()),
            path: episode
                .provenance
                .as_ref()
                .map(|provenance| provenance.task_manifest_relative_path.clone()),
            uri: None,
            summary: episode.task_title.clone(),
        });
    }
    if let Some(execution_id) = &episode.execution_id {
        refs.push(LearningEvidenceRef {
            kind: "execution".to_string(),
            id: Some(execution_id.clone()),
            path: episode
                .provenance
                .as_ref()
                .map(|provenance| provenance.execution_state_relative_path.clone()),
            uri: None,
            summary: episode.execution_status.clone(),
        });
    }
    if let Some(session_id) = chat_session_id(episode) {
        refs.push(LearningEvidenceRef {
            kind: "chat_session".to_string(),
            id: Some(session_id),
            path: None,
            uri: None,
            summary: episode.ui_thread_id.clone(),
        });
    }
    refs
}

fn episode_summary_value(episode: &V3EpisodeRecord) -> Value {
    json!({
        "episode_id": episode.episode_id,
        "agent_id": episode.agent_id,
        "goal_key": episode.goal_key,
        "trigger_type": episode.trigger_type,
        "completed_at": episode.completed_at,
        "outcome_kind": episode.outcome_kind,
        "outcome_summary": truncate_chars(&episode.outcome_summary, 1_000),
        "last_error": episode.last_error.as_ref().map(|value| truncate_chars(value, 800)),
        "task_title": episode.task_title,
        "actions_taken": episode.actions_taken.iter().take(10).map(|action| {
            json!({
                "action_type": action.action_type,
                "description": truncate_chars(&action.description, 300),
                "tool": action.tool,
                "succeeded": action.succeeded,
            })
        }).collect::<Vec<_>>(),
        "memory_candidates": episode.memory_candidates.iter().take(6).map(|candidate| {
            json!({
                "candidate_type": candidate.candidate_type,
                "target_hint": candidate.target_hint,
                "confidence": candidate.confidence,
                "rationale": truncate_chars(&candidate.rationale, 400),
                "value": compact_value(&candidate.value, 0),
            })
        }).collect::<Vec<_>>(),
    })
}

fn extra_context_with_learning_signals(
    extra_context: &Value,
    workflow_signals: Value,
    procedure_usage_context: &LearningProcedureUsageContext,
) -> Value {
    let mut context = match extra_context {
        Value::Object(map) => Value::Object(map.clone()),
        Value::Null => json!({}),
        other => json!({ "caller_context": compact_value(other, 0) }),
    };
    if let Value::Object(map) = &mut context {
        map.insert("workflow_detection".to_string(), workflow_signals);
        if !procedure_usage_context.is_empty() {
            map.insert(
                "procedure_feedback".to_string(),
                procedure_usage_context.as_prompt_json(),
            );
        }
    }
    context
}

fn build_workflow_signal_context(current: &V3EpisodeRecord, related: &[V3EpisodeRecord]) -> Value {
    let current_signature = workflow_signature_for_episode(current);
    let mut groups: HashMap<String, Vec<WorkflowSignatureEpisode>> = HashMap::new();
    for episode in std::iter::once(current).chain(related.iter()) {
        let Some(signature) = workflow_signature_for_episode(episode) else {
            continue;
        };
        groups
            .entry(signature.signature.clone())
            .or_default()
            .push(signature);
    }

    let mut repeated = groups
        .into_iter()
        .filter_map(|(signature, episodes)| {
            if episodes.len() < MIN_REUSABLE_WORKFLOW_EPISODES {
                return None;
            }
            let successful_count = episodes.iter().filter(|episode| episode.successful).count();
            if successful_count < MIN_REUSABLE_WORKFLOW_EPISODES {
                return None;
            }
            let includes_current = episodes
                .iter()
                .any(|episode| episode.episode_id == current.episode_id);
            Some(json!({
                "signature": signature,
                "episode_count": episodes.len(),
                "successful_count": successful_count,
                "includes_current_episode": includes_current,
                "tool_sequence": episodes.first().map(|episode| episode.tool_sequence.clone()).unwrap_or_default(),
                "action_sequence": episodes.first().map(|episode| episode.action_sequence.clone()).unwrap_or_default(),
                "episodes": episodes.iter().map(|episode| {
                    json!({
                        "episode_id": episode.episode_id,
                        "agent_id": episode.agent_id,
                        "goal_key": episode.goal_key,
                        "task_title": episode.task_title,
                        "outcome_kind": episode.outcome_kind,
                        "outcome_summary": truncate_chars(&episode.outcome_summary, 500),
                    })
                }).collect::<Vec<_>>(),
            }))
        })
        .collect::<Vec<_>>();
    repeated.sort_by(|left, right| {
        let left_count = left
            .get("successful_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let right_count = right
            .get("successful_count")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        right_count.cmp(&left_count)
    });
    repeated.truncate(MAX_WORKFLOW_GROUPS);

    json!({
        "strategy": "cheap_workflow_signature_plus_llm_gate",
        "threshold": {
            "minimum_reusable_workflow_episodes": MIN_REUSABLE_WORKFLOW_EPISODES,
            "max_actions_per_signature": MAX_WORKFLOW_ACTIONS
        },
        "current_episode_signature": current_signature.map(|signature| {
            json!({
                "signature": signature.signature,
                "successful": signature.successful,
                "tool_sequence": signature.tool_sequence,
                "action_sequence": signature.action_sequence,
            })
        }),
        "repeated_successful_workflows": repeated,
        "llm_gate": "Create memory_procedure candidates only when the grouped episodes reveal reusable procedural knowledge or the user explicitly asked to make the workflow reusable. Avoid duplicates when an existing procedure already covers the behavior."
    })
}

#[derive(Debug, Clone)]
struct WorkflowSignatureEpisode {
    episode_id: String,
    agent_id: String,
    goal_key: String,
    task_title: Option<String>,
    outcome_kind: String,
    outcome_summary: String,
    successful: bool,
    signature: String,
    tool_sequence: Vec<String>,
    action_sequence: Vec<String>,
}

fn workflow_signature_for_episode(episode: &V3EpisodeRecord) -> Option<WorkflowSignatureEpisode> {
    if episode.actions_taken.is_empty() {
        return None;
    }
    let mut tool_sequence = Vec::new();
    let mut action_sequence = Vec::new();
    for action in episode.actions_taken.iter().take(MAX_WORKFLOW_ACTIONS) {
        let tool = normalize_workflow_token(&action.tool);
        let action_type = normalize_workflow_token(&action.action_type);
        if !tool.is_empty() {
            tool_sequence.push(tool);
        }
        if !action_type.is_empty() {
            action_sequence.push(action_type);
        }
    }
    if tool_sequence.is_empty() && action_sequence.is_empty() {
        return None;
    }
    let signature = format!(
        "agent:{}|tools:{}|actions:{}",
        normalize_workflow_token(&episode.agent_id),
        tool_sequence.join(">"),
        action_sequence.join(">")
    );
    Some(WorkflowSignatureEpisode {
        episode_id: episode.episode_id.clone(),
        agent_id: episode.agent_id.clone(),
        goal_key: episode.goal_key.clone(),
        task_title: episode.task_title.clone(),
        outcome_kind: episode.outcome_kind.clone(),
        outcome_summary: episode.outcome_summary.clone(),
        successful: workflow_episode_looks_successful(episode),
        signature,
        tool_sequence,
        action_sequence,
    })
}

fn workflow_episode_looks_successful(episode: &V3EpisodeRecord) -> bool {
    let outcome = normalize_workflow_token(&episode.outcome_kind);
    let status = episode
        .execution_status
        .as_deref()
        .map(normalize_workflow_token)
        .unwrap_or_default();
    let successful_actions = episode
        .actions_taken
        .iter()
        .filter(|action| action.succeeded)
        .count();
    if successful_actions == 0 {
        return false;
    }

    let has_success_outcome = episode.outcome_is_succeeded()
        || workflow_success_outcome_token(outcome.as_str())
        || workflow_success_status_token(status.as_str());
    let has_failure_outcome = episode.outcome_is_failed()
        || workflow_failure_outcome_token(outcome.as_str())
        || workflow_failure_status_token(status.as_str())
        || episode.last_error.is_some()
        || episode.failure_count.unwrap_or_default() > 0;
    has_success_outcome && !has_failure_outcome
}

fn workflow_success_outcome_token(value: &str) -> bool {
    matches!(
        value,
        "goal_achieved" | "partial_progress" | "success" | "succeeded" | "completed"
    )
}

fn workflow_success_status_token(value: &str) -> bool {
    matches!(value, "completed" | "success" | "succeeded")
}

fn workflow_failure_outcome_token(value: &str) -> bool {
    matches!(
        value,
        "failed"
            | "failure"
            | "budget_exhausted"
            | "circuit_open"
            | "cannot_proceed"
            | "loop_detected"
            | "cancelled"
            | "canceled"
            | "user_intervened"
            | "not_completed"
            | "not_achieved"
            | "goal_not_achieved"
            | "incomplete"
            | "unsuccessful"
    )
}

fn workflow_failure_status_token(value: &str) -> bool {
    matches!(
        value,
        "failed"
            | "failure"
            | "runtime_error"
            | "budget_exhausted"
            | "max_iterations_reached"
            | "cannot_proceed"
            | "loop_detected"
            | "cancelled"
            | "canceled"
            | "not_completed"
            | "incomplete"
            | "unsuccessful"
    )
}

fn normalize_workflow_token(value: &str) -> String {
    value
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' })
        .collect::<String>()
        .split('_')
        .filter(|part| !part.is_empty())
        .take(8)
        .collect::<Vec<_>>()
        .join("_")
}

fn compact_value(value: &Value, depth: usize) -> Value {
    if depth >= 6 {
        return Value::String("[truncated:depth]".to_string());
    }
    match value {
        Value::String(text) => Value::String(truncate_chars(text, 3_000)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .take(24)
                .map(|item| compact_value(item, depth + 1))
                .collect(),
        ),
        Value::Object(map) => {
            // Wide caller-owned objects must not crowd out the identity and
            // learning-signal fields that make a reflection attributable and
            // actionable. Retain those keys first, then fill the remaining
            // width deterministically from the source map.
            const PRIORITY_KEYS: &[&str] = &[
                "episode_id",
                "task_id",
                "execution_id",
                "agent_id",
                "scope",
                "outcome_kind",
                "outcome_summary",
                "workflow_detection",
                "procedure_feedback",
                "critical_identity",
            ];
            let mut compacted = serde_json::Map::new();
            for key in PRIORITY_KEYS {
                if let Some(nested) = map.get(*key) {
                    compacted.insert((*key).to_string(), compact_value(nested, depth + 1));
                }
            }
            for (key, nested) in map {
                if compacted.len() >= MAX_REFLECTION_OBJECT_FIELDS {
                    break;
                }
                if !compacted.contains_key(key) {
                    compacted.insert(key.clone(), compact_value(nested, depth + 1));
                }
            }
            Value::Object(compacted)
        },
        other => other.clone(),
    }
}

fn bounded_reflection_json(mut value: Value, max_chars: usize) -> Result<String> {
    value = compact_value(&value, 0);
    loop {
        let rendered = serde_json::to_string_pretty(&value)?;
        let rendered_chars = rendered.chars().count();
        if rendered_chars <= max_chars {
            return Ok(rendered);
        }
        let largest_string = reflection_largest_string_chars(&value);
        if largest_string > 48 {
            let excess = rendered_chars.saturating_sub(max_chars);
            let reduction = excess.max(largest_string / 3).min(largest_string - 24);
            if shrink_reflection_string(&mut value, largest_string, largest_string - reduction) {
                continue;
            }
        }
        if drop_reflection_tail(&mut value) {
            continue;
        }
        return Ok(
            "{\"truncated\":true,\"reason\":\"reflection_context_exceeded_hard_limit\"}"
                .to_string(),
        );
    }
}

fn validate_reflection_request_size(
    system_prompt: &str,
    user_prompt: &str,
    tool_schema: &str,
) -> Result<()> {
    let rendered_chars = system_prompt
        .chars()
        .count()
        .saturating_add(user_prompt.chars().count())
        .saturating_add(tool_schema.chars().count());
    anyhow::ensure!(
        rendered_chars <= MAX_REFLECTION_REQUEST_CHARS,
        "learning_reflection_prompt_exceeded_hard_limit:{rendered_chars}>{MAX_REFLECTION_REQUEST_CHARS}"
    );
    Ok(())
}

fn reflection_largest_string_chars(value: &Value) -> usize {
    match value {
        Value::String(text) => text.chars().count(),
        Value::Array(items) => items
            .iter()
            .map(reflection_largest_string_chars)
            .max()
            .unwrap_or_default(),
        Value::Object(map) => map
            .values()
            .map(reflection_largest_string_chars)
            .max()
            .unwrap_or_default(),
        _ => 0,
    }
}

fn shrink_reflection_string(value: &mut Value, target_len: usize, new_len: usize) -> bool {
    match value {
        Value::String(text) if text.chars().count() == target_len => {
            *text = truncate_chars(text, new_len);
            true
        },
        Value::Array(items) => items
            .iter_mut()
            .any(|item| shrink_reflection_string(item, target_len, new_len)),
        Value::Object(map) => map
            .values_mut()
            .any(|item| shrink_reflection_string(item, target_len, new_len)),
        _ => false,
    }
}

fn drop_reflection_tail(value: &mut Value) -> bool {
    match value {
        Value::Array(items) if items.len() > 1 => {
            items.pop();
            true
        },
        Value::Array(items) => items.iter_mut().any(drop_reflection_tail),
        Value::Object(map) => map.values_mut().any(drop_reflection_tail),
        _ => false,
    }
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push_str("...[truncated]");
    out
}

/// Inject the resolved focus-area program document path onto a program-state
/// candidate's proposed change. Mirrors the program-state bridge's payload
/// selection so the path lands on the same object the bridge reads for the
/// `patch`, replacing whatever path the model proposed.
fn set_program_state_candidate_path(proposed_change: &mut Value, program_relative_path: &str) {
    let Some(map) = proposed_change.as_object_mut() else {
        return;
    };
    let path_value = Value::String(program_relative_path.to_string());
    if let Some(entry) = map
        .get_mut("program_state_update")
        .and_then(Value::as_object_mut)
    {
        entry.insert("program_relative_path".to_string(), path_value);
    } else if let Some(entry) = map.get_mut("program_state").and_then(Value::as_object_mut) {
        entry.insert("program_relative_path".to_string(), path_value);
    } else {
        map.insert("program_relative_path".to_string(), path_value);
    }
}

fn parse_candidate_type(value: &str) -> LearningCandidateType {
    match normalize_token(value).as_str() {
        "memory_fact" => LearningCandidateType::MemoryFact,
        "memory_preference" => LearningCandidateType::MemoryPreference,
        "memory_procedure" => LearningCandidateType::MemoryProcedure,
        "skill_update" => LearningCandidateType::SkillUpdate,
        "capability_update" => LearningCandidateType::CapabilityUpdate,
        "tool_schema_update" => LearningCandidateType::ToolSchemaUpdate,
        "tool_wrapper_fix" => LearningCandidateType::ToolWrapperFix,
        "agent_persona_update" => LearningCandidateType::AgentPersonaUpdate,
        "workflow_template" => LearningCandidateType::WorkflowTemplate,
        "evaluation_case" => LearningCandidateType::EvaluationCase,
        "program_state_update" => LearningCandidateType::ProgramStateUpdate,
        "harness_profile_revision" => LearningCandidateType::HarnessProfileRevision,
        "bug_report" => LearningCandidateType::BugReport,
        "docs_update" => LearningCandidateType::DocsUpdate,
        _ => LearningCandidateType::Other,
    }
}

fn parse_risk_level(value: &str) -> LearningRiskLevel {
    match normalize_token(value).as_str() {
        "low" => LearningRiskLevel::Low,
        "high" => LearningRiskLevel::High,
        "critical" => LearningRiskLevel::Critical,
        _ => LearningRiskLevel::Medium,
    }
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn reflection_tool_schema() -> String {
    json!({
        "name": "record_learning_reflection",
        "description": "Record learning reflection candidates or a no-op reason. Memory, reusable procedure, evaluation, capability-evolution, skill, workflow, and low-risk program-state candidates may be routed after recording; all other candidate types stay review-gated.",
        "parameters": {
            "type": "object",
            "additionalProperties": false,
            "required": ["no_op_reason", "candidates"],
            "properties": {
                "no_op_reason": {
                    "type": ["string", "null"],
                    "description": "Short reason when no durable learning candidate should be created."
                },
                "procedure_feedback": {
                    "type": "array",
                    "maxItems": 12,
                    "description": "Per-procedure judgement for procedures supplied in extra_context.procedure_feedback.used_procedures. Required for changing procedure success/failure counters; omit or use unknown only when evidence is insufficient.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": [
                            "procedure_id",
                            "verdict",
                            "rationale",
                            "confidence",
                            "deprecation_recommended"
                        ],
                        "properties": {
                            "procedure_id": { "type": "string" },
                            "verdict": {
                                "type": "string",
                                "enum": [
                                    "useful",
                                    "harmful",
                                    "stale",
                                    "misleading",
                                    "too_broad",
                                    "too_narrow",
                                    "irrelevant",
                                    "unknown"
                                ]
                            },
                            "rationale": {
                                "type": "string",
                                "description": "Evidence-backed reason for the verdict."
                            },
                            "confidence": {
                                "type": "number",
                                "minimum": 0,
                                "maximum": 1
                            },
                            "deprecation_recommended": {
                                "type": "boolean",
                                "description": "True only when the procedure should stop being injected; the runtime creates the reviewed deprecation candidate from this top-level feedback."
                            }
                        }
                    }
                },
                "candidates": {
                    "type": "array",
                    "maxItems": 8,
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": [
                            "candidate_type",
                            "title",
                            "summary",
                            "rationale",
                            "proposed_change",
                            "proposed_target",
                            "confidence",
                            "risk_level",
                            "review_required",
                            "review_reason",
                            "promotion_target",
                            "evidence"
                        ],
                        "properties": {
                            "candidate_type": {
                                "type": "string",
                                "enum": [
                                    "memory_fact",
                                    "memory_preference",
                                    "memory_procedure",
                                    "skill_update",
                                    "capability_update",
                                    "tool_schema_update",
                                    "tool_wrapper_fix",
                                    "agent_persona_update",
                                    "workflow_template",
                                    "evaluation_case",
                                    "program_state_update",
                                    "bug_report",
                                    "docs_update",
                                    "other"
                                ]
                            },
                            "title": { "type": "string" },
                            "summary": { "type": "string" },
                            "rationale": { "type": "string" },
                            "proposed_change": {
                                "type": "object",
                                "description": "Structured proposal. For memory fact/preference candidates include proposed_change.memory with scope, target_tier, operation, key, value, explicit_user_request, explicit_user_correction, and optional replaces_key. For memory_procedure candidates include proposed_change.procedure with title, summary, activation, workflow/procedure_steps, decision_points, verification/verification_steps, failure_modes, workflow_signature, source_refs, success_count, and failure_count; use existing_procedure_id or procedure_id only when updating/reusing a known procedure. Do not emit separate deprecation procedure candidates for retrieved procedure feedback; use top-level procedure_feedback.deprecation_recommended so the runtime creates one reviewed deprecation proposal. For evaluation candidates include proposed_change.evaluation with case_kind, priority, target_agent_id, focus_area, goal, failure_mode, expected_behavior, reproduction_steps, success_criteria, optional commands/regression_commands, and source_refs. For capability/tool candidates include proposed_change.capability_evolution with capability_id, failure_pattern, proposed_fix_type, proposed_files, required_eval, promotion_gate, expected_behavior, and source_refs. For skill/workflow candidates include proposed_change.skill_update or proposed_change.workflow_template with target skill, source_procedure_id when promoting an existing procedure, workflow_signature, trigger_conditions, procedure_steps, tool_choice_guidance, common_failure_modes, examples, verification_steps, when_not_to_use, proposed_files, required_eval, promotion_gate, and source_refs. For program-state candidates include proposed_change.program_state_update with program_relative_path, optional program_section, optional goal_id, patch, reason, and source_refs. For failed or questionable outcomes include proposed_change.meta_harness_diagnosis with outcome_correctness, evidence_quality, hallucinated_completion, missed_requirements, tool_misuse, memory_miss, bad_retrieval, capability_weakness, prompt_weakness, evaluation_gap, recommended_candidate_type, and recommended_followup when supported. Do not include secrets."
                            },
                            "proposed_target": {
                                "type": ["string", "null"],
                                "description": "Memory tier, skill, capability id, wrapper path, eval suite, persona, program, or docs target."
                            },
                            "confidence": {
                                "type": "number",
                                "minimum": 0,
                                "maximum": 1
                            },
                            "risk_level": {
                                "type": "string",
                                "enum": ["low", "medium", "high", "critical"]
                            },
                            "review_required": { "type": "boolean" },
                            "review_reason": { "type": ["string", "null"] },
                            "promotion_target": { "type": ["string", "null"] },
                            "evidence": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Concise evidence snippets or references from the supplied experience."
                            }
                        }
                    }
                }
            }
        }
    })
    .to_string()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::magician_v2::agents::ActionSummary;
    use serde_json::json;

    fn workflow_episode(outcome_kind: &str, execution_status: Option<&str>) -> V3EpisodeRecord {
        V3EpisodeRecord {
            schema_version: "v3".to_string(),
            record_type: "memory_episode".to_string(),
            principal: Some("anonymous".to_string()),
            workspace: Some("default".to_string()),
            agent_id: "personal-assistant".to_string(),
            episode_id: "episode_reflection_test".to_string(),
            goal_key: "goal".to_string(),
            consolidation_key: "goal".to_string(),
            trigger_type: "test".to_string(),
            trigger_seq: 1,
            trigger_timestamp: "2026-05-15T00:00:00Z".to_string(),
            trigger_payload: None,
            started_at: "2026-05-15T00:00:00Z".to_string(),
            completed_at: "2026-05-15T00:00:01Z".to_string(),
            outcome_kind: outcome_kind.to_string(),
            outcome_summary: outcome_kind.to_string(),
            outcome_remaining: None,
            pending_actions: Vec::new(),
            failure_count: None,
            last_error: None,
            task_id: Some("task_reflection_test".to_string()),
            execution_id: Some("exec_reflection_test".to_string()),
            root_execution_id: None,
            parent_execution_id: None,
            relationship_type: None,
            ui_thread_id: None,
            task_title: None,
            task_description: None,
            execution_status: execution_status.map(str::to_string),
            outcome_type: None,
            execution_output_id: None,
            task_agent_output_id: None,
            task_user_output_id: None,
            source_output_ids: Vec::new(),
            actions_taken: vec![ActionSummary {
                action_type: "tool_call".to_string(),
                description: "Ran a tool".to_string(),
                tool: "browser".to_string(),
                succeeded: true,
                duration_ms: Some(10),
                metadata: HashMap::new(),
            }],
            observations: Vec::new(),
            memory_updates: Vec::new(),
            memory_candidates: Vec::new(),
            strategy_summary: None,
            context_at_start: None,
            artifact_output: None,
            provenance: None,
            origin_surface: None,
            origin_meeting: None,
        }
    }

    #[test]
    fn workflow_success_detector_rejects_negated_completion_status() {
        let episode = workflow_episode("goal_achieved", Some("not completed"));
        assert!(!workflow_episode_looks_successful(&episode));
    }

    #[test]
    fn workflow_success_detector_accepts_canonical_success_with_action() {
        let episode = workflow_episode("goal_achieved", Some("completed"));
        assert!(workflow_episode_looks_successful(&episode));
    }

    #[test]
    fn workflow_success_detector_requires_successful_action() {
        let mut episode = workflow_episode("goal_achieved", Some("completed"));
        episode.actions_taken[0].succeeded = false;
        assert!(!workflow_episode_looks_successful(&episode));
    }

    #[test]
    fn duplicate_feedback_deprecation_candidate_is_skipped() {
        let judgements = vec![LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_stale_flow".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Stale,
            rationale: "The retrieved procedure is stale.".to_string(),
            confidence: Some(0.8),
            deprecation_recommended: true,
        }];
        let targets = procedure_feedback_deprecation_targets(&judgements);
        let candidate = ReflectedCandidate {
            candidate_type: "memory_procedure".to_string(),
            title: "Deprecate stale flow".to_string(),
            summary: "Stop injecting stale flow.".to_string(),
            rationale: "Top-level feedback already asked for deprecation.".to_string(),
            proposed_change: json!({
                "procedure": {
                    "existing_procedure_id": "proc_stale_flow",
                    "deprecation_recommended": true,
                    "deprecation_reason": "stale flow"
                }
            }),
            proposed_target: Some("proc_stale_flow".to_string()),
            confidence: Some(0.8),
            risk_level: "medium".to_string(),
            review_required: true,
            review_reason: Some("review deprecation".to_string()),
            promotion_target: Some("learning_procedure".to_string()),
            evidence: vec!["feedback".to_string()],
        };

        assert!(reflected_candidate_duplicates_feedback_deprecation(
            &candidate, &targets
        ));
    }

    #[test]
    fn procedure_update_candidate_for_same_feedback_target_is_not_skipped() {
        let judgements = vec![LearningProcedureRunFeedbackJudgement {
            procedure_id: "proc_stale_flow".to_string(),
            verdict: LearningProcedureRunFeedbackVerdict::Stale,
            rationale: "The retrieved procedure needs clearer boundaries.".to_string(),
            confidence: Some(0.8),
            deprecation_recommended: true,
        }];
        let targets = procedure_feedback_deprecation_targets(&judgements);
        let candidate = ReflectedCandidate {
            candidate_type: "memory_procedure".to_string(),
            title: "Improve stale flow boundaries".to_string(),
            summary: "Add a missing avoid_when boundary.".to_string(),
            rationale: "This is a substantive update, not a duplicate deprecation.".to_string(),
            proposed_change: json!({
                "procedure": {
                    "existing_procedure_id": "proc_stale_flow",
                    "activation": {
                        "avoid_when": ["the page uses the redesigned flow"]
                    }
                }
            }),
            proposed_target: Some("proc_stale_flow".to_string()),
            confidence: Some(0.8),
            risk_level: "medium".to_string(),
            review_required: true,
            review_reason: Some("review update".to_string()),
            promotion_target: Some("learning_procedure".to_string()),
            evidence: vec!["feedback".to_string()],
        };

        assert!(!reflected_candidate_duplicates_feedback_deprecation(
            &candidate, &targets
        ));
    }

    #[test]
    fn reflection_json_is_valid_and_hard_bounded() {
        let oversized = json!({
            "critical_identity": "episode-1",
            "huge": "context ".repeat(20_000),
            "nested": (0..100).map(|index| json!({
                "index": index,
                "value": "nested ".repeat(2_000),
            })).collect::<Vec<_>>(),
        });
        let rendered = bounded_reflection_json(oversized, 4_000).expect("bounded JSON");
        let parsed: Value = serde_json::from_str(&rendered).expect("valid JSON");
        assert!(rendered.chars().count() <= 4_000);
        assert_eq!(parsed["critical_identity"], "episode-1");
    }

    #[test]
    fn reflection_request_ceiling_includes_templates_and_tool_schema() {
        let system = "s".repeat(12_000);
        let user = "u".repeat(44_000);
        let schema = "t".repeat(MAX_REFLECTION_REQUEST_CHARS - 56_000);
        assert!(validate_reflection_request_size(&system, &user, &schema).is_ok());

        let oversized_schema = format!("{schema}overflow");
        assert!(validate_reflection_request_size(&system, &user, &oversized_schema).is_err());
    }

    #[test]
    fn reflection_compaction_bounds_object_width() {
        let wide = Value::Object(
            (0..100)
                .map(|index| (format!("field-{index:03}"), json!(index)))
                .collect(),
        );
        let compact = compact_value(&wide, 0);
        assert_eq!(
            compact.as_object().expect("object").len(),
            MAX_REFLECTION_OBJECT_FIELDS
        );
    }

    #[test]
    fn reflection_compaction_preserves_identity_and_learning_signals_in_wide_objects() {
        let mut wide = serde_json::Map::new();
        for index in 0..100 {
            wide.insert(format!("field-{index:03}"), json!(index));
        }
        wide.insert("episode_id".to_string(), json!("episode-essential"));
        wide.insert("workflow_detection".to_string(), json!({"repeat": true}));
        wide.insert(
            "procedure_feedback".to_string(),
            json!({"used": ["proc-1"]}),
        );

        let compact = compact_value(&Value::Object(wide), 0);
        let compact = compact.as_object().expect("object");
        assert_eq!(compact.len(), MAX_REFLECTION_OBJECT_FIELDS);
        assert_eq!(compact["episode_id"], "episode-essential");
        assert_eq!(compact["workflow_detection"]["repeat"], true);
        assert_eq!(compact["procedure_feedback"]["used"][0], "proc-1");
    }
}

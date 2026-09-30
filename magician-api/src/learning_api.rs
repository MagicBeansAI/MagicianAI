use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use actix_web::{web, HttpRequest, HttpResponse};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::process::Command;
use tokio::time::{timeout, Duration};
use uuid::Uuid;

use crate::scope::resolve_required_scope;
use magician::magician_v2::artifact_v2::workspace::ArtifactV2Workspace;
use magician::magician_v2::learning::{
    build_learning_gap_audit, record_teaching_feedback, run_learning_growth_evaluation,
    CreateLearningCandidateRequest, CreateLearningEventRequest, CreateLearningProcedureRequest,
    CreateLearningTeachingFeedbackRequest, LearningCandidateFilters, LearningCandidateState,
    LearningCandidateType, LearningCapabilityEvolutionApplicationFilters,
    LearningCapabilityEvolutionApplicationMode, LearningCapabilityEvolutionApplicationRecord,
    LearningCapabilityEvolutionApplicationStatus, LearningCapabilityEvolutionAppliedFile,
    LearningCapabilityEvolutionBacklogFilters, LearningCapabilityEvolutionBacklogItem,
    LearningCapabilityEvolutionBacklogStatus, LearningCapabilityEvolutionImplementationFilters,
    LearningCapabilityEvolutionImplementationRecord,
    LearningCapabilityEvolutionPostPromotionMonitorFilters,
    LearningCapabilityEvolutionPostPromotionMonitorRecord,
    LearningCapabilityEvolutionPostPromotionMonitorStatus,
    LearningCapabilityEvolutionPromotionFilters, LearningCapabilityEvolutionPromotionRecord,
    LearningCapabilityEvolutionProposal, LearningCapabilityEvolutionProposalFilters,
    LearningCapabilityEvolutionProposalPatch, LearningCapabilityEvolutionProposalStatus,
    LearningCapabilityEvolutionRollbackRecommendationFilters,
    LearningCapabilityEvolutionRollbackRecommendationRecord,
    LearningCapabilityEvolutionRollbackRecommendationStatus,
    LearningCapabilityEvolutionStewardAction, LearningCapabilityEvolutionStewardActionStatus,
    LearningCapabilityEvolutionStewardRunFilters, LearningCapabilityEvolutionStewardRunReport,
    LearningCapabilityEvolutionStewardRunStatus, LearningCapabilityEvolutionValidationFilters,
    LearningCapabilityEvolutionValidationReport, LearningCapabilityEvolutionValidationStatus,
    LearningEvaluationBacklogFilters, LearningEvaluationBacklogItem,
    LearningEvaluationBacklogStatus, LearningEvaluationRunFilters, LearningEvaluationRunReport,
    LearningEvaluationRunStatus, LearningEvidenceRef, LearningGrowthEvaluationRunFilters,
    LearningHarnessProfileBridge, LearningMemoryBridge, LearningProcedureBridge,
    LearningProcedureFilters, LearningProcedureSkillPromotionBridge, LearningProgramStateBridge,
    LearningRiskLevel, LearningScope, LearningSkillInvocationEvidence,
    LearningSkillInvocationStatus, LearningStore, PromoteLearningProcedureToSkillRequest,
    RunLearningGrowthEvaluationRequest, TransitionLearningCandidateRequest,
    TransitionLearningProcedureRequest,
};

const MAX_CAPABILITY_APPLICATION_FILE_CHARS: usize = 500_000;
const CAPABILITY_APPLICATION_ALLOWED_TOP_LEVEL_DIRS: &[&str] = &["skills"];
const MAX_CAPABILITY_VALIDATION_COMMANDS: usize = 8;
const MAX_CAPABILITY_VALIDATION_OUTPUT_CHARS: usize = 12_000;
const DEFAULT_CAPABILITY_VALIDATION_TIMEOUT_SECONDS: u64 = 120;
const MAX_CAPABILITY_VALIDATION_TIMEOUT_SECONDS: u64 = 600;
const LEARNING_SKILL_GUIDANCE_MARKER_PREFIX: &str = "<!-- magician-learning-guidance:";
const DEFAULT_CAPABILITY_STEWARD_BACKLOG_LIMIT: usize = 10;
const DEFAULT_CAPABILITY_STEWARD_MAX_PROPOSALS: usize = 3;
const DEFAULT_CAPABILITY_STEWARD_MAX_EVALUATIONS: usize = 5;
const DEFAULT_CAPABILITY_STEWARD_MAX_VALIDATIONS: usize = 2;
const DEFAULT_CAPABILITY_STEWARD_MAX_IMPLEMENTATIONS: usize = 2;
const DEFAULT_CAPABILITY_STEWARD_MAX_DRY_RUNS: usize = 2;
const DEFAULT_CAPABILITY_STEWARD_MAX_TOUCHED_FILES: usize = 8;
const DEFAULT_CAPABILITY_STEWARD_VALIDATION_TIMEOUT_SECONDS: u64 = 60;
const PHASE4_REVIEW_MARKER_PREFIX: &str = "magician-phase4-review:";
const DEFAULT_PHASE7_MONITOR_INVOCATION_LIMIT: usize = 500;
const DEFAULT_PHASE7_MONITOR_MIN_AFTER_INVOCATIONS: u64 = 3;
const DEFAULT_PHASE7_MONITOR_FAILURE_DELTA_THRESHOLD: f64 = 0.20;

#[derive(Clone)]
pub struct LearningApi {
    store: LearningStore,
    repo_root: PathBuf,
}

impl LearningApi {
    pub fn new(workspace_layout: ArtifactV2Workspace, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            store: LearningStore::new(workspace_layout),
            repo_root: repo_root.into(),
        }
    }

    fn resolve_scope(
        &self,
        req: &HttpRequest,
        workspace: Option<String>,
    ) -> Result<LearningScope, HttpResponse> {
        let (principal, workspace) = resolve_required_scope(req.headers(), workspace)?;
        Ok(LearningScope::new(principal, workspace))
    }
}

fn learning_procedure_error_response(error: impl ToString) -> HttpResponse {
    let message = error.to_string();
    if message.contains("was not found") {
        return HttpResponse::NotFound().json(json!({
            "error": message
        }));
    }
    if message.contains("exists in multiple status directories")
        || message.contains("already exists in this scope")
    {
        return HttpResponse::Conflict().json(json!({
            "error": message
        }));
    }
    if message.contains("must be created as draft")
        || message.contains("learning procedure id must")
        || message.contains("may contain only ASCII")
        || message.contains("only memory_procedure candidates can be promoted")
        || message.contains("cannot be promoted through the procedure bridge")
        || message.contains("is terminal in state")
    {
        return HttpResponse::BadRequest().json(json!({
            "error": message
        }));
    }
    HttpResponse::InternalServerError().json(json!({
        "error": message
    }))
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningScopeQuery {
    pub workspace: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCandidateListQuery {
    pub workspace: Option<String>,
    pub state: Option<String>,
    pub candidate_type: Option<String>,
    pub source_agent_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningEventsQuery {
    pub workspace: Option<String>,
    pub max_lines: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessCyclesQuery {
    pub workspace: Option<String>,
    /// Optional: restrict to one harness agent (e.g. `ceo`).
    pub agent_id: Option<String>,
    /// Max harness-cycle events to return (newest first).
    pub limit: Option<usize>,
    /// How many recent learning events to scan before filtering (the events are
    /// interleaved with all other learning events in one per-scope log).
    pub scan: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessAnomaliesQuery {
    pub workspace: Option<String>,
    /// Optional wire-status filter: `open|fix_dispatched|resolved|dismissed`.
    pub status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HarnessProgramStateRevertRequest {
    pub workspace: Option<String>,
    /// The `harness_program_state_auto_applied` event id to revert.
    pub event_id: String,
    /// Optional human-readable reason recorded on the inverse update.
    pub reason: Option<String>,
    /// How many recent learning events to scan to locate the auto-apply event.
    pub scan: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningEvaluationBacklogQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub target_agent_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningEvaluationRunQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningGrowthEvaluationRunQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub suite_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningProcedureListQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub owner_agent: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionBacklogQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub candidate_type: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionProposalQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionValidationQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionImplementationQuery {
    pub workspace: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionApplicationQuery {
    pub workspace: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionRollbackRecommendationQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub application_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecideLearningCapabilityEvolutionRollbackRecommendationRequest {
    pub workspace: Option<String>,
    pub status: LearningCapabilityEvolutionRollbackRecommendationStatus,
    pub actor: String,
    pub summary: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionPromotionQuery {
    pub workspace: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionPostPromotionMonitorQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub candidate_id: Option<String>,
    pub capability_id: Option<String>,
    pub promotion_id: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunLearningCapabilityEvolutionPostPromotionMonitorRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_post_promotion_monitor_actor")]
    pub actor: String,
    pub invocation_limit: Option<usize>,
    pub min_after_invocations: Option<u64>,
    pub failure_delta_threshold: Option<f64>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LearningCapabilityEvolutionStewardRunQuery {
    pub workspace: Option<String>,
    pub status: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct UpsertLearningCapabilityEvolutionProposalRequest {
    pub workspace: Option<String>,
    pub status: Option<LearningCapabilityEvolutionProposalStatus>,
    pub title: Option<String>,
    pub summary: Option<String>,
    pub capability_id: Option<String>,
    pub proposed_fix_type: Option<String>,
    #[serde(default)]
    pub proposed_files: Vec<String>,
    pub change_plan: Option<Value>,
    #[serde(default)]
    pub patches: Vec<LearningCapabilityEvolutionProposalPatch>,
    pub eval_plan: Option<Value>,
    pub validation_plan: Option<Value>,
    pub promotion_gate: Option<Value>,
    pub generated_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftLearningCapabilityEvolutionProposalsRequest {
    pub workspace: Option<String>,
    pub candidate_id: Option<String>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub overwrite: bool,
    pub ready_for_review: Option<bool>,
    pub generated_by: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DecideLearningCapabilityEvolutionProposalRequest {
    pub workspace: Option<String>,
    pub status: LearningCapabilityEvolutionProposalStatus,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GenerateLearningCapabilityEvolutionEvaluationRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunLearningEvaluationBacklogRequest {
    pub workspace: Option<String>,
    pub runner: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    pub include_regression: Option<bool>,
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunLearningCapabilityEvolutionValidationRequest {
    pub workspace: Option<String>,
    pub runner: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub commands: Vec<String>,
    pub include_regression: Option<bool>,
    pub timeout_seconds: Option<u64>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordLearningCapabilityEvolutionValidationRequest {
    pub workspace: Option<String>,
    pub status: LearningCapabilityEvolutionValidationStatus,
    pub runner: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub commands: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub metrics: Value,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordLearningCapabilityEvolutionImplementationRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    pub summary: String,
    pub validation_id: Option<String>,
    #[serde(default)]
    pub applied_files: Vec<String>,
    #[serde(default)]
    pub patches: Vec<LearningCapabilityEvolutionProposalPatch>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftLearningCapabilityEvolutionImplementationRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    pub validation_id: Option<String>,
    pub summary: Option<String>,
    #[serde(default)]
    pub overwrite: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApplyLearningCapabilityEvolutionImplementationRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    pub summary: Option<String>,
    pub target_surface: Option<LearningCapabilityEvolutionApplicationTargetSurface>,
    #[serde(default)]
    pub apply: bool,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunLearningCapabilityEvolutionStewardRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_steward_actor")]
    pub actor: String,
    pub backlog_limit: Option<usize>,
    pub max_proposals: Option<usize>,
    pub max_evaluations: Option<usize>,
    pub max_validations: Option<usize>,
    pub max_implementations: Option<usize>,
    pub max_dry_runs: Option<usize>,
    pub max_touched_files: Option<usize>,
    pub timeout_seconds: Option<u64>,
    pub include_regression: Option<bool>,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionApplicationTargetSurface {
    ScopedSkill,
    SystemSkill,
    SourceSkill,
}

impl LearningCapabilityEvolutionApplicationTargetSurface {
    fn as_str(&self) -> &'static str {
        match self {
            Self::ScopedSkill => "scoped_skill",
            Self::SystemSkill => "system_skill",
            Self::SourceSkill => "source_skill",
        }
    }

    fn description(&self) -> &'static str {
        match self {
            Self::ScopedSkill => "scoped workspace skill layer",
            Self::SystemSkill => "system skill layer",
            Self::SourceSkill => "source skillshub layer",
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecordLearningCapabilityEvolutionPromotionRequest {
    pub workspace: Option<String>,
    #[serde(default = "default_review_actor")]
    pub actor: String,
    pub summary: String,
    pub validation_id: Option<String>,
    pub implementation_id: Option<String>,
    pub application_id: Option<String>,
    #[serde(default)]
    pub applied_files: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

pub async fn get_learning_audit_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match build_learning_gap_audit(
        api.store.workspace_layout(),
        &scope.principal,
        &scope.workspace,
    ) {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_candidates_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCandidateListQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCandidateFilters {
        state: query.state.clone(),
        candidate_type: query.candidate_type.clone(),
        source_agent_id: query.source_agent_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api.store.list_candidates(&scope, filters) {
        Ok(candidates) => HttpResponse::Ok().json(serde_json::json!({
            "scope": scope,
            "count": candidates.len(),
            "candidates": candidates
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_candidate_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_candidate_with_decisions(&scope, path.as_str())
    {
        Ok(candidate) => HttpResponse::Ok().json(candidate),
        Err(error) => HttpResponse::NotFound().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn transition_learning_candidate_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<TransitionLearningCandidateRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if request.to_state == LearningCandidateState::Promoted {
        match api.store.read_candidate(&scope, path.as_str()) {
            Ok(candidate) if candidate.candidate_type == LearningCandidateType::MemoryProcedure => {
                let bridge = LearningProcedureBridge::new(api.store.workspace_layout().clone());
                return match bridge.promote_reviewed_candidate(
                    &api.store,
                    &scope,
                    &candidate,
                    &request.actor,
                    &request.reason,
                ) {
                    Ok(candidate) => HttpResponse::Ok().json(candidate),
                    Err(error) => learning_procedure_error_response(error),
                };
            },
            Ok(candidate) if candidate.candidate_type.is_memory_candidate() => {
                let bridge = LearningMemoryBridge::new(api.store.workspace_layout().clone());
                return match bridge
                    .promote_reviewed_candidate(
                        &api.store,
                        &scope,
                        &candidate,
                        &request.actor,
                        &request.reason,
                    )
                    .await
                {
                    Ok(candidate) => HttpResponse::Ok().json(candidate),
                    Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
                        "error": error.to_string()
                    })),
                };
            },
            Ok(candidate) if candidate.candidate_type.is_harness_profile_candidate() => {
                // Boundary D. Promoting is the owner's apply, and it is
                // refused with the reason when the lane has not approved the
                // candidate or no passing evaluation has been recorded. The
                // sibling arms treat a promote as the approval; this one does
                // not, because the boundary asks for the approval and the
                // apply to be separate acts.
                let bridge = LearningHarnessProfileBridge::new(api.store.workspace_layout().clone());
                return match bridge.apply_approved_candidate(
                    &api.store,
                    &scope,
                    &candidate,
                    &request.actor,
                ) {
                    Ok(outcome) if outcome.applied => match api
                        .store
                        .read_candidate(&scope, &candidate.id)
                    {
                        Ok(candidate) => HttpResponse::Ok().json(serde_json::json!({
                            "candidate": candidate,
                            "revision": outcome.revision,
                            "evaluation_id": outcome.evaluation_id,
                        })),
                        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
                            "error": error.to_string()
                        })),
                    },
                    Ok(outcome) => HttpResponse::Conflict().json(serde_json::json!({
                        "error": "harness_profile_revision_not_applied",
                        "reason": outcome.reason,
                    })),
                    Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
                        "error": error.to_string()
                    })),
                };
            },
            Ok(candidate)
                if candidate.candidate_type
                    == magician::magician_v2::learning::LearningCandidateType::ProgramStateUpdate =>
            {
                let bridge = LearningProgramStateBridge::new(api.store.workspace_layout().clone());
                return match bridge
                    .promote_reviewed_candidate(
                        &api.store,
                        &scope,
                        &candidate,
                        &request.actor,
                        &request.reason,
                    )
                    .await
                {
                    Ok(candidate) => HttpResponse::Ok().json(candidate),
                    Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
                        "error": error.to_string()
                    })),
                };
            },
            Ok(_) => {},
            Err(error) => {
                return HttpResponse::NotFound().json(serde_json::json!({
                    "error": error.to_string()
                }));
            },
        }
    }
    match api.store.transition_candidate(
        &scope,
        path.as_str(),
        request.to_state,
        request.actor,
        request.decision,
        request.reason,
        request.evidence_refs,
    ) {
        Ok(candidate) => HttpResponse::Ok().json(candidate),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct RecordHarnessProfileEvaluationRequest {
    pub workspace: Option<String>,
    /// Who or what ran the proposal's evaluation case.
    pub runner: String,
    /// What was observed, in the runner's words.
    pub observed: String,
    /// Whether the observation met the proposal's acceptance condition.
    pub passed: bool,
}

/// Boundary D's controlled run, recorded. A failing run is recorded exactly
/// like a passing one and blocks the apply; it is never discarded.
pub async fn record_harness_profile_evaluation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RecordHarnessProfileEvaluationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate = match api.store.read_candidate(&scope, path.as_str()) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(serde_json::json!({
                "error": error.to_string()
            }))
        },
    };
    if !candidate.candidate_type.is_harness_profile_candidate() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "not_a_harness_profile_candidate",
            "candidate_type": candidate.candidate_type.as_str(),
        }));
    }
    let bridge = LearningHarnessProfileBridge::new(api.store.workspace_layout().clone());
    match bridge.record_evaluation(
        &api.store,
        &scope,
        &candidate,
        &request.runner,
        &request.observed,
        request.passed,
    ) {
        Ok(record) => HttpResponse::Created().json(record),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn create_learning_candidate_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<CreateLearningCandidateRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.store.create_candidate(scope, request) {
        Ok(candidate) => HttpResponse::Created().json(candidate),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_events_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningEventsQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let max_lines = query.max_lines.unwrap_or(100).clamp(1, 1_000);
    match api.store.list_events(&scope, max_lines) {
        Ok(events) => HttpResponse::Ok().json(serde_json::json!({
            "scope": scope,
            "count": events.len(),
            "events": events
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

/// Observability read for the harness self-improvement loop: the recent
/// `harness_cycle_dispatch_outcome` events, plus a per-outcome tally so an
/// operator can answer "why does this harness agent record 0 episodes?" from
/// data (dropped_disabled / dropped_reservation_changed / dispatched /
/// episode_persisted / episode_persist_failed ...).
pub async fn list_harness_cycles_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<HarnessCyclesQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let scan = query.scan.unwrap_or(2_000).clamp(1, 20_000);
    let limit = query.limit.unwrap_or(200).clamp(1, 5_000);
    match api.store.list_events(&scope, scan) {
        Ok(events) => {
            let agent_filter = query.agent_id.clone();
            let cycles: Vec<_> = events
                .into_iter()
                .filter(|event| {
                    event.event_type
                        == magician::magician_v2::harness::HARNESS_CYCLE_DISPATCH_OUTCOME_EVENT
                })
                .filter(|event| match &agent_filter {
                    Some(agent_id) => event.agent_id.as_deref() == Some(agent_id.as_str()),
                    None => true,
                })
                .take(limit)
                .collect();
            let mut outcome_counts: std::collections::BTreeMap<String, usize> =
                std::collections::BTreeMap::new();
            for event in &cycles {
                let outcome = event
                    .payload
                    .get("outcome")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                *outcome_counts.entry(outcome).or_insert(0) += 1;
            }
            HttpResponse::Ok().json(serde_json::json!({
                "scope": scope,
                "count": cycles.len(),
                "outcome_counts": outcome_counts,
                "cycles": cycles,
            }))
        },
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

/// List persisted harness anomalies for a scope (Open-first, newest-first),
/// reusing the unit-tested `AnomalyStore::list`. An optional `status` query
/// param filters by wire status (`open|fix_dispatched|resolved|dismissed`).
pub async fn list_harness_anomalies_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<HarnessAnomaliesQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let store =
        magician::magician_v2::harness::AnomalyStore::new(api.store.workspace_layout().clone());
    let mut anomalies = store
        .list(&scope.principal, &scope.workspace)
        .unwrap_or_default();
    if let Some(filter) = query.status.as_deref() {
        anomalies.retain(|anomaly| anomaly.status_wire() == filter);
    }
    HttpResponse::Ok().json(serde_json::json!({
        "scope": scope,
        "count": anomalies.len(),
        "anomalies": anomalies,
    }))
}

/// Revert a previously auto-applied harness bookkeeping update (Phase 4.1).
/// Applies the inverse `before` snapshot captured in the auto-apply event as a
/// new provenance-stamped update (actor=`owner_revert`); never deletes history,
/// never escapes the bookkeeping whitelist.
pub async fn revert_harness_program_state_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<HarnessProgramStateRevertRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let event_id = request.event_id.trim().to_string();
    if event_id.is_empty() {
        return HttpResponse::BadRequest().json(serde_json::json!({
            "error": "event_id is required"
        }));
    }
    let reason = request
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("owner reverted harness bookkeeping auto-apply");
    let scan = request.scan.unwrap_or(5_000).clamp(1, 50_000);
    let bridge = LearningProgramStateBridge::new(api.store.workspace_layout().clone());
    match bridge
        .revert_auto_applied(&api.store, &scope, &event_id, reason, scan)
        .await
    {
        Ok(outcome) => HttpResponse::Ok().json(outcome),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn create_learning_event_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<CreateLearningEventRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.store.append_event(scope, request) {
        Ok(event) => HttpResponse::Created().json(event),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn record_learning_teaching_feedback_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<CreateLearningTeachingFeedbackRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match record_teaching_feedback(&api.store, api.store.workspace_layout(), scope, request).await {
        Ok(response) => HttpResponse::Created().json(response),
        Err(error) => HttpResponse::BadRequest().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_evaluations_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningEvaluationBacklogQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningEvaluationBacklogFilters {
        status: query.status.clone(),
        target_agent_id: query.target_agent_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api.store.list_evaluation_backlog_items(&scope, filters) {
        Ok(items) => HttpResponse::Ok().json(serde_json::json!({
            "scope": scope,
            "count": items.len(),
            "evaluations": items
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_evaluation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_evaluation_backlog_item(&scope, path.as_str())
    {
        Ok(item) => HttpResponse::Ok().json(item),
        Err(error) => HttpResponse::NotFound().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_evaluation_runs_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningEvaluationRunQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningEvaluationRunFilters {
        status: query.status.clone(),
        candidate_id: query.candidate_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api.store.list_evaluation_run_reports(&scope, filters) {
        Ok(runs) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": runs.len(),
            "runs": runs
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_evaluation_run_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, run_id) = path.into_inner();
    match api
        .store
        .read_evaluation_run_report(&scope, &candidate_id, &run_id)
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn run_learning_evaluation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RunLearningEvaluationBacklogRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let mut backlog = match api
        .store
        .read_evaluation_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if matches!(
        backlog.status,
        LearningEvaluationBacklogStatus::Rejected | LearningEvaluationBacklogStatus::Archived
    ) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning evaluation backlog item `{}` is `{}` and cannot run",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => Some(candidate),
        Err(error) if api.store.error_is_not_found(&error) => None,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate
        .as_ref()
        .is_some_and(|candidate| candidate.state.is_terminal())
    {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{candidate_id}` is terminal and cannot run evaluation"
            )
        }));
    }

    let include_regression = request.include_regression.unwrap_or(true);
    let command_specs =
        planned_evaluation_commands(&backlog, &request.commands, include_regression);
    let command_strings: Vec<String> = command_specs
        .iter()
        .map(|spec| spec.command.clone())
        .collect();
    if request.dry_run {
        return HttpResponse::Ok().json(json!({
            "scope": scope,
            "evaluation": backlog,
            "planned_commands": command_specs,
            "include_regression": include_regression,
            "dry_run": true
        }));
    }

    let runner = optional_non_empty(request.runner)
        .unwrap_or_else(|| "learning_eval_backlog_worker".to_string());
    let timeout_seconds = request
        .timeout_seconds
        .unwrap_or(DEFAULT_CAPABILITY_VALIDATION_TIMEOUT_SECONDS)
        .clamp(1, MAX_CAPABILITY_VALIDATION_TIMEOUT_SECONDS);
    let previous_backlog_status = backlog.status.clone();
    if matches!(backlog.status, LearningEvaluationBacklogStatus::Queued) {
        backlog.status = LearningEvaluationBacklogStatus::InReview;
        backlog.updated_at = Utc::now();
        if let Err(error) = api.store.write_evaluation_backlog_item(&backlog) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
    }
    let scope_root = api
        .store
        .workspace_layout()
        .scope_root(&scope.principal, &scope.workspace);
    let started_at = Utc::now();
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_evaluation_run_started".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!("Learning evaluation worker started `{}`.", backlog.id),
            evidence_refs: backlog.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "backlog_id": backlog.id.clone(),
                "runner": runner.clone(),
                "command_count": command_specs.len(),
                "include_regression": include_regression,
                "timeout_seconds": timeout_seconds,
                "scope_root": scope_root.display().to_string(),
                "from_backlog_status": previous_backlog_status.as_str(),
                "to_backlog_status": backlog.status.as_str()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let mut results = Vec::new();
    for spec in command_specs
        .iter()
        .take(MAX_CAPABILITY_VALIDATION_COMMANDS)
    {
        results.push(run_validation_command(spec, &scope_root, timeout_seconds).await);
    }
    let command_count = command_specs.len().min(MAX_CAPABILITY_VALIDATION_COMMANDS);
    let passed_count = results
        .iter()
        .filter(|result| result.success && !result.timed_out && result.spawn_error.is_none())
        .count();
    let failed_count = command_count.saturating_sub(passed_count);
    let regression_checked = command_specs.iter().any(|spec| spec.regression);
    let phase5_fixture_cases = phase5_fixture_cases_from_case_spec(&backlog.case_spec);
    let phase5_fixture_results =
        phase5_fixture_results_for_commands(&phase5_fixture_cases, &results);
    let phase5_fixture_metrics =
        phase5_fixture_result_metrics(&phase5_fixture_cases, &phase5_fixture_results);
    let run_status = if command_specs.is_empty() {
        LearningEvaluationRunStatus::Blocked
    } else if failed_count == 0 {
        LearningEvaluationRunStatus::Passed
    } else {
        LearningEvaluationRunStatus::Failed
    };
    let summary = optional_non_empty(request.summary).unwrap_or_else(|| {
        if command_specs.is_empty() {
            "Evaluation worker found no executable commands in the backlog case_spec."
                .to_string()
        } else {
            format!(
                "Evaluation worker executed {command_count} command(s): {passed_count} passed, {failed_count} failed."
            )
        }
    });
    let report = LearningEvaluationRunReport {
        id: format!("ler_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        backlog_id: backlog.id.clone(),
        status: run_status.clone(),
        runner: runner.clone(),
        summary,
        commands: command_strings,
        evidence_refs: backlog.evidence_refs.clone(),
        metrics: json!({
            "command_count": command_count,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "regression_checked": regression_checked,
            "phase5_fixture": phase5_fixture_metrics
        }),
        payload: json!({
            "runner_payload": request.payload,
            "started_at": started_at,
            "ended_at": Utc::now(),
            "scope_root": scope_root.display().to_string(),
            "include_regression": include_regression,
            "regression_checked": regression_checked,
            "max_commands": MAX_CAPABILITY_VALIDATION_COMMANDS,
            "timeout_seconds": timeout_seconds,
            "case_kind": backlog.case_kind.clone(),
            "case_spec": backlog.case_spec.clone(),
            "planned_commands": command_specs,
            "results": results,
            "phase5_fixture_cases": phase5_fixture_cases,
            "phase5_fixture_results": phase5_fixture_results
        }),
        created_at: Utc::now(),
    };
    if let Err(error) = api.store.write_evaluation_run_report(&report) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let post_run_backlog_status = if report.status == LearningEvaluationRunStatus::Blocked {
        LearningEvaluationBacklogStatus::InReview
    } else {
        LearningEvaluationBacklogStatus::Evaluated
    };
    let before_record_status = backlog.status.clone();
    backlog.status = post_run_backlog_status;
    backlog.updated_at = report.created_at;
    if let Err(error) = api.store.write_evaluation_backlog_item(&backlog) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let candidate = match candidate {
        Some(candidate) if report.status != LearningEvaluationRunStatus::Blocked => {
            match api.store.transition_candidate(
                &scope,
                &candidate_id,
                LearningCandidateState::Evaluated,
                report.runner.clone(),
                format!("learning_evaluation_{}", report.status.as_str()),
                format!(
                    "Learning evaluation run `{}` completed as {}.",
                    report.id,
                    report.status.as_str()
                ),
                report.evidence_refs.clone(),
            ) {
                Ok(candidate) => Some(candidate),
                Err(_) => Some(candidate),
            }
        },
        other => other,
    };

    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_evaluation_run_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Learning evaluation run `{}` completed as {}.",
                report.id,
                report.status.as_str()
            ),
            evidence_refs: report.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "backlog_id": backlog.id.clone(),
                "run_id": report.id.clone(),
                "run_status": report.status.as_str(),
                "runner": report.runner.clone(),
                "command_count": command_count,
                "passed_count": passed_count,
                "failed_count": failed_count,
                "regression_checked": regression_checked,
                "from_backlog_status": before_record_status.as_str(),
                "to_backlog_status": backlog.status.as_str(),
                "to_candidate_state": candidate.as_ref().map(|candidate| candidate.state.as_str())
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "run": report,
        "evaluation": backlog,
        "candidate": candidate
    }))
}

pub async fn list_learning_growth_evaluation_runs_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningGrowthEvaluationRunQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningGrowthEvaluationRunFilters {
        status: query.status.clone(),
        suite_id: query.suite_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_growth_evaluation_run_reports(&scope, filters)
    {
        Ok(runs) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": runs.len(),
            "runs": runs
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_growth_evaluation_run_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_growth_evaluation_run_report(&scope, path.as_str())
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn run_learning_growth_evaluation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<RunLearningGrowthEvaluationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let report = match run_learning_growth_evaluation(&api.store, scope.clone(), request) {
        Ok(report) => report,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if let Err(error) = api.store.write_growth_evaluation_run_report(&report) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    if let Err(error) = api.store.append_event(
        scope,
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_growth_evaluation_run_recorded".to_string(),
            agent_id: None,
            task_id: None,
            execution_id: None,
            chat_session_id: None,
            summary: report.summary.clone(),
            evidence_refs: report.evidence_refs.clone(),
            payload: json!({
                "run_id": report.id.clone(),
                "suite_id": report.suite_id.clone(),
                "run_status": report.status.as_str(),
                "metrics": report.metrics.clone()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    HttpResponse::Ok().json(json!({
        "run": report
    }))
}

pub async fn list_learning_procedures_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningProcedureListQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningProcedureFilters {
        status: query.status.clone(),
        owner_agent: query.owner_agent.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api.store.list_procedures(&scope, filters) {
        Ok(procedures) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": procedures.len(),
            "procedures": procedures
        })),
        Err(error) => learning_procedure_error_response(error),
    }
}

pub async fn create_learning_procedure_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<CreateLearningProcedureRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api.store.create_procedure(scope.clone(), request) {
        Ok(procedure) => {
            if let Err(error) = api.store.append_event(
                scope,
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_procedure_created".to_string(),
                    agent_id: procedure.owner_agent.clone(),
                    task_id: procedure.source_task_ids.first().cloned(),
                    execution_id: None,
                    chat_session_id: procedure.source_chat_session_ids.first().cloned(),
                    summary: format!("Learning procedure `{}` recorded.", procedure.id),
                    evidence_refs: procedure.evidence_refs.clone(),
                    payload: json!({
                        "procedure_id": procedure.id.clone(),
                        "procedure_status": procedure.status.as_str(),
                        "title": procedure.title.clone(),
                        "owner_agent": procedure.owner_agent.clone(),
                        "source_candidate_id": procedure.source_candidate_id.clone(),
                        "source_task_ids": procedure.source_task_ids.clone(),
                        "source_chat_session_ids": procedure.source_chat_session_ids.clone()
                    }),
                },
            ) {
                tracing::warn!(
                    error = %error,
                    procedure_id = %procedure.id,
                    "failed to append learning procedure creation event after procedure was recorded"
                );
            }
            HttpResponse::Created().json(procedure)
        },
        Err(error) => learning_procedure_error_response(error),
    }
}

pub async fn get_learning_procedure_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_procedure_with_decisions(&scope, path.as_str())
    {
        Ok(payload) => HttpResponse::Ok().json(payload),
        Err(error) => learning_procedure_error_response(error),
    }
}

pub async fn transition_learning_procedure_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<TransitionLearningProcedureRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let to_status = request.to_status.clone();
    let decision = if request.decision.trim().is_empty() {
        format!("procedure_status_{}", to_status.as_str())
    } else {
        request.decision.clone()
    };
    match api.store.transition_procedure_status(
        &scope,
        path.as_str(),
        to_status.clone(),
        request.actor.clone(),
        decision,
        request.reason.clone(),
        request.evidence_refs.clone(),
    ) {
        Ok(procedure) => {
            let event_type = match &procedure.status {
                magician::magician_v2::learning::LearningProcedureStatus::Active => {
                    "learning_procedure_promoted"
                },
                magician::magician_v2::learning::LearningProcedureStatus::Deprecated => {
                    "learning_procedure_deprecated"
                },
                magician::magician_v2::learning::LearningProcedureStatus::Archived => {
                    "learning_procedure_archived"
                },
                magician::magician_v2::learning::LearningProcedureStatus::Draft => {
                    "learning_procedure_updated"
                },
            };
            if let Err(error) = api.store.append_event(
                scope,
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: event_type.to_string(),
                    agent_id: procedure.owner_agent.clone(),
                    task_id: procedure.source_task_ids.first().cloned(),
                    execution_id: None,
                    chat_session_id: procedure.source_chat_session_ids.first().cloned(),
                    summary: format!(
                        "Learning procedure `{}` moved to {}.",
                        procedure.id,
                        procedure.status.as_str()
                    ),
                    evidence_refs: request.evidence_refs,
                    payload: json!({
                        "procedure_id": procedure.id.clone(),
                        "procedure_status": procedure.status.as_str(),
                        "actor": request.actor,
                        "reason": request.reason,
                        "version": procedure.version
                    }),
                },
            ) {
                tracing::warn!(
                    error = %error,
                    procedure_id = %procedure.id,
                    procedure_status = procedure.status.as_str(),
                    "failed to append learning procedure transition event after status changed"
                );
            }
            HttpResponse::Ok().json(procedure)
        },
        Err(error) => learning_procedure_error_response(error),
    }
}

pub async fn promote_learning_procedure_to_skill_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<PromoteLearningProcedureToSkillRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let mut request = body.into_inner();
    request.force = false;
    request.route_to_backlog = true;
    request.create_eval_candidate = true;
    request.min_success_count = None;
    request.min_evidence_count = None;
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match LearningProcedureSkillPromotionBridge::new(api.store.workspace_layout().clone())
        .promote_procedure(&api.store, &scope, path.as_str(), request)
    {
        Ok(outcome) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "outcome": outcome
        })),
        Err(error) => learning_procedure_error_response(error),
    }
}

pub async fn list_learning_capability_evolution_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionBacklogQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionBacklogFilters {
        status: query.status.clone(),
        candidate_type: query.candidate_type.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_backlog_items(&scope, filters)
    {
        Ok(items) => HttpResponse::Ok().json(serde_json::json!({
            "scope": scope,
            "count": items.len(),
            "capability_evolution": items
        })),
        Err(error) => HttpResponse::InternalServerError().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_capability_evolution_backlog_item(&scope, path.as_str())
    {
        Ok(item) => HttpResponse::Ok().json(item),
        Err(error) => HttpResponse::NotFound().json(serde_json::json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_capability_evolution_proposals_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionProposalQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionProposalFilters {
        status: query.status.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_proposals(&scope, filters)
    {
        Ok(proposals) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": proposals.len(),
            "proposals": proposals
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_proposal_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_capability_evolution_proposal(&scope, path.as_str())
    {
        Ok(proposal) => HttpResponse::Ok().json(proposal),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn draft_learning_capability_evolution_proposals_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<DraftLearningCapabilityEvolutionProposalsRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let generated_by = optional_non_empty(request.generated_by)
        .unwrap_or_else(|| "learning_capability_evolution_proposal_drafter".to_string());
    let ready_for_review = request.ready_for_review.unwrap_or(true);
    let backlogs = if let Some(candidate_id) = optional_non_empty(request.candidate_id) {
        match api
            .store
            .read_capability_evolution_backlog_item(&scope, &candidate_id)
        {
            Ok(item) => vec![item],
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        }
    } else {
        match api.store.list_capability_evolution_backlog_items(
            &scope,
            LearningCapabilityEvolutionBacklogFilters {
                status: Some(
                    LearningCapabilityEvolutionBacklogStatus::Queued
                        .as_str()
                        .to_string(),
                ),
                candidate_type: None,
                capability_id: None,
                limit: request.limit.map(|limit| limit.clamp(1, 50)).or(Some(10)),
            },
        ) {
            Ok(items) => items,
            Err(error) => {
                return HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                }));
            },
        }
    };

    let mut drafted = Vec::new();
    let mut skipped = Vec::new();
    for mut backlog in backlogs {
        let existing = api
            .store
            .read_capability_evolution_proposal(&scope, &backlog.candidate_id)
            .ok();
        if existing.is_some() && !request.overwrite {
            skipped.push(json!({
                "candidate_id": backlog.candidate_id,
                "reason": "proposal_already_exists"
            }));
            continue;
        }
        if let Some(existing) = existing.as_ref() {
            if !matches!(
                existing.status,
                LearningCapabilityEvolutionProposalStatus::Draft
                    | LearningCapabilityEvolutionProposalStatus::ReadyForReview
            ) {
                skipped.push(json!({
                    "candidate_id": backlog.candidate_id,
                    "proposal_id": existing.id,
                    "reason": format!("proposal_status_is_{}", existing.status.as_str())
                }));
                continue;
            }
        }
        let mut proposal = draft_capability_evolution_proposal(
            &scope,
            &backlog,
            existing.as_ref(),
            &generated_by,
            ready_for_review,
        );
        enrich_capability_evolution_proposal_draft(
            api.store.workspace_layout(),
            &api.repo_root,
            &scope,
            &backlog,
            &mut proposal,
        );
        if let Err(error) = api.store.write_capability_evolution_proposal(&proposal) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
        let previous_backlog_status = backlog.status.clone();
        if matches!(
            backlog.status,
            LearningCapabilityEvolutionBacklogStatus::Queued
        ) {
            backlog.status = LearningCapabilityEvolutionBacklogStatus::InReview;
            backlog.updated_at = proposal.updated_at;
            if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
                return HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                }));
            }
        }
        if let Err(error) = api.store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_capability_proposal_drafted".to_string(),
                agent_id: backlog.source_agent_id.clone(),
                task_id: backlog.source_task_id.clone(),
                execution_id: backlog.source_execution_id.clone(),
                chat_session_id: backlog.source_chat_session_id.clone(),
                summary: format!(
                    "Capability-evolution proposal `{}` drafted for candidate `{}`.",
                    proposal.id, proposal.candidate_id
                ),
                evidence_refs: backlog.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": proposal.candidate_id.clone(),
                    "backlog_id": backlog.id.clone(),
                    "proposal_id": proposal.id.clone(),
                    "status": proposal.status.as_str(),
                    "generated_by": generated_by.clone(),
                    "from_backlog_status": previous_backlog_status.as_str(),
                    "to_backlog_status": backlog.status.as_str(),
                    "proposed_files": proposal.proposed_files.clone(),
                    "has_eval_plan": proposal.eval_plan.is_some(),
                    "has_validation_plan": proposal.validation_plan.is_some(),
                    "has_promotion_gate": proposal.promotion_gate.is_some()
                }),
            },
        ) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
        drafted.push(proposal);
    }

    HttpResponse::Ok().json(json!({
        "scope": scope,
        "drafted_count": drafted.len(),
        "skipped_count": skipped.len(),
        "proposals": drafted,
        "skipped": skipped
    }))
}

pub async fn list_learning_capability_evolution_validations_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionValidationQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionValidationFilters {
        status: query.status.clone(),
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_validation_reports(&scope, filters)
    {
        Ok(validations) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": validations.len(),
            "validations": validations
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_validation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, validation_id) = path.into_inner();
    match api.store.read_capability_evolution_validation_report(
        &scope,
        &candidate_id,
        &validation_id,
    ) {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_capability_evolution_implementations_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionImplementationQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionImplementationFilters {
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_implementation_records(&scope, filters)
    {
        Ok(implementations) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": implementations.len(),
            "implementations": implementations
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_implementation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, implementation_id) = path.into_inner();
    match api.store.read_capability_evolution_implementation_record(
        &scope,
        &candidate_id,
        &implementation_id,
    ) {
        Ok(record) => HttpResponse::Ok().json(record),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_capability_evolution_applications_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionApplicationQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionApplicationFilters {
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_application_records(&scope, filters)
    {
        Ok(applications) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": applications.len(),
            "applications": applications
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_application_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, application_id) = path.into_inner();
    match api.store.read_capability_evolution_application_record(
        &scope,
        &candidate_id,
        &application_id,
    ) {
        Ok(record) => HttpResponse::Ok().json(record),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_capability_evolution_rollback_recommendations_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionRollbackRecommendationQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionRollbackRecommendationFilters {
        status: query.status.clone(),
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        application_id: query.application_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_rollback_recommendation_records(&scope, filters)
    {
        Ok(recommendations) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": recommendations.len(),
            "rollback_recommendations": recommendations
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_rollback_recommendation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, recommendation_id) = path.into_inner();
    match api
        .store
        .read_capability_evolution_rollback_recommendation_record(
            &scope,
            &candidate_id,
            &recommendation_id,
        ) {
        Ok(recommendation) => HttpResponse::Ok().json(json!({
            "rollback_recommendation": recommendation
        })),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn decide_learning_capability_evolution_rollback_recommendation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    body: web::Json<DecideLearningCapabilityEvolutionRollbackRecommendationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    if request.status == LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended {
        return HttpResponse::BadRequest().json(json!({
            "error": "rollback recommendation decision status must be dismissed or superseded"
        }));
    }
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "rollback recommendation decision actor is required"
        }));
    }
    let summary = request.summary.trim().to_string();
    if summary.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "rollback recommendation decision summary is required"
        }));
    }

    let (candidate_id, recommendation_id) = path.into_inner();
    let mut recommendation = match api
        .store
        .read_capability_evolution_rollback_recommendation_record(
            &scope,
            &candidate_id,
            &recommendation_id,
        ) {
        Ok(recommendation) => recommendation,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    let previous_status = recommendation.status.clone();
    let decided_at = Utc::now();
    recommendation.status = request.status.clone();
    recommendation
        .evidence_refs
        .extend(request.evidence_refs.clone());
    merge_application_payload_field(
        &mut recommendation.payload,
        "decision",
        json!({
            "previous_status": previous_status.as_str(),
            "status": recommendation.status.as_str(),
            "actor": actor,
            "summary": summary,
            "decided_at": decided_at,
            "evidence_refs": request.evidence_refs,
            "payload": request.payload
        }),
    );
    if let Err(error) = api
        .store
        .write_capability_evolution_rollback_recommendation_record(&recommendation)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_rollback_recommendation_decided".to_string(),
            agent_id: None,
            task_id: None,
            execution_id: None,
            chat_session_id: None,
            summary: format!(
                "Rollback recommendation `{}` marked {} by {}.",
                recommendation.id,
                recommendation.status.as_str(),
                actor
            ),
            evidence_refs: recommendation.evidence_refs.clone(),
            payload: json!({
                "candidate_id": recommendation.candidate_id.clone(),
                "proposal_id": recommendation.proposal_id.clone(),
                "validation_id": recommendation.validation_id.clone(),
                "implementation_id": recommendation.implementation_id.clone(),
                "application_id": recommendation.application_id.clone(),
                "promotion_id": recommendation.promotion_id.clone(),
                "recommendation_id": recommendation.id.clone(),
                "capability_id": recommendation.capability_id.clone(),
                "previous_status": previous_status.as_str(),
                "status": recommendation.status.as_str(),
                "trigger_kind": recommendation.trigger_kind.clone(),
                "severity": recommendation.severity.clone(),
                "actor": actor,
                "decision_summary": summary
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "rollback_recommendation": recommendation
    }))
}

pub async fn list_learning_capability_evolution_promotions_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionPromotionQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionPromotionFilters {
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_promotion_records(&scope, filters)
    {
        Ok(promotions) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": promotions.len(),
            "promotions": promotions
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_promotion_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_capability_evolution_promotion_record(&scope, path.as_str())
    {
        Ok(record) => HttpResponse::Ok().json(record),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn list_learning_capability_evolution_post_promotion_monitors_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionPostPromotionMonitorQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionPostPromotionMonitorFilters {
        status: query.status.clone(),
        candidate_id: query.candidate_id.clone(),
        capability_id: query.capability_id.clone(),
        promotion_id: query.promotion_id.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_post_promotion_monitor_records(&scope, filters)
    {
        Ok(monitors) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": monitors.len(),
            "post_promotion_monitors": monitors
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_post_promotion_monitor_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_capability_evolution_post_promotion_monitor_record(&scope, path.as_str())
    {
        Ok(record) => HttpResponse::Ok().json(json!({
            "post_promotion_monitor": record
        })),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn run_learning_capability_evolution_post_promotion_monitor_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RunLearningCapabilityEvolutionPostPromotionMonitorRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let promotion_id = path.into_inner();
    let promotion = match api
        .store
        .read_capability_evolution_promotion_record(&scope, &promotion_id)
    {
        Ok(promotion) => promotion,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    let options = Phase7PostPromotionMonitorOptions {
        invocation_limit: request
            .invocation_limit
            .unwrap_or(DEFAULT_PHASE7_MONITOR_INVOCATION_LIMIT)
            .clamp(1, 5_000),
        min_after_invocations: request
            .min_after_invocations
            .unwrap_or(DEFAULT_PHASE7_MONITOR_MIN_AFTER_INVOCATIONS)
            .max(1),
        failure_delta_threshold: request
            .failure_delta_threshold
            .unwrap_or(DEFAULT_PHASE7_MONITOR_FAILURE_DELTA_THRESHOLD)
            .clamp(0.0, 1.0),
        actor: request.actor,
        request_payload: request.payload,
    };
    match phase7_record_post_promotion_monitor(&api, &scope, &promotion, options) {
        Ok(monitor) => HttpResponse::Ok().json(json!({
            "post_promotion_monitor": monitor
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error
        })),
    }
}

pub async fn list_learning_capability_evolution_steward_runs_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    query: web::Query<LearningCapabilityEvolutionStewardRunQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let filters = LearningCapabilityEvolutionStewardRunFilters {
        status: query.status.clone(),
        limit: query.limit.map(|limit| limit.clamp(1, 1_000)),
    };
    match api
        .store
        .list_capability_evolution_steward_run_reports(&scope, filters)
    {
        Ok(runs) => HttpResponse::Ok().json(json!({
            "scope": scope,
            "count": runs.len(),
            "runs": runs
        })),
        Err(error) => HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn get_learning_capability_evolution_steward_run_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    query: web::Query<LearningScopeQuery>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let scope = match api.resolve_scope(&req, query.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match api
        .store
        .read_capability_evolution_steward_run_report(&scope, path.as_str())
    {
        Ok(report) => HttpResponse::Ok().json(report),
        Err(error) => HttpResponse::NotFound().json(json!({
            "error": error.to_string()
        })),
    }
}

pub async fn run_learning_capability_evolution_steward_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    body: web::Json<RunLearningCapabilityEvolutionStewardRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let report = run_capability_evolution_steward_cycle(&api, scope, request).await;
    if let Err(error) = api
        .store
        .write_capability_evolution_steward_run_report(&report)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string(),
            "report": report
        }));
    }
    let status = report.status.clone();
    let response = json!({
        "run": report
    });
    match status {
        LearningCapabilityEvolutionStewardRunStatus::Completed => HttpResponse::Ok().json(response),
        LearningCapabilityEvolutionStewardRunStatus::Failed => {
            HttpResponse::InternalServerError().json(response)
        },
    }
}

pub async fn upsert_learning_capability_evolution_proposal_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<UpsertLearningCapabilityEvolutionProposalRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let mut backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if !capability_backlog_accepts_proposal_upsert(&backlog.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}` and cannot accept draft proposal updates",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let existing = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => Some(proposal),
        Err(error) if api.store.error_is_not_found(&error) => None,
        Err(error) => {
            return HttpResponse::InternalServerError().json(json!({
                "error": format!(
                    "failed to read existing capability-evolution proposal before update: {error}"
                )
            }));
        },
    };
    if let Some(existing) = existing.as_ref() {
        if proposal_status_requires_decision_endpoint(&existing.status) {
            return HttpResponse::BadRequest().json(json!({
                "error": format!(
                    "capability-evolution proposal `{}` is {} and cannot be edited through the generic upsert endpoint",
                    existing.id,
                    existing.status.as_str()
                )
            }));
        }
    }
    let requested_status = request.status.clone();
    if let Some(status) = requested_status.as_ref() {
        if proposal_status_requires_decision_endpoint(status) {
            return HttpResponse::BadRequest().json(json!({
                "error": format!(
                    "capability-evolution proposal status `{}` requires a dedicated review decision endpoint",
                    status.as_str()
                )
            }));
        }
    }
    let now = Utc::now();
    let proposal = LearningCapabilityEvolutionProposal {
        id: existing
            .as_ref()
            .map(|proposal| proposal.id.clone())
            .unwrap_or_else(|| format!("lcep_{}", candidate_id.trim_start_matches("lc_"))),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        backlog_id: backlog.id.clone(),
        status: requested_status
            .or_else(|| existing.as_ref().map(|proposal| proposal.status.clone()))
            .unwrap_or(LearningCapabilityEvolutionProposalStatus::Draft),
        title: optional_non_empty(request.title)
            .or_else(|| existing.as_ref().map(|proposal| proposal.title.clone()))
            .unwrap_or_else(|| format!("Review fix proposal: {}", backlog.title)),
        summary: optional_non_empty(request.summary)
            .or_else(|| existing.as_ref().map(|proposal| proposal.summary.clone()))
            .unwrap_or_else(|| backlog.summary.clone()),
        capability_id: request
            .capability_id
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|proposal| proposal.capability_id.clone())
            })
            .or_else(|| backlog.capability_id.clone()),
        proposed_fix_type: request
            .proposed_fix_type
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|proposal| proposal.proposed_fix_type.clone())
            })
            .or_else(|| backlog.proposed_fix_type.clone()),
        proposed_files: if request.proposed_files.is_empty() {
            existing
                .as_ref()
                .map(|proposal| proposal.proposed_files.clone())
                .filter(|files| !files.is_empty())
                .unwrap_or_else(|| backlog.proposed_files.clone())
        } else {
            request.proposed_files
        },
        change_plan: request
            .change_plan
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|proposal| proposal.change_plan.clone())
            })
            .unwrap_or_else(|| default_capability_change_plan(&backlog)),
        patches: if request.patches.is_empty() {
            existing
                .as_ref()
                .map(|proposal| proposal.patches.clone())
                .unwrap_or_default()
        } else {
            request.patches
        },
        eval_plan: request
            .eval_plan
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|proposal| proposal.eval_plan.clone())
            })
            .or_else(|| backlog.required_eval.clone()),
        validation_plan: request.validation_plan.or_else(|| {
            existing
                .as_ref()
                .and_then(|proposal| proposal.validation_plan.clone())
                .or_else(|| default_capability_validation_plan(&backlog))
        }),
        promotion_gate: request
            .promotion_gate
            .or_else(|| {
                existing
                    .as_ref()
                    .and_then(|proposal| proposal.promotion_gate.clone())
            })
            .or_else(|| backlog.promotion_gate.clone()),
        generated_by: optional_non_empty(request.generated_by)
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|proposal| proposal.generated_by.clone())
            })
            .unwrap_or_else(|| "learning_capability_evolution_proposal_api".to_string()),
        created_at: existing
            .as_ref()
            .map(|proposal| proposal.created_at)
            .unwrap_or(now),
        updated_at: now,
    };
    if let Err(error) = api.store.write_capability_evolution_proposal(&proposal) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    if matches!(
        backlog.status,
        LearningCapabilityEvolutionBacklogStatus::Queued
    ) {
        backlog.status = LearningCapabilityEvolutionBacklogStatus::InReview;
        backlog.updated_at = now;
        if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
    }
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_proposal_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution proposal `{}` recorded for candidate `{}`.",
                proposal.id, candidate_id
            ),
            evidence_refs: backlog.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "backlog_id": backlog.id.clone(),
                "proposal_id": proposal.id.clone(),
                "status": proposal.status.as_str(),
                "capability_id": proposal.capability_id.clone(),
                "proposed_fix_type": proposal.proposed_fix_type.clone(),
                "proposed_files": proposal.proposed_files.clone(),
                "patch_count": proposal.patches.len()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    HttpResponse::Ok().json(proposal)
}

pub async fn decide_learning_capability_evolution_proposal_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<DecideLearningCapabilityEvolutionProposalRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    if !proposal_status_requires_decision_endpoint(&request.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal status `{}` is not a review decision status",
                request.status.as_str()
            )
        }));
    }
    let reason = request.reason.trim().to_string();
    if reason.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "review decision reason is required"
        }));
    }
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "review decision actor is required"
        }));
    }
    let mut proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    let mut backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if let Some(error) = validate_proposal_review_transition(&proposal.status, &request.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": error
        }));
    }
    if !capability_backlog_accepts_review_decision(
        &backlog.status,
        &proposal.status,
        &request.status,
    ) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is already implemented and cannot accept review decision updates",
                backlog.id
            )
        }));
    }
    let target_candidate_state = proposal_decision_candidate_state(&request.status);
    if candidate.state.is_terminal()
        && !candidate_satisfies_proposal_decision(&candidate.state, &target_candidate_state)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot be marked `{}` by proposal review",
                candidate.id,
                candidate.state.as_str(),
                target_candidate_state.as_str()
            )
        }));
    }
    let high_risk_review_evidence_required = request.status
        == LearningCapabilityEvolutionProposalStatus::Approved
        && skill_evolution_review_evidence_required(&candidate.risk_level, &backlog.risk_level);
    if high_risk_review_evidence_required
        && !skill_evolution_review_has_material_evidence(&request.evidence_refs, &request.payload)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "high-risk skill evolution approval requires explicit review evidence: provide an evidence_ref or non-empty review payload"
        }));
    }
    if request.status == LearningCapabilityEvolutionProposalStatus::Approved
        && skill_evolution_approval_requires_eval_plan(&proposal)
        && !skill_evolution_proposal_has_eval_plan(&proposal)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "scoped skill evolution approval requires a non-empty eval_plan on the proposal"
        }));
    }
    if request.status == LearningCapabilityEvolutionProposalStatus::Approved
        && skill_evolution_approval_requires_promotion_gate(&proposal)
        && !skill_evolution_proposal_has_promotion_gate(&proposal)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "scoped skill evolution approval requires a non-empty promotion_gate on the proposal"
        }));
    }

    let previous_proposal_status = proposal.status.clone();
    let previous_backlog_status = backlog.status.clone();
    let now = Utc::now();
    proposal.status = request.status.clone();
    proposal.updated_at = now;
    backlog.status = proposal_decision_backlog_status(&request.status, &backlog.status);
    backlog.updated_at = now;

    if let Err(error) = api.store.write_capability_evolution_proposal(&proposal) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let candidate =
        if candidate_satisfies_proposal_decision(&candidate.state, &target_candidate_state) {
            candidate
        } else {
            match api.store.transition_candidate(
                &scope,
                &candidate_id,
                target_candidate_state.clone(),
                actor.clone(),
                format!("capability_proposal_{}", request.status.as_str()),
                reason.clone(),
                request.evidence_refs.clone(),
            ) {
                Ok(candidate) => candidate,
                Err(error) => {
                    return HttpResponse::BadRequest().json(json!({
                        "error": error.to_string()
                    }));
                },
            }
        };

    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_proposal_decided".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution proposal `{}` was marked {} by {}.",
                proposal.id,
                proposal.status.as_str(),
                actor
            ),
            evidence_refs: request.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "backlog_id": backlog.id.clone(),
                "proposal_id": proposal.id.clone(),
                "actor": actor,
                "reason": reason,
                "from_proposal_status": previous_proposal_status.as_str(),
                "to_proposal_status": proposal.status.as_str(),
                "from_backlog_status": previous_backlog_status.as_str(),
                "to_backlog_status": backlog.status.as_str(),
                "to_candidate_state": candidate.state.as_str(),
                "decision_payload": request.payload
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "proposal": proposal,
        "backlog": backlog,
        "candidate": candidate
    }))
}

pub async fn generate_learning_capability_evolution_evaluation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<GenerateLearningCapabilityEvolutionEvaluationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "evaluation generation actor is required"
        }));
    }
    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal_status_is_terminal(&proposal.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}` and cannot generate evaluation backlog items",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot generate evaluation backlog items",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }
    let existing = api
        .store
        .read_evaluation_backlog_item(&scope, &candidate_id)
        .ok();
    if existing.is_some() && !request.overwrite {
        return HttpResponse::Conflict().json(json!({
            "error": format!(
                "evaluation backlog item for candidate `{candidate_id}` already exists; set overwrite=true to replace it"
            ),
            "evaluation": existing
        }));
    }

    let generated = build_capability_evolution_evaluation_backlog_item(
        &scope,
        &candidate,
        &backlog,
        &proposal,
        existing.as_ref(),
        request.payload.clone(),
    );
    if let Err(error) = api.store.write_evaluation_backlog_item(&generated) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let backlog_path = api
        .store
        .workspace_layout()
        .learning_evaluation_backlog_path(&scope.principal, &scope.workspace, &candidate_id);
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_eval_case_generated".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution proposal `{}` generated evaluation backlog item `{}`.",
                proposal.id, generated.id
            ),
            evidence_refs: generated.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "backlog_id": backlog.id.clone(),
                "evaluation_backlog_id": generated.id.clone(),
                "evaluation_backlog_path": backlog_path.display().to_string(),
                "actor": actor,
                "overwrite": request.overwrite,
                "case_kind": generated.case_kind.clone(),
                "priority": generated.priority.clone(),
                "capability_id": proposal.capability_id.clone(),
                "request_payload": request.payload
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "evaluation": generated,
        "evaluation_backlog_path": backlog_path.display().to_string(),
        "proposal": proposal,
        "backlog": backlog,
        "candidate": candidate
    }))
}

pub async fn record_learning_capability_evolution_validation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RecordLearningCapabilityEvolutionValidationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let summary = request.summary.trim().to_string();
    if summary.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "validation summary is required"
        }));
    }
    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; validation reports require an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let mut backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if !capability_backlog_accepts_validation(&backlog.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}` and cannot accept validation reports",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot accept capability-evolution validation reports",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let now = Utc::now();
    let runner = optional_non_empty(request.runner)
        .unwrap_or_else(|| "capability_evolution_validation_api".to_string());
    let commands = request.commands;
    let evidence_refs = request.evidence_refs;
    let request_metrics = request.metrics;
    let request_payload = request.payload;
    if request.status == LearningCapabilityEvolutionValidationStatus::Passed
        && !capability_validation_has_material_evidence(
            &commands,
            &evidence_refs,
            &request_metrics,
            &request_payload,
        )
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "passed validation requires material evidence: provide at least one command, evidence_ref, non-empty metrics, or non-empty payload"
        }));
    }
    let existing_eval_backlog = api
        .store
        .read_evaluation_backlog_item(&scope, &candidate_id)
        .ok();
    let eval_backlog = build_capability_evolution_evaluation_backlog_item(
        &scope,
        &candidate,
        &backlog,
        &proposal,
        existing_eval_backlog.as_ref(),
        json!({
            "source": "capability_evolution_validation_api",
            "runner": runner.clone()
        }),
    );
    if let Err(error) = api.store.write_evaluation_backlog_item(&eval_backlog) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let eval_backlog_path = api
        .store
        .workspace_layout()
        .learning_evaluation_backlog_path(&scope.principal, &scope.workspace, &candidate_id);
    let phase5_fixture_cases = phase5_fixture_cases_from_proposal(&proposal);
    let phase5_fixture_results =
        phase5_fixture_results_from_payload(&phase5_fixture_cases, &request_payload);
    let phase5_fixture_metrics =
        phase5_fixture_manual_metrics(&phase5_fixture_cases, &phase5_fixture_results);
    let metrics = phase5_metrics_with_fixture(request_metrics, phase5_fixture_metrics);
    let payload = if request_payload.is_null() {
        json!({
            "evaluation_backlog_id": eval_backlog.id.clone(),
            "evaluation_backlog_path": eval_backlog_path.display().to_string(),
            "phase5_fixture_cases": phase5_fixture_cases,
            "phase5_fixture_results": phase5_fixture_results
        })
    } else {
        json!({
            "evaluation_backlog_id": eval_backlog.id.clone(),
            "evaluation_backlog_path": eval_backlog_path.display().to_string(),
            "request_payload": request_payload,
            "phase5_fixture_cases": phase5_fixture_cases,
            "phase5_fixture_results": phase5_fixture_results
        })
    };
    let mut report = LearningCapabilityEvolutionValidationReport {
        id: format!("lcev_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        status: request.status.clone(),
        capability_id: proposal.capability_id.clone(),
        runner,
        summary,
        commands,
        evidence_refs,
        metrics,
        payload,
        created_at: now,
    };

    if let Err(error) = api
        .store
        .write_capability_evolution_validation_report(&report)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let previous_backlog_status = backlog.status.clone();
    let candidate = if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
        backlog.status = LearningCapabilityEvolutionBacklogStatus::Validated;
        backlog.updated_at = now;
        if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
        if matches!(
            candidate.state,
            LearningCandidateState::Evaluated | LearningCandidateState::Implemented
        ) {
            candidate
        } else {
            match api.store.transition_candidate(
                &scope,
                &candidate_id,
                LearningCandidateState::Evaluated,
                report.runner.clone(),
                "capability_validation_passed".to_string(),
                "Approved capability-evolution proposal passed validation.".to_string(),
                report.evidence_refs.clone(),
            ) {
                Ok(candidate) => candidate,
                Err(error) => {
                    return HttpResponse::BadRequest().json(json!({
                        "error": error.to_string()
                    }));
                },
            }
        }
    } else {
        candidate
    };

    let (rollback_recommendation, rollback_recommendation_error) =
        phase6_record_validation_rollback_recommendation_outcome(
            &api,
            &scope,
            &proposal,
            &mut report,
        );
    if rollback_recommendation_error.is_some() {
        if let Err(error) = api
            .store
            .write_capability_evolution_validation_report(&report)
        {
            return HttpResponse::InternalServerError().json(json!({
                "error": format!(
                    "failed to persist rollback recommendation error on validation report `{}`: {error}",
                    report.id
                ),
                "validation": report,
                "rollback_recommendation_error": rollback_recommendation_error
            }));
        }
    }

    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_validation_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution validation `{}` recorded as {} for proposal `{}`.",
                report.id,
                report.status.as_str(),
                proposal.id
            ),
            evidence_refs: report.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": report.id.clone(),
                "validation_status": report.status.as_str(),
                "capability_id": proposal.capability_id.clone(),
                "evaluation_backlog_id": eval_backlog.id.clone(),
                "evaluation_backlog_path": eval_backlog_path.display().to_string(),
                "from_backlog_status": previous_backlog_status.as_str(),
                "to_backlog_status": backlog.status.as_str(),
                "to_candidate_state": candidate.state.as_str(),
                "runner": report.runner.clone(),
                "command_count": report.commands.len(),
                "phase6_rollback_recommendation": rollback_recommendation
                    .as_ref()
                    .map(phase6_rollback_recommendation_summary),
                "phase6_rollback_recommendation_error": rollback_recommendation_error.clone()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "validation": report,
        "proposal": proposal,
        "backlog": backlog,
        "candidate": candidate,
        "rollback_recommendation": rollback_recommendation,
        "rollback_recommendation_error": rollback_recommendation_error
    }))
}

pub async fn run_learning_capability_evolution_validation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RunLearningCapabilityEvolutionValidationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; validation runner requires an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let mut backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if !capability_backlog_accepts_validation(&backlog.status) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}` and cannot run validation",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot run capability-evolution validation",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let include_regression = request.include_regression.unwrap_or(true);
    let command_specs =
        planned_validation_commands(&proposal, &request.commands, include_regression);
    let command_strings: Vec<String> = command_specs
        .iter()
        .map(|spec| spec.command.clone())
        .collect();
    let existing_eval_backlog = api
        .store
        .read_evaluation_backlog_item(&scope, &candidate_id)
        .ok();
    let eval_backlog_preview = build_capability_evolution_evaluation_backlog_item(
        &scope,
        &candidate,
        &backlog,
        &proposal,
        existing_eval_backlog.as_ref(),
        json!({
            "source": "capability_evolution_validation_runner",
            "dry_run": request.dry_run
        }),
    );
    if request.dry_run {
        return HttpResponse::Ok().json(json!({
            "scope": scope,
            "proposal": proposal,
            "evaluation": eval_backlog_preview,
            "planned_commands": command_specs,
            "include_regression": include_regression,
            "dry_run": true
        }));
    }

    let runner = optional_non_empty(request.runner)
        .unwrap_or_else(|| "capability_evolution_validation_runner".to_string());
    let timeout_seconds = request
        .timeout_seconds
        .unwrap_or(DEFAULT_CAPABILITY_VALIDATION_TIMEOUT_SECONDS)
        .clamp(1, MAX_CAPABILITY_VALIDATION_TIMEOUT_SECONDS);
    let scope_root = api
        .store
        .workspace_layout()
        .scope_root(&scope.principal, &scope.workspace);
    if let Err(error) = api
        .store
        .write_evaluation_backlog_item(&eval_backlog_preview)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let eval_backlog_path = api
        .store
        .workspace_layout()
        .learning_evaluation_backlog_path(&scope.principal, &scope.workspace, &candidate_id);
    let started_at = Utc::now();
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "capability_evolution_eval_started".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution validation runner started for proposal `{}`.",
                proposal.id
            ),
            evidence_refs: backlog.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "runner": runner.clone(),
                "command_count": command_specs.len(),
                "include_regression": include_regression,
                "timeout_seconds": timeout_seconds,
                "scope_root": scope_root.display().to_string(),
                "evaluation_backlog_id": eval_backlog_preview.id.clone(),
                "evaluation_backlog_path": eval_backlog_path.display().to_string()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let mut results = Vec::new();
    for spec in command_specs
        .iter()
        .take(MAX_CAPABILITY_VALIDATION_COMMANDS)
    {
        results.push(run_validation_command(spec, &scope_root, timeout_seconds).await);
    }
    let command_count = command_specs.len().min(MAX_CAPABILITY_VALIDATION_COMMANDS);
    let passed_count = results
        .iter()
        .filter(|result| result.success && !result.timed_out && result.spawn_error.is_none())
        .count();
    let failed_count = command_count.saturating_sub(passed_count);
    let regression_checked = command_specs.iter().any(|spec| spec.regression);
    let phase5_fixture_cases = phase5_fixture_cases_from_proposal(&proposal);
    let phase5_fixture_results =
        phase5_fixture_results_for_commands(&phase5_fixture_cases, &results);
    let phase5_fixture_metrics =
        phase5_fixture_result_metrics(&phase5_fixture_cases, &phase5_fixture_results);
    let status = if command_specs.is_empty() {
        LearningCapabilityEvolutionValidationStatus::Blocked
    } else if failed_count == 0 {
        LearningCapabilityEvolutionValidationStatus::Passed
    } else {
        LearningCapabilityEvolutionValidationStatus::Failed
    };
    let summary = optional_non_empty(request.summary).unwrap_or_else(|| {
        if command_specs.is_empty() {
            "Validation runner found no executable commands in the proposal eval or validation plan."
                .to_string()
        } else {
            format!(
                "Validation runner executed {command_count} command(s): {passed_count} passed, {failed_count} failed."
            )
        }
    });
    let payload = json!({
        "runner_payload": request.payload,
        "started_at": started_at,
        "ended_at": Utc::now(),
        "scope_root": scope_root.display().to_string(),
        "evaluation_backlog_id": eval_backlog_preview.id,
        "evaluation_backlog_path": eval_backlog_path.display().to_string(),
        "include_regression": include_regression,
        "regression_checked": regression_checked,
        "max_commands": MAX_CAPABILITY_VALIDATION_COMMANDS,
        "timeout_seconds": timeout_seconds,
        "planned_commands": command_specs,
        "results": results,
        "phase5_fixture_cases": phase5_fixture_cases,
        "phase5_fixture_results": phase5_fixture_results
    });
    let metrics = json!({
        "command_count": command_count,
        "passed_count": passed_count,
        "failed_count": failed_count,
        "regression_checked": regression_checked,
        "phase5_fixture": phase5_fixture_metrics
    });
    let mut report = LearningCapabilityEvolutionValidationReport {
        id: format!("lcev_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        status: status.clone(),
        capability_id: proposal.capability_id.clone(),
        runner: runner.clone(),
        summary,
        commands: command_strings,
        evidence_refs: backlog.evidence_refs.clone(),
        metrics,
        payload,
        created_at: Utc::now(),
    };
    if status == LearningCapabilityEvolutionValidationStatus::Passed
        && !capability_validation_has_material_evidence(
            &report.commands,
            &report.evidence_refs,
            &report.metrics,
            &report.payload,
        )
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "passed validation requires material evidence: validation runner produced no material evidence"
        }));
    }
    if let Err(error) = api
        .store
        .write_capability_evolution_validation_report(&report)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    let previous_backlog_status = backlog.status.clone();
    let candidate = if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
        backlog.status = LearningCapabilityEvolutionBacklogStatus::Validated;
        backlog.updated_at = report.created_at;
        if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
        if matches!(
            candidate.state,
            LearningCandidateState::Evaluated | LearningCandidateState::Implemented
        ) {
            candidate
        } else {
            match api.store.transition_candidate(
                &scope,
                &candidate_id,
                LearningCandidateState::Evaluated,
                report.runner.clone(),
                "capability_validation_passed".to_string(),
                "Approved capability-evolution proposal passed automated validation.".to_string(),
                report.evidence_refs.clone(),
            ) {
                Ok(candidate) => candidate,
                Err(error) => {
                    return HttpResponse::BadRequest().json(json!({
                        "error": error.to_string()
                    }));
                },
            }
        }
    } else {
        candidate
    };

    let (rollback_recommendation, rollback_recommendation_error) =
        phase6_record_validation_rollback_recommendation_outcome(
            &api,
            &scope,
            &proposal,
            &mut report,
        );
    if rollback_recommendation_error.is_some() {
        if let Err(error) = api
            .store
            .write_capability_evolution_validation_report(&report)
        {
            return HttpResponse::InternalServerError().json(json!({
                "error": format!(
                    "failed to persist rollback recommendation error on validation report `{}`: {error}",
                    report.id
                ),
                "validation": report,
                "rollback_recommendation_error": rollback_recommendation_error
            }));
        }
    }

    for event_type in [
        "capability_evolution_eval_completed",
        "learning_capability_validation_recorded",
    ] {
        if let Err(error) = api.store.append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: event_type.to_string(),
                agent_id: backlog.source_agent_id.clone(),
                task_id: backlog.source_task_id.clone(),
                execution_id: backlog.source_execution_id.clone(),
                chat_session_id: backlog.source_chat_session_id.clone(),
                summary: format!(
                    "Capability-evolution validation `{}` completed as {} for proposal `{}`.",
                    report.id,
                    report.status.as_str(),
                    proposal.id
                ),
                evidence_refs: report.evidence_refs.clone(),
                payload: json!({
                    "candidate_id": candidate_id.clone(),
                    "proposal_id": proposal.id.clone(),
                    "validation_id": report.id.clone(),
                    "validation_status": report.status.as_str(),
                    "capability_id": proposal.capability_id.clone(),
                    "from_backlog_status": previous_backlog_status.as_str(),
                    "to_backlog_status": backlog.status.as_str(),
                    "to_candidate_state": candidate.state.as_str(),
                    "runner": report.runner.clone(),
                    "command_count": report.commands.len(),
                    "passed_count": passed_count,
                    "failed_count": failed_count,
                    "regression_checked": regression_checked,
                    "phase6_rollback_recommendation": rollback_recommendation
                        .as_ref()
                        .map(phase6_rollback_recommendation_summary),
                    "phase6_rollback_recommendation_error": rollback_recommendation_error.clone()
                }),
            },
        ) {
            return HttpResponse::InternalServerError().json(json!({
                "error": error.to_string()
            }));
        }
    }

    HttpResponse::Ok().json(json!({
        "validation": report,
        "proposal": proposal,
        "backlog": backlog,
        "candidate": candidate,
        "rollback_recommendation": rollback_recommendation,
        "rollback_recommendation_error": rollback_recommendation_error
    }))
}

pub async fn draft_learning_capability_evolution_implementation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<DraftLearningCapabilityEvolutionImplementationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "implementation draft actor is required"
        }));
    }

    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; implementation drafts require an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}`; implementation drafts require validated backlog state",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot accept capability-evolution implementation drafts",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let validation_id = optional_non_empty(request.validation_id);
    let validation = match validation_id {
        Some(validation_id) => match api.store.read_capability_evolution_validation_report(
            &scope,
            &candidate_id,
            &validation_id,
        ) {
            Ok(report) => report,
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        },
        None => match api.store.list_capability_evolution_validation_reports(
            &scope,
            LearningCapabilityEvolutionValidationFilters {
                status: Some(
                    LearningCapabilityEvolutionValidationStatus::Passed
                        .as_str()
                        .to_string(),
                ),
                candidate_id: Some(candidate_id.clone()),
                capability_id: proposal.capability_id.clone(),
                limit: Some(1),
            },
        ) {
            Ok(mut reports) => {
                let Some(report) = reports.pop() else {
                    return HttpResponse::BadRequest().json(json!({
                        "error": "implementation drafts require at least one passed validation report"
                    }));
                };
                report
            },
            Err(error) => {
                return HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                }));
            },
        },
    };
    if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` is `{}`; implementation drafts require passed validation",
                validation.id,
                validation.status.as_str()
            )
        }));
    }
    if validation.candidate_id != candidate_id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to candidate `{}` instead of `{}`",
                validation.id,
                validation.candidate_id,
                candidate_id
            )
        }));
    }
    if validation.proposal_id != proposal.id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to proposal `{}` instead of `{}`",
                validation.id,
                validation.proposal_id,
                proposal.id
            )
        }));
    }

    if !request.overwrite {
        if let Some(existing) = latest_implementation(api.get_ref(), &scope, &backlog, &proposal) {
            return HttpResponse::Ok().json(json!({
                "drafted": false,
                "reason": "implementation_bundle_already_exists",
                "implementation": existing,
                "proposal": proposal,
                "validation": validation,
                "backlog": backlog,
                "candidate": candidate
            }));
        }
    }

    let record = match draft_capability_evolution_implementation_bundle_record(
        &scope,
        &actor,
        &backlog,
        &proposal,
        &validation,
        request.summary.as_deref(),
        request.payload,
    ) {
        Ok(record) => record,
        Err(error) => {
            return HttpResponse::BadRequest().json(json!({
                "error": error
            }));
        },
    };
    if let Err(error) = api
        .store
        .write_capability_evolution_implementation_record(&record)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_implementation_drafted".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Skill Evolution implementation bundle `{}` drafted by {} for proposal `{}`.",
                record.id, actor, proposal.id
            ),
            evidence_refs: record.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": validation.id.clone(),
                "implementation_id": record.id.clone(),
                "capability_id": proposal.capability_id.clone(),
                "actor": actor,
                "applied_files": record.applied_files.clone(),
                "patch_count": record.patches.len()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "drafted": true,
        "implementation": record,
        "proposal": proposal,
        "validation": validation,
        "backlog": backlog,
        "candidate": candidate
    }))
}

pub async fn record_learning_capability_evolution_implementation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RecordLearningCapabilityEvolutionImplementationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "implementation actor is required"
        }));
    }
    let summary = request.summary.trim().to_string();
    if summary.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "implementation summary is required"
        }));
    }

    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; implementation records require an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}`; implementation records require validated backlog state",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot accept capability-evolution implementation records",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let validation_id = optional_non_empty(request.validation_id);
    let validation = match validation_id {
        Some(validation_id) => match api.store.read_capability_evolution_validation_report(
            &scope,
            &candidate_id,
            &validation_id,
        ) {
            Ok(report) => report,
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        },
        None => match api.store.list_capability_evolution_validation_reports(
            &scope,
            LearningCapabilityEvolutionValidationFilters {
                status: Some(
                    LearningCapabilityEvolutionValidationStatus::Passed
                        .as_str()
                        .to_string(),
                ),
                candidate_id: Some(candidate_id.clone()),
                capability_id: proposal.capability_id.clone(),
                limit: Some(1),
            },
        ) {
            Ok(mut reports) => {
                let Some(report) = reports.pop() else {
                    return HttpResponse::BadRequest().json(json!({
                        "error": "implementation records require at least one passed validation report"
                    }));
                };
                report
            },
            Err(error) => {
                return HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                }));
            },
        },
    };
    if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` is `{}`; implementation records require passed validation",
                validation.id,
                validation.status.as_str()
            )
        }));
    }
    if validation.candidate_id != candidate_id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to candidate `{}` instead of `{}`",
                validation.id,
                validation.candidate_id,
                candidate_id
            )
        }));
    }
    if validation.proposal_id != proposal.id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to proposal `{}` instead of `{}`",
                validation.id,
                validation.proposal_id,
                proposal.id
            )
        }));
    }

    let request_supplied_applied_files = !request.applied_files.is_empty();
    let applied_files = if request.applied_files.is_empty() {
        proposal.proposed_files.clone()
    } else {
        request.applied_files
    };
    let patches = if request.patches.is_empty() {
        proposal.patches.clone()
    } else {
        request.patches
    };
    let evidence_refs = request.evidence_refs;
    let payload = request.payload;
    if !request_supplied_applied_files
        && patches.is_empty()
        && evidence_refs.is_empty()
        && payload.is_null()
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "implementation evidence is required; provide explicit applied_files, patches, evidence_refs, or a non-null payload"
        }));
    }

    let record = LearningCapabilityEvolutionImplementationRecord {
        id: format!("lceimpl_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        validation_id: validation.id.clone(),
        capability_id: proposal.capability_id.clone(),
        actor: actor.clone(),
        summary: summary.clone(),
        applied_files,
        patches,
        evidence_refs,
        payload,
        created_at: Utc::now(),
    };
    if let Err(error) = api
        .store
        .write_capability_evolution_implementation_record(&record)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_implementation_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution implementation `{}` recorded by {} for proposal `{}`.",
                record.id, actor, proposal.id
            ),
            evidence_refs: record.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": validation.id.clone(),
                "implementation_id": record.id.clone(),
                "capability_id": proposal.capability_id.clone(),
                "actor": actor,
                "applied_files": record.applied_files.clone(),
                "patch_count": record.patches.len()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "implementation": record,
        "proposal": proposal,
        "validation": validation,
        "backlog": backlog,
        "candidate": candidate
    }))
}

pub async fn apply_learning_capability_evolution_implementation_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<(String, String)>,
    body: web::Json<ApplyLearningCapabilityEvolutionImplementationRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let (candidate_id, implementation_id) = path.into_inner();
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "application actor is required"
        }));
    }
    let summary = optional_non_empty(request.summary).unwrap_or_else(|| {
        if request.apply {
            format!("Applied implementation bundle `{implementation_id}`.")
        } else {
            format!("Prepared dry-run for implementation bundle `{implementation_id}`.")
        }
    });

    let implementation = match api.store.read_capability_evolution_implementation_record(
        &scope,
        &candidate_id,
        &implementation_id,
    ) {
        Ok(record) => record,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if implementation.candidate_id != candidate_id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "implementation record `{}` belongs to candidate `{}` instead of `{}`",
                implementation.id,
                implementation.candidate_id,
                candidate_id
            )
        }));
    }

    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; implementation application requires an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    if implementation.proposal_id != proposal.id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "implementation record `{}` belongs to proposal `{}` instead of `{}`",
                implementation.id,
                implementation.proposal_id,
                proposal.id
            )
        }));
    }

    let backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}`; implementation application requires validated backlog state",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot accept capability-evolution implementation application",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let validation = match api.store.read_capability_evolution_validation_report(
        &scope,
        &candidate_id,
        &implementation.validation_id,
    ) {
        Ok(report) => report,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` is `{}`; implementation application requires passed validation",
                validation.id,
                validation.status.as_str()
            )
        }));
    }
    if validation.candidate_id != candidate_id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to candidate `{}` instead of `{}`",
                validation.id,
                validation.candidate_id,
                candidate_id
            )
        }));
    }
    if validation.proposal_id != proposal.id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to proposal `{}` instead of `{}`",
                validation.id,
                validation.proposal_id,
                proposal.id
            )
        }));
    }

    let file_changes = match extract_capability_application_file_changes(&implementation) {
        Ok(changes) => changes,
        Err(error) => {
            return HttpResponse::BadRequest().json(json!({
                "error": error
            }));
        },
    };
    let target_surface = request
        .target_surface
        .unwrap_or(LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill);
    // The system-shared skills tier was retired (v0.6.572) — writing
    // capability-evolution applications to `<data_root>/system/skills/`
    // would silently misfire (catalog walks workspace + extras, never
    // system). Reject the targeting up-front instead of letting the
    // write land in a tree nothing loads.
    if matches!(
        target_surface,
        LearningCapabilityEvolutionApplicationTargetSurface::SystemSkill
    ) {
        return HttpResponse::BadRequest().json(json!({
            "error": "target_surface=`system_skill` is no longer supported — the system-shared skills tier was retired. Use `scoped_skill` (writes to <scope>/skills/) or `source_skill` (writes to skillshub/) instead."
        }));
    }
    let target_root = capability_application_target_root(
        api.store.workspace_layout(),
        &api.repo_root,
        &scope,
        target_surface,
    );
    let prepared_changes =
        match prepare_capability_file_changes(&target_root, target_surface, &file_changes) {
            Ok(prepared_changes) => prepared_changes,
            Err(error) => {
                return HttpResponse::BadRequest().json(json!({
                    "error": error
                }));
            },
        };
    let changed_files = prepared_changes
        .iter()
        .map(|change| change.audit.clone())
        .collect::<Vec<_>>();
    let mode = if request.apply {
        LearningCapabilityEvolutionApplicationMode::Apply
    } else {
        LearningCapabilityEvolutionApplicationMode::DryRun
    };
    let request_payload = request.payload;
    let phase6_rollback_snapshot =
        phase6_rollback_snapshot(&changed_files, target_surface, &target_root);
    let mut record = LearningCapabilityEvolutionApplicationRecord {
        id: format!("lceapp_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        validation_id: validation.id.clone(),
        implementation_id: implementation.id.clone(),
        capability_id: proposal.capability_id.clone(),
        actor: actor.clone(),
        summary: summary.clone(),
        mode,
        status: LearningCapabilityEvolutionApplicationStatus::Prepared,
        changed_files,
        evidence_refs: request.evidence_refs,
        payload: if request_payload.is_null() {
            json!({
                "target_surface": target_surface.as_str(),
                "target_surface_description": target_surface.description(),
                "target_root": target_root.display().to_string(),
                "phase6_rollback_snapshot": phase6_rollback_snapshot
            })
        } else {
            json!({
                "target_surface": target_surface.as_str(),
                "target_surface_description": target_surface.description(),
                "target_root": target_root.display().to_string(),
                "phase6_rollback_snapshot": phase6_rollback_snapshot,
                "request_payload": request_payload
            })
        },
        created_at: Utc::now(),
    };
    if let Err(error) = api
        .store
        .write_capability_evolution_application_record(&record)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let mut rollback_recommendation = None;
    if request.apply {
        if let Err(error) = apply_prepared_capability_file_changes(&prepared_changes) {
            return HttpResponse::BadRequest().json(json!({
                "error": error,
                "application": record
            }));
        }
        record.status = LearningCapabilityEvolutionApplicationStatus::Applied;
        let catalog_refresh = refresh_runtime_skill_catalog_after_application(
            api.store.workspace_layout(),
            &api.repo_root,
            &scope,
            target_surface,
            &record,
        );
        merge_application_payload_field(
            &mut record.payload,
            "runtime_catalog_refresh",
            catalog_refresh,
        );
        if application_record_has_failed_catalog_refresh(&record) {
            match phase6_record_rollback_recommendation(
                &api,
                &scope,
                &record,
                Phase6RollbackRecommendationTrigger {
                    kind: "catalog_refresh_failed",
                    severity: "high",
                    source_id: record.id.clone(),
                    actor: actor.clone(),
                    summary: format!(
                        "Runtime skill catalog refresh failed after applying `{}`; rollback should be reviewed before promotion.",
                        record.id
                    ),
                    validation_id: Some(validation.id.clone()),
                    promotion_id: None,
                    evidence_refs: record.evidence_refs.clone(),
                    payload: json!({
                        "runtime_catalog_refresh": record.payload.get("runtime_catalog_refresh").cloned(),
                        "phase6_rollback_snapshot": record.payload.get("phase6_rollback_snapshot").cloned()
                    }),
                },
            ) {
                Ok(recommendation) => {
                    if let Some(recommendation) = recommendation {
                        merge_application_payload_field(
                            &mut record.payload,
                            "phase6_rollback_recommendation",
                            phase6_rollback_recommendation_summary(&recommendation),
                        );
                        rollback_recommendation = Some(recommendation);
                    }
                },
                Err(error) => {
                    merge_application_payload_field(
                        &mut record.payload,
                        "phase6_rollback_recommendation_error",
                        json!(error),
                    );
                },
            }
        }
        if let Err(error) = api
            .store
            .write_capability_evolution_application_record(&record)
        {
            let rollback_errors = rollback_prepared_capability_file_changes(&prepared_changes);
            record.status = LearningCapabilityEvolutionApplicationStatus::Prepared;
            return HttpResponse::InternalServerError().json(json!({
                "error": format!(
                    "applied files but failed to update application record `{}` to applied: {error}{}",
                    record.id,
                    format_rollback_errors(&rollback_errors)
                ),
                "application": record
            }));
        }
    }
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_application_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution application `{}` recorded as {} for implementation `{}`.",
                record.id,
                record.status.as_str(),
                implementation.id
            ),
            evidence_refs: record.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": validation.id.clone(),
                "implementation_id": implementation.id.clone(),
                "application_id": record.id.clone(),
                "capability_id": proposal.capability_id.clone(),
                "mode": record.mode.as_str(),
                "status": record.status.as_str(),
                "target_surface": target_surface.as_str(),
                "target_root": target_root.display().to_string(),
                "runtime_catalog_refresh": record.payload.get("runtime_catalog_refresh").cloned(),
                "phase6_rollback_snapshot": record
                    .payload
                    .get("phase6_rollback_snapshot")
                    .cloned(),
                "phase6_rollback_recommendation": rollback_recommendation
                    .as_ref()
                    .map(phase6_rollback_recommendation_summary),
                "actor": actor,
                "changed_files": record
                    .changed_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>(),
                "changed_file_count": record.changed_files.len()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }

    HttpResponse::Ok().json(json!({
        "application": record,
        "implementation": implementation,
        "proposal": proposal,
        "validation": validation,
        "backlog": backlog,
        "candidate": candidate,
        "rollback_recommendation": rollback_recommendation
    }))
}

pub async fn record_learning_capability_evolution_promotion_handler(
    req: HttpRequest,
    api: Option<web::Data<LearningApi>>,
    path: web::Path<String>,
    body: web::Json<RecordLearningCapabilityEvolutionPromotionRequest>,
) -> HttpResponse {
    let Some(api) = api else {
        return learning_unavailable();
    };
    let request = body.into_inner();
    let scope = match api.resolve_scope(&req, request.workspace.clone()) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    let candidate_id = path.into_inner();
    let actor = request.actor.trim().to_string();
    if actor.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion actor is required"
        }));
    }
    let summary = request.summary.trim().to_string();
    if summary.is_empty() {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion summary is required"
        }));
    }
    let proposal = match api
        .store
        .read_capability_evolution_proposal(&scope, &candidate_id)
    {
        Ok(proposal) => proposal,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution proposal `{}` is `{}`; promotion requires an approved proposal",
                proposal.id,
                proposal.status.as_str()
            )
        }));
    }
    let mut backlog = match api
        .store
        .read_capability_evolution_backlog_item(&scope, &candidate_id)
    {
        Ok(item) => item,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if backlog.status == LearningCapabilityEvolutionBacklogStatus::Implemented {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is already implemented",
                backlog.id
            )
        }));
    }
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "capability-evolution backlog item `{}` is `{}`; promotion requires validated backlog state",
                backlog.id,
                backlog.status.as_str()
            )
        }));
    }
    let candidate = match api.store.read_candidate(&scope, &candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            return HttpResponse::NotFound().json(json!({
                "error": error.to_string()
            }));
        },
    };
    if candidate.state.is_terminal() {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "learning candidate `{}` is terminal in state `{}` and cannot accept capability-evolution promotion records",
                candidate.id,
                candidate.state.as_str()
            )
        }));
    }

    let application = match optional_non_empty(request.application_id) {
        Some(application_id) => match api.store.read_capability_evolution_application_record(
            &scope,
            &candidate_id,
            &application_id,
        ) {
            Ok(record) => {
                if record.candidate_id != candidate_id {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!(
                            "application record `{}` belongs to candidate `{}` instead of `{}`",
                            record.id,
                            record.candidate_id,
                            candidate_id
                        )
                    }));
                }
                if record.proposal_id != proposal.id {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!(
                            "application record `{}` belongs to proposal `{}` instead of `{}`",
                            record.id,
                            record.proposal_id,
                            proposal.id
                        )
                    }));
                }
                if record.status != LearningCapabilityEvolutionApplicationStatus::Applied {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!(
                            "application record `{}` is `{}`; promotion requires an applied application record",
                            record.id,
                            record.status.as_str()
                        )
                    }));
                }
                Some(record)
            },
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        },
        None => None,
    };

    let implementation_id = optional_non_empty(request.implementation_id).or_else(|| {
        application
            .as_ref()
            .map(|record| record.implementation_id.clone())
    });
    let implementation = match implementation_id {
        Some(implementation_id) => match api.store.read_capability_evolution_implementation_record(
            &scope,
            &candidate_id,
            &implementation_id,
        ) {
            Ok(record) => {
                if record.candidate_id != candidate_id {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!(
                            "implementation record `{}` belongs to candidate `{}` instead of `{}`",
                            record.id,
                            record.candidate_id,
                            candidate_id
                        )
                    }));
                }
                if record.proposal_id != proposal.id {
                    return HttpResponse::BadRequest().json(json!({
                        "error": format!(
                            "implementation record `{}` belongs to proposal `{}` instead of `{}`",
                            record.id,
                            record.proposal_id,
                            proposal.id
                        )
                    }));
                }
                Some(record)
            },
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        },
        None => None,
    };

    let validation_id = optional_non_empty(request.validation_id).or_else(|| {
        implementation
            .as_ref()
            .map(|record| record.validation_id.clone())
            .or_else(|| {
                application
                    .as_ref()
                    .map(|record| record.validation_id.clone())
            })
    });
    let validation = match validation_id {
        Some(validation_id) => match api.store.read_capability_evolution_validation_report(
            &scope,
            &candidate_id,
            &validation_id,
        ) {
            Ok(report) => report,
            Err(error) => {
                return HttpResponse::NotFound().json(json!({
                    "error": error.to_string()
                }));
            },
        },
        None => match api.store.list_capability_evolution_validation_reports(
            &scope,
            LearningCapabilityEvolutionValidationFilters {
                status: Some(
                    LearningCapabilityEvolutionValidationStatus::Passed
                        .as_str()
                        .to_string(),
                ),
                candidate_id: Some(candidate_id.clone()),
                capability_id: proposal.capability_id.clone(),
                limit: Some(1),
            },
        ) {
            Ok(mut reports) => {
                let Some(report) = reports.pop() else {
                    return HttpResponse::BadRequest().json(json!({
                        "error": "promotion requires at least one passed validation report"
                    }));
                };
                report
            },
            Err(error) => {
                return HttpResponse::InternalServerError().json(json!({
                    "error": error.to_string()
                }));
            },
        },
    };
    if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` is `{}`; promotion requires passed validation",
                validation.id,
                validation.status.as_str()
            )
        }));
    }
    if !capability_validation_has_material_evidence(
        &validation.commands,
        &validation.evidence_refs,
        &validation.metrics,
        &validation.payload,
    ) {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` has no material evidence; promotion requires at least one command, evidence_ref, non-empty metrics, or non-empty payload",
                validation.id
            )
        }));
    }
    if promotion_gate_requires_regression(&proposal)
        && !validation_report_has_regression_evidence(&validation)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` has no regression evidence; proposal promotion_gate requires regression validation before promotion",
                validation.id
            )
        }));
    }
    if validation.candidate_id != candidate_id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to candidate `{}` instead of `{}`",
                validation.id,
                validation.candidate_id,
                candidate_id
            )
        }));
    }
    if validation.proposal_id != proposal.id {
        return HttpResponse::BadRequest().json(json!({
            "error": format!(
                "validation report `{}` belongs to proposal `{}` instead of `{}`",
                validation.id,
                validation.proposal_id,
                proposal.id
            )
        }));
    }

    if let Some(implementation) = implementation.as_ref() {
        if implementation.validation_id != validation.id {
            return HttpResponse::BadRequest().json(json!({
                "error": format!(
                    "implementation record `{}` belongs to validation `{}` instead of `{}`",
                    implementation.id,
                    implementation.validation_id,
                    validation.id
                )
            }));
        }
    }
    if let Some(application) = application.as_ref() {
        if application.validation_id != validation.id {
            return HttpResponse::BadRequest().json(json!({
                "error": format!(
                    "application record `{}` belongs to validation `{}` instead of `{}`",
                    application.id,
                    application.validation_id,
                    validation.id
                )
            }));
        }
        if let Some(implementation) = implementation.as_ref() {
            if application.implementation_id != implementation.id {
                return HttpResponse::BadRequest().json(json!({
                    "error": format!(
                        "application record `{}` belongs to implementation `{}` instead of `{}`",
                        application.id,
                        application.implementation_id,
                        implementation.id
                    )
                }));
            }
        }
    }
    let scoped_application_targets = scoped_application_required_targets(
        &proposal,
        implementation.as_ref(),
        &request.applied_files,
    );
    let application_required = !scoped_application_targets.is_empty();
    if application_required && application.is_none() {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion for scoped skill targets requires application_id referencing an applied skill-evolution application record"
        }));
    }
    if application_required
        && !application
            .as_ref()
            .map(application_record_contains_scoped_change)
            .unwrap_or(false)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion for scoped skill targets requires an applied application record with at least one scoped skills file change"
        }));
    }
    if application_required
        && !application
            .as_ref()
            .map(|record| {
                application_record_covers_scoped_targets(record, &scoped_application_targets)
            })
            .unwrap_or(false)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion for scoped skill targets requires the applied application record to cover at least one scoped skill target from the proposal, implementation, or promotion request"
        }));
    }
    if application
        .as_ref()
        .map(application_record_has_failed_catalog_refresh)
        .unwrap_or(false)
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion requires a successful runtime skill catalog refresh for applied scoped/system skill changes"
        }));
    }

    let now = Utc::now();
    let applied_files = if request.applied_files.is_empty() {
        application
            .as_ref()
            .map(|record| {
                record
                    .changed_files
                    .iter()
                    .map(|file| file.path.clone())
                    .collect::<Vec<_>>()
            })
            .filter(|files| !files.is_empty())
            .or_else(|| {
                implementation
                    .as_ref()
                    .map(|record| record.applied_files.clone())
                    .filter(|files| !files.is_empty())
            })
            .unwrap_or_else(|| proposal.proposed_files.clone())
    } else {
        request.applied_files
    };
    let evidence_refs = if request.evidence_refs.is_empty() {
        application
            .as_ref()
            .map(|record| record.evidence_refs.clone())
            .filter(|refs| !refs.is_empty())
            .or_else(|| {
                implementation
                    .as_ref()
                    .map(|record| record.evidence_refs.clone())
                    .filter(|refs| !refs.is_empty())
            })
            .unwrap_or_default()
    } else {
        request.evidence_refs
    };
    let payload = if request.payload.is_null() {
        application
            .as_ref()
            .map(|record| record.payload.clone())
            .filter(|payload| !payload.is_null())
            .or_else(|| {
                implementation
                    .as_ref()
                    .map(|record| record.payload.clone())
                    .filter(|payload| !payload.is_null())
            })
            .unwrap_or(Value::Null)
    } else {
        request.payload
    };
    if evidence_refs.is_empty()
        && payload.is_null()
        && implementation.is_none()
        && application.is_none()
    {
        return HttpResponse::BadRequest().json(json!({
            "error": "promotion evidence is required; provide implementation_id, application_id, evidence_refs, or a non-null payload"
        }));
    }
    let record = LearningCapabilityEvolutionPromotionRecord {
        id: format!("lcepromo_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        validation_id: validation.id.clone(),
        implementation_id: implementation.as_ref().map(|record| record.id.clone()),
        application_id: application.as_ref().map(|record| record.id.clone()),
        capability_id: proposal.capability_id.clone(),
        actor: actor.clone(),
        summary: summary.clone(),
        applied_files,
        evidence_refs,
        payload,
        created_at: now,
    };
    if let Err(error) = api
        .store
        .append_capability_evolution_promotion_record(&record)
    {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let (post_promotion_monitor, post_promotion_monitor_error) =
        phase7_record_initial_post_promotion_monitor_outcome(&api, &scope, &record, actor.clone());

    let previous_backlog_status = backlog.status.clone();
    backlog.status = LearningCapabilityEvolutionBacklogStatus::Implemented;
    backlog.updated_at = now;
    if let Err(error) = api.store.write_capability_evolution_backlog_item(&backlog) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string()
        }));
    }
    let candidate = if candidate.state == LearningCandidateState::Implemented {
        candidate
    } else {
        match api.store.transition_candidate(
            &scope,
            &candidate_id,
            LearningCandidateState::Implemented,
            actor.clone(),
            "capability_promotion_recorded".to_string(),
            summary.clone(),
            record.evidence_refs.clone(),
        ) {
            Ok(candidate) => candidate,
            Err(error) => {
                return HttpResponse::BadRequest().json(json!({
                    "error": error.to_string()
                }));
            },
        }
    };
    if let Err(error) = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_promotion_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Capability-evolution promotion `{}` recorded by {} for proposal `{}`.",
                record.id, actor, proposal.id
            ),
            evidence_refs: record.evidence_refs.clone(),
            payload: json!({
                "candidate_id": candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": validation.id.clone(),
                "implementation_id": record.implementation_id.clone(),
                "application_id": record.application_id.clone(),
                "promotion_id": record.id.clone(),
                "capability_id": proposal.capability_id.clone(),
                "from_backlog_status": previous_backlog_status.as_str(),
                "to_backlog_status": backlog.status.as_str(),
                "to_candidate_state": candidate.state.as_str(),
                "actor": actor,
                "applied_files": record.applied_files.clone(),
                "post_promotion_monitor": post_promotion_monitor.as_ref().map(|monitor| json!({
                    "id": monitor.id.clone(),
                    "status": monitor.status.as_str(),
                    "after_invocation_count": monitor.after_invocation_count
                })),
                "post_promotion_monitor_error": post_promotion_monitor_error.clone()
            }),
        },
    ) {
        return HttpResponse::InternalServerError().json(json!({
            "error": error.to_string(),
            "post_promotion_monitor_error": post_promotion_monitor_error
        }));
    }

    HttpResponse::Ok().json(json!({
        "promotion": record,
        "proposal": proposal,
        "validation": validation,
        "implementation": implementation,
        "application": application,
        "backlog": backlog,
        "candidate": candidate,
        "post_promotion_monitor": post_promotion_monitor,
        "post_promotion_monitor_error": post_promotion_monitor_error
    }))
}

/// Per-scope result of one scheduled harness steward/growth-eval drain tick
/// (Phase 4.2). Returned to the scheduler so it can emit a `harness_steward_tick`
/// durable learning event with concrete counts.
#[derive(Debug, Clone, Default)]
pub struct HarnessStewardTickScopeReport {
    /// `completed` / `failed` — the steward run status.
    pub steward_status: String,
    pub steward_run_id: Option<String>,
    /// New capability/skill proposals drafted this tick (deduped, ≤ `max_proposals`).
    pub proposals_drafted: u64,
    /// Drafted proposals awaiting owner review (final apply stays owner-gated).
    pub steward_attention_required: u64,
    /// `passed` / `failed` / `blocked` — the growth-evaluation run status.
    pub growth_eval_status: Option<String>,
    pub growth_eval_run_id: Option<String>,
    /// Non-fatal errors (persist/event failures) collected so the scheduler can
    /// surface them on the tick event without aborting the whole drain.
    pub errors: Vec<String>,
}

impl LearningApi {
    /// Run one scheduled harness steward + growth-evaluation drain for `scope`
    /// (Phase 4.2 — the operator-only workers, now on an autonomous cadence).
    ///
    /// Drains the skill/capability backlog into at most `max_proposals` deduped
    /// DRAFT proposals (the steward already dedupes via the backlog
    /// `dedupe_fingerprint` + skips items that already have a proposal), then
    /// runs + persists a growth evaluation. This is **drafting only**: the
    /// validation / implementation / dry-run budgets are pinned to zero so a
    /// background tick never auto-validates or auto-applies even an already
    /// owner-approved proposal — final apply stays on the operator-gated HTTP
    /// path, unchanged.
    pub async fn run_harness_steward_tick_for_scope(
        &self,
        scope: LearningScope,
        max_proposals: usize,
    ) -> HarnessStewardTickScopeReport {
        let mut report = HarnessStewardTickScopeReport::default();

        // --- Lane 1: drain the skill/capability backlog into deduped DRAFTS. ---
        let steward_request = RunLearningCapabilityEvolutionStewardRequest {
            workspace: None,
            actor: default_steward_actor(),
            backlog_limit: None,
            max_proposals: Some(max_proposals),
            max_evaluations: None,
            // Drafting only: never validate / implement / dry-run from a
            // background tick. The steward already short-circuits to
            // `request_review` for any non-Approved proposal, but pinning these
            // to zero makes the no-auto-apply invariant explicit + robust to a
            // pre-Approved backlog item.
            max_validations: Some(0),
            max_implementations: Some(0),
            max_dry_runs: Some(0),
            max_touched_files: None,
            timeout_seconds: None,
            include_regression: None,
            dry_run: false,
            payload: json!({ "origin": "harness_steward_tick" }),
        };
        let steward_report =
            run_capability_evolution_steward_cycle(self, scope.clone(), steward_request).await;
        report.steward_status = steward_report.status.as_str().to_string();
        report.steward_run_id = Some(steward_report.id.clone());
        report.proposals_drafted = steward_report
            .metrics
            .get("proposal_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        report.steward_attention_required = steward_report
            .metrics
            .get("attention_required_count")
            .and_then(|value| value.as_u64())
            .unwrap_or(0);
        if let Err(error) = self
            .store
            .write_capability_evolution_steward_run_report(&steward_report)
        {
            report
                .errors
                .push(format!("steward_report_persist_failed: {error}"));
        }

        // --- Lane 2: run + persist a growth evaluation (the eval lane). ---
        let growth_request = RunLearningGrowthEvaluationRequest {
            principal: None,
            workspace: None,
            suite_id: None,
            window_days: None,
            payload: json!({ "origin": "harness_steward_tick" }),
        };
        match run_learning_growth_evaluation(&self.store, scope.clone(), growth_request) {
            Ok(growth_report) => {
                report.growth_eval_status = Some(growth_report.status.as_str().to_string());
                report.growth_eval_run_id = Some(growth_report.id.clone());
                if let Err(error) = self
                    .store
                    .write_growth_evaluation_run_report(&growth_report)
                {
                    report
                        .errors
                        .push(format!("growth_eval_persist_failed: {error}"));
                } else if let Err(error) = self.store.append_event(
                    scope.clone(),
                    CreateLearningEventRequest {
                        principal: None,
                        workspace: None,
                        event_type: "learning_growth_evaluation_run_recorded".to_string(),
                        agent_id: None,
                        task_id: None,
                        execution_id: None,
                        chat_session_id: None,
                        summary: growth_report.summary.clone(),
                        evidence_refs: growth_report.evidence_refs.clone(),
                        payload: json!({
                            "run_id": growth_report.id.clone(),
                            "suite_id": growth_report.suite_id.clone(),
                            "run_status": growth_report.status.as_str(),
                            "metrics": growth_report.metrics.clone(),
                            "origin": "harness_steward_tick"
                        }),
                    },
                ) {
                    report
                        .errors
                        .push(format!("growth_eval_event_failed: {error}"));
                }
            },
            Err(error) => {
                report.errors.push(format!("growth_eval_failed: {error}"));
            },
        }

        report
    }
}

#[derive(Debug, Clone)]
struct CapabilityStewardBudgets {
    backlog_limit: usize,
    max_proposals: usize,
    max_evaluations: usize,
    max_validations: usize,
    max_implementations: usize,
    max_dry_runs: usize,
    max_touched_files: usize,
    timeout_seconds: u64,
    include_regression: bool,
}

impl CapabilityStewardBudgets {
    fn from_request(request: &RunLearningCapabilityEvolutionStewardRequest) -> Self {
        Self {
            backlog_limit: request
                .backlog_limit
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_BACKLOG_LIMIT)
                .clamp(1, 100),
            max_proposals: request
                .max_proposals
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_PROPOSALS)
                .min(25),
            max_evaluations: request
                .max_evaluations
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_EVALUATIONS)
                .min(50),
            max_validations: request
                .max_validations
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_VALIDATIONS)
                .min(10),
            max_implementations: request
                .max_implementations
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_IMPLEMENTATIONS)
                .min(10),
            max_dry_runs: request
                .max_dry_runs
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_DRY_RUNS)
                .min(10),
            max_touched_files: request
                .max_touched_files
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_MAX_TOUCHED_FILES)
                .clamp(1, 100),
            timeout_seconds: request
                .timeout_seconds
                .unwrap_or(DEFAULT_CAPABILITY_STEWARD_VALIDATION_TIMEOUT_SECONDS)
                .clamp(1, MAX_CAPABILITY_VALIDATION_TIMEOUT_SECONDS),
            include_regression: request.include_regression.unwrap_or(true),
        }
    }

    fn as_json(&self) -> Value {
        json!({
            "backlog_limit": self.backlog_limit,
            "max_proposals": self.max_proposals,
            "max_evaluations": self.max_evaluations,
            "max_validations": self.max_validations,
            "max_implementations": self.max_implementations,
            "max_dry_runs": self.max_dry_runs,
            "max_touched_files": self.max_touched_files,
            "timeout_seconds": self.timeout_seconds,
            "include_regression": self.include_regression
        })
    }
}

#[derive(Debug, Default)]
struct CapabilityStewardCounters {
    proposals: usize,
    evaluations: usize,
    validations: usize,
    implementations: usize,
    dry_runs: usize,
}

async fn run_capability_evolution_steward_cycle(
    api: &LearningApi,
    scope: LearningScope,
    request: RunLearningCapabilityEvolutionStewardRequest,
) -> LearningCapabilityEvolutionStewardRunReport {
    let started_at = Utc::now();
    let actor =
        optional_non_empty(Some(request.actor.clone())).unwrap_or_else(default_steward_actor);
    let budgets = CapabilityStewardBudgets::from_request(&request);
    let mut counters = CapabilityStewardCounters::default();
    let mut actions = Vec::new();

    let backlogs = match steward_open_backlog_items(&api.store, &scope, budgets.backlog_limit) {
        Ok(items) => items,
        Err(error) => {
            actions.push(steward_action(
                "read_ranked_backlog",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                None,
                None,
                Some(error),
                Value::Null,
            ));
            return finalize_steward_report(
                scope,
                actor,
                budgets,
                counters,
                actions,
                request.dry_run,
                request.payload,
                started_at,
            );
        },
    };

    if backlogs.is_empty() {
        actions.push(steward_action(
            "read_ranked_backlog",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            None,
            None,
            Some("no_open_backlog_items".to_string()),
            json!({
                "statuses": ["queued", "in_review", "validated"],
                "limit": budgets.backlog_limit
            }),
        ));
    }

    for backlog in backlogs {
        steward_process_backlog_item(
            api,
            &scope,
            &actor,
            &budgets,
            request.dry_run,
            &mut counters,
            &mut actions,
            backlog,
        )
        .await;
    }

    finalize_steward_report(
        scope,
        actor,
        budgets,
        counters,
        actions,
        request.dry_run,
        request.payload,
        started_at,
    )
}

async fn steward_process_backlog_item(
    api: &LearningApi,
    scope: &LearningScope,
    actor: &str,
    budgets: &CapabilityStewardBudgets,
    dry_run: bool,
    counters: &mut CapabilityStewardCounters,
    actions: &mut Vec<LearningCapabilityEvolutionStewardAction>,
    mut backlog: LearningCapabilityEvolutionBacklogItem,
) {
    let mut proposal = match api
        .store
        .read_capability_evolution_proposal(scope, &backlog.candidate_id)
    {
        Ok(proposal) => {
            actions.push(steward_backlog_action(
                "draft_proposal",
                LearningCapabilityEvolutionStewardActionStatus::Skipped,
                &backlog,
                Some("proposal_already_exists".to_string()),
                json!({
                    "proposal_id": proposal.id.clone(),
                    "proposal_status": proposal.status.as_str()
                }),
            ));
            Some(proposal)
        },
        Err(_) => {
            if !matches!(
                backlog.status,
                LearningCapabilityEvolutionBacklogStatus::Queued
            ) {
                actions.push(steward_backlog_action(
                    "draft_proposal",
                    LearningCapabilityEvolutionStewardActionStatus::Skipped,
                    &backlog,
                    Some(format!("backlog_status_is_{}", backlog.status.as_str())),
                    Value::Null,
                ));
                None
            } else if let Some(reason) = steward_proposal_draft_skip_reason(&backlog) {
                actions.push(steward_backlog_action(
                    "draft_proposal",
                    LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                    &backlog,
                    Some(reason),
                    json!({
                        "owner_hints": backlog.owner_hints.clone(),
                        "risk_level": backlog.risk_level.as_str()
                    }),
                ));
                None
            } else if counters.proposals >= budgets.max_proposals {
                actions.push(steward_backlog_action(
                    "draft_proposal",
                    LearningCapabilityEvolutionStewardActionStatus::Skipped,
                    &backlog,
                    Some("proposal_budget_exhausted".to_string()),
                    json!({"max_proposals": budgets.max_proposals}),
                ));
                None
            } else if dry_run {
                actions.push(steward_backlog_action(
                    "draft_proposal",
                    LearningCapabilityEvolutionStewardActionStatus::Skipped,
                    &backlog,
                    Some("dry_run_would_draft_proposal".to_string()),
                    Value::Null,
                ));
                None
            } else {
                let mut drafted =
                    draft_capability_evolution_proposal(scope, &backlog, None, actor, true);
                enrich_capability_evolution_proposal_draft(
                    api.store.workspace_layout(),
                    &api.repo_root,
                    scope,
                    &backlog,
                    &mut drafted,
                );
                match api.store.write_capability_evolution_proposal(&drafted) {
                    Ok(()) => {
                        counters.proposals += 1;
                        let previous_status = backlog.status.clone();
                        backlog.status = LearningCapabilityEvolutionBacklogStatus::InReview;
                        backlog.updated_at = drafted.updated_at;
                        if let Err(error) =
                            api.store.write_capability_evolution_backlog_item(&backlog)
                        {
                            actions.push(steward_backlog_action(
                                "draft_proposal",
                                LearningCapabilityEvolutionStewardActionStatus::Failed,
                                &backlog,
                                Some(format!(
                                    "proposal_written_but_backlog_update_failed: {error}"
                                )),
                                json!({"proposal_id": drafted.id.clone()}),
                            ));
                        } else {
                            let _ = api.store.append_event(
                                scope.clone(),
                                CreateLearningEventRequest {
                                    principal: None,
                                    workspace: None,
                                    event_type:
                                        "learning_capability_steward_proposal_drafted".to_string(),
                                    agent_id: backlog.source_agent_id.clone(),
                                    task_id: backlog.source_task_id.clone(),
                                    execution_id: backlog.source_execution_id.clone(),
                                    chat_session_id: backlog.source_chat_session_id.clone(),
                                    summary: format!(
                                        "Skill Evolution steward drafted proposal `{}` for candidate `{}`.",
                                        drafted.id, drafted.candidate_id
                                    ),
                                    evidence_refs: backlog.evidence_refs.clone(),
                                    payload: json!({
                                        "candidate_id": drafted.candidate_id.clone(),
                                        "proposal_id": drafted.id.clone(),
                                        "from_backlog_status": previous_status.as_str(),
                                        "to_backlog_status": backlog.status.as_str(),
                                        "actor": actor
                                    }),
                                },
                            );
                            let mut action = steward_backlog_action(
                                "draft_proposal",
                                LearningCapabilityEvolutionStewardActionStatus::Completed,
                                &backlog,
                                None,
                                json!({
                                    "proposal_status": drafted.status.as_str(),
                                    "proposed_files": drafted.proposed_files.clone(),
                                    "patch_count": drafted.patches.len()
                                }),
                            );
                            action.proposal_id = Some(drafted.id.clone());
                            actions.push(action);
                        }
                        Some(drafted)
                    },
                    Err(error) => {
                        actions.push(steward_backlog_action(
                            "draft_proposal",
                            LearningCapabilityEvolutionStewardActionStatus::Failed,
                            &backlog,
                            Some(error.to_string()),
                            Value::Null,
                        ));
                        None
                    },
                }
            }
        },
    };

    let Some(current_proposal) = proposal.as_mut() else {
        return;
    };

    steward_generate_evaluation_if_needed(
        api,
        scope,
        actor,
        budgets,
        dry_run,
        counters,
        actions,
        &backlog,
        current_proposal,
    );

    match current_proposal.status {
        LearningCapabilityEvolutionProposalStatus::Draft
        | LearningCapabilityEvolutionProposalStatus::ReadyForReview => {
            let mut action = steward_backlog_action(
                "request_review",
                LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                &backlog,
                Some("proposal_review_required".to_string()),
                json!({
                    "proposal_status": current_proposal.status.as_str(),
                    "next_action": "approve_or_reject_proposal"
                }),
            );
            action.proposal_id = Some(current_proposal.id.clone());
            actions.push(action);
            return;
        },
        LearningCapabilityEvolutionProposalStatus::Approved => {},
        LearningCapabilityEvolutionProposalStatus::Rejected
        | LearningCapabilityEvolutionProposalStatus::Superseded
        | LearningCapabilityEvolutionProposalStatus::Archived => {
            let mut action = steward_backlog_action(
                "continue_after_review",
                LearningCapabilityEvolutionStewardActionStatus::Skipped,
                &backlog,
                Some(format!(
                    "proposal_status_is_{}",
                    current_proposal.status.as_str()
                )),
                Value::Null,
            );
            action.proposal_id = Some(current_proposal.id.clone());
            actions.push(action);
            return;
        },
    }

    let validation = steward_run_validation_if_allowed(
        api,
        scope,
        actor,
        budgets,
        dry_run,
        counters,
        actions,
        &mut backlog,
        current_proposal,
    )
    .await;
    let Some(validation) = validation else {
        return;
    };

    let implementation = steward_record_implementation_if_allowed(
        api,
        scope,
        actor,
        budgets,
        dry_run,
        counters,
        actions,
        &backlog,
        current_proposal,
        &validation,
    );
    let Some(implementation) = implementation else {
        return;
    };

    steward_prepare_dry_run_if_allowed(
        api,
        scope,
        actor,
        budgets,
        dry_run,
        counters,
        actions,
        &backlog,
        current_proposal,
        &validation,
        &implementation,
    );
}

fn steward_generate_evaluation_if_needed(
    api: &LearningApi,
    scope: &LearningScope,
    actor: &str,
    budgets: &CapabilityStewardBudgets,
    dry_run: bool,
    counters: &mut CapabilityStewardCounters,
    actions: &mut Vec<LearningCapabilityEvolutionStewardAction>,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) {
    if api
        .store
        .read_evaluation_backlog_item(scope, &backlog.candidate_id)
        .is_ok()
    {
        let mut action = steward_backlog_action(
            "generate_evaluation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("evaluation_backlog_already_exists".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        action.evaluation_backlog_id = Some(format!(
            "leb_{}",
            proposal.candidate_id.trim_start_matches("lc_")
        ));
        actions.push(action);
        return;
    }
    if counters.evaluations >= budgets.max_evaluations {
        let mut action = steward_backlog_action(
            "generate_evaluation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("evaluation_budget_exhausted".to_string()),
            json!({"max_evaluations": budgets.max_evaluations}),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return;
    }
    if dry_run {
        let mut action = steward_backlog_action(
            "generate_evaluation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_would_generate_evaluation".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return;
    }
    let candidate = match api.store.read_candidate(scope, &backlog.candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            let mut action = steward_backlog_action(
                "generate_evaluation",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(error.to_string()),
                Value::Null,
            );
            action.proposal_id = Some(proposal.id.clone());
            actions.push(action);
            return;
        },
    };
    if candidate.state.is_terminal() {
        let mut action = steward_backlog_action(
            "generate_evaluation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some(format!(
                "candidate_state_is_terminal_{}",
                candidate.state.as_str()
            )),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return;
    }
    let generated = build_capability_evolution_evaluation_backlog_item(
        scope,
        &candidate,
        backlog,
        proposal,
        None,
        json!({
            "source": "skill_evolution_steward",
            "actor": actor
        }),
    );
    match api.store.write_evaluation_backlog_item(&generated) {
        Ok(()) => {
            counters.evaluations += 1;
            let eval_path = api
                .store
                .workspace_layout()
                .learning_evaluation_backlog_path(
                    &scope.principal,
                    &scope.workspace,
                    &backlog.candidate_id,
                );
            let _ = api.store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type:
                        "learning_capability_steward_eval_case_generated".to_string(),
                    agent_id: backlog.source_agent_id.clone(),
                    task_id: backlog.source_task_id.clone(),
                    execution_id: backlog.source_execution_id.clone(),
                    chat_session_id: backlog.source_chat_session_id.clone(),
                    summary: format!(
                        "Skill Evolution steward generated evaluation backlog item `{}` for proposal `{}`.",
                        generated.id, proposal.id
                    ),
                    evidence_refs: generated.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": backlog.candidate_id.clone(),
                        "proposal_id": proposal.id.clone(),
                        "evaluation_backlog_id": generated.id.clone(),
                        "evaluation_backlog_path": eval_path.display().to_string(),
                        "actor": actor
                    }),
                },
            );
            let mut action = steward_backlog_action(
                "generate_evaluation",
                LearningCapabilityEvolutionStewardActionStatus::Completed,
                backlog,
                None,
                json!({
                    "case_kind": generated.case_kind.clone(),
                    "priority": generated.priority.clone(),
                    "evaluation_backlog_path": eval_path.display().to_string()
                }),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.evaluation_backlog_id = Some(generated.id);
            actions.push(action);
        },
        Err(error) => {
            let mut action = steward_backlog_action(
                "generate_evaluation",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(error.to_string()),
                Value::Null,
            );
            action.proposal_id = Some(proposal.id.clone());
            actions.push(action);
        },
    }
}

async fn steward_run_validation_if_allowed(
    api: &LearningApi,
    scope: &LearningScope,
    actor: &str,
    budgets: &CapabilityStewardBudgets,
    dry_run: bool,
    counters: &mut CapabilityStewardCounters,
    actions: &mut Vec<LearningCapabilityEvolutionStewardAction>,
    backlog: &mut LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Option<LearningCapabilityEvolutionValidationReport> {
    if let Some(existing) = latest_passed_validation(api, scope, backlog, proposal) {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("passed_validation_already_exists".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(existing.id.clone());
        actions.push(action);
        return Some(existing);
    }
    if counters.validations >= budgets.max_validations {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("validation_budget_exhausted".to_string()),
            json!({"max_validations": budgets.max_validations}),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    if !matches!(backlog.risk_level, LearningRiskLevel::Low) {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
            backlog,
            Some("risk_not_low_for_autonomous_validation".to_string()),
            json!({
                "risk_level": backlog.risk_level.as_str(),
                "next_action": "operator_or_specialist_runs_validation"
            }),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    if !capability_backlog_accepts_validation(&backlog.status) {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some(format!("backlog_status_is_{}", backlog.status.as_str())),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    let command_specs = planned_validation_commands(proposal, &[], budgets.include_regression);
    if command_specs.is_empty() {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("no_validation_commands".to_string()),
            json!({
                "include_regression": budgets.include_regression,
                "next_action": "add_allowlisted_validation_commands_to_proposal"
            }),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    if let Some(blocked) = command_specs
        .iter()
        .find(|spec| !steward_validation_command_allowlisted(&spec.command))
    {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
            backlog,
            Some("validation_command_not_allowlisted".to_string()),
            json!({
                "command": blocked.command.clone(),
                "source": blocked.source.clone(),
                "allowed_prefixes": steward_allowed_validation_prefixes()
            }),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    if dry_run {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_would_run_validation".to_string()),
            json!({
                "commands": command_specs
                    .iter()
                    .map(|spec| spec.command.clone())
                    .collect::<Vec<_>>()
            }),
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }

    let candidate = match api.store.read_candidate(scope, &backlog.candidate_id) {
        Ok(candidate) => candidate,
        Err(error) => {
            let mut action = steward_backlog_action(
                "run_validation",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(error.to_string()),
                Value::Null,
            );
            action.proposal_id = Some(proposal.id.clone());
            actions.push(action);
            return None;
        },
    };
    if candidate.state.is_terminal() {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some(format!(
                "candidate_state_is_terminal_{}",
                candidate.state.as_str()
            )),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }

    let existing_eval_backlog = api
        .store
        .read_evaluation_backlog_item(scope, &backlog.candidate_id)
        .ok();
    let eval_backlog = build_capability_evolution_evaluation_backlog_item(
        scope,
        &candidate,
        backlog,
        proposal,
        existing_eval_backlog.as_ref(),
        json!({
            "source": "skill_evolution_steward_validation_runner",
            "actor": actor
        }),
    );
    if let Err(error) = api.store.write_evaluation_backlog_item(&eval_backlog) {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Failed,
            backlog,
            Some(error.to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }

    let scope_root = api
        .store
        .workspace_layout()
        .scope_root(&scope.principal, &scope.workspace);
    let started_at = Utc::now();
    let mut results = Vec::new();
    for spec in command_specs
        .iter()
        .take(MAX_CAPABILITY_VALIDATION_COMMANDS)
    {
        results.push(run_validation_command(spec, &scope_root, budgets.timeout_seconds).await);
    }
    counters.validations += 1;
    let command_count = command_specs.len().min(MAX_CAPABILITY_VALIDATION_COMMANDS);
    let passed_count = results
        .iter()
        .filter(|result| result.success && !result.timed_out && result.spawn_error.is_none())
        .count();
    let failed_count = command_count.saturating_sub(passed_count);
    let regression_checked = command_specs.iter().any(|spec| spec.regression);
    let phase5_fixture_cases = phase5_fixture_cases_from_proposal(proposal);
    let phase5_fixture_results =
        phase5_fixture_results_for_commands(&phase5_fixture_cases, &results);
    let phase5_fixture_metrics =
        phase5_fixture_result_metrics(&phase5_fixture_cases, &phase5_fixture_results);
    let status = if failed_count == 0 {
        LearningCapabilityEvolutionValidationStatus::Passed
    } else {
        LearningCapabilityEvolutionValidationStatus::Failed
    };
    let command_strings = command_specs
        .iter()
        .map(|spec| spec.command.clone())
        .collect::<Vec<_>>();
    let report = LearningCapabilityEvolutionValidationReport {
        id: format!("lcev_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: backlog.candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        status: status.clone(),
        capability_id: proposal.capability_id.clone(),
        runner: actor.to_string(),
        summary: format!(
            "Skill Evolution steward executed {command_count} allowlisted validation command(s): {passed_count} passed, {failed_count} failed."
        ),
        commands: command_strings,
        evidence_refs: backlog.evidence_refs.clone(),
        metrics: json!({
            "command_count": command_count,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "regression_checked": regression_checked,
            "phase5_fixture": phase5_fixture_metrics
        }),
        payload: json!({
            "source": "skill_evolution_steward",
            "started_at": started_at,
            "ended_at": Utc::now(),
            "scope_root": scope_root.display().to_string(),
            "evaluation_backlog_id": eval_backlog.id.clone(),
            "include_regression": budgets.include_regression,
            "regression_checked": regression_checked,
            "timeout_seconds": budgets.timeout_seconds,
            "results": results,
            "phase5_fixture_cases": phase5_fixture_cases,
            "phase5_fixture_results": phase5_fixture_results
        }),
        created_at: Utc::now(),
    };
    if status == LearningCapabilityEvolutionValidationStatus::Passed
        && !capability_validation_has_material_evidence(
            &report.commands,
            &report.evidence_refs,
            &report.metrics,
            &report.payload,
        )
    {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Failed,
            backlog,
            Some("validation_runner_produced_no_material_evidence".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }
    if let Err(error) = api
        .store
        .write_capability_evolution_validation_report(&report)
    {
        let mut action = steward_backlog_action(
            "run_validation",
            LearningCapabilityEvolutionStewardActionStatus::Failed,
            backlog,
            Some(error.to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        actions.push(action);
        return None;
    }

    let previous_backlog_status = backlog.status.clone();
    if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
        backlog.status = LearningCapabilityEvolutionBacklogStatus::Validated;
        backlog.updated_at = report.created_at;
        if let Err(error) = api.store.write_capability_evolution_backlog_item(backlog) {
            let mut action = steward_backlog_action(
                "run_validation",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(format!(
                    "validation_written_but_backlog_update_failed: {error}"
                )),
                json!({"validation_id": report.id.clone()}),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(report.id.clone());
            actions.push(action);
            return Some(report);
        }
        if !matches!(
            candidate.state,
            LearningCandidateState::Evaluated | LearningCandidateState::Implemented
        ) {
            let _ = api.store.transition_candidate(
                scope,
                &backlog.candidate_id,
                LearningCandidateState::Evaluated,
                actor.to_string(),
                "capability_validation_passed".to_string(),
                "Approved capability-evolution proposal passed steward validation.".to_string(),
                report.evidence_refs.clone(),
            );
        }
    }

    let _ = api.store.append_event(
        scope.clone(),
        CreateLearningEventRequest {
            principal: None,
            workspace: None,
            event_type: "learning_capability_steward_validation_recorded".to_string(),
            agent_id: backlog.source_agent_id.clone(),
            task_id: backlog.source_task_id.clone(),
            execution_id: backlog.source_execution_id.clone(),
            chat_session_id: backlog.source_chat_session_id.clone(),
            summary: format!(
                "Skill Evolution steward recorded validation `{}` as {} for proposal `{}`.",
                report.id,
                report.status.as_str(),
                proposal.id
            ),
            evidence_refs: report.evidence_refs.clone(),
            payload: json!({
                "candidate_id": backlog.candidate_id.clone(),
                "proposal_id": proposal.id.clone(),
                "validation_id": report.id.clone(),
                "validation_status": report.status.as_str(),
                "from_backlog_status": previous_backlog_status.as_str(),
                "to_backlog_status": backlog.status.as_str(),
                "actor": actor,
                "passed_count": passed_count,
                "failed_count": failed_count
            }),
        },
    );

    let mut action = steward_backlog_action(
        "run_validation",
        if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
            LearningCapabilityEvolutionStewardActionStatus::Completed
        } else {
            LearningCapabilityEvolutionStewardActionStatus::Failed
        },
        backlog,
        if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
            None
        } else {
            Some("validation_failed".to_string())
        },
        json!({
            "validation_status": report.status.as_str(),
            "command_count": command_count,
            "passed_count": passed_count,
            "failed_count": failed_count,
            "regression_checked": regression_checked
        }),
    );
    action.proposal_id = Some(proposal.id.clone());
    action.validation_id = Some(report.id.clone());
    actions.push(action);
    (report.status == LearningCapabilityEvolutionValidationStatus::Passed).then_some(report)
}

fn steward_record_implementation_if_allowed(
    api: &LearningApi,
    scope: &LearningScope,
    actor: &str,
    budgets: &CapabilityStewardBudgets,
    dry_run: bool,
    counters: &mut CapabilityStewardCounters,
    actions: &mut Vec<LearningCapabilityEvolutionStewardAction>,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    validation: &LearningCapabilityEvolutionValidationReport,
) -> Option<LearningCapabilityEvolutionImplementationRecord> {
    if let Some(existing) = latest_implementation(api, scope, backlog, proposal) {
        let mut action = steward_backlog_action(
            "record_implementation_bundle",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("implementation_bundle_already_exists".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        action.implementation_id = Some(existing.id.clone());
        actions.push(action);
        return Some(existing);
    }
    if counters.implementations >= budgets.max_implementations {
        let mut action = steward_backlog_action(
            "record_implementation_bundle",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("implementation_budget_exhausted".to_string()),
            json!({"max_implementations": budgets.max_implementations}),
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        actions.push(action);
        return None;
    }
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        let mut action = steward_backlog_action(
            "record_implementation_bundle",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some(format!("backlog_status_is_{}", backlog.status.as_str())),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        actions.push(action);
        return None;
    }
    let drafted_record = match draft_capability_evolution_implementation_bundle_record(
        scope,
        actor,
        backlog,
        proposal,
        validation,
        None,
        json!({"request_source": "skill_evolution_steward"}),
    ) {
        Ok(record) => record,
        Err(error) => {
            let mut action = steward_backlog_action(
                "record_implementation_bundle",
                LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                backlog,
                Some(error),
                json!({"next_action": "draft_or_attach_implementation_bundle"}),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            actions.push(action);
            return None;
        },
    };
    if dry_run {
        let mut action = steward_backlog_action(
            "record_implementation_bundle",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_would_record_implementation_bundle".to_string()),
            json!({"patch_count": drafted_record.patches.len()}),
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        actions.push(action);
        return None;
    }
    let record = drafted_record;
    match api
        .store
        .write_capability_evolution_implementation_record(&record)
    {
        Ok(()) => {
            counters.implementations += 1;
            let _ = api.store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_capability_steward_implementation_recorded".to_string(),
                    agent_id: backlog.source_agent_id.clone(),
                    task_id: backlog.source_task_id.clone(),
                    execution_id: backlog.source_execution_id.clone(),
                    chat_session_id: backlog.source_chat_session_id.clone(),
                    summary: format!(
                        "Skill Evolution steward prepared implementation `{}` for proposal `{}`.",
                        record.id, proposal.id
                    ),
                    evidence_refs: record.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": backlog.candidate_id.clone(),
                        "proposal_id": proposal.id.clone(),
                        "validation_id": validation.id.clone(),
                        "implementation_id": record.id.clone(),
                        "actor": actor,
                        "patch_count": record.patches.len()
                    }),
                },
            );
            let mut action = steward_backlog_action(
                "record_implementation_bundle",
                LearningCapabilityEvolutionStewardActionStatus::Completed,
                backlog,
                None,
                json!({
                    "applied_files": record.applied_files.clone(),
                    "patch_count": record.patches.len()
                }),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            action.implementation_id = Some(record.id.clone());
            actions.push(action);
            Some(record)
        },
        Err(error) => {
            let mut action = steward_backlog_action(
                "record_implementation_bundle",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(error.to_string()),
                Value::Null,
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            actions.push(action);
            None
        },
    }
}

fn draft_capability_evolution_implementation_bundle_record(
    scope: &LearningScope,
    actor: &str,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    validation: &LearningCapabilityEvolutionValidationReport,
    summary: Option<&str>,
    request_payload: Value,
) -> Result<LearningCapabilityEvolutionImplementationRecord, String> {
    if proposal.status != LearningCapabilityEvolutionProposalStatus::Approved {
        return Err(format!(
            "implementation bundle drafting requires an approved proposal; proposal `{}` is `{}`",
            proposal.id,
            proposal.status.as_str()
        ));
    }
    if backlog.status != LearningCapabilityEvolutionBacklogStatus::Validated {
        return Err(format!(
            "implementation bundle drafting requires validated backlog state; backlog `{}` is `{}`",
            backlog.id,
            backlog.status.as_str()
        ));
    }
    if backlog.risk_level != LearningRiskLevel::Low {
        return Err(format!(
            "implementation bundle drafter currently only auto-drafts low-risk scoped SKILL.md guidance, tool_schema.yaml, or wrapper script changes; backlog `{}` is `{}`",
            backlog.id,
            backlog.risk_level.as_str()
        ));
    }
    if validation.status != LearningCapabilityEvolutionValidationStatus::Passed {
        return Err(format!(
            "implementation bundle drafting requires passed validation; validation `{}` is `{}`",
            validation.id,
            validation.status.as_str()
        ));
    }
    if validation.candidate_id != proposal.candidate_id {
        return Err(format!(
            "validation `{}` belongs to candidate `{}` instead of `{}`",
            validation.id, validation.candidate_id, proposal.candidate_id
        ));
    }
    if validation.proposal_id != proposal.id {
        return Err(format!(
            "validation `{}` belongs to proposal `{}` instead of `{}`",
            validation.id, validation.proposal_id, proposal.id
        ));
    }

    let patches = draft_capability_evolution_implementation_patches(proposal, validation)?;
    let mut applied_files = patches
        .iter()
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();
    applied_files.sort();
    applied_files.dedup();
    let summary = summary
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            format!(
                "Drafted low-risk scoped Skill Evolution implementation bundle from proposal `{}`.",
                proposal.id
            )
        });
    let bundle_scope = implementation_bundle_scope_for_patches(&patches);
    let mut payload = json!({
        "source": "skill_evolution_implementation_bundle_drafter",
        "review_required_before_apply": true,
        "target_surface": "scoped_skill",
        "scope": bundle_scope,
        "proposal_id": proposal.id.clone(),
        "validation_id": validation.id.clone(),
        "validation_status": validation.status.as_str(),
        "patch_count": patches.len(),
        "rollback_notes": phase4_rollback_notes(proposal),
        "docs_changelog_version_requirements": phase4_docs_changelog_version_requirements(proposal)
    });
    if !request_payload.is_null() {
        merge_json_object_field(&mut payload, "request_payload", request_payload);
    }

    Ok(LearningCapabilityEvolutionImplementationRecord {
        id: format!("lceimpl_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: backlog.candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        validation_id: validation.id.clone(),
        capability_id: proposal.capability_id.clone(),
        actor: actor.to_string(),
        summary,
        applied_files,
        patches,
        evidence_refs: validation.evidence_refs.clone(),
        payload,
        created_at: Utc::now(),
    })
}

fn draft_capability_evolution_implementation_patches(
    proposal: &LearningCapabilityEvolutionProposal,
    validation: &LearningCapabilityEvolutionValidationReport,
) -> Result<Vec<LearningCapabilityEvolutionProposalPatch>, String> {
    if proposal.patches.is_empty() {
        return Err(
            "proposal has no reviewable full-replacement patches to draft into an implementation bundle"
                .to_string(),
        );
    }
    let mut patches = Vec::new();
    for patch in &proposal.patches {
        let Some(artifact_kind) = implementation_bundle_supported_patch_kind(&patch.path) else {
            return Err(format!(
                "implementation bundle drafter currently only supports low-risk scoped skills/<skill>/SKILL.md guidance, skills/<skill>/tool_schema.yaml, or skills/<skill>/(scripts|wrapper|wrappers)/... patches with reviewed full-replacement metadata; `{}` needs manual review",
                patch.path
            ));
        };
        let Some(content) = extract_patch_replacement_content(patch)? else {
            return Err(format!(
                "proposal patch `{}` needs full replacement content in metadata before implementation bundle drafting",
                patch.path
            ));
        };
        if implementation_bundle_patch_requires_explicit_content_review(artifact_kind)
            && !patch_has_reviewed_full_replacement_metadata(patch)
        {
            return Err(format!(
                "proposal patch `{}` targets {artifact_kind} and must set metadata.reviewed_patch_content=true after operator review before implementation bundle drafting",
                patch.path
            ));
        }
        let mut metadata = object_metadata_or_wrapped(&patch.metadata);
        merge_json_object_field(
            &mut metadata,
            "generated_by",
            json!("skill_evolution_implementation_bundle_drafter"),
        );
        merge_json_object_field(&mut metadata, "content_kind", json!("full_replacement"));
        merge_json_object_field(&mut metadata, "target_surface", json!("scoped_skill"));
        merge_json_object_field(&mut metadata, "artifact_kind", json!(artifact_kind));
        merge_json_object_field(&mut metadata, "review_required_before_apply", json!(true));
        merge_json_object_field(&mut metadata, "proposal_id", json!(proposal.id.clone()));
        merge_json_object_field(&mut metadata, "validation_id", json!(validation.id.clone()));
        merge_json_object_field(
            &mut metadata,
            "validation_status",
            json!(validation.status.as_str()),
        );
        merge_json_object_field(
            &mut metadata,
            "exact_target_file",
            json!(patch.path.clone()),
        );
        merge_json_object_field(&mut metadata, "new_content", json!(content));
        merge_json_object_field(
            &mut metadata,
            "rollback_notes",
            phase4_rollback_notes(proposal),
        );
        merge_json_object_field(
            &mut metadata,
            "docs_changelog_version_requirements",
            phase4_docs_changelog_version_requirements(proposal),
        );
        patches.push(LearningCapabilityEvolutionProposalPatch {
            path: patch.path.clone(),
            operation: patch.operation.clone(),
            summary: format!("Implementation bundle: {}", patch.summary),
            diff: patch.diff.clone().or_else(|| {
                Some(
                    "Full replacement implementation patch; review metadata.new_content before dry-run or apply."
                        .to_string(),
                )
            }),
            metadata,
        });
    }
    Ok(patches)
}

fn steward_prepare_dry_run_if_allowed(
    api: &LearningApi,
    scope: &LearningScope,
    actor: &str,
    budgets: &CapabilityStewardBudgets,
    dry_run: bool,
    counters: &mut CapabilityStewardCounters,
    actions: &mut Vec<LearningCapabilityEvolutionStewardAction>,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    validation: &LearningCapabilityEvolutionValidationReport,
    implementation: &LearningCapabilityEvolutionImplementationRecord,
) {
    if let Some(existing) = latest_dry_run_application(api, scope, backlog, implementation) {
        let mut skipped = steward_backlog_action(
            "prepare_dry_run",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_application_already_exists".to_string()),
            Value::Null,
        );
        skipped.proposal_id = Some(proposal.id.clone());
        skipped.validation_id = Some(validation.id.clone());
        skipped.implementation_id = Some(implementation.id.clone());
        skipped.application_id = Some(existing.id.clone());
        actions.push(skipped);
        let mut attention = steward_backlog_action(
            "request_apply_approval",
            LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
            backlog,
            Some("operator_approval_required_before_apply_or_promotion".to_string()),
            json!({
                "application_id": existing.id.clone(),
                "next_action": "review_dry_run_then_apply_or_reject"
            }),
        );
        attention.proposal_id = Some(proposal.id.clone());
        attention.validation_id = Some(validation.id.clone());
        attention.implementation_id = Some(implementation.id.clone());
        attention.application_id = Some(existing.id);
        actions.push(attention);
        return;
    }
    if counters.dry_runs >= budgets.max_dry_runs {
        let mut action = steward_backlog_action(
            "prepare_dry_run",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_budget_exhausted".to_string()),
            json!({"max_dry_runs": budgets.max_dry_runs}),
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        action.implementation_id = Some(implementation.id.clone());
        actions.push(action);
        return;
    }
    if dry_run {
        let mut action = steward_backlog_action(
            "prepare_dry_run",
            LearningCapabilityEvolutionStewardActionStatus::Skipped,
            backlog,
            Some("dry_run_would_prepare_application_dry_run".to_string()),
            Value::Null,
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        action.implementation_id = Some(implementation.id.clone());
        actions.push(action);
        return;
    }
    let file_changes = match extract_capability_application_file_changes(implementation) {
        Ok(changes) => changes,
        Err(error) => {
            let mut action = steward_backlog_action(
                "prepare_dry_run",
                LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                backlog,
                Some(error),
                json!({"next_action": "fix_implementation_bundle_patch_metadata"}),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            action.implementation_id = Some(implementation.id.clone());
            actions.push(action);
            return;
        },
    };
    if file_changes.len() > budgets.max_touched_files {
        let mut action = steward_backlog_action(
            "prepare_dry_run",
            LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
            backlog,
            Some("max_touched_files_exceeded".to_string()),
            json!({
                "touched_file_count": file_changes.len(),
                "max_touched_files": budgets.max_touched_files
            }),
        );
        action.proposal_id = Some(proposal.id.clone());
        action.validation_id = Some(validation.id.clone());
        action.implementation_id = Some(implementation.id.clone());
        actions.push(action);
        return;
    }
    let target_surface = LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill;
    let target_root = capability_application_target_root(
        api.store.workspace_layout(),
        &api.repo_root,
        scope,
        target_surface,
    );
    let prepared =
        match prepare_capability_file_changes(&target_root, target_surface, &file_changes) {
            Ok(prepared) => prepared,
            Err(error) => {
                let mut action = steward_backlog_action(
                    "prepare_dry_run",
                    LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                    backlog,
                    Some(error),
                    json!({"next_action": "review_target_paths_or_patch_content"}),
                );
                action.proposal_id = Some(proposal.id.clone());
                action.validation_id = Some(validation.id.clone());
                action.implementation_id = Some(implementation.id.clone());
                actions.push(action);
                return;
            },
        };
    let changed_files = prepared
        .iter()
        .map(|change| change.audit.clone())
        .collect::<Vec<_>>();
    let record = LearningCapabilityEvolutionApplicationRecord {
        id: format!("lceapp_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: backlog.candidate_id.clone(),
        proposal_id: proposal.id.clone(),
        validation_id: validation.id.clone(),
        implementation_id: implementation.id.clone(),
        capability_id: proposal.capability_id.clone(),
        actor: actor.to_string(),
        summary: format!(
            "Skill Evolution steward prepared dry-run application for implementation `{}`.",
            implementation.id
        ),
        mode: LearningCapabilityEvolutionApplicationMode::DryRun,
        status: LearningCapabilityEvolutionApplicationStatus::Prepared,
        changed_files,
        evidence_refs: implementation.evidence_refs.clone(),
        payload: json!({
            "source": "skill_evolution_steward",
            "target_surface": target_surface.as_str(),
            "target_surface_description": target_surface.description(),
            "target_root": target_root.display().to_string(),
            "apply_requires_operator_approval": true
        }),
        created_at: Utc::now(),
    };
    match api
        .store
        .write_capability_evolution_application_record(&record)
    {
        Ok(()) => {
            counters.dry_runs += 1;
            let _ = api.store.append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type:
                        "learning_capability_steward_application_dry_run_prepared".to_string(),
                    agent_id: backlog.source_agent_id.clone(),
                    task_id: backlog.source_task_id.clone(),
                    execution_id: backlog.source_execution_id.clone(),
                    chat_session_id: backlog.source_chat_session_id.clone(),
                    summary: format!(
                        "Skill Evolution steward prepared dry-run application `{}` for implementation `{}`.",
                        record.id, implementation.id
                    ),
                    evidence_refs: record.evidence_refs.clone(),
                    payload: json!({
                        "candidate_id": backlog.candidate_id.clone(),
                        "proposal_id": proposal.id.clone(),
                        "validation_id": validation.id.clone(),
                        "implementation_id": implementation.id.clone(),
                        "application_id": record.id.clone(),
                        "actor": actor,
                        "changed_file_count": record.changed_files.len()
                    }),
                },
            );
            let mut action = steward_backlog_action(
                "prepare_dry_run",
                LearningCapabilityEvolutionStewardActionStatus::Completed,
                backlog,
                None,
                json!({
                    "changed_files": record
                        .changed_files
                        .iter()
                        .map(|file| file.path.clone())
                        .collect::<Vec<_>>(),
                    "changed_file_count": record.changed_files.len()
                }),
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            action.implementation_id = Some(implementation.id.clone());
            action.application_id = Some(record.id.clone());
            actions.push(action);

            let mut attention = steward_backlog_action(
                "request_apply_approval",
                LearningCapabilityEvolutionStewardActionStatus::AttentionRequired,
                backlog,
                Some("operator_approval_required_before_apply_or_promotion".to_string()),
                json!({
                    "application_id": record.id.clone(),
                    "next_action": "review_dry_run_then_apply_or_reject"
                }),
            );
            attention.proposal_id = Some(proposal.id.clone());
            attention.validation_id = Some(validation.id.clone());
            attention.implementation_id = Some(implementation.id.clone());
            attention.application_id = Some(record.id);
            actions.push(attention);
        },
        Err(error) => {
            let mut action = steward_backlog_action(
                "prepare_dry_run",
                LearningCapabilityEvolutionStewardActionStatus::Failed,
                backlog,
                Some(error.to_string()),
                Value::Null,
            );
            action.proposal_id = Some(proposal.id.clone());
            action.validation_id = Some(validation.id.clone());
            action.implementation_id = Some(implementation.id.clone());
            actions.push(action);
        },
    }
}

fn steward_open_backlog_items(
    store: &LearningStore,
    scope: &LearningScope,
    limit: usize,
) -> Result<Vec<LearningCapabilityEvolutionBacklogItem>, String> {
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for status in [
        LearningCapabilityEvolutionBacklogStatus::Queued,
        LearningCapabilityEvolutionBacklogStatus::InReview,
        LearningCapabilityEvolutionBacklogStatus::Validated,
    ] {
        let page = store
            .list_capability_evolution_backlog_items(
                scope,
                LearningCapabilityEvolutionBacklogFilters {
                    status: Some(status.as_str().to_string()),
                    candidate_type: None,
                    capability_id: None,
                    limit: Some(limit),
                },
            )
            .map_err(|error| error.to_string())?;
        for item in page {
            if seen.insert(item.candidate_id.clone()) {
                items.push(item);
            }
        }
    }
    items.sort_by(|left, right| {
        right
            .rank_score
            .partial_cmp(&left.rank_score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| right.updated_at.cmp(&left.updated_at))
    });
    items.truncate(limit);
    Ok(items)
}

fn steward_proposal_draft_skip_reason(
    backlog: &LearningCapabilityEvolutionBacklogItem,
) -> Option<String> {
    if matches!(
        backlog.risk_level,
        LearningRiskLevel::High | LearningRiskLevel::Critical
    ) {
        return Some(format!(
            "risk_{}_requires_operator_review",
            backlog.risk_level.as_str()
        ));
    }
    if backlog
        .owner_hints
        .iter()
        .any(|hint| hint == "source_code_change_needed")
    {
        return Some("source_code_change_requires_operator_review".to_string());
    }
    if !backlog
        .owner_hints
        .iter()
        .any(|hint| hint == "autonomous_steward_can_draft_validate")
    {
        return Some("owner_hints_do_not_allow_autonomous_steward".to_string());
    }
    None
}

fn latest_passed_validation(
    api: &LearningApi,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Option<LearningCapabilityEvolutionValidationReport> {
    api.store
        .list_capability_evolution_validation_reports(
            scope,
            LearningCapabilityEvolutionValidationFilters {
                status: Some(
                    LearningCapabilityEvolutionValidationStatus::Passed
                        .as_str()
                        .to_string(),
                ),
                candidate_id: Some(backlog.candidate_id.clone()),
                capability_id: proposal.capability_id.clone(),
                limit: Some(5),
            },
        )
        .ok()?
        .into_iter()
        .find(|report| report.proposal_id == proposal.id)
}

fn latest_implementation(
    api: &LearningApi,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Option<LearningCapabilityEvolutionImplementationRecord> {
    api.store
        .list_capability_evolution_implementation_records(
            scope,
            LearningCapabilityEvolutionImplementationFilters {
                candidate_id: Some(backlog.candidate_id.clone()),
                capability_id: proposal.capability_id.clone(),
                limit: Some(5),
            },
        )
        .ok()?
        .into_iter()
        .find(|record| record.proposal_id == proposal.id)
}

fn latest_dry_run_application(
    api: &LearningApi,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    implementation: &LearningCapabilityEvolutionImplementationRecord,
) -> Option<LearningCapabilityEvolutionApplicationRecord> {
    api.store
        .list_capability_evolution_application_records(
            scope,
            LearningCapabilityEvolutionApplicationFilters {
                candidate_id: Some(backlog.candidate_id.clone()),
                capability_id: implementation.capability_id.clone(),
                limit: Some(10),
            },
        )
        .ok()?
        .into_iter()
        .find(|record| {
            record.implementation_id == implementation.id
                && record.mode == LearningCapabilityEvolutionApplicationMode::DryRun
        })
}

fn steward_allowed_validation_prefixes() -> Vec<&'static str> {
    vec![
        "cargo ", "make ", "npm ", "pnpm ", "yarn ", "python ", "python3 ", "./", "scripts/",
    ]
}

fn steward_validation_command_allowlisted(command: &str) -> bool {
    let trimmed = command.trim();
    if trimmed.is_empty() || trimmed.contains('\n') || trimmed.contains('\r') {
        return false;
    }
    if trimmed
        .chars()
        .any(|ch| matches!(ch, '|' | ';' | '&' | '`' | '$' | '<' | '>'))
    {
        return false;
    }
    steward_allowed_validation_prefixes()
        .iter()
        .any(|prefix| trimmed.starts_with(prefix))
}

fn steward_backlog_action(
    action: &str,
    status: LearningCapabilityEvolutionStewardActionStatus,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    reason: Option<String>,
    payload: Value,
) -> LearningCapabilityEvolutionStewardAction {
    steward_action(
        action,
        status,
        Some(backlog.candidate_id.clone()),
        Some(backlog.id.clone()),
        reason,
        payload,
    )
}

fn steward_action(
    action: &str,
    status: LearningCapabilityEvolutionStewardActionStatus,
    candidate_id: Option<String>,
    backlog_id: Option<String>,
    reason: Option<String>,
    payload: Value,
) -> LearningCapabilityEvolutionStewardAction {
    LearningCapabilityEvolutionStewardAction {
        action: action.to_string(),
        status,
        candidate_id,
        backlog_id,
        proposal_id: None,
        evaluation_backlog_id: None,
        validation_id: None,
        implementation_id: None,
        application_id: None,
        reason,
        payload,
    }
}

fn finalize_steward_report(
    scope: LearningScope,
    actor: String,
    budgets: CapabilityStewardBudgets,
    counters: CapabilityStewardCounters,
    actions: Vec<LearningCapabilityEvolutionStewardAction>,
    dry_run: bool,
    request_payload: Value,
    started_at: chrono::DateTime<Utc>,
) -> LearningCapabilityEvolutionStewardRunReport {
    let failed_count = actions
        .iter()
        .filter(|action| action.status == LearningCapabilityEvolutionStewardActionStatus::Failed)
        .count();
    let completed_count = actions
        .iter()
        .filter(|action| action.status == LearningCapabilityEvolutionStewardActionStatus::Completed)
        .count();
    let skipped_count = actions
        .iter()
        .filter(|action| action.status == LearningCapabilityEvolutionStewardActionStatus::Skipped)
        .count();
    let attention_count = actions
        .iter()
        .filter(|action| {
            action.status == LearningCapabilityEvolutionStewardActionStatus::AttentionRequired
        })
        .count();
    let status = if failed_count > 0 {
        LearningCapabilityEvolutionStewardRunStatus::Failed
    } else {
        LearningCapabilityEvolutionStewardRunStatus::Completed
    };
    let summary = format!(
        "Skill Evolution steward {} with {completed_count} completed, {skipped_count} skipped, {attention_count} attention-required, and {failed_count} failed action(s).",
        status.as_str()
    );
    LearningCapabilityEvolutionStewardRunReport {
        id: format!("lcesteward_{}", Uuid::new_v4().simple()),
        scope,
        status,
        actor,
        summary,
        budgets: budgets.as_json(),
        metrics: json!({
            "completed_count": completed_count,
            "skipped_count": skipped_count,
            "attention_required_count": attention_count,
            "failed_count": failed_count,
            "proposal_count": counters.proposals,
            "evaluation_count": counters.evaluations,
            "validation_count": counters.validations,
            "implementation_count": counters.implementations,
            "dry_run_application_count": counters.dry_runs,
            "action_count": actions.len()
        }),
        actions,
        payload: if request_payload.is_null() {
            json!({
                "dry_run": dry_run,
                "attention_integration": "approval-required steps are reported as attention_required actions; UI Attention routing is handled by the operator flow phase."
            })
        } else {
            json!({
                "dry_run": dry_run,
                "attention_integration": "approval-required steps are reported as attention_required actions; UI Attention routing is handled by the operator flow phase.",
                "request_payload": request_payload
            })
        },
        started_at,
        completed_at: Utc::now(),
    }
}

#[derive(Debug, Clone)]
struct PendingCapabilityFileChange {
    path: String,
    operation: String,
    content: String,
}

#[derive(Debug, Clone)]
struct PreparedCapabilityFileChange {
    target: PathBuf,
    audit: LearningCapabilityEvolutionAppliedFile,
}

fn extract_capability_application_file_changes(
    implementation: &LearningCapabilityEvolutionImplementationRecord,
) -> Result<Vec<PendingCapabilityFileChange>, String> {
    let mut changes = Vec::new();
    for patch in &implementation.patches {
        let operation = patch.operation.trim().to_ascii_lowercase();
        if operation.is_empty() {
            return Err(format!("patch for `{}` has an empty operation", patch.path));
        }
        if !capability_application_operation_is_supported(&operation) {
            return Err(format!(
                "patch operation `{}` for `{}` is not supported by the scoped apply path; use create, write, upsert, replace, update, modify, or overwrite with metadata.content/new_content/after/full_content/text/body",
                patch.operation, patch.path
            ));
        }
        let Some(content) = extract_patch_replacement_content(patch)? else {
            return Err(format!(
                "patch `{}` requires full replacement content in metadata.content, metadata.new_content, metadata.after, metadata.full_content, metadata.text, or metadata.body; unified diff application is intentionally not supported",
                patch.path
            ));
        };
        if content.chars().count() > MAX_CAPABILITY_APPLICATION_FILE_CHARS {
            return Err(format!(
                "patch `{}` content exceeds the {} character scoped apply limit",
                patch.path, MAX_CAPABILITY_APPLICATION_FILE_CHARS
            ));
        }
        changes.push(PendingCapabilityFileChange {
            path: patch.path.clone(),
            operation,
            content,
        });
    }
    if changes.is_empty() {
        return Err(
            "implementation bundle has no applicable file patches; record patches with full replacement content before applying"
                .to_string(),
        );
    }
    Ok(changes)
}

fn capability_application_operation_is_supported(operation: &str) -> bool {
    matches!(
        operation,
        "create"
            | "create_file"
            | "write"
            | "write_file"
            | "upsert"
            | "upsert_file"
            | "replace"
            | "replace_file"
            | "update"
            | "update_file"
            | "modify"
            | "modify_file"
            | "edit"
            | "edit_file"
            | "overwrite"
            | "overwrite_file"
    )
}

fn extract_patch_replacement_content(
    patch: &LearningCapabilityEvolutionProposalPatch,
) -> Result<Option<String>, String> {
    for key in [
        "content",
        "new_content",
        "after",
        "full_content",
        "text",
        "body",
    ] {
        if let Some(value) = patch.metadata.get(key) {
            return value
                .as_str()
                .map(|text| Some(text.to_string()))
                .ok_or_else(|| {
                    format!(
                        "patch `{}` metadata.{key} must be a string when present",
                        patch.path
                    )
                });
        }
    }
    Ok(None)
}

fn object_metadata_or_wrapped(value: &Value) -> Value {
    match value {
        Value::Object(_) => value.clone(),
        Value::Null => json!({}),
        other => json!({
            "proposal_patch_metadata": other.clone()
        }),
    }
}

fn merge_json_object_field(target: &mut Value, key: &str, value: Value) {
    match target {
        Value::Object(map) => {
            map.insert(key.to_string(), value);
        },
        Value::Null => {
            *target = json!({ key: value });
        },
        other => {
            let previous = std::mem::take(other);
            *other = json!({
                "previous_value": previous,
                key: value
            });
        },
    }
}

fn phase4_exact_target_files(proposal: &LearningCapabilityEvolutionProposal) -> Vec<String> {
    let mut targets = proposal.proposed_files.clone();
    targets.extend(proposal.patches.iter().map(|patch| patch.path.clone()));
    targets.sort();
    targets.dedup();
    targets
}

fn phase4_rollback_notes(proposal: &LearningCapabilityEvolutionProposal) -> Value {
    json!({
        "strategy": "review application records before apply; scoped apply records previous_content for every changed file and refuses symlinks, parent traversal, hidden segments, node_modules, and large unaudited files",
        "candidate_id": proposal.candidate_id.clone(),
        "proposal_id": proposal.id.clone(),
        "manual_review_required": true
    })
}

fn phase4_docs_changelog_version_requirements(
    proposal: &LearningCapabilityEvolutionProposal,
) -> Value {
    let targets = phase4_exact_target_files(proposal);
    let touches_docs = targets.iter().any(|path| path.starts_with("docs/"));
    let touches_source_or_wrapper = targets.iter().any(|path| {
        path.starts_with("skillshub/")
            || path_targets_tool_schema(path)
            || path_targets_wrapper_or_script(path)
    });
    json!({
        "docs_update": touches_docs || touches_source_or_wrapper,
        "changelog_update": touches_source_or_wrapper,
        "version_bump": false,
        "notes": "Scoped SKILL.md guidance-only changes normally do not require a changelog or version bump; source, schema, wrapper, and docs-touching changes require reviewer decision."
    })
}

fn patch_targets_scoped_skill_guidance(path: &str) -> bool {
    let Some(normalized) = normalized_scoped_skill_path(path) else {
        return false;
    };
    let Some(parts) = normal_path_components(Path::new(&normalized)) else {
        return false;
    };
    parts.len() == 3 && parts[0] == "skills" && parts[2] == "SKILL.md"
}

fn patch_targets_scoped_tool_schema(path: &str) -> bool {
    let Some(normalized) = normalized_scoped_skill_path(path) else {
        return false;
    };
    let Some(parts) = normal_path_components(Path::new(&normalized)) else {
        return false;
    };
    parts.len() == 3 && parts[0] == "skills" && parts[2] == "tool_schema.yaml"
}

fn implementation_bundle_supported_patch_kind(path: &str) -> Option<&'static str> {
    if patch_targets_scoped_skill_guidance(path) {
        Some("skill_guidance")
    } else if patch_targets_scoped_tool_schema(path) {
        Some("tool_schema")
    } else if patch_targets_scoped_wrapper_script(path) {
        Some("wrapper_script")
    } else {
        None
    }
}

fn implementation_bundle_patch_requires_explicit_content_review(artifact_kind: &str) -> bool {
    matches!(artifact_kind, "tool_schema" | "wrapper_script")
}

fn patch_has_reviewed_full_replacement_metadata(
    patch: &LearningCapabilityEvolutionProposalPatch,
) -> bool {
    patch
        .metadata
        .get("reviewed_patch_content")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || patch
            .metadata
            .get("phase4")
            .and_then(|phase4| phase4.get("reviewed_patch_content"))
            .and_then(Value::as_bool)
            .unwrap_or(false)
        || patch
            .metadata
            .get("implementation_bundle_reviewed")
            .and_then(Value::as_bool)
            .unwrap_or(false)
}

fn implementation_bundle_scope_for_patches(
    patches: &[LearningCapabilityEvolutionProposalPatch],
) -> &'static str {
    let has_guidance = patches.iter().any(|patch| {
        patch.metadata.get("artifact_kind").and_then(Value::as_str) == Some("skill_guidance")
    });
    let has_schema = patches.iter().any(|patch| {
        patch.metadata.get("artifact_kind").and_then(Value::as_str) == Some("tool_schema")
    });
    let has_wrapper = patches.iter().any(|patch| {
        patch.metadata.get("artifact_kind").and_then(Value::as_str) == Some("wrapper_script")
    });
    match (has_guidance, has_schema, has_wrapper) {
        (true, true, true) => "low_risk_skill_guidance_tool_schema_and_wrapper_script",
        (true, true, false) => "low_risk_skill_guidance_and_tool_schema",
        (true, false, true) => "low_risk_skill_guidance_and_wrapper_script",
        (false, true, true) => "low_risk_tool_schema_and_wrapper_script",
        (true, false, false) => "low_risk_skill_guidance",
        (false, true, false) => "low_risk_tool_schema",
        (false, false, true) => "low_risk_wrapper_script",
        (false, false, false) => "low_risk_skill_evolution",
    }
}

fn patch_targets_scoped_wrapper_script(path: &str) -> bool {
    let Some(normalized) = normalized_scoped_skill_path(path) else {
        return false;
    };
    let Some(parts) = normal_path_components(Path::new(&normalized)) else {
        return false;
    };
    if parts.len() < 3 || parts[0] != "skills" {
        return false;
    }
    let Some(file_name) = parts.last().map(String::as_str) else {
        return false;
    };
    let script_extension = file_name.ends_with(".py")
        || file_name.ends_with(".sh")
        || file_name.ends_with(".js")
        || file_name.ends_with(".ts");
    if !script_extension {
        return false;
    }
    let under_wrapper_dir =
        parts.len() >= 4 && matches!(parts[2].as_str(), "scripts" | "wrapper" | "wrappers");
    let root_wrapper_file = parts.len() == 3
        && (file_name.contains("wrapper")
            || file_name.starts_with("run_")
            || file_name.starts_with("run-"));
    under_wrapper_dir || root_wrapper_file
}

fn path_targets_tool_schema(path: &str) -> bool {
    normal_path_components(Path::new(path))
        .map(|parts| parts.last().map(String::as_str) == Some("tool_schema.yaml"))
        .unwrap_or(false)
}

fn path_targets_wrapper_or_script(path: &str) -> bool {
    let Some(parts) = normal_path_components(Path::new(path)) else {
        return false;
    };
    parts.iter().any(|part| {
        part == "wrapper"
            || part == "wrappers"
            || part == "scripts"
            || part.ends_with("_wrapper.py")
            || part.ends_with("-wrapper.py")
            || part.ends_with(".sh")
            || part.ends_with(".py")
            || part.ends_with(".ts")
            || part.ends_with(".js")
    })
}

fn scoped_application_required_targets(
    proposal: &LearningCapabilityEvolutionProposal,
    implementation: Option<&LearningCapabilityEvolutionImplementationRecord>,
    promotion_applied_files: &[String],
) -> Vec<String> {
    let mut targets = Vec::new();
    extend_scoped_targets(
        &mut targets,
        proposal.patches.iter().map(|patch| patch.path.as_str()),
    );
    extend_scoped_targets(
        &mut targets,
        proposal.proposed_files.iter().map(String::as_str),
    );
    if let Some(implementation) = implementation {
        extend_scoped_targets(
            &mut targets,
            implementation
                .patches
                .iter()
                .map(|patch| patch.path.as_str()),
        );
        extend_scoped_targets(
            &mut targets,
            implementation.applied_files.iter().map(String::as_str),
        );
    }
    extend_scoped_targets(
        &mut targets,
        promotion_applied_files.iter().map(String::as_str),
    );
    targets.sort();
    targets.dedup();
    targets
}

fn extend_scoped_targets<'a>(targets: &mut Vec<String>, paths: impl IntoIterator<Item = &'a str>) {
    targets.extend(paths.into_iter().filter_map(normalized_scoped_skill_path));
}

fn application_record_contains_scoped_change(
    application: &LearningCapabilityEvolutionApplicationRecord,
) -> bool {
    application
        .changed_files
        .iter()
        .any(|file| capability_application_path_targets_allowed_tree(&file.path))
}

fn application_record_covers_scoped_targets(
    application: &LearningCapabilityEvolutionApplicationRecord,
    targets: &[String],
) -> bool {
    if targets.is_empty() {
        return true;
    }
    application
        .changed_files
        .iter()
        .filter_map(|file| normalized_scoped_skill_path(&file.path))
        .any(|path| targets.iter().any(|target| target == &path))
}

fn application_record_has_failed_catalog_refresh(
    application: &LearningCapabilityEvolutionApplicationRecord,
) -> bool {
    let Some(status) = application
        .payload
        .get("runtime_catalog_refresh")
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .map(|value| value.trim().to_ascii_lowercase())
    else {
        return false;
    };
    matches!(
        status.as_str(),
        "refresh_failed" | "source_discovery_failed"
    )
}

fn capability_validation_has_material_evidence(
    commands: &[String],
    evidence_refs: &[LearningEvidenceRef],
    metrics: &Value,
    payload: &Value,
) -> bool {
    commands.iter().any(|command| !command.trim().is_empty())
        || !evidence_refs.is_empty()
        || value_has_material_validation_evidence(metrics)
        || value_has_material_validation_evidence(payload)
}

fn value_has_material_validation_evidence(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(_) | Value::Number(_) => true,
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

fn skill_evolution_review_evidence_required(
    candidate_risk: &LearningRiskLevel,
    backlog_risk: &LearningRiskLevel,
) -> bool {
    learning_risk_requires_explicit_review(candidate_risk)
        || learning_risk_requires_explicit_review(backlog_risk)
}

fn learning_risk_requires_explicit_review(risk: &LearningRiskLevel) -> bool {
    matches!(risk, LearningRiskLevel::High | LearningRiskLevel::Critical)
}

fn skill_evolution_review_has_material_evidence(
    evidence_refs: &[LearningEvidenceRef],
    payload: &Value,
) -> bool {
    !evidence_refs.is_empty() || value_has_material_validation_evidence(payload)
}

fn promotion_gate_requires_regression(proposal: &LearningCapabilityEvolutionProposal) -> bool {
    proposal
        .promotion_gate
        .as_ref()
        .map(|gate| value_field_truthy(gate, "regression_required"))
        .unwrap_or(false)
}

fn validation_report_has_regression_evidence(
    report: &LearningCapabilityEvolutionValidationReport,
) -> bool {
    value_field_truthy(&report.metrics, "regression_checked")
        || value_field_truthy(&report.payload, "regression_checked")
        || report
            .commands
            .iter()
            .any(|command| command.to_ascii_lowercase().contains("regression"))
}

fn value_field_truthy(value: &Value, field: &str) -> bool {
    match value {
        Value::Object(map) => {
            if let Some(value) = map.get(field) {
                return match value {
                    Value::Bool(flag) => *flag,
                    Value::String(text) => matches!(
                        text.trim().to_ascii_lowercase().as_str(),
                        "true" | "yes" | "required" | "done" | "passed"
                    ),
                    Value::Number(number) => number.as_i64().unwrap_or_default() > 0,
                    Value::Array(items) => !items.is_empty(),
                    Value::Object(map) => !map.is_empty(),
                    Value::Null => false,
                };
            }
            map.values().any(|entry| value_field_truthy(entry, field))
        },
        Value::Array(items) => items.iter().any(|entry| value_field_truthy(entry, field)),
        _ => false,
    }
}

fn skill_evolution_approval_requires_eval_plan(
    proposal: &LearningCapabilityEvolutionProposal,
) -> bool {
    !scoped_application_required_targets(proposal, None, &[]).is_empty()
}

fn skill_evolution_proposal_has_eval_plan(proposal: &LearningCapabilityEvolutionProposal) -> bool {
    proposal
        .eval_plan
        .as_ref()
        .map(value_has_material_plan_content)
        .unwrap_or(false)
}

fn skill_evolution_approval_requires_promotion_gate(
    proposal: &LearningCapabilityEvolutionProposal,
) -> bool {
    !scoped_application_required_targets(proposal, None, &[]).is_empty()
}

fn skill_evolution_proposal_has_promotion_gate(
    proposal: &LearningCapabilityEvolutionProposal,
) -> bool {
    proposal
        .promotion_gate
        .as_ref()
        .map(value_has_material_plan_content)
        .unwrap_or(false)
}

fn value_has_material_plan_content(value: &Value) -> bool {
    match value {
        Value::String(text) => !text.trim().is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn prepare_capability_file_changes(
    target_root: &Path,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
    changes: &[PendingCapabilityFileChange],
) -> Result<Vec<PreparedCapabilityFileChange>, String> {
    let mut prepared = Vec::new();
    for change in changes {
        let target =
            resolve_capability_application_path(target_root, target_surface, &change.path)?;
        let previous_exists = target.exists();
        validate_capability_application_operation(
            &change.operation,
            &change.path,
            previous_exists,
        )?;
        let previous_content = if previous_exists {
            let symlink_metadata = fs::symlink_metadata(&target).map_err(|error| {
                format!("reading metadata for `{}` failed: {error}", change.path)
            })?;
            if symlink_metadata.file_type().is_symlink() {
                return Err(format!(
                    "target `{}` is a symlink; scoped apply refuses symlink writes",
                    change.path
                ));
            }
            let metadata = fs::metadata(&target).map_err(|error| {
                format!("reading metadata for `{}` failed: {error}", change.path)
            })?;
            if !metadata.is_file() {
                return Err(format!(
                    "target `{}` exists but is not a regular file",
                    change.path
                ));
            }
            if metadata.len() as usize > MAX_CAPABILITY_APPLICATION_FILE_CHARS {
                return Err(format!(
                    "target `{}` exceeds the {} byte audit limit; scoped apply refuses unaudited large-file replacement",
                    change.path, MAX_CAPABILITY_APPLICATION_FILE_CHARS
                ));
            }
            Some(fs::read_to_string(&target).map_err(|error| {
                format!(
                    "reading existing target `{}` as UTF-8 text failed: {error}",
                    change.path
                )
            })?)
        } else {
            None
        };
        prepared.push(PreparedCapabilityFileChange {
            target,
            audit: LearningCapabilityEvolutionAppliedFile {
                path: change.path.clone(),
                operation: change.operation.clone(),
                previous_exists,
                previous_content,
                new_content: Some(change.content.clone()),
            },
        });
    }
    Ok(prepared)
}

fn capability_application_target_root(
    workspace_layout: &ArtifactV2Workspace,
    repo_root: &Path,
    scope: &LearningScope,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
) -> PathBuf {
    match target_surface {
        LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill => {
            workspace_layout.scope_root(&scope.principal, &scope.workspace)
        },
        LearningCapabilityEvolutionApplicationTargetSurface::SystemSkill => {
            workspace_layout.system_root()
        },
        LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill => {
            repo_root.join("skillshub")
        },
    }
}

fn apply_prepared_capability_file_changes(
    prepared: &[PreparedCapabilityFileChange],
) -> Result<(), String> {
    for (index, prepared_change) in prepared.iter().enumerate() {
        if let Some(parent) = prepared_change.target.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                let rollback_errors = if index == 0 {
                    Vec::new()
                } else {
                    rollback_prepared_capability_file_changes_through(prepared, index - 1)
                };
                return Err(format!(
                    "creating parent directory for `{}` failed: {error}",
                    prepared_change.audit.path
                ) + &format_rollback_errors(&rollback_errors));
            }
        }
        let content = prepared_change.audit.new_content.as_deref().unwrap_or("");
        if let Err(error) = fs::write(&prepared_change.target, content.as_bytes()) {
            let rollback_errors =
                rollback_prepared_capability_file_changes_through(prepared, index);
            return Err(
                format!("writing `{}` failed: {error}", prepared_change.audit.path)
                    + &format_rollback_errors(&rollback_errors),
            );
        }
    }
    Ok(())
}

fn refresh_runtime_skill_catalog_after_application(
    workspace_layout: &ArtifactV2Workspace,
    repo_root: &Path,
    scope: &LearningScope,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
    application: &LearningCapabilityEvolutionApplicationRecord,
) -> Value {
    let changed_skill_names = changed_skill_names_from_application(application, target_surface);
    match target_surface {
        LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill
        | LearningCapabilityEvolutionApplicationTargetSurface::SystemSkill => {
            let workspace_dir =
                workspace_layout.scope_skills_root(&scope.principal, &scope.workspace);
            let mut search_paths: Vec<std::path::PathBuf> = vec![workspace_dir];
            search_paths.extend(magician::magician_v2::config_extras::extra_skills_dirs());
            let target_root = capability_application_target_root(
                workspace_layout,
                repo_root,
                scope,
                target_surface,
            );
            let pack_defs = magician::magician_v2::execution::load_pack_defs_from_skills_dir(
                &target_root.join("skills"),
            );
            match magician::magician_v2::skills::SkillLoader::new(search_paths).discover() {
                Ok(manifests) => {
                    let procedure_skill_count = manifests
                        .iter()
                        .filter(|manifest| {
                            manifest.inferred_kind()
                                == magician::magician_v2::skills::InferredKind::Procedure
                        })
                        .count();
                    let changed_skill_discovery =
                        phase6_changed_skill_discovery(&manifests, &changed_skill_names);
                    json!({
                        "status": "refreshed",
                        "mechanism": "disk_backed_skill_discovery",
                        "target_surface": target_surface.as_str(),
                        "changed_skill_names": changed_skill_names,
                        "changed_skill_discovery": changed_skill_discovery,
                        "procedure_skill_count": procedure_skill_count,
                        "pack_definition_count_from_target_surface": pack_defs.len(),
                        "runtime_visibility": "scoped chat and agent execution rebuild the scope-effective skill/tool catalog from disk on subsequent turns/executions; this refresh verifies post-apply discovery without requiring process restart."
                    })
                },
                Err(error) => {
                    json!({
                        "status": "refresh_failed",
                        "mechanism": "disk_backed_skill_discovery",
                        "target_surface": target_surface.as_str(),
                        "changed_skill_names": changed_skill_names,
                        "pack_definition_count_from_target_surface": pack_defs.len(),
                        "error": error.to_string(),
                        "runtime_visibility": "runtime skill discovery failed after apply; inspect the changed SKILL.md frontmatter and source paths before promotion."
                    })
                },
            }
        },
        LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill => {
            let source_root = repo_root.join("skillshub");
            let discovery =
                magician::magician_v2::skills::SkillLoader::new(vec![source_root.clone()])
                    .discover()
                    .map(|manifests| {
                        let procedure_skill_count = manifests
                            .iter()
                            .filter(|manifest| {
                                manifest.inferred_kind()
                                    == magician::magician_v2::skills::InferredKind::Procedure
                            })
                            .count();
                        let changed_skill_discovery =
                            phase6_changed_skill_discovery(&manifests, &changed_skill_names);
                        (procedure_skill_count, changed_skill_discovery)
                    });
            match discovery {
                Ok((procedure_skill_count, changed_skill_discovery)) => json!({
                    "status": "source_updated",
                    "mechanism": "source_skill_apply",
                    "target_surface": target_surface.as_str(),
                    "changed_skill_names": changed_skill_names,
                    "changed_skill_discovery": changed_skill_discovery,
                    "source_procedure_skill_count": procedure_skill_count,
                    "runtime_visibility": "source skillshub changes are not live until installed into system or scoped runtime skills; run the skill install/sync path before expecting activate_skill or scoped tool catalogs to see them."
                }),
                Err(error) => json!({
                    "status": "source_discovery_failed",
                    "mechanism": "source_skill_apply",
                    "target_surface": target_surface.as_str(),
                    "changed_skill_names": changed_skill_names,
                    "source_root": source_root.display().to_string(),
                    "error": error.to_string(),
                    "runtime_visibility": "source skillshub changes were written, but source discovery failed; fix the source skill before installing it into runtime layers."
                }),
            }
        },
    }
}

fn phase6_rollback_snapshot(
    changed_files: &[LearningCapabilityEvolutionAppliedFile],
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
    target_root: &Path,
) -> Value {
    let mut created_file_count = 0usize;
    let mut restored_file_count = 0usize;
    let mut restorable_file_count = 0usize;
    let files = changed_files
        .iter()
        .map(|file| {
            let previous_content = file.previous_content.as_deref();
            let new_content = file.new_content.as_deref();
            let rollback_action = if file.previous_exists {
                restored_file_count += 1;
                "restore_previous_content"
            } else {
                created_file_count += 1;
                "remove_created_file"
            };
            if previous_content.is_some() || !file.previous_exists {
                restorable_file_count += 1;
            }
            json!({
                "path": file.path,
                "operation": file.operation,
                "previous_exists": file.previous_exists,
                "previous_content_stored": previous_content.is_some(),
                "previous_size_bytes": previous_content.map(|content| content.len()),
                "previous_sha256": previous_content.map(sha256_hex),
                "new_content_stored": new_content.is_some(),
                "new_size_bytes": new_content.map(|content| content.len()),
                "new_sha256": new_content.map(sha256_hex),
                "rollback_action": rollback_action
            })
        })
        .collect::<Vec<_>>();
    json!({
        "source": "capability_evolution_phase6_rollback_snapshot",
        "target_surface": target_surface.as_str(),
        "target_root": target_root.display().to_string(),
        "file_count": changed_files.len(),
        "created_file_count": created_file_count,
        "restored_file_count": restored_file_count,
        "restorable_file_count": restorable_file_count,
        "content_storage": "full previous/new file bodies remain in application.changed_files; this summary keeps hashes, sizes, and rollback actions for quick review.",
        "files": files
    })
}

fn phase6_changed_skill_discovery(
    manifests: &[magician::magician_v2::skills::SkillManifest],
    changed_skill_names: &[String],
) -> Value {
    let mut discovered = 0usize;
    let skills = changed_skill_names
        .iter()
        .map(|skill_name| {
            let manifest = manifests
                .iter()
                .find(|manifest| manifest.name == *skill_name);
            if let Some(manifest) = manifest {
                discovered += 1;
                json!({
                    "skill_name": skill_name,
                    "discovered": true,
                    "manifest_name": manifest.name,
                    "description": manifest.description,
                    "kind": skill_manifest_kind_as_str(manifest),
                    "source_dir": manifest.source_dir.display().to_string(),
                    "allowed_tools": manifest.allowed_tools,
                    "requires": {
                        "bins": manifest
                            .metadata
                            .magician
                            .as_ref()
                            .map(|metadata| metadata.requires.bins.clone())
                            .unwrap_or_default(),
                        "env": manifest
                            .metadata
                            .magician
                            .as_ref()
                            .map(|metadata| metadata.requires.env.clone())
                            .unwrap_or_default(),
                        "scripts": manifest
                            .metadata
                            .magician
                            .as_ref()
                            .map(|metadata| {
                                let mut names = metadata
                                    .requires
                                    .scripts
                                    .keys()
                                    .cloned()
                                    .collect::<Vec<_>>();
                                names.sort();
                                names
                            })
                            .unwrap_or_default()
                    }
                })
            } else {
                json!({
                    "skill_name": skill_name,
                    "discovered": false,
                    "error": "changed skill was not present in post-apply discovery results"
                })
            }
        })
        .collect::<Vec<_>>();
    json!({
        "source": "capability_evolution_phase6_changed_skill_discovery",
        "changed_skill_count": changed_skill_names.len(),
        "discovered_changed_skill_count": discovered,
        "missing_changed_skill_count": changed_skill_names.len().saturating_sub(discovered),
        "skills": skills
    })
}

fn skill_manifest_kind_as_str(
    manifest: &magician::magician_v2::skills::SkillManifest,
) -> &'static str {
    match manifest.inferred_kind() {
        magician::magician_v2::skills::InferredKind::Procedure => "procedure",
        magician::magician_v2::skills::InferredKind::PersonalityMode => "personality_mode",
    }
}

fn sha256_hex(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    hex::encode(hasher.finalize())
}

struct Phase6RollbackRecommendationTrigger {
    kind: &'static str,
    severity: &'static str,
    source_id: String,
    actor: String,
    summary: String,
    validation_id: Option<String>,
    promotion_id: Option<String>,
    evidence_refs: Vec<LearningEvidenceRef>,
    payload: Value,
}

fn phase6_record_validation_rollback_recommendation(
    api: &LearningApi,
    scope: &LearningScope,
    proposal: &LearningCapabilityEvolutionProposal,
    report: &LearningCapabilityEvolutionValidationReport,
) -> Result<Option<LearningCapabilityEvolutionRollbackRecommendationRecord>, String> {
    let Some(application) = phase6_latest_applied_application(
        api,
        scope,
        &report.candidate_id,
        &proposal.id,
        &report.capability_id,
    )?
    else {
        return Ok(None);
    };
    let (kind, severity) = match report.status {
        LearningCapabilityEvolutionValidationStatus::Failed => {
            ("validation_failed_after_application", "medium")
        },
        LearningCapabilityEvolutionValidationStatus::Blocked => {
            ("validation_blocked_after_application", "medium")
        },
        LearningCapabilityEvolutionValidationStatus::Passed => return Ok(None),
    };
    phase6_record_rollback_recommendation(
        api,
        scope,
        &application,
        Phase6RollbackRecommendationTrigger {
            kind,
            severity,
            source_id: report.id.clone(),
            actor: report.runner.clone(),
            summary: format!(
                "Validation `{}` is `{}` after application `{}`; rollback should be reviewed before further promotion.",
                report.id,
                report.status.as_str(),
                application.id
            ),
            validation_id: Some(report.id.clone()),
            promotion_id: None,
            evidence_refs: report.evidence_refs.clone(),
            payload: json!({
                "validation_id": report.id,
                "validation_status": report.status.as_str(),
                "proposal_id": proposal.id,
                "commands": report.commands,
                "metrics": report.metrics,
                "validation_payload": report.payload
            }),
        },
    )
}

fn phase6_record_validation_rollback_recommendation_outcome(
    api: &LearningApi,
    scope: &LearningScope,
    proposal: &LearningCapabilityEvolutionProposal,
    report: &mut LearningCapabilityEvolutionValidationReport,
) -> (
    Option<LearningCapabilityEvolutionRollbackRecommendationRecord>,
    Option<String>,
) {
    if report.status == LearningCapabilityEvolutionValidationStatus::Passed {
        return (None, None);
    }
    match phase6_record_validation_rollback_recommendation(api, scope, proposal, report) {
        Ok(recommendation) => (recommendation, None),
        Err(error) => {
            tracing::warn!(
                candidate_id = %report.candidate_id,
                proposal_id = %proposal.id,
                validation_id = %report.id,
                error = %error,
                "failed to record Skill Evolution rollback recommendation for non-passing validation"
            );
            let error = error.to_string();
            merge_application_payload_field(
                &mut report.payload,
                "phase6_rollback_recommendation_error",
                json!(error.clone()),
            );
            (None, Some(error))
        },
    }
}

fn phase6_latest_applied_application(
    api: &LearningApi,
    scope: &LearningScope,
    candidate_id: &str,
    proposal_id: &str,
    capability_id: &Option<String>,
) -> Result<Option<LearningCapabilityEvolutionApplicationRecord>, String> {
    let records = api
        .store
        .list_capability_evolution_application_records(
            scope,
            LearningCapabilityEvolutionApplicationFilters {
                candidate_id: Some(candidate_id.to_string()),
                capability_id: capability_id.clone(),
                limit: Some(25),
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(records.into_iter().find(|record| {
        record.mode == LearningCapabilityEvolutionApplicationMode::Apply
            && record.status == LearningCapabilityEvolutionApplicationStatus::Applied
            && record.proposal_id == proposal_id
    }))
}

fn phase6_record_rollback_recommendation(
    api: &LearningApi,
    scope: &LearningScope,
    application: &LearningCapabilityEvolutionApplicationRecord,
    trigger: Phase6RollbackRecommendationTrigger,
) -> Result<Option<LearningCapabilityEvolutionRollbackRecommendationRecord>, String> {
    let existing = api
        .store
        .list_capability_evolution_rollback_recommendation_records(
            scope,
            LearningCapabilityEvolutionRollbackRecommendationFilters {
                status: Some(
                    LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended
                        .as_str()
                        .to_string(),
                ),
                candidate_id: Some(application.candidate_id.clone()),
                capability_id: application.capability_id.clone(),
                application_id: Some(application.id.clone()),
                limit: Some(50),
            },
        )
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|record| {
            record.trigger_kind == trigger.kind
                && record
                    .payload
                    .get("trigger")
                    .and_then(|value| value.get("source_id"))
                    .and_then(Value::as_str)
                    == Some(trigger.source_id.as_str())
        });
    if let Some(existing) = existing {
        return Ok(Some(existing));
    }

    let rollback_files = application
        .changed_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<Vec<_>>();
    let recommendation = LearningCapabilityEvolutionRollbackRecommendationRecord {
        id: format!("lcerollback_{}", Uuid::new_v4().simple()),
        scope: scope.clone(),
        candidate_id: application.candidate_id.clone(),
        proposal_id: application.proposal_id.clone(),
        validation_id: trigger
            .validation_id
            .clone()
            .or_else(|| Some(application.validation_id.clone())),
        implementation_id: Some(application.implementation_id.clone()),
        application_id: application.id.clone(),
        promotion_id: trigger.promotion_id.clone(),
        capability_id: application.capability_id.clone(),
        status: LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended,
        trigger_kind: trigger.kind.to_string(),
        severity: trigger.severity.to_string(),
        actor: trigger.actor,
        summary: trigger.summary,
        rollback_files,
        evidence_refs: trigger.evidence_refs,
        payload: json!({
            "source": "capability_evolution_phase6_rollback_recommender",
            "trigger": {
                "kind": trigger.kind,
                "severity": trigger.severity,
                "source_id": trigger.source_id
            },
            "application": {
                "id": application.id,
                "mode": application.mode.as_str(),
                "status": application.status.as_str(),
                "changed_file_count": application.changed_files.len()
            },
            "phase6_rollback_snapshot": application
                .payload
                .get("phase6_rollback_snapshot")
                .cloned(),
            "runtime_catalog_refresh": application
                .payload
                .get("runtime_catalog_refresh")
                .cloned(),
            "recommended_action": "Review the rollback snapshot, restore previous_content for existing files, and remove created files listed in rollback_files if the regression is confirmed.",
            "trigger_payload": trigger.payload
        }),
        created_at: Utc::now(),
    };
    api.store
        .write_capability_evolution_rollback_recommendation_record(&recommendation)
        .map_err(|error| error.to_string())?;
    Ok(Some(recommendation))
}

fn phase6_rollback_recommendation_summary(
    recommendation: &LearningCapabilityEvolutionRollbackRecommendationRecord,
) -> Value {
    json!({
        "id": recommendation.id,
        "status": recommendation.status.as_str(),
        "trigger_kind": recommendation.trigger_kind,
        "severity": recommendation.severity,
        "application_id": recommendation.application_id,
        "rollback_files": recommendation.rollback_files,
        "created_at": recommendation.created_at
    })
}

struct Phase7PostPromotionMonitorOptions {
    invocation_limit: usize,
    min_after_invocations: u64,
    failure_delta_threshold: f64,
    actor: String,
    request_payload: Value,
}

#[derive(Default)]
struct Phase7InvocationStats {
    invocation_count: u64,
    success_count: u64,
    failure_count: u64,
    failure_classes: HashSet<String>,
    failure_signatures: HashSet<String>,
    evidence_refs: Vec<LearningEvidenceRef>,
}

impl Phase7InvocationStats {
    fn success_rate(&self) -> Option<f64> {
        if self.invocation_count == 0 {
            None
        } else {
            Some(self.success_count as f64 / self.invocation_count as f64)
        }
    }

    fn failure_rate(&self) -> f64 {
        if self.invocation_count == 0 {
            0.0
        } else {
            self.failure_count as f64 / self.invocation_count as f64
        }
    }
}

fn phase7_record_post_promotion_monitor(
    api: &LearningApi,
    scope: &LearningScope,
    promotion: &LearningCapabilityEvolutionPromotionRecord,
    options: Phase7PostPromotionMonitorOptions,
) -> Result<LearningCapabilityEvolutionPostPromotionMonitorRecord, String> {
    let previous_monitor = api
        .store
        .read_capability_evolution_post_promotion_monitor_record(scope, &promotion.id)
        .ok();
    let skill_names = phase7_promoted_skill_names(promotion);
    let evidence = phase7_post_promotion_invocation_evidence(
        api,
        scope,
        &skill_names,
        options.invocation_limit,
    )?;
    let (before, after) = phase7_split_invocation_stats(&evidence, promotion.created_at);
    let same_failure_recurrence_count = after
        .failure_signatures
        .intersection(&before.failure_signatures)
        .count() as u64;
    let mut new_failure_classes = after
        .failure_classes
        .difference(&before.failure_classes)
        .cloned()
        .collect::<Vec<_>>();
    new_failure_classes.sort();
    let user_negative_feedback_count =
        phase7_user_negative_feedback_count(api, scope, &skill_names, promotion.created_at)?;
    let failure_delta = after.failure_rate() - before.failure_rate();
    let has_negative_feedback = user_negative_feedback_count > 0;
    let has_regression_signal = failure_delta >= options.failure_delta_threshold
        || same_failure_recurrence_count > 0
        || !new_failure_classes.is_empty()
        || has_negative_feedback;
    let has_sufficient_evidence =
        after.invocation_count >= options.min_after_invocations || has_negative_feedback;
    let regression_detected = has_sufficient_evidence && has_regression_signal;
    let status = if regression_detected {
        LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
    } else if after.invocation_count == 0 {
        LearningCapabilityEvolutionPostPromotionMonitorStatus::NoUsage
    } else if after.invocation_count < options.min_after_invocations {
        LearningCapabilityEvolutionPostPromotionMonitorStatus::InsufficientEvidence
    } else {
        LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable
    };
    let mut evidence_refs = after
        .evidence_refs
        .iter()
        .chain(before.evidence_refs.iter())
        .cloned()
        .collect::<Vec<_>>();
    evidence_refs.truncate(32);
    let previous_rollback_recommendation_id = previous_monitor
        .as_ref()
        .and_then(|monitor| monitor.rollback_recommendation_id.clone());
    let mut rollback_recommendation_id =
        previous_rollback_recommendation_id
            .as_deref()
            .and_then(|recommendation_id| {
                phase7_active_rollback_recommendation_id(
                    api,
                    scope,
                    &promotion.candidate_id,
                    recommendation_id,
                )
            });
    let previous_follow_up_candidate_id = previous_monitor
        .as_ref()
        .and_then(|monitor| monitor.follow_up_candidate_id.clone());
    let mut follow_up_candidate_id = previous_follow_up_candidate_id
        .as_deref()
        .and_then(|candidate_id| phase7_active_follow_up_candidate_id(api, scope, candidate_id));
    let summary = phase7_monitor_summary(
        &status,
        promotion,
        &skill_names,
        before.failure_rate(),
        after.failure_rate(),
        after.invocation_count,
    );
    let now = Utc::now();
    let mut monitor = LearningCapabilityEvolutionPostPromotionMonitorRecord {
        id: format!("lceppm_{}", promotion.id),
        scope: scope.clone(),
        promotion_id: promotion.id.clone(),
        candidate_id: promotion.candidate_id.clone(),
        proposal_id: promotion.proposal_id.clone(),
        validation_id: promotion.validation_id.clone(),
        implementation_id: promotion.implementation_id.clone(),
        application_id: promotion.application_id.clone(),
        capability_id: promotion.capability_id.clone(),
        status,
        summary,
        skill_names: skill_names.clone(),
        before_invocation_count: before.invocation_count,
        after_invocation_count: after.invocation_count,
        before_success_count: before.success_count,
        after_success_count: after.success_count,
        before_failure_count: before.failure_count,
        after_failure_count: after.failure_count,
        before_success_rate: before.success_rate(),
        after_success_rate: after.success_rate(),
        same_failure_recurrence_count,
        new_failure_classes: new_failure_classes.clone(),
        user_negative_feedback_count,
        rollback_recommendation_id: rollback_recommendation_id.clone(),
        follow_up_candidate_id: follow_up_candidate_id.clone(),
        evidence_refs,
        payload: json!({
            "source": "capability_evolution_phase7_post_promotion_monitor",
            "promotion": {
                "id": promotion.id.clone(),
                "created_at": promotion.created_at,
                "applied_files": promotion.applied_files.clone()
            },
            "options": {
                "invocation_limit": options.invocation_limit,
                "min_after_invocations": options.min_after_invocations,
                "failure_delta_threshold": options.failure_delta_threshold
            },
            "failure_rates": {
                "before": before.failure_rate(),
                "after": after.failure_rate(),
                "delta": failure_delta
            },
            "failure_classes": {
                "before": sorted_set_values(&before.failure_classes),
                "after": sorted_set_values(&after.failure_classes),
                "new": new_failure_classes
            },
            "request_payload": options.request_payload
        }),
        created_at: previous_monitor
            .as_ref()
            .map(|monitor| monitor.created_at)
            .unwrap_or(now),
        updated_at: now,
    };

    if monitor.status == LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected {
        if let Some(application_id) = promotion.application_id.as_deref() {
            if rollback_recommendation_id.is_none() {
                match api.store.read_capability_evolution_application_record(
                    scope,
                    &promotion.candidate_id,
                    application_id,
                ) {
                    Ok(application) => {
                        if let Some(recommendation) =
                            phase7_record_regression_rollback_recommendation(
                                api,
                                scope,
                                &application,
                                &monitor,
                                &options.actor,
                            )?
                        {
                            rollback_recommendation_id = Some(recommendation.id.clone());
                            monitor.rollback_recommendation_id = rollback_recommendation_id.clone();
                            merge_application_payload_field(
                                &mut monitor.payload,
                                "rollback_recommendation",
                                phase6_rollback_recommendation_summary(&recommendation),
                            );
                        }
                    },
                    Err(error) => {
                        tracing::warn!(
                            promotion_id = %promotion.id,
                            application_id = %application_id,
                            error = %error,
                            "post-promotion monitor could not read application record for rollback recommendation"
                        );
                        merge_application_payload_field(
                            &mut monitor.payload,
                            "application_lookup_error",
                            json!({
                                "application_id": application_id,
                                "error": error.to_string(),
                                "remediation": "follow_up_candidate"
                            }),
                        );
                    },
                }
            }
            if rollback_recommendation_id.is_none() && follow_up_candidate_id.is_none() {
                let candidate = phase7_create_regression_follow_up_candidate(
                    api,
                    scope,
                    &monitor,
                    &options.actor,
                )?;
                follow_up_candidate_id = Some(candidate.id.clone());
                monitor.follow_up_candidate_id = follow_up_candidate_id.clone();
                merge_application_payload_field(
                    &mut monitor.payload,
                    "follow_up_candidate_id",
                    json!(candidate.id),
                );
            }
        } else if follow_up_candidate_id.is_none() {
            let candidate =
                phase7_create_regression_follow_up_candidate(api, scope, &monitor, &options.actor)?;
            follow_up_candidate_id = Some(candidate.id.clone());
            monitor.follow_up_candidate_id = follow_up_candidate_id.clone();
            merge_application_payload_field(
                &mut monitor.payload,
                "follow_up_candidate_id",
                json!(candidate.id),
            );
        }
    }

    api.store
        .write_capability_evolution_post_promotion_monitor_record(&monitor)
        .map_err(|error| error.to_string())?;
    api.store
        .append_event(
            scope.clone(),
            CreateLearningEventRequest {
                principal: None,
                workspace: None,
                event_type: "learning_capability_post_promotion_monitor_recorded".to_string(),
                agent_id: None,
                task_id: None,
                execution_id: None,
                chat_session_id: None,
                summary: monitor.summary.clone(),
                evidence_refs: monitor.evidence_refs.clone(),
                payload: json!({
                    "promotion_id": monitor.promotion_id.clone(),
                    "candidate_id": monitor.candidate_id.clone(),
                    "proposal_id": monitor.proposal_id.clone(),
                    "application_id": monitor.application_id.clone(),
                    "capability_id": monitor.capability_id.clone(),
                    "status": monitor.status.as_str(),
                    "skill_names": monitor.skill_names.clone(),
                    "before_invocation_count": monitor.before_invocation_count,
                    "after_invocation_count": monitor.after_invocation_count,
                    "before_success_rate": monitor.before_success_rate,
                    "after_success_rate": monitor.after_success_rate,
                    "same_failure_recurrence_count": monitor.same_failure_recurrence_count,
                    "new_failure_classes": monitor.new_failure_classes.clone(),
                    "user_negative_feedback_count": monitor.user_negative_feedback_count,
                    "rollback_recommendation_id": monitor.rollback_recommendation_id.clone(),
                    "follow_up_candidate_id": monitor.follow_up_candidate_id.clone()
                }),
            },
        )
        .map_err(|error| error.to_string())?;
    Ok(monitor)
}

fn phase7_record_initial_post_promotion_monitor_outcome(
    api: &LearningApi,
    scope: &LearningScope,
    promotion: &LearningCapabilityEvolutionPromotionRecord,
    actor: String,
) -> (
    Option<LearningCapabilityEvolutionPostPromotionMonitorRecord>,
    Option<String>,
) {
    match phase7_record_post_promotion_monitor(
        api,
        scope,
        promotion,
        Phase7PostPromotionMonitorOptions {
            invocation_limit: DEFAULT_PHASE7_MONITOR_INVOCATION_LIMIT,
            min_after_invocations: DEFAULT_PHASE7_MONITOR_MIN_AFTER_INVOCATIONS,
            failure_delta_threshold: DEFAULT_PHASE7_MONITOR_FAILURE_DELTA_THRESHOLD,
            actor,
            request_payload: json!({
                "source": "promotion_recorded_initial_monitor"
            }),
        },
    ) {
        Ok(monitor) => (Some(monitor), None),
        Err(error) => {
            tracing::warn!(
                promotion_id = %promotion.id,
                error = %error,
                "failed to create initial post-promotion monitor"
            );
            (None, Some(error))
        },
    }
}

fn phase7_active_rollback_recommendation_id(
    api: &LearningApi,
    scope: &LearningScope,
    candidate_id: &str,
    recommendation_id: &str,
) -> Option<String> {
    match api
        .store
        .read_capability_evolution_rollback_recommendation_record(
            scope,
            candidate_id,
            recommendation_id,
        ) {
        Ok(recommendation)
            if recommendation.status
                == LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended =>
        {
            Some(recommendation.id)
        },
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(
                candidate_id = %candidate_id,
                recommendation_id = %recommendation_id,
                error = %error,
                "post-promotion monitor ignored stale rollback recommendation link"
            );
            None
        },
    }
}

fn phase7_active_follow_up_candidate_id(
    api: &LearningApi,
    scope: &LearningScope,
    candidate_id: &str,
) -> Option<String> {
    match api.store.read_candidate(scope, candidate_id) {
        Ok(candidate) if !candidate.state.is_terminal() => Some(candidate.id),
        Ok(_) => None,
        Err(error) => {
            tracing::warn!(
                candidate_id = %candidate_id,
                error = %error,
                "post-promotion monitor ignored stale follow-up candidate link"
            );
            None
        },
    }
}

fn phase7_promoted_skill_names(
    promotion: &LearningCapabilityEvolutionPromotionRecord,
) -> Vec<String> {
    let mut names = Vec::new();
    if let Some(capability_id) = promotion.capability_id.as_deref() {
        if let Some(name) = capability_id.strip_prefix("skill:") {
            if !name.trim().is_empty() {
                names.push(name.trim().to_string());
            }
        }
    }
    for path in &promotion.applied_files {
        if let Some(name) = skill_name_from_promotion_path(path) {
            names.push(name);
        }
    }
    names.sort();
    names.dedup();
    names
}

fn skill_name_from_promotion_path(path: &str) -> Option<String> {
    for prefix in ["skills/", "skillshub/"] {
        let Some(rest) = path.strip_prefix(prefix) else {
            continue;
        };
        let name = rest
            .split('/')
            .next()
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        return Some(name.to_string());
    }
    None
}

fn phase7_post_promotion_invocation_evidence(
    api: &LearningApi,
    scope: &LearningScope,
    skill_names: &[String],
    invocation_limit: usize,
) -> Result<Vec<LearningSkillInvocationEvidence>, String> {
    let mut by_id = HashMap::new();
    for skill_name in skill_names {
        let records = api
            .store
            .list_skill_invocation_evidence(
                scope,
                magician::magician_v2::learning::LearningSkillInvocationEvidenceFilters {
                    skill_name: Some(skill_name.clone()),
                    limit: Some(invocation_limit),
                    ..Default::default()
                },
            )
            .map_err(|error| error.to_string())?;
        for record in records {
            by_id.entry(record.id.clone()).or_insert(record);
        }
    }
    let mut records = by_id.into_values().collect::<Vec<_>>();
    records.sort_by(|left, right| right.created_at.cmp(&left.created_at));
    Ok(records)
}

fn phase7_split_invocation_stats(
    records: &[LearningSkillInvocationEvidence],
    promotion_at: chrono::DateTime<Utc>,
) -> (Phase7InvocationStats, Phase7InvocationStats) {
    let mut before = Phase7InvocationStats::default();
    let mut after = Phase7InvocationStats::default();
    for record in records {
        let stats = if record.created_at < promotion_at {
            &mut before
        } else {
            &mut after
        };
        stats.invocation_count += 1;
        if record.status == LearningSkillInvocationStatus::Succeeded {
            stats.success_count += 1;
        } else {
            stats.failure_count += 1;
            if let Some(failure_class) = record.failure_class.as_ref() {
                let class = failure_class.as_str().to_string();
                stats.failure_classes.insert(class.clone());
                stats
                    .failure_signatures
                    .insert(format!("{}:{}", class, record.input_fingerprint));
            }
        }
        if stats.evidence_refs.len() < 24 {
            stats.evidence_refs.push(LearningEvidenceRef {
                kind: "skill_invocation_evidence".to_string(),
                id: Some(record.id.clone()),
                path: None,
                uri: None,
                summary: Some(format!(
                    "{} invocation for `{}` at {}",
                    record.status.as_str(),
                    record.skill_name,
                    record.created_at
                )),
            });
        }
    }
    (before, after)
}

fn phase7_user_negative_feedback_count(
    api: &LearningApi,
    scope: &LearningScope,
    skill_names: &[String],
    promotion_at: chrono::DateTime<Utc>,
) -> Result<u64, String> {
    let events = api
        .store
        .list_events(scope, 500)
        .map_err(|error| error.to_string())?;
    Ok(events
        .into_iter()
        .filter(|event| event.created_at >= promotion_at)
        .filter(|event| event.event_type == "learning_user_teaching_recorded")
        .filter(|event| {
            let action = event.payload.get("action").and_then(Value::as_str);
            matches!(action, Some("this_was_wrong" | "correct" | "never_do_this"))
        })
        .filter(|event| phase7_event_mentions_skill(&event.payload, skill_names))
        .count() as u64)
}

fn phase7_event_mentions_skill(payload: &Value, skill_names: &[String]) -> bool {
    if skill_names.is_empty() {
        return false;
    }
    let haystack = serde_json::to_string(payload)
        .unwrap_or_default()
        .to_ascii_lowercase();
    skill_names
        .iter()
        .any(|name| haystack.contains(&name.to_ascii_lowercase()))
}

fn phase7_monitor_summary(
    status: &LearningCapabilityEvolutionPostPromotionMonitorStatus,
    promotion: &LearningCapabilityEvolutionPromotionRecord,
    skill_names: &[String],
    before_failure_rate: f64,
    after_failure_rate: f64,
    after_count: u64,
) -> String {
    let skill_label = if skill_names.is_empty() {
        promotion
            .capability_id
            .clone()
            .unwrap_or_else(|| "unknown skill".to_string())
    } else {
        skill_names.join(", ")
    };
    match status {
        LearningCapabilityEvolutionPostPromotionMonitorStatus::NoUsage => format!(
            "Promotion `{}` has no post-promotion invocation evidence yet for {}.",
            promotion.id, skill_label
        ),
        LearningCapabilityEvolutionPostPromotionMonitorStatus::InsufficientEvidence => format!(
            "Promotion `{}` has only {} post-promotion invocation(s) for {}; keep monitoring.",
            promotion.id, after_count, skill_label
        ),
        LearningCapabilityEvolutionPostPromotionMonitorStatus::Stable => format!(
            "Promotion `{}` is stable for {}; failure rate {:.0}% -> {:.0}%.",
            promotion.id,
            skill_label,
            before_failure_rate * 100.0,
            after_failure_rate * 100.0
        ),
        LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected => format!(
            "Promotion `{}` shows post-promotion regression for {}; failure rate {:.0}% -> {:.0}%.",
            promotion.id,
            skill_label,
            before_failure_rate * 100.0,
            after_failure_rate * 100.0
        ),
        LearningCapabilityEvolutionPostPromotionMonitorStatus::Pending => format!(
            "Promotion `{}` is pending post-promotion monitoring for {}.",
            promotion.id, skill_label
        ),
    }
}

fn phase7_record_regression_rollback_recommendation(
    api: &LearningApi,
    scope: &LearningScope,
    application: &LearningCapabilityEvolutionApplicationRecord,
    monitor: &LearningCapabilityEvolutionPostPromotionMonitorRecord,
    actor: &str,
) -> Result<Option<LearningCapabilityEvolutionRollbackRecommendationRecord>, String> {
    phase6_record_rollback_recommendation(
        api,
        scope,
        application,
        Phase6RollbackRecommendationTrigger {
            kind: "post_promotion_regression",
            severity: if monitor.user_negative_feedback_count > 0 {
                "high"
            } else {
                "medium"
            },
            source_id: monitor.id.clone(),
            actor: actor.to_string(),
            summary: format!(
                "Post-promotion monitoring found regression after promotion `{}`; rollback should be reviewed.",
                monitor.promotion_id
            ),
            validation_id: Some(monitor.validation_id.clone()),
            promotion_id: Some(monitor.promotion_id.clone()),
            evidence_refs: monitor.evidence_refs.clone(),
            payload: json!({
                "post_promotion_monitor_id": monitor.id.clone(),
                "promotion_id": monitor.promotion_id.clone(),
                "status": monitor.status.as_str(),
                "skill_names": monitor.skill_names.clone(),
                "after_invocation_count": monitor.after_invocation_count,
                "before_success_rate": monitor.before_success_rate,
                "after_success_rate": monitor.after_success_rate,
                "same_failure_recurrence_count": monitor.same_failure_recurrence_count,
                "new_failure_classes": monitor.new_failure_classes.clone(),
                "user_negative_feedback_count": monitor.user_negative_feedback_count
            }),
        },
    )
}

fn phase7_create_regression_follow_up_candidate(
    api: &LearningApi,
    scope: &LearningScope,
    monitor: &LearningCapabilityEvolutionPostPromotionMonitorRecord,
    actor: &str,
) -> Result<magician::magician_v2::learning::LearningCandidate, String> {
    api.store
        .create_candidate(
            scope.clone(),
            CreateLearningCandidateRequest {
                principal: None,
                workspace: None,
                candidate_type: LearningCandidateType::SkillUpdate,
                state: LearningCandidateState::Proposed,
                title: format!(
                    "Investigate post-promotion regression for `{}`",
                    monitor
                        .capability_id
                        .clone()
                        .unwrap_or_else(|| monitor.skill_names.join(", "))
                ),
                summary: monitor.summary.clone(),
                rationale: "Phase 7 post-promotion monitoring detected degraded real invocation evidence after a promotion.".to_string(),
                proposed_change: json!({
                    "source": "capability_evolution_phase7_post_promotion_monitor",
                    "skill_update": {
                        "promotion_id": monitor.promotion_id.clone(),
                        "monitor_id": monitor.id.clone(),
                        "skill_names": monitor.skill_names.clone(),
                        "new_failure_classes": monitor.new_failure_classes.clone(),
                        "same_failure_recurrence_count": monitor.same_failure_recurrence_count,
                        "user_negative_feedback_count": monitor.user_negative_feedback_count
                    }
                }),
                proposed_target: monitor.capability_id.clone(),
                confidence: Some(0.82),
                source_agent_id: Some(actor.to_string()),
                source_task_id: None,
                source_execution_id: None,
                source_chat_session_id: None,
                event_refs: Vec::new(),
                evidence_refs: monitor.evidence_refs.clone(),
                risk_level: LearningRiskLevel::High,
                review_required: true,
                review_reason: Some(
                    "Post-promotion regression must be reviewed before another skill change is applied."
                        .to_string(),
                ),
                review_policy: json!({
                    "source": "phase7_post_promotion_monitor",
                    "requires_operator_review": true
                }),
                promotion_target: monitor.capability_id.clone(),
                promotion_policy: json!({
                    "requires_capability_evolution_review": true
                }),
            },
        )
        .map_err(|error| error.to_string())
}

fn sorted_set_values(values: &HashSet<String>) -> Vec<String> {
    let mut out = values.iter().cloned().collect::<Vec<_>>();
    out.sort();
    out
}

fn changed_skill_names_from_application(
    application: &LearningCapabilityEvolutionApplicationRecord,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
) -> Vec<String> {
    let mut names = application
        .changed_files
        .iter()
        .filter_map(|file| skill_name_from_application_path(&file.path, target_surface))
        .collect::<Vec<_>>();
    names.sort();
    names.dedup();
    names
}

fn skill_name_from_application_path(
    relative_path: &str,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
) -> Option<String> {
    let parts = normal_path_components(Path::new(relative_path))?;
    match target_surface {
        LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill
        | LearningCapabilityEvolutionApplicationTargetSurface::SystemSkill => {
            (parts.len() >= 2 && parts[0] == "skills").then(|| parts[1].clone())
        },
        LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill => {
            if parts.len() >= 2 && parts[0] == "skillshub" {
                Some(parts[1].clone())
            } else if parts.len() >= 2 && parts[0] == "skills" {
                Some(parts[1].clone())
            } else {
                parts.first().cloned()
            }
        },
    }
}

fn merge_application_payload_field(payload: &mut Value, key: &str, value: Value) {
    match payload {
        Value::Object(map) => {
            map.insert(key.to_string(), value);
        },
        Value::Null => {
            *payload = json!({ key: value });
        },
        other => {
            let previous = std::mem::take(other);
            *other = json!({
                "request_payload": previous,
                key: value
            });
        },
    }
}

fn rollback_prepared_capability_file_changes(
    prepared: &[PreparedCapabilityFileChange],
) -> Vec<String> {
    if prepared.is_empty() {
        return Vec::new();
    }
    rollback_prepared_capability_file_changes_through(prepared, prepared.len() - 1)
}

fn rollback_prepared_capability_file_changes_through(
    prepared: &[PreparedCapabilityFileChange],
    through_index: usize,
) -> Vec<String> {
    if prepared.is_empty() {
        return Vec::new();
    }
    let mut errors = Vec::new();
    let capped_index = through_index.min(prepared.len().saturating_sub(1));
    for prepared_change in prepared.iter().take(capped_index + 1).rev() {
        let result = if let Some(previous_content) = prepared_change.audit.previous_content.as_ref()
        {
            fs::write(&prepared_change.target, previous_content.as_bytes())
        } else if prepared_change.target.exists() {
            fs::remove_file(&prepared_change.target)
        } else {
            Ok(())
        };
        if let Err(error) = result {
            errors.push(format!(
                "rollback for `{}` failed: {error}",
                prepared_change.audit.path
            ));
        }
    }
    errors
}

fn format_rollback_errors(errors: &[String]) -> String {
    if errors.is_empty() {
        "; rollback completed".to_string()
    } else {
        format!("; rollback errors: {}", errors.join("; "))
    }
}

fn validate_capability_application_operation(
    operation: &str,
    path: &str,
    previous_exists: bool,
) -> Result<(), String> {
    match operation {
        "create" | "create_file" if previous_exists => Err(format!(
            "target `{path}` already exists; create operations require a new file"
        )),
        "replace" | "replace_file" | "update" | "update_file" | "modify" | "modify_file"
        | "edit" | "edit_file"
            if !previous_exists =>
        {
            Err(format!(
                "target `{path}` does not exist; {operation} operations require an existing file"
            ))
        },
        _ => Ok(()),
    }
}

fn resolve_capability_application_path(
    target_root: &Path,
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
    relative_path: &str,
) -> Result<PathBuf, String> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() {
        return Err("capability application path cannot be empty".to_string());
    }
    let relative = Path::new(trimmed);
    if relative.is_absolute() {
        return Err(format!(
            "capability application path `{trimmed}` must be relative to the scoped workspace root"
        ));
    }
    for component in relative.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!(
                    "capability application path `{trimmed}` cannot contain parent traversal or root prefixes"
                ));
            },
        }
    }
    let normalized_path =
        normalize_capability_application_path_for_surface(target_surface, relative)?;
    let target = target_root.join(&normalized_path);
    if let Err(error) = std::fs::create_dir_all(target_root) {
        return Err(format!(
            "capability application target root `{}` is not accessible: {error}",
            target_root.display()
        ));
    }
    let canonical_root = target_root.canonicalize().map_err(|error| {
        format!(
            "capability application target root `{}` could not be canonicalized: {error}",
            target_root.display()
        )
    })?;
    let parent = target
        .parent()
        .ok_or_else(|| format!("capability application path `{trimmed}` has no parent"))?;
    let nearest_existing =
        nearest_existing_ancestor(parent).unwrap_or_else(|| canonical_root.clone());
    let canonical_parent = nearest_existing.canonicalize().map_err(|error| {
        format!("capability application path `{trimmed}` could not be canonicalized: {error}")
    })?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(format!(
            "capability application path `{trimmed}` escapes the {} root",
            target_surface.description()
        ));
    }
    Ok(target)
}

fn normalize_capability_application_path_for_surface(
    target_surface: LearningCapabilityEvolutionApplicationTargetSurface,
    relative: &Path,
) -> Result<PathBuf, String> {
    let first_normal_component = first_normal_path_component(relative)
        .ok_or_else(|| "capability application path must target a skill file".to_string())?;
    match target_surface {
        LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill
        | LearningCapabilityEvolutionApplicationTargetSurface::SystemSkill => {
            let parts = normal_path_components(relative).ok_or_else(|| {
                format!(
                    "capability application path `{}` cannot contain parent traversal or root prefixes",
                    relative.display()
                )
            })?;
            if !CAPABILITY_APPLICATION_ALLOWED_TOP_LEVEL_DIRS
                .contains(&first_normal_component.as_str())
            {
                return Err(format!(
                    "capability application path `{}` must stay under skills/ for {}; refused top-level directory `{first_normal_component}`",
                    relative.display(),
                    target_surface.description()
                ));
            }
            validate_capability_application_safe_segments(&parts).map_err(|error| {
                format!(
                    "capability application path `{}` {error}",
                    relative.display()
                )
            })?;
            Ok(relative.to_path_buf())
        },
        LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill => {
            let parts = normal_path_components(relative).ok_or_else(|| {
                format!(
                    "capability application path `{}` cannot contain parent traversal or root prefixes",
                    relative.display()
                )
            })?;
            let source_parts = if parts.len() >= 3 && parts[0] == "skills" {
                parts[1..].to_vec()
            } else if parts.len() >= 3 && parts[0] == "skillshub" {
                parts[1..].to_vec()
            } else {
                return Err(format!(
                    "source skill applications must use skills/<skill>/... or skillshub/<skill>/... paths; got `{}`",
                    relative.display()
                ));
            };
            let skill = source_parts[0].as_str();
            if skill.is_empty() || skill.starts_with('.') || skill.contains("..") {
                return Err(format!("source skill name `{skill}` is not allowed"));
            }
            validate_capability_application_safe_segments(&source_parts).map_err(|error| {
                format!(
                    "source skill application path `{}` {error}",
                    relative.display()
                )
            })?;
            let mut normalized = PathBuf::new();
            for part in &source_parts {
                normalized.push(part);
            }
            Ok(normalized)
        },
    }
}

fn capability_application_path_targets_allowed_tree(relative_path: &str) -> bool {
    normalized_scoped_skill_path(relative_path).is_some()
}

fn normalized_scoped_skill_path(relative_path: &str) -> Option<String> {
    let trimmed = relative_path.trim();
    if trimmed.is_empty() {
        return None;
    }
    let relative = Path::new(trimmed);
    if relative.is_absolute() {
        return None;
    }
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str()?),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        return None;
    }
    if !CAPABILITY_APPLICATION_ALLOWED_TOP_LEVEL_DIRS.contains(&parts[0]) {
        return None;
    }
    validate_capability_application_safe_segments(&parts).ok()?;
    Some(parts.join("/"))
}

fn first_normal_path_component(relative: &Path) -> Option<String> {
    relative.components().find_map(|component| match component {
        Component::Normal(value) => value.to_str().map(str::to_string),
        _ => None,
    })
}

fn normal_path_components(relative: &Path) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for component in relative.components() {
        match component {
            Component::Normal(value) => parts.push(value.to_str()?.to_string()),
            Component::CurDir => {},
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts)
    }
}

fn validate_capability_application_safe_segments<S: AsRef<str>>(parts: &[S]) -> Result<(), String> {
    if parts.iter().any(|part| part.as_ref().is_empty()) {
        return Err("contains an empty path segment".to_string());
    }
    if let Some(part) = parts
        .iter()
        .map(AsRef::as_ref)
        .find(|part| part.starts_with('.'))
    {
        return Err(format!("contains hidden path segment `{part}`"));
    }
    if parts.iter().any(|part| part.as_ref() == "node_modules") {
        return Err("contains node_modules segment".to_string());
    }
    Ok(())
}

fn nearest_existing_ancestor(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.exists())
        .map(Path::to_path_buf)
}

fn optional_non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
}

fn read_string_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(|entry| match entry {
            Value::String(text) => Some(text.trim().to_string()).filter(|text| !text.is_empty()),
            Value::Number(number) => Some(number.to_string()),
            Value::Bool(flag) => Some(flag.to_string()),
            _ => None,
        })
}

fn normalize_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace('-', "_")
}

fn default_review_actor() -> String {
    "operator".to_string()
}

fn default_steward_actor() -> String {
    "skill_evolution_steward".to_string()
}

fn default_post_promotion_monitor_actor() -> String {
    "skill_evolution_monitor".to_string()
}

#[derive(Debug, Clone, Serialize)]
struct ValidationCommandSpec {
    command: String,
    source: String,
    regression: bool,
}

#[derive(Debug, Clone, Serialize)]
struct ValidationCommandResult {
    command: String,
    source: String,
    regression: bool,
    success: bool,
    status_code: Option<i32>,
    timed_out: bool,
    spawn_error: Option<String>,
    duration_ms: u128,
    stdout: String,
    stderr: String,
}

fn draft_capability_evolution_proposal(
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    existing: Option<&LearningCapabilityEvolutionProposal>,
    generated_by: &str,
    ready_for_review: bool,
) -> LearningCapabilityEvolutionProposal {
    let now = Utc::now();
    let proposed_files = existing
        .map(|proposal| proposal.proposed_files.clone())
        .filter(|files| !files.is_empty())
        .unwrap_or_else(|| {
            if backlog.proposed_files.is_empty() {
                infer_scoped_skill_proposed_files(backlog)
            } else {
                backlog.proposed_files.clone()
            }
        });
    LearningCapabilityEvolutionProposal {
        id: existing
            .map(|proposal| proposal.id.clone())
            .unwrap_or_else(|| format!("lcep_{}", backlog.candidate_id.trim_start_matches("lc_"))),
        scope: scope.clone(),
        candidate_id: backlog.candidate_id.clone(),
        backlog_id: backlog.id.clone(),
        status: existing
            .map(|proposal| proposal.status.clone())
            .unwrap_or_else(|| {
                if ready_for_review {
                    LearningCapabilityEvolutionProposalStatus::ReadyForReview
                } else {
                    LearningCapabilityEvolutionProposalStatus::Draft
                }
            }),
        title: existing
            .map(|proposal| proposal.title.clone())
            .unwrap_or_else(|| format!("Review fix proposal: {}", backlog.title)),
        summary: existing
            .map(|proposal| proposal.summary.clone())
            .unwrap_or_else(|| backlog.summary.clone()),
        capability_id: existing
            .and_then(|proposal| proposal.capability_id.clone())
            .or_else(|| backlog.capability_id.clone()),
        proposed_fix_type: existing
            .and_then(|proposal| proposal.proposed_fix_type.clone())
            .or_else(|| backlog.proposed_fix_type.clone()),
        proposed_files,
        change_plan: existing
            .map(|proposal| proposal.change_plan.clone())
            .unwrap_or_else(|| default_capability_change_plan(backlog)),
        patches: existing
            .map(|proposal| proposal.patches.clone())
            .unwrap_or_default(),
        eval_plan: existing
            .and_then(|proposal| proposal.eval_plan.clone())
            .or_else(|| backlog.required_eval.clone())
            .or_else(|| default_capability_eval_plan(backlog)),
        validation_plan: existing
            .and_then(|proposal| proposal.validation_plan.clone())
            .or_else(|| default_capability_validation_plan(backlog)),
        promotion_gate: existing
            .and_then(|proposal| proposal.promotion_gate.clone())
            .or_else(|| backlog.promotion_gate.clone())
            .or_else(|| default_capability_promotion_gate(backlog)),
        generated_by: generated_by.to_string(),
        created_at: existing.map(|proposal| proposal.created_at).unwrap_or(now),
        updated_at: now,
    }
}

fn enrich_capability_evolution_proposal_draft(
    workspace_layout: &ArtifactV2Workspace,
    repo_root: &Path,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &mut LearningCapabilityEvolutionProposal,
) {
    if proposal.patches.is_empty() {
        proposal.patches =
            draft_capability_proposal_patches(workspace_layout, scope, backlog, proposal);
    }
    enrich_capability_phase4_metadata(backlog, proposal);
    enrich_capability_phase5_validation_plan(workspace_layout, repo_root, scope, backlog, proposal);
    if let Some(eval_plan) = proposal.eval_plan.as_mut() {
        if let Value::Object(map) = eval_plan {
            map.entry("generated_eval_backlog".to_string()).or_insert_with(|| {
                json!({
                    "route": format!(
                        "/api/magician/v2/learning/skill-evolution/proposals/{}/evaluation/generate",
                        proposal.candidate_id
                    ),
                    "purpose": "Create a meta-harness/evaluation backlog item from this eval_plan before or during validation."
                })
            });
        }
    }
}

fn draft_capability_proposal_patches(
    workspace_layout: &ArtifactV2Workspace,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Vec<LearningCapabilityEvolutionProposalPatch> {
    let mut paths = proposal
        .proposed_files
        .iter()
        .filter_map(|path| skill_guidance_path_for_capability_target(path))
        .collect::<Vec<_>>();
    if let Some(skill_name) = proposal
        .capability_id
        .as_deref()
        .and_then(skill_name_from_capability_id)
    {
        paths.push(format!("skills/{skill_name}/SKILL.md"));
    }
    paths.sort();
    paths.dedup();

    let scope_root = workspace_layout.scope_root(&scope.principal, &scope.workspace);
    let mut patches = paths
        .into_iter()
        .filter_map(|path| {
            let target = resolve_capability_application_path(
                &scope_root,
                LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
                &path,
            )
            .ok()?;
            let previous_content = fs::read_to_string(&target).unwrap_or_else(|_| {
                default_skill_guidance_file_content(&path, proposal)
            });
            let marker = format!(
                "{}{} -->",
                LEARNING_SKILL_GUIDANCE_MARKER_PREFIX, proposal.candidate_id
            );
            let section = build_learning_skill_guidance_section(&marker, backlog, proposal);
            let new_content =
                append_or_replace_marked_section(&previous_content, &marker, &section);
            Some(LearningCapabilityEvolutionProposalPatch {
                path,
                operation: if target.exists() {
                    "update".to_string()
                } else {
                    "create".to_string()
                },
                summary: "Add durable skill guidance from the observed learning candidate."
                    .to_string(),
                diff: Some(
                    "Full replacement patch generated from the capability-evolution backlog; review metadata.new_content before bundling."
                        .to_string(),
                ),
                metadata: json!({
                    "generated_by": "learning_capability_evolution_proposal_drafter",
                    "target_surface": "scoped_skill",
                    "previous_exists": target.exists(),
                    "content_kind": "full_replacement",
                    "new_content": new_content
                }),
            })
        })
        .collect::<Vec<_>>();
    patches.extend(draft_capability_proposal_schema_wrapper_patch_scaffolds(
        &scope_root,
        backlog,
        proposal,
    ));
    patches
}

fn draft_capability_proposal_schema_wrapper_patch_scaffolds(
    scope_root: &Path,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Vec<LearningCapabilityEvolutionProposalPatch> {
    let mut paths = proposal
        .proposed_files
        .iter()
        .filter_map(|path| normalized_scoped_skill_path(path))
        .filter(|path| {
            patch_targets_scoped_tool_schema(path) || patch_targets_scoped_wrapper_script(path)
        })
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();

    paths
        .into_iter()
        .filter_map(|path| {
            let artifact_kind = implementation_bundle_supported_patch_kind(&path)?;
            let target = resolve_capability_application_path(
                scope_root,
                LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
                &path,
            )
            .ok()?;
            let previous_exists = target.exists();
            let previous_content = match fs::read_to_string(&target) {
                Ok(content) => content,
                Err(_) if previous_exists => return None,
                Err(_) => String::new(),
            };
            let generated = generate_phase4_schema_wrapper_scaffold_content(
                artifact_kind,
                &path,
                &previous_content,
                backlog,
                proposal,
            );
            let (summary, review_instruction, diff) = match artifact_kind {
                "tool_schema" => (
                    "Scaffold reviewed tool schema full-replacement patch.",
                    "Review or replace metadata.new_content with the full tool_schema.yaml content, run schema validation, then set metadata.reviewed_patch_content=true before implementation bundle drafting.",
                    "Evidence-aware full replacement schema scaffold generated from the Skill Evolution backlog; review metadata.new_content and set reviewed_patch_content=true before bundling.",
                ),
                "wrapper_script" => (
                    "Scaffold reviewed wrapper script full-replacement patch.",
                    "Review or replace metadata.new_content with the full wrapper script content, run wrapper smoke validation, then set metadata.reviewed_patch_content=true before implementation bundle drafting.",
                    "Evidence-aware full replacement wrapper scaffold generated from the Skill Evolution backlog; review metadata.new_content and set reviewed_patch_content=true before bundling.",
                ),
                _ => return None,
            };
            Some(LearningCapabilityEvolutionProposalPatch {
                path,
                operation: if previous_exists {
                    "update".to_string()
                } else {
                    "create".to_string()
                },
                summary: summary.to_string(),
                diff: Some(diff.to_string()),
                metadata: json!({
                    "generated_by": "learning_capability_evolution_proposal_drafter",
                    "target_surface": "scoped_skill",
                    "previous_exists": previous_exists,
                    "content_kind": "full_replacement",
                    "artifact_kind": artifact_kind,
                    "patch_scaffold": true,
                    "reviewed_patch_content": false,
                    "review_instruction": review_instruction,
                    "phase4_content_generation": generated.metadata,
                    "new_content": generated.content
                }),
            })
        })
        .collect()
}

struct Phase4GeneratedPatchScaffold {
    content: String,
    metadata: Value,
}

fn generate_phase4_schema_wrapper_scaffold_content(
    artifact_kind: &str,
    path: &str,
    previous_content: &str,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Phase4GeneratedPatchScaffold {
    let base_content = if previous_content.trim().is_empty() {
        match artifact_kind {
            "tool_schema" => default_phase4_tool_schema_content(path, proposal),
            "wrapper_script" => default_phase4_wrapper_script_content(path, proposal),
            _ => String::new(),
        }
    } else {
        previous_content.to_string()
    };
    let marker = format!("{}{}", PHASE4_REVIEW_MARKER_PREFIX, proposal.candidate_id);
    let suggested_actions = phase4_suggested_review_actions(artifact_kind);
    let mut review_lines =
        phase4_review_scaffold_lines(artifact_kind, path, backlog, proposal, &suggested_actions);
    let local_artifact = match artifact_kind {
        "tool_schema" => {
            let introspection = phase4_tool_schema_introspection(&base_content);
            if let Some(summary) = phase4_local_schema_summary(&introspection) {
                review_lines.push(summary);
            }
            introspection
        },
        "wrapper_script" => {
            let introspection = phase4_wrapper_script_introspection(path, &base_content);
            if let Some(summary) = phase4_local_wrapper_summary(&introspection) {
                review_lines.push(summary);
            }
            introspection
        },
        _ => Value::Null,
    };
    let comment_prefix = phase4_comment_prefix_for_path(path);
    let review_block = phase4_review_comment_block(comment_prefix, &marker, &review_lines);
    let content = match artifact_kind {
        "tool_schema" => {
            prepend_or_replace_phase4_review_block(&base_content, &marker, &review_block)
        },
        "wrapper_script" => {
            insert_or_replace_phase4_wrapper_review_block(&base_content, &marker, &review_block)
        },
        _ => base_content,
    };

    Phase4GeneratedPatchScaffold {
        content,
        metadata: json!({
            "source": "observed_failure_and_local_artifact_metadata",
            "reviewed_patch_content": false,
            "marker": marker,
            "candidate_id": proposal.candidate_id.clone(),
            "proposal_id": proposal.id.clone(),
            "capability_id": proposal.capability_id.clone(),
            "proposed_fix_type": proposal.proposed_fix_type.clone(),
            "failure_pattern": backlog.failure_pattern.clone(),
            "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
            "fix_spec": backlog.fix_spec.clone(),
            "evidence_refs": backlog.evidence_refs.clone(),
            "local_artifact": local_artifact,
            "suggested_review_actions": suggested_actions
        }),
    }
}

fn phase4_suggested_review_actions(artifact_kind: &str) -> Vec<String> {
    match artifact_kind {
        "tool_schema" => vec![
            "Align parameter names, types, defaults, and descriptions with the observed wrapper/runtime behavior.".to_string(),
            "Expose any argument the wrapper accepts and remove or document arguments the wrapper does not honor.".to_string(),
            "Run schema parse/lint and at least one targeted tool invocation smoke test before marking reviewed_patch_content=true.".to_string(),
        ],
        "wrapper_script" => vec![
            "Map reviewed schema arguments to the runtime command or API without silently dropping values.".to_string(),
            "Preserve required environment/auth propagation, timeouts, and stdout/stderr parsing behavior.".to_string(),
            "Run wrapper --help or a redacted fixture smoke invocation before marking reviewed_patch_content=true.".to_string(),
        ],
        _ => Vec::new(),
    }
}

fn phase4_review_scaffold_lines(
    artifact_kind: &str,
    path: &str,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    suggested_actions: &[String],
) -> Vec<String> {
    let artifact_label = match artifact_kind {
        "tool_schema" => "tool schema",
        "wrapper_script" => "wrapper script",
        _ => "skill artifact",
    };
    let failure = backlog
        .failure_pattern
        .as_deref()
        .map(|value| truncate_chars(&markdown_inline(value), 300))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "Unspecified observed failure pattern.".to_string());
    let expected_behavior = backlog
        .fix_spec
        .get("expected_behavior")
        .and_then(Value::as_str)
        .map(|value| truncate_chars(&markdown_inline(value), 300))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            "Reviewer must confirm corrected behavior from proposal evidence.".to_string()
        });
    let fix_spec = phase4_fix_spec_summary(&backlog.fix_spec)
        .unwrap_or_else(|| "No structured fix_spec details were provided.".to_string());
    let mut lines = vec![
        format!("Skill Evolution Phase 4 review scaffold for {artifact_label}."),
        format!("Candidate: {}", proposal.candidate_id),
        format!("Proposal: {}", proposal.id),
        format!(
            "Capability: {}",
            proposal
                .capability_id
                .as_deref()
                .unwrap_or("unknown capability")
        ),
        format!(
            "Fix type: {}",
            proposal
                .proposed_fix_type
                .as_deref()
                .unwrap_or("unspecified")
        ),
        format!("Target file: {path}"),
        format!("Observed failure: {failure}"),
        format!("Expected behavior: {expected_behavior}"),
        format!("Fix spec summary: {fix_spec}"),
        format!("Evidence refs: {}", backlog.evidence_refs.len()),
        "Review gate: edit/confirm this full replacement, run validation, then set metadata.reviewed_patch_content=true before implementation bundle drafting.".to_string(),
    ];
    lines.extend(
        suggested_actions
            .iter()
            .map(|action| format!("Suggested review action: {action}")),
    );
    lines
}

fn phase4_fix_spec_summary(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::Object(map) if map.is_empty() => None,
        _ => Some(truncate_chars(&markdown_inline(&value.to_string()), 600)),
    }
}

fn phase4_review_comment_block(prefix: &str, marker: &str, lines: &[String]) -> String {
    let mut block = String::new();
    block.push_str(&format!("{prefix} {marker}\n"));
    for line in lines {
        block.push_str(prefix);
        block.push(' ');
        block.push_str(&phase4_comment_line(line));
        block.push('\n');
    }
    block.push_str(&format!("{prefix} /{marker}\n"));
    block
}

fn phase4_comment_line(value: &str) -> String {
    truncate_chars(&markdown_inline(value), 900)
}

fn phase4_comment_prefix_for_path(path: &str) -> &'static str {
    if path.ends_with(".js") || path.ends_with(".ts") {
        "//"
    } else {
        "#"
    }
}

fn prepend_or_replace_phase4_review_block(current: &str, marker: &str, block: &str) -> String {
    if let Some(updated) = replace_existing_phase4_review_block(current, marker, block) {
        return updated;
    }
    if current.trim().is_empty() {
        return block.trim_end().to_string();
    }
    format!("{}\n{}", block.trim_end(), current.trim_start())
}

fn insert_or_replace_phase4_wrapper_review_block(
    current: &str,
    marker: &str,
    block: &str,
) -> String {
    if let Some(updated) = replace_existing_phase4_review_block(current, marker, block) {
        return updated;
    }
    if current.starts_with("#!") {
        if let Some(first_newline) = current.find('\n') {
            let shebang = &current[..=first_newline];
            let rest = current[first_newline + 1..].trim_start();
            return format!("{shebang}{}\n{rest}", block.trim_end());
        }
        return format!("{}\n{}", current.trim_end(), block.trim_end());
    }
    if current.trim().is_empty() {
        return block.trim_end().to_string();
    }
    format!("{}\n{}", block.trim_end(), current.trim_start())
}

fn replace_existing_phase4_review_block(
    current: &str,
    marker: &str,
    block: &str,
) -> Option<String> {
    let marker_start = current.find(marker)?;
    let line_start = current[..marker_start]
        .rfind('\n')
        .map(|offset| offset + 1)
        .unwrap_or(0);
    let end_marker = format!("/{marker}");
    let relative_end = current[marker_start..].find(&end_marker)?;
    let end_marker_end = marker_start + relative_end + end_marker.len();
    let line_end = current[end_marker_end..]
        .find('\n')
        .map(|offset| end_marker_end + offset + 1)
        .unwrap_or_else(|| current.len());
    let mut out = String::new();
    out.push_str(current[..line_start].trim_end());
    if !out.is_empty() {
        out.push('\n');
    }
    out.push_str(block.trim_end());
    if line_end < current.len() {
        out.push('\n');
        out.push_str(current[line_end..].trim_start());
    }
    Some(out)
}

fn default_phase4_tool_schema_content(
    path: &str,
    proposal: &LearningCapabilityEvolutionProposal,
) -> String {
    let skill_name = scoped_skill_name_from_path(path)
        .or_else(|| {
            proposal
                .capability_id
                .as_deref()
                .and_then(skill_name_from_capability_id)
        })
        .unwrap_or_else(|| "reviewed-skill".to_string());
    format!(
        "name: {skill_name}\ndescription: |\n  Review scaffold generated from Skill Evolution proposal `{}`. Replace this text with the reviewed tool description before approval.\nparameters: []\n",
        proposal.id
    )
}

fn default_phase4_wrapper_script_content(
    path: &str,
    proposal: &LearningCapabilityEvolutionProposal,
) -> String {
    let payload = json!({
        "ok": false,
        "review_required": true,
        "candidate_id": proposal.candidate_id.clone(),
        "proposal_id": proposal.id.clone()
    })
    .to_string();
    if path.ends_with(".sh") {
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nprintf '%s\\n' '{}' >&2\nexit 1\n",
            payload.replace('\'', "'\\''")
        )
    } else if path.ends_with(".js") || path.ends_with(".ts") {
        let payload_string =
            serde_json::to_string(&payload).unwrap_or_else(|_| "\"{}\"".to_string());
        format!("#!/usr/bin/env node\nconsole.error({payload_string});\nprocess.exit(1);\n")
    } else {
        let payload_string =
            serde_json::to_string(&payload).unwrap_or_else(|_| "\"{}\"".to_string());
        format!(
            "#!/usr/bin/env python3\nimport sys\n\n\ndef main() -> int:\n    print({payload_string}, file=sys.stderr)\n    return 1\n\n\nif __name__ == \"__main__\":\n    raise SystemExit(main())\n"
        )
    }
}

fn phase4_tool_schema_introspection(content: &str) -> Value {
    if content.trim().is_empty() {
        return json!({
            "parsed": false,
            "reason": "empty"
        });
    }
    let parsed = match serde_yaml::from_str::<serde_yaml::Value>(content) {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "parsed": false,
                "error": truncate_chars(&error.to_string(), 300)
            });
        },
    };
    let Some(mapping) = parsed.as_mapping() else {
        return json!({
            "parsed": true,
            "root_kind": "non_mapping"
        });
    };
    let tool_name = yaml_mapping_get(mapping, "name").and_then(serde_yaml::Value::as_str);
    let parameters = yaml_mapping_get(mapping, "parameters")
        .and_then(serde_yaml::Value::as_sequence)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| {
                    item.as_mapping()
                        .and_then(|entry| yaml_mapping_get(entry, "name"))
                        .and_then(serde_yaml::Value::as_str)
                        .map(str::to_string)
                })
                .take(32)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({
        "parsed": true,
        "tool_name": tool_name,
        "parameter_names": parameters,
        "parameter_count": parameters.len()
    })
}

fn phase4_wrapper_script_introspection(path: &str, content: &str) -> Value {
    let language = if path.ends_with(".py") {
        "python"
    } else if path.ends_with(".sh") {
        "shell"
    } else if path.ends_with(".ts") {
        "typescript"
    } else if path.ends_with(".js") {
        "javascript"
    } else {
        "unknown"
    };
    let line_count = content.lines().count();
    let lower = content.to_ascii_lowercase();
    json!({
        "language": language,
        "line_count": line_count,
        "has_shebang": content.starts_with("#!"),
        "argument_parsing_hints": {
            "argparse": lower.contains("argparse"),
            "click": lower.contains("click"),
            "process_argv": lower.contains("process.argv"),
            "commander": lower.contains("commander")
        },
        "environment_hints": {
            "reads_env": lower.contains("env") || lower.contains("os.environ") || lower.contains("process.env"),
            "spawns_process": lower.contains("subprocess") || lower.contains("child_process") || lower.contains("exec")
        }
    })
}

fn phase4_local_schema_summary(introspection: &Value) -> Option<String> {
    if introspection.get("parsed").and_then(Value::as_bool) != Some(true) {
        return Some("Local schema metadata: existing content did not parse as YAML; repair parse errors before review.".to_string());
    }
    let tool_name = introspection
        .get("tool_name")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let parameters = introspection
        .get("parameter_names")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "none".to_string());
    Some(format!(
        "Local schema metadata: tool name `{tool_name}`, parameters `{parameters}`."
    ))
}

fn phase4_local_wrapper_summary(introspection: &Value) -> Option<String> {
    Some(format!(
        "Local wrapper metadata: language `{}`, lines `{}`, shebang `{}`.",
        introspection
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or("unknown"),
        introspection
            .get("line_count")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        introspection
            .get("has_shebang")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    ))
}

fn yaml_mapping_get<'a>(
    mapping: &'a serde_yaml::Mapping,
    key: &str,
) -> Option<&'a serde_yaml::Value> {
    mapping.get(serde_yaml::Value::String(key.to_string()))
}

fn scoped_skill_name_from_path(path: &str) -> Option<String> {
    let normalized = normalized_scoped_skill_path(path)?;
    let parts = normal_path_components(Path::new(&normalized))?;
    if parts.len() >= 2 && parts[0] == "skills" {
        Some(parts[1].clone())
    } else {
        None
    }
}

fn enrich_capability_phase4_metadata(
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &mut LearningCapabilityEvolutionProposal,
) {
    let exact_target_files = phase4_exact_target_files(proposal);
    let tool_schema_targets = exact_target_files
        .iter()
        .filter(|path| path_targets_tool_schema(path))
        .cloned()
        .collect::<Vec<_>>();
    let wrapper_targets = exact_target_files
        .iter()
        .filter(|path| path_targets_wrapper_or_script(path))
        .cloned()
        .collect::<Vec<_>>();
    let schema_related = !tool_schema_targets.is_empty()
        || proposal
            .proposed_fix_type
            .as_deref()
            .map(|value| value.contains("schema"))
            .unwrap_or(false);
    let wrapper_related = !wrapper_targets.is_empty()
        || proposal
            .proposed_fix_type
            .as_deref()
            .map(|value| value.contains("wrapper") || value.contains("runtime"))
            .unwrap_or(false);
    let guidance_patch_targets = proposal
        .patches
        .iter()
        .filter(|patch| patch_targets_scoped_skill_guidance(&patch.path))
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();
    let tool_schema_patch_targets = proposal
        .patches
        .iter()
        .filter(|patch| patch_targets_scoped_tool_schema(&patch.path))
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();
    let wrapper_patch_targets = proposal
        .patches
        .iter()
        .filter(|patch| patch_targets_scoped_wrapper_script(&patch.path))
        .map(|patch| patch.path.clone())
        .collect::<Vec<_>>();

    let mut phase4 = json!({
        "source": "capability_evolution_phase4_drafter",
        "exact_target_files": exact_target_files,
        "reviewable_patch_targets": proposal
            .patches
            .iter()
            .map(|patch| patch.path.clone())
            .collect::<Vec<_>>(),
        "implementation_bundle_policy": {
            "auto_draft_scope": "low_risk_scoped_skill_guidance_tool_schema_or_wrapper_script",
            "requires_review_before_apply": true,
            "requires_full_replacement_patch_metadata": true,
            "requires_reviewed_patch_content_for": ["tool_schema", "wrapper_script"]
        },
        "docs_changelog_version_requirements": phase4_docs_changelog_version_requirements(proposal),
        "rollback_notes": phase4_rollback_notes(proposal),
        "risk_level": backlog.risk_level.as_str()
    });
    if schema_related {
        merge_json_object_field(
            &mut phase4,
            "tool_schema_change_plan",
            json!({
                "status": if tool_schema_patch_targets.is_empty() {
                    "review_required"
                } else {
                    "implementation_bundle_drafter_supported_after_review"
                },
                "target_files": tool_schema_targets,
                "reviewable_patch_targets": tool_schema_patch_targets.clone(),
                "required_patch_content": "reviewed full replacement metadata before implementation drafting",
                "validation": ["schema parse/lint", "targeted tool invocation smoke test"]
            }),
        );
    }
    if wrapper_related {
        merge_json_object_field(
            &mut phase4,
            "wrapper_script_patch_plan",
            json!({
                "status": if wrapper_patch_targets.is_empty() {
                    "review_required"
                } else {
                    "implementation_bundle_drafter_supported_after_review"
                },
                "target_files": wrapper_targets,
                "reviewable_patch_targets": wrapper_patch_targets.clone(),
                "required_patch_content": "reviewed full replacement metadata before implementation drafting",
                "validation": ["wrapper --help or smoke invocation", "fixture invocation with redacted/sample inputs"]
            }),
        );
    }
    if !guidance_patch_targets.is_empty() {
        merge_json_object_field(
            &mut phase4,
            "skill_guidance_patch_plan",
            json!({
                "status": "implementation_bundle_drafter_supported",
                "target_files": guidance_patch_targets,
                "patch_content": "metadata.new_content full replacement"
            }),
        );
    }
    if !tool_schema_patch_targets.is_empty() {
        merge_json_object_field(
            &mut phase4,
            "tool_schema_patch_plan",
            json!({
                "status": "implementation_bundle_drafter_supported_after_review",
                "target_files": tool_schema_patch_targets,
                "patch_content": "metadata.new_content full replacement"
            }),
        );
    }
    if !wrapper_patch_targets.is_empty() {
        merge_json_object_field(
            &mut phase4,
            "wrapper_script_bundle_plan",
            json!({
                "status": "implementation_bundle_drafter_supported_after_review",
                "target_files": wrapper_patch_targets,
                "patch_content": "metadata.new_content full replacement"
            }),
        );
    }
    merge_json_object_field(&mut proposal.change_plan, "phase4", phase4.clone());

    let rollback_notes = phase4_rollback_notes(proposal);
    let docs_requirements = phase4_docs_changelog_version_requirements(proposal);
    for patch in &mut proposal.patches {
        merge_json_object_field(&mut patch.metadata, "phase4", {
            let artifact_kind = implementation_bundle_supported_patch_kind(&patch.path);
            let supported = artifact_kind.is_some();
            let requires_reviewed_patch_content = artifact_kind
                .map(implementation_bundle_patch_requires_explicit_content_review)
                .unwrap_or(false);
            json!({
                "exact_target_file": patch.path.clone(),
                "implementation_bundle_drafter_supported": supported,
                "requires_reviewed_patch_content": requires_reviewed_patch_content,
                "requires_review_before_apply": true,
                "rollback_notes": rollback_notes.clone(),
                "docs_changelog_version_requirements": docs_requirements.clone()
            })
        });
    }
}

fn default_skill_guidance_file_content(
    path: &str,
    proposal: &LearningCapabilityEvolutionProposal,
) -> String {
    let skill_name = path
        .trim_start_matches("skills/")
        .split('/')
        .next()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("learned-skill");
    let description = markdown_inline(&proposal.summary)
        .trim()
        .chars()
        .take(180)
        .collect::<String>();
    let title = skill_name
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    let mut out = first.to_ascii_uppercase().to_string();
                    out.push_str(chars.as_str());
                    out
                },
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "---\nname: {}\ndescription: {}\n---\n\n# {}\n\n",
        yaml_json_string(skill_name),
        yaml_json_string(if description.is_empty() {
            "Learned reusable operating guidance."
        } else {
            description.as_str()
        }),
        if title.is_empty() {
            "Learned Skill"
        } else {
            &title
        }
    )
}

fn yaml_json_string(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

fn skill_guidance_path_for_capability_target(path: &str) -> Option<String> {
    let normalized = normalized_scoped_skill_path(path)?;
    let parts = normal_path_components(Path::new(&normalized))?;
    if parts.len() < 2 || parts[0] != "skills" {
        return None;
    }
    Some(format!("skills/{}/SKILL.md", parts[1]))
}

fn build_learning_skill_guidance_section(
    marker: &str,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> String {
    let failure = backlog
        .failure_pattern
        .as_deref()
        .map(|text| truncate_chars(text.trim(), 1_200))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "Unspecified failure pattern from the learning backlog.".to_string());
    let expected_behavior = backlog
        .fix_spec
        .get("expected_behavior")
        .and_then(Value::as_str)
        .map(|text| truncate_chars(text.trim(), 1_200))
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| {
            "Use the proposal change plan and validation evidence to confirm the corrected behavior."
                .to_string()
        });
    let capability_id = proposal
        .capability_id
        .as_deref()
        .unwrap_or("unknown capability");
    let fix_type = proposal
        .proposed_fix_type
        .as_deref()
        .unwrap_or("skill_update");
    let skill_payload = skill_workflow_guidance_payload(&backlog.fix_spec);
    let mut details = String::new();
    push_optional_guidance_line(
        &mut details,
        "Workflow signature",
        skill_payload.workflow_signature.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "Trigger conditions",
        skill_payload.trigger_conditions.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "Procedure steps",
        skill_payload.procedure_steps.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "Tool choice guidance",
        skill_payload.tool_choice_guidance.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "Common failure modes",
        skill_payload.common_failure_modes.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "Verification steps",
        skill_payload.verification_steps.as_deref(),
    );
    push_optional_guidance_line(
        &mut details,
        "When not to use",
        skill_payload.when_not_to_use.as_deref(),
    );
    push_optional_guidance_line(&mut details, "Examples", skill_payload.examples.as_deref());
    format!(
        "\n{marker}\n## Learned Operating Guidance\n\n- Source candidate: `{}`\n- Capability/skill: `{}`\n- Fix type: `{}`\n- Failure or workflow pattern: {}\n- Expected behavior: {}\n{}- Promotion verification: run or create the eval described by this proposal's `eval_plan`, then record validation evidence before promotion.\n\n",
        proposal.candidate_id,
        capability_id,
        fix_type,
        markdown_inline(&failure),
        markdown_inline(&expected_behavior),
        details
    )
}

#[derive(Debug, Default)]
struct SkillWorkflowGuidancePayload {
    workflow_signature: Option<String>,
    trigger_conditions: Option<String>,
    procedure_steps: Option<String>,
    tool_choice_guidance: Option<String>,
    common_failure_modes: Option<String>,
    verification_steps: Option<String>,
    when_not_to_use: Option<String>,
    examples: Option<String>,
}

fn skill_workflow_guidance_payload(value: &Value) -> SkillWorkflowGuidancePayload {
    SkillWorkflowGuidancePayload {
        workflow_signature: guidance_text_any(value, &["workflow_signature", "signature"]),
        trigger_conditions: guidance_text_any(value, &["trigger_conditions", "triggers"]),
        procedure_steps: guidance_text_any(value, &["procedure_steps", "steps"]),
        tool_choice_guidance: guidance_text_any(value, &["tool_choice_guidance", "tool_guidance"]),
        common_failure_modes: guidance_text_any(value, &["common_failure_modes", "failure_modes"]),
        verification_steps: guidance_text_any(value, &["verification_steps", "verification"]),
        when_not_to_use: guidance_text_any(value, &["when_not_to_use", "boundaries"]),
        examples: guidance_text_any(value, &["examples"]),
    }
}

fn guidance_text_any(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| value.get(*key))
        .find_map(guidance_text)
        .map(|text| truncate_chars(&text, 1_200))
}

fn guidance_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(markdown_inline(text)).filter(|text| !text.is_empty()),
        Value::Array(items) => {
            let lines = items.iter().filter_map(guidance_text).collect::<Vec<_>>();
            (!lines.is_empty()).then(|| lines.join("; "))
        },
        Value::Object(_) => Some(markdown_inline(&value.to_string())),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn push_optional_guidance_line(out: &mut String, label: &str, value: Option<&str>) {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return;
    };
    out.push_str(&format!("- {label}: {}\n", markdown_inline(value)));
}

fn append_or_replace_marked_section(current: &str, marker: &str, section: &str) -> String {
    if let Some(start) = current.find(marker) {
        let next_marker = current[start + marker.len()..]
            .find(LEARNING_SKILL_GUIDANCE_MARKER_PREFIX)
            .map(|offset| start + marker.len() + offset)
            .unwrap_or_else(|| current.len());
        let mut out = String::new();
        out.push_str(current[..start].trim_end());
        out.push('\n');
        out.push_str(section.trim_start());
        if next_marker < current.len() {
            out.push('\n');
            out.push_str(current[next_marker..].trim_start());
        }
        out
    } else {
        let mut out = current.trim_end().to_string();
        out.push('\n');
        out.push_str(section);
        out
    }
}

fn markdown_inline(value: &str) -> String {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_capability_evolution_evaluation_backlog_item(
    scope: &LearningScope,
    candidate: &magician::magician_v2::learning::LearningCandidate,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    existing: Option<&LearningEvaluationBacklogItem>,
    request_payload: Value,
) -> LearningEvaluationBacklogItem {
    let now = Utc::now();
    let created_at = existing.map(|item| item.created_at).unwrap_or(now);
    let status = existing
        .map(|item| item.status.clone())
        .unwrap_or(LearningEvaluationBacklogStatus::Queued);
    let eval_plan = proposal
        .eval_plan
        .clone()
        .or_else(|| default_capability_eval_plan(backlog))
        .unwrap_or_else(|| json!({}));
    let case_kind = read_string_any(
        &eval_plan,
        &["case_kind", "kind", "eval_kind", "evaluation_kind", "type"],
    )
    .map(|value| normalize_token(&value))
    .filter(|value| !value.is_empty())
    .unwrap_or_else(|| "capability_regression".to_string());
    let priority = read_string_any(&eval_plan, &["priority", "severity"])
        .map(|value| normalize_token(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| match &backlog.risk_level {
            LearningRiskLevel::Low => "low".to_string(),
            LearningRiskLevel::Medium => "normal".to_string(),
            LearningRiskLevel::High | LearningRiskLevel::Critical => "high".to_string(),
        });
    let phase5_fixture_cases = phase5_fixture_cases_from_proposal(proposal);
    let phase5_failure_classes = phase5_fixture_failure_classes(&phase5_fixture_cases);

    LearningEvaluationBacklogItem {
        id: format!("leb_{}", proposal.candidate_id.trim_start_matches("lc_")),
        scope: scope.clone(),
        candidate_id: proposal.candidate_id.clone(),
        status,
        title: format!("Evaluate capability evolution: {}", proposal.title),
        summary: proposal.summary.clone(),
        rationale: format!(
            "Generated from capability-evolution proposal `{}` so the meta-harness can validate the proposed fix and regression coverage before promotion.",
            proposal.id
        ),
        case_kind,
        priority,
        target_agent_id: backlog.source_agent_id.clone(),
        focus_area: proposal.capability_id.clone(),
        proposed_target: proposal
            .capability_id
            .clone()
            .or_else(|| candidate.proposed_target.clone()),
        source_agent_id: backlog.source_agent_id.clone(),
        source_task_id: backlog.source_task_id.clone(),
        source_execution_id: backlog.source_execution_id.clone(),
        source_chat_session_id: backlog.source_chat_session_id.clone(),
        evidence_refs: backlog.evidence_refs.clone(),
        case_spec: json!({
            "source": "capability_evolution_eval_plan",
            "candidate_id": proposal.candidate_id.clone(),
            "proposal_id": proposal.id.clone(),
            "backlog_id": backlog.id.clone(),
            "capability_id": proposal.capability_id.clone(),
            "candidate_type": backlog.candidate_type.as_str(),
            "failure_pattern": backlog.failure_pattern.clone(),
            "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
            "eval_plan": eval_plan,
            "validation_plan": proposal.validation_plan.clone(),
            "phase5_fixture_cases": phase5_fixture_cases,
            "phase5_failure_classes": phase5_failure_classes,
            "promotion_gate": proposal.promotion_gate.clone(),
            "request_payload": request_payload,
            "meta_harness": {
                "required": true,
                "source": "capability_evolution",
                "review_goal": "Validate that the proposed capability/skill change fixes the observed failure and does not regress existing behavior."
            }
        }),
        created_at,
        updated_at: now,
    }
}

fn default_capability_change_plan(backlog: &LearningCapabilityEvolutionBacklogItem) -> Value {
    json!({
        "source": "capability_evolution_backlog",
        "candidate_id": backlog.candidate_id.clone(),
        "backlog_id": backlog.id.clone(),
        "capability_id": backlog.capability_id.clone(),
        "candidate_type": backlog.candidate_type.as_str(),
        "failure_pattern": backlog.failure_pattern.clone(),
        "proposed_fix_type": backlog.proposed_fix_type.clone(),
        "proposed_files": backlog.proposed_files.clone(),
        "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
        "fix_spec": backlog.fix_spec.clone()
    })
}

fn default_capability_eval_plan(backlog: &LearningCapabilityEvolutionBacklogItem) -> Option<Value> {
    Some(json!({
        "source": "capability_evolution_proposal_drafter",
        "candidate_id": backlog.candidate_id.clone(),
        "capability_id": backlog.capability_id.clone(),
        "failure_pattern": backlog.failure_pattern.clone(),
        "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
        "meta_harness": {
            "required": true,
            "goal": "Create or run the smallest eval that proves this skill/tool-pack fix addresses the observed failure without regressing existing behavior.",
            "source_evidence_refs": backlog.evidence_refs.clone()
        },
        "commands": [],
        "regression_commands": [],
        "notes": "Executable commands may be added by the proposal author or meta-harness worker before approval."
    }))
}

fn default_capability_validation_plan(
    backlog: &LearningCapabilityEvolutionBacklogItem,
) -> Option<Value> {
    Some(json!({
        "required_eval": backlog.required_eval.clone(),
        "promotion_gate": backlog.promotion_gate.clone(),
        "human_review_required": true,
        "local_validation_required": true,
        "regression_required": true,
        "commands": [],
        "regression_commands": [],
        "implementation_policy": "Do not mutate skill/tool-pack files until the proposal is reviewed, an eval plan is attached, and validation evidence has been recorded."
    }))
}

fn default_capability_promotion_gate(
    backlog: &LearningCapabilityEvolutionBacklogItem,
) -> Option<Value> {
    Some(json!({
        "source": "capability_evolution_proposal_drafter",
        "candidate_id": backlog.candidate_id.clone(),
        "meta_harness_review": "required before promotion",
        "human_approval": "required for high-risk or scoped skill changes",
        "local_validation_required": true,
        "regression_required": true,
        "application_required_for_scoped_skills": true,
        "rollback": "Applied application records retain previous file content for review and rollback."
    }))
}

fn infer_scoped_skill_proposed_files(
    backlog: &LearningCapabilityEvolutionBacklogItem,
) -> Vec<String> {
    let Some(skill_name) = backlog
        .capability_id
        .as_deref()
        .and_then(skill_name_from_capability_id)
    else {
        return Vec::new();
    };
    let fix_type = backlog
        .proposed_fix_type
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let target = if fix_type.contains("schema") {
        "tool_schema.yaml"
    } else {
        "SKILL.md"
    };
    vec![format!("skills/{skill_name}/{target}")]
}

fn skill_name_from_capability_id(capability_id: &str) -> Option<String> {
    let trimmed = capability_id.trim();
    let without_prefix = trimmed
        .strip_prefix("skill:")
        .or_else(|| trimmed.strip_prefix("skills/"))
        .unwrap_or(trimmed);
    let skill_name = without_prefix.split('/').next()?.trim();
    if skill_name.is_empty()
        || skill_name.contains("..")
        || skill_name.contains('\\')
        || skill_name.starts_with('.')
    {
        return None;
    }
    Some(skill_name.to_string())
}

fn enrich_capability_phase5_validation_plan(
    workspace_layout: &ArtifactV2Workspace,
    repo_root: &Path,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &mut LearningCapabilityEvolutionProposal,
) {
    let generated =
        phase5_generated_validation_plan(workspace_layout, repo_root, scope, backlog, proposal);
    let has_commands = generated
        .get("commands")
        .and_then(Value::as_array)
        .map(|items| !items.is_empty())
        .unwrap_or(false)
        || generated
            .get("regression_commands")
            .and_then(Value::as_array)
            .map(|items| !items.is_empty())
            .unwrap_or(false)
        || generated
            .get("fixture_cases")
            .and_then(Value::as_array)
            .map(|items| !items.is_empty())
            .unwrap_or(false);
    if !has_commands {
        return;
    }
    let validation_plan = proposal.validation_plan.get_or_insert_with(|| json!({}));
    merge_json_object_field(validation_plan, "phase5_generated", generated.clone());
    merge_json_object_field(
        validation_plan,
        "expected_behavior",
        backlog
            .fix_spec
            .get("expected_behavior")
            .cloned()
            .unwrap_or(Value::Null),
    );
    merge_json_object_field(
        validation_plan,
        "observed_failure",
        backlog
            .failure_pattern
            .as_ref()
            .map(|value| json!(value))
            .unwrap_or(Value::Null),
    );
    merge_json_object_field(
        validation_plan,
        "evidence_refs",
        json!(backlog.evidence_refs.clone()),
    );
}

fn phase5_generated_validation_plan(
    workspace_layout: &ArtifactV2Workspace,
    repo_root: &Path,
    scope: &LearningScope,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
) -> Value {
    let mut commands = Vec::new();
    let mut regression_commands = Vec::new();
    let mut fixture_cases = Vec::new();
    let mut target_kinds = Vec::new();
    let mut test_scanned_skill_dirs = Vec::new();
    let scope_root = workspace_layout.scope_root(&scope.principal, &scope.workspace);
    let exact_targets = phase4_exact_target_files(proposal);
    for path in &exact_targets {
        if let Some(skill_dir) = scoped_skill_dir_from_path(path) {
            if !test_scanned_skill_dirs
                .iter()
                .any(|existing| existing == &skill_dir)
            {
                for command in phase5_skill_local_test_commands(&scope_root, &skill_dir) {
                    push_unique_string(&mut commands, command);
                }
                test_scanned_skill_dirs.push(skill_dir);
            }
        }
        if patch_targets_scoped_skill_guidance(path) {
            target_kinds.push(json!({"path": path, "kind": "skill_guidance"}));
            if let Some(skill_dir) = scoped_skill_dir_from_path(path) {
                if let Some(command) = phase5_skill_frontmatter_command(repo_root, &skill_dir) {
                    push_unique_string(&mut commands, command);
                }
            }
        }
        if patch_targets_scoped_tool_schema(path) {
            target_kinds.push(json!({"path": path, "kind": "tool_schema"}));
            if let Some(command) = phase5_tool_schema_command(repo_root, path) {
                push_unique_string(&mut commands, command);
            }
        }
        if patch_targets_scoped_wrapper_script(path) {
            target_kinds.push(json!({"path": path, "kind": "wrapper_script"}));
            for command in phase5_wrapper_validation_commands(path) {
                push_unique_string(&mut commands, command);
            }
            for command in phase5_wrapper_regression_commands(path, backlog) {
                push_unique_string(&mut regression_commands, command);
            }
            if phase5_has_real_failure_evidence(backlog) {
                if let Some(schema_path) = phase5_schema_path_for_wrapper(path, &exact_targets) {
                    let fixture_command =
                        phase5_wrapper_fixture_command(repo_root, &schema_path, path);
                    if let Some(command) = fixture_command.as_ref() {
                        push_unique_string(&mut regression_commands, command.clone());
                    }
                    if let Some(fixture_case) = phase5_wrapper_fixture_case(
                        &scope_root,
                        backlog,
                        proposal,
                        path,
                        &schema_path,
                        fixture_command.as_deref(),
                    ) {
                        fixture_cases.push(fixture_case);
                    }
                }
            }
        }
        if path.starts_with("docs/") {
            target_kinds.push(json!({"path": path, "kind": "docs"}));
            if let Some(command) = phase5_docs_guard_command(repo_root) {
                push_unique_string(&mut commands, command);
            }
        }
    }

    json!({
        "source": "capability_evolution_phase5_validation_drafter",
        "candidate_id": proposal.candidate_id.clone(),
        "proposal_id": proposal.id.clone(),
        "capability_id": proposal.capability_id.clone(),
        "failure_pattern": backlog.failure_pattern.clone(),
        "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
        "commands": commands,
        "regression_commands": regression_commands,
        "fixture_cases": fixture_cases,
        "target_files": exact_targets,
        "target_kinds": target_kinds,
        "notes": "Generated commands are conservative validation suggestions for scoped skill artifacts. Reviewers can edit them before approval; the steward still applies its local allowlist before autonomous execution."
    })
}

fn phase5_skill_frontmatter_command(repo_root: &Path, skill_dir: &str) -> Option<String> {
    let script = command_safe_absolute_path(
        &repo_root
            .join("skillshub")
            .join("scripts")
            .join("validate_skill_md.py"),
    )?;
    let skill_dir = command_safe_relative_path(skill_dir)?;
    Some(format!("python3 {script} {skill_dir}"))
}

fn phase5_tool_schema_command(repo_root: &Path, path: &str) -> Option<String> {
    let script =
        command_safe_absolute_path(&repo_root.join("scripts").join("validate_skill_artifact.py"))?;
    let path = command_safe_relative_path(path)?;
    Some(format!("python3 {script} --tool-schema {path}"))
}

fn phase5_wrapper_fixture_command(
    repo_root: &Path,
    schema_path: &str,
    wrapper_path: &str,
) -> Option<String> {
    let script =
        command_safe_absolute_path(&repo_root.join("scripts").join("validate_skill_artifact.py"))?;
    let schema_path = command_safe_relative_path(schema_path)?;
    let wrapper_path = command_safe_relative_path(wrapper_path)?;
    Some(format!(
        "python3 {script} --tool-schema {schema_path} --wrapper-fixture {wrapper_path}"
    ))
}

fn phase5_docs_guard_command(repo_root: &Path) -> Option<String> {
    let script = command_safe_absolute_path(&repo_root.join("scripts").join("docs_guard.py"))?;
    Some(format!("python3 {script} --working-tree"))
}

fn phase5_wrapper_validation_commands(path: &str) -> Vec<String> {
    let Some(path) = command_safe_relative_path(path) else {
        return Vec::new();
    };
    if path.ends_with(".py") {
        vec![format!("python3 -m py_compile {path}")]
    } else if path.ends_with(".sh") {
        vec![format!("./{path} --help")]
    } else if path.ends_with(".js") {
        vec![format!("node --check {path}")]
    } else {
        Vec::new()
    }
}

fn phase5_wrapper_regression_commands(
    path: &str,
    backlog: &LearningCapabilityEvolutionBacklogItem,
) -> Vec<String> {
    if !phase5_has_real_failure_evidence(backlog) {
        return Vec::new();
    }
    let Some(path) = command_safe_relative_path(path) else {
        return Vec::new();
    };
    if path.ends_with(".py") {
        vec![format!("python3 {path} --help")]
    } else if path.ends_with(".sh") {
        vec![format!("./{path} --help")]
    } else if path.ends_with(".js") {
        vec![format!("node {path} --help")]
    } else {
        Vec::new()
    }
}

fn phase5_skill_local_test_commands(scope_root: &Path, skill_dir: &str) -> Vec<String> {
    let Some(skill_dir) = command_safe_relative_path(skill_dir) else {
        return Vec::new();
    };
    let tests_rel = format!("{skill_dir}/tests");
    let Some(tests_rel) = command_safe_relative_path(&tests_rel) else {
        return Vec::new();
    };
    let tests_dir = scope_root.join(&tests_rel);
    let Ok(entries) = fs::read_dir(&tests_dir) else {
        return Vec::new();
    };
    let mut has_python_tests = false;
    let mut shell_tests = Vec::new();
    for entry in entries.flatten().take(64) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        if !(file_name.starts_with("test_") || file_name.ends_with("_test.sh")) {
            continue;
        }
        if file_name.ends_with(".py") {
            has_python_tests = true;
        } else if file_name.ends_with(".sh") {
            let rel = format!("{tests_rel}/{file_name}");
            if let Some(rel) = command_safe_relative_path(&rel) {
                shell_tests.push(format!("./{rel}"));
            }
        }
    }
    let mut commands = Vec::new();
    if has_python_tests {
        commands.push(format!("python3 -m unittest discover -s {tests_rel}"));
    }
    shell_tests.sort();
    shell_tests.dedup();
    commands.extend(shell_tests.into_iter().take(4));
    commands
}

fn phase5_has_real_failure_evidence(backlog: &LearningCapabilityEvolutionBacklogItem) -> bool {
    backlog.failure_pattern.is_some() || !backlog.evidence_refs.is_empty()
}

fn phase5_schema_path_for_wrapper(wrapper_path: &str, exact_targets: &[String]) -> Option<String> {
    let skill_dir = scoped_skill_dir_from_path(wrapper_path)?;
    exact_targets
        .iter()
        .find(|target| {
            patch_targets_scoped_tool_schema(target)
                && scoped_skill_dir_from_path(target).as_deref() == Some(skill_dir.as_str())
        })
        .cloned()
        .or_else(|| Some(format!("{skill_dir}/tool_schema.yaml")))
}

fn phase5_wrapper_fixture_case(
    scope_root: &Path,
    backlog: &LearningCapabilityEvolutionBacklogItem,
    proposal: &LearningCapabilityEvolutionProposal,
    wrapper_path: &str,
    schema_path: &str,
    validation_command: Option<&str>,
) -> Option<Value> {
    let schema_content = phase5_schema_content_for_fixture(scope_root, proposal, schema_path)?;
    let fixture_inputs = phase5_fixture_inputs_from_schema(&schema_content);
    Some(json!({
        "id": phase5_fixture_case_id(&proposal.candidate_id, wrapper_path),
        "source": "capability_evolution_phase5_fixture_generator",
        "kind": "wrapper_regression_fixture",
        "candidate_id": proposal.candidate_id.clone(),
        "proposal_id": proposal.id.clone(),
        "capability_id": proposal.capability_id.clone(),
        "target_file": wrapper_path,
        "tool_schema_file": schema_path,
        "regression_command": validation_command,
        "failure_class": phase5_failure_class(backlog),
        "observed_failure": backlog.failure_pattern.clone(),
        "expected_behavior": backlog.fix_spec.get("expected_behavior").cloned(),
        "evidence_refs": backlog.evidence_refs.clone(),
        "sample_inputs": fixture_inputs.get("sample_inputs").cloned().unwrap_or_else(|| json!({})),
        "input_source": fixture_inputs.get("input_source").cloned(),
        "parameter_names": fixture_inputs.get("parameter_names").cloned(),
        "command_blueprint": {
            "wrapper": wrapper_path,
            "env": phase5_fixture_env_from_inputs(
                fixture_inputs.get("sample_inputs").unwrap_or(&Value::Null)
            ),
            "execution_note": "Meta-harness or reviewer can run the wrapper with these redacted/sample inputs after reviewing safety for the target skill."
        },
        "review_goal": "Exercise the observed failure class with schema-shaped sample inputs before promotion."
    }))
}

fn phase5_schema_content_for_fixture(
    scope_root: &Path,
    proposal: &LearningCapabilityEvolutionProposal,
    schema_path: &str,
) -> Option<String> {
    if let Some(content) = proposal
        .patches
        .iter()
        .find(|patch| patch.path == schema_path)
        .and_then(|patch| patch.metadata.get("new_content"))
        .and_then(Value::as_str)
        .filter(|content| !content.trim().is_empty())
    {
        return Some(content.to_string());
    }
    let target = resolve_capability_application_path(
        scope_root,
        LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
        schema_path,
    )
    .ok()?;
    fs::read_to_string(target).ok()
}

fn phase5_fixture_inputs_from_schema(schema_content: &str) -> Value {
    let parsed = match serde_yaml::from_str::<serde_yaml::Value>(schema_content) {
        Ok(value) => value,
        Err(error) => {
            return json!({
                "input_source": "tool_schema_parse_error",
                "parse_error": truncate_chars(&error.to_string(), 300),
                "parameter_names": [],
                "sample_inputs": {}
            });
        },
    };
    let Some(mapping) = parsed.as_mapping() else {
        return json!({
            "input_source": "tool_schema_non_mapping",
            "parameter_names": [],
            "sample_inputs": {}
        });
    };
    let mut sample_inputs = serde_json::Map::new();
    let mut parameter_names = Vec::new();
    if let Some(parameters) =
        yaml_mapping_get(mapping, "parameters").and_then(serde_yaml::Value::as_sequence)
    {
        for parameter in parameters.iter().take(16) {
            let Some(parameter_mapping) = parameter.as_mapping() else {
                continue;
            };
            let Some(name) = yaml_mapping_get(parameter_mapping, "name")
                .and_then(serde_yaml::Value::as_str)
                .map(str::trim)
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            parameter_names.push(json!(name));
            let required = yaml_mapping_get(parameter_mapping, "required")
                .and_then(yaml_bool_value)
                .unwrap_or(false);
            let has_default = yaml_mapping_get(parameter_mapping, "default").is_some();
            if required || has_default {
                sample_inputs.insert(
                    name.to_string(),
                    phase5_sample_value_for_parameter(name, parameter_mapping),
                );
            }
        }
    }
    json!({
        "input_source": "tool_schema_required_and_default_parameters",
        "parameter_names": parameter_names,
        "sample_inputs": Value::Object(sample_inputs)
    })
}

fn phase5_sample_value_for_parameter(name: &str, parameter: &serde_yaml::Mapping) -> Value {
    if let Some(default) = yaml_mapping_get(parameter, "default") {
        return yaml_value_to_json(default);
    }
    if let Some(enum_value) = yaml_mapping_get(parameter, "enum_values")
        .and_then(serde_yaml::Value::as_sequence)
        .and_then(|items| items.first())
    {
        return yaml_value_to_json(enum_value);
    }
    let type_hint = yaml_mapping_get(parameter, "param_type")
        .or_else(|| yaml_mapping_get(parameter, "type"))
        .and_then(serde_yaml::Value::as_str)
        .map(normalize_token)
        .unwrap_or_else(|| "string".to_string());
    match type_hint.as_str() {
        "integer" | "int" => json!(1),
        "number" | "float" | "double" => json!(1.0),
        "boolean" | "bool" => json!(true),
        "array" | "list" => json!(["sample"]),
        "object" | "map" => json!({"sample": "value"}),
        _ => json!(phase5_sample_string_for_parameter(name)),
    }
}

fn phase5_sample_string_for_parameter(name: &str) -> String {
    let normalized = normalize_token(name);
    if normalized.contains("query") || normalized.contains("search") {
        "skill evolution regression fixture".to_string()
    } else if normalized.contains("path")
        || normalized == "target"
        || normalized.ends_with("_target")
    {
        ".".to_string()
    } else if normalized.contains("url") {
        "https://example.invalid/skill-evolution-fixture".to_string()
    } else {
        format!("sample_{normalized}")
    }
}

fn phase5_fixture_env_from_inputs(inputs: &Value) -> Value {
    let Some(map) = inputs.as_object() else {
        return json!({});
    };
    let env = map
        .iter()
        .map(|(key, value)| {
            let env_key = format!("_TOOL_{}", phase5_env_key_fragment(key));
            (env_key, phase5_fixture_env_value(value))
        })
        .collect::<serde_json::Map<_, _>>();
    Value::Object(env)
}

fn phase5_fixture_env_value(value: &Value) -> Value {
    match value {
        Value::Null => json!(""),
        Value::String(text) => json!(text),
        Value::Bool(flag) => json!(flag.to_string()),
        Value::Number(number) => json!(number.to_string()),
        _ => json!(value.to_string()),
    }
}

fn phase5_env_key_fragment(value: &str) -> String {
    let mut out = String::new();
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_uppercase());
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "INPUT".to_string()
    } else {
        out
    }
}

fn phase5_fixture_case_id(candidate_id: &str, wrapper_path: &str) -> String {
    let mut token = format!("{candidate_id}_{wrapper_path}");
    token = token
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while token.contains("__") {
        token = token.replace("__", "_");
    }
    format!("phase5_fixture_{}", token.trim_matches('_'))
}

fn phase5_failure_class(backlog: &LearningCapabilityEvolutionBacklogItem) -> String {
    let raw = backlog
        .failure_pattern
        .as_deref()
        .or(backlog.proposed_fix_type.as_deref())
        .unwrap_or_else(|| backlog.candidate_type.as_str());
    let mut token: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    while token.contains("__") {
        token = token.replace("__", "_");
    }
    token = token.trim_matches('_').chars().take(80).collect();
    if token.is_empty() {
        "unspecified_failure".to_string()
    } else {
        token
    }
}

fn phase5_fixture_cases_from_proposal(
    proposal: &LearningCapabilityEvolutionProposal,
) -> Vec<Value> {
    proposal
        .validation_plan
        .as_ref()
        .and_then(|plan| plan.get("phase5_generated"))
        .and_then(|generated| generated.get("fixture_cases"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

fn phase5_fixture_cases_from_case_spec(case_spec: &Value) -> Vec<Value> {
    case_spec
        .get("phase5_fixture_cases")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            case_spec
                .get("validation_plan")
                .and_then(|plan| plan.get("phase5_generated"))
                .and_then(|generated| generated.get("fixture_cases"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
}

fn phase5_fixture_failure_classes(fixture_cases: &[Value]) -> Vec<Value> {
    let mut classes = Vec::new();
    for case in fixture_cases {
        if let Some(class) = case
            .get("failure_class")
            .and_then(Value::as_str)
            .filter(|class| !class.trim().is_empty())
        {
            push_unique_string(&mut classes, class.to_string());
        }
    }
    classes.into_iter().map(Value::String).collect()
}

fn phase5_fixture_results_for_commands(
    fixture_cases: &[Value],
    results: &[ValidationCommandResult],
) -> Vec<Value> {
    fixture_cases
        .iter()
        .map(|case| {
            let validation_command = case
                .get("regression_command")
                .or_else(|| case.get("validation_command"))
                .and_then(Value::as_str)
                .filter(|command| !command.trim().is_empty());
            let result = validation_command
                .and_then(|command| results.iter().find(|result| result.command == command));
            let command_success = result
                .map(|result| result.success && !result.timed_out && result.spawn_error.is_none())
                .unwrap_or(false);
            let status = match (validation_command, result, command_success) {
                (None, _, _) => "no_validation_command",
                (Some(_), None, _) => "not_run",
                (Some(_), Some(_), true) => "passed",
                (Some(_), Some(_), false) => "failed",
            };
            json!({
                "fixture_case_id": case.get("id").cloned().unwrap_or(Value::Null),
                "failure_class": case.get("failure_class").cloned().unwrap_or(Value::Null),
                "target_file": case.get("target_file").cloned().unwrap_or(Value::Null),
                "tool_schema_file": case.get("tool_schema_file").cloned().unwrap_or(Value::Null),
                "observed_failure": case.get("observed_failure").cloned().unwrap_or(Value::Null),
                "expected_behavior": case.get("expected_behavior").cloned().unwrap_or(Value::Null),
                "regression_command": validation_command,
                "status": status,
                "matched_command_result": result.is_some(),
                "command_source": result.map(|result| result.source.clone()),
                "regression": result.map(|result| result.regression).unwrap_or(false),
                "success": command_success,
                "status_code": result.and_then(|result| result.status_code),
                "timed_out": result.map(|result| result.timed_out).unwrap_or(false),
                "spawn_error": result.and_then(|result| result.spawn_error.clone()),
                "validator_output": result
                    .map(phase5_fixture_validator_output_summary)
                    .unwrap_or(Value::Null)
            })
        })
        .collect()
}

fn phase5_fixture_results_from_payload(fixture_cases: &[Value], payload: &Value) -> Vec<Value> {
    let results = payload
        .get("phase5_fixture_results")
        .or_else(|| payload.get("fixture_results"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if !results.is_empty() {
        return results;
    }
    if fixture_cases.is_empty() {
        return Vec::new();
    }
    Vec::new()
}

fn phase5_fixture_result_metrics(fixture_cases: &[Value], fixture_results: &[Value]) -> Value {
    let mut covered_failure_classes = Vec::new();
    let mut passed_count = 0usize;
    let mut failed_count = 0usize;
    let mut not_run_count = 0usize;
    let mut no_command_count = 0usize;
    for result in fixture_results {
        match result
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
        {
            "passed" => passed_count += 1,
            "failed" => failed_count += 1,
            "not_run" => not_run_count += 1,
            "no_validation_command" => no_command_count += 1,
            _ => {},
        }
        if matches!(
            result.get("status").and_then(Value::as_str),
            Some("passed" | "failed")
        ) {
            if let Some(class) = result
                .get("failure_class")
                .and_then(Value::as_str)
                .filter(|class| !class.trim().is_empty())
            {
                push_unique_string(&mut covered_failure_classes, class.to_string());
            }
        }
    }
    json!({
        "case_count": fixture_cases.len(),
        "result_count": fixture_results.len(),
        "passed_count": passed_count,
        "failed_count": failed_count,
        "not_run_count": not_run_count,
        "no_validation_command_count": no_command_count,
        "failure_classes": phase5_fixture_failure_classes(fixture_cases),
        "covered_failure_classes": covered_failure_classes
    })
}

fn phase5_fixture_manual_metrics(fixture_cases: &[Value], fixture_results: &[Value]) -> Value {
    phase5_fixture_result_metrics(fixture_cases, fixture_results)
}

fn phase5_metrics_with_fixture(metrics: Value, fixture_metrics: Value) -> Value {
    match metrics {
        Value::Object(mut map) => {
            map.insert("phase5_fixture".to_string(), fixture_metrics);
            Value::Object(map)
        },
        other => json!({
            "request_metrics": other,
            "phase5_fixture": fixture_metrics
        }),
    }
}

fn phase5_fixture_validator_output_summary(result: &ValidationCommandResult) -> Value {
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        return Value::Null;
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(value) => json!({
            "parsed": true,
            "fixture_count": value
                .get("fixtures")
                .and_then(Value::as_array)
                .map(|items| items.len())
                .unwrap_or(0)
        }),
        Err(_) => json!({
            "parsed": false,
            "stdout": truncate_chars(trimmed, 1000)
        }),
    }
}

fn yaml_value_to_json(value: &serde_yaml::Value) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

fn yaml_bool_value(value: &serde_yaml::Value) -> Option<bool> {
    value.as_bool().or_else(|| {
        value
            .as_str()
            .and_then(|text| match normalize_token(text).as_str() {
                "true" | "yes" | "1" => Some(true),
                "false" | "no" | "0" => Some(false),
                _ => None,
            })
    })
}

fn command_safe_absolute_path(path: &Path) -> Option<String> {
    let text = path.to_str()?.to_string();
    command_safe_token(&text).then_some(text)
}

fn command_safe_relative_path(path: &str) -> Option<String> {
    let normalized = normalized_scoped_skill_path(path)
        .or_else(|| normal_path_components(Path::new(path)).map(|parts| parts.join("/")))?;
    command_safe_token(&normalized).then_some(normalized)
}

fn command_safe_token(value: &str) -> bool {
    !value.is_empty()
        && !value.chars().any(|ch| {
            matches!(
                ch,
                '|' | ';' | '&' | '`' | '$' | '<' | '>' | '\n' | '\r' | ' ' | '"' | '\''
            )
        })
}

fn scoped_skill_dir_from_path(path: &str) -> Option<String> {
    let normalized = normalized_scoped_skill_path(path)?;
    let parts = normal_path_components(Path::new(&normalized))?;
    if parts.len() >= 2 && parts[0] == "skills" {
        Some(format!("skills/{}", parts[1]))
    } else {
        None
    }
}

fn push_unique_string(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn planned_validation_commands(
    proposal: &LearningCapabilityEvolutionProposal,
    override_commands: &[String],
    include_regression: bool,
) -> Vec<ValidationCommandSpec> {
    let mut commands = Vec::new();
    if !override_commands.is_empty() {
        for command in override_commands {
            push_validation_command(
                &mut commands,
                command,
                "request.commands",
                false,
                include_regression,
            );
        }
        return dedupe_validation_commands(commands);
    }
    if let Some(plan) = proposal.validation_plan.as_ref() {
        collect_validation_commands(plan, "validation_plan", include_regression, &mut commands);
    }
    if let Some(plan) = proposal.eval_plan.as_ref() {
        collect_validation_commands(plan, "eval_plan", include_regression, &mut commands);
    }
    dedupe_validation_commands(commands)
}

fn planned_evaluation_commands(
    backlog: &LearningEvaluationBacklogItem,
    override_commands: &[String],
    include_regression: bool,
) -> Vec<ValidationCommandSpec> {
    let mut commands = Vec::new();
    if !override_commands.is_empty() {
        for command in override_commands {
            push_validation_command(
                &mut commands,
                command,
                "request.commands",
                false,
                include_regression,
            );
        }
        return dedupe_validation_commands(commands);
    }
    collect_validation_commands(
        &backlog.case_spec,
        "evaluation.case_spec",
        include_regression,
        &mut commands,
    );
    dedupe_validation_commands(commands)
}

fn collect_validation_commands(
    value: &Value,
    source: &str,
    include_regression: bool,
    commands: &mut Vec<ValidationCommandSpec>,
) {
    match value {
        Value::Object(map) => {
            for (key, entry) in map {
                let key_lower = key.to_ascii_lowercase();
                let is_regression = key_lower.contains("regression");
                let is_command_key = key_lower == "command"
                    || key_lower == "commands"
                    || key_lower.ends_with("_command")
                    || key_lower.ends_with("_commands");
                if is_command_key {
                    collect_command_values(
                        entry,
                        &format!("{source}.{key}"),
                        is_regression,
                        include_regression,
                        commands,
                    );
                } else if matches!(entry, Value::Object(_) | Value::Array(_)) {
                    collect_validation_commands(
                        entry,
                        &format!("{source}.{key}"),
                        include_regression,
                        commands,
                    );
                }
            }
        },
        Value::Array(items) => {
            for (index, entry) in items.iter().enumerate() {
                collect_validation_commands(
                    entry,
                    &format!("{source}[{index}]"),
                    include_regression,
                    commands,
                );
            }
        },
        _ => {},
    }
}

fn collect_command_values(
    value: &Value,
    source: &str,
    regression: bool,
    include_regression: bool,
    commands: &mut Vec<ValidationCommandSpec>,
) {
    match value {
        Value::String(command) => {
            push_validation_command(commands, command, source, regression, include_regression);
        },
        Value::Array(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_command_values(
                    item,
                    &format!("{source}[{index}]"),
                    regression,
                    include_regression,
                    commands,
                );
            }
        },
        Value::Object(map) => {
            if let Some(command) = map.get("command").and_then(Value::as_str) {
                push_validation_command(commands, command, source, regression, include_regression);
            }
        },
        _ => {},
    }
}

fn push_validation_command(
    commands: &mut Vec<ValidationCommandSpec>,
    command: &str,
    source: &str,
    regression: bool,
    include_regression: bool,
) {
    let trimmed = command.trim();
    if trimmed.is_empty() || (regression && !include_regression) {
        return;
    }
    if commands.len() >= MAX_CAPABILITY_VALIDATION_COMMANDS {
        return;
    }
    commands.push(ValidationCommandSpec {
        command: trimmed.to_string(),
        source: source.to_string(),
        regression,
    });
}

fn dedupe_validation_commands(commands: Vec<ValidationCommandSpec>) -> Vec<ValidationCommandSpec> {
    let mut deduped = Vec::new();
    for command in commands {
        if !deduped
            .iter()
            .any(|existing: &ValidationCommandSpec| existing.command == command.command)
        {
            deduped.push(command);
        }
    }
    deduped
}

async fn run_validation_command(
    spec: &ValidationCommandSpec,
    cwd: &Path,
    timeout_seconds: u64,
) -> ValidationCommandResult {
    let started = Instant::now();
    let mut command = Command::new("/bin/sh");
    command
        .arg("-lc")
        .arg(&spec.command)
        .current_dir(cwd)
        .kill_on_drop(true);
    let output = timeout(Duration::from_secs(timeout_seconds), command.output()).await;
    match output {
        Ok(Ok(output)) => ValidationCommandResult {
            command: spec.command.clone(),
            source: spec.source.clone(),
            regression: spec.regression,
            success: output.status.success(),
            status_code: output.status.code(),
            timed_out: false,
            spawn_error: None,
            duration_ms: started.elapsed().as_millis(),
            stdout: truncate_chars(
                &String::from_utf8_lossy(&output.stdout),
                MAX_CAPABILITY_VALIDATION_OUTPUT_CHARS,
            ),
            stderr: truncate_chars(
                &String::from_utf8_lossy(&output.stderr),
                MAX_CAPABILITY_VALIDATION_OUTPUT_CHARS,
            ),
        },
        Ok(Err(error)) => ValidationCommandResult {
            command: spec.command.clone(),
            source: spec.source.clone(),
            regression: spec.regression,
            success: false,
            status_code: None,
            timed_out: false,
            spawn_error: Some(error.to_string()),
            duration_ms: started.elapsed().as_millis(),
            stdout: String::new(),
            stderr: String::new(),
        },
        Err(_) => ValidationCommandResult {
            command: spec.command.clone(),
            source: spec.source.clone(),
            regression: spec.regression,
            success: false,
            status_code: None,
            timed_out: true,
            spawn_error: None,
            duration_ms: started.elapsed().as_millis(),
            stdout: String::new(),
            stderr: format!("command timed out after {timeout_seconds}s"),
        },
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(max_chars).collect();
    truncated.push_str("\n...[truncated]");
    truncated
}

fn proposal_status_requires_decision_endpoint(
    status: &LearningCapabilityEvolutionProposalStatus,
) -> bool {
    matches!(
        status,
        LearningCapabilityEvolutionProposalStatus::Approved
            | LearningCapabilityEvolutionProposalStatus::Rejected
            | LearningCapabilityEvolutionProposalStatus::Superseded
            | LearningCapabilityEvolutionProposalStatus::Archived
    )
}

fn proposal_status_is_terminal(status: &LearningCapabilityEvolutionProposalStatus) -> bool {
    matches!(
        status,
        LearningCapabilityEvolutionProposalStatus::Rejected
            | LearningCapabilityEvolutionProposalStatus::Superseded
            | LearningCapabilityEvolutionProposalStatus::Archived
    )
}

fn validate_proposal_review_transition(
    from: &LearningCapabilityEvolutionProposalStatus,
    to: &LearningCapabilityEvolutionProposalStatus,
) -> Option<String> {
    if from == to {
        return None;
    }
    match from {
        LearningCapabilityEvolutionProposalStatus::Draft
        | LearningCapabilityEvolutionProposalStatus::ReadyForReview => None,
        LearningCapabilityEvolutionProposalStatus::Approved => {
            if matches!(
                to,
                LearningCapabilityEvolutionProposalStatus::Superseded
                    | LearningCapabilityEvolutionProposalStatus::Archived
            ) {
                None
            } else {
                Some(
                    "approved capability-evolution proposals can only be superseded or archived"
                        .to_string(),
                )
            }
        },
        LearningCapabilityEvolutionProposalStatus::Rejected
        | LearningCapabilityEvolutionProposalStatus::Superseded
        | LearningCapabilityEvolutionProposalStatus::Archived => Some(format!(
            "capability-evolution proposal is terminal in status `{}` and cannot transition to `{}`",
            from.as_str(),
            to.as_str()
        )),
    }
}

fn proposal_decision_candidate_state(
    status: &LearningCapabilityEvolutionProposalStatus,
) -> LearningCandidateState {
    match status {
        LearningCapabilityEvolutionProposalStatus::Approved => LearningCandidateState::Approved,
        LearningCapabilityEvolutionProposalStatus::Rejected => LearningCandidateState::Rejected,
        LearningCapabilityEvolutionProposalStatus::Superseded => LearningCandidateState::Superseded,
        LearningCapabilityEvolutionProposalStatus::Archived => LearningCandidateState::Archived,
        LearningCapabilityEvolutionProposalStatus::Draft
        | LearningCapabilityEvolutionProposalStatus::ReadyForReview => {
            LearningCandidateState::Triaged
        },
    }
}

fn capability_backlog_accepts_proposal_upsert(
    status: &LearningCapabilityEvolutionBacklogStatus,
) -> bool {
    matches!(
        status,
        LearningCapabilityEvolutionBacklogStatus::Queued
            | LearningCapabilityEvolutionBacklogStatus::InReview
    )
}

fn capability_backlog_accepts_validation(
    status: &LearningCapabilityEvolutionBacklogStatus,
) -> bool {
    matches!(
        status,
        LearningCapabilityEvolutionBacklogStatus::Queued
            | LearningCapabilityEvolutionBacklogStatus::InReview
            | LearningCapabilityEvolutionBacklogStatus::Validated
    )
}

fn capability_backlog_accepts_review_decision(
    status: &LearningCapabilityEvolutionBacklogStatus,
    current_proposal_status: &LearningCapabilityEvolutionProposalStatus,
    requested_proposal_status: &LearningCapabilityEvolutionProposalStatus,
) -> bool {
    let backlog_is_terminal = matches!(
        status,
        LearningCapabilityEvolutionBacklogStatus::Implemented
            | LearningCapabilityEvolutionBacklogStatus::Rejected
            | LearningCapabilityEvolutionBacklogStatus::Superseded
            | LearningCapabilityEvolutionBacklogStatus::Archived
    );
    !backlog_is_terminal || current_proposal_status == requested_proposal_status
}

fn candidate_satisfies_proposal_decision(
    state: &LearningCandidateState,
    target: &LearningCandidateState,
) -> bool {
    match target {
        LearningCandidateState::Approved => matches!(
            state,
            LearningCandidateState::Approved
                | LearningCandidateState::Evaluated
                | LearningCandidateState::Implemented
        ),
        _ => state == target,
    }
}

fn proposal_decision_backlog_status(
    proposal_status: &LearningCapabilityEvolutionProposalStatus,
    current_status: &LearningCapabilityEvolutionBacklogStatus,
) -> LearningCapabilityEvolutionBacklogStatus {
    match proposal_status {
        LearningCapabilityEvolutionProposalStatus::Approved => match current_status {
            LearningCapabilityEvolutionBacklogStatus::Queued => {
                LearningCapabilityEvolutionBacklogStatus::InReview
            },
            LearningCapabilityEvolutionBacklogStatus::Rejected
            | LearningCapabilityEvolutionBacklogStatus::Superseded
            | LearningCapabilityEvolutionBacklogStatus::Archived
            | LearningCapabilityEvolutionBacklogStatus::Implemented => current_status.clone(),
            _ => current_status.clone(),
        },
        LearningCapabilityEvolutionProposalStatus::Rejected => {
            LearningCapabilityEvolutionBacklogStatus::Rejected
        },
        LearningCapabilityEvolutionProposalStatus::Superseded => {
            LearningCapabilityEvolutionBacklogStatus::Superseded
        },
        LearningCapabilityEvolutionProposalStatus::Archived => {
            LearningCapabilityEvolutionBacklogStatus::Archived
        },
        LearningCapabilityEvolutionProposalStatus::Draft
        | LearningCapabilityEvolutionProposalStatus::ReadyForReview => current_status.clone(),
    }
}

fn learning_unavailable() -> HttpResponse {
    HttpResponse::ServiceUnavailable().json(serde_json::json!({
        "error": "Learning layer is not initialized"
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    use serde_json::json;

    fn phase5_test_repo_root() -> &'static Path {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("workspace root")
    }

    fn phase7_test_context() -> (tempfile::TempDir, LearningApi, LearningScope) {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, phase5_test_repo_root());
        let scope = LearningScope::new("anonymous".to_string(), "default".to_string());
        (temp_dir, api, scope)
    }

    fn phase7_test_application(
        scope: &LearningScope,
        promoted_at: chrono::DateTime<Utc>,
    ) -> LearningCapabilityEvolutionApplicationRecord {
        LearningCapabilityEvolutionApplicationRecord {
            id: "lceapp_phase7".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: "implementation".to_string(),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "applied".to_string(),
            mode: LearningCapabilityEvolutionApplicationMode::Apply,
            status: LearningCapabilityEvolutionApplicationStatus::Applied,
            changed_files: vec![LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: Some("# Old\n".to_string()),
                new_content: Some("# New\n".to_string()),
            }],
            evidence_refs: Vec::new(),
            payload: json!({
                "phase6_rollback_snapshot": {
                    "source": "capability_evolution_phase6_rollback_snapshot"
                }
            }),
            created_at: promoted_at,
        }
    }

    fn phase7_test_promotion(
        scope: &LearningScope,
        promoted_at: chrono::DateTime<Utc>,
        application: Option<&LearningCapabilityEvolutionApplicationRecord>,
    ) -> LearningCapabilityEvolutionPromotionRecord {
        LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_phase7".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: application.map(|record| record.implementation_id.clone()),
            application_id: application.map(|record| record.id.clone()),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "promoted".to_string(),
            applied_files: vec!["skills/foo/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: promoted_at,
        }
    }

    fn phase7_write_regression_invocations(
        api: &LearningApi,
        scope: &LearningScope,
        promoted_at: chrono::DateTime<Utc>,
    ) {
        for (id, created_at, status, failure_class, fingerprint) in [
            (
                "before_ok",
                promoted_at - chrono::Duration::minutes(5),
                LearningSkillInvocationStatus::Succeeded,
                None,
                "ok",
            ),
            (
                "after_bad_1",
                promoted_at + chrono::Duration::minutes(5),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
            (
                "after_bad_2",
                promoted_at + chrono::Duration::minutes(6),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
            (
                "after_bad_3",
                promoted_at + chrono::Duration::minutes(7),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
        ] {
            api.store
                .write_skill_invocation_evidence(&LearningSkillInvocationEvidence {
                    id: format!("lsi_{id}"),
                    scope: scope.clone(),
                    source:
                        magician::magician_v2::learning::LearningSkillInvocationSource::CompiledPack,
                    skill_name: "foo".to_string(),
                    tool_action_name: None,
                    agent_id: None,
                    task_id: None,
                    execution_id: None,
                    chat_session_id: None,
                    input_fingerprint: fingerprint.to_string(),
                    input_shape: json!({"shape": fingerprint}),
                    status,
                    failure_class,
                    error_summary: Some("bad guidance".to_string()),
                    result_summary: None,
                    duration_ms: 10,
                    retry_count: 0,
                    evidence_refs: Vec::new(),
                    payload: Value::Null,
                    created_at,
                })
                .expect("write invocation evidence");
        }
    }

    fn phase7_append_negative_feedback(api: &LearningApi, scope: &LearningScope) {
        api.store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_user_teaching_recorded".to_string(),
                    agent_id: None,
                    task_id: None,
                    execution_id: None,
                    chat_session_id: None,
                    summary: "User marked foo output wrong.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "action": "this_was_wrong",
                        "skill_name": "foo"
                    }),
                },
            )
            .expect("write teaching event");
    }

    fn phase6_test_proposal(scope: &LearningScope) -> LearningCapabilityEvolutionProposal {
        let now = Utc::now();
        LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Improve foo skill".to_string(),
            summary: "Improve foo skill".to_string(),
            capability_id: Some("skill:foo".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/foo/SKILL.md".to_string()],
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: None,
            validation_plan: None,
            promotion_gate: None,
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        }
    }

    fn phase6_test_validation_report(
        scope: &LearningScope,
    ) -> LearningCapabilityEvolutionValidationReport {
        LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            status: LearningCapabilityEvolutionValidationStatus::Failed,
            capability_id: Some("skill:foo".to_string()),
            runner: "tester".to_string(),
            summary: "Validation failed after apply.".to_string(),
            commands: vec!["make check-skill".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": false}),
            payload: Value::Null,
            created_at: Utc::now(),
        }
    }

    #[test]
    fn approved_decision_does_not_downgrade_evaluated_or_implemented_candidates() {
        assert!(candidate_satisfies_proposal_decision(
            &LearningCandidateState::Approved,
            &LearningCandidateState::Approved
        ));
        assert!(candidate_satisfies_proposal_decision(
            &LearningCandidateState::Evaluated,
            &LearningCandidateState::Approved
        ));
        assert!(candidate_satisfies_proposal_decision(
            &LearningCandidateState::Implemented,
            &LearningCandidateState::Approved
        ));
        assert!(!candidate_satisfies_proposal_decision(
            &LearningCandidateState::Triaged,
            &LearningCandidateState::Approved
        ));
    }

    #[test]
    fn draft_proposal_upsert_is_blocked_after_validation_or_promotion() {
        assert!(capability_backlog_accepts_proposal_upsert(
            &LearningCapabilityEvolutionBacklogStatus::Queued
        ));
        assert!(capability_backlog_accepts_proposal_upsert(
            &LearningCapabilityEvolutionBacklogStatus::InReview
        ));
        assert!(!capability_backlog_accepts_proposal_upsert(
            &LearningCapabilityEvolutionBacklogStatus::Validated
        ));
        assert!(!capability_backlog_accepts_proposal_upsert(
            &LearningCapabilityEvolutionBacklogStatus::Implemented
        ));
    }

    #[test]
    fn validation_reports_are_blocked_after_implementation_or_closed_statuses() {
        assert!(capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::InReview
        ));
        assert!(capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::Validated
        ));
        assert!(!capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::Implemented
        ));
        assert!(!capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::Rejected
        ));
        assert!(!capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::Superseded
        ));
        assert!(!capability_backlog_accepts_validation(
            &LearningCapabilityEvolutionBacklogStatus::Archived
        ));
    }

    #[test]
    fn review_decision_is_idempotent_only_after_implementation() {
        assert!(capability_backlog_accepts_review_decision(
            &LearningCapabilityEvolutionBacklogStatus::Implemented,
            &LearningCapabilityEvolutionProposalStatus::Approved,
            &LearningCapabilityEvolutionProposalStatus::Approved
        ));
        assert!(!capability_backlog_accepts_review_decision(
            &LearningCapabilityEvolutionBacklogStatus::Implemented,
            &LearningCapabilityEvolutionProposalStatus::Approved,
            &LearningCapabilityEvolutionProposalStatus::Archived
        ));
        assert!(capability_backlog_accepts_review_decision(
            &LearningCapabilityEvolutionBacklogStatus::Validated,
            &LearningCapabilityEvolutionProposalStatus::Approved,
            &LearningCapabilityEvolutionProposalStatus::Archived
        ));
    }

    #[test]
    fn scoped_application_paths_reject_absolute_and_parent_escape() {
        let root = std::env::temp_dir().join(format!(
            "magician-learning-apply-test-{}",
            Uuid::new_v4().simple()
        ));
        std::fs::create_dir_all(&root).expect("create temp scope root");

        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "skills/foo/SKILL.md"
        )
        .is_ok());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "../outside"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "/tmp/outside"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "secrets/token.json"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "tasks/task_123/state.json"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "skills/foo/node_modules/tool.js"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "skills/foo/.env"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            "skills/.hidden/SKILL.md"
        )
        .is_err());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill,
            "skills/foo/SKILL.md"
        )
        .is_ok());
        assert!(resolve_capability_application_path(
            &root,
            LearningCapabilityEvolutionApplicationTargetSurface::SourceSkill,
            "skills/foo/node_modules/tool.js"
        )
        .is_err());
    }

    #[test]
    fn scoped_application_requires_full_replacement_content() {
        let patch = LearningCapabilityEvolutionProposalPatch {
            path: "skills/foo/SKILL.md".to_string(),
            operation: "update".to_string(),
            summary: "update guide".to_string(),
            diff: Some("--- old\n+++ new".to_string()),
            metadata: json!({}),
        };
        let error = extract_patch_replacement_content(&patch)
            .expect("content lookup should not fail")
            .is_none();
        assert!(error);

        let patch = LearningCapabilityEvolutionProposalPatch {
            metadata: json!({"new_content": "# Foo\n"}),
            ..patch
        };
        assert_eq!(
            extract_patch_replacement_content(&patch).expect("content lookup"),
            Some("# Foo\n".to_string())
        );
    }

    #[test]
    fn scoped_skill_path_normalization_rejects_non_skill_and_unsafe_paths() {
        assert_eq!(
            normalized_scoped_skill_path("skills/foo/SKILL.md"),
            Some("skills/foo/SKILL.md".to_string())
        );
        assert_eq!(
            normalized_scoped_skill_path("./skills/foo/./tool_schema.yaml"),
            Some("skills/foo/tool_schema.yaml".to_string())
        );
        assert_eq!(normalized_scoped_skill_path("docs/foo.md"), None);
        assert_eq!(normalized_scoped_skill_path("../skills/foo/SKILL.md"), None);
        assert_eq!(
            normalized_scoped_skill_path("/tmp/skills/foo/SKILL.md"),
            None
        );
        assert_eq!(
            normalized_scoped_skill_path("skills/foo/node_modules/tool.js"),
            None
        );
        assert_eq!(normalized_scoped_skill_path("skills/foo/.env"), None);
        assert_eq!(
            normalized_scoped_skill_path("skills/.hidden/SKILL.md"),
            None
        );
    }

    #[test]
    fn scoped_application_targets_include_proposed_and_applied_files() {
        let now = Utc::now();
        let proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Improve skill".to_string(),
            summary: "Improve skill".to_string(),
            capability_id: Some("skill:test".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec![
                "skills/foo/tool_schema.yaml".to_string(),
                "docs/not-skill.md".to_string(),
            ],
            change_plan: json!({}),
            patches: vec![LearningCapabilityEvolutionProposalPatch {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                summary: "update guide".to_string(),
                diff: None,
                metadata: json!({"new_content": "# Foo\n"}),
            }],
            eval_plan: None,
            validation_plan: None,
            promotion_gate: None,
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        let implementation = LearningCapabilityEvolutionImplementationRecord {
            id: "implementation".to_string(),
            scope: proposal.scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            capability_id: Some("skill:test".to_string()),
            actor: "tester".to_string(),
            summary: "implemented".to_string(),
            applied_files: vec!["skills/bar/SKILL.md".to_string()],
            patches: Vec::new(),
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: now,
        };
        let promotion_applied_files = vec!["skills/baz/SKILL.md".to_string()];

        let targets = scoped_application_required_targets(
            &proposal,
            Some(&implementation),
            &promotion_applied_files,
        );
        assert_eq!(
            targets,
            vec![
                "skills/bar/SKILL.md".to_string(),
                "skills/baz/SKILL.md".to_string(),
                "skills/foo/SKILL.md".to_string(),
                "skills/foo/tool_schema.yaml".to_string(),
            ]
        );
    }

    #[test]
    fn scoped_application_gate_requires_actual_scoped_file_change() {
        let mut application = LearningCapabilityEvolutionApplicationRecord {
            id: "lceapp_test".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: "implementation".to_string(),
            capability_id: Some("capability".to_string()),
            actor: "tester".to_string(),
            summary: "applied".to_string(),
            mode: LearningCapabilityEvolutionApplicationMode::Apply,
            status: LearningCapabilityEvolutionApplicationStatus::Applied,
            changed_files: Vec::new(),
            evidence_refs: Vec::new(),
            payload: json!({}),
            created_at: Utc::now(),
        };

        assert!(!application_record_contains_scoped_change(&application));
        assert!(!application_record_covers_scoped_targets(
            &application,
            &["skills/foo/SKILL.md".to_string()]
        ));
        application
            .changed_files
            .push(LearningCapabilityEvolutionAppliedFile {
                path: "skills/bar/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: None,
                new_content: Some("# Bar\n".to_string()),
            });
        assert!(application_record_contains_scoped_change(&application));
        assert!(!application_record_covers_scoped_targets(
            &application,
            &["skills/foo/SKILL.md".to_string()]
        ));
        application
            .changed_files
            .push(LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: None,
                new_content: Some("# Foo\n".to_string()),
            });
        assert!(application_record_covers_scoped_targets(
            &application,
            &["skills/foo/SKILL.md".to_string()]
        ));
    }

    #[test]
    fn phase6_rollback_snapshot_summarizes_restore_actions_without_raw_body_duplication() {
        let changed_files = vec![
            LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: Some("# Old\n".to_string()),
                new_content: Some("# New\n".to_string()),
            },
            LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/scripts/run.py".to_string(),
                operation: "create".to_string(),
                previous_exists: false,
                previous_content: None,
                new_content: Some("print('ok')\n".to_string()),
            },
        ];

        let snapshot = phase6_rollback_snapshot(
            &changed_files,
            LearningCapabilityEvolutionApplicationTargetSurface::ScopedSkill,
            Path::new("/tmp/scope"),
        );

        assert_eq!(
            snapshot.get("source").and_then(Value::as_str),
            Some("capability_evolution_phase6_rollback_snapshot")
        );
        assert_eq!(snapshot.get("file_count").and_then(Value::as_u64), Some(2));
        assert_eq!(
            snapshot.get("created_file_count").and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            snapshot
                .get("restorable_file_count")
                .and_then(Value::as_u64),
            Some(2)
        );
        let files = snapshot
            .get("files")
            .and_then(Value::as_array)
            .expect("files");
        assert_eq!(
            files[0].get("rollback_action").and_then(Value::as_str),
            Some("restore_previous_content")
        );
        assert_eq!(
            files[1].get("rollback_action").and_then(Value::as_str),
            Some("remove_created_file")
        );
        assert!(files[0]
            .get("previous_sha256")
            .and_then(Value::as_str)
            .is_some());
        assert!(files[0].get("previous_content").is_none());
        assert!(files[0].get("new_content").is_none());
    }

    #[test]
    fn phase6_changed_skill_discovery_reports_missing_and_discovered_skills() {
        let manifest = magician::magician_v2::skills::loader::parse_manifest(
            "---\nname: foo\ndescription: Foo skill\nallowed-tools: shell\n---\nBody\n",
            Path::new("/tmp/scope/skills/foo"),
        )
        .expect("manifest");

        let discovery = phase6_changed_skill_discovery(
            &[manifest],
            &["foo".to_string(), "missing".to_string()],
        );

        assert_eq!(
            discovery
                .get("discovered_changed_skill_count")
                .and_then(Value::as_u64),
            Some(1)
        );
        assert_eq!(
            discovery
                .get("missing_changed_skill_count")
                .and_then(Value::as_u64),
            Some(1)
        );
        let skills = discovery
            .get("skills")
            .and_then(Value::as_array)
            .expect("skills");
        assert_eq!(
            skills[0].get("kind").and_then(Value::as_str),
            Some("procedure")
        );
        assert_eq!(
            skills[1].get("discovered").and_then(Value::as_bool),
            Some(false)
        );
    }

    #[test]
    fn phase6_rollback_recommendation_records_and_dedupes_by_trigger() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, phase5_test_repo_root());
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let application = LearningCapabilityEvolutionApplicationRecord {
            id: "lceapp_test".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: "implementation".to_string(),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "applied".to_string(),
            mode: LearningCapabilityEvolutionApplicationMode::Apply,
            status: LearningCapabilityEvolutionApplicationStatus::Applied,
            changed_files: vec![LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: Some("# Old\n".to_string()),
                new_content: Some("# New\n".to_string()),
            }],
            evidence_refs: Vec::new(),
            payload: json!({
                "phase6_rollback_snapshot": {
                    "source": "capability_evolution_phase6_rollback_snapshot"
                },
                "runtime_catalog_refresh": {
                    "status": "refresh_failed"
                }
            }),
            created_at: Utc::now(),
        };

        let first = phase6_record_rollback_recommendation(
            &api,
            &scope,
            &application,
            Phase6RollbackRecommendationTrigger {
                kind: "catalog_refresh_failed",
                severity: "high",
                source_id: application.id.clone(),
                actor: "tester".to_string(),
                summary: "catalog failed".to_string(),
                validation_id: Some("validation".to_string()),
                promotion_id: None,
                evidence_refs: Vec::new(),
                payload: json!({"status": "refresh_failed"}),
            },
        )
        .expect("recommendation")
        .expect("created");
        let second = phase6_record_rollback_recommendation(
            &api,
            &scope,
            &application,
            Phase6RollbackRecommendationTrigger {
                kind: "catalog_refresh_failed",
                severity: "high",
                source_id: application.id.clone(),
                actor: "tester".to_string(),
                summary: "catalog failed".to_string(),
                validation_id: Some("validation".to_string()),
                promotion_id: None,
                evidence_refs: Vec::new(),
                payload: json!({"status": "refresh_failed"}),
            },
        )
        .expect("recommendation")
        .expect("deduped");

        assert_eq!(first.id, second.id);
        assert_eq!(
            first.status,
            LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended
        );
        assert_eq!(first.rollback_files, vec!["skills/foo/SKILL.md"]);
        let listed = api
            .store
            .list_capability_evolution_rollback_recommendation_records(
                &scope,
                LearningCapabilityEvolutionRollbackRecommendationFilters {
                    status: Some("recommended".to_string()),
                    candidate_id: Some("candidate".to_string()),
                    capability_id: Some("skill:foo".to_string()),
                    application_id: Some(application.id.clone()),
                    limit: Some(10),
                },
            )
            .expect("listed");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].trigger_kind, "catalog_refresh_failed");
    }

    #[test]
    fn phase6_validation_rollback_errors_are_recorded_on_report_payload() {
        let (_temp_dir, api, scope) = phase7_test_context();
        let proposal = phase6_test_proposal(&scope);
        let mut report = phase6_test_validation_report(&scope);
        let applications_dir = api
            .store
            .workspace_layout()
            .capability_evolution_candidate_applications_dir(
                &scope.principal,
                &scope.workspace,
                &report.candidate_id,
            );
        std::fs::create_dir_all(&applications_dir).expect("create application dir");
        std::fs::write(applications_dir.join("broken.json"), "{not-json")
            .expect("write malformed application record");

        let (recommendation, error) = phase6_record_validation_rollback_recommendation_outcome(
            &api,
            &scope,
            &proposal,
            &mut report,
        );

        assert!(recommendation.is_none());
        let error = error.expect("rollback recommendation error");
        assert!(error.contains("broken.json") || error.contains("expected"));
        assert_eq!(
            report
                .payload
                .get("phase6_rollback_recommendation_error")
                .and_then(Value::as_str),
            Some(error.as_str())
        );
    }

    #[test]
    fn phase7_initial_monitor_errors_are_returned_to_promotion_caller() {
        let (_temp_dir, api, scope) = phase7_test_context();
        let promotion = phase7_test_promotion(&scope, Utc::now(), None);
        let events_dir = api
            .store
            .workspace_layout()
            .learning_events_dir(&scope.principal, &scope.workspace);
        std::fs::create_dir_all(&events_dir).expect("create events dir");
        std::fs::write(events_dir.join("broken.jsonl"), "{not-json")
            .expect("write malformed event log");

        let (monitor, error) = phase7_record_initial_post_promotion_monitor_outcome(
            &api,
            &scope,
            &promotion,
            "tester".to_string(),
        );

        assert!(monitor.is_none());
        let error = error.expect("post-promotion monitor error");
        assert!(error.contains("broken.jsonl") || error.contains("expected"));
    }

    #[test]
    fn phase7_promoted_skill_names_derive_from_capability_and_paths() {
        let promotion = LearningCapabilityEvolutionPromotionRecord {
            id: "promotion".to_string(),
            scope: LearningScope::new("anonymous".to_string(), "default".to_string()),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: None,
            application_id: None,
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "promoted".to_string(),
            applied_files: vec![
                "skills/bar/SKILL.md".to_string(),
                "skillshub/baz/tool_schema.yaml".to_string(),
                "README.md".to_string(),
            ],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: Utc::now(),
        };

        assert_eq!(
            phase7_promoted_skill_names(&promotion),
            vec!["bar".to_string(), "baz".to_string(), "foo".to_string()]
        );
    }

    #[test]
    fn phase7_monitor_detects_post_promotion_regression_and_creates_rollback_recommendation() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, phase5_test_repo_root());
        let scope = LearningScope::new("anonymous".to_string(), "default".to_string());
        let promoted_at = Utc::now();
        let application = LearningCapabilityEvolutionApplicationRecord {
            id: "lceapp_phase7".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: "implementation".to_string(),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "applied".to_string(),
            mode: LearningCapabilityEvolutionApplicationMode::Apply,
            status: LearningCapabilityEvolutionApplicationStatus::Applied,
            changed_files: vec![LearningCapabilityEvolutionAppliedFile {
                path: "skills/foo/SKILL.md".to_string(),
                operation: "update".to_string(),
                previous_exists: true,
                previous_content: Some("# Old\n".to_string()),
                new_content: Some("# New\n".to_string()),
            }],
            evidence_refs: Vec::new(),
            payload: json!({
                "phase6_rollback_snapshot": {
                    "source": "capability_evolution_phase6_rollback_snapshot"
                }
            }),
            created_at: promoted_at,
        };
        api.store
            .write_capability_evolution_application_record(&application)
            .expect("write application");
        let promotion = LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_phase7".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: Some("implementation".to_string()),
            application_id: Some(application.id.clone()),
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "promoted".to_string(),
            applied_files: vec!["skills/foo/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: promoted_at,
        };
        api.store
            .append_capability_evolution_promotion_record(&promotion)
            .expect("write promotion");
        for (id, created_at, status, failure_class, fingerprint) in [
            (
                "before_ok",
                promoted_at - chrono::Duration::minutes(5),
                LearningSkillInvocationStatus::Succeeded,
                None,
                "ok",
            ),
            (
                "after_bad_1",
                promoted_at + chrono::Duration::minutes(5),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
            (
                "after_bad_2",
                promoted_at + chrono::Duration::minutes(6),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
            (
                "after_bad_3",
                promoted_at + chrono::Duration::minutes(7),
                LearningSkillInvocationStatus::Failed,
                Some(magician::magician_v2::learning::LearningSkillInvocationFailureClass::ToolMisuse),
                "shape",
            ),
        ] {
            api.store
                .write_skill_invocation_evidence(&LearningSkillInvocationEvidence {
                    id: format!("lsi_{id}"),
                    scope: scope.clone(),
                    source:
                        magician::magician_v2::learning::LearningSkillInvocationSource::CompiledPack,
                    skill_name: "foo".to_string(),
                    tool_action_name: None,
                    agent_id: None,
                    task_id: None,
                    execution_id: None,
                    chat_session_id: None,
                    input_fingerprint: fingerprint.to_string(),
                    input_shape: json!({"shape": fingerprint}),
                    status,
                    failure_class,
                    error_summary: Some("bad guidance".to_string()),
                    result_summary: None,
                    duration_ms: 10,
                    retry_count: 0,
                    evidence_refs: Vec::new(),
                    payload: Value::Null,
                    created_at,
                })
                .expect("write invocation evidence");
        }

        let monitor = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("monitor");

        assert_eq!(
            monitor.status,
            LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        );
        assert_eq!(monitor.after_invocation_count, 3);
        assert_eq!(monitor.new_failure_classes, vec!["tool_misuse".to_string()]);
        assert!(monitor.rollback_recommendation_id.is_some());
    }

    #[test]
    fn phase7_negative_feedback_creates_follow_up_without_invocation_threshold() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, phase5_test_repo_root());
        let scope = LearningScope::new("anonymous".to_string(), "default".to_string());
        let promoted_at = Utc::now();
        let promotion = LearningCapabilityEvolutionPromotionRecord {
            id: "lcepromo_phase7_feedback".to_string(),
            scope: scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            validation_id: "validation".to_string(),
            implementation_id: None,
            application_id: None,
            capability_id: Some("skill:foo".to_string()),
            actor: "tester".to_string(),
            summary: "promoted".to_string(),
            applied_files: vec!["skills/foo/SKILL.md".to_string()],
            evidence_refs: Vec::new(),
            payload: Value::Null,
            created_at: promoted_at,
        };
        api.store
            .append_capability_evolution_promotion_record(&promotion)
            .expect("write promotion");
        api.store
            .append_event(
                scope.clone(),
                CreateLearningEventRequest {
                    principal: None,
                    workspace: None,
                    event_type: "learning_user_teaching_recorded".to_string(),
                    agent_id: None,
                    task_id: None,
                    execution_id: None,
                    chat_session_id: None,
                    summary: "User marked foo output wrong.".to_string(),
                    evidence_refs: Vec::new(),
                    payload: json!({
                        "action": "this_was_wrong",
                        "skill_name": "foo"
                    }),
                },
            )
            .expect("write teaching event");

        let monitor = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("monitor");

        assert_eq!(
            monitor.status,
            LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        );
        assert_eq!(monitor.after_invocation_count, 0);
        assert_eq!(monitor.user_negative_feedback_count, 1);
        assert!(monitor.rollback_recommendation_id.is_none());
        assert!(monitor.follow_up_candidate_id.is_some());
    }

    #[test]
    fn phase7_monitor_rerun_replaces_dismissed_rollback_recommendation() {
        let (_temp_dir, api, scope) = phase7_test_context();
        let promoted_at = Utc::now();
        let application = phase7_test_application(&scope, promoted_at);
        api.store
            .write_capability_evolution_application_record(&application)
            .expect("write application");
        let promotion = phase7_test_promotion(&scope, promoted_at, Some(&application));
        api.store
            .append_capability_evolution_promotion_record(&promotion)
            .expect("write promotion");
        phase7_write_regression_invocations(&api, &scope, promoted_at);

        let first = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("first monitor");
        let first_recommendation_id = first
            .rollback_recommendation_id
            .clone()
            .expect("first rollback recommendation");
        let mut recommendation = api
            .store
            .read_capability_evolution_rollback_recommendation_record(
                &scope,
                &promotion.candidate_id,
                &first_recommendation_id,
            )
            .expect("read first rollback recommendation");
        recommendation.status = LearningCapabilityEvolutionRollbackRecommendationStatus::Dismissed;
        api.store
            .write_capability_evolution_rollback_recommendation_record(&recommendation)
            .expect("dismiss first rollback recommendation");

        let second = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("second monitor");
        let second_recommendation_id = second
            .rollback_recommendation_id
            .clone()
            .expect("second rollback recommendation");

        assert_ne!(first_recommendation_id, second_recommendation_id);
        let active = api
            .store
            .read_capability_evolution_rollback_recommendation_record(
                &scope,
                &promotion.candidate_id,
                &second_recommendation_id,
            )
            .expect("read second rollback recommendation");
        assert_eq!(
            active.status,
            LearningCapabilityEvolutionRollbackRecommendationStatus::Recommended
        );
    }

    #[test]
    fn phase7_monitor_creates_follow_up_when_application_record_missing() {
        let (_temp_dir, api, scope) = phase7_test_context();
        let promoted_at = Utc::now();
        let mut promotion = phase7_test_promotion(&scope, promoted_at, None);
        promotion.application_id = Some("missing_application".to_string());
        promotion.implementation_id = Some("implementation".to_string());
        api.store
            .append_capability_evolution_promotion_record(&promotion)
            .expect("write promotion");
        phase7_write_regression_invocations(&api, &scope, promoted_at);

        let monitor = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("monitor");

        assert_eq!(
            monitor.status,
            LearningCapabilityEvolutionPostPromotionMonitorStatus::RegressionDetected
        );
        assert!(monitor.rollback_recommendation_id.is_none());
        let follow_up_id = monitor
            .follow_up_candidate_id
            .clone()
            .expect("fallback follow-up candidate");
        assert!(monitor.payload.get("application_lookup_error").is_some());
        let follow_up = api
            .store
            .read_candidate(&scope, &follow_up_id)
            .expect("read fallback follow-up");
        assert_eq!(follow_up.state, LearningCandidateState::Proposed);
    }

    #[test]
    fn phase7_monitor_rerun_replaces_terminal_follow_up_candidate() {
        let (_temp_dir, api, scope) = phase7_test_context();
        let promoted_at = Utc::now();
        let promotion = phase7_test_promotion(&scope, promoted_at, None);
        api.store
            .append_capability_evolution_promotion_record(&promotion)
            .expect("write promotion");
        phase7_append_negative_feedback(&api, &scope);

        let first = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("first monitor");
        let first_follow_up_id = first
            .follow_up_candidate_id
            .clone()
            .expect("first follow-up candidate");
        api.store
            .transition_candidate(
                &scope,
                &first_follow_up_id,
                LearningCandidateState::Archived,
                "tester",
                "follow_up_resolved",
                "The original follow-up was resolved; persistent regression needs a fresh candidate.",
                Vec::new(),
            )
            .expect("archive first follow-up candidate");

        let second = phase7_record_post_promotion_monitor(
            &api,
            &scope,
            &promotion,
            Phase7PostPromotionMonitorOptions {
                invocation_limit: 100,
                min_after_invocations: 3,
                failure_delta_threshold: 0.2,
                actor: "tester".to_string(),
                request_payload: Value::Null,
            },
        )
        .expect("second monitor");
        let second_follow_up_id = second
            .follow_up_candidate_id
            .clone()
            .expect("second follow-up candidate");

        assert_ne!(first_follow_up_id, second_follow_up_id);
        let second_follow_up = api
            .store
            .read_candidate(&scope, &second_follow_up_id)
            .expect("read second follow-up");
        assert_eq!(second_follow_up.state, LearningCandidateState::Proposed);
    }

    #[test]
    fn scoped_application_requires_full_replacement_content_for_skill_patch() {
        let skills_patch = LearningCapabilityEvolutionProposalPatch {
            path: "skills/foo/SKILL.md".to_string(),
            operation: "update".to_string(),
            summary: "update guide".to_string(),
            diff: None,
            metadata: json!({"new_content": "# Foo\n"}),
        };
        assert_eq!(
            extract_patch_replacement_content(&skills_patch).expect("content lookup"),
            Some("# Foo\n".to_string())
        );
    }

    #[test]
    fn passed_validation_requires_material_evidence() {
        assert!(!capability_validation_has_material_evidence(
            &[],
            &[],
            &Value::Null,
            &json!({})
        ));
        assert!(!capability_validation_has_material_evidence(
            &["  ".to_string()],
            &[],
            &json!([]),
            &Value::Null
        ));
        assert!(capability_validation_has_material_evidence(
            &["make test".to_string()],
            &[],
            &Value::Null,
            &Value::Null
        ));
        assert!(capability_validation_has_material_evidence(
            &[],
            &[],
            &json!({"passed": true}),
            &Value::Null
        ));
    }

    #[test]
    fn high_risk_skill_approval_requires_review_evidence() {
        assert!(!skill_evolution_review_evidence_required(
            &LearningRiskLevel::Low,
            &LearningRiskLevel::Medium
        ));
        assert!(skill_evolution_review_evidence_required(
            &LearningRiskLevel::High,
            &LearningRiskLevel::Medium
        ));
        assert!(skill_evolution_review_evidence_required(
            &LearningRiskLevel::Low,
            &LearningRiskLevel::Critical
        ));

        assert!(!skill_evolution_review_has_material_evidence(
            &[],
            &Value::Null
        ));
        assert!(skill_evolution_review_has_material_evidence(
            &[],
            &json!({"approval": "manual review complete"})
        ));
        assert!(skill_evolution_review_has_material_evidence(
            &[LearningEvidenceRef {
                kind: "review".to_string(),
                id: Some("approval-1".to_string()),
                path: None,
                uri: None,
                summary: Some("Manual high-risk review".to_string()),
            }],
            &Value::Null
        ));
    }

    #[test]
    fn scoped_skill_approval_requires_non_empty_eval_plan() {
        let now = Utc::now();
        let mut proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::ReadyForReview,
            title: "Improve skill".to_string(),
            summary: "Improve skill".to_string(),
            capability_id: Some("skill:test".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/foo/SKILL.md".to_string()],
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: None,
            validation_plan: None,
            promotion_gate: None,
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };

        assert!(skill_evolution_approval_requires_eval_plan(&proposal));
        assert!(!skill_evolution_proposal_has_eval_plan(&proposal));

        proposal.eval_plan = Some(json!({}));
        assert!(!skill_evolution_proposal_has_eval_plan(&proposal));

        proposal.eval_plan = Some(json!(true));
        assert!(!skill_evolution_proposal_has_eval_plan(&proposal));

        proposal.eval_plan = Some(json!({
            "commands": ["cargo test -p magician scoped_skill_approval_requires_non_empty_eval_plan --lib"],
            "expected": "focused skill evolution regression passes"
        }));
        assert!(skill_evolution_proposal_has_eval_plan(&proposal));

        proposal.proposed_files = vec!["docs/not-skill.md".to_string()];
        proposal.patches.clear();
        assert!(!skill_evolution_approval_requires_eval_plan(&proposal));
    }

    #[test]
    fn scoped_skill_approval_requires_non_empty_promotion_gate() {
        let now = Utc::now();
        let mut proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::ReadyForReview,
            title: "Improve skill".to_string(),
            summary: "Improve skill".to_string(),
            capability_id: Some("skill:test".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/foo/SKILL.md".to_string()],
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: Some(json!({
                "commands": ["cargo test -p magician scoped_skill_approval_requires_non_empty_promotion_gate --lib"],
                "expected": "focused skill evolution regression passes"
            })),
            validation_plan: None,
            promotion_gate: None,
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };

        assert!(skill_evolution_approval_requires_promotion_gate(&proposal));
        assert!(!skill_evolution_proposal_has_promotion_gate(&proposal));

        proposal.promotion_gate = Some(json!({}));
        assert!(!skill_evolution_proposal_has_promotion_gate(&proposal));

        proposal.promotion_gate = Some(json!(42));
        assert!(!skill_evolution_proposal_has_promotion_gate(&proposal));

        proposal.promotion_gate = Some(json!({
            "meta_harness_review": "required before promotion",
            "human_approval": "required for scoped skill mutation",
            "rollback": "previous file content is stored in application record"
        }));
        assert!(skill_evolution_proposal_has_promotion_gate(&proposal));

        proposal.proposed_files = vec!["docs/not-skill.md".to_string()];
        proposal.patches.clear();
        assert!(!skill_evolution_approval_requires_promotion_gate(&proposal));
    }

    #[test]
    fn proposal_drafter_seeds_skill_eval_validation_and_promotion_gate() {
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_candidate".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Queued,
            candidate_type:
                magician::magician_v2::learning::LearningCandidateType::ToolSchemaUpdate,
            title: "Improve WhatsApp schema".to_string(),
            summary: "React command needs guide/schema coverage".to_string(),
            rationale: "Observed wrapper friction".to_string(),
            capability_id: Some("skill:whatsapp".to_string()),
            failure_pattern: Some("reaction command failed".to_string()),
            proposed_fix_type: Some("tool_schema_update".to_string()),
            proposed_files: Vec::new(),
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:whatsapp".to_string()),
            risk_level: LearningRiskLevel::Medium,
            dedupe_fingerprint: None,
            recurrence_count: 0,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: false,
            rank_score: 0.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "reaction command can be used correctly"}),
            created_at: now,
            updated_at: now,
        };

        let proposal =
            draft_capability_evolution_proposal(&scope, &backlog, None, "test_drafter", true);

        assert_eq!(
            proposal.status,
            LearningCapabilityEvolutionProposalStatus::ReadyForReview
        );
        assert_eq!(
            proposal.proposed_files,
            vec!["skills/whatsapp/tool_schema.yaml".to_string()]
        );
        assert!(skill_evolution_proposal_has_eval_plan(&proposal));
        assert!(skill_evolution_proposal_has_promotion_gate(&proposal));
        assert!(proposal.validation_plan.is_some());
    }

    #[test]
    fn proposal_drafter_creates_discoverable_skill_guidance_stub() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_workflow".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Queued,
            candidate_type:
                magician::magician_v2::learning::LearningCandidateType::WorkflowTemplate,
            title: "Reusable analyst workflow".to_string(),
            summary: "Repeated analyst workflow should become reusable.".to_string(),
            rationale: "Two successful runs used the same sequence.".to_string(),
            capability_id: Some("skill:analyst-workflow".to_string()),
            failure_pattern: Some("repeatable data workflow".to_string()),
            proposed_fix_type: Some("workflow_template".to_string()),
            proposed_files: Vec::new(),
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:analyst-workflow".to_string()),
            risk_level: LearningRiskLevel::Medium,
            dedupe_fingerprint: None,
            recurrence_count: 0,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: false,
            rank_score: 0.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({
                "expected_behavior": "analyst workflow can be activated later",
                "procedure_steps": ["inspect source data", "validate result"]
            }),
            created_at: now,
            updated_at: now,
        };

        let mut proposal =
            draft_capability_evolution_proposal(&scope, &backlog, None, "test_drafter", true);
        enrich_capability_evolution_proposal_draft(
            &workspace,
            phase5_test_repo_root(),
            &scope,
            &backlog,
            &mut proposal,
        );

        let patch = proposal
            .patches
            .iter()
            .find(|patch| patch.path == "skills/analyst-workflow/SKILL.md")
            .expect("skill guidance patch");
        let content = patch
            .metadata
            .get("new_content")
            .and_then(Value::as_str)
            .expect("new content");
        assert!(content.starts_with("---\nname: \"analyst-workflow\"\n"));
        assert!(
            content.contains("description: \"Repeated analyst workflow should become reusable.\"")
        );
        assert!(content.contains("## Learned Operating Guidance"));
        assert!(content.contains("Procedure steps"));
    }

    #[test]
    fn implementation_bundle_drafter_materializes_low_risk_skill_guidance_patch() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_guidance".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Validated,
            candidate_type: magician::magician_v2::learning::LearningCandidateType::SkillUpdate,
            title: "Improve analyst guidance".to_string(),
            summary: "Analyst skill needs activation guidance.".to_string(),
            rationale: "Repeated routing misses selected the wrong skill.".to_string(),
            capability_id: Some("skill:analyst-workflow".to_string()),
            failure_pattern: Some("activation guidance was too vague".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: Vec::new(),
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:analyst-workflow".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: None,
            recurrence_count: 2,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 10.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({
                "expected_behavior": "analyst workflow is selected only for repeated data analysis tasks",
                "trigger_conditions": "Use when the user asks to inspect, validate, and summarize tabular evidence."
            }),
            created_at: now,
            updated_at: now,
        };
        let mut proposal =
            draft_capability_evolution_proposal(&scope, &backlog, None, "test_drafter", true);
        enrich_capability_evolution_proposal_draft(
            &workspace,
            phase5_test_repo_root(),
            &scope,
            &backlog,
            &mut proposal,
        );
        proposal.status = LearningCapabilityEvolutionProposalStatus::Approved;
        let validation = LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: scope.clone(),
            candidate_id: proposal.candidate_id.clone(),
            proposal_id: proposal.id.clone(),
            status: LearningCapabilityEvolutionValidationStatus::Passed,
            capability_id: proposal.capability_id.clone(),
            runner: "test".to_string(),
            summary: "Validation passed.".to_string(),
            commands: vec!["cargo test -p magician implementation_bundle_drafter".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": true}),
            payload: Value::Null,
            created_at: now,
        };

        let record = draft_capability_evolution_implementation_bundle_record(
            &scope,
            "test_actor",
            &backlog,
            &proposal,
            &validation,
            None,
            Value::Null,
        )
        .expect("implementation bundle");

        assert_eq!(
            record.applied_files,
            vec!["skills/analyst-workflow/SKILL.md"]
        );
        assert_eq!(record.patches.len(), 1);
        let patch = &record.patches[0];
        assert_eq!(patch.path, "skills/analyst-workflow/SKILL.md");
        assert_eq!(
            patch.metadata.get("generated_by").and_then(Value::as_str),
            Some("skill_evolution_implementation_bundle_drafter")
        );
        assert_eq!(
            patch
                .metadata
                .get("exact_target_file")
                .and_then(Value::as_str),
            Some("skills/analyst-workflow/SKILL.md")
        );
        let content = extract_patch_replacement_content(patch)
            .expect("content lookup")
            .expect("full replacement content");
        assert!(content.contains("## Learned Operating Guidance"));
        assert_eq!(
            record
                .payload
                .get("review_required_before_apply")
                .and_then(Value::as_bool),
            Some(true)
        );
    }

    #[test]
    fn implementation_bundle_drafter_materializes_low_risk_tool_schema_patch() {
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_schema".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Validated,
            candidate_type:
                magician::magician_v2::learning::LearningCandidateType::ToolSchemaUpdate,
            title: "Improve demo tool schema".to_string(),
            summary: "Demo tool schema needs a reviewed argument fix.".to_string(),
            rationale: "Observed wrapper/schema mismatch.".to_string(),
            capability_id: Some("skill:demo-tool".to_string()),
            failure_pattern: Some("required argument was missing from schema".to_string()),
            proposed_fix_type: Some("tool_schema_update".to_string()),
            proposed_files: vec!["skills/demo-tool/tool_schema.yaml".to_string()],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:demo-tool".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: None,
            recurrence_count: 2,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 10.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "schema exposes the argument accepted by the wrapper"}),
            created_at: now,
            updated_at: now,
        };
        let schema_content = "tools:\n  - name: demo\n    description: Demo tool.\n";
        let proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: scope.clone(),
            candidate_id: backlog.candidate_id.clone(),
            backlog_id: backlog.id.clone(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Review schema fix".to_string(),
            summary: "Schema fix is reviewed and ready to bundle.".to_string(),
            capability_id: backlog.capability_id.clone(),
            proposed_fix_type: backlog.proposed_fix_type.clone(),
            proposed_files: backlog.proposed_files.clone(),
            change_plan: json!({}),
            patches: vec![LearningCapabilityEvolutionProposalPatch {
                path: "skills/demo-tool/tool_schema.yaml".to_string(),
                operation: "update".to_string(),
                summary: "Update demo tool schema.".to_string(),
                diff: Some(
                    "Full replacement schema patch reviewed by proposal approval.".to_string(),
                ),
                metadata: json!({
                    "content_kind": "full_replacement",
                    "new_content": schema_content,
                    "reviewed_patch_content": true
                }),
            }],
            eval_plan: None,
            validation_plan: Some(json!({"commands": ["make check-all"]})),
            promotion_gate: Some(json!({"regression_required": false})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        let validation = LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: scope.clone(),
            candidate_id: proposal.candidate_id.clone(),
            proposal_id: proposal.id.clone(),
            status: LearningCapabilityEvolutionValidationStatus::Passed,
            capability_id: proposal.capability_id.clone(),
            runner: "test".to_string(),
            summary: "Schema validation passed.".to_string(),
            commands: vec!["make check-all".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": true}),
            payload: Value::Null,
            created_at: now,
        };

        let record = draft_capability_evolution_implementation_bundle_record(
            &scope,
            "test_actor",
            &backlog,
            &proposal,
            &validation,
            None,
            Value::Null,
        )
        .expect("schema implementation bundle");

        assert_eq!(
            record.applied_files,
            vec!["skills/demo-tool/tool_schema.yaml"]
        );
        assert_eq!(
            record.payload.get("scope").and_then(Value::as_str),
            Some("low_risk_tool_schema")
        );
        let patch = &record.patches[0];
        assert_eq!(
            patch.metadata.get("artifact_kind").and_then(Value::as_str),
            Some("tool_schema")
        );
        assert_eq!(
            patch
                .metadata
                .get("exact_target_file")
                .and_then(Value::as_str),
            Some("skills/demo-tool/tool_schema.yaml")
        );
        assert_eq!(
            extract_patch_replacement_content(patch)
                .expect("content lookup")
                .as_deref(),
            Some(schema_content)
        );
    }

    #[test]
    fn implementation_bundle_drafter_materializes_low_risk_wrapper_script_patch() {
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_wrapper".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Validated,
            candidate_type: magician::magician_v2::learning::LearningCandidateType::ToolWrapperFix,
            title: "Improve demo tool wrapper".to_string(),
            summary: "Demo tool wrapper needs a reviewed smoke-safe fix.".to_string(),
            rationale: "Observed wrapper runtime mismatch.".to_string(),
            capability_id: Some("skill:demo-tool".to_string()),
            failure_pattern: Some("wrapper passed the wrong argument name".to_string()),
            proposed_fix_type: Some("tool_wrapper_fix".to_string()),
            proposed_files: vec!["skills/demo-tool/scripts/run_wrapper.py".to_string()],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:demo-tool".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: None,
            recurrence_count: 2,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 10.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "wrapper maps reviewed schema args to runtime args"}),
            created_at: now,
            updated_at: now,
        };
        let wrapper_content = "#!/usr/bin/env python3\nprint('demo wrapper')\n";
        let proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: scope.clone(),
            candidate_id: backlog.candidate_id.clone(),
            backlog_id: backlog.id.clone(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Review wrapper fix".to_string(),
            summary: "Wrapper fix is reviewed and ready to bundle.".to_string(),
            capability_id: backlog.capability_id.clone(),
            proposed_fix_type: backlog.proposed_fix_type.clone(),
            proposed_files: backlog.proposed_files.clone(),
            change_plan: json!({}),
            patches: vec![LearningCapabilityEvolutionProposalPatch {
                path: "skills/demo-tool/scripts/run_wrapper.py".to_string(),
                operation: "update".to_string(),
                summary: "Update demo wrapper script.".to_string(),
                diff: Some(
                    "Full replacement wrapper patch reviewed by proposal approval.".to_string(),
                ),
                metadata: json!({
                    "content_kind": "full_replacement",
                    "new_content": wrapper_content,
                    "reviewed_patch_content": true
                }),
            }],
            eval_plan: None,
            validation_plan: Some(json!({
                "commands": ["python3 skills/demo-tool/scripts/run_wrapper.py --help"]
            })),
            promotion_gate: Some(json!({"regression_required": false})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        let validation = LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: scope.clone(),
            candidate_id: proposal.candidate_id.clone(),
            proposal_id: proposal.id.clone(),
            status: LearningCapabilityEvolutionValidationStatus::Passed,
            capability_id: proposal.capability_id.clone(),
            runner: "test".to_string(),
            summary: "Wrapper smoke validation passed.".to_string(),
            commands: vec!["python3 skills/demo-tool/scripts/run_wrapper.py --help".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": true}),
            payload: Value::Null,
            created_at: now,
        };

        let record = draft_capability_evolution_implementation_bundle_record(
            &scope,
            "test_actor",
            &backlog,
            &proposal,
            &validation,
            None,
            Value::Null,
        )
        .expect("wrapper implementation bundle");

        assert_eq!(
            record.applied_files,
            vec!["skills/demo-tool/scripts/run_wrapper.py"]
        );
        assert_eq!(
            record.payload.get("scope").and_then(Value::as_str),
            Some("low_risk_wrapper_script")
        );
        let patch = &record.patches[0];
        assert_eq!(
            patch.metadata.get("artifact_kind").and_then(Value::as_str),
            Some("wrapper_script")
        );
        assert_eq!(
            patch
                .metadata
                .get("exact_target_file")
                .and_then(Value::as_str),
            Some("skills/demo-tool/scripts/run_wrapper.py")
        );
        assert_eq!(
            extract_patch_replacement_content(patch)
                .expect("content lookup")
                .as_deref(),
            Some(wrapper_content)
        );
    }

    #[test]
    fn implementation_bundle_drafter_refuses_unreviewed_wrapper_script_patch() {
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_wrapper_unreviewed".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Validated,
            candidate_type: magician::magician_v2::learning::LearningCandidateType::ToolWrapperFix,
            title: "Improve demo tool wrapper".to_string(),
            summary: "Demo tool wrapper needs a reviewed smoke-safe fix.".to_string(),
            rationale: "Observed wrapper runtime mismatch.".to_string(),
            capability_id: Some("skill:demo-tool".to_string()),
            failure_pattern: Some("wrapper passed the wrong argument name".to_string()),
            proposed_fix_type: Some("tool_wrapper_fix".to_string()),
            proposed_files: vec!["skills/demo-tool/scripts/run_wrapper.py".to_string()],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:demo-tool".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: None,
            recurrence_count: 2,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 10.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "wrapper maps reviewed schema args to runtime args"}),
            created_at: now,
            updated_at: now,
        };
        let proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: scope.clone(),
            candidate_id: backlog.candidate_id.clone(),
            backlog_id: backlog.id.clone(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Review wrapper fix".to_string(),
            summary: "Wrapper fix is not reviewed yet.".to_string(),
            capability_id: backlog.capability_id.clone(),
            proposed_fix_type: backlog.proposed_fix_type.clone(),
            proposed_files: backlog.proposed_files.clone(),
            change_plan: json!({}),
            patches: vec![LearningCapabilityEvolutionProposalPatch {
                path: "skills/demo-tool/scripts/run_wrapper.py".to_string(),
                operation: "update".to_string(),
                summary: "Update demo wrapper script.".to_string(),
                diff: Some("Full replacement wrapper patch scaffold.".to_string()),
                metadata: json!({
                    "content_kind": "full_replacement",
                    "new_content": "#!/usr/bin/env python3\nprint('demo wrapper')\n"
                }),
            }],
            eval_plan: None,
            validation_plan: Some(json!({
                "commands": ["python3 skills/demo-tool/scripts/run_wrapper.py --help"]
            })),
            promotion_gate: Some(json!({"regression_required": false})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        let validation = LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: scope.clone(),
            candidate_id: proposal.candidate_id.clone(),
            proposal_id: proposal.id.clone(),
            status: LearningCapabilityEvolutionValidationStatus::Passed,
            capability_id: proposal.capability_id.clone(),
            runner: "test".to_string(),
            summary: "Wrapper smoke validation passed.".to_string(),
            commands: vec!["python3 skills/demo-tool/scripts/run_wrapper.py --help".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"passed": true}),
            payload: Value::Null,
            created_at: now,
        };

        let error = draft_capability_evolution_implementation_bundle_record(
            &scope,
            "test_actor",
            &backlog,
            &proposal,
            &validation,
            None,
            Value::Null,
        )
        .expect_err("unreviewed wrapper patch should not bundle");

        assert!(error.contains("metadata.reviewed_patch_content=true"));
    }

    #[test]
    fn proposal_phase4_phase5_metadata_marks_schema_wrapper_and_validation_plans_for_review() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let now = Utc::now();
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: "backlog".to_string(),
            scope: scope.clone(),
            candidate_id: "lc_schema_wrapper".to_string(),
            status: LearningCapabilityEvolutionBacklogStatus::Queued,
            candidate_type: magician::magician_v2::learning::LearningCandidateType::ToolWrapperFix,
            title: "Improve demo tool wrapper".to_string(),
            summary: "Wrapper and schema both need reviewable changes.".to_string(),
            rationale: "Observed runtime argument mismatch.".to_string(),
            capability_id: Some("skill:demo-tool".to_string()),
            failure_pattern: Some("wrapper accepted args that schema did not expose".to_string()),
            proposed_fix_type: Some("tool_wrapper_fix".to_string()),
            proposed_files: vec![
                "skills/demo-tool/tool_schema.yaml".to_string(),
                "skills/demo-tool/scripts/run_wrapper.py".to_string(),
            ],
            required_eval: None,
            promotion_gate: None,
            proposed_target: Some("skill:demo-tool".to_string()),
            risk_level: LearningRiskLevel::Medium,
            dedupe_fingerprint: None,
            recurrence_count: 1,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: false,
            rank_score: 0.0,
            rank_reasons: Vec::new(),
            owner_hints: Vec::new(),
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "schema and wrapper agree on accepted args"}),
            created_at: now,
            updated_at: now,
        };
        let scope_root = workspace.scope_root(&scope.principal, &scope.workspace);
        std::fs::create_dir_all(scope_root.join("skills/demo-tool/scripts"))
            .expect("create demo skill dirs");
        std::fs::create_dir_all(scope_root.join("skills/demo-tool/tests"))
            .expect("create demo skill test dirs");
        std::fs::write(
            scope_root.join("skills/demo-tool/tool_schema.yaml"),
            "name: demo-tool\nparameters:\n- name: command\n  required: true\n  type: string\n  description: Existing command.\n",
        )
        .expect("write local schema");
        std::fs::write(
            scope_root.join("skills/demo-tool/scripts/run_wrapper.py"),
            "#!/usr/bin/env python3\nimport argparse\nprint('ok')\n",
        )
        .expect("write local wrapper");
        std::fs::write(
            scope_root.join("skills/demo-tool/tests/test_wrapper.py"),
            "import unittest\n\n\nclass WrapperSmokeTest(unittest.TestCase):\n    def test_smoke(self):\n        self.assertTrue(True)\n\n\nif __name__ == '__main__':\n    unittest.main()\n",
        )
        .expect("write local wrapper test");

        let mut proposal =
            draft_capability_evolution_proposal(&scope, &backlog, None, "test_drafter", true);
        enrich_capability_evolution_proposal_draft(
            &workspace,
            phase5_test_repo_root(),
            &scope,
            &backlog,
            &mut proposal,
        );

        let schema_patch = proposal
            .patches
            .iter()
            .find(|patch| patch.path == "skills/demo-tool/tool_schema.yaml")
            .expect("tool schema scaffold patch");
        assert_eq!(schema_patch.operation, "update");
        assert_eq!(
            schema_patch
                .metadata
                .get("artifact_kind")
                .and_then(Value::as_str),
            Some("tool_schema")
        );
        assert_eq!(
            schema_patch
                .metadata
                .get("patch_scaffold")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            schema_patch
                .metadata
                .get("reviewed_patch_content")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            schema_patch
                .metadata
                .get("phase4")
                .and_then(|value| value.get("requires_reviewed_patch_content"))
                .and_then(Value::as_bool),
            Some(true)
        );
        let schema_content = schema_patch
            .metadata
            .get("new_content")
            .and_then(Value::as_str)
            .expect("schema new content");
        assert!(schema_content.contains("Skill Evolution Phase 4 review scaffold"));
        assert!(schema_content.contains("wrapper accepted args that schema did not expose"));
        assert!(schema_content.contains("schema and wrapper agree on accepted args"));
        assert!(schema_content.contains("Local schema metadata: tool name `demo-tool`"));
        assert!(schema_content.contains("name: demo-tool"));
        serde_yaml::from_str::<serde_yaml::Value>(schema_content).expect("schema yaml parses");
        assert_eq!(
            schema_patch
                .metadata
                .get("phase4_content_generation")
                .and_then(|value| value.get("local_artifact"))
                .and_then(|value| value.get("tool_name"))
                .and_then(Value::as_str),
            Some("demo-tool")
        );

        let wrapper_patch = proposal
            .patches
            .iter()
            .find(|patch| patch.path == "skills/demo-tool/scripts/run_wrapper.py")
            .expect("wrapper scaffold patch");
        assert_eq!(wrapper_patch.operation, "update");
        assert_eq!(
            wrapper_patch
                .metadata
                .get("artifact_kind")
                .and_then(Value::as_str),
            Some("wrapper_script")
        );
        assert_eq!(
            wrapper_patch
                .metadata
                .get("patch_scaffold")
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            wrapper_patch
                .metadata
                .get("reviewed_patch_content")
                .and_then(Value::as_bool),
            Some(false)
        );
        assert_eq!(
            wrapper_patch
                .metadata
                .get("phase4")
                .and_then(|value| value.get("requires_reviewed_patch_content"))
                .and_then(Value::as_bool),
            Some(true)
        );
        let wrapper_content = wrapper_patch
            .metadata
            .get("new_content")
            .and_then(Value::as_str)
            .expect("wrapper new content");
        assert!(wrapper_content.starts_with("#!/usr/bin/env python3\n# magician-phase4-review:"));
        assert!(wrapper_content.contains("Skill Evolution Phase 4 review scaffold"));
        assert!(wrapper_content.contains("wrapper accepted args that schema did not expose"));
        assert!(wrapper_content.contains("Local wrapper metadata: language `python`"));
        assert!(wrapper_content.contains("import argparse"));
        assert_eq!(
            wrapper_patch
                .metadata
                .get("phase4_content_generation")
                .and_then(|value| value.get("local_artifact"))
                .and_then(|value| value.get("language"))
                .and_then(Value::as_str),
            Some("python")
        );

        let phase4 = proposal.change_plan.get("phase4").expect("phase4 metadata");
        assert_eq!(
            phase4
                .get("tool_schema_change_plan")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("implementation_bundle_drafter_supported_after_review")
        );
        assert_eq!(
            phase4
                .get("wrapper_script_patch_plan")
                .and_then(|value| value.get("status"))
                .and_then(Value::as_str),
            Some("implementation_bundle_drafter_supported_after_review")
        );
        assert_eq!(
            phase4
                .get("implementation_bundle_policy")
                .and_then(|value| value.get("auto_draft_scope"))
                .and_then(Value::as_str),
            Some("low_risk_scoped_skill_guidance_tool_schema_or_wrapper_script")
        );

        let validation_plan = proposal.validation_plan.as_ref().expect("validation plan");
        let phase5 = validation_plan
            .get("phase5_generated")
            .expect("phase5 generated validation plan");
        assert_eq!(
            phase5.get("source").and_then(Value::as_str),
            Some("capability_evolution_phase5_validation_drafter")
        );
        let generated_commands = phase5
            .get("commands")
            .and_then(Value::as_array)
            .expect("phase5 commands")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        let skill_command =
            phase5_skill_frontmatter_command(phase5_test_repo_root(), "skills/demo-tool")
                .expect("skill validation command");
        let schema_command = phase5_tool_schema_command(
            phase5_test_repo_root(),
            "skills/demo-tool/tool_schema.yaml",
        )
        .expect("schema validation command");
        assert!(generated_commands.contains(&skill_command.as_str()));
        assert!(generated_commands.contains(&schema_command.as_str()));
        assert!(generated_commands
            .contains(&"python3 -m py_compile skills/demo-tool/scripts/run_wrapper.py"));
        assert!(
            generated_commands.contains(&"python3 -m unittest discover -s skills/demo-tool/tests")
        );
        let generated_regression_commands = phase5
            .get("regression_commands")
            .and_then(Value::as_array)
            .expect("phase5 regression commands")
            .iter()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>();
        assert!(generated_regression_commands
            .contains(&"python3 skills/demo-tool/scripts/run_wrapper.py --help"));
        let fixture_command = phase5_wrapper_fixture_command(
            phase5_test_repo_root(),
            "skills/demo-tool/tool_schema.yaml",
            "skills/demo-tool/scripts/run_wrapper.py",
        )
        .expect("fixture validation command");
        assert!(generated_regression_commands.contains(&fixture_command.as_str()));
        let fixture_case = phase5
            .get("fixture_cases")
            .and_then(Value::as_array)
            .and_then(|items| items.first())
            .expect("phase5 fixture case");
        assert_eq!(
            fixture_case.get("kind").and_then(Value::as_str),
            Some("wrapper_regression_fixture")
        );
        assert_eq!(
            fixture_case
                .get("regression_command")
                .and_then(Value::as_str),
            Some(fixture_command.as_str())
        );
        assert_eq!(
            fixture_case
                .get("sample_inputs")
                .and_then(|value| value.get("command"))
                .and_then(Value::as_str),
            Some("sample_command")
        );
        assert_eq!(
            fixture_case
                .get("command_blueprint")
                .and_then(|value| value.get("env"))
                .and_then(|value| value.get("_TOOL_COMMAND"))
                .and_then(Value::as_str),
            Some("sample_command")
        );

        let planned_without_regression = planned_validation_commands(&proposal, &[], false);
        assert!(planned_without_regression
            .iter()
            .any(|spec| spec.command == schema_command && !spec.regression));
        assert!(planned_without_regression.iter().any(|spec| {
            spec.command == "python3 -m unittest discover -s skills/demo-tool/tests"
                && !spec.regression
        }));
        assert!(!planned_without_regression
            .iter()
            .any(|spec| spec.command.contains("--help") && spec.regression));
        let planned_with_regression = planned_validation_commands(&proposal, &[], true);
        assert!(planned_with_regression.iter().any(|spec| {
            spec.command == "python3 skills/demo-tool/scripts/run_wrapper.py --help"
                && spec.regression
        }));
        assert!(planned_with_regression
            .iter()
            .any(|spec| spec.command == fixture_command && spec.regression));
    }

    #[test]
    fn phase5_fixture_results_record_failure_class_coverage() {
        let fixture_cases = vec![json!({
            "id": "phase5_fixture_lc_demo_wrapper",
            "kind": "wrapper_regression_fixture",
            "failure_class": "wrapper_argument_mismatch",
            "target_file": "skills/demo-tool/scripts/run_wrapper.py",
            "tool_schema_file": "skills/demo-tool/tool_schema.yaml",
            "observed_failure": "wrapper accepted args that schema did not expose",
            "expected_behavior": "schema and wrapper agree on accepted args",
            "regression_command": "python3 scripts/validate_skill_artifact.py --tool-schema skills/demo-tool/tool_schema.yaml --wrapper-fixture skills/demo-tool/scripts/run_wrapper.py"
        })];
        let command_results = vec![ValidationCommandResult {
            command: "python3 scripts/validate_skill_artifact.py --tool-schema skills/demo-tool/tool_schema.yaml --wrapper-fixture skills/demo-tool/scripts/run_wrapper.py".to_string(),
            source: "validation_plan.phase5_generated.regression_commands[0]".to_string(),
            regression: true,
            success: true,
            status_code: Some(0),
            timed_out: false,
            spawn_error: None,
            duration_ms: 12,
            stdout: r#"{"fixtures":[{"tool_name":"demo-tool"}]}"#.to_string(),
            stderr: "OK: 1 tool schema file(s) validated".to_string(),
        }];

        let fixture_results = phase5_fixture_results_for_commands(&fixture_cases, &command_results);
        assert_eq!(fixture_results.len(), 1);
        assert_eq!(
            fixture_results[0].get("status").and_then(Value::as_str),
            Some("passed")
        );
        assert_eq!(
            fixture_results[0]
                .get("failure_class")
                .and_then(Value::as_str),
            Some("wrapper_argument_mismatch")
        );
        assert_eq!(
            fixture_results[0]
                .get("validator_output")
                .and_then(|value| value.get("fixture_count"))
                .and_then(Value::as_u64),
            Some(1)
        );

        let metrics = phase5_fixture_result_metrics(&fixture_cases, &fixture_results);
        assert_eq!(metrics.get("case_count").and_then(Value::as_u64), Some(1));
        assert_eq!(metrics.get("passed_count").and_then(Value::as_u64), Some(1));
        assert_eq!(
            metrics
                .get("covered_failure_classes")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .and_then(Value::as_str),
            Some("wrapper_argument_mismatch")
        );
    }

    #[test]
    fn validation_command_planner_extracts_and_filters_regression_commands() {
        let now = Utc::now();
        let mut proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Improve skill".to_string(),
            summary: "Improve skill".to_string(),
            capability_id: Some("skill:test".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/foo/SKILL.md".to_string()],
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: Some(json!({
                "commands": ["printf eval"],
                "nested": { "regression_commands": ["printf regression"] }
            })),
            validation_plan: Some(json!({
                "commands": ["printf validation", "printf validation"]
            })),
            promotion_gate: Some(json!({"regression_required": true})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };

        let without_regression = planned_validation_commands(&proposal, &[], false);
        assert_eq!(
            without_regression
                .iter()
                .map(|spec| spec.command.as_str())
                .collect::<Vec<_>>(),
            vec!["printf validation", "printf eval"]
        );

        let with_regression = planned_validation_commands(&proposal, &[], true);
        assert!(with_regression.iter().any(|spec| spec.regression));
        assert_eq!(with_regression.len(), 3);

        proposal.validation_plan = None;
        let overrides =
            planned_validation_commands(&proposal, &["printf override".to_string()], true);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].command, "printf override");
    }

    #[test]
    fn evaluation_command_planner_reuses_case_spec_commands() {
        let now = Utc::now();
        let backlog = LearningEvaluationBacklogItem {
            id: "leb_candidate".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            status: LearningEvaluationBacklogStatus::Queued,
            title: "Regression eval".to_string(),
            summary: "Regression eval".to_string(),
            rationale: "Needed by meta harness".to_string(),
            case_kind: "regression".to_string(),
            priority: "high".to_string(),
            target_agent_id: Some("personal-assistant".to_string()),
            focus_area: Some("browser".to_string()),
            proposed_target: None,
            source_agent_id: Some("personal-assistant".to_string()),
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            case_spec: json!({
                "commands": ["printf eval"],
                "nested": {
                    "regression_commands": ["printf regression"]
                }
            }),
            created_at: now,
            updated_at: now,
        };

        let without_regression = planned_evaluation_commands(&backlog, &[], false);
        assert_eq!(without_regression.len(), 1);
        assert_eq!(without_regression[0].command, "printf eval");

        let with_regression = planned_evaluation_commands(&backlog, &[], true);
        assert_eq!(with_regression.len(), 2);
        assert!(with_regression.iter().any(|spec| spec.regression));

        let overrides =
            planned_evaluation_commands(&backlog, &["printf override".to_string()], true);
        assert_eq!(overrides.len(), 1);
        assert_eq!(overrides[0].source, "request.commands");
    }

    #[test]
    fn promotion_gate_requires_regression_validation_evidence_when_requested() {
        let now = Utc::now();
        let proposal = LearningCapabilityEvolutionProposal {
            id: "proposal".to_string(),
            scope: LearningScope {
                principal: "anonymous".to_string(),
                workspace: "default".to_string(),
            },
            candidate_id: "candidate".to_string(),
            backlog_id: "backlog".to_string(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Improve skill".to_string(),
            summary: "Improve skill".to_string(),
            capability_id: Some("skill:test".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/foo/SKILL.md".to_string()],
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: Some(json!({"commands": ["printf eval"]})),
            validation_plan: Some(json!({"commands": ["printf validation"]})),
            promotion_gate: Some(json!({"regression_required": true})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        assert!(promotion_gate_requires_regression(&proposal));

        let mut report = LearningCapabilityEvolutionValidationReport {
            id: "validation".to_string(),
            scope: proposal.scope.clone(),
            candidate_id: "candidate".to_string(),
            proposal_id: "proposal".to_string(),
            status: LearningCapabilityEvolutionValidationStatus::Passed,
            capability_id: proposal.capability_id.clone(),
            runner: "test".to_string(),
            summary: "passed".to_string(),
            commands: vec!["printf validation".to_string()],
            evidence_refs: Vec::new(),
            metrics: json!({"regression_checked": false}),
            payload: json!({}),
            created_at: now,
        };
        assert!(!validation_report_has_regression_evidence(&report));

        report.metrics = json!({"regression_checked": true});
        assert!(validation_report_has_regression_evidence(&report));
    }

    #[tokio::test]
    async fn steward_drafts_proposal_and_generates_eval_for_safe_backlog() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, temp_dir.path());
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let candidate = api
            .store
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::SkillUpdate,
                    state: LearningCandidateState::Proposed,
                    title: "Improve demo skill".to_string(),
                    summary: "Demo skill needs durable guidance.".to_string(),
                    rationale: "Observed repeated success pattern.".to_string(),
                    proposed_change: json!({}),
                    proposed_target: Some("skill:demo-skill".to_string()),
                    confidence: Some(0.92),
                    source_agent_id: None,
                    source_task_id: None,
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    risk_level: LearningRiskLevel::Low,
                    review_required: false,
                    review_reason: None,
                    review_policy: json!({}),
                    promotion_target: None,
                    promotion_policy: json!({}),
                },
            )
            .expect("candidate");
        let now = Utc::now();
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: format!("lceb_{}", candidate.id.trim_start_matches("lc_")),
            scope: scope.clone(),
            candidate_id: candidate.id.clone(),
            status: LearningCapabilityEvolutionBacklogStatus::Queued,
            candidate_type: LearningCandidateType::SkillUpdate,
            title: candidate.title.clone(),
            summary: candidate.summary.clone(),
            rationale: candidate.rationale.clone(),
            capability_id: Some("skill:demo-skill".to_string()),
            failure_pattern: Some("repeatable workflow needs activation guidance".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: Vec::new(),
            required_eval: Some(json!({"commands": ["make docs-remind"]})),
            promotion_gate: None,
            proposed_target: Some("skill:demo-skill".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: Some("demo".to_string()),
            recurrence_count: 2,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 42.0,
            rank_reasons: vec!["low risk".to_string()],
            owner_hints: vec!["autonomous_steward_can_draft_validate".to_string()],
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "demo skill can be selected reliably"}),
            created_at: now,
            updated_at: now,
        };
        api.store
            .write_capability_evolution_backlog_item(&backlog)
            .expect("backlog");

        let report = run_capability_evolution_steward_cycle(
            &api,
            scope.clone(),
            RunLearningCapabilityEvolutionStewardRequest {
                workspace: None,
                actor: "test_steward".to_string(),
                backlog_limit: Some(5),
                max_proposals: Some(1),
                max_evaluations: Some(1),
                max_validations: Some(0),
                max_implementations: Some(0),
                max_dry_runs: Some(0),
                max_touched_files: Some(4),
                timeout_seconds: Some(1),
                include_regression: Some(false),
                dry_run: false,
                payload: Value::Null,
            },
        )
        .await;

        assert_eq!(
            report.status,
            LearningCapabilityEvolutionStewardRunStatus::Completed
        );
        assert!(report.actions.iter().any(|action| {
            action.action == "draft_proposal"
                && action.status == LearningCapabilityEvolutionStewardActionStatus::Completed
        }));
        assert!(report.actions.iter().any(|action| {
            action.action == "generate_evaluation"
                && action.status == LearningCapabilityEvolutionStewardActionStatus::Completed
        }));
        assert!(report.actions.iter().any(|action| {
            action.action == "request_review"
                && action.status
                    == LearningCapabilityEvolutionStewardActionStatus::AttentionRequired
        }));

        let proposal = api
            .store
            .read_capability_evolution_proposal(&scope, &candidate.id)
            .expect("proposal");
        assert_eq!(
            proposal.status,
            LearningCapabilityEvolutionProposalStatus::ReadyForReview
        );
        let evaluation = api
            .store
            .read_evaluation_backlog_item(&scope, &candidate.id)
            .expect("evaluation");
        assert_eq!(evaluation.candidate_id, candidate.id);
        let backlog_after = api
            .store
            .read_capability_evolution_backlog_item(&scope, &candidate.id)
            .expect("backlog after steward");
        assert_eq!(
            backlog_after.status,
            LearningCapabilityEvolutionBacklogStatus::InReview
        );
    }

    #[tokio::test]
    async fn steward_refuses_non_allowlisted_validation_command() {
        let temp_dir = tempfile::TempDir::new().expect("tempdir");
        let workspace = ArtifactV2Workspace::new(temp_dir.path());
        let api = LearningApi::new(workspace, temp_dir.path());
        let scope = LearningScope {
            principal: "anonymous".to_string(),
            workspace: "default".to_string(),
        };
        let candidate = api
            .store
            .create_candidate(
                scope.clone(),
                CreateLearningCandidateRequest {
                    principal: None,
                    workspace: None,
                    candidate_type: LearningCandidateType::SkillUpdate,
                    state: LearningCandidateState::Approved,
                    title: "Improve unsafe validation skill".to_string(),
                    summary: "Approved proposal has unsafe validation command.".to_string(),
                    rationale: "Regression coverage should be gated.".to_string(),
                    proposed_change: json!({}),
                    proposed_target: Some("skill:unsafe-skill".to_string()),
                    confidence: Some(0.95),
                    source_agent_id: None,
                    source_task_id: None,
                    source_execution_id: None,
                    source_chat_session_id: None,
                    event_refs: Vec::new(),
                    evidence_refs: Vec::new(),
                    risk_level: LearningRiskLevel::Low,
                    review_required: false,
                    review_reason: None,
                    review_policy: json!({}),
                    promotion_target: None,
                    promotion_policy: json!({}),
                },
            )
            .expect("candidate");
        let now = Utc::now();
        let backlog = LearningCapabilityEvolutionBacklogItem {
            id: format!("lceb_{}", candidate.id.trim_start_matches("lc_")),
            scope: scope.clone(),
            candidate_id: candidate.id.clone(),
            status: LearningCapabilityEvolutionBacklogStatus::InReview,
            candidate_type: LearningCandidateType::SkillUpdate,
            title: candidate.title.clone(),
            summary: candidate.summary.clone(),
            rationale: candidate.rationale.clone(),
            capability_id: Some("skill:unsafe-skill".to_string()),
            failure_pattern: Some("needs validation".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: vec!["skills/unsafe-skill/SKILL.md".to_string()],
            required_eval: None,
            promotion_gate: Some(json!({"regression_required": false})),
            proposed_target: Some("skill:unsafe-skill".to_string()),
            risk_level: LearningRiskLevel::Low,
            dedupe_fingerprint: Some("unsafe".to_string()),
            recurrence_count: 1,
            blocked_task_count: 0,
            user_pain_signal_count: 0,
            validation_failure_count: 0,
            local_validation_available: true,
            rank_score: 40.0,
            rank_reasons: vec!["low risk".to_string()],
            owner_hints: vec!["autonomous_steward_can_draft_validate".to_string()],
            supersedes_candidate_ids: Vec::new(),
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            evidence_refs: Vec::new(),
            fix_spec: json!({"expected_behavior": "unsafe validation command is blocked"}),
            created_at: now,
            updated_at: now,
        };
        api.store
            .write_capability_evolution_backlog_item(&backlog)
            .expect("backlog");
        let proposal = LearningCapabilityEvolutionProposal {
            id: format!("lcep_{}", candidate.id.trim_start_matches("lc_")),
            scope: scope.clone(),
            candidate_id: candidate.id.clone(),
            backlog_id: backlog.id.clone(),
            status: LearningCapabilityEvolutionProposalStatus::Approved,
            title: "Review fix proposal: unsafe".to_string(),
            summary: "Unsafe validation should not auto-run.".to_string(),
            capability_id: Some("skill:unsafe-skill".to_string()),
            proposed_fix_type: Some("skill_update".to_string()),
            proposed_files: backlog.proposed_files.clone(),
            change_plan: json!({}),
            patches: Vec::new(),
            eval_plan: None,
            validation_plan: Some(json!({
                "commands": ["make docs-remind; rm -rf /"]
            })),
            promotion_gate: Some(json!({"regression_required": false})),
            generated_by: "test".to_string(),
            created_at: now,
            updated_at: now,
        };
        api.store
            .write_capability_evolution_proposal(&proposal)
            .expect("proposal");

        let report = run_capability_evolution_steward_cycle(
            &api,
            scope.clone(),
            RunLearningCapabilityEvolutionStewardRequest {
                workspace: None,
                actor: "test_steward".to_string(),
                backlog_limit: Some(5),
                max_proposals: Some(0),
                max_evaluations: Some(1),
                max_validations: Some(1),
                max_implementations: Some(0),
                max_dry_runs: Some(0),
                max_touched_files: Some(4),
                timeout_seconds: Some(1),
                include_regression: Some(false),
                dry_run: false,
                payload: Value::Null,
            },
        )
        .await;

        assert_eq!(
            report.status,
            LearningCapabilityEvolutionStewardRunStatus::Completed
        );
        assert!(report.actions.iter().any(|action| {
            action.action == "run_validation"
                && action.status
                    == LearningCapabilityEvolutionStewardActionStatus::AttentionRequired
                && action.reason.as_deref() == Some("validation_command_not_allowlisted")
        }));
        let validations = api
            .store
            .list_capability_evolution_validation_reports(
                &scope,
                LearningCapabilityEvolutionValidationFilters {
                    status: None,
                    candidate_id: Some(candidate.id.clone()),
                    capability_id: None,
                    limit: None,
                },
            )
            .expect("validations");
        assert!(validations.is_empty());
    }
}

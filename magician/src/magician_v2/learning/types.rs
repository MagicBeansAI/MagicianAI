use anyhow::{anyhow, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningScope {
    pub principal: String,
    pub workspace: String,
}

impl LearningScope {
    pub fn new(principal: impl Into<String>, workspace: impl Into<String>) -> Self {
        Self {
            principal: principal.into(),
            workspace: workspace.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCandidateType {
    MemoryFact,
    MemoryPreference,
    MemoryProcedure,
    SkillUpdate,
    CapabilityUpdate,
    ToolSchemaUpdate,
    ToolWrapperFix,
    AgentPersonaUpdate,
    WorkflowTemplate,
    EvaluationCase,
    ProgramStateUpdate,
    /// Boundary D: a proposed revision to versioned supplemental harness
    /// guidance. It can never reach the program document, authority, tool
    /// grants, trust policies, schedules or approval rules — see
    /// `harness::supplemental_profile::ALLOWED_SECTIONS`.
    HarnessProfileRevision,
    BugReport,
    DocsUpdate,
    Other,
}

impl LearningCandidateType {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MemoryFact => "memory_fact",
            Self::MemoryPreference => "memory_preference",
            Self::MemoryProcedure => "memory_procedure",
            Self::SkillUpdate => "skill_update",
            Self::CapabilityUpdate => "capability_update",
            Self::ToolSchemaUpdate => "tool_schema_update",
            Self::ToolWrapperFix => "tool_wrapper_fix",
            Self::AgentPersonaUpdate => "agent_persona_update",
            Self::WorkflowTemplate => "workflow_template",
            Self::EvaluationCase => "evaluation_case",
            Self::ProgramStateUpdate => "program_state_update",
            Self::HarnessProfileRevision => "harness_profile_revision",
            Self::BugReport => "bug_report",
            Self::DocsUpdate => "docs_update",
            Self::Other => "other",
        }
    }

    pub fn is_memory_candidate(&self) -> bool {
        matches!(self, Self::MemoryFact | Self::MemoryPreference)
    }

    pub fn is_procedure_candidate(&self) -> bool {
        matches!(self, Self::MemoryProcedure)
    }

    pub fn is_evaluation_candidate(&self) -> bool {
        matches!(self, Self::EvaluationCase)
    }

    pub fn is_capability_evolution_candidate(&self) -> bool {
        matches!(
            self,
            Self::CapabilityUpdate | Self::ToolSchemaUpdate | Self::ToolWrapperFix
        )
    }

    pub fn is_skill_or_workflow_candidate(&self) -> bool {
        matches!(self, Self::SkillUpdate | Self::WorkflowTemplate)
    }

    /// Boundary D's type. Called out separately because it is the only
    /// candidate that proposes a change to how the harness operates, and so
    /// is the only one that must never auto-apply.
    pub fn is_harness_profile_candidate(&self) -> bool {
        matches!(self, Self::HarnessProfileRevision)
    }

    pub fn is_skill_or_capability_evolution_candidate(&self) -> bool {
        self.is_skill_or_workflow_candidate() || self.is_capability_evolution_candidate()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCandidateState {
    Observed,
    Proposed,
    Triaged,
    Approved,
    Implemented,
    Evaluated,
    Promoted,
    Rejected,
    Superseded,
    Archived,
}

impl LearningCandidateState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Proposed => "proposed",
            Self::Triaged => "triaged",
            Self::Approved => "approved",
            Self::Implemented => "implemented",
            Self::Evaluated => "evaluated",
            Self::Promoted => "promoted",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Archived => "archived",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Promoted | Self::Rejected | Self::Superseded | Self::Archived
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningEvaluationBacklogStatus {
    Queued,
    InReview,
    Evaluated,
    Rejected,
    Archived,
}

impl LearningEvaluationBacklogStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InReview => "in_review",
            Self::Evaluated => "evaluated",
            Self::Rejected => "rejected",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningEvaluationRunStatus {
    Passed,
    Failed,
    Blocked,
}

impl LearningEvaluationRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionBacklogStatus {
    Queued,
    InReview,
    Validated,
    Implemented,
    Rejected,
    Superseded,
    Archived,
}

impl LearningCapabilityEvolutionBacklogStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::InReview => "in_review",
            Self::Validated => "validated",
            Self::Implemented => "implemented",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Archived => "archived",
        }
    }

    pub fn is_open_for_dedupe(&self) -> bool {
        matches!(self, Self::Queued | Self::InReview | Self::Validated)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionProposalStatus {
    Draft,
    ReadyForReview,
    Approved,
    Rejected,
    Superseded,
    Archived,
}

impl LearningCapabilityEvolutionProposalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::ReadyForReview => "ready_for_review",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Superseded => "superseded",
            Self::Archived => "archived",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionValidationStatus {
    Passed,
    Failed,
    Blocked,
}

impl LearningCapabilityEvolutionValidationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningRiskLevel {
    Low,
    Medium,
    High,
    Critical,
}

impl LearningRiskLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningEvidenceRef {
    pub kind: String,
    pub id: Option<String>,
    pub path: Option<String>,
    pub uri: Option<String>,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningEventRef {
    pub event_id: String,
    pub event_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningEvent {
    pub id: String,
    pub scope: LearningScope,
    pub event_type: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub summary: String,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningSkillInvocationSource {
    CompiledPack,
    CompiledProvider,
    PrimitiveCompiledProvider,
    PrimitiveCliTemplate,
    GovernedRuntime,
    NativeTool,
    BrowserTool,
    RuntimeTool,
    Unknown,
}

impl LearningSkillInvocationSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::CompiledPack => "compiled_pack",
            Self::CompiledProvider => "compiled_provider",
            Self::PrimitiveCompiledProvider => "primitive_compiled_provider",
            Self::PrimitiveCliTemplate => "primitive_cli_template",
            Self::GovernedRuntime => "governed_runtime",
            Self::NativeTool => "native_tool",
            Self::BrowserTool => "browser_tool",
            Self::RuntimeTool => "runtime_tool",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningSkillInvocationStatus {
    Succeeded,
    Failed,
    Blocked,
    Cancelled,
}

impl LearningSkillInvocationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningSkillInvocationFailureClass {
    MissingEnvConfig,
    BadSchema,
    WrapperCrash,
    ParseFailure,
    AuthFailure,
    NetworkServiceFailure,
    Timeout,
    ResourceAuthorityDenied,
    UserDeniedOrHitlBlocked,
    ToolMisuse,
    CapabilityUnavailable,
    Cancelled,
    Unknown,
}

impl LearningSkillInvocationFailureClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::MissingEnvConfig => "missing_env_config",
            Self::BadSchema => "bad_schema",
            Self::WrapperCrash => "wrapper_crash",
            Self::ParseFailure => "parse_failure",
            Self::AuthFailure => "auth_failure",
            Self::NetworkServiceFailure => "network_service_failure",
            Self::Timeout => "timeout",
            Self::ResourceAuthorityDenied => "resource_authority_denied",
            Self::UserDeniedOrHitlBlocked => "user_denied_or_hitl_blocked",
            Self::ToolMisuse => "tool_misuse",
            Self::CapabilityUnavailable => "capability_unavailable",
            Self::Cancelled => "cancelled",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningSkillInvocationEvidence {
    pub id: String,
    pub scope: LearningScope,
    pub source: LearningSkillInvocationSource,
    pub skill_name: String,
    pub tool_action_name: Option<String>,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub input_fingerprint: String,
    pub input_shape: Value,
    pub status: LearningSkillInvocationStatus,
    pub failure_class: Option<LearningSkillInvocationFailureClass>,
    pub error_summary: Option<String>,
    pub result_summary: Option<String>,
    pub duration_ms: u64,
    pub retry_count: u32,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningSkillInvocationFailureCluster {
    pub id: String,
    pub scope: LearningScope,
    pub cluster_key: String,
    pub source: LearningSkillInvocationSource,
    pub skill_name: String,
    pub tool_action_name: Option<String>,
    pub failure_class: LearningSkillInvocationFailureClass,
    pub input_fingerprint: String,
    pub occurrence_count: u64,
    pub first_seen_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub last_error_summary: Option<String>,
    pub last_result_summary: Option<String>,
    pub candidate_id: Option<String>,
    pub routed_at: Option<DateTime<Utc>>,
    pub route_reason: Option<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCandidate {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_type: LearningCandidateType,
    pub state: LearningCandidateState,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub proposed_change: Value,
    pub proposed_target: Option<String>,
    pub confidence: Option<f64>,
    pub source_agent_id: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_chat_session_id: Option<String>,
    pub event_refs: Vec<LearningEventRef>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub risk_level: LearningRiskLevel,
    pub review_required: bool,
    pub review_reason: Option<String>,
    pub review_policy: Value,
    pub promotion_target: Option<String>,
    pub promotion_policy: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningDecisionLogEntry {
    pub id: String,
    pub candidate_id: String,
    pub scope: LearningScope,
    pub actor: String,
    pub from_state: Option<LearningCandidateState>,
    pub to_state: LearningCandidateState,
    pub decision: String,
    pub reason: String,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningEvaluationBacklogItem {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub status: LearningEvaluationBacklogStatus,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub case_kind: String,
    pub priority: String,
    pub target_agent_id: Option<String>,
    pub focus_area: Option<String>,
    pub proposed_target: Option<String>,
    pub source_agent_id: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_chat_session_id: Option<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub case_spec: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningEvaluationRunReport {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub backlog_id: String,
    pub status: LearningEvaluationRunStatus,
    pub runner: String,
    pub summary: String,
    pub commands: Vec<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub metrics: Value,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningGrowthEvaluationRunReport {
    pub id: String,
    pub scope: LearningScope,
    pub suite_id: String,
    pub status: LearningEvaluationRunStatus,
    pub summary: String,
    pub dimensions: Vec<LearningGrowthEvaluationDimensionReport>,
    pub scenarios: Vec<LearningGrowthEvaluationScenarioReport>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub metrics: Value,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningGrowthEvaluationDimensionReport {
    pub dimension: String,
    pub status: LearningEvaluationRunStatus,
    pub score: f64,
    pub summary: String,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub metrics: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningGrowthEvaluationScenarioReport {
    pub scenario: String,
    pub status: LearningEvaluationRunStatus,
    pub summary: String,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub metrics: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunLearningGrowthEvaluationRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub suite_id: Option<String>,
    pub window_days: Option<i64>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionBacklogItem {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub status: LearningCapabilityEvolutionBacklogStatus,
    pub candidate_type: LearningCandidateType,
    pub title: String,
    pub summary: String,
    pub rationale: String,
    pub capability_id: Option<String>,
    pub failure_pattern: Option<String>,
    pub proposed_fix_type: Option<String>,
    pub proposed_files: Vec<String>,
    pub required_eval: Option<Value>,
    pub promotion_gate: Option<Value>,
    pub proposed_target: Option<String>,
    pub risk_level: LearningRiskLevel,
    #[serde(default)]
    pub dedupe_fingerprint: Option<String>,
    #[serde(default)]
    pub recurrence_count: u64,
    #[serde(default)]
    pub blocked_task_count: u64,
    #[serde(default)]
    pub user_pain_signal_count: u64,
    #[serde(default)]
    pub validation_failure_count: u64,
    #[serde(default)]
    pub local_validation_available: bool,
    #[serde(default)]
    pub rank_score: f64,
    #[serde(default)]
    pub rank_reasons: Vec<String>,
    #[serde(default)]
    pub owner_hints: Vec<String>,
    #[serde(default)]
    pub supersedes_candidate_ids: Vec<String>,
    pub source_agent_id: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_chat_session_id: Option<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub fix_spec: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionProposalPatch {
    pub path: String,
    pub operation: String,
    pub summary: String,
    #[serde(default)]
    pub diff: Option<String>,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionProposal {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub backlog_id: String,
    pub status: LearningCapabilityEvolutionProposalStatus,
    pub title: String,
    pub summary: String,
    pub capability_id: Option<String>,
    pub proposed_fix_type: Option<String>,
    pub proposed_files: Vec<String>,
    pub change_plan: Value,
    pub patches: Vec<LearningCapabilityEvolutionProposalPatch>,
    pub eval_plan: Option<Value>,
    pub validation_plan: Option<Value>,
    pub promotion_gate: Option<Value>,
    pub generated_by: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionValidationReport {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub proposal_id: String,
    pub status: LearningCapabilityEvolutionValidationStatus,
    pub capability_id: Option<String>,
    pub runner: String,
    pub summary: String,
    pub commands: Vec<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub metrics: Value,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionImplementationRecord {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub proposal_id: String,
    pub validation_id: String,
    pub capability_id: Option<String>,
    pub actor: String,
    pub summary: String,
    pub applied_files: Vec<String>,
    pub patches: Vec<LearningCapabilityEvolutionProposalPatch>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionApplicationMode {
    DryRun,
    Apply,
}

impl LearningCapabilityEvolutionApplicationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::DryRun => "dry_run",
            Self::Apply => "apply",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionApplicationStatus {
    Prepared,
    Applied,
}

impl LearningCapabilityEvolutionApplicationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Prepared => "prepared",
            Self::Applied => "applied",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionAppliedFile {
    pub path: String,
    pub operation: String,
    pub previous_exists: bool,
    #[serde(default)]
    pub previous_content: Option<String>,
    #[serde(default)]
    pub new_content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionApplicationRecord {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub proposal_id: String,
    pub validation_id: String,
    pub implementation_id: String,
    pub capability_id: Option<String>,
    pub actor: String,
    pub summary: String,
    pub mode: LearningCapabilityEvolutionApplicationMode,
    pub status: LearningCapabilityEvolutionApplicationStatus,
    pub changed_files: Vec<LearningCapabilityEvolutionAppliedFile>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionPromotionRecord {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub proposal_id: String,
    pub validation_id: String,
    #[serde(default)]
    pub implementation_id: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    pub capability_id: Option<String>,
    pub actor: String,
    pub summary: String,
    pub applied_files: Vec<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionPostPromotionMonitorStatus {
    Pending,
    NoUsage,
    InsufficientEvidence,
    Stable,
    RegressionDetected,
}

impl LearningCapabilityEvolutionPostPromotionMonitorStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::NoUsage => "no_usage",
            Self::InsufficientEvidence => "insufficient_evidence",
            Self::Stable => "stable",
            Self::RegressionDetected => "regression_detected",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionPostPromotionMonitorRecord {
    pub id: String,
    pub scope: LearningScope,
    pub promotion_id: String,
    pub candidate_id: String,
    pub proposal_id: String,
    pub validation_id: String,
    #[serde(default)]
    pub implementation_id: Option<String>,
    #[serde(default)]
    pub application_id: Option<String>,
    pub capability_id: Option<String>,
    pub status: LearningCapabilityEvolutionPostPromotionMonitorStatus,
    pub summary: String,
    pub skill_names: Vec<String>,
    pub before_invocation_count: u64,
    pub after_invocation_count: u64,
    pub before_success_count: u64,
    pub after_success_count: u64,
    pub before_failure_count: u64,
    pub after_failure_count: u64,
    pub before_success_rate: Option<f64>,
    pub after_success_rate: Option<f64>,
    pub same_failure_recurrence_count: u64,
    pub new_failure_classes: Vec<String>,
    pub user_negative_feedback_count: u64,
    pub rollback_recommendation_id: Option<String>,
    pub follow_up_candidate_id: Option<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionRollbackRecommendationStatus {
    Recommended,
    Dismissed,
    Superseded,
}

impl LearningCapabilityEvolutionRollbackRecommendationStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Recommended => "recommended",
            Self::Dismissed => "dismissed",
            Self::Superseded => "superseded",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionRollbackRecommendationRecord {
    pub id: String,
    pub scope: LearningScope,
    pub candidate_id: String,
    pub proposal_id: String,
    #[serde(default)]
    pub validation_id: Option<String>,
    #[serde(default)]
    pub implementation_id: Option<String>,
    pub application_id: String,
    #[serde(default)]
    pub promotion_id: Option<String>,
    pub capability_id: Option<String>,
    pub status: LearningCapabilityEvolutionRollbackRecommendationStatus,
    pub trigger_kind: String,
    pub severity: String,
    pub actor: String,
    pub summary: String,
    pub rollback_files: Vec<String>,
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub payload: Value,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionStewardRunStatus {
    Completed,
    Failed,
}

impl LearningCapabilityEvolutionStewardRunStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningCapabilityEvolutionStewardActionStatus {
    Completed,
    Skipped,
    Failed,
    AttentionRequired,
}

impl LearningCapabilityEvolutionStewardActionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Skipped => "skipped",
            Self::Failed => "failed",
            Self::AttentionRequired => "attention_required",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionStewardAction {
    pub action: String,
    pub status: LearningCapabilityEvolutionStewardActionStatus,
    pub candidate_id: Option<String>,
    pub backlog_id: Option<String>,
    pub proposal_id: Option<String>,
    pub evaluation_backlog_id: Option<String>,
    pub validation_id: Option<String>,
    pub implementation_id: Option<String>,
    pub application_id: Option<String>,
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningCapabilityEvolutionStewardRunReport {
    pub id: String,
    pub scope: LearningScope,
    pub status: LearningCapabilityEvolutionStewardRunStatus,
    pub actor: String,
    pub summary: String,
    pub budgets: Value,
    pub metrics: Value,
    pub actions: Vec<LearningCapabilityEvolutionStewardAction>,
    #[serde(default)]
    pub payload: Value,
    pub started_at: DateTime<Utc>,
    pub completed_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LearningProcedureStatus {
    Draft,
    Active,
    Deprecated,
    Archived,
}

impl LearningProcedureStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Archived => "archived",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Draft, Self::Active, Self::Deprecated, Self::Archived]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct LearningProcedureActivation {
    #[serde(default)]
    pub use_when: Vec<String>,
    #[serde(default)]
    pub avoid_when: Vec<String>,
    #[serde(default)]
    pub example_goals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningProcedure {
    pub id: String,
    pub scope: LearningScope,
    pub status: LearningProcedureStatus,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub owner_agent: Option<String>,
    #[serde(default)]
    pub activation: LearningProcedureActivation,
    #[serde(default)]
    pub workflow: Vec<String>,
    #[serde(default)]
    pub decision_points: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub failure_modes: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub source_candidate_id: Option<String>,
    #[serde(default)]
    pub source_task_ids: Vec<String>,
    #[serde(default)]
    pub source_chat_session_ids: Vec<String>,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    #[serde(default)]
    pub payload: Value,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub version: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LearningProcedureDecisionLogEntry {
    pub id: String,
    pub procedure_id: String,
    pub scope: LearningScope,
    pub actor: String,
    pub from_status: Option<LearningProcedureStatus>,
    pub to_status: LearningProcedureStatus,
    pub decision: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLearningEventRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub event_type: String,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub summary: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLearningSkillInvocationEvidenceRequest {
    pub source: LearningSkillInvocationSource,
    pub skill_name: String,
    pub tool_action_name: Option<String>,
    pub agent_id: Option<String>,
    pub task_id: Option<String>,
    pub execution_id: Option<String>,
    pub chat_session_id: Option<String>,
    pub input_fingerprint: String,
    #[serde(default)]
    pub input_shape: Value,
    pub status: LearningSkillInvocationStatus,
    pub failure_class: Option<LearningSkillInvocationFailureClass>,
    pub error_summary: Option<String>,
    pub result_summary: Option<String>,
    #[serde(default)]
    pub duration_ms: u64,
    #[serde(default)]
    pub retry_count: u32,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLearningCandidateRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub candidate_type: LearningCandidateType,
    #[serde(default = "default_candidate_state")]
    pub state: LearningCandidateState,
    pub title: String,
    pub summary: String,
    #[serde(default)]
    pub rationale: String,
    #[serde(default)]
    pub proposed_change: Value,
    pub proposed_target: Option<String>,
    pub confidence: Option<f64>,
    pub source_agent_id: Option<String>,
    pub source_task_id: Option<String>,
    pub source_execution_id: Option<String>,
    pub source_chat_session_id: Option<String>,
    #[serde(default)]
    pub event_refs: Vec<LearningEventRef>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    #[serde(default = "default_risk_level")]
    pub risk_level: LearningRiskLevel,
    #[serde(default)]
    pub review_required: bool,
    pub review_reason: Option<String>,
    #[serde(default)]
    pub review_policy: Value,
    pub promotion_target: Option<String>,
    #[serde(default)]
    pub promotion_policy: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionLearningCandidateRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub to_state: LearningCandidateState,
    #[serde(default = "default_actor")]
    pub actor: String,
    pub decision: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateLearningProcedureRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub id: Option<String>,
    #[serde(default = "default_actor")]
    pub actor: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default = "default_procedure_status")]
    pub status: LearningProcedureStatus,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    pub owner_agent: Option<String>,
    #[serde(default)]
    pub activation: LearningProcedureActivation,
    #[serde(default)]
    pub workflow: Vec<String>,
    #[serde(default)]
    pub decision_points: Vec<String>,
    #[serde(default)]
    pub verification: Vec<String>,
    #[serde(default)]
    pub failure_modes: Vec<String>,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
    pub source_candidate_id: Option<String>,
    #[serde(default)]
    pub source_task_ids: Vec<String>,
    #[serde(default)]
    pub source_chat_session_ids: Vec<String>,
    #[serde(default)]
    pub success_count: u64,
    #[serde(default)]
    pub failure_count: u64,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TransitionLearningProcedureRequest {
    pub principal: Option<String>,
    pub workspace: Option<String>,
    pub to_status: LearningProcedureStatus,
    #[serde(default = "default_actor")]
    pub actor: String,
    #[serde(default)]
    pub decision: String,
    pub reason: String,
    #[serde(default)]
    pub evidence_refs: Vec<LearningEvidenceRef>,
}

impl CreateLearningEventRequest {
    pub fn into_event(self, scope: LearningScope) -> LearningEvent {
        LearningEvent {
            id: format!("le_{}", Uuid::new_v4().simple()),
            scope,
            event_type: self.event_type,
            agent_id: self.agent_id,
            task_id: self.task_id,
            execution_id: self.execution_id,
            chat_session_id: self.chat_session_id,
            summary: self.summary,
            evidence_refs: self.evidence_refs,
            payload: self.payload,
            created_at: Utc::now(),
        }
    }
}

impl CreateLearningSkillInvocationEvidenceRequest {
    pub fn into_skill_invocation_evidence(
        self,
        scope: LearningScope,
    ) -> LearningSkillInvocationEvidence {
        LearningSkillInvocationEvidence {
            id: format!("lsi_{}", Uuid::new_v4().simple()),
            scope,
            source: self.source,
            skill_name: self.skill_name,
            tool_action_name: self.tool_action_name,
            agent_id: self.agent_id,
            task_id: self.task_id,
            execution_id: self.execution_id,
            chat_session_id: self.chat_session_id,
            input_fingerprint: self.input_fingerprint,
            input_shape: self.input_shape,
            status: self.status,
            failure_class: self.failure_class,
            error_summary: self.error_summary,
            result_summary: self.result_summary,
            duration_ms: self.duration_ms,
            retry_count: self.retry_count,
            evidence_refs: self.evidence_refs,
            payload: self.payload,
            created_at: Utc::now(),
        }
    }
}

impl CreateLearningCandidateRequest {
    /// Build the candidate the request describes, filed under `scope`.
    ///
    /// `scope` is the sole authority for where the candidate lands. The
    /// request's `principal`/`workspace` fields predate it and stay on the wire
    /// for compatibility, but a mirror that disagrees with `scope` means the
    /// caller and the scoped path resolved different principals or workspaces —
    /// an ambiguity about where data would be written — so this fails closed
    /// rather than silently preferring either. Absent or agreeing mirrors are
    /// accepted.
    pub fn into_candidate(self, scope: LearningScope) -> Result<LearningCandidate> {
        if let Some(principal) = self.principal.as_deref() {
            if principal != scope.principal {
                return Err(anyhow!(
                    "request principal `{principal}` disagrees with scoped principal `{}`",
                    scope.principal
                ));
            }
        }
        if let Some(workspace) = self.workspace.as_deref() {
            if workspace != scope.workspace {
                return Err(anyhow!(
                    "request workspace `{workspace}` disagrees with scoped workspace `{}`",
                    scope.workspace
                ));
            }
        }
        let now = Utc::now();
        Ok(LearningCandidate {
            id: format!("lc_{}", Uuid::new_v4().simple()),
            scope,
            candidate_type: self.candidate_type,
            state: self.state,
            title: self.title,
            summary: self.summary,
            rationale: self.rationale,
            proposed_change: self.proposed_change,
            proposed_target: self.proposed_target,
            confidence: self
                .confidence
                .filter(|value| value.is_finite())
                .map(|value| value.clamp(0.0, 1.0)),
            source_agent_id: self.source_agent_id,
            source_task_id: self.source_task_id,
            source_execution_id: self.source_execution_id,
            source_chat_session_id: self.source_chat_session_id,
            event_refs: self.event_refs,
            evidence_refs: self.evidence_refs,
            risk_level: self.risk_level,
            review_required: self.review_required,
            review_reason: self.review_reason,
            review_policy: self.review_policy,
            promotion_target: self.promotion_target,
            promotion_policy: self.promotion_policy,
            created_at: now,
            updated_at: now,
        })
    }
}

impl CreateLearningProcedureRequest {
    pub fn into_procedure(self, scope: LearningScope) -> LearningProcedure {
        let now = Utc::now();
        LearningProcedure {
            id: self
                .id
                .map(|id| id.trim().to_string())
                .filter(|id| !id.is_empty())
                .unwrap_or_else(|| format!("proc_{}", Uuid::new_v4().simple())),
            scope,
            status: self.status,
            title: self.title,
            summary: self.summary,
            owner_agent: self.owner_agent,
            activation: self.activation,
            workflow: self.workflow,
            decision_points: self.decision_points,
            verification: self.verification,
            failure_modes: self.failure_modes,
            evidence_refs: self.evidence_refs,
            source_candidate_id: self.source_candidate_id,
            source_task_ids: self.source_task_ids,
            source_chat_session_ids: self.source_chat_session_ids,
            success_count: self.success_count,
            failure_count: self.failure_count,
            payload: self.payload,
            created_at: now,
            updated_at: now,
            last_used_at: None,
            version: 1,
        }
    }
}

fn default_candidate_state() -> LearningCandidateState {
    LearningCandidateState::Observed
}

fn default_procedure_status() -> LearningProcedureStatus {
    LearningProcedureStatus::Draft
}

fn default_risk_level() -> LearningRiskLevel {
    LearningRiskLevel::Medium
}

fn default_actor() -> String {
    "system".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate_request(
        principal: Option<String>,
        workspace: Option<String>,
    ) -> CreateLearningCandidateRequest {
        CreateLearningCandidateRequest {
            principal,
            workspace,
            candidate_type: LearningCandidateType::SkillUpdate,
            state: default_candidate_state(),
            title: "candidate".to_string(),
            summary: "scope mirror checks".to_string(),
            rationale: String::new(),
            proposed_change: Value::Null,
            proposed_target: None,
            confidence: None,
            source_agent_id: None,
            source_task_id: None,
            source_execution_id: None,
            source_chat_session_id: None,
            event_refs: Vec::new(),
            evidence_refs: Vec::new(),
            risk_level: default_risk_level(),
            review_required: false,
            review_reason: None,
            review_policy: Value::Null,
            promotion_target: None,
            promotion_policy: Value::Null,
        }
    }

    /// Scope mirrors that agree with `scope` — and mirrors left unset by
    /// callers that never sent them — are both accepted.
    #[test]
    fn candidate_scope_mirrors_that_agree_or_are_absent_are_accepted() {
        let agreeing = candidate_request(Some("owner".into()), Some("ws".into()))
            .into_candidate(LearningScope::new("owner", "ws"))
            .unwrap();
        assert_eq!(agreeing.scope.principal, "owner");
        assert_eq!(agreeing.scope.workspace, "ws");

        let absent = candidate_request(None, None)
            .into_candidate(LearningScope::new("owner", "ws"))
            .unwrap();
        assert_eq!(absent.scope.principal, "owner");
        assert_eq!(absent.scope.workspace, "ws");
    }

    /// A mirror disagreeing with `scope` is an ambiguity about where the
    /// candidate would land, so it fails closed and names the mismatch.
    #[test]
    fn a_candidate_scope_mirror_that_disagrees_with_scope_fails_closed() {
        let principal_error = candidate_request(Some("other".into()), Some("ws".into()))
            .into_candidate(LearningScope::new("owner", "ws"))
            .unwrap_err();
        assert!(principal_error.to_string().contains("principal"));
        assert!(principal_error.to_string().contains("other"));

        let workspace_error = candidate_request(Some("owner".into()), Some("elsewhere".into()))
            .into_candidate(LearningScope::new("owner", "ws"))
            .unwrap_err();
        assert!(workspace_error.to_string().contains("workspace"));
        assert!(workspace_error.to_string().contains("elsewhere"));
    }
}

//! Scoped learning substrate for agent-growth loops.
//!
//! This module is intentionally storage-first. Learning candidates keep their
//! provenance and decision history; narrow bridges may promote explicit
//! low-risk memory and program-state candidates while review-gating broader
//! behavior changes.

mod audit;
mod capability_bridge;
mod eval_bridge;
mod evidence_memory_bridge;
mod growth_eval;
mod harness_profile_bridge;
mod memory_bridge;
mod procedure_bridge;
mod procedure_feedback;
mod procedure_index;
mod procedure_prompt_blocks;
mod procedure_skill_promotion;
mod program_state_bridge;
mod reflection;
mod skill_invocation;
mod store;
mod teaching;
mod types;
mod work_ledger_program_state;

pub use audit::{build_learning_gap_audit, LearningAuditCounts, LearningGapAuditReport};
pub use capability_bridge::{
    log_capability_route_error, LearningCapabilityEvolutionBridge,
    LearningCapabilityEvolutionRouteOutcome,
};
pub use eval_bridge::{log_eval_route_error, LearningEvalBridge, LearningEvalRouteOutcome};
pub use evidence_memory_bridge::route_user_evidence_to_memory;
pub use growth_eval::run_learning_growth_evaluation;
pub use harness_profile_bridge::{
    log_harness_profile_route_error, HarnessProfileEvaluationRecord,
    LearningHarnessProfileApplyOutcome, LearningHarnessProfileBridge,
    LearningHarnessProfileRouteOutcome, HARNESS_PROFILE_CANDIDATE_INVALID_EVENT,
    HARNESS_PROFILE_CANDIDATE_STAGED_EVENT, HARNESS_PROFILE_EVALUATION_RECORDED_EVENT,
    HARNESS_PROFILE_REVISION_APPLIED_EVENT,
};
pub use memory_bridge::{log_memory_route_error, LearningMemoryBridge, LearningMemoryRouteOutcome};
pub use procedure_bridge::{LearningProcedureBridge, LearningProcedureRouteOutcome};
pub use procedure_feedback::{
    LearningProcedureFeedbackBridge, LearningProcedureFeedbackOutcome,
    LearningProcedureRunFeedbackJudgement, LearningProcedureRunFeedbackVerdict,
    LearningProcedureUsageContext, LearningProcedureUsageRecord,
};
pub use procedure_index::start_procedure_index_maintainer;
pub use procedure_prompt_blocks::{
    render_active_procedures_for_prompt, render_active_procedures_for_prompt_with_hybrid,
    LearningProcedurePromptRenderResult, LearningProcedurePromptRetrievalBackend,
    LearningProcedurePromptSelection, LearningProcedureRenderRequest,
};
pub use procedure_skill_promotion::{
    LearningProcedureSkillPromotionBridge, LearningProcedureSkillPromotionOutcome,
    PromoteLearningProcedureToSkillRequest,
};
pub use program_state_bridge::{
    log_program_state_route_error, LearningProgramStateBridge, LearningProgramStateRevertOutcome,
    LearningProgramStateRouteOutcome, HARNESS_PROGRAM_STATE_AUTO_APPLIED_EVENT,
    HARNESS_PROGRAM_STATE_REVERTED_EVENT,
};
pub use reflection::{
    spawn_learning_reflection, LearningReflectionInput, LearningReflectionRun,
    LearningReflectionRuntime,
};
pub use skill_invocation::{
    classify_skill_invocation_failure, fingerprint_input_shape, redacted_input_shape,
};
pub use store::{
    LearningCandidateFilters, LearningCapabilityEvolutionApplicationFilters,
    LearningCapabilityEvolutionBacklogFilters, LearningCapabilityEvolutionImplementationFilters,
    LearningCapabilityEvolutionPostPromotionMonitorFilters,
    LearningCapabilityEvolutionPromotionFilters, LearningCapabilityEvolutionProposalFilters,
    LearningCapabilityEvolutionRollbackRecommendationFilters,
    LearningCapabilityEvolutionStewardRunFilters, LearningCapabilityEvolutionValidationFilters,
    LearningEvaluationBacklogFilters, LearningEvaluationRunFilters,
    LearningGrowthEvaluationRunFilters, LearningProcedureFilters,
    LearningSkillInvocationEvidenceFilters, LearningStore,
};
pub use teaching::{
    record_teaching_feedback, CreateLearningTeachingFeedbackRequest, LearningTeachingAction,
    LearningTeachingFeedbackResponse, LearningTeachingRouteOutcome, LearningTeachingTarget,
};
pub use types::{
    CreateLearningCandidateRequest, CreateLearningEventRequest, CreateLearningProcedureRequest,
    CreateLearningSkillInvocationEvidenceRequest, LearningCandidate, LearningCandidateState,
    LearningCandidateType, LearningCapabilityEvolutionApplicationMode,
    LearningCapabilityEvolutionApplicationRecord, LearningCapabilityEvolutionApplicationStatus,
    LearningCapabilityEvolutionAppliedFile, LearningCapabilityEvolutionBacklogItem,
    LearningCapabilityEvolutionBacklogStatus, LearningCapabilityEvolutionImplementationRecord,
    LearningCapabilityEvolutionPostPromotionMonitorRecord,
    LearningCapabilityEvolutionPostPromotionMonitorStatus,
    LearningCapabilityEvolutionPromotionRecord, LearningCapabilityEvolutionProposal,
    LearningCapabilityEvolutionProposalPatch, LearningCapabilityEvolutionProposalStatus,
    LearningCapabilityEvolutionRollbackRecommendationRecord,
    LearningCapabilityEvolutionRollbackRecommendationStatus,
    LearningCapabilityEvolutionStewardAction, LearningCapabilityEvolutionStewardActionStatus,
    LearningCapabilityEvolutionStewardRunReport, LearningCapabilityEvolutionStewardRunStatus,
    LearningCapabilityEvolutionValidationReport, LearningCapabilityEvolutionValidationStatus,
    LearningDecisionLogEntry, LearningEvaluationBacklogItem, LearningEvaluationBacklogStatus,
    LearningEvaluationRunReport, LearningEvaluationRunStatus, LearningEvent, LearningEventRef,
    LearningEvidenceRef, LearningGrowthEvaluationDimensionReport,
    LearningGrowthEvaluationRunReport, LearningGrowthEvaluationScenarioReport, LearningProcedure,
    LearningProcedureActivation, LearningProcedureDecisionLogEntry, LearningProcedureStatus,
    LearningRiskLevel, LearningScope, LearningSkillInvocationEvidence,
    LearningSkillInvocationFailureClass, LearningSkillInvocationFailureCluster,
    LearningSkillInvocationSource, LearningSkillInvocationStatus,
    RunLearningGrowthEvaluationRequest, TransitionLearningCandidateRequest,
    TransitionLearningProcedureRequest,
};
pub use work_ledger_program_state::{
    distill_work_unit_to_program_state, WorkUnitProgramStateOutcome,
};

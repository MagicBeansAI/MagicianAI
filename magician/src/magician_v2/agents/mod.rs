//! # Agents Module
//!
//! TRUE_AGENTS Phase 3 provides:
//! - Core identity + routing types
//! - Full declarative agent definition contracts
//! - Memory tier and consolidation contracts
//! - File-based agent memory + storage services
//! - Lifecycle event helpers
//! - Runtime interpreters: prompt pipeline, circuit breaker, evaluation,
//!   feedback loops, trust policy, approval gate/service,
//!   agent scheduler, memory tier rendering

pub mod agent_update;
pub mod agent_update_emitter;
pub mod agent_update_journal;
pub mod app_memory_ingress;
pub mod approval;
pub mod approval_service;
pub mod approval_store;
pub mod autonomous_goal;
pub mod circuit_breaker;
pub mod condition;
pub mod consequence_class;
pub mod definition_store;
pub mod evaluation;
pub mod events;
pub mod feed_stall_watchdog;
pub mod feedback;
pub mod memory;
pub mod memory_consolidator;
pub mod memory_defaults;
pub mod memory_lifecycle;
pub mod memory_prompt_blocks;
pub mod memory_prompt_snapshot;
pub mod memory_provenance;
pub mod outward_actions;
pub mod outward_receipts;
pub(crate) mod personal_agent_retrieval;
#[cfg(test)]
mod presentation_maker_evals;
pub mod task_ownership;
// The refusals that stand in front of an outward act: may we contact this
// person at all, does the work narrow to this capability, and did it dispatch.
pub mod memory_scope;
pub mod memory_tier_interpreter;
pub mod memory_utility_reviewer;
pub mod outward_gate;
pub mod project_knowledge;
pub mod prompt_pipeline;
pub mod proposals;
pub mod runtime;
pub mod scheduler;
pub mod state_machine;
pub mod storage;
pub mod trust;
pub mod types;
pub mod wake_up_queue;

pub use types::tool_name_matches_block_entry;
pub use types::unknown_llm_routing_references;

/// Facade modules that re-export items moved to `magician-vector-index`.
///
/// The LanceDB/Arrow-heavy index and candidate code now lives in the
/// `magician-vector-index` crate so it can be compiled independently of
/// magician. These thin modules keep existing
/// `crate::magician_v2::agents::memory_{tiers,candidates,index}::Foo`
/// import paths working without touching every caller.
pub mod memory_tiers {
    pub use magician_vector_index::memory_tiers::*;
}

pub mod memory_candidates {
    pub use magician_vector_index::memory_candidates::*;
}

pub mod memory_hot_projections {
    pub use magician_vector_index::memory_hot_projections::*;
}

pub mod memory_index {
    pub use magician_vector_index::memory_index::*;
}

pub mod memory_temperature {
    pub use magician_vector_index::memory_temperature::*;
}

/// Tier-*effectiveness* metrics, as distinct from retrieval metrics: is the
/// active tier earned, is the working set bounded, does the tier predict use.
pub mod memory_tier_health {
    pub use magician_vector_index::memory_tier_health::*;
}

/// §5A.2 engagement containment for retrieval. Re-exported here so memory
/// callers reach the rule from the same path they reach candidates from, and
/// nobody has to know it physically lives beside the index.
pub mod retrieval_scope {
    pub use magician_vector_index::retrieval_scope::*;
}

pub use agent_update::{
    AgentUpdate, AgentUpdateKind, ApprovalId, ArtifactId, ArtifactRef, CycleOutcome, TaskId,
    ThreadId, WorkspaceId,
};
pub use app_memory_ingress::{
    AppMemoryContributionStateItemV1, AppMemoryContributionStateReasonV1,
    AppMemoryContributionStateSnapshotV1, AppMemoryContributionStateV1,
    AppMemoryDestinationApplyResult, AppMemoryDestinationProjectionV1,
    AppMemoryInvalidationHighWaterV1, AppMemoryProjectionEntryV1, AppMemoryProjectionStateV1,
    APP_MEMORY_CONTRIBUTION_STATE_MAX_BYTES, APP_MEMORY_CONTRIBUTION_STATE_MAX_ITEMS,
};
pub use approval::{
    ApprovalCheck, ApprovalGate, ApprovalResult, ExecutionPlan, ExecutionStep, PendingApproval,
    StandingConsent, WaivedApproval,
};
pub use approval_service::{
    ApprovalDecision, ApprovalResolveOutcome, ApprovalService, ApprovalServiceError, FanOutResult,
};
pub use approval_store::{
    ApprovalDelivery, ApprovalRequest, ApprovalStatus, ApprovalStore, ApprovalStoreError,
    DeliveryStatus,
};
pub use autonomous_goal::{
    build_autonomous_goal, build_autonomous_overrides, build_autonomous_trust_context,
    derive_allowed_action_types, focus_area_goal_id, resolve_focus_area_for_goal_id,
    resolve_focus_area_goal_description, resolve_focus_area_task_title,
};
pub use circuit_breaker::{CircuitBreakerInterpreter, CircuitDecision};
pub use consequence_class::{
    consequence_class_for, consequence_class_for_approval_rule, consequence_class_for_dispatch,
    ConsequenceClass,
};
pub use definition_store::{AgentDefinitionStore, DefinitionRecord, DefinitionStoreError};
pub use evaluation::{EvaluationInput, EvaluationInterpreter, EvaluationResult};
pub use events::{emit_agent_cycle_completed, emit_agent_cycle_started, emit_agent_triggered};
pub use feedback::{
    FeedbackLoopInterpreter, FeedbackSignal, FeedbackTransformerRegistry, RecordStrategyTransformer,
};
pub use memory::{
    AgentContextProfile, AgentMemoryError, AgentMemoryResolver, AgentMemoryService, Correction,
    CorrectionCategory, EpisodeOutcome, EpisodeRecallTool, UserProfile,
};
pub use memory_candidates::{
    candidate_engagement_label, item_memory_key, load_memory_candidate_documents,
    retain_candidates_for_scope, MemoryCandidateDocument, MemoryCandidateRequest,
    SemanticMemoryType,
};
pub use memory_consolidator::is_memory_file_lock_timeout;
pub use memory_consolidator::{
    ConsolidationOutcome, ConsolidationReport, EpisodeProjectResolver, MemoryClarificationRuntime,
    MemoryConsolidator, MemoryConsolidatorError, MemoryContradictionSweepSummary,
    RetentionSweepOutcome,
};
pub use memory_defaults::default_memory_config_for_personal_agent;
pub use memory_hot_projections::{
    apply_memory_hot_projection_maintenance, apply_memory_hot_projection_upserts,
    apply_memory_hot_projection_usage, load_memory_hot_projection_index,
    load_memory_hot_projection_index_snapshot, maintain_memory_hot_projections,
    memory_hot_projection_lane_allows_projection, memory_hot_projection_lane_allows_t0,
    memory_hot_projections_path, migrate_memory_hot_projection_keys,
    migrate_memory_hot_projection_keys_for_scope, record_memory_hot_projection_usage,
    save_memory_hot_projection_index, source_text_hash, upsert_memory_hot_projections,
    MemoryHotProjectionIndex, MemoryHotProjectionMaintenancePolicy,
    MemoryHotProjectionMaintenanceSummary, MemoryHotProjectionRecord, MemoryHotProjectionUpsert,
    MemoryHotProjectionUpsertSummary, MemoryHotProjectionUsageSummary, MEMORY_HOT_PROJECTIONS_FILE,
    MEMORY_HOT_PROJECTIONS_SCHEMA_VERSION, MEMORY_HOT_PROJECTION_POLICY_VERSION,
};
pub use memory_index::{
    acknowledge_memory_index_changes, apply_memory_index_change_snapshot,
    discover_goal_ids_for_tier, expand_memory_retrieval_query, inspect_scope_memory_index,
    inspect_scope_memory_index_fast, load_fresh_index_documents, memory_candidate_index_key,
    memory_candidate_index_score_key, memory_index_stale_reason_is_soft,
    memory_index_stale_reason_is_transient_lancedb, memory_index_staleness_class,
    optimize_scope_memory_index, quarantine_scope_memory_lancedb_index, rebuild_scope_memory_index,
    rebuild_scope_memory_index_with_lancedb_quarantine, reconcile_scope_memory_index,
    score_fresh_memory_hybrid_index, score_fresh_memory_hybrid_index_for_agent_with_status,
    score_fresh_memory_hybrid_index_for_prompt_with_status,
    score_fresh_memory_hybrid_index_with_status, score_fresh_memory_index,
    snapshot_memory_index_changes, MemoryHybridIndexScoreResult, MemoryIndexAgentSummary,
    MemoryIndexChange, MemoryIndexChangeSnapshot, MemoryIndexIncrementalUpdateOutcome,
    MemoryIndexIncrementalUpdateResult, MemoryIndexManifest, MemoryIndexOptimizeOutcome,
    MemoryIndexRebuildOutcome, MemoryIndexStalenessClass, MemoryIndexStatus,
    MemoryLanceDbOptimizeReport, MemoryLanceDbWriteReport,
};
pub use memory_prompt_blocks::{
    configure_memory_prompt_budgets, configure_memory_prompt_lane_budgets,
    render_memory_tiers_for_prompt, render_memory_tiers_for_prompt_result,
    render_memory_tiers_for_prompt_with_index, render_memory_tiers_for_prompt_with_index_result,
    render_memory_tiers_for_prompt_with_scores_result, score_hybrid_index_for_prompt,
    MemoryPromptRenderResult, MemoryPromptRenderTiming, MemoryPromptRetrievalBackend,
    MemoryPromptSelectedCandidate, MemoryRenderRequest,
};
pub use memory_provenance::{MemoryKind, MemoryTrust};
pub use memory_scope::{
    extract_scope_from_text, is_owner_confirmable_memory_tier, CandidateAttributes, MemoryScope,
    OWNER_CONFIRMABLE_MEMORY_TIERS,
};
pub use memory_temperature::{
    apply_memory_temperature_supersessions, compact_memory_temperature_overlay,
    default_temperature_tier, legacy_memory_temperature_candidate_key,
    load_memory_temperature_overlay, load_memory_temperature_overlay_snapshot,
    maintain_memory_temperature_overlay, memory_candidate_has_superseded_lifecycle,
    memory_temperature_candidate_key, memory_temperature_candidate_key_from_parts,
    memory_temperature_candidate_key_is_current, memory_temperature_candidate_key_renames,
    memory_temperature_entry_is_superseded, memory_temperature_overlay_path,
    memory_temperature_scope_partition_for_health, memory_temperature_tiers_for_prompt,
    memory_temperature_utility_review_was_applied, migrate_memory_temperature_overlay_keys,
    parse_memory_temperature_candidate_key, record_memory_temperature_outcome_usage,
    record_memory_temperature_prompt_usage, record_memory_temperature_retrieval_usage,
    record_memory_temperature_supersessions, record_memory_temperature_utility_review,
    resync_memory_temperature_overlay_full_scope, save_memory_temperature_overlay,
    sync_memory_temperature_overlay, MemoryTemperatureCandidateKeyParts,
    MemoryTemperatureCompactionSummary, MemoryTemperatureEntry,
    MemoryTemperatureMaintenanceSummary, MemoryTemperatureOutcomeSignal,
    MemoryTemperatureOutcomeUsageSummary, MemoryTemperatureOverlay,
    MemoryTemperatureRetentionPolicy, MemoryTemperatureSupersession,
    MemoryTemperatureSupersessionSummary, MemoryTemperatureTier, MemoryTemperatureUsageSummary,
    MemoryTemperatureUtilityLabel, MemoryTemperatureUtilityReviewJudgement,
    MemoryTemperatureUtilityReviewSummary, MEMORY_TEMPERATURE_OVERLAY_FILE,
    MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION, SUPERSEDED_BY_CANDIDATE_KEY_METADATA_KEY,
    SUPERSEDED_BY_ITEM_KEY_METADATA_KEY,
};
pub use memory_tier_health::{
    compute_memory_tier_health, evaluate_memory_tier_health, memory_tier_partitions,
    MemoryTierHealthGateResult, MemoryTierHealthGates, MemoryTierHealthMetrics,
    MemoryTierLaneHealth,
};
pub use memory_tier_interpreter::{DefaultMemoryRenderer, MemoryTierInterpreter};
pub use memory_tiers::{
    ActionSummary, ArchiveSummary, ArchiveTier, BuiltinTransform, ConsolidationInput,
    ConsolidationTransform, ConsolidationTrigger, DeltaOperation, EntitiesTier, EntityRecord,
    InsightRecord, KnowledgeTier, MemoryConsolidationRule, MemoryDelta, MemoryRenderer,
    MemoryTierDefinition, MergeStrategy, ParsedTierRef, PatternRecord, RenderConfig, RetentionMode,
    SourceRef, StrategyEffectiveness, StrategyRecord, TaskProgressItem, TaskProgressTier,
    TierFieldSchema, TierScope, TransformOutput,
};
pub use memory_utility_reviewer::run_memory_temperature_utility_batch_maintenance_with_telemetry;
pub use memory_utility_reviewer::{
    enqueue_memory_temperature_utility_review, memory_temperature_utility_queue_health,
    review_memory_temperature_utility, review_memory_temperature_utility_batch,
    run_memory_temperature_utility_batch_maintenance, spawn_memory_temperature_utility_review,
    spawn_memory_temperature_utility_review_batch, MemoryTemperatureUtilityBatchMaintenanceConfig,
    MemoryTemperatureUtilityBatchMaintenanceSummary, MemoryTemperatureUtilityQueueHealth,
    MemoryTemperatureUtilityReviewBatchInput, MemoryTemperatureUtilityReviewInput,
    MemoryTemperatureUtilityReviewTraceItem,
};
pub use outward_actions::{
    capability_outward_class, outward_action_class, outward_dispatch_class, outward_disposition,
    OutwardClass, OutwardDisposition,
};
pub(crate) use personal_agent_retrieval::{
    PersonalAgentRetrievalApplyOutcomeV1, PersonalAgentRetrievalError,
    PersonalAgentRetrievalExpectedHeadV1, PersonalAgentRetrievalGrantFenceV1,
    PersonalAgentRetrievalOwner, PersonalAgentRetrievalProviderIdentityV1,
    PersonalAgentRetrievalReceiptV1,
};
pub use personal_agent_retrieval::{
    PersonalAgentRetrievalInspectionItemV1, PersonalAgentRetrievalInspectionSnapshotV1,
    PersonalAgentRetrievalInspectionStateV1,
};
pub use prompt_pipeline::{
    PromptPipelineError, PromptPipelineInputs, PromptPipelineInterpreter,
    PromptPipelineRuntimeInputs,
};
pub use proposals::{
    DefinitionProposal, NewDefinitionProposal, ProposalApplicationError, ProposalDecision,
    ProposalFilter, ProposalResolveOutcome, ProposalStatus, ProposalStore, ProposalStoreError,
};
pub use retrieval_scope::{
    label_for_item, label_from_metadata, label_from_token, stamp_engagement_scope, ContextLabel,
    RetrievalScope, ScopeDecision, ENGAGEMENT_SCOPE_KEY, ENGAGEMENT_TOKEN_PREFIX,
    MEETING_TOKEN_PREFIX, NEUTRAL_TOKEN,
};
pub use runtime::{
    default_expected_artifact_declarations, AgentCycleReservation, AgentGoalRecord, AgentRuntime,
    CycleContext, GoalLaunchGate, GoalOutcomeTransition, GoalTaskOptions, GoalTriggerReceipt,
    RuntimeDelegationDispatcher, TriggerAdmission,
};
pub use scheduler::{
    AgentScheduler, ScheduledTrigger, SchedulerEntryState, SchedulerError, SchedulerState,
    SchedulerTriggerRegistration,
};
pub use state_machine::{
    StateMachineAction, StateMachineDefinition, StateMachineError, StateMachineGuard,
    StateMachineInterpreter, StateMachineTransition, StateMachineTransitionResolution,
    StateMachineTrigger, TransitionContext,
};
pub use storage::{sanitize_segment, slugify_name, AgentStorage, AgentStorageError, FileLockGuard};
pub use trust::{TrustPolicyEnforcer, TrustPolicyError, TrustPolicyFile};
pub use types::{
    disabled_agent_hierarchy, is_system_agent_id, resolve_effective_delegation_target_ids,
    resolve_effective_delegation_target_ids_for_surface, ActionPattern, AgentAppToolContract,
    AgentConstraints, AgentDefinition, AgentDefinitionError, AgentDelegationPolicy,
    AgentDiscoverability, AgentId, AgentInvocationContext, AgentInvocationPolicy,
    AgentRoutingContext, ApprovalCondition, ApprovalRule, ArtifactDeclaration, ArtifactProvenance,
    ChannelConfig, CircuitAction, CircuitBreakerOverride, CircuitBreakerPolicy,
    CircuitBreakerThreshold, CircuitRecovery, CoordinationConfig, CorrectionRetention,
    CountConstraint, CycleId, EpisodeRetention, EvaluationCriterion, FeatureMode, FeedbackExtract,
    FeedbackLoopDefinition, GoalId, GoalPriority, GoalSource, InvocationSourceKind,
    InvocationSurface, LlmEndpoint, LlmRoutingConfig, NotificationRule, NotificationSeverity,
    PromptOutputRules, PromptPipelineConfig, PromptSection, RetentionPolicy, StepArtifact,
    StrategyPreference, SurfaceAudience, TierRef, ToolActionPattern, TrustLevel, TrustPolicy,
    VersionRetention, AGENT_KEY_PREFIX, SYSTEM_AGENT_ID_PREFIX,
};

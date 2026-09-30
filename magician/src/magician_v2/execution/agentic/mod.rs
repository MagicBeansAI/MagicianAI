//! # Agentic Execution Module
//!
//! This module implements the observe-decide-execute loop for autonomous task completion.
//! Instead of single-pass action execution, the agentic executor loops until each step's
//! goal is achieved, using the LLM to make decisions based on observed environment state.
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────┐    ┌─────────┐    ┌─────────┐
//! │ OBSERVE │───▶│ DECIDE  │───▶│ EXECUTE │
//! └─────────┘    └─────────┘    └─────────┘
//!      │              │              │
//!      ▼              ▼              ▼
//! State + History  Native LLM    Dispatcher
//!                  (agentic_decision)
//! ```
//!
//! ## LLM Calls
//!
//! 1. **Native LLM** (agentic_decision): Receives runtime context and chooses a tool call.
//!
//! ## Supported Action Types
//!
//! - **Browser**: Via the browser inner loop and pinned `agent-browser` CLI
//! - **File**: Via tokio::fs for filesystem operations
//! - **HTTP**: Via reqwest for API calls
//! - **Bash**: Via tokio::process for shell commands

mod app_invocation;
mod app_tool_feedback;
mod decision;
pub mod delegation_dispatch;
pub mod environment_knowledge;
mod executor;
mod input_interpreter;
mod loop_detector;
pub mod native_adapter;
pub mod native_catalog;
pub mod native_integration;
pub mod native_lowering;
pub mod native_types;
pub mod output_verifier;
/// Tier 4: bind a completed outward dispatch to the provider message it became.
pub mod outward_settle;
pub mod ownership_runtime;
pub mod policy_snapshot;
pub mod preflight;
/// The stateless-loop extraction — see `run_loop/mod.rs` for the landing order.
pub mod run_loop;
pub mod scheduler;
pub mod shell_tool;
pub mod types;
pub mod yield_decision;

pub use decision::build_delegation_results_section;
pub use decision::Decision;
pub use delegation_dispatch::{
    DelegationDispatcher, DelegationSpawnResult, DelegationTarget, DispatchError,
    SpawnedDelegationChild,
};
pub(crate) use executor::persist_pause_with_workflow_authority_receipt;
pub use executor::{
    admit_live_agent_loop, app_pause_continuation_bytes, pause_authority_hash,
    persist_pause_with_workflow_authority, sync_tool_lane_allowed_action_types,
    validate_app_pause_continuation_classified, AppPauseContinuationValidationError,
};
pub(crate) use executor::{
    delegation_successor_deferred_until, park_resume_address,
    protected_delegation_claim_deferred_until, protected_delegation_is_already_consumed,
    stateless_worker_id_for_exact_segment,
};
pub use executor::{
    exact_pause_admission_failure, execute_agent_cycle, execute_agentically,
    execute_agentically_continue, execute_agentically_resume_exact,
    execute_agentically_resume_with_validation, forget_exact_pause_process_authority,
    format_input_type_for_event, sanitize_retry_previous_answer, session_id_for_execution,
    ActionExecutors, AgenticExecutor, ApiReplayMeta, ExecutionPauseKind, FullPauseData,
    FullPauseStore, PauseDurabilityError, PendingPauseInfo, ResumeResult, SerializablePauseData,
    StatelessTerminalAgentLifecycleExclusion, StatelessTerminalAgentPauseAdmission,
    StatelessTerminalAgentSetLifecycleExclusion, StatelessTerminalExecutionLifecycleExclusion,
    StatelessTerminalLifecycleExclusion,
};
// STEP 3 — single-execution-context pipeline. The owner-swap RAII guard used by
// the orchestrator's `execute_pipeline_single_context` engine to run each stage
// under its own agent persona/trust on the SAME ExecutionRun. `pub` —
// internal engine machinery, not part of the public execution surface.
pub use executor::StageGuard;
// The definition of *"what name does the dispatch check compare against"*.
// Re-exported because anything validating a grant has to answer the same
// question, and the engagement ceiling validator used to answer it by
// hand-writing the same strings in another crate.
pub use executor::executable_action_policy_name;
// Plane Task 4 — `execution::plane::dispatch` calls the same governed
// action path the loop does. These stay crate-visible rather than making
// `mod executor` public.
pub(crate) use executor::{
    approval_step_from_candidate, checked_approval, execute_action,
    is_interrupted_recovery_already_owned, refresh_trust_dispatch_guard_for_decision,
    stable_confirmation_action_json, DurablePauseCommit, ManualResumeClaimBinding,
    ManualResumeClaimRenewal, ManualResumeRetirementOutcome,
};
pub use input_interpreter::{try_simple_extraction, AgenticInputInterpreter, InterpretationResult};
pub use loop_detector::{
    ActionFingerprint, BrowserFingerprint, EnvironmentFingerprint, FilesystemFingerprint,
    HttpFingerprint, LoopCheckResult, LoopDetector, ShellFingerprint,
};
pub use native_types::{
    ExecutionDecisionEnvelope, ExecutionNativeRequest, ExecutionNativeResponse, ExecutionToolCall,
    NativeDecisionOutcome, NativeExecutionTool,
};
pub use ownership_runtime::{OwnerExecutionProfile, OwnershipRuntime};
pub use policy_snapshot::{
    extend_snapshot_with_runtime_tool, feature_agent_id, feature_surface_is_authorized,
    parse_leading_feature_invocation, parse_leading_feature_marker, remove_snapshot_runtime_tool,
    resolve_effective_tool_policy_snapshot, validate_agent_invocation, EffectiveToolPolicySnapshot,
    SnapshotResolutionInput,
};
pub use types::{
    is_secret_param_name, parse_redacted_placeholder, redacted_placeholder,
    split_hitl_correlation_id, ActionOutcomeCategory, ActionResultRecord, AgenticContext,
    AgenticContextOverrides, AgenticOutcome, AgenticPauseState, Artifact, AutonomousPromptControls,
    BudgetDimension, ChoiceOption, DiffApprovalFile, EnvironmentState, ExecutionHistory,
    FilesystemState, FormAnswer, FormQuestion, HttpState, IterationRecord, PendingInput,
    PendingInputSource, PipelineLoopStateSegment, PromptAgentKind, PromptIdentityContext,
    ShellState, UserInputResponse, UserInputType, UserInputValue, HITL_ASK_SEPARATOR,
    REDACTED_PREFIX, REDACTED_SUFFIX,
};
pub use yield_decision::{
    dispose_yield, YieldBlocker, YieldBlockerKind, YieldDecision, YieldDisposition,
    YieldSelfClassification,
};
// NOTE: BrowserState removed - use PageState from execution/types.rs instead

mod desktop_capture;

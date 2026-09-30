//! Execution engine scaffolding for Magician V2.
//!
//! This module exposes shared lowering/types plus the direct agentic execution
//! path used by the unified 3-mode runtime.

pub mod actions;
pub mod agent_resources;
pub mod agent_roster_data_provider;
pub mod agentic;
pub mod builtin_action_types;
pub mod capability;
pub mod capability_eval;
pub mod capability_pack;
pub mod coding_engine;
pub mod compiled_dispatch;
pub mod compiled_handlers;
pub mod compiled_providers;
pub mod constants;
pub mod dom_parser;
pub mod durable_task_state;
pub mod effective_action;
pub mod error;
pub mod evidence_data_provider;
pub mod execution_summary;
pub mod file_edit;
pub mod flat_loop;
pub mod harness_provider;
pub mod inference;
pub mod interactive_process;
pub mod internal_data_provider;
pub mod media_edit;
pub mod meetings_data_provider;
pub mod memory_data_provider;
pub mod merkle;
pub mod native_executors;
pub mod notes_data_provider;
pub mod pack_provider;
pub mod plane;
pub mod primitive_dispatch;
pub mod resource_authority_provider;
pub mod restricted_action;
pub mod restricted_toolset;
pub mod runtime_boundary;
pub mod tasks_data_provider;
// Bounding rectangle type used by `execution::types`. Lives in its own
// module so it can stay pure-data while the SoM screenshot-annotation
// chain is retired alongside the old magicutor-driven flow.
pub mod bounding_rect;
pub mod task_reconcile;
pub mod task_state_provider;
pub mod thinking_maps_data_provider;
pub mod treasurer_provider;
pub mod trusted_store;
// Verification controller: holds a VibeDev candidate success before the
// terminal transaction, runs the project's required checks against an
// immutable snapshot of that candidate, and releases completion only on
// green. Inert until `VerificationActivation::Enforce` — see the module docs.
pub mod verification;
pub mod verified_executor;
// NOTE: recovery module removed - agentic execution handles state recovery via observe-decide-execute loop
pub mod executor_factory;
pub mod scoped_capability_resolver;
pub mod screenshot_cache;

mod lowering;
mod magicutor_client;
mod multi_llm_agent_adapter;
// `observation_policy` was deleted 2026-08-27. It held an action-aware policy
// whose only input, `LoopProtectiveState::last_action_context`, had no producer
// anywhere in the workspace — three sites cleared it, none filled it — so
// `should_observe_after_action` answered `Full` on every iteration, its
// `Skip { action_context }` arm was unreachable, and the `latency_savings_ms`
// figures (2s/3s/6s) described a policy that was not operating. The per-iteration
// re-capture it was written to skip had already been retired: in the flat loop
// the post-action state IS the observation. Nothing else in the module had a
// consumer either — `EscalationTrigger` and `is_form_submission_action` were
// exported and never called. See `run_loop/phases/observe.rs` for what remains.
pub mod tool_catalog_prompt;
mod types;

#[cfg(any(test, feature = "test-fixtures"))]
mod lowering_tests;

pub use agent_roster_data_provider::{AgentRosterDataProvider, AGENT_ROSTER_DATA_TOOL_NAME};
pub use effective_action::{resolve_effective_action, EffectiveAction};
pub use error::{ExecutionError, ExecutionResult};
pub use evidence_data_provider::{EvidenceDataProvider, EVIDENCE_DATA_TOOL_NAME};
pub use lowering::{
    extract_placeholder_intent, extract_placeholder_param_intent, is_browser_tool, is_file_tool,
    is_http_tool, is_placeholder_param, is_placeholder_selector, is_shell_tool,
    lower_plan_to_executable_steps_with_registry, lower_step_with_registry,
};
#[doc(hidden)]
pub use lowering::{lower_plan_to_executable_steps, lower_step_to_executable_action};
pub use magicutor_client::{ExecutionConfig, MagicutorClient, MagicutorClientError};
pub use meetings_data_provider::{MeetingsDataProvider, MEETINGS_DATA_TOOL_NAME};
pub use memory_data_provider::{MemoryDataProvider, MEMORY_DATA_TOOL_NAME};
pub use multi_llm_agent_adapter::MultiLlmAgentAdapter;
#[cfg(any(test, debug_assertions))]
pub use multi_llm_agent_adapter::TestNativeResponseQueue;
pub use notes_data_provider::{NotesDataProvider, NOTES_DATA_TOOL_NAME};
pub use restricted_action::{
    has_restricted_form, is_dispatch_routing_key, restrict, restricted_form_for, BoundDispatch,
    RestrictedAction, RestrictionError, SenderIdentity,
};
pub use restricted_toolset::{project_capability_tool, split_capability_leaf, ToolProjection};
pub use tasks_data_provider::{TasksDataProvider, TASKS_DATA_TOOL_NAME};
pub use types::*;

// Re-export action types for multi-action execution
// NOTE: types::ExecutableStep.action is now ExecutableAction (unified for all action types).
// All actions (Browser, File, HTTP, Bash) are lowered during batch lowering.
pub use actions::{
    ActionResult, BashAction, DuckDbAction, ExecutableAction, FileAction, HttpAction, HttpMethod,
};
pub use execution_summary::{
    render_stage_goal, work_outcome_from_summary, work_outcome_input_from_summary,
    AgenticExecutionSummaryRecord, ChildDeliverableSummary, ChildMediaRef, PipelineStage,
    PipelineState,
};

// Re-export capability system types
pub use capability::{
    CapabilityPackDefinition, CapabilityProvider, CapabilityRegistry, ChatInlineAdapter,
    CompositeStep, ExecutionMetadata, ImplementationType, ParameterDef, ParameterType,
    ResolvedCapabilityTool,
};
pub use capability_eval::{
    evaluate_transition, CapabilityEvaluationDecision, CapabilityEvaluationInput,
    CapabilityEvaluationThresholds, CapabilityLifecycleStatus,
};
pub use capability_pack::{
    is_legacy_browser_api_replay_definition, CapabilityPackCatalog, CapabilityPackMetadata,
    CapabilityPackRecord, CapabilityPackSource, CapabilityPackStore, CapabilityPromotionAuditEvent,
    CapabilityPromotionBridge, CapabilityPromotionReport,
};
pub use coding_engine::{
    compute_shadow_workspace_patch, prepare_shadow_workspace, proposal_pending_approval_response,
    stage_shadow_workspace_patch, CodingEngineAdapter, CodingEngineEvent, CodingEngineEventKind,
    CodingEngineKind, CodingEngineRequest, CodingEngineRunResult, PiCodingEngineAdapter,
    ShadowPatchOptions,
};
pub use compiled_providers::{
    build_compiled_registry, embedded_compiled_pack_defs, embedded_compiled_pack_defs_ref,
    embedded_compiled_pack_yaml, load_pack_defs_from_skills_dir, load_skills_dir,
    pack_defs_to_tool_infos, prune_runtime_disabled_pack_defs, prune_unexecutable_pack_defs,
    PackToolInfo, SkillLoadFailure, SkillsDirLoad,
};
pub use harness_provider::{register_harness_action_providers, register_harness_read_providers};
pub use internal_data_provider::{InternalDataProvider, INTERNAL_DATA_TOOL_NAME};
pub use pack_provider::{load_from_yaml, PackCapabilityProvider};
pub use scoped_capability_resolver::{
    ScopedCapabilityCacheStatus, ScopedCapabilityResolver, ScopedCapabilitySnapshot,
};
pub use task_state_provider::TaskStateProvider;
pub use thinking_maps_data_provider::{ThinkingMapsDataProvider, THINKING_MAPS_DATA_TOOL_NAME};
pub use treasurer_provider::{TreasurerCapabilityProvider, TREASURER_TOOL_NAME};

// Re-export native executors
pub use native_executors::{
    execute_bash_action, execute_duckdb_action, execute_file_action, execute_http_action,
    validate_path_security, DuckDbSession, ShellStreamContext,
};

// NOTE: Recovery types removed - agentic execution handles state recovery implicitly.
// See execution/agentic/ module for the observe-decide-execute loop.

// Re-export Merkle tree types for page state tracking
pub use merkle::{
    diff_siblings, match_siblings_lcs, ChangedSubtree, HashableProperties, MerkleBoundingBox,
    MerkleDiff, MerkleNode, MerkleNodeType, PageMerkleTree, SiblingMatchResult, SubtreeChangeType,
};

// Re-export screenshot storage for observability
pub use screenshot_cache::{
    DecisionRecord, ScreenshotMetadata, ScreenshotStorage, StorageStats, StoredScreenshot,
};

// Re-export agentic execution types
pub use agentic::{
    execute_agentically, AgenticContext, AgenticContextOverrides, AgenticExecutor, AgenticOutcome,
    Artifact, AutonomousPromptControls, Decision, EnvironmentState, ExecutionHistory,
    FilesystemState, HttpState, IterationRecord, PromptAgentKind, PromptIdentityContext,
    ShellState,
};
// NOTE: BrowserState removed - use PageState from types module instead

// Re-export DOM parser types for Merkle tree building
pub use dom_parser::{parse_dom_elements, BoundingBox, DomElement, DomParserConfig};

// NOTE: DOM parsing is done directly via dom_parser module for Merkle tree construction.
// MappedElement is defined in types.rs and exported via `pub use types::*`

// NOTE: SelectorResolver removed - SoM visual grounding is now the primary targeting mechanism.

// `BoundingRect` is the only piece of the old screenshot-annotation
// surface still consumed by `execution::types`. Re-exported under its
// historical alias `SoMBoundingRect` so the 22 consumers there don't
// need a rename.
pub use bounding_rect::BoundingRect as SoMBoundingRect;

// Re-export action candidate metadata used by the visible agent runtime.
pub use verified_executor::{ActionCandidate, CandidateBatch};

//! Shared planning graph structures produced by exploration strategies.
//!
//! These types provide a stable contract between planning strategies and
//! downstream execution / elicitation layers. Strategies should populate the
//! structures with as much structured information as they can reasonably
//! extract during planning. Consumers can rely on the graph even while the
//! underlying strategies evolve.

use std::{collections::HashMap, fmt::Write as _};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Complete planning graph emitted by a strategy.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanGraph {
    /// Ordered list of steps that constitute the plan.
    #[serde(default)]
    pub steps: Vec<PlanStep>,
    /// Directed edges describing dependencies between steps.
    #[serde(default)]
    pub edges: Vec<PlanEdge>,
    /// Unresolved inputs that require clarification from the user.
    #[serde(default)]
    pub unresolved_inputs: Vec<UnresolvedInput>,
    /// Overall confidence assigned by the strategy.
    #[serde(default)]
    pub confidence: f32,
    /// Provenance information for audit/debugging.
    #[serde(default)]
    pub provenance: PlanProvenance,

    // ========== v1.1 Extensions ==========
    /// Session policy for execution.
    #[serde(default)]
    pub session_policy: SessionPolicy,

    /// Metadata from the planning pipeline.
    #[serde(default)]
    pub planning_metadata: PlanningMetadata,

    // ========== v1.2 Extensions ==========
    /// Contract describing the expected response shape for the plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_contract: Option<ResponseContract>,
}

/// Single planned step referencing a tool invocation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanStep {
    /// Stable identifier for the step (usually matches exploration node id).
    pub id: String,
    /// Natural language description of the task.
    pub task: String,
    /// Tool the step intends to execute, if known.
    pub tool: Option<String>,
    /// Parameters already resolved for the tool.
    ///
    /// I-13: changed from `HashMap<String, String>` to `HashMap<String, serde_json::Value>`
    /// so numeric, boolean, and array parameters flow through without string round-trips.
    #[serde(default)]
    pub parameters: HashMap<String, serde_json::Value>,
    /// Expected outputs or side effects (best-effort).
    pub expected_outputs: Vec<String>,
    /// Confidence assigned to this particular step.
    pub confidence: f32,
    /// Additional metadata (strategy specific hints, reasoning, etc.).
    pub metadata: HashMap<String, String>,
    /// Custom timeout override for this step (seconds).
    ///
    /// If set, overrides agent-type and default timeouts.
    /// Still capped by tool_max_timeout_secs (600s).
    /// Use for known long-running operations like complex browser automation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_override_secs: Option<u64>,

    // ========== v1.1 Extensions ==========
    /// Ordered list of step IDs this step depends on (derived from edges by PlannerAgent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Prose description of what constitutes success for this step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub success_criteria: Option<String>,

    /// Provenance tracking for each resolved parameter.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub parameter_provenance: HashMap<String, ParameterProvenance>,

    // ========== v1.2 Extensions ==========
    /// Semantic role of this step within the plan graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<StepRole>,

    // ========== v1.3 Extensions (Phase 1 scheduling rearchitecture) ==========
    /// Hash for plan invalidation: `hash(providing_agent_id + tool_id)`.
    /// If the agent or tool is removed/changed, this step needs re-planning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_hash: Option<String>,

    /// Which agent provides the tool for this step.
    /// Used for delegation routing during execution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providing_agent_id: Option<String>,

    // ========== v1.4 Extensions (Progressive Planning V2) ==========
    /// Readiness classification for progressive refinement.
    /// Absent (None) is treated as Weak for backward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<StepReadiness>,

    // ========== v1.5 Extensions (Delegation sub-steps) ==========
    /// Sub-steps discovered during execution (e.g., actions taken by a delegated agent).
    /// Preserved through plan hardening so replayed plans retain the full action sequence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_steps: Vec<SubStep>,
}

/// A sub-step within a plan step, representing a discrete action taken during execution.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SubStep {
    /// Human-readable description of the sub-step.
    pub label: String,
}

impl PlanStep {
    /// Compute the step hash from the providing agent and tool.
    ///
    /// Uses the standard library `DefaultHasher` to produce a 16-hex-char digest.
    /// The hash is intentionally cheap and deterministic (not cryptographic) — it
    /// exists purely as a cache-invalidation signal for the planner.
    pub fn compute_hash(agent_id: &str, tool_id: &str) -> String {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        agent_id.hash(&mut hasher);
        tool_id.hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }

    /// Check if this step's hash is still valid against the given agent + tool pair.
    ///
    /// Returns `true` when:
    /// - `step_hash` is `None` (legacy plans without hashes are always valid), or
    /// - `step_hash` matches the recomputed hash for the given `agent_id` + `tool_id`.
    pub fn is_hash_valid(&self, agent_id: &str, tool_id: &str) -> bool {
        match &self.step_hash {
            Some(hash) => *hash == Self::compute_hash(agent_id, tool_id),
            None => true, // No hash = always valid (legacy plans)
        }
    }
}

/// Compute a stable hash for step provenance validation.
///
/// Returns a 16-hex-char digest of `providing_agent_id` + `tool_id` using
/// the standard library `DefaultHasher`. This is a convenience alias for
/// [`PlanStep::compute_hash`].
pub fn compute_step_hash(providing_agent_id: &str, tool_id: &str) -> String {
    PlanStep::compute_hash(providing_agent_id, tool_id)
}

/// Directed dependency between two steps.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanEdge {
    /// Source step identifier.
    pub from: String,
    /// Target step identifier.
    pub to: String,
    /// Human readable explanation of the dependency.
    pub reason: String,
}

/// Priority level for eliciting an unresolved input from the user.
///
/// Used by the sensible elicitation system (Tier 1) to classify and batch questions appropriately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum QuestionPriority {
    /// Must have before planning; affects plan structure (e.g., tool_selection, auth_method)
    Critical,
    /// Needed before execution starts, but doesn't affect plan (e.g., user_email, repository_url)
    #[default]
    PreExecution,
    /// Ask right before the step that needs it runs (e.g., commit_message, confirmation)
    JustInTime,
    /// Use defaults, ask only if inference fails (e.g., meeting_title, notification_enabled)
    Optional,
    /// Try inference first, ask only if confidence < threshold (e.g., duration, timezone)
    Inferrable,
}

/// When to ask the user for this unresolved input.
///
/// Used by the sensible elicitation system (Tier 4) for just-in-time vs upfront questioning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AskTiming {
    /// Ask during planning phase (before execution starts)
    #[default]
    PreExecution,
    /// Ask right before the step that needs it (during execution)
    JustInTime,
    /// Never ask user (use default/inference only)
    Never,
}

/// When to attempt autonomous discovery for this unresolved input.
///
/// Used by the sensible elicitation system (Tier 5) to decide discovery timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryTiming {
    /// Discover during planning phase (before execution)
    PreExecution,
    /// Discover right before the step that needs it (during execution)
    JustInTime,
    /// Auto-detect based on parameter properties (shared vs step-specific, etc.)
    #[default]
    Auto,
}

/// Source/provenance of the unresolved input or its value.
///
/// Tracks where this input came from or how its value was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    /// Created by planning strategy
    #[default]
    Planner,
    /// Created by plan validator
    Validator,
    /// Extracted from user's original query
    QueryAnalysis,
    /// Inferred from context/history/defaults
    Inference,
    /// Discovered autonomously by agent
    Discovery,
    /// Provided directly by user
    UserReply,
    /// Auto-filled from available parameters
    AutoFill,
}

/// Resolution status of an unresolved input during elicitation lifecycle.
///
/// Tracks the current state during the elicitation/discovery/resolution process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputStatus {
    /// Waiting for user input or automated resolution
    Pending,
    /// Value has been filled (automatically or manually)
    Resolved,
    /// Autonomous discovery in progress
    Discovering,
    /// Value was inferred (may need user confirmation)
    Inferred,
    /// User explicitly skipped (using default instead)
    Skipped,
}

/// Unified representation of an unresolved input that flows through the entire lifecycle:
/// planning → validation → orchestration → elicitation → discovery → inference → execution.
///
/// This structure replaces the previous separate `InputHole` and `SlotRegistration` structures,
/// providing a single source of truth with all metadata needed for intelligent question management.
///
/// # Lifecycle
/// 1. **Planning**: Strategy creates UnresolvedInput with basic fields (parameter, prompt, priority)
/// 2. **Validation**: Validator enriches with schema, requirements, display names
/// 3. **Orchestration**: Orchestrator classifies, discovers, infers, or asks
/// 4. **Elicitation**: Manager registers with storage fields (created_at, status)
/// 5. **Execution**: Executor uses resolved values
///
/// # Sensible Elicitation
/// Fields marked with tier numbers support the 5-tier sensible elicitation system:
/// - Tier 1: Priority classification (priority field)
/// - Tier 2: Inference engine (default_value, inference_hints, auto_fill)
/// - Tier 3: Upfront batching (priority classification)
/// - Tier 4: Just-in-time questions (ask_timing field)
/// - Tier 5: Autonomous discovery (discovery_timing field)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnresolvedInput {
    // ========== Core Identity ==========
    /// Unique identifier for this input (e.g., "slot_github_repo_url", "param_duration")
    pub id: String,

    /// Logical parameter name (e.g., "github_repo_url", "duration_minutes")
    pub parameter: String,

    /// Human-friendly display name shown in UI (e.g., "GitHub Repository", "Duration")
    #[serde(default)]
    pub display_name: String,

    // ========== Planning Context ==========
    /// Step that requires this input (None if shared across steps)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_id: Option<String>,

    /// All steps that depend on this input being resolved
    #[serde(default)]
    pub linked_steps: Vec<String>,

    /// Expected datatype or shape (e.g., "string", "number", "slot", "consent_flag")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_type: Option<String>,

    /// JSON schema for validation and UI generation
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,

    // ========== Elicitation ==========
    /// Prompt shown to user when asking for this input
    #[serde(default)]
    pub prompt: String,

    /// Whether this input is required for execution (vs optional with default)
    #[serde(default = "default_true")]
    pub required: bool,

    /// Additional notes, context, or instructions
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,

    // ========== Sensible Elicitation (Tiers 1-5) ==========
    /// Priority classification for question management (Tier 1)
    #[serde(default)]
    pub priority: QuestionPriority,

    /// When to ask user: during planning vs during execution (Tier 4)
    #[serde(default)]
    pub ask_timing: AskTiming,

    /// When to discover: pre-execution vs just-in-time (Tier 5)
    #[serde(default)]
    pub discovery_timing: DiscoveryTiming,

    /// Default value to use if user doesn't provide one (Tier 2)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_value: Option<Value>,

    /// Hints for inference engine (e.g., ["user_email", "recent_contacts"]) (Tier 2)
    #[serde(default)]
    pub inference_hints: Vec<String>,

    /// Minimum confidence threshold for inference (0.0-1.0) (Tier 2)
    #[serde(default = "default_inference_threshold")]
    pub inference_threshold: f64,

    // ========== Auto-Fill / Inference Results ==========
    /// Pre-filled value from query analysis, inference, or discovery
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_fill: Option<Value>,

    /// Confidence score for auto-filled value (0.0-1.0)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_fill_confidence: Option<f32>,

    /// Source/provenance of this input or its value
    #[serde(default)]
    pub source: InputSource,

    // ========== Storage Metadata (populated when persisted) ==========
    /// When this input was first registered in storage (None during planning)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<DateTime<Utc>>,

    /// When this input was last updated in storage (None during planning)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<DateTime<Utc>>,

    /// Current resolution status (None during planning)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<InputStatus>,
}

// Default helper functions
fn default_true() -> bool {
    true
}

fn default_inference_threshold() -> f64 {
    0.7
}

impl Default for UnresolvedInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            parameter: String::new(),
            display_name: String::new(),
            step_id: None,
            linked_steps: Vec::new(),
            expected_type: None,
            json_schema: None,
            prompt: String::new(),
            required: true,
            notes: None,
            priority: QuestionPriority::default(),
            ask_timing: AskTiming::default(),
            discovery_timing: DiscoveryTiming::default(),
            default_value: None,
            inference_hints: Vec::new(),
            inference_threshold: 0.7,
            auto_fill: None,
            auto_fill_confidence: None,
            source: InputSource::default(),
            created_at: None,
            updated_at: None,
            status: None,
        }
    }
}

impl UnresolvedInput {
    /// Create a minimal UnresolvedInput with just ID and parameter (for migration/testing).
    pub fn minimal(id: impl Into<String>, parameter: impl Into<String>) -> Self {
        let param = parameter.into();
        Self {
            id: id.into(),
            parameter: param.clone(),
            display_name: humanize_parameter(&param),
            prompt: format!("Please provide {}", param),
            ..Default::default()
        }
    }

    /// Create from legacy InputHole-style data (for migration compatibility).
    pub fn from_legacy(
        parameter: String,
        step_id: Option<String>,
        expected_type: Option<String>,
        prompt: Option<String>,
        notes: Option<String>,
    ) -> Self {
        let id = format!("input_{}", parameter);
        Self {
            id: id.clone(),
            parameter: parameter.clone(),
            display_name: humanize_parameter(&parameter),
            step_id,
            expected_type,
            prompt: prompt.unwrap_or_else(|| format!("Please provide {}", parameter)),
            notes,
            ..Default::default()
        }
    }
}

/// Helper to convert snake_case parameter names to human-readable display names.
pub fn humanize_parameter(param: &str) -> String {
    param
        .replace('_', " ")
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Provenance tracking for a single resolved parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterProvenance {
    /// Source of the parameter value.
    pub source: InputSource,
    /// Confidence in the resolved value.
    ///
    /// M-14: `None` = "not evaluated"; `Some(0.0)` = "evaluated at zero confidence".
    /// Previously `f32` made these two cases indistinguishable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    /// Slot ID if resolved from elicitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slot_id: Option<String>,
    /// Method used for resolution (e.g., "user_reply", "inference", "default").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
}

// ==================== v1.2 Types ====================

/// The kind of response a plan is expected to produce.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseKind {
    /// The plan results in an executable action (tool invocation, side-effect).
    Action,
    /// The plan results in a textual answer to the user.
    Text,
    /// The plan spawns a child task / sub-goal.
    SpawnTask,
    /// The plan produces no user-visible output (background bookkeeping).
    Silent,
    /// The plan produces a combination of response kinds.
    Composite,
}

/// Contract describing the expected response shape for a plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseContract {
    /// What kind of response the plan produces.
    pub kind: ResponseKind,
    /// Step ID of the terminal step whose output becomes the plan response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_step: Option<String>,
}

/// Readiness classification for progressive planning (V2).
///
/// Determines whether a step can be executed immediately, deferred to
/// runtime, or needs pre-execution refinement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepReadiness {
    /// Tool + params resolved, confidence >= threshold. Execute as-is.
    Ready,
    /// Depends on runtime state (page content, API response, etc).
    /// Executor handles via agentic OEO loop.
    JustInTime,
    /// Needs refinement — missing params, ambiguous tool, or low confidence.
    /// May trigger JIT elicitation during step execution.
    Weak,
}

// ---------------------------------------------------------------------------
// Readiness classification (Phase C)
// ---------------------------------------------------------------------------

/// Classify readiness for all steps in a PlanGraph (no LLM needed).
///
/// Applies deterministic heuristics to each step and sets `step.readiness`.
/// Call this after the planner produces the initial PlanGraph.
pub fn classify_readiness(graph: &mut PlanGraph) {
    for step in &mut graph.steps {
        step.readiness = Some(classify_step_readiness(step));
    }
}

/// Classify a single step's readiness based on deterministic rules.
///
/// Exported so that [`PlanPatcherAgent`] can re-classify individual steps
/// after refinement without re-running the whole graph.
pub fn classify_step_readiness(step: &PlanStep) -> StepReadiness {
    // Rule 1: runtime-dependent steps are JIT
    if step
        .metadata
        .get("runtime_dependent")
        .map(|v| v == "true")
        .unwrap_or(false)
    {
        return StepReadiness::JustInTime;
    }

    // Rule 2: confidence >= 0.8 and tool is set → Ready
    let has_tool = step.tool.is_some();
    let has_good_confidence = step.confidence >= 0.8;
    if has_tool && has_good_confidence {
        return StepReadiness::Ready;
    }

    // Rule 3: everything else is Weak
    StepReadiness::Weak
}

/// Semantic role of this step within the plan graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepRole {
    /// The step is the entry point of the plan.
    Initial,
    /// The step is an intermediate transformation step.
    Intermediate,
    /// The step is the terminal / final step whose output surfaces as the plan response.
    Terminal,
    /// A step that validates the output of a previous step.
    Validation,
    /// A conditional branch point; `condition` describes the branching predicate.
    Branch { condition: String },
}

/// Session policy controlling execution isolation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionPolicy {
    /// All steps share one browser/tool session (default).
    #[default]
    Shared,
    /// Each step gets a fresh session.
    Isolated,
    /// Inherit from parent orchestrator.
    Inherit,
}

/// Metadata collected during the planning pipeline.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanningMetadata {
    /// The clarified/rewritten task from elicitation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clarified_task: Option<String>,
    /// Constraints identified during planning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<String>,
    /// Objectives identified during planning.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub objectives: Vec<String>,
    /// Number of elicitation rounds completed.
    #[serde(default)]
    pub elicitation_rounds: u32,
    /// Number of slots resolved.
    #[serde(default)]
    pub slots_resolved: u32,
    /// Average confidence across resolved slots.
    #[serde(default)]
    pub slot_confidence: f32,
    /// Total upstream LLM calls during planning.
    #[serde(default)]
    pub upstream_llm_calls: u32,
}

/// Provenance / audit metadata for the generated plan.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct PlanProvenance {
    /// Strategy that produced the plan (e.g., "GuidedSearch", "AtomicComposition").
    pub strategy: String,
    /// Model or algorithm responsible for the reasoning.
    pub generator: Option<String>,
    /// Additional notes (prompt hashes, retries, etc.).
    pub notes: Option<String>,
}

impl PlanGraph {
    /// Validate the [`ResponseContract`], if present, against the steps in this graph.
    ///
    /// Checks:
    /// 1. `terminal_step`, if set, must reference an existing step ID.
    /// 2. `Action` and `SpawnTask` kinds require `terminal_step` to be `Some`.
    /// 3. `Composite` kind requires at least 2 steps in the graph.
    pub fn validate_contract(&self) -> Result<(), String> {
        let Some(ref contract) = self.response_contract else {
            return Ok(());
        };

        // Check 1: terminal_step existence.
        if let Some(ref terminal_id) = contract.terminal_step {
            let exists = self.steps.iter().any(|s| &s.id == terminal_id);
            if !exists {
                return Err(format!(
                    "ResponseContract.terminal_step '{}' does not reference a known step id",
                    terminal_id
                ));
            }
        }

        // Check 2: Action and SpawnTask require a terminal_step.
        match contract.kind {
            ResponseKind::Action | ResponseKind::SpawnTask => {
                if contract.terminal_step.is_none() {
                    return Err(format!(
                        "ResponseContract kind {:?} requires terminal_step to be set",
                        contract.kind
                    ));
                }
            },
            // Text and Silent may omit terminal_step.
            ResponseKind::Text | ResponseKind::Silent => {},
            // Check 3: Composite requires at least 2 steps.
            ResponseKind::Composite => {
                if self.steps.len() < 2 {
                    return Err(format!(
                        "ResponseContract kind Composite requires at least 2 steps, \
                         but the graph has {}",
                        self.steps.len()
                    ));
                }
            },
        }

        Ok(())
    }
}

fn compact_plan_text(value: &str) -> Option<String> {
    let collapsed = value.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Derive a concise success criterion from a [`PlanGraph`] without converting
/// the graph into legacy taskplan markdown.
pub fn derive_plan_graph_success_criteria(plan: &PlanGraph, goal: &str) -> String {
    let terminal_step = plan
        .response_contract
        .as_ref()
        .and_then(|contract| contract.terminal_step.as_deref())
        .and_then(|step_id| plan.steps.iter().find(|step| step.id == step_id));

    for step in [terminal_step, plan.steps.last(), plan.steps.first()]
        .into_iter()
        .flatten()
    {
        if let Some(success_criteria) = step.success_criteria.as_deref().and_then(compact_plan_text)
        {
            return success_criteria;
        }

        let outputs: Vec<String> = step
            .expected_outputs
            .iter()
            .filter_map(|output| compact_plan_text(output))
            .collect();
        if !outputs.is_empty() {
            return outputs.join("; ");
        }
    }

    format!("Goal achieved: {}", goal)
}

/// Render an approved/precomputed [`PlanGraph`] as runtime prompt context.
///
/// This is intentionally not taskplan markdown: execution remains agentic, and
/// the graph is advisory context the model can use, revise, or supersede based
/// on runtime observations.
pub fn render_plan_graph_runtime_context(
    goal: &str,
    success_criteria: &str,
    plan: &PlanGraph,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "## Preplanned Runtime Context");
    let _ = writeln!(
        out,
        "Use this approved PlanGraph as advisory execution context. Follow it when it matches runtime observations; adapt when the live environment proves it stale or incomplete."
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "### Goal");
    let _ = writeln!(out, "{}", goal.trim());
    let _ = writeln!(out);
    let _ = writeln!(out, "### Success Criteria");
    let _ = writeln!(out, "{}", success_criteria.trim());

    if let Some(clarified_task) = plan
        .planning_metadata
        .clarified_task
        .as_deref()
        .and_then(compact_plan_text)
    {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Clarified Task");
        let _ = writeln!(out, "{}", clarified_task);
    }

    if !plan.planning_metadata.objectives.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Objectives");
        for objective in &plan.planning_metadata.objectives {
            if let Some(objective) = compact_plan_text(objective) {
                let _ = writeln!(out, "- {}", objective);
            }
        }
    }

    if !plan.planning_metadata.constraints.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Constraints");
        for constraint in &plan.planning_metadata.constraints {
            if let Some(constraint) = compact_plan_text(constraint) {
                let _ = writeln!(out, "- {}", constraint);
            }
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "### Plan Metadata");
    let _ = writeln!(out, "- Steps: {}", plan.steps.len());
    let _ = writeln!(out, "- Confidence: {:.2}", plan.confidence);
    let _ = writeln!(out, "- Session policy: {:?}", plan.session_policy);
    if !plan.provenance.strategy.trim().is_empty() {
        let _ = writeln!(out, "- Strategy: {}", plan.provenance.strategy.trim());
    }
    if let Some(generator) = plan
        .provenance
        .generator
        .as_deref()
        .and_then(compact_plan_text)
    {
        let _ = writeln!(out, "- Generator: {}", generator);
    }
    if let Some(contract) = plan.response_contract.as_ref() {
        let _ = writeln!(
            out,
            "- Response contract: kind={:?}, terminal_step={}",
            contract.kind,
            contract.terminal_step.as_deref().unwrap_or("none")
        );
    }

    if !plan.steps.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Planned Steps");
        for (index, step) in plan.steps.iter().enumerate() {
            let label = compact_plan_text(&step.task)
                .unwrap_or_else(|| "Untitled planned step".to_string());
            let _ = writeln!(out, "{}. `{}` - {}", index + 1, step.id.trim(), label);
            if let Some(tool) = step.tool.as_deref().and_then(compact_plan_text) {
                let _ = writeln!(out, "   - Tool: {}", tool);
            }
            if let Some(provider) = step
                .providing_agent_id
                .as_deref()
                .and_then(compact_plan_text)
            {
                let _ = writeln!(out, "   - Providing agent: {}", provider);
            }
            if !step.depends_on.is_empty() {
                let _ = writeln!(out, "   - Depends on: {}", step.depends_on.join(", "));
            }
            if let Some(role) = step.role.as_ref() {
                let _ = writeln!(out, "   - Role: {:?}", role);
            }
            if let Some(readiness) = step.readiness.as_ref() {
                let _ = writeln!(out, "   - Readiness: {:?}", readiness);
            }
            if let Some(success) = step.success_criteria.as_deref().and_then(compact_plan_text) {
                let _ = writeln!(out, "   - Success: {}", success);
            }
            if !step.expected_outputs.is_empty() {
                let outputs: Vec<String> = step
                    .expected_outputs
                    .iter()
                    .filter_map(|output| compact_plan_text(output))
                    .collect();
                if !outputs.is_empty() {
                    let _ = writeln!(out, "   - Expected outputs: {}", outputs.join("; "));
                }
            }
            if !step.parameters.is_empty() {
                let rendered = serde_json::to_string(&step.parameters)
                    .unwrap_or_else(|_| "<unrenderable parameters>".to_string());
                let _ = writeln!(out, "   - Parameters: {}", rendered);
            }
            if !step.sub_steps.is_empty() {
                let labels: Vec<String> = step
                    .sub_steps
                    .iter()
                    .filter_map(|sub_step| compact_plan_text(&sub_step.label))
                    .collect();
                if !labels.is_empty() {
                    let _ = writeln!(out, "   - Known sub-steps: {}", labels.join("; "));
                }
            }
        }
    }

    if !plan.edges.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Dependencies");
        for edge in &plan.edges {
            let reason = compact_plan_text(&edge.reason).unwrap_or_else(|| "required order".into());
            let _ = writeln!(
                out,
                "- `{}` -> `{}`: {}",
                edge.from.trim(),
                edge.to.trim(),
                reason
            );
        }
    }

    if !plan.unresolved_inputs.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "### Unresolved Inputs");
        for input in &plan.unresolved_inputs {
            let prompt = compact_plan_text(&input.prompt)
                .or_else(|| compact_plan_text(&input.display_name))
                .unwrap_or_else(|| input.parameter.clone());
            let _ = writeln!(
                out,
                "- `{}` for `{}`: {} (required={}, ask_timing={:?})",
                input.id.trim(),
                input.parameter.trim(),
                prompt,
                input.required,
                input.ask_timing
            );
        }
    }

    out
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// Helper: v1.0-style JSON with only the original fields (no v1.1 extensions).
    fn v1_0_plan_graph_json() -> &'static str {
        r#"{
            "steps": [
                {
                    "id": "s1",
                    "task": "Open page",
                    "tool": "browser_navigate",
                    "parameters": {"url": "https://example.com"},
                    "expected_outputs": ["page loaded"],
                    "confidence": 0.95,
                    "metadata": {"hint": "use headless"}
                }
            ],
            "edges": [
                {"from": "s1", "to": "s2", "reason": "needs page"}
            ],
            "unresolved_inputs": [],
            "confidence": 0.9,
            "provenance": {
                "strategy": "GuidedSearch",
                "generator": "plan_builder",
                "notes": null
            }
        }"#
    }

    #[test]
    fn v1_0_plangraph_backward_compat() {
        let graph: PlanGraph =
            serde_json::from_str(v1_0_plan_graph_json()).expect("v1.0 JSON should deserialize");

        assert_eq!(graph.steps.len(), 1);
        assert_eq!(graph.steps[0].id, "s1");
        assert_eq!(graph.confidence, 0.9);
        assert_eq!(graph.provenance.strategy, "GuidedSearch");

        // v1.1 fields should have their defaults
        assert_eq!(graph.session_policy, SessionPolicy::Shared);
        assert!(graph.planning_metadata.clarified_task.is_none());
        assert!(graph.planning_metadata.constraints.is_empty());
        assert_eq!(graph.planning_metadata.elicitation_rounds, 0);

        // PlanStep v1.1 defaults
        assert!(graph.steps[0].depends_on.is_empty());
        assert!(graph.steps[0].success_criteria.is_none());
        assert!(graph.steps[0].parameter_provenance.is_empty());
    }

    #[test]
    fn v1_1_plangraph_json_roundtrip() {
        let mut prov = HashMap::new();
        prov.insert(
            "url".to_string(),
            ParameterProvenance {
                source: InputSource::UserReply,
                confidence: Some(0.99),
                slot_id: Some("slot_url".to_string()),
                method: Some("user_reply".to_string()),
            },
        );

        let graph = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "Navigate".to_string(),
                tool: Some("browser_navigate".to_string()),
                parameters: {
                    let mut m = HashMap::new();
                    m.insert("url".to_string(), serde_json::json!("https://example.com"));
                    m
                },
                expected_outputs: vec!["page loaded".to_string()],
                confidence: 0.95,
                metadata: HashMap::new(),
                timeout_override_secs: None,
                depends_on: vec!["s0".to_string()],
                success_criteria: Some("Page title contains Example".to_string()),
                parameter_provenance: prov,
                ..Default::default()
            }],
            edges: vec![PlanEdge {
                from: "s0".to_string(),
                to: "s1".to_string(),
                reason: "prerequisite".to_string(),
            }],
            unresolved_inputs: Vec::new(),
            confidence: 0.9,
            provenance: PlanProvenance {
                strategy: "TestStrategy".to_string(),
                generator: Some("test".to_string()),
                notes: None,
            },
            session_policy: SessionPolicy::Isolated,
            planning_metadata: PlanningMetadata {
                clarified_task: Some("Navigate to example".to_string()),
                constraints: vec!["must use HTTPS".to_string()],
                objectives: vec!["load page".to_string()],
                elicitation_rounds: 2,
                slots_resolved: 1,
                slot_confidence: 0.95,
                upstream_llm_calls: 3,
            },
            ..Default::default()
        };

        let json = serde_json::to_string_pretty(&graph).expect("serialize");
        let restored: PlanGraph = serde_json::from_str(&json).expect("deserialize");

        assert_eq!(restored.steps.len(), 1);
        assert_eq!(restored.steps[0].id, "s1");
        assert_eq!(restored.steps[0].depends_on, vec!["s0".to_string()]);
        assert_eq!(
            restored.steps[0].success_criteria.as_deref(),
            Some("Page title contains Example")
        );
        assert_eq!(
            restored.steps[0].parameter_provenance["url"].confidence,
            Some(0.99_f32)
        );
        assert_eq!(
            restored.steps[0].parameter_provenance["url"].source,
            InputSource::UserReply
        );
        assert_eq!(restored.session_policy, SessionPolicy::Isolated);
        assert_eq!(
            restored.planning_metadata.clarified_task.as_deref(),
            Some("Navigate to example")
        );
        assert_eq!(restored.planning_metadata.elicitation_rounds, 2);
        assert_eq!(restored.planning_metadata.upstream_llm_calls, 3);
    }

    #[test]
    fn v1_1_plangraph_yaml_roundtrip() {
        let graph = PlanGraph {
            steps: vec![PlanStep {
                id: "y1".to_string(),
                task: "yaml task".to_string(),
                depends_on: vec!["y0".to_string()],
                success_criteria: Some("done".to_string()),
                ..Default::default()
            }],
            session_policy: SessionPolicy::Inherit,
            planning_metadata: PlanningMetadata {
                clarified_task: Some("yaml test".to_string()),
                constraints: vec!["c1".to_string()],
                ..Default::default()
            },
            ..Default::default()
        };

        let yaml = serde_yaml::to_string(&graph).expect("yaml serialize");
        let restored: PlanGraph = serde_yaml::from_str(&yaml).expect("yaml deserialize");

        assert_eq!(restored.steps[0].id, "y1");
        assert_eq!(restored.steps[0].depends_on, vec!["y0".to_string()]);
        assert_eq!(restored.session_policy, SessionPolicy::Inherit);
        assert_eq!(
            restored.planning_metadata.clarified_task.as_deref(),
            Some("yaml test")
        );
    }

    #[test]
    fn session_policy_defaults_to_shared() {
        assert_eq!(SessionPolicy::default(), SessionPolicy::Shared);
    }

    #[test]
    fn planning_metadata_defaults_empty() {
        let md = PlanningMetadata::default();
        assert!(md.clarified_task.is_none());
        assert!(md.constraints.is_empty());
        assert!(md.objectives.is_empty());
        assert_eq!(md.elicitation_rounds, 0);
        assert_eq!(md.slots_resolved, 0);
        assert_eq!(md.slot_confidence, 0.0);
        assert_eq!(md.upstream_llm_calls, 0);
    }

    #[test]
    fn parameter_provenance_serde_roundtrip_full() {
        let pp = ParameterProvenance {
            source: InputSource::Discovery,
            confidence: Some(0.87),
            slot_id: Some("slot_123".to_string()),
            method: Some("inference".to_string()),
        };
        let json = serde_json::to_string(&pp).expect("serialize");
        let restored: ParameterProvenance = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.confidence, Some(0.87_f32));
        assert_eq!(restored.source, InputSource::Discovery);
        assert_eq!(restored.slot_id.as_deref(), Some("slot_123"));
        assert_eq!(restored.method.as_deref(), Some("inference"));
    }

    #[test]
    fn parameter_provenance_serde_roundtrip_minimal() {
        // Only required fields; optional fields absent in JSON.
        let json = r#"{"source":"planner","confidence":0.5}"#;
        let pp: ParameterProvenance = serde_json::from_str(json).expect("deserialize minimal");
        assert_eq!(pp.source, InputSource::Planner);
        assert_eq!(pp.confidence, Some(0.5_f32));
        assert!(pp.slot_id.is_none());
        assert!(pp.method.is_none());

        // Re-serialize and verify optionals are absent
        let reserialized = serde_json::to_string(&pp).expect("reserialize");
        assert!(!reserialized.contains("slot_id"));
        assert!(!reserialized.contains("method"));
    }

    #[test]
    fn depends_on_empty_skipped_in_serialization() {
        let step = PlanStep {
            id: "skip-test".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert!(
            !json.contains("depends_on"),
            "empty depends_on should be skipped: {}",
            json
        );
    }

    #[test]
    fn parameter_provenance_empty_skipped_in_serialization() {
        let step = PlanStep {
            id: "skip-test".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert!(
            !json.contains("parameter_provenance"),
            "empty parameter_provenance should be skipped: {}",
            json
        );
    }

    #[test]
    fn success_criteria_none_skipped_in_serialization() {
        let step = PlanStep {
            id: "skip-test".to_string(),
            ..Default::default()
        };
        let json = serde_json::to_string(&step).expect("serialize");
        assert!(
            !json.contains("success_criteria"),
            "None success_criteria should be skipped: {}",
            json
        );
    }

    #[test]
    fn existing_plan_step_construction_compiles() {
        // Regression guard: Default::default() still works.
        let step: PlanStep = Default::default();
        assert!(step.id.is_empty());
        assert!(step.depends_on.is_empty());
        assert!(step.success_criteria.is_none());
        assert!(step.parameter_provenance.is_empty());
    }

    #[test]
    fn existing_strategy_output_deserializes_unchanged() {
        let graph: PlanGraph =
            serde_json::from_str(v1_0_plan_graph_json()).expect("v1.0 JSON should deserialize");

        // Verify all original fields are intact
        assert_eq!(graph.steps[0].id, "s1");
        assert_eq!(graph.steps[0].task, "Open page");
        assert_eq!(graph.steps[0].tool.as_deref(), Some("browser_navigate"));
        assert_eq!(
            graph.steps[0]
                .parameters
                .get("url")
                .and_then(|v| v.as_str()),
            Some("https://example.com")
        );
        assert_eq!(graph.steps[0].expected_outputs, vec!["page loaded"]);
        assert_eq!(graph.steps[0].confidence, 0.95);
        assert_eq!(
            graph.steps[0].metadata.get("hint").map(|s| s.as_str()),
            Some("use headless")
        );
        assert_eq!(graph.edges[0].from, "s1");
        assert_eq!(graph.edges[0].to, "s2");
        assert_eq!(graph.confidence, 0.9);
        assert_eq!(graph.provenance.strategy, "GuidedSearch");
    }

    // ==================== v1.2 Tests ====================

    #[test]
    fn v1_2_response_contract_serde_roundtrip() {
        let variants = vec![
            ResponseKind::Action,
            ResponseKind::Text,
            ResponseKind::SpawnTask,
            ResponseKind::Silent,
            ResponseKind::Composite,
        ];
        for kind in variants {
            let contract = ResponseContract {
                kind: kind.clone(),
                terminal_step: Some("s_final".to_string()),
            };
            let json = serde_json::to_string(&contract).expect("serialize ResponseContract");
            let restored: ResponseContract =
                serde_json::from_str(&json).expect("deserialize ResponseContract");
            assert_eq!(restored.kind, kind);
            assert_eq!(restored.terminal_step.as_deref(), Some("s_final"));
        }

        // Also verify a contract with terminal_step = None roundtrips
        let contract_none = ResponseContract {
            kind: ResponseKind::Silent,
            terminal_step: None,
        };
        let json = serde_json::to_string(&contract_none).expect("serialize");
        let restored: ResponseContract = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(restored.kind, ResponseKind::Silent);
        assert!(restored.terminal_step.is_none());
    }

    #[test]
    fn v1_2_step_role_serde_roundtrip() {
        let role = StepRole::Terminal;

        // JSON roundtrip
        let json = serde_json::to_string(&role).expect("serialize StepRole to JSON");
        let restored_json: StepRole =
            serde_json::from_str(&json).expect("deserialize StepRole from JSON");
        assert_eq!(restored_json, StepRole::Terminal);
        assert_eq!(json, r#""terminal""#);

        // YAML roundtrip
        let yaml = serde_yaml::to_string(&role).expect("serialize StepRole to YAML");
        let restored_yaml: StepRole =
            serde_yaml::from_str(&yaml).expect("deserialize StepRole from YAML");
        assert_eq!(restored_yaml, StepRole::Terminal);
    }

    #[test]
    fn v1_2_backward_compat() {
        // v1.0 JSON has no response_contract or role fields
        let graph: PlanGraph = serde_json::from_str(v1_0_plan_graph_json())
            .expect("v1.0 JSON should still deserialize");

        // v1.2 fields default to None
        assert!(graph.response_contract.is_none());
        assert!(graph.steps[0].role.is_none());

        // Original fields still intact
        assert_eq!(graph.steps[0].id, "s1");
        assert_eq!(graph.confidence, 0.9);
    }

    #[test]
    fn v1_2_response_contract_skip_serializing_when_none() {
        let graph = PlanGraph {
            response_contract: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&graph).expect("serialize PlanGraph");
        assert!(
            !json.contains("response_contract"),
            "None response_contract should be omitted: {}",
            json
        );
    }

    #[test]
    fn v1_2_role_skip_serializing_when_none() {
        let step = PlanStep {
            id: "skip-test".to_string(),
            role: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&step).expect("serialize PlanStep");
        assert!(
            !json.contains("\"role\""),
            "None role should be omitted: {}",
            json
        );
    }

    #[test]
    fn v1_2_full_plangraph_yaml_roundtrip() {
        let graph = PlanGraph {
            steps: vec![
                PlanStep {
                    id: "s1".to_string(),
                    task: "fetch data".to_string(),
                    tool: Some("http_get".to_string()),
                    ..Default::default()
                },
                PlanStep {
                    id: "s2".to_string(),
                    task: "summarize".to_string(),
                    depends_on: vec!["s1".to_string()],
                    role: Some(StepRole::Terminal),
                    ..Default::default()
                },
            ],
            edges: vec![PlanEdge {
                from: "s1".to_string(),
                to: "s2".to_string(),
                reason: "needs data".to_string(),
            }],
            confidence: 0.88,
            provenance: PlanProvenance {
                strategy: "v1_2_test".to_string(),
                generator: Some("test".to_string()),
                notes: None,
            },
            response_contract: Some(ResponseContract {
                kind: ResponseKind::Text,
                terminal_step: Some("s2".to_string()),
            }),
            ..Default::default()
        };

        let yaml = serde_yaml::to_string(&graph).expect("YAML serialize");
        let restored: PlanGraph = serde_yaml::from_str(&yaml).expect("YAML deserialize");

        // Verify response_contract roundtripped
        let rc = restored
            .response_contract
            .expect("response_contract should be present");
        assert_eq!(rc.kind, ResponseKind::Text);
        assert_eq!(rc.terminal_step.as_deref(), Some("s2"));

        // Verify terminal step role roundtripped
        assert!(restored.steps[0].role.is_none());
        assert_eq!(restored.steps[1].role, Some(StepRole::Terminal));

        // Verify rest of graph
        assert_eq!(restored.steps.len(), 2);
        assert_eq!(restored.edges.len(), 1);
        assert_eq!(restored.confidence, 0.88);
        assert_eq!(restored.provenance.strategy, "v1_2_test");
    }

    // -----------------------------------------------------------------------
    // I-22: validate_contract() kind-specific invariant tests
    // -----------------------------------------------------------------------

    fn single_step_graph(kind: ResponseKind, terminal_step: Option<&str>) -> PlanGraph {
        PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "do something".to_string(),
                ..Default::default()
            }],
            response_contract: Some(ResponseContract {
                kind,
                terminal_step: terminal_step.map(|s| s.to_string()),
            }),
            ..Default::default()
        }
    }

    /// I-22: Action without terminal_step must fail validation.
    #[test]
    fn validate_contract_action_requires_terminal_step() {
        let graph = single_step_graph(ResponseKind::Action, None);
        let err = graph.validate_contract().unwrap_err();
        assert!(
            err.contains("terminal_step"),
            "error should mention terminal_step, got: {err}"
        );
    }

    /// I-22: SpawnTask without terminal_step must fail validation.
    #[test]
    fn validate_contract_spawn_task_requires_terminal_step() {
        let graph = single_step_graph(ResponseKind::SpawnTask, None);
        let err = graph.validate_contract().unwrap_err();
        assert!(
            err.contains("terminal_step"),
            "error should mention terminal_step, got: {err}"
        );
    }

    /// I-22: Text and Silent are valid without terminal_step.
    #[test]
    fn validate_contract_text_and_silent_allow_no_terminal_step() {
        let text_graph = single_step_graph(ResponseKind::Text, None);
        assert!(
            text_graph.validate_contract().is_ok(),
            "Text without terminal_step should be valid"
        );

        let silent_graph = single_step_graph(ResponseKind::Silent, None);
        assert!(
            silent_graph.validate_contract().is_ok(),
            "Silent without terminal_step should be valid"
        );
    }

    /// I-22: Composite with fewer than 2 steps must fail validation.
    #[test]
    fn validate_contract_composite_requires_at_least_two_steps() {
        // One step — should fail.
        let one_step = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "do something".to_string(),
                ..Default::default()
            }],
            response_contract: Some(ResponseContract {
                kind: ResponseKind::Composite,
                terminal_step: None,
            }),
            ..Default::default()
        };
        let err = one_step.validate_contract().unwrap_err();
        assert!(
            err.contains("Composite"),
            "error should mention Composite, got: {err}"
        );

        // Two steps — should pass.
        let two_steps = PlanGraph {
            steps: vec![
                PlanStep {
                    id: "s1".to_string(),
                    task: "a".to_string(),
                    ..Default::default()
                },
                PlanStep {
                    id: "s2".to_string(),
                    task: "b".to_string(),
                    ..Default::default()
                },
            ],
            response_contract: Some(ResponseContract {
                kind: ResponseKind::Composite,
                terminal_step: None,
            }),
            ..Default::default()
        };
        assert!(
            two_steps.validate_contract().is_ok(),
            "Composite with 2 steps should be valid"
        );
    }

    // -----------------------------------------------------------------------
    // v1.3: PlanStep hash tests
    // -----------------------------------------------------------------------

    #[test]
    fn step_hash_compute_is_deterministic() {
        let h1 = PlanStep::compute_hash("agent-a", "browser_navigate");
        let h2 = PlanStep::compute_hash("agent-a", "browser_navigate");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16, "hash should be 16 hex characters");
    }

    #[test]
    fn step_hash_differs_by_agent() {
        let h1 = PlanStep::compute_hash("agent-a", "shell");
        let h2 = PlanStep::compute_hash("agent-b", "shell");
        assert_ne!(
            h1, h2,
            "different agents with same tool should produce different hashes"
        );
    }

    #[test]
    fn step_hash_differs_by_tool() {
        let h1 = PlanStep::compute_hash("agent-a", "shell");
        let h2 = PlanStep::compute_hash("agent-a", "browser");
        assert_ne!(
            h1, h2,
            "same agent with different tools should produce different hashes"
        );
    }

    #[test]
    fn step_hash_valid_check() {
        let hash = PlanStep::compute_hash("agent-a", "shell");
        let step = PlanStep {
            id: "s1".to_string(),
            step_hash: Some(hash.clone()),
            providing_agent_id: Some("agent-a".to_string()),
            ..Default::default()
        };
        assert!(step.is_hash_valid("agent-a", "shell"));
        assert!(!step.is_hash_valid("agent-b", "shell"));
        assert!(!step.is_hash_valid("agent-a", "browser"));
    }

    #[test]
    fn step_hash_none_is_always_valid() {
        let step = PlanStep {
            id: "legacy".to_string(),
            step_hash: None,
            ..Default::default()
        };
        assert!(step.is_hash_valid("any-agent", "any-tool"));
    }

    #[test]
    fn v1_3_backward_compat() {
        // v1.0 JSON has no step_hash or providing_agent_id
        let graph: PlanGraph = serde_json::from_str(v1_0_plan_graph_json())
            .expect("v1.0 JSON should still deserialize");
        assert!(graph.steps[0].step_hash.is_none());
        assert!(graph.steps[0].providing_agent_id.is_none());
    }

    #[test]
    fn v1_3_step_hash_skip_serializing_when_none() {
        let step = PlanStep {
            id: "skip-test".to_string(),
            step_hash: None,
            providing_agent_id: None,
            ..Default::default()
        };
        let json = serde_json::to_string(&step).expect("serialize PlanStep");
        assert!(
            !json.contains("step_hash"),
            "None step_hash should be omitted: {}",
            json
        );
        assert!(
            !json.contains("providing_agent_id"),
            "None providing_agent_id should be omitted: {}",
            json
        );
    }

    // -----------------------------------------------------------------------
    // v1.3: compute_step_hash() standalone function tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_compute_step_hash_deterministic() {
        let h1 = compute_step_hash("agent-a", "tool-1");
        let h2 = compute_step_hash("agent-a", "tool-1");
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 16);
        // Different inputs produce different hashes
        let h3 = compute_step_hash("agent-b", "tool-1");
        assert_ne!(h1, h3);
    }

    #[test]
    fn compute_step_hash_matches_plan_step_compute_hash() {
        // The standalone function must produce the same output as the
        // associated method so they can be used interchangeably.
        let standalone = compute_step_hash("agent-x", "browser_navigate");
        let method = PlanStep::compute_hash("agent-x", "browser_navigate");
        assert_eq!(standalone, method);
    }

    // -----------------------------------------------------------------------
    // Phase C: classify_readiness tests
    // -----------------------------------------------------------------------

    #[test]
    fn classify_readiness_ready() {
        let mut graph = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "navigate".to_string(),
                tool: Some("browser_navigate".to_string()),
                confidence: 0.9,
                ..Default::default()
            }],
            ..Default::default()
        };
        classify_readiness(&mut graph);
        assert_eq!(graph.steps[0].readiness, Some(StepReadiness::Ready));
    }

    #[test]
    fn classify_readiness_jit() {
        let mut meta = HashMap::new();
        meta.insert("runtime_dependent".to_string(), "true".to_string());
        let mut graph = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "scrape page".to_string(),
                tool: Some("browser_extract".to_string()),
                confidence: 0.95,
                metadata: meta,
                ..Default::default()
            }],
            ..Default::default()
        };
        classify_readiness(&mut graph);
        assert_eq!(graph.steps[0].readiness, Some(StepReadiness::JustInTime));
    }

    #[test]
    fn classify_readiness_weak() {
        let mut graph = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "do something vague".to_string(),
                tool: None,
                confidence: 0.3,
                ..Default::default()
            }],
            ..Default::default()
        };
        classify_readiness(&mut graph);
        assert_eq!(graph.steps[0].readiness, Some(StepReadiness::Weak));
    }

    #[test]
    fn classify_readiness_low_confidence_with_tool_is_weak() {
        // Tool is set but confidence is below 0.8 → Weak
        let mut graph = PlanGraph {
            steps: vec![PlanStep {
                id: "s1".to_string(),
                task: "maybe navigate".to_string(),
                tool: Some("browser_navigate".to_string()),
                confidence: 0.5,
                ..Default::default()
            }],
            ..Default::default()
        };
        classify_readiness(&mut graph);
        assert_eq!(graph.steps[0].readiness, Some(StepReadiness::Weak));
    }

    #[test]
    fn classify_readiness_mixed_steps() {
        let mut meta = HashMap::new();
        meta.insert("runtime_dependent".to_string(), "true".to_string());
        let mut graph = PlanGraph {
            steps: vec![
                PlanStep {
                    id: "ready".to_string(),
                    task: "nav".to_string(),
                    tool: Some("browser_navigate".to_string()),
                    confidence: 0.9,
                    ..Default::default()
                },
                PlanStep {
                    id: "jit".to_string(),
                    task: "scrape".to_string(),
                    metadata: meta,
                    confidence: 0.9,
                    tool: Some("extract".to_string()),
                    ..Default::default()
                },
                PlanStep {
                    id: "weak".to_string(),
                    task: "unclear".to_string(),
                    confidence: 0.2,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        classify_readiness(&mut graph);
        assert_eq!(graph.steps[0].readiness, Some(StepReadiness::Ready));
        assert_eq!(graph.steps[1].readiness, Some(StepReadiness::JustInTime));
        assert_eq!(graph.steps[2].readiness, Some(StepReadiness::Weak));
    }
}

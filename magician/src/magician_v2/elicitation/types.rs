//! Types for progressive elicitation system
//!
//! This module defines the types used by the sensible progressive elicitation system.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

// Re-export from existing modules for convenience
pub use crate::magician_v2::state_tracker::types::ObservationAsset;
pub use crate::magician_v2::tool_matcher::types::ToolMetadata;

// ============================================================================
// NEW TYPES FOR PROGRESSIVE ELICITATION
// ============================================================================

/// Decision output from sensible elicitation orchestrator
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ElicitationDecision {
    /// ID of the unresolved input
    pub input_id: String,

    /// Whether to ask the user
    pub should_ask: bool,

    /// Whether to attempt autonomous discovery
    pub should_discover: bool,

    /// Inferred value (if inference succeeded)
    pub inferred_value: Option<Value>,

    /// Confidence in the decision (0.0 - 1.0)
    pub confidence: f64,

    /// Batch key for grouping similar questions
    pub batch_key: Option<String>,

    /// Reasoning for the decision (for debugging)
    pub reasoning: String,
}

/// Context for parameter elicitation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterContext {
    /// Execution ID
    pub execution_id: String,

    /// User's message that triggered this
    pub user_message: String,

    /// Tool being executed (if known)
    pub tool_context: Option<ToolMetadata>,

    /// Already resolved slots/parameters
    pub slot_context: HashMap<String, Value>,

    /// Recent observations
    pub observations: Vec<ObservationAsset>,

    /// Workflow stage (planning, execution, etc.)
    pub stage: WorkflowStage,

    /// Optional prompt identity for safe personality injection in planning-stage prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_identity: Option<crate::magician_v2::execution::PromptIdentityContext>,
}

/// Result from parameter inference
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceResult {
    /// Inferred value
    pub value: Option<Value>,

    /// Confidence in inference (0.0 - 1.0)
    pub confidence: f64,

    /// Method used for inference
    pub method: InferenceMethod,

    /// Explanation of how inference was performed
    pub explanation: String,

    /// Sources used for inference
    pub sources: Vec<InferenceSource>,
}

/// Method used for inference
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum InferenceMethod {
    /// LLM-based inference
    LLMBased,

    /// Rule-based inference from context
    RuleBased,

    /// Inference from user history
    Historical,

    /// Default value
    Default,

    /// Auto-fill from hints
    AutoFill,
}

/// Source of inferred value
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InferenceSource {
    /// Type of source
    pub source_type: String,

    /// Weight/confidence contribution
    pub weight: f64,

    /// Description
    pub description: String,
}

/// Result from autonomous discovery
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscoveryResult {
    /// Discovered value
    pub value: Option<Value>,

    /// Confidence in discovery (0.0 - 1.0)
    pub confidence: f64,

    /// Discovery method used
    pub method: DiscoveryMethod,

    /// Explanation of discovery
    pub explanation: String,

    /// Whether discovery required external actions
    pub external_actions_performed: bool,
}

/// Method used for discovery
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum DiscoveryMethod {
    /// Web search
    WebSearch,

    /// Filesystem search
    FilesystemSearch,

    /// API query
    APIQuery,

    /// Tool execution
    ToolExecution,

    /// Context analysis
    ContextAnalysis,
}

/// Feedback for refinement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum RefinementFeedback {
    /// Value confirmed by user
    Confirmed(Value),

    /// Value rejected by user
    Rejected,

    /// User requested more context
    RequestedContext,

    /// User provided alternative value
    Alternative(Value),
}

/// Workflow stage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowStage {
    Planning,
    PreExecution,
    Execution,
    PostExecution,
}

// ============================================================================
// PLANNING CONTEXT (Phase 1.2 Progressive Orchestrator)
// ============================================================================

/// Context tracking during progressive plan generation
///
/// This context is maintained during the progressive step-by-step planning process.
/// It tracks all resolved parameters to enable deduplication and value reuse across steps.
#[derive(Debug, Clone)]
pub struct PlanningContext {
    /// Resolved parameters (parameter_name -> resolved value + metadata)
    resolved_params: HashMap<String, ResolvedParameter>,

    /// Inference results for tracking
    inference_results: Vec<InferenceResult>,

    /// Discovery results for tracking
    discovery_results: Vec<DiscoveryResult>,

    /// User-provided answers for tracking
    user_answers: HashMap<String, UserAnswer>,
}

impl PlanningContext {
    /// Create a new empty planning context
    pub fn new() -> Self {
        Self {
            resolved_params: HashMap::new(),
            inference_results: Vec::new(),
            discovery_results: Vec::new(),
            user_answers: HashMap::new(),
        }
    }

    /// Check if a parameter has been resolved
    ///
    /// O(1) HashMap lookup for efficient deduplication
    pub fn has_value(&self, param_name: &str) -> bool {
        self.resolved_params.contains_key(param_name)
    }

    /// Get a resolved parameter value
    pub fn get_value(&self, param_name: &str) -> Option<&Value> {
        self.resolved_params.get(param_name).map(|r| &r.value)
    }

    /// Get the full resolved parameter metadata
    pub fn get_resolved(&self, param_name: &str) -> Option<&ResolvedParameter> {
        self.resolved_params.get(param_name)
    }

    /// Set a resolved parameter
    pub fn set_resolved(&mut self, param_name: String, resolved: ResolvedParameter) {
        self.resolved_params.insert(param_name, resolved);
    }

    /// Add an inference result for tracking
    pub fn add_inference_result(&mut self, result: InferenceResult) {
        self.inference_results.push(result);
    }

    /// Add a discovery result for tracking
    pub fn add_discovery_result(&mut self, result: DiscoveryResult) {
        self.discovery_results.push(result);
    }

    /// Add a user answer for tracking
    pub fn add_user_answer(&mut self, param_name: String, answer: UserAnswer) {
        self.user_answers.insert(param_name, answer);
    }

    /// Get all resolved parameter names
    pub fn resolved_param_names(&self) -> Vec<String> {
        self.resolved_params.keys().cloned().collect()
    }

    /// Get all resolved parameters
    pub fn resolved_params(&self) -> &HashMap<String, ResolvedParameter> {
        &self.resolved_params
    }

    /// Get inference results
    pub fn inference_results(&self) -> &[InferenceResult] {
        &self.inference_results
    }

    /// Get discovery results
    pub fn discovery_results(&self) -> &[DiscoveryResult] {
        &self.discovery_results
    }

    /// Get user answers
    pub fn user_answers(&self) -> &HashMap<String, UserAnswer> {
        &self.user_answers
    }
}

impl Default for PlanningContext {
    fn default() -> Self {
        Self::new()
    }
}

/// A resolved parameter with its value and metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedParameter {
    /// The parameter name
    pub parameter_name: String,

    /// The resolved value
    pub value: Value,

    /// Source of the resolution
    pub source: ResolutionSource,

    /// Confidence in the resolution (0.0 - 1.0)
    pub confidence: f64,

    /// Timestamp when resolved
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Whether this is a deferred (JIT) parameter
    pub deferred: bool,
}

impl ResolvedParameter {
    /// Create from an inference result
    pub fn from_inference(param_name: String, result: InferenceResult) -> Self {
        Self {
            parameter_name: param_name,
            value: result.value.unwrap_or(Value::Null),
            source: ResolutionSource::Inference(result.method),
            confidence: result.confidence,
            resolved_at: Some(chrono::Utc::now()),
            deferred: false,
        }
    }

    /// Create from a discovery result
    pub fn from_discovery(param_name: String, result: DiscoveryResult) -> Self {
        Self {
            parameter_name: param_name,
            value: result.value.unwrap_or(Value::Null),
            source: ResolutionSource::Discovery(result.method),
            confidence: result.confidence,
            resolved_at: Some(chrono::Utc::now()),
            deferred: false,
        }
    }

    /// Create from a user answer
    pub fn from_user(param_name: String, answer: UserAnswer) -> Self {
        Self {
            parameter_name: param_name.clone(),
            value: answer.value.clone(),
            source: ResolutionSource::User,
            confidence: 1.0, // User answers have 100% confidence
            resolved_at: Some(chrono::Utc::now()),
            deferred: false,
        }
    }

    /// Create a deferred (JIT) parameter
    pub fn deferred(param_name: String) -> Self {
        Self {
            parameter_name: param_name,
            value: Value::Null,
            source: ResolutionSource::Deferred,
            confidence: 0.0,
            resolved_at: None,
            deferred: true,
        }
    }
}

/// Source of parameter resolution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ResolutionSource {
    /// Resolved via inference
    Inference(InferenceMethod),

    /// Resolved via discovery
    Discovery(DiscoveryMethod),

    /// Provided by user
    User,

    /// Deferred to execution (JIT)
    Deferred,

    /// From default value
    Default,
}

/// User-provided answer to a question
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserAnswer {
    /// The parameter name being answered
    pub parameter_name: String,

    /// The value provided by user
    pub value: Value,

    /// When the answer was provided
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answered_at: Option<chrono::DateTime<chrono::Utc>>,

    /// Source of the answer (UI, API, voice, etc.)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub answer_source: Option<String>,
}

// ============================================================================
// PHASE 3: QUESTION TIMING (Confidence Thresholds & Classification)
// ============================================================================

/// Confidence thresholds for auto-filling inferred values
///
/// These thresholds represent risk tolerance for automated decisions based on parameter priority.
/// Similar to database pool sizes or retry intervals, these are tunable system parameters.
pub mod confidence_thresholds {
    /// Critical parameters: High bar for destructive actions
    /// Only auto-fill if inference has very high confidence
    pub const CRITICAL: f64 = 0.9;

    /// PreExecution parameters: Moderate bar for parameters affecting plan structure
    /// Auto-fill with reasonable confidence
    pub const PRE_EXECUTION: f64 = 0.7;

    /// JustInTime parameters: Lower bar for contextual parameters
    /// Can auto-fill with moderate confidence since they're asked during execution if wrong
    pub const JUST_IN_TIME: f64 = 0.6;

    /// Optional parameters: Lowest bar for parameters with fallback to asking
    /// Liberal auto-fill since user can always override
    pub const OPTIONAL: f64 = 0.5;
}

/// Classified inputs grouped by priority level for question timing
#[derive(Debug, Default, Clone)]
pub struct ClassifiedInputs {
    /// Critical priority inputs (must ask upfront)
    pub critical: Vec<crate::magician_v2::strategy::plan::UnresolvedInput>,

    /// PreExecution priority inputs (ask upfront before any execution)
    pub pre_execution: Vec<crate::magician_v2::strategy::plan::UnresolvedInput>,

    /// JustInTime priority inputs (defer to execution)
    pub just_in_time: Vec<crate::magician_v2::strategy::plan::UnresolvedInput>,

    /// Optional priority inputs (try inference, ask if fails)
    pub optional: Vec<crate::magician_v2::strategy::plan::UnresolvedInput>,

    /// Inferrable priority inputs (try inference, ask if fails)
    pub inferrable: Vec<crate::magician_v2::strategy::plan::UnresolvedInput>,
}

/// Classify unresolved inputs by priority level for optimal question timing
///
/// Pure programmatic enum matching to group inputs - no semantic reasoning needed.
/// This is an O(n) single-pass operation for efficient batching.
pub fn classify_inputs(
    inputs: &[crate::magician_v2::strategy::plan::UnresolvedInput],
) -> ClassifiedInputs {
    use crate::magician_v2::strategy::plan::QuestionPriority;

    let mut result = ClassifiedInputs::default();

    for input in inputs {
        match input.priority {
            QuestionPriority::Critical => result.critical.push(input.clone()),
            QuestionPriority::PreExecution => result.pre_execution.push(input.clone()),
            QuestionPriority::JustInTime => result.just_in_time.push(input.clone()),
            QuestionPriority::Optional => result.optional.push(input.clone()),
            QuestionPriority::Inferrable => result.inferrable.push(input.clone()),
        }
    }

    result
}

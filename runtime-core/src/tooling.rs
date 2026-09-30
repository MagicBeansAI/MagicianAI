use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Basic tool information for category-aware filtering
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub category: String,
    /// Multiple categories for fuzzy matching (from ToolDefinition.categories)
    #[serde(default)]
    pub categories: Vec<String>,
    pub parameters: Vec<ParameterDefinition>,
    pub enhanced_description: Option<String>,
    pub keywords: Vec<String>,
    pub use_cases: Vec<String>,
    /// Composition category for grouping atomic tools (e.g.,
    /// "browser_automation", "shell_operations", "database")
    #[serde(default)]
    pub composition_category: Option<String>,

    /// The agent that provides/owns this tool. `None` = the task's own agent (self).
    /// Populated when tools are sourced from delegate agents in a multi-agent topology.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providing_agent_id: Option<String>,
}

impl ToolInfo {
    /// Return a clone of this tool with `providing_agent_id` set.
    ///
    /// Used when merging delegate agent catalogs so each tool is tagged with the
    /// delegate that owns it.
    pub fn with_providing_agent(mut self, agent_id: String) -> Self {
        self.providing_agent_id = Some(agent_id);
        self
    }
}

/// Result of attempting to find a tool match for a task
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatchResult {
    pub primary_match: Option<ToolMatch>,
    pub match_confidence: f32,
    pub missing_capabilities: Vec<String>,
    pub parameter_coverage: f32,
    pub executable: bool,
}

impl Default for ToolMatchResult {
    fn default() -> Self {
        Self {
            primary_match: None,
            match_confidence: 0.0,
            missing_capabilities: vec![],
            parameter_coverage: 0.0,
            executable: false,
        }
    }
}

/// Result of finding multiple viable tool matches for multi-path planning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MultipleToolMatchResult {
    /// Multiple viable tool options ranked by confidence
    pub matches: Vec<ToolMatch>,
    /// Different strategic approaches to achieve the task
    pub match_strategies: Vec<String>,
    /// Range between highest and lowest confidence (indicates certainty)
    pub confidence_spread: f32,
    /// Index of recommended approach in matches vec
    pub recommended_approach: Option<usize>,
    /// Overall executability - true if any match is executable
    pub any_executable: bool,
    /// Aggregated missing capabilities across all approaches
    pub aggregate_missing_capabilities: Vec<String>,
}

impl Default for MultipleToolMatchResult {
    fn default() -> Self {
        Self {
            matches: vec![],
            match_strategies: vec![],
            confidence_spread: 0.0,
            recommended_approach: None,
            any_executable: false,
            aggregate_missing_capabilities: vec![],
        }
    }
}

/// Individual tool match with confidence and parameter mapping
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatch {
    pub tool_name: String,
    pub capability_match: f32,
    pub parameter_mapping: HashMap<String, ParameterMapping>,
    pub execution_confidence: f32,
    pub tool_metadata: ToolMetadata,
}

/// Tool metadata for enhanced matching and execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub category: String,
    pub typical_use_cases: Vec<String>,
    pub input_schema: serde_json::Value,
    pub output_schema: serde_json::Value,
    pub success_rate: f32,
    pub avg_execution_time: f32,
    pub enhanced_description: Option<String>,
    pub keywords: Vec<String>,
    pub use_cases: Vec<String>,
    pub confidence_score: f32,
    pub success_metrics: SuccessMetrics,
    pub last_updated: i64,
}

/// Parameter mapping for tool execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterMapping {
    pub required_param: String,
    pub source_value: ParameterSource,
    pub transformation: Option<String>,
    pub validation: Option<String>,
}

/// Sources for parameter values
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParameterSource {
    UserInput {
        slot_name: String,
    },
    PreviousStepOutput {
        step_id: String,
        output_name: String,
    },
    StaticValue {
        value: serde_json::Value,
    },
    Derived {
        expression: String,
    },
    Environment {
        variable: String,
    },
    LLMExtracted {
        confidence: f32,
    },
}

/// Parameter values for LLM-enhanced parameter extraction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParameterValue {
    String(String),
    Integer(i64),
    Number(f64),
    Boolean(bool),
    Array(Vec<String>),
}

/// Parameter definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ParameterDefinition {
    pub name: String,
    pub param_type: String,
    pub required: bool,
    pub description: String,
    pub validation_rules: Vec<String>,
    pub default_value: Option<serde_json::Value>,
    /// Allowed values for enum-constrained parameters (e.g., browser action names).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enum_values: Option<Vec<String>>,
    /// Canonical JSON Schema for this parameter's accepted value. Always
    /// populated by upstream pack-loading code (or its
    /// `#[serde(default)]` produces `{"type": "string"}` for legacy
    /// serialized data without a schema field). Catalog generators emit
    /// this verbatim to the LLM. The other fields on this struct
    /// (`param_type`, `enum_values`, etc.) are coarse views retained for
    /// runtime coercion / matching, but the schema is the source of truth
    /// for what the LLM sees.
    #[serde(default = "default_runtime_param_schema")]
    pub schema: serde_json::Value,
}

fn default_runtime_param_schema() -> serde_json::Value {
    serde_json::json!({"type": "string"})
}

/// Success metrics for tools
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessMetrics {
    pub success_rate: f32,
    pub avg_execution_time: f32,
    pub reliability_score: f32,
    pub last_updated: i64,
}

/// Result item from semantic search.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticMatch {
    pub tool_name: String,
    pub similarity_score: f64,
    pub enabled: bool,
    pub hidden: bool,
}

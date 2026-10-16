//! Core types for V2 Tool Matcher.
//!
//! These types are shared by the compact router + optional LLM disambiguation
//! matcher pipeline.

use serde::{Deserialize, Serialize};

use runtime_core::{ExecutionContext, ToolInfo};

/// Request for tool matching with optional route hints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatchRequest {
    /// Task description to match against
    pub task: String,

    /// Suggested categories from decomposition/query analysis.
    /// Used as optional hints during route inference.
    pub suggested_categories: Vec<String>,

    /// Execution context for security filtering
    pub execution_context: ExecutionContext,

    /// Minimum confidence threshold for selection
    pub confidence_threshold: f32,

    /// Execution ID for progress tracking (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,

    /// Correlation ID for progress tracking (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,

    /// Required capabilities for completeness checking (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_capabilities:
        Option<Vec<crate::magician_v2::query_analysis::unified_analyzer::RequiredCapability>>,

    /// Parent task description for hierarchical context (optional)
    /// Used to provide context for child task tool matching
    /// Example: parent="Perform Google Search", child="Define Search Query"
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_task: Option<String>,

    /// Parent's matched tool name for context (optional)
    /// Helps disambiguate similar keywords in different domains
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_tool_name: Option<String>,
}

/// Result of tool matching with detailed metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatchResult {
    /// Selected tool name
    pub tool_name: String,

    /// Final confidence score (0.0-1.0).
    /// Produced from router score plus optional LLM disambiguation.
    pub confidence: f32,

    /// Which tier provided the final match
    pub matching_tier: MatchingTier,

    /// Breakdown of scores from each tier
    pub tier_scores: TierScores,

    /// Tool metadata for execution
    pub tool_metadata: ToolMetadata,

    /// Category matches (tools that matched suggested categories)
    pub category_matched: bool,

    /// Total time spent on matching (ms)
    pub match_time_ms: u64,

    /// The agent that provides/owns this tool. `None` = the task's own agent.
    /// Propagated from `ToolInfo::providing_agent_id` for delegation routing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providing_agent_id: Option<String>,
}

/// Breakdown of scores from each stage of matching.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierScores {
    /// Lexical/router-local score component.
    pub rule_score: f32,

    /// Semantic similarity score component.
    pub semantic_score: f32,

    /// LLM disambiguation score component.
    pub llm_score: f32,

    /// Combined non-LLM score before final selection.
    pub combined_pre_llm: f32,
}

/// Which stage provided the final tool match.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum MatchingTier {
    /// Legacy: category filtering stage.
    CategoryFilter,

    /// Rule/router-local stage.
    RuleBased,

    /// Semantic stage.
    Semantic,

    /// Candidate selection stage.
    CandidateSelection,

    /// LLM evaluation/disambiguation stage.
    LlmEvaluation,
}

/// Tool metadata for execution and context
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMetadata {
    pub name: String,
    pub description: String,
    pub category: String,
    pub parameters: Vec<ToolParameter>,
    pub enhanced_description: Option<String>,
    pub keywords: Vec<String>,
    pub use_cases: Vec<String>,
}

/// Parameter information for tool execution (V2 format)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolParameter {
    pub name: String,
    pub param_type: String,
    pub required: bool,
    pub description: String,
}

/// Alias for backward compatibility
pub type ParameterInfo = ToolParameter;

/// Intermediate tool candidate during matching process
#[derive(Debug, Clone)]
pub struct ToolCandidate {
    /// Tool name
    pub tool_name: String,

    /// Tool category
    pub category: String,

    /// Rule-based score (Tier 1)
    pub rule_score: f32,

    /// Semantic similarity score (Tier 2)
    pub semantic_score: f32,

    /// LLM evaluation score (Tier 4)
    pub llm_score: f32,

    /// Combined score (weighted sum of rule + semantic)
    pub combined_score: f32,

    /// Final score after dynamic confidence weighting (includes LLM)
    /// Uses a squared-weight algorithm where high confidence dominates
    pub final_score: f32,

    /// Reasoning for the final score calculation
    pub score_reasoning: String,

    /// Whether tool matches any suggested category
    pub category_matched: bool,

    /// Full tool metadata
    pub tool_info: ToolInfo,
}

impl ToolCandidate {
    /// Create a new tool candidate
    pub fn new(tool_info: ToolInfo, category_matched: bool) -> Self {
        Self {
            tool_name: tool_info.name.clone(),
            category: tool_info.category.clone(),
            rule_score: 0.0,
            semantic_score: 0.0,
            llm_score: 0.0,
            combined_score: 0.0,
            final_score: 0.0,
            score_reasoning: String::new(),
            category_matched,
            tool_info,
        }
    }

    /// Update combined score based on tier weights
    /// Rule: 15%, Semantic: 30% = 45% total before LLM
    pub fn update_combined_score(&mut self, rule_weight: f32, semantic_weight: f32) {
        self.combined_score =
            (self.rule_score * rule_weight) + (self.semantic_score * semantic_weight);

        // Apply category bonus if matched
        if self.category_matched {
            // Small boost for category matches (already applied in individual
            // tier scores) This is just to ensure the combined
            // score reflects the boost
        }
    }
}

/// Error types for tool matching failures
#[derive(Debug, thiserror::Error)]
pub enum ToolMatchError {
    #[error("Empty categories provided for filtering")]
    EmptyCategories,

    #[error("No tools match the requested categories: {categories:?}")]
    NoMatchingCategories { categories: Vec<String> },

    #[error("Insufficient tools after filtering: {count} tools, need at least {required}")]
    InsufficientTools { count: usize, required: usize },

    #[error("Security filtering failed: {0}")]
    SecurityFilteringFailed(String),

    #[error("LLM evaluation failed: {0}")]
    LlmEvaluationFailed(String),

    #[error("Candidate selection failed: {0}")]
    CandidateSelectionFailed(String),

    #[error("No tools exceed confidence threshold: {threshold} (best: {best_confidence:.3})")]
    NoConfidentMatches {
        threshold: f32,
        best_confidence: f32,
    },

    #[error("Tool discovery error: {0}")]
    ToolDiscoveryError(String),
}

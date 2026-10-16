use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;

use crate::{ExecutionContext, MultipleToolMatchResult, SemanticMatch, ToolInfo, ToolMatchResult};

/// Deterministic, low-risk tools in this category are visible to every agent
/// unless the agent explicitly excludes the category or tool name.
pub const CORE_UTILITY_CATEGORY: &str = "core_utility";

/// Catalog queries for security-filtered tool listings and metadata.
#[async_trait]
pub trait ToolCatalog: Send + Sync {
    /// Return the names of all tools accessible in the given context.
    async fn list_tool_names(&self, context: &ExecutionContext) -> Vec<String>;

    /// Return available categories for decomposition planning.
    async fn available_categories(&self, context: &ExecutionContext) -> Vec<String>;

    /// Fetch detailed tool metadata for planning purposes.
    async fn get_tool_metadata(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, Value>>;

    /// Fetch tools filtered by specific categories (OR semantics).
    async fn filtered_tools_by_categories(
        &self,
        categories: &[String],
        context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>>;

    /// Return all security-filtered tools for the given context.
    async fn all_tools(&self, context: &ExecutionContext) -> Result<Vec<ToolInfo>>;

    /// Return tools filtered by agent tool whitelist/blacklist.
    ///
    /// When `tools` is empty, no whitelist restriction is applied
    /// (all visible tools pass). When non-empty, only tools whose `name` or
    /// `categories` intersect the whitelist are included. Tools in
    /// [`CORE_UTILITY_CATEGORY`] are also included unless explicitly excluded.
    /// Tools whose `name` or `categories` intersect `excluded_tools` are always
    /// removed.
    ///
    /// The default implementation delegates to [`all_tools`] and applies
    /// name + category-based filtering. Implementations may override for efficiency.
    async fn agent_filtered_tools(
        &self,
        tools: &[String],
        excluded_tools: &[String],
        context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        let all = self.all_tools(context).await?;
        if tools.is_empty() && excluded_tools.is_empty() {
            return Ok(all);
        }
        Ok(all
            .into_iter()
            .filter(|tool| {
                let in_allowed = tools.is_empty()
                    || tools.contains(&tool.name)
                    || tool.categories.iter().any(|cat| tools.contains(cat))
                    || tool
                        .categories
                        .iter()
                        .any(|cat| cat == CORE_UTILITY_CATEGORY);
                let not_excluded = !excluded_tools.contains(&tool.name)
                    && !tool
                        .categories
                        .iter()
                        .any(|cat| excluded_tools.contains(cat));
                in_allowed && not_excluded
            })
            .collect())
    }

    /// Return the distribution of tools across categories.
    async fn category_tool_counts(
        &self,
        context: &ExecutionContext,
    ) -> Result<HashMap<String, usize>>;
}

/// Matching interfaces for selecting tools based on task descriptions.
#[async_trait]
pub trait ToolMatching: Send + Sync {
    /// Find the best tool match for a task.
    async fn best_match(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> ToolMatchResult;

    /// Find multiple viable tool matches for multi-path planning.
    async fn multiple_matches(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> MultipleToolMatchResult;

    /// Check if a tool is accessible in the given context.
    async fn is_tool_available(&self, tool_name: &str, context: &ExecutionContext) -> bool;

    /// Validate that a tool can execute with the provided parameters.
    async fn validate_tool_execution(
        &self,
        tool_name: &str,
        parameters: &Value,
        context: &ExecutionContext,
    ) -> bool;

    /// Estimate relative execution cost for ranking/planning.
    async fn estimate_execution_cost(
        &self,
        tool_name: &str,
        parameters: &Value,
        context: &ExecutionContext,
    ) -> f64;
}

/// Convenience super-trait bundling catalog + matching behaviors.
pub trait ToolServices: ToolCatalog + ToolMatching {}

impl<T> ToolServices for T where T: ToolCatalog + ToolMatching {}

/// Similarity search abstraction (lexical or semantic backend).
#[async_trait]
pub trait SemanticSearch: Send + Sync {
    /// Return ranked similarity scores for tools.
    /// Implementations may apply thresholding and should truncate to configured
    /// backend limits (for example `semantic_search.max_results`).
    async fn search_all_tools_with_scores(&self, query: &str) -> Result<Vec<SemanticMatch>>;

    /// Return top matching categories with similarity scores.
    async fn search_categories(&self, category: &str, top_k: usize) -> Result<Vec<(String, f32)>>;
}

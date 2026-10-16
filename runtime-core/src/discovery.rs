use std::collections::HashMap;

use anyhow::Result;
use serde_json::Value;

use crate::{
    prelude::async_trait, ExecutionContext, MultipleToolMatchResult, ToolInfo, ToolMatchResult,
};

/// Abstract interface for tool discovery systems.
#[async_trait]
pub trait ToolDiscovery: Send + Sync {
    /// Find the best tool match with security context for proper filtering.
    async fn find_best_match_with_context(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> ToolMatchResult;

    /// Find multiple viable tool matches for multi-path planning.
    async fn find_multiple_matches_with_context(
        &self,
        task_description: &str,
        context: &ExecutionContext,
    ) -> MultipleToolMatchResult;

    /// Check if a specific tool is available and accessible in the given context.
    async fn is_tool_available(&self, tool_name: &str, context: &ExecutionContext) -> bool;

    /// Get detailed tool metadata for planning purposes.
    async fn get_tool_metadata(
        &self,
        tool_name: &str,
        context: &ExecutionContext,
    ) -> Option<HashMap<String, Value>>;

    /// Get list of all available tools in the given context.
    async fn get_available_tools(&self, context: &ExecutionContext) -> Vec<String>;

    /// Get all available tool categories for decomposition planning.
    /// Returns a sorted vector of unique category strings filtered by user context.
    async fn get_available_categories(&self, _: &ExecutionContext) -> Vec<String> {
        vec![]
    }

    /// Get tools filtered by specific categories with security context.
    async fn get_filtered_tools_by_categories(
        &self,
        _categories: &[String],
        _context: &ExecutionContext,
    ) -> Result<Vec<ToolInfo>> {
        Ok(vec![])
    }

    /// Get all tools with security filtering applied.
    async fn get_all_filtered_tools(&self, _context: &ExecutionContext) -> Result<Vec<ToolInfo>> {
        Ok(vec![])
    }

    /// Get distribution of tools across categories with security filtering.
    async fn get_category_tool_counts(
        &self,
        _context: &ExecutionContext,
    ) -> Result<HashMap<String, usize>> {
        Ok(HashMap::new())
    }

    /// Validate that a tool can be executed with the given parameters.
    async fn validate_tool_execution(
        &self,
        _tool_name: &str,
        _parameters: &Value,
        context: &ExecutionContext,
    ) -> bool {
        self.is_tool_available(_tool_name, context).await
    }

    /// Get execution cost estimate for planning optimization.
    async fn estimate_execution_cost(
        &self,
        _tool_name: &str,
        _parameters: &Value,
        _context: &ExecutionContext,
    ) -> f64 {
        1.0
    }
}

//! Traits for strategy-based exploration system

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;

use super::types::*;

// =============================================================================
// CORE EXPLORATION STRATEGY TRAIT
// =============================================================================

/// Base trait for all exploration strategies
#[async_trait]
pub trait ExplorationStrategy: Send + Sync {
    /// Execute the strategy to explore possible tool matches and decompositions
    ///
    /// # Arguments
    /// * `context` - Strategy context with analysis, budget, and tools
    /// * `query` - Original user query to explore
    ///
    /// # Returns
    /// * `ExplorationResult` - Complete exploration results including best path
    async fn explore(
        &mut self,
        context: &StrategyContext,
        query: &str,
    ) -> Result<ExplorationResult>;

    /// Get the strategy type
    fn strategy_type(&self) -> StrategyType;

    /// Check if strategy can continue with current budget
    fn can_continue(&self, budget: &ResourceBudget) -> bool {
        budget.can_continue()
    }

    /// Get strategy name for logging
    fn name(&self) -> &'static str {
        match self.strategy_type() {
            StrategyType::GuidedSearch => "GuidedSearch",
            StrategyType::AtomicComposition => "AtomicComposition",
        }
    }
}

// =============================================================================
// STRATEGY SPECIFIC TRAITS
// =============================================================================

/// Trait for strategies that support node expansion
#[async_trait]
pub trait NodeExpansion: Send + Sync {
    /// Expand a node by finding child tasks or tool matches
    async fn expand_node(
        &mut self,
        node: &ExplorationNode,
        context: &StrategyContext,
    ) -> Result<Vec<ExplorationNode>>;

    /// Evaluate a node's potential (for selection in tree search)
    fn evaluate_node(&self, node: &ExplorationNode, context: &StrategyContext) -> f32;
}

/// Trait for strategies that support backpropagation
pub trait Backpropagation {
    /// Backpropagate value from child to parent nodes
    fn backpropagate(
        &mut self,
        path: &[String],
        value: f32,
        nodes: &mut HashMap<String, ExplorationNode>,
    );

    /// Update node statistics after exploration
    fn update_node_stats(&mut self, node: &mut ExplorationNode, value: f32, visits: u32);
}

/// Trait for strategies that use selection algorithms
pub trait NodeSelection {
    /// Select best node for expansion using selection algorithm (e.g., PUCT)
    fn select_node(
        &self,
        nodes: &HashMap<String, ExplorationNode>,
        selection_root: &str,
    ) -> Option<String>;

    /// Calculate selection value for a node (e.g., PUCT value)
    fn calculate_selection_value(&self, node: &ExplorationNode, parent_visits: u32) -> f32;
}

// =============================================================================
// UTILITY TRAITS
// =============================================================================

/// Trait for strategies that support failure recovery
pub trait FailureRecovery {
    /// Handle strategy failure and suggest next action
    fn handle_failure(&self, failure: StrategyFailure, context: &StrategyContext) -> NextAction;

    /// Check if strategy should abort early
    fn should_abort(&self, context: &StrategyContext, _current_result: &ExplorationResult) -> bool {
        !context.resource_budget.can_continue()
    }
}

/// Trait for debugging and introspection
pub trait StrategyIntrospection {
    /// Get current strategy state for debugging
    fn get_debug_info(&self) -> serde_json::Value;

    /// Get performance metrics
    fn get_metrics(&self) -> StrategyMetrics;
}

/// Performance metrics for strategies
#[derive(Debug, Clone)]
pub struct StrategyMetrics {
    pub nodes_explored: u32,
    pub llm_calls_made: u32,
    pub average_confidence: f32,
    pub exploration_depth: u32,
    pub time_per_iteration_ms: f32,
}

impl Default for StrategyMetrics {
    fn default() -> Self {
        Self {
            nodes_explored: 0,
            llm_calls_made: 0,
            average_confidence: 0.0,
            exploration_depth: 0,
            time_per_iteration_ms: 0.0,
        }
    }
}

/// Strategy-specific error types for typed error handling
///
/// This enum replaces dangerous string-based error parsing with type-safe discriminants.
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StrategyError {
    /// Strategy execution paused due to budget constraints
    ///
    /// Contains structured information about which budget was exceeded and why.
    /// The orchestrator can use these fields for logging, metrics, or user feedback.
    #[error("Budget hold: {stage} - {channel} - {reason}")]
    BudgetHold {
        /// Stage context where budget was exceeded (e.g., "PlanningBootstrap")
        stage: String,
        /// Recommended escalation channel (e.g., "InApp", "Email")
        channel: String,
        /// Human-readable reason for the hold
        reason: String,
    },

    /// Strategy execution failed with a specific error message
    ///
    /// This is a catch-all for other strategy failures that don't fit the above categories.
    #[error("Strategy execution failed: {0}")]
    ExecutionFailed(String),

    /// Wraps other error types from dependencies
    ///
    /// This allows StrategyError to be converted from anyhow::Error while preserving
    /// the original error chain.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl StrategyError {
    /// Create a BudgetHold error from context
    ///
    /// Helper method for cleaner error construction at call sites.
    pub fn budget_hold(
        stage: impl Into<String>,
        channel: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        Self::BudgetHold {
            stage: stage.into(),
            channel: channel.into(),
            reason: reason.into(),
        }
    }

    /// Check if this error is a budget hold
    ///
    /// Useful for quick boolean checks without full pattern matching.
    pub fn is_budget_hold(&self) -> bool {
        matches!(self, Self::BudgetHold { .. })
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_budget_hold() {
        let err = StrategyError::budget_hold("PlanningBootstrap", "InApp", "LLM budget exceeded");
        assert!(err.is_budget_hold());
        assert!(err.to_string().contains("Budget hold"));
        assert!(err.to_string().contains("PlanningBootstrap"));
    }

    #[test]
    fn test_pattern_matching() {
        let err = StrategyError::BudgetHold {
            stage: "Test".to_string(),
            channel: "InApp".to_string(),
            reason: "Testing".to_string(),
        };

        match err {
            StrategyError::BudgetHold { stage, .. } => {
                assert_eq!(stage, "Test");
            },
            _ => panic!("Wrong error type"),
        }
    }
}

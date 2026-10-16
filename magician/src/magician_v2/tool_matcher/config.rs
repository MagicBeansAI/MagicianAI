//! Configuration for V2 Tool Matcher.
//!
//! This module defines scoring/selection parameters for the compact router +
//! optional LLM disambiguation flow.

use serde::{Deserialize, Serialize};

/// Configuration for V2 Tool Matcher
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolMatcherConfig {
    /// Tier weights (must sum to 1.0 for semantic + LLM, rule is separate)
    pub weights: TierWeights,

    /// Category bonus values
    pub category_bonuses: CategoryBonuses,

    /// Thresholds and limits
    pub limits: MatchingLimits,

    /// Fail-fast behavior
    pub fail_fast: bool,
}

impl Default for ToolMatcherConfig {
    fn default() -> Self {
        Self {
            weights: TierWeights::default(),
            category_bonuses: CategoryBonuses::default(),
            limits: MatchingLimits::default(),
            fail_fast: true, // Fail fast by default (no silent fallbacks)
        }
    }
}

/// Tier weight configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TierWeights {
    /// Rule-based matching weight (default: 15%)
    pub rule_weight: f32,

    /// Semantic matching weight (default: 30%)
    pub semantic_weight: f32,

    /// LLM evaluation weight (default: 55%)
    pub llm_weight: f32,
}

impl Default for TierWeights {
    fn default() -> Self {
        Self {
            rule_weight: 0.15,
            semantic_weight: 0.30,
            llm_weight: 0.55,
        }
    }
}

impl TierWeights {
    /// Validate that weights sum to approximately 1.0
    pub fn validate(&self) -> Result<(), String> {
        let total = self.rule_weight + self.semantic_weight + self.llm_weight;
        if (total - 1.0).abs() > 0.01 {
            return Err(format!("Tier weights must sum to 1.0, got {}", total));
        }
        Ok(())
    }
}

/// Category bonus configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryBonuses {
    /// Bonus for category match in rule-based matching (default: +0.15)
    pub rule_category_bonus: f32,

    /// Bonus for category match in semantic matching (default: +0.20)
    pub semantic_category_bonus: f32,
}

impl Default for CategoryBonuses {
    fn default() -> Self {
        Self {
            rule_category_bonus: 0.15,
            semantic_category_bonus: 0.20,
        }
    }
}

/// Matching limits and thresholds
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatchingLimits {
    /// Maximum number of candidates to send to LLM (Tier 4)
    /// Default: 30 (aligned with the proven 4-tier selection strategy)
    pub max_llm_candidates: usize,

    /// Minimum confidence threshold for final selection
    /// Default: 0.7 (70%)
    pub min_confidence_threshold: f32,

    /// Default success rate for matched tools (when no history available)
    /// Default: 0.9 (90%)
    pub default_success_rate: f32,

    /// Maximum time for rule-based matching (ms)
    /// Default: 100ms
    pub max_rule_matching_time_ms: u64,

    /// Maximum time for semantic matching (ms)
    /// Default: 200ms
    pub max_semantic_matching_time_ms: u64,

    /// Maximum time for LLM evaluation (ms)
    /// Default: 5000ms (5 seconds)
    pub max_llm_evaluation_time_ms: u64,
}

impl Default for MatchingLimits {
    fn default() -> Self {
        Self {
            max_llm_candidates: 30, // Raised from 20 for stronger candidate recall
            min_confidence_threshold: 0.7,
            default_success_rate: 0.9,
            max_rule_matching_time_ms: 100,
            max_semantic_matching_time_ms: 200,
            max_llm_evaluation_time_ms: 5000,
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_default_weights_sum_to_one() {
        let weights = TierWeights::default();
        assert!(weights.validate().is_ok());
    }

    #[test]
    fn test_invalid_weights() {
        let weights = TierWeights {
            rule_weight: 0.2,
            semantic_weight: 0.3,
            llm_weight: 0.4,
        };
        assert!(weights.validate().is_err());
    }

    #[test]
    fn test_default_config_is_valid() {
        let config = ToolMatcherConfig::default();
        assert!(config.weights.validate().is_ok());
        assert!(config.fail_fast);
        assert_eq!(config.limits.max_llm_candidates, 30); // Updated from 20
    }
}

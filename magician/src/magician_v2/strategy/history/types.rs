//! Types for strategy history tracking
//!
//! This module defines the core data structures used for recording and
//! analyzing strategy execution history to enable learning and intelligent
//! strategy selection.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::super::StrategyType;

/// Record of a single strategy execution
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StrategyExecutionRecord {
    /// When this strategy was executed
    pub timestamp: DateTime<Utc>,

    /// Original user query
    pub query: String,

    /// Query complexity score (0.0-1.0)
    pub complexity: f32,

    /// Tool categories detected in query
    pub categories: Vec<String>,

    /// Which strategy was used
    pub strategy_used: StrategyType,

    /// Confidence score achieved (0.0-1.0)
    pub confidence_achieved: f32,

    /// Time taken in milliseconds
    pub time_ms: u64,

    /// Number of LLM calls made
    pub llm_calls: u32,

    /// Whether execution was considered successful
    /// (confidence >= threshold and tool executed successfully)
    pub success: bool,

    /// Optional: Tool name that was selected
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,

    /// Optional: Failure reason if unsuccessful
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_reason: Option<String>,
}

/// Filter criteria for querying historical records
#[derive(Debug, Clone, Default)]
pub struct HistoryFilter {
    /// Filter by strategy type
    pub strategy: Option<StrategyType>,

    /// Filter by minimum complexity
    pub min_complexity: Option<f32>,

    /// Filter by maximum complexity
    pub max_complexity: Option<f32>,

    /// Filter by categories (any overlap)
    pub categories: Option<Vec<String>>,

    /// Filter by success status
    pub success_only: Option<bool>,

    /// Filter by time range (from)
    pub from_timestamp: Option<DateTime<Utc>>,

    /// Filter by time range (to)
    pub to_timestamp: Option<DateTime<Utc>>,

    /// Limit number of results
    pub limit: Option<usize>,
}

/// Statistics about the history storage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageStats {
    /// Total number of records
    pub total_records: usize,

    /// Storage size in bytes
    pub size_bytes: u64,

    /// Oldest record timestamp
    pub oldest_record: Option<DateTime<Utc>>,

    /// Newest record timestamp
    pub newest_record: Option<DateTime<Utc>>,

    /// Records per strategy type
    pub records_per_strategy: std::collections::HashMap<String, usize>,

    /// Success rate per strategy
    pub success_rate_per_strategy: std::collections::HashMap<String, f32>,
}

impl StrategyExecutionRecord {
    /// Check if this record matches the given filter
    pub fn matches_filter(&self, filter: &HistoryFilter) -> bool {
        // Strategy filter
        if let Some(ref strategy) = filter.strategy {
            if self.strategy_used != *strategy {
                return false;
            }
        }

        // Complexity range filter
        if let Some(min_complexity) = filter.min_complexity {
            if self.complexity < min_complexity {
                return false;
            }
        }

        if let Some(max_complexity) = filter.max_complexity {
            if self.complexity > max_complexity {
                return false;
            }
        }

        // Category overlap filter
        if let Some(ref filter_categories) = filter.categories {
            if !filter_categories
                .iter()
                .any(|c| self.categories.contains(c))
            {
                return false;
            }
        }

        // Success filter
        if let Some(success_only) = filter.success_only {
            if success_only && !self.success {
                return false;
            }
        }

        // Time range filter
        if let Some(from) = filter.from_timestamp {
            if self.timestamp < from {
                return false;
            }
        }

        if let Some(to) = filter.to_timestamp {
            if self.timestamp > to {
                return false;
            }
        }

        true
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn create_test_record(
        complexity: f32,
        categories: Vec<&str>,
        success: bool,
    ) -> StrategyExecutionRecord {
        StrategyExecutionRecord {
            timestamp: Utc::now(),
            query: "test query".to_string(),
            complexity,
            categories: categories.iter().map(|s| s.to_string()).collect(),
            strategy_used: StrategyType::GuidedSearch,
            confidence_achieved: 0.8,
            time_ms: 500,
            llm_calls: 1,
            success,
            tool_name: Some("test_tool".to_string()),
            failure_reason: None,
        }
    }

    #[test]
    fn test_filter_by_complexity() {
        let record = create_test_record(0.5, vec!["network"], true);

        let filter = HistoryFilter {
            min_complexity: Some(0.3),
            max_complexity: Some(0.7),
            ..Default::default()
        };

        assert!(record.matches_filter(&filter));

        let filter_too_high = HistoryFilter {
            min_complexity: Some(0.6),
            ..Default::default()
        };

        assert!(!record.matches_filter(&filter_too_high));
    }

    #[test]
    fn test_filter_by_categories() {
        let record = create_test_record(0.5, vec!["network", "deployment"], true);

        let filter = HistoryFilter {
            categories: Some(vec!["network".to_string()]),
            ..Default::default()
        };

        assert!(record.matches_filter(&filter));

        let filter_no_overlap = HistoryFilter {
            categories: Some(vec!["database".to_string()]),
            ..Default::default()
        };

        assert!(!record.matches_filter(&filter_no_overlap));
    }

    #[test]
    fn test_filter_by_success() {
        let success_record = create_test_record(0.5, vec!["network"], true);
        let failure_record = create_test_record(0.5, vec!["network"], false);

        let filter = HistoryFilter {
            success_only: Some(true),
            ..Default::default()
        };

        assert!(success_record.matches_filter(&filter));
        assert!(!failure_record.matches_filter(&filter));
    }
}

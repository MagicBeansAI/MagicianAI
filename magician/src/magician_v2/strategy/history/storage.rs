//! Abstract storage interface for strategy history
//!
//! This module defines the trait that all storage backends must implement,
//! allowing for flexible storage strategies (file-based, database, in-memory,
//! etc.)

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use super::types::{HistoryFilter, StorageStats, StrategyExecutionRecord};
use crate::magician_v2::{query_analysis::UnifiedQueryAnalysis, strategy::StrategyType};

/// Abstract trait for strategy history storage backends
///
/// Implementations must be thread-safe (Send + Sync) to allow concurrent access
/// from multiple async tasks.
#[async_trait]
pub trait StrategyHistoryStorage: Send + Sync {
    /// Save a new strategy execution record
    ///
    /// # Arguments
    /// * `record` - The execution record to save
    ///
    /// # Returns
    /// * `Ok(())` on success
    /// * `Err` if storage operation fails
    async fn save_record(&self, record: &StrategyExecutionRecord) -> Result<()>;

    /// Load records matching the given filter
    ///
    /// # Arguments
    /// * `filter` - Optional filter criteria. If None, returns all records.
    ///
    /// # Returns
    /// * Vector of matching records
    /// * Empty vector if no matches
    async fn load_records(
        &self,
        filter: Option<HistoryFilter>,
    ) -> Result<Vec<StrategyExecutionRecord>>;

    /// Find records with similar query patterns
    ///
    /// Similarity is determined by:
    /// - Similar complexity score (±0.2)
    /// - Overlapping categories
    ///
    /// # Arguments
    /// * `query_analysis` - Analysis of current query to find similar past
    ///   queries
    ///
    /// # Returns
    /// * Vector of similar records, sorted by relevance (most similar first)
    async fn find_similar(
        &self,
        query_analysis: &UnifiedQueryAnalysis,
    ) -> Result<Vec<StrategyExecutionRecord>>;

    /// Calculate success rate for a strategy on similar queries
    ///
    /// # Arguments
    /// * `strategy` - The strategy type to check
    /// * `similar_records` - Pre-filtered similar records (from find_similar)
    ///
    /// # Returns
    /// * Success rate as f32 (0.0-1.0)
    /// * 0.5 if no records found (neutral/unknown)
    async fn get_success_rate(
        &self,
        strategy: StrategyType,
        similar_records: &[StrategyExecutionRecord],
    ) -> Result<f32>;

    /// Remove old records based on retention policy
    ///
    /// # Arguments
    /// * `older_than` - Delete records older than this timestamp
    ///
    /// # Returns
    /// * Number of records deleted
    async fn cleanup_old_records(&self, older_than: DateTime<Utc>) -> Result<usize>;

    /// Get statistics about the storage
    ///
    /// # Returns
    /// * StorageStats with metadata about stored records
    async fn get_stats(&self) -> Result<StorageStats>;

    /// Clear all records (primarily for testing)
    ///
    /// # Returns
    /// * Number of records cleared
    async fn clear_all(&self) -> Result<usize>;
}

/// Helper function to calculate similarity score between two queries
///
/// Score is based on:
/// - Complexity similarity (40%)
/// - Category overlap (60%)
///
/// # Returns
/// * Similarity score 0.0-1.0 (higher = more similar)
pub fn calculate_similarity_score(
    query1_complexity: f32,
    query1_categories: &[String],
    query2_complexity: f32,
    query2_categories: &[String],
) -> f32 {
    // Complexity similarity (inverse of difference, normalized)
    let complexity_diff = (query1_complexity - query2_complexity).abs();
    let complexity_similarity = (1.0 - complexity_diff).max(0.0);

    // Category overlap (Jaccard similarity)
    let set1: std::collections::HashSet<_> = query1_categories.iter().collect();
    let set2: std::collections::HashSet<_> = query2_categories.iter().collect();

    let intersection_size = set1.intersection(&set2).count();
    let union_size = set1.union(&set2).count();

    let category_similarity = if union_size == 0 {
        0.0
    } else {
        intersection_size as f32 / union_size as f32
    };

    // Weighted combination
    (complexity_similarity * 0.4) + (category_similarity * 0.6)
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_similarity_identical_queries() {
        let score = calculate_similarity_score(
            0.5,
            &["network".to_string(), "deployment".to_string()],
            0.5,
            &["network".to_string(), "deployment".to_string()],
        );

        assert!(
            (score - 1.0).abs() < 0.01,
            "Identical queries should have similarity ~1.0"
        );
    }

    #[test]
    fn test_similarity_different_complexity() {
        let score = calculate_similarity_score(
            0.2,
            &["network".to_string()],
            0.8,
            &["network".to_string()],
        );

        // Same category but very different complexity
        // Complexity similarity = 1.0 - 0.6 = 0.4
        // Category similarity = 1.0
        // Total = 0.4 * 0.4 + 1.0 * 0.6 = 0.76
        assert!((score - 0.76).abs() < 0.01);
    }

    #[test]
    fn test_similarity_no_overlap() {
        let score = calculate_similarity_score(
            0.5,
            &["network".to_string()],
            0.5,
            &["database".to_string()],
        );

        // Same complexity but no category overlap
        // Complexity similarity = 1.0
        // Category similarity = 0.0
        // Total = 1.0 * 0.4 + 0.0 * 0.6 = 0.4
        assert!((score - 0.4).abs() < 0.01);
    }

    #[test]
    fn test_similarity_partial_overlap() {
        let score = calculate_similarity_score(
            0.5,
            &["network".to_string(), "deployment".to_string()],
            0.5,
            &["network".to_string(), "database".to_string()],
        );

        // Same complexity, 1/3 category overlap (1 common, 3 total unique)
        // Complexity similarity = 1.0
        // Category similarity = 1/3 = 0.333...
        // Total = 1.0 * 0.4 + 0.333 * 0.6 = 0.6
        assert!((score - 0.6).abs() < 0.01);
    }
}

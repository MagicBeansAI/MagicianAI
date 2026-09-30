//! High-level strategy history store with factory methods
//!
//! This module provides the main interface for recording and querying strategy
//! execution history. It wraps the storage trait implementations and provides
//! factory methods for different backends.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use tracing::{debug, info};

use super::{
    file_storage::FileStrategyStorage,
    memory_storage::MemoryStrategyStorage,
    storage::StrategyHistoryStorage,
    types::{HistoryFilter, StorageStats, StrategyExecutionRecord},
};
use crate::magician_v2::{query_analysis::UnifiedQueryAnalysis, strategy::StrategyType};

/// High-level strategy history store
///
/// Provides factory methods for different storage backends and high-level
/// operations for recording and querying strategy execution history.
pub struct StrategyHistoryStore {
    /// Underlying storage implementation
    storage: Arc<dyn StrategyHistoryStorage>,

    /// Default retention period for cleanup (days)
    retention_days: i64,
}

impl StrategyHistoryStore {
    /// Create file-based storage (production)
    ///
    /// # Arguments
    /// * `storage_path` - Directory to store history files
    ///
    /// # Returns
    /// * Initialized store with file-based backend
    pub async fn new_file_based(storage_path: PathBuf) -> Result<Self> {
        let storage = FileStrategyStorage::new(storage_path)
            .await
            .context("Failed to create file-based storage")?;

        info!("[MAGICIAN-V2-STRATEGY] StrategyHistoryStore initialized with file-based storage");

        Ok(Self {
            storage: Arc::new(storage),
            retention_days: 60,
        })
    }

    /// Create file-based storage with custom configuration
    ///
    /// # Arguments
    /// * `storage_path` - Directory to store history files
    /// * `max_file_size_mb` - Maximum file size before rotation (MB)
    /// * `retention_days` - Retention period for cleanup (days)
    pub async fn new_file_based_with_config(
        storage_path: PathBuf,
        max_file_size_mb: u64,
        retention_days: i64,
    ) -> Result<Self> {
        let storage =
            FileStrategyStorage::with_config(storage_path, max_file_size_mb, retention_days)
                .await
                .context("Failed to create file-based storage with config")?;

        info!(
            "[MAGICIAN-V2-STRATEGY] StrategyHistoryStore initialized with custom config (size: \
             {}MB, retention: {} days)",
            max_file_size_mb, retention_days
        );

        Ok(Self {
            storage: Arc::new(storage),
            retention_days,
        })
    }

    /// Create memory-based storage (testing/ephemeral)
    ///
    /// # Arguments
    /// * `max_records` - Maximum number of records before LRU eviction
    ///
    /// # Returns
    /// * Initialized store with memory-based backend
    pub fn new_memory_based(max_records: usize) -> Self {
        let storage = MemoryStrategyStorage::new(max_records);

        debug!(
            "[MAGICIAN-V2-STRATEGY] StrategyHistoryStore initialized with memory-based storage \
             (max: {})",
            max_records
        );

        Self {
            storage: Arc::new(storage),
            retention_days: 60,
        }
    }

    /// Create memory-based storage with default capacity (10,000 records)
    pub fn new_memory_based_default() -> Self {
        Self::new_memory_based(10_000)
    }

    /// Record a strategy execution
    ///
    /// # Arguments
    /// * `record` - The execution record to save
    pub async fn record_execution(&self, record: &StrategyExecutionRecord) -> Result<()> {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Recording strategy execution: query='{}', strategy={:?}, \
             success={}",
            record.query, record.strategy_used, record.success
        );

        self.storage.save_record(record).await?;
        Ok(())
    }

    /// Load historical records with optional filtering
    ///
    /// # Arguments
    /// * `filter` - Optional filter criteria
    ///
    /// # Returns
    /// * Vector of matching records
    pub async fn load_history(
        &self,
        filter: Option<HistoryFilter>,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        self.storage.load_records(filter).await
    }

    /// Find similar historical queries
    ///
    /// Uses similarity scoring based on:
    /// - Complexity similarity (40%)
    /// - Category overlap (60%)
    ///
    /// # Arguments
    /// * `query_analysis` - Analysis of current query
    ///
    /// # Returns
    /// * Vector of similar records, sorted by relevance (most similar first)
    /// * Empty vector if no similar queries found
    pub async fn find_similar_queries(
        &self,
        query_analysis: &UnifiedQueryAnalysis,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        debug!(
            "[MAGICIAN-V2-STRATEGY] Finding similar queries for complexity={:.2}, categories={:?}",
            query_analysis.complexity.score, query_analysis.categories.categories
        );

        let similar = self.storage.find_similar(query_analysis).await?;

        debug!(
            "[MAGICIAN-V2-STRATEGY] Found {} similar historical queries",
            similar.len()
        );

        Ok(similar)
    }

    /// Get success rate for a strategy on similar queries
    ///
    /// # Arguments
    /// * `strategy` - Strategy type to check
    /// * `similar_records` - Pre-filtered similar records (from
    ///   find_similar_queries)
    ///
    /// # Returns
    /// * Success rate as f32 (0.0-1.0)
    /// * 0.5 if no historical data (neutral/unknown)
    pub async fn get_strategy_success_rate(
        &self,
        strategy: StrategyType,
        similar_records: &[StrategyExecutionRecord],
    ) -> Result<f32> {
        let rate = self
            .storage
            .get_success_rate(strategy, similar_records)
            .await?;

        debug!(
            "[MAGICIAN-V2-STRATEGY] Success rate for {:?} on {} similar queries: {:.1}%",
            strategy,
            similar_records.len(),
            rate * 100.0
        );

        Ok(rate)
    }

    /// Get success rates for all strategies on similar queries
    ///
    /// # Arguments
    /// * `similar_records` - Pre-filtered similar records
    ///
    /// # Returns
    /// * HashMap of strategy -> success rate
    pub async fn get_all_strategy_success_rates(
        &self,
        similar_records: &[StrategyExecutionRecord],
    ) -> Result<std::collections::HashMap<StrategyType, f32>> {
        use std::collections::HashMap;

        let mut rates = HashMap::new();

        for strategy in &[StrategyType::GuidedSearch, StrategyType::AtomicComposition] {
            let rate = self
                .get_strategy_success_rate(*strategy, similar_records)
                .await?;
            rates.insert(*strategy, rate);
        }

        Ok(rates)
    }

    /// Get statistics about the history storage
    ///
    /// # Returns
    /// * StorageStats with metadata about stored records
    pub async fn get_stats(&self) -> Result<StorageStats> {
        self.storage.get_stats().await
    }

    /// Clean up old records based on retention policy
    ///
    /// # Arguments
    /// * `older_than_days` - Optional override for retention period (uses
    ///   default if None)
    ///
    /// # Returns
    /// * Number of records deleted
    pub async fn cleanup_old_records(&self, older_than_days: Option<i64>) -> Result<usize> {
        let days = older_than_days.unwrap_or(self.retention_days);
        let cutoff = Utc::now() - ChronoDuration::days(days);

        info!(
            "[MAGICIAN-V2-STRATEGY] Cleaning up records older than {} days",
            days
        );

        let deleted = self.storage.cleanup_old_records(cutoff).await?;

        info!("[MAGICIAN-V2-STRATEGY] Deleted {} old records", deleted);

        Ok(deleted)
    }

    /// Clear all records (primarily for testing)
    ///
    /// # Returns
    /// * Number of records cleared
    pub async fn clear_all(&self) -> Result<usize> {
        info!("[MAGICIAN-V2-STRATEGY] Clearing all strategy history records");

        let count = self.storage.clear_all().await?;

        info!("[MAGICIAN-V2-STRATEGY] Cleared {} records", count);

        Ok(count)
    }

    /// Get recent failures for a specific query pattern
    ///
    /// Helper method to quickly find recent failures for debugging or LLM
    /// consultation
    ///
    /// # Arguments
    /// * `query_analysis` - Current query analysis
    /// * `limit` - Maximum number of failures to return
    ///
    /// # Returns
    /// * Vector of recent failure records
    pub async fn get_recent_failures(
        &self,
        query_analysis: &UnifiedQueryAnalysis,
        limit: usize,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        // Find similar queries
        let similar = self.find_similar_queries(query_analysis).await?;

        // Filter for failures only
        let failures: Vec<_> = similar
            .into_iter()
            .filter(|r| !r.success)
            .take(limit)
            .collect();

        debug!(
            "[MAGICIAN-V2-STRATEGY] Found {} recent failures for similar queries",
            failures.len()
        );

        Ok(failures)
    }

    /// Get recommended strategy based on historical performance
    ///
    /// Returns the strategy with highest success rate on similar queries,
    /// or None if insufficient historical data (< 3 similar queries)
    ///
    /// # Arguments
    /// * `query_analysis` - Current query analysis
    ///
    /// # Returns
    /// * Optional tuple of (recommended strategy, confidence score 0.0-1.0)
    pub async fn get_recommended_strategy(
        &self,
        query_analysis: &UnifiedQueryAnalysis,
    ) -> Result<Option<(StrategyType, f32)>> {
        let similar = self.find_similar_queries(query_analysis).await?;

        // Need at least 3 similar queries for meaningful recommendation
        if similar.len() < 3 {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Insufficient historical data ({} queries) for \
                 recommendation",
                similar.len()
            );
            return Ok(None);
        }

        let rates = self.get_all_strategy_success_rates(&similar).await?;

        // Find strategy with highest success rate
        let best = rates
            .into_iter()
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        if let Some((strategy, rate)) = best {
            debug!(
                "[MAGICIAN-V2-STRATEGY] Recommended strategy: {:?} (success rate: {:.1}% on {} \
                 similar queries)",
                strategy,
                rate * 100.0,
                similar.len()
            );
            Ok(Some((strategy, rate)))
        } else {
            Ok(None)
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn create_test_record(
        query: &str,
        complexity: f32,
        categories: Vec<&str>,
        strategy: StrategyType,
        success: bool,
    ) -> StrategyExecutionRecord {
        StrategyExecutionRecord {
            timestamp: Utc::now(),
            query: query.to_string(),
            complexity,
            categories: categories.iter().map(|s| s.to_string()).collect(),
            strategy_used: strategy,
            confidence_achieved: 0.8,
            time_ms: 500,
            llm_calls: 1,
            success,
            tool_name: Some("test_tool".to_string()),
            failure_reason: if !success {
                Some("test failure".to_string())
            } else {
                None
            },
        }
    }

    fn create_test_query_analysis(complexity: f32, categories: Vec<&str>) -> UnifiedQueryAnalysis {
        use crate::magician_v2::query_analysis::{
            CategoryAnalysis, ComplexityAnalysis, DependencyAnalysis, ExtractedEntities,
            QueryIntent, ResourceEstimate,
        };

        UnifiedQueryAnalysis {
            original_query: "test query".to_string(),
            complexity: ComplexityAnalysis {
                score: complexity,
                factors: vec!["test factor".to_string()],
                reasoning: "test reasoning".to_string(),
            },
            categories: CategoryAnalysis {
                categories: categories.iter().map(|s| s.to_string()).collect(),
                reasoning: "test category reasoning".to_string(),
            },
            dependencies: DependencyAnalysis {
                is_multi_step: false,
                dependencies: vec![],
                workflow_steps: vec![],
                reasoning: "test dependency reasoning".to_string(),
                required_capabilities: vec![],
            },
            resource_estimate: ResourceEstimate {
                expected_tokens: 1000,
                expected_duration_ms: 5000,
                expected_iterations: 3,
            },
            extracted_entities: ExtractedEntities::default(),
            intent: QueryIntent::NewTask,
            slot_match: None,
            llm_calls_used: 0, // Test data
            task_clarity: crate::magician_v2::query_analysis::TaskClarity::default(),
        }
    }

    #[tokio::test]
    async fn test_memory_based_store() {
        let store = StrategyHistoryStore::new_memory_based(100);

        let record = create_test_record(
            "ping google.com",
            0.2,
            vec!["network"],
            StrategyType::GuidedSearch,
            true,
        );

        store.record_execution(&record).await.unwrap();

        let loaded = store.load_history(None).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].query, "ping google.com");
    }

    #[tokio::test]
    async fn test_get_recommended_strategy() {
        let store = StrategyHistoryStore::new_memory_based(100);

        // Record multiple successful Greedy executions for simple queries
        for i in 0..5 {
            let record = create_test_record(
                &format!("simple query {}", i),
                0.2,
                vec!["network"],
                StrategyType::GuidedSearch,
                true,
            );
            store.record_execution(&record).await.unwrap();
        }

        // Record failed BeamSearch on similar queries
        for i in 0..3 {
            let record = create_test_record(
                &format!("simple query {}", i),
                0.25,
                vec!["network"],
                StrategyType::GuidedSearch,
                false,
            );
            store.record_execution(&record).await.unwrap();
        }

        // Query analysis for similar simple query
        let query_analysis = create_test_query_analysis(0.22, vec!["network"]);

        let recommendation = store
            .get_recommended_strategy(&query_analysis)
            .await
            .unwrap();

        assert!(recommendation.is_some());
        let (strategy, confidence) = recommendation.unwrap();
        assert_eq!(strategy, StrategyType::GuidedSearch);
        assert!(confidence > 0.5); // Should have high confidence based on 5
                                   // successes
    }

    #[tokio::test]
    async fn test_insufficient_data_recommendation() {
        let store = StrategyHistoryStore::new_memory_based(100);

        // Record only 2 executions (below threshold of 3)
        for i in 0..2 {
            let record = create_test_record(
                &format!("query {}", i),
                0.2,
                vec!["network"],
                StrategyType::GuidedSearch,
                true,
            );
            store.record_execution(&record).await.unwrap();
        }

        let query_analysis = create_test_query_analysis(0.2, vec!["network"]);

        let recommendation = store
            .get_recommended_strategy(&query_analysis)
            .await
            .unwrap();

        // Should return None due to insufficient data
        assert!(recommendation.is_none());
    }

    #[tokio::test]
    async fn test_get_recent_failures() {
        let store = StrategyHistoryStore::new_memory_based(100);

        // Record successful and failed executions
        store
            .record_execution(&create_test_record(
                "query1",
                0.2,
                vec!["network"],
                StrategyType::GuidedSearch,
                true,
            ))
            .await
            .unwrap();

        store
            .record_execution(&create_test_record(
                "query2",
                0.25,
                vec!["network"],
                StrategyType::GuidedSearch,
                false,
            ))
            .await
            .unwrap();

        store
            .record_execution(&create_test_record(
                "query3",
                0.22,
                vec!["network"],
                StrategyType::GuidedSearch,
                false,
            ))
            .await
            .unwrap();

        let query_analysis = create_test_query_analysis(0.23, vec!["network"]);

        let failures = store.get_recent_failures(&query_analysis, 5).await.unwrap();

        assert_eq!(failures.len(), 2); // Only 2 failures
        assert!(failures.iter().all(|r| !r.success));
    }

    #[tokio::test]
    async fn test_get_stats() {
        let store = StrategyHistoryStore::new_memory_based(100);

        store
            .record_execution(&create_test_record(
                "query1",
                0.2,
                vec!["network"],
                StrategyType::GuidedSearch,
                true,
            ))
            .await
            .unwrap();

        store
            .record_execution(&create_test_record(
                "query2",
                0.5,
                vec!["deployment"],
                StrategyType::GuidedSearch,
                false,
            ))
            .await
            .unwrap();

        let stats = store.get_stats().await.unwrap();

        assert_eq!(stats.total_records, 2);
        assert_eq!(stats.records_per_strategy.get("GuidedSearch"), Some(&2));
    }
}

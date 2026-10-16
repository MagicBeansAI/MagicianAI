//! In-memory storage for strategy history (testing and ephemeral deployments)
//!
//! This implementation provides fast, non-persistent storage suitable for:
//! - Unit and integration testing
//! - Ephemeral deployments
//! - Performance benchmarking
//! - Situations where persistence is not required

use std::{collections::HashMap, sync::Arc};

use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::debug;

use super::{
    storage::{calculate_similarity_score, StrategyHistoryStorage},
    types::{HistoryFilter, StorageStats, StrategyExecutionRecord},
};
use crate::magician_v2::{query_analysis::UnifiedQueryAnalysis, strategy::StrategyType};

/// In-memory storage implementation with LRU eviction
///
/// # Features
/// - Fast (no I/O operations)
/// - Thread-safe with RwLock
/// - LRU eviction when max_records exceeded
/// - Perfect for testing
pub struct MemoryStrategyStorage {
    /// Stored records (newest first)
    records: Arc<RwLock<Vec<StrategyExecutionRecord>>>,

    /// Maximum number of records to keep
    max_records: usize,
}

impl MemoryStrategyStorage {
    /// Create new in-memory storage
    ///
    /// # Arguments
    /// * `max_records` - Maximum records before LRU eviction (default: 10,000)
    ///
    /// # Returns
    /// * Initialized storage
    pub fn new(max_records: usize) -> Self {
        debug!(
            "[MAGICIAN-V2-STRATEGY] MemoryStrategyStorage initialized with max_records={}",
            max_records
        );

        Self {
            records: Arc::new(RwLock::new(Vec::new())),
            max_records,
        }
    }

    /// Create with default capacity (10,000 records)
    pub fn default() -> Self {
        Self::new(10_000)
    }

    /// Evict oldest records if over capacity (LRU)
    async fn evict_if_needed(&self) {
        let mut records = self.records.write().await;

        if records.len() > self.max_records {
            let excess = records.len() - self.max_records;

            // Records are sorted newest first, so remove from end
            records.truncate(self.max_records);

            debug!(
                "[MAGICIAN-V2-STRATEGY] Evicted {} old records (LRU)",
                excess
            );
        }
    }
}

#[async_trait]
impl StrategyHistoryStorage for MemoryStrategyStorage {
    async fn save_record(&self, record: &StrategyExecutionRecord) -> Result<()> {
        let mut records = self.records.write().await;

        // Add at beginning (newest first)
        records.insert(0, record.clone());

        drop(records); // Release lock before eviction

        // Check if eviction needed
        self.evict_if_needed().await;

        Ok(())
    }

    async fn load_records(
        &self,
        filter: Option<HistoryFilter>,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        let records = self.records.read().await;

        let mut results: Vec<_> = records
            .iter()
            .filter(|record| {
                if let Some(ref f) = filter {
                    record.matches_filter(f)
                } else {
                    true
                }
            })
            .cloned()
            .collect();

        // Apply limit if specified
        if let Some(ref f) = filter {
            if let Some(limit) = f.limit {
                results.truncate(limit);
            }
        }

        Ok(results)
    }

    async fn find_similar(
        &self,
        query_analysis: &UnifiedQueryAnalysis,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        let records = self.records.read().await;

        let mut scored_records: Vec<_> = records
            .iter()
            .map(|record| {
                let score = calculate_similarity_score(
                    query_analysis.complexity.score,
                    &query_analysis.categories.categories,
                    record.complexity,
                    &record.categories,
                );
                (score, record.clone())
            })
            .filter(|(score, _)| *score > 0.3) // Only include records with >30% similarity
            .collect();

        // Sort by similarity score (highest first)
        scored_records.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

        // Return top 50 most similar
        Ok(scored_records
            .into_iter()
            .take(50)
            .map(|(_, record)| record)
            .collect())
    }

    async fn get_success_rate(
        &self,
        strategy: StrategyType,
        similar_records: &[StrategyExecutionRecord],
    ) -> Result<f32> {
        let strategy_attempts: Vec<_> = similar_records
            .iter()
            .filter(|r| r.strategy_used == strategy)
            .collect();

        if strategy_attempts.is_empty() {
            return Ok(0.5); // Neutral/unknown
        }

        let successes = strategy_attempts.iter().filter(|r| r.success).count();
        Ok(successes as f32 / strategy_attempts.len() as f32)
    }

    async fn cleanup_old_records(&self, older_than: DateTime<Utc>) -> Result<usize> {
        let mut records = self.records.write().await;

        let original_len = records.len();

        // Remove records older than threshold
        records.retain(|record| record.timestamp >= older_than);

        let deleted = original_len - records.len();

        if deleted > 0 {
            debug!("[MAGICIAN-V2-STRATEGY] Cleaned up {} old records", deleted);
        }

        Ok(deleted)
    }

    async fn get_stats(&self) -> Result<StorageStats> {
        let records = self.records.read().await;

        let total_records = records.len();
        let oldest_record = records.iter().map(|r| r.timestamp).min();
        let newest_record = records.iter().map(|r| r.timestamp).max();

        // Count records per strategy
        let mut records_per_strategy = HashMap::new();
        let mut success_per_strategy: HashMap<String, (usize, usize)> = HashMap::new();

        for record in records.iter() {
            let strategy_name = format!("{:?}", record.strategy_used);

            *records_per_strategy
                .entry(strategy_name.clone())
                .or_insert(0) += 1;

            let (successes, total) = success_per_strategy.entry(strategy_name).or_insert((0, 0));
            *total += 1;
            if record.success {
                *successes += 1;
            }
        }

        // Calculate success rates
        let success_rate_per_strategy = success_per_strategy
            .into_iter()
            .map(|(strategy, (successes, total))| (strategy, successes as f32 / total as f32))
            .collect();

        Ok(StorageStats {
            total_records,
            size_bytes: 0, // Not applicable for memory storage
            oldest_record,
            newest_record,
            records_per_strategy,
            success_rate_per_strategy,
        })
    }

    async fn clear_all(&self) -> Result<usize> {
        let mut records = self.records.write().await;
        let count = records.len();
        records.clear();

        debug!(
            "[MAGICIAN-V2-STRATEGY] Cleared all {} records from memory",
            count
        );

        Ok(count)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::strategy::StrategyType;

    fn create_test_record(query: &str, complexity: f32, success: bool) -> StrategyExecutionRecord {
        StrategyExecutionRecord {
            timestamp: Utc::now(),
            query: query.to_string(),
            complexity,
            categories: vec!["network".to_string()],
            strategy_used: StrategyType::GuidedSearch,
            confidence_achieved: 0.8,
            time_ms: 500,
            llm_calls: 1,
            success,
            tool_name: Some("test_tool".to_string()),
            failure_reason: None,
        }
    }

    #[tokio::test]
    async fn test_save_and_load() {
        let storage = MemoryStrategyStorage::new(100);

        let record = create_test_record("ping google.com", 0.2, true);

        storage.save_record(&record).await.unwrap();

        let loaded = storage.load_records(None).await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].query, "ping google.com");
    }

    #[tokio::test]
    async fn test_lru_eviction() {
        let storage = MemoryStrategyStorage::new(3);

        // Add 5 records (should keep only 3 newest)
        storage
            .save_record(&create_test_record("query1", 0.2, true))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        storage
            .save_record(&create_test_record("query2", 0.3, true))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        storage
            .save_record(&create_test_record("query3", 0.4, true))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        storage
            .save_record(&create_test_record("query4", 0.5, true))
            .await
            .unwrap();
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        storage
            .save_record(&create_test_record("query5", 0.6, true))
            .await
            .unwrap();

        let loaded = storage.load_records(None).await.unwrap();

        // Should have only 3 newest records
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded[0].query, "query5");
        assert_eq!(loaded[1].query, "query4");
        assert_eq!(loaded[2].query, "query3");
    }

    #[tokio::test]
    async fn test_filter_by_complexity() {
        let storage = MemoryStrategyStorage::new(100);

        storage
            .save_record(&create_test_record("query1", 0.2, true))
            .await
            .unwrap();
        storage
            .save_record(&create_test_record("query2", 0.5, true))
            .await
            .unwrap();
        storage
            .save_record(&create_test_record("query3", 0.8, true))
            .await
            .unwrap();

        let filter = HistoryFilter {
            min_complexity: Some(0.4),
            max_complexity: Some(0.6),
            ..Default::default()
        };

        let loaded = storage.load_records(Some(filter)).await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].query, "query2");
    }

    #[tokio::test]
    async fn test_cleanup_old_records() {
        let storage = MemoryStrategyStorage::new(100);

        // Add old record
        let mut old_record = create_test_record("old_query", 0.2, true);
        old_record.timestamp = Utc::now() - chrono::Duration::days(90);
        storage.save_record(&old_record).await.unwrap();

        // Add recent record
        storage
            .save_record(&create_test_record("recent_query", 0.3, true))
            .await
            .unwrap();

        // Cleanup records older than 60 days
        let cutoff = Utc::now() - chrono::Duration::days(60);
        let deleted = storage.cleanup_old_records(cutoff).await.unwrap();

        assert_eq!(deleted, 1);

        let loaded = storage.load_records(None).await.unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].query, "recent_query");
    }

    #[tokio::test]
    async fn test_get_stats() {
        let storage = MemoryStrategyStorage::new(100);

        storage
            .save_record(&create_test_record("query1", 0.2, true))
            .await
            .unwrap();
        storage
            .save_record(&create_test_record("query2", 0.3, false))
            .await
            .unwrap();
        storage
            .save_record(&create_test_record("query3", 0.4, true))
            .await
            .unwrap();

        let stats = storage.get_stats().await.unwrap();

        assert_eq!(stats.total_records, 3);
        assert_eq!(stats.records_per_strategy.get("GuidedSearch"), Some(&3));
        assert_eq!(stats.size_bytes, 0); // Memory storage doesn't track size
    }

    #[tokio::test]
    async fn test_clear_all() {
        let storage = MemoryStrategyStorage::new(100);

        storage
            .save_record(&create_test_record("query1", 0.2, true))
            .await
            .unwrap();
        storage
            .save_record(&create_test_record("query2", 0.3, true))
            .await
            .unwrap();

        let count = storage.clear_all().await.unwrap();

        assert_eq!(count, 2);

        let loaded = storage.load_records(None).await.unwrap();
        assert_eq!(loaded.len(), 0);
    }
}

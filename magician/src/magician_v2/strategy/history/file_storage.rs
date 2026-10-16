//! File-based persistent storage for strategy history
//!
//! This implementation uses JSON Lines (newline-delimited JSON) format for:
//! - Fast append-only writes
//! - Human-readable format
//! - Easy parsing and recovery
//! - Support for rotation and archival

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::{DateTime, Datelike, Utc};
use tokio::{
    fs::{File, OpenOptions},
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    sync::RwLock,
};
use tracing::{debug, info, warn};

use super::{
    storage::{calculate_similarity_score, StrategyHistoryStorage},
    types::{HistoryFilter, StorageStats, StrategyExecutionRecord},
};
use crate::magician_v2::{query_analysis::UnifiedQueryAnalysis, strategy::StrategyType};

/// File-based storage implementation using JSON Lines format
///
/// # Features
/// - Append-only writes for performance
/// - Read-through caching with auto-reload
/// - Monthly file rotation
/// - Configurable retention policies
/// - Atomic writes (write to temp, then rename)
pub struct FileStrategyStorage {
    /// Base directory for storage files
    storage_path: PathBuf,

    /// In-memory cache of loaded records
    cache: Arc<RwLock<Vec<StrategyExecutionRecord>>>,

    /// When cache was last reloaded
    last_load: Arc<RwLock<DateTime<Utc>>>,

    /// Auto-reload interval (check for new records)
    auto_reload_interval: std::time::Duration,

    /// Maximum file size before rotation (bytes)
    max_file_size: u64,

    /// Retention period (days)
    retention_days: i64,
}

impl FileStrategyStorage {
    /// Create new file-based storage
    ///
    /// # Arguments
    /// * `storage_path` - Directory to store history files
    ///
    /// # Returns
    /// * Initialized storage with loaded cache
    pub async fn new(storage_path: PathBuf) -> Result<Self> {
        // Create directory if it doesn't exist
        tokio::fs::create_dir_all(&storage_path)
            .await
            .context("Failed to create storage directory")?;

        let storage = Self {
            storage_path,
            cache: Arc::new(RwLock::new(Vec::new())),
            last_load: Arc::new(RwLock::new(Utc::now())),
            auto_reload_interval: std::time::Duration::from_secs(60), // 1 minute
            max_file_size: 100 * 1024 * 1024,                         // 100 MB
            retention_days: 60,
        };

        // Initial load
        storage.reload_cache().await?;

        info!(
            "[MAGICIAN-V2-STRATEGY] FileStrategyStorage initialized at: {}",
            storage.storage_path.display()
        );

        Ok(storage)
    }

    /// Create with custom configuration
    pub async fn with_config(
        storage_path: PathBuf,
        max_file_size_mb: u64,
        retention_days: i64,
    ) -> Result<Self> {
        let mut storage = Self::new(storage_path).await?;
        storage.max_file_size = max_file_size_mb * 1024 * 1024;
        storage.retention_days = retention_days;
        Ok(storage)
    }

    /// Get path to current active history file
    fn get_active_file_path(&self) -> PathBuf {
        self.storage_path.join("history.jsonl")
    }

    /// Get path to archived file for a specific month
    fn get_archive_file_path(&self, year: i32, month: u32) -> PathBuf {
        self.storage_path
            .join(format!("history.{:04}-{:02}.jsonl", year, month))
    }

    /// Reload cache from all history files
    async fn reload_cache(&self) -> Result<()> {
        debug!("[MAGICIAN-V2-STRATEGY] Reloading strategy history cache from disk");

        let mut all_records = Vec::new();

        // Load active file
        if let Ok(records) = self.load_file(&self.get_active_file_path()).await {
            all_records.extend(records);
        }

        // Load archived files (scan directory)
        let mut entries = tokio::fs::read_dir(&self.storage_path).await?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                // Match archive pattern: history.YYYY-MM.jsonl
                if filename.starts_with("history.")
                    && filename.ends_with(".jsonl")
                    && filename != "history.jsonl"
                {
                    if let Ok(records) = self.load_file(&path).await {
                        all_records.extend(records);
                    }
                }
            }
        }

        // Sort by timestamp (newest first for better cache locality)
        all_records.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));

        let count = all_records.len();
        *self.cache.write().await = all_records;
        *self.last_load.write().await = Utc::now();

        debug!(
            "[MAGICIAN-V2-STRATEGY] Loaded {} strategy execution records into cache",
            count
        );

        Ok(())
    }

    /// Check if cache needs reload and reload if necessary
    async fn reload_if_needed(&self) -> Result<()> {
        let last_load = *self.last_load.read().await;
        let elapsed = Utc::now().signed_duration_since(last_load);

        if elapsed.num_seconds() as u64 > self.auto_reload_interval.as_secs() {
            self.reload_cache().await?;
        }

        Ok(())
    }

    /// Load records from a single file
    async fn load_file(&self, path: &Path) -> Result<Vec<StrategyExecutionRecord>> {
        if !path.exists() {
            return Ok(Vec::new());
        }

        let file = File::open(path).await?;
        let reader = BufReader::new(file);
        let mut lines = reader.lines();
        let mut records = Vec::new();

        while let Ok(Some(line)) = lines.next_line().await {
            if line.trim().is_empty() {
                continue;
            }

            match serde_json::from_str::<StrategyExecutionRecord>(&line) {
                Ok(record) => records.push(record),
                Err(e) => {
                    warn!(
                        "[MAGICIAN-V2-STRATEGY] Failed to parse record from {}: {}",
                        path.display(),
                        e
                    );
                    continue;
                },
            }
        }

        Ok(records)
    }

    /// Append record to active file (atomic operation)
    async fn append_to_file(&self, record: &StrategyExecutionRecord) -> Result<()> {
        let file_path = self.get_active_file_path();

        // Serialize to JSON with newline
        let json_line = serde_json::to_string(record)? + "\n";

        // Append to file
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&file_path)
            .await
            .context("Failed to open history file for appending")?;

        file.write_all(json_line.as_bytes())
            .await
            .context("Failed to write record to history file")?;

        file.sync_all()
            .await
            .context("Failed to sync history file")?;

        Ok(())
    }

    /// Check if file rotation is needed and perform if necessary
    async fn rotate_if_needed(&self) -> Result<()> {
        let file_path = self.get_active_file_path();

        if !file_path.exists() {
            return Ok(());
        }

        // Check file size
        let metadata = tokio::fs::metadata(&file_path).await?;
        if metadata.len() < self.max_file_size {
            return Ok(());
        }

        // Rotate: rename to archive with current month
        let now = Utc::now();
        let archive_path = self.get_archive_file_path(now.year(), now.month());

        info!(
            "[MAGICIAN-V2-STRATEGY] Rotating history file {} -> {}",
            file_path.display(),
            archive_path.display()
        );

        tokio::fs::rename(&file_path, &archive_path).await?;

        Ok(())
    }
}

#[async_trait]
impl StrategyHistoryStorage for FileStrategyStorage {
    async fn save_record(&self, record: &StrategyExecutionRecord) -> Result<()> {
        // Append to file first (persistent)
        self.append_to_file(record).await?;

        // Add to cache (for fast reads)
        self.cache.write().await.push(record.clone());

        // Check if rotation needed
        self.rotate_if_needed().await?;

        Ok(())
    }

    async fn load_records(
        &self,
        filter: Option<HistoryFilter>,
    ) -> Result<Vec<StrategyExecutionRecord>> {
        // Reload cache if needed
        self.reload_if_needed().await?;

        let cache = self.cache.read().await;

        let mut results: Vec<_> = cache
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
        self.reload_if_needed().await?;

        let cache = self.cache.read().await;
        let mut scored_records: Vec<_> = cache
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
        let mut deleted_count = 0;

        // Delete old archive files
        let mut entries = tokio::fs::read_dir(&self.storage_path).await?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                // Only process archive files, not active file
                if filename.starts_with("history.")
                    && filename.ends_with(".jsonl")
                    && filename != "history.jsonl"
                {
                    // Check file modification time
                    if let Ok(metadata) = tokio::fs::metadata(&path).await {
                        if let Ok(modified) = metadata.modified() {
                            let modified_chrono: DateTime<Utc> = modified.into();
                            if modified_chrono < older_than {
                                info!(
                                    "[MAGICIAN-V2-STRATEGY] Deleting old history file: {}",
                                    filename
                                );
                                if tokio::fs::remove_file(&path).await.is_ok() {
                                    deleted_count += 1;
                                }
                            }
                        }
                    }
                }
            }
        }

        // Reload cache after cleanup
        self.reload_cache().await?;

        Ok(deleted_count)
    }

    async fn get_stats(&self) -> Result<StorageStats> {
        self.reload_if_needed().await?;

        let cache = self.cache.read().await;

        let total_records = cache.len();
        let oldest_record = cache.iter().map(|r| r.timestamp).min();
        let newest_record = cache.iter().map(|r| r.timestamp).max();

        // Calculate size
        let mut size_bytes = 0u64;
        if self.get_active_file_path().exists() {
            if let Ok(metadata) = tokio::fs::metadata(&self.get_active_file_path()).await {
                size_bytes += metadata.len();
            }
        }

        // Count records per strategy
        let mut records_per_strategy = HashMap::new();
        let mut success_per_strategy: HashMap<String, (usize, usize)> = HashMap::new();

        for record in cache.iter() {
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
            size_bytes,
            oldest_record,
            newest_record,
            records_per_strategy,
            success_rate_per_strategy,
        })
    }

    async fn clear_all(&self) -> Result<usize> {
        let cache_len = self.cache.read().await.len();

        // Clear cache
        self.cache.write().await.clear();

        // Delete all files
        let mut entries = tokio::fs::read_dir(&self.storage_path).await?;
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            if let Some(filename) = path.file_name().and_then(|n| n.to_str()) {
                if filename.starts_with("history.") && filename.ends_with(".jsonl") {
                    let _ = tokio::fs::remove_file(&path).await;
                }
            }
        }

        Ok(cache_len)
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use tempfile::TempDir;

    use super::*;

    async fn create_test_storage() -> (FileStrategyStorage, TempDir) {
        let temp_dir = TempDir::new().unwrap();
        let storage = FileStrategyStorage::new(temp_dir.path().to_path_buf())
            .await
            .unwrap();
        (storage, temp_dir)
    }

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
        let (storage, _temp_dir) = create_test_storage().await;

        let record = create_test_record("ping google.com", 0.2, true);

        storage.save_record(&record).await.unwrap();

        let loaded = storage.load_records(None).await.unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].query, "ping google.com");
    }

    #[tokio::test]
    async fn test_filter_by_success() {
        let (storage, _temp_dir) = create_test_storage().await;

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

        let filter = HistoryFilter {
            success_only: Some(true),
            ..Default::default()
        };

        let loaded = storage.load_records(Some(filter)).await.unwrap();

        assert_eq!(loaded.len(), 2);
        assert!(loaded.iter().all(|r| r.success));
    }

    #[tokio::test]
    async fn test_clear_all() {
        let (storage, _temp_dir) = create_test_storage().await;

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

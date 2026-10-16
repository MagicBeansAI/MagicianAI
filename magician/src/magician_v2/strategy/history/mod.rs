//! Strategy history tracking module
//!
//! This module provides:
//! - Historical tracking of strategy execution patterns
//! - Persistent storage with JSON Lines format
//! - In-memory storage for testing
//! - Similarity matching for query patterns

pub mod file_storage;
pub mod memory_storage;
pub mod storage;
pub mod store;
pub mod types;

// Re-export main types
pub use storage::{calculate_similarity_score, StrategyHistoryStorage};
pub use store::StrategyHistoryStore;
pub use types::{HistoryFilter, StorageStats, StrategyExecutionRecord};

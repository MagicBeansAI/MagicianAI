//! Question Batch Tracker
//!
//! Tracks the state of question batches to determine when all questions
//! in a batch have been answered, enabling smart resume decisions.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use tracing::{debug, info, warn};

use crate::magician_v2::slot_graph::SlotRecord;

/// Tracks the state of question batches across workflows
pub struct QuestionBatchTracker {
    batches: DashMap<String, BatchState>,
}

/// State of a single batch of questions
#[derive(Debug, Clone)]
pub struct BatchState {
    /// Unique identifier for this batch
    pub batch_id: String,

    /// Workflow this batch belongs to
    pub workflow_id: String,

    /// All question IDs in this batch
    pub question_ids: Vec<String>,

    /// Question IDs that have been answered
    pub answered_ids: HashSet<String>,

    /// Total number of questions in batch
    pub total_count: usize,

    /// Number of questions answered so far
    pub answered_count: usize,

    /// When this batch was created
    pub created_at: DateTime<Utc>,

    /// Optional metadata for debugging/monitoring
    pub metadata: Option<BatchMetadata>,

    /// Slot records extracted from each answered question (Gap #9, #13 fix)
    /// Maps question_id -> Vec<SlotRecord>
    pub answered_slots: HashMap<String, Vec<SlotRecord>>,
}

/// Optional metadata for batch tracking
#[derive(Debug, Clone)]
pub struct BatchMetadata {
    /// What triggered this batch (e.g., "upfront_elicitation", "planning_discovery")
    pub source: String,

    /// Planning iteration that created this batch (for ask-plan-ask loop)
    pub iteration: Option<usize>,

    /// Average priority of questions in batch
    pub avg_priority: Option<f64>,
}

impl QuestionBatchTracker {
    /// Create a new batch tracker
    pub fn new() -> Self {
        Self {
            batches: DashMap::new(),
        }
    }

    /// Register a new batch of questions
    ///
    /// # Arguments
    /// * `batch_id` - Unique identifier for this batch
    /// * `workflow_id` - Workflow this batch belongs to
    /// * `question_ids` - List of question IDs in this batch
    /// * `metadata` - Optional metadata for debugging/monitoring
    pub fn register_batch(
        &self,
        batch_id: String,
        workflow_id: String,
        question_ids: Vec<String>,
        metadata: Option<BatchMetadata>,
    ) {
        let total_count = question_ids.len();

        let state = BatchState {
            batch_id: batch_id.clone(),
            workflow_id: workflow_id.clone(),
            question_ids: question_ids.clone(),
            answered_ids: HashSet::new(),
            total_count,
            answered_count: 0,
            created_at: Utc::now(),
            metadata,
            answered_slots: HashMap::new(), // Gap #13 fix: Initialize empty map
        };

        self.batches.insert(batch_id.clone(), state);

        info!(
            "[BATCH-TRACKER] Registered batch {} with {} questions for workflow {}",
            batch_id, total_count, workflow_id
        );
    }

    /// Mark a question as answered in its batch
    ///
    /// Returns the new answered count, or None if batch not found
    pub fn mark_answered(&self, batch_id: &str, question_id: &str) -> Option<usize> {
        let mut entry = self.batches.get_mut(batch_id)?;

        if !entry.answered_ids.contains(question_id) {
            entry.answered_ids.insert(question_id.to_string());
            entry.answered_count += 1;

            debug!(
                "[BATCH-TRACKER] Batch {} progress: {}/{}",
                batch_id, entry.answered_count, entry.total_count
            );
        } else {
            warn!(
                "[BATCH-TRACKER] Question {} already marked as answered in batch {}",
                question_id, batch_id
            );
        }

        Some(entry.answered_count)
    }

    /// Check if all questions in a batch have been answered
    pub fn is_batch_complete(&self, batch_id: &str) -> bool {
        self.batches
            .get(batch_id)
            .map(|entry| {
                let complete = entry.answered_count >= entry.total_count;
                if complete {
                    info!(
                        "[BATCH-TRACKER] Batch {} complete: {}/{} questions answered",
                        batch_id, entry.answered_count, entry.total_count
                    );
                }
                complete
            })
            .unwrap_or(false)
    }

    /// Get batch progress (answered_count, total_count)
    ///
    /// Returns None if batch not found
    pub fn get_progress(&self, batch_id: &str) -> Option<(usize, usize)> {
        self.batches
            .get(batch_id)
            .map(|entry| (entry.answered_count, entry.total_count))
    }

    /// Get the percentage of completion (0.0 to 1.0)
    pub fn get_completion_percentage(&self, batch_id: &str) -> Option<f64> {
        self.batches.get(batch_id).map(|entry| {
            if entry.total_count == 0 {
                1.0 // Empty batch is "complete"
            } else {
                entry.answered_count as f64 / entry.total_count as f64
            }
        })
    }

    /// Get remaining (unanswered) question IDs in a batch
    pub fn get_remaining_questions(&self, batch_id: &str) -> Option<Vec<String>> {
        self.batches.get(batch_id).map(|entry| {
            entry
                .question_ids
                .iter()
                .filter(|q_id| !entry.answered_ids.contains(*q_id))
                .cloned()
                .collect()
        })
    }

    /// Find batch ID for a specific question
    ///
    /// Returns None if question not found in any batch
    pub fn find_batch_for_question(&self, question_id: &str) -> Option<String> {
        self.batches
            .iter()
            .find(|entry| entry.question_ids.contains(&question_id.to_string()))
            .map(|entry| entry.batch_id.clone())
    }

    /// Get all batches for a specific workflow
    pub fn get_workflow_batches(&self, workflow_id: &str) -> Vec<BatchState> {
        self.batches
            .iter()
            .filter(|entry| entry.workflow_id == workflow_id)
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Remove a batch (cleanup after completion or cancellation)
    ///
    /// Returns the removed batch state if it existed
    pub fn remove_batch(&self, batch_id: &str) -> Option<BatchState> {
        let state = self.batches.remove(batch_id).map(|(_, state)| state);

        if let Some(ref s) = state {
            info!(
                "[BATCH-TRACKER] Removed batch {} ({}/{} answered)",
                batch_id, s.answered_count, s.total_count
            );
        }

        state
    }

    /// Get total number of active batches
    pub fn active_batch_count(&self) -> usize {
        self.batches.len()
    }

    /// Store slot records for a specific question (Gap #9 fix)
    ///
    /// This is called after parsing each answer to preserve the extracted slots.
    /// The slots are stored in the batch tracker so they can be collected when
    /// the entire batch is complete.
    pub fn store_question_slots(&self, batch_id: &str, question_id: &str, slots: Vec<SlotRecord>) {
        if let Some(mut entry) = self.batches.get_mut(batch_id) {
            entry
                .answered_slots
                .insert(question_id.to_string(), slots.clone());

            // LOCATION 1: Enhanced logging when storing slots for a question
            info!(
                "[MAGICIAN-BATCH-TRACKER] 💾 STORED SLOTS for question in batch {}\n\
                 - Question ID: {}\n\
                 - Slots stored: {}\n\
                 - Slot details: {:?}\n\
                 - Total answered questions with slots: {}\n\
                 - Batch progress: {}/{} questions answered",
                batch_id,
                question_id,
                slots.len(),
                slots
                    .iter()
                    .map(|s| format!("{} = {:?} (conf: {:.2})", s.id, s.value, s.confidence))
                    .collect::<Vec<_>>(),
                entry.answered_slots.len(),
                entry.answered_count,
                entry.total_count
            );
        } else {
            warn!(
                "[MAGICIAN-BATCH-TRACKER] ❌ Tried to store slots for question {} in non-existent batch {}",
                question_id, batch_id
            );
        }
    }

    /// Collect all slots from all answered questions in a batch (Gap #9 fix)
    ///
    /// Returns a flattened vector of all SlotRecords from all questions.
    /// This is used when the batch is complete to gather all information for query rewriting.
    pub fn collect_batch_slots(&self, batch_id: &str) -> Vec<SlotRecord> {
        self.batches
            .get(batch_id)
            .map(|entry| {
                let slots: Vec<SlotRecord> =
                    entry.answered_slots.values().flatten().cloned().collect();

                // LOCATION 2: Enhanced logging when collecting batch slots
                info!(
                    "[MAGICIAN-BATCH-TRACKER] 📦 COLLECTED BATCH SLOTS for batch {}\n\
                     - Total slots collected: {}\n\
                     - Questions with slots: {}\n\
                     - Question IDs: {:?}\n\
                     - Batch completion: {}/{} questions\n\
                     - Collected slot details: {:?}",
                    batch_id,
                    slots.len(),
                    entry.answered_slots.len(),
                    entry.answered_slots.keys().collect::<Vec<_>>(),
                    entry.answered_count,
                    entry.total_count,
                    slots
                        .iter()
                        .map(|s| format!("{} = {:?} (conf: {:.2})", s.id, s.value, s.confidence))
                        .collect::<Vec<_>>()
                );

                slots
            })
            .unwrap_or_default()
    }

    /// Get number of answered questions in a batch (Gap #12 fix)
    pub fn answered_count(&self, batch_id: &str) -> usize {
        self.batches
            .get(batch_id)
            .map(|entry| entry.answered_ids.len())
            .unwrap_or(0)
    }

    /// Get total number of questions in a batch (Gap #12 fix)
    pub fn total_count(&self, batch_id: &str) -> usize {
        self.batches
            .get(batch_id)
            .map(|entry| entry.total_count)
            .unwrap_or(0)
    }

    /// Invalidate a batch when correction requires replanning (Gap #6 fix)
    ///
    /// Called when a user provides a correction that invalidates the entire batch.
    /// Removes the batch and returns the count of cancelled questions.
    pub fn invalidate_batch(&self, batch_id: &str) -> usize {
        if let Some(batch) = self.batches.get_mut(batch_id) {
            // Calculate remaining unanswered questions using correct field names
            let remaining = batch.total_count - batch.answered_ids.len();
            drop(batch); // Release mutable borrow before removal
            self.batches.remove(batch_id);
            info!(
                "[BATCH-TRACKER] Batch {} invalidated - {} questions cancelled",
                batch_id, remaining
            );
            remaining
        } else {
            warn!(
                "[BATCH-TRACKER] Tried to invalidate non-existent batch {}",
                batch_id
            );
            0
        }
    }

    /// Remove all batches associated with a workflow. Used when a replan supersedes previous batches.
    pub fn clear_workflow_batches(&self, workflow_id: &str) {
        let batch_ids: Vec<String> = self
            .batches
            .iter()
            .filter(|entry| entry.workflow_id == workflow_id)
            .map(|entry| entry.batch_id.clone())
            .collect();

        if batch_ids.is_empty() {
            return;
        }

        info!(
            "[BATCH-TRACKER] Clearing {} existing batches for workflow {}",
            batch_ids.len(),
            workflow_id
        );

        for batch_id in batch_ids {
            self.batches.remove(&batch_id);
        }
    }

    /// Get statistics for monitoring
    pub fn get_stats(&self) -> BatchTrackerStats {
        let mut stats = BatchTrackerStats {
            total_batches: 0,
            complete_batches: 0,
            total_questions: 0,
            answered_questions: 0,
            avg_completion: 0.0,
        };

        let mut completion_sum = 0.0;

        for entry in self.batches.iter() {
            stats.total_batches += 1;
            stats.total_questions += entry.total_count;
            stats.answered_questions += entry.answered_count;

            if entry.answered_count >= entry.total_count {
                stats.complete_batches += 1;
            }

            completion_sum += entry.answered_count as f64 / entry.total_count.max(1) as f64;
        }

        if stats.total_batches > 0 {
            stats.avg_completion = completion_sum / stats.total_batches as f64;
        }

        stats
    }
}

impl Default for QuestionBatchTracker {
    fn default() -> Self {
        Self::new()
    }
}

/// Statistics for monitoring batch tracker health
#[derive(Debug, Clone)]
pub struct BatchTrackerStats {
    pub total_batches: usize,
    pub complete_batches: usize,
    pub total_questions: usize,
    pub answered_questions: usize,
    pub avg_completion: f64,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::slot_graph::SlotType;
    use serde_json::json;

    fn make_slot(id: &str, value: &str, confidence: f64) -> SlotRecord {
        let now = Utc::now();
        SlotRecord {
            id: id.to_string(),
            slot_type: SlotType::Modifier,
            value: json!(value),
            confidence,
            provenance: Vec::new(),
            evidence_links: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn test_batch_registration() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string(), "q3".to_string()],
            None,
        );

        let (answered, total) = tracker.get_progress("batch-1").unwrap();
        assert_eq!(answered, 0);
        assert_eq!(total, 3);
        assert!(!tracker.is_batch_complete("batch-1"));
    }

    #[test]
    fn test_batch_completion_detection() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string(), "q3".to_string()],
            None,
        );

        assert!(!tracker.is_batch_complete("batch-1"));

        tracker.mark_answered("batch-1", "q1");
        assert!(!tracker.is_batch_complete("batch-1"));

        tracker.mark_answered("batch-1", "q2");
        assert!(!tracker.is_batch_complete("batch-1"));

        tracker.mark_answered("batch-1", "q3");
        assert!(tracker.is_batch_complete("batch-1"));
    }

    #[test]
    fn test_completion_percentage() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec![
                "q1".to_string(),
                "q2".to_string(),
                "q3".to_string(),
                "q4".to_string(),
            ],
            None,
        );

        assert_eq!(tracker.get_completion_percentage("batch-1").unwrap(), 0.0);

        tracker.mark_answered("batch-1", "q1");
        assert_eq!(tracker.get_completion_percentage("batch-1").unwrap(), 0.25);

        tracker.mark_answered("batch-1", "q2");
        assert_eq!(tracker.get_completion_percentage("batch-1").unwrap(), 0.5);

        tracker.mark_answered("batch-1", "q3");
        tracker.mark_answered("batch-1", "q4");
        assert_eq!(tracker.get_completion_percentage("batch-1").unwrap(), 1.0);
    }

    #[test]
    fn test_remaining_questions() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string(), "q3".to_string()],
            None,
        );

        let remaining = tracker.get_remaining_questions("batch-1").unwrap();
        assert_eq!(remaining.len(), 3);

        tracker.mark_answered("batch-1", "q1");
        let remaining = tracker.get_remaining_questions("batch-1").unwrap();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.contains(&"q1".to_string()));
        assert!(remaining.contains(&"q2".to_string()));
        assert!(remaining.contains(&"q3".to_string()));
    }

    #[test]
    fn test_find_batch_for_question() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string()],
            None,
        );

        tracker.register_batch(
            "batch-2".to_string(),
            "workflow-1".to_string(),
            vec!["q3".to_string(), "q4".to_string()],
            None,
        );

        assert_eq!(tracker.find_batch_for_question("q1").unwrap(), "batch-1");
        assert_eq!(tracker.find_batch_for_question("q3").unwrap(), "batch-2");
        assert!(tracker.find_batch_for_question("q5").is_none());
    }

    #[test]
    fn test_duplicate_answer_handling() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string()],
            None,
        );

        assert_eq!(tracker.mark_answered("batch-1", "q1").unwrap(), 1);
        // Answering same question again shouldn't increase count
        assert_eq!(tracker.mark_answered("batch-1", "q1").unwrap(), 1);

        assert_eq!(tracker.mark_answered("batch-1", "q2").unwrap(), 2);
        assert!(tracker.is_batch_complete("batch-1"));
    }

    #[test]
    fn test_batch_removal() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string()],
            None,
        );

        assert!(tracker.get_progress("batch-1").is_some());

        let removed = tracker.remove_batch("batch-1").unwrap();
        assert_eq!(removed.batch_id, "batch-1");

        assert!(tracker.get_progress("batch-1").is_none());
    }

    #[test]
    fn test_stats_calculation() {
        let tracker = QuestionBatchTracker::new();

        tracker.register_batch(
            "batch-1".to_string(),
            "workflow-1".to_string(),
            vec!["q1".to_string(), "q2".to_string()],
            None,
        );

        tracker.register_batch(
            "batch-2".to_string(),
            "workflow-1".to_string(),
            vec!["q3".to_string(), "q4".to_string()],
            None,
        );

        tracker.mark_answered("batch-1", "q1");
        tracker.mark_answered("batch-1", "q2");

        let stats = tracker.get_stats();
        assert_eq!(stats.total_batches, 2);
        assert_eq!(stats.complete_batches, 1);
        assert_eq!(stats.total_questions, 4);
        assert_eq!(stats.answered_questions, 2);
        assert_eq!(stats.avg_completion, 0.5); // (1.0 + 0.0) / 2
    }

    #[test]
    fn test_store_and_collect_slots_accumulates_across_questions() {
        let tracker = QuestionBatchTracker::new();
        tracker.register_batch(
            "batch-1".into(),
            "workflow-1".into(),
            vec!["q1".into(), "q2".into()],
            None,
        );

        tracker.mark_answered("batch-1", "q1");
        tracker.store_question_slots(
            "batch-1",
            "q1",
            vec![
                make_slot("slot-a", "value-a", 0.8),
                make_slot("slot-b", "value-b", 0.9),
            ],
        );
        tracker.mark_answered("batch-1", "q2");
        tracker.store_question_slots("batch-1", "q2", vec![make_slot("slot-c", "value-c", 0.7)]);

        let collected = tracker.collect_batch_slots("batch-1");
        assert_eq!(collected.len(), 3);
        assert!(collected.iter().any(|slot| slot.id == "slot-a"));
        assert!(collected.iter().any(|slot| slot.id == "slot-b"));
        assert!(collected.iter().any(|slot| slot.id == "slot-c"));
    }

    #[test]
    fn test_store_question_slots_overwrites_existing_entry() {
        let tracker = QuestionBatchTracker::new();
        tracker.register_batch(
            "batch-1".into(),
            "workflow-1".into(),
            vec!["q1".into()],
            None,
        );

        tracker.mark_answered("batch-1", "q1");
        tracker.store_question_slots("batch-1", "q1", vec![make_slot("slot-a", "first", 0.5)]);
        tracker.store_question_slots("batch-1", "q1", vec![make_slot("slot-a", "updated", 0.9)]);

        let collected = tracker.collect_batch_slots("batch-1");
        assert_eq!(collected.len(), 1);
        assert_eq!(collected[0].value, json!("updated"));
        assert!((collected[0].confidence - 0.9).abs() < f64::EPSILON);
    }
}

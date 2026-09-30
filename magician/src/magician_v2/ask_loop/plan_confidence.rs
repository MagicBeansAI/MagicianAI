//! Plan Confidence Tracking
//!
//! Tracks confidence evolution through the iterative ask-plan-ask loop.
//! Helps determine when enough information has been gathered to proceed with execution.

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, info_span};

/// Tracks confidence evolution for a single workflow through planning iterations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanConfidenceSnapshot {
    /// Planning iteration number (0 = initial plan)
    pub iteration: usize,

    /// Overall plan confidence (0.0-1.0)
    pub overall_confidence: f64,

    /// Confidence per unresolved parameter
    pub parameter_confidence: std::collections::HashMap<String, f64>,

    /// What triggered this iteration (InitialPlan, UserAnswer, Correction, Discovery)
    pub trigger: ConfidenceTrigger,

    /// Change in confidence from previous iteration
    pub confidence_delta: Option<f64>,

    /// Number of unresolved parameters
    pub unresolved_count: usize,

    /// Timestamp of this snapshot
    pub timestamp: DateTime<Utc>,

    /// Optional notes about this iteration
    pub notes: Option<String>,

    /// Detailed deltas for slots touched in this iteration
    #[serde(default)]
    pub slot_deltas: Vec<SlotConfidenceDelta>,
}

/// Telemetry describing how a single slot's confidence changed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotConfidenceDelta {
    pub slot_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<f64>,
    pub updated: f64,
}

/// What triggered a planning iteration
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConfidenceTrigger {
    /// Initial plan generation
    InitialPlan,

    /// User provided answer to question
    UserAnswer { question_id: String },

    /// User corrected previous information
    Correction { question_id: String },

    /// Discovery from answer led to replanning
    Discovery { question_id: String },

    /// Manual replanning requested
    ManualReplan,
}

/// Confidence threshold configuration
#[derive(Debug, Clone)]
pub struct ConfidenceThresholds {
    /// Minimum overall confidence to proceed (default: 0.7)
    pub min_overall: f64,

    /// Minimum confidence for critical parameters (default: 0.8)
    pub min_critical: f64,

    /// Maximum allowed unresolved critical parameters (default: 0)
    pub max_unresolved_critical: usize,

    /// Number of consecutive evaluations that must meet thresholds
    pub stability_margin: usize,
}

impl Default for ConfidenceThresholds {
    fn default() -> Self {
        Self {
            min_overall: 0.7,
            min_critical: 0.8,
            max_unresolved_critical: 0,
            stability_margin: 1,
        }
    }
}

/// Service for tracking plan confidence across workflows
pub struct PlanConfidenceTracker {
    /// Confidence snapshots per workflow
    snapshots: DashMap<String, Vec<PlanConfidenceSnapshot>>,

    /// Confidence thresholds
    thresholds: ConfidenceThresholds,
}

impl PlanConfidenceTracker {
    /// Create a new plan confidence tracker
    pub fn new(thresholds: ConfidenceThresholds) -> Self {
        Self {
            snapshots: DashMap::new(),
            thresholds,
        }
    }

    /// Create with default thresholds
    pub fn with_defaults() -> Self {
        Self::new(ConfidenceThresholds::default())
    }

    /// Record a confidence snapshot for a workflow
    pub fn record_snapshot(&self, workflow_id: &str, snapshot: PlanConfidenceSnapshot) {
        let span = info_span!(
            "plan_confidence.record_snapshot",
            workflow_id = workflow_id,
            iteration = snapshot.iteration,
            trigger = ?snapshot.trigger
        );
        let _enter = span.enter();
        let mut entry = self
            .snapshots
            .entry(workflow_id.to_string())
            .or_insert_with(Vec::new);

        info!(
            "[PLAN-CONFIDENCE] Iteration {} for workflow {}: overall={:.2}, unresolved={}, trigger={:?}",
            snapshot.iteration,
            workflow_id,
            snapshot.overall_confidence,
            snapshot.unresolved_count,
            snapshot.trigger
        );

        if !snapshot.slot_deltas.is_empty() {
            for delta in &snapshot.slot_deltas {
                debug!(
                    "[PLAN-CONFIDENCE] Slot {} confidence {:?} -> {:.2}",
                    delta.slot_id,
                    delta.previous.map(|v| format!("{:.2}", v)),
                    delta.updated
                );
            }
        }

        if let Some(delta) = snapshot.confidence_delta {
            if delta > 0.0 {
                info!(
                    "[PLAN-CONFIDENCE] Confidence improved by {:.2} (+{:.1}%)",
                    delta,
                    delta * 100.0
                );
            } else if delta < 0.0 {
                info!(
                    "[PLAN-CONFIDENCE] Confidence decreased by {:.2} ({:.1}%)",
                    delta.abs(),
                    delta * 100.0
                );
            }
        }

        entry.push(snapshot);
    }

    /// Get the latest confidence snapshot for a workflow
    pub fn get_latest(&self, workflow_id: &str) -> Option<PlanConfidenceSnapshot> {
        self.snapshots
            .get(workflow_id)
            .and_then(|snapshots| snapshots.last().cloned())
    }

    /// Get all confidence snapshots for a workflow
    pub fn get_history(&self, workflow_id: &str) -> Vec<PlanConfidenceSnapshot> {
        self.snapshots
            .get(workflow_id)
            .map(|snapshots| snapshots.clone())
            .unwrap_or_default()
    }

    /// Get the confidence trend (positive = improving, negative = degrading)
    pub fn get_trend(&self, workflow_id: &str) -> Option<f64> {
        let snapshots = self.snapshots.get(workflow_id)?;

        if snapshots.len() < 2 {
            return None;
        }

        // Simple linear trend: (latest - first) / iterations
        let first = snapshots.first()?.overall_confidence;
        let latest = snapshots.last()?.overall_confidence;
        let iterations = snapshots.len() as f64;

        Some((latest - first) / iterations)
    }

    /// Check if confidence is sufficient to proceed with execution
    pub fn is_sufficient(&self, workflow_id: &str) -> bool {
        let Some(entry) = self.snapshots.get(workflow_id) else {
            debug!(
                "[PLAN-CONFIDENCE] No confidence data for workflow {}",
                workflow_id
            );
            return false;
        };

        if entry.is_empty() {
            return false;
        }

        let latest = entry.last().expect("entry.is_empty() checked");
        if !self.snapshot_meets_thresholds(latest) {
            debug!(
                "[PLAN-CONFIDENCE] Latest snapshot below threshold for workflow {} (overall={:.2}, required={:.2})",
                workflow_id, latest.overall_confidence, self.thresholds.min_overall
            );
            return false;
        }

        let required = self.thresholds.stability_margin.max(1);
        if entry.len() < required {
            debug!(
                "[PLAN-CONFIDENCE] Workflow {} needs {} consecutive stable snapshots but only has {}",
                workflow_id,
                required,
                entry.len()
            );
            return false;
        }

        if required > 1 {
            let start = entry.len().saturating_sub(required);
            if entry[start..]
                .iter()
                .any(|snapshot| !self.snapshot_meets_thresholds(snapshot))
            {
                debug!(
                    "[PLAN-CONFIDENCE] Workflow {} has snapshots that failed the stability margin check (required {})",
                    workflow_id, required
                );
                return false;
            }
        }

        info!(
            "[PLAN-CONFIDENCE] Confidence sufficient for workflow {}: overall={:.2}, unresolved={}",
            workflow_id, latest.overall_confidence, latest.unresolved_count
        );
        true
    }

    fn snapshot_meets_thresholds(&self, snapshot: &PlanConfidenceSnapshot) -> bool {
        if snapshot.overall_confidence < self.thresholds.min_overall {
            return false;
        }

        let low_critical_params = snapshot
            .parameter_confidence
            .iter()
            .filter(|(_, &conf)| conf < self.thresholds.min_critical)
            .count();

        low_critical_params <= self.thresholds.max_unresolved_critical
    }

    /// Get confidence statistics for a workflow
    pub fn get_stats(&self, workflow_id: &str) -> Option<ConfidenceStats> {
        let snapshots = self.snapshots.get(workflow_id)?;

        if snapshots.is_empty() {
            return None;
        }

        let initial = snapshots.first()?;
        let latest = snapshots.last()?;

        let corrections_count = snapshots
            .iter()
            .filter(|s| matches!(s.trigger, ConfidenceTrigger::Correction { .. }))
            .count();

        let discoveries_count = snapshots
            .iter()
            .filter(|s| matches!(s.trigger, ConfidenceTrigger::Discovery { .. }))
            .count();

        Some(ConfidenceStats {
            total_iterations: snapshots.len(),
            initial_confidence: initial.overall_confidence,
            current_confidence: latest.overall_confidence,
            confidence_improvement: latest.overall_confidence - initial.overall_confidence,
            corrections_count,
            discoveries_count,
            is_sufficient: self.is_sufficient(workflow_id),
        })
    }

    /// Clear confidence history for a workflow
    pub fn clear(&self, workflow_id: &str) {
        self.snapshots.remove(workflow_id);
        debug!(
            "[PLAN-CONFIDENCE] Cleared history for workflow {}",
            workflow_id
        );
    }
}

/// Confidence statistics for a workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfidenceStats {
    pub total_iterations: usize,
    pub initial_confidence: f64,
    pub current_confidence: f64,
    pub confidence_improvement: f64,
    pub corrections_count: usize,
    pub discoveries_count: usize,
    pub is_sufficient: bool,
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn test_confidence_tracking() {
        let tracker = PlanConfidenceTracker::with_defaults();
        let workflow_id = "test_workflow";

        // Initial plan
        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 0,
                overall_confidence: 0.5,
                parameter_confidence: [("param1".to_string(), 0.3), ("param2".to_string(), 0.6)]
                    .into_iter()
                    .collect(),
                trigger: ConfidenceTrigger::InitialPlan,
                confidence_delta: None,
                unresolved_count: 2,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        // After first answer
        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 1,
                overall_confidence: 0.82,
                parameter_confidence: [("param1".to_string(), 0.9), ("param2".to_string(), 0.85)]
                    .into_iter()
                    .collect(),
                trigger: ConfidenceTrigger::UserAnswer {
                    question_id: "q1".to_string(),
                },
                confidence_delta: Some(0.32),
                unresolved_count: 1,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        // Check latest
        let latest = tracker.get_latest(workflow_id).unwrap();
        assert_eq!(latest.iteration, 1);
        assert!((latest.overall_confidence - 0.82).abs() < 1e-6);

        // Check trend
        let trend = tracker.get_trend(workflow_id).unwrap();
        assert!(trend > 0.0); // Confidence is improving

        // Check stats
        let stats = tracker.get_stats(workflow_id).unwrap();
        assert_eq!(stats.total_iterations, 2);
        assert!((stats.confidence_improvement - 0.32).abs() < 1e-6);
        assert!(stats.is_sufficient); // Above 0.7 threshold
    }

    #[test]
    fn test_confidence_thresholds() {
        let tracker = PlanConfidenceTracker::new(ConfidenceThresholds {
            min_overall: 0.8,
            min_critical: 0.9,
            max_unresolved_critical: 0,
            stability_margin: 1,
        });

        let workflow_id = "test_workflow";

        // Below threshold
        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 0,
                overall_confidence: 0.75,
                parameter_confidence: [("param1".to_string(), 0.85)].into_iter().collect(),
                trigger: ConfidenceTrigger::InitialPlan,
                confidence_delta: None,
                unresolved_count: 1,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        assert!(!tracker.is_sufficient(workflow_id));

        // Above threshold
        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 1,
                overall_confidence: 0.85,
                parameter_confidence: [("param1".to_string(), 0.95)].into_iter().collect(),
                trigger: ConfidenceTrigger::UserAnswer {
                    question_id: "q1".to_string(),
                },
                confidence_delta: Some(0.1),
                unresolved_count: 0,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        assert!(tracker.is_sufficient(workflow_id));
    }

    #[test]
    fn test_stability_margin_requires_consecutive_snapshots() {
        let tracker = PlanConfidenceTracker::new(ConfidenceThresholds {
            min_overall: 0.8,
            min_critical: 0.75,
            max_unresolved_critical: 0,
            stability_margin: 2,
        });

        let workflow_id = "stability_workflow";

        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 0,
                overall_confidence: 0.82,
                parameter_confidence: [("param1".to_string(), 0.9)].into_iter().collect(),
                trigger: ConfidenceTrigger::InitialPlan,
                confidence_delta: None,
                unresolved_count: 0,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        assert!(
            !tracker.is_sufficient(workflow_id),
            "single snapshot should not satisfy stability margin"
        );

        tracker.record_snapshot(
            workflow_id,
            PlanConfidenceSnapshot {
                iteration: 1,
                overall_confidence: 0.85,
                parameter_confidence: [("param1".to_string(), 0.92)].into_iter().collect(),
                trigger: ConfidenceTrigger::UserAnswer {
                    question_id: "q1".to_string(),
                },
                confidence_delta: Some(0.03),
                unresolved_count: 0,
                timestamp: Utc::now(),
                notes: None,
                slot_deltas: Vec::new(),
            },
        );

        assert!(
            tracker.is_sufficient(workflow_id),
            "two consecutive stable snapshots should satisfy the margin"
        );
    }
}

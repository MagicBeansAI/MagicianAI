use std::collections::HashSet;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::slot_graph::{ProvenanceRecord, ProvenanceSource, SlotRecord, SlotType};

/// Calculates confidence metrics for slots and workflows.
#[derive(Debug, Clone)]
pub struct ConfidenceService {
    config: ConfidenceConfig,
}

impl ConfidenceService {
    pub fn new(config: ConfidenceConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &ConfidenceConfig {
        &self.config
    }

    /// Calculate confidence for a single slot by combining stored confidence and provenance.
    pub fn calculate_slot_confidence(&self, slot: &SlotRecord) -> f64 {
        let base = slot.confidence.clamp(0.0, 1.0);

        if slot.provenance.is_empty() {
            return base;
        }

        let provenance_score =
            aggregate_provenance_confidence(&slot.provenance, &self.config.weights);
        ((base + provenance_score) / 2.0).clamp(0.0, 1.0)
    }

    /// Aggregate slot confidences into an overall workflow confidence.
    pub fn calculate_overall_confidence(&self, slots: &[SlotRecord]) -> f64 {
        if slots.is_empty() {
            return 0.0;
        }

        let critical: HashSet<_> = self.config.critical_slots.iter().cloned().collect();

        let mut weighted_sum = 0.0;
        let mut total_weight = 0.0;

        for slot in slots {
            let score = self.calculate_slot_confidence(slot);
            let weight = if critical.contains(&slot.slot_type) {
                2.0
            } else {
                1.0
            };
            weighted_sum += score * weight;
            total_weight += weight;
        }

        if total_weight == 0.0 {
            0.0
        } else {
            (weighted_sum / total_weight).clamp(0.0, 1.0)
        }
    }

    /// Calculate a confidence slope over time using linear regression.
    pub fn calculate_confidence_slope(&self, history: &[(DateTime<Utc>, f64)]) -> Option<f64> {
        let min_samples = self.config.min_samples_for_slope.max(2);
        if history.len() < min_samples {
            return None;
        }

        let window = self.config.slope_window_size.max(min_samples);
        let take = history.len().min(window);
        let slice = &history[history.len() - take..];

        let first_ts = slice.first()?.0;
        let mut times: Vec<f64> = slice
            .iter()
            .map(|(ts, _)| ts.signed_duration_since(first_ts).num_milliseconds() as f64 / 1_000.0)
            .collect();
        // Fall back to evenly spaced samples if timestamps are identical (same millisecond),
        // otherwise the regression denominator becomes zero and we lose the slope signal.
        let all_zero_times = times.iter().all(|t| t.abs() < f64::EPSILON);
        if all_zero_times {
            times = (0..times.len()).map(|idx| idx as f64).collect();
        }
        let values: Vec<f64> = slice.iter().map(|(_, v)| *v).collect();

        let mean_x = times.iter().sum::<f64>() / times.len() as f64;
        let mean_y = values.iter().sum::<f64>() / values.len() as f64;

        let mut numerator = 0.0;
        let mut denominator = 0.0;

        for (x, y) in times.into_iter().zip(values.into_iter()) {
            let dx = x - mean_x;
            numerator += dx * (y - mean_y);
            denominator += dx.powi(2);
        }

        if denominator.abs() < f64::EPSILON {
            None
        } else {
            Some(numerator / denominator)
        }
    }

    /// Check whether all critical slot types have a confident value.
    pub fn is_critical_slot_satisfied(&self, slots: &[SlotRecord], threshold: f64) -> bool {
        if self.config.critical_slots.is_empty() {
            return true;
        }

        let mut satisfied = HashSet::new();
        for slot in slots {
            if !self.config.critical_slots.contains(&slot.slot_type) {
                continue;
            }

            let score = self.calculate_slot_confidence(slot);
            if score >= threshold {
                satisfied.insert(slot.slot_type.clone());
            }
        }

        self.config
            .critical_slots
            .iter()
            .all(|slot_type| satisfied.contains(slot_type))
    }

    /// Summarise confidence across slots, reporting unresolved items.
    pub fn summarize_confidence(&self, slots: &[SlotRecord]) -> ConfidenceSummary {
        let overall = self.calculate_overall_confidence(slots);
        let mut min_critical = 1.0;
        let mut any_critical = false;
        let mut unresolved = Vec::new();

        for slot in slots {
            let score = self.calculate_slot_confidence(slot);
            if self.config.critical_slots.contains(&slot.slot_type) {
                any_critical = true;
                if score < min_critical {
                    min_critical = score;
                }
            }

            if score < self.config.unresolved_slot_threshold || is_value_unresolved(&slot.value) {
                unresolved.push(slot.id.clone());
            }
        }

        if !any_critical {
            min_critical = 0.0;
        }

        ConfidenceSummary {
            overall,
            min_critical_slot: min_critical,
            unresolved_slots: unresolved,
        }
    }
}

impl Default for ConfidenceService {
    fn default() -> Self {
        Self::new(ConfidenceConfig::default())
    }
}

#[derive(Debug, Clone)]
pub struct ConfidenceConfig {
    pub slope_window_size: usize,
    pub min_samples_for_slope: usize,
    pub critical_slots: Vec<SlotType>,
    pub weights: ConfidenceWeights,
    pub unresolved_slot_threshold: f64,
}

impl Default for ConfidenceConfig {
    fn default() -> Self {
        Self {
            slope_window_size: 5,
            min_samples_for_slope: 2,
            critical_slots: Vec::new(),
            weights: ConfidenceWeights::default(),
            unresolved_slot_threshold: 0.7,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConfidenceWeights {
    pub llm_primary: f64,
    pub user_reply: f64,
    pub screenshot_inference: f64,
    pub deterministic_check: f64,
    pub memory_lookup: f64,
}

impl Default for ConfidenceWeights {
    fn default() -> Self {
        Self {
            llm_primary: 0.7,
            user_reply: 1.0,
            screenshot_inference: 0.6,
            deterministic_check: 0.9,
            memory_lookup: 0.8,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfidenceSummary {
    pub overall: f64,
    pub min_critical_slot: f64,
    pub unresolved_slots: Vec<String>,
}

fn aggregate_provenance_confidence(
    provenance: &[ProvenanceRecord],
    weights: &ConfidenceWeights,
) -> f64 {
    if provenance.is_empty() {
        return 0.0;
    }

    let weighted_sum: f64 = provenance
        .iter()
        .map(|p| {
            match p.source {
                ProvenanceSource::LlmPrimary => weights.llm_primary,
                ProvenanceSource::UserReply => weights.user_reply,
                ProvenanceSource::ScreenshotInference => weights.screenshot_inference,
                ProvenanceSource::DeterministicCheck => weights.deterministic_check,
                ProvenanceSource::MemoryLookup => weights.memory_lookup,
                ProvenanceSource::OutlinePrerequisite => 0.0, // Unfilled prerequisites have no confidence
            }
        })
        .sum();

    let total_weight = provenance.len() as f64;
    (weighted_sum / total_weight).clamp(0.0, 1.0)
}

fn is_value_unresolved(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Null => true,
        serde_json::Value::String(s) => s.trim().is_empty(),
        serde_json::Value::Array(arr) => arr.is_empty(),
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    }
}

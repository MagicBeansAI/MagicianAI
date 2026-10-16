//! Trainer-side refusal. A weak snapshot is not written.

#[derive(Debug, Clone, PartialEq)]
pub struct GateConfig {
    pub min_labels: usize,
    pub max_ece: f64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            min_labels: 40,
            max_ece: 0.10,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TrainingMetrics {
    pub label_count: usize,
    pub positive: usize,
    pub negative: usize,
    pub usable: usize,
    pub unlinked: usize,
    pub auc: f64,
    pub ece: f64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateVerdict {
    Passed,
    Refused { reasons: Vec<String> },
}

impl GateVerdict {
    pub fn explain(&self) -> String {
        match self {
            Self::Passed => "passed".to_string(),
            Self::Refused { reasons } => reasons.join("; "),
        }
    }
}

pub fn evaluate_gates(metrics: &TrainingMetrics, config: &GateConfig) -> GateVerdict {
    let mut reasons = Vec::new();
    if metrics.label_count < config.min_labels {
        reasons.push(format!(
            "labels {} below minimum {}",
            metrics.label_count, config.min_labels
        ));
    }
    if metrics.auc <= 0.5 {
        reasons.push(format!(
            "holdout AUC {:.3} does not beat the 0.5 baseline",
            metrics.auc
        ));
    }
    if metrics.ece > config.max_ece {
        reasons.push(format!(
            "calibration ECE {:.3} exceeds maximum {:.3}",
            metrics.ece, config.max_ece
        ));
    }
    if reasons.is_empty() {
        GateVerdict::Passed
    } else {
        GateVerdict::Refused { reasons }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    fn config() -> GateConfig {
        GateConfig::default()
    }

    fn metrics_with(labels: usize, auc: f64, ece: f64) -> TrainingMetrics {
        TrainingMetrics {
            label_count: labels,
            positive: labels / 2,
            negative: labels - labels / 2,
            usable: labels,
            unlinked: 0,
            auc,
            ece,
        }
    }

    #[test]
    fn a_snapshot_is_refused_below_the_label_minimum_with_a_reason() {
        let verdict = evaluate_gates(&metrics_with(20, 0.9, 0.05), &config());
        assert!(matches!(verdict, GateVerdict::Refused { .. }));
        // A refusal is a status report, not a failure: it must say what is short.
        assert!(verdict.explain().contains("labels"));
    }

    #[test]
    fn a_snapshot_is_refused_when_it_does_not_beat_the_baseline() {
        // Beating a trivial baseline is the point. Matching it is not.
        let verdict = evaluate_gates(&metrics_with(500, 0.50, 0.02), &config());
        assert!(matches!(verdict, GateVerdict::Refused { .. }));
        assert!(verdict.explain().contains("baseline"));
    }

    #[test]
    fn a_snapshot_is_refused_when_poorly_calibrated_even_if_it_ranks_well() {
        // High AUC with bad calibration is exactly the model that looks good and
        // is useless to an asymmetric decision.
        let verdict = evaluate_gates(&metrics_with(500, 0.95, 0.40), &config());
        assert!(matches!(verdict, GateVerdict::Refused { .. }));
        assert!(verdict.explain().contains("calibration"));
    }

    #[test]
    fn a_good_model_passes() {
        assert!(matches!(
            evaluate_gates(&metrics_with(500, 0.78, 0.04), &config()),
            GateVerdict::Passed
        ));
    }
}

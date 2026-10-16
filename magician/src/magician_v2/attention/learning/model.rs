//! Pure Bayesian kNN estimator and Slice-1 replay metrics.

use crate::config::AttentionLearningConfig;

use super::{AttentionOutcomeKind, AttentionSurface};

#[derive(Debug, Clone, PartialEq)]
pub struct BayesianKnnLabel {
    pub outcome: AttentionOutcomeKind,
    pub embedding: Vec<f32>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BayesianProbability {
    pub probability: f64,
    pub evidence_weight: f64,
    pub neighbor_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BayesianKnnEstimate {
    pub usefulness: BayesianProbability,
    pub actionability: BayesianProbability,
}

impl BayesianKnnEstimate {
    pub fn surface_score(self, surface: AttentionSurface) -> Option<f64> {
        let usefulness =
            (self.usefulness.evidence_weight > 0.0).then_some(self.usefulness.probability);
        let actionability =
            (self.actionability.evidence_weight > 0.0).then_some(self.actionability.probability);
        match surface {
            // Keep Slice 1 scientifically legible: each surface consumes its
            // registered task target directly. Cross-task blends must come
            // from a fitted, versioned model rather than unexplained weights.
            AttentionSurface::FollowUp => actionability,
            AttentionSurface::WorthALook => usefulness,
        }
    }
}

#[derive(Debug, Clone)]
pub struct BayesianKnnEvaluator {
    neighbor_count: usize,
    prior_alpha: f64,
    prior_beta: f64,
    kernel_bandwidth: f64,
    min_evidence_weight: f64,
}

impl BayesianKnnEvaluator {
    pub fn new(config: &AttentionLearningConfig) -> Self {
        Self {
            neighbor_count: config.neighbor_count.max(1),
            prior_alpha: config.prior_alpha,
            prior_beta: config.prior_beta,
            kernel_bandwidth: config.kernel_bandwidth,
            min_evidence_weight: config.min_evidence_weight,
        }
    }

    pub fn estimate(
        &self,
        candidate_embedding: &[f32],
        labels: &[BayesianKnnLabel],
    ) -> BayesianKnnEstimate {
        BayesianKnnEstimate {
            usefulness: self.posterior(
                candidate_embedding,
                labels,
                AttentionOutcomeKind::usefulness_target,
            ),
            actionability: self.posterior(
                candidate_embedding,
                labels,
                AttentionOutcomeKind::actionability_target,
            ),
        }
    }

    fn posterior(
        &self,
        candidate_embedding: &[f32],
        labels: &[BayesianKnnLabel],
        target: fn(AttentionOutcomeKind) -> Option<bool>,
    ) -> BayesianProbability {
        // Each registered task gets its own nearest-neighbor set. Filtering
        // before truncation prevents neutral or task-inapplicable labels from
        // occupying all k slots and hiding relevant owner evidence.
        let mut neighbors: Vec<(f64, &BayesianKnnLabel, bool)> = labels
            .iter()
            .filter_map(|label| {
                let value = target(label.outcome)?;
                cosine_similarity(candidate_embedding, &label.embedding)
                    .map(|similarity| (similarity, label, value))
            })
            .collect();
        neighbors.sort_by(|left, right| right.0.total_cmp(&left.0));
        neighbors.truncate(self.neighbor_count);
        let mut positive = 0.0_f64;
        let mut negative = 0.0_f64;
        let mut neighbor_count = 0_usize;
        for (similarity, _label, value) in neighbors {
            let distance = (1.0 - similarity.clamp(-1.0, 1.0)).max(0.0);
            let scaled = distance / self.kernel_bandwidth;
            let weight = (-0.5 * scaled * scaled).exp();
            if !weight.is_finite() || weight <= 0.0 {
                continue;
            }
            neighbor_count += 1;
            if value {
                positive += weight;
            } else {
                negative += weight;
            }
        }
        let evidence_weight = positive + negative;
        let probability = if evidence_weight < self.min_evidence_weight {
            self.prior_alpha / (self.prior_alpha + self.prior_beta)
        } else {
            (self.prior_alpha + positive) / (self.prior_alpha + self.prior_beta + evidence_weight)
        };
        BayesianProbability {
            probability,
            evidence_weight: if evidence_weight < self.min_evidence_weight {
                0.0
            } else {
                evidence_weight
            },
            neighbor_count,
        }
    }
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f64> {
    if left.is_empty() || left.len() != right.len() {
        return None;
    }
    let mut dot = 0.0_f64;
    let mut left_norm = 0.0_f64;
    let mut right_norm = 0.0_f64;
    for (&left, &right) in left.iter().zip(right) {
        if !left.is_finite() || !right.is_finite() {
            return None;
        }
        let left = f64::from(left);
        let right = f64::from(right);
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }
    if left_norm <= f64::EPSILON || right_norm <= f64::EPSILON {
        return None;
    }
    Some((dot / (left_norm.sqrt() * right_norm.sqrt())).clamp(-1.0, 1.0))
}

/// Frozen replay comparison for Slice 1. The evaluator intentionally accepts
/// already-ranked IDs/outcomes so data extraction and policy evaluation stay
/// separate and group-aware callers can split before invoking it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RankingReplayMetrics {
    pub evaluated: usize,
    pub positive_at_k: usize,
    pub negative_at_k: usize,
    pub repeat_negative_at_k: usize,
}

pub fn evaluate_top_k(
    ranked_outcomes: &[AttentionOutcomeKind],
    repeated_negative: &[bool],
    k: usize,
) -> RankingReplayMetrics {
    let evaluated = ranked_outcomes.len().min(k);
    let mut positive_at_k = 0;
    let mut negative_at_k = 0;
    let mut repeat_negative_at_k = 0;
    for (index, outcome) in ranked_outcomes.iter().take(evaluated).enumerate() {
        if outcome.usefulness_target() == Some(true) || outcome.actionability_target() == Some(true)
        {
            positive_at_k += 1;
        }
        if outcome.usefulness_target() == Some(false)
            || outcome.actionability_target() == Some(false)
        {
            negative_at_k += 1;
            if repeated_negative.get(index).copied().unwrap_or(false) {
                repeat_negative_at_k += 1;
            }
        }
    }
    RankingReplayMetrics {
        evaluated,
        positive_at_k,
        negative_at_k,
        repeat_negative_at_k,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn attention_surface_as_str_is_pinned() {
        // Wire pin: the comms identity test that carried this assertion was
        // deleted with the Phase 5 glob trim; the mapping lives here now.
        assert_eq!(
            crate::magician_v2::attention::learning::AttentionSurface::WorthALook.as_str(),
            "worth_a_look"
        );
    }

    fn config() -> AttentionLearningConfig {
        AttentionLearningConfig {
            enabled: true,
            semantic_ranking_enabled: true,
            rescore_limit: 100,
            neighbor_count: 8,
            prior_alpha: 1.0,
            prior_beta: 1.0,
            kernel_bandwidth: 0.2,
            min_evidence_weight: 0.1,
            embedding_timeout_ms: 100,
            background_embedding_timeout_ms: 100,
            semantic_backfill: Default::default(),
            rank_recompute: Default::default(),
            historical_bootstrap: Default::default(),
            actionability: Default::default(),
            grouping: Default::default(),
            routing: Default::default(),
            bandit: Default::default(),
        }
    }

    #[test]
    fn similar_negative_feedback_lowers_both_posteriors() {
        let evaluator = BayesianKnnEvaluator::new(&config());
        let estimate = evaluator.estimate(
            &[1.0, 0.0],
            &[BayesianKnnLabel {
                outcome: AttentionOutcomeKind::Irrelevant,
                embedding: vec![0.99, 0.01],
            }],
        );
        assert!(estimate.usefulness.probability < 0.5);
        assert!(estimate.actionability.probability < 0.5);
        assert!(estimate.usefulness.evidence_weight > 0.0);
    }

    #[test]
    fn neutral_feedback_has_zero_model_evidence() {
        let evaluator = BayesianKnnEvaluator::new(&config());
        let estimate = evaluator.estimate(
            &[1.0, 0.0],
            &[BayesianKnnLabel {
                outcome: AttentionOutcomeKind::NeutralSeen,
                embedding: vec![1.0, 0.0],
            }],
        );
        assert_eq!(estimate.usefulness.evidence_weight, 0.0);
        assert_eq!(estimate.actionability.evidence_weight, 0.0);
        assert_eq!(estimate.surface_score(AttentionSurface::FollowUp), None);
    }

    #[test]
    fn task_inapplicable_neighbors_cannot_crow_out_relevant_evidence() {
        let mut narrow = config();
        narrow.neighbor_count = 1;
        let evaluator = BayesianKnnEvaluator::new(&narrow);
        let estimate = evaluator.estimate(
            &[1.0, 0.0],
            &[
                BayesianKnnLabel {
                    outcome: AttentionOutcomeKind::NeutralSeen,
                    embedding: vec![1.0, 0.0],
                },
                BayesianKnnLabel {
                    outcome: AttentionOutcomeKind::Useful,
                    embedding: vec![0.999, 0.001],
                },
                BayesianKnnLabel {
                    outcome: AttentionOutcomeKind::ActionCompleted,
                    embedding: vec![0.99, 0.01],
                },
            ],
        );

        assert_eq!(estimate.actionability.neighbor_count, 1);
        assert!(estimate.actionability.evidence_weight > 0.0);
        assert!(estimate.actionability.probability > 0.5);
    }

    #[test]
    fn surfaces_use_only_their_registered_task_target() {
        let estimate = BayesianKnnEstimate {
            usefulness: BayesianProbability {
                probability: 0.9,
                evidence_weight: 1.0,
                neighbor_count: 1,
            },
            actionability: BayesianProbability {
                probability: 0.2,
                evidence_weight: 1.0,
                neighbor_count: 1,
            },
        };
        assert_eq!(
            estimate.surface_score(AttentionSurface::FollowUp),
            Some(0.2)
        );
        assert_eq!(
            estimate.surface_score(AttentionSurface::WorthALook),
            Some(0.9)
        );
    }

    #[test]
    fn replay_metrics_count_repeat_negatives_without_treating_neutral_as_negative() {
        let metrics = evaluate_top_k(
            &[
                AttentionOutcomeKind::Irrelevant,
                AttentionOutcomeKind::NeutralSeen,
                AttentionOutcomeKind::Useful,
            ],
            &[true, false, false],
            20,
        );
        assert_eq!(metrics.negative_at_k, 1);
        assert_eq!(metrics.repeat_negative_at_k, 1);
        assert_eq!(metrics.positive_at_k, 1);
    }
}

//! Capability evolution evaluation harness.
//!
//! This module centralizes scoring and lifecycle transitions for generated
//! capability packs. It is intentionally deterministic so promotion decisions
//! are auditable and reproducible.

use serde::{Deserialize, Serialize};

/// Lifecycle status for generated capability packs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityLifecycleStatus {
    Trial,
    Validated,
    Trusted,
    Deprecated,
}

/// Input signals used for promotion/demotion decisions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEvaluationInput {
    /// Total replay attempts observed for this capability.
    pub attempts: u32,
    /// Successful replays.
    pub successes: u32,
    /// Verification pass rate in [0, 1].
    pub verification_pass_rate: f64,
    /// Number of sandbox/safety violations observed.
    pub safety_violations: u32,
}

impl CapabilityEvaluationInput {
    /// Replay success rate in [0, 1].
    pub fn success_rate(&self) -> f64 {
        if self.attempts == 0 {
            return 0.0;
        }
        (self.successes as f64 / self.attempts as f64).clamp(0.0, 1.0)
    }

    /// Composite score in [0, 1] blending replay and verification quality.
    pub fn score(&self) -> f64 {
        let success = self.success_rate();
        let verification = self.verification_pass_rate.clamp(0.0, 1.0);
        let violation_penalty = if self.safety_violations == 0 {
            0.0
        } else {
            0.25
        };
        (success * 0.65 + verification * 0.35 - violation_penalty).clamp(0.0, 1.0)
    }
}

/// Promotion/demotion threshold configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapabilityEvaluationThresholds {
    pub trial_to_validated_min_attempts: u32,
    pub trial_to_validated_min_success_rate: f64,
    pub trial_to_validated_min_score: f64,
    pub validated_to_trusted_min_attempts: u32,
    pub validated_to_trusted_min_success_rate: f64,
    pub validated_to_trusted_min_score: f64,
    pub max_failure_rate_before_demotion: f64,
}

impl Default for CapabilityEvaluationThresholds {
    fn default() -> Self {
        Self {
            trial_to_validated_min_attempts: 5,
            trial_to_validated_min_success_rate: 0.8,
            trial_to_validated_min_score: 0.8,
            validated_to_trusted_min_attempts: 20,
            validated_to_trusted_min_success_rate: 0.95,
            validated_to_trusted_min_score: 0.9,
            max_failure_rate_before_demotion: 0.5,
        }
    }
}

/// Lifecycle decision produced by the evaluator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityEvaluationDecision {
    NoChange,
    PromoteToValidated,
    PromoteToTrusted,
    DemoteToTrial,
    DemoteToValidated,
    Deprecate,
}

/// Evaluate lifecycle transition for a generated capability.
pub fn evaluate_transition(
    current: CapabilityLifecycleStatus,
    input: &CapabilityEvaluationInput,
    thresholds: &CapabilityEvaluationThresholds,
) -> CapabilityEvaluationDecision {
    if current == CapabilityLifecycleStatus::Deprecated {
        return CapabilityEvaluationDecision::NoChange;
    }

    if input.safety_violations > 0 {
        return CapabilityEvaluationDecision::Deprecate;
    }

    let success_rate = input.success_rate();
    let failure_rate = (1.0 - success_rate).clamp(0.0, 1.0);
    let score = input.score();

    match current {
        CapabilityLifecycleStatus::Trial => {
            if input.attempts >= thresholds.trial_to_validated_min_attempts
                && success_rate >= thresholds.trial_to_validated_min_success_rate
                && score >= thresholds.trial_to_validated_min_score
            {
                CapabilityEvaluationDecision::PromoteToValidated
            } else {
                CapabilityEvaluationDecision::NoChange
            }
        },
        CapabilityLifecycleStatus::Validated => {
            if input.attempts >= thresholds.validated_to_trusted_min_attempts
                && success_rate >= thresholds.validated_to_trusted_min_success_rate
                && score >= thresholds.validated_to_trusted_min_score
            {
                return CapabilityEvaluationDecision::PromoteToTrusted;
            }
            if input.attempts >= thresholds.trial_to_validated_min_attempts
                && failure_rate > thresholds.max_failure_rate_before_demotion
            {
                return CapabilityEvaluationDecision::DemoteToTrial;
            }
            CapabilityEvaluationDecision::NoChange
        },
        CapabilityLifecycleStatus::Trusted => {
            if input.attempts >= thresholds.trial_to_validated_min_attempts
                && failure_rate > thresholds.max_failure_rate_before_demotion
            {
                return CapabilityEvaluationDecision::DemoteToValidated;
            }
            CapabilityEvaluationDecision::NoChange
        },
        CapabilityLifecycleStatus::Deprecated => CapabilityEvaluationDecision::NoChange,
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn capability_pack_eval_trial_to_validated_requires_thresholds() {
        let thresholds = CapabilityEvaluationThresholds::default();

        let below = CapabilityEvaluationInput {
            attempts: 5,
            successes: 3,
            verification_pass_rate: 0.9,
            safety_violations: 0,
        };
        assert_eq!(
            evaluate_transition(CapabilityLifecycleStatus::Trial, &below, &thresholds),
            CapabilityEvaluationDecision::NoChange
        );

        let passing = CapabilityEvaluationInput {
            attempts: 5,
            successes: 4,
            verification_pass_rate: 0.9,
            safety_violations: 0,
        };
        assert_eq!(passing.success_rate(), 0.8);
        assert_eq!(
            evaluate_transition(CapabilityLifecycleStatus::Trial, &passing, &thresholds),
            CapabilityEvaluationDecision::PromoteToValidated
        );
    }

    #[test]
    fn capability_pack_eval_safety_violations_deprecate() {
        let thresholds = CapabilityEvaluationThresholds::default();
        let input = CapabilityEvaluationInput {
            attempts: 20,
            successes: 20,
            verification_pass_rate: 1.0,
            safety_violations: 1,
        };
        assert_eq!(
            evaluate_transition(CapabilityLifecycleStatus::Validated, &input, &thresholds),
            CapabilityEvaluationDecision::Deprecate
        );
    }
}

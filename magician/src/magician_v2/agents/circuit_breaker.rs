//! Phase 3 circuit breaker interpreter scaffolding.

use super::types::{CircuitAction, CircuitBreakerPolicy};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CircuitDecision {
    NoAction,
    InjectFailureContext {
        escalation: Option<String>,
    },
    OpenCircuit {
        threshold_failures: usize,
        effective_max_failures: usize,
        notify_targets: Vec<String>,
    },
}

#[derive(Debug, Clone, Default)]
pub struct CircuitBreakerInterpreter;

impl CircuitBreakerInterpreter {
    pub fn decide(
        &self,
        policy: &CircuitBreakerPolicy,
        goal_id: &str,
        consecutive_failures: usize,
        default_max_failures: usize,
    ) -> CircuitDecision {
        let effective_max = policy
            .per_goal_override
            .get(goal_id)
            .map(|o| o.max_failures)
            .unwrap_or(default_max_failures);

        let Some(threshold) = policy
            .thresholds
            .iter()
            .filter(|t| consecutive_failures >= t.failures)
            .max_by_key(|t| t.failures)
        else {
            return CircuitDecision::NoAction;
        };

        match threshold.action {
            CircuitAction::InjectFailureContext => CircuitDecision::InjectFailureContext {
                escalation: threshold.escalation.clone(),
            },
            CircuitAction::OpenCircuit if consecutive_failures >= effective_max => {
                CircuitDecision::OpenCircuit {
                    threshold_failures: threshold.failures,
                    effective_max_failures: effective_max,
                    notify_targets: threshold.notify.clone(),
                }
            },
            CircuitAction::OpenCircuit => {
                // Can't open yet (under effective_max). Fall back to the highest
                // InjectFailureContext threshold that still matches, so the agent
                // keeps getting failure-context hints in the gap.
                policy
                    .thresholds
                    .iter()
                    .filter(|t| consecutive_failures >= t.failures)
                    .filter(|t| matches!(t.action, CircuitAction::InjectFailureContext))
                    .max_by_key(|t| t.failures)
                    .map(|t| CircuitDecision::InjectFailureContext {
                        escalation: t.escalation.clone(),
                    })
                    .unwrap_or(CircuitDecision::NoAction)
            },
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::agents::types::{
        CircuitAction, CircuitBreakerOverride, CircuitBreakerPolicy, CircuitBreakerThreshold,
    };
    use std::collections::HashMap;

    fn policy() -> CircuitBreakerPolicy {
        CircuitBreakerPolicy {
            thresholds: vec![
                CircuitBreakerThreshold {
                    failures: 1,
                    action: CircuitAction::InjectFailureContext,
                    escalation: Some("try another strategy".to_string()),
                    notify: vec![],
                },
                CircuitBreakerThreshold {
                    failures: 3,
                    action: CircuitAction::OpenCircuit,
                    escalation: None,
                    notify: vec!["chat".to_string()],
                },
            ],
            recovery: Default::default(),
            per_goal_override: HashMap::new(),
            ..Default::default()
        }
    }

    #[test]
    fn inject_failure_context_on_first_failure() {
        let decision = CircuitBreakerInterpreter.decide(&policy(), "g1", 1, 3);
        assert!(matches!(
            decision,
            CircuitDecision::InjectFailureContext { .. }
        ));
    }

    #[test]
    fn open_circuit_respects_per_goal_override() {
        let mut policy = policy();
        policy
            .per_goal_override
            .insert("g1".to_string(), CircuitBreakerOverride { max_failures: 5 });
        // At 3 failures with OpenCircuit threshold at 3 but effective_max=5,
        // circuit can't open yet — falls back to InjectFailureContext (threshold at 1).
        let decision = CircuitBreakerInterpreter.decide(&policy, "g1", 3, 3);
        assert!(matches!(
            decision,
            CircuitDecision::InjectFailureContext { .. }
        ));
        // At 5 failures, the override max is reached and circuit opens.
        let decision = CircuitBreakerInterpreter.decide(&policy, "g1", 5, 3);
        assert!(matches!(decision, CircuitDecision::OpenCircuit { .. }));
    }

    #[test]
    fn open_circuit_blocked_returns_no_action_without_inject_threshold() {
        // Only an OpenCircuit threshold, no InjectFailureContext → NoAction fallback.
        let policy = CircuitBreakerPolicy {
            thresholds: vec![CircuitBreakerThreshold {
                failures: 2,
                action: CircuitAction::OpenCircuit,
                escalation: None,
                notify: vec!["chat".to_string()],
            }],
            recovery: Default::default(),
            per_goal_override: HashMap::from([(
                "g1".to_string(),
                CircuitBreakerOverride { max_failures: 5 },
            )]),
            ..Default::default()
        };
        // Matches OpenCircuit threshold but can't fire (2 < 5), no InjectFailureContext to fall back to.
        let decision = CircuitBreakerInterpreter.decide(&policy, "g1", 2, 3);
        assert_eq!(decision, CircuitDecision::NoAction);
    }

    #[test]
    fn no_action_when_zero_consecutive_failures() {
        let decision = CircuitBreakerInterpreter.decide(&policy(), "g1", 0, 3);
        assert_eq!(decision, CircuitDecision::NoAction);
    }

    #[test]
    fn fallback_preserves_escalation_text() {
        let mut policy = policy();
        policy
            .per_goal_override
            .insert("g1".to_string(), CircuitBreakerOverride { max_failures: 5 });
        // At 3 failures: OpenCircuit threshold matches but can't fire (3 < 5),
        // falls back to InjectFailureContext threshold at 1 with its escalation text.
        let decision = CircuitBreakerInterpreter.decide(&policy, "g1", 3, 3);
        assert_eq!(
            decision,
            CircuitDecision::InjectFailureContext {
                escalation: Some("try another strategy".to_string()),
            }
        );
    }

    #[test]
    fn highest_matching_threshold_wins_even_when_unsorted() {
        let policy = CircuitBreakerPolicy {
            thresholds: vec![
                CircuitBreakerThreshold {
                    failures: 3,
                    action: CircuitAction::OpenCircuit,
                    escalation: None,
                    notify: vec!["chat".to_string()],
                },
                CircuitBreakerThreshold {
                    failures: 1,
                    action: CircuitAction::InjectFailureContext,
                    escalation: None,
                    notify: vec![],
                },
            ],
            recovery: Default::default(),
            per_goal_override: HashMap::new(),
            ..Default::default()
        };
        let decision = CircuitBreakerInterpreter.decide(&policy, "g1", 3, 3);
        assert!(matches!(decision, CircuitDecision::OpenCircuit { .. }));
    }
}

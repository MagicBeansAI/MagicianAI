//! Per-gate budgets — rounds, cost and elapsed time, owned by this controller.
//!
//! ## Why the gate owns its own budgets
//!
//! The global agentic ceiling (`DEFAULT_AGENTIC_MAX_DURATION_SECS`, currently
//! 2400s) bounds a *coding turn*. A verification gate outlives many of those:
//! run checks, hand back diagnostics, wait for a repair, run checks again. If
//! the gate simply inherited the coding ceiling, a legitimate repair loop
//! would die inside it and the failure would look like verification failing
//! rather than a budget being wrong.
//!
//! So these budgets bound the **gate**, and the coding turn keeps its own.
//! They are complements, not alternatives. Raising the global ceiling (the
//! separate run-duration workstream) is still required before bounded repair
//! is enabled in production — this module makes the gate's own limits explicit
//! and configurable so the controller does not silently depend on that.
//!
//! ## Three dimensions, checked independently
//!
//! Rounds alone is not enough: a single round can be arbitrarily expensive
//! (this repository tests in ~16 minutes) and a loop can stall without
//! consuming a round at all. Cost alone is not enough either — a cheap loop
//! can still spin for hours. Each dimension is checked separately and the
//! first to blow stops the gate, with the dimension named in the reason so an
//! operator can tell "it kept failing" from "it ran out of time".

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};

use super::gate::{GateBudgets, GateSpend, VerificationGate};

/// Which limit stopped the gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    RepairRounds,
    Spend,
    Elapsed,
}

impl BudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            BudgetDimension::RepairRounds => "repair_rounds",
            BudgetDimension::Spend => "spend",
            BudgetDimension::Elapsed => "elapsed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BudgetVerdict {
    Within,
    Exhausted {
        dimension: BudgetDimension,
        reason: String,
    },
}

impl BudgetVerdict {
    pub fn is_within(&self) -> bool {
        matches!(self, BudgetVerdict::Within)
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            BudgetVerdict::Within => None,
            BudgetVerdict::Exhausted { reason, .. } => Some(reason),
        }
    }
}

/// Evaluate all three dimensions.
///
/// Ordered rounds → spend → elapsed only so the message is predictable; the
/// dimensions are independent and any one of them stops the gate.
pub fn evaluate(
    budgets: &GateBudgets,
    spend: &GateSpend,
    created_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> BudgetVerdict {
    if spend.repair_rounds >= budgets.max_repair_rounds {
        return BudgetVerdict::Exhausted {
            dimension: BudgetDimension::RepairRounds,
            reason: format!(
                "repair budget exhausted: {} of {} rounds used",
                spend.repair_rounds, budgets.max_repair_rounds
            ),
        };
    }
    if let Some(max) = budgets.max_spend_usd {
        if spend.spend_usd >= max {
            return BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Spend,
                reason: format!(
                    "verification spend budget exhausted: ${:.4} of ${:.4}",
                    spend.spend_usd, max
                ),
            };
        }
    }
    if let Some(max) = budgets.max_elapsed_secs {
        let elapsed = now.signed_duration_since(created_at);
        if elapsed >= ChronoDuration::seconds(max as i64) {
            return BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Elapsed,
                reason: format!(
                    "verification elapsed budget exhausted: {}s of {}s",
                    elapsed.num_seconds().max(0),
                    max
                ),
            };
        }
    }
    BudgetVerdict::Within
}

/// Whether it is worth *starting* another pass.
///
/// Distinct from [`evaluate`] on purpose. `evaluate` asks "may we repair
/// again?"; this asks "should we begin work at all?". A gate whose elapsed
/// ceiling has already passed must not start a fresh 16-minute test run just
/// because it still has a repair round in hand — it would burn the machine to
/// produce a result the gate can no longer act on.
pub fn may_start_pass(gate: &VerificationGate, now: DateTime<Utc>) -> BudgetVerdict {
    if let Some(max) = gate.budgets.max_elapsed_secs {
        let elapsed = now.signed_duration_since(gate.created_at);
        if elapsed >= ChronoDuration::seconds(max as i64) {
            return BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Elapsed,
                reason: format!(
                    "verification elapsed budget exhausted before the pass began: {}s of {}s",
                    elapsed.num_seconds().max(0),
                    max
                ),
            };
        }
    }
    if let Some(max) = gate.budgets.max_spend_usd {
        if gate.spend.spend_usd >= max {
            return BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Spend,
                reason: format!(
                    "verification spend budget exhausted before the pass began: ${:.4} of ${:.4}",
                    gate.spend.spend_usd, max
                ),
            };
        }
    }
    // Rounds are deliberately *not* checked here: the first pass of a gate has
    // consumed zero rounds, and a gate that has used its last round still owes
    // that round's verification run.
    BudgetVerdict::Within
}

/// Charge cost against a gate's ledger.
///
/// Saturating and non-negative: a provider that reports a nonsense negative
/// cost must not be able to refund a gate back under its ceiling.
pub fn charge(spend: &GateSpend, usd: f64) -> GateSpend {
    let delta = if usd.is_finite() && usd > 0.0 {
        usd
    } else {
        0.0
    };
    GateSpend {
        repair_rounds: spend.repair_rounds,
        spend_usd: spend.spend_usd + delta,
    }
}

/// Record one consumed repair round.
pub fn consume_round(spend: &GateSpend) -> GateSpend {
    GateSpend {
        repair_rounds: spend.repair_rounds.saturating_add(1),
        spend_usd: spend.spend_usd,
    }
}

/// Configuration source for gate budgets.
///
/// Separate from [`GateBudgets`] so configuration can be absent, partial or
/// malformed without producing a gate with nonsense limits. Missing values
/// fall back to the defaults rather than to "unlimited" — an unbounded gate is
/// the failure mode this whole module exists to prevent.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct BudgetConfig {
    #[serde(default)]
    pub max_repair_rounds: Option<u32>,
    #[serde(default)]
    pub max_spend_usd: Option<f64>,
    #[serde(default)]
    pub max_elapsed_secs: Option<u64>,
}

impl BudgetConfig {
    pub fn resolve(&self) -> GateBudgets {
        let defaults = GateBudgets::default();
        GateBudgets {
            // Zero rounds is a legitimate configuration — "verify once, never
            // repair" — so it is honoured rather than treated as unset.
            max_repair_rounds: self.max_repair_rounds.unwrap_or(defaults.max_repair_rounds),
            // A non-positive or non-finite ceiling is meaningless; fall back
            // rather than creating a gate that is instantly exhausted.
            max_spend_usd: self
                .max_spend_usd
                .filter(|v| v.is_finite() && *v > 0.0)
                .or(defaults.max_spend_usd),
            max_elapsed_secs: self
                .max_elapsed_secs
                .filter(|v| *v > 0)
                .or(defaults.max_elapsed_secs),
        }
    }
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use crate::magician_v2::execution::file_edit::transaction::TransactionScope;
    use crate::magician_v2::execution::verification::gate::GateOrigin;
    use crate::magician_v2::execution::verification::ids::CandidateRevision;

    fn gate(budgets: GateBudgets) -> VerificationGate {
        VerificationGate::new(
            TransactionScope {
                principal: "anonymous".into(),
                workspace: "default".into(),
            },
            "proj-a",
            "task-1",
            "exec-1",
            CandidateRevision::new("ccp-1", 1).unwrap(),
            GateOrigin {
                engineer_agent_id: "engineer".into(),
                coding_profile: None,
                coding_engine: None,
                constraint_auto: false,
                coding_invocation_ref: None,
                child_execution_id: None,
            },
            budgets,
        )
        .unwrap()
    }

    #[test]
    fn a_fresh_gate_is_within_budget() {
        let b = GateBudgets::default();
        let verdict = evaluate(&b, &GateSpend::default(), Utc::now(), Utc::now());
        assert!(verdict.is_within());
    }

    #[test]
    fn each_dimension_stops_the_gate_independently() {
        let now = Utc::now();

        let rounds = evaluate(
            &GateBudgets {
                max_repair_rounds: 2,
                max_spend_usd: None,
                max_elapsed_secs: None,
            },
            &GateSpend {
                repair_rounds: 2,
                spend_usd: 0.0,
            },
            now,
            now,
        );
        assert!(matches!(
            rounds,
            BudgetVerdict::Exhausted {
                dimension: BudgetDimension::RepairRounds,
                ..
            }
        ));

        let spend = evaluate(
            &GateBudgets {
                max_repair_rounds: 10,
                max_spend_usd: Some(1.0),
                max_elapsed_secs: None,
            },
            &GateSpend {
                repair_rounds: 0,
                spend_usd: 1.0,
            },
            now,
            now,
        );
        assert!(matches!(
            spend,
            BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Spend,
                ..
            }
        ));

        let elapsed = evaluate(
            &GateBudgets {
                max_repair_rounds: 10,
                max_spend_usd: None,
                max_elapsed_secs: Some(60),
            },
            &GateSpend::default(),
            now,
            now + ChronoDuration::seconds(61),
        );
        assert!(matches!(
            elapsed,
            BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Elapsed,
                ..
            }
        ));
    }

    #[test]
    fn the_reason_names_the_dimension_so_an_operator_can_tell_them_apart() {
        let now = Utc::now();
        let verdict = evaluate(
            &GateBudgets {
                max_repair_rounds: 10,
                max_spend_usd: None,
                max_elapsed_secs: Some(1),
            },
            &GateSpend::default(),
            now,
            now + ChronoDuration::seconds(5),
        );
        let reason = verdict.reason().unwrap();
        assert!(
            reason.contains("elapsed"),
            "\"it kept failing\" and \"it ran out of time\" must be distinguishable: {reason}"
        );
    }

    #[test]
    fn an_expired_gate_does_not_start_another_expensive_pass() {
        // Rounds remain, but the clock is gone. Starting a 16-minute test run
        // here would burn the machine for a result the gate cannot act on.
        let mut g = gate(GateBudgets {
            max_repair_rounds: 5,
            max_spend_usd: None,
            max_elapsed_secs: Some(60),
        });
        g.spend.repair_rounds = 0;

        let verdict = may_start_pass(&g, g.created_at + ChronoDuration::seconds(61));
        assert!(matches!(
            verdict,
            BudgetVerdict::Exhausted {
                dimension: BudgetDimension::Elapsed,
                ..
            }
        ));
    }

    #[test]
    fn a_gate_on_its_last_round_still_gets_to_run_that_rounds_checks() {
        // `may_start_pass` must not check rounds: the round was already
        // consumed when repair started, and refusing to verify its result
        // would strand the gate.
        let mut g = gate(GateBudgets {
            max_repair_rounds: 1,
            max_spend_usd: None,
            max_elapsed_secs: Some(3600),
        });
        g.spend.repair_rounds = 1;

        assert!(may_start_pass(&g, Utc::now()).is_within());
        // But it may not repair again.
        assert!(!evaluate(&g.budgets, &g.spend, g.created_at, Utc::now()).is_within());
    }

    #[test]
    fn charging_is_saturating_and_refuses_refunds() {
        let spend = GateSpend {
            repair_rounds: 1,
            spend_usd: 2.0,
        };
        assert_eq!(charge(&spend, 1.5).spend_usd, 3.5);

        // A provider reporting nonsense must not buy the gate more budget.
        assert_eq!(charge(&spend, -5.0).spend_usd, 2.0);
        assert_eq!(charge(&spend, f64::NAN).spend_usd, 2.0);
        assert_eq!(charge(&spend, f64::INFINITY).spend_usd, 2.0);

        // Rounds are untouched by a cost charge.
        assert_eq!(charge(&spend, 1.0).repair_rounds, 1);
    }

    #[test]
    fn consuming_a_round_leaves_cost_alone() {
        let spend = GateSpend {
            repair_rounds: 1,
            spend_usd: 2.0,
        };
        let next = consume_round(&spend);
        assert_eq!(next.repair_rounds, 2);
        assert_eq!(next.spend_usd, 2.0);
    }

    #[test]
    fn absent_config_falls_back_to_defaults_never_to_unlimited() {
        let resolved = BudgetConfig::default().resolve();
        let defaults = GateBudgets::default();
        assert_eq!(resolved.max_repair_rounds, defaults.max_repair_rounds);
        assert_eq!(resolved.max_elapsed_secs, defaults.max_elapsed_secs);
        assert!(
            resolved.max_elapsed_secs.is_some(),
            "an unbounded gate is the failure mode this module prevents"
        );
    }

    #[test]
    fn malformed_config_values_fall_back_rather_than_creating_a_dead_gate() {
        let resolved = BudgetConfig {
            max_repair_rounds: None,
            max_spend_usd: Some(-1.0),
            max_elapsed_secs: Some(0),
        }
        .resolve();

        // A zero/negative ceiling would exhaust the gate instantly.
        assert_eq!(resolved.max_spend_usd, GateBudgets::default().max_spend_usd);
        assert_eq!(
            resolved.max_elapsed_secs,
            GateBudgets::default().max_elapsed_secs
        );
    }

    #[test]
    fn zero_repair_rounds_is_honoured_as_verify_once_never_repair() {
        let resolved = BudgetConfig {
            max_repair_rounds: Some(0),
            ..BudgetConfig::default()
        }
        .resolve();
        assert_eq!(resolved.max_repair_rounds, 0);

        // Which means the very first red is terminal.
        let verdict = evaluate(&resolved, &GateSpend::default(), Utc::now(), Utc::now());
        assert!(matches!(
            verdict,
            BudgetVerdict::Exhausted {
                dimension: BudgetDimension::RepairRounds,
                ..
            }
        ));
    }

    #[test]
    fn gate_helper_agrees_with_the_ledger() {
        // `VerificationGate::repair_budget_remains` and `evaluate` must not
        // drift apart — two answers to the same question is a bug waiting to
        // happen.
        let now = Utc::now();
        for (rounds, max_rounds) in [(0u32, 3u32), (2, 3), (3, 3), (5, 3)] {
            let mut g = gate(GateBudgets {
                max_repair_rounds: max_rounds,
                max_spend_usd: None,
                max_elapsed_secs: None,
            });
            g.spend.repair_rounds = rounds;
            assert_eq!(
                g.repair_budget_remains(now),
                evaluate(&g.budgets, &g.spend, g.created_at, now).is_within(),
                "disagreement at rounds={rounds}/{max_rounds}"
            );
        }
    }
}

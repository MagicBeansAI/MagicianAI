//! Spend gate metadata extracted from SpendDeclaration at lowering time.
//!
//! A `SpendGate` captures the resource cost information needed for reserve/commit
//! gating without needing the original capability pack definition.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::magician_v2::execution::capability::SpendDeclaration;

/// Gate metadata for a spend-bearing action.
///
/// Produced during lowering from a pack's `SpendDeclaration` and attached to
/// `SpendGatedAction`. `spend_session::admit` reads these fields at reserve.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpendGate {
    /// The commodity being spent (e.g., "USD", "EMAIL_SENDS", "GITHUB_PUSHES").
    pub commodity: String,
    /// Estimated cost for the reservation. For `Committed` this is the exact cost;
    /// for `Metered` this is the estimated cost; for `Counted` this is cost_per_action.
    pub estimated_cost: Decimal,
    /// Name of the capability pack that declared this spend.
    pub capability_name: String,
    /// Whether this is a metered (variable-cost) action.
    /// When true, `commit_spend` accepts an actual cost that may differ from estimated.
    pub metered: bool,
    /// Hard cap for metered actions. If set, reservation uses min(max_cost, estimated * safety).
    pub max_cost: Option<Decimal>,
}

impl SpendGate {
    /// Lower a pack/skill `SpendDeclaration` into gate metadata.
    pub fn from_declaration(
        spend: &SpendDeclaration,
        capability_name: &str,
        resolved_params: &HashMap<String, serde_json::Value>,
    ) -> Self {
        match spend {
            SpendDeclaration::Committed {
                commodity,
                cost_parameter,
            } => {
                let cost = resolved_params
                    .get(cost_parameter)
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.parse::<Decimal>().ok())
                    .or_else(|| {
                        resolved_params
                            .get(cost_parameter)
                            .and_then(|v| v.as_f64())
                            .map(|f| Decimal::from_f64_retain(f).unwrap_or_default())
                    })
                    .unwrap_or_else(|| {
                        tracing::warn!(
                            capability = capability_name,
                            parameter = %cost_parameter,
                            "committed spend cost_parameter is missing or unparsable; \
                             admit will reject this call if a budget row exists"
                        );
                        Decimal::ZERO
                    });
                Self {
                    commodity: super::canonicalize_commodity(commodity),
                    estimated_cost: cost,
                    capability_name: capability_name.to_owned(),
                    metered: false,
                    max_cost: None,
                }
            },
            SpendDeclaration::Metered {
                commodity,
                estimated_cost,
                max_cost,
            } => Self {
                commodity: super::canonicalize_commodity(commodity),
                estimated_cost: *estimated_cost,
                capability_name: capability_name.to_owned(),
                metered: true,
                max_cost: *max_cost,
            },
            SpendDeclaration::Counted {
                commodity,
                cost_per_action,
            } => Self {
                commodity: super::canonicalize_commodity(commodity),
                estimated_cost: *cost_per_action,
                capability_name: capability_name.to_owned(),
                metered: false,
                max_cost: None,
            },
        }
    }

    /// Amount reserved before I/O. Metered actions take `3 × estimated`,
    /// capped by `max_cost` when set.
    pub fn reserve_amount(&self) -> Decimal {
        if self.metered {
            let multiplied = self.estimated_cost * Decimal::from(3);
            match self.max_cost {
                Some(cap) => multiplied.min(cap),
                None => multiplied,
            }
        } else {
            self.estimated_cost
        }
    }
}

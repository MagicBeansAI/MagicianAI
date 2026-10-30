//! Owner-approved, independent allowance windows over one cumulative ledger.
use std::collections::BTreeSet;

use anyhow::{ensure, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::budget::{BudgetLedger, Settlement, Spend};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExhaustedLimits {
    pub cost: bool,
    pub time: bool,
    pub attempts: bool,
}

/// Missing reset fields retain their existing window; zero removes that cap.
/// This is owner authority, not actor input; constructing the request alone
/// grants no productive work.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetReset {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_microusd: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_secs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts: Option<u32>,
}

impl BudgetReset {
    pub fn validate(&self, exhausted: ExhaustedLimits) -> Result<()> {
        ensure!(
            self.cost_microusd.is_some()
                || self.wall_clock_secs.is_some()
                || self.attempts.is_some(),
            "select an exhausted allowance to reset"
        );
        ensure!(
            self.cost_microusd.is_none_or(|_| exhausted.cost),
            "only an exhausted cost allowance can reset"
        );
        ensure!(
            self.wall_clock_secs.is_none_or(|_| exhausted.time),
            "only an exhausted time allowance can reset"
        );
        ensure!(
            self.attempts.is_none_or(|_| exhausted.attempts),
            "only an exhausted attempt allowance can reset"
        );
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedBudgetReset {
    pub id: Uuid,
    pub approved_by: String,
    pub approved_at: DateTime<Utc>,
    pub reset: BudgetReset,
    pub attempts_before: u32,
    pub spend_before: Spend,
    pub deadline_before: DateTime<Utc>,
    /// Already funded operations/headroom remain charged to their original
    /// window. A late receipt cannot refill the newly approved cost allowance.
    pub prior_operation_ids: BTreeSet<String>,
    pub prior_allowance_ids: BTreeSet<String>,
}

impl ApprovedBudgetReset {
    pub fn prior_cost(&self, ledger: &BudgetLedger) -> Result<u64> {
        ledger.spend()?;
        let mut total = 0u64;
        for (id, operation) in ledger.operations() {
            if !self.prior_operation_ids.contains(id)
                && !operation
                    .funding_allowance_id
                    .as_ref()
                    .is_some_and(|id| self.prior_allowance_ids.contains(id))
            {
                continue;
            }
            let amount = match &operation.settlement {
                Settlement::Reserved | Settlement::OutcomeUnknown => {
                    operation.intent.maximum_cost_microusd
                },
                Settlement::Settled {
                    actual_cost_microusd,
                    ..
                } => *actual_cost_microusd,
                Settlement::NotPerformed { .. } => 0,
            };
            total = total
                .checked_add(amount)
                .context("prior-round cost overflow")?;
        }
        for id in &self.prior_allowance_ids {
            let allowance = ledger
                .allowances()
                .get(id)
                .context("prior review allowance missing")?;
            total = total
                .checked_add(allowance.remaining_microusd)
                .context("prior-round headroom overflow")?;
        }
        Ok(total)
    }
}

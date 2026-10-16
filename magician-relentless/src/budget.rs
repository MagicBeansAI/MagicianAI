//! Shared admission meter. Mutations must be committed with operation intent
//! under the run store's lock before any adapter dispatches. Unknown usage
//! retains the full reservation; restart is not a refund.
use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::contract::valid_hash;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationClass {
    Actor,
    Critic,
    Read,
    Media,
    Delivery,
    Reconciliation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperationIntent {
    pub id: String,
    pub input_hash: String,
    pub class: OperationClass,
    pub maximum_cost_microusd: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Settlement {
    Reserved,
    OutcomeUnknown,
    Settled {
        actual_cost_microusd: u64,
        receipt_hash: String,
    },
    NotPerformed {
        evidence_hash: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Operation {
    pub intent: OperationIntent,
    pub settlement: Settlement,
    #[serde(default)]
    pub funding_allowance_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowance {
    pub id: String,
    pub input_hash: String,
    pub maximum_microusd: u64,
    pub remaining_microusd: u64,
    pub closed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetLedger {
    operations: BTreeMap<String, Operation>,
    allowances: BTreeMap<String, Allowance>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Spend {
    pub settled_microusd: u64,
    pub reserved_microusd: u64,
}

impl Spend {
    pub fn total(self) -> Result<u64> {
        self.settled_microusd
            .checked_add(self.reserved_microusd)
            .ok_or_else(|| anyhow::anyhow!("spend overflow"))
    }
}

impl BudgetLedger {
    pub fn operations(&self) -> &BTreeMap<String, Operation> {
        &self.operations
    }

    pub fn allowances(&self) -> &BTreeMap<String, Allowance> {
        &self.allowances
    }

    pub fn spend(&self) -> Result<Spend> {
        let mut spend = Spend {
            settled_microusd: 0,
            reserved_microusd: 0,
        };
        for (id, operation) in &self.operations {
            ensure!(
                id == &operation.intent.id && valid_hash(&operation.intent.input_hash),
                "invalid operation identity"
            );
            match &operation.settlement {
                Settlement::Reserved | Settlement::OutcomeUnknown => {
                    spend.reserved_microusd = spend
                        .reserved_microusd
                        .checked_add(operation.intent.maximum_cost_microusd)
                        .ok_or_else(|| anyhow::anyhow!("reservation overflow"))?;
                },
                Settlement::Settled {
                    actual_cost_microusd,
                    receipt_hash,
                } => {
                    ensure!(
                        *actual_cost_microusd <= operation.intent.maximum_cost_microusd
                            && valid_hash(receipt_hash),
                        "invalid cost receipt"
                    );
                    spend.settled_microusd = spend
                        .settled_microusd
                        .checked_add(*actual_cost_microusd)
                        .ok_or_else(|| anyhow::anyhow!("settled cost overflow"))?;
                },
                Settlement::NotPerformed { evidence_hash } => {
                    ensure!(
                        valid_hash(evidence_hash),
                        "invalid non-performance evidence"
                    );
                },
            }
        }
        for (id, allowance) in &self.allowances {
            ensure!(
                id == &allowance.id && valid_hash(&allowance.input_hash),
                "invalid allowance identity"
            );
            ensure!(
                !allowance.closed || allowance.remaining_microusd == 0,
                "closed allowance retains unallocated money"
            );
            ensure!(
                allowance.remaining_microusd <= allowance.maximum_microusd,
                "allowance exceeds admitted maximum"
            );
            spend.reserved_microusd = spend
                .reserved_microusd
                .checked_add(allowance.remaining_microusd)
                .ok_or_else(|| anyhow::anyhow!("allowance overflow"))?;
        }
        spend.total()?;
        Ok(spend)
    }

    fn validate_intent(&self, intent: &OperationIntent) -> Result<()> {
        ensure!(
            !intent.id.trim().is_empty() && valid_hash(&intent.input_hash),
            "invalid operation intent"
        );
        ensure!(
            !self.allowances.contains_key(&intent.id),
            "operation ID collides with allowance"
        );
        Ok(())
    }

    /// Atomically reserve actor maximum plus critic/retry headroom. Caller
    /// increments attempts in the same journal transaction. No partial group
    /// is admitted if the sum exceeds the hard ceiling.
    pub fn reserve_attempt(
        &mut self,
        actor: OperationIntent,
        review: Allowance,
        ceiling: u64,
    ) -> Result<()> {
        self.validate_intent(&actor)?;
        ensure!(
            !review.id.trim().is_empty()
                && valid_hash(&review.input_hash)
                && !review.closed
                && review.remaining_microusd == review.maximum_microusd,
            "invalid review allowance"
        );
        ensure!(
            actor.id != review.id && !self.operations.contains_key(&review.id),
            "allowance ID collision"
        );
        if let (Some(old_actor), Some(old_review)) = (
            self.operations.get(&actor.id),
            self.allowances.get(&review.id),
        ) {
            ensure!(
                old_actor.intent == actor
                    && old_review.id == review.id
                    && old_review.input_hash == review.input_hash
                    && old_review.maximum_microusd == review.maximum_microusd,
                "attempt reservation replay differs"
            );
            return Ok(());
        }
        ensure!(
            !self.operations.contains_key(&actor.id) && !self.allowances.contains_key(&review.id),
            "partial/duplicate attempt reservation"
        );
        let total = self
            .spend()?
            .total()?
            .checked_add(actor.maximum_cost_microusd)
            .and_then(|n| n.checked_add(review.remaining_microusd))
            .ok_or_else(|| anyhow::anyhow!("attempt reservation overflow"))?;
        ensure!(
            total <= ceiling,
            "insufficient actor and required-review headroom"
        );
        self.operations.insert(
            actor.id.clone(),
            Operation {
                intent: actor,
                settlement: Settlement::Reserved,
                funding_allowance_id: None,
            },
        );
        self.allowances.insert(review.id.clone(), review);
        Ok(())
    }

    pub fn reserve(&mut self, intent: OperationIntent, ceiling: u64) -> Result<()> {
        self.validate_intent(&intent)?;
        if let Some(old) = self.operations.get(&intent.id) {
            ensure!(
                old.intent == intent,
                "operation ID rebound to different input or cost"
            );
            return Ok(());
        }
        let total = self
            .spend()?
            .total()?
            .checked_add(intent.maximum_cost_microusd)
            .ok_or_else(|| anyhow::anyhow!("reservation overflow"))?;
        ensure!(total <= ceiling, "cost ceiling reached");
        self.operations.insert(
            intent.id.clone(),
            Operation {
                intent,
                settlement: Settlement::Reserved,
                funding_allowance_id: None,
            },
        );
        Ok(())
    }

    /// Spend existing headroom, never a new independent child budget. This
    /// transfer leaves settled-plus-reserved spend unchanged.
    pub fn allocate_review(&mut self, allowance_id: &str, intent: OperationIntent) -> Result<()> {
        self.validate_intent(&intent)?;
        ensure!(
            intent.class == OperationClass::Critic,
            "review allowance may fund only critics"
        );
        if let Some(old) = self.operations.get(&intent.id) {
            ensure!(
                old.intent == intent && old.funding_allowance_id.as_deref() == Some(allowance_id),
                "review operation or funding parent rebound"
            );
            return Ok(());
        }
        let allowance = self
            .allowances
            .get_mut(allowance_id)
            .ok_or_else(|| anyhow::anyhow!("missing review headroom"))?;
        ensure!(!allowance.closed, "review allowance closed");
        let remaining = allowance
            .remaining_microusd
            .checked_sub(intent.maximum_cost_microusd)
            .ok_or_else(|| anyhow::anyhow!("review headroom exhausted"))?;
        allowance.remaining_microusd = remaining;
        self.operations.insert(
            intent.id.clone(),
            Operation {
                intent,
                settlement: Settlement::Reserved,
                funding_allowance_id: Some(allowance_id.into()),
            },
        );
        Ok(())
    }

    /// Release only unallocated review headroom when no further review may be
    /// dispatched. Outstanding operation reservations are untouched.
    pub fn close_review(&mut self, id: &str) -> Result<()> {
        let allowance = self
            .allowances
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("missing review headroom"))?;
        allowance.closed = true;
        allowance.remaining_microusd = 0;
        Ok(())
    }

    pub fn mark_unknown(&mut self, id: &str) -> Result<()> {
        let operation = self
            .operations
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("missing operation intent"))?;
        ensure!(
            matches!(
                operation.settlement,
                Settlement::Reserved | Settlement::OutcomeUnknown
            ),
            "settled operation cannot become unknown"
        );
        operation.settlement = Settlement::OutcomeUnknown;
        Ok(())
    }

    pub fn settle(&mut self, id: &str, actual: u64, receipt_hash: String) -> Result<()> {
        ensure!(valid_hash(&receipt_hash), "invalid receipt hash");
        let operation = self
            .operations
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("receipt without recorded intent"))?;
        ensure!(
            actual <= operation.intent.maximum_cost_microusd,
            "receipt exceeds enforceable reserved maximum"
        );
        let settlement = Settlement::Settled {
            actual_cost_microusd: actual,
            receipt_hash,
        };
        match &operation.settlement {
            Settlement::Reserved | Settlement::OutcomeUnknown => operation.settlement = settlement,
            prior => ensure!(prior == &settlement, "conflicting duplicate receipt"),
        }
        Ok(())
    }

    pub fn not_performed(&mut self, id: &str, evidence_hash: String) -> Result<()> {
        ensure!(
            valid_hash(&evidence_hash),
            "missing non-performance evidence"
        );
        let operation = self
            .operations
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("missing operation intent"))?;
        let settlement = Settlement::NotPerformed { evidence_hash };
        match &operation.settlement {
            Settlement::Reserved | Settlement::OutcomeUnknown => operation.settlement = settlement,
            prior => ensure!(prior == &settlement, "cannot release a settled charge"),
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn hash(text: &str) -> String {
        blake3::hash(text.as_bytes()).to_hex().to_string()
    }
    fn intent(id: &str, cost: u64, class: OperationClass) -> OperationIntent {
        OperationIntent {
            id: id.into(),
            input_hash: hash(id),
            class,
            maximum_cost_microusd: cost,
        }
    }
    #[test]
    fn actor_cannot_consume_critic_headroom_and_group_failure_is_atomic() {
        let mut ledger = BudgetLedger::default();
        let allowance = Allowance {
            id: "reviews".into(),
            input_hash: hash("reviews"),
            maximum_microusd: 40,
            remaining_microusd: 40,
            closed: false,
        };
        assert!(ledger
            .reserve_attempt(
                intent("actor", 70, OperationClass::Actor),
                allowance.clone(),
                100
            )
            .is_err());
        assert_eq!(ledger, BudgetLedger::default());
        ledger
            .reserve_attempt(intent("actor", 60, OperationClass::Actor), allowance, 100)
            .unwrap();
        assert!(ledger
            .reserve(intent("delegate", 1, OperationClass::Actor), 100)
            .is_err());
        ledger
            .allocate_review("reviews", intent("critic", 30, OperationClass::Critic))
            .unwrap();
        assert!(ledger
            .allocate_review(
                "another-parent",
                intent("critic", 30, OperationClass::Critic)
            )
            .is_err());
        assert_eq!(ledger.spend().unwrap().total().unwrap(), 100);
        assert!(ledger
            .allocate_review("reviews", intent("extra", 11, OperationClass::Critic))
            .is_err());
        ledger.settle("actor", 20, hash("actor receipt")).unwrap();
        ledger.settle("critic", 10, hash("critic receipt")).unwrap();
        ledger.close_review("reviews").unwrap();
        assert_eq!(ledger.spend().unwrap().total().unwrap(), 30);
    }
    #[test]
    fn unknown_usage_survives_recovery_and_receipts_settle_once() {
        let mut ledger = BudgetLedger::default();
        ledger
            .reserve(intent("remote-job", 80, OperationClass::Media), 100)
            .unwrap();
        ledger.mark_unknown("remote-job").unwrap();
        let mut recovered: BudgetLedger =
            serde_json::from_slice(&serde_json::to_vec(&ledger).unwrap()).unwrap();
        assert!(recovered
            .reserve(intent("replacement-job", 21, OperationClass::Media), 100)
            .is_err());
        recovered.settle("remote-job", 50, hash("receipt")).unwrap();
        recovered.settle("remote-job", 50, hash("receipt")).unwrap();
        assert!(recovered.settle("remote-job", 49, hash("receipt")).is_err());
        assert!(recovered
            .not_performed("remote-job", hash("absent"))
            .is_err());
        assert_eq!(recovered.spend().unwrap().settled_microusd, 50);
    }
    #[test]
    fn identity_rebinding_overflow_and_unbounded_receipts_are_refused() {
        let mut ledger = BudgetLedger::default();
        ledger
            .reserve(intent("call", 80, OperationClass::Actor), 100)
            .unwrap();
        assert!(ledger
            .reserve(intent("call", 1, OperationClass::Actor), 100)
            .is_err());
        assert!(ledger.settle("call", 81, hash("receipt")).is_err());
        assert!(ledger.settle("missing", 1, hash("receipt")).is_err());
        assert!(ledger
            .reserve(
                intent("overflow", u64::MAX, OperationClass::Actor),
                u64::MAX
            )
            .is_err());
        ledger
            .not_performed("call", hash("definitely not dispatched"))
            .unwrap();
        assert_eq!(ledger.spend().unwrap().total().unwrap(), 0);
    }
}

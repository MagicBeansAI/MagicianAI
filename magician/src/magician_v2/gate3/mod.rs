//! Decision Gate 3 — accepted performance and capacity budgets.
//!
//! Numbers live in `magician-storage` so adapter crates share the same SLO.

pub use magician_storage::{
    accepted_budgets, assert_budget, assert_latency_budget, latency_budgets_enforced,
    percentile_ms, BudgetLine, BudgetVerdict, Gate3Budgets, ENFORCE_LATENCY_ENV, GATE3_ACCEPTED_AT,
    GATE3_CLOSED, GATE3_OWNER, ONLINE_MIGRATION_REQUIRED,
};

#[cfg(test)]
mod drills;

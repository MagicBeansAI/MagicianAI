//! Relentless: a controller-owned contract around existing agent execution.
//!
//! The actor produces candidates; independent critics observe sealed bytes;
//! deterministic code retains, accepts or stops. This module never grants
//! outward authority. Acceptance and delivery are separate runtime operations.
pub mod budget;
pub mod budget_round;
pub mod contract;
pub mod controller;
pub mod decision;
pub mod report;
pub mod review;
pub mod routing;
pub mod specification;
pub mod store;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod store_tests;

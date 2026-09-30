//! Task 18 Track A acceptance: remote-ready while locally active.
//!
//! Default startup stays `local_embedded`. Remote adapters and migration
//! remain unselected. This is not Track B activation.

mod catalog;
mod matrix;

pub use catalog::{load_catalog, CatalogFile, CatalogOwner};
pub use matrix::{load_support_matrix, RemoteOutcome, SupportEntry, SupportMatrix};

pub const TASK1_BASELINE_LINES: u64 = 1_340_581;
pub const TASK1_BASELINE_AT: &str = "2026-08-31";

#[cfg(test)]
mod drills;

//! Disposable Decision Gate 1 spike.
//!
//! Synthetic Magician-shaped transactional workloads for comparing local
//! SQLite with a PostgreSQL dialect. This crate is not a production adapter:
//! `magician-bin` must not depend on it, it must not open user data, and it
//! is not a source of truth.

pub mod postgres;
pub mod scenarios;
pub mod sqlite;
pub mod store;

pub use postgres::PostgresStore;
pub use scenarios::{run_matrix, ScenarioReport};
pub use sqlite::SqliteStore;
pub use store::Gate1Store;

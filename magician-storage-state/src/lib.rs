//! Dormant transactional-state foundations.
//!
//! SQLite is the local pool. PostgreSQL is the remote pool and SQL lease
//! adapter. `magician-bin` must not depend on this crate.

mod example;
mod lease;
mod postgres;
mod sqlite;

pub use example::{ExampleItem, ExampleRepository};
pub use lease::{PostgresLeaseStore, SqliteLeaseStore};
pub use postgres::{
    open_from_profile as open_postgres_from_profile, open_postgres_config, PostgresOptions,
    PostgresPool,
};
pub use sqlite::{PoolSlot, SqlitePool, SqlitePoolPolicy};

pub const EXAMPLE_STORE_ID: &str = "example";

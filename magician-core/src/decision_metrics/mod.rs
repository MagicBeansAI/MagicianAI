//! Incremental decision observability reducers and bounded SQLite summaries.
pub mod health;
pub mod store;
#[cfg(test)]
mod tests;
pub mod types;
pub use health::{HealthQuery, HealthReport};
pub use types::{Dashboard, MetricsQuery, ReviewUpdate};

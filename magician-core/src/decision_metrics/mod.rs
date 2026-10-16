//! Incremental decision observability reducers and bounded SQLite summaries.
pub mod store;
#[cfg(test)]
mod tests;
pub mod types;
pub use types::{Dashboard, MetricsQuery, ReviewUpdate};

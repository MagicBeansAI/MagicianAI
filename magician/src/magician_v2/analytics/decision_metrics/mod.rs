//! Bounded Decision Model capture; no journal replay at startup.
mod service;
#[cfg(test)]
mod tests;
pub use magician_core::decision_metrics::{
    Dashboard, HealthQuery, HealthReport, MetricsQuery, ReviewUpdate,
};
pub use service::{capture, DecisionMetrics};

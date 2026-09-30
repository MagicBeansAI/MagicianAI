//! Module-level constants for the execution engine.
//!
//! All tuning parameters are defined here rather than scattered across files.
//! This follows the same pattern as `verified_executor/constants.rs`.
//!
//! See also: `docs/components/magician/execution/CROSS_LAYER_CONSTANTS.md` for values
//! that must stay synchronized between Rust and JavaScript layers.

// =============================================================================
// Budget Thresholds
// =============================================================================

/// Budget utilization threshold at which a warning is recorded (75%).
/// Used in `executor.rs` budget tracking to flag approaching limits.
pub const BUDGET_WARNING_THRESHOLD: f64 = 0.75;

/// Budget utilization threshold at which a critical alert is recorded (90%).
/// Used in `executor.rs` budget tracking to flag near-exhaustion.
pub const BUDGET_CRITICAL_THRESHOLD: f64 = 0.90;

// =============================================================================
// Inflight Request Deduplication
// =============================================================================

/// Maximum age (seconds) of an inflight/recent request before it is considered
/// stale and eligible for retry. Used in `executor.rs` to prevent duplicate
/// sends when recovering from crashes or reconnects.
pub const INFLIGHT_TIMEOUT_SECS: i64 = 30;

// =============================================================================
// Inference Confidence
// =============================================================================

/// Default confidence threshold for parameter inference from page state.
/// Used in both `types.rs` (static threshold) and `executor.rs` (inline default).
pub const INFERENCE_CONFIDENCE_THRESHOLD: f64 = 0.75;

// =============================================================================
// Observation Policy Defaults
// =============================================================================

/// Minimum time between observations in seconds.
/// Prevents excessive re-observation when steps execute quickly.
pub const MIN_OBSERVATION_INTERVAL_SECS: i64 = 5;

/// Budget threshold below which observations are skipped (fraction remaining).
/// When less than 20% of budget remains, skip observations to preserve budget.
pub const OBSERVATION_BUDGET_THRESHOLD: f64 = 0.2;

// =============================================================================
// Scroll Defaults
// =============================================================================

/// Default scroll amount in pixels when lowering plan steps to browser actions.
/// Intentionally larger than ACTION_SCROLL_DEFAULT_PX (300px) because planning
/// benefits from larger scroll steps to cover more ground per iteration.
///
/// CROSS-LAYER: The action-level default (300px) lives in
/// `magicutor/src/types/execution.rs` and must match the JS extension default.
///
pub const PLAN_SCROLL_DEFAULT_PX: i32 = 400;

// =============================================================================
// Test Automation Attributes
// =============================================================================

/// Canonical list of test-automation attributes checked when building element
/// selectors or enriching observation context. Ordered by convention priority:
/// 1. data-testid (React Testing Library / general convention)
/// 2. data-test-id (alternative hyphenation)
/// 3. data-cy (Cypress convention)
/// 4. data-test (generic convention)
///
/// CROSS-LAYER: Must match `TEST_ATTRIBUTES` in `magicutor/extension/config.js`.
pub const TEST_AUTOMATION_ATTRIBUTES: &[&str] =
    &["data-testid", "data-test-id", "data-cy", "data-test"];

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    /// Verify that constants preserve their original values.
    /// This test exists to catch accidental changes during refactoring.
    #[test]
    fn constants_preserve_original_values() {
        assert_eq!(BUDGET_WARNING_THRESHOLD, 0.75);
        assert_eq!(BUDGET_CRITICAL_THRESHOLD, 0.90);
        assert_eq!(INFLIGHT_TIMEOUT_SECS, 30);
        assert_eq!(INFERENCE_CONFIDENCE_THRESHOLD, 0.75);
        assert_eq!(MIN_OBSERVATION_INTERVAL_SECS, 5);
        assert_eq!(OBSERVATION_BUDGET_THRESHOLD, 0.2);
        assert_eq!(PLAN_SCROLL_DEFAULT_PX, 400);
        assert_eq!(
            TEST_AUTOMATION_ATTRIBUTES,
            &["data-testid", "data-test-id", "data-cy", "data-test"]
        );
    }
}

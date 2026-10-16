//! Module-level constants for browser observation stability.
//!
//! All tuning parameters are defined here rather than in YAML config.
//! This simplifies the system and avoids config/code sync issues.

// =============================================================================
// Stability Verification
// =============================================================================

/// Consecutive matching snapshots needed to consider page stable
pub const STABILITY_SNAPSHOTS_REQUIRED: usize = 2;

/// Interval between stability snapshots (milliseconds)
pub const STABILITY_SNAPSHOT_INTERVAL_MS: u64 = 50;

/// Maximum wait time for page stability (milliseconds)
pub const STABILITY_MAX_WAIT_MS: u64 = 2000;

// =============================================================================
// Criticality Defaults
// =============================================================================

/// Default criticality level when LLM doesn't provide one
/// NOTE: Missing criticality_hint logs an error - this is a prompt bug
pub const DEFAULT_CRITICALITY: &str = "medium";

/// Default value for is_non_idempotent when not provided
pub const DEFAULT_IS_NON_IDEMPOTENT: bool = false;

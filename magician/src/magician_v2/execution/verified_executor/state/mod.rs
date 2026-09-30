//! State Stability Module
//!
//! This module provides state hashing and stability waits for page observation.
//! It does not verify browser action success; the browser inner loop feeds tool
//! results and artifacts back to the LLM, which decides whether the task is done.
//!
//! ## Module Structure
//!
//! - `fingerprint` - State hashing for stability detection
//! - `stability` - Page stability detection (waiting for page to settle)
//!
//! ## Design Philosophy
//!
//! The state module supports observation stability only. It waits for
//! consecutive matching state hashes so the LLM receives a settled snapshot.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use verified_executor::state::wait_for_stability;
//!
//! let result = wait_for_stability(|| async { observe_page().await }).await;
//!
//! ```

pub mod fingerprint;
pub mod stability;

// Re-export primary types
pub use fingerprint::compute_state_hash;
pub use stability::{
    check_immediate_stability, wait_for_stability, wait_for_stability_progressive,
    wait_for_stability_with_config, StabilityResult,
};

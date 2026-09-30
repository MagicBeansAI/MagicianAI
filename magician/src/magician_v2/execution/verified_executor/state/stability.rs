//! Page Stability Detection (v2.0.0)
//!
//! This module provides stability detection for action execution.
//! It waits for the page to settle before evaluating verification
//! to avoid false failures from animation or async content loading.
//!
//! ## Design Philosophy
//!
//! Stability detection is separate from verification:
//! - Stability = waiting for page to stop changing (animations, loading)
//! - Verification = diff-based state comparison
//!
//! The stability window ensures verification is evaluated against a consistent
//! page state rather than a transitional state.

use std::future::Future;
use std::time::Instant;

use tracing::{debug, info, warn};

use super::fingerprint::compute_state_hash;
use crate::magician_v2::execution::types::PageState;
use crate::magician_v2::execution::verified_executor::constants::{
    STABILITY_MAX_WAIT_MS, STABILITY_SNAPSHOTS_REQUIRED, STABILITY_SNAPSHOT_INTERVAL_MS,
};

// =============================================================================
// Stability Types
// =============================================================================

/// Result of stability detection.
#[derive(Debug, Clone)]
pub struct StabilityResult {
    /// Whether the page reached a stable state
    pub is_stable: bool,

    /// The final stable state (or last captured state if timed out)
    pub final_state: PageState,

    /// Number of snapshots taken during stability detection
    pub snapshots_taken: usize,

    /// Time spent waiting for stability in milliseconds
    pub wait_time_ms: u64,

    /// Whether stability detection timed out
    pub timed_out: bool,

    /// Whether the result is degraded (e.g., using minimal state fallback without screenshot).
    /// When true, downstream verification should lower confidence or skip vision-based checks.
    pub degraded: bool,
}

impl StabilityResult {
    /// Create a result for immediate stability (no wait needed).
    pub fn immediate(state: PageState) -> Self {
        Self {
            is_stable: true,
            final_state: state,
            snapshots_taken: 1,
            wait_time_ms: 0,
            timed_out: false,
            degraded: false,
        }
    }

    /// Create a result for successful stability detection.
    pub fn stable(state: PageState, snapshots: usize, wait_ms: u64) -> Self {
        Self {
            is_stable: true,
            final_state: state,
            snapshots_taken: snapshots,
            wait_time_ms: wait_ms,
            timed_out: false,
            degraded: false,
        }
    }

    /// Create a result for successful stability detection with degraded observation.
    /// Used when full capture failed and minimal state is returned as fallback.
    /// Downstream verification should lower confidence or skip vision-based checks.
    pub fn stable_degraded(state: PageState, snapshots: usize, wait_ms: u64) -> Self {
        Self {
            is_stable: true,
            final_state: state,
            snapshots_taken: snapshots,
            wait_time_ms: wait_ms,
            timed_out: false,
            degraded: true,
        }
    }

    /// Create a result for timed out stability detection.
    pub fn timed_out(state: PageState, snapshots: usize, wait_ms: u64) -> Self {
        Self {
            is_stable: false,
            final_state: state,
            snapshots_taken: snapshots,
            wait_time_ms: wait_ms,
            timed_out: true,
            degraded: false,
        }
    }

    /// Create a result for timed out stability detection with degraded observation.
    pub fn timed_out_degraded(state: PageState, snapshots: usize, wait_ms: u64) -> Self {
        Self {
            is_stable: false,
            final_state: state,
            snapshots_taken: snapshots,
            wait_time_ms: wait_ms,
            timed_out: true,
            degraded: true,
        }
    }
}

// =============================================================================
// Stability Detection
// =============================================================================

/// Snapshot of page state with its hash.
struct StateSnapshot {
    hash: u64,
    state: PageState,
}

/// Wait for page stability by capturing consecutive matching snapshots.
///
/// This function captures page state at regular intervals and waits until
/// N consecutive snapshots have the same hash, indicating the page has
/// stopped changing.
///
/// # Arguments
///
/// * `capture_state` - Async function to capture current page state
///
/// # Returns
///
/// `StabilityResult` containing the stable state and timing information.
pub async fn wait_for_stability<F, Fut, E>(capture_state: F) -> StabilityResult
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<PageState, E>>,
    E: std::fmt::Debug,
{
    let start = Instant::now();
    let mut snapshots: Vec<StateSnapshot> = Vec::new();

    info!(
        "[STABILITY] Starting stability detection (need {} matching snapshots)",
        STABILITY_SNAPSHOTS_REQUIRED
    );

    loop {
        // Capture current state
        let current_state = match capture_state().await {
            Ok(state) => state,
            Err(e) => {
                warn!("[STABILITY] State capture failed: {:?}", e);
                // Return last known state if available
                if let Some(last) = snapshots.pop() {
                    return StabilityResult::timed_out(
                        last.state,
                        snapshots.len() + 1,
                        start.elapsed().as_millis() as u64,
                    );
                }
                // No state captured at all - return default
                return StabilityResult::timed_out(
                    PageState::default(),
                    0,
                    start.elapsed().as_millis() as u64,
                );
            },
        };

        // Compute hash for comparison
        let hash = compute_state_hash(&current_state);

        debug!(
            "[STABILITY] Captured snapshot #{} with hash {:x}",
            snapshots.len() + 1,
            hash
        );

        snapshots.push(StateSnapshot {
            hash,
            state: current_state,
        });

        // Check if last N snapshots match
        if snapshots.len() >= STABILITY_SNAPSHOTS_REQUIRED {
            let recent: Vec<u64> = snapshots
                .iter()
                .rev()
                .take(STABILITY_SNAPSHOTS_REQUIRED)
                .map(|s| s.hash)
                .collect();

            // Check if all recent hashes are the same
            if recent.windows(2).all(|w| w[0] == w[1]) {
                let stable_state = snapshots.pop().unwrap().state;
                info!(
                    "[STABILITY] Page stable after {} snapshots, {}ms",
                    snapshots.len() + 1,
                    start.elapsed().as_millis()
                );
                return StabilityResult::stable(
                    stable_state,
                    snapshots.len() + 1,
                    start.elapsed().as_millis() as u64,
                );
            }
        }

        // Check for timeout
        if start.elapsed().as_millis() > STABILITY_MAX_WAIT_MS as u128 {
            let last_state = snapshots.pop().unwrap().state;
            warn!(
                "[STABILITY] Timed out after {}ms, {} snapshots",
                start.elapsed().as_millis(),
                snapshots.len() + 1
            );
            return StabilityResult::timed_out(
                last_state,
                snapshots.len() + 1,
                start.elapsed().as_millis() as u64,
            );
        }

        // Wait before next snapshot
        tokio::time::sleep(tokio::time::Duration::from_millis(
            STABILITY_SNAPSHOT_INTERVAL_MS,
        ))
        .await;
    }
}

/// Wait for stability with a custom configuration.
///
/// This allows overriding the default stability parameters for specific
/// use cases (e.g., longer wait for slow pages, more snapshots for
/// highly dynamic content).
pub async fn wait_for_stability_with_config<F, Fut, E>(
    capture_state: F,
    snapshots_required: usize,
    interval_ms: u64,
    max_wait_ms: u64,
) -> StabilityResult
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<PageState, E>>,
    E: std::fmt::Debug,
{
    let start = Instant::now();
    let mut snapshots: Vec<StateSnapshot> = Vec::new();

    debug!(
        "[STABILITY] Starting stability detection (need {} matching snapshots, max {}ms)",
        snapshots_required, max_wait_ms
    );

    loop {
        // Capture current state
        let current_state = match capture_state().await {
            Ok(state) => state,
            Err(e) => {
                warn!("[STABILITY] State capture failed: {:?}", e);
                if let Some(last) = snapshots.pop() {
                    return StabilityResult::timed_out(
                        last.state,
                        snapshots.len() + 1,
                        start.elapsed().as_millis() as u64,
                    );
                }
                return StabilityResult::timed_out(
                    PageState::default(),
                    0,
                    start.elapsed().as_millis() as u64,
                );
            },
        };

        let hash = compute_state_hash(&current_state);
        snapshots.push(StateSnapshot {
            hash,
            state: current_state,
        });

        // Check if last N snapshots match
        if snapshots.len() >= snapshots_required {
            let recent: Vec<u64> = snapshots
                .iter()
                .rev()
                .take(snapshots_required)
                .map(|s| s.hash)
                .collect();

            if recent.windows(2).all(|w| w[0] == w[1]) {
                let stable_state = snapshots.pop().unwrap().state;
                return StabilityResult::stable(
                    stable_state,
                    snapshots.len() + 1,
                    start.elapsed().as_millis() as u64,
                );
            }
        }

        // Check for timeout
        if start.elapsed().as_millis() > max_wait_ms as u128 {
            let last_state = snapshots.pop().unwrap().state;
            return StabilityResult::timed_out(
                last_state,
                snapshots.len() + 1,
                start.elapsed().as_millis() as u64,
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(interval_ms)).await;
    }
}

// =============================================================================
// Progressive Stability Check
// =============================================================================

/// Progressive stability check: uses minimal observations for hash comparison,
/// then one full observation at the end.
///
/// This is ~2-3x faster than `wait_for_stability` because:
/// - Hash checks use minimal observation (~100ms each vs ~1s)
/// - Full observation only happens once when stability is confirmed
///
/// # Arguments
///
/// * `capture_minimal` - Fast observation for hash comparison (DOM only, no screenshot)
/// * `capture_full` - Full observation for final state (screenshot, interactive elements, etc.)
///
/// # Returns
///
/// `StabilityResult` containing the stable state (from full observation) and timing info.
pub async fn wait_for_stability_progressive<FMin, FutMin, FFull, FutFull, E>(
    capture_minimal: FMin,
    capture_full: FFull,
) -> StabilityResult
where
    FMin: Fn() -> FutMin,
    FutMin: Future<Output = Result<PageState, E>>,
    FFull: Fn() -> FutFull,
    FutFull: Future<Output = Result<PageState, E>>,
    E: std::fmt::Debug,
{
    let start = Instant::now();
    let mut hashes: Vec<u64> = Vec::new();
    // Track last successful minimal state for fallback when full capture fails
    let mut last_minimal_state: Option<PageState> = None;

    debug!(
        "[STABILITY-PROG] Starting progressive stability detection (need {} matching hashes)",
        STABILITY_SNAPSHOTS_REQUIRED
    );

    loop {
        // Use MINIMAL observation for hash comparison
        let (hash, minimal_state) = match capture_minimal().await {
            Ok(state) => {
                let h = compute_state_hash(&state);
                (h, state)
            },
            Err(e) => {
                warn!("[STABILITY-PROG] Minimal state capture failed: {:?}", e);
                // Fall back to full observation, or use last minimal state
                return match capture_full().await {
                    Ok(state) => StabilityResult::timed_out(
                        state,
                        hashes.len() + 1, // +1 for the fallback full capture
                        start.elapsed().as_millis() as u64,
                    ),
                    Err(full_err) => {
                        warn!("[STABILITY-PROG] Full capture also failed: {:?}", full_err);
                        // Use last minimal state if available, otherwise default
                        // Mark as degraded since we're using minimal state (no screenshot)
                        let fallback_state = last_minimal_state.take().unwrap_or_default();
                        StabilityResult::timed_out_degraded(
                            fallback_state,
                            hashes.len(), // No +1 here since full capture failed
                            start.elapsed().as_millis() as u64,
                        )
                    },
                };
            },
        };

        // Store the minimal state for fallback
        last_minimal_state = Some(minimal_state);

        debug!(
            "[STABILITY-PROG] Hash check #{} = {:x} ({}ms elapsed)",
            hashes.len() + 1,
            hash,
            start.elapsed().as_millis()
        );

        hashes.push(hash);

        // Check if last N hashes match
        if hashes.len() >= STABILITY_SNAPSHOTS_REQUIRED {
            let recent: Vec<u64> = hashes
                .iter()
                .rev()
                .take(STABILITY_SNAPSHOTS_REQUIRED)
                .copied()
                .collect();

            if recent.windows(2).all(|w| w[0] == w[1]) {
                // Hashes match! Now do ONE full observation for final state
                debug!(
                    "[STABILITY-PROG] Page stable after {} hash checks ({}ms), capturing full state",
                    hashes.len(),
                    start.elapsed().as_millis()
                );

                return match capture_full().await {
                    Ok(state) => {
                        debug!(
                            "[STABILITY-PROG] Full capture complete, total {}ms",
                            start.elapsed().as_millis()
                        );
                        StabilityResult::stable(
                            state,
                            hashes.len() + 1, // +1 for final full capture
                            start.elapsed().as_millis() as u64,
                        )
                    },
                    Err(e) => {
                        warn!(
                            "[STABILITY-PROG] Full capture failed after stability confirmed: {:?}",
                            e
                        );
                        // Use last minimal state as fallback (page was stable, so this is valid)
                        // Mark as degraded because only the minimal state is available
                        let fallback_state = last_minimal_state.take().unwrap_or_default();
                        warn!(
                            "[STABILITY-PROG] Using last minimal state as fallback (url: {:?}, degraded=true)",
                            fallback_state.url
                        );
                        StabilityResult::stable_degraded(
                            fallback_state,
                            hashes.len(),
                            start.elapsed().as_millis() as u64,
                        )
                    },
                };
            }
        }

        // Check for timeout
        if start.elapsed().as_millis() > STABILITY_MAX_WAIT_MS as u128 {
            warn!(
                "[STABILITY-PROG] Timed out after {}ms, {} hash checks",
                start.elapsed().as_millis(),
                hashes.len()
            );
            // Timeout - do full observation for whatever state we have
            return match capture_full().await {
                Ok(state) => StabilityResult::timed_out(
                    state,
                    hashes.len() + 1,
                    start.elapsed().as_millis() as u64,
                ),
                Err(e) => {
                    warn!("[STABILITY-PROG] Full capture failed on timeout: {:?}", e);
                    // Use last minimal state as fallback
                    // Mark as degraded since minimal state has no screenshot
                    let fallback_state = last_minimal_state.take().unwrap_or_default();
                    StabilityResult::timed_out_degraded(
                        fallback_state,
                        hashes.len(),
                        start.elapsed().as_millis() as u64,
                    )
                },
            };
        }

        // Wait before next hash check
        tokio::time::sleep(tokio::time::Duration::from_millis(
            STABILITY_SNAPSHOT_INTERVAL_MS,
        ))
        .await;
    }
}

// =============================================================================
// Quick Stability Check
// =============================================================================

/// Quick stability check that returns immediately if page is already stable.
///
/// This is useful when you want to verify stability without waiting for
/// the full stability window. It captures two snapshots quickly and
/// compares them.
pub async fn check_immediate_stability<F, Fut, E>(capture_state: F) -> bool
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<PageState, E>>,
    E: std::fmt::Debug,
{
    // Capture first snapshot
    let first = match capture_state().await {
        Ok(state) => compute_state_hash(&state),
        Err(_) => return false,
    };

    // Brief wait
    tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

    // Capture second snapshot
    let second = match capture_state().await {
        Ok(state) => compute_state_hash(&state),
        Err(_) => return false,
    };

    first == second
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stability_result_immediate() {
        let state = PageState::default();
        let result = StabilityResult::immediate(state);

        assert!(result.is_stable);
        assert!(!result.timed_out);
        assert_eq!(result.snapshots_taken, 1);
        assert_eq!(result.wait_time_ms, 0);
    }

    #[tokio::test]
    async fn test_stability_result_timed_out() {
        let state = PageState::default();
        let result = StabilityResult::timed_out(state, 5, 2000);

        assert!(!result.is_stable);
        assert!(result.timed_out);
        assert_eq!(result.snapshots_taken, 5);
        assert_eq!(result.wait_time_ms, 2000);
    }

    #[tokio::test]
    async fn test_wait_for_stability_immediate() {
        // Mock state capture that always returns the same state
        let state = PageState {
            url: Some("https://example.com".to_string()),
            ..Default::default()
        };

        let capture = || async { Ok::<_, String>(state.clone()) };

        let result = wait_for_stability(capture).await;

        assert!(result.is_stable);
        assert!(!result.timed_out);
        assert!(result.snapshots_taken >= STABILITY_SNAPSHOTS_REQUIRED);
    }

    #[tokio::test]
    async fn test_progressive_stability_success() {
        // Minimal observation returns consistent hash
        let minimal_state = PageState {
            url: Some("https://example.com".to_string()),
            ..Default::default()
        };

        // Full observation returns complete state
        let full_state = PageState {
            url: Some("https://example.com".to_string()),
            title: Some("Example Page".to_string()),
            ..Default::default()
        };

        let capture_minimal = || {
            let state = minimal_state.clone();
            async move { Ok::<_, String>(state) }
        };

        let capture_full = || {
            let state = full_state.clone();
            async move { Ok::<_, String>(state) }
        };

        let result = wait_for_stability_progressive(capture_minimal, capture_full).await;

        assert!(result.is_stable);
        assert!(!result.timed_out);
        // Final state should be from full observation
        assert_eq!(result.final_state.title, Some("Example Page".to_string()));
    }

    #[tokio::test]
    async fn test_progressive_stability_full_capture_fails_uses_minimal_fallback() {
        // Minimal observation works
        let minimal_state = PageState {
            url: Some("https://fallback.com".to_string()),
            ..Default::default()
        };

        let capture_minimal = || {
            let state = minimal_state.clone();
            async move { Ok::<_, String>(state) }
        };

        // Full observation always fails
        let capture_full = || async { Err::<PageState, String>("Full capture failed".to_string()) };

        let result = wait_for_stability_progressive(capture_minimal, capture_full).await;

        // Should still be stable (hashes matched) but using minimal state as fallback
        assert!(result.is_stable);
        assert!(!result.timed_out);
        // Should be marked as degraded since we're using minimal state (no screenshot)
        assert!(
            result.degraded,
            "Result should be degraded when using minimal fallback"
        );
        // Should have the minimal state URL, not default empty
        assert_eq!(
            result.final_state.url,
            Some("https://fallback.com".to_string())
        );
    }

    #[tokio::test]
    async fn test_progressive_stability_missing_hash_treated_as_unstable() {
        // This tests that when minimal observation fails, we don't treat it as stable
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::Arc;

        let call_count = Arc::new(AtomicUsize::new(0));

        // First call succeeds, subsequent calls fail
        let capture_minimal = {
            let call_count = call_count.clone();
            move || {
                let count = call_count.fetch_add(1, Ordering::SeqCst);
                async move {
                    if count == 0 {
                        Ok::<PageState, String>(PageState {
                            url: Some("https://first.com".to_string()),
                            ..Default::default()
                        })
                    } else {
                        Err::<PageState, String>("Minimal capture failed".to_string())
                    }
                }
            }
        };

        let capture_full = || {
            async move {
                // Full capture also fails - should use last minimal state
                Err::<PageState, String>("Full capture failed".to_string())
            }
        };

        let result = wait_for_stability_progressive(capture_minimal, capture_full).await;

        // Should be timed out (couldn't confirm stability), but have the first state
        assert!(result.timed_out);
        // Should have the first minimal state URL, not default empty
        assert_eq!(
            result.final_state.url,
            Some("https://first.com".to_string())
        );
    }
}

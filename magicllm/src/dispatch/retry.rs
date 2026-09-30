//! Retry policy: decide what to do after a failed attempt.

use std::time::Duration;

use super::config::DispatchConfig;
use super::job::{AttemptHistory, ErrorClass};

/// The retry decision after observing an attempt's outcome.
#[derive(Debug)]
pub enum RetryDecision {
    /// Retry in place after sleeping for the supplied delay.
    RetryInPlace {
        backoff: Duration,
        next_attempt: u32,
    },
    /// Exhausted in-place budget — re-queue at TAIL of same lane after delay.
    Requeue { backoff: Duration, next_cycle: u32 },
    /// Terminal failure; surface to caller.
    TerminalFail,
}

/// Decide based on classified outcome + current history + config.
///
/// `class` is the just-observed error's class; for a `RateLimit` with a
/// non-default Retry-After, the caller may want to override the backoff
/// — pass `override_backoff = Some(...)` to do so.
pub fn decide(
    history: &AttemptHistory,
    class: ErrorClass,
    config: &DispatchConfig,
    override_backoff: Option<Duration>,
) -> RetryDecision {
    // Non-retriable always fails fast.
    if !class.is_retriable() {
        return RetryDecision::TerminalFail;
    }

    let ceiling = config.retry.hard_ceiling();
    if history.total >= ceiling {
        return RetryDecision::TerminalFail;
    }

    // Check if there is budget left in the current dispatch cycle.
    // `history.dispatch` is the count of attempts made so far in this cycle
    // (just incremented by the worker on failure). If that count is still
    // below the per-cycle max, we have room for another in-place retry.
    // Off-by-one note: must use `<`, not `<=` — when history.dispatch == max
    // we've already made max attempts and should escalate to re-queue.
    if history.dispatch < config.retry.max_attempts_per_dispatch {
        let next_attempt = history.dispatch + 1;
        let backoff = override_backoff.unwrap_or_else(|| config.in_place_backoff(next_attempt));
        return RetryDecision::RetryInPlace {
            backoff,
            next_attempt,
        };
    }

    // In-place budget exhausted; can we re-queue?
    let next_cycle = history.cycle + 1;
    if next_cycle >= config.retry.max_dispatch_cycles {
        return RetryDecision::TerminalFail;
    }
    let backoff = override_backoff.unwrap_or_else(|| config.requeue_backoff(next_cycle));
    RetryDecision::Requeue {
        backoff,
        next_cycle,
    }
}

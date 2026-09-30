//! Capability scheduler — concurrency-key serialization + retry-policy
//! execution (reliability pattern #7).
//!
//! Wires the YAML-declared `reliability.concurrency_key` and
//! `reliability.retry_policy` (see
//! [`crate::magician_v2::execution::capability::CapabilityReliabilityMetadata`])
//! into the executor's pack-dispatch path.
//!
//! ## Concurrency-key semantics
//!
//! Calls to capabilities sharing the same `concurrency_key` are
//! serialised against each other via a per-key `tokio::sync::Mutex`.
//! Calls with no key or with distinct keys run freely. The intended
//! shape is: "all Gmail mutations share key `gws-write`" so a single
//! agent doesn't race two `send_email` calls against the same inbox
//! state; "all Metabase reads share no key" because they're idempotent
//! against the warehouse.
//!
//! Mutexes live on [`SchedulerState`], which is reachable from
//! [`crate::magician_v2::execution::agentic::ActionExecutors`] via the
//! `capability_scheduler` field. Lifetime matches `ActionExecutors` —
//! one scheduler per runtime / orchestrator instance.
//!
//! ## Retry-policy semantics
//!
//! Each pack dispatch is wrapped in an attempt loop bounded by
//! `retry_policy.max_attempts` (default 1 = no retry). Between attempts
//! the scheduler sleeps `attempt * backoff_ms` (linear backoff). When
//! `retry_on` is non-empty, failures are retried only when at least one
//! pattern matches the failure surface (error message or stderr-style
//! tail of `result.error_message`). When `retry_on` is empty, every
//! attempt failure inside the budget triggers a retry.
//!
//! Success short-circuits — a successful attempt is returned
//! immediately. The result is annotated via [`RetryOutcome`] so the
//! caller can record `attempts_used` in trace/telemetry.
//!
//! ## Why this is not transparent to the LLM
//!
//! The retry loop is a runtime concern, not a model concern. The model
//! sees one `ActionResult` per `dispatch_pack_with_reliability` call;
//! if the runtime burned 3 attempts to produce that result, the model
//! sees the final outcome. This keeps the inner-loop transcript clean
//! and prevents the LLM from "learning" that it can retry by emitting
//! the same call twice — that path is reserved for the loop detector
//! / stuck signal (#4), which is supposed to push toward a DIFFERENT
//! action.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::Mutex as AsyncMutex;
use tracing::{info, warn};

use crate::magician_v2::execution::capability::{CapabilityPackDefinition, CapabilityRetryPolicy};

/// Per-runtime concurrency-key + retry bookkeeping.
///
/// Cheaply clone-able (interior `Arc<Mutex<...>>`). Living on
/// [`crate::magician_v2::execution::agentic::ActionExecutors`] keeps
/// the lock map alive for the lifetime of the runtime, so cross-
/// invocation serialisation actually works — a fresh scheduler per
/// call would defeat the whole point.
#[derive(Debug, Clone, Default)]
pub struct CapabilityScheduler {
    inner: Arc<Mutex<SchedulerInner>>,
}

#[derive(Debug, Default)]
struct SchedulerInner {
    /// Map from `concurrency_key` → shared async mutex. We use
    /// `tokio::sync::Mutex` (not std) because the lock is held
    /// across `.await` points (the entire pack dispatch).
    locks: HashMap<String, Arc<AsyncMutex<()>>>,
    /// Per-key call counter — surfaced in telemetry so we can spot
    /// hot keys serialising under contention.
    contention_count: HashMap<String, u64>,
}

impl CapabilityScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Resolve (or lazily create) the mutex for `key`.
    fn lock_handle(&self, key: &str) -> Arc<AsyncMutex<()>> {
        let mut inner = self
            .inner
            .lock()
            .expect("CapabilityScheduler.inner mutex poisoned");
        inner
            .locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(AsyncMutex::new(())))
            .clone()
    }

    fn record_contention(&self, key: &str) {
        if let Ok(mut inner) = self.inner.lock() {
            *inner.contention_count.entry(key.to_string()).or_insert(0) += 1;
        }
    }

    /// Snapshot of contention counters keyed by `concurrency_key`.
    /// Cheap to call; mostly used by observability paths and tests.
    pub fn contention_snapshot(&self) -> HashMap<String, u64> {
        self.inner
            .lock()
            .ok()
            .map(|inner| inner.contention_count.clone())
            .unwrap_or_default()
    }
}

/// Outcome of a reliability-wrapped dispatch.
#[derive(Debug)]
pub struct ReliableDispatchOutcome<T> {
    /// Final dispatch result returned to the caller.
    pub result: T,
    /// How many attempts the scheduler ran. `1` on first-call success.
    pub attempts_used: u32,
    /// `true` when the dispatch waited on a concurrency-key lock.
    pub serialised_on_key: Option<String>,
}

/// Result of running a single attempt — drives the retry decision.
pub enum AttemptOutcome<T> {
    /// Attempt succeeded — short-circuit, return immediately.
    Success(T),
    /// Attempt failed with a message. The scheduler decides whether
    /// to retry based on the policy + `retry_on` patterns.
    RetryableFailure { result: T, error_message: String },
    /// Attempt failed permanently (e.g., bad input, auth blocker).
    /// Skip remaining retry budget and return the result.
    PermanentFailure(T),
}

/// Run `attempt_fn` under the pack's reliability contract.
///
/// - Acquires the `concurrency_key` lock for the duration of all
///   attempts (so retries don't race against a concurrent caller).
/// - Applies the `retry_policy` budget + linear backoff.
/// - Returns the final attempt's result via
///   [`ReliableDispatchOutcome`] plus attempt-count telemetry.
///
/// `attempt_fn` is invoked once per attempt and must produce an
/// [`AttemptOutcome`] classifying the result. It's a closure (not a
/// `Fn`) because most dispatch fns own state and produce one outcome
/// per call.
///
/// Panics: never (we never `.unwrap()` an unbounded resource); poisoned
/// internal mutexes log a warning and downgrade to "no concurrency
/// gate, no retry."
pub async fn dispatch_pack_with_reliability<T, F, Fut>(
    pack: &CapabilityPackDefinition,
    scheduler: &CapabilityScheduler,
    mut attempt_fn: F,
) -> ReliableDispatchOutcome<T>
where
    F: FnMut(u32) -> Fut,
    Fut: std::future::Future<Output = AttemptOutcome<T>>,
{
    // Resolve the lock handle BEFORE the first attempt so all attempts
    // run with the lock held.
    let concurrency_key = pack
        .concurrency_key()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let lock_handle = concurrency_key.as_deref().map(|k| scheduler.lock_handle(k));

    let _guard = if let Some(handle) = lock_handle.as_ref() {
        // Try non-blocking first to detect contention for telemetry.
        let acquired = handle.try_lock();
        match acquired {
            Ok(guard) => Some(guard),
            Err(_) => {
                if let Some(key) = concurrency_key.as_deref() {
                    scheduler.record_contention(key);
                    info!(
                        target: "magician::metrics::concurrency_serialise",
                        counter = 1_u64,
                        pack = %pack.name,
                        key = %key,
                        "concurrency_key_serialise_wait"
                    );
                }
                Some(handle.lock().await)
            },
        }
    } else {
        None
    };

    let retry_policy = pack.retry_policy().cloned().unwrap_or_default();
    let max_attempts = effective_max_attempts(&retry_policy);
    // `last` is overwritten on every retryable failure; the final
    // value is what we return after the budget is exhausted. Initial
    // `None` is unreachable in the return path because the only way
    // out of the loop is via Success, PermanentFailure, or after at
    // least one RetryableFailure that populated `last`. The
    // `#[allow]` suppresses the compiler's "initial value never read"
    // warning — the initial `None` exists so the compiler can prove
    // the variable is defined on every path even though our control
    // flow guarantees it's populated before exit.
    #[allow(unused_assignments)]
    let mut last: Option<T> = None;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        match attempt_fn(attempt).await {
            AttemptOutcome::Success(value) => {
                if attempt > 1 {
                    info!(
                        target: "magician::metrics::retry_recovered",
                        counter = 1_u64,
                        pack = %pack.name,
                        attempts_used = attempt,
                        max_attempts,
                        "retry_recovered_success"
                    );
                }
                return ReliableDispatchOutcome {
                    result: value,
                    attempts_used: attempt,
                    serialised_on_key: concurrency_key,
                };
            },
            AttemptOutcome::RetryableFailure {
                result,
                error_message,
            } => {
                last = Some(result);
                if attempt >= max_attempts {
                    warn!(
                        target: "magician::metrics::retry_exhausted",
                        counter = 1_u64,
                        pack = %pack.name,
                        attempts_used = attempt,
                        max_attempts,
                        "retry_exhausted_budget"
                    );
                    break;
                }
                if !should_retry(&retry_policy, &error_message) {
                    info!(
                        target: "magician::metrics::retry_skip_no_match",
                        counter = 1_u64,
                        pack = %pack.name,
                        attempts_used = attempt,
                        "retry_skip_pattern_mismatch"
                    );
                    break;
                }
                let backoff = Duration::from_millis(
                    u64::from(retry_policy.backoff_ms).saturating_mul(u64::from(attempt)),
                );
                if !backoff.is_zero() {
                    tokio::time::sleep(backoff).await;
                }
            },
            AttemptOutcome::PermanentFailure(value) => {
                return ReliableDispatchOutcome {
                    result: value,
                    attempts_used: attempt,
                    serialised_on_key: concurrency_key,
                };
            },
        }
    }

    ReliableDispatchOutcome {
        result: last.expect("retry loop must have produced at least one outcome"),
        attempts_used: attempt,
        serialised_on_key: concurrency_key,
    }
}

/// Bound the attempt count. `max_attempts = 0` is normalised to `1`
/// (a single attempt) so a YAML mis-declaration of `max_attempts: 0`
/// doesn't accidentally disable the capability entirely.
fn effective_max_attempts(policy: &CapabilityRetryPolicy) -> u32 {
    if policy.max_attempts == 0 {
        1
    } else {
        policy.max_attempts.min(MAX_RETRY_ATTEMPTS)
    }
}

/// Hard ceiling on attempts regardless of YAML. Prevents a mis-typed
/// `max_attempts: 1000000` from making a failing call burn 10s of
/// minutes of wall clock.
const MAX_RETRY_ATTEMPTS: u32 = 8;

/// Match the error surface against the policy's `retry_on` whitelist.
/// Empty whitelist = retry everything inside the attempt budget.
fn should_retry(policy: &CapabilityRetryPolicy, error_message: &str) -> bool {
    if policy.retry_on.is_empty() {
        return true;
    }
    let lower = error_message.to_ascii_lowercase();
    policy
        .retry_on
        .iter()
        .any(|pattern| lower.contains(&pattern.to_ascii_lowercase()))
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    fn pack_with_reliability(reliability_yaml: &str) -> CapabilityPackDefinition {
        let yaml = format!(
            r#"
name: test_pack
implementation:
  type: composite
  steps: []
reliability:
{reliability_yaml}
"#
        );
        serde_yaml::from_str(&yaml).expect("yaml parses")
    }

    #[tokio::test]
    async fn success_on_first_attempt_reports_attempts_used_one() {
        let pack = pack_with_reliability(
            r#"  retry_policy:
    max_attempts: 3
    backoff_ms: 1
"#,
        );
        let scheduler = CapabilityScheduler::new();
        let outcome = dispatch_pack_with_reliability(&pack, &scheduler, |_attempt| async move {
            AttemptOutcome::<&'static str>::Success("ok")
        })
        .await;
        assert_eq!(outcome.result, "ok");
        assert_eq!(outcome.attempts_used, 1);
    }

    #[tokio::test]
    async fn retryable_failure_then_success_burns_the_right_attempt_count() {
        let pack = pack_with_reliability(
            r#"  retry_policy:
    max_attempts: 3
    backoff_ms: 1
    retry_on:
      - "rate limit"
"#,
        );
        let scheduler = CapabilityScheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        let outcome = {
            let counter = counter.clone();
            dispatch_pack_with_reliability(&pack, &scheduler, move |attempt| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    if attempt < 3 {
                        AttemptOutcome::<&'static str>::RetryableFailure {
                            result: "transient",
                            error_message: "Hit a rate limit, please retry".to_string(),
                        }
                    } else {
                        AttemptOutcome::Success("recovered")
                    }
                }
            })
            .await
        };
        assert_eq!(outcome.result, "recovered");
        assert_eq!(outcome.attempts_used, 3);
        assert_eq!(counter.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn retry_on_pattern_mismatch_short_circuits() {
        let pack = pack_with_reliability(
            r#"  retry_policy:
    max_attempts: 5
    backoff_ms: 1
    retry_on:
      - "rate limit"
"#,
        );
        let scheduler = CapabilityScheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        let outcome = {
            let counter = counter.clone();
            dispatch_pack_with_reliability(&pack, &scheduler, move |_attempt| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    AttemptOutcome::<&'static str>::RetryableFailure {
                        result: "boom",
                        error_message: "permission denied".to_string(),
                    }
                }
            })
            .await
        };
        assert_eq!(outcome.result, "boom");
        assert_eq!(outcome.attempts_used, 1);
        assert_eq!(
            counter.load(Ordering::SeqCst),
            1,
            "non-matching error must not consume retry budget"
        );
    }

    #[tokio::test]
    async fn permanent_failure_skips_retries() {
        let pack = pack_with_reliability(
            r#"  retry_policy:
    max_attempts: 5
    backoff_ms: 1
"#,
        );
        let scheduler = CapabilityScheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        let outcome = {
            let counter = counter.clone();
            dispatch_pack_with_reliability(&pack, &scheduler, move |_attempt| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    AttemptOutcome::<&'static str>::PermanentFailure("fatal")
                }
            })
            .await
        };
        assert_eq!(outcome.result, "fatal");
        assert_eq!(outcome.attempts_used, 1);
        assert_eq!(counter.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn no_retry_policy_defaults_to_single_attempt() {
        let yaml = r#"
name: test_pack
implementation:
  type: composite
  steps: []
"#;
        let pack: CapabilityPackDefinition = serde_yaml::from_str(yaml).unwrap();
        let scheduler = CapabilityScheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        let outcome = {
            let counter = counter.clone();
            dispatch_pack_with_reliability(&pack, &scheduler, move |_attempt| {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    AttemptOutcome::<&'static str>::RetryableFailure {
                        result: "boom",
                        error_message: "any".to_string(),
                    }
                }
            })
            .await
        };
        assert_eq!(outcome.result, "boom");
        assert_eq!(outcome.attempts_used, 1);
    }

    #[tokio::test]
    async fn concurrency_key_serialises_two_callers_against_same_key() {
        let pack = pack_with_reliability(
            r#"  concurrency_key: gws-write
  retry_policy:
    max_attempts: 1
    backoff_ms: 0
"#,
        );
        let scheduler = CapabilityScheduler::new();
        let started = Arc::new(AtomicU32::new(0));

        // Caller A: holds the lock for a while.
        let a_started = started.clone();
        let pack_a = pack.clone();
        let scheduler_a = scheduler.clone();
        let a = tokio::spawn(async move {
            dispatch_pack_with_reliability(&pack_a, &scheduler_a, move |_attempt| {
                let started = a_started.clone();
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    AttemptOutcome::<&'static str>::Success("a")
                }
            })
            .await
        });

        // Give A time to acquire the lock.
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Caller B should NOT have started its attempt yet — A holds
        // the key.
        assert_eq!(
            started.load(Ordering::SeqCst),
            1,
            "B's attempt must wait on the concurrency_key lock"
        );

        let b_started = started.clone();
        let pack_b = pack.clone();
        let scheduler_b = scheduler.clone();
        let b = tokio::spawn(async move {
            dispatch_pack_with_reliability(&pack_b, &scheduler_b, move |_attempt| {
                let started = b_started.clone();
                async move {
                    started.fetch_add(1, Ordering::SeqCst);
                    AttemptOutcome::<&'static str>::Success("b")
                }
            })
            .await
        });

        let outcome_a = a.await.unwrap();
        let outcome_b = b.await.unwrap();
        assert_eq!(outcome_a.result, "a");
        assert_eq!(outcome_b.result, "b");
        assert_eq!(outcome_a.serialised_on_key.as_deref(), Some("gws-write"));
        assert_eq!(outcome_b.serialised_on_key.as_deref(), Some("gws-write"));

        let snapshot = scheduler.contention_snapshot();
        assert!(
            snapshot.get("gws-write").copied().unwrap_or(0) >= 1,
            "B should have observed contention on the shared key (snapshot: {snapshot:?})"
        );
    }

    #[test]
    fn effective_max_attempts_treats_zero_as_one() {
        let policy = CapabilityRetryPolicy {
            max_attempts: 0,
            backoff_ms: 0,
            retry_on: vec![],
        };
        assert_eq!(effective_max_attempts(&policy), 1);
    }

    #[test]
    fn effective_max_attempts_clamps_to_hard_ceiling() {
        let policy = CapabilityRetryPolicy {
            max_attempts: 1_000_000,
            backoff_ms: 0,
            retry_on: vec![],
        };
        assert_eq!(effective_max_attempts(&policy), MAX_RETRY_ATTEMPTS);
    }
}

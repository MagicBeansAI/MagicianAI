//! Per-provider resilience state: concurrency cap, cool-down on 429,
//! circuit breaker on sustained failures.

use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;

use crate::capability::LLMProviderKind;
use crate::error::LLMError;

use super::config::{BreakerConfig, ProviderConcurrencyConfig};
use super::job::ErrorClass;

/// Top-level map keyed by provider kind.
pub struct ProviderStateMap {
    providers: DashMap<String, Arc<ProviderState>>,
    concurrency_config: ProviderConcurrencyConfig,
    breaker_config: BreakerConfig,
}

impl ProviderStateMap {
    /// Construct an empty map; entries are materialised lazily on first access.
    pub fn new(
        concurrency_config: ProviderConcurrencyConfig,
        breaker_config: BreakerConfig,
    ) -> Self {
        Self {
            providers: DashMap::new(),
            concurrency_config,
            breaker_config,
        }
    }

    /// Get-or-create the state slot for a provider.
    pub fn get(&self, kind: &LLMProviderKind) -> Arc<ProviderState> {
        let key = kind.as_str().to_string();
        if let Some(existing) = self.providers.get(&key) {
            return existing.clone();
        }
        let permits = self.concurrency_config.for_provider(kind);
        let state = Arc::new(ProviderState {
            kind: kind.clone(),
            concurrency: Arc::new(Semaphore::new(permits)),
            permits_total: permits,
            cooldown_until_unix_ms: AtomicI64::new(0),
            breaker: Mutex::new(CircuitBreaker::new(self.breaker_config.clone())),
            consecutive_failures: AtomicU32::new(0),
            recent_429_count: AtomicU32::new(0),
            in_flight: AtomicU32::new(0),
            requests_total: AtomicU64::new(0),
            errors_429: AtomicU64::new(0),
            errors_5xx: AtomicU64::new(0),
            errors_4xx: AtomicU64::new(0),
        });
        self.providers.entry(key).or_insert(state).clone()
    }

    /// Iterate all materialised provider states (for snapshots).
    pub fn iter(&self) -> Vec<Arc<ProviderState>> {
        self.providers
            .iter()
            .map(|entry| entry.value().clone())
            .collect()
    }
}

/// Per-provider state slot.
pub struct ProviderState {
    pub kind: LLMProviderKind,
    pub concurrency: Arc<Semaphore>,
    pub permits_total: usize,
    /// Unix millis at which cool-down expires; 0 = none active.
    cooldown_until_unix_ms: AtomicI64,
    breaker: Mutex<CircuitBreaker>,
    pub consecutive_failures: AtomicU32,
    recent_429_count: AtomicU32,
    pub in_flight: AtomicU32,
    pub requests_total: AtomicU64,
    pub errors_429: AtomicU64,
    pub errors_5xx: AtomicU64,
    pub errors_4xx: AtomicU64,
}

impl ProviderState {
    /// Returns `Some(remaining)` if cooldown is currently active; `None` otherwise.
    pub fn cooldown_remaining(&self) -> Option<Duration> {
        let until_ms = self.cooldown_until_unix_ms.load(Ordering::Relaxed);
        if until_ms == 0 {
            return None;
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        if until_ms <= now_ms {
            // expired — clear it
            let _ = self.cooldown_until_unix_ms.compare_exchange(
                until_ms,
                0,
                Ordering::AcqRel,
                Ordering::Relaxed,
            );
            return None;
        }
        Some(Duration::from_millis((until_ms - now_ms) as u64))
    }

    /// Engage cool-down for the supplied delay (from a 429 response).
    /// Uses saturating arithmetic so a pathological delay can't overflow
    /// the i64 unix-millis timestamp.
    pub fn engage_cooldown(&self, delay: Duration) {
        let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
        let until_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_add(delay_ms);
        self.cooldown_until_unix_ms
            .store(until_ms, Ordering::Release);
        self.recent_429_count.fetch_add(1, Ordering::Relaxed);
    }

    /// Whether the circuit breaker is currently rejecting new work.
    pub fn breaker_is_open(&self) -> bool {
        self.breaker.lock().is_open_now()
    }

    /// Snapshot of breaker state for the viewer.
    pub fn breaker_state(&self) -> BreakerState {
        self.breaker.lock().state()
    }

    /// Update state based on a call outcome.
    pub fn observe(&self, class: Option<ErrorClass>) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        match class {
            None => {
                // success
                self.consecutive_failures.store(0, Ordering::Relaxed);
                // Decay the 429 counter on success so the fallback Retry-After
                // doesn't stay maxed-out indefinitely after a transient burst.
                self.recent_429_count
                    .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |c| {
                        Some(c.saturating_sub(1))
                    })
                    .ok();
                self.breaker.lock().on_success();
            },
            Some(ErrorClass::RateLimit) => {
                self.errors_429.fetch_add(1, Ordering::Relaxed);
                // breaker tracks 5xx/network, not 429; do not count toward open
            },
            Some(ErrorClass::Server5xx) | Some(ErrorClass::Network) | Some(ErrorClass::Timeout) => {
                self.errors_5xx.fetch_add(1, Ordering::Relaxed);
                self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
                self.breaker.lock().on_failure();
            },
            Some(ErrorClass::Provider4xx) | Some(ErrorClass::ContentPolicy) => {
                self.errors_4xx.fetch_add(1, Ordering::Relaxed);
                // 4xx is caller bug, not provider fault — don't trip breaker
            },
            Some(_) => {
                // ParseError / Cancelled / Unknown — record but don't trip
            },
        }
    }

    /// Compute a "Retry-After" fallback when the provider didn't supply one.
    pub fn fallback_retry_after(&self) -> Duration {
        let count = self.recent_429_count.load(Ordering::Relaxed).clamp(1, 8) as u64;
        Duration::from_secs((count * 5).min(60))
    }
}

/// Circuit breaker tri-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BreakerState {
    Closed,
    Open,
    HalfOpen,
}

struct CircuitBreaker {
    config: BreakerConfig,
    state: BreakerState,
    consecutive_failures: u32,
    window_start: Instant,
    opened_at: Option<Instant>,
    /// Doubled on each repeat open.
    current_cooldown: Duration,
}

impl CircuitBreaker {
    fn new(config: BreakerConfig) -> Self {
        let cooldown = Duration::from_secs(config.cooldown_secs);
        Self {
            config,
            state: BreakerState::Closed,
            consecutive_failures: 0,
            window_start: Instant::now(),
            opened_at: None,
            current_cooldown: cooldown,
        }
    }

    fn is_open_now(&mut self) -> bool {
        if self.state == BreakerState::Open {
            if let Some(opened) = self.opened_at {
                if opened.elapsed() >= self.current_cooldown {
                    // transition Open -> HalfOpen
                    self.state = BreakerState::HalfOpen;
                    return false; // one probe allowed
                }
            }
            return true;
        }
        false
    }

    fn state(&self) -> BreakerState {
        self.state
    }

    fn on_success(&mut self) {
        match self.state {
            BreakerState::Closed => {
                self.consecutive_failures = 0;
            },
            BreakerState::HalfOpen => {
                // probe succeeded — close + reset cooldown
                self.state = BreakerState::Closed;
                self.consecutive_failures = 0;
                self.opened_at = None;
                self.current_cooldown = Duration::from_secs(self.config.cooldown_secs);
            },
            BreakerState::Open => {
                // shouldn't normally happen (calls fast-fail in Open), but be defensive
            },
        }
    }

    fn on_failure(&mut self) {
        let now = Instant::now();
        if now.duration_since(self.window_start) > Duration::from_secs(self.config.window_secs) {
            self.window_start = now;
            self.consecutive_failures = 0;
        }
        self.consecutive_failures += 1;

        match self.state {
            BreakerState::Closed if self.consecutive_failures >= self.config.threshold => {
                self.state = BreakerState::Open;
                self.opened_at = Some(now);
            },
            BreakerState::HalfOpen => {
                // probe failed — back to Open with doubled cooldown
                self.state = BreakerState::Open;
                self.opened_at = Some(now);
                self.current_cooldown = (self.current_cooldown * 2)
                    .min(Duration::from_secs(self.config.max_cooldown_secs));
            },
            _ => {},
        }
    }
}

/// Extract `Retry-After` from an LLMError::RateLimited, or None.
pub fn retry_after_from(err: &LLMError) -> Option<Duration> {
    match err.root_cause() {
        LLMError::RateLimited { retry_after } => *retry_after,
        _ => None,
    }
}

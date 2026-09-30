//! Static + dynamic configuration knobs for the dispatch queue.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::capability::LLMProviderKind;

use super::capacity::DispatchEngine;
use super::quota::ProviderQuota;

/// Top-level dispatch configuration. Loaded once at boot; thresholds in
/// substructs may be hot-reloaded via `Arc<RwLock<DispatchConfig>>`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchConfig {
    /// Master switch for routing LLM calls through the queue. When `false`,
    /// the queue is still constructed at boot (for observability/cancellation
    /// plumbing) but callers fall back to calling the router directly — the
    /// kill-switch for the `route-through-the-queue` cut. Default `true`.
    #[serde(default = "default_dispatch_enabled")]
    pub enabled: bool,
    /// Execution engine. Magician YAML seed is `provider_isolated`.
    /// `legacy_worker_pool` runs HTTP inside the scheduler worker (rollback).
    #[serde(default = "default_dispatch_engine")]
    pub engine: DispatchEngine,
    /// Number of worker tasks draining the queue.
    pub workers: usize,
    /// Workers kept exclusively for high/normal work. At least one worker is
    /// always allowed to drain background work, even when this equals or
    /// exceeds `workers`.
    #[serde(default = "default_reserved_interactive_workers")]
    pub reserved_interactive_workers: usize,
    /// Process-wide cap on simultaneous non-Ollama provider HTTP. Independent
    /// of per-provider semaphores and the scheduler worker count. `0` disables
    /// the extra cap. Default 16.
    #[serde(default = "default_global_cloud_concurrency")]
    pub global_cloud_concurrency: usize,
    /// Bounded capacity for each priority lane.
    pub queue_capacity_high: usize,
    pub queue_capacity_normal: usize,
    pub queue_capacity_background: usize,
    /// Retained-byte admission complements entry caps so one huge prompt (or
    /// many medium prompts) cannot exhaust the process while waiting in lanes.
    #[serde(default = "default_max_request_bytes")]
    pub max_request_bytes: u64,
    #[serde(default = "default_queue_bytes_high")]
    pub queue_bytes_high: u64,
    #[serde(default = "default_queue_bytes_normal")]
    pub queue_bytes_normal: u64,
    #[serde(default = "default_queue_bytes_background")]
    pub queue_bytes_background: u64,
    #[serde(default = "default_queue_bytes_global")]
    pub queue_bytes_global: u64,
    /// Per-provider concurrency overrides (default applied if absent).
    pub provider_concurrency: ProviderConcurrencyConfig,
    /// Per-provider RPM/TPM token buckets. Missing provider or `0` = unlimited.
    #[serde(default)]
    pub provider_quota: HashMap<String, ProviderQuota>,
    /// Log warn when any lane crosses this depth.
    pub queue_depth_warn: usize,
    /// Multiplier on `profile.timeout_secs` before watchdog fires.
    pub watchdog_factor: f64,
    /// How long to keep recently-completed results for idempotency dedup.
    pub idempotency_window_secs: u64,
    /// How many error entries to record per AttemptHistory before discarding oldest.
    pub max_recorded_errors: usize,
    /// Retry knobs.
    pub retry: RetryConfig,
    /// Circuit-breaker knobs (per provider).
    pub breaker: BreakerConfig,
    /// Local pre-processing config.
    pub local_prep: LocalPrepConfig,
    /// Graceful shutdown deadline for in-flight jobs.
    pub shutdown_timeout_secs: u64,
    /// Ring buffer cap for completed jobs in the registry.
    pub completed_ring_capacity: usize,
    /// Ring buffer cap for tombstoned jobs in the registry.
    pub tombstone_ring_capacity: usize,
}

fn default_dispatch_enabled() -> bool {
    true
}

fn default_dispatch_engine() -> DispatchEngine {
    DispatchEngine::LegacyWorkerPool
}

fn default_reserved_interactive_workers() -> usize {
    1
}

fn default_global_cloud_concurrency() -> usize {
    16
}

fn default_max_request_bytes() -> u64 {
    64 * 1024 * 1024
}

fn default_queue_bytes_high() -> u64 {
    256 * 1024 * 1024
}

fn default_queue_bytes_normal() -> u64 {
    512 * 1024 * 1024
}

fn default_queue_bytes_background() -> u64 {
    512 * 1024 * 1024
}

fn default_queue_bytes_global() -> u64 {
    768 * 1024 * 1024
}

impl Default for DispatchConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            engine: DispatchEngine::LegacyWorkerPool,
            workers: 5,
            reserved_interactive_workers: default_reserved_interactive_workers(),
            global_cloud_concurrency: default_global_cloud_concurrency(),
            queue_capacity_high: 200,
            queue_capacity_normal: 500,
            queue_capacity_background: 1000,
            max_request_bytes: default_max_request_bytes(),
            queue_bytes_high: default_queue_bytes_high(),
            queue_bytes_normal: default_queue_bytes_normal(),
            queue_bytes_background: default_queue_bytes_background(),
            queue_bytes_global: default_queue_bytes_global(),
            provider_concurrency: ProviderConcurrencyConfig::default(),
            provider_quota: HashMap::new(),
            queue_depth_warn: 50,
            watchdog_factor: 2.0,
            idempotency_window_secs: 60,
            max_recorded_errors: 6,
            retry: RetryConfig::default(),
            breaker: BreakerConfig::default(),
            local_prep: LocalPrepConfig::default(),
            shutdown_timeout_secs: 30,
            completed_ring_capacity: 500,
            tombstone_ring_capacity: 500,
        }
    }
}

/// Per-provider concurrency caps. Keyed by `LLMProviderKind` string form.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConcurrencyConfig {
    /// Default cap for any provider not explicitly listed.
    pub default: usize,
    /// Per-provider overrides.
    pub overrides: HashMap<String, usize>,
}

impl Default for ProviderConcurrencyConfig {
    fn default() -> Self {
        let mut overrides = HashMap::new();
        overrides.insert("anthropic".to_string(), 4);
        overrides.insert("openai".to_string(), 4);
        overrides.insert("openrouter".to_string(), 2);
        overrides.insert("ollama".to_string(), 8);
        overrides.insert("gemini".to_string(), 3);
        overrides.insert("deepseek".to_string(), 3);
        overrides.insert("minimax".to_string(), 3);
        Self {
            default: 3,
            overrides,
        }
    }
}

impl ProviderConcurrencyConfig {
    /// Resolve the configured concurrency for a provider, falling back to default.
    pub fn for_provider(&self, kind: &LLMProviderKind) -> usize {
        self.overrides
            .get(kind.as_str())
            .copied()
            .unwrap_or(self.default)
            .max(1)
    }
}

/// Public helper so other modules can derive the same key.
pub fn provider_kind_key(kind: &LLMProviderKind) -> String {
    kind.as_str().to_string()
}

/// Retry policy knobs (two-tier: in-place retries × re-queue cycles).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    pub max_attempts_per_dispatch: u32,
    pub max_dispatch_cycles: u32,
    pub backoff_base_ms: u64,
    pub backoff_cap_ms: u64,
    pub backoff_jitter_pct: f64,
    pub requeue_backoff_base_ms: u64,
    pub requeue_backoff_cap_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts_per_dispatch: 3,
            max_dispatch_cycles: 2,
            backoff_base_ms: 200,
            backoff_cap_ms: 30_000,
            backoff_jitter_pct: 0.25,
            requeue_backoff_base_ms: 5_000,
            requeue_backoff_cap_ms: 60_000,
        }
    }
}

impl RetryConfig {
    /// Hard ceiling: total attempts across all cycles.
    pub fn hard_ceiling(&self) -> u32 {
        self.max_attempts_per_dispatch * self.max_dispatch_cycles
    }
}

/// Circuit-breaker config per provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BreakerConfig {
    /// Consecutive failures (5xx + network) to trip open.
    pub threshold: u32,
    /// Window over which `threshold` is evaluated.
    pub window_secs: u64,
    /// Initial cooldown before transitioning to HalfOpen.
    pub cooldown_secs: u64,
    /// Cap for exponential backoff on repeated trips.
    pub max_cooldown_secs: u64,
}

impl Default for BreakerConfig {
    fn default() -> Self {
        Self {
            threshold: 5,
            window_secs: 60,
            cooldown_secs: 30,
            max_cooldown_secs: 300,
        }
    }
}

/// Local-prep (Ollama summarisation) configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalPrepConfig {
    pub enabled: bool,
    /// Router operation whose mapped Ollama profile supplies the model,
    /// endpoint, and context for direct local prep. Magician resolves this at
    /// config-load time so local prep avoids recursive dispatch without
    /// duplicating model settings.
    #[serde(default)]
    pub operation: Option<String>,
    #[serde(default)]
    pub model: String,
    pub threshold_chars: usize,
    pub max_chars_out: usize,
    pub timeout_secs: u64,
    #[serde(default)]
    pub base_url: String,
    /// Optional Ollama request `keep_alive` override for local-prep calls.
    /// `None` uses the crate/Magician runtime default.
    #[serde(default)]
    pub keep_alive: Option<String>,
    /// Ollama context allocated to local-prep generation requests.
    #[serde(default)]
    pub context_tokens: u32,
    /// Map purpose-name → prompt-template path (resolved by caller).
    pub purpose_prompts: HashMap<String, String>,
    /// When true (default), a job that would call Ollama for local-prep
    /// yields its dispatch worker and waits on the cap-1 coordinator.
    /// `false` restores the pre-PR6 in-worker HOL for emergency rollback.
    #[serde(default = "default_local_prep_yield_worker")]
    pub yield_worker: bool,
}

impl Default for LocalPrepConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            operation: None,
            model: String::new(),
            threshold_chars: 8000,
            max_chars_out: 1500,
            timeout_secs: 60,
            base_url: String::new(),
            keep_alive: None,
            context_tokens: 0,
            purpose_prompts: HashMap::new(),
            yield_worker: default_local_prep_yield_worker(),
        }
    }
}

fn default_local_prep_yield_worker() -> bool {
    true
}

impl DispatchConfig {
    /// Helper to fetch the lane capacity by priority.
    pub fn capacity_for(&self, p: super::types::Priority) -> usize {
        match p {
            super::types::Priority::High => self.queue_capacity_high,
            super::types::Priority::Normal => self.queue_capacity_normal,
            super::types::Priority::Background => self.queue_capacity_background,
        }
    }

    pub fn byte_capacity_for(&self, p: super::types::Priority) -> u64 {
        match p {
            super::types::Priority::High => self.queue_bytes_high,
            super::types::Priority::Normal => self.queue_bytes_normal,
            super::types::Priority::Background => self.queue_bytes_background,
        }
    }

    /// Compute jittered backoff for the n-th in-place retry attempt (1-based).
    pub fn in_place_backoff(&self, attempt: u32) -> Duration {
        backoff(
            self.retry.backoff_base_ms,
            self.retry.backoff_cap_ms,
            attempt,
            self.retry.backoff_jitter_pct,
        )
    }

    /// Compute backoff before a re-queue cycle becomes pickable.
    pub fn requeue_backoff(&self, cycle: u32) -> Duration {
        backoff(
            self.retry.requeue_backoff_base_ms,
            self.retry.requeue_backoff_cap_ms,
            cycle,
            self.retry.backoff_jitter_pct,
        )
    }
}

fn backoff(base_ms: u64, cap_ms: u64, attempt: u32, jitter_pct: f64) -> Duration {
    // base * 2^(attempt-1), capped, with ±jitter.
    let attempt = attempt.max(1);
    let factor = 1u64
        .checked_shl(attempt.saturating_sub(1).min(30))
        .unwrap_or(u64::MAX);
    let raw = base_ms.saturating_mul(factor).min(cap_ms);
    let jitter_range = (raw as f64 * jitter_pct).abs();
    let delta = if jitter_range == 0.0 {
        0.0
    } else {
        // Deterministic-ish but adequate: use rand thread_rng with a small range.
        use rand::Rng;
        rand::thread_rng().gen_range(-jitter_range..=jitter_range)
    };
    let final_ms = (raw as f64 + delta).max(0.0) as u64;
    Duration::from_millis(final_ms)
}

#[cfg(test)]
mod tests {
    use super::LocalPrepConfig;

    #[test]
    fn local_prep_has_no_compiled_model_or_context_fallback() {
        let config = LocalPrepConfig::default();
        assert!(config.operation.is_none());
        assert!(config.model.is_empty());
        assert_eq!(config.context_tokens, 0);
    }
}

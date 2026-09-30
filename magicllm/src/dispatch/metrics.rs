//! Lightweight in-process gauges + counters for the dispatch queue.
//!
//! Designed to be polled by an HTTP /metrics endpoint or scraped by the
//! viewer. Phase 6 will plug into the existing DuckDB analytics pipeline.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;

/// Top-level metrics handle. Cheap to clone.
#[derive(Clone, Default)]
pub struct DispatchMetrics {
    inner: Arc<MetricsInner>,
}

#[derive(Default)]
struct MetricsInner {
    workers_busy: AtomicU64,
    background_oldest_wait_ms: AtomicU64,
    submissions_total: AtomicU64,
    dispatches_total: AtomicU64,
    completions_total: AtomicU64,
    failures_total: AtomicU64,
    tombstones_total: AtomicU64,
    requeues_total: AtomicU64,
    local_prep_runs: AtomicU64,
    local_prep_skips: AtomicU64,
    lag_observations: Mutex<Vec<u64>>,
}

impl DispatchMetrics {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn worker_started(&self) {
        self.inner.workers_busy.fetch_add(1, Ordering::Relaxed);
    }

    pub fn worker_finished(&self) {
        self.inner.workers_busy.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn workers_busy(&self) -> u64 {
        self.inner.workers_busy.load(Ordering::Relaxed)
    }

    pub fn incr_submitted(&self) {
        self.inner.submissions_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_dispatched(&self) {
        self.inner.dispatches_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_completed(&self) {
        self.inner.completions_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_failed(&self) {
        self.inner.failures_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_tombstoned(&self) {
        self.inner.tombstones_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_requeued(&self) {
        self.inner.requeues_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_local_prep_run(&self) {
        self.inner.local_prep_runs.fetch_add(1, Ordering::Relaxed);
    }

    pub fn incr_local_prep_skip(&self) {
        self.inner.local_prep_skips.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_wait_ms(&self, ms: u64) {
        let mut v = self.inner.lag_observations.lock();
        if v.len() >= 4096 {
            v.remove(0);
        }
        v.push(ms);
    }

    pub fn record_background_wait_ms(&self, ms: u64) {
        self.inner
            .background_oldest_wait_ms
            .store(ms, Ordering::Relaxed);
    }

    pub fn background_oldest_wait_ms(&self) -> u64 {
        self.inner.background_oldest_wait_ms.load(Ordering::Relaxed)
    }

    pub fn snapshot(&self) -> MetricsSnapshot {
        let lag = self.inner.lag_observations.lock().clone();
        MetricsSnapshot {
            workers_busy: self.workers_busy(),
            submissions_total: self.inner.submissions_total.load(Ordering::Relaxed),
            dispatches_total: self.inner.dispatches_total.load(Ordering::Relaxed),
            completions_total: self.inner.completions_total.load(Ordering::Relaxed),
            failures_total: self.inner.failures_total.load(Ordering::Relaxed),
            tombstones_total: self.inner.tombstones_total.load(Ordering::Relaxed),
            requeues_total: self.inner.requeues_total.load(Ordering::Relaxed),
            local_prep_runs: self.inner.local_prep_runs.load(Ordering::Relaxed),
            local_prep_skips: self.inner.local_prep_skips.load(Ordering::Relaxed),
            background_oldest_wait_ms: self.background_oldest_wait_ms(),
            wait_observations: lag,
        }
    }
}

/// Plain serialisable snapshot for /metrics-style endpoints.
#[derive(Debug, Clone, serde::Serialize)]
pub struct MetricsSnapshot {
    pub workers_busy: u64,
    pub submissions_total: u64,
    pub dispatches_total: u64,
    pub completions_total: u64,
    pub failures_total: u64,
    pub tombstones_total: u64,
    pub requeues_total: u64,
    pub local_prep_runs: u64,
    pub local_prep_skips: u64,
    pub background_oldest_wait_ms: u64,
    pub wait_observations: Vec<u64>,
}

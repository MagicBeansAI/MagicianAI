//! Content-free wait and occupancy gauges for retrieval HOL.
//!
//! These counters exist so a burst can show *which* queue is blocking without
//! logging query text, scores, or canonical memory. They are observe-only.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::embedding_scheduler::embedding_admission_stats;
use crate::hybrid_result_cache::hybrid_result_cache_enabled;
use crate::lance_runtime::lance_runtime_mode_label;
use crate::lance_table_pool::lance_table_pool_enabled;
use crate::query_embed_batcher::query_embed_batch_enabled;
use crate::query_vector_cache::query_vector_cache_enabled;
use crate::vector_search_mode::vector_search_mode_label;

static JOURNAL_WAITERS: AtomicUsize = AtomicUsize::new(0);
static JOURNAL_WAIT_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static JOURNAL_MAX_WAIT_MS: AtomicUsize = AtomicUsize::new(0);
static JOURNAL_ACQUISITIONS: AtomicU64 = AtomicU64::new(0);

static LANCE_WAITERS: AtomicUsize = AtomicUsize::new(0);
static LANCE_WAIT_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static LANCE_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static LANCE_IN_FLIGHT_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static LANCE_TIMEOUTS: AtomicU64 = AtomicU64::new(0);
static LANCE_CANCELS: AtomicU64 = AtomicU64::new(0);
static LANCE_MAX_WAIT_MS: AtomicUsize = AtomicUsize::new(0);

static READ_SNAPSHOT_HITS: AtomicU64 = AtomicU64::new(0);
static READ_SNAPSHOT_HYDRATES: AtomicU64 = AtomicU64::new(0);
static READ_SNAPSHOT_STALE_IDENTITY: AtomicU64 = AtomicU64::new(0);

static RESULT_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_STORES: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_SINGLEFLIGHT_JOINS: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_LEADER_CANCELS: AtomicU64 = AtomicU64::new(0);
static RESULT_CACHE_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static RESULT_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);
static RESULT_CACHE_WAITERS: AtomicUsize = AtomicUsize::new(0);
static RESULT_CACHE_WAIT_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);

static TABLE_POOL_HITS: AtomicU64 = AtomicU64::new(0);
static TABLE_POOL_MISSES: AtomicU64 = AtomicU64::new(0);
static TABLE_POOL_DISCARDS: AtomicU64 = AtomicU64::new(0);
static TABLE_POOL_INVALIDATIONS: AtomicU64 = AtomicU64::new(0);
static TABLE_POOL_IDLE: AtomicUsize = AtomicUsize::new(0);

static QUERY_VECTOR_CACHE_HITS: AtomicU64 = AtomicU64::new(0);
static QUERY_VECTOR_CACHE_MISSES: AtomicU64 = AtomicU64::new(0);
static QUERY_VECTOR_CACHE_STORES: AtomicU64 = AtomicU64::new(0);
static QUERY_VECTOR_CACHE_EVICTIONS: AtomicU64 = AtomicU64::new(0);
static QUERY_VECTOR_CACHE_ENTRIES: AtomicUsize = AtomicUsize::new(0);
static QUERY_VECTOR_CACHE_BYTES: AtomicUsize = AtomicUsize::new(0);

static QUERY_EMBED_BATCH_PHYSICAL_CALLS: AtomicU64 = AtomicU64::new(0);
static QUERY_EMBED_BATCH_LOGICAL_QUERIES: AtomicU64 = AtomicU64::new(0);
static QUERY_EMBED_BATCH_MAX_FILL: AtomicUsize = AtomicUsize::new(0);
static QUERY_EMBED_BATCH_VALIDATION_FAILURES: AtomicU64 = AtomicU64::new(0);
static QUERY_EMBED_BATCH_DEADLINE_LOSSES: AtomicU64 = AtomicU64::new(0);
static QUERY_EMBED_BATCH_WAITERS: AtomicUsize = AtomicUsize::new(0);

static IVF_PRESENT: AtomicUsize = AtomicUsize::new(0);
static IVF_GENERATION: AtomicU64 = AtomicU64::new(0);
static IVF_DISK_BYTES: AtomicU64 = AtomicU64::new(0);
static IVF_LAST_BUILD_UNIX_MS: AtomicU64 = AtomicU64::new(0);
static ANN_QUERIES: AtomicU64 = AtomicU64::new(0);
static ANN_FALLBACKS: AtomicU64 = AtomicU64::new(0);
static ANN_SHADOW_COMPARES: AtomicU64 = AtomicU64::new(0);
static ANN_SHADOW_MISMATCHES: AtomicU64 = AtomicU64::new(0);
static ANN_SHADOW_RECALL_MILLES: AtomicUsize = AtomicUsize::new(0);
static LAST_FLAT_VECTOR_MS: AtomicUsize = AtomicUsize::new(0);
static LAST_ANN_VECTOR_MS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct RetrievalHolSnapshot {
    pub journal_lock_waiters: usize,
    pub journal_lock_wait_high_water: usize,
    pub journal_lock_max_wait_ms: usize,
    pub journal_lock_acquisitions: u64,
    pub embedding_waiting_foreground: usize,
    pub embedding_active_foreground: usize,
    pub embedding_waiting_writes: usize,
    pub embedding_active_write: bool,
    pub embedding_oldest_write_wait_ms: usize,
    pub lance_runtime: &'static str,
    pub lance_waiters: usize,
    pub lance_wait_high_water: usize,
    pub lance_in_flight: usize,
    pub lance_in_flight_high_water: usize,
    pub lance_max_wait_ms: usize,
    pub lance_timeouts: u64,
    pub lance_cancels: u64,
    pub read_snapshot_hits: u64,
    pub read_snapshot_hydrates: u64,
    pub read_snapshot_stale_identity: u64,
    pub result_cache_enabled: bool,
    pub result_cache_hits: u64,
    pub result_cache_misses: u64,
    pub result_cache_stores: u64,
    pub result_cache_evictions: u64,
    pub result_cache_singleflight_joins: u64,
    pub result_cache_leader_cancels: u64,
    pub result_cache_entries: usize,
    pub result_cache_bytes: usize,
    pub result_cache_waiters: usize,
    pub result_cache_wait_high_water: usize,
    pub lance_table_pool_enabled: bool,
    pub lance_table_pool_hits: u64,
    pub lance_table_pool_misses: u64,
    pub lance_table_pool_discards: u64,
    pub lance_table_pool_invalidations: u64,
    pub lance_table_pool_idle: usize,
    pub query_vector_cache_enabled: bool,
    pub query_vector_cache_hits: u64,
    pub query_vector_cache_misses: u64,
    pub query_vector_cache_stores: u64,
    pub query_vector_cache_evictions: u64,
    pub query_vector_cache_entries: usize,
    pub query_vector_cache_bytes: usize,
    pub query_embed_batch_enabled: bool,
    pub query_embed_batch_physical_calls: u64,
    pub query_embed_batch_logical_queries: u64,
    pub query_embed_batch_max_fill: usize,
    pub query_embed_batch_validation_failures: u64,
    pub query_embed_batch_deadline_losses: u64,
    pub query_embed_batch_waiters: usize,
    pub vector_search_mode: &'static str,
    pub ivf_present: bool,
    pub ivf_generation: u64,
    pub ivf_disk_bytes: u64,
    pub ivf_last_build_unix_ms: u64,
    pub ann_queries: u64,
    pub ann_fallbacks: u64,
    pub ann_shadow_compares: u64,
    pub ann_shadow_mismatches: u64,
    pub ann_shadow_recall_milles: usize,
    pub last_flat_vector_ms: usize,
    pub last_ann_vector_ms: usize,
}

pub fn retrieval_hol_snapshot() -> RetrievalHolSnapshot {
    let embedding = embedding_admission_stats();
    RetrievalHolSnapshot {
        journal_lock_waiters: JOURNAL_WAITERS.load(Ordering::Relaxed),
        journal_lock_wait_high_water: JOURNAL_WAIT_HIGH_WATER.load(Ordering::Relaxed),
        journal_lock_max_wait_ms: JOURNAL_MAX_WAIT_MS.load(Ordering::Relaxed),
        journal_lock_acquisitions: JOURNAL_ACQUISITIONS.load(Ordering::Relaxed),
        embedding_waiting_foreground: embedding.waiting_foreground_reads,
        embedding_active_foreground: embedding.active_foreground_reads,
        embedding_waiting_writes: embedding.waiting_writes,
        embedding_active_write: embedding.active_write,
        embedding_oldest_write_wait_ms: embedding.oldest_write_wait_ms,
        lance_runtime: lance_runtime_mode_label(),
        lance_waiters: LANCE_WAITERS.load(Ordering::Relaxed),
        lance_wait_high_water: LANCE_WAIT_HIGH_WATER.load(Ordering::Relaxed),
        lance_in_flight: LANCE_IN_FLIGHT.load(Ordering::Relaxed),
        lance_in_flight_high_water: LANCE_IN_FLIGHT_HIGH_WATER.load(Ordering::Relaxed),
        lance_max_wait_ms: LANCE_MAX_WAIT_MS.load(Ordering::Relaxed),
        lance_timeouts: LANCE_TIMEOUTS.load(Ordering::Relaxed),
        lance_cancels: LANCE_CANCELS.load(Ordering::Relaxed),
        read_snapshot_hits: READ_SNAPSHOT_HITS.load(Ordering::Relaxed),
        read_snapshot_hydrates: READ_SNAPSHOT_HYDRATES.load(Ordering::Relaxed),
        read_snapshot_stale_identity: READ_SNAPSHOT_STALE_IDENTITY.load(Ordering::Relaxed),
        result_cache_enabled: hybrid_result_cache_enabled(),
        result_cache_hits: RESULT_CACHE_HITS.load(Ordering::Relaxed),
        result_cache_misses: RESULT_CACHE_MISSES.load(Ordering::Relaxed),
        result_cache_stores: RESULT_CACHE_STORES.load(Ordering::Relaxed),
        result_cache_evictions: RESULT_CACHE_EVICTIONS.load(Ordering::Relaxed),
        result_cache_singleflight_joins: RESULT_CACHE_SINGLEFLIGHT_JOINS.load(Ordering::Relaxed),
        result_cache_leader_cancels: RESULT_CACHE_LEADER_CANCELS.load(Ordering::Relaxed),
        result_cache_entries: RESULT_CACHE_ENTRIES.load(Ordering::Relaxed),
        result_cache_bytes: RESULT_CACHE_BYTES.load(Ordering::Relaxed),
        result_cache_waiters: RESULT_CACHE_WAITERS.load(Ordering::Relaxed),
        result_cache_wait_high_water: RESULT_CACHE_WAIT_HIGH_WATER.load(Ordering::Relaxed),
        lance_table_pool_enabled: lance_table_pool_enabled(),
        lance_table_pool_hits: TABLE_POOL_HITS.load(Ordering::Relaxed),
        lance_table_pool_misses: TABLE_POOL_MISSES.load(Ordering::Relaxed),
        lance_table_pool_discards: TABLE_POOL_DISCARDS.load(Ordering::Relaxed),
        lance_table_pool_invalidations: TABLE_POOL_INVALIDATIONS.load(Ordering::Relaxed),
        lance_table_pool_idle: TABLE_POOL_IDLE.load(Ordering::Relaxed),
        query_vector_cache_enabled: query_vector_cache_enabled(),
        query_vector_cache_hits: QUERY_VECTOR_CACHE_HITS.load(Ordering::Relaxed),
        query_vector_cache_misses: QUERY_VECTOR_CACHE_MISSES.load(Ordering::Relaxed),
        query_vector_cache_stores: QUERY_VECTOR_CACHE_STORES.load(Ordering::Relaxed),
        query_vector_cache_evictions: QUERY_VECTOR_CACHE_EVICTIONS.load(Ordering::Relaxed),
        query_vector_cache_entries: QUERY_VECTOR_CACHE_ENTRIES.load(Ordering::Relaxed),
        query_vector_cache_bytes: QUERY_VECTOR_CACHE_BYTES.load(Ordering::Relaxed),
        query_embed_batch_enabled: query_embed_batch_enabled(),
        query_embed_batch_physical_calls: QUERY_EMBED_BATCH_PHYSICAL_CALLS.load(Ordering::Relaxed),
        query_embed_batch_logical_queries: QUERY_EMBED_BATCH_LOGICAL_QUERIES
            .load(Ordering::Relaxed),
        query_embed_batch_max_fill: QUERY_EMBED_BATCH_MAX_FILL.load(Ordering::Relaxed),
        query_embed_batch_validation_failures: QUERY_EMBED_BATCH_VALIDATION_FAILURES
            .load(Ordering::Relaxed),
        query_embed_batch_deadline_losses: QUERY_EMBED_BATCH_DEADLINE_LOSSES
            .load(Ordering::Relaxed),
        query_embed_batch_waiters: QUERY_EMBED_BATCH_WAITERS.load(Ordering::Relaxed),
        vector_search_mode: vector_search_mode_label(),
        ivf_present: IVF_PRESENT.load(Ordering::Relaxed) != 0,
        ivf_generation: IVF_GENERATION.load(Ordering::Relaxed),
        ivf_disk_bytes: IVF_DISK_BYTES.load(Ordering::Relaxed),
        ivf_last_build_unix_ms: IVF_LAST_BUILD_UNIX_MS.load(Ordering::Relaxed),
        ann_queries: ANN_QUERIES.load(Ordering::Relaxed),
        ann_fallbacks: ANN_FALLBACKS.load(Ordering::Relaxed),
        ann_shadow_compares: ANN_SHADOW_COMPARES.load(Ordering::Relaxed),
        ann_shadow_mismatches: ANN_SHADOW_MISMATCHES.load(Ordering::Relaxed),
        ann_shadow_recall_milles: ANN_SHADOW_RECALL_MILLES.load(Ordering::Relaxed),
        last_flat_vector_ms: LAST_FLAT_VECTOR_MS.load(Ordering::Relaxed),
        last_ann_vector_ms: LAST_ANN_VECTOR_MS.load(Ordering::Relaxed),
    }
}

fn journal_wait_begin() {
    let waiting = JOURNAL_WAITERS.fetch_add(1, Ordering::Relaxed) + 1;
    update_max(&JOURNAL_WAIT_HIGH_WATER, waiting);
}

fn journal_wait_end(wait: Duration, acquired: bool) {
    decrement(&JOURNAL_WAITERS);
    if acquired {
        JOURNAL_ACQUISITIONS.fetch_add(1, Ordering::Relaxed);
    }
    update_max(&JOURNAL_MAX_WAIT_MS, millis(wait));
}

fn lance_wait_begin() {
    let waiting = LANCE_WAITERS.fetch_add(1, Ordering::Relaxed) + 1;
    update_max(&LANCE_WAIT_HIGH_WATER, waiting);
}

fn lance_wait_end(wait: Duration, _acquired: bool) {
    decrement(&LANCE_WAITERS);
    update_max(&LANCE_MAX_WAIT_MS, millis(wait));
}

/// Occupancy wait that still ends if the caller is cancelled mid-acquire.
pub(crate) struct OccupancyWait {
    started: Instant,
    end: fn(Duration, bool),
    active: bool,
}

impl OccupancyWait {
    pub(crate) fn journal() -> Self {
        journal_wait_begin();
        Self {
            started: Instant::now(),
            end: journal_wait_end,
            active: true,
        }
    }

    pub(crate) fn lance() -> Self {
        lance_wait_begin();
        Self {
            started: Instant::now(),
            end: lance_wait_end,
            active: true,
        }
    }

    pub(crate) fn finish(mut self) {
        self.end_now(true);
    }

    fn end_now(&mut self, acquired: bool) {
        if self.active {
            self.active = false;
            (self.end)(self.started.elapsed(), acquired);
        }
    }
}

impl Drop for OccupancyWait {
    fn drop(&mut self) {
        // Cancelled before acquire: drop waiters, do not count an acquisition.
        self.end_now(false);
    }
}

pub(crate) fn lance_in_flight_begin() {
    let active = LANCE_IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
    update_max(&LANCE_IN_FLIGHT_HIGH_WATER, active);
}

pub(crate) fn lance_in_flight_end() {
    decrement(&LANCE_IN_FLIGHT);
}

pub(crate) fn record_lance_timeout() {
    LANCE_TIMEOUTS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_lance_cancel() {
    LANCE_CANCELS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_read_snapshot_hit() {
    READ_SNAPSHOT_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_read_snapshot_hydrate() {
    READ_SNAPSHOT_HYDRATES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_hit() {
    RESULT_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_miss() {
    RESULT_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_store() {
    RESULT_CACHE_STORES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_eviction() {
    RESULT_CACHE_EVICTIONS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_singleflight_join() {
    RESULT_CACHE_SINGLEFLIGHT_JOINS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_result_cache_leader_cancel() {
    RESULT_CACHE_LEADER_CANCELS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn set_result_cache_occupancy(entries: usize, bytes: usize) {
    RESULT_CACHE_ENTRIES.store(entries, Ordering::Relaxed);
    RESULT_CACHE_BYTES.store(bytes, Ordering::Relaxed);
}

pub(crate) fn result_cache_wait_begin() {
    let waiting = RESULT_CACHE_WAITERS.fetch_add(1, Ordering::Relaxed) + 1;
    update_max(&RESULT_CACHE_WAIT_HIGH_WATER, waiting);
}

pub(crate) fn result_cache_wait_end() {
    decrement(&RESULT_CACHE_WAITERS);
}

pub(crate) fn record_lance_table_pool_hit() {
    TABLE_POOL_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_lance_table_pool_miss() {
    TABLE_POOL_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_lance_table_pool_discard() {
    TABLE_POOL_DISCARDS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_lance_table_pool_invalidate() {
    TABLE_POOL_INVALIDATIONS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn set_lance_table_pool_idle(idle: usize) {
    TABLE_POOL_IDLE.store(idle, Ordering::Relaxed);
}

pub(crate) fn record_query_vector_cache_hit() {
    QUERY_VECTOR_CACHE_HITS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_query_vector_cache_miss() {
    QUERY_VECTOR_CACHE_MISSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_query_vector_cache_store() {
    QUERY_VECTOR_CACHE_STORES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_query_vector_cache_eviction() {
    QUERY_VECTOR_CACHE_EVICTIONS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn set_query_vector_cache_occupancy(entries: usize, bytes: usize) {
    QUERY_VECTOR_CACHE_ENTRIES.store(entries, Ordering::Relaxed);
    QUERY_VECTOR_CACHE_BYTES.store(bytes, Ordering::Relaxed);
}

pub(crate) fn record_query_embed_batch_dispatch(logical_queries: usize, unique_fill: usize) {
    QUERY_EMBED_BATCH_PHYSICAL_CALLS.fetch_add(1, Ordering::Relaxed);
    QUERY_EMBED_BATCH_LOGICAL_QUERIES.fetch_add(logical_queries as u64, Ordering::Relaxed);
    update_max(&QUERY_EMBED_BATCH_MAX_FILL, unique_fill);
}

pub(crate) fn record_query_embed_batch_validation_failure() {
    QUERY_EMBED_BATCH_VALIDATION_FAILURES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_query_embed_batch_deadline_loss() {
    QUERY_EMBED_BATCH_DEADLINE_LOSSES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn set_query_embed_batch_waiters(waiters: usize) {
    QUERY_EMBED_BATCH_WAITERS.store(waiters, Ordering::Relaxed);
}

pub(crate) fn set_ivf_health(
    present: bool,
    disk_bytes: u64,
    last_build_unix_ms: Option<u64>,
    new_generation: bool,
) {
    IVF_PRESENT.store(usize::from(present), Ordering::Relaxed);
    IVF_DISK_BYTES.store(disk_bytes, Ordering::Relaxed);
    if let Some(built_at) = last_build_unix_ms {
        IVF_LAST_BUILD_UNIX_MS.store(built_at, Ordering::Relaxed);
    }
    if new_generation {
        IVF_GENERATION.fetch_add(1, Ordering::Relaxed);
    }
}

pub(crate) fn record_ann_query() {
    ANN_QUERIES.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_ann_fallback() {
    ANN_FALLBACKS.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_ann_shadow_compare(
    recall_milles: u16,
    mismatch: bool,
    flat_ms: usize,
    ann_ms: usize,
) {
    ANN_SHADOW_COMPARES.fetch_add(1, Ordering::Relaxed);
    if mismatch {
        ANN_SHADOW_MISMATCHES.fetch_add(1, Ordering::Relaxed);
    }
    ANN_SHADOW_RECALL_MILLES.store(recall_milles as usize, Ordering::Relaxed);
    LAST_FLAT_VECTOR_MS.store(flat_ms, Ordering::Relaxed);
    LAST_ANN_VECTOR_MS.store(ann_ms, Ordering::Relaxed);
}

pub(crate) fn record_vector_leg_times(flat_ms: Option<usize>, ann_ms: Option<usize>) {
    if let Some(ms) = flat_ms {
        LAST_FLAT_VECTOR_MS.store(ms, Ordering::Relaxed);
    }
    if let Some(ms) = ann_ms {
        LAST_ANN_VECTOR_MS.store(ms, Ordering::Relaxed);
    }
}

fn millis(wait: Duration) -> usize {
    wait.as_millis().min(usize::MAX as u128) as usize
}

fn decrement(slot: &AtomicUsize) {
    let mut current = slot.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(1);
        match slot.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn update_max(slot: &AtomicUsize, candidate: usize) {
    let mut current = slot.load(Ordering::Relaxed);
    while candidate > current {
        match slot.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_content_free_and_tracks_waiters() {
        OccupancyWait::journal().finish();
        OccupancyWait::lance().finish();
        lance_in_flight_begin();
        record_lance_timeout();
        record_lance_cancel();
        record_read_snapshot_hit();
        record_read_snapshot_hydrate();
        lance_in_flight_end();

        let snap = retrieval_hol_snapshot();
        assert!(snap.journal_lock_acquisitions >= 1);
        assert!(snap.lance_timeouts >= 1);
        assert!(snap.lance_cancels >= 1);
        assert!(snap.read_snapshot_hits >= 1);
        assert!(snap.read_snapshot_hydrates >= 1);
        assert!(matches!(snap.lance_runtime, "ambient" | "dedicated"));
        assert!(matches!(
            snap.vector_search_mode,
            "flat" | "ann_shadow" | "ann"
        ));
    }

    #[test]
    fn occupancy_wait_drop_releases_waiters_without_counting_an_acquire() {
        {
            let _wait = OccupancyWait::journal();
            assert!(retrieval_hol_snapshot().journal_lock_waiters >= 1);
        }
        OccupancyWait::journal().finish();
        assert!(retrieval_hol_snapshot().journal_lock_acquisitions >= 1);
    }

    #[test]
    fn ann_shadow_facts_are_content_free() {
        record_ann_query();
        record_ann_fallback();
        record_ann_shadow_compare(750, true, 12, 4);
        set_ivf_health(true, 1024, Some(1), true);
        let snap = retrieval_hol_snapshot();
        assert!(snap.ann_queries >= 1);
        assert!(snap.ann_fallbacks >= 1);
        assert!(snap.ann_shadow_compares >= 1);
        assert!(snap.ann_shadow_mismatches >= 1);
        assert_eq!(snap.ann_shadow_recall_milles, 750);
        assert!(snap.ivf_present);
        assert_eq!(snap.ivf_disk_bytes, 1024);
        assert!(snap.ivf_generation >= 1);
        assert!(matches!(
            snap.vector_search_mode,
            "flat" | "ann_shadow" | "ann"
        ));
    }
}

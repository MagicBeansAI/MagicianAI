use std::{
    fmt,
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex, OnceLock,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio::time::timeout;

/// Max concurrent harness-root cycles (officer standups + SRE + autofix repair
/// dispatches). Sized to the full officer roster (CEO/CTO/CRO/CMO/CPO) plus the
/// harness-SRE and autofix headroom so the manual company-loop sequence — which
/// fires every officer as a simultaneous StartNow root — and cron collisions do
/// not 429-drop cycles the moment two roots overlap. Was 2 (roster is 6+), which
/// starved the loop and produced spurious `CycleDropped` anomalies.
///
/// This is a compile-time const because the semaphore below is
/// `Semaphore::const_new`. Making it runtime-tunable from `HarnessConfig`
/// requires promoting `HARNESS_ROOT_SEMAPHORE` to a `OnceLock<Semaphore>`
/// initialised at boot from config — tracked as a cross-file request against
/// `magician/src/config.rs` (`HarnessConfig`).
pub const HARNESS_ROOT_PERMIT_LIMIT: usize = 8;

/// Delegated-child permits, **one independent pool per delegation level**.
///
/// A delegated child holds its permit for its whole run — including any
/// delegation IT performs. With a single global pool that is a hold-and-wait
/// cycle: four concurrent level-0 children that each delegate take all four
/// permits and then block, forever, on grandchildren that can never be
/// admitted. `acquire_delegated_child_when_available` has no timeout, and for
/// a coding execution with no armed deadline the watchdog never fires either,
/// so the tree hung permanently.
///
/// Splitting the pool by level removes the cycle rather than shortening it: a
/// holder of a level-`L` permit only ever waits on level `L+1`, so waits are
/// strictly ordered by level and no set of waiters can close a loop. A fan of
/// four that legitimately nests now makes progress — the grandchildren draw on
/// their own level's budget instead of queueing behind their own ancestors.
///
/// Level 0 keeps the historical limit of 4. Deeper levels taper: deep chains
/// are rare and should stay narrow, and a smaller budget only serializes them
/// (each level's holders finish without needing anything from that same
/// level), it never deadlocks them.
pub const DELEGATED_CHILD_LEVELS: usize = 4;
pub const DELEGATED_CHILD_PERMITS_BY_LEVEL: [usize; DELEGATED_CHILD_LEVELS] = [4, 4, 2, 1];

/// Level-0 (root-child) capacity. Kept as a named constant because callers and
/// dashboards refer to "the" delegated-child limit.
pub const DELEGATED_CHILD_PERMIT_LIMIT: usize = DELEGATED_CHILD_PERMITS_BY_LEVEL[0];

/// Ceiling on how long a delegation deeper than the ordered table may wait.
///
/// Past the last level the level ordering no longer separates waiters from
/// holders, so an unbounded wait could reintroduce the cycle. Bound it and let
/// the delegation fail cleanly instead: the caller turns the error into a
/// failed child with a readable reason, which is strictly better than a hung
/// tree.
const DEEP_DELEGATION_MAX_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

pub const INTERNAL_SYSTEM_ANALYST_AGENT_ID: &str = "internal-system-analyst";
pub const INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT: usize = 1;

/// Default hard live-agent cap (same numeric value as the historical
/// observe-only soft limit). PR8 overwrites this from the resolved runtime
/// plan via [`configure_live_agent_limit`]. `0` means observe-only rollback:
/// loops are counted and would-throttle is recorded, but admission never
/// rejects.
pub const DEFAULT_LIVE_AGENT_LIMIT: usize = 50;

/// Default outstanding agent-loop cap per `agent_id`. Other agents remain
/// admissible when one agent is at this cap. `0` disables the per-agent gate.
pub const DEFAULT_PER_AGENT_OUTSTANDING_LIMIT: usize = 8;

const ACTIVE_AGENT_LOOP_SOFT_LIMIT: usize = DEFAULT_LIVE_AGENT_LIMIT;
const SNAPSHOT_SCHEMA_VERSION: u32 = 8;
const PENDING_AGENT_TRIGGER_SOFT_LIMIT: usize = 200;
const RUNTIME_EVENT_BACKLOG_SOFT_LIMIT: usize = 1_000;
const EVENT_APPEND_LOCK_WAIT_SOFT_LIMIT_MS: usize = 100;
const MEMORY_OVERLAY_LOCK_WAIT_SOFT_LIMIT_MS: usize = 100;
const JOURNAL_LOCK_WAITER_SOFT_LIMIT: usize = 4;
const JOURNAL_LOCK_WAIT_SOFT_LIMIT_MS: usize = 100;
const EMBEDDING_WAITING_FOREGROUND_SOFT_LIMIT: usize = 8;
const LANCE_IN_FLIGHT_SOFT_LIMIT: usize = 4;
const LANCE_WAITER_SOFT_LIMIT: usize = 8;

static ACTIVE_AGENT_LOOPS: AtomicUsize = AtomicUsize::new(0);
static ACTIVE_AGENT_LOOPS_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static AGENT_LOOPS_STARTED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_LOOP_WOULD_THROTTLE_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_LOOP_REJECTED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_LOOP_RSS_BLOCKED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_LOOP_PER_AGENT_REJECTED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static RSS_TRIPPED: AtomicBool = AtomicBool::new(false);
static RSS_PROBE: OnceLock<fn() -> u64> = OnceLock::new();

static PENDING_AGENT_TRIGGERS: AtomicUsize = AtomicUsize::new(0);
static PENDING_AGENT_TRIGGERS_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static AGENT_TRIGGERS_QUEUED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_TRIGGER_DUPLICATES_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_TRIGGER_QUEUE_FULL_TOTAL: AtomicUsize = AtomicUsize::new(0);
static AGENT_TRIGGER_WOULD_THROTTLE_TOTAL: AtomicUsize = AtomicUsize::new(0);

static RUNTIME_EVENT_BACKLOG: AtomicUsize = AtomicUsize::new(0);
static RUNTIME_EVENT_BACKLOG_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static RUNTIME_EVENT_BACKLOG_WOULD_THROTTLE_TOTAL: AtomicUsize = AtomicUsize::new(0);

static EVENT_APPEND_LOCK_MAX_WAIT_MS: AtomicUsize = AtomicUsize::new(0);
static EVENT_APPEND_LOCK_PRESSURE_OBSERVATIONS: AtomicUsize = AtomicUsize::new(0);

static LLM_DIRECT_ROUTE_TOTAL: AtomicUsize = AtomicUsize::new(0);
static LLM_DIRECT_STREAM_ROUTE_TOTAL: AtomicUsize = AtomicUsize::new(0);

static HARNESS_ROOT_SEMAPHORE: Semaphore = Semaphore::const_new(HARNESS_ROOT_PERMIT_LIMIT);
static DELEGATED_CHILD_SEMAPHORES: [Semaphore; DELEGATED_CHILD_LEVELS] = [
    Semaphore::const_new(DELEGATED_CHILD_PERMITS_BY_LEVEL[0]),
    Semaphore::const_new(DELEGATED_CHILD_PERMITS_BY_LEVEL[1]),
    Semaphore::const_new(DELEGATED_CHILD_PERMITS_BY_LEVEL[2]),
    Semaphore::const_new(DELEGATED_CHILD_PERMITS_BY_LEVEL[3]),
];
static INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE: Semaphore =
    Semaphore::const_new(INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT);

fn live_agent_limit_slot() -> &'static AtomicUsize {
    static SLOT: OnceLock<AtomicUsize> = OnceLock::new();
    SLOT.get_or_init(|| AtomicUsize::new(DEFAULT_LIVE_AGENT_LIMIT))
}

fn per_agent_outstanding_limit_slot() -> &'static AtomicUsize {
    static SLOT: OnceLock<AtomicUsize> = OnceLock::new();
    SLOT.get_or_init(|| AtomicUsize::new(DEFAULT_PER_AGENT_OUTSTANDING_LIMIT))
}

struct RssTripwireSlots {
    high_bytes: AtomicU64,
    low_bytes: AtomicU64,
}

fn rss_tripwire_slots() -> &'static RssTripwireSlots {
    static SLOTS: OnceLock<RssTripwireSlots> = OnceLock::new();
    SLOTS.get_or_init(|| RssTripwireSlots {
        high_bytes: AtomicU64::new(0),
        low_bytes: AtomicU64::new(0),
    })
}

fn agent_outstanding_counts() -> &'static DashMap<String, usize> {
    static MAP: OnceLock<DashMap<String, usize>> = OnceLock::new();
    MAP.get_or_init(DashMap::new)
}

/// Set the process-wide hard live-agent cap. Default is
/// [`DEFAULT_LIVE_AGENT_LIMIT`] (50). `0` is observe-only rollback and never
/// rejects. magician-bin already calls this from the resolved runtime plan;
/// later calls update the `AtomicUsize` in place.
pub fn configure_live_agent_limit(limit: usize) {
    live_agent_limit_slot().store(limit, Ordering::SeqCst);
}

/// Current hard live-agent cap. `0` means observe-only.
pub fn live_agent_limit() -> usize {
    live_agent_limit_slot().load(Ordering::SeqCst)
}

/// Set the outstanding-loop cap per `agent_id`. Default is
/// [`DEFAULT_PER_AGENT_OUTSTANDING_LIMIT`] (8). `0` disables the per-agent
/// gate. PR8 calls this from the resolved runtime plan.
pub fn configure_per_agent_outstanding_limit(limit: usize) {
    per_agent_outstanding_limit_slot().store(limit, Ordering::SeqCst);
}

/// Current per-agent outstanding-loop cap. `0` disables the gate.
pub fn per_agent_outstanding_limit() -> usize {
    per_agent_outstanding_limit_slot().load(Ordering::SeqCst)
}

/// Install a process RSS probe. Magician-core does not depend on `sysinfo`;
/// the host crate supplies the function. The first call wins (`OnceLock`).
/// If never set, RSS admission is skipped.
pub fn configure_rss_probe(probe: fn() -> u64) {
    let _ = RSS_PROBE.set(probe);
}

/// Set RSS tripwire thresholds in bytes. Admission rejects while RSS is at or
/// above `high_bytes`, and only recovers after RSS falls to `low_bytes`
/// (hysteresis). `high_bytes == 0` disables the tripwire even if a probe is
/// installed. PR8 calls this from the resolved runtime plan.
pub fn configure_rss_tripwire(high_bytes: u64, low_bytes: u64) {
    let slots = rss_tripwire_slots();
    let (high_bytes, low_bytes) = if high_bytes == 0 {
        RSS_TRIPPED.store(false, Ordering::SeqCst);
        (0, 0)
    } else if low_bytes == 0 || low_bytes >= high_bytes {
        // Recover as soon as RSS is strictly below high when no valid band
        // was configured. A low of 0 would otherwise latch the tripwire
        // until RSS is literally zero.
        (high_bytes, high_bytes.saturating_sub(1))
    } else {
        (high_bytes, low_bytes)
    };
    slots.high_bytes.store(high_bytes, Ordering::SeqCst);
    slots.low_bytes.store(low_bytes, Ordering::SeqCst);
}

fn rss_high_bytes() -> u64 {
    rss_tripwire_slots().high_bytes.load(Ordering::SeqCst)
}

fn rss_low_bytes() -> u64 {
    rss_tripwire_slots().low_bytes.load(Ordering::SeqCst)
}

/// Currently admitted (active) agent loops in this process.
pub fn active_agent_loop_count() -> usize {
    ACTIVE_AGENT_LOOPS.load(Ordering::SeqCst)
}

#[must_use]
pub struct HarnessRootPermit {
    _permit: SemaphorePermit<'static>,
}

pub fn try_acquire_harness_root() -> Option<HarnessRootPermit> {
    HARNESS_ROOT_SEMAPHORE
        .try_acquire()
        .ok()
        .map(|permit| HarnessRootPermit { _permit: permit })
}

#[must_use]
pub struct DelegatedChildPermit {
    _global_permit: SemaphorePermit<'static>,
    _target_permit: Option<SemaphorePermit<'static>>,
}

/// Deepest level with its own pool. Anything deeper shares this pool and must
/// therefore use a bounded wait.
fn deepest_ordered_level() -> usize {
    DELEGATED_CHILD_LEVELS - 1
}

fn delegated_child_semaphore(delegation_level: usize) -> &'static Semaphore {
    &DELEGATED_CHILD_SEMAPHORES[delegation_level.min(deepest_ordered_level())]
}

async fn acquire_delegated_child_permits(
    target_agent_id: &str,
    delegation_level: usize,
) -> Result<DelegatedChildPermit, String> {
    let target_permit = if target_agent_id == INTERNAL_SYSTEM_ANALYST_AGENT_ID {
        Some(
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE
                .acquire()
                .await
                .map_err(|_| {
                    "internal-system-analyst delegated-child semaphore closed".to_string()
                })?,
        )
    } else {
        None
    };
    let global_permit = delegated_child_semaphore(delegation_level)
        .acquire()
        .await
        .map_err(|_| format!("delegated-child semaphore for level {delegation_level} closed"))?;

    Ok(DelegatedChildPermit {
        _global_permit: global_permit,
        _target_permit: target_permit,
    })
}

pub async fn acquire_delegated_child(
    target_agent_id: &str,
    delegation_level: usize,
    max_wait: std::time::Duration,
) -> Result<DelegatedChildPermit, String> {
    let acquisition = acquire_delegated_child_permits(target_agent_id, delegation_level);

    timeout(max_wait, acquisition).await.map_err(|_| {
        format!(
            "timed out after {max_wait:?} waiting for a level-{delegation_level} delegated-child permit for target {target_agent_id}"
        )
    })?
}

/// Wait for delegated-child capacity without consuming the caller's active
/// work budget. The caller remains responsible for cancellation (for example,
/// when the parent execution is explicitly stopped).
///
/// `delegation_level` is how many delegation hops already sit above this child
/// (0 for a child of a root execution). The wait is unbounded only while the
/// level ordering guarantees progress — a level-`L` waiter is never blocked by
/// a level-`L` holder, because every holder above it is at a strictly lower
/// level and none of them needs a level-`L` permit to finish. Past the last
/// ordered level that guarantee is gone, so the wait is bounded and the
/// delegation fails cleanly instead of hanging.
pub async fn acquire_delegated_child_when_available(
    target_agent_id: &str,
    delegation_level: usize,
) -> Result<DelegatedChildPermit, String> {
    if delegation_level < deepest_ordered_level() {
        return acquire_delegated_child_permits(target_agent_id, delegation_level).await;
    }
    acquire_delegated_child(target_agent_id, delegation_level, DEEP_DELEGATION_MAX_WAIT).await
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalResourceGovernorSnapshot {
    pub schema_version: u32,
    pub captured_at_ms: u64,
    pub mode: &'static str,
    pub observe_only: bool,
    /// Hybrid vector-leg mode label (`flat`, `ann_shadow`, or `ann`).
    pub vector_search_mode: &'static str,
    /// Currently admitted (active) agent loops.
    #[serde(default)]
    pub admitted: usize,
    /// Total live-agent-cap rejections.
    #[serde(default)]
    pub rejected: usize,
    /// Total RSS-tripwire rejections.
    #[serde(default)]
    pub rss_blocked: usize,
    /// Total per-agent outstanding-cap rejections.
    #[serde(default)]
    pub per_agent_rejects: usize,
    #[serde(default)]
    pub blocking_admission_waiting: usize,
    #[serde(default)]
    pub blocking_admission_in_flight: usize,
    #[serde(default)]
    pub blocking_admission_high_water: usize,
    #[serde(default)]
    pub blocking_admission_wait_high_water_ms: usize,
    #[serde(default)]
    pub blocking_admission_admitted_total: usize,
    pub status: ResourceSeverity,
    pub summary: LocalResourceSummary,
    pub resources: Vec<ResourceGauge>,
    pub pressure: Vec<ResourcePressureSignal>,
    pub counters: LocalResourceCounters,
    pub latest_agent_loop: Option<ObservedAgentLoop>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalResourceSummary {
    pub resource_count: usize,
    pub pressure_count: usize,
    pub watch_count: usize,
    pub would_throttle_total: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourceGauge {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: &'static str,
    pub unit: &'static str,
    pub current: usize,
    pub high_water: usize,
    pub soft_limit: Option<usize>,
    pub severity: ResourceSeverity,
    pub would_throttle_total: usize,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ResourcePressureSignal {
    pub resource_id: &'static str,
    pub label: &'static str,
    pub severity: ResourceSeverity,
    pub current: usize,
    pub soft_limit: usize,
    pub unit: &'static str,
    pub advice: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalResourceCounters {
    pub agent_loops_started_total: usize,
    pub agent_loop_would_throttle_total: usize,
    #[serde(default)]
    pub agent_loop_rejected_total: usize,
    #[serde(default)]
    pub agent_loop_rss_blocked_total: usize,
    #[serde(default)]
    pub agent_loop_per_agent_rejected_total: usize,
    pub agent_triggers_queued_total: usize,
    pub agent_trigger_duplicates_total: usize,
    pub agent_trigger_queue_full_total: usize,
    pub agent_trigger_would_throttle_total: usize,
    pub runtime_event_backlog_would_throttle_total: usize,
    pub event_append_lock_pressure_observations: usize,
    pub llm_direct_route_total: usize,
    pub llm_direct_stream_route_total: usize,
    #[serde(default)]
    pub lance_timeouts: usize,
    #[serde(default)]
    pub lance_cancels: usize,
    #[serde(default)]
    pub journal_lock_acquisitions: usize,
    #[serde(default)]
    pub read_snapshot_hits: usize,
    #[serde(default)]
    pub read_snapshot_hydrates: usize,
    #[serde(default)]
    pub result_cache_hits: usize,
    #[serde(default)]
    pub result_cache_misses: usize,
    #[serde(default)]
    pub result_cache_stores: usize,
    #[serde(default)]
    pub result_cache_evictions: usize,
    #[serde(default)]
    pub result_cache_entries: usize,
    #[serde(default)]
    pub result_cache_bytes: usize,
    #[serde(default)]
    pub lance_table_pool_hits: usize,
    #[serde(default)]
    pub lance_table_pool_misses: usize,
    #[serde(default)]
    pub lance_table_pool_idle: usize,
    #[serde(default)]
    pub embedding_oldest_write_wait_ms: usize,
    #[serde(default)]
    pub query_vector_cache_hits: usize,
    #[serde(default)]
    pub query_vector_cache_misses: usize,
    #[serde(default)]
    pub query_vector_cache_entries: usize,
    #[serde(default)]
    pub query_vector_cache_bytes: usize,
    #[serde(default)]
    pub query_embed_batch_physical_calls: usize,
    #[serde(default)]
    pub query_embed_batch_logical_queries: usize,
    #[serde(default)]
    pub query_embed_batch_max_fill: usize,
    #[serde(default)]
    pub query_embed_batch_waiters: usize,
    #[serde(default)]
    pub ann_queries: usize,
    #[serde(default)]
    pub ann_fallbacks: usize,
    #[serde(default)]
    pub ann_shadow_compares: usize,
    #[serde(default)]
    pub ann_shadow_mismatches: usize,
    #[serde(default)]
    pub ann_shadow_recall_milles: usize,
    #[serde(default)]
    pub ivf_present: usize,
    #[serde(default)]
    pub ivf_disk_bytes: usize,
    #[serde(default)]
    pub ivf_generation: usize,
    #[serde(default)]
    pub blocking_admission_admitted_total: usize,
    #[serde(default)]
    pub blocking_admission_would_throttle_total: usize,
    #[serde(default)]
    pub blocking_admission_wait_high_water_ms: usize,
}

/// Injected retrieval HOL gauges. magician-core stays free of vector-index.
#[derive(Debug, Clone, Copy)]
pub struct RetrievalHolGauges {
    pub journal_lock_waiters: usize,
    pub journal_lock_max_wait_ms: usize,
    pub embedding_waiting_foreground: usize,
    pub embedding_active_foreground: usize,
    pub embedding_waiting_writes: usize,
    pub lance_in_flight: usize,
    pub lance_waiters: usize,
    pub lance_timeouts: usize,
    pub lance_cancels: usize,
    pub read_snapshot_hits: usize,
    pub read_snapshot_hydrates: usize,
    pub journal_lock_acquisitions: usize,
    pub result_cache_hits: usize,
    pub result_cache_misses: usize,
    pub result_cache_stores: usize,
    pub result_cache_evictions: usize,
    pub result_cache_entries: usize,
    pub result_cache_bytes: usize,
    pub result_cache_waiters: usize,
    pub lance_table_pool_hits: usize,
    pub lance_table_pool_misses: usize,
    pub lance_table_pool_idle: usize,
    pub embedding_oldest_write_wait_ms: usize,
    pub query_vector_cache_hits: usize,
    pub query_vector_cache_misses: usize,
    pub query_vector_cache_entries: usize,
    pub query_vector_cache_bytes: usize,
    pub query_embed_batch_physical_calls: usize,
    pub query_embed_batch_logical_queries: usize,
    pub query_embed_batch_max_fill: usize,
    pub query_embed_batch_waiters: usize,
    pub vector_search_mode: &'static str,
    pub ivf_present: usize,
    pub ivf_disk_bytes: usize,
    pub ivf_generation: usize,
    pub ann_queries: usize,
    pub ann_fallbacks: usize,
    pub ann_shadow_compares: usize,
    pub ann_shadow_mismatches: usize,
    pub ann_shadow_recall_milles: usize,
}

impl Default for RetrievalHolGauges {
    fn default() -> Self {
        Self {
            journal_lock_waiters: 0,
            journal_lock_max_wait_ms: 0,
            embedding_waiting_foreground: 0,
            embedding_active_foreground: 0,
            embedding_waiting_writes: 0,
            lance_in_flight: 0,
            lance_waiters: 0,
            lance_timeouts: 0,
            lance_cancels: 0,
            read_snapshot_hits: 0,
            read_snapshot_hydrates: 0,
            journal_lock_acquisitions: 0,
            result_cache_hits: 0,
            result_cache_misses: 0,
            result_cache_stores: 0,
            result_cache_evictions: 0,
            result_cache_entries: 0,
            result_cache_bytes: 0,
            result_cache_waiters: 0,
            lance_table_pool_hits: 0,
            lance_table_pool_misses: 0,
            lance_table_pool_idle: 0,
            embedding_oldest_write_wait_ms: 0,
            query_vector_cache_hits: 0,
            query_vector_cache_misses: 0,
            query_vector_cache_entries: 0,
            query_vector_cache_bytes: 0,
            query_embed_batch_physical_calls: 0,
            query_embed_batch_logical_queries: 0,
            query_embed_batch_max_fill: 0,
            query_embed_batch_waiters: 0,
            vector_search_mode: "flat",
            ivf_present: 0,
            ivf_disk_bytes: 0,
            ivf_generation: 0,
            ann_queries: 0,
            ann_fallbacks: 0,
            ann_shadow_compares: 0,
            ann_shadow_mismatches: 0,
            ann_shadow_recall_milles: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservedAgentLoop {
    pub started_at_ms: u64,
    pub active_after_start: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceSeverity {
    Ok,
    Watch,
    Pressure,
}

/// Why [`admit_agent_loop`] refused a new cycle. The live executor must fail
/// the cycle closed without constructing the agentic execute future.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentAdmissionRejected {
    LiveAgentLimit {
        active: usize,
        limit: usize,
    },
    PerAgentLimit {
        agent_id: String,
        outstanding: usize,
        limit: usize,
    },
    RssTripwire {
        rss_bytes: u64,
        high_bytes: u64,
    },
}

impl fmt::Display for AgentAdmissionRejected {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LiveAgentLimit { active, limit } => write!(
                f,
                "agent admission rejected: live-agent cap {limit} reached ({active} active)"
            ),
            Self::PerAgentLimit {
                agent_id,
                outstanding,
                limit,
            } => write!(
                f,
                "agent admission rejected: agent {agent_id} has {outstanding} outstanding loops (cap {limit})"
            ),
            Self::RssTripwire {
                rss_bytes,
                high_bytes,
            } => write!(
                f,
                "agent admission rejected: process RSS {rss_bytes} bytes exceeds tripwire {high_bytes}"
            ),
        }
    }
}

impl std::error::Error for AgentAdmissionRejected {}

enum RssDecision {
    Skip,
    Allow,
    Block { rss_bytes: u64, high_bytes: u64 },
}

fn rss_decision() -> RssDecision {
    let Some(probe) = RSS_PROBE.get().copied() else {
        return RssDecision::Skip;
    };
    let high_bytes = rss_high_bytes();
    if high_bytes == 0 {
        return RssDecision::Skip;
    }
    let rss_bytes = probe();
    let low_bytes = rss_low_bytes();
    if rss_bytes >= high_bytes {
        RSS_TRIPPED.store(true, Ordering::SeqCst);
        return RssDecision::Block {
            rss_bytes,
            high_bytes,
        };
    }
    if RSS_TRIPPED.load(Ordering::SeqCst) {
        if rss_bytes <= low_bytes {
            RSS_TRIPPED.store(false, Ordering::SeqCst);
            return RssDecision::Allow;
        }
        return RssDecision::Block {
            rss_bytes,
            high_bytes,
        };
    }
    RSS_TRIPPED.store(false, Ordering::SeqCst);
    RssDecision::Allow
}

fn try_increment_global(cap: usize) -> Result<usize, usize> {
    loop {
        let current = ACTIVE_AGENT_LOOPS.load(Ordering::SeqCst);
        if cap > 0 && current >= cap {
            return Err(current);
        }
        match ACTIVE_AGENT_LOOPS.compare_exchange_weak(
            current,
            current + 1,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => return Ok(current + 1),
            Err(_) => continue,
        }
    }
}

fn try_increment_agent(agent_id: &str, cap: usize) -> Result<usize, usize> {
    let mut entry = agent_outstanding_counts()
        .entry(agent_id.to_string())
        .or_insert(0);
    if *entry >= cap {
        return Err(*entry);
    }
    *entry += 1;
    Ok(*entry)
}

fn decrement_agent_outstanding(agent_id: &str) {
    use dashmap::mapref::entry::Entry;
    match agent_outstanding_counts().entry(agent_id.to_string()) {
        Entry::Occupied(mut occupied) => {
            let next = occupied.get().saturating_sub(1);
            if next == 0 {
                occupied.remove();
            } else {
                *occupied.get_mut() = next;
            }
        },
        Entry::Vacant(_) => {},
    }
}

fn record_latest_agent_loop(
    agent_id: Option<&str>,
    task_id: Option<&str>,
    execution_id: Option<&str>,
    active: usize,
) {
    *latest_agent_loop()
        .lock()
        .expect("local resource governor latest agent loop lock poisoned") =
        Some(ObservedAgentLoop {
            started_at_ms: now_ms(),
            active_after_start: active,
            agent_id: agent_id.map(str::to_string),
            task_id: task_id.map(str::to_string),
            execution_id: execution_id.map(str::to_string),
        });
}

/// Guard for an admitted (or metrics-only observed) agent loop. Dropping it
/// releases the process-wide slot and any per-agent outstanding count.
#[must_use = "dropping the guard releases the admitted agent-loop slot"]
pub struct AgentLoopObservationGuard {
    agent_id: Option<String>,
    holds_slot: bool,
}

impl Drop for AgentLoopObservationGuard {
    fn drop(&mut self) {
        if !self.holds_slot {
            return;
        }
        decrement_saturating(&ACTIVE_AGENT_LOOPS, 1);
        if let Some(agent_id) = &self.agent_id {
            decrement_agent_outstanding(agent_id);
        }
    }
}

/// Admit one live agent loop. Rejects without incrementing active counts when
/// the hard cap, per-agent outstanding cap, or RSS tripwire is hit.
///
/// A hard cap of `0` is observe-only rollback: the loop is always admitted.
/// The returned guard decrements on drop; dropping it is what returns the
/// slot. Callers that receive `Err` must not construct the execute future.
pub fn admit_agent_loop(
    agent_id: Option<&str>,
    task_id: Option<&str>,
    execution_id: Option<&str>,
) -> Result<AgentLoopObservationGuard, AgentAdmissionRejected> {
    let agent_id = agent_id.filter(|id| !id.is_empty());
    let observe_only = live_agent_limit() == 0;
    let mut tracked_agent = None;
    if !observe_only {
        match rss_decision() {
            RssDecision::Skip | RssDecision::Allow => {},
            RssDecision::Block {
                rss_bytes,
                high_bytes,
            } => {
                AGENT_LOOP_RSS_BLOCKED_TOTAL.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    rss_bytes,
                    high_bytes,
                    "rejecting agent loop: process RSS tripwire"
                );
                return Err(AgentAdmissionRejected::RssTripwire {
                    rss_bytes,
                    high_bytes,
                });
            },
        }

        let per_agent_cap = per_agent_outstanding_limit();
        tracked_agent = agent_id.filter(|_| per_agent_cap > 0);
        if let Some(id) = tracked_agent {
            if let Err(outstanding) = try_increment_agent(id, per_agent_cap) {
                AGENT_LOOP_PER_AGENT_REJECTED_TOTAL.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(
                    agent_id = id,
                    outstanding,
                    limit = per_agent_cap,
                    "rejecting agent loop: per-agent outstanding cap"
                );
                return Err(AgentAdmissionRejected::PerAgentLimit {
                    agent_id: id.to_string(),
                    outstanding,
                    limit: per_agent_cap,
                });
            }
        }
    }

    let cap = live_agent_limit();
    match try_increment_global(cap) {
        Ok(active) => {
            AGENT_LOOPS_STARTED_TOTAL.fetch_add(1, Ordering::Relaxed);
            update_atomic_max(&ACTIVE_AGENT_LOOPS_HIGH_WATER, active);
            if active > ACTIVE_AGENT_LOOP_SOFT_LIMIT {
                AGENT_LOOP_WOULD_THROTTLE_TOTAL.fetch_add(1, Ordering::Relaxed);
            }
            record_latest_agent_loop(agent_id, task_id, execution_id, active);
            Ok(AgentLoopObservationGuard {
                agent_id: tracked_agent.map(str::to_string),
                holds_slot: true,
            })
        },
        Err(active) => {
            if let Some(id) = tracked_agent {
                decrement_agent_outstanding(id);
            }
            AGENT_LOOP_REJECTED_TOTAL.fetch_add(1, Ordering::Relaxed);
            AGENT_LOOP_WOULD_THROTTLE_TOTAL.fetch_add(1, Ordering::Relaxed);
            tracing::warn!(active, limit = cap, "rejecting agent loop: live-agent cap");
            Err(AgentAdmissionRejected::LiveAgentLimit { active, limit: cap })
        },
    }
}

/// Observe-only wrapper around [`admit_agent_loop`].
///
/// On reject this does **not** take a slot (no leak). Prefer
/// [`admit_agent_loop`] on the live executor path, which fails the cycle
/// closed.
pub fn observe_agent_loop(
    agent_id: Option<&str>,
    task_id: Option<&str>,
    execution_id: Option<&str>,
) -> AgentLoopObservationGuard {
    admit_agent_loop(agent_id, task_id, execution_id).unwrap_or_else(|_| {
        AgentLoopObservationGuard {
            agent_id: None,
            holds_slot: false,
        }
    })
}

pub fn record_agent_trigger_queued(_queue_position: usize) {
    AGENT_TRIGGERS_QUEUED_TOTAL.fetch_add(1, Ordering::Relaxed);
    let pending = PENDING_AGENT_TRIGGERS.fetch_add(1, Ordering::Relaxed) + 1;
    update_atomic_max(&PENDING_AGENT_TRIGGERS_HIGH_WATER, pending);
    if pending > PENDING_AGENT_TRIGGER_SOFT_LIMIT {
        AGENT_TRIGGER_WOULD_THROTTLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_agent_trigger_dequeued() {
    decrement_saturating(&PENDING_AGENT_TRIGGERS, 1);
}

pub fn record_agent_trigger_dequeued_many(count: usize) {
    decrement_saturating(&PENDING_AGENT_TRIGGERS, count);
}

pub fn record_agent_trigger_duplicate() {
    AGENT_TRIGGER_DUPLICATES_TOTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn record_agent_trigger_queue_full() {
    AGENT_TRIGGER_QUEUE_FULL_TOTAL.fetch_add(1, Ordering::Relaxed);
}

pub fn record_runtime_event_backlog(backlog: usize) {
    RUNTIME_EVENT_BACKLOG.store(backlog, Ordering::Relaxed);
    update_atomic_max(&RUNTIME_EVENT_BACKLOG_HIGH_WATER, backlog);
    if backlog > RUNTIME_EVENT_BACKLOG_SOFT_LIMIT {
        RUNTIME_EVENT_BACKLOG_WOULD_THROTTLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_event_append_lock_wait(wait_ms: usize) {
    update_atomic_max(&EVENT_APPEND_LOCK_MAX_WAIT_MS, wait_ms);
    if wait_ms > EVENT_APPEND_LOCK_WAIT_SOFT_LIMIT_MS {
        EVENT_APPEND_LOCK_PRESSURE_OBSERVATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn record_llm_direct_route(streaming: bool) -> usize {
    if streaming {
        LLM_DIRECT_STREAM_ROUTE_TOTAL.fetch_add(1, Ordering::Relaxed) + 1
    } else {
        LLM_DIRECT_ROUTE_TOTAL.fetch_add(1, Ordering::Relaxed) + 1
    }
}

/// `memory_overlay_max_wait_ms` is injected by the caller (the API layer
/// reads it from `magician_vector_index::memory_temperature`) so this module
/// stays free of the vector-index dependency tree.
pub fn snapshot(memory_overlay_max_wait_ms: usize) -> LocalResourceGovernorSnapshot {
    snapshot_with_retrieval_hol(memory_overlay_max_wait_ms, RetrievalHolGauges::default())
}

pub fn snapshot_with_retrieval_hol(
    memory_overlay_max_wait_ms: usize,
    hol: RetrievalHolGauges,
) -> LocalResourceGovernorSnapshot {
    let blocking = crate::blocking_admission::blocking_admission_snapshot();
    let hard_cap = live_agent_limit();
    let agent_loop_limit = if hard_cap > 0 {
        hard_cap
    } else {
        ACTIVE_AGENT_LOOP_SOFT_LIMIT
    };
    let resources = vec![
        gauge(
            "agent_loops",
            "Agent loops",
            "concurrency",
            "loops",
            ACTIVE_AGENT_LOOPS.load(Ordering::Relaxed),
            ACTIVE_AGENT_LOOPS_HIGH_WATER.load(Ordering::Relaxed),
            Some(agent_loop_limit),
            AGENT_LOOP_WOULD_THROTTLE_TOTAL.load(Ordering::Relaxed),
            "Active agent execution loops in this process.",
        ),
        gauge(
            "pending_agent_triggers",
            "Pending goal triggers",
            "queue",
            "triggers",
            PENDING_AGENT_TRIGGERS.load(Ordering::Relaxed),
            PENDING_AGENT_TRIGGERS_HIGH_WATER.load(Ordering::Relaxed),
            Some(PENDING_AGENT_TRIGGER_SOFT_LIMIT),
            AGENT_TRIGGER_WOULD_THROTTLE_TOTAL.load(Ordering::Relaxed),
            "Goal triggers waiting behind active cycles.",
        ),
        gauge(
            "runtime_event_backlog",
            "Runtime event backlog",
            "queue",
            "events",
            RUNTIME_EVENT_BACKLOG.load(Ordering::Relaxed),
            RUNTIME_EVENT_BACKLOG_HIGH_WATER.load(Ordering::Relaxed),
            Some(RUNTIME_EVENT_BACKLOG_SOFT_LIMIT),
            RUNTIME_EVENT_BACKLOG_WOULD_THROTTLE_TOTAL.load(Ordering::Relaxed),
            "Canonical runtime events queued for filesystem append.",
        ),
        gauge(
            "event_append_lock_wait",
            "Event append lock wait",
            "lock",
            "ms",
            EVENT_APPEND_LOCK_MAX_WAIT_MS.load(Ordering::Relaxed),
            EVENT_APPEND_LOCK_MAX_WAIT_MS.load(Ordering::Relaxed),
            Some(EVENT_APPEND_LOCK_WAIT_SOFT_LIMIT_MS),
            EVENT_APPEND_LOCK_PRESSURE_OBSERVATIONS.load(Ordering::Relaxed),
            "High-water wait for the canonical event append lock.",
        ),
        gauge(
            "memory_overlay_lock_wait",
            "Memory overlay lock wait",
            "lock",
            "ms",
            memory_overlay_max_wait_ms,
            memory_overlay_max_wait_ms,
            Some(MEMORY_OVERLAY_LOCK_WAIT_SOFT_LIMIT_MS),
            0,
            "High-water wait for memory temperature overlay writes.",
        ),
        gauge(
            "journal_lock_waiters",
            "Memory journal lock waiters",
            "queue",
            "waiters",
            hol.journal_lock_waiters,
            hol.journal_lock_waiters,
            Some(JOURNAL_LOCK_WAITER_SOFT_LIMIT),
            0,
            "Callers waiting on the process-wide memory index change journal lock.",
        ),
        gauge(
            "journal_lock_wait",
            "Memory journal lock wait",
            "lock",
            "ms",
            hol.journal_lock_max_wait_ms,
            hol.journal_lock_max_wait_ms,
            Some(JOURNAL_LOCK_WAIT_SOFT_LIMIT_MS),
            0,
            "High-water wait for the memory index change journal lock.",
        ),
        gauge(
            "embedding_waiting_foreground",
            "Foreground embedding waiters",
            "queue",
            "waiters",
            hol.embedding_waiting_foreground,
            hol.embedding_waiting_foreground,
            Some(EMBEDDING_WAITING_FOREGROUND_SOFT_LIMIT),
            0,
            "Foreground retrievals waiting for the single-sequence embedding daemon.",
        ),
        gauge(
            "lance_in_flight",
            "Lance searches in flight",
            "concurrency",
            "searches",
            hol.lance_in_flight,
            hol.lance_in_flight,
            Some(LANCE_IN_FLIGHT_SOFT_LIMIT),
            0,
            "Request-path Lance searches occupying a search permit.",
        ),
        gauge(
            "lance_waiters",
            "Lance search waiters",
            "queue",
            "waiters",
            hol.lance_waiters,
            hol.lance_waiters,
            Some(LANCE_WAITER_SOFT_LIMIT),
            0,
            "Request-path Lance searches waiting for a search permit.",
        ),
        gauge(
            "result_cache_entries",
            "Hybrid result cache entries",
            "cache",
            "entries",
            hol.result_cache_entries,
            hol.result_cache_entries,
            None,
            0,
            "Revision-bound hybrid score maps retained in the process-wide result cache.",
        ),
        gauge(
            "result_cache_waiters",
            "Hybrid result cache waiters",
            "queue",
            "waiters",
            hol.result_cache_waiters,
            hol.result_cache_waiters,
            None,
            0,
            "Retrievals waiting on an in-flight identical hybrid score computation.",
        ),
        gauge(
            "lance_table_pool_idle",
            "Lance table pool idle",
            "cache",
            "handles",
            hol.lance_table_pool_idle,
            hol.lance_table_pool_idle,
            None,
            0,
            "Idle generation-keyed Lance search tables retained for reuse.",
        ),
        gauge(
            "embedding_oldest_write_wait",
            "Oldest embedding write wait",
            "lock",
            "ms",
            hol.embedding_oldest_write_wait_ms,
            hol.embedding_oldest_write_wait_ms,
            None,
            0,
            "Age of the oldest durable embedding write waiting behind foreground retrieval.",
        ),
        gauge(
            "query_vector_cache_entries",
            "Query vector cache entries",
            "cache",
            "entries",
            hol.query_vector_cache_entries,
            hol.query_vector_cache_entries,
            None,
            0,
            "Exact-query embedding vectors retained in the process-wide LRU.",
        ),
        gauge(
            "query_embed_batch_waiters",
            "Query embedding batch waiters",
            "queue",
            "waiters",
            hol.query_embed_batch_waiters,
            hol.query_embed_batch_waiters,
            None,
            0,
            "Distinct query embeddings waiting to share a physical embed call.",
        ),
        gauge(
            "ivf_present",
            "Memory IVF_PQ present",
            "index",
            "flag",
            hol.ivf_present,
            hol.ivf_present,
            None,
            0,
            "1 when the memory embedding column has an IVF_PQ index.",
        ),
        gauge(
            "ann_shadow_recall_milles",
            "ANN shadow recall milles",
            "quality",
            "milles",
            hol.ann_shadow_recall_milles,
            hol.ann_shadow_recall_milles,
            None,
            0,
            "Last ANN-shadow recall@k in milles (1000 = full top-k overlap with flat).",
        ),
        gauge(
            "blocking_admission",
            "Blocking admission",
            "concurrency",
            "tasks",
            blocking.in_flight,
            blocking.high_water,
            if blocking.pass_through || blocking.permits == 0 {
                None
            } else {
                Some(blocking.permits)
            },
            blocking.would_throttle_total,
            "Magician-owned spawn_blocking jobs occupying an admission permit.",
        ),
    ];
    let pressure = resources
        .iter()
        .filter_map(pressure_signal_for)
        .collect::<Vec<_>>();
    let pressure_count = pressure
        .iter()
        .filter(|signal| signal.severity == ResourceSeverity::Pressure)
        .count();
    let watch_count = pressure
        .iter()
        .filter(|signal| signal.severity == ResourceSeverity::Watch)
        .count();
    let status = if pressure_count > 0 {
        ResourceSeverity::Pressure
    } else if watch_count > 0 {
        ResourceSeverity::Watch
    } else {
        ResourceSeverity::Ok
    };
    let counters = LocalResourceCounters {
        agent_loops_started_total: AGENT_LOOPS_STARTED_TOTAL.load(Ordering::Relaxed),
        agent_loop_would_throttle_total: AGENT_LOOP_WOULD_THROTTLE_TOTAL.load(Ordering::Relaxed),
        agent_loop_rejected_total: AGENT_LOOP_REJECTED_TOTAL.load(Ordering::Relaxed),
        agent_loop_rss_blocked_total: AGENT_LOOP_RSS_BLOCKED_TOTAL.load(Ordering::Relaxed),
        agent_loop_per_agent_rejected_total: AGENT_LOOP_PER_AGENT_REJECTED_TOTAL
            .load(Ordering::Relaxed),
        agent_triggers_queued_total: AGENT_TRIGGERS_QUEUED_TOTAL.load(Ordering::Relaxed),
        agent_trigger_duplicates_total: AGENT_TRIGGER_DUPLICATES_TOTAL.load(Ordering::Relaxed),
        agent_trigger_queue_full_total: AGENT_TRIGGER_QUEUE_FULL_TOTAL.load(Ordering::Relaxed),
        agent_trigger_would_throttle_total: AGENT_TRIGGER_WOULD_THROTTLE_TOTAL
            .load(Ordering::Relaxed),
        runtime_event_backlog_would_throttle_total: RUNTIME_EVENT_BACKLOG_WOULD_THROTTLE_TOTAL
            .load(Ordering::Relaxed),
        event_append_lock_pressure_observations: EVENT_APPEND_LOCK_PRESSURE_OBSERVATIONS
            .load(Ordering::Relaxed),
        llm_direct_route_total: LLM_DIRECT_ROUTE_TOTAL.load(Ordering::Relaxed),
        llm_direct_stream_route_total: LLM_DIRECT_STREAM_ROUTE_TOTAL.load(Ordering::Relaxed),
        lance_timeouts: hol.lance_timeouts,
        lance_cancels: hol.lance_cancels,
        journal_lock_acquisitions: hol.journal_lock_acquisitions,
        read_snapshot_hits: hol.read_snapshot_hits,
        read_snapshot_hydrates: hol.read_snapshot_hydrates,
        result_cache_hits: hol.result_cache_hits,
        result_cache_misses: hol.result_cache_misses,
        result_cache_stores: hol.result_cache_stores,
        result_cache_evictions: hol.result_cache_evictions,
        result_cache_entries: hol.result_cache_entries,
        result_cache_bytes: hol.result_cache_bytes,
        lance_table_pool_hits: hol.lance_table_pool_hits,
        lance_table_pool_misses: hol.lance_table_pool_misses,
        lance_table_pool_idle: hol.lance_table_pool_idle,
        embedding_oldest_write_wait_ms: hol.embedding_oldest_write_wait_ms,
        query_vector_cache_hits: hol.query_vector_cache_hits,
        query_vector_cache_misses: hol.query_vector_cache_misses,
        query_vector_cache_entries: hol.query_vector_cache_entries,
        query_vector_cache_bytes: hol.query_vector_cache_bytes,
        query_embed_batch_physical_calls: hol.query_embed_batch_physical_calls,
        query_embed_batch_logical_queries: hol.query_embed_batch_logical_queries,
        query_embed_batch_max_fill: hol.query_embed_batch_max_fill,
        query_embed_batch_waiters: hol.query_embed_batch_waiters,
        ann_queries: hol.ann_queries,
        ann_fallbacks: hol.ann_fallbacks,
        ann_shadow_compares: hol.ann_shadow_compares,
        ann_shadow_mismatches: hol.ann_shadow_mismatches,
        ann_shadow_recall_milles: hol.ann_shadow_recall_milles,
        ivf_present: hol.ivf_present,
        ivf_disk_bytes: hol.ivf_disk_bytes,
        ivf_generation: hol.ivf_generation,
        blocking_admission_admitted_total: blocking.admitted_total,
        blocking_admission_would_throttle_total: blocking.would_throttle_total,
        blocking_admission_wait_high_water_ms: blocking.wait_high_water_ms,
    };
    let would_throttle_total = counters.agent_loop_would_throttle_total
        + counters.agent_trigger_would_throttle_total
        + counters.runtime_event_backlog_would_throttle_total
        + counters.event_append_lock_pressure_observations
        + counters.blocking_admission_would_throttle_total;
    let observe_only = hard_cap == 0;
    LocalResourceGovernorSnapshot {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        captured_at_ms: now_ms(),
        mode: if observe_only {
            "observe_only"
        } else {
            "hard_admission"
        },
        observe_only,
        vector_search_mode: if hol.vector_search_mode.is_empty() {
            "flat"
        } else {
            hol.vector_search_mode
        },
        admitted: ACTIVE_AGENT_LOOPS.load(Ordering::Relaxed),
        rejected: counters.agent_loop_rejected_total,
        rss_blocked: counters.agent_loop_rss_blocked_total,
        per_agent_rejects: counters.agent_loop_per_agent_rejected_total,
        blocking_admission_waiting: blocking.waiting,
        blocking_admission_in_flight: blocking.in_flight,
        blocking_admission_high_water: blocking.high_water,
        blocking_admission_wait_high_water_ms: blocking.wait_high_water_ms,
        blocking_admission_admitted_total: blocking.admitted_total,
        status,
        summary: LocalResourceSummary {
            resource_count: resources.len(),
            pressure_count,
            watch_count,
            would_throttle_total,
        },
        resources,
        pressure,
        counters,
        latest_agent_loop: latest_agent_loop()
            .lock()
            .expect("local resource governor latest agent loop lock poisoned")
            .clone(),
    }
}

fn gauge(
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    unit: &'static str,
    current: usize,
    high_water: usize,
    soft_limit: Option<usize>,
    would_throttle_total: usize,
    description: &'static str,
) -> ResourceGauge {
    ResourceGauge {
        id,
        label,
        kind,
        unit,
        current,
        high_water,
        soft_limit,
        severity: soft_limit
            .map(|limit| severity_for(current, limit))
            .unwrap_or(ResourceSeverity::Ok),
        would_throttle_total,
        description,
    }
}

fn pressure_signal_for(gauge: &ResourceGauge) -> Option<ResourcePressureSignal> {
    let limit = gauge.soft_limit?;
    if gauge.severity == ResourceSeverity::Ok {
        return None;
    }
    Some(ResourcePressureSignal {
        resource_id: gauge.id,
        label: gauge.label,
        severity: gauge.severity,
        current: gauge.current,
        soft_limit: limit,
        unit: gauge.unit,
        advice: advice_for(gauge.id),
    })
}

fn severity_for(current: usize, limit: usize) -> ResourceSeverity {
    if limit == 0 {
        return ResourceSeverity::Ok;
    }
    if current >= limit {
        return ResourceSeverity::Pressure;
    }
    if current.saturating_mul(100) >= limit.saturating_mul(80) {
        return ResourceSeverity::Watch;
    }
    ResourceSeverity::Ok
}

fn advice_for(resource_id: &str) -> &'static str {
    match resource_id {
        "agent_loops" => "Reduce active agent fan-out or move work to a paced queue.",
        "pending_agent_triggers" => {
            "Inspect goal schedules; repeated triggers are accumulating behind active cycles."
        },
        "runtime_event_backlog" => "Check event writer throughput and filesystem latency.",
        "event_append_lock_wait" => {
            "Look for high-frequency canonical event writes from many concurrent executions."
        },
        "memory_overlay_lock_wait" => {
            "Run memory maintenance away from peak agent fan-out or shard overlay writes."
        },
        "journal_lock_waiters" | "journal_lock_wait" => {
            "Keep request-path retrieval on the in-memory journal snapshot; inspect writers if waiters stay high."
        },
        "embedding_waiting_foreground" => {
            "Reuse query embeddings or reduce concurrent hybrid retrievals; do not raise embedding_num_parallel yet."
        },
        "lance_in_flight" | "lance_waiters" => {
            "Confirm Lance searches run on magician-lance, not HTTP/execution workers."
        },
        "blocking_admission" => {
            "Pace Magician-owned spawn_blocking work; do not raise Tokio max_blocking_threads."
        },
        _ => "Inspect local runtime pressure before enabling enforcement.",
    }
}

fn latest_agent_loop() -> &'static Mutex<Option<ObservedAgentLoop>> {
    static LATEST: OnceLock<Mutex<Option<ObservedAgentLoop>>> = OnceLock::new();
    LATEST.get_or_init(|| Mutex::new(None))
}

fn decrement_saturating(slot: &AtomicUsize, amount: usize) {
    let mut current = slot.load(Ordering::Relaxed);
    loop {
        let next = current.saturating_sub(amount);
        match slot.compare_exchange_weak(current, next, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn update_atomic_max(slot: &AtomicUsize, candidate: usize) {
    let mut current = slot.load(Ordering::Relaxed);
    while candidate > current {
        match slot.compare_exchange_weak(current, candidate, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(observed) => current = observed,
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        acquire_delegated_child, acquire_delegated_child_when_available, delegated_child_semaphore,
        severity_for, try_acquire_harness_root, ResourceSeverity, DELEGATED_CHILD_PERMITS_BY_LEVEL,
        DELEGATED_CHILD_PERMIT_LIMIT, HARNESS_ROOT_PERMIT_LIMIT, HARNESS_ROOT_SEMAPHORE,
        INTERNAL_SYSTEM_ANALYST_AGENT_ID, INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT,
        INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE,
    };

    static PERMIT_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Level 0 — the pool every child of a root execution draws from.
    const ROOT_LEVEL: usize = 0;

    #[test]
    fn snapshot_includes_injected_retrieval_hol_gauges() {
        let snapshot = super::snapshot_with_retrieval_hol(
            0,
            super::RetrievalHolGauges {
                journal_lock_waiters: 2,
                journal_lock_max_wait_ms: 40,
                embedding_waiting_foreground: 3,
                lance_in_flight: 1,
                lance_waiters: 5,
                lance_timeouts: 9,
                ..super::RetrievalHolGauges::default()
            },
        );
        let ids: Vec<_> = snapshot.resources.iter().map(|gauge| gauge.id).collect();
        assert!(ids.contains(&"journal_lock_waiters"));
        assert!(ids.contains(&"lance_in_flight"));
        assert!(ids.contains(&"embedding_waiting_foreground"));
        assert_eq!(snapshot.counters.lance_timeouts, 9);
        assert!(ids.contains(&"result_cache_entries"));
        assert_eq!(snapshot.schema_version, super::SNAPSHOT_SCHEMA_VERSION);
        assert_eq!(super::SNAPSHOT_SCHEMA_VERSION, 8);
        assert_eq!(snapshot.vector_search_mode, "flat");
        assert!(ids.contains(&"lance_table_pool_idle"));
        assert!(ids.contains(&"query_vector_cache_entries"));
        assert!(ids.contains(&"embedding_oldest_write_wait"));
        assert!(ids.contains(&"ivf_present"));
        assert!(ids.contains(&"ann_shadow_recall_milles"));
        assert!(ids.contains(&"blocking_admission"));
        let lance_waiters = snapshot
            .resources
            .iter()
            .find(|gauge| gauge.id == "lance_waiters")
            .expect("lance waiters gauge");
        assert_eq!(lance_waiters.current, 5);
    }

    #[test]
    fn severity_uses_watch_band_before_pressure() {
        assert_eq!(severity_for(0, 50), ResourceSeverity::Ok);
        assert_eq!(severity_for(39, 50), ResourceSeverity::Ok);
        assert_eq!(severity_for(40, 50), ResourceSeverity::Watch);
        assert_eq!(severity_for(50, 50), ResourceSeverity::Pressure);
    }

    #[tokio::test]
    async fn harness_root_permits_enforce_limit_and_release_on_drop() {
        let _test_lock = PERMIT_TEST_LOCK.lock().await;
        assert_eq!(
            HARNESS_ROOT_SEMAPHORE.available_permits(),
            HARNESS_ROOT_PERMIT_LIMIT
        );

        // Drain the full pool, then assert one more is refused.
        let mut held: Vec<_> = (0..HARNESS_ROOT_PERMIT_LIMIT)
            .map(|i| {
                try_acquire_harness_root()
                    .unwrap_or_else(|| panic!("harness root permit {i} within the limit"))
            })
            .collect();
        assert!(try_acquire_harness_root().is_none());

        // Releasing one frees exactly one slot.
        let first = held.pop().expect("at least one permit held");
        drop(first);
        let replacement = try_acquire_harness_root().expect("released harness root permit");
        drop(held);
        drop(replacement);
        assert_eq!(
            HARNESS_ROOT_SEMAPHORE.available_permits(),
            HARNESS_ROOT_PERMIT_LIMIT
        );
    }

    #[tokio::test]
    async fn delegated_child_permits_enforce_global_and_target_limits() {
        let _test_lock = PERMIT_TEST_LOCK.lock().await;
        let mut global_permits = Vec::new();
        for index in 0..DELEGATED_CHILD_PERMIT_LIMIT {
            global_permits.push(
                acquire_delegated_child(
                    &format!("general-agent-{index}"),
                    ROOT_LEVEL,
                    Duration::from_secs(1),
                )
                .await
                .expect("delegated child permit within the level-0 limit"),
            );
        }
        assert_eq!(delegated_child_semaphore(ROOT_LEVEL).available_permits(), 0);

        let global_timeout =
            acquire_delegated_child("global-overflow", ROOT_LEVEL, Duration::from_millis(10))
                .await
                .err()
                .expect("level-0 delegated-child limit should time out");
        assert!(global_timeout.contains("timed out"));

        drop(global_permits.pop());
        let replacement =
            acquire_delegated_child("global-replacement", ROOT_LEVEL, Duration::from_secs(1))
                .await
                .expect("released level-0 delegated-child permit");
        drop((global_permits, replacement));

        let analyst = acquire_delegated_child(
            INTERNAL_SYSTEM_ANALYST_AGENT_ID,
            ROOT_LEVEL,
            Duration::from_secs(1),
        )
        .await
        .expect("first internal-system-analyst permit");
        let target_timeout = acquire_delegated_child(
            INTERNAL_SYSTEM_ANALYST_AGENT_ID,
            ROOT_LEVEL,
            Duration::from_millis(10),
        )
        .await
        .err()
        .expect("internal-system-analyst target limit should time out");
        assert!(target_timeout.contains("timed out"));

        drop(analyst);
        let analyst_replacement = acquire_delegated_child(
            INTERNAL_SYSTEM_ANALYST_AGENT_ID,
            ROOT_LEVEL,
            Duration::from_secs(1),
        )
        .await
        .expect("released internal-system-analyst permit");
        drop(analyst_replacement);
        assert_eq!(
            delegated_child_semaphore(ROOT_LEVEL).available_permits(),
            DELEGATED_CHILD_PERMIT_LIMIT
        );
        assert_eq!(
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE.available_permits(),
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT
        );
    }

    /// The deadlock this pool split exists to remove: a full fan of level-0
    /// children that each want to delegate. Every level-0 permit is held for
    /// the holder's whole run, so with one shared pool the grandchildren could
    /// never be admitted and the tree hung. Each level having its own budget
    /// means the nested wave is admitted while its parents still hold theirs.
    #[tokio::test]
    async fn a_full_root_level_fan_can_still_nest() {
        let _test_lock = PERMIT_TEST_LOCK.lock().await;

        // Four concurrent depth-1 children, holding every level-0 permit.
        let mut root_children = Vec::new();
        for index in 0..DELEGATED_CHILD_PERMIT_LIMIT {
            root_children.push(
                acquire_delegated_child_when_available(&format!("fan-child-{index}"), ROOT_LEVEL)
                    .await
                    .expect("level-0 permit within the limit"),
            );
        }
        assert_eq!(delegated_child_semaphore(ROOT_LEVEL).available_permits(), 0);

        // Each of them now delegates. Under the old single global pool this is
        // exactly where all four blocked forever.
        let mut grandchildren = Vec::new();
        for index in 0..DELEGATED_CHILD_PERMITS_BY_LEVEL[1] {
            grandchildren.push(
                acquire_delegated_child("grandchild", 1, Duration::from_millis(250))
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "grandchild {index} must not queue behind its own ancestors: {error}"
                        )
                    }),
            );
        }
        assert_eq!(delegated_child_semaphore(ROOT_LEVEL).available_permits(), 0);
        assert_eq!(delegated_child_semaphore(1).available_permits(), 0);

        drop(grandchildren);
        drop(root_children);
        assert_eq!(
            delegated_child_semaphore(ROOT_LEVEL).available_permits(),
            DELEGATED_CHILD_PERMITS_BY_LEVEL[0]
        );
        assert_eq!(
            delegated_child_semaphore(1).available_permits(),
            DELEGATED_CHILD_PERMITS_BY_LEVEL[1]
        );
    }

    /// Past the last ordered level, holders and waiters share one pool, so the
    /// wait MUST be bounded — a clean delegation failure instead of a hang.
    #[tokio::test]
    async fn delegation_deeper_than_the_ordered_table_fails_instead_of_hanging() {
        let _test_lock = PERMIT_TEST_LOCK.lock().await;
        let deepest = DELEGATED_CHILD_PERMITS_BY_LEVEL.len() - 1;

        let mut held = Vec::new();
        for index in 0..DELEGATED_CHILD_PERMITS_BY_LEVEL[deepest] {
            held.push(
                acquire_delegated_child(&format!("deep-{index}"), deepest, Duration::from_secs(1))
                    .await
                    .expect("deepest-level permit within the limit"),
            );
        }

        let overflow = acquire_delegated_child(
            "deeper-than-the-table",
            deepest + 3,
            Duration::from_millis(10),
        )
        .await
        .err()
        .expect("an over-deep delegation must fail, not hang");
        assert!(overflow.contains("timed out"));

        drop(held);
        assert_eq!(
            delegated_child_semaphore(deepest).available_permits(),
            DELEGATED_CHILD_PERMITS_BY_LEVEL[deepest]
        );
    }

    #[tokio::test]
    async fn cancelling_delegated_child_acquisition_releases_partial_permits() {
        let _test_lock = PERMIT_TEST_LOCK.lock().await;
        let mut global_permits = Vec::new();
        for index in 0..DELEGATED_CHILD_PERMIT_LIMIT {
            global_permits.push(
                acquire_delegated_child(
                    &format!("blocking-agent-{index}"),
                    ROOT_LEVEL,
                    Duration::from_secs(1),
                )
                .await
                .expect("delegated child permit within the level-0 limit"),
            );
        }

        let pending = tokio::spawn(acquire_delegated_child(
            INTERNAL_SYSTEM_ANALYST_AGENT_ID,
            ROOT_LEVEL,
            Duration::from_secs(30),
        ));
        for _ in 0..100 {
            if INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE.available_permits() == 0 {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE.available_permits(),
            0,
            "pending acquisition should hold its target-specific permit"
        );

        pending.abort();
        let cancellation = match pending.await {
            Err(error) => error,
            Ok(_) => panic!("task should be cancelled"),
        };
        assert!(
            cancellation.is_cancelled(),
            "aborting the acquisition should cancel its task"
        );
        assert_eq!(
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE.available_permits(),
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT
        );

        drop(global_permits.pop());
        let analyst = acquire_delegated_child(
            INTERNAL_SYSTEM_ANALYST_AGENT_ID,
            ROOT_LEVEL,
            Duration::from_secs(1),
        )
        .await
        .expect("cancelled acquisition must not leak either permit");
        drop((global_permits, analyst));
        assert_eq!(
            delegated_child_semaphore(ROOT_LEVEL).available_permits(),
            DELEGATED_CHILD_PERMIT_LIMIT
        );
        assert_eq!(
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_SEMAPHORE.available_permits(),
            INTERNAL_SYSTEM_ANALYST_DELEGATED_CHILD_PERMIT_LIMIT
        );
    }

    static ADMISSION_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    static TEST_RSS_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    fn test_rss_probe() -> u64 {
        TEST_RSS_BYTES.load(std::sync::atomic::Ordering::SeqCst)
    }

    struct AdmissionTest {
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl AdmissionTest {
        fn lock() -> Self {
            let lock = ADMISSION_TEST_LOCK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            super::configure_live_agent_limit(super::DEFAULT_LIVE_AGENT_LIMIT);
            super::configure_per_agent_outstanding_limit(
                super::DEFAULT_PER_AGENT_OUTSTANDING_LIMIT,
            );
            super::configure_rss_probe(test_rss_probe);
            super::configure_rss_tripwire(0, 0);
            TEST_RSS_BYTES.store(0, std::sync::atomic::Ordering::SeqCst);
            super::RSS_TRIPPED.store(false, std::sync::atomic::Ordering::SeqCst);
            assert_eq!(
                super::active_agent_loop_count(),
                0,
                "admission tests require no leaked agent-loop guards"
            );
            Self { _lock: lock }
        }
    }

    impl Drop for AdmissionTest {
        fn drop(&mut self) {
            super::configure_live_agent_limit(super::DEFAULT_LIVE_AGENT_LIMIT);
            super::configure_per_agent_outstanding_limit(
                super::DEFAULT_PER_AGENT_OUTSTANDING_LIMIT,
            );
            super::configure_rss_tripwire(0, 0);
            TEST_RSS_BYTES.store(0, std::sync::atomic::Ordering::SeqCst);
            super::RSS_TRIPPED.store(false, std::sync::atomic::Ordering::SeqCst);
        }
    }

    #[test]
    fn admit_agent_loop_allows_the_hard_cap_and_rejects_the_next() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(50);
        let rejected_before = super::snapshot(0).rejected;
        let guards: Vec<_> = (0..50)
            .map(|index| {
                super::admit_agent_loop(Some(&format!("cap-agent-{index}")), None, None)
                    .unwrap_or_else(|error| panic!("admit {index} within cap: {error}"))
            })
            .collect();
        assert_eq!(super::active_agent_loop_count(), 50);
        let overflow = super::admit_agent_loop(Some("cap-overflow"), None, None)
            .err()
            .expect("51st loop must be rejected");
        assert_eq!(
            overflow,
            super::AgentAdmissionRejected::LiveAgentLimit {
                active: 50,
                limit: 50,
            }
        );
        assert_eq!(super::active_agent_loop_count(), 50);
        assert_eq!(super::snapshot(0).rejected, rejected_before + 1);
        assert_eq!(super::snapshot(0).admitted, 50);
        drop(guards);
        assert_eq!(super::active_agent_loop_count(), 0);
    }

    #[test]
    fn admit_agent_loop_releases_on_drop_and_admits_again() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(1);
        let first = super::admit_agent_loop(Some("drop-agent-a"), None, None)
            .expect("first loop within cap");
        assert!(super::admit_agent_loop(Some("drop-agent-b"), None, None).is_err());
        drop(first);
        let replacement = super::admit_agent_loop(Some("drop-agent-b"), None, None)
            .expect("released slot must admit again");
        drop(replacement);
        assert_eq!(super::active_agent_loop_count(), 0);
    }

    #[test]
    fn admit_agent_loop_enforces_per_agent_outstanding_cap() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(50);
        super::configure_per_agent_outstanding_limit(8);
        let per_agent_before = super::snapshot(0).per_agent_rejects;
        let hog: Vec<_> = (0..8)
            .map(|index| {
                super::admit_agent_loop(Some("hog-agent"), None, None)
                    .unwrap_or_else(|error| panic!("hog admit {index}: {error}"))
            })
            .collect();
        let hog_overflow = super::admit_agent_loop(Some("hog-agent"), None, None)
            .err()
            .expect("ninth loop for the same agent must be rejected");
        assert_eq!(
            hog_overflow,
            super::AgentAdmissionRejected::PerAgentLimit {
                agent_id: "hog-agent".to_string(),
                outstanding: 8,
                limit: 8,
            }
        );
        let other = super::admit_agent_loop(Some("other-agent"), None, None)
            .expect("a different agent must still admit");
        assert_eq!(super::active_agent_loop_count(), 9);
        assert_eq!(super::snapshot(0).per_agent_rejects, per_agent_before + 1);
        drop(hog);
        drop(other);
        assert_eq!(super::active_agent_loop_count(), 0);
    }

    #[test]
    fn admit_agent_loop_rss_tripwire_rejects_high_and_recovers_below_low() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(50);
        super::configure_rss_probe(test_rss_probe);
        super::configure_rss_tripwire(1_000, 500);
        let rss_before = super::snapshot(0).rss_blocked;

        TEST_RSS_BYTES.store(1_000, std::sync::atomic::Ordering::SeqCst);
        let high = super::admit_agent_loop(Some("rss-agent"), None, None)
            .err()
            .expect("RSS at high must reject");
        assert_eq!(
            high,
            super::AgentAdmissionRejected::RssTripwire {
                rss_bytes: 1_000,
                high_bytes: 1_000,
            }
        );
        assert_eq!(super::active_agent_loop_count(), 0);

        TEST_RSS_BYTES.store(700, std::sync::atomic::Ordering::SeqCst);
        assert!(
            super::admit_agent_loop(Some("rss-agent"), None, None).is_err(),
            "hysteresis must keep rejecting between high and low"
        );

        TEST_RSS_BYTES.store(500, std::sync::atomic::Ordering::SeqCst);
        let recovered = super::admit_agent_loop(Some("rss-agent"), None, None)
            .expect("RSS at low must recover");
        assert_eq!(super::snapshot(0).rss_blocked, rss_before + 2);
        drop(recovered);
        assert_eq!(super::active_agent_loop_count(), 0);
    }

    #[test]
    fn admit_agent_loop_cap_zero_is_observe_only_and_never_rejects() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(0);
        let rejected_before = super::snapshot(0).rejected;
        let would_before = super::snapshot(0).counters.agent_loop_would_throttle_total;
        let guards: Vec<_> = (0..60)
            .map(|index| {
                super::admit_agent_loop(Some(&format!("observe-agent-{index}")), None, None)
                    .unwrap_or_else(|error| panic!("observe-only must not reject {index}: {error}"))
            })
            .collect();
        let snapshot = super::snapshot(0);
        assert!(snapshot.observe_only);
        assert_eq!(snapshot.mode, "observe_only");
        assert_eq!(snapshot.admitted, 60);
        assert_eq!(snapshot.rejected, rejected_before);
        assert!(snapshot.counters.agent_loop_would_throttle_total > would_before);
        drop(guards);
        assert_eq!(super::active_agent_loop_count(), 0);

        super::configure_rss_probe(test_rss_probe);
        super::configure_rss_tripwire(1, 0);
        TEST_RSS_BYTES.store(10_000, std::sync::atomic::Ordering::SeqCst);
        let hog: Vec<_> = (0..9)
            .map(|index| {
                super::admit_agent_loop(Some("observe-hog"), None, None).unwrap_or_else(|error| {
                    panic!("observe-only must not reject hog {index} or RSS: {error}")
                })
            })
            .collect();
        assert_eq!(hog.len(), 9);
        drop(hog);
        TEST_RSS_BYTES.store(0, std::sync::atomic::Ordering::SeqCst);
        super::configure_rss_tripwire(0, 0);
        assert_eq!(super::active_agent_loop_count(), 0);
    }

    /// Admission-count harness for the 300-task contract. This is not the
    /// owner-gated live 300-task soak: it only proves the hard cap admits
    /// 300, rejects 301, and returns to zero after every guard drops.
    #[test]
    fn admit_agent_loop_300_task_admission_count_harness() {
        let _test = AdmissionTest::lock();
        super::configure_live_agent_limit(300);
        let guards: Vec<_> = (0..300)
            .map(|index| {
                super::admit_agent_loop(Some(&format!("soak-agent-{index}")), None, None)
                    .unwrap_or_else(|error| panic!("admit {index} within 300: {error}"))
            })
            .collect();
        assert_eq!(super::active_agent_loop_count(), 300);
        assert_eq!(super::snapshot(0).admitted, 300);
        let overflow = super::admit_agent_loop(Some("soak-overflow"), None, None)
            .err()
            .expect("301st loop must be rejected");
        assert_eq!(
            overflow,
            super::AgentAdmissionRejected::LiveAgentLimit {
                active: 300,
                limit: 300,
            }
        );
        assert_eq!(super::active_agent_loop_count(), 300);
        drop(guards);
        assert_eq!(super::active_agent_loop_count(), 0);
        assert_eq!(super::snapshot(0).admitted, 0);
    }

    #[test]
    fn delegated_child_permit_table_remains_split_by_level() {
        assert_eq!(DELEGATED_CHILD_PERMITS_BY_LEVEL, [4, 4, 2, 1]);
    }
}

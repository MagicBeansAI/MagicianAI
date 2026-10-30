//! Temperature overlay for memory candidates.
//!
//! Canonical memory stays in tier/user/episode files. This module maintains a
//! small JSON overlay under `memory/index/temperature_overlay.json` keyed by the
//! same candidate identity used by the derived memory index.
//!
//! Usage-counter bookkeeping (`record_*_prompt_usage`, `record_*_retrieval_usage`,
//! `record_*_outcome_usage`) is write-behind: each update is applied to the
//! in-process snapshot at once and persisted by one overlay write per
//! [`MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY`]. A crash loses at most that
//! window of counters. Every other mutation writes through before returning
//! and carries the pending counters with it.

use std::{
    borrow::Cow,
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering as AtomicOrdering},
        Arc, Mutex, MutexGuard, OnceLock, RwLock, Weak,
    },
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    key_encoding::{
        bounded_key_segment, length_prefixed_key_segment, parse_length_prefixed_key_segment,
    },
    memory_candidates::{MemoryCandidateDocument, SemanticMemoryType},
    memory_tiers::TierScope,
    storage_trait::{MemoryStorage, MemoryStorageError},
};

pub const MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION: u32 = 6;
const MAX_APPLIED_UTILITY_REVIEW_RUNS: usize = 1_024;
pub const MEMORY_TEMPERATURE_OVERLAY_FILE: &str = "temperature_overlay.json";
const OVERLAY_LOCK_SLOW_WARN_AFTER: Duration = Duration::from_millis(100);
const PROMPT_TIER_SNAPSHOT_CACHE_MAX_ENTRIES: usize = 16;
const PROMPT_TIER_SNAPSHOT_REUSE: Duration = Duration::from_secs(30);
const PROMPT_TIER_SNAPSHOT_CACHE_TTL: Duration = Duration::from_secs(60);
/// How long usage-counter bookkeeping waits in memory before one overlay write
/// persists it. This is the crash-loss window for those counters: a process
/// that dies without its shutdown flush loses at most this much of them.
pub const MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY: Duration = Duration::from_secs(2);
/// Bound on the per-overlay replay log of pending counter updates. The log is
/// only replayed when the file is rewritten outside this process while updates
/// are pending; trimming it is logged, and the trimmed updates still persist
/// with the in-process snapshot they were applied to.
const MAX_PENDING_COUNTER_MUTATIONS: usize = 4_096;
/// Timer-driven flush attempts after a failure before the timer stops retrying.
/// The pending updates stay; the next counter update, durable overlay write or
/// shutdown flush tries again.
const MAX_COUNTER_FLUSH_TIMER_ATTEMPTS: u32 = 6;
const COUNTER_FLUSH_MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MemoryTemperatureTier {
    T0,
    T1,
    T2,
    T3,
}

impl Default for MemoryTemperatureTier {
    fn default() -> Self {
        Self::T2
    }
}

impl MemoryTemperatureTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::T0 => "t0",
            Self::T1 => "t1",
            Self::T2 => "t2",
            Self::T3 => "t3",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTemperatureUtilityLabel {
    Referenced,
    Useful,
    LoadBearing,
    Irrelevant,
    Stale,
    Harmful,
    Unknown,
}

impl MemoryTemperatureUtilityLabel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Referenced => "referenced",
            Self::Useful => "useful",
            Self::LoadBearing => "load_bearing",
            Self::Irrelevant => "irrelevant",
            Self::Stale => "stale",
            Self::Harmful => "harmful",
            Self::Unknown => "unknown",
        }
    }

    pub fn from_review_label(value: &str) -> Self {
        match value.trim().to_ascii_lowercase().as_str() {
            "referenced" | "mentioned" | "cited" => Self::Referenced,
            "useful" | "helpful" => Self::Useful,
            "load_bearing" | "load-bearing" | "essential" | "critical" => Self::LoadBearing,
            "irrelevant" | "unused" | "not_useful" | "not useful" => Self::Irrelevant,
            "stale" | "outdated" | "obsolete" => Self::Stale,
            "harmful" | "wrong" | "misleading" | "contradictory" => Self::Harmful,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryTemperatureEntry {
    pub memory_candidate_key: String,
    pub semantic_memory_type: SemanticMemoryType,
    pub temperature_tier: MemoryTemperatureTier,
    pub temperature_score: f64,
    pub confidence: Option<f64>,
    #[serde(default)]
    pub last_retrieved_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_selected_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_injected_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_used_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub retrieved_count: u32,
    #[serde(default)]
    pub selected_count: u32,
    #[serde(default)]
    pub injected_count: u32,
    #[serde(default)]
    pub successful_use_count: u32,
    #[serde(default)]
    pub failed_use_count: u32,
    #[serde(default)]
    pub reviewed_referenced_count: u32,
    #[serde(default)]
    pub reviewed_useful_count: u32,
    #[serde(default)]
    pub reviewed_load_bearing_count: u32,
    #[serde(default)]
    pub reviewed_irrelevant_count: u32,
    #[serde(default)]
    pub reviewed_stale_count: u32,
    #[serde(default)]
    pub reviewed_harmful_count: u32,
    #[serde(default)]
    pub source_ids: Vec<String>,
    #[serde(default)]
    pub superseded_by: Option<String>,
    #[serde(default)]
    pub superseded_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub supersession_reason: Option<String>,
    #[serde(default)]
    pub supersession_confidence: Option<f64>,
    #[serde(default)]
    pub supersession_source: Option<String>,
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// When this entry first entered the overlay.
    ///
    /// Distinct from `updated_at`, which maintenance refreshes whenever a score
    /// drifts — which is nearly every pass, since recency decay is continuous.
    /// Retention age therefore cannot be derived from `updated_at`: a dead
    /// entry would keep resetting its own clock and never age out. `None` marks
    /// an entry written before this field existed.
    #[serde(default)]
    pub first_seen_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_temperature_review_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_temperature_change_reason: Option<String>,
    #[serde(default)]
    pub last_utility_review_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub last_utility_review_run_id: Option<String>,
    #[serde(default)]
    pub last_utility_review_label: Option<MemoryTemperatureUtilityLabel>,
    #[serde(default)]
    pub last_utility_review_confidence: Option<f64>,
    #[serde(default)]
    pub last_utility_review_reason: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryTemperatureOverlay {
    pub schema_version: u32,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub entries: BTreeMap<String, MemoryTemperatureEntry>,
    /// Bounded exactly-once ledger for durable utility-review delivery. The
    /// queue may replay an item after a crash between the overlay write and
    /// lease completion; retaining the run identity prevents counters and
    /// temperature signals from being applied twice.
    #[serde(default)]
    pub applied_utility_review_runs: BTreeMap<String, DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OverlayFileStamp {
    len: u64,
    modified_nanos: Option<u128>,
}

#[derive(Debug, Clone)]
struct OverlaySnapshotCacheEntry {
    stamp: Option<OverlayFileStamp>,
    overlay: Arc<MemoryTemperatureOverlay>,
}

fn overlay_snapshot_cache() -> &'static RwLock<HashMap<PathBuf, OverlaySnapshotCacheEntry>> {
    static CACHE: OnceLock<RwLock<HashMap<PathBuf, OverlaySnapshotCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| RwLock::new(HashMap::new()))
}

struct PromptTierSnapshotCacheEntry {
    overlay: Arc<MemoryTemperatureOverlay>,
    created_at: Instant,
    tiers: Arc<OnceLock<Arc<BTreeMap<String, MemoryTemperatureTier>>>>,
    last_used: Instant,
}

fn prompt_tier_snapshot_cache() -> &'static Mutex<Vec<PromptTierSnapshotCacheEntry>> {
    static CACHE: OnceLock<Mutex<Vec<PromptTierSnapshotCacheEntry>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Vec::new()))
}

impl Default for MemoryTemperatureOverlay {
    fn default() -> Self {
        Self {
            schema_version: MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION,
            updated_at: Utc::now(),
            entries: BTreeMap::new(),
            applied_utility_review_runs: BTreeMap::new(),
        }
    }
}

/// Serialize overlay read-modify-write cycles within the process. The atomic
/// file write prevents torn files, but concurrent `load -> modify -> save` cycles
/// would otherwise lose updates (last writer wins). Holding this guard across a
/// cycle makes the overlay mutations linearizable for the in-process callers
/// (prompt renders, usage/outcome/review recorders, maintenance). Process-global
/// is fine — overlay writes are infrequent and short. Only the leaf entry points
/// take it (never nested), so it cannot deadlock.
fn overlay_write_lock() -> &'static tokio::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| tokio::sync::Mutex::new(()))
}

fn overlay_lock_max_wait_ms() -> &'static AtomicUsize {
    static MAX_WAIT_MS: std::sync::OnceLock<AtomicUsize> = std::sync::OnceLock::new();
    MAX_WAIT_MS.get_or_init(|| AtomicUsize::new(0))
}

pub fn memory_temperature_overlay_max_lock_wait_ms() -> usize {
    overlay_lock_max_wait_ms().load(AtomicOrdering::Relaxed)
}

fn update_overlay_lock_max_wait(candidate: usize) -> bool {
    let slot = overlay_lock_max_wait_ms();
    let mut current = slot.load(AtomicOrdering::Relaxed);
    while candidate > current {
        match slot.compare_exchange_weak(
            current,
            candidate,
            AtomicOrdering::Relaxed,
            AtomicOrdering::Relaxed,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
    false
}

async fn lock_overlay_for_write(operation: &'static str) -> tokio::sync::MutexGuard<'static, ()> {
    let wait_started = Instant::now();
    let guard = overlay_write_lock().lock().await;
    let wait = wait_started.elapsed();
    let wait_ms = wait.as_millis() as usize;
    if wait >= OVERLAY_LOCK_SLOW_WARN_AFTER && update_overlay_lock_max_wait(wait_ms) {
        tracing::warn!(
            target: "magician::metrics::memory_temperature",
            operation,
            wait_ms,
            "memory temperature overlay lock wait exceeded threshold"
        );
    }
    guard
}

pub fn memory_temperature_overlay_path(storage: &dyn MemoryStorage) -> PathBuf {
    storage
        .root()
        .join("index")
        .join(MEMORY_TEMPERATURE_OVERLAY_FILE)
}

pub async fn load_memory_temperature_overlay(
    storage: &dyn MemoryStorage,
) -> Result<MemoryTemperatureOverlay, MemoryStorageError> {
    Ok((*load_memory_temperature_overlay_snapshot(storage).await?).clone())
}

/// Load a process-shared immutable overlay snapshot.
///
/// Prompt rendering only reads this data, while mutation entry points retain
/// their existing owned read-modify-write cycle. Keeping the parsed snapshot
/// behind an `Arc` avoids repeatedly deserializing a multi-megabyte overlay on
/// every prompt without weakening atomic-file freshness checks.
///
/// The snapshot includes pending write-behind counter updates: they are
/// applied to the cached revision before they are written. The one exception
/// is an overlay file rewritten by another process while updates are pending;
/// readers then see that file's revision until the next locked cycle in this
/// process (at the latest the flush, within
/// [`MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY`]) re-applies them.
pub async fn load_memory_temperature_overlay_snapshot(
    storage: &dyn MemoryStorage,
) -> Result<Arc<MemoryTemperatureOverlay>, MemoryStorageError> {
    Ok(load_overlay_snapshot_stamped(storage).await?.overlay)
}

/// A snapshot and the overlay file stamp it is current for.
struct StampedOverlaySnapshot {
    overlay: Arc<MemoryTemperatureOverlay>,
    stamp: Option<OverlayFileStamp>,
    /// `false` when the file changed while it was being read: no stamp is
    /// known to describe `overlay`, so nothing derived from it may be cached.
    stamp_verified: bool,
}

async fn load_overlay_snapshot_stamped(
    storage: &dyn MemoryStorage,
) -> Result<StampedOverlaySnapshot, MemoryStorageError> {
    let path = memory_temperature_overlay_path(storage);
    let stamp = overlay_file_stamp(&path).await?;
    if let Ok(cache) = overlay_snapshot_cache().read() {
        if let Some(entry) = cache.get(&path) {
            if entry.stamp == stamp {
                return Ok(StampedOverlaySnapshot {
                    overlay: Arc::clone(&entry.overlay),
                    stamp,
                    stamp_verified: true,
                });
            }
        }
    }

    let value = match storage.read_json_value(&path).await {
        Ok(value) => value,
        Err(MemoryStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            let overlay = install_read_overlay_snapshot(
                path,
                stamp,
                Arc::new(MemoryTemperatureOverlay::default()),
            );
            return Ok(StampedOverlaySnapshot {
                overlay,
                stamp,
                stamp_verified: true,
            });
        },
        Err(error) => return Err(error),
    };
    let overlay = Arc::new(serde_json::from_value(value).map_err(MemoryStorageError::Json)?);

    // Cache only if the file did not change while we were reading it. Stamping
    // afterwards alone would file this content under a *later* revision's
    // stamp, and every subsequent load would then match that stamp and return
    // content that was already stale — indefinitely, until the file changed
    // again. Readers do not hold the write lock, so this races by design.
    //
    // The consequence reaches past stale reads: every locked read-modify-write
    // cycle begins with this same loader, so a mis-stamped entry makes the next
    // writer load the old revision, mutate it, and save — discarding the newer
    // one. That is precisely the lost update the write lock exists to prevent.
    let stamp_after = overlay_file_stamp(&path).await?;
    if stamp_after == stamp {
        let overlay = install_read_overlay_snapshot(path, stamp_after, overlay);
        return Ok(StampedOverlaySnapshot {
            overlay,
            stamp: stamp_after,
            stamp_verified: true,
        });
    }
    Ok(StampedOverlaySnapshot {
        overlay,
        stamp: stamp_after,
        stamp_verified: false,
    })
}

/// Cache an overlay a reader just parsed from disk, unless the cache already
/// holds a revision for the same file stamp. That revision is the same file
/// content, possibly with pending write-behind counters applied, so it is
/// never older than what was just read, and replacing it would hide those
/// counters from readers. Returns the snapshot now cached.
fn install_read_overlay_snapshot(
    path: PathBuf,
    stamp: Option<OverlayFileStamp>,
    overlay: Arc<MemoryTemperatureOverlay>,
) -> Arc<MemoryTemperatureOverlay> {
    let Ok(mut cache) = overlay_snapshot_cache().write() else {
        return overlay;
    };
    if let Some(existing) = cache.get(&path) {
        if existing.stamp == stamp {
            return Arc::clone(&existing.overlay);
        }
    }
    let previous = cache.insert(
        path,
        OverlaySnapshotCacheEntry {
            stamp,
            overlay: Arc::clone(&overlay),
        },
    );
    drop(cache);
    drop(previous);
    overlay
}

/// Persist an overlay the caller already holds. Serialization runs on the
/// blocking pool; the leaf mutators below go through
/// `mutate_memory_temperature_overlay` instead, which also keeps the clone
/// and the mutation off async workers.
///
/// The caller's overlay replaces the file whole, so it also settles pending
/// write-behind counters: the ones applied before the caller loaded its copy
/// are in it, and the ones applied after are discarded, as they always were
/// by a save that replaced a newer file.
pub async fn save_memory_temperature_overlay(
    storage: &dyn MemoryStorage,
    overlay: &MemoryTemperatureOverlay,
) -> Result<(), MemoryStorageError> {
    let _overlay_guard = lock_overlay_for_write("save").await;
    let overlay = Arc::new(overlay.clone());
    let (overlay, bytes) = run_overlay_cpu_work(move || {
        let bytes = serialize_memory_temperature_overlay(&overlay);
        (overlay, bytes)
    })
    .await?;
    write_memory_temperature_overlay_bytes(storage, overlay, bytes?).await
}

/// The on-disk encoding: the overlay serialized straight to pretty JSON.
///
/// Earlier revisions went through an intermediate `serde_json::Value`, which
/// sorted object fields alphabetically and roughly doubled the CPU and
/// allocation cost of every save. Readers parse JSON by field name, so only
/// the field order inside each object differs.
fn serialize_memory_temperature_overlay(
    overlay: &MemoryTemperatureOverlay,
) -> Result<Vec<u8>, MemoryStorageError> {
    serde_json::to_vec_pretty(overlay).map_err(MemoryStorageError::Json)
}

/// Run CPU-bound overlay work (deep clone, mutation, maintenance,
/// serialization, dropping a whole overlay) on Tokio's blocking pool.
///
/// A multi-megabyte overlay costs hundreds of milliseconds per cycle in a
/// debug build; on an async worker that starves every other task of the same
/// chat turn. This crate cannot use magician's blocking admission, but every
/// leaf mutator runs this while holding the process-global
/// [`overlay_write_lock`], so the module submits at most one such job at a
/// time.
async fn run_overlay_cpu_work<R, F>(work: F) -> Result<R, MemoryStorageError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(value) => Ok(value),
        // Keep the old failure mode: a panic in overlay code still unwinds
        // through the caller instead of turning into a logged error.
        Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
        Err(error) => Err(MemoryStorageError::Other(format!(
            "memory temperature overlay work was cancelled: {error}"
        ))),
    }
}

/// One locked `load -> modify -> save` cycle.
///
/// `mutate` returns its result and whether the overlay changed. It runs on
/// the blocking pool against a private copy of the current snapshot; an
/// unchanged overlay is neither serialized nor rewritten. The write lock is
/// held from the load until the new snapshot is installed in the cache, so
/// cycles stay linearizable exactly as before: every cycle starts from the
/// previous cycle's committed overlay.
///
/// Returns the committed overlay (the cached snapshot when nothing changed).
async fn mutate_memory_temperature_overlay<R, F>(
    storage: &dyn MemoryStorage,
    operation: &'static str,
    mutate: F,
) -> Result<(R, Arc<MemoryTemperatureOverlay>), MemoryStorageError>
where
    F: FnOnce(&mut MemoryTemperatureOverlay) -> (R, bool) + Send + 'static,
    R: Send + 'static,
{
    let _overlay_guard = lock_overlay_for_write(operation).await;
    mutate_memory_temperature_overlay_locked(storage, mutate).await
}

/// [`mutate_memory_temperature_overlay`] for a caller that already holds the
/// overlay write lock and has more to do under it.
///
/// The cycle starts from the revision with every pending write-behind counter
/// applied, so a write here persists those counters exactly once and settles
/// them.
async fn mutate_memory_temperature_overlay_locked<R, F>(
    storage: &dyn MemoryStorage,
    mutate: F,
) -> Result<(R, Arc<MemoryTemperatureOverlay>), MemoryStorageError>
where
    F: FnOnce(&mut MemoryTemperatureOverlay) -> (R, bool) + Send + 'static,
    R: Send + 'static,
{
    let snapshot = load_overlay_base_locked(storage).await?.overlay;
    let (result, overlay, bytes) = run_overlay_cpu_work(move || {
        let mut overlay = (*snapshot).clone();
        let (result, changed) = mutate(&mut overlay);
        if !changed {
            // Drop the copy here, on the blocking thread, and hand back the
            // snapshot: nothing durable differs from it.
            drop(overlay);
            return Ok((result, snapshot, None));
        }
        drop(snapshot);
        let bytes = serialize_memory_temperature_overlay(&overlay)?;
        Ok::<_, MemoryStorageError>((result, Arc::new(overlay), Some(bytes)))
    })
    .await??;
    if let Some(bytes) = bytes {
        write_memory_temperature_overlay_bytes(storage, Arc::clone(&overlay), bytes).await?;
    }
    Ok((result, overlay))
}

/// Unwrap a committed overlay for the APIs that return it by value. The
/// cache usually shares the `Arc`, so this is a deep clone; it runs on the
/// blocking pool.
async fn owned_memory_temperature_overlay(
    overlay: Arc<MemoryTemperatureOverlay>,
) -> Result<MemoryTemperatureOverlay, MemoryStorageError> {
    run_overlay_cpu_work(move || {
        Arc::try_unwrap(overlay).unwrap_or_else(|shared| (*shared).clone())
    })
    .await
}

async fn write_memory_temperature_overlay_bytes(
    storage: &dyn MemoryStorage,
    overlay: Arc<MemoryTemperatureOverlay>,
    bytes: Vec<u8>,
) -> Result<(), MemoryStorageError> {
    let path = memory_temperature_overlay_path(storage);
    storage.write_bytes_atomic(&path, &bytes).await?;
    drop(bytes);
    // Every overlay write starts from the revision with the pending counters
    // applied (or, for a save, replaces it whole), so the file now carries
    // them. Settle them before anything else can fail: a stale replay log
    // would apply them a second time onto this file.
    settle_pending_counter_writes(&path);
    let stamp = overlay_file_stamp(&path).await?;
    let previous = update_overlay_snapshot_cache(path, stamp, overlay);
    retire_overlay_snapshot(previous);
    Ok(())
}

/// Release a replaced snapshot. When this was its last reference, freeing a
/// multi-megabyte overlay is real work, so it happens on the blocking pool.
fn retire_overlay_snapshot(previous: Option<Arc<MemoryTemperatureOverlay>>) {
    let Some(previous) = previous else {
        return;
    };
    if Arc::strong_count(&previous) == 1 {
        drop(tokio::task::spawn_blocking(move || drop(previous)));
    }
}

async fn overlay_file_stamp(
    path: &PathBuf,
) -> Result<Option<OverlayFileStamp>, MemoryStorageError> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(MemoryStorageError::Io(error)),
    };
    let modified_nanos = metadata
        .modified()
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos());
    Ok(Some(OverlayFileStamp {
        len: metadata.len(),
        modified_nanos,
    }))
}

fn update_overlay_snapshot_cache(
    path: PathBuf,
    stamp: Option<OverlayFileStamp>,
    overlay: Arc<MemoryTemperatureOverlay>,
) -> Option<Arc<MemoryTemperatureOverlay>> {
    // Hand the replaced snapshot back so the caller frees it outside the
    // cache lock (and, on the write path, off the async worker).
    let mut cache = overlay_snapshot_cache().write().ok()?;
    cache
        .insert(path, OverlaySnapshotCacheEntry { stamp, overlay })
        .map(|previous| previous.overlay)
}

// Usage-counter write-behind.
//
// Prompt, retrieval and outcome bookkeeping fires several times per chat turn
// and each write re-serializes the whole overlay. Those updates are applied to
// the cached snapshot at once (readers see them) and persisted by one write
// per `MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY`. Lock order: the overlay write
// lock, then (briefly, never across an await) the pending-writes mutex.

/// One usage-counter update held for write-behind. It carries its own
/// timestamp, so re-applying it onto another base revision produces the same
/// counters and `last_*_at` values as the original application.
#[derive(Debug, Clone)]
enum PendingCounterMutation {
    PromptUsage {
        retrieved_candidate_keys: Arc<[String]>,
        selected_candidate_keys: Arc<BTreeSet<String>>,
        at: DateTime<Utc>,
    },
    OutcomeUsage {
        memory_candidate_keys: Arc<[String]>,
        signal: MemoryTemperatureOutcomeSignal,
        at: DateTime<Utc>,
    },
}

#[derive(Debug, Clone, Copy)]
enum CounterMutationSummary {
    PromptUsage(MemoryTemperatureUsageSummary),
    OutcomeUsage(MemoryTemperatureOutcomeUsageSummary),
}

impl PendingCounterMutation {
    fn at(&self) -> DateTime<Utc> {
        match self {
            Self::PromptUsage { at, .. } | Self::OutcomeUsage { at, .. } => *at,
        }
    }

    /// Apply only the counter update, without maintenance.
    fn apply_counters(&self, overlay: &mut MemoryTemperatureOverlay) -> CounterMutationSummary {
        match self {
            Self::PromptUsage {
                retrieved_candidate_keys,
                selected_candidate_keys,
                at,
            } => CounterMutationSummary::PromptUsage(apply_memory_temperature_prompt_usage(
                overlay,
                retrieved_candidate_keys,
                selected_candidate_keys,
                *at,
            )),
            Self::OutcomeUsage {
                memory_candidate_keys,
                signal,
                at,
            } => CounterMutationSummary::OutcomeUsage(apply_memory_temperature_outcome_usage(
                overlay,
                memory_candidate_keys,
                *signal,
                *at,
            )),
        }
    }

    /// The whole recording cycle: the counter update, then maintenance at the
    /// same instant. Returns the caller's summary and whether anything changed.
    fn apply_cycle(
        &self,
        overlay: &mut MemoryTemperatureOverlay,
    ) -> (CounterMutationSummary, bool) {
        let at = self.at();
        match self.apply_counters(overlay) {
            CounterMutationSummary::PromptUsage(summary) => {
                let maintenance = apply_memory_temperature_maintenance(overlay, at);
                let changed = summary.retrieved > 0
                    || summary.selected > 0
                    || summary.injected > 0
                    || maintenance.changed > 0;
                (CounterMutationSummary::PromptUsage(summary), changed)
            },
            CounterMutationSummary::OutcomeUsage(mut summary) => {
                summary.maintenance = apply_memory_temperature_maintenance(overlay, at);
                let changed = summary.used > 0 || summary.maintenance.changed > 0;
                (CounterMutationSummary::OutcomeUsage(summary), changed)
            },
        }
    }
}

/// Counter updates applied in memory but not yet written, for one overlay file.
struct PendingCounterWrites {
    /// Replay log, oldest first. Every update in it is already applied to
    /// `applied_to`; it is replayed only if that revision is displaced by a
    /// file rewritten outside this process.
    mutations: VecDeque<PendingCounterMutation>,
    /// Updates trimmed from the replay log at [`MAX_PENDING_COUNTER_MUTATIONS`].
    /// They are still applied to `applied_to` and persist with it.
    trimmed: usize,
    /// The in-process revision holding every pending update. Weak, so it
    /// neither keeps a replaced overlay alive nor counts toward the strong
    /// references `retire_overlay_snapshot` inspects; the allocation it pins
    /// keeps the pointer comparison free of address reuse.
    applied_to: Weak<MemoryTemperatureOverlay>,
    /// The handle the deferred flush writes through.
    storage: Arc<dyn MemoryStorage>,
    /// When the scheduled flush is due; `None` while none is scheduled.
    flush_due_at: Option<Instant>,
    /// Consecutive failed timer flushes.
    failed_attempts: u32,
}

impl PendingCounterWrites {
    fn pending(&self) -> usize {
        self.mutations.len() + self.trimmed
    }
}

fn pending_counter_writes() -> &'static Mutex<HashMap<PathBuf, PendingCounterWrites>> {
    static PENDING: OnceLock<Mutex<HashMap<PathBuf, PendingCounterWrites>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Short critical sections only: never held across an await, and never taken
/// before the overlay write lock.
fn lock_pending_counter_writes() -> MutexGuard<'static, HashMap<PathBuf, PendingCounterWrites>> {
    pending_counter_writes()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// How many usage-counter updates to this storage's overlay are applied in
/// memory but not yet written.
pub fn memory_temperature_pending_counter_mutations(storage: &dyn MemoryStorage) -> usize {
    let path = memory_temperature_overlay_path(storage);
    lock_pending_counter_writes()
        .get(&path)
        .map_or(0, PendingCounterWrites::pending)
}

/// The pending updates to replay onto `overlay`, when it is not the revision
/// they were applied to.
fn pending_counter_replay(
    path: &Path,
    overlay: &Arc<MemoryTemperatureOverlay>,
) -> Option<(Vec<PendingCounterMutation>, usize)> {
    let pending = lock_pending_counter_writes();
    let entry = pending.get(path)?;
    if std::ptr::eq(entry.applied_to.as_ptr(), Arc::as_ptr(overlay)) {
        return None;
    }
    Some((entry.mutations.iter().cloned().collect(), entry.trimmed))
}

fn mark_pending_counter_writes_applied(path: &Path, overlay: &Arc<MemoryTemperatureOverlay>) {
    if let Some(entry) = lock_pending_counter_writes().get_mut(path) {
        entry.applied_to = Arc::downgrade(overlay);
    }
}

/// Forget the pending updates of an overlay whose file now carries them.
fn settle_pending_counter_writes(path: &Path) {
    let settled = lock_pending_counter_writes().remove(path);
    drop(settled);
}

/// The revision a locked overlay cycle starts from: the current snapshot with
/// every pending counter update applied exactly once.
///
/// Pending updates are normally already part of the cached snapshot. They are
/// replayed only when that revision was displaced, which happens when the file
/// was rewritten outside this process and a reader reloaded it. Every write in
/// this process settles the pending updates it carries before anything else
/// can observe the new file, so a displaced revision never already holds them.
async fn load_overlay_base_locked(
    storage: &dyn MemoryStorage,
) -> Result<StampedOverlaySnapshot, MemoryStorageError> {
    let path = memory_temperature_overlay_path(storage);
    let loaded = load_overlay_snapshot_stamped(storage).await?;
    let Some((mutations, trimmed)) = pending_counter_replay(&path, &loaded.overlay) else {
        return Ok(loaded);
    };
    tracing::warn!(
        target: "magician::metrics::memory_temperature",
        path = %path.display(),
        replayed = mutations.len(),
        trimmed,
        "memory temperature overlay was rewritten under pending counter updates; re-applying them"
    );
    let StampedOverlaySnapshot {
        overlay: base,
        stamp,
        stamp_verified,
    } = loaded;
    let overlay = run_overlay_cpu_work(move || {
        let mut overlay = (*base).clone();
        drop(base);
        for mutation in &mutations {
            mutation.apply_counters(&mut overlay);
        }
        apply_memory_temperature_maintenance(&mut overlay, Utc::now());
        Arc::new(overlay)
    })
    .await?;
    if stamp_verified {
        let previous = update_overlay_snapshot_cache(path.clone(), stamp, Arc::clone(&overlay));
        retire_overlay_snapshot(previous);
    }
    mark_pending_counter_writes_applied(&path, &overlay);
    Ok(StampedOverlaySnapshot {
        overlay,
        stamp,
        stamp_verified,
    })
}

/// Record one usage-counter update.
///
/// With an owned storage handle the update is applied to the cached snapshot
/// under the write lock, and its overlay write is deferred and shared with
/// every other counter update of the next
/// [`MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY`]. Without one it writes through,
/// like every other cycle.
async fn record_memory_temperature_counter_mutation(
    storage: &dyn MemoryStorage,
    operation: &'static str,
    mutation: PendingCounterMutation,
) -> Result<CounterMutationSummary, MemoryStorageError> {
    let Some(handle) = storage.shared_handle() else {
        let (summary, _) = mutate_memory_temperature_overlay(storage, operation, move |overlay| {
            mutation.apply_cycle(overlay)
        })
        .await?;
        return Ok(summary);
    };
    let _overlay_guard = lock_overlay_for_write(operation).await;
    let base = load_overlay_base_locked(storage).await?;
    if !base.stamp_verified {
        // The file changed while this cycle read it, so no stamp describes an
        // unwritten revision built on it. Write this update through.
        let (summary, _) = mutate_memory_temperature_overlay_locked(storage, move |overlay| {
            mutation.apply_cycle(overlay)
        })
        .await?;
        return Ok(summary);
    }
    let path = memory_temperature_overlay_path(storage);
    let base_stamp = base.stamp;
    let snapshot = base.overlay;
    let pending = mutation.clone();
    let (summary, committed) = run_overlay_cpu_work(move || {
        let mut overlay = (*snapshot).clone();
        let (summary, changed) = mutation.apply_cycle(&mut overlay);
        if !changed {
            drop(overlay);
            return (summary, None);
        }
        drop(snapshot);
        (summary, Some(Arc::new(overlay)))
    })
    .await?;
    let Some(committed) = committed else {
        return Ok(summary);
    };
    // Cached under the stamp of the file it was built on: readers keep being
    // served this revision until the file changes.
    let previous = update_overlay_snapshot_cache(path.clone(), base_stamp, Arc::clone(&committed));
    retire_overlay_snapshot(previous);
    queue_pending_counter_write(path, pending, &committed, handle);
    Ok(summary)
}

fn queue_pending_counter_write(
    path: PathBuf,
    mutation: PendingCounterMutation,
    applied_to: &Arc<MemoryTemperatureOverlay>,
    storage: Arc<dyn MemoryStorage>,
) {
    let now = Instant::now();
    let (schedule, trimmed) = {
        let mut pending = lock_pending_counter_writes();
        let entry = pending
            .entry(path.clone())
            .or_insert_with(|| PendingCounterWrites {
                mutations: VecDeque::new(),
                trimmed: 0,
                applied_to: Weak::new(),
                storage: Arc::clone(&storage),
                flush_due_at: None,
                failed_attempts: 0,
            });
        entry.mutations.push_back(mutation);
        let mut trimmed = None;
        if entry.mutations.len() > MAX_PENDING_COUNTER_MUTATIONS {
            entry.mutations.pop_front();
            entry.trimmed += 1;
            trimmed = Some(entry.trimmed);
        }
        entry.applied_to = Arc::downgrade(applied_to);
        entry.storage = storage;
        // A flush overdue by a whole window lost its timer (its runtime shut
        // down, say). Schedule another; a duplicate finds nothing pending.
        let timer_lost = entry
            .flush_due_at
            .is_some_and(|due| now >= due + counter_flush_delay());
        let schedule = entry.flush_due_at.is_none() || timer_lost;
        if schedule {
            entry.flush_due_at = Some(now + counter_flush_delay());
        }
        (schedule, trimmed)
    };
    if let Some(trimmed) = trimmed.filter(|trimmed| *trimmed == 1 || *trimmed % 1_024 == 0) {
        tracing::warn!(
            target: "magician::metrics::memory_temperature",
            path = %path.display(),
            trimmed,
            "memory temperature counter replay log is full; the oldest updates persist only with the in-process snapshot"
        );
    }
    if schedule {
        schedule_counter_flush(path, counter_flush_delay());
    }
}

/// Tests flush explicitly and assert exact write counts; a real 2 s timer
/// firing during a stall on a loaded machine would make them flaky.
fn counter_flush_delay() -> Duration {
    if cfg!(test) {
        Duration::from_secs(3_600)
    } else {
        MEMORY_TEMPERATURE_COUNTER_FLUSH_DELAY
    }
}

fn counter_flush_retry_delay(failed_attempts: u32) -> Duration {
    counter_flush_delay()
        .saturating_mul(1u32 << failed_attempts.min(5))
        .min(COUNTER_FLUSH_MAX_RETRY_DELAY)
}

fn schedule_counter_flush(path: PathBuf, delay: Duration) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        // No runtime to run a timer on: the next counter update reschedules
        // once this one is overdue, and any durable write or the shutdown
        // flush persists the updates meanwhile.
        return;
    };
    runtime.spawn(async move {
        tokio::time::sleep(delay).await;
        run_scheduled_counter_flush(path).await;
    });
}

async fn run_scheduled_counter_flush(path: PathBuf) {
    let _overlay_guard = lock_overlay_for_write("counter_flush").await;
    let storage = {
        let mut pending = lock_pending_counter_writes();
        let Some(entry) = pending.get_mut(&path) else {
            // A durable write or an explicit flush already persisted them.
            return;
        };
        if entry.flush_due_at.is_some_and(|due| Instant::now() < due) {
            // A later timer (a retry, or the replacement for a timer presumed
            // lost) owns this flush.
            return;
        }
        entry.flush_due_at = None;
        Arc::clone(&entry.storage)
    };
    let Err(error) = flush_pending_counter_writes_locked(storage.as_ref()).await else {
        return;
    };
    let retry = {
        let mut pending = lock_pending_counter_writes();
        pending.get_mut(&path).map(|entry| {
            entry.failed_attempts = entry.failed_attempts.saturating_add(1);
            let delay = (entry.failed_attempts < MAX_COUNTER_FLUSH_TIMER_ATTEMPTS)
                .then(|| counter_flush_retry_delay(entry.failed_attempts));
            if let Some(delay) = delay {
                entry.flush_due_at = Some(Instant::now() + delay);
            }
            (entry.failed_attempts, entry.pending(), delay)
        })
    };
    let Some((failed_attempts, retained, retry_in)) = retry else {
        return;
    };
    tracing::warn!(
        target: "magician::metrics::memory_temperature",
        path = %path.display(),
        error = %error,
        failed_attempts,
        retained,
        retry_in = ?retry_in,
        "memory temperature counter flush failed; pending counter updates retained"
    );
    if let Some(delay) = retry_in {
        schedule_counter_flush(path, delay);
    }
}

/// Write the pending counter updates of `storage`'s overlay in one write. The
/// caller holds the overlay write lock. Returns how many updates the write
/// persisted (0 when none were pending); on error they stay pending.
async fn flush_pending_counter_writes_locked(
    storage: &dyn MemoryStorage,
) -> Result<usize, MemoryStorageError> {
    let path = memory_temperature_overlay_path(storage);
    let pending = lock_pending_counter_writes()
        .get(&path)
        .map(PendingCounterWrites::pending);
    let Some(pending) = pending else {
        return Ok(0);
    };
    let overlay = load_overlay_base_locked(storage).await?.overlay;
    let (overlay, bytes) = run_overlay_cpu_work(move || {
        let bytes = serialize_memory_temperature_overlay(&overlay);
        (overlay, bytes)
    })
    .await?;
    write_memory_temperature_overlay_bytes(storage, overlay, bytes?).await?;
    Ok(pending)
}

/// Persist the pending counter updates of one overlay now, in one write.
/// Returns how many updates were written (0 when none were pending). On error
/// the updates stay pending for the next attempt.
pub async fn flush_pending_memory_temperature_writes_for(
    storage: &dyn MemoryStorage,
) -> Result<usize, MemoryStorageError> {
    let _overlay_guard = lock_overlay_for_write("counter_flush").await;
    flush_pending_counter_writes_locked(storage).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureCounterFlushSummary {
    /// Overlays written.
    pub flushed_overlays: usize,
    /// Counter updates those writes persisted.
    pub flushed_mutations: usize,
    /// Overlays whose write failed; their updates stay pending.
    pub failed_overlays: usize,
    /// Counter updates still pending after the failed writes.
    pub retained_mutations: usize,
}

/// Persist every pending counter update in this process, one write per
/// overlay. Graceful shutdown calls this once counter producers have stopped.
/// Failures are logged and their updates stay pending.
pub async fn flush_pending_memory_temperature_writes() -> MemoryTemperatureCounterFlushSummary {
    let targets = lock_pending_counter_writes()
        .iter()
        .map(|(path, entry)| (path.clone(), Arc::clone(&entry.storage)))
        .collect::<Vec<_>>();
    let mut summary = MemoryTemperatureCounterFlushSummary::default();
    for (path, storage) in targets {
        let _overlay_guard = lock_overlay_for_write("counter_flush").await;
        match flush_pending_counter_writes_locked(storage.as_ref()).await {
            Ok(0) => {},
            Ok(flushed) => {
                summary.flushed_overlays += 1;
                summary.flushed_mutations += flushed;
            },
            Err(error) => {
                let retained = lock_pending_counter_writes()
                    .get(&path)
                    .map_or(0, PendingCounterWrites::pending);
                summary.failed_overlays += 1;
                summary.retained_mutations += retained;
                tracing::warn!(
                    target: "magician::metrics::memory_temperature",
                    path = %path.display(),
                    error = %error,
                    retained,
                    "memory temperature counter flush failed; pending counter updates retained"
                );
            },
        }
    }
    summary
}

/// Ensure every current candidate has a durable overlay entry. Existing
/// entries keep their temperature score/tier; only structural metadata is
/// refreshed.
///
/// This path **never removes** an entry, because its callers routinely hold a
/// subset of the scope's candidates (one prompt render, one `search_memory`
/// result) and absence here means "not in this batch", not "deleted". Bounded
/// retention lives in [`resync_memory_temperature_overlay_full_scope`], which
/// is handed the complete candidate set.
pub async fn sync_memory_temperature_overlay(
    storage: &dyn MemoryStorage,
    candidates: &[MemoryCandidateDocument],
) -> Result<MemoryTemperatureOverlay, MemoryStorageError> {
    let overlay = sync_memory_temperature_overlay_shared(storage, Arc::from(candidates)).await?;
    owned_memory_temperature_overlay(overlay).await
}

/// [`sync_memory_temperature_overlay`] for callers that already share their
/// candidates and only need the committed snapshot: no candidate copy and no
/// owned overlay clone. The whole cycle's CPU work runs on the blocking pool.
pub async fn sync_memory_temperature_overlay_shared(
    storage: &dyn MemoryStorage,
    candidates: Arc<[MemoryCandidateDocument]>,
) -> Result<Arc<MemoryTemperatureOverlay>, MemoryStorageError> {
    let ((), overlay) = mutate_memory_temperature_overlay(storage, "sync", move |overlay| {
        // Rename any pre-v6 key this candidate set can match before syncing,
        // so an un-migrated overlay does not accumulate a second entry per
        // candidate under the new encoding and lose its history. Renaming
        // only; nothing is dropped here, because this caller may hold a
        // partial candidate set.
        let mut changed = migrate_memory_temperature_overlay_keys(overlay, &candidates) > 0;
        if overlay.schema_version != MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION {
            overlay.schema_version = MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION;
            changed = true;
        }

        let now = Utc::now();
        changed |= apply_memory_temperature_overlay_sync(overlay, &candidates, now);

        let maintenance_summary = apply_memory_temperature_maintenance(overlay, now);
        let changed = changed || maintenance_summary.changed > 0;
        if changed {
            overlay.updated_at = now;
        }
        ((), changed)
    })
    .await?;
    Ok(overlay)
}

/// Refresh the structural, candidate-derived fields of every overlay entry,
/// creating entries that do not exist yet. Returns whether anything changed.
fn apply_memory_temperature_overlay_sync(
    overlay: &mut MemoryTemperatureOverlay,
    candidates: &[MemoryCandidateDocument],
    now: DateTime<Utc>,
) -> bool {
    let mut changed = false;
    for candidate in candidates {
        let key = memory_temperature_candidate_key(candidate);
        let source_ids = source_ids_for_candidate(candidate);
        match overlay.entries.get_mut(&key) {
            Some(entry) => {
                // Backfill the retention anchor for entries written before the
                // field existed — which, after the v6 key migration, is every
                // pre-existing entry: migration preserves counters but stamps
                // no `first_seen_at`. Without this the compaction TTL is
                // silently inert for the whole population, because a `None`
                // anchor is treated as "older than any TTL".
                //
                // Anchored at first sight under the current code rather than at
                // `updated_at`, which maintenance refreshes continuously. Only
                // entries with a live candidate reach this path, so genuine
                // orphans keep their `None` and stay immediately collectable —
                // which is the intended behaviour for something that describes
                // nothing and evidences nothing.
                if entry.first_seen_at.is_none() {
                    entry.first_seen_at = Some(now);
                    changed = true;
                }
                if entry.semantic_memory_type != candidate.semantic_memory_type {
                    entry.semantic_memory_type = candidate.semantic_memory_type;
                    entry.updated_at = now;
                    changed = true;
                }
                if entry.confidence != candidate.confidence {
                    entry.confidence = candidate.confidence;
                    entry.updated_at = now;
                    changed = true;
                }
                if entry.source_ids != source_ids {
                    entry.source_ids = source_ids;
                    entry.updated_at = now;
                    changed = true;
                }
                let superseded_by = superseded_by_for_candidate(candidate);
                let superseded_at = superseded_at_for_candidate(candidate);
                let supersession_reason = supersession_reason_for_candidate(candidate);
                let supersession_confidence = supersession_confidence_for_candidate(candidate);
                let supersession_source = supersession_source_for_candidate(candidate);
                if entry.superseded_by != superseded_by
                    || entry.superseded_at != superseded_at
                    || entry.supersession_reason != supersession_reason
                    || entry.supersession_confidence != supersession_confidence
                    || entry.supersession_source != supersession_source
                {
                    entry.superseded_by = superseded_by;
                    entry.superseded_at = superseded_at;
                    entry.supersession_reason = supersession_reason;
                    entry.supersession_confidence = supersession_confidence;
                    entry.supersession_source = supersession_source;
                    entry.updated_at = now;
                    changed = true;
                }
            },
            None => {
                overlay.entries.insert(
                    key.clone(),
                    MemoryTemperatureEntry::from_candidate(key, candidate, now),
                );
                changed = true;
            },
        }
    }
    changed
}

/// Return whether an immutable overlay has every prompt-affecting candidate
/// field that a synchronous overlay sync would establish.
///
/// Source-id changes are audit metadata and do not affect prompt ranking. The
/// fields checked here can change semantic partitioning, temperature scoring,
/// or supersession filtering, so any mismatch must take the canonical sync
/// path before rendering.
pub fn memory_temperature_overlay_is_prompt_current(
    overlay: &MemoryTemperatureOverlay,
    candidates: &[MemoryCandidateDocument],
) -> bool {
    if overlay.schema_version != MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION {
        return false;
    }
    candidates.iter().all(|candidate| {
        let key = memory_temperature_candidate_key(candidate);
        overlay
            .entries
            .get(&key)
            .is_some_and(|entry| memory_temperature_entry_is_prompt_current(entry, candidate))
    })
}

pub fn memory_temperature_entry_is_prompt_current(
    entry: &MemoryTemperatureEntry,
    candidate: &MemoryCandidateDocument,
) -> bool {
    entry.semantic_memory_type == candidate.semantic_memory_type
        && entry.confidence == candidate.confidence
        && entry.superseded_by == superseded_by_for_candidate(candidate)
        && entry.superseded_at == superseded_at_for_candidate(candidate)
        && entry.supersession_reason == supersession_reason_for_candidate(candidate)
        && entry.supersession_confidence == supersession_confidence_for_candidate(candidate)
        && entry.supersession_source == supersession_source_for_candidate(candidate)
}

impl MemoryTemperatureEntry {
    pub fn from_candidate(
        memory_candidate_key: String,
        candidate: &MemoryCandidateDocument,
        now: DateTime<Utc>,
    ) -> Self {
        // Temperature is a derived overlay. Package metadata cannot assign a
        // tier or force prompt inclusion, including for app-sourced envelopes.
        Self {
            memory_candidate_key,
            semantic_memory_type: candidate.semantic_memory_type,
            temperature_tier: initial_temperature_tier(candidate.semantic_memory_type),
            temperature_score: default_temperature_score(candidate.semantic_memory_type),
            confidence: candidate.confidence,
            last_retrieved_at: None,
            last_selected_at: None,
            last_injected_at: None,
            last_used_at: None,
            retrieved_count: 0,
            selected_count: 0,
            injected_count: 0,
            successful_use_count: 0,
            failed_use_count: 0,
            reviewed_referenced_count: 0,
            reviewed_useful_count: 0,
            reviewed_load_bearing_count: 0,
            reviewed_irrelevant_count: 0,
            reviewed_stale_count: 0,
            reviewed_harmful_count: 0,
            source_ids: source_ids_for_candidate(candidate),
            superseded_by: superseded_by_for_candidate(candidate),
            superseded_at: superseded_at_for_candidate(candidate),
            supersession_reason: supersession_reason_for_candidate(candidate),
            supersession_confidence: supersession_confidence_for_candidate(candidate),
            supersession_source: supersession_source_for_candidate(candidate),
            supersedes: Vec::new(),
            first_seen_at: Some(now),
            last_temperature_review_at: None,
            last_temperature_change_reason: None,
            last_utility_review_at: None,
            last_utility_review_run_id: None,
            last_utility_review_label: None,
            last_utility_review_confidence: None,
            last_utility_review_reason: None,
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MemoryTemperatureUsageSummary {
    pub retrieved: usize,
    pub selected: usize,
    pub injected: usize,
    pub missing: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryTemperatureOutcomeSignal {
    Successful,
    Failed,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureOutcomeUsageSummary {
    pub used: usize,
    pub successful: usize,
    pub failed: usize,
    pub neutral: usize,
    pub missing: usize,
    pub maintenance: MemoryTemperatureMaintenanceSummary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityReviewJudgement {
    pub memory_candidate_key: String,
    pub label: MemoryTemperatureUtilityLabel,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub compact_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryTemperatureSupersession {
    pub superseded_candidate_key: String,
    pub superseded_by_candidate_key: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub source: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureUtilityReviewSummary {
    pub reviewed: usize,
    pub referenced: usize,
    pub useful: usize,
    pub load_bearing: usize,
    pub irrelevant: usize,
    pub stale: usize,
    pub harmful: usize,
    pub unknown: usize,
    pub missing: usize,
    pub maintenance: MemoryTemperatureMaintenanceSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureSupersessionSummary {
    pub reviewed: usize,
    pub superseded: usize,
    pub already_current: usize,
    pub missing_superseded: usize,
    pub missing_successor: usize,
    pub skipped: usize,
    pub maintenance: MemoryTemperatureMaintenanceSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct MemoryTemperatureMaintenanceSummary {
    pub reviewed: usize,
    pub changed: usize,
    pub promoted: usize,
    pub demoted: usize,
}

#[derive(Debug, Clone, Copy)]
struct MemoryTemperatureLanePolicy {
    hot_percent: usize,
    active_percent: usize,
    warm_percent: usize,
    max_hot: usize,
    max_active: usize,
    max_warm: usize,
    hot_min_score: f64,
    active_min_score: f64,
    warm_min_score: f64,
    allow_hot: bool,
}

pub async fn record_memory_temperature_prompt_usage(
    storage: &dyn MemoryStorage,
    retrieved_candidate_keys: &[String],
    selected_candidate_keys: &BTreeSet<String>,
) -> Result<MemoryTemperatureUsageSummary, MemoryStorageError> {
    // Counter bookkeeping: write-behind, see `record_memory_temperature_counter_mutation`.
    let mutation = PendingCounterMutation::PromptUsage {
        retrieved_candidate_keys: Arc::from(retrieved_candidate_keys),
        selected_candidate_keys: Arc::new(selected_candidate_keys.clone()),
        at: Utc::now(),
    };
    match record_memory_temperature_counter_mutation(storage, "prompt_usage", mutation).await? {
        CounterMutationSummary::PromptUsage(summary) => Ok(summary),
        CounterMutationSummary::OutcomeUsage(_) => {
            unreachable!("a prompt-usage update yields a prompt-usage summary")
        },
    }
}

pub async fn record_memory_temperature_retrieval_usage(
    storage: &dyn MemoryStorage,
    retrieved_candidate_keys: &[String],
) -> Result<MemoryTemperatureUsageSummary, MemoryStorageError> {
    record_memory_temperature_prompt_usage(storage, retrieved_candidate_keys, &BTreeSet::new())
        .await
}

pub async fn record_memory_temperature_outcome_usage(
    storage: &dyn MemoryStorage,
    memory_candidate_keys: &[String],
    signal: MemoryTemperatureOutcomeSignal,
) -> Result<MemoryTemperatureOutcomeUsageSummary, MemoryStorageError> {
    // Counter bookkeeping: write-behind, see `record_memory_temperature_counter_mutation`.
    let mutation = PendingCounterMutation::OutcomeUsage {
        memory_candidate_keys: Arc::from(memory_candidate_keys),
        signal,
        at: Utc::now(),
    };
    match record_memory_temperature_counter_mutation(storage, "outcome_usage", mutation).await? {
        CounterMutationSummary::OutcomeUsage(summary) => Ok(summary),
        CounterMutationSummary::PromptUsage(_) => {
            unreachable!("an outcome-usage update yields an outcome-usage summary")
        },
    }
}

pub async fn record_memory_temperature_utility_review(
    storage: &dyn MemoryStorage,
    review_run_id: &str,
    judgements: &[MemoryTemperatureUtilityReviewJudgement],
) -> Result<MemoryTemperatureUtilityReviewSummary, MemoryStorageError> {
    let review_run_id = review_run_id.to_string();
    let judgements = judgements.to_vec();
    // Written synchronously before returning: the queue completes the review
    // lease only after this call, and the applied-run ledger is what makes a
    // replay after a crash idempotent.
    let (summary, _) =
        mutate_memory_temperature_overlay(storage, "utility_review", move |overlay| {
            overlay.schema_version = MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION;
            let now = Utc::now();
            let normalized_run_id = review_run_id.trim();
            let already_applied = !normalized_run_id.is_empty()
                && overlay
                    .applied_utility_review_runs
                    .contains_key(normalized_run_id);
            let mut summary =
                apply_memory_temperature_utility_review(overlay, &review_run_id, &judgements, now);
            summary.maintenance = apply_memory_temperature_maintenance(overlay, now);
            let changed = (!normalized_run_id.is_empty() && !already_applied)
                || summary.reviewed > 0
                || summary.maintenance.changed > 0;
            (summary, changed)
        })
        .await?;
    Ok(summary)
}

/// Whether a durable utility-review run has already committed its primary
/// temperature-overlay effects. Queue drainers use this after lease recovery
/// to settle an at-least-once replay without issuing another model call.
pub async fn memory_temperature_utility_review_was_applied(
    storage: &dyn MemoryStorage,
    review_run_id: &str,
) -> Result<bool, MemoryStorageError> {
    let review_run_id = review_run_id.trim();
    if review_run_id.is_empty() {
        return Ok(false);
    }
    let overlay = load_memory_temperature_overlay_snapshot(storage).await?;
    Ok(overlay
        .applied_utility_review_runs
        .contains_key(review_run_id))
}

pub async fn record_memory_temperature_supersessions(
    storage: &dyn MemoryStorage,
    supersessions: &[MemoryTemperatureSupersession],
) -> Result<MemoryTemperatureSupersessionSummary, MemoryStorageError> {
    let supersessions = supersessions.to_vec();
    let (summary, _) =
        mutate_memory_temperature_overlay(storage, "supersessions", move |overlay| {
            let now = Utc::now();
            let mut summary = apply_memory_temperature_supersessions(overlay, &supersessions, now);
            summary.maintenance = apply_memory_temperature_maintenance(overlay, now);
            let changed = summary.superseded > 0 || summary.maintenance.changed > 0;
            (summary, changed)
        })
        .await?;
    Ok(summary)
}

pub fn apply_memory_temperature_prompt_usage(
    overlay: &mut MemoryTemperatureOverlay,
    retrieved_candidate_keys: &[String],
    selected_candidate_keys: &BTreeSet<String>,
    now: DateTime<Utc>,
) -> MemoryTemperatureUsageSummary {
    let mut summary = MemoryTemperatureUsageSummary::default();
    let retrieved_unique = retrieved_candidate_keys
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    for key in retrieved_unique {
        let Some(entry) = overlay.entries.get_mut(&key) else {
            summary.missing += 1;
            continue;
        };
        entry.last_retrieved_at = Some(now);
        entry.retrieved_count = entry.retrieved_count.saturating_add(1);
        entry.updated_at = now;
        summary.retrieved += 1;
        if selected_candidate_keys.contains(&key) {
            entry.last_selected_at = Some(now);
            entry.last_injected_at = Some(now);
            entry.selected_count = entry.selected_count.saturating_add(1);
            entry.injected_count = entry.injected_count.saturating_add(1);
            summary.selected += 1;
            summary.injected += 1;
        }
    }

    overlay.updated_at = now;
    summary
}

pub fn apply_memory_temperature_outcome_usage(
    overlay: &mut MemoryTemperatureOverlay,
    memory_candidate_keys: &[String],
    signal: MemoryTemperatureOutcomeSignal,
    now: DateTime<Utc>,
) -> MemoryTemperatureOutcomeUsageSummary {
    let mut summary = MemoryTemperatureOutcomeUsageSummary::default();
    let unique_keys = memory_candidate_keys
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();

    for key in unique_keys {
        let Some(entry) = overlay.entries.get_mut(&key) else {
            summary.missing += 1;
            continue;
        };
        entry.last_used_at = Some(now);
        entry.updated_at = now;
        summary.used += 1;
        match signal {
            MemoryTemperatureOutcomeSignal::Successful => {
                entry.successful_use_count = entry.successful_use_count.saturating_add(1);
                summary.successful += 1;
            },
            MemoryTemperatureOutcomeSignal::Failed => {
                entry.failed_use_count = entry.failed_use_count.saturating_add(1);
                summary.failed += 1;
            },
            MemoryTemperatureOutcomeSignal::Neutral => {
                summary.neutral += 1;
            },
        }
    }

    if summary.used > 0 {
        overlay.updated_at = now;
    }
    summary
}

pub fn apply_memory_temperature_utility_review(
    overlay: &mut MemoryTemperatureOverlay,
    review_run_id: &str,
    judgements: &[MemoryTemperatureUtilityReviewJudgement],
    now: DateTime<Utc>,
) -> MemoryTemperatureUtilityReviewSummary {
    let mut summary = MemoryTemperatureUtilityReviewSummary::default();
    let review_run_id = review_run_id.trim();
    if !review_run_id.is_empty()
        && overlay
            .applied_utility_review_runs
            .contains_key(review_run_id)
    {
        return summary;
    }
    let mut seen = BTreeSet::<String>::new();

    for judgement in judgements {
        let key = judgement.memory_candidate_key.trim();
        if key.is_empty() || !seen.insert(key.to_string()) {
            continue;
        }
        let Some(entry) = overlay.entries.get_mut(key) else {
            summary.missing += 1;
            continue;
        };

        entry.last_utility_review_at = Some(now);
        entry.last_utility_review_run_id = Some(review_run_id.to_string());
        entry.last_utility_review_label = Some(judgement.label);
        entry.last_utility_review_confidence =
            judgement.confidence.map(|value| value.clamp(0.0, 1.0));
        entry.last_utility_review_reason = judgement
            .reason
            .as_deref()
            .map(|reason| truncate_temperature_reason(reason, 1_000));
        entry.updated_at = now;
        summary.reviewed += 1;

        match judgement.label {
            MemoryTemperatureUtilityLabel::Referenced => {
                entry.last_used_at = Some(now);
                entry.reviewed_referenced_count = entry.reviewed_referenced_count.saturating_add(1);
                summary.referenced += 1;
            },
            MemoryTemperatureUtilityLabel::Useful => {
                entry.last_used_at = Some(now);
                entry.reviewed_useful_count = entry.reviewed_useful_count.saturating_add(1);
                entry.successful_use_count = entry.successful_use_count.saturating_add(1);
                summary.useful += 1;
            },
            MemoryTemperatureUtilityLabel::LoadBearing => {
                entry.last_used_at = Some(now);
                entry.reviewed_load_bearing_count =
                    entry.reviewed_load_bearing_count.saturating_add(1);
                entry.successful_use_count = entry.successful_use_count.saturating_add(2);
                summary.load_bearing += 1;
            },
            MemoryTemperatureUtilityLabel::Irrelevant => {
                entry.reviewed_irrelevant_count = entry.reviewed_irrelevant_count.saturating_add(1);
                entry.failed_use_count = entry.failed_use_count.saturating_add(1);
                summary.irrelevant += 1;
            },
            MemoryTemperatureUtilityLabel::Stale => {
                entry.reviewed_stale_count = entry.reviewed_stale_count.saturating_add(1);
                entry.failed_use_count = entry.failed_use_count.saturating_add(3);
                summary.stale += 1;
            },
            MemoryTemperatureUtilityLabel::Harmful => {
                entry.reviewed_harmful_count = entry.reviewed_harmful_count.saturating_add(1);
                entry.failed_use_count = entry.failed_use_count.saturating_add(4);
                summary.harmful += 1;
            },
            MemoryTemperatureUtilityLabel::Unknown => {
                summary.unknown += 1;
            },
        }
    }

    if !review_run_id.is_empty() {
        overlay
            .applied_utility_review_runs
            .insert(review_run_id.to_string(), now);
        overlay.updated_at = now;
        if overlay.applied_utility_review_runs.len() > MAX_APPLIED_UTILITY_REVIEW_RUNS {
            let mut oldest = overlay
                .applied_utility_review_runs
                .iter()
                .map(|(run_id, applied_at)| (run_id.clone(), *applied_at))
                .collect::<Vec<_>>();
            oldest.sort_by(|left, right| left.1.cmp(&right.1).then_with(|| left.0.cmp(&right.0)));
            let remove_count = overlay
                .applied_utility_review_runs
                .len()
                .saturating_sub(MAX_APPLIED_UTILITY_REVIEW_RUNS);
            for (run_id, _) in oldest.into_iter().take(remove_count) {
                overlay.applied_utility_review_runs.remove(&run_id);
            }
        }
    }

    if summary.reviewed > 0 {
        overlay.updated_at = now;
    }
    summary
}

pub fn apply_memory_temperature_supersessions(
    overlay: &mut MemoryTemperatureOverlay,
    supersessions: &[MemoryTemperatureSupersession],
    now: DateTime<Utc>,
) -> MemoryTemperatureSupersessionSummary {
    let mut summary = MemoryTemperatureSupersessionSummary::default();
    let mut seen = BTreeSet::<(String, String)>::new();

    for supersession in supersessions {
        let superseded_key = supersession.superseded_candidate_key.trim();
        let successor_key = supersession.superseded_by_candidate_key.trim();
        if superseded_key.is_empty()
            || successor_key.is_empty()
            || superseded_key == successor_key
            || !seen.insert((superseded_key.to_string(), successor_key.to_string()))
        {
            summary.skipped += 1;
            continue;
        }
        summary.reviewed += 1;
        if !overlay.entries.contains_key(superseded_key) {
            summary.missing_superseded += 1;
            continue;
        }
        if !overlay.entries.contains_key(successor_key) {
            summary.missing_successor += 1;
            continue;
        }

        let reason = supersession
            .reason
            .as_deref()
            .map(|reason| truncate_temperature_reason(reason, 1_000));
        let confidence = supersession.confidence.map(|value| value.clamp(0.0, 1.0));
        let source = supersession
            .source
            .as_deref()
            .map(|source| truncate_temperature_reason(source, 240));

        let mut changed = false;
        if let Some(entry) = overlay.entries.get_mut(superseded_key) {
            if entry.superseded_by.as_deref() == Some(successor_key)
                && entry.supersession_reason == reason
                && entry.supersession_confidence == confidence
                && entry.supersession_source == source
            {
                summary.already_current += 1;
            } else {
                entry.superseded_by = Some(successor_key.to_string());
                entry.superseded_at = Some(now);
                entry.supersession_reason = reason;
                entry.supersession_confidence = confidence;
                entry.supersession_source = source;
                entry.temperature_tier = MemoryTemperatureTier::T3;
                entry.temperature_score = 0.0;
                entry.last_temperature_review_at = Some(now);
                entry.last_temperature_change_reason =
                    Some(supersession_change_reason(entry, successor_key));
                entry.updated_at = now;
                changed = true;
            }
        }
        if let Some(successor) = overlay.entries.get_mut(successor_key) {
            if !successor
                .supersedes
                .iter()
                .any(|existing| existing == superseded_key)
            {
                successor.supersedes.push(superseded_key.to_string());
                successor.supersedes.sort();
                successor.supersedes.dedup();
                successor.updated_at = now;
                changed = true;
            }
        }

        if changed {
            summary.superseded += 1;
        }
    }

    if summary.superseded > 0 {
        overlay.updated_at = now;
    }
    summary
}

pub async fn maintain_memory_temperature_overlay(
    storage: &dyn MemoryStorage,
) -> Result<MemoryTemperatureMaintenanceSummary, MemoryStorageError> {
    let (summary, _) = mutate_memory_temperature_overlay(storage, "maintenance", |overlay| {
        let summary = apply_memory_temperature_maintenance(overlay, Utc::now());
        let changed = summary.changed > 0;
        (summary, changed)
    })
    .await?;
    Ok(summary)
}

pub fn apply_memory_temperature_maintenance(
    overlay: &mut MemoryTemperatureOverlay,
    now: DateTime<Utc>,
) -> MemoryTemperatureMaintenanceSummary {
    let mut lane_groups: BTreeMap<(String, SemanticMemoryType), Vec<String>> = BTreeMap::new();
    for (key, entry) in &overlay.entries {
        lane_groups
            .entry((
                memory_temperature_scope_partition(key).into_owned(),
                entry.semantic_memory_type,
            ))
            .or_default()
            .push(key.clone());
    }

    let mut summary = MemoryTemperatureMaintenanceSummary::default();
    for ((_scope_partition, semantic_memory_type), mut keys) in lane_groups {
        let policy = lane_policy(semantic_memory_type);
        let total = keys.len();
        let hot_cap = percentage_cap(total, policy.hot_percent, policy.max_hot);
        let active_cap = percentage_cap(total, policy.active_percent, policy.max_active);
        let warm_cap = percentage_cap(total, policy.warm_percent, policy.max_warm);
        let mut score_changed_keys = BTreeSet::new();

        for key in &keys {
            if let Some(entry) = overlay.entries.get_mut(key) {
                let previous_score = entry.temperature_score;
                let new_score = if memory_temperature_entry_is_superseded(entry) {
                    0.0
                } else {
                    compute_temperature_score(entry, now)
                };
                if (previous_score - new_score).abs() >= 0.001 {
                    entry.temperature_score = new_score;
                    score_changed_keys.insert(key.clone());
                }
            }
        }

        keys.sort_by(|left_key, right_key| {
            let left = overlay.entries.get(left_key).expect("left entry");
            let right = overlay.entries.get(right_key).expect("right entry");
            right
                .temperature_score
                .partial_cmp(&left.temperature_score)
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    last_temperature_activity(right, right.updated_at)
                        .cmp(&last_temperature_activity(left, left.updated_at))
                })
                .then_with(|| left_key.cmp(right_key))
        });

        let mut hot_count = 0usize;
        let mut active_count = 0usize;
        let mut warm_count = 0usize;
        for (rank, key) in keys.iter().enumerate() {
            let Some(entry) = overlay.entries.get_mut(key) else {
                continue;
            };
            let previous_tier = entry.temperature_tier;
            let score_changed = score_changed_keys.contains(key);
            let superseded = memory_temperature_entry_is_superseded(entry);
            let next_tier = if superseded {
                MemoryTemperatureTier::T3
            } else {
                desired_temperature_tier(
                    entry,
                    &policy,
                    hot_cap,
                    active_cap,
                    warm_cap,
                    &mut hot_count,
                    &mut active_count,
                    &mut warm_count,
                )
            };

            summary.reviewed += 1;
            if next_tier != previous_tier || score_changed {
                entry.temperature_tier = next_tier;
                entry.last_temperature_review_at = Some(now);
                entry.updated_at = now;
                entry.last_temperature_change_reason = if superseded {
                    Some(supersession_change_reason(
                        entry,
                        entry.superseded_by.as_deref().unwrap_or("unknown"),
                    ))
                } else {
                    Some(temperature_change_reason(
                        entry,
                        rank,
                        previous_tier,
                        next_tier,
                    ))
                };
                summary.changed += 1;
                match tier_distance(previous_tier).cmp(&tier_distance(next_tier)) {
                    Ordering::Greater => summary.promoted += 1,
                    Ordering::Less => summary.demoted += 1,
                    Ordering::Equal => {},
                }
            }
        }
    }

    if summary.changed > 0 {
        overlay.updated_at = now;
    }
    summary
}

/// Compute the temperature tiers a maintenance pass would expose to prompt
/// rendering without cloning or mutating the full overlay.
///
/// This follows the same partitioning, score, ordering, capacity, and
/// supersession rules as [`apply_memory_temperature_maintenance`]. The prompt
/// path can therefore consume an immutable cached overlay and retain identical
/// tier decisions while durable maintenance remains on the write/maintainer
/// path.
pub fn memory_temperature_tiers_for_prompt(
    overlay: &MemoryTemperatureOverlay,
    now: DateTime<Utc>,
) -> BTreeMap<String, MemoryTemperatureTier> {
    let mut lane_groups: BTreeMap<(String, SemanticMemoryType), Vec<&String>> = BTreeMap::new();
    for (key, entry) in &overlay.entries {
        lane_groups
            .entry((
                memory_temperature_scope_partition(key).into_owned(),
                entry.semantic_memory_type,
            ))
            .or_default()
            .push(key);
    }

    let mut tiers = BTreeMap::new();
    for ((_scope_partition, semantic_memory_type), mut keys) in lane_groups {
        let policy = lane_policy(semantic_memory_type);
        let total = keys.len();
        let hot_cap = percentage_cap(total, policy.hot_percent, policy.max_hot);
        let active_cap = percentage_cap(total, policy.active_percent, policy.max_active);
        let warm_cap = percentage_cap(total, policy.warm_percent, policy.max_warm);
        let scores = keys
            .iter()
            .map(|key| {
                let entry = overlay.entries.get(*key).expect("temperature entry");
                let score = if memory_temperature_entry_is_superseded(entry) {
                    0.0
                } else {
                    compute_temperature_score(entry, now)
                };
                ((*key).clone(), score)
            })
            .collect::<BTreeMap<_, _>>();

        keys.sort_by(|left_key, right_key| {
            let left = overlay.entries.get(*left_key).expect("left entry");
            let right = overlay.entries.get(*right_key).expect("right entry");
            scores
                .get(*right_key)
                .expect("right score")
                .partial_cmp(scores.get(*left_key).expect("left score"))
                .unwrap_or(Ordering::Equal)
                .then_with(|| {
                    last_temperature_activity(right, right.updated_at)
                        .cmp(&last_temperature_activity(left, left.updated_at))
                })
                .then_with(|| left_key.cmp(right_key))
        });

        let mut hot_count = 0usize;
        let mut active_count = 0usize;
        let mut warm_count = 0usize;
        for key in keys {
            let entry = overlay.entries.get(key).expect("temperature entry");
            let tier = if memory_temperature_entry_is_superseded(entry) {
                MemoryTemperatureTier::T3
            } else {
                desired_temperature_tier_for_score(
                    entry,
                    *scores.get(key).expect("temperature score"),
                    &policy,
                    hot_cap,
                    active_cap,
                    warm_cap,
                    &mut hot_count,
                    &mut active_count,
                    &mut warm_count,
                )
            };
            tiers.insert(key.clone(), tier);
        }
    }
    tiers
}

/// Share the exact effective-tier preparation across concurrent scope renders.
///
/// User and agent memory are rendered concurrently for one chat turn and read
/// the same immutable overlay. The score model changes on day-scale recency
/// inputs and rounds to three decimals, so a bounded 30-second reuse window
/// preserves the effective result while preventing successive turns from
/// rebuilding and sorting the complete lane map. A new overlay `Arc` after any
/// persisted or externally observed write misses this disposable cache
/// immediately.
pub fn memory_temperature_tiers_for_prompt_snapshot(
    overlay: &Arc<MemoryTemperatureOverlay>,
    now: DateTime<Utc>,
) -> Arc<BTreeMap<String, MemoryTemperatureTier>> {
    let cache_cell = {
        let now_instant = Instant::now();
        let mut cache = prompt_tier_snapshot_cache()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        cache.retain(|entry| {
            now_instant.saturating_duration_since(entry.last_used) <= PROMPT_TIER_SNAPSHOT_CACHE_TTL
        });
        if let Some(entry) = cache.iter_mut().find(|entry| {
            now_instant.saturating_duration_since(entry.created_at) <= PROMPT_TIER_SNAPSHOT_REUSE
                && Arc::ptr_eq(&entry.overlay, overlay)
        }) {
            entry.last_used = now_instant;
            Arc::clone(&entry.tiers)
        } else {
            while cache.len() >= PROMPT_TIER_SNAPSHOT_CACHE_MAX_ENTRIES {
                let oldest = cache
                    .iter()
                    .enumerate()
                    .min_by_key(|(_, entry)| entry.last_used)
                    .map(|(index, _)| index)
                    .unwrap_or(0);
                cache.swap_remove(oldest);
            }
            let tiers = Arc::new(OnceLock::new());
            cache.push(PromptTierSnapshotCacheEntry {
                overlay: Arc::clone(overlay),
                created_at: now_instant,
                tiers: Arc::clone(&tiers),
                last_used: now_instant,
            });
            tiers
        }
    };
    Arc::clone(
        cache_cell.get_or_init(|| Arc::new(memory_temperature_tiers_for_prompt(overlay, now))),
    )
}

fn desired_temperature_tier(
    entry: &MemoryTemperatureEntry,
    policy: &MemoryTemperatureLanePolicy,
    hot_cap: usize,
    active_cap: usize,
    warm_cap: usize,
    hot_count: &mut usize,
    active_count: &mut usize,
    warm_count: &mut usize,
) -> MemoryTemperatureTier {
    desired_temperature_tier_for_score(
        entry,
        entry.temperature_score,
        policy,
        hot_cap,
        active_cap,
        warm_cap,
        hot_count,
        active_count,
        warm_count,
    )
}

#[allow(clippy::too_many_arguments)]
fn desired_temperature_tier_for_score(
    entry: &MemoryTemperatureEntry,
    temperature_score: f64,
    policy: &MemoryTemperatureLanePolicy,
    hot_cap: usize,
    active_cap: usize,
    warm_cap: usize,
    hot_count: &mut usize,
    active_count: &mut usize,
    warm_count: &mut usize,
) -> MemoryTemperatureTier {
    if policy.allow_hot
        && *hot_count < hot_cap
        && temperature_score >= policy.hot_min_score
        && has_strong_temperature_use(entry)
    {
        *hot_count += 1;
        return MemoryTemperatureTier::T0;
    }
    // T1 is the *active* working set, and the documented contract is that
    // maintenance promotes memory the agent actually used. Score alone is not
    // evidence of use: a lane's default score already clears several lanes'
    // `active_min_score`, and `percentage_cap` ceilings every non-empty lane to
    // at least one slot. Without this check, memory living in a one-entry
    // partition — one per task goal, of which there are hundreds — is handed an
    // active slot it never earned, crowding the prior against memory that
    // competed for it. Fresh memory enters at T2 and rises on first retrieval.
    if *active_count < active_cap
        && temperature_score >= policy.active_min_score
        && has_any_temperature_signal(entry)
    {
        *active_count += 1;
        return MemoryTemperatureTier::T1;
    }
    if *warm_count < warm_cap && temperature_score >= policy.warm_min_score {
        *warm_count += 1;
        return MemoryTemperatureTier::T2;
    }
    MemoryTemperatureTier::T3
}

fn compute_temperature_score(entry: &MemoryTemperatureEntry, now: DateTime<Utc>) -> f64 {
    let mut score = default_temperature_score(entry.semantic_memory_type);
    if let Some(confidence) = entry.confidence {
        score += (confidence.clamp(0.0, 1.0) - 0.5) * 0.10;
    }

    // Retrieval is intentionally weak evidence: it can keep a memory from
    // freezing, but should not promote it into T0 without selection/use.
    score += bounded_log_signal(entry.retrieved_count, 0.012, 0.07);
    score += bounded_log_signal(entry.selected_count, 0.055, 0.22);
    score += bounded_log_signal(entry.injected_count, 0.045, 0.18);
    score += bounded_log_signal(entry.successful_use_count, 0.080, 0.30);
    score -= bounded_log_signal(entry.failed_use_count, 0.100, 0.35);
    score += bounded_log_signal(entry.reviewed_useful_count, 0.090, 0.24);
    score += bounded_log_signal(entry.reviewed_load_bearing_count, 0.140, 0.34);
    score -= bounded_log_signal(entry.reviewed_irrelevant_count, 0.120, 0.30);
    score -= bounded_log_signal(
        entry
            .reviewed_stale_count
            .saturating_add(entry.reviewed_harmful_count),
        0.220,
        0.45,
    );
    score += last_utility_review_label_adjustment(entry.last_utility_review_label);
    score += recency_bonus(entry.last_selected_at, now, 0.07, 14.0);
    score += recency_bonus(entry.last_injected_at, now, 0.06, 14.0);
    score += recency_bonus(entry.last_used_at, now, 0.10, 21.0);
    score += recency_bonus(entry.last_retrieved_at, now, 0.025, 7.0);
    score -= stale_penalty(entry, now);
    score -= retrieval_only_noise_penalty(entry);
    round_temperature_score(score)
}

fn lane_policy(semantic_memory_type: SemanticMemoryType) -> MemoryTemperatureLanePolicy {
    match semantic_memory_type {
        SemanticMemoryType::UserPreference => MemoryTemperatureLanePolicy {
            hot_percent: 10,
            active_percent: 45,
            warm_percent: 35,
            max_hot: 16,
            max_active: 128,
            max_warm: 512,
            hot_min_score: 0.88,
            active_min_score: 0.50,
            warm_min_score: 0.25,
            allow_hot: true,
        },
        SemanticMemoryType::Procedure => MemoryTemperatureLanePolicy {
            hot_percent: 10,
            active_percent: 35,
            warm_percent: 35,
            max_hot: 12,
            max_active: 96,
            max_warm: 384,
            hot_min_score: 0.86,
            active_min_score: 0.55,
            warm_min_score: 0.25,
            allow_hot: true,
        },
        SemanticMemoryType::ProjectContext => MemoryTemperatureLanePolicy {
            hot_percent: 10,
            active_percent: 40,
            warm_percent: 40,
            max_hot: 16,
            max_active: 128,
            max_warm: 512,
            hot_min_score: 0.86,
            active_min_score: 0.55,
            warm_min_score: 0.25,
            allow_hot: true,
        },
        SemanticMemoryType::Entity => MemoryTemperatureLanePolicy {
            hot_percent: 5,
            active_percent: 25,
            warm_percent: 50,
            max_hot: 8,
            max_active: 96,
            max_warm: 512,
            hot_min_score: 0.86,
            active_min_score: 0.70,
            warm_min_score: 0.35,
            allow_hot: true,
        },
        SemanticMemoryType::Environment => MemoryTemperatureLanePolicy {
            hot_percent: 5,
            active_percent: 20,
            warm_percent: 40,
            max_hot: 4,
            max_active: 64,
            max_warm: 256,
            hot_min_score: 0.86,
            active_min_score: 0.68,
            warm_min_score: 0.35,
            allow_hot: true,
        },
        SemanticMemoryType::AgentContext => MemoryTemperatureLanePolicy {
            hot_percent: 5,
            active_percent: 25,
            warm_percent: 45,
            max_hot: 8,
            max_active: 96,
            max_warm: 384,
            hot_min_score: 0.86,
            active_min_score: 0.68,
            warm_min_score: 0.35,
            allow_hot: true,
        },
        SemanticMemoryType::Episode => MemoryTemperatureLanePolicy {
            hot_percent: 0,
            active_percent: 10,
            warm_percent: 40,
            max_hot: 0,
            max_active: 48,
            max_warm: 512,
            hot_min_score: 1.0,
            active_min_score: 0.72,
            warm_min_score: 0.35,
            allow_hot: false,
        },
        SemanticMemoryType::SourceEvidence => MemoryTemperatureLanePolicy {
            hot_percent: 0,
            active_percent: 5,
            warm_percent: 25,
            max_hot: 0,
            max_active: 24,
            max_warm: 256,
            hot_min_score: 1.0,
            active_min_score: 0.80,
            warm_min_score: 0.45,
            allow_hot: false,
        },
        SemanticMemoryType::CodeKnowledge => MemoryTemperatureLanePolicy {
            hot_percent: 8,
            active_percent: 35,
            warm_percent: 45,
            max_hot: 10,
            max_active: 96,
            max_warm: 512,
            hot_min_score: 0.86,
            active_min_score: 0.55,
            warm_min_score: 0.25,
            allow_hot: true,
        },
        SemanticMemoryType::Social => MemoryTemperatureLanePolicy {
            hot_percent: 10,
            active_percent: 40,
            warm_percent: 40,
            max_hot: 10,
            max_active: 100,
            max_warm: 500,
            hot_min_score: 0.86,
            active_min_score: 0.55,
            warm_min_score: 0.25,
            allow_hot: true,
        },
    }
}

fn percentage_cap(total: usize, percent: usize, max_cap: usize) -> usize {
    if total == 0 || percent == 0 || max_cap == 0 {
        return 0;
    }
    ((total * percent).saturating_add(99) / 100)
        .max(1)
        .min(max_cap)
        .min(total)
}

fn bounded_log_signal(count: u32, scale: f64, cap: f64) -> f64 {
    ((count as f64 + 1.0).ln() * scale).min(cap)
}

fn recency_bonus(
    instant: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
    max_bonus: f64,
    half_life_days: f64,
) -> f64 {
    let Some(instant) = instant else {
        return 0.0;
    };
    let days = days_since(now, instant);
    max_bonus / (1.0 + (days / half_life_days.max(1.0)))
}

fn stale_penalty(entry: &MemoryTemperatureEntry, now: DateTime<Utc>) -> f64 {
    let Some(last_activity) = latest_temperature_activity(entry) else {
        return 0.03;
    };
    let days = days_since(now, last_activity);
    let stale_after_days = match entry.semantic_memory_type {
        SemanticMemoryType::Environment => 7.0,
        SemanticMemoryType::AgentContext | SemanticMemoryType::Episode => 30.0,
        SemanticMemoryType::CodeKnowledge => 60.0,
        SemanticMemoryType::SourceEvidence => 180.0,
        SemanticMemoryType::UserPreference
        | SemanticMemoryType::Procedure
        | SemanticMemoryType::Entity
        | SemanticMemoryType::Social
        | SemanticMemoryType::ProjectContext => 90.0,
    };
    if days <= stale_after_days {
        0.0
    } else {
        (((days - stale_after_days) / stale_after_days) * 0.05).min(0.20)
    }
}

fn retrieval_only_noise_penalty(entry: &MemoryTemperatureEntry) -> f64 {
    if has_strong_temperature_use(entry) || entry.retrieved_count <= 3 {
        return 0.0;
    }
    ((entry.retrieved_count - 3) as f64 * 0.015).min(0.15)
}

fn last_utility_review_label_adjustment(label: Option<MemoryTemperatureUtilityLabel>) -> f64 {
    match label {
        Some(MemoryTemperatureUtilityLabel::LoadBearing) => 0.08,
        Some(MemoryTemperatureUtilityLabel::Useful) => 0.04,
        Some(MemoryTemperatureUtilityLabel::Irrelevant) => -0.10,
        Some(MemoryTemperatureUtilityLabel::Stale) => -0.20,
        Some(MemoryTemperatureUtilityLabel::Harmful) => -0.30,
        Some(
            MemoryTemperatureUtilityLabel::Referenced | MemoryTemperatureUtilityLabel::Unknown,
        )
        | None => 0.0,
    }
}

/// Any durable evidence that this memory has participated in a real turn —
/// retrieval, selection, injection, an outcome signal, a utility review, or
/// having won a supersession.
///
/// Deliberately weaker than [`has_strong_temperature_use`]: retrieval alone
/// qualifies here (it admits T1) but not there (T0 still needs selection or
/// use). `retrieval_only_noise_penalty` keeps repeated retrieval-without-use
/// from riding this into a high score.
///
/// Superseding something counts because it is a decision, not an accident: the
/// entry went through conflict review against an incumbent and replaced it. A
/// brand-new memory that just displaced the fact an agent was relying on should
/// not have to wait for its first retrieval to reach the active tier.
fn has_any_temperature_signal(entry: &MemoryTemperatureEntry) -> bool {
    has_strong_temperature_use(entry)
        || entry.retrieved_count > 0
        || entry.failed_use_count > 0
        || entry.reviewed_referenced_count > 0
        || entry.reviewed_irrelevant_count > 0
        || entry.reviewed_stale_count > 0
        || entry.reviewed_harmful_count > 0
        || entry.last_utility_review_at.is_some()
        || !entry.supersedes.is_empty()
        // A Neutral outcome stamps `last_used_at` and increments no counter
        // (`apply_memory_temperature_outcome_usage`). Without this an entry
        // whose whole history is neutral outcomes reads as never-touched, and
        // compaction treats durable proof of participation as waste.
        || entry.last_used_at.is_some()
}

fn has_strong_temperature_use(entry: &MemoryTemperatureEntry) -> bool {
    entry.selected_count > 0
        || entry.injected_count > 0
        || entry.successful_use_count > 0
        || entry.reviewed_useful_count > 0
        || entry.reviewed_load_bearing_count > 0
}

fn latest_temperature_activity(entry: &MemoryTemperatureEntry) -> Option<DateTime<Utc>> {
    [
        entry.last_used_at,
        entry.last_injected_at,
        entry.last_selected_at,
        entry.last_retrieved_at,
    ]
    .into_iter()
    .flatten()
    .max()
}

fn last_temperature_activity(
    entry: &MemoryTemperatureEntry,
    fallback: DateTime<Utc>,
) -> DateTime<Utc> {
    latest_temperature_activity(entry).unwrap_or(fallback)
}

fn days_since(now: DateTime<Utc>, instant: DateTime<Utc>) -> f64 {
    now.signed_duration_since(instant).num_seconds().max(0) as f64 / 86_400.0
}

fn round_temperature_score(score: f64) -> f64 {
    (score.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
}

fn truncate_temperature_reason(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        value.trim().to_string()
    } else {
        value.chars().take(max_chars).collect::<String>()
    }
}

fn tier_distance(tier: MemoryTemperatureTier) -> u8 {
    match tier {
        MemoryTemperatureTier::T0 => 0,
        MemoryTemperatureTier::T1 => 1,
        MemoryTemperatureTier::T2 => 2,
        MemoryTemperatureTier::T3 => 3,
    }
}

fn temperature_change_reason(
    entry: &MemoryTemperatureEntry,
    rank: usize,
    previous_tier: MemoryTemperatureTier,
    next_tier: MemoryTemperatureTier,
) -> String {
    format!(
        "score={:.3}; lane_rank={}; previous_tier={}; next_tier={}; retrieved={}; selected={}; injected={}; successful={}; failed={}",
        entry.temperature_score,
        rank + 1,
        previous_tier.as_str(),
        next_tier.as_str(),
        entry.retrieved_count,
        entry.selected_count,
        entry.injected_count,
        entry.successful_use_count,
        entry.failed_use_count
    )
}

fn supersession_change_reason(entry: &MemoryTemperatureEntry, successor_key: &str) -> String {
    let reason = entry
        .supersession_reason
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("superseded");
    format!(
        "superseded_by={}; reason={}; confidence={}",
        successor_key,
        reason,
        entry
            .supersession_confidence
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "unknown".to_string())
    )
}

/// The lane partition of a key, for health/diagnostic reporting.
///
/// Same rule the maintenance pass groups by, exposed so tier metrics measure
/// the partitioning that actually happens rather than a re-derived guess.
pub fn memory_temperature_scope_partition_for_health(memory_candidate_key: &str) -> Cow<'_, str> {
    memory_temperature_scope_partition(memory_candidate_key)
}

/// The lane partition a candidate competes inside: its retrieval universe of
/// `(scope, agent, goal)`.
///
/// Current keys are parsed exactly. Legacy keys keep the historical
/// `splitn(4, ':')` behaviour rather than being forced into one bucket, so an
/// overlay that has not yet been migrated groups exactly as it does today
/// instead of degrading at the moment of upgrade.
fn memory_temperature_scope_partition(memory_candidate_key: &str) -> Cow<'_, str> {
    // The partition is a contiguous prefix of the key in both encodings, so it
    // is borrowed rather than rebuilt. This runs once per entry inside
    // `apply_memory_temperature_maintenance`, which itself runs on every
    // prompt-usage, outcome, review, and supersession record — rebuilding it
    // with four allocations per entry was thousands of needless allocations per
    // chat turn.
    if let Some(end) = current_key_partition_end(memory_candidate_key) {
        return Cow::Borrowed(&memory_candidate_key[..end]);
    }
    let mut boundary = None;
    let mut seen = 0;
    for (index, byte) in memory_candidate_key.bytes().enumerate() {
        if byte == b':' {
            seen += 1;
            if seen == 3 {
                boundary = Some(index);
                break;
            }
        }
    }
    match boundary {
        Some(index) => Cow::Borrowed(&memory_candidate_key[..index]),
        // Fewer than three separators: the whole key is its own partition,
        // which matches what the old `splitn(4)` join produced for it.
        None => Cow::Owned(legacy_partition_join(memory_candidate_key)),
    }
}

/// Byte offset just past the `goal` segment of a current-format key, or `None`
/// if the key is not current-format or is malformed.
fn current_key_partition_end(memory_candidate_key: &str) -> Option<usize> {
    let rest = memory_candidate_key.strip_prefix(MEMORY_TEMPERATURE_CANDIDATE_KEY_PREFIX)?;
    let mut consumed = MEMORY_TEMPERATURE_CANDIDATE_KEY_PREFIX.len();
    let mut cursor = rest;
    for _ in 0..3 {
        let before = cursor.len();
        let (_, next) = parse_length_prefixed_key_segment(cursor)?;
        // `parse_length_prefixed_key_segment` consumes the segment and its
        // trailing separator when one follows.
        consumed += before - next.len();
        cursor = next;
    }
    Some(consumed)
}

/// Reproduce the pre-v6 partition string for a key with fewer than three
/// separators, where the old `splitn(4, ':')` join padded missing segments.
fn legacy_partition_join(memory_candidate_key: &str) -> String {
    let mut parts = memory_candidate_key.splitn(4, ':');
    let scope = parts.next().unwrap_or_default();
    let agent = parts.next().unwrap_or_default();
    let goal = parts.next().unwrap_or_default();
    format!("{scope}:{agent}:{goal}")
}

pub fn default_temperature_tier(semantic_memory_type: SemanticMemoryType) -> MemoryTemperatureTier {
    match semantic_memory_type {
        SemanticMemoryType::UserPreference
        | SemanticMemoryType::Procedure
        | SemanticMemoryType::ProjectContext
        | SemanticMemoryType::Social
        | SemanticMemoryType::CodeKnowledge => MemoryTemperatureTier::T1,
        SemanticMemoryType::Entity
        | SemanticMemoryType::Environment
        | SemanticMemoryType::AgentContext => MemoryTemperatureTier::T2,
        SemanticMemoryType::Episode | SemanticMemoryType::SourceEvidence => {
            MemoryTemperatureTier::T3
        },
    }
}

/// The tier a brand-new overlay entry arrives in.
///
/// Never warmer than T2. Several lanes default to T1, but a new entry has no
/// usage evidence by construction, so maintenance would demote it on its very
/// next pass — churn, plus a "demoted" count for what is really just an
/// arrival. [`default_temperature_tier`] is unchanged: it is also the prompt
/// fallback for candidates with no overlay entry at all, where a lane default
/// is the right answer.
fn initial_temperature_tier(semantic_memory_type: SemanticMemoryType) -> MemoryTemperatureTier {
    match default_temperature_tier(semantic_memory_type) {
        MemoryTemperatureTier::T0 | MemoryTemperatureTier::T1 => MemoryTemperatureTier::T2,
        colder => colder,
    }
}

pub fn default_temperature_score(semantic_memory_type: SemanticMemoryType) -> f64 {
    match semantic_memory_type {
        SemanticMemoryType::UserPreference => 0.75,
        SemanticMemoryType::Procedure => 0.68,
        SemanticMemoryType::ProjectContext => 0.64,
        SemanticMemoryType::CodeKnowledge => 0.62,
        SemanticMemoryType::Social => 0.60,
        SemanticMemoryType::Entity => 0.55,
        SemanticMemoryType::Environment => 0.48,
        SemanticMemoryType::AgentContext => 0.45,
        SemanticMemoryType::Episode => 0.30,
        SemanticMemoryType::SourceEvidence => 0.25,
    }
}

/// Prefix identifying the length-prefixed (schema v6+) candidate-key encoding.
///
/// A bare prefix check is enough to tell the two generations apart because no
/// legacy key can begin with it: legacy keys start with a scope label.
pub const MEMORY_TEMPERATURE_CANDIDATE_KEY_PREFIX: &str = "mt1:";

/// The identity of one memory candidate inside the temperature overlay.
///
/// Every segment is length-prefixed, so this is injective: goal ids carrying
/// colons (`chat:<uuid>`) or the planner's whole free-text goal, and agent ids
/// such as `system:scheduler`, can no longer shift the segment boundaries or
/// collapse two distinct candidates onto one entry. Segments are also bounded,
/// so a goal that arrives as three kilobytes of prose contributes a digest
/// rather than three kilobytes of key. See [`crate::key_encoding`].
pub fn memory_temperature_candidate_key(candidate: &MemoryCandidateDocument) -> String {
    memory_temperature_candidate_key_from_parts(
        &candidate.scope,
        candidate.agent_id.as_deref(),
        candidate.goal_id.as_deref(),
        &candidate.tier_name,
        &candidate.item_key,
    )
}

/// Build a candidate key from its parts, for callers naming a candidate they do
/// not hold.
///
/// Supersession is the reason this exists: the replacement for a superseded
/// item is always a sibling — same scope, agent, goal and tier — so its key can
/// be composed from the superseded candidate's identity plus the successor's
/// item segment, without the writer having to know any of that context.
///
/// [`memory_temperature_candidate_key`] is defined in terms of this, so a
/// composed key and a loaded candidate's key cannot encode differently.
pub fn memory_temperature_candidate_key_from_parts(
    scope: &TierScope,
    agent_id: Option<&str>,
    goal_id: Option<&str>,
    tier_name: &str,
    item_key: &str,
) -> String {
    format!(
        "{MEMORY_TEMPERATURE_CANDIDATE_KEY_PREFIX}{}:{}:{}:{}:{}",
        length_prefixed_key_segment(&bounded_key_segment(scope_label(scope))),
        length_prefixed_key_segment(&bounded_key_segment(agent_id.unwrap_or(""))),
        length_prefixed_key_segment(&bounded_key_segment(goal_id.unwrap_or(""))),
        length_prefixed_key_segment(&bounded_key_segment(tier_name)),
        length_prefixed_key_segment(&bounded_key_segment(item_key))
    )
}

/// The pre-v6 key: a bare `:` join of the same five segments.
///
/// Retained solely so migration can find an existing entry written under the
/// old encoding. Never write new keys with this.
pub fn legacy_memory_temperature_candidate_key(candidate: &MemoryCandidateDocument) -> String {
    format!(
        "{}:{}:{}:{}:{}",
        scope_label(&candidate.scope),
        candidate.agent_id.as_deref().unwrap_or(""),
        candidate.goal_id.as_deref().unwrap_or(""),
        candidate.tier_name,
        candidate.item_key
    )
}

/// The five identifier segments a candidate key is built from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryTemperatureCandidateKeyParts<'a> {
    pub scope: &'a str,
    pub agent_id: &'a str,
    pub goal_id: &'a str,
    pub tier_name: &'a str,
    pub item_key: &'a str,
}

/// Recover the segments of a length-prefixed candidate key.
///
/// Returns `None` for legacy keys and for anything malformed — callers decide
/// whether to fall back or to treat the key as unmigrated.
pub fn parse_memory_temperature_candidate_key(
    memory_candidate_key: &str,
) -> Option<MemoryTemperatureCandidateKeyParts<'_>> {
    let rest = memory_candidate_key.strip_prefix(MEMORY_TEMPERATURE_CANDIDATE_KEY_PREFIX)?;
    let (scope, rest) = parse_length_prefixed_key_segment(rest)?;
    let (agent_id, rest) = parse_length_prefixed_key_segment(rest)?;
    let (goal_id, rest) = parse_length_prefixed_key_segment(rest)?;
    let (tier_name, rest) = parse_length_prefixed_key_segment(rest)?;
    let (item_key, rest) = parse_length_prefixed_key_segment(rest)?;
    if !rest.is_empty() {
        return None;
    }
    Some(MemoryTemperatureCandidateKeyParts {
        scope,
        agent_id,
        goal_id,
        tier_name,
        item_key,
    })
}

/// Whether a key uses the current length-prefixed encoding.
pub fn memory_temperature_candidate_key_is_current(memory_candidate_key: &str) -> bool {
    parse_memory_temperature_candidate_key(memory_candidate_key).is_some()
}

pub fn memory_temperature_entry_is_superseded(entry: &MemoryTemperatureEntry) -> bool {
    entry
        .superseded_by
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
}

pub fn memory_candidate_has_superseded_lifecycle(candidate: &MemoryCandidateDocument) -> bool {
    candidate_metadata_has_superseded_lifecycle(&candidate.metadata_json)
}

/// Canonical lifecycle gate for both indexed documents and source-entry readers.
/// Keeping raw source adapters on this predicate prevents retired entries with
/// unchanged text from being treated as current activity.
pub fn candidate_metadata_has_superseded_lifecycle(metadata: &Value) -> bool {
    metadata
        .get("memory_lifecycle")
        .or_else(|| metadata.get("lifecycle"))
        .or_else(|| metadata.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "superseded" | "replaced" | "retracted" | "expired" | "pending_review"
            )
        })
        || metadata
            .get("valid_until")
            .and_then(Value::as_str)
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .is_some_and(|until| until <= chrono::Utc::now())
}

/// Metadata field naming the *item* that replaced a superseded one.
///
/// Written by the consolidator, which knows the replacement item but not the
/// scope/agent/goal/tier it lives under. The reader supplies that context, so
/// the two halves of a supersession never have to agree on more than the item.
pub const SUPERSEDED_BY_ITEM_KEY_METADATA_KEY: &str = "superseded_by_item_key";

/// Metadata field naming the replacement as a whole candidate key, for writers
/// that do hold the full identity.
pub const SUPERSEDED_BY_CANDIDATE_KEY_METADATA_KEY: &str = "superseded_by_candidate_key";

/// Resolve the candidate key of whatever replaced this candidate.
///
/// Three sources, most specific first:
///
/// 1. `superseded_by_item_key` — the replacement's item segment, composed here
///    against this candidate's own scope/agent/goal/tier. The successor of a
///    superseded item is always a sibling in the same collection, so that
///    context is the right context.
/// 2. `superseded_by_candidate_key` — an already-whole candidate key.
/// 3. `superseded_by` / `superseded_by_key` — legacy. The consolidator writes
///    its own item-matching key (`durable::…`, `similarity::…`, `json_hash::…`)
///    into `superseded_by`, which is a different namespace and will not resolve
///    against the overlay. Kept as a last resort so historical records read
///    exactly as they do today rather than disappearing.
fn superseded_by_for_candidate(candidate: &MemoryCandidateDocument) -> Option<String> {
    if !memory_candidate_has_superseded_lifecycle(candidate) {
        return None;
    }
    if let Some(item_key) = candidate
        .metadata_json
        .get(SUPERSEDED_BY_ITEM_KEY_METADATA_KEY)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Some(memory_temperature_candidate_key_from_parts(
            &candidate.scope,
            candidate.agent_id.as_deref(),
            candidate.goal_id.as_deref(),
            &candidate.tier_name,
            item_key,
        ));
    }
    candidate
        .metadata_json
        .get(SUPERSEDED_BY_CANDIDATE_KEY_METADATA_KEY)
        .or_else(|| candidate.metadata_json.get("superseded_by"))
        .or_else(|| candidate.metadata_json.get("superseded_by_key"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn superseded_at_for_candidate(candidate: &MemoryCandidateDocument) -> Option<DateTime<Utc>> {
    if !memory_candidate_has_superseded_lifecycle(candidate) {
        return None;
    }
    candidate
        .metadata_json
        .get("superseded_at")
        .and_then(Value::as_str)
        .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&Utc))
}

fn supersession_reason_for_candidate(candidate: &MemoryCandidateDocument) -> Option<String> {
    if !memory_candidate_has_superseded_lifecycle(candidate) {
        return None;
    }
    candidate
        .metadata_json
        .get("supersession_reason")
        .or_else(|| candidate.metadata_json.get("superseded_reason"))
        .and_then(Value::as_str)
        .map(|value| truncate_temperature_reason(value, 1_000))
}

fn supersession_confidence_for_candidate(candidate: &MemoryCandidateDocument) -> Option<f64> {
    if !memory_candidate_has_superseded_lifecycle(candidate) {
        return None;
    }
    candidate
        .metadata_json
        .get("supersession_confidence")
        .and_then(Value::as_f64)
        .map(|value| value.clamp(0.0, 1.0))
}

fn supersession_source_for_candidate(candidate: &MemoryCandidateDocument) -> Option<String> {
    if !memory_candidate_has_superseded_lifecycle(candidate) {
        return None;
    }
    candidate
        .metadata_json
        .get("supersession_source")
        .and_then(Value::as_str)
        .map(|value| truncate_temperature_reason(value, 240))
}

fn source_ids_for_candidate(candidate: &MemoryCandidateDocument) -> Vec<String> {
    let mut source_ids = Vec::new();
    if let Some(path) = candidate.source_path.as_ref() {
        let mut source = path.display().to_string();
        if !candidate.json_pointer.is_empty() {
            source.push('#');
            source.push_str(&candidate.json_pointer);
        }
        source_ids.push(source);
    }
    if let Some(source_ids_value) = candidate.metadata_json.get("source_ids") {
        push_json_source_ids(source_ids_value, &mut source_ids);
    }
    source_ids.sort();
    source_ids.dedup();
    source_ids
}

fn push_json_source_ids(value: &Value, source_ids: &mut Vec<String>) {
    match value {
        Value::String(value) if !value.trim().is_empty() => {
            source_ids.push(value.trim().to_string());
        },
        Value::Array(values) => {
            for value in values {
                push_json_source_ids(value, source_ids);
            }
        },
        _ => {},
    }
}

fn scope_label(scope: &TierScope) -> &'static str {
    match scope {
        TierScope::User => "user",
        TierScope::Agent => "agent",
        TierScope::AgentGoal => "agent_goal",
    }
}

/// Bounded rolling record of what compaction dropped and why.
pub const MEMORY_TEMPERATURE_COMPACTION_AUDIT_FILE: &str = "temperature_compaction_audit.json";

/// Retention bounds for the temperature overlay.
///
/// The overlay was originally append-only on purpose — deleted and superseded
/// memory stayed behind so later passes could audit it. That is still the right
/// instinct for anything carrying evidence, but it made the file unbounded:
/// every ephemeral task goal leaves an entry forever, and the whole file is
/// deep-cloned, re-serialized, and rewritten on each maintenance write. These
/// bounds keep the audit value while capping the cost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryTemperatureRetentionPolicy {
    /// Hard ceiling on retained entries.
    pub max_entries: usize,
    /// How long an entry with no live candidate and no signal is kept.
    pub dead_entry_ttl_days: i64,
    /// How many compaction batches the audit file retains.
    pub max_audit_batches: usize,
}

impl Default for MemoryTemperatureRetentionPolicy {
    fn default() -> Self {
        Self {
            max_entries: 20_000,
            dead_entry_ttl_days: 30,
            max_audit_batches: 32,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryTemperatureCompactionSummary {
    pub scanned: usize,
    pub migrated_keys: usize,
    pub evicted_dead: usize,
    pub evicted_over_cap: usize,
    /// How many evicted entries still carried the pre-v6 key encoding.
    pub evicted_legacy_encoding: usize,
    pub retained: usize,
}

impl MemoryTemperatureCompactionSummary {
    pub fn evicted(&self) -> usize {
        self.evicted_dead.saturating_add(self.evicted_over_cap)
    }

    pub fn changed(&self) -> bool {
        self.migrated_keys > 0 || self.evicted() > 0
    }
}

/// The `legacy key -> current key` table implied by a candidate set.
///
/// Computed from candidates alone so every durable structure keyed by candidate
/// identity — the temperature overlay and the hot-projection index — migrates
/// through one table and cannot drift apart.
pub fn memory_temperature_candidate_key_renames(
    candidates: &[MemoryCandidateDocument],
) -> BTreeMap<String, String> {
    let mut renames = BTreeMap::new();
    for candidate in candidates {
        let current_key = memory_temperature_candidate_key(candidate);
        let legacy_key = legacy_memory_temperature_candidate_key(candidate);
        if legacy_key != current_key {
            renames.insert(legacy_key, current_key);
        }
    }
    renames
}

/// Rewrite pre-v6 keys to the length-prefixed encoding, preserving every
/// counter and timestamp.
///
/// Migration is driven by candidates rather than by parsing old keys, because
/// the old encoding is not invertible — that ambiguity is the defect being
/// repaired. A candidate yields both its legacy key and its current key, so the
/// rename is exact. Cross-references (`superseded_by`, `supersedes`) are
/// remapped through the same table so supersession chains survive.
///
/// Safe to call with a partial candidate set: it only renames what it can match
/// and never removes anything.
pub fn migrate_memory_temperature_overlay_keys(
    overlay: &mut MemoryTemperatureOverlay,
    candidates: &[MemoryCandidateDocument],
) -> usize {
    let renames = memory_temperature_candidate_key_renames(candidates)
        .into_iter()
        .filter(|(legacy_key, current_key)| {
            overlay.entries.contains_key(legacy_key) && !overlay.entries.contains_key(current_key)
        })
        .collect::<BTreeMap<_, _>>();
    if renames.is_empty() {
        return 0;
    }

    let mut migrated = 0usize;
    for (legacy_key, current_key) in &renames {
        let Some(mut entry) = overlay.entries.remove(legacy_key) else {
            continue;
        };
        entry.memory_candidate_key = current_key.clone();
        overlay.entries.insert(current_key.clone(), entry);
        migrated += 1;
    }

    for entry in overlay.entries.values_mut() {
        if let Some(superseded_by) = entry.superseded_by.as_ref() {
            if let Some(current) = renames.get(superseded_by) {
                entry.superseded_by = Some(current.clone());
            }
        }
        if entry.supersedes.iter().any(|key| renames.contains_key(key)) {
            for key in &mut entry.supersedes {
                if let Some(current) = renames.get(key) {
                    *key = current.clone();
                }
            }
            entry.supersedes.sort();
            entry.supersedes.dedup();
        }
    }

    migrated
}

/// Whether an entry must be kept regardless of age, because dropping it would
/// destroy evidence rather than reclaim waste.
fn memory_temperature_entry_is_evidence(entry: &MemoryTemperatureEntry) -> bool {
    // Supersession *winners* are covered by `has_any_temperature_signal`; this
    // adds the losers, which have no signal of their own but are the audit
    // record of what replaced what.
    memory_temperature_entry_is_superseded(entry) || has_any_temperature_signal(entry)
}

/// Drop entries that no longer describe anything and carry no evidence.
///
/// Only ever safe with the **complete** candidate set for the scope — a partial
/// set would look like mass deletion. Callers holding a subset must use
/// [`sync_memory_temperature_overlay`], which never evicts. Returns an empty
/// summary when handed an empty live set, so a failed candidate collection
/// cannot be mistaken for an empty scope.
pub fn compact_memory_temperature_overlay(
    overlay: &mut MemoryTemperatureOverlay,
    live_candidate_keys: &BTreeSet<String>,
    policy: MemoryTemperatureRetentionPolicy,
    now: DateTime<Utc>,
) -> (MemoryTemperatureCompactionSummary, Vec<String>) {
    let mut summary = MemoryTemperatureCompactionSummary {
        scanned: overlay.entries.len(),
        ..Default::default()
    };
    let mut evicted_samples = Vec::new();
    if live_candidate_keys.is_empty() {
        summary.retained = overlay.entries.len();
        return (summary, evicted_samples);
    }

    // Keys named as a successor by something we are keeping stay reachable so a
    // supersession chain never dangles.
    let successor_keys = overlay
        .entries
        .values()
        .filter_map(|entry| entry.superseded_by.clone())
        .collect::<BTreeSet<_>>();

    let ttl_days = policy.dead_entry_ttl_days.max(0);
    let evictable =
        |key: &String| !live_candidate_keys.contains(key) && !successor_keys.contains(key);

    let mut dead = Vec::new();
    for (key, entry) in &overlay.entries {
        if !evictable(key) || memory_temperature_entry_is_evidence(entry) {
            continue;
        }
        // `None` means the entry never had activity *and* predates
        // `first_seen_at`, so it was written before this deploy and is at least
        // as old as the TTL by construction.
        let aged_out = match latest_temperature_activity(entry).or(entry.first_seen_at) {
            Some(anchor) => days_since(now, anchor) >= ttl_days as f64,
            None => true,
        };
        if aged_out {
            dead.push(key.clone());
        }
    }
    for key in dead {
        if overlay.entries.remove(&key).is_some() {
            summary.evicted_dead += 1;
            // The map key is the authoritative identity; the entry's own copy
            // of it is a denormalised convenience.
            if !memory_temperature_candidate_key_is_current(&key) {
                summary.evicted_legacy_encoding += 1;
            }
            push_compaction_sample(&mut evicted_samples, &key);
        }
    }

    if overlay.entries.len() > policy.max_entries {
        let mut ranked = overlay
            .entries
            .iter()
            .filter(|(key, entry)| evictable(key) && !memory_temperature_entry_is_superseded(entry))
            .map(|(key, entry)| {
                (
                    key.clone(),
                    entry.temperature_score,
                    latest_temperature_activity(entry).unwrap_or(entry.updated_at),
                )
            })
            .collect::<Vec<_>>();
        // Coldest and least recently touched first.
        ranked.sort_by(|left, right| {
            left.1
                .partial_cmp(&right.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.2.cmp(&right.2))
                .then_with(|| left.0.cmp(&right.0))
        });
        let over_cap = overlay.entries.len().saturating_sub(policy.max_entries);
        for (key, _, _) in ranked.into_iter().take(over_cap) {
            if overlay.entries.remove(&key).is_some() {
                summary.evicted_over_cap += 1;
                if !memory_temperature_candidate_key_is_current(&key) {
                    summary.evicted_legacy_encoding += 1;
                }
                push_compaction_sample(&mut evicted_samples, &key);
            }
        }
    }

    summary.retained = overlay.entries.len();
    if summary.evicted() > 0 {
        overlay.updated_at = now;
    }
    (summary, evicted_samples)
}

const MAX_COMPACTION_SAMPLES: usize = 20;
const MAX_COMPACTION_SAMPLE_CHARS: usize = 160;

fn push_compaction_sample(samples: &mut Vec<String>, key: &str) {
    if samples.len() >= MAX_COMPACTION_SAMPLES {
        return;
    }
    samples.push(truncate_temperature_reason(
        key,
        MAX_COMPACTION_SAMPLE_CHARS,
    ));
}

pub fn memory_temperature_compaction_audit_path(storage: &dyn MemoryStorage) -> PathBuf {
    storage
        .root()
        .join("index")
        .join(MEMORY_TEMPERATURE_COMPACTION_AUDIT_FILE)
}

async fn append_memory_temperature_compaction_audit(
    storage: &dyn MemoryStorage,
    summary: &MemoryTemperatureCompactionSummary,
    evicted_samples: &[String],
    policy: MemoryTemperatureRetentionPolicy,
    now: DateTime<Utc>,
) -> Result<(), MemoryStorageError> {
    let path = memory_temperature_compaction_audit_path(storage);
    let mut batches = match storage.read_json_value(&path).await {
        Ok(Value::Array(batches)) => batches,
        Ok(_) => Vec::new(),
        Err(MemoryStorageError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            Vec::new()
        },
        Err(error) => return Err(error),
    };
    batches.push(serde_json::json!({
        "at": now.to_rfc3339(),
        "summary": summary,
        "evicted_sample_keys": evicted_samples,
    }));
    let keep = policy.max_audit_batches.max(1);
    if batches.len() > keep {
        batches.drain(..batches.len() - keep);
    }
    storage
        .write_json_value_atomic(&path, &Value::Array(batches))
        .await
}

/// Full-scope overlay reconciliation: migrate keys, refresh structural
/// metadata, compact, then maintain — in that order, under one write lock.
///
/// This is the only path that may evict, so it must be handed every candidate
/// in the scope.
pub async fn resync_memory_temperature_overlay_full_scope(
    storage: &dyn MemoryStorage,
    candidates: &[MemoryCandidateDocument],
    policy: MemoryTemperatureRetentionPolicy,
) -> Result<(MemoryTemperatureOverlay, MemoryTemperatureCompactionSummary), MemoryStorageError> {
    // Held through the audit append below, which is its own read-modify-write.
    let _overlay_guard = lock_overlay_for_write("full_scope_resync").await;
    let candidates: Arc<[MemoryCandidateDocument]> = Arc::from(candidates);
    let ((summary, evicted_samples, now), overlay) =
        mutate_memory_temperature_overlay_locked(storage, move |overlay| {
            let now = Utc::now();

            let migrated = migrate_memory_temperature_overlay_keys(overlay, &candidates);
            let mut changed = migrated > 0;
            if overlay.schema_version != MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION {
                overlay.schema_version = MEMORY_TEMPERATURE_OVERLAY_SCHEMA_VERSION;
                changed = true;
            }

            let live_keys = candidates
                .iter()
                .map(memory_temperature_candidate_key)
                .collect::<BTreeSet<_>>();
            changed |= apply_memory_temperature_overlay_sync(overlay, &candidates, now);

            let (mut summary, evicted_samples) =
                compact_memory_temperature_overlay(overlay, &live_keys, policy, now);
            summary.migrated_keys = migrated;

            let maintenance = apply_memory_temperature_maintenance(overlay, now);
            let changed = changed || summary.changed() || maintenance.changed > 0;
            if changed {
                overlay.updated_at = now;
            }
            ((summary, evicted_samples, now), changed)
        })
        .await?;
    let overlay = owned_memory_temperature_overlay(overlay).await?;
    if summary.changed() {
        // Best-effort on purpose. The overlay write above has already committed
        // the eviction; failing the call here would report "nothing happened"
        // about a deletion that did happen, and the caller would have no
        // summary to act on. Losing the audit line is the lesser harm, and it
        // is loud.
        if let Err(error) = append_memory_temperature_compaction_audit(
            storage,
            &summary,
            &evicted_samples,
            policy,
            now,
        )
        .await
        {
            tracing::warn!(
                error = %error,
                evicted = summary.evicted(),
                migrated_keys = summary.migrated_keys,
                "memory temperature compaction succeeded but its audit record could not be written"
            );
        }
    }
    Ok((overlay, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn app_source_envelope_cannot_assign_temperature() {
        use crate::memory_candidates::{
            memory_candidate_has_app_source_envelope, strip_package_assigned_memory_heat,
            APP_SOURCE_ELIGIBILITY_METADATA_KEY,
        };
        use crate::memory_tiers::TierScope;
        use serde_json::json;

        let mut metadata = json!({
            APP_SOURCE_ELIGIBILITY_METADATA_KEY: {"candidate_id": "memory:1"},
            "temperature": "hot",
            "force_prompt_inclusion": true
        });
        assert!(memory_candidate_has_app_source_envelope(&metadata));
        strip_package_assigned_memory_heat(&mut metadata);
        assert!(metadata.get("temperature").is_none());
        assert!(metadata.get("force_prompt_inclusion").is_none());

        let candidate = MemoryCandidateDocument {
            principal: None,
            workspace: None,
            agent_id: None,
            scope: TierScope::User,
            tier_name: "entities".to_string(),
            semantic_memory_type: SemanticMemoryType::Entity,
            goal_id: None,
            item_key: "asha".to_string(),
            source_path: None,
            json_pointer: String::new(),
            content_hash: "hash".to_string(),
            last_updated: Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, 0).unwrap(),
            confidence: None,
            text: "Asha is a mentor".to_string(),
            metadata_json: json!({
                APP_SOURCE_ELIGIBILITY_METADATA_KEY: {"candidate_id": "memory:1"},
                "temperature": "hot",
                "force_prompt_inclusion": true
            }),
        };
        let entry = MemoryTemperatureEntry::from_candidate(
            memory_temperature_candidate_key(&candidate),
            &candidate,
            Utc.with_ymd_and_hms(2026, 8, 18, 0, 0, 1).unwrap(),
        );
        assert_eq!(
            entry.temperature_tier,
            default_temperature_tier(SemanticMemoryType::Entity)
        );
        assert_eq!(
            entry.temperature_score,
            default_temperature_score(SemanticMemoryType::Entity)
        );
    }

    #[test]
    fn default_temperature_keeps_raw_evidence_cold() {
        assert_eq!(
            default_temperature_tier(SemanticMemoryType::UserPreference),
            MemoryTemperatureTier::T1
        );
        assert_eq!(
            default_temperature_tier(SemanticMemoryType::SourceEvidence),
            MemoryTemperatureTier::T3
        );
        assert!(
            default_temperature_score(SemanticMemoryType::UserPreference)
                > default_temperature_score(SemanticMemoryType::SourceEvidence)
        );
    }

    #[test]
    fn concurrent_prompt_tier_preparation_reuses_immutable_snapshot() {
        let now = Utc.with_ymd_and_hms(2026, 7, 18, 12, 0, 0).unwrap();
        let overlay = Arc::new(MemoryTemperatureOverlay::default());
        let first = memory_temperature_tiers_for_prompt_snapshot(&overlay, now);
        let second = memory_temperature_tiers_for_prompt_snapshot(
            &overlay,
            now + chrono::Duration::milliseconds(500),
        );

        assert!(Arc::ptr_eq(&first, &second));
        assert_eq!(*first, memory_temperature_tiers_for_prompt(&overlay, now));
    }

    #[test]
    fn prompt_tier_snapshot_survives_former_one_second_window_but_not_overlay_revision() {
        let now = Utc.with_ymd_and_hms(2026, 7, 26, 12, 0, 0).unwrap();
        let overlay = Arc::new(MemoryTemperatureOverlay::default());
        let first = memory_temperature_tiers_for_prompt_snapshot(&overlay, now);

        // Age this exact entry beyond the former one-second reuse window
        // without sleeping. The bounded prompt-turn cache must still reuse it.
        {
            let mut cache = prompt_tier_snapshot_cache()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let entry = cache
                .iter_mut()
                .find(|entry| Arc::ptr_eq(&entry.overlay, &overlay))
                .expect("snapshot cache entry");
            entry.created_at = Instant::now() - Duration::from_secs(2);
        }
        let reused = memory_temperature_tiers_for_prompt_snapshot(
            &overlay,
            now + chrono::Duration::seconds(2),
        );
        assert!(Arc::ptr_eq(&first, &reused));

        // Persisted/external writes install a new immutable overlay revision,
        // which must never inherit tiers cached for the previous Arc.
        let revised_overlay = Arc::new((*overlay).clone());
        let revised = memory_temperature_tiers_for_prompt_snapshot(
            &revised_overlay,
            now + chrono::Duration::seconds(2),
        );
        assert!(!Arc::ptr_eq(&first, &revised));
    }

    #[test]
    fn prompt_usage_updates_retrieved_selected_and_injected_counts() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(
            "user:::preferences.items:key:coffee".to_string(),
            MemoryTemperatureEntry {
                memory_candidate_key: "user:::preferences.items:key:coffee".to_string(),
                semantic_memory_type: SemanticMemoryType::UserPreference,
                temperature_tier: MemoryTemperatureTier::T1,
                temperature_score: 0.75,
                confidence: Some(1.0),
                last_retrieved_at: None,
                last_selected_at: None,
                last_injected_at: None,
                last_used_at: None,
                retrieved_count: 0,
                selected_count: 0,
                injected_count: 0,
                successful_use_count: 0,
                failed_use_count: 0,
                reviewed_referenced_count: 0,
                reviewed_useful_count: 0,
                reviewed_load_bearing_count: 0,
                reviewed_irrelevant_count: 0,
                reviewed_stale_count: 0,
                reviewed_harmful_count: 0,
                source_ids: Vec::new(),
                superseded_by: None,
                superseded_at: None,
                supersession_reason: None,
                supersession_confidence: None,
                supersession_source: None,
                supersedes: Vec::new(),
                first_seen_at: Some(now),
                last_temperature_review_at: None,
                last_temperature_change_reason: None,
                last_utility_review_at: None,
                last_utility_review_run_id: None,
                last_utility_review_label: None,
                last_utility_review_confidence: None,
                last_utility_review_reason: None,
                updated_at: now,
            },
        );

        let retrieved = vec![
            "user:::preferences.items:key:coffee".to_string(),
            "missing".to_string(),
            "user:::preferences.items:key:coffee".to_string(),
        ];
        let selected = BTreeSet::from(["user:::preferences.items:key:coffee".to_string()]);

        let summary =
            apply_memory_temperature_prompt_usage(&mut overlay, &retrieved, &selected, now);
        let entry = overlay
            .entries
            .get("user:::preferences.items:key:coffee")
            .expect("entry");

        assert_eq!(summary.retrieved, 1);
        assert_eq!(summary.selected, 1);
        assert_eq!(summary.injected, 1);
        assert_eq!(summary.missing, 1);
        assert_eq!(entry.retrieved_count, 1);
        assert_eq!(entry.selected_count, 1);
        assert_eq!(entry.injected_count, 1);
        assert_eq!(entry.last_retrieved_at, Some(now));
        assert_eq!(entry.last_selected_at, Some(now));
        assert_eq!(entry.last_injected_at, Some(now));
    }

    #[test]
    fn outcome_usage_updates_success_and_last_used() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let mut entry = test_entry(
            "user:::procedures.items:useful-workflow",
            SemanticMemoryType::Procedure,
            now,
        );
        entry.selected_count = 2;
        entry.injected_count = 2;
        overlay
            .entries
            .insert(entry.memory_candidate_key.clone(), entry);

        let summary = apply_memory_temperature_outcome_usage(
            &mut overlay,
            &["user:::procedures.items:useful-workflow".to_string()],
            MemoryTemperatureOutcomeSignal::Successful,
            now,
        );
        let maintenance = apply_memory_temperature_maintenance(&mut overlay, now);
        let entry = overlay
            .entries
            .get("user:::procedures.items:useful-workflow")
            .expect("entry");

        assert_eq!(summary.used, 1);
        assert_eq!(summary.successful, 1);
        assert_eq!(entry.successful_use_count, 1);
        assert_eq!(entry.last_used_at, Some(now));
        assert!(maintenance.promoted >= 1);
        assert_eq!(entry.temperature_tier, MemoryTemperatureTier::T0);
    }

    #[test]
    fn immutable_prompt_tiers_match_mutating_maintenance() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for index in 0..48 {
            let scope = if index % 2 == 0 { "user" } else { "agent:a" };
            let semantic_type = match index % 4 {
                0 => SemanticMemoryType::UserPreference,
                1 => SemanticMemoryType::Entity,
                2 => SemanticMemoryType::Procedure,
                _ => SemanticMemoryType::Episode,
            };
            let key = format!("{scope}::tier.items:item-{index}");
            let mut entry = test_entry(&key, semantic_type, now);
            entry.retrieved_count = index as u32;
            entry.selected_count = (index / 3) as u32;
            entry.injected_count = (index / 4) as u32;
            entry.successful_use_count = (index / 7) as u32;
            entry.failed_use_count = (index / 11) as u32;
            if index == 9 {
                entry.superseded_by = Some("user::tier.items:item-10".to_string());
            }
            overlay.entries.insert(key, entry);
        }

        let immutable_tiers = memory_temperature_tiers_for_prompt(&overlay, now);
        let original_tiers = overlay
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.temperature_tier))
            .collect::<BTreeMap<_, _>>();
        let mut maintained = overlay.clone();
        apply_memory_temperature_maintenance(&mut maintained, now);
        let maintained_tiers = maintained
            .entries
            .iter()
            .map(|(key, entry)| (key.clone(), entry.temperature_tier))
            .collect::<BTreeMap<_, _>>();

        assert_eq!(immutable_tiers, maintained_tiers);
        assert_eq!(
            overlay
                .entries
                .iter()
                .map(|(key, entry)| (key.clone(), entry.temperature_tier))
                .collect::<BTreeMap<_, _>>(),
            original_tiers,
            "prompt projection must not mutate the shared overlay"
        );
    }

    #[test]
    fn outcome_usage_updates_failed_signal() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let entry = test_entry(
            "agent:agent-1::entities.items:noisy-entity",
            SemanticMemoryType::Entity,
            now,
        );
        overlay
            .entries
            .insert(entry.memory_candidate_key.clone(), entry);

        let summary = apply_memory_temperature_outcome_usage(
            &mut overlay,
            &["agent:agent-1::entities.items:noisy-entity".to_string()],
            MemoryTemperatureOutcomeSignal::Failed,
            now,
        );
        let entry = overlay
            .entries
            .get("agent:agent-1::entities.items:noisy-entity")
            .expect("entry");

        assert_eq!(summary.used, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(entry.failed_use_count, 1);
        assert_eq!(entry.last_used_at, Some(now));
    }

    #[test]
    fn utility_review_promotes_load_bearing_memory() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for idx in 0..10 {
            let mut entry = test_entry(
                &format!("user:::procedures.items:reviewed-workflow-{idx}"),
                SemanticMemoryType::Procedure,
                now,
            );
            if idx == 0 {
                entry.selected_count = 2;
                entry.injected_count = 2;
                entry.last_selected_at = Some(now);
                entry.last_injected_at = Some(now);
            }
            overlay
                .entries
                .insert(entry.memory_candidate_key.clone(), entry);
        }

        let summary = apply_memory_temperature_utility_review(
            &mut overlay,
            "run-1",
            &[MemoryTemperatureUtilityReviewJudgement {
                memory_candidate_key: "user:::procedures.items:reviewed-workflow-0".to_string(),
                label: MemoryTemperatureUtilityLabel::LoadBearing,
                confidence: Some(0.92),
                reason: Some("It directly determined the successful workflow.".to_string()),
                compact_text: None,
            }],
            now,
        );
        let maintenance = apply_memory_temperature_maintenance(&mut overlay, now);
        let entry = overlay
            .entries
            .get("user:::procedures.items:reviewed-workflow-0")
            .expect("entry");

        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.load_bearing, 1);
        assert_eq!(entry.reviewed_load_bearing_count, 1);
        assert_eq!(entry.successful_use_count, 2);
        assert_eq!(entry.last_utility_review_run_id.as_deref(), Some("run-1"));
        assert_eq!(
            entry.last_utility_review_label,
            Some(MemoryTemperatureUtilityLabel::LoadBearing)
        );
        assert!(maintenance.promoted >= 1);
        assert_eq!(entry.temperature_tier, MemoryTemperatureTier::T0);
    }

    #[test]
    fn utility_review_run_ledger_makes_crash_replay_idempotent() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let entry = test_entry(
            "user:::procedures.items:durable-workflow",
            SemanticMemoryType::Procedure,
            now,
        );
        overlay
            .entries
            .insert(entry.memory_candidate_key.clone(), entry);
        let judgement = MemoryTemperatureUtilityReviewJudgement {
            memory_candidate_key: "user:::procedures.items:durable-workflow".to_string(),
            label: MemoryTemperatureUtilityLabel::Useful,
            confidence: Some(0.9),
            reason: Some("It materially helped the completed run.".to_string()),
            compact_text: None,
        };

        let first = apply_memory_temperature_utility_review(
            &mut overlay,
            "durable-run-1",
            std::slice::from_ref(&judgement),
            now,
        );
        let replay = apply_memory_temperature_utility_review(
            &mut overlay,
            "durable-run-1",
            std::slice::from_ref(&judgement),
            now + chrono::Duration::seconds(1),
        );
        let next_run = apply_memory_temperature_utility_review(
            &mut overlay,
            "durable-run-2",
            &[judgement],
            now + chrono::Duration::seconds(2),
        );
        let entry = overlay
            .entries
            .get("user:::procedures.items:durable-workflow")
            .expect("reviewed entry");

        assert_eq!(first.reviewed, 1);
        assert_eq!(replay.reviewed, 0);
        assert_eq!(next_run.reviewed, 1);
        assert_eq!(entry.reviewed_useful_count, 2);
        assert_eq!(entry.successful_use_count, 2);
        assert_eq!(overlay.applied_utility_review_runs.len(), 2);
    }

    #[test]
    fn utility_review_run_ledger_prunes_oldest_without_losing_new_marker() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for index in 0..MAX_APPLIED_UTILITY_REVIEW_RUNS {
            overlay.applied_utility_review_runs.insert(
                format!("old-{index:04}"),
                now - chrono::Duration::seconds((index + 1) as i64),
            );
        }

        apply_memory_temperature_utility_review(
            &mut overlay,
            "new-run",
            &[],
            now + chrono::Duration::seconds(1),
        );

        assert_eq!(
            overlay.applied_utility_review_runs.len(),
            MAX_APPLIED_UTILITY_REVIEW_RUNS
        );
        assert!(overlay.applied_utility_review_runs.contains_key("new-run"));
        assert!(!overlay.applied_utility_review_runs.contains_key("old-1023"));
    }

    #[test]
    fn pre_v5_overlay_deserializes_with_an_empty_utility_run_ledger() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let value = serde_json::json!({
            "schema_version": 4,
            "updated_at": now,
            "entries": {}
        });

        let overlay: MemoryTemperatureOverlay =
            serde_json::from_value(value).expect("deserialize pre-v5 overlay");

        assert_eq!(overlay.schema_version, 4);
        assert!(overlay.applied_utility_review_runs.is_empty());
    }

    #[test]
    fn utility_review_demotes_stale_memory() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let entry = test_entry(
            "agent:agent-1::entities.items:stale-entity",
            SemanticMemoryType::Entity,
            now,
        );
        overlay
            .entries
            .insert(entry.memory_candidate_key.clone(), entry);

        let summary = apply_memory_temperature_utility_review(
            &mut overlay,
            "run-2",
            &[MemoryTemperatureUtilityReviewJudgement {
                memory_candidate_key: "agent:agent-1::entities.items:stale-entity".to_string(),
                label: MemoryTemperatureUtilityLabel::Stale,
                confidence: Some(0.84),
                reason: Some("The final answer did not use this older entity fact.".to_string()),
                compact_text: None,
            }],
            now,
        );
        apply_memory_temperature_maintenance(&mut overlay, now);
        let entry = overlay
            .entries
            .get("agent:agent-1::entities.items:stale-entity")
            .expect("entry");

        assert_eq!(summary.reviewed, 1);
        assert_eq!(summary.stale, 1);
        assert_eq!(entry.reviewed_stale_count, 1);
        assert_eq!(entry.failed_use_count, 3);
        assert_eq!(
            entry.last_utility_review_label,
            Some(MemoryTemperatureUtilityLabel::Stale)
        );
        assert_eq!(entry.temperature_tier, MemoryTemperatureTier::T3);
    }

    #[test]
    fn supersession_marks_old_memory_cold_and_links_successor() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let mut old_entry = test_entry(
            "user:::preferences.items:key:coffee_old",
            SemanticMemoryType::UserPreference,
            now,
        );
        old_entry.selected_count = 3;
        old_entry.injected_count = 3;
        old_entry.temperature_tier = MemoryTemperatureTier::T0;
        old_entry.temperature_score = 0.95;
        let new_entry = test_entry(
            "user:::preferences.items:key:coffee_new",
            SemanticMemoryType::UserPreference,
            now,
        );
        overlay
            .entries
            .insert(old_entry.memory_candidate_key.clone(), old_entry);
        overlay
            .entries
            .insert(new_entry.memory_candidate_key.clone(), new_entry);

        let summary = apply_memory_temperature_supersessions(
            &mut overlay,
            &[MemoryTemperatureSupersession {
                superseded_candidate_key: "user:::preferences.items:key:coffee_old".to_string(),
                superseded_by_candidate_key: "user:::preferences.items:key:coffee_new".to_string(),
                reason: Some("Newer explicit preference contradicts old coffee order.".to_string()),
                confidence: Some(0.91),
                source: Some("test".to_string()),
            }],
            now,
        );
        let maintenance = apply_memory_temperature_maintenance(&mut overlay, now);
        let old_entry = overlay
            .entries
            .get("user:::preferences.items:key:coffee_old")
            .expect("old entry");
        let new_entry = overlay
            .entries
            .get("user:::preferences.items:key:coffee_new")
            .expect("new entry");

        assert_eq!(summary.superseded, 1);
        assert_eq!(old_entry.temperature_tier, MemoryTemperatureTier::T3);
        assert_eq!(old_entry.temperature_score, 0.0);
        assert_eq!(
            old_entry.superseded_by.as_deref(),
            Some("user:::preferences.items:key:coffee_new")
        );
        assert_eq!(old_entry.supersession_confidence, Some(0.91));
        assert!(memory_temperature_entry_is_superseded(old_entry));
        assert_eq!(
            new_entry.supersedes,
            vec!["user:::preferences.items:key:coffee_old".to_string()]
        );
        assert_eq!(maintenance.demoted, 0);
        assert!(old_entry
            .last_temperature_change_reason
            .as_deref()
            .unwrap_or_default()
            .contains("superseded_by=user:::preferences.items:key:coffee_new"));
    }

    #[test]
    fn maintenance_promotes_selected_memory_to_hot_within_lane_cap() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for idx in 0..10 {
            let mut entry = test_entry(
                &format!("user:::procedures.items:workflow-{idx}"),
                SemanticMemoryType::Procedure,
                now,
            );
            if idx == 0 {
                entry.retrieved_count = 4;
                entry.selected_count = 3;
                entry.injected_count = 3;
                entry.successful_use_count = 1;
                entry.last_selected_at = Some(now);
                entry.last_injected_at = Some(now);
                entry.last_used_at = Some(now);
            }
            overlay
                .entries
                .insert(entry.memory_candidate_key.clone(), entry);
        }

        let summary = apply_memory_temperature_maintenance(&mut overlay, now);
        let hot_entries = overlay
            .entries
            .values()
            .filter(|entry| entry.temperature_tier == MemoryTemperatureTier::T0)
            .collect::<Vec<_>>();

        assert!(summary.promoted >= 1);
        assert_eq!(hot_entries.len(), 1);
        assert_eq!(
            hot_entries[0].memory_candidate_key,
            "user:::procedures.items:workflow-0"
        );
    }

    #[test]
    fn maintenance_does_not_promote_retrieval_only_memory_to_hot() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let mut entry = test_entry(
            "user:::preferences.items:retrieval-only",
            SemanticMemoryType::UserPreference,
            now,
        );
        entry.retrieved_count = 20;
        entry.last_retrieved_at = Some(now);
        overlay
            .entries
            .insert(entry.memory_candidate_key.clone(), entry);

        apply_memory_temperature_maintenance(&mut overlay, now);
        let entry = overlay
            .entries
            .get("user:::preferences.items:retrieval-only")
            .expect("entry");

        assert_ne!(entry.temperature_tier, MemoryTemperatureTier::T0);
        assert_ne!(
            entry.temperature_score,
            default_temperature_score(SemanticMemoryType::UserPreference)
        );
        assert_eq!(entry.last_temperature_review_at, Some(now));
    }

    #[test]
    fn maintenance_applies_lane_pressure_to_demote_tail() {
        let now = Utc.with_ymd_and_hms(2026, 6, 14, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for idx in 0..10 {
            let mut entry = test_entry(
                &format!("agent:agent-1::entities.items:entity-{idx}"),
                SemanticMemoryType::Entity,
                now,
            );
            if idx < 2 {
                entry.selected_count = 4;
                entry.injected_count = 4;
                entry.last_selected_at = Some(now);
                entry.last_injected_at = Some(now);
            }
            if idx >= 8 {
                entry.confidence = Some(0.1);
                entry.failed_use_count = 4;
            }
            overlay
                .entries
                .insert(entry.memory_candidate_key.clone(), entry);
        }

        apply_memory_temperature_maintenance(&mut overlay, now);
        let cold_tail = overlay
            .entries
            .values()
            .filter(|entry| {
                entry.memory_candidate_key.ends_with("entity-8")
                    || entry.memory_candidate_key.ends_with("entity-9")
            })
            .all(|entry| entry.temperature_tier == MemoryTemperatureTier::T3);

        assert!(cold_tail);
    }

    fn test_candidate(
        scope: TierScope,
        agent_id: Option<&str>,
        goal_id: Option<&str>,
        tier_name: &str,
        item_key: &str,
    ) -> MemoryCandidateDocument {
        MemoryCandidateDocument {
            principal: None,
            workspace: None,
            agent_id: agent_id.map(ToString::to_string),
            scope,
            tier_name: tier_name.to_string(),
            semantic_memory_type: SemanticMemoryType::ProjectContext,
            goal_id: goal_id.map(ToString::to_string),
            item_key: item_key.to_string(),
            source_path: None,
            json_pointer: String::new(),
            content_hash: "hash".to_string(),
            last_updated: Utc.with_ymd_and_hms(2026, 8, 21, 0, 0, 0).unwrap(),
            confidence: None,
            text: "text".to_string(),
            metadata_json: serde_json::json!({}),
        }
    }

    /// The live-overlay shape this repairs: one `project_context` entry per task
    /// goal, each alone in its `(scope, agent, goal)` partition, none ever
    /// retrieved. `percentage_cap` ceilings every non-empty lane to at least one
    /// slot and the lane's default score already clears `active_min_score`, so
    /// before the evidence requirement every one of these took an active slot.
    #[test]
    fn zero_signal_memory_alone_in_a_lane_cannot_take_an_active_slot() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        for index in 0..64 {
            let candidate = test_candidate(
                TierScope::AgentGoal,
                Some("personal-assistant"),
                Some(&format!("task_{index:032x}")),
                "task_progress.notes",
                "0",
            );
            let key = memory_temperature_candidate_key(&candidate);
            overlay.entries.insert(
                key.clone(),
                test_entry(&key, SemanticMemoryType::ProjectContext, now),
            );
        }

        apply_memory_temperature_maintenance(&mut overlay, now);

        let active = overlay
            .entries
            .values()
            .filter(|entry| {
                matches!(
                    entry.temperature_tier,
                    MemoryTemperatureTier::T0 | MemoryTemperatureTier::T1
                )
            })
            .count();
        assert_eq!(
            active, 0,
            "memory with no retrieval, selection, or use must not occupy the active tier"
        );
    }

    #[test]
    fn retrieved_memory_alone_in_a_lane_may_take_the_active_slot() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let candidate = test_candidate(
            TierScope::AgentGoal,
            Some("personal-assistant"),
            Some("task_solo"),
            "task_progress.notes",
            "0",
        );
        let key = memory_temperature_candidate_key(&candidate);
        let mut entry = test_entry(&key, SemanticMemoryType::ProjectContext, now);
        entry.retrieved_count = 1;
        entry.last_retrieved_at = Some(now);
        overlay.entries.insert(key.clone(), entry);

        apply_memory_temperature_maintenance(&mut overlay, now);

        assert_eq!(
            overlay.entries[&key].temperature_tier,
            MemoryTemperatureTier::T1,
            "a single real retrieval is enough evidence for the active tier"
        );
    }

    #[test]
    fn key_segments_containing_colons_do_not_shift_the_lane_partition() {
        // Both of these are real shapes from the live overlay.
        let chat_goal = test_candidate(
            TierScope::Agent,
            Some("personal-assistant"),
            Some("chat:2705464a-3f98-4261-a5f7-e4353b275509"),
            "episodes",
            "chat_58a699225bd2451fb3ab71eb0b11cd12",
        );
        let colon_agent = test_candidate(
            TierScope::Agent,
            Some("system:scheduler"),
            None,
            "agent_context",
            "0",
        );

        let chat_key = memory_temperature_candidate_key(&chat_goal);
        let chat_parts = parse_memory_temperature_candidate_key(&chat_key).expect("parses");
        assert_eq!(chat_parts.agent_id, "personal-assistant");
        assert_eq!(
            chat_parts.goal_id,
            "chat:2705464a-3f98-4261-a5f7-e4353b275509"
        );
        assert_eq!(chat_parts.item_key, "chat_58a699225bd2451fb3ab71eb0b11cd12");

        let colon_agent_key = memory_temperature_candidate_key(&colon_agent);
        let agent_parts = parse_memory_temperature_candidate_key(&colon_agent_key).expect("parses");
        assert_eq!(agent_parts.agent_id, "system:scheduler");
        assert_eq!(agent_parts.goal_id, "");

        // Two chat sessions of one agent are distinct partitions, where the
        // legacy `splitn(4, ':')` parse collapsed every session onto one.
        let other_chat = test_candidate(
            TierScope::Agent,
            Some("personal-assistant"),
            Some("chat:9f0c1d22-0000-4000-8000-0000000000ff"),
            "episodes",
            "chat_other",
        );
        assert_ne!(
            memory_temperature_scope_partition(&memory_temperature_candidate_key(&chat_goal)),
            memory_temperature_scope_partition(&memory_temperature_candidate_key(&other_chat)),
        );
        assert_eq!(
            memory_temperature_scope_partition(&legacy_memory_temperature_candidate_key(
                &chat_goal
            )),
            memory_temperature_scope_partition(&legacy_memory_temperature_candidate_key(
                &other_chat
            )),
            "legacy keys still group the old way, so an un-migrated overlay is unchanged"
        );
    }

    #[test]
    fn a_prose_goal_contributes_a_digest_not_kilobytes_of_key() {
        // The real shape: an entire personality-override prompt used as a goal id.
        let prose = format!(
            "## Personality Override (inherited from personal-assistant)\n\n{}",
            "For this cycle only, adopt the witty personality. ".repeat(60)
        );
        assert!(prose.len() > 2_000);
        let candidate = test_candidate(
            TierScope::Agent,
            Some("executive-assistant"),
            Some(&prose),
            "episodes",
            "exec_ceabbe55abe44025b5e17c0b55",
        );

        let key = memory_temperature_candidate_key(&candidate);
        assert!(key.len() < 1_024, "key was {} bytes", key.len());
        assert!(
            legacy_memory_temperature_candidate_key(&candidate).len() > 2_000,
            "the legacy key is the thing being fixed"
        );

        // Still identifying: a different goal is still a different key, and the
        // key still parses back into exactly five segments.
        let other = test_candidate(
            TierScope::Agent,
            Some("executive-assistant"),
            Some(&format!("{prose} and be brief")),
            "episodes",
            "exec_ceabbe55abe44025b5e17c0b55",
        );
        assert_ne!(key, memory_temperature_candidate_key(&other));
        let parts = parse_memory_temperature_candidate_key(&key).expect("parses");
        assert_eq!(parts.agent_id, "executive-assistant");
        assert_eq!(parts.tier_name, "episodes");
        assert_eq!(parts.item_key, "exec_ceabbe55abe44025b5e17c0b55");
    }

    /// The stored field names are a wire format: metadata already on disk uses
    /// these exact strings. Pinned here so a rename cannot silently orphan it.
    #[test]
    fn supersession_metadata_field_names_are_stable() {
        assert_eq!(
            SUPERSEDED_BY_ITEM_KEY_METADATA_KEY,
            "superseded_by_item_key"
        );
        assert_eq!(
            SUPERSEDED_BY_CANDIDATE_KEY_METADATA_KEY,
            "superseded_by_candidate_key"
        );
    }

    /// The composed successor key must be byte-identical to the key the
    /// candidate loader will produce for that same successor. If these two ever
    /// disagree, every consolidator supersession dangles again — which is the
    /// whole defect this repairs.
    #[test]
    fn composed_successor_key_matches_the_successors_own_candidate_key() {
        let mut superseded = test_candidate(
            TierScope::User,
            None,
            None,
            "preferences.preferences",
            "key:coffee_old",
        );
        superseded.metadata_json = serde_json::json!({
            "memory_lifecycle": "superseded",
            "superseded_by": "durable::key::coffee current",
            "superseded_by_item_key": "key:coffee_current",
        });

        let successor = test_candidate(
            TierScope::User,
            None,
            None,
            "preferences.preferences",
            "key:coffee_current",
        );

        assert_eq!(
            superseded_by_for_candidate(&superseded),
            Some(memory_temperature_candidate_key(&successor)),
        );
    }

    /// The sibling rule carries the superseded item's own scope/agent/goal, not
    /// a default — an agent-goal supersession must not resolve into user scope.
    #[test]
    fn composition_inherits_the_superseded_candidates_own_scope() {
        let mut superseded = test_candidate(
            TierScope::AgentGoal,
            Some("personal-assistant"),
            Some("chat:2705464a-3f98-4261-a5f7-e4353b275509"),
            "task_progress.notes",
            "key:old",
        );
        superseded.metadata_json = serde_json::json!({
            "memory_lifecycle": "superseded",
            "superseded_by_item_key": "key:new",
        });

        let resolved = superseded_by_for_candidate(&superseded).expect("resolves");
        let parts = parse_memory_temperature_candidate_key(&resolved).expect("parses");
        assert_eq!(parts.scope, "agent_goal");
        assert_eq!(parts.agent_id, "personal-assistant");
        assert_eq!(parts.goal_id, "chat:2705464a-3f98-4261-a5f7-e4353b275509");
        assert_eq!(parts.tier_name, "task_progress.notes");
        assert_eq!(parts.item_key, "key:new");
    }

    /// An item with no stable identity writes no item key, and the reader must
    /// degrade to exactly today's behaviour rather than inventing one.
    #[test]
    fn missing_item_key_falls_back_to_the_legacy_value_unchanged() {
        let mut superseded =
            test_candidate(TierScope::User, None, None, "preferences.preferences", "0");
        superseded.metadata_json = serde_json::json!({
            "memory_lifecycle": "superseded",
            "superseded_by": "json_hash::abc123",
        });
        assert_eq!(
            superseded_by_for_candidate(&superseded),
            Some("json_hash::abc123".to_string()),
        );

        // A blank item key is the same as an absent one.
        superseded.metadata_json = serde_json::json!({
            "memory_lifecycle": "superseded",
            "superseded_by": "json_hash::abc123",
            "superseded_by_item_key": "   ",
        });
        assert_eq!(
            superseded_by_for_candidate(&superseded),
            Some("json_hash::abc123".to_string()),
        );
    }

    /// A writer that does hold the whole identity can say so directly, and that
    /// must win over the consolidator's legacy item-matching key.
    #[test]
    fn an_explicit_candidate_key_outranks_the_legacy_field() {
        let successor = test_candidate(
            TierScope::User,
            None,
            None,
            "preferences.preferences",
            "new",
        );
        let successor_key = memory_temperature_candidate_key(&successor);
        let mut superseded = test_candidate(
            TierScope::User,
            None,
            None,
            "preferences.preferences",
            "old",
        );
        superseded.metadata_json = serde_json::json!({
            "memory_lifecycle": "superseded",
            "superseded_by": "durable::key::whatever",
            "superseded_by_candidate_key": successor_key.clone(),
        });
        assert_eq!(
            superseded_by_for_candidate(&superseded),
            Some(successor_key)
        );
    }

    /// Supersession metadata on a candidate that is not actually superseded
    /// must resolve to nothing.
    #[test]
    fn a_candidate_without_superseded_lifecycle_resolves_to_nothing() {
        let mut candidate =
            test_candidate(TierScope::User, None, None, "preferences.preferences", "k");
        candidate.metadata_json = serde_json::json!({
            "superseded_by_item_key": "key:new",
        });
        assert_eq!(superseded_by_for_candidate(&candidate), None);
    }

    #[test]
    fn composed_keys_and_loaded_keys_use_one_encoding() {
        let candidate = test_candidate(
            TierScope::Agent,
            Some("system:scheduler"),
            None,
            "agent_context",
            "0",
        );
        assert_eq!(
            memory_temperature_candidate_key(&candidate),
            memory_temperature_candidate_key_from_parts(
                &candidate.scope,
                candidate.agent_id.as_deref(),
                candidate.goal_id.as_deref(),
                &candidate.tier_name,
                &candidate.item_key,
            ),
        );
    }

    #[test]
    fn distinct_candidates_cannot_collapse_onto_one_key() {
        // Under a bare `:` join these two produce the identical string.
        let left = test_candidate(TierScope::Agent, Some("a"), Some("b:c"), "t", "i");
        let right = test_candidate(TierScope::Agent, Some("a"), Some("b"), "c:t", "i");
        assert_eq!(
            legacy_memory_temperature_candidate_key(&left),
            legacy_memory_temperature_candidate_key(&right),
            "this collision is the defect being repaired"
        );
        assert_ne!(
            memory_temperature_candidate_key(&left),
            memory_temperature_candidate_key(&right),
        );
    }

    #[test]
    fn legacy_keys_migrate_in_place_preserving_counters_and_links() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let old = test_candidate(
            TierScope::Agent,
            Some("cto"),
            None,
            "design_decisions.entries",
            "0",
        );
        let new = test_candidate(
            TierScope::Agent,
            Some("cto"),
            None,
            "design_decisions.entries",
            "1",
        );
        let old_legacy = legacy_memory_temperature_candidate_key(&old);
        let new_legacy = legacy_memory_temperature_candidate_key(&new);

        let mut overlay = MemoryTemperatureOverlay::default();
        let mut old_entry = test_entry(&old_legacy, SemanticMemoryType::ProjectContext, now);
        old_entry.selected_count = 7;
        old_entry.successful_use_count = 3;
        old_entry.superseded_by = Some(new_legacy.clone());
        overlay.entries.insert(old_legacy.clone(), old_entry);
        let mut new_entry = test_entry(&new_legacy, SemanticMemoryType::ProjectContext, now);
        new_entry.supersedes = vec![old_legacy.clone()];
        overlay.entries.insert(new_legacy.clone(), new_entry);

        let migrated =
            migrate_memory_temperature_overlay_keys(&mut overlay, &[old.clone(), new.clone()]);
        assert_eq!(migrated, 2);

        let old_current = memory_temperature_candidate_key(&old);
        let new_current = memory_temperature_candidate_key(&new);
        assert!(!overlay.entries.contains_key(&old_legacy));
        let migrated_old = &overlay.entries[&old_current];
        assert_eq!(migrated_old.selected_count, 7);
        assert_eq!(migrated_old.successful_use_count, 3);
        assert_eq!(migrated_old.memory_candidate_key, old_current);
        assert_eq!(
            migrated_old.superseded_by.as_deref(),
            Some(new_current.as_str()),
            "supersession links must follow the rename or the chain dangles"
        );
        assert_eq!(overlay.entries[&new_current].supersedes, vec![old_current]);
    }

    #[test]
    fn migration_never_overwrites_an_existing_current_entry() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let candidate = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "0");
        let legacy = legacy_memory_temperature_candidate_key(&candidate);
        let current = memory_temperature_candidate_key(&candidate);

        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(
            legacy.clone(),
            test_entry(&legacy, SemanticMemoryType::ProjectContext, now),
        );
        let mut current_entry = test_entry(&current, SemanticMemoryType::ProjectContext, now);
        current_entry.selected_count = 99;
        overlay.entries.insert(current.clone(), current_entry);

        assert_eq!(
            migrate_memory_temperature_overlay_keys(&mut overlay, &[candidate]),
            0
        );
        assert_eq!(overlay.entries[&current].selected_count, 99);
    }

    #[test]
    fn compaction_drops_dead_entries_but_keeps_live_signal_and_evidence() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let long_ago = now - chrono::Duration::days(120);
        let mut overlay = MemoryTemperatureOverlay::default();

        let live = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "live");
        let live_key = memory_temperature_candidate_key(&live);
        overlay.entries.insert(
            live_key.clone(),
            test_entry(&live_key, SemanticMemoryType::ProjectContext, long_ago),
        );

        let dead_key = "mt1:5:agent:3:cto:0::4:tier:4:dead".to_string();
        overlay.entries.insert(
            dead_key.clone(),
            test_entry(&dead_key, SemanticMemoryType::ProjectContext, long_ago),
        );

        let used_key = "mt1:5:agent:3:cto:0::4:tier:4:used".to_string();
        let mut used = test_entry(&used_key, SemanticMemoryType::ProjectContext, long_ago);
        used.selected_count = 2;
        overlay.entries.insert(used_key.clone(), used);

        let superseded_key = "mt1:5:agent:3:cto:0::4:tier:3:old".to_string();
        let mut superseded = test_entry(
            &superseded_key,
            SemanticMemoryType::ProjectContext,
            long_ago,
        );
        superseded.superseded_by = Some(live_key.clone());
        overlay.entries.insert(superseded_key.clone(), superseded);

        let live_keys = [live_key.clone()].into_iter().collect::<BTreeSet<_>>();
        let (summary, samples) = compact_memory_temperature_overlay(
            &mut overlay,
            &live_keys,
            MemoryTemperatureRetentionPolicy::default(),
            now,
        );

        assert_eq!(summary.evicted_dead, 1);
        assert_eq!(samples.len(), 1);
        assert!(!overlay.entries.contains_key(&dead_key));
        assert!(overlay.entries.contains_key(&live_key), "live candidate");
        assert!(overlay.entries.contains_key(&used_key), "carries signal");
        assert!(
            overlay.entries.contains_key(&superseded_key),
            "supersession evidence is the reason retention existed"
        );
    }

    /// The partition must stay injective after the borrow rewrite: two keys
    /// differing only in a segment that is *inside* the partition must not
    /// collapse, and two differing only after it must group together.
    #[test]
    fn borrowed_partitions_group_exactly_as_the_segments_dictate() {
        let a = memory_temperature_candidate_key(&test_candidate(
            TierScope::AgentGoal,
            Some("pa"),
            Some("chat:1"),
            "tier",
            "0",
        ));
        let b = memory_temperature_candidate_key(&test_candidate(
            TierScope::AgentGoal,
            Some("pa"),
            Some("chat:2"),
            "tier",
            "0",
        ));
        let c = memory_temperature_candidate_key(&test_candidate(
            TierScope::AgentGoal,
            Some("pa"),
            Some("chat:1"),
            "other_tier",
            "9",
        ));

        assert_ne!(
            memory_temperature_scope_partition(&a),
            memory_temperature_scope_partition(&b),
            "different goals are different partitions"
        );
        assert_eq!(
            memory_temperature_scope_partition(&a),
            memory_temperature_scope_partition(&c),
            "tier and item live outside the partition"
        );
    }

    /// Legacy keys must group exactly as they did before the rewrite, or an
    /// un-migrated overlay would silently regroup mid-upgrade.
    #[test]
    fn legacy_partitions_are_unchanged_by_the_borrow_rewrite() {
        for key in [
            "agent:cto::design_decisions.entries:0",
            "user:::preferences.items:key:coffee",
            "agent_goal:pa:task_1:task_progress.notes:0",
        ] {
            let mut parts = key.splitn(4, ':');
            let expected = format!(
                "{}:{}:{}",
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default()
            );
            assert_eq!(memory_temperature_scope_partition(key), expected, "{key}");
        }
    }

    /// A neutral outcome stamps `last_used_at` and increments no counter, so
    /// an entry whose whole history is neutral outcomes must still read as
    /// evidence — otherwise compaction treats real participation as waste.
    #[test]
    fn a_neutral_outcome_still_counts_as_evidence() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let key = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(
            key.clone(),
            test_entry(&key, SemanticMemoryType::ProjectContext, now),
        );

        apply_memory_temperature_outcome_usage(
            &mut overlay,
            std::slice::from_ref(&key),
            MemoryTemperatureOutcomeSignal::Neutral,
            now,
        );
        let entry = &overlay.entries[&key];
        assert_eq!(
            entry.successful_use_count, 0,
            "neutral increments no counter"
        );
        assert!(entry.last_used_at.is_some());
        assert!(
            memory_temperature_entry_is_evidence(entry),
            "durable proof of participation must not read as waste"
        );

        // And compaction must therefore keep it.
        let live = ["mt1:5:agent:3:cto:0::4:tier:4:live".to_string()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (summary, _) = compact_memory_temperature_overlay(
            &mut overlay,
            &live,
            MemoryTemperatureRetentionPolicy::default(),
            now + chrono::Duration::days(365),
        );
        assert_eq!(summary.evicted_dead, 0);
    }

    /// After the v6 migration every pre-existing entry carries no
    /// `first_seen_at`, and a `None` anchor is treated as older than any TTL —
    /// so without a backfill the retention grace period is inert for the entire
    /// live population, permanently.
    #[test]
    fn sync_backfills_the_retention_anchor_for_entries_that_predate_it() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let candidate = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "0");
        let key = memory_temperature_candidate_key(&candidate);

        let mut overlay = MemoryTemperatureOverlay::default();
        let mut migrated = test_entry(&key, SemanticMemoryType::ProjectContext, now);
        migrated.first_seen_at = None; // what migration leaves behind
        overlay.entries.insert(key.clone(), migrated);

        let changed = apply_memory_temperature_overlay_sync(&mut overlay, &[candidate], now);

        assert!(
            changed,
            "the backfill is a durable change and must be saved"
        );
        assert_eq!(overlay.entries[&key].first_seen_at, Some(now));
    }

    /// The anchor is set once and never moved, or it would drift forward with
    /// every sync and reproduce the `updated_at` bug it exists to avoid.
    #[test]
    fn the_retention_anchor_is_never_moved_once_set() {
        let first_seen = Utc.with_ymd_and_hms(2026, 7, 1, 0, 0, 0).unwrap();
        let later = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let candidate = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "0");
        let key = memory_temperature_candidate_key(&candidate);

        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(
            key.clone(),
            test_entry(&key, SemanticMemoryType::ProjectContext, first_seen),
        );

        apply_memory_temperature_overlay_sync(&mut overlay, &[candidate], later);
        assert_eq!(overlay.entries[&key].first_seen_at, Some(first_seen));
    }

    /// An orphan never reaches the sync path, so it keeps a `None` anchor and
    /// stays immediately collectable — the intended behaviour for an entry that
    /// describes nothing and evidences nothing.
    #[test]
    fn an_orphan_keeps_no_anchor_and_stays_collectable() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let live = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "live");
        let live_key = memory_temperature_candidate_key(&live);
        let orphan_key = "mt1:5:agent:3:cto:0::4:tier:6:orphan".to_string();

        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(
            live_key.clone(),
            test_entry(&live_key, SemanticMemoryType::ProjectContext, now),
        );
        let mut orphan = test_entry(&orphan_key, SemanticMemoryType::ProjectContext, now);
        orphan.first_seen_at = None;
        overlay.entries.insert(orphan_key.clone(), orphan);

        apply_memory_temperature_overlay_sync(&mut overlay, &[live], now);
        assert_eq!(
            overlay.entries[&orphan_key].first_seen_at, None,
            "sync must not touch an entry with no live candidate"
        );

        let live_keys = [live_key].into_iter().collect::<BTreeSet<_>>();
        let (summary, _) = compact_memory_temperature_overlay(
            &mut overlay,
            &live_keys,
            MemoryTemperatureRetentionPolicy::default(),
            now,
        );
        assert_eq!(summary.evicted_dead, 1);
    }

    /// Retention age must not come from `updated_at`. Maintenance refreshes it
    /// whenever a score drifts, which recency decay makes near-continuous, so a
    /// dead entry would keep resetting its own clock and outlive every TTL.
    #[test]
    fn dead_entries_age_out_even_though_maintenance_keeps_touching_updated_at() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let created = now - chrono::Duration::days(120);
        let mut overlay = MemoryTemperatureOverlay::default();

        let live = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "live");
        let live_key = memory_temperature_candidate_key(&live);
        overlay.entries.insert(
            live_key.clone(),
            test_entry(&live_key, SemanticMemoryType::ProjectContext, created),
        );

        let dead_key = "mt1:5:agent:3:cto:0::4:tier:4:dead".to_string();
        let mut dead = test_entry(&dead_key, SemanticMemoryType::ProjectContext, created);
        dead.first_seen_at = Some(created);
        // A maintenance pass moments ago stamped this forward.
        dead.updated_at = now;
        overlay.entries.insert(dead_key.clone(), dead);

        let live_keys = [live_key].into_iter().collect::<BTreeSet<_>>();
        let (summary, _) = compact_memory_temperature_overlay(
            &mut overlay,
            &live_keys,
            MemoryTemperatureRetentionPolicy::default(),
            now,
        );

        assert_eq!(summary.evicted_dead, 1);
        assert!(!overlay.entries.contains_key(&dead_key));
    }

    /// Entries written before `first_seen_at` existed have no honest age
    /// anchor. They predate the deploy that added it, so once they have no live
    /// candidate and no signal they are exactly the accumulated waste the first
    /// sweep is meant to clear.
    #[test]
    fn pre_field_entries_without_any_activity_are_evictable() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let live = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "live");
        let live_key = memory_temperature_candidate_key(&live);
        overlay.entries.insert(
            live_key.clone(),
            test_entry(&live_key, SemanticMemoryType::ProjectContext, now),
        );

        let legacy_key = "agent:cto::tier:0".to_string();
        let mut legacy = test_entry(&legacy_key, SemanticMemoryType::ProjectContext, now);
        legacy.first_seen_at = None;
        overlay.entries.insert(legacy_key.clone(), legacy);

        let live_keys = [live_key].into_iter().collect::<BTreeSet<_>>();
        let (summary, _) = compact_memory_temperature_overlay(
            &mut overlay,
            &live_keys,
            MemoryTemperatureRetentionPolicy::default(),
            now,
        );

        assert_eq!(summary.evicted_dead, 1);
        assert_eq!(summary.evicted_legacy_encoding, 1);
        assert!(!overlay.entries.contains_key(&legacy_key));
    }

    #[test]
    fn compaction_is_inert_when_the_live_candidate_set_is_empty() {
        let now = Utc.with_ymd_and_hms(2026, 8, 21, 12, 0, 0).unwrap();
        let mut overlay = MemoryTemperatureOverlay::default();
        let key = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        overlay.entries.insert(
            key.clone(),
            test_entry(
                &key,
                SemanticMemoryType::ProjectContext,
                now - chrono::Duration::days(365),
            ),
        );

        let (summary, _) = compact_memory_temperature_overlay(
            &mut overlay,
            &BTreeSet::new(),
            MemoryTemperatureRetentionPolicy::default(),
            now,
        );

        assert_eq!(summary.evicted(), 0);
        assert!(
            overlay.entries.contains_key(&key),
            "a failed candidate collection must never read as a scope-wide delete"
        );
    }

    /// File-backed storage that counts overlay writes, so a test can tell a
    /// skipped save from a rewrite of identical bytes. Clones share the
    /// counters and the failure switch, so the write-behind flush (which
    /// writes through a shared handle) is counted too.
    #[derive(Clone)]
    struct CountingOverlayStorage {
        root: PathBuf,
        overlay_writes: Arc<AtomicUsize>,
        tmp_counter: Arc<AtomicUsize>,
        fail_overlay_writes: Arc<std::sync::atomic::AtomicBool>,
    }

    impl CountingOverlayStorage {
        fn new(root: PathBuf) -> Self {
            Self {
                root,
                overlay_writes: Arc::new(AtomicUsize::new(0)),
                tmp_counter: Arc::new(AtomicUsize::new(0)),
                fail_overlay_writes: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            }
        }

        fn overlay_writes(&self) -> usize {
            self.overlay_writes.load(AtomicOrdering::SeqCst)
        }

        fn fail_overlay_writes(&self, fail: bool) {
            self.fail_overlay_writes.store(fail, AtomicOrdering::SeqCst);
        }
    }

    #[async_trait::async_trait]
    impl MemoryStorage for CountingOverlayStorage {
        fn root(&self) -> &std::path::Path {
            &self.root
        }

        fn scope_segments(&self) -> Option<(String, String)> {
            None
        }

        fn agent_tiers_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError> {
            Ok(self.root.join("agents").join(agent_id))
        }

        fn agent_episodes_dir(&self, agent_id: &str) -> Result<PathBuf, MemoryStorageError> {
            Ok(self.root.join("agents").join(agent_id).join("episodes"))
        }

        fn agent_tier_path(
            &self,
            agent_id: &str,
            tier_name: &str,
            _scope: &TierScope,
            _goal_id: Option<&str>,
        ) -> Result<PathBuf, MemoryStorageError> {
            Ok(self
                .agent_tiers_dir(agent_id)?
                .join(format!("{tier_name}.json")))
        }

        fn memory_lancedb_index_dir(&self) -> PathBuf {
            self.root.join("index").join("lancedb")
        }

        fn memory_index_documents_path(&self) -> PathBuf {
            self.root.join("index").join("documents.jsonl")
        }

        fn memory_index_manifest_path(&self) -> PathBuf {
            self.root.join("index").join("manifest.json")
        }

        fn user_knowledge_path(&self) -> PathBuf {
            self.root.join("users").join("knowledge.json")
        }

        fn user_root(&self) -> PathBuf {
            self.root.join("users")
        }

        async fn load_user_knowledge(&self) -> Result<Value, MemoryStorageError> {
            Ok(serde_json::json!({}))
        }

        async fn load_native_tier_value(
            &self,
            _agent_id: &str,
            _tier_name: &str,
            _scope: &TierScope,
            _goal_id: Option<&str>,
        ) -> Result<Option<Value>, MemoryStorageError> {
            Ok(None)
        }

        async fn read_json_value(
            &self,
            path: &std::path::Path,
        ) -> Result<Value, MemoryStorageError> {
            let bytes = tokio::fs::read(path).await?;
            Ok(serde_json::from_slice(&bytes)?)
        }

        async fn write_bytes_atomic(
            &self,
            path: &std::path::Path,
            bytes: &[u8],
        ) -> Result<(), MemoryStorageError> {
            if path.file_name().and_then(|name| name.to_str())
                == Some(MEMORY_TEMPERATURE_OVERLAY_FILE)
            {
                if self.fail_overlay_writes.load(AtomicOrdering::SeqCst) {
                    return Err(MemoryStorageError::Io(std::io::Error::other(
                        "injected overlay write failure",
                    )));
                }
                self.overlay_writes.fetch_add(1, AtomicOrdering::SeqCst);
            }
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            let counter = self.tmp_counter.fetch_add(1, AtomicOrdering::SeqCst);
            let tmp = path.with_extension(format!("tmp{counter}"));
            tokio::fs::write(&tmp, bytes).await?;
            tokio::fs::rename(&tmp, path).await?;
            Ok(())
        }

        async fn write_json_value_atomic(
            &self,
            path: &std::path::Path,
            value: &Value,
        ) -> Result<(), MemoryStorageError> {
            let bytes = serde_json::to_vec_pretty(value)?;
            self.write_bytes_atomic(path, &bytes).await
        }

        fn shared_handle(&self) -> Option<Arc<dyn MemoryStorage>> {
            Some(Arc::new(self.clone()))
        }
    }

    async fn read_overlay_from_disk(storage: &dyn MemoryStorage) -> MemoryTemperatureOverlay {
        let bytes = tokio::fs::read(memory_temperature_overlay_path(storage))
            .await
            .expect("overlay file");
        serde_json::from_slice(&bytes).expect("overlay json")
    }

    /// A cycle whose mutation changes nothing must not serialize or rewrite
    /// the overlay, and must hand back the committed snapshot itself.
    #[tokio::test]
    async fn an_unchanged_overlay_is_not_rewritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CountingOverlayStorage::new(dir.path().to_path_buf());
        let candidate = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "unchanged");

        sync_memory_temperature_overlay(&storage, std::slice::from_ref(&candidate))
            .await
            .expect("first sync");
        assert_eq!(
            storage.overlay_writes(),
            1,
            "the first sync creates the entry"
        );
        let committed = load_memory_temperature_overlay_snapshot(&storage)
            .await
            .expect("snapshot");
        let bytes_before = tokio::fs::read(memory_temperature_overlay_path(&storage))
            .await
            .expect("overlay file");

        let resynced = sync_memory_temperature_overlay_shared(&storage, Arc::from(vec![candidate]))
            .await
            .expect("second sync");
        maintain_memory_temperature_overlay(&storage)
            .await
            .expect("maintenance");
        let usage = record_memory_temperature_prompt_usage(
            &storage,
            &["mt1:not-in-the-overlay".to_string()],
            &BTreeSet::new(),
        )
        .await
        .expect("prompt usage");
        assert_eq!(usage.missing, 1);
        record_memory_temperature_outcome_usage(
            &storage,
            &[],
            MemoryTemperatureOutcomeSignal::Successful,
        )
        .await
        .expect("outcome usage");

        assert_eq!(
            storage.overlay_writes(),
            1,
            "no-op cycles must not rewrite the overlay"
        );
        assert!(Arc::ptr_eq(&resynced, &committed));
        assert_eq!(
            tokio::fs::read(memory_temperature_overlay_path(&storage))
                .await
                .expect("overlay file"),
            bytes_before
        );
    }

    /// The prompt-repair sync, the prompt-usage worker and chat outcome
    /// feedback race each other within one chat turn. Every update must land,
    /// on disk and in the snapshot readers are served.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_updates_from_the_three_turn_paths_are_all_reflected() {
        const ROUNDS: u32 = 8;
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = Arc::new(CountingOverlayStorage::new(dir.path().to_path_buf()));
        let first = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "first");
        let second = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "second");
        let added = test_candidate(TierScope::Agent, Some("cto"), None, "tier", "added");
        let first_key = memory_temperature_candidate_key(&first);
        let second_key = memory_temperature_candidate_key(&second);
        let added_key = memory_temperature_candidate_key(&added);
        sync_memory_temperature_overlay(storage.as_ref(), &[first.clone(), second.clone()])
            .await
            .expect("seed sync");

        let mut tasks = Vec::new();
        for round in 0..ROUNDS {
            let usage_storage = Arc::clone(&storage);
            let retrieved = vec![first_key.clone(), second_key.clone()];
            let selected = BTreeSet::from([first_key.clone()]);
            tasks.push(tokio::spawn(async move {
                record_memory_temperature_prompt_usage(
                    usage_storage.as_ref(),
                    &retrieved,
                    &selected,
                )
                .await
                .map(|_| ())
            }));
            let outcome_storage = Arc::clone(&storage);
            let used = vec![second_key.clone()];
            tasks.push(tokio::spawn(async move {
                record_memory_temperature_outcome_usage(
                    outcome_storage.as_ref(),
                    &used,
                    MemoryTemperatureOutcomeSignal::Successful,
                )
                .await
                .map(|_| ())
            }));
            if round == ROUNDS / 2 {
                let sync_storage = Arc::clone(&storage);
                let candidates: Arc<[MemoryCandidateDocument]> =
                    Arc::from(vec![first.clone(), second.clone(), added.clone()]);
                tasks.push(tokio::spawn(async move {
                    sync_memory_temperature_overlay_shared(sync_storage.as_ref(), candidates)
                        .await
                        .map(|_| ())
                }));
            }
        }
        for task in tasks {
            task.await.expect("task").expect("overlay update");
        }
        // Counter updates after the mid-run sync are still write-behind.
        flush_pending_memory_temperature_writes_for(storage.as_ref())
            .await
            .expect("flush pending counters");

        let on_disk = read_overlay_from_disk(storage.as_ref()).await;
        let first_entry = &on_disk.entries[&first_key];
        assert_eq!(first_entry.retrieved_count, ROUNDS);
        assert_eq!(first_entry.selected_count, ROUNDS);
        assert_eq!(first_entry.injected_count, ROUNDS);
        let second_entry = &on_disk.entries[&second_key];
        assert_eq!(second_entry.retrieved_count, ROUNDS);
        assert_eq!(second_entry.selected_count, 0);
        assert_eq!(second_entry.successful_use_count, ROUNDS);
        assert!(on_disk.entries.contains_key(&added_key));

        let served = load_memory_temperature_overlay_snapshot(storage.as_ref())
            .await
            .expect("snapshot");
        assert_eq!(
            serde_json::to_value(&*served).expect("served"),
            serde_json::to_value(&on_disk).expect("on disk"),
            "readers must be served exactly the committed overlay"
        );
    }

    /// Seed one entry through the durable sync and return its key and the
    /// overlay write count afterwards.
    async fn seed_counter_test_overlay(
        storage: &CountingOverlayStorage,
        name: &str,
    ) -> (String, usize) {
        let candidate = test_candidate(TierScope::Agent, Some("cto"), None, "tier", name);
        let key = memory_temperature_candidate_key(&candidate);
        sync_memory_temperature_overlay(storage, std::slice::from_ref(&candidate))
            .await
            .expect("seed sync");
        (key, storage.overlay_writes())
    }

    /// Counter updates inside one window are applied in memory at once and
    /// persisted by a single overlay write that carries all of them.
    #[tokio::test]
    async fn counter_updates_within_the_window_coalesce_into_one_write() {
        const UPDATES: u32 = 5;
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CountingOverlayStorage::new(dir.path().to_path_buf());
        let (key, seeded) = seed_counter_test_overlay(&storage, "coalesced").await;

        for _ in 0..UPDATES {
            record_memory_temperature_prompt_usage(
                &storage,
                std::slice::from_ref(&key),
                &BTreeSet::from([key.clone()]),
            )
            .await
            .expect("prompt usage");
            record_memory_temperature_outcome_usage(
                &storage,
                std::slice::from_ref(&key),
                MemoryTemperatureOutcomeSignal::Successful,
            )
            .await
            .expect("outcome usage");
        }
        assert_eq!(
            storage.overlay_writes(),
            seeded,
            "counter updates must not write through"
        );
        assert_eq!(
            memory_temperature_pending_counter_mutations(&storage),
            (UPDATES * 2) as usize
        );
        let served = load_memory_temperature_overlay_snapshot(&storage)
            .await
            .expect("snapshot");
        assert_eq!(served.entries[&key].retrieved_count, UPDATES);
        assert_eq!(
            served.entries[&key].successful_use_count, UPDATES,
            "readers in this process see pending counters"
        );

        let flushed = flush_pending_memory_temperature_writes_for(&storage)
            .await
            .expect("flush");
        assert_eq!(flushed, (UPDATES * 2) as usize);
        assert_eq!(storage.overlay_writes(), seeded + 1, "one write per window");
        let on_disk = read_overlay_from_disk(&storage).await;
        let entry = &on_disk.entries[&key];
        assert_eq!(entry.retrieved_count, UPDATES);
        assert_eq!(entry.selected_count, UPDATES);
        assert_eq!(entry.injected_count, UPDATES);
        assert_eq!(entry.successful_use_count, UPDATES);
        assert_eq!(memory_temperature_pending_counter_mutations(&storage), 0);

        assert_eq!(
            flush_pending_memory_temperature_writes_for(&storage)
                .await
                .expect("second flush"),
            0
        );
        assert_eq!(
            storage.overlay_writes(),
            seeded + 1,
            "nothing left to write"
        );
    }

    /// A durable write in the middle of a window persists the pending counters
    /// with it, exactly once; the later flush must not apply them again.
    #[tokio::test]
    async fn a_durable_write_carries_pending_counters_exactly_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CountingOverlayStorage::new(dir.path().to_path_buf());
        let (key, seeded) = seed_counter_test_overlay(&storage, "folded").await;

        for _ in 0..3 {
            record_memory_temperature_retrieval_usage(&storage, std::slice::from_ref(&key))
                .await
                .expect("retrieval usage");
        }
        record_memory_temperature_utility_review(
            &storage,
            "review-folds-counters",
            &[MemoryTemperatureUtilityReviewJudgement {
                memory_candidate_key: key.clone(),
                label: MemoryTemperatureUtilityLabel::Useful,
                confidence: None,
                reason: None,
                compact_text: None,
            }],
        )
        .await
        .expect("utility review");
        assert_eq!(storage.overlay_writes(), seeded + 1);
        assert_eq!(memory_temperature_pending_counter_mutations(&storage), 0);
        let on_disk = read_overlay_from_disk(&storage).await;
        assert_eq!(on_disk.entries[&key].retrieved_count, 3);
        assert_eq!(on_disk.entries[&key].reviewed_useful_count, 1);
        assert!(on_disk
            .applied_utility_review_runs
            .contains_key("review-folds-counters"));

        assert_eq!(
            flush_pending_memory_temperature_writes_for(&storage)
                .await
                .expect("flush"),
            0,
            "the durable write settled the pending counters"
        );
        record_memory_temperature_retrieval_usage(&storage, std::slice::from_ref(&key))
            .await
            .expect("retrieval usage");
        flush_pending_memory_temperature_writes_for(&storage)
            .await
            .expect("flush");
        let on_disk = read_overlay_from_disk(&storage).await;
        assert_eq!(
            on_disk.entries[&key].retrieved_count, 4,
            "applied once each"
        );
        assert_eq!(storage.overlay_writes(), seeded + 2);
    }

    /// A failed flush keeps every pending update, still visible to readers,
    /// and the next attempt writes them once.
    #[tokio::test]
    async fn a_failed_flush_retains_pending_counters_for_the_next_attempt() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CountingOverlayStorage::new(dir.path().to_path_buf());
        let (key, seeded) = seed_counter_test_overlay(&storage, "retained").await;

        for _ in 0..2 {
            record_memory_temperature_outcome_usage(
                &storage,
                std::slice::from_ref(&key),
                MemoryTemperatureOutcomeSignal::Failed,
            )
            .await
            .expect("outcome usage");
        }
        storage.fail_overlay_writes(true);
        flush_pending_memory_temperature_writes_for(&storage)
            .await
            .expect_err("injected write failure");
        assert_eq!(memory_temperature_pending_counter_mutations(&storage), 2);
        assert_eq!(storage.overlay_writes(), seeded);
        assert_eq!(
            read_overlay_from_disk(&storage).await.entries[&key].failed_use_count,
            0
        );
        let served = load_memory_temperature_overlay_snapshot(&storage)
            .await
            .expect("snapshot");
        assert_eq!(served.entries[&key].failed_use_count, 2);

        storage.fail_overlay_writes(false);
        assert_eq!(
            flush_pending_memory_temperature_writes_for(&storage)
                .await
                .expect("flush"),
            2
        );
        assert_eq!(storage.overlay_writes(), seeded + 1);
        assert_eq!(
            read_overlay_from_disk(&storage).await.entries[&key].failed_use_count,
            2
        );
        assert_eq!(memory_temperature_pending_counter_mutations(&storage), 0);
    }

    /// Another process rewriting the file under pending updates displaces the
    /// revision they were applied to; the flush replays them onto the new
    /// file, once, keeping that process's change.
    #[tokio::test]
    async fn pending_counters_are_replayed_once_onto_an_externally_rewritten_overlay() {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage = CountingOverlayStorage::new(dir.path().to_path_buf());
        let (key, _) = seed_counter_test_overlay(&storage, "replayed").await;

        for _ in 0..2 {
            record_memory_temperature_retrieval_usage(&storage, std::slice::from_ref(&key))
                .await
                .expect("retrieval usage");
        }
        let mut external = read_overlay_from_disk(&storage).await;
        external
            .applied_utility_review_runs
            .insert("external-run".to_string(), Utc::now());
        tokio::fs::write(
            memory_temperature_overlay_path(&storage),
            serde_json::to_vec_pretty(&external).expect("external json"),
        )
        .await
        .expect("external rewrite");

        assert_eq!(
            flush_pending_memory_temperature_writes_for(&storage)
                .await
                .expect("flush"),
            2
        );
        let on_disk = read_overlay_from_disk(&storage).await;
        assert_eq!(on_disk.entries[&key].retrieved_count, 2);
        assert!(on_disk
            .applied_utility_review_runs
            .contains_key("external-run"));
        let served = load_memory_temperature_overlay_snapshot(&storage)
            .await
            .expect("snapshot");
        assert_eq!(served.entries[&key].retrieved_count, 2);
    }

    /// Saves now serialize the overlay directly instead of through a
    /// `serde_json::Value`. Only object field order may differ; the parsed
    /// document must be identical.
    #[test]
    fn direct_overlay_encoding_matches_the_former_value_encoding() {
        let now = Utc.with_ymd_and_hms(2026, 10, 28, 12, 0, 0).unwrap();
        let key = "mt1:5:agent:3:cto:0::4:tier:1:0".to_string();
        let mut entry = test_entry(&key, SemanticMemoryType::ProjectContext, now);
        entry.retrieved_count = 3;
        entry.last_used_at = Some(now);
        entry.last_utility_review_label = Some(MemoryTemperatureUtilityLabel::LoadBearing);
        entry.temperature_score = 0.6125;
        let mut overlay = MemoryTemperatureOverlay::default();
        overlay.entries.insert(key, entry);
        overlay
            .applied_utility_review_runs
            .insert("review-1".to_string(), now);

        let direct: Value = serde_json::from_slice(
            &serialize_memory_temperature_overlay(&overlay).expect("direct"),
        )
        .expect("direct json");
        let former: Value = serde_json::from_slice(
            &serde_json::to_vec_pretty(&serde_json::to_value(&overlay).expect("value"))
                .expect("former"),
        )
        .expect("former json");
        assert_eq!(direct, former);
    }

    fn test_entry(
        key: &str,
        semantic_memory_type: SemanticMemoryType,
        now: DateTime<Utc>,
    ) -> MemoryTemperatureEntry {
        MemoryTemperatureEntry {
            memory_candidate_key: key.to_string(),
            semantic_memory_type,
            temperature_tier: default_temperature_tier(semantic_memory_type),
            temperature_score: default_temperature_score(semantic_memory_type),
            confidence: Some(0.9),
            last_retrieved_at: None,
            last_selected_at: None,
            last_injected_at: None,
            last_used_at: None,
            retrieved_count: 0,
            selected_count: 0,
            injected_count: 0,
            successful_use_count: 0,
            failed_use_count: 0,
            reviewed_referenced_count: 0,
            reviewed_useful_count: 0,
            reviewed_load_bearing_count: 0,
            reviewed_irrelevant_count: 0,
            reviewed_stale_count: 0,
            reviewed_harmful_count: 0,
            source_ids: Vec::new(),
            superseded_by: None,
            superseded_at: None,
            supersession_reason: None,
            supersession_confidence: None,
            supersession_source: None,
            supersedes: Vec::new(),
            first_seen_at: Some(now),
            last_temperature_review_at: None,
            last_temperature_change_reason: None,
            last_utility_review_at: None,
            last_utility_review_run_id: None,
            last_utility_review_label: None,
            last_utility_review_confidence: None,
            last_utility_review_reason: None,
            updated_at: now,
        }
    }
}

//! Revision-bound hybrid score-map cache and singleflight.
//!
//! Crate `Default` stays off so unit tests that never install Magician config
//! still pass through. Magician YAML default is on. A hit is the frozen-index
//! 100× path: the caller skips journal I/O, manifest inspect, query embedding,
//! Lance setup, and both search legs.
//! The cached value is the complete revision-bound score map and status, never
//! prompt text. Failures, timeouts, direct fallbacks, stale overlays, and
//! cancelled leaders are not retained.

use std::{
    collections::{BTreeMap, HashMap},
    env,
    future::Future,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Instant,
};

use anyhow::{anyhow, Result};
use tokio::sync::Notify;

use crate::hol_stats;

/// Flat and ANN-shadow served score maps share this contract. ANN activation
/// uses `base + 1` via [`crate::vector_search_mode::served_hybrid_scoring_contract`]
/// so an in-process cache cannot mix flat and ANN payloads. IVF create and
/// ANN parameter changes bump [`crate::vector_search_mode::vector_search_ranking_epoch`]
/// which is also part of [`HybridScoreCacheKey`].
pub const MEMORY_HYBRID_SCORE_CONTRACT_VERSION: u32 = 1;
pub const DEFAULT_HYBRID_RESULT_CACHE_MAX_ENTRIES: usize = 512;
pub const DEFAULT_HYBRID_RESULT_CACHE_MAX_BYTES: usize = 64 * 1024 * 1024;

/// Serializes tests that mutate the process-global hybrid result cache.
#[doc(hidden)]
pub static HYBRID_RESULT_CACHE_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HybridResultCacheSettings {
    pub enabled: bool,
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl Default for HybridResultCacheSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: DEFAULT_HYBRID_RESULT_CACHE_MAX_ENTRIES,
            max_bytes: DEFAULT_HYBRID_RESULT_CACHE_MAX_BYTES,
        }
    }
}

/// Complete cached hybrid score payload. Prompt text is never stored.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedHybridScore {
    pub scores: Option<BTreeMap<String, f32>>,
    pub fallback_reason: Option<String>,
    pub stale: bool,
    pub current_document_count: usize,
    /// When false the value is returned to in-flight followers but not
    /// retained. Used when index generation moved during the load.
    pub retain: bool,
}

impl CachedHybridScore {
    pub fn cacheable(&self) -> bool {
        self.retain && self.scores.is_some() && self.fallback_reason.is_none() && !self.stale
    }

    pub fn retained_bytes(&self) -> usize {
        const OVERHEAD: usize = 128;
        let scores = self
            .scores
            .as_ref()
            .map(|scores| {
                scores
                    .iter()
                    .map(|(key, _)| key.len().saturating_add(16))
                    .sum::<usize>()
            })
            .unwrap_or(0);
        let reason = self.fallback_reason.as_ref().map(String::len).unwrap_or(0);
        OVERHEAD.saturating_add(scores).saturating_add(reason)
    }
}

/// Exact-match cache identity. No whitespace or semantic normalization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HybridScoreCacheKey {
    pub storage_root: PathBuf,
    pub predicate: String,
    pub predicate_present: bool,
    pub expanded_query: String,
    pub embedding_contract_id: String,
    pub index_generation: String,
    pub pending_identity: String,
    pub search_limit: usize,
    pub scoring_contract: u32,
    /// Isolation for ANN parameter changes and IVF_PQ create, which do not
    /// bump the manifest generation.
    pub ranking_epoch: u32,
    pub candidate_multiplier: usize,
    pub nprobes: usize,
}

struct ReadyEntry {
    value: Arc<CachedHybridScore>,
    bytes: usize,
    last_access: Instant,
}

struct PublishedGeneration {
    token: String,
    rebuilt_at_nanos: i64,
}

struct InFlight {
    outcome: Mutex<Option<SharedOutcome>>,
    notify: Notify,
}

enum SharedOutcome {
    Ready(Arc<CachedHybridScore>),
    Failed(Arc<str>),
    Retry,
}

struct CacheState {
    settings: HybridResultCacheSettings,
    ready: HashMap<HybridScoreCacheKey, ReadyEntry>,
    inflight: HashMap<HybridScoreCacheKey, Arc<InFlight>>,
    generations: HashMap<PathBuf, PublishedGeneration>,
    live_embedding_contract: Option<String>,
    retained_bytes: usize,
}

impl CacheState {
    fn new(settings: HybridResultCacheSettings) -> Self {
        Self {
            settings,
            ready: HashMap::new(),
            inflight: HashMap::new(),
            generations: HashMap::new(),
            live_embedding_contract: None,
            retained_bytes: 0,
        }
    }
}

static CACHE_ENABLED: AtomicBool = AtomicBool::new(false);

fn cache_state() -> &'static Mutex<CacheState> {
    static STATE: OnceLock<Mutex<CacheState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CacheState::new(HybridResultCacheSettings::default())))
}

fn lock_state() -> std::sync::MutexGuard<'static, CacheState> {
    cache_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_result_cache_override() -> Option<bool> {
    let value = env::var("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE").ok()?;
    let trimmed = value.trim();
    if trimmed.eq_ignore_ascii_case("off")
        || trimmed.eq_ignore_ascii_case("pass_through")
        || trimmed.eq_ignore_ascii_case("disabled")
        || trimmed.eq_ignore_ascii_case("false")
        || trimmed == "0"
    {
        Some(false)
    } else if trimmed.eq_ignore_ascii_case("on")
        || trimmed.eq_ignore_ascii_case("enabled")
        || trimmed.eq_ignore_ascii_case("true")
        || trimmed == "1"
    {
        Some(true)
    } else {
        None
    }
}

pub fn hybrid_result_cache_enabled() -> bool {
    env_result_cache_override().unwrap_or_else(|| CACHE_ENABLED.load(Ordering::Acquire))
}

pub fn hybrid_result_cache_settings() -> HybridResultCacheSettings {
    let mut settings = lock_state().settings;
    if let Some(enabled) = env_result_cache_override() {
        settings.enabled = enabled;
    }
    settings
}

pub fn install_hybrid_result_cache(settings: HybridResultCacheSettings) {
    let enabled = settings.enabled;
    let mut state = lock_state();
    state.settings = HybridResultCacheSettings {
        enabled,
        max_entries: settings.max_entries.max(1),
        max_bytes: settings.max_bytes.max(1),
    };
    CACHE_ENABLED.store(enabled, Ordering::Release);
    if !enabled {
        state.ready.clear();
        state.retained_bytes = 0;
    } else {
        evict_to_bounds(&mut state);
    }
    publish_occupancy(&state);
}

/// Publish a usable index generation. A later inspect of an older manifest
/// must not roll the token back: compare `rebuilt_at` and keep the newest.
pub fn publish_hybrid_index_generation(root: PathBuf, token: String, rebuilt_at_nanos: i64) {
    let mut state = lock_state();
    if let Some(existing) = state.generations.get(&root) {
        if existing.rebuilt_at_nanos > rebuilt_at_nanos {
            return;
        }
        if existing.rebuilt_at_nanos == rebuilt_at_nanos && existing.token == token {
            return;
        }
    }
    state.generations.insert(
        root,
        PublishedGeneration {
            token,
            rebuilt_at_nanos,
        },
    );
}

pub fn published_hybrid_index_generation(root: &Path) -> Option<String> {
    lock_state()
        .generations
        .get(root)
        .map(|generation| generation.token.clone())
}

/// Drop retained scores and the published generation for a root. Used when
/// inspect-fast proves the derived index is not overlay-usable (missing files,
/// incompatible contract, hard stale). Transient Lance timeouts must not call
/// this.
fn lancedb_dir_for_root(root: &Path) -> PathBuf {
    root.join("index").join("lancedb")
}

/// Drop retained scores and the published generation for every storage root
/// whose Lance directory is `index_dir`. IVF_PQ create changes ANN ranking
/// without a new manifest generation.
pub fn invalidate_hybrid_results_for_lancedb_dir(index_dir: &Path) {
    let mut state = lock_state();
    let mut drop_roots: Vec<PathBuf> = state
        .generations
        .keys()
        .filter(|root| lancedb_dir_for_root(root) == index_dir)
        .cloned()
        .collect();
    let mut dropped_bytes = 0usize;
    state.ready.retain(|key, entry| {
        if lancedb_dir_for_root(&key.storage_root) == index_dir {
            dropped_bytes = dropped_bytes.saturating_add(entry.bytes);
            if !drop_roots.iter().any(|root| root == &key.storage_root) {
                drop_roots.push(key.storage_root.clone());
            }
            false
        } else {
            true
        }
    });
    state.retained_bytes = state.retained_bytes.saturating_sub(dropped_bytes);
    for root in drop_roots {
        state.generations.remove(&root);
    }
    publish_occupancy(&state);
}

pub fn invalidate_hybrid_results_for_root(root: &Path) {
    let mut state = lock_state();
    let mut dropped_bytes = 0usize;
    state.ready.retain(|key, entry| {
        if key.storage_root.as_path() == root {
            dropped_bytes = dropped_bytes.saturating_add(entry.bytes);
            false
        } else {
            true
        }
    });
    state.retained_bytes = state.retained_bytes.saturating_sub(dropped_bytes);
    state.generations.remove(root);
    publish_occupancy(&state);
}

pub fn cached_live_embedding_contract(compute: impl FnOnce() -> String) -> String {
    {
        let state = lock_state();
        if let Some(existing) = state.live_embedding_contract.as_ref() {
            return existing.clone();
        }
    }
    let computed = compute();
    let mut state = lock_state();
    if let Some(existing) = state.live_embedding_contract.as_ref() {
        return existing.clone();
    }
    state.live_embedding_contract = Some(computed.clone());
    computed
}

pub fn clear_live_embedding_contract() {
    lock_state().live_embedding_contract = None;
}

pub fn reset_hybrid_result_cache_for_tests() {
    CACHE_ENABLED.store(false, Ordering::Release);
    let mut state = lock_state();
    *state = CacheState::new(HybridResultCacheSettings::default());
    publish_occupancy(&state);
}

enum Join {
    Hit(Arc<CachedHybridScore>),
    Follower(Arc<InFlight>),
    Leader(LeaderGuard),
}

struct LeaderGuard {
    key: HybridScoreCacheKey,
    inflight: Arc<InFlight>,
    finished: bool,
}

impl LeaderGuard {
    fn finish(mut self, outcome: SharedOutcome) {
        {
            let mut slot = self
                .inflight
                .outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            *slot = Some(outcome);
        }
        self.inflight.notify.notify_waiters();
        let mut state = lock_state();
        state.inflight.remove(&self.key);
        self.finished = true;
        // Drop after releasing the cache lock so a waiter that becomes the
        // next leader does not pile onto this mutex.
        drop(state);
    }
}

impl Drop for LeaderGuard {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        {
            let mut slot = self
                .inflight
                .outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_none() {
                *slot = Some(SharedOutcome::Retry);
            }
        }
        self.inflight.notify.notify_waiters();
        let mut state = lock_state();
        state.inflight.remove(&self.key);
        hol_stats::record_result_cache_leader_cancel();
    }
}

fn try_join(key: &HybridScoreCacheKey) -> Join {
    let mut state = lock_state();
    if let Some(entry) = state.ready.get_mut(key) {
        entry.last_access = Instant::now();
        let value = Arc::clone(&entry.value);
        hol_stats::record_result_cache_hit();
        return Join::Hit(value);
    }
    if let Some(inflight) = state.inflight.get(key) {
        hol_stats::record_result_cache_singleflight_join();
        return Join::Follower(Arc::clone(inflight));
    }
    hol_stats::record_result_cache_miss();
    let inflight = Arc::new(InFlight {
        outcome: Mutex::new(None),
        notify: Notify::new(),
    });
    state.inflight.insert(key.clone(), Arc::clone(&inflight));
    Join::Leader(LeaderGuard {
        key: key.clone(),
        inflight,
        finished: false,
    })
}

enum Wait {
    Ready(Arc<CachedHybridScore>),
    Failed(Arc<str>),
    Retry,
}

async fn wait_inflight(inflight: &InFlight) -> Wait {
    hol_stats::result_cache_wait_begin();
    struct WaiterGuard;
    impl Drop for WaiterGuard {
        fn drop(&mut self) {
            hol_stats::result_cache_wait_end();
        }
    }
    let _guard = WaiterGuard;
    loop {
        // `notify_waiters` does not store a permit. Enable the waiter before
        // reading the outcome so a notify that lands between the check and
        // the await cannot be lost.
        let notified = inflight.notify.notified();
        tokio::pin!(notified);
        let _ = notified.as_mut().enable();
        {
            let outcome = inflight
                .outcome
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            match outcome.as_ref() {
                Some(SharedOutcome::Ready(value)) => return Wait::Ready(Arc::clone(value)),
                Some(SharedOutcome::Failed(message)) => {
                    return Wait::Failed(Arc::clone(message));
                },
                Some(SharedOutcome::Retry) => return Wait::Retry,
                None => {},
            }
        }
        notified.await;
    }
}

fn store_ready(key: HybridScoreCacheKey, value: Arc<CachedHybridScore>) {
    let bytes = value.retained_bytes();
    let mut state = lock_state();
    match state.generations.get(&key.storage_root) {
        Some(generation) if generation.token == key.index_generation => {},
        _ => return,
    }
    if let Some(previous) = state.ready.remove(&key) {
        state.retained_bytes = state.retained_bytes.saturating_sub(previous.bytes);
    }
    state.ready.insert(
        key,
        ReadyEntry {
            value,
            bytes,
            last_access: Instant::now(),
        },
    );
    state.retained_bytes = state.retained_bytes.saturating_add(bytes);
    hol_stats::record_result_cache_store();
    evict_to_bounds(&mut state);
    publish_occupancy(&state);
}

fn evict_to_bounds(state: &mut CacheState) {
    let max_entries = state.settings.max_entries.max(1);
    let max_bytes = state.settings.max_bytes.max(1);
    while (state.ready.len() > max_entries || state.retained_bytes > max_bytes)
        && state.ready.len() > 1
    {
        let oldest = state
            .ready
            .iter()
            .min_by_key(|(_, entry)| entry.last_access)
            .map(|(key, _)| key.clone());
        let Some(oldest) = oldest else {
            break;
        };
        if let Some(entry) = state.ready.remove(&oldest) {
            state.retained_bytes = state.retained_bytes.saturating_sub(entry.bytes);
            hol_stats::record_result_cache_eviction();
        }
    }
}

fn publish_occupancy(state: &CacheState) {
    hol_stats::set_result_cache_occupancy(state.ready.len(), state.retained_bytes);
}

/// Insert a cacheable result under an already-correct key. Used for the
/// cold-start miss that discovers generation only after inspect. Uncacheable
/// values are ignored.
pub fn store_cacheable(key: HybridScoreCacheKey, value: CachedHybridScore) {
    if !hybrid_result_cache_enabled() || !value.cacheable() {
        return;
    }
    store_ready(key, Arc::new(value));
}

/// Load through singleflight. Successful cacheable results are retained until
/// LRU/bytes evict them. Uncacheable successes are shared with in-flight
/// followers only. Errors are not retained. A cancelled leader does not fail
/// followers: they retry as a new leader.
pub async fn get_or_load<F, Fut>(key: HybridScoreCacheKey, load: F) -> Result<CachedHybridScore>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<CachedHybridScore>>,
{
    if !hybrid_result_cache_enabled() {
        return load().await;
    }
    let mut load = Some(load);
    loop {
        match try_join(&key) {
            Join::Hit(value) => return Ok((*value).clone()),
            Join::Follower(inflight) => match wait_inflight(&inflight).await {
                Wait::Ready(value) => return Ok((*value).clone()),
                Wait::Failed(message) => return Err(anyhow!("{}", message)),
                Wait::Retry => continue,
            },
            Join::Leader(guard) => {
                let loader = load.take().ok_or_else(|| {
                    anyhow!("hybrid result cache leader retry exhausted the loader")
                })?;
                match loader().await {
                    Ok(value) => {
                        let shared = Arc::new(value);
                        if shared.cacheable() {
                            store_ready(key.clone(), Arc::clone(&shared));
                        }
                        guard.finish(SharedOutcome::Ready(Arc::clone(&shared)));
                        return Ok((*shared).clone());
                    },
                    Err(error) => {
                        let message: Arc<str> = format!("{error:#}").into();
                        guard.finish(SharedOutcome::Failed(Arc::clone(&message)));
                        return Err(error);
                    },
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    fn publish_for(key: &HybridScoreCacheKey) {
        publish_hybrid_index_generation(key.storage_root.clone(), key.index_generation.clone(), 1);
    }

    fn test_key(query: &str) -> HybridScoreCacheKey {
        HybridScoreCacheKey {
            storage_root: PathBuf::from("/tmp/cache-root"),
            predicate: String::new(),
            predicate_present: false,
            expanded_query: query.to_string(),
            embedding_contract_id: "contract-a".to_string(),
            index_generation: "gen-1".to_string(),
            pending_identity: "pending-0".to_string(),
            search_limit: 128,
            scoring_contract: MEMORY_HYBRID_SCORE_CONTRACT_VERSION,
            ranking_epoch: 0,
            candidate_multiplier: 4,
            nprobes: 20,
        }
    }

    fn score(id: &str, value: f32) -> CachedHybridScore {
        let mut scores = BTreeMap::new();
        scores.insert(id.to_string(), value);
        CachedHybridScore {
            scores: Some(scores),
            fallback_reason: None,
            stale: false,
            current_document_count: 1,
            retain: true,
        }
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = env::var(key).ok();
            env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = env::var(key).ok();
            env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => env::set_var(self.key, value),
                None => env::remove_var(self.key),
            }
        }
    }

    #[tokio::test]
    async fn default_is_pass_through_and_does_not_store() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::unset("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE");
        let calls = Arc::new(AtomicUsize::new(0));
        let key = test_key("q");
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let loaded = get_or_load(key.clone(), || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(score("a", 1.0))
                }
            })
            .await
            .expect("load");
            assert_eq!(loaded, score("a", 1.0));
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(lock_state().ready.len(), 0);
    }

    #[tokio::test]
    async fn enabled_cache_reuses_exact_key_and_isolates_components() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let key = test_key("q");
        publish_for(&key);
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            get_or_load(key.clone(), || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(score("a", 1.0))
                }
            })
            .await
            .expect("load");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.expanded_query = "q2".to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("b", 2.0))
            }
        })
        .await
        .expect("other query");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.predicate_present = true;
        other.predicate = "agent:a".to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("c", 3.0))
            }
        })
        .await
        .expect("predicate");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.embedding_contract_id = "contract-b".to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("d", 4.0))
            }
        })
        .await
        .expect("contract");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.index_generation = "gen-2".to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("e", 5.0))
            }
        })
        .await
        .expect("generation");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.pending_identity = "pending-1".to_string();
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("f", 6.0))
            }
        })
        .await
        .expect("pending");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.search_limit = 64;
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("g", 7.0))
            }
        })
        .await
        .expect("limit");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.scoring_contract = MEMORY_HYBRID_SCORE_CONTRACT_VERSION + 1;
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other, || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("h", 8.0))
            }
        })
        .await
        .expect("scoring contract");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.ranking_epoch = 1;
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other, || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("h-epoch", 8.5))
            }
        })
        .await
        .expect("ranking epoch");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.candidate_multiplier = 8;
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other, || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("h-mult", 8.6))
            }
        })
        .await
        .expect("candidate multiplier");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key.clone();
        other.nprobes = 40;
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other, || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("h-nprobes", 8.7))
            }
        })
        .await
        .expect("nprobes");
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut other = key;
        other.storage_root = PathBuf::from("/tmp/other-root");
        let calls = Arc::new(AtomicUsize::new(0));
        get_or_load(other, || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("i", 9.0))
            }
        })
        .await
        .expect("root");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn failures_and_stale_results_are_not_retained() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let key = test_key("fail");
        let calls = Arc::new(AtomicUsize::new(0));
        let err = get_or_load(key.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow!("timeout"))
            }
        })
        .await
        .expect_err("failure");
        assert!(err.to_string().contains("timeout"));
        get_or_load(key.clone(), || {
            let calls = Arc::clone(&calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Err(anyhow!("timeout"))
            }
        })
        .await
        .expect_err("second failure");
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let stale = CachedHybridScore {
            scores: Some(BTreeMap::new()),
            fallback_reason: None,
            stale: true,
            current_document_count: 1,
            retain: true,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let stale_key = test_key("stale");
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let stale = stale.clone();
            get_or_load(stale_key.clone(), || {
                let calls = Arc::clone(&calls);
                let stale = stale.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(stale)
                }
            })
            .await
            .expect("stale");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let fallback = CachedHybridScore {
            scores: None,
            fallback_reason: Some("memory_index_stale:pending_changes".to_string()),
            stale: true,
            current_document_count: 1,
            retain: true,
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let fallback_key = test_key("fallback");
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let fallback = fallback.clone();
            get_or_load(fallback_key.clone(), || {
                let calls = Arc::clone(&calls);
                let fallback = fallback.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(fallback)
                }
            })
            .await
            .expect("fallback");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(lock_state().ready.is_empty());
    }

    #[tokio::test]
    async fn singleflight_shares_one_load_and_cancelled_leader_recovers() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let key = test_key("shared");
        publish_for(&key);
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new(tokio::sync::Notify::new());
        let entered = Arc::new(tokio::sync::Notify::new());

        let leader_calls = Arc::clone(&calls);
        let leader_release = Arc::clone(&release);
        let leader_entered = Arc::clone(&entered);
        let leader_key = key.clone();
        let leader = tokio::spawn(async move {
            get_or_load(leader_key, || {
                let calls = Arc::clone(&leader_calls);
                let release = Arc::clone(&leader_release);
                let entered = Arc::clone(&leader_entered);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_waiters();
                    release.notified().await;
                    Ok(score("a", 1.0))
                }
            })
            .await
        });
        entered.notified().await;

        let follower_calls = Arc::clone(&calls);
        let follower_key = key.clone();
        let follower = tokio::spawn(async move {
            get_or_load(follower_key, || {
                let calls = Arc::clone(&follower_calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(score("a", 1.0))
                }
            })
            .await
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        release.notify_waiters();
        let leader_result = leader.await.expect("leader join").expect("leader");
        let follower_result = follower.await.expect("follower join").expect("follower");
        assert_eq!(leader_result, follower_result);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let cancel_key = test_key("cancel");
        publish_for(&cancel_key);
        let cancel_calls = Arc::new(AtomicUsize::new(0));
        let cancel_entered = Arc::new(tokio::sync::Notify::new());
        let leader_calls = Arc::clone(&cancel_calls);
        let leader_entered = Arc::clone(&cancel_entered);
        let leader_key = cancel_key.clone();
        let cancelled_leader = tokio::spawn(async move {
            get_or_load(leader_key, || {
                let calls = Arc::clone(&leader_calls);
                let entered = Arc::clone(&leader_entered);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    entered.notify_waiters();
                    std::future::pending::<Result<CachedHybridScore>>().await
                }
            })
            .await
        });
        cancel_entered.notified().await;
        cancelled_leader.abort();
        let _ = cancelled_leader.await;

        let recovered = get_or_load(cancel_key, || {
            let calls = Arc::clone(&cancel_calls);
            async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(score("z", 9.0))
            }
        })
        .await
        .expect("recovered leader");
        assert_eq!(recovered, score("z", 9.0));
        assert!(cancel_calls.load(Ordering::SeqCst) >= 2);
    }

    #[tokio::test]
    async fn byte_and_entry_caps_evict_oldest_and_keep_newest() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            max_entries: 2,
            max_bytes: 10_000,
        });
        publish_for(&test_key("one"));
        for query in ["one", "two", "three"] {
            let key = test_key(query);
            get_or_load(key, || {
                let query = query.to_string();
                async move { Ok(score(&query, 1.0)) }
            })
            .await
            .expect("store");
        }
        assert_eq!(lock_state().ready.len(), 2);
        assert!(!lock_state().ready.contains_key(&test_key("one")));
        assert!(lock_state().ready.contains_key(&test_key("two")));
        assert!(lock_state().ready.contains_key(&test_key("three")));

        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            max_entries: 8,
            max_bytes: 1,
        });
        let huge = score("huge-key-that-is-already-over-budget", 1.0);
        assert!(huge.retained_bytes() > 1);
        get_or_load(test_key("huge"), || {
            let huge = huge.clone();
            async move { Ok(huge) }
        })
        .await
        .expect("huge");
        assert_eq!(lock_state().ready.len(), 1);
        assert!(lock_state().ready.contains_key(&test_key("huge")));
    }

    #[tokio::test]
    async fn env_off_kills_an_enabled_install() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "off");
        assert!(!hybrid_result_cache_enabled());
        let calls = Arc::new(AtomicUsize::new(0));
        let key = test_key("kill");
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            get_or_load(key.clone(), || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(score("a", 1.0))
                }
            })
            .await
            .expect("pass through");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn retain_false_is_shared_with_followers_but_not_stored() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let key = test_key("ephemeral");
        let calls = Arc::new(AtomicUsize::new(0));
        let mut ephemeral = score("a", 1.0);
        ephemeral.retain = false;
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            let ephemeral = ephemeral.clone();
            get_or_load(key.clone(), || {
                let calls = Arc::clone(&calls);
                let ephemeral = ephemeral.clone();
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(ephemeral)
                }
            })
            .await
            .expect("ephemeral");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert!(lock_state().ready.is_empty());
    }

    #[tokio::test]
    async fn generation_publish_is_per_root_and_rejects_older_rebuilt_at() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let a = PathBuf::from("/tmp/a");
        let b = PathBuf::from("/tmp/b");
        publish_hybrid_index_generation(a.clone(), "gen-a".to_string(), 10);
        publish_hybrid_index_generation(b.clone(), "gen-b".to_string(), 10);
        assert_eq!(
            published_hybrid_index_generation(&a).as_deref(),
            Some("gen-a")
        );
        assert_eq!(
            published_hybrid_index_generation(&b).as_deref(),
            Some("gen-b")
        );
        publish_hybrid_index_generation(a.clone(), "gen-a2".to_string(), 11);
        assert_eq!(
            published_hybrid_index_generation(&a).as_deref(),
            Some("gen-a2")
        );
        publish_hybrid_index_generation(a.clone(), "gen-stale".to_string(), 5);
        assert_eq!(
            published_hybrid_index_generation(&a).as_deref(),
            Some("gen-a2"),
            "an older inspect must not roll generation back"
        );
        assert_eq!(
            published_hybrid_index_generation(&b).as_deref(),
            Some("gen-b")
        );
    }

    #[tokio::test]
    async fn hard_stale_invalidation_drops_ready_entries_and_rejects_late_stores() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let key = test_key("keep");
        publish_for(&key);
        get_or_load(key.clone(), || async { Ok(score("a", 1.0)) })
            .await
            .expect("store");
        assert_eq!(lock_state().ready.len(), 1);

        invalidate_hybrid_results_for_root(&key.storage_root);
        assert!(lock_state().ready.is_empty());
        assert!(published_hybrid_index_generation(&key.storage_root).is_none());

        store_cacheable(key.clone(), score("a", 1.0));
        assert!(
            lock_state().ready.is_empty(),
            "a store after invalidation must not resurrect scores"
        );

        let calls = Arc::new(AtomicUsize::new(0));
        for _ in 0..2 {
            let calls = Arc::clone(&calls);
            get_or_load(key.clone(), || {
                let calls = Arc::clone(&calls);
                async move {
                    calls.fetch_add(1, Ordering::SeqCst);
                    Ok(score("a", 1.0))
                }
            })
            .await
            .expect("uncached after invalidate");
        }
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn lancedb_dir_invalidation_drops_matching_root_only() {
        let _lock = HYBRID_RESULT_CACHE_TEST_LOCK.lock().await;
        reset_hybrid_result_cache_for_tests();
        let _env = EnvVarGuard::set("MAGICIAN_MEMORY_HYBRID_RESULT_CACHE", "on");
        install_hybrid_result_cache(HybridResultCacheSettings {
            enabled: true,
            ..HybridResultCacheSettings::default()
        });
        let keep = test_key("keep");
        let mut drop_key = test_key("drop");
        drop_key.storage_root = PathBuf::from("/tmp/ivf-root");
        publish_for(&keep);
        publish_for(&drop_key);
        get_or_load(keep.clone(), || async { Ok(score("k", 1.0)) })
            .await
            .expect("keep");
        get_or_load(drop_key.clone(), || async { Ok(score("d", 1.0)) })
            .await
            .expect("drop");
        assert_eq!(lock_state().ready.len(), 2);

        invalidate_hybrid_results_for_lancedb_dir(
            &drop_key.storage_root.join("index").join("lancedb"),
        );
        assert_eq!(lock_state().ready.len(), 1);
        assert!(published_hybrid_index_generation(&drop_key.storage_root).is_none());
        assert!(published_hybrid_index_generation(&keep.storage_root).is_some());
        store_cacheable(drop_key.clone(), score("d", 1.0));
        assert_eq!(
            lock_state().ready.len(),
            1,
            "IVF invalidation must reject a late store without republishing generation"
        );
    }
}

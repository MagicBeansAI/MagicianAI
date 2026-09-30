//! Bounded exact-query embedding vector LRU.
//!
//! Crate `Default` stays off so unit tests that never install Magician config
//! still pass through. Magician YAML default is on. A hit skips the embedding
//! daemon but still requires Lance.
//! The key is the same contract-plus-query identity the in-flight coalescer
//! uses. Failures are never retained. `MAGICIAN_QUERY_VECTOR_CACHE=off` is
//! the restart-bound kill switch.

use std::{
    collections::HashMap,
    env,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::Instant,
};

use crate::hol_stats;

pub const DEFAULT_QUERY_VECTOR_CACHE_MAX_ENTRIES: usize = 1024;
pub const DEFAULT_QUERY_VECTOR_CACHE_MAX_BYTES: usize = 32 * 1024 * 1024;

#[doc(hidden)]
pub static QUERY_VECTOR_CACHE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueryVectorCacheSettings {
    pub enabled: bool,
    pub max_entries: usize,
    pub max_bytes: usize,
}

impl Default for QueryVectorCacheSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            max_entries: DEFAULT_QUERY_VECTOR_CACHE_MAX_ENTRIES,
            max_bytes: DEFAULT_QUERY_VECTOR_CACHE_MAX_BYTES,
        }
    }
}

struct ReadyEntry {
    vector: Vec<f32>,
    bytes: usize,
    last_access: Instant,
}

struct CacheState {
    settings: QueryVectorCacheSettings,
    ready: HashMap<String, ReadyEntry>,
    retained_bytes: usize,
}

impl CacheState {
    fn new(settings: QueryVectorCacheSettings) -> Self {
        Self {
            settings,
            ready: HashMap::new(),
            retained_bytes: 0,
        }
    }
}

static CACHE_ENABLED: AtomicBool = AtomicBool::new(false);

fn cache_state() -> &'static Mutex<CacheState> {
    static STATE: OnceLock<Mutex<CacheState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(CacheState::new(QueryVectorCacheSettings::default())))
}

fn lock_state() -> std::sync::MutexGuard<'static, CacheState> {
    cache_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_cache_override() -> Option<bool> {
    let value = env::var("MAGICIAN_QUERY_VECTOR_CACHE").ok()?;
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

pub fn query_vector_cache_enabled() -> bool {
    env_cache_override().unwrap_or_else(|| CACHE_ENABLED.load(Ordering::Acquire))
}

pub fn query_vector_cache_settings() -> QueryVectorCacheSettings {
    let mut settings = lock_state().settings;
    if let Some(enabled) = env_cache_override() {
        settings.enabled = enabled;
    }
    settings
}

pub fn install_query_vector_cache(settings: QueryVectorCacheSettings) {
    let enabled = settings.enabled;
    let mut state = lock_state();
    state.settings = QueryVectorCacheSettings {
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

pub fn clear_query_vector_cache() {
    let mut state = lock_state();
    state.ready.clear();
    state.retained_bytes = 0;
    publish_occupancy(&state);
}

pub fn reset_query_vector_cache_for_tests() {
    CACHE_ENABLED.store(false, Ordering::Release);
    let mut state = lock_state();
    *state = CacheState::new(QueryVectorCacheSettings::default());
    publish_occupancy(&state);
}

fn retained_bytes_for(key: &str, vector: &[f32]) -> usize {
    const OVERHEAD: usize = 64;
    OVERHEAD
        .saturating_add(key.len())
        .saturating_add(vector.len().saturating_mul(4))
}

fn publish_occupancy(state: &CacheState) {
    hol_stats::set_query_vector_cache_occupancy(state.ready.len(), state.retained_bytes);
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
            hol_stats::record_query_vector_cache_eviction();
        }
    }
}

pub fn get_query_vector(key: &str) -> Option<Vec<f32>> {
    if !query_vector_cache_enabled() {
        return None;
    }
    let mut state = lock_state();
    let Some(entry) = state.ready.get_mut(key) else {
        hol_stats::record_query_vector_cache_miss();
        return None;
    };
    entry.last_access = Instant::now();
    let vector = entry.vector.clone();
    hol_stats::record_query_vector_cache_hit();
    Some(vector)
}

pub fn store_query_vector(key: String, vector: Vec<f32>) {
    if !query_vector_cache_enabled() || vector.is_empty() {
        return;
    }
    let bytes = retained_bytes_for(&key, &vector);
    let mut state = lock_state();
    if !state.settings.enabled && env_cache_override() != Some(true) {
        return;
    }
    if let Some(previous) = state.ready.remove(&key) {
        state.retained_bytes = state.retained_bytes.saturating_sub(previous.bytes);
    }
    state.ready.insert(
        key,
        ReadyEntry {
            vector,
            bytes,
            last_access: Instant::now(),
        },
    );
    state.retained_bytes = state.retained_bytes.saturating_add(bytes);
    hol_stats::record_query_vector_cache_store();
    evict_to_bounds(&mut state);
    publish_occupancy(&state);
}

#[cfg(test)]
mod tests {
    use super::*;

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
    async fn disabled_cache_does_not_store_or_hit() {
        let _lock = QUERY_VECTOR_CACHE_TEST_LOCK.lock().await;
        reset_query_vector_cache_for_tests();
        store_query_vector("k".to_string(), vec![1.0, 0.0]);
        assert!(get_query_vector("k").is_none());
    }

    #[tokio::test]
    async fn enabled_cache_hits_exact_key_and_misses_other() {
        let _lock = QUERY_VECTOR_CACHE_TEST_LOCK.lock().await;
        reset_query_vector_cache_for_tests();
        install_query_vector_cache(QueryVectorCacheSettings {
            enabled: true,
            ..QueryVectorCacheSettings::default()
        });
        store_query_vector("k1".to_string(), vec![1.0, 0.0]);
        assert_eq!(get_query_vector("k1"), Some(vec![1.0, 0.0]));
        assert!(get_query_vector("k2").is_none());
        store_query_vector("k-model-a".to_string(), vec![1.0, 0.0]);
        store_query_vector("k-model-b".to_string(), vec![0.0, 1.0]);
        assert_eq!(get_query_vector("k-model-a"), Some(vec![1.0, 0.0]));
        assert_eq!(get_query_vector("k-model-b"), Some(vec![0.0, 1.0]));
        reset_query_vector_cache_for_tests();
    }

    #[tokio::test]
    async fn byte_bound_evicts_oldest_access() {
        let _lock = QUERY_VECTOR_CACHE_TEST_LOCK.lock().await;
        reset_query_vector_cache_for_tests();
        install_query_vector_cache(QueryVectorCacheSettings {
            enabled: true,
            max_entries: 8,
            max_bytes: 150,
        });
        store_query_vector("a".to_string(), vec![1.0]);
        store_query_vector("b".to_string(), vec![2.0]);
        store_query_vector("c".to_string(), vec![3.0]);
        assert!(
            get_query_vector("a").is_none(),
            "byte cap must evict the oldest vector"
        );
        assert_eq!(get_query_vector("c"), Some(vec![3.0]));
        reset_query_vector_cache_for_tests();
    }

    #[tokio::test]
    async fn env_off_disables_installed_cache() {
        let _lock = QUERY_VECTOR_CACHE_TEST_LOCK.lock().await;
        reset_query_vector_cache_for_tests();
        install_query_vector_cache(QueryVectorCacheSettings {
            enabled: true,
            ..QueryVectorCacheSettings::default()
        });
        let _guard = EnvVarGuard::set("MAGICIAN_QUERY_VECTOR_CACHE", "off");
        assert!(!query_vector_cache_enabled());
        store_query_vector("k".to_string(), vec![1.0]);
        assert!(get_query_vector("k").is_none());
        reset_query_vector_cache_for_tests();
    }

    #[tokio::test]
    async fn clear_drops_retained_vectors() {
        let _lock = QUERY_VECTOR_CACHE_TEST_LOCK.lock().await;
        reset_query_vector_cache_for_tests();
        install_query_vector_cache(QueryVectorCacheSettings {
            enabled: true,
            ..QueryVectorCacheSettings::default()
        });
        store_query_vector("k".to_string(), vec![1.0, 2.0]);
        clear_query_vector_cache();
        assert!(get_query_vector("k").is_none());
        reset_query_vector_cache_for_tests();
    }
}

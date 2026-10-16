//! Generation-keyed idle pool of request-path Lance search tables.
//!
//! Hybrid search used to open the same table twice, once per leg. This pool:
//!
//! - hybrid legs check out a pair of independent handles so two DataFusion
//!   plans never share one NativeTable, and so one idle handle cannot be
//!   fused with a table opened after an intervening invalidate;
//! - a cold hybrid still opens twice against one captured directory epoch
//!   (retrying if that epoch moves); a warm hybrid opens none;
//! - optionally retains idle handles across requests for the same generation;
//! - discards a handle on cancel, timeout, or search error so a poisoned
//!   in-flight poll cannot be reused;
//! - drops idle handles when the index generation changes or a writer
//!   invalidates the directory.
//!
//! `MAGICIAN_LANCE_TABLE_POOL=off` is the restart-bound kill switch: each
//! checkout opens a fresh table and Drop discards it. A hybrid with the pool
//! off therefore still opens twice, matching the pre-pool path.

use std::{
    collections::HashMap,
    env,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
};

use anyhow::{Context, Result};
use lancedb::{connect, Table};

use crate::hol_stats;

/// Must match `memory_index::MEMORY_LANCEDB_TABLE`.
pub(crate) const SEARCH_TABLE_NAME: &str = "memory_candidates";

pub const DEFAULT_LANCE_TABLE_POOL_MAX_IDLE: usize = 4;

#[doc(hidden)]
pub static LANCE_TABLE_POOL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

static POOL_ENABLED: AtomicBool = AtomicBool::new(true);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanceTablePoolSettings {
    pub enabled: bool,
    pub max_idle: usize,
}

impl Default for LanceTablePoolSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_idle: DEFAULT_LANCE_TABLE_POOL_MAX_IDLE,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct PoolKey {
    index_dir: PathBuf,
    generation: String,
}

struct HandlePool<T> {
    idle: HashMap<PoolKey, Vec<T>>,
    dir_epoch: HashMap<PathBuf, u64>,
    max_idle: usize,
    idle_count: usize,
}

impl<T> HandlePool<T> {
    fn new(max_idle: usize) -> Self {
        Self {
            idle: HashMap::new(),
            dir_epoch: HashMap::new(),
            max_idle: max_idle.max(1),
            idle_count: 0,
        }
    }

    fn epoch(&self, index_dir: &Path) -> u64 {
        self.dir_epoch.get(index_dir).copied().unwrap_or(0)
    }

    fn checkout(&mut self, key: &PoolKey) -> Option<T> {
        let slot = self.idle.get_mut(key)?;
        let handle = slot.pop()?;
        self.idle_count = self.idle_count.saturating_sub(1);
        if slot.is_empty() {
            self.idle.remove(key);
        }
        Some(handle)
    }

    /// Pop exactly two idle handles, or leave the slot untouched.
    ///
    /// Hybrid fusion must not mix a handle that survived from before an
    /// invalidate with a table opened after that invalidate. Taking one idle
    /// handle and opening the other would create that window.
    fn checkout_pair(&mut self, key: &PoolKey) -> Option<(T, T)> {
        let slot = self.idle.get_mut(key)?;
        if slot.len() < 2 {
            return None;
        }
        let second = slot.pop().expect("checkout_pair: len checked");
        let first = slot.pop().expect("checkout_pair: len checked");
        self.idle_count = self.idle_count.saturating_sub(2);
        if slot.is_empty() {
            self.idle.remove(key);
        }
        Some((first, second))
    }

    fn release(&mut self, key: PoolKey, epoch: u64, handle: T) -> bool {
        if self.epoch(&key.index_dir) != epoch {
            return false;
        }
        if self.idle_count >= self.max_idle {
            return false;
        }
        self.idle.entry(key).or_default().push(handle);
        self.idle_count = self.idle_count.saturating_add(1);
        true
    }

    fn set_max_idle(&mut self, max_idle: usize) {
        self.max_idle = max_idle.max(1);
        while self.idle_count > self.max_idle {
            let Some(key) = self.idle.keys().next().cloned() else {
                break;
            };
            let Some(slot) = self.idle.get_mut(&key) else {
                break;
            };
            slot.pop();
            self.idle_count = self.idle_count.saturating_sub(1);
            if slot.is_empty() {
                self.idle.remove(&key);
            }
        }
    }

    fn invalidate_dir(&mut self, index_dir: &Path) {
        self.idle.retain(|key, handles| {
            if key.index_dir.as_path() == index_dir {
                self.idle_count = self.idle_count.saturating_sub(handles.len());
                false
            } else {
                true
            }
        });
        let next = self.epoch(index_dir).saturating_add(1);
        self.dir_epoch.insert(index_dir.to_path_buf(), next);
    }
}

struct PoolState {
    settings: LanceTablePoolSettings,
    idle: HandlePool<Table>,
}

fn pool_state() -> &'static Mutex<PoolState> {
    static STATE: OnceLock<Mutex<PoolState>> = OnceLock::new();
    STATE.get_or_init(|| {
        Mutex::new(PoolState {
            settings: LanceTablePoolSettings::default(),
            idle: HandlePool::new(DEFAULT_LANCE_TABLE_POOL_MAX_IDLE),
        })
    })
}

fn lock_state() -> std::sync::MutexGuard<'static, PoolState> {
    pool_state()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn env_pool_override() -> Option<bool> {
    let value = env::var("MAGICIAN_LANCE_TABLE_POOL").ok()?;
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

pub fn lance_table_pool_enabled() -> bool {
    env_pool_override().unwrap_or_else(|| POOL_ENABLED.load(Ordering::Acquire))
}

pub fn install_lance_table_pool(settings: LanceTablePoolSettings) {
    let enabled = settings.enabled;
    let max_idle = settings.max_idle.max(1);
    POOL_ENABLED.store(enabled, Ordering::Release);
    let mut state = lock_state();
    state.settings = LanceTablePoolSettings { enabled, max_idle };
    if !enabled {
        state.idle = HandlePool::new(max_idle);
    } else {
        state.idle.set_max_idle(max_idle);
    }
    hol_stats::set_lance_table_pool_idle(state.idle.idle_count);
}

pub fn reset_lance_table_pool_for_tests() {
    POOL_ENABLED.store(true, Ordering::Release);
    let mut state = lock_state();
    *state = PoolState {
        settings: LanceTablePoolSettings::default(),
        idle: HandlePool::new(DEFAULT_LANCE_TABLE_POOL_MAX_IDLE),
    };
    hol_stats::set_lance_table_pool_idle(0);
}

pub fn invalidate_lance_table_pool(index_dir: &Path) {
    let mut state = lock_state();
    state.idle.invalidate_dir(index_dir);
    hol_stats::set_lance_table_pool_idle(state.idle.idle_count);
    hol_stats::record_lance_table_pool_invalidate();
}

/// Request-path table handle. Recycle only after both hybrid legs succeed.
/// Drop without recycle discards the handle so an aborted/failed poll cannot
/// re-enter the idle list.
pub struct PooledSearchTable {
    table: Option<Table>,
    key: PoolKey,
    epoch: u64,
    recycle: bool,
    pool_enabled: bool,
}

impl PooledSearchTable {
    pub fn clone_table(&self) -> Table {
        self.table
            .as_ref()
            .expect("pooled search table still owned")
            .clone()
    }

    pub fn recycle(mut self) {
        self.recycle = true;
    }
}

impl Drop for PooledSearchTable {
    fn drop(&mut self) {
        let Some(table) = self.table.take() else {
            return;
        };
        if !self.pool_enabled {
            return;
        }
        if !self.recycle {
            hol_stats::record_lance_table_pool_discard();
            return;
        }
        let mut state = lock_state();
        if state.idle.release(self.key.clone(), self.epoch, table) {
            hol_stats::set_lance_table_pool_idle(state.idle.idle_count);
        } else {
            hol_stats::record_lance_table_pool_discard();
        }
    }
}

pub async fn checkout_search_table(
    index_dir: &Path,
    generation: &str,
) -> Result<PooledSearchTable> {
    let key = PoolKey {
        index_dir: index_dir.to_path_buf(),
        generation: generation.to_string(),
    };
    let pool_enabled = lance_table_pool_enabled();
    let epoch;
    if pool_enabled {
        let mut state = lock_state();
        epoch = state.idle.epoch(&key.index_dir);
        if let Some(table) = state.idle.checkout(&key) {
            hol_stats::record_lance_table_pool_hit();
            hol_stats::set_lance_table_pool_idle(state.idle.idle_count);
            return Ok(PooledSearchTable {
                table: Some(table),
                key,
                epoch,
                recycle: false,
                pool_enabled,
            });
        }
        hol_stats::record_lance_table_pool_miss();
    } else {
        epoch = 0;
    }
    let table = open_search_table(index_dir).await?;
    Ok(wrap_pooled(table, key, epoch, pool_enabled))
}

const HYBRID_OPEN_EPOCH_RETRIES: usize = 2;

/// Two independent tables for concurrent hybrid legs.
///
/// Idle checkout is atomic: either both handles come from the current
/// generation slot, or neither does. A cold pair opens both tables against
/// one captured directory epoch and retries if a writer invalidates during
/// those opens, so RRF cannot fuse mixed snapshots.
pub async fn checkout_search_table_pair(
    index_dir: &Path,
    generation: &str,
) -> Result<(PooledSearchTable, PooledSearchTable)> {
    let key = PoolKey {
        index_dir: index_dir.to_path_buf(),
        generation: generation.to_string(),
    };
    let pool_enabled = lance_table_pool_enabled();
    for _attempt in 0..=HYBRID_OPEN_EPOCH_RETRIES {
        let epoch;
        if pool_enabled {
            let mut state = lock_state();
            epoch = state.idle.epoch(&key.index_dir);
            if let Some((first, second)) = state.idle.checkout_pair(&key) {
                hol_stats::record_lance_table_pool_hit();
                hol_stats::record_lance_table_pool_hit();
                hol_stats::set_lance_table_pool_idle(state.idle.idle_count);
                return Ok((
                    wrap_pooled(first, key.clone(), epoch, true),
                    wrap_pooled(second, key, epoch, true),
                ));
            }
            hol_stats::record_lance_table_pool_miss();
            hol_stats::record_lance_table_pool_miss();
        } else {
            epoch = 0;
        }

        let (first, second) =
            tokio::try_join!(open_search_table(index_dir), open_search_table(index_dir),)?;

        if pool_enabled {
            let current = lock_state().idle.epoch(&key.index_dir);
            if current != epoch {
                drop((first, second));
                continue;
            }
        }

        return Ok((
            wrap_pooled(first, key.clone(), epoch, pool_enabled),
            wrap_pooled(second, key, epoch, pool_enabled),
        ));
    }
    anyhow::bail!(
        "LanceDB hybrid search table epoch changed while opening {}",
        index_dir.display()
    )
}

fn wrap_pooled(table: Table, key: PoolKey, epoch: u64, pool_enabled: bool) -> PooledSearchTable {
    PooledSearchTable {
        table: Some(table),
        key,
        epoch,
        recycle: false,
        pool_enabled,
    }
}

async fn open_search_table(index_dir: &Path) -> Result<Table> {
    let index_uri = index_dir.to_string_lossy().into_owned();
    let db = connect(&index_uri)
        .execute()
        .await
        .with_context(|| format!("opening LanceDB memory index {}", index_dir.display()))?;
    db.open_table(SEARCH_TABLE_NAME)
        .execute()
        .await
        .context("opening LanceDB memory candidate table")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_pool_checkouts_release_and_caps_idle() {
        let mut pool = HandlePool::new(2);
        let key = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        assert!(pool.checkout(&key).is_none());
        assert!(pool.release(key.clone(), 0, 1u32));
        assert!(pool.release(key.clone(), 0, 2u32));
        assert!(
            !pool.release(key.clone(), 0, 3u32),
            "over max_idle is dropped"
        );
        assert_eq!(pool.checkout(&key), Some(2));
        assert_eq!(pool.checkout(&key), Some(1));
        assert!(pool.checkout(&key).is_none());
    }

    #[test]
    fn handle_pool_checkout_pair_is_atomic() {
        let mut pool = HandlePool::new(4);
        let key = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        assert!(pool.checkout_pair(&key).is_none());
        assert!(pool.release(key.clone(), 0, 1u8));
        assert!(
            pool.checkout_pair(&key).is_none(),
            "a single idle handle must stay in the slot"
        );
        assert_eq!(
            pool.checkout(&key),
            Some(1),
            "the lone handle was not consumed"
        );
        assert!(pool.release(key.clone(), 0, 1u8));
        assert!(pool.release(key.clone(), 0, 2u8));
        assert!(pool.release(key.clone(), 0, 3u8));
        assert_eq!(pool.checkout_pair(&key), Some((2, 3)));
        assert_eq!(pool.idle_count, 1);
        assert_eq!(pool.checkout(&key), Some(1));
        assert!(pool.checkout_pair(&key).is_none());
    }

    #[test]
    fn handle_pool_isolates_generation_and_directory() {
        let mut pool = HandlePool::new(8);
        let a1 = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        let a2 = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g2".to_string(),
        };
        let b1 = PoolKey {
            index_dir: PathBuf::from("/tmp/b"),
            generation: "g1".to_string(),
        };
        assert!(pool.release(a1.clone(), 0, "a1"));
        assert!(pool.release(a2.clone(), 0, "a2"));
        assert!(pool.release(b1.clone(), 0, "b1"));
        assert_eq!(pool.checkout(&a1), Some("a1"));
        assert!(pool.checkout(&a1).is_none());
        assert_eq!(pool.checkout(&a2), Some("a2"));
        assert_eq!(pool.checkout(&b1), Some("b1"));
    }

    #[test]
    fn handle_pool_invalidate_dir_drops_every_generation() {
        let mut pool = HandlePool::new(8);
        let a1 = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        let a2 = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g2".to_string(),
        };
        let b1 = PoolKey {
            index_dir: PathBuf::from("/tmp/b"),
            generation: "g1".to_string(),
        };
        assert!(pool.release(a1.clone(), 0, 1u8));
        assert!(pool.release(a2.clone(), 0, 2u8));
        assert!(pool.release(b1.clone(), 0, 3u8));
        pool.invalidate_dir(Path::new("/tmp/a"));
        assert!(pool.checkout(&a1).is_none());
        assert!(pool.checkout(&a2).is_none());
        assert_eq!(pool.checkout(&b1), Some(3));
        assert!(
            !pool.release(a1, 0, 9u8),
            "a handle checked out before invalidate must not re-enter"
        );
    }

    #[test]
    fn handle_pool_trim_drops_excess_when_max_idle_shrinks() {
        let mut pool = HandlePool::new(4);
        let key = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        assert!(pool.release(key.clone(), 0, 1u8));
        assert!(pool.release(key.clone(), 0, 2u8));
        assert!(pool.release(key.clone(), 0, 3u8));
        assert_eq!(pool.idle_count, 3);
        pool.set_max_idle(1);
        assert_eq!(pool.idle_count, 1);
        assert!(pool.checkout(&key).is_some());
        assert!(pool.checkout(&key).is_none());
    }

    #[test]
    fn handle_pool_dropped_checkout_is_not_reusable() {
        let mut pool = HandlePool::new(4);
        let key = PoolKey {
            index_dir: PathBuf::from("/tmp/a"),
            generation: "g1".to_string(),
        };
        assert!(pool.release(key.clone(), 0, 7u8));
        let _abandoned = pool.checkout(&key);
        assert!(
            pool.checkout(&key).is_none(),
            "an unreycled checkout must not reappear in the idle list"
        );
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
    async fn env_off_disables_pool_without_install() {
        let _lock = LANCE_TABLE_POOL_TEST_LOCK.lock().await;
        let _guard = EnvVarGuard::set("MAGICIAN_LANCE_TABLE_POOL", "off");
        assert!(!lance_table_pool_enabled());
    }

    #[test]
    fn search_table_name_matches_memory_index_contract() {
        assert_eq!(SEARCH_TABLE_NAME, "memory_candidates");
    }
}

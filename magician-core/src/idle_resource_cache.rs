//! Idle process-local owners. Borrowed Arcs pin an owner through in-flight work.
//! Retirement happens outside the cache lock; the same key waits until close
//! finishes before it can be reopened. Durable files are never removed.
use std::{
    collections::{HashMap, HashSet},
    fmt,
    hash::Hash,
    sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, Weak},
    time::{Duration, Instant},
};

pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub const REAP_INTERVAL: Duration = Duration::from_secs(15);
pub const MAX_IDLE_OWNERS: usize = 8;

pub trait ReapIdle: Send + Sync {
    fn reap(&self, now: Instant) -> usize;
}
pub fn register_idle_reaper(owner: Arc<dyn ReapIdle>) {
    let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
    registry.retain(|cache| cache.strong_count() > 0);
    registry.push(Arc::downgrade(&owner));
}
fn registry() -> &'static Mutex<Vec<Weak<dyn ReapIdle>>> {
    static REGISTRY: OnceLock<Mutex<Vec<Weak<dyn ReapIdle>>>> = OnceLock::new();
    REGISTRY.get_or_init(Mutex::default)
}

/// Called by one retained host task on an admitted blocking thread.
pub fn reap_idle_resources() -> usize {
    let caches = {
        let mut registry = registry().lock().unwrap_or_else(|e| e.into_inner());
        registry.retain(|cache| cache.strong_count() > 0);
        registry
            .iter()
            .filter_map(Weak::upgrade)
            .collect::<Vec<_>>()
    };
    let now = Instant::now();
    caches.iter().map(|cache| cache.reap(now)).sum()
}

struct Entry<V> {
    owner: Arc<V>,
    last_used: Instant,
}
struct State<K, V> {
    entries: HashMap<K, Entry<V>>,
    closing: HashSet<K>,
}
pub struct IdleResourceCache<K, V> {
    state: Mutex<State<K, V>>,
    closed: Condvar,
    idle_timeout: Duration,
    max_idle: usize,
}
impl<K, V> fmt::Debug for IdleResourceCache<K, V> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IdleResourceCache")
            .field("idle_timeout", &self.idle_timeout)
            .field("max_idle", &self.max_idle)
            .finish_non_exhaustive()
    }
}

impl<K, V> IdleResourceCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Send + Sync + 'static,
{
    pub fn new() -> Arc<Self> {
        Self::with_policy(IDLE_TIMEOUT, MAX_IDLE_OWNERS)
    }

    pub fn with_policy(idle_timeout: Duration, max_idle: usize) -> Arc<Self> {
        let cache = Arc::new(Self {
            state: Mutex::new(State {
                entries: HashMap::new(),
                closing: HashSet::new(),
            }),
            closed: Condvar::new(),
            idle_timeout,
            max_idle,
        });
        register_idle_reaper(cache.clone());
        cache
    }

    fn ready(&self, key: &K) -> MutexGuard<'_, State<K, V>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.closing.contains(key) {
            state = self.closed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        state
    }

    pub fn get(&self, key: &K) -> Option<Arc<V>> {
        let mut state = self.ready(key);
        state.entries.get_mut(key).map(|entry| {
            entry.last_used = Instant::now();
            Arc::clone(&entry.owner)
        })
    }

    /// Async fast paths defer a closing/missing owner to their blocking open.
    pub fn peek(&self, key: &K) -> Option<Arc<V>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.entries.get_mut(key).map(|entry| {
            entry.last_used = Instant::now();
            Arc::clone(&entry.owner)
        })
    }

    pub fn insert(&self, key: K, owner: Arc<V>) -> Arc<V> {
        let result = {
            let mut state = self.ready(&key);
            let entry = state.entries.entry(key).or_insert_with(|| Entry {
                owner: owner.clone(),
                last_used: Instant::now(),
            });
            entry.last_used = Instant::now();
            Arc::clone(&entry.owner)
        };
        drop(owner);
        self.reap_at(Instant::now(), None);
        result
    }

    /// Serialize first opens (analytics pools/sinks and cheap per-scope gates).
    pub fn get_or_try_insert_with<E>(
        &self,
        key: K,
        open: impl FnOnce() -> Result<Arc<V>, E>,
    ) -> Result<Arc<V>, E> {
        let result = {
            let mut state = self.ready(&key);
            if let Some(entry) = state.entries.get_mut(&key) {
                entry.last_used = Instant::now();
                Arc::clone(&entry.owner)
            } else {
                let owner = open()?;
                state.entries.insert(
                    key,
                    Entry {
                        owner: owner.clone(),
                        last_used: Instant::now(),
                    },
                );
                owner
            }
        };
        self.reap_at(Instant::now(), None);
        Ok(result)
    }

    /// Startup preparation should not keep a dormant database warm forever.
    pub fn release_if_unused(&self, key: &K) -> usize {
        self.reap_at(Instant::now(), Some(key))
    }

    fn reap_at(&self, now: Instant, release: Option<&K>) -> usize {
        let retired = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut idle = Vec::new();
            for (key, entry) in &mut state.entries {
                if Arc::strong_count(&entry.owner) == 1 {
                    idle.push((key.clone(), entry.last_used));
                } else {
                    // Long queries get their grace period after their last use.
                    entry.last_used = now;
                }
            }
            idle.sort_by_key(|(_, used)| *used);
            let excess = idle.len().saturating_sub(self.max_idle);
            let keys = idle
                .into_iter()
                .enumerate()
                .filter_map(|(index, (key, used))| {
                    (index < excess
                        || now.saturating_duration_since(used) >= self.idle_timeout
                        || release == Some(&key))
                    .then_some(key)
                })
                .collect::<Vec<_>>();
            keys.into_iter()
                .filter_map(|key| {
                    let entry = state.entries.remove(&key)?;
                    state.closing.insert(key.clone());
                    Some((key, entry.owner))
                })
                .collect::<Vec<_>>()
        };
        let count = retired.len();
        if count == 0 {
            return 0;
        }
        // Closing a DB can checkpoint and join native workers. Only callers
        // reopening these keys wait; unrelated workspaces can still proceed.
        let completion = CloseCompletion {
            cache: self,
            keys: retired.iter().map(|(key, _)| key.clone()).collect(),
        };
        drop(retired);
        drop(completion);
        count
    }
}

struct CloseCompletion<'a, K: Eq + Hash, V> {
    cache: &'a IdleResourceCache<K, V>,
    keys: Vec<K>,
}
impl<K: Eq + Hash, V> Drop for CloseCompletion<'_, K, V> {
    fn drop(&mut self) {
        let mut state = self.cache.state.lock().unwrap_or_else(|e| e.into_inner());
        for key in &self.keys {
            state.closing.remove(key);
        }
        self.cache.closed.notify_all();
    }
}
impl<K, V> ReapIdle for IdleResourceCache<K, V>
where
    K: Eq + Hash + Clone + Send + 'static,
    V: Send + Sync + 'static,
{
    fn reap(&self, now: Instant) -> usize {
        self.reap_at(now, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Owner(Arc<AtomicUsize>);
    impl Drop for Owner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn lookup_renews_the_idle_deadline() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = IdleResourceCache::with_policy(Duration::from_secs(10), 8);
        drop(cache.insert("scope", Arc::new(Owner(drops.clone()))));
        cache
            .state
            .lock()
            .unwrap()
            .entries
            .get_mut(&"scope")
            .unwrap()
            .last_used = Instant::now() - Duration::from_secs(60);
        drop(cache.get(&"scope").unwrap());
        assert_eq!(
            cache.reap_at(Instant::now() + Duration::from_secs(5), None),
            0
        );
        assert_eq!(
            cache.reap_at(Instant::now() + Duration::from_secs(11), None),
            1
        );
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn registry_does_not_keep_a_dropped_cache_or_its_owners_alive() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = IdleResourceCache::with_policy(Duration::from_secs(600), 8);
        let weak = Arc::downgrade(&cache);
        drop(cache.insert("scope", Arc::new(Owner(drops.clone()))));
        drop(cache);
        assert!(weak.upgrade().is_none());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn idle_owner_closes_but_inflight_clones_pin_it_until_work_finishes() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = IdleResourceCache::with_policy(Duration::from_secs(10), 8);
        let active = cache.insert("workspace", Arc::new(Owner(drops.clone())));
        let later = Instant::now() + Duration::from_secs(11);
        assert_eq!(cache.reap_at(later, None), 0);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(active);
        assert_eq!(cache.reap_at(later + Duration::from_secs(11), None), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn capacity_releases_idle_owners_without_evicting_an_active_workspace() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = IdleResourceCache::with_policy(Duration::from_secs(600), 1);
        let active = cache.insert("active", Arc::new(Owner(drops.clone())));
        drop(cache.insert("idle", Arc::new(Owner(drops.clone()))));
        drop(cache.insert("new", Arc::new(Owner(drops.clone()))));
        cache.reap_at(Instant::now(), None);
        assert!(cache.get(&"active").is_some());
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        drop(active);
    }

    #[test]
    fn startup_release_closes_only_unborrowed_owners_and_next_use_reopens() {
        let drops = Arc::new(AtomicUsize::new(0));
        let cache = IdleResourceCache::with_policy(Duration::from_secs(600), 8);
        let active = cache.insert("scope", Arc::new(Owner(drops.clone())));
        assert_eq!(cache.release_if_unused(&"scope"), 0);
        drop(active);
        assert_eq!(cache.release_if_unused(&"scope"), 1);
        assert!(cache.get(&"scope").is_none());
        let reopened = cache
            .get_or_try_insert_with("scope", || Ok::<_, ()>(Arc::new(Owner(drops.clone()))))
            .unwrap();
        assert!(Arc::ptr_eq(&reopened, &cache.get(&"scope").unwrap()));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn slow_close_blocks_only_the_same_workspace_and_prevents_overlap() {
        use std::sync::mpsc;
        struct SlowOwner {
            started: mpsc::Sender<()>,
            finish: Mutex<mpsc::Receiver<()>>,
        }
        impl Drop for SlowOwner {
            fn drop(&mut self) {
                self.started.send(()).unwrap();
                self.finish.lock().unwrap().recv().unwrap();
            }
        }
        let cache = IdleResourceCache::with_policy(Duration::ZERO, 8);
        let (started_tx, started_rx) = mpsc::channel();
        let (finish_tx, finish_rx) = mpsc::channel();
        drop(cache.insert(
            "closing",
            Arc::new(SlowOwner {
                started: started_tx,
                finish: Mutex::new(finish_rx),
            }),
        ));
        let reaper = cache.clone();
        let reaping = std::thread::spawn(move || reaper.release_if_unused(&"closing"));
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(cache.get(&"unrelated").is_none());
        let lookup = cache.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let waiting = std::thread::spawn(move || {
            let result = lookup.get(&"closing");
            done_tx.send(result.is_none()).unwrap();
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        finish_tx.send(()).unwrap();
        assert_eq!(reaping.join().unwrap(), 1);
        assert!(done_rx.recv_timeout(Duration::from_secs(2)).unwrap());
        waiting.join().unwrap();
    }
}

//! Bounded memoization of pure source validation. Filesystem discovery and
//! owner/scope binding must happen outside this cache on every catalog read.

use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

use serde::Serialize;

const MAX_ENTRIES: usize = 512;
const MAX_BYTES: usize = 16 * 1024 * 1024;

type ParseCell<T> = Arc<OnceLock<Arc<T>>>;

struct Entry<T> {
    bytes: usize,
    last_used: u64,
    value: ParseCell<T>,
}

struct Entries<T> {
    values: BTreeMap<[u8; 32], Entry<T>>,
    bytes: usize,
    clock: u64,
}

impl<T> Entries<T> {
    fn evict_oldest(&mut self) {
        if let Some(key) = self
            .values
            .iter()
            .min_by_key(|(_, entry)| entry.last_used)
            .map(|(key, _)| *key)
        {
            self.bytes -= self.values.remove(&key).expect("entry was present").bytes;
        }
    }
}

pub(super) struct SourceParseCache<T> {
    entries: Mutex<Entries<T>>,
    max_entries: usize,
    max_bytes: usize,
}

impl<T> Default for SourceParseCache<T> {
    fn default() -> Self {
        Self::new(MAX_ENTRIES, MAX_BYTES)
    }
}

impl<T> SourceParseCache<T> {
    fn new(max_entries: usize, max_bytes: usize) -> Self {
        Self {
            entries: Mutex::new(Entries {
                values: BTreeMap::new(),
                bytes: 0,
                clock: 0,
            }),
            max_entries,
            max_bytes,
        }
    }
}

impl<T: Serialize> SourceParseCache<T> {
    pub(super) fn get_or_init(
        &self,
        source: &[u8],
        context: &[u8],
        parse: impl FnOnce() -> T,
    ) -> Arc<T> {
        let input_bytes = source.len().saturating_add(context.len());
        if input_bytes > self.max_bytes || self.max_entries == 0 {
            return Arc::new(parse());
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(&(source.len() as u64).to_le_bytes());
        hasher.update(source);
        hasher.update(context);
        let key = *hasher.finalize().as_bytes();
        let cell = {
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            entries.clock = entries.clock.wrapping_add(1);
            let last_used = entries.clock;
            if let Some(entry) = entries.values.get_mut(&key) {
                entry.last_used = last_used;
                Arc::clone(&entry.value)
            } else {
                while entries.values.len() >= self.max_entries
                    || entries.bytes.saturating_add(input_bytes) > self.max_bytes
                {
                    entries.evict_oldest();
                }
                let cell = Arc::new(OnceLock::new());
                entries.bytes += input_bytes;
                entries.values.insert(
                    key,
                    Entry {
                        bytes: input_bytes,
                        last_used,
                        value: Arc::clone(&cell),
                    },
                );
                cell
            }
        };
        // Only callers of the same content wait for this validation. Parsing
        // never holds the map lock or serializes unrelated scopes/documents.
        Arc::clone(cell.get_or_init(|| {
            let value = Arc::new(parse());
            let bytes = serde_json::to_vec(value.as_ref())
                .map(|json| input_bytes.saturating_add(json.len()))
                .unwrap_or(usize::MAX);
            let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = entries.values.get(&key) {
                if Arc::ptr_eq(&entry.value, &cell) {
                    let previous_bytes = entry.bytes;
                    if bytes > self.max_bytes {
                        entries.values.remove(&key);
                        entries.bytes -= previous_bytes;
                    } else {
                        entries
                            .values
                            .get_mut(&key)
                            .expect("entry was present")
                            .bytes = bytes;
                        entries.bytes = entries.bytes - previous_bytes + bytes;
                        while entries.bytes > self.max_bytes {
                            entries.evict_oldest();
                        }
                    }
                }
            }
            value
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Barrier,
    };

    #[test]
    fn exact_content_and_validation_context_are_required_for_reuse() {
        let cache = SourceParseCache::default();
        assert_eq!(*cache.get_or_init(b"old", b"folder-a", || Some(1)), Some(1));
        assert_eq!(
            *cache.get_or_init(b"old", b"folder-a", || panic!("parsed again")),
            Some(1)
        );
        assert_eq!(*cache.get_or_init(b"new", b"folder-a", || None), None);
        assert_eq!(*cache.get_or_init(b"old", b"folder-b", || None), None);
        assert_eq!(
            *cache.get_or_init(b"new", b"folder-a", || panic!(
                "invalid source parsed again"
            )),
            None
        );
    }

    #[test]
    fn concurrent_readers_validate_identical_content_once() {
        let cache = Arc::new(SourceParseCache::default());
        let barrier = Arc::new(Barrier::new(8));
        let parsed = Arc::new(AtomicUsize::new(0));
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let (cache, barrier, parsed) = (
                    Arc::clone(&cache),
                    Arc::clone(&barrier),
                    Arc::clone(&parsed),
                );
                std::thread::spawn(move || {
                    barrier.wait();
                    cache.get_or_init(b"source", b"", || {
                        parsed.fetch_add(1, Ordering::SeqCst);
                        42
                    })
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(*worker.join().unwrap(), 42);
        }
        assert_eq!(parsed.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn cache_bounds_cover_entry_count_and_serialized_output() {
        let cache = SourceParseCache::new(2, 128);
        cache.get_or_init(b"one", b"", || "one".to_owned());
        cache.get_or_init(b"two", b"", || "two".to_owned());
        cache.get_or_init(b"one", b"", || panic!("recent source was parsed again"));
        cache.get_or_init(b"three", b"", || "three".to_owned());
        assert_eq!(
            &*cache.get_or_init(b"one", b"", || panic!("recent source was evicted")),
            "one"
        );
        assert_eq!(
            &*cache.get_or_init(b"two", b"", || "reparsed".to_owned()),
            "reparsed"
        );
        cache.get_or_init(b"large", b"", || "x".repeat(129));
        assert_eq!(
            &*cache.get_or_init(b"large", b"", || "not retained".to_owned()),
            "not retained"
        );
        let entries = cache.entries.lock().unwrap();
        assert!(entries.values.len() <= 2);
        assert!(entries.bytes <= 128);
    }
}

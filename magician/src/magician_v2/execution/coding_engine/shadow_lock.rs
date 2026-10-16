//! Per-shadow admission lock — serialize concurrent coding work that shares one
//! persistent shadow workspace.
//!
//! The persistent coding shadow is keyed purely by
//! [`persistent_shadow_key`](super::persistent_shadow_key)
//! (`blake3(repo_path)`) with **no** synchronization across the sync sites
//! (`run_coding_task` and `run_project_checks` both call
//! [`sync_persistent_workspace`](super::sync_persistent_workspace), then mutate
//! the shadow with a copy/prune + a shadow-vs-real byte diff). Two concurrent
//! runs on the *same* repo therefore interleave those steps and corrupt each
//! other — today only an unenforced one-active-session UX convention prevents
//! it (plan §13.3 #3 / #12).
//!
//! This gives each shadow key a process-global async [`Mutex`]. A run takes it
//! for the whole sync → turn → proposal-capture window, so same-repo runs
//! **serialize** while distinct repos still proceed fully in parallel (distinct
//! keys → distinct locks). It is in-process only — that matches the shadow's
//! lifetime (one Magician process owns the scope's shadows); a cross-process
//! upgrade would swap the inner `Mutex<()>` for an `fs2` advisory file lock
//! without changing callers.

use std::{collections::HashMap, sync::Arc};

use once_cell::sync::Lazy;
use tokio::sync::Mutex;

/// `shadow_key -> admission mutex`. The outer mutex guards only the brief map
/// lookup/insert; the returned per-key mutex is what callers hold across their
/// shadow window. Entries are never removed — the key space is bounded by the
/// number of distinct repos a process touches, and a stale empty mutex costs
/// nothing.
static SHADOW_LOCKS: Lazy<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// The admission mutex for one persistent-shadow key. Distinct keys return
/// distinct mutexes (so different repos never block each other); the same key
/// always returns the same `Arc<Mutex<()>>`.
///
/// Callers acquire and **hold** the guard across the entire shadow mutation
/// window: ```ignore
/// let admission = shadow_admission_lock(&persistent_shadow_key(&repo_path)).await;
/// let _shadow_guard = admission.lock().await; // released at end of scope
/// // sync_persistent_workspace(...) → run the turn → capture the proposal
/// ```
pub async fn shadow_admission_lock(shadow_key: &str) -> Arc<Mutex<()>> {
    let mut map = SHADOW_LOCKS.lock().await;
    map.entry(shadow_key.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use std::{
        sync::atomic::{AtomicUsize, Ordering},
        time::Duration,
    };

    use super::*;

    #[tokio::test]
    async fn same_key_returns_the_same_mutex_distinct_keys_differ() {
        let a1 = shadow_admission_lock("repo-a").await;
        let a2 = shadow_admission_lock("repo-a").await;
        let b = shadow_admission_lock("repo-b").await;
        assert!(
            Arc::ptr_eq(&a1, &a2),
            "same key must share one admission mutex"
        );
        assert!(
            !Arc::ptr_eq(&a1, &b),
            "distinct keys must get distinct mutexes"
        );
    }

    #[tokio::test]
    async fn same_key_acquisitions_serialize() {
        // Two tasks contending for the same shadow key must run their critical sections
        // one at a time; the observed max-concurrency stays 1.
        let live = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..8 {
            let live = live.clone();
            let max_seen = max_seen.clone();
            handles.push(tokio::spawn(async move {
                let admission = shadow_admission_lock("serialize-key").await;
                let _guard = admission.lock().await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(5)).await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "same-key critical sections must not overlap"
        );
    }

    #[tokio::test]
    async fn distinct_keys_proceed_concurrently() {
        // Two different shadow keys must NOT block each other — both can hold their
        // guards at once.
        let live = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for i in 0..6 {
            let live = live.clone();
            let max_seen = max_seen.clone();
            handles.push(tokio::spawn(async move {
                let admission = shadow_admission_lock(&format!("distinct-{i}")).await;
                let _guard = admission.lock().await;
                let now = live.fetch_add(1, Ordering::SeqCst) + 1;
                max_seen.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(10)).await;
                live.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert!(
            max_seen.load(Ordering::SeqCst) > 1,
            "distinct keys must run concurrently"
        );
    }
}

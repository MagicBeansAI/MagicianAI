//! Idempotency: dedup duplicate submissions by caller-supplied key.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::mapref::entry::Entry;
use dashmap::DashMap;
use parking_lot::Mutex;
use tokio::sync::broadcast;

use crate::error::LLMResult;

use super::job::DispatchedResponse;
use super::types::JobId;

/// Idempotency index: caller-supplied key → currently in-flight job +
/// shared result broadcast.
pub struct InflightIndex {
    in_flight: DashMap<String, InflightEntry>,
    recent_results: Mutex<RecentResults>,
    window: Duration,
    recent_cap: usize,
    recent_byte_cap: usize,
}

const DEFAULT_RECENT_RESULT_CAP: usize = 1_000;
const DEFAULT_RECENT_RESULT_BYTE_CAP: usize = 64 * 1024 * 1024;

struct InflightEntry {
    #[allow(dead_code)]
    job_id: JobId,
    sender: broadcast::Sender<Arc<LLMResult<DispatchedResponse>>>,
}

struct RecentResult {
    key: String,
    inserted_at: Instant,
    response: Arc<LLMResult<DispatchedResponse>>,
    retained_bytes: usize,
}

#[derive(Default)]
struct RecentResults {
    entries: VecDeque<RecentResult>,
    retained_bytes: usize,
}

/// Outcome of consulting the index before submitting a new job.
pub enum LookupOutcome {
    /// No active or recent match — caller is now the registered owner and
    /// should proceed with the actual submission. The worker will call
    /// `resolve` with the terminal result.
    Miss,
    /// Active job already running — subscribe to its result.
    InFlight(broadcast::Receiver<Arc<LLMResult<DispatchedResponse>>>),
    /// Recently completed — return cached result immediately.
    Cached(Arc<LLMResult<DispatchedResponse>>),
}

impl InflightIndex {
    /// Construct with the supplied dedup window.
    pub fn new(window: Duration) -> Self {
        Self {
            in_flight: DashMap::new(),
            recent_results: Mutex::new(RecentResults::default()),
            window,
            recent_cap: DEFAULT_RECENT_RESULT_CAP,
            recent_byte_cap: DEFAULT_RECENT_RESULT_BYTE_CAP,
        }
    }

    #[cfg(test)]
    fn with_limits(window: Duration, recent_cap: usize, recent_byte_cap: usize) -> Self {
        Self {
            in_flight: DashMap::new(),
            recent_results: Mutex::new(RecentResults::default()),
            window,
            recent_cap,
            recent_byte_cap,
        }
    }

    /// Atomically check the cache + register a new in-flight slot if absent.
    ///
    /// This is the only way callers should consult the index. The previous
    /// split lookup → register pattern had a race where two concurrent
    /// submits both saw Miss, both registered, and the second overwrote
    /// the first — defeating idempotency. `DashMap::entry()` makes the
    /// "insert if not present" atomic.
    pub fn lookup_or_register(&self, key: &str, job_id: JobId) -> LookupOutcome {
        // Cached-result check first (no in-flight entry exists for an
        // already-terminated job).
        self.prune_expired();
        let recents = self.recent_results.lock();
        if let Some(cached) = recents.entries.iter().rev().find(|r| r.key == key) {
            return LookupOutcome::Cached(cached.response.clone());
        }

        // Keep the recent-results lock through the in-flight entry decision.
        // `resolve` uses the same lock ordering (recent then in-flight), so a
        // submit cannot slip between publishing a terminal cache entry and
        // removing its active registration and accidentally become a Miss.
        let outcome = match self.in_flight.entry(key.to_string()) {
            Entry::Occupied(occupied) => LookupOutcome::InFlight(occupied.get().sender.subscribe()),
            Entry::Vacant(vacant) => {
                let (tx, _rx) = broadcast::channel(8);
                vacant.insert(InflightEntry { job_id, sender: tx });
                LookupOutcome::Miss
            },
        };
        drop(recents);
        outcome
    }

    /// Resolve the in-flight entry with a terminal result.
    ///
    /// Important: we push into `recent_results` BEFORE removing the
    /// in_flight entry, so a concurrent `lookup_or_register` that's
    /// between its recents-check and in_flight-check sees the cached
    /// result (Cached outcome) instead of inserting a duplicate Miss.
    pub fn resolve(
        &self,
        key: &str,
        expected_job_id: &JobId,
        response: Arc<LLMResult<DispatchedResponse>>,
    ) -> bool {
        // Reject obvious stale/repeated generations before walking a possibly
        // large response to estimate its cache footprint. Ownership is
        // revalidated atomically below after acquiring the recent-cache lock.
        if self
            .in_flight
            .get(key)
            .is_none_or(|entry| &entry.job_id != expected_job_id)
        {
            return false;
        }
        let retained_bytes = estimated_result_bytes(response.as_ref());
        let mut recents = self.recent_results.lock();
        let entry = match self.in_flight.entry(key.to_string()) {
            Entry::Occupied(entry) if &entry.get().job_id == expected_job_id => entry,
            // A repeated completion for the same generation is a no-op once
            // the first completion removed its owner. More importantly, an
            // old generation may never remove or publish over a newer owner
            // that reused this idempotency key after cache eviction.
            Entry::Occupied(_) | Entry::Vacant(_) => return false,
        };

        // There should be no recent value while a matching owner is active,
        // but defensively replace one rather than allowing duplicate keys to
        // retain bytes or make lookup return an older payload.
        if let Some(position) = recents.entries.iter().position(|recent| recent.key == key) {
            if let Some(replaced) = recents.entries.remove(position) {
                recents.retained_bytes = recents
                    .retained_bytes
                    .saturating_sub(replaced.retained_bytes);
            }
        }
        recents.retained_bytes = recents.retained_bytes.saturating_add(retained_bytes);
        recents.entries.push_back(RecentResult {
            key: key.to_string(),
            inserted_at: Instant::now(),
            response: response.clone(),
            retained_bytes,
        });
        while recents.entries.len() > self.recent_cap
            || recents.retained_bytes > self.recent_byte_cap
        {
            let Some(evicted) = recents.entries.pop_front() else {
                break;
            };
            recents.retained_bytes = recents
                .retained_bytes
                .saturating_sub(evicted.retained_bytes);
        }
        let entry = entry.remove();
        drop(recents);
        let _ = entry.sender.send(response);
        true
    }

    /// Relinquish an owner generation that never reached the dispatch lane.
    ///
    /// Unlike [`Self::resolve`], abandonment is not cached: a cancelled submit
    /// must wake existing subscribers but must not poison the idempotency key
    /// with a synthetic terminal result. The recent-results lock preserves the
    /// same publication/removal ordering as normal resolution, while the
    /// `JobId` comparison prevents an old cancelled submit from removing a
    /// newer generation.
    pub fn abandon(
        &self,
        key: &str,
        expected_job_id: &JobId,
        response: Arc<LLMResult<DispatchedResponse>>,
    ) -> bool {
        let recents = self.recent_results.lock();
        let entry = match self.in_flight.entry(key.to_string()) {
            Entry::Occupied(entry) if &entry.get().job_id == expected_job_id => entry,
            Entry::Occupied(_) | Entry::Vacant(_) => return false,
        };
        let entry = entry.remove();
        drop(recents);
        let _ = entry.sender.send(response);
        true
    }

    /// Whether a key is currently registered. (Used by tests/diagnostics.)
    #[allow(dead_code)]
    pub fn is_in_flight(&self, key: &str) -> bool {
        self.in_flight.contains_key(key)
    }

    /// Look up the JobId currently tracked for a key, if any.
    /// (Used by tests/diagnostics.)
    #[allow(dead_code)]
    pub fn job_id_for(&self, key: &str) -> Option<JobId> {
        self.in_flight.get(key).map(|e| e.job_id.clone())
    }

    #[cfg(test)]
    pub fn has_recent_result_for_test(&self, key: &str) -> bool {
        self.prune_expired();
        self.recent_results
            .lock()
            .entries
            .iter()
            .any(|entry| entry.key == key)
    }

    fn prune_expired(&self) {
        let mut recents = self.recent_results.lock();
        let cutoff = Instant::now();
        while recents
            .entries
            .front()
            .is_some_and(|result| cutoff.duration_since(result.inserted_at) > self.window)
        {
            if let Some(expired) = recents.entries.pop_front() {
                recents.retained_bytes = recents
                    .retained_bytes
                    .saturating_sub(expired.retained_bytes);
            }
        }
    }
}

fn estimated_result_bytes(result: &LLMResult<DispatchedResponse>) -> usize {
    match result {
        Ok(response) => response.estimated_retained_bytes(),
        Err(error) => std::mem::size_of_val(error).saturating_add(error.to_string().len()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::LLMError;
    use crate::trace::{LlmTraceContext, LlmTraceReceipt, LlmWorkloadClass};
    use crate::types::LLMResponse;

    fn dispatched(text: &str) -> DispatchedResponse {
        let trace_receipt = LlmTraceReceipt::direct(LlmTraceContext::legacy(
            Some("inflight-index-test"),
            LlmWorkloadClass::Evaluation,
        ));
        DispatchedResponse {
            response: Arc::new(LLMResponse {
                text: Some(Arc::<str>::from(text)),
                trace_receipt: Some(trace_receipt.clone()),
                ..LLMResponse::default()
            }),
            wait: Duration::ZERO,
            execution: Duration::ZERO,
            local_prep: None,
            attempts: 1,
            trace_receipt,
        }
    }

    #[test]
    fn cached_idempotent_results_share_the_immutable_provider_payload() {
        let index = InflightIndex::new(Duration::from_secs(30));
        let job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("same", job_id.clone()),
            LookupOutcome::Miss
        ));
        let owner = Arc::new(Ok(dispatched(&"x".repeat(64 * 1024))));
        let owner_payload = match owner.as_ref() {
            Ok(response) => Arc::clone(&response.response),
            Err(_) => unreachable!("fixture is successful"),
        };
        assert!(index.resolve("same", &job_id, owner));

        let LookupOutcome::Cached(cached) = index.lookup_or_register("same", JobId::new()) else {
            panic!("terminal result should be cached");
        };
        let cached_payload = match cached.as_ref() {
            Ok(response) => &response.response,
            Err(_) => unreachable!("fixture is successful"),
        };
        assert!(Arc::ptr_eq(&owner_payload, cached_payload));
    }

    #[test]
    fn recent_result_cache_evicts_by_bytes_even_below_entry_cap() {
        let first = dispatched(&"a".repeat(32 * 1024));
        let second = dispatched(&"b".repeat(32 * 1024));
        let byte_cap = first
            .estimated_retained_bytes()
            .max(second.estimated_retained_bytes())
            .saturating_add(1);
        let index = InflightIndex::with_limits(Duration::from_secs(30), 100, byte_cap);

        let first_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("first", first_job_id.clone()),
            LookupOutcome::Miss
        ));
        assert!(index.resolve("first", &first_job_id, Arc::new(Ok(first))));
        let second_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("second", second_job_id.clone()),
            LookupOutcome::Miss
        ));
        assert!(index.resolve("second", &second_job_id, Arc::new(Ok(second))));

        assert!(matches!(
            index.lookup_or_register("first", JobId::new()),
            LookupOutcome::Miss
        ));
        assert!(matches!(
            index.lookup_or_register("second", JobId::new()),
            LookupOutcome::Cached(_)
        ));
    }

    #[test]
    fn repeated_terminal_resolution_is_a_noop_for_the_completed_generation() {
        let index = InflightIndex::new(Duration::from_secs(30));
        let job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("same", job_id.clone()),
            LookupOutcome::Miss
        ));
        assert!(index.resolve("same", &job_id, Arc::new(Ok(dispatched("first")))));
        assert!(!index.resolve("same", &job_id, Arc::new(Ok(dispatched("repeated")))));

        let LookupOutcome::Cached(cached) = index.lookup_or_register("same", JobId::new()) else {
            panic!("latest terminal result should remain cached");
        };
        let cached = cached
            .as_ref()
            .as_ref()
            .expect("successful cached response");
        assert_eq!(cached.response.text.as_deref(), Some("first"));

        let recents = index.recent_results.lock();
        assert_eq!(recents.entries.len(), 1);
        assert_eq!(recents.retained_bytes, recents.entries[0].retained_bytes);
    }

    #[tokio::test]
    async fn stale_resolution_cannot_replace_a_new_owner_after_cache_eviction() {
        let index = InflightIndex::with_limits(Duration::from_secs(30), 100, 1);
        let old_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("reused", old_job_id.clone()),
            LookupOutcome::Miss
        ));
        assert!(index.resolve("reused", &old_job_id, Arc::new(Ok(dispatched("old"))),));

        // The one-byte cache cap evicts the old terminal value immediately,
        // so the same logical key can acquire a new generation.
        let new_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("reused", new_job_id.clone()),
            LookupOutcome::Miss
        ));
        let LookupOutcome::InFlight(mut new_subscriber) =
            index.lookup_or_register("reused", JobId::new())
        else {
            panic!("new generation should retain its subscriber");
        };

        assert!(!index.resolve(
            "reused",
            &old_job_id,
            Arc::new(Ok(dispatched("stale-repeat"))),
        ));
        assert_eq!(index.job_id_for("reused"), Some(new_job_id.clone()));

        assert!(index.resolve("reused", &new_job_id, Arc::new(Ok(dispatched("new"))),));
        let delivered = new_subscriber.recv().await.expect("new generation result");
        let delivered = delivered.as_ref().as_ref().expect("successful result");
        assert_eq!(delivered.response.text.as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn abandoned_generation_wakes_subscribers_without_caching_or_removing_a_new_owner() {
        let index = InflightIndex::new(Duration::from_secs(30));
        let old_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("cancelled", old_job_id.clone()),
            LookupOutcome::Miss
        ));
        let LookupOutcome::InFlight(mut old_subscriber) =
            index.lookup_or_register("cancelled", JobId::new())
        else {
            panic!("old generation should have an active subscriber");
        };
        let cancelled = Arc::new(Err(LLMError::Cancelled {
            reason: "owner_submission_cancelled".to_string(),
        }));
        assert!(index.abandon("cancelled", &old_job_id, Arc::clone(&cancelled)));
        assert!(Arc::ptr_eq(
            &cancelled,
            &old_subscriber
                .recv()
                .await
                .expect("subscriber cancellation")
        ));

        let new_job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("cancelled", new_job_id.clone()),
            LookupOutcome::Miss
        ));
        assert!(!index.abandon("cancelled", &old_job_id, cancelled));
        assert_eq!(index.job_id_for("cancelled"), Some(new_job_id));
    }

    #[tokio::test]
    async fn byte_eviction_never_drops_an_active_idempotent_subscriber() {
        let index = InflightIndex::with_limits(Duration::from_secs(30), 100, 1);
        let job_id = JobId::new();
        assert!(matches!(
            index.lookup_or_register("large", job_id.clone()),
            LookupOutcome::Miss
        ));
        let LookupOutcome::InFlight(mut subscriber) =
            index.lookup_or_register("large", JobId::new())
        else {
            panic!("second caller should subscribe to active owner");
        };
        let terminal = Arc::new(Ok(dispatched(&"z".repeat(128 * 1024))));
        assert!(index.resolve("large", &job_id, Arc::clone(&terminal)));

        let delivered = subscriber.recv().await.expect("subscriber result");
        assert!(Arc::ptr_eq(&terminal, &delivered));
        assert!(matches!(
            index.lookup_or_register("large", JobId::new()),
            LookupOutcome::Miss
        ));
    }
}

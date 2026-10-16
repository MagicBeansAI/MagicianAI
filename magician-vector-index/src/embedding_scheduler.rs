//! Process-wide admission control for the dedicated embedding daemon.
//!
//! Admission is parameterized by the daemon's verified physical parallelism.
//! Foreground retrieval always runs ahead of queued background work. At
//! capacity `1`, an in-flight provider request cannot be preempted, but every
//! document/centrality batch releases its permit so a queued retrieval is the
//! next request admitted. At larger capacities, background work deliberately
//! leaves one slot free for foreground retrieval.

use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicU64, AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::{Duration, Instant},
};

use anyhow::{anyhow, Result};
use tokio::sync::{Notify, OnceCell};

const QUERY_EMBEDDING_REUSE_GRACE: Duration = Duration::from_millis(25);
const MAX_COMPLETED_QUERY_EMBEDDINGS: usize = 128;
static QUERY_EMBEDDING_PROVIDER_CALLS: AtomicU64 = AtomicU64::new(0);
static QUERY_EMBEDDING_COALESCED_REUSES: AtomicU64 = AtomicU64::new(0);
static INSTALLED_EMBEDDING_CAPACITY: AtomicUsize = AtomicUsize::new(0);
/// Serializes tests in all crates that touch the process-global embedding
/// scheduler. Public only because dependent-crate tests share the same global.
#[doc(hidden)]
pub static EMBEDDING_ADMISSION_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

/// Install the single process-wide physical daemon capacity. Configuration
/// does this once before request traffic. Every admission path reads this same
/// value, so a caller carrying stale `num_parallel=2` cannot overlap a
/// capacity-one caller.
pub fn install_embedding_admission_capacity(capacity: usize) {
    INSTALLED_EMBEDDING_CAPACITY.store(capacity.max(1), Ordering::Release);
}

pub fn installed_embedding_admission_capacity() -> usize {
    INSTALLED_EMBEDDING_CAPACITY.load(Ordering::Acquire)
}

/// Initialize the global authority from the first standalone client, or reject
/// a client whose daemon contract disagrees with the already-installed value.
pub fn validate_embedding_admission_capacity(requested_capacity: usize) -> Result<usize> {
    let requested_capacity = requested_capacity.max(1);
    let installed = INSTALLED_EMBEDDING_CAPACITY.load(Ordering::Acquire);
    let installed = if installed == 0 {
        match INSTALLED_EMBEDDING_CAPACITY.compare_exchange(
            0,
            requested_capacity,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => requested_capacity,
            Err(concurrent) => concurrent,
        }
    } else {
        installed
    };
    if installed != requested_capacity {
        return Err(anyhow!(
            "embedding daemon capacity mismatch: process authority is {installed}, caller requested {requested_capacity}"
        ));
    }
    Ok(installed)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct QueryEmbeddingCoalescerStats {
    pub provider_calls: u64,
    pub coalesced_reuses: u64,
}

impl QueryEmbeddingCoalescerStats {
    pub fn saturating_delta(self, earlier: Self) -> Self {
        Self {
            provider_calls: self.provider_calls.saturating_sub(earlier.provider_calls),
            coalesced_reuses: self
                .coalesced_reuses
                .saturating_sub(earlier.coalesced_reuses),
        }
    }
}

pub fn query_embedding_coalescer_stats() -> QueryEmbeddingCoalescerStats {
    QueryEmbeddingCoalescerStats {
        provider_calls: QUERY_EMBEDDING_PROVIDER_CALLS.load(Ordering::Relaxed),
        coalesced_reuses: QUERY_EMBEDDING_COALESCED_REUSES.load(Ordering::Relaxed),
    }
}

type SharedQueryEmbeddingResult = std::result::Result<Arc<Vec<f32>>, Arc<str>>;

#[derive(Debug)]
struct QueryEmbeddingEntry {
    started_at: Instant,
    completed_at: OnceLock<Instant>,
    result: OnceCell<SharedQueryEmbeddingResult>,
    first_reuse_signal: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
}

impl QueryEmbeddingEntry {
    fn new(first_reuse_signal: Option<tokio::sync::oneshot::Sender<()>>) -> Self {
        Self {
            started_at: Instant::now(),
            completed_at: OnceLock::new(),
            result: OnceCell::new(),
            first_reuse_signal: Mutex::new(first_reuse_signal),
        }
    }
}

#[derive(Debug, Default)]
struct QueryEmbeddingCoalescer {
    entries: Mutex<HashMap<String, Arc<QueryEmbeddingEntry>>>,
    slot_available: Notify,
}

fn query_embedding_coalescer() -> &'static QueryEmbeddingCoalescer {
    static COALESCER: OnceLock<QueryEmbeddingCoalescer> = OnceLock::new();
    COALESCER.get_or_init(QueryEmbeddingCoalescer::default)
}

/// Share an exact query embedding across concurrent retrieval branches.
///
/// Successful results remain reusable for a very short post-completion grace so
/// sibling branches that reach the embedder after small scheduling or
/// candidate-loading skew still avoid a duplicate provider call. Failures are
/// removed immediately so fallback in one branch cannot suppress a healthy
/// retry in the next request.
#[cfg(test)]
pub(crate) async fn coalesce_query_embedding<F, Fut>(key: String, load: F) -> Result<Vec<f32>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<f32>>>,
{
    coalesce_query_embedding_until(key, None, load).await
}

/// Deadline-aware form used by request-path retrieval. When all bounded
/// coalescer slots contain live unique keys, a new key waits for a slot rather
/// than bypassing coalescing or failing immediately. The wait and the shared
/// result both consume the caller's original absolute query budget.
#[cfg(test)]
pub(crate) async fn coalesce_query_embedding_until<F, Fut>(
    key: String,
    deadline: Option<tokio::time::Instant>,
    load: F,
) -> Result<Vec<f32>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<f32>>>,
{
    coalesce_query_embedding_until_with_reuse_signal(key, deadline, None, load).await
}

/// Deadline-aware coalescing with an exact-key first-reuse signal for live
/// contention evals. The signal is attached only when this call creates the
/// entry and fires only when another caller joins that same key.
pub(crate) async fn coalesce_query_embedding_until_with_reuse_signal<F, Fut>(
    key: String,
    deadline: Option<tokio::time::Instant>,
    first_reuse_signal: Option<tokio::sync::oneshot::Sender<()>>,
    load: F,
) -> Result<Vec<f32>>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<Vec<f32>>>,
{
    let coalescer = query_embedding_coalescer();
    let mut first_reuse_signal = first_reuse_signal;
    let (entry, reused) = loop {
        // Register before inspecting the map so completion/removal cannot race
        // between the full-map observation and this wait.
        let slot_available = coalescer.slot_available.notified();
        let admission = {
            let now = Instant::now();
            let mut entries = coalescer
                .entries
                .lock()
                .expect("query embedding coalescer mutex poisoned");
            entries.retain(|_, entry| {
                entry.completed_at.get().is_none_or(|completed_at| {
                    now.saturating_duration_since(*completed_at) < QUERY_EMBEDDING_REUSE_GRACE
                })
            });

            if let Some(entry) = entries.get(&key) {
                if let Some(signal) = entry
                    .first_reuse_signal
                    .lock()
                    .expect("query embedding reuse-signal mutex poisoned")
                    .take()
                {
                    let _ = signal.send(());
                }
                Some((Arc::clone(entry), true))
            } else {
                if entries.len() >= MAX_COMPLETED_QUERY_EMBEDDINGS {
                    let oldest_completed_key = entries
                        .iter()
                        .filter(|(_, entry)| entry.result.get().is_some())
                        .min_by_key(|(_, entry)| {
                            entry
                                .completed_at
                                .get()
                                .copied()
                                .unwrap_or(entry.started_at)
                        })
                        .map(|(key, _)| key.clone());
                    if let Some(oldest_completed_key) = oldest_completed_key {
                        entries.remove(&oldest_completed_key);
                    }
                }
                if entries.len() < MAX_COMPLETED_QUERY_EMBEDDINGS {
                    let entry = Arc::new(QueryEmbeddingEntry::new(first_reuse_signal.take()));
                    entries.insert(key.clone(), Arc::clone(&entry));
                    Some((entry, false))
                } else {
                    None
                }
            }
        };
        if let Some(admission) = admission {
            break admission;
        }
        if let Some(deadline) = deadline {
            tokio::time::timeout_at(deadline, slot_available)
                .await
                .map_err(|_| {
                    anyhow!("query embedding coalescer admission exhausted the request deadline")
                })?;
        } else {
            slot_available.await;
        }
    };

    if reused {
        QUERY_EMBEDDING_COALESCED_REUSES.fetch_add(1, Ordering::Relaxed);
    } else {
        QUERY_EMBEDDING_PROVIDER_CALLS.fetch_add(1, Ordering::Relaxed);
    }
    tracing::trace!(reused, "query embedding coalescer admission");
    let lease = QueryEmbeddingLease {
        coalescer,
        key: key.clone(),
        entry,
    };
    let load = load;
    let shared_future = lease.entry.result.get_or_init(|| async move {
        load()
            .await
            .map(Arc::new)
            .map_err(|error| Arc::<str>::from(format!("{error:#}")))
    });
    let shared = if let Some(deadline) = deadline {
        tokio::time::timeout_at(deadline, shared_future)
            .await
            .map_err(|_| anyhow!("query embedding exhausted the request deadline"))?
    } else {
        shared_future.await
    };
    lease.entry.completed_at.get_or_init(Instant::now);
    coalescer.slot_available.notify_waiters();

    if shared.is_err() {
        let mut entries = coalescer
            .entries
            .lock()
            .expect("query embedding coalescer mutex poisoned");
        if entries
            .get(&key)
            .is_some_and(|current| Arc::ptr_eq(current, &lease.entry))
        {
            entries.remove(&key);
            coalescer.slot_available.notify_waiters();
        }
    }

    match shared {
        Ok(vector) => Ok(vector.as_ref().clone()),
        Err(error) => Err(anyhow!(error.to_string())),
    }
}

/// Removes an abandoned live-key reservation when the last caller waiting on
/// it is cancelled. Without this guard, 128 cancelled leaders could consume
/// the bounded coalescer forever.
struct QueryEmbeddingLease {
    coalescer: &'static QueryEmbeddingCoalescer,
    key: String,
    entry: Arc<QueryEmbeddingEntry>,
}

impl Drop for QueryEmbeddingLease {
    fn drop(&mut self) {
        if self.entry.result.get().is_some() {
            return;
        }
        let mut entries = self
            .coalescer
            .entries
            .lock()
            .expect("query embedding coalescer mutex poisoned");
        // The map and this lease are the final two owners only when every
        // sibling waiter has gone away.
        if Arc::strong_count(&self.entry) == 2
            && entries
                .get(&self.key)
                .is_some_and(|current| Arc::ptr_eq(current, &self.entry))
        {
            entries.remove(&self.key);
            drop(entries);
            self.coalescer.slot_available.notify_waiters();
        }
    }
}

/// Clear only the query-coalescer's ephemeral process state. This is exported
/// solely so dependent-crate tests can isolate global-state assertions.
#[doc(hidden)]
pub fn reset_query_embedding_coalescer_for_tests() {
    query_embedding_coalescer()
        .entries
        .lock()
        .expect("query embedding coalescer mutex poisoned")
        .clear();
    query_embedding_coalescer().slot_available.notify_waiters();
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingPriority {
    /// Latency-sensitive request-path retrieval.
    Read,
    /// Optional/background query work such as resurfacing centrality.
    BackgroundRead,
    /// Background document/index construction.
    Write,
}

#[derive(Debug, Default)]
struct AdmissionState {
    active_foreground_reads: usize,
    active_background_reads: usize,
    active_write: bool,
    waiting_foreground_reads: usize,
    waiting_writes: usize,
    oldest_write_wait_started: Option<Instant>,
}

#[derive(Debug, Default)]
struct EmbeddingAdmission {
    state: Mutex<AdmissionState>,
    notify: Notify,
}

/// Read-only, content-free admission snapshot for diagnostics and live evals.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EmbeddingAdmissionStats {
    pub installed_capacity: usize,
    pub active_foreground_reads: usize,
    pub active_background_reads: usize,
    pub active_write: bool,
    pub waiting_foreground_reads: usize,
    pub waiting_writes: usize,
    pub oldest_write_wait_ms: usize,
}

fn admission() -> &'static EmbeddingAdmission {
    static ADMISSION: OnceLock<EmbeddingAdmission> = OnceLock::new();
    ADMISSION.get_or_init(EmbeddingAdmission::default)
}

pub fn embedding_admission_stats() -> EmbeddingAdmissionStats {
    let admission = admission();
    let state = admission
        .state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    EmbeddingAdmissionStats {
        installed_capacity: installed_embedding_admission_capacity(),
        active_foreground_reads: state.active_foreground_reads,
        active_background_reads: state.active_background_reads,
        active_write: state.active_write,
        waiting_foreground_reads: state.waiting_foreground_reads,
        waiting_writes: state.waiting_writes,
        oldest_write_wait_ms: state
            .oldest_write_wait_started
            .map(|started| {
                Instant::now()
                    .saturating_duration_since(started)
                    .as_millis()
                    .min(usize::MAX as u128) as usize
            })
            .unwrap_or(0),
    }
}

#[derive(Debug)]
pub struct EmbeddingPermit {
    priority: EmbeddingPriority,
    admission: &'static EmbeddingAdmission,
}

impl Drop for EmbeddingPermit {
    fn drop(&mut self) {
        if let Ok(mut state) = self.admission.state.lock() {
            match self.priority {
                EmbeddingPriority::Read => {
                    state.active_foreground_reads = state.active_foreground_reads.saturating_sub(1)
                },
                EmbeddingPriority::BackgroundRead => {
                    state.active_background_reads = state.active_background_reads.saturating_sub(1)
                },
                EmbeddingPriority::Write => state.active_write = false,
            }
        }
        self.admission.notify.notify_waiters();
    }
}

struct WaitingForegroundRead {
    admission: &'static EmbeddingAdmission,
    registered: bool,
}

struct WaitingWrite {
    admission: &'static EmbeddingAdmission,
    registered: bool,
}

impl WaitingWrite {
    fn register(admission: &'static EmbeddingAdmission) -> Self {
        if let Ok(mut state) = admission.state.lock() {
            state.waiting_writes = state.waiting_writes.saturating_add(1);
            if state.oldest_write_wait_started.is_none() {
                state.oldest_write_wait_started = Some(Instant::now());
            }
        }
        Self {
            admission,
            registered: true,
        }
    }

    fn admit(mut self) {
        self.registered = false;
    }
}

impl Drop for WaitingWrite {
    fn drop(&mut self) {
        if self.registered {
            if let Ok(mut state) = self.admission.state.lock() {
                state.waiting_writes = state.waiting_writes.saturating_sub(1);
                if state.waiting_writes == 0 {
                    state.oldest_write_wait_started = None;
                }
            }
            self.admission.notify.notify_waiters();
        }
    }
}

impl WaitingForegroundRead {
    fn register(admission: &'static EmbeddingAdmission) -> Self {
        if let Ok(mut state) = admission.state.lock() {
            state.waiting_foreground_reads = state.waiting_foreground_reads.saturating_add(1);
        }
        Self {
            admission,
            registered: true,
        }
    }

    fn admit(mut self) {
        self.registered = false;
    }
}

impl Drop for WaitingForegroundRead {
    fn drop(&mut self) {
        if self.registered {
            if let Ok(mut state) = self.admission.state.lock() {
                state.waiting_foreground_reads = state.waiting_foreground_reads.saturating_sub(1);
            }
            self.admission.notify.notify_waiters();
        }
    }
}

/// Acquire against the conservative single-slot default.
///
/// Kept for compatibility with callers that do not own an
/// [`OllamaEmbedderConfig`](crate::vector_toolkit::OllamaEmbedderConfig). New
/// HTTP clients should pass their verified daemon capacity through
/// [`acquire_embedding_permit_with_capacity`].
pub async fn acquire_embedding_permit(priority: EmbeddingPriority) -> EmbeddingPermit {
    let capacity = installed_embedding_admission_capacity().max(1);
    acquire_embedding_permit_with_capacity(priority, capacity)
        .await
        .expect("default embedding admission capacity must match process authority")
}

/// Acquire one embedding-provider slot with foreground-priority admission.
///
/// The first standalone caller initializes the process-wide authority. Later
/// callers must match it exactly or fail closed; the runtime config installer
/// may set it explicitly before traffic.
/// Background work uses at most
/// `capacity - 1` slots when capacity is larger than one, reserving one slot for
/// retrieval; at capacity one it may use the sole slot but must release it
/// between batches. A queued foreground request prevents any new background
/// request from starting.
pub async fn acquire_embedding_permit_with_capacity(
    priority: EmbeddingPriority,
    requested_capacity: usize,
) -> Result<EmbeddingPermit> {
    acquire_embedding_permit_with_capacity_inner(priority, requested_capacity, None).await
}

/// Eval-only exact-loader hook. For foreground reads, `wait_started` is sent
/// immediately after this request registers as a waiter and before admission.
#[doc(hidden)]
pub async fn acquire_embedding_permit_with_capacity_and_wait_signal(
    priority: EmbeddingPriority,
    requested_capacity: usize,
    wait_started: tokio::sync::oneshot::Sender<()>,
) -> Result<EmbeddingPermit> {
    acquire_embedding_permit_with_capacity_inner(priority, requested_capacity, Some(wait_started))
        .await
}

async fn acquire_embedding_permit_with_capacity_inner(
    priority: EmbeddingPriority,
    requested_capacity: usize,
    wait_started: Option<tokio::sync::oneshot::Sender<()>>,
) -> Result<EmbeddingPermit> {
    let admission = admission();
    let capacity = validate_embedding_admission_capacity(requested_capacity)?;
    let waiting_read =
        (priority == EmbeddingPriority::Read).then(|| WaitingForegroundRead::register(admission));
    let waiting_write =
        (priority == EmbeddingPriority::Write).then(|| WaitingWrite::register(admission));
    if let Some(wait_started) = wait_started {
        let _ = wait_started.send(());
    }

    loop {
        let notified = admission.notify.notified();
        let admitted = {
            let mut state = admission
                .state
                .lock()
                .expect("embedding admission mutex poisoned");
            let active_total = state.active_foreground_reads
                + state.active_background_reads
                + usize::from(state.active_write);
            let background_capacity = capacity.saturating_sub(1).max(1);
            match priority {
                EmbeddingPriority::Read if active_total < capacity => {
                    state.active_foreground_reads = state.active_foreground_reads.saturating_add(1);
                    state.waiting_foreground_reads =
                        state.waiting_foreground_reads.saturating_sub(1);
                    true
                },
                EmbeddingPriority::BackgroundRead
                    if !state.active_write
                        && state.waiting_foreground_reads == 0
                        && state.waiting_writes == 0
                        && state.active_background_reads < background_capacity
                        && active_total < capacity =>
                {
                    state.active_background_reads = state.active_background_reads.saturating_add(1);
                    true
                },
                EmbeddingPriority::Write
                    if !state.active_write
                        && state.active_foreground_reads == 0
                        && state.active_background_reads == 0
                        && state.waiting_foreground_reads == 0
                        && active_total < capacity =>
                {
                    state.active_write = true;
                    state.waiting_writes = state.waiting_writes.saturating_sub(1);
                    if state.waiting_writes == 0 {
                        state.oldest_write_wait_started = None;
                    }
                    true
                },
                _ => false,
            }
        };
        if admitted {
            if let Some(waiting_read) = waiting_read {
                waiting_read.admit();
            }
            if let Some(waiting_write) = waiting_write {
                waiting_write.admit();
            }
            return Ok(EmbeddingPermit {
                priority,
                admission,
            });
        }
        notified.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::time::{sleep, timeout, Duration};

    #[tokio::test]
    async fn foreground_runs_next_after_active_background_at_capacity_one() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(1);
        let first_write = acquire_embedding_permit(EmbeddingPriority::Write).await;
        let next_write =
            tokio::spawn(async move { acquire_embedding_permit(EmbeddingPriority::Write).await });
        let read = tokio::spawn(async { acquire_embedding_permit(EmbeddingPriority::Read).await });
        sleep(Duration::from_millis(10)).await;
        assert!(!read.is_finished());
        assert!(!next_write.is_finished());

        drop(first_write);
        let read = timeout(Duration::from_secs(1), read)
            .await
            .expect("foreground read should run next")
            .expect("foreground task should join");
        assert!(
            !next_write.is_finished(),
            "queued background work must remain behind the active foreground request"
        );
        drop(read);
        let next_write = timeout(Duration::from_secs(1), next_write)
            .await
            .expect("background write should resume")
            .expect("background task should join");
        drop(next_write);
    }

    #[tokio::test]
    async fn oldest_write_wait_is_visible_while_foreground_holds_the_slot() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(1);
        let read = acquire_embedding_permit(EmbeddingPriority::Read).await;
        let write =
            tokio::spawn(async { acquire_embedding_permit(EmbeddingPriority::Write).await });
        sleep(Duration::from_millis(25)).await;
        let stats = embedding_admission_stats();
        assert_eq!(stats.waiting_writes, 1);
        assert!(
            stats.oldest_write_wait_ms >= 10,
            "starved durable writes must expose age, got {}ms",
            stats.oldest_write_wait_ms
        );
        drop(read);
        drop(timeout(Duration::from_secs(1), write).await.unwrap());
        assert_eq!(embedding_admission_stats().oldest_write_wait_ms, 0);
    }

    #[tokio::test]
    async fn cancelled_foreground_waiter_does_not_strand_background_work() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(1);
        let active_background = acquire_embedding_permit(EmbeddingPriority::Write).await;
        let cancelled_read = tokio::spawn(async {
            acquire_embedding_permit_with_capacity(EmbeddingPriority::Read, 1).await
        });
        sleep(Duration::from_millis(10)).await;
        assert!(!cancelled_read.is_finished());
        cancelled_read.abort();
        let _ = cancelled_read.await;
        drop(active_background);

        let write_after_cancellation = timeout(
            Duration::from_secs(1),
            acquire_embedding_permit(EmbeddingPriority::Write),
        )
        .await
        .expect("a cancelled read must not leave the write lane starved");
        drop(write_after_cancellation);
    }

    #[tokio::test]
    async fn verified_capacity_is_honored_and_reserves_foreground_slot() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(2);
        let background =
            acquire_embedding_permit_with_capacity(EmbeddingPriority::BackgroundRead, 2)
                .await
                .unwrap();
        let foreground = acquire_embedding_permit_with_capacity(EmbeddingPriority::Read, 2)
            .await
            .unwrap();
        let second_background = tokio::spawn(async {
            acquire_embedding_permit_with_capacity(EmbeddingPriority::BackgroundRead, 2).await
        });
        sleep(Duration::from_millis(10)).await;
        assert!(
            !second_background.is_finished(),
            "background work must not consume the reserved foreground slot"
        );
        drop(background);
        let admitted_background = timeout(Duration::from_secs(1), second_background)
            .await
            .expect("background work should resume when its lane is free")
            .expect("background task should join")
            .expect("background capacity should match");
        drop(foreground);
        drop(admitted_background);
        install_embedding_admission_capacity(1);
    }

    #[tokio::test]
    async fn durable_write_runs_before_queued_optional_background_read() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(1);
        let active = acquire_embedding_permit_with_capacity(EmbeddingPriority::BackgroundRead, 1)
            .await
            .unwrap();
        let write = tokio::spawn(async {
            acquire_embedding_permit_with_capacity(EmbeddingPriority::Write, 1).await
        });
        sleep(Duration::from_millis(10)).await;
        let next_optional = tokio::spawn(async {
            acquire_embedding_permit_with_capacity(EmbeddingPriority::BackgroundRead, 1).await
        });
        sleep(Duration::from_millis(10)).await;
        drop(active);

        let write = timeout(Duration::from_secs(1), write)
            .await
            .expect("durable write should run next")
            .expect("write task should join")
            .expect("write capacity should match");
        assert!(!next_optional.is_finished());
        drop(write);
        let optional = timeout(Duration::from_secs(1), next_optional)
            .await
            .expect("optional work should eventually resume")
            .expect("optional task should join")
            .expect("optional capacity should match");
        drop(optional);
    }

    #[tokio::test]
    async fn admission_stats_report_active_and_waiting_transitions_without_payload_data() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        install_embedding_admission_capacity(1);
        let background = acquire_embedding_permit(EmbeddingPriority::BackgroundRead).await;
        let active = embedding_admission_stats();
        assert_eq!(active.installed_capacity, 1);
        assert_eq!(active.active_background_reads, 1);
        assert_eq!(active.active_foreground_reads, 0);

        let foreground =
            tokio::spawn(async { acquire_embedding_permit(EmbeddingPriority::Read).await });
        timeout(Duration::from_secs(1), async {
            loop {
                if embedding_admission_stats().waiting_foreground_reads == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("foreground waiter should become observable");
        drop(background);
        let foreground = timeout(Duration::from_secs(1), foreground)
            .await
            .expect("foreground should be admitted")
            .expect("foreground task should join");
        let active = embedding_admission_stats();
        assert_eq!(active.active_foreground_reads, 1);
        assert_eq!(active.waiting_foreground_reads, 0);
        drop(foreground);
        assert_eq!(embedding_admission_stats().active_foreground_reads, 0);
    }

    #[tokio::test]
    async fn standalone_capacity_initializes_once_and_mismatch_fails_closed() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        INSTALLED_EMBEDDING_CAPACITY.store(0, Ordering::Release);
        let permit = acquire_embedding_permit_with_capacity(EmbeddingPriority::Read, 2)
            .await
            .expect("first standalone client initializes authority");
        drop(permit);
        assert_eq!(installed_embedding_admission_capacity(), 2);
        let error = acquire_embedding_permit_with_capacity(EmbeddingPriority::Read, 1)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("capacity mismatch"));
        install_embedding_admission_capacity(1);
    }

    #[tokio::test]
    async fn identical_query_embeddings_share_in_flight_and_recent_work() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        reset_query_embedding_coalescer_for_tests();
        let key = format!("coalesced-query-{}", std::process::id());
        let calls = Arc::new(AtomicUsize::new(0));
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let first = coalesce_query_embedding(key.clone(), move || async move {
            first_calls.fetch_add(1, Ordering::SeqCst);
            sleep(Duration::from_millis(25)).await;
            Ok(vec![1.0, 2.0])
        });
        let second = coalesce_query_embedding(key.clone(), move || async move {
            second_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![9.0, 9.0])
        });

        let (first, second) = tokio::join!(first, second);
        assert_eq!(first.expect("first embedding"), vec![1.0, 2.0]);
        assert_eq!(second.expect("shared embedding"), vec![1.0, 2.0]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let cached_calls = Arc::clone(&calls);
        let cached = coalesce_query_embedding(key, move || async move {
            cached_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![8.0, 8.0])
        })
        .await
        .expect("recent embedding");
        assert_eq!(cached, vec![1.0, 2.0]);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        sleep(QUERY_EMBEDDING_REUSE_GRACE + Duration::from_millis(5)).await;
        let expired_calls = Arc::clone(&calls);
        let expired = coalesce_query_embedding(
            format!("coalesced-query-{}", std::process::id()),
            move || async move {
                expired_calls.fetch_add(1, Ordering::SeqCst);
                Ok(vec![5.0, 6.0])
            },
        )
        .await
        .expect("expired embedding");
        assert_eq!(expired, vec![5.0, 6.0]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn unique_inflight_query_keys_are_bounded_without_evicting_live_entries() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        reset_query_embedding_coalescer_for_tests();
        let prefix = format!("coalescer-bound-{}-", std::process::id());
        let mut handles = Vec::new();
        for index in 0..MAX_COMPLETED_QUERY_EMBEDDINGS {
            let key = format!("{prefix}{index}");
            handles.push(tokio::spawn(async move {
                coalesce_query_embedding(key, || async {
                    std::future::pending::<Result<Vec<f32>>>().await
                })
                .await
            }));
        }
        timeout(Duration::from_secs(1), async {
            loop {
                let count = query_embedding_coalescer()
                    .entries
                    .lock()
                    .expect("coalescer mutex")
                    .keys()
                    .filter(|key| key.starts_with(&prefix))
                    .count();
                if count == MAX_COMPLETED_QUERY_EMBEDDINGS {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("all bounded in-flight keys should register");
        let overflow = tokio::spawn(coalesce_query_embedding_until(
            format!("{prefix}overflow"),
            Some(tokio::time::Instant::now() + Duration::from_secs(1)),
            || async { Ok(vec![1.0]) },
        ));
        sleep(Duration::from_millis(10)).await;
        assert!(
            !overflow.is_finished(),
            "the bounded 129th key should backpressure, not fail or bypass"
        );
        let cancelled = handles.pop().expect("one live reservation");
        cancelled.abort();
        let _ = cancelled.await;
        assert_eq!(
            timeout(Duration::from_secs(1), overflow)
                .await
                .expect("backpressured key should wake")
                .expect("overflow task")
                .expect("overflow embedding"),
            vec![1.0]
        );
        for handle in handles {
            handle.abort();
            let _ = handle.await;
        }
        reset_query_embedding_coalescer_for_tests();
    }

    #[tokio::test]
    async fn shared_query_enforces_each_waiters_deadline_in_both_arrival_orders() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        reset_query_embedding_coalescer_for_tests();

        // A short follower must time out without cancelling or extending a
        // long-budget leader.
        let key = format!("deadline-long-first-{}", std::process::id());
        let long = tokio::spawn(coalesce_query_embedding_until(
            key.clone(),
            Some(tokio::time::Instant::now() + Duration::from_millis(250)),
            || async {
                sleep(Duration::from_millis(75)).await;
                Ok(vec![1.0])
            },
        ));
        sleep(Duration::from_millis(5)).await;
        let short = coalesce_query_embedding_until(
            key,
            Some(tokio::time::Instant::now() + Duration::from_millis(15)),
            || async { Ok(vec![9.0]) },
        )
        .await;
        assert!(short.unwrap_err().to_string().contains("request deadline"));
        assert_eq!(long.await.unwrap().unwrap(), vec![1.0]);

        // If the short-budget leader expires, its cancelled initializer must
        // let the long waiter run its own loader rather than inherit failure.
        let key = format!("deadline-short-first-{}", std::process::id());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let short = tokio::spawn(coalesce_query_embedding_until(
            key.clone(),
            Some(tokio::time::Instant::now() + Duration::from_millis(20)),
            || async {
                let _ = started_tx.send(());
                std::future::pending::<Result<Vec<f32>>>().await
            },
        ));
        started_rx.await.expect("short loader started");
        let long = coalesce_query_embedding_until(
            key,
            Some(tokio::time::Instant::now() + Duration::from_millis(250)),
            || async { Ok(vec![2.0]) },
        );
        let (short, long) = tokio::join!(short, long);
        assert!(short
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("request deadline"));
        assert_eq!(long.unwrap(), vec![2.0]);
        reset_query_embedding_coalescer_for_tests();
    }

    #[tokio::test]
    async fn failed_query_embeddings_are_not_reused() {
        let _test_guard = EMBEDDING_ADMISSION_TEST_LOCK.lock().await;
        reset_query_embedding_coalescer_for_tests();
        let key = format!("failed-query-{}", std::process::id());
        let calls = Arc::new(AtomicUsize::new(0));
        let failed_calls = Arc::clone(&calls);
        let failed = coalesce_query_embedding(key.clone(), move || async move {
            failed_calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("provider unavailable"))
        })
        .await;
        assert!(failed.is_err());

        let retry_calls = Arc::clone(&calls);
        let retry = coalesce_query_embedding(key, move || async move {
            retry_calls.fetch_add(1, Ordering::SeqCst);
            Ok(vec![3.0, 4.0])
        })
        .await
        .expect("retry embedding");
        assert_eq!(retry, vec![3.0, 4.0]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}

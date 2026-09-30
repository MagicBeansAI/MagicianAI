//! Bounded admission for Magician-owned `spawn_blocking` work.
//!
//! Tokio blocking pools keep their default width. This module limits how
//! many Magician-owned blocking jobs are *submitted*, so a 300-task burst
//! cannot dump unbounded fsync/SQLite work onto those pools.
//!
//! [`spawn_blocking_admitted`] acquires a permit on the async runtime, then
//! calls [`tokio::task::spawn_blocking`]. Never acquire a permit from inside
//! a blocking thread: the closure must not call [`spawn_blocking_admitted`]
//! or the internal acquire. Nested awaits from async code are fine.
//!
//! `configure_blocking_admission(0)` is unlimited (no wait) and still
//! records in-flight. `MAGICIAN_BLOCKING_ADMISSION=off` (also
//! `pass_through` / `0` / `false`) skips the semaphore and still counts
//! in-flight.

use std::{
    cell::Cell,
    fmt,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
    time::Instant,
};

use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinError,
};

/// Seed-profile (`current` / `auto`) default. Not Tokio's 512-cap.
pub const DEFAULT_BLOCKING_ADMISSION_PERMITS: usize = 16;

/// Restart-bound kill switch. `off` / `pass_through` / `0` / `false` skip
/// the semaphore.
pub const BLOCKING_ADMISSION_ENV: &str = "MAGICIAN_BLOCKING_ADMISSION";

thread_local! {
    static IN_ADMITTED_BLOCKING: Cell<bool> = Cell::new(false);
}

static WAITING: AtomicUsize = AtomicUsize::new(0);
static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static IN_FLIGHT_HIGH_WATER: AtomicUsize = AtomicUsize::new(0);
static WAIT_HIGH_WATER_MS: AtomicUsize = AtomicUsize::new(0);
static ADMITTED_TOTAL: AtomicUsize = AtomicUsize::new(0);
static WOULD_THROTTLE_TOTAL: AtomicUsize = AtomicUsize::new(0);

struct AdmissionSlots {
    permits: AtomicUsize,
    semaphore: Mutex<Option<Arc<Semaphore>>>,
}

fn slots() -> &'static AdmissionSlots {
    static SLOTS: OnceLock<AdmissionSlots> = OnceLock::new();
    SLOTS.get_or_init(|| AdmissionSlots {
        permits: AtomicUsize::new(DEFAULT_BLOCKING_ADMISSION_PERMITS),
        semaphore: Mutex::new(Some(Arc::new(Semaphore::new(
            DEFAULT_BLOCKING_ADMISSION_PERMITS,
        )))),
    })
}

fn in_admitted_blocking() -> bool {
    IN_ADMITTED_BLOCKING.with(Cell::get)
}

pub(crate) fn is_pass_through_value(raw: &str) -> bool {
    let value = raw.trim();
    value.eq_ignore_ascii_case("off")
        || value.eq_ignore_ascii_case("pass_through")
        || value == "0"
        || value.eq_ignore_ascii_case("false")
}

fn env_pass_through() -> bool {
    std::env::var(BLOCKING_ADMISSION_ENV)
        .ok()
        .is_some_and(|value| is_pass_through_value(&value))
}

fn current_semaphore() -> Option<Arc<Semaphore>> {
    slots()
        .semaphore
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
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

/// Set the process-wide permit count. `0` is unlimited (no wait). Boot
/// calls this from the resolved runtime plan; later calls replace the
/// semaphore.
pub fn configure_blocking_admission(permits: usize) {
    let slots = slots();
    slots.permits.store(permits, Ordering::SeqCst);
    let mut semaphore = slots
        .semaphore
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *semaphore = if permits == 0 {
        None
    } else {
        Some(Arc::new(Semaphore::new(permits)))
    };
}

/// Configured permit count. `0` is unlimited. The env kill switch does not
/// change this number; it skips the semaphore at spawn time.
pub fn blocking_admission_permits() -> usize {
    slots().permits.load(Ordering::SeqCst)
}

/// Env kill switch is active (`off` / `pass_through` / `0` / `false`).
pub fn blocking_admission_pass_through() -> bool {
    env_pass_through()
}

/// Why [`spawn_blocking_admitted`] failed.
#[derive(Debug)]
pub enum BlockingAdmissionError {
    /// The blocking task panicked.
    Join(JoinError),
    /// Caller tried to acquire from inside an admitted blocking closure.
    NestedFromBlocking,
    /// Process semaphore was closed (should not happen in production).
    Closed,
}

impl fmt::Display for BlockingAdmissionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Join(error) => {
                write!(f, "admitted blocking task panicked: {error}")
            },
            Self::NestedFromBlocking => write!(
                f,
                "spawn_blocking_admitted must not be called from inside a blocking thread"
            ),
            Self::Closed => write!(f, "blocking admission semaphore closed"),
        }
    }
}

impl std::error::Error for BlockingAdmissionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Join(error) => Some(error),
            Self::NestedFromBlocking | Self::Closed => None,
        }
    }
}

/// Process-local blocking-admission gauges.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockingAdmissionSnapshot {
    pub permits: usize,
    pub pass_through: bool,
    pub waiting: usize,
    pub in_flight: usize,
    pub high_water: usize,
    pub wait_high_water_ms: usize,
    pub admitted_total: usize,
    pub would_throttle_total: usize,
}

pub fn blocking_admission_snapshot() -> BlockingAdmissionSnapshot {
    BlockingAdmissionSnapshot {
        permits: blocking_admission_permits(),
        pass_through: env_pass_through(),
        waiting: WAITING.load(Ordering::Relaxed),
        in_flight: IN_FLIGHT.load(Ordering::Relaxed),
        high_water: IN_FLIGHT_HIGH_WATER.load(Ordering::Relaxed),
        wait_high_water_ms: WAIT_HIGH_WATER_MS.load(Ordering::Relaxed),
        admitted_total: ADMITTED_TOTAL.load(Ordering::Relaxed),
        would_throttle_total: WOULD_THROTTLE_TOTAL.load(Ordering::Relaxed),
    }
}

struct WaitingGuard;

impl Drop for WaitingGuard {
    fn drop(&mut self) {
        decrement_saturating(&WAITING, 1);
    }
}

struct InFlightGuard;

impl InFlightGuard {
    fn enter() -> Self {
        let now = IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
        update_atomic_max(&IN_FLIGHT_HIGH_WATER, now);
        ADMITTED_TOTAL.fetch_add(1, Ordering::Relaxed);
        Self
    }
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        decrement_saturating(&IN_FLIGHT, 1);
    }
}

struct NestedBlockingGuard;

impl NestedBlockingGuard {
    fn enter() -> Self {
        IN_ADMITTED_BLOCKING.with(|flag| flag.set(true));
        Self
    }
}

impl Drop for NestedBlockingGuard {
    fn drop(&mut self) {
        IN_ADMITTED_BLOCKING.with(|flag| flag.set(false));
    }
}

async fn acquire_permit() -> Result<Option<OwnedSemaphorePermit>, BlockingAdmissionError> {
    if in_admitted_blocking() {
        return Err(BlockingAdmissionError::NestedFromBlocking);
    }
    if env_pass_through() {
        return Ok(None);
    }
    if blocking_admission_permits() == 0 {
        return Ok(None);
    }
    let Some(semaphore) = current_semaphore() else {
        return Ok(None);
    };

    WAITING.fetch_add(1, Ordering::Relaxed);
    let _waiting = WaitingGuard;
    let started = Instant::now();
    let permit = semaphore
        .acquire_owned()
        .await
        .map_err(|_| BlockingAdmissionError::Closed)?;
    let wait_ms = started.elapsed().as_millis().min(usize::MAX as u128) as usize;
    update_atomic_max(&WAIT_HIGH_WATER_MS, wait_ms);
    if wait_ms > 0 {
        WOULD_THROTTLE_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
    Ok(Some(permit))
}

/// Held admission slot. Dropping it releases the semaphore without spawning
/// work. [`spawn_blocking_with_admission`] consumes it.
///
/// In-flight is counted from acquire until this value is dropped (after the
/// blocking job, or earlier if the caller aborts before spawning). Durable
/// writes acquire before rename so a saturated pool cannot leave a published
/// file waiting on parent-dir fsync.
pub struct BlockingAdmissionPermit {
    // Ownership, rather than reads, keeps the semaphore slot reserved.
    _inner: Option<OwnedSemaphorePermit>,
    _in_flight: InFlightGuard,
}

/// Acquire a permit on the async runtime. Must not be called from an
/// admitted blocking thread.
pub async fn acquire_blocking_admission() -> Result<BlockingAdmissionPermit, BlockingAdmissionError>
{
    let inner = acquire_permit().await?;
    Ok(BlockingAdmissionPermit {
        _inner: inner,
        _in_flight: InFlightGuard::enter(),
    })
}

/// Run `f` on Tokio's blocking pool using an already-held permit.
pub async fn spawn_blocking_with_admission<F, R>(
    permit: BlockingAdmissionPermit,
    f: F,
) -> Result<R, BlockingAdmissionError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    if in_admitted_blocking() {
        return Err(BlockingAdmissionError::NestedFromBlocking);
    }
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let _nested = NestedBlockingGuard::enter();
        f()
    })
    .await
    .map_err(BlockingAdmissionError::Join)
}

/// Acquire (async), then [`tokio::task::spawn_blocking`].
///
/// The closure runs on a Tokio blocking thread and must not call this
/// function or otherwise acquire an admission permit.
pub async fn spawn_blocking_admitted<F, R>(f: F) -> Result<R, BlockingAdmissionError>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    let permit = acquire_blocking_admission().await?;
    spawn_blocking_with_admission(permit, f).await
}

#[cfg(test)]
pub(crate) static BLOCKING_ADMISSION_TEST_LOCK: tokio::sync::Mutex<()> =
    tokio::sync::Mutex::const_new(());

#[cfg(test)]
static TEST_LOCK_OWNER: Mutex<Option<tokio::task::Id>> = Mutex::new(None);

#[cfg(test)]
fn reset_metrics() {
    WAITING.store(0, Ordering::SeqCst);
    IN_FLIGHT.store(0, Ordering::SeqCst);
    IN_FLIGHT_HIGH_WATER.store(0, Ordering::SeqCst);
    WAIT_HIGH_WATER_MS.store(0, Ordering::SeqCst);
    ADMITTED_TOTAL.store(0, Ordering::SeqCst);
    WOULD_THROTTLE_TOTAL.store(0, Ordering::SeqCst);
}

#[cfg(test)]
pub(crate) struct BlockingAdmissionTestGuard {
    _lock: tokio::sync::MutexGuard<'static, ()>,
    saved_env: Option<String>,
}

#[cfg(test)]
impl Drop for BlockingAdmissionTestGuard {
    fn drop(&mut self) {
        if let Ok(mut owner) = TEST_LOCK_OWNER.lock() {
            *owner = None;
        }
        match &self.saved_env {
            Some(value) => std::env::set_var(BLOCKING_ADMISSION_ENV, value),
            None => std::env::remove_var(BLOCKING_ADMISSION_ENV),
        }
        configure_blocking_admission(DEFAULT_BLOCKING_ADMISSION_PERMITS);
        reset_metrics();
    }
}

#[cfg(test)]
pub(crate) async fn lock_blocking_admission_for_test() -> BlockingAdmissionTestGuard {
    let lock = BLOCKING_ADMISSION_TEST_LOCK.lock().await;
    let saved_env = std::env::var(BLOCKING_ADMISSION_ENV).ok();
    std::env::remove_var(BLOCKING_ADMISSION_ENV);
    reset_metrics();
    configure_blocking_admission(DEFAULT_BLOCKING_ADMISSION_PERMITS);
    *TEST_LOCK_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = tokio::task::try_id();
    BlockingAdmissionTestGuard {
        _lock: lock,
        saved_env,
    }
}

/// Same-task reentry is a no-op so `write_bytes_durably` can nest under a
/// test that already holds the lock. Other tasks wait.
#[cfg(test)]
pub(crate) async fn ensure_blocking_admission_test_lock() -> Option<BlockingAdmissionTestGuard> {
    let owner = *TEST_LOCK_OWNER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let (Some(owner), Some(id)) = (owner, tokio::task::try_id()) {
        if owner == id {
            return None;
        }
    }
    Some(lock_blocking_admission_for_test().await)
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicBool, AtomicUsize, Ordering},
            Arc,
        },
        time::Duration,
    };

    use super::*;

    #[test]
    fn pass_through_values_match_kill_switch() {
        assert!(is_pass_through_value("off"));
        assert!(is_pass_through_value("OFF"));
        assert!(is_pass_through_value(" pass_through "));
        assert!(is_pass_through_value("0"));
        assert!(is_pass_through_value("false"));
        assert!(is_pass_through_value("FALSE"));
        assert!(!is_pass_through_value("on"));
        assert!(!is_pass_through_value("16"));
        assert!(!is_pass_through_value(""));
    }

    #[tokio::test]
    async fn two_permits_cap_in_flight_at_two() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(2);

        let release = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let release = Arc::clone(&release);
            let entered = Arc::clone(&entered);
            handles.push(tokio::spawn(async move {
                spawn_blocking_admitted(move || {
                    entered.fetch_add(1, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    1usize
                })
                .await
            }));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let snap = blocking_admission_snapshot();
            if entered.load(Ordering::SeqCst) == 2 && snap.waiting >= 1 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "expected two admitted jobs plus a waiter, entered={}, snapshot={:?}",
                    entered.load(Ordering::SeqCst),
                    snap
                );
            }
            tokio::task::yield_now().await;
        }

        let snap = blocking_admission_snapshot();
        assert_eq!(snap.in_flight, 2);
        assert_eq!(snap.high_water, 2);
        assert!(
            snap.waiting >= 1,
            "third job should be waiting for a permit: {snap:?}"
        );
        assert_eq!(entered.load(Ordering::SeqCst), 2);

        release.store(true, Ordering::SeqCst);
        let mut results = Vec::new();
        for handle in handles {
            results.push(handle.await.expect("join").expect("admitted"));
        }
        assert_eq!(results, vec![1, 1, 1]);
        let after = blocking_admission_snapshot();
        assert_eq!(after.in_flight, 0);
        assert_eq!(after.high_water, 2);
        assert_eq!(after.admitted_total, 3);
        assert!(after.wait_high_water_ms > 0);
        assert!(after.would_throttle_total > 0);
    }

    #[tokio::test]
    async fn zero_permits_do_not_wait() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(0);
        assert_eq!(blocking_admission_permits(), 0);

        let release = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let release = Arc::clone(&release);
            let entered = Arc::clone(&entered);
            handles.push(tokio::spawn(async move {
                spawn_blocking_admitted(move || {
                    entered.fetch_add(1, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    1usize
                })
                .await
            }));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if entered.load(Ordering::SeqCst) == 3 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "unlimited admission should run all three, entered={}, snapshot={:?}",
                    entered.load(Ordering::SeqCst),
                    blocking_admission_snapshot()
                );
            }
            tokio::task::yield_now().await;
        }

        let snap = blocking_admission_snapshot();
        assert_eq!(snap.in_flight, 3);
        assert_eq!(snap.waiting, 0);
        assert_eq!(snap.would_throttle_total, 0);
        release.store(true, Ordering::SeqCst);
        for handle in handles {
            handle.await.expect("join").expect("admitted");
        }
        assert_eq!(blocking_admission_snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn env_off_skips_semaphore_and_still_completes() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(2);
        std::env::set_var(BLOCKING_ADMISSION_ENV, "off");
        assert!(blocking_admission_pass_through());
        assert_eq!(blocking_admission_permits(), 2);

        let release = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let release = Arc::clone(&release);
            let entered = Arc::clone(&entered);
            handles.push(tokio::spawn(async move {
                spawn_blocking_admitted(move || {
                    entered.fetch_add(1, Ordering::SeqCst);
                    while !release.load(Ordering::SeqCst) {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    1usize
                })
                .await
            }));
        }

        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            if entered.load(Ordering::SeqCst) == 3 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "env off should pass through all three, entered={}, snapshot={:?}",
                    entered.load(Ordering::SeqCst),
                    blocking_admission_snapshot()
                );
            }
            tokio::task::yield_now().await;
        }

        let snap = blocking_admission_snapshot();
        assert!(snap.pass_through);
        assert_eq!(snap.in_flight, 3);
        assert_eq!(snap.waiting, 0);
        assert_eq!(snap.would_throttle_total, 0);
        release.store(true, Ordering::SeqCst);
        for handle in handles {
            handle.await.expect("join").expect("admitted");
        }
    }

    #[tokio::test]
    async fn nested_from_blocking_is_rejected() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(2);
        let result = spawn_blocking_admitted(|| {
            // A nested runtime, not Handle::current().block_on, so this does
            // not deadlock the parent worker pool. The thread-local still
            // rejects the inner acquire.
            tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("nested test runtime")
                .block_on(spawn_blocking_admitted(|| ()))
        })
        .await
        .expect("outer admitted job");
        assert!(
            matches!(result, Err(BlockingAdmissionError::NestedFromBlocking)),
            "inner acquire from a blocking thread must fail closed, got {result:?}"
        );
    }

    #[tokio::test]
    async fn admitted_from_async_can_await_sequentially() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(1);
        let total = async {
            let a = spawn_blocking_admitted(|| 1usize).await?;
            let b = spawn_blocking_admitted(|| 2usize).await?;
            Ok::<_, BlockingAdmissionError>(a + b)
        }
        .await
        .expect("nested-from-async");
        assert_eq!(total, 3);
        assert_eq!(blocking_admission_snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn held_permit_blocks_other_admitted_work() {
        let _guard = lock_blocking_admission_for_test().await;
        configure_blocking_admission(1);
        let permit = acquire_blocking_admission().await.expect("permit");
        assert_eq!(blocking_admission_snapshot().in_flight, 1);

        let started = Arc::new(AtomicBool::new(false));
        let started_flag = Arc::clone(&started);
        let handle = tokio::spawn(async move {
            spawn_blocking_admitted(move || {
                started_flag.store(true, Ordering::SeqCst);
                1usize
            })
            .await
        });

        let deadline = tokio::time::Instant::now() + Duration::from_millis(200);
        loop {
            if blocking_admission_snapshot().waiting >= 1 {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!(
                    "second job should wait on the held permit: {:?}",
                    blocking_admission_snapshot()
                );
            }
            tokio::task::yield_now().await;
        }
        assert!(
            !started.load(Ordering::SeqCst),
            "held permit must not let the waiter run yet"
        );

        drop(permit);
        assert_eq!(handle.await.expect("join").expect("admitted"), 1);
        assert_eq!(blocking_admission_snapshot().in_flight, 0);
    }

    #[tokio::test]
    async fn durable_write_uses_admitted_parent_sync() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("durable.txt");
        let write_path = path.clone();
        tokio::spawn(async move {
            let _guard = lock_blocking_admission_for_test().await;
            configure_blocking_admission(2);
            crate::durable_io::write_bytes_durably(&write_path, b"r12-admitted")
                .await
                .expect("durable write");
            assert!(blocking_admission_snapshot().admitted_total >= 1);
        })
        .await
        .expect("admitted writer task");
        assert_eq!(std::fs::read(&path).expect("read"), b"r12-admitted");
    }
}

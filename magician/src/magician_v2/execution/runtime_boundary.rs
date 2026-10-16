//! Process-wide runtime boundary for stack-heavy execution jobs.
//!
//! Actix owns separate worker runtimes with comparatively small thread stacks.
//! Agentic futures must therefore be constructed and polled on the dedicated
//! execution runtime configured by `bin/magician.rs`, even when an HTTP/chat
//! caller initiates the work. Keeping this runtime separate from the general
//! application runtime isolates execution scheduling and prevents an HTTP
//! worker from becoming the owner of long-lived agent work. The executor's
//! public future is definition-level type-erased, so these workers deliberately
//! use Tokio's normal stack size rather than compensating for future growth
//! with a larger thread stack.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};

use tracing::{info, warn};

static EXECUTION_RUNTIME_HANDLE: OnceLock<tokio::runtime::Handle> = OnceLock::new();
static EXECUTION_JOB_REGISTRY: OnceLock<Arc<ExecutionJobRegistry>> = OnceLock::new();

#[derive(Default)]
struct ExecutionJobRegistryState {
    quiescing: bool,
    next_id: u64,
    jobs: HashMap<u64, Option<tokio::task::AbortHandle>>,
}

struct ExecutionJobRegistry {
    state: Mutex<ExecutionJobRegistryState>,
    changed: tokio::sync::Notify,
}

impl ExecutionJobRegistry {
    fn new() -> Self {
        Self {
            state: Mutex::new(ExecutionJobRegistryState::default()),
            changed: tokio::sync::Notify::new(),
        }
    }

    fn reserve(self: &Arc<Self>) -> Option<ExecutionJobRegistration> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if state.quiescing {
            return None;
        }
        let id = state.next_id;
        state.next_id = state.next_id.wrapping_add(1);
        state.jobs.insert(id, None);
        Some(ExecutionJobRegistration {
            registry: Arc::clone(self),
            id,
        })
    }

    /// Publish into an earlier reservation. Reservation and quiescing share
    /// one mutex, so shutdown either observes this handle or this publisher
    /// observes shutdown and aborts it itself.
    fn publish_abort_handle(&self, id: u64, abort: tokio::task::AbortHandle) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let quiescing = state.quiescing;
        if let Some(slot) = state.jobs.get_mut(&id) {
            *slot = Some(abort);
            if quiescing {
                self.changed.notify_one();
            }
            return quiescing;
        }
        false
    }

    fn begin_quiescing(&self) -> usize {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.quiescing = true;
        state.jobs.len()
    }

    fn prune_and_snapshot(&self) -> (usize, Vec<(u64, tokio::task::AbortHandle)>) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.jobs.retain(|_, abort| {
            !abort
                .as_ref()
                .is_some_and(tokio::task::AbortHandle::is_finished)
        });
        let handles = state
            .jobs
            .iter()
            .filter_map(|(id, abort)| abort.clone().map(|abort| (*id, abort)))
            .collect();
        (state.jobs.len(), handles)
    }

    async fn quiesce(&self, timeout: Duration) -> ExecutionJobShutdownReport {
        let observed_jobs = self.begin_quiescing();
        let deadline = tokio::time::Instant::now() + timeout;
        let mut signalled = HashSet::new();

        loop {
            // Register the notification before observing the registry so a
            // completion or reserved-handle publication cannot be lost between
            // the empty check and the await.
            let changed = self.changed.notified();
            let (remaining_jobs, handles) = self.prune_and_snapshot();
            for (id, abort) in handles {
                if signalled.insert(id) {
                    abort.abort();
                }
            }
            if remaining_jobs == 0 {
                return ExecutionJobShutdownReport {
                    observed_jobs,
                    abort_signalled: signalled.len(),
                    remaining_jobs: 0,
                    timed_out: false,
                };
            }
            if tokio::time::timeout_at(deadline, changed).await.is_err() {
                let (remaining_jobs, handles) = self.prune_and_snapshot();
                for (id, abort) in handles {
                    if signalled.insert(id) {
                        abort.abort();
                    }
                }
                return ExecutionJobShutdownReport {
                    observed_jobs,
                    abort_signalled: signalled.len(),
                    remaining_jobs,
                    timed_out: remaining_jobs > 0,
                };
            }
        }
    }
}

struct ExecutionJobRegistration {
    registry: Arc<ExecutionJobRegistry>,
    id: u64,
}

impl Drop for ExecutionJobRegistration {
    fn drop(&mut self) {
        let removed = self
            .registry
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .jobs
            .remove(&self.id)
            .is_some();
        if removed {
            self.registry.changed.notify_one();
        }
    }
}

fn execution_job_registry() -> Arc<ExecutionJobRegistry> {
    Arc::clone(EXECUTION_JOB_REGISTRY.get_or_init(|| Arc::new(ExecutionJobRegistry::new())))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutionJobShutdownReport {
    pub observed_jobs: usize,
    pub abort_signalled: usize,
    pub remaining_jobs: usize,
    pub timed_out: bool,
}

/// Agentic loops are mostly awaiting providers and tools, so a small dedicated
/// pool preserves concurrency without occupying general runtime workers.
pub const EXECUTION_WORKER_THREADS: usize = 4;

/// Build the process-owned runtime used exclusively for full agentic execution
/// and resume jobs. Uses [`EXECUTION_WORKER_THREADS`], the `current` profile
/// default. Boot uses [`build_execution_runtime_with_threads`].
pub fn build_execution_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    build_execution_runtime_with_threads(EXECUTION_WORKER_THREADS)
}

/// Build the execution runtime with an explicit worker-thread count from the
/// pre-bootstrap plan.
pub fn build_execution_runtime_with_threads(
    worker_threads: usize,
) -> std::io::Result<tokio::runtime::Runtime> {
    let worker_threads = worker_threads.max(1);
    info!(
        worker_threads,
        stack_policy = "tokio_default",
        thread_name = "magician-execution-worker",
        "built dedicated agentic execution runtime"
    );
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .thread_name("magician-execution-worker")
        .enable_all()
        .build()
}

/// Register the process's dedicated execution runtime during startup.
/// Repeated registration is harmless; the first handle remains authoritative.
pub fn set_execution_runtime_handle(handle: tokio::runtime::Handle) {
    if EXECUTION_RUNTIME_HANDLE.set(handle).is_ok() {
        info!(
            stack_policy = "tokio_default",
            thread_name = "magician-execution-worker",
            "registered dedicated agentic execution runtime handle"
        );
    }
}

/// Spawn a lazily constructed job on a specific runtime.
///
/// The closure, not an already-created future, crosses the runtime boundary.
/// This ensures the target runtime owns both construction and first polling of
/// a potentially large async state machine.
fn spawn_execution_job_on_handle<F, Fut, T>(
    handle: &tokio::runtime::Handle,
    job: F,
) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    spawn_execution_job_on_handle_with_registry(handle, execution_job_registry(), job)
}

fn spawn_execution_job_on_handle_with_registry<F, Fut, T>(
    handle: &tokio::runtime::Handle,
    registry: Arc<ExecutionJobRegistry>,
    job: F,
) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    let Some(registration) = registry.reserve() else {
        // Preserve the JoinHandle API without constructing or polling the
        // caller's execution future after shutdown admission has closed.
        let rejected = handle.spawn(std::future::pending::<T>());
        rejected.abort();
        return rejected;
    };
    let registration_id = registration.id;
    let task: std::pin::Pin<Box<dyn Future<Output = T> + Send>> = Box::pin(async move {
        // Captured before spawn: abort-before-first-poll still drops this guard
        // and retires the reservation from the shutdown registry.
        let _registration = registration;
        // Compile the last-mile credential redactors before constructing the
        // broad agentic future. Their lazy regex compiler is stack-intensive;
        // doing this at the scheduler root prevents first-use timing from
        // determining whether an otherwise ordinary execution stack survives.
        crate::magician_v2::secrets::injection::warm_provider_sanitizer();
        Box::pin(job()).await
    });
    let spawned = handle.spawn(task);
    if registry.publish_abort_handle(registration_id, spawned.abort_handle()) {
        spawned.abort();
    }
    spawned
}

/// Spawn full agentic execution work on the registered execution runtime.
///
/// The ambient fallback keeps isolated library tests and embedded consumers
/// functional, while production startup always registers the dedicated handle.
pub fn spawn_execution_job<F, Fut, T>(job: F) -> tokio::task::JoinHandle<T>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    if let Some(handle) = EXECUTION_RUNTIME_HANDLE.get() {
        return spawn_execution_job_on_handle(handle, job);
    }

    warn!("[EXECUTION-RUNTIME] Dedicated runtime is not registered; using the ambient runtime");
    let handle = tokio::runtime::Handle::current();
    spawn_execution_job_on_handle(&handle, job)
}

/// Close process-wide execution-job admission, abort every in-memory execution
/// future, and wait for their RAII registrations to drain. This never writes a
/// terminal runtime state: durable stateless checkpoints remain restart
/// authority for work interrupted by process shutdown.
pub async fn quiesce_execution_jobs(timeout: Duration) -> ExecutionJobShutdownReport {
    execution_job_registry().quiesce(timeout).await
}

/// Run one lazily constructed execution job and abort it if its waiter is
/// dropped. HTTP/chat callers use this awaited form so disconnecting the
/// request cannot detach a provider/tool call on the execution runtime.
pub async fn run_execution_job<F, Fut, T>(job: F) -> Result<T, tokio::task::JoinError>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    struct AbortOnDropJoinHandle<T> {
        handle: tokio::task::JoinHandle<T>,
        armed: bool,
    }

    impl<T> Drop for AbortOnDropJoinHandle<T> {
        fn drop(&mut self) {
            if self.armed {
                self.handle.abort();
            }
        }
    }

    let mut guarded = AbortOnDropJoinHandle {
        handle: spawn_execution_job(job),
        armed: true,
    };
    let result = (&mut guarded.handle).await;
    guarded.armed = false;
    result
}

#[cfg(any(test, feature = "test-fixtures"))]
mod tests {
    use super::*;

    #[test]
    fn execution_job_is_constructed_and_polled_on_selected_runtime() {
        let execution_runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .thread_name("execution-runtime-boundary-test")
            .enable_all()
            .build()
            .expect("execution runtime");
        let caller_runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("caller runtime");
        let execution_handle = execution_runtime.handle().clone();

        let (constructed_on, polled_on) = caller_runtime.block_on(async move {
            spawn_execution_job_on_handle(&execution_handle, || {
                let constructed_on = std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string();
                async move {
                    let polled_on = std::thread::current()
                        .name()
                        .unwrap_or("unnamed")
                        .to_string();
                    (constructed_on, polled_on)
                }
            })
            .await
            .expect("execution job")
        });

        assert_eq!(constructed_on, "execution-runtime-boundary-test");
        assert_eq!(polled_on, "execution-runtime-boundary-test");
    }

    #[test]
    fn dedicated_execution_runtime_uses_distinct_default_stack_workers() {
        assert!(EXECUTION_WORKER_THREADS >= 2);

        let source = include_str!("runtime_boundary.rs");
        let builder = source
            .split_once("pub fn build_execution_runtime")
            .expect("execution runtime builder")
            .1
            .split_once("pub fn set_execution_runtime_handle")
            .expect("execution runtime registration after builder")
            .0;
        assert!(!builder.contains("thread_stack_size"));
        assert!(!builder.contains("stack_size"));

        let execution_runtime = build_execution_runtime().expect("execution runtime");
        let thread_name = execution_runtime.block_on(async {
            tokio::spawn(async {
                std::thread::current()
                    .name()
                    .unwrap_or("unnamed")
                    .to_string()
            })
            .await
            .expect("named execution worker")
        });

        assert_eq!(thread_name, "magician-execution-worker");
    }

    #[tokio::test]
    async fn awaited_execution_job_aborts_when_its_waiter_is_dropped() {
        struct DropSignal(Option<tokio::sync::oneshot::Sender<()>>);

        impl Drop for DropSignal {
            fn drop(&mut self) {
                if let Some(sender) = self.0.take() {
                    let _ = sender.send(());
                }
            }
        }

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        let waiter = tokio::spawn(async move {
            let _ = run_execution_job(move || async move {
                let _drop_signal = DropSignal(Some(dropped_tx));
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            })
            .await;
        });

        started_rx.await.expect("execution job started");
        waiter.abort();
        let _ = waiter.await;
        tokio::time::timeout(std::time::Duration::from_secs(1), dropped_rx)
            .await
            .expect("detached execution job was aborted")
            .expect("drop signal");
    }

    #[tokio::test]
    async fn shutdown_registry_aborts_existing_jobs_and_rejects_late_spawns() {
        let registry = Arc::new(ExecutionJobRegistry::new());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let existing = spawn_execution_job_on_handle_with_registry(
            &tokio::runtime::Handle::current(),
            Arc::clone(&registry),
            move || async move {
                let _ = started_tx.send(());
                std::future::pending::<()>().await;
            },
        );
        started_rx.await.expect("tracked job started");

        let report = registry.quiesce(Duration::from_secs(1)).await;
        assert_eq!(report.observed_jobs, 1);
        assert_eq!(report.abort_signalled, 1);
        assert_eq!(report.remaining_jobs, 0);
        assert!(!report.timed_out);
        assert!(existing
            .await
            .expect_err("tracked job must be aborted")
            .is_cancelled());

        let late_ran = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let late_ran_in_job = Arc::clone(&late_ran);
        let late = spawn_execution_job_on_handle_with_registry(
            &tokio::runtime::Handle::current(),
            registry,
            move || async move {
                late_ran_in_job.store(true, std::sync::atomic::Ordering::SeqCst);
            },
        );
        assert!(late
            .await
            .expect_err("late job must be rejected")
            .is_cancelled());
        assert!(!late_ran.load(std::sync::atomic::Ordering::SeqCst));
    }
}

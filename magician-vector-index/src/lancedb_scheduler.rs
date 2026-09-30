//! Scheduler-root boundary for request-path LanceDB work.
//!
//! LanceDB constructs DataFusion query plans synchronously inside async polls.
//! Request paths must not poll those third-party frames underneath an already
//! deep Chat, agentic, or procedure-retrieval future.

use std::{future::Future, pin::Pin, time::Duration};

use anyhow::{Context, Result};
use tokio::time::timeout;

use crate::hol_stats;
use crate::lance_runtime::spawn_on_lance_runtime;

pub(crate) type LanceSearchFuture<T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'static>>;
pub(crate) type LanceSearchJob<T> = Box<dyn FnOnce() -> LanceSearchFuture<T> + Send + 'static>;

/// Lazily construct and poll one LanceDB request from a fresh Tokio task.
///
/// The lazy boxed factory resets both construction and polling at a scheduler
/// root and keeps the spawn payload pointer-sized. Dropping the waiter aborts
/// the child task, so an upstream request cancellation never detaches work.
/// When a dedicated Lance runtime is registered, that handle owns construction
/// and polling; otherwise this falls back to `tokio::spawn` on the caller
/// runtime (`MAGICIAN_LANCE_RUNTIME=ambient` forces that fallback).
pub(crate) async fn run_lancedb_search_at_scheduler_root<T: Send + 'static>(
    operation: &'static str,
    job: LanceSearchJob<T>,
) -> Result<T> {
    struct AbortOnDropSearch<T> {
        handle: tokio::task::JoinHandle<Result<T>>,
        armed: bool,
    }

    impl<T> Drop for AbortOnDropSearch<T> {
        fn drop(&mut self) {
            if self.armed {
                hol_stats::record_lance_cancel();
                self.handle.abort();
            }
        }
    }

    // The outer future contains only the boxed lazy factory. `job()` and the
    // concrete Lance/DataFusion future are first constructed while Tokio polls
    // this new task, rather than on the already-deep caller stack.
    let task: LanceSearchFuture<T> = Box::pin(async move { job().await });
    let mut guarded = AbortOnDropSearch {
        handle: spawn_on_lance_runtime(task),
        armed: true,
    };
    let joined = (&mut guarded.handle).await;
    guarded.armed = false;

    joined.with_context(|| format!("{operation} scheduler-root task failed to join"))?
}

/// Scheduler-root LanceDB work with a hard caller-visible deadline.
///
/// `timeout` cancels its inner future on expiry. That drops the guarded join
/// handle above and aborts the child without awaiting a synchronous third-party
/// poll a second time.
pub(crate) async fn run_lancedb_search_at_scheduler_root_with_timeout<T: Send + 'static>(
    timeout_duration: Duration,
    operation: &'static str,
    job: LanceSearchJob<T>,
) -> Result<T> {
    match timeout(
        timeout_duration,
        run_lancedb_search_at_scheduler_root(operation, job),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            hol_stats::record_lance_timeout();
            anyhow::bail!(
                "timed out after {}ms querying {operation}",
                timeout_duration.as_millis()
            )
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::oneshot;

    tokio::task_local! {
        static LANCE_CALLER_TASK_MARKER: ();
    }

    struct SearchDropSignal(Option<oneshot::Sender<()>>);

    impl Drop for SearchDropSignal {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn search_factory_is_constructed_and_polled_in_a_fresh_task() {
        let marker_was_absent = LANCE_CALLER_TASK_MARKER
            .scope((), async {
                run_lancedb_search_at_scheduler_root(
                    "test LanceDB search",
                    Box::new(|| {
                        assert!(
                            LANCE_CALLER_TASK_MARKER.try_with(|_| ()).is_err(),
                            "the lazy search factory must be constructed outside the caller task"
                        );
                        Box::pin(async { Ok(LANCE_CALLER_TASK_MARKER.try_with(|_| ()).is_err()) })
                    }),
                )
                .await
                .unwrap()
            })
            .await;

        assert!(
            marker_was_absent,
            "the Lance future must be polled from a fresh scheduler task"
        );
    }

    #[tokio::test]
    async fn dropping_search_waiter_aborts_the_scheduler_root_task() {
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let waiter = tokio::spawn(async move {
            run_lancedb_search_at_scheduler_root(
                "test LanceDB search",
                Box::new(move || {
                    Box::pin(async move {
                        let _drop_signal = SearchDropSignal(Some(dropped_tx));
                        let _ = started_tx.send(());
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                }),
            )
            .await
        });

        started_rx.await.expect("search should start");
        waiter.abort();
        let _ = waiter.await;
        timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("aborted search should be dropped promptly")
            .expect("search drop signal should remain connected");
    }

    #[test]
    fn dedicated_runtime_polls_search_off_the_caller_thread() {
        let runtime = crate::lance_runtime::build_lance_runtime().expect("lance runtime");
        let handle = runtime.handle().clone();
        let caller = std::thread::current()
            .name()
            .unwrap_or("unnamed")
            .to_string();
        let polled = runtime.block_on(async move {
            handle
                .spawn(async {
                    std::thread::current()
                        .name()
                        .unwrap_or("unnamed")
                        .to_string()
                })
                .await
                .expect("lance worker")
        });
        assert_ne!(polled, caller);
        assert_eq!(polled, crate::lance_runtime::LANCE_THREAD_NAME);
        drop(runtime);
    }

    #[tokio::test]
    async fn search_timeout_aborts_instead_of_detaching_work() {
        let (started_tx, started_rx) = oneshot::channel();
        let (dropped_tx, dropped_rx) = oneshot::channel();
        let search = tokio::spawn(async move {
            run_lancedb_search_at_scheduler_root_with_timeout(
                Duration::from_millis(25),
                "test LanceDB search",
                Box::new(move || {
                    Box::pin(async move {
                        let _drop_signal = SearchDropSignal(Some(dropped_tx));
                        let _ = started_tx.send(());
                        std::future::pending::<()>().await;
                        Ok(())
                    })
                }),
            )
            .await
        });

        started_rx.await.expect("search should start");
        let error = search
            .await
            .expect("timeout waiter should not panic")
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("timed out after 25ms querying test LanceDB search"));
        timeout(Duration::from_secs(1), dropped_rx)
            .await
            .expect("timed-out search should be dropped promptly")
            .expect("search drop signal should remain connected");
    }
}

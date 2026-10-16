//! Keep owned work polled while control operations wait on resources it holds.
use std::future::Future;

use futures_util::{stream::FuturesUnordered, StreamExt};

/// Admission and cancellation may need a lock held by an in-flight task.
/// Awaiting them alone would stop polling the very task that can release it.
/// Return every completed outcome to the owner and retain unfinished work.
pub(super) async fn await_with_progress<C, W>(
    control: C,
    running: &mut FuturesUnordered<W>,
) -> (C::Output, Vec<W::Output>)
where
    C: Future,
    W: Future,
{
    let mut control = Box::pin(control);
    let mut completed = Vec::new();
    loop {
        tokio::select! {
            result = &mut control => return (result, completed),
            Some(result) = running.next(), if !running.is_empty() => completed.push(result),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, time::Duration};
    use tokio::sync::{oneshot, Mutex};

    #[tokio::test]
    async fn a_control_wait_keeps_the_lock_owning_task_progressing() {
        let lock = Arc::new(Mutex::new(()));
        let held = lock.clone().lock_owned().await;
        let mut running = FuturesUnordered::new();
        running.push(async move {
            tokio::task::yield_now().await;
            drop(held);
            "participant settled"
        });
        let control = async {
            let _guard = lock.lock().await;
            "control admitted"
        };
        let (result, completed) = tokio::time::timeout(
            Duration::from_secs(1),
            await_with_progress(control, &mut running),
        )
        .await
        .expect("a control wait must poll the work that can release its lock");
        assert_eq!(result, "control admitted");
        assert_eq!(completed, ["participant settled"]);
        assert!(running.is_empty());
    }

    #[tokio::test]
    async fn completing_control_preserves_work_that_still_needs_draining() {
        let (send, receive) = oneshot::channel();
        let mut running = FuturesUnordered::new();
        running.push(async { receive.await.unwrap() });
        let (result, completed) = await_with_progress(std::future::ready(7), &mut running).await;
        assert_eq!(result, 7);
        assert!(completed.is_empty());
        assert_eq!(running.len(), 1);
        send.send(11).unwrap();
        assert_eq!(running.next().await, Some(11));
    }

    #[tokio::test]
    async fn control_also_completes_when_no_work_is_active() {
        let mut running = FuturesUnordered::<std::future::Ready<()>>::new();
        let (result, completed) = await_with_progress(std::future::ready(7), &mut running).await;
        assert_eq!(result, 7);
        assert!(completed.is_empty());
    }
}

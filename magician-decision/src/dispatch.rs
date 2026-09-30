//! Default structured-model dispatch. Waiting jobs share the LLM queue's
//! bounded owner-round-robin lane; model routing remains in DecisionRuntime.
//!
//! A granted job runs in its caller's task, preserving cancellation and attempt
//! accounting. The existing adapter owns the actual CPU/GPU/network worker.
use runtime_core::fair_queue::{FairJob, FairLane};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use std::time::Instant;
use tokio::sync::Notify;

use crate::DecisionError;

pub(crate) const DEFAULT_CAPACITY: usize = 64;
pub(crate) const DEFAULT_BYTES: usize = 4 * 1024 * 1024;

struct Ticket {
    granted: AtomicBool,
    ready: Notify,
}
struct Job {
    owner: String,
    ticket: Arc<Ticket>,
    bytes: usize,
    background: bool,
}
impl FairJob for Job {
    fn owner_key(&self) -> String {
        self.owner.clone()
    }
}
struct State {
    limit: usize,
    capacity: usize,
    byte_limit: usize,
    active: usize,
    background_active: usize,
    queued_bytes: usize,
    foreground_streak: usize,
    foreground: FairLane<Job>,
    background: FairLane<Job>,
}

/// Shared by every operation/locality and retained across compatible reloads.
pub(crate) struct ModelDispatch {
    state: Mutex<State>,
}
impl ModelDispatch {
    pub fn new(limit: usize) -> Self {
        Self {
            state: Mutex::new(State {
                limit,
                capacity: DEFAULT_CAPACITY,
                byte_limit: DEFAULT_BYTES,
                active: 0,
                background_active: 0,
                queued_bytes: 0,
                foreground_streak: 0,
                foreground: FairLane::new(1024),
                background: FairLane::new(1024),
            }),
        }
    }
    pub fn configure(&self, limit: usize, capacity: usize, byte_limit: usize) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.limit = limit;
        state.capacity = capacity;
        state.byte_limit = byte_limit;
        state.dispatch_ready();
    }
    pub async fn acquire(
        self: &Arc<Self>,
        owner: &str,
        bytes: usize,
        background: bool,
    ) -> Result<DispatchPermit, DecisionError> {
        let started = Instant::now();
        let ticket = Arc::new(Ticket {
            granted: AtomicBool::new(false),
            ready: Notify::new(),
        });
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if state.foreground.len() + state.background.len() >= state.capacity
                || bytes > state.byte_limit.saturating_sub(state.queued_bytes)
            {
                return Err(DecisionError::DispatchFull);
            }
            let job = Job {
                owner: owner.into(),
                ticket: ticket.clone(),
                bytes,
                background,
            };
            let lane = if background {
                &state.background
            } else {
                &state.foreground
            };
            lane.try_push(job)
                .map_err(|_| DecisionError::DispatchFull)?;
            state.queued_bytes += bytes;
            state.dispatch_ready();
        }
        // This guard exists before the first await: cancellation cannot leak a
        // queue entry, a byte reservation or a slot granted just before cancellation.
        let mut permit = DispatchPermit {
            lease: Arc::new(Reservation {
                dispatch: self.clone(),
                ticket,
                bytes,
                background,
            }),
            queue_wait_ms: 0,
        };
        loop {
            let ready = permit.lease.ticket.ready.notified();
            tokio::pin!(ready);
            ready.as_mut().enable();
            if permit.lease.ticket.granted.load(Ordering::Acquire) {
                break;
            }
            ready.await;
        }
        permit.queue_wait_ms = started.elapsed().as_millis() as u64;
        Ok(permit)
    }
}
impl State {
    fn dispatch_ready(&mut self) {
        while self.active < self.limit {
            // Reserve one foreground slot where possible. At concurrency one,
            // running GPU work is never preempted; priority affects next pickup.
            let background_ready = !self.background.is_empty()
                && self.background_active < self.limit.saturating_sub(1).max(1);
            let job = if background_ready
                && (self.foreground.is_empty() || self.foreground_streak >= 3)
            {
                self.foreground_streak = 0;
                self.background.pop_next()
            } else if !self.foreground.is_empty() {
                self.foreground_streak = self.foreground_streak.saturating_add(1);
                self.foreground.pop_next()
            } else {
                None
            };
            let Some(job) = job else {
                break;
            };
            self.queued_bytes -= job.bytes;
            self.active += 1;
            self.background_active += usize::from(job.background);
            job.ticket.granted.store(true, Ordering::Release);
            job.ticket.ready.notify_one();
        }
    }
}

pub(crate) struct DispatchPermit {
    lease: Arc<Reservation>,
    pub queue_wait_ms: u64,
}
impl DispatchPermit {
    pub fn lease(&self) -> Arc<Reservation> {
        self.lease.clone()
    }
}
/// Blocking/GPU workers retain this lease until physical work has settled,
/// even when their caller's future has already timed out.
pub(crate) struct Reservation {
    dispatch: Arc<ModelDispatch>,
    ticket: Arc<Ticket>,
    bytes: usize,
    background: bool,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self
            .dispatch
            .state
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if self.ticket.granted.load(Ordering::Acquire) {
            state.active -= 1;
            state.background_active -= usize::from(self.background);
        } else {
            let lane = if self.background {
                &state.background
            } else {
                &state.foreground
            };
            lane.remove_where(|job| Arc::ptr_eq(&job.ticket, &self.ticket));
            state.queued_bytes -= self.bytes;
        }
        state.dispatch_ready();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        future::Future,
        pin::Pin,
        task::{Context, Poll, Waker},
    };

    fn waiting<F: Future>(future: Pin<&mut F>) {
        assert!(matches!(
            future.poll(&mut Context::from_waker(Waker::noop())),
            Poll::Pending
        ));
    }

    #[tokio::test]
    async fn physical_worker_retains_capacity_after_caller_cancellation() {
        let queue = Arc::new(ModelDispatch::new(1));
        let caller = queue.acquire("running", 10, true).await.unwrap();
        let worker = caller.lease();
        drop(caller);
        let mut next = Box::pin(queue.acquire("next", 10, false));
        waiting(next.as_mut());
        assert_eq!(queue.state.lock().unwrap().active, 1);
        drop(worker);
        drop(next.await.unwrap());
        let state = queue.state.lock().unwrap();
        assert_eq!(state.active, 0);
        assert_eq!(state.background_active, 0);
        assert_eq!(state.queued_bytes, 0);
    }

    #[tokio::test]
    async fn owners_are_round_robin_and_cancelled_grants_release_capacity() {
        let queue = Arc::new(ModelDispatch::new(1));
        let held = queue.acquire("running", 10, false).await.unwrap();
        let mut a1 = Box::pin(queue.acquire("A", 10, false));
        let mut a2 = Box::pin(queue.acquire("A", 10, false));
        let mut b = Box::pin(queue.acquire("B", 10, false));
        waiting(a1.as_mut());
        waiting(a2.as_mut());
        waiting(b.as_mut());
        drop(held);
        let first = a1.await.unwrap();
        waiting(a2.as_mut());
        waiting(b.as_mut());
        drop(first);
        waiting(a2.as_mut());
        // B has been granted but its future has not resumed. Cancelling here
        // must free the active slot as well as dropping a queued job would.
        drop(b);
        drop(a2.await.unwrap());
        assert_eq!(queue.state.lock().unwrap().active, 0);
    }

    #[tokio::test]
    async fn waiting_cancellation_reclaims_count_and_bytes_without_a_slot_leak() {
        let queue = Arc::new(ModelDispatch::new(1));
        queue.configure(1, 1, 10);
        let held = queue.acquire("running", 10, false).await.unwrap();
        let mut waiter = Box::pin(queue.acquire("waiting", 10, false));
        waiting(waiter.as_mut());
        assert!(matches!(
            queue.acquire("overflow", 1, false).await,
            Err(DecisionError::DispatchFull)
        ));
        drop(waiter);
        let mut replacement = Box::pin(queue.acquire("replacement", 10, false));
        waiting(replacement.as_mut());
        drop(held);
        drop(replacement.await.unwrap());
        let state = queue.state.lock().unwrap();
        assert_eq!(
            (state.active, state.queued_bytes, state.foreground.len()),
            (0, 0, 0)
        );
    }

    #[tokio::test]
    async fn byte_bound_applies_even_when_job_count_is_below_capacity() {
        let queue = Arc::new(ModelDispatch::new(1));
        queue.configure(1, 10, 10);
        let held = queue.acquire("running", 10, false).await.unwrap();
        let mut a = Box::pin(queue.acquire("A", 6, false));
        waiting(a.as_mut());
        assert!(matches!(
            queue.acquire("B", 5, false).await,
            Err(DecisionError::DispatchFull)
        ));
        drop(a);
        drop(held);
    }

    #[tokio::test]
    async fn background_gets_a_turn_under_continuous_foreground_load() {
        let queue = Arc::new(ModelDispatch::new(1));
        let held = queue.acquire("running", 1, false).await.unwrap();
        let mut background = Box::pin(queue.acquire("background", 1, true));
        waiting(background.as_mut());
        let mut normal: Vec<_> = (0..5)
            .map(|_| Box::pin(queue.acquire("chat", 1, false)))
            .collect();
        for future in &mut normal {
            waiting(future.as_mut());
        }
        drop(held);
        drop(normal.remove(0).await.unwrap());
        drop(normal.remove(0).await.unwrap());
        for future in &mut normal {
            waiting(future.as_mut());
        }
        drop(background.await.unwrap());
        for future in normal {
            drop(future.await.unwrap());
        }
    }

    #[tokio::test]
    async fn resize_preserves_active_work_and_background_reserves_foreground_slot() {
        let queue = Arc::new(ModelDispatch::new(2));
        let held = queue.acquire("background", 1, true).await.unwrap();
        let mut waiting_bg = Box::pin(queue.acquire("background", 1, true));
        waiting(waiting_bg.as_mut());
        let foreground = queue.acquire("chat", 1, false).await.unwrap();
        queue.configure(1, 64, DEFAULT_BYTES);
        drop(held);
        waiting(waiting_bg.as_mut());
        drop(foreground);
        drop(waiting_bg.await.unwrap());
    }
}

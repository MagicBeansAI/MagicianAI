//! Bounded local-prep admission that does not occupy a dispatch worker.
//!
//! When local-prep would call the serial Ollama generation daemon, the worker
//! parks the job here (same shape as provider-capacity waiters) and returns to
//! the lane. One coordinator task holds at most one Ollama generate at a time.
//! After prep, the job is re-enqueued with `local_prep_done` so the worker
//! proceeds to provider admission.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use parking_lot::Mutex;
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use super::job::LlmJob;
use super::types::Priority;

pub(crate) struct ParkedLocalPrepJob {
    pub job: LlmJob,
    pub task_cancel_token: CancellationToken,
    pub wait_started: Instant,
}

pub struct LocalPrepCoordinator {
    lanes: Mutex<LocalPrepLanes>,
    pub(crate) notify: Notify,
    started: AtomicBool,
    concurrency: Arc<Semaphore>,
    /// Jobs popped into the cap-1 generate. They are not in `lanes` but still
    /// occupy the coordinator; snapshots must count them.
    in_progress: AtomicUsize,
}

#[derive(Default)]
struct LocalPrepLanes {
    high: VecDeque<ParkedLocalPrepJob>,
    normal: VecDeque<ParkedLocalPrepJob>,
    background: VecDeque<ParkedLocalPrepJob>,
}

impl LocalPrepCoordinator {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            lanes: Mutex::new(LocalPrepLanes::default()),
            notify: Notify::new(),
            started: AtomicBool::new(false),
            concurrency: Arc::new(Semaphore::new(1)),
            in_progress: AtomicUsize::new(0),
        })
    }

    pub(crate) fn concurrency(&self) -> Arc<Semaphore> {
        Arc::clone(&self.concurrency)
    }

    pub(crate) fn push(&self, parked: ParkedLocalPrepJob) {
        let mut lanes = self.lanes.lock();
        match parked.job.priority {
            Priority::High => lanes.high.push_back(parked),
            Priority::Normal => lanes.normal.push_back(parked),
            Priority::Background => lanes.background.push_back(parked),
        }
        self.notify.notify_one();
    }

    pub(crate) fn pop_next(&self) -> Option<ParkedLocalPrepJob> {
        let mut lanes = self.lanes.lock();
        lanes
            .high
            .pop_front()
            .or_else(|| lanes.normal.pop_front())
            .or_else(|| lanes.background.pop_front())
    }

    pub(crate) fn drain(&self) -> Vec<ParkedLocalPrepJob> {
        let mut lanes = self.lanes.lock();
        // Field access through the MutexGuard's Deref borrows the whole
        // guard, so chained `lanes.x.drain(..)` trips E0499. Destructuring
        // once splits the lanes into disjoint borrows.
        let LocalPrepLanes {
            high,
            normal,
            background,
        } = &mut *lanes;
        high.drain(..)
            .chain(normal.drain(..))
            .chain(background.drain(..))
            .collect()
    }

    pub fn waiting_count(&self) -> usize {
        self.lane_waiting_count()
            .saturating_add(self.in_progress.load(Ordering::Acquire))
    }

    /// Jobs still in priority lanes. The admission loop must use this for
    /// empty-wait, not [`waiting_count`]: in-progress generate is not poppable.
    pub(crate) fn lane_waiting_count(&self) -> usize {
        let lanes = self.lanes.lock();
        lanes
            .high
            .len()
            .saturating_add(lanes.normal.len())
            .saturating_add(lanes.background.len())
    }

    pub(crate) fn begin_prep(&self) {
        self.in_progress.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn end_prep(&self) {
        let _ = self
            .in_progress
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |n| {
                Some(n.saturating_sub(1))
            });
    }

    pub fn notify_all(&self) {
        self.notify.notify_waiters();
    }

    pub(crate) fn claim_start(&self) -> bool {
        self.started
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    /// Remove waiters selected by `classify`, preserving FIFO within each lane.
    pub(crate) fn take_where<T>(
        &self,
        mut classify: impl FnMut(&ParkedLocalPrepJob) -> Option<T>,
    ) -> Vec<(ParkedLocalPrepJob, T)> {
        fn split_lane<T>(
            lane: &mut VecDeque<ParkedLocalPrepJob>,
            classify: &mut impl FnMut(&ParkedLocalPrepJob) -> Option<T>,
            taken: &mut Vec<(ParkedLocalPrepJob, T)>,
        ) {
            let mut kept = VecDeque::with_capacity(lane.len());
            while let Some(parked) = lane.pop_front() {
                if let Some(extra) = classify(&parked) {
                    taken.push((parked, extra));
                } else {
                    kept.push_back(parked);
                }
            }
            *lane = kept;
        }

        let mut lanes = self.lanes.lock();
        let mut taken = Vec::new();
        split_lane(&mut lanes.high, &mut classify, &mut taken);
        split_lane(&mut lanes.normal, &mut classify, &mut taken);
        split_lane(&mut lanes.background, &mut classify, &mut taken);
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dispatch::types::JobOrigin;
    use crate::types::LLMRequest;

    #[test]
    fn pops_high_before_normal_and_background() {
        let coord = LocalPrepCoordinator::new();
        let mk = |priority: Priority, op: &str| {
            let (mut job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op(op));
            job.priority = priority;
            ParkedLocalPrepJob {
                job,
                task_cancel_token: CancellationToken::new(),
                wait_started: Instant::now(),
            }
        };
        coord.push(mk(Priority::Background, "bg"));
        coord.push(mk(Priority::Normal, "n"));
        coord.push(mk(Priority::High, "h"));
        assert_eq!(coord.waiting_count(), 3);
        assert_eq!(coord.pop_next().unwrap().job.origin.operation, "h");
        assert_eq!(coord.pop_next().unwrap().job.origin.operation, "n");
        assert_eq!(coord.pop_next().unwrap().job.origin.operation, "bg");
        assert!(coord.pop_next().is_none());
    }

    #[test]
    fn drain_empties_all_lanes_and_waiting_count_tracks_push() {
        let coord = LocalPrepCoordinator::new();
        let mk = |priority: Priority, op: &str| {
            let (mut job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op(op));
            job.priority = priority;
            ParkedLocalPrepJob {
                job,
                task_cancel_token: CancellationToken::new(),
                wait_started: Instant::now(),
            }
        };
        coord.push(mk(Priority::High, "h"));
        assert_eq!(coord.waiting_count(), 1);
        coord.push(mk(Priority::Normal, "n"));
        coord.push(mk(Priority::Background, "bg"));
        assert_eq!(coord.waiting_count(), 3);

        let drained = coord.drain();
        assert_eq!(drained.len(), 3);
        assert_eq!(
            drained
                .iter()
                .map(|parked| parked.job.origin.operation.as_str())
                .collect::<Vec<_>>(),
            vec!["h", "n", "bg"]
        );
        assert_eq!(coord.waiting_count(), 0);
        assert!(coord.pop_next().is_none());
        assert!(coord.drain().is_empty());
    }

    #[test]
    fn waiting_count_includes_in_progress_generate() {
        let coord = LocalPrepCoordinator::new();
        let (mut job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op("p"));
        job.priority = Priority::Normal;
        coord.push(ParkedLocalPrepJob {
            job,
            task_cancel_token: CancellationToken::new(),
            wait_started: Instant::now(),
        });
        assert_eq!(coord.waiting_count(), 1);
        let parked = coord.pop_next().unwrap();
        coord.begin_prep();
        assert_eq!(coord.lane_waiting_count(), 0);
        assert_eq!(coord.waiting_count(), 1, "popped generate still counts");
        coord.end_prep();
        assert_eq!(coord.waiting_count(), 0);
        drop(parked);
    }
}

//! Bounded owner-fair job lane shared by LLM and structured model dispatch.
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use tokio::sync::Notify;

/// Scheduling identity; contains no request payload or routing policy.
pub trait FairJob {
    fn owner_key(&self) -> String;
}

/// Failure from a non-blocking [`FairLane::try_push`].
pub enum TryPushError<T> {
    Full(T),
    Closed(T),
}

impl<T> TryPushError<T> {
    pub fn into_inner(self) -> T {
        match self {
            Self::Full(job) | Self::Closed(job) => job,
        }
    }
}

struct State<T> {
    len: usize,
    closed: bool,
    /// Owners that currently have at least one job, in round-robin order.
    order: VecDeque<String>,
    queues: HashMap<String, VecDeque<T>>,
}

struct Inner<T> {
    capacity: usize,
    state: Mutex<State<T>>,
    not_empty: Notify,
    not_full: Notify,
}

/// Cloneable MPMC lane. Capacity is the same bound as today's channel cap.
pub struct FairLane<T> {
    inner: Arc<Inner<T>>,
}

impl<T: FairJob> FairLane<T> {
    pub fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        Self {
            inner: Arc::new(Inner {
                capacity,
                state: Mutex::new(State {
                    len: 0,
                    closed: false,
                    order: VecDeque::new(),
                    queues: HashMap::new(),
                }),
                not_empty: Notify::new(),
                not_full: Notify::new(),
            }),
        }
    }

    pub fn capacity(&self) -> usize {
        self.inner.capacity
    }

    #[allow(dead_code)]
    pub fn len(&self) -> usize {
        self.inner.state.lock().len
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.inner.state.lock().len == 0
    }

    fn is_closed(&self) -> bool {
        self.inner.state.lock().closed
    }

    /// Non-blocking push. `Full` returns the job; `Closed` after [`Self::close`].
    pub fn try_push(&self, job: T) -> Result<(), TryPushError<T>> {
        let mut state = self.inner.state.lock();
        if state.closed {
            return Err(TryPushError::Closed(job));
        }
        if state.len >= self.inner.capacity {
            return Err(TryPushError::Full(job));
        }
        push_locked(&mut state, job);
        drop(state);
        self.inner.not_empty.notify_one();
        Ok(())
    }

    /// Wait for capacity when the lane is full. `Err` is a closed lane.
    pub async fn push(&self, mut job: T) -> Result<(), T> {
        loop {
            match self.try_push(job) {
                Ok(()) => return Ok(()),
                Err(TryPushError::Closed(returned)) => return Err(returned),
                Err(TryPushError::Full(returned)) => {
                    job = returned;
                    let notified = self.inner.not_full.notified();
                    tokio::pin!(notified);
                    let _ = notified.as_mut().enable();
                    match self.try_push(job) {
                        Ok(()) => return Ok(()),
                        Err(TryPushError::Closed(returned)) => return Err(returned),
                        Err(TryPushError::Full(returned)) => {
                            job = returned;
                            notified.await;
                        },
                    }
                },
            }
        }
    }

    /// Pop the next owner-round-robin job, or `None` if empty.
    pub fn pop_next(&self) -> Option<T> {
        let mut state = self.inner.state.lock();
        let job = pop_locked(&mut state)?;
        drop(state);
        self.inner.not_full.notify_one();
        Some(job)
    }

    /// Wait until a job is available. `None` after [`Self::close`] once empty.
    /// Workers wait with [`Self::wait_not_empty`] instead so `select!` cannot
    /// drop a popped job.
    #[allow(dead_code)]
    pub async fn recv(&self) -> Option<T> {
        loop {
            if let Some(job) = self.pop_next() {
                return Some(job);
            }
            if self.is_closed() {
                return None;
            }
            let notified = self.inner.not_empty.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            if let Some(job) = self.pop_next() {
                return Some(job);
            }
            if self.is_closed() {
                return None;
            }
            notified.await;
        }
    }

    /// Wait until this lane has at least one job. Does not pop.
    ///
    /// Workers `select!` across lanes with this, then pick through
    /// `try_pick_ready_job`, so a Ready `recv()` on an unchosen branch cannot
    /// drop a job. Closed empty lanes stay pending; the worker exits on the
    /// shutdown token instead.
    pub async fn wait_not_empty(&self) {
        loop {
            if self.inner.state.lock().len > 0 {
                return;
            }
            let notified = self.inner.not_empty.notified();
            tokio::pin!(notified);
            let _ = notified.as_mut().enable();
            if self.inner.state.lock().len > 0 {
                return;
            }
            notified.await;
        }
    }

    pub fn drain(&self) -> Vec<T> {
        let mut state = self.inner.state.lock();
        let jobs = drain_locked(&mut state);
        drop(state);
        if !jobs.is_empty() {
            self.inner.not_full.notify_waiters();
        }
        jobs
    }

    /// Remove cancelled waiting work without changing remaining pickup order.
    pub fn remove_where(&self, mut remove: impl FnMut(&T) -> bool) {
        let mut state = self.inner.state.lock();
        let mut removed = 0;
        for queue in state.queues.values_mut() {
            let before = queue.len();
            queue.retain(|job| !remove(job));
            removed += before - queue.len();
        }
        state.queues.retain(|_, queue| !queue.is_empty());
        let live: std::collections::HashSet<_> = state.queues.keys().cloned().collect();
        state.order.retain(|key| live.contains(key));
        state.len -= removed;
        drop(state);
        if removed > 0 {
            self.inner.not_full.notify_waiters();
        }
    }

    /// Stop new pushes. Waiters on [`Self::recv`] / [`Self::push`] wake.
    pub fn close(&self) {
        let mut state = self.inner.state.lock();
        if state.closed {
            return;
        }
        state.closed = true;
        drop(state);
        self.inner.not_empty.notify_waiters();
        self.inner.not_full.notify_waiters();
    }
}

fn push_locked<T: FairJob>(state: &mut State<T>, job: T) {
    let key = job.owner_key();
    match state.queues.entry(key) {
        std::collections::hash_map::Entry::Occupied(mut occupied) => {
            occupied.get_mut().push_back(job);
        },
        std::collections::hash_map::Entry::Vacant(vacant) => {
            let key = vacant.key().clone();
            let mut queue = VecDeque::new();
            queue.push_back(job);
            vacant.insert(queue);
            state.order.push_back(key);
        },
    }
    state.len = state.len.saturating_add(1);
}

fn pop_locked<T>(state: &mut State<T>) -> Option<T> {
    loop {
        let key = state.order.pop_front()?;
        let Some(queue) = state.queues.get_mut(&key) else {
            continue;
        };
        let Some(job) = queue.pop_front() else {
            state.queues.remove(&key);
            continue;
        };
        if queue.is_empty() {
            state.queues.remove(&key);
        } else {
            state.order.push_back(key);
        }
        state.len = state.len.saturating_sub(1);
        return Some(job);
    }
}

fn drain_locked<T>(state: &mut State<T>) -> Vec<T> {
    let mut out = Vec::with_capacity(state.len);
    while let Some(key) = state.order.pop_front() {
        if let Some(mut queue) = state.queues.remove(&key) {
            out.extend(queue.drain(..));
        }
    }
    for (_, mut queue) in state.queues.drain() {
        out.extend(queue.drain(..));
    }
    state.len = 0;
    out
}

impl<T> Clone for FairLane<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

//! Worker loop: gates → call → retry decision → resolution.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use parking_lot::{Mutex, RwLock};
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info};

use super::router_handle::DispatchRouter;
use crate::capability::LLMProviderKind;
use crate::error::{LLMError, LLMResult};
use crate::types::{LLMRequest, LLMResponse};

use super::cancellation::TaskStateView;
use super::capacity::DispatchEngine;
use super::classifier::classify;
use super::cloud_admission::{CloudAdmission, CloudPermit};
use super::config::DispatchConfig;
use super::events::{EventBus, LlmQueueEvent};
use super::fair_lane::{FairLane, TryPushError};
use super::inflight_index::InflightIndex;
use super::job::{
    AttemptError, DispatchedResponse, ErrorClass, JobMeta, LlmJob, QueueByteCounters,
};
use super::ledger::{LlmCallLedgerEvent, TaskLedgerSink};
use super::local_prep::{
    local_prep_needs_ollama, maybe_local_prep_direct_cancellable, LocalPrepCancellation,
};
use super::local_prep_coordinator::{LocalPrepCoordinator, ParkedLocalPrepJob};
use super::metrics::DispatchMetrics;
use super::provider_state::{retry_after_from, ProviderStateMap};
use super::quota::{estimate_request_tokens, ProviderQuotaMap};
use super::registry::JobRegistry;
use super::retry::{decide, RetryDecision};
use super::types::{JobId, JobState, LocalPrepStat, Priority, TokenSummary, TombstoneReason};

/// Context shared with every worker.
pub struct WorkerContext {
    pub router: Arc<dyn DispatchRouter>,
    pub registry: Arc<JobRegistry>,
    pub provider_state: Arc<ProviderStateMap>,
    pub inflight_index: Arc<InflightIndex>,
    pub task_state: Arc<dyn TaskStateView>,
    pub ledger_sink: Arc<dyn TaskLedgerSink>,
    pub events: EventBus,
    pub metrics: DispatchMetrics,
    pub config: Arc<RwLock<DispatchConfig>>,
    /// Stops work that has not begun a provider attempt (provider waiters and
    /// delayed requeues) without prematurely cancelling calls that are already
    /// in flight during a graceful drain.
    pub pending_shutdown: CancellationToken,
    pub shutdown: CancellationToken,
    pub deferred_tasks: TaskTracker,
    pub provider_waiters: Arc<ProviderAdmissionQueues>,
    pub byte_waiters: Arc<QueueByteAdmissionWaiters>,
    pub local_prep: Arc<LocalPrepCoordinator>,
    pub cloud_admission: Arc<CloudAdmission>,
    pub provider_quota: Arc<ProviderQuotaMap>,
    /// Boot-time engine. Scheduler workers still do pickup/gates/prep/admit;
    /// only the HTTP tail differs.
    pub engine: DispatchEngine,
    /// Isolated HTTP tasks. Force-shutdown aborts these if drain times out.
    pub isolated_executors: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    pub(super) queued_bytes: Arc<QueueByteCounters>,
    /// Shared owner-round-robin lanes. Cloneable; `try_push` for requeue
    /// paths that must not block, `push().await` for local-prep wait.
    pub(crate) lane_high: FairLane,
    pub(crate) lane_normal: FairLane,
    pub(crate) lane_background: FairLane,
}

/// One fair coordinator for already-admitted jobs waiting to reacquire a
/// retained-byte reservation after a provider attempt. Keeping these jobs out
/// of worker slots prevents a full lane from deadlocking behind workers parked
/// on byte capacity, while retry eligibility no longer depends on a transient
/// race with a newer submission.
pub struct QueueByteAdmissionWaiters {
    lanes: Mutex<ByteWaitLanes>,
    notify: Notify,
    started: AtomicBool,
}

impl QueueByteAdmissionWaiters {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            lanes: Mutex::new(ByteWaitLanes::default()),
            notify: Notify::new(),
            started: AtomicBool::new(false),
        })
    }

    fn push(&self, parked: ParkedByteJob) {
        self.lanes.lock().push_back(parked);
    }

    fn drain(&self) -> Vec<ParkedByteJob> {
        self.lanes.lock().drain()
    }

    fn is_empty(&self) -> bool {
        self.lanes.lock().is_empty()
    }

    pub fn waiting_count(&self) -> usize {
        self.lanes.lock().len()
    }

    pub fn notify_all(&self) {
        self.notify.notify_one();
    }
}

#[derive(Default)]
struct ByteWaitLanes {
    high: VecDeque<ParkedByteJob>,
    normal: VecDeque<ParkedByteJob>,
    background: VecDeque<ParkedByteJob>,
    fairness: LaneFairness,
}

impl ByteWaitLanes {
    fn push_back(&mut self, parked: ParkedByteJob) {
        match parked.job.priority {
            Priority::High => self.high.push_back(parked),
            Priority::Normal => self.normal.push_back(parked),
            Priority::Background => self.background.push_back(parked),
        }
    }

    fn push_front(&mut self, parked: ParkedByteJob) {
        match parked.job.priority {
            Priority::High => self.high.push_front(parked),
            Priority::Normal => self.normal.push_front(parked),
            Priority::Background => self.background.push_front(parked),
        }
    }

    fn candidate_priorities(&self) -> [Priority; 3] {
        if self.fairness.consecutive_foreground >= MAX_CONSECUTIVE_FOREGROUND_PICKUPS
            && !self.background.is_empty()
        {
            [Priority::Background, Priority::High, Priority::Normal]
        } else if self.fairness.consecutive_high >= MAX_CONSECUTIVE_HIGH_PICKUPS
            && !self.normal.is_empty()
        {
            [Priority::Normal, Priority::High, Priority::Background]
        } else {
            [Priority::High, Priority::Normal, Priority::Background]
        }
    }

    fn pop_front(&mut self, priority: Priority) -> Option<ParkedByteJob> {
        match priority {
            Priority::High => self.high.pop_front(),
            Priority::Normal => self.normal.pop_front(),
            Priority::Background => self.background.pop_front(),
        }
    }

    fn drain(&mut self) -> Vec<ParkedByteJob> {
        self.high
            .drain(..)
            .chain(self.normal.drain(..))
            .chain(self.background.drain(..))
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.high.is_empty() && self.normal.is_empty() && self.background.is_empty()
    }

    fn len(&self) -> usize {
        self.high
            .len()
            .saturating_add(self.normal.len())
            .saturating_add(self.background.len())
    }
}

struct ParkedByteJob {
    job: LlmJob,
    task_cancel_token: CancellationToken,
    ready_at: Instant,
    shutdown_reason: &'static str,
}

struct PermanentlyOversizedByteJob {
    parked: ParkedByteJob,
    bytes: u64,
    capacity_bytes: u64,
}

fn try_admit_byte_waiter(
    queue: &QueueByteAdmissionWaiters,
    queued_bytes: &Arc<QueueByteCounters>,
    config: &DispatchConfig,
) -> (Option<ParkedByteJob>, Vec<PermanentlyOversizedByteJob>) {
    let mut admitted = None;
    let mut permanently_oversized = Vec::new();
    let mut lanes = queue.lanes.lock();
    for priority in lanes.candidate_priorities() {
        let Some(mut parked) = lanes.pop_front(priority) else {
            continue;
        };
        let request_bytes = parked.job.request.estimated_retained_bytes() as u64;
        let lane_capacity = config.byte_capacity_for(priority);
        let effective_capacity = config
            .max_request_bytes
            .min(lane_capacity)
            .min(config.queue_bytes_global);
        if request_bytes > effective_capacity {
            permanently_oversized.push(PermanentlyOversizedByteJob {
                parked,
                bytes: request_bytes,
                capacity_bytes: effective_capacity,
            });
            continue;
        }
        match queued_bytes.try_reserve(
            priority,
            request_bytes,
            lane_capacity,
            config.queue_bytes_global,
        ) {
            Ok(permit) => {
                lanes.fairness.record(priority);
                parked.job.queued_byte_permit = Some(permit);
                admitted = Some(parked);
                break;
            },
            Err(_) => lanes.push_front(parked),
        }
    }
    (admitted, permanently_oversized)
}

impl WorkerContext {
    fn lane_sender(&self, p: Priority) -> &FairLane {
        match p {
            Priority::High => &self.lane_high,
            Priority::Normal => &self.lane_normal,
            Priority::Background => &self.lane_background,
        }
    }
}

/// Provider + optional cloud permits held around one physical HTTP attempt.
/// Dropped together after the attempt (or on cancel/shutdown) so both slots
/// free before retry accounting and ledger work.
struct AttemptPermits {
    _provider: OwnedSemaphorePermit,
    _cloud: Option<CloudPermit>,
}

/// Bounded, provider-specific admission schedulers. There is at most one
/// scheduler task per materialised provider, rather than one detached task per
/// saturated job. Jobs retain their queue admission reservation while parked.
pub struct ProviderAdmissionQueues {
    queues: DashMap<LLMProviderKind, Arc<ProviderWaitQueue>>,
}

impl ProviderAdmissionQueues {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            queues: DashMap::new(),
        })
    }

    fn get_or_insert(
        &self,
        provider: &LLMProviderKind,
        concurrency: Arc<Semaphore>,
    ) -> Arc<ProviderWaitQueue> {
        self.queues
            .entry(provider.clone())
            .or_insert_with(|| Arc::new(ProviderWaitQueue::new(provider.clone(), concurrency)))
            .clone()
    }

    pub fn notify_all(&self) {
        for queue in self.queues.iter() {
            queue.notify.notify_one();
        }
    }

    pub fn waiting_counts(&self) -> ProviderWaitingCounts {
        let mut counts = ProviderWaitingCounts::default();
        for queue in self.queues.iter() {
            let lengths = queue.lengths();
            counts.high = counts.high.saturating_add(lengths.high);
            counts.normal = counts.normal.saturating_add(lengths.normal);
            counts.background = counts.background.saturating_add(lengths.background);
        }
        counts
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ProviderWaitingCounts {
    pub high: usize,
    pub normal: usize,
    pub background: usize,
}

impl ProviderWaitingCounts {
    pub fn total(self) -> usize {
        self.high
            .saturating_add(self.normal)
            .saturating_add(self.background)
    }
}

struct ProviderWaitQueue {
    provider: LLMProviderKind,
    concurrency: Arc<Semaphore>,
    lanes: Mutex<ProviderWaitLanes>,
    notify: Notify,
    started: AtomicBool,
}

impl ProviderWaitQueue {
    fn new(provider: LLMProviderKind, concurrency: Arc<Semaphore>) -> Self {
        Self {
            provider,
            concurrency,
            lanes: Mutex::new(ProviderWaitLanes::default()),
            notify: Notify::new(),
            started: AtomicBool::new(false),
        }
    }

    fn push(&self, parked: ParkedProviderJob) {
        let mut lanes = self.lanes.lock();
        match parked.job.priority {
            Priority::High => lanes.high.push_back(parked),
            Priority::Normal => lanes.normal.push_back(parked),
            Priority::Background => lanes.background.push_back(parked),
        }
    }

    fn pop_next(&self) -> Option<ParkedProviderJob> {
        self.lanes.lock().pop_next()
    }

    fn drain(&self) -> Vec<ParkedProviderJob> {
        self.lanes.lock().drain()
    }

    fn lengths(&self) -> ProviderWaitingCounts {
        let lanes = self.lanes.lock();
        ProviderWaitingCounts {
            high: lanes.high.len(),
            normal: lanes.normal.len(),
            background: lanes.background.len(),
        }
    }
}

#[derive(Default)]
struct ProviderWaitLanes {
    high: VecDeque<ParkedProviderJob>,
    normal: VecDeque<ParkedProviderJob>,
    background: VecDeque<ParkedProviderJob>,
    fairness: LaneFairness,
}

impl ProviderWaitLanes {
    fn pop_next(&mut self) -> Option<ParkedProviderJob> {
        let selected = if self.fairness.consecutive_foreground >= MAX_CONSECUTIVE_FOREGROUND_PICKUPS
            && !self.background.is_empty()
        {
            self.background.pop_front()
        } else if self.fairness.consecutive_high >= MAX_CONSECUTIVE_HIGH_PICKUPS
            && !self.normal.is_empty()
        {
            self.normal.pop_front()
        } else {
            self.high
                .pop_front()
                .or_else(|| self.normal.pop_front())
                .or_else(|| self.background.pop_front())
        };
        if let Some(job) = selected.as_ref() {
            self.fairness.record(job.job.priority);
        }
        selected
    }

    fn drain(&mut self) -> Vec<ParkedProviderJob> {
        self.high
            .drain(..)
            .chain(self.normal.drain(..))
            .chain(self.background.drain(..))
            .collect()
    }
}

struct ParkedProviderJob {
    job: LlmJob,
    task_cancel_token: CancellationToken,
    wait_started: Instant,
}

// Preserve foreground preference without allowing a continuously-ready lane
// to monopolize a worker forever. These are pickup bounds, not concurrency
// limits: provider semaphores still enforce the configured execution caps.
const MAX_CONSECUTIVE_HIGH_PICKUPS: usize = 8;
const MAX_CONSECUTIVE_FOREGROUND_PICKUPS: usize = 12;

#[derive(Default)]
struct LaneFairness {
    consecutive_high: usize,
    consecutive_foreground: usize,
}

impl LaneFairness {
    fn record(&mut self, priority: Priority) {
        match priority {
            Priority::High => {
                self.consecutive_high = self.consecutive_high.saturating_add(1);
                self.consecutive_foreground = self.consecutive_foreground.saturating_add(1);
            },
            Priority::Normal => {
                self.consecutive_high = 0;
                self.consecutive_foreground = self.consecutive_foreground.saturating_add(1);
            },
            Priority::Background => {
                self.consecutive_high = 0;
                self.consecutive_foreground = 0;
            },
        }
    }
}

fn try_pick_ready_job(
    fairness: &mut LaneFairness,
    allow_background: bool,
    high: &FairLane,
    normal: &FairLane,
    background: &FairLane,
) -> Option<LlmJob> {
    if allow_background && fairness.consecutive_foreground >= MAX_CONSECUTIVE_FOREGROUND_PICKUPS {
        if let Some(job) = background.pop_next() {
            fairness.record(job.priority);
            return Some(job);
        }
        // Do not throttle foreground solely because a historical burst hit
        // its bound when no background job is actually waiting.
        fairness.consecutive_foreground = 0;
    }

    if fairness.consecutive_high >= MAX_CONSECUTIVE_HIGH_PICKUPS {
        if let Some(job) = normal.pop_next() {
            fairness.record(job.priority);
            return Some(job);
        }
        if allow_background {
            if let Some(job) = background.pop_next() {
                fairness.record(job.priority);
                return Some(job);
            }
        }
        fairness.consecutive_high = 0;
    }

    let job = high.pop_next().or_else(|| normal.pop_next()).or_else(|| {
        if allow_background {
            background.pop_next()
        } else {
            None
        }
    })?;
    fairness.record(job.priority);
    Some(job)
}

/// Run an all-lanes worker until the shutdown token fires or all lanes close.
/// Retained as the stable public entry point for downstream queue embeddings.
pub(crate) async fn worker_loop(
    worker_id: usize,
    ctx: Arc<WorkerContext>,
    high: FairLane,
    normal: FairLane,
    background: FairLane,
) {
    worker_loop_with_lane_policy(worker_id, true, ctx, high, normal, background).await;
}

/// Internal pool entry point carrying the per-worker background-lane policy.
pub(crate) async fn worker_loop_with_lane_policy(
    worker_id: usize,
    allow_background: bool,
    ctx: Arc<WorkerContext>,
    high: FairLane,
    normal: FairLane,
    background: FairLane,
) {
    debug!(worker_id, "dispatch worker starting");
    let mut fairness = LaneFairness::default();
    loop {
        if let Some(job) =
            try_pick_ready_job(&mut fairness, allow_background, &high, &normal, &background)
        {
            ctx.metrics.worker_started();
            process_job(worker_id, ctx.clone(), job).await;
            ctx.metrics.worker_finished();
            continue;
        }

        // Wait without popping. `FairLane::recv` is not cancel-safe under
        // `select!` (a Ready branch pops; dropping the other Ready recvs
        // would lose those jobs). After a wake, loop back to
        // `try_pick_ready_job` so owner RR and consecutive-high bounds apply.
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {
                debug!(worker_id, "dispatch worker shutting down");
                break;
            }
            _ = high.wait_not_empty() => {},
            _ = normal.wait_not_empty() => {},
            _ = background.wait_not_empty(), if allow_background => {},
        }
    }
}

async fn process_job(worker_id: usize, ctx: Arc<WorkerContext>, mut job: LlmJob) {
    let job_id = job.job_id.clone();
    let priority = job.priority;
    let cfg_snapshot = ctx.config.read().clone();
    let routing_router = job
        .router_snapshot
        .clone()
        .unwrap_or_else(|| Arc::clone(&ctx.router));

    // 0. External cancellation intent — handles the race between
    // cancel_task / cancel_job removing the registry entry and the
    // worker picking up the LlmJob still sitting in the channel.
    if let Some(reason) = ctx.registry.take_cancellation_intent(&job_id) {
        tombstone_at_pickup(&ctx, job, reason, "external_cancel_intent").await;
        return;
    }

    // 1. Submission-deadline gate.
    if let Some(deadline) = job.submission_deadline {
        if Instant::now() >= deadline {
            tombstone_at_pickup(
                &ctx,
                job,
                TombstoneReason::DeadlineExceeded,
                "submission_deadline_exceeded",
            )
            .await;
            return;
        }
    }

    // 2. Pre-dispatch cancellation gate. An in-place retry previously stayed
    // inside this function's provider loop, so yielding it to the byte
    // coordinator must not add another storage-backed task snapshot. Sticky
    // cancellation intent/token checks still run on every resumed attempt.
    if !job.resume_in_place_retry {
        if let Some(task_ref) = &job.task_ref {
            match ctx.task_state.snapshot(&task_ref.task_id).await {
                Ok(None) => {
                    tombstone_at_pickup(&ctx, job, TombstoneReason::TaskMissing, "task_missing")
                        .await;
                    return;
                },
                // A job that PRODUCES the terminal record is exempt: it runs
                // after the execution is terminal by construction, so gating it
                // on the task's terminal state is circular — no output means
                // the task fails, and a failed task cancels the output.
                Ok(Some(snap))
                    if snap.is_cancelled_or_terminal_failed && !task_ref.survives_terminal_task =>
                {
                    let reason = TombstoneReason::TaskCancelled {
                        reason: snap.cancel_reason.clone(),
                    };
                    tombstone_at_pickup(&ctx, job, reason, "task_cancelled_pre_dispatch").await;
                    return;
                },
                _ => {},
            }
        }
    }

    // Resolve provider kind from the request's profile.
    let provider_kind = resolve_provider_kind(&*routing_router, &job.request);
    let provider_state = ctx.provider_state.get(&provider_kind);

    // 3. Provider cool-down gate. Sleep+requeue via detached task so
    // workers don't tight-loop pulling the same job out of the channel
    // and pushing it back in. The job is moved back to `pending` so the
    // registry snapshot reflects state.
    // Local-prep re-entry already passed these gates before the park. A
    // 60–300s generate must not inherit a sibling's cooldown/breaker and
    // terminal-fail a job the in-worker path would have invoked.
    if !job.resume_in_place_retry && !job.local_prep_done {
        if let Some(remaining) = provider_state.cooldown_remaining() {
            debug!(
                worker_id,
                ?provider_kind,
                cooldown_ms = remaining.as_millis() as u64,
                "provider in cooldown — deferred re-queue"
            );
            // A provider-capacity waiter may have resumed just as another call
            // established a cooldown. Do not reserve that provider slot while the
            // job sleeps outside the worker pool.
            ctx.registry.release_capacity_permit(&job_id);
            defer_requeue(&ctx, job, priority, remaining);
            return;
        }
    }
    // 3b. Circuit-breaker gate.
    if !job.resume_in_place_retry && !job.local_prep_done && provider_state.breaker_is_open() {
        terminal_fail(
            &ctx,
            job,
            LLMError::ProviderUnavailable,
            ErrorClass::Server5xx,
        )
        .await;
        return;
    }

    // Subscribe to the per-task cancel token BEFORE marking in-flight (and
    // before the local-prep await below). `fire_cancel` only fires a token that
    // already exists; if we subscribed only after local-prep — which can block
    // on an Ollama summarisation for seconds — a cancel landing in that window
    // would find no token, no-op, and the freshly-created token would never
    // fire, leaving the in-flight provider call running (and billing). Creating
    // it here closes that window: the `select!` below races this (sticky) token,
    // so a cancel fired any time after this point aborts the call.
    let cancel_token = ctx.task_state.subscribe_cancel(job.task_ref.as_ref());

    // Mark in_flight.
    let provider_kind_for_meta = provider_kind.clone();
    let pickup_wait_ms = job.submitted_at.elapsed().as_millis() as u64;
    ctx.registry.mark_in_flight(&job_id, |meta| {
        meta.state = JobState::InFlight;
        if meta.wait_ms.is_none() {
            meta.wait_ms = Some(pickup_wait_ms);
        }
        meta.provider = Some(provider_kind_for_meta);
    });

    // Re-check cancellation intent AFTER mark_in_flight to plug the race
    // window where `cancel_task` recorded an intent between the worker's
    // initial intent check and now. The intent remains separate from the
    // lifecycle state until this terminal transition is published.
    if let Some(reason) = ctx.registry.take_cancellation_intent(&job_id) {
        tombstone_in_flight(&ctx, job, reason, "external_cancel_intent_post_mark").await;
        return;
    }

    // 4. Local-prep. Cheap inlining (disabled / disclosure / below threshold)
    // stays on this worker so `summarisable_blocks` never leak into
    // `provider.invoke`. Ollama-bound prep parks on LocalPrepCoordinator and
    // resumes with `local_prep_done` instead of occupying a worker slot.
    let had_summarisable_blocks = !job.request.summarisable_blocks.is_empty();
    let (local_prep_stat, local_prep_cancellation) = if job.resume_in_place_retry {
        (None, None)
    } else if job.local_prep_done {
        (job.local_prep_stat.take(), None)
    } else if local_prep_needs_ollama(&job.request, &cfg_snapshot.local_prep) {
        defer_until_local_prep(&ctx, job, cancel_token);
        return;
    } else {
        maybe_local_prep_direct_cancellable(
            &mut job.request,
            &cfg_snapshot.local_prep,
            Some(&cancel_token),
            Some(&ctx.shutdown),
        )
        .await
    };
    if local_prep_stat.is_some() {
        ctx.metrics.incr_local_prep_run();
        let local_prep_for_meta = local_prep_stat.clone();
        if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
            meta.local_prep = local_prep_for_meta;
        }
    } else if had_summarisable_blocks {
        ctx.metrics.incr_local_prep_skip();
    }
    if let Some(cancellation) = local_prep_cancellation {
        let (reason, log_reason) = match cancellation {
            LocalPrepCancellation::Task => (
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_during_local_prep",
            ),
            LocalPrepCancellation::QueueShutdown => (
                TombstoneReason::QueueShutdown,
                "queue_shutdown_during_local_prep",
            ),
        };
        tombstone_in_flight(&ctx, job, reason, log_reason).await;
        return;
    }

    // 5. Per-provider concurrency gate.
    let permit = if let Some(permit) = ctx.registry.take_capacity_permit(&job_id, &provider_kind) {
        permit
    } else {
        match provider_state.concurrency.clone().try_acquire_owned() {
            Ok(permit) => permit,
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                defer_until_provider_capacity(
                    &ctx,
                    job,
                    provider_kind.clone(),
                    provider_state.concurrency.clone(),
                    cancel_token,
                );
                return;
            },
            Err(tokio::sync::TryAcquireError::Closed) => {
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::QueueShutdown,
                    "semaphore_closed",
                )
                .await;
                return;
            },
        }
    };

    // Local preparation and provider-capacity wait are not provider
    // execution. Set the first provider-phase boundary only after both
    // have completed, preserving it across retry requeues.
    let first_dispatch = if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
        if meta.dispatched_at_ms.is_none() {
            meta.dispatched_at_ms = Some(chrono::Utc::now().timestamp_millis());
            true
        } else {
            false
        }
    } else {
        false
    };
    // A job is dispatched only once it owns the matching provider permit
    // and is about to make an actual provider attempt. Local preparation
    // and provider-capacity parking are observable but are not dispatches.
    if first_dispatch {
        ctx.metrics.incr_dispatched();
        emit_with_meta(&ctx, &job_id, |m| LlmQueueEvent::Dispatched { meta: m });
    }

    // Isolated: scheduler worker already owns the provider permit and has
    // published dispatched_at. Hand HTTP + retry/tombstone to an executor so
    // `worker_finished` runs before provider I/O. Legacy keeps HTTP in-worker.
    if ctx.engine == DispatchEngine::ProviderIsolated {
        let exec_ctx = Arc::clone(&ctx);
        let handle = tokio::spawn(async move {
            run_physical_attempt(
                worker_id,
                exec_ctx,
                job,
                permit,
                cancel_token,
                local_prep_stat,
                routing_router,
                cfg_snapshot,
                provider_kind,
                pickup_wait_ms,
            )
            .await;
        });
        let mut handles = ctx.isolated_executors.lock();
        handles.retain(|pending| !pending.is_finished());
        handles.push(handle);
        return;
    }

    run_physical_attempt(
        worker_id,
        ctx,
        job,
        permit,
        cancel_token,
        local_prep_stat,
        routing_router,
        cfg_snapshot,
        provider_kind,
        pickup_wait_ms,
    )
    .await;
}

/// Watchdog + route + retry/tombstone tail. Shared by the in-worker
/// (`LegacyWorkerPool`) path and the `ProviderIsolated` executor so
/// retry/tombstone/idempotency stay identical. Requeues go back to lanes.
async fn run_physical_attempt(
    worker_id: usize,
    ctx: Arc<WorkerContext>,
    mut job: LlmJob,
    permit: OwnedSemaphorePermit,
    cancel_token: CancellationToken,
    local_prep_stat: Option<LocalPrepStat>,
    routing_router: Arc<dyn DispatchRouter>,
    cfg_snapshot: DispatchConfig,
    provider_kind: LLMProviderKind,
    pickup_wait_ms: u64,
) {
    let job_id = job.job_id.clone();
    let priority = job.priority;

    if let Some(task_ref) = job.task_ref.clone() {
        let ledger_event = LlmCallLedgerEvent::AttemptStart {
            job_id: job_id.clone(),
            attempt: job.attempts.total + 1,
            dispatched_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        };
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {
                drop(permit);
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_during_attempt_ledger",
                )
                .await;
                return;
            }
            _ = cancel_token.cancelled() => {
                drop(permit);
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_during_attempt_ledger",
                )
                .await;
                return;
            }
            _ = ctx.ledger_sink.append(&task_ref, ledger_event) => {},
        }
    }

    // RPM/TPM after the provider permit and before the cloud cap so a
    // rate-limit sleep does not occupy a global cloud slot. Missing/0 is
    // a no-op. Isolated executors wait here without occupying a scheduler
    // worker.
    let estimated_tokens = estimate_request_tokens(&job.request);
    tokio::select! {
        biased;
        _ = ctx.shutdown.cancelled() => {
            drop(permit);
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::QueueShutdown,
                "queue_shutdown_waiting_for_quota",
            )
            .await;
            return;
        }
        _ = cancel_token.cancelled() => {
            drop(permit);
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_waiting_for_quota",
            )
            .await;
            return;
        }
        _ = ctx.provider_quota.acquire(&provider_kind, estimated_tokens) => {},
    }

    // Global cloud cap after the provider permit and quota, before route().
    // Ollama and `global_cloud_concurrency == 0` skip this. Isolated
    // executors wait here without occupying a scheduler worker.
    let cloud_permit = tokio::select! {
        biased;
        _ = ctx.shutdown.cancelled() => {
            drop(permit);
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::QueueShutdown,
                "queue_shutdown_waiting_for_cloud",
            )
            .await;
            return;
        }
        _ = cancel_token.cancelled() => {
            drop(permit);
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_waiting_for_cloud",
            )
            .await;
            return;
        }
        acquired = ctx.cloud_admission.acquire(&provider_kind) => match acquired {
            Ok(cloud_permit) => cloud_permit,
            Err(_) => {
                drop(permit);
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::QueueShutdown,
                    "cloud_semaphore_closed",
                )
                .await;
                return;
            },
        },
    };
    let permits = AttemptPermits {
        _provider: permit,
        _cloud: cloud_permit,
    };

    // 6. Watchdog deadline.
    let profile_timeout = resolve_profile_timeout(&*routing_router, &job.request);
    let watchdog_deadline = Instant::now()
        + Duration::from_secs_f64(profile_timeout.as_secs_f64() * cfg_snapshot.watchdog_factor);

    // 7. Race provider call against cancel + watchdog.
    // Arm order matters under `biased`: when multiple arms are ready in
    // the same poll, the first listed wins. We want:
    //   1. cancel first  — cancellation should always preempt.
    //   2. route second  — a successful response should win over a
    //      simultaneously-firing watchdog (don't discard a completed
    //      call as a watchdog timeout).
    //   3. watchdog last — fallback for the genuinely hung case.
    // This request is no longer pending: it owns the matching provider
    // permit and is about to enter the physical router call. Keep the byte
    // reservation through lane pickup, local preparation, cooldown and
    // provider-capacity parking so none of those queues can retain an
    // unbounded payload population outside admission accounting.
    job.queued_byte_permit.take();
    job.resume_in_place_retry = false;
    let attempt_start = Instant::now();
    let request_clone = job.request.clone();
    let protected_physical_attempt = job.request.metadata.single_physical_attempt;
    let mut result = if protected_physical_attempt {
        if cancel_token.is_cancelled() || ctx.shutdown.is_cancelled() {
            let shutdown = ctx.shutdown.is_cancelled();
            drop(permits);
            if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                meta.add_provider_execution(attempt_start.elapsed());
            }
            tombstone_in_flight(
                &ctx,
                job,
                if shutdown {
                    TombstoneReason::QueueShutdown
                } else {
                    TombstoneReason::TaskCancelledInFlight { reason: None }
                },
                if shutdown {
                    "queue_shutdown_in_flight"
                } else {
                    "task_cancelled_in_flight"
                },
            )
            .await;
            return;
        }
        // Once the router starts a protected physical attempt it owns a
        // move-only durable resource permit. Dropping the route future on
        // cancel/shutdown/watchdog would strand Held usage because Drop
        // cannot perform the required async uncertain settlement. The
        // provider request has its own exact bounded timeout; drain it to
        // the router settlement boundary, then discard/tombstone the
        // response when cancellation won logically.
        let result = routing_router.route(request_clone).await;
        if cancel_token.is_cancelled() {
            drop(permits);
            if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                meta.add_provider_execution(attempt_start.elapsed());
            }
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_in_flight",
            )
            .await;
            return;
        }
        if ctx.shutdown.is_cancelled() {
            drop(permits);
            if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                meta.add_provider_execution(attempt_start.elapsed());
            }
            tombstone_in_flight(
                &ctx,
                job,
                TombstoneReason::QueueShutdown,
                "queue_shutdown_in_flight",
            )
            .await;
            return;
        }
        if Instant::now() >= watchdog_deadline {
            Err(LLMError::WorkerWatchdog {
                factor: cfg_snapshot.watchdog_factor,
            })
        } else {
            result
        }
    } else {
        tokio::select! {
            biased;
            _ = cancel_token.cancelled() => {
                drop(permits);
                if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                    meta.add_provider_execution(attempt_start.elapsed());
                }
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_in_flight",
                ).await;
                return;
            }
            _ = ctx.shutdown.cancelled() => {
                drop(permits);
                if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                    meta.add_provider_execution(attempt_start.elapsed());
                }
                tombstone_in_flight(
                    &ctx,
                    job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_in_flight",
                ).await;
                return;
            }
            r = routing_router.route(request_clone) => r,
            _ = tokio::time::sleep_until(watchdog_deadline.into()) => {
                Err(LLMError::WorkerWatchdog { factor: cfg_snapshot.watchdog_factor })
            }
        }
    };
    let attempt_duration = attempt_start.elapsed();
    drop(permits);
    if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
        meta.add_provider_execution(attempt_duration);
    }

    // The router increments the request-local shared counter immediately
    // before each physical provider invocation. This matters when one
    // outer dispatch attempt traverses a fallback profile: both provider
    // calls receive distinct ordinals even though retry policy sees one
    // routed request.
    job.attempts.provider_total = job.request.metadata.provider_attempt_count();
    if result.is_ok() && job.attempts.provider_total == 0 {
        result = Err(LLMError::Other(
            "router returned a response without recording a provider attempt".to_string(),
        ));
    }
    let provider_attempt_ordinal = job.attempts.provider_total;
    let provider_attempt_id = (provider_attempt_ordinal > 0).then(|| {
        job.trace_context
            .provider_attempt_id(provider_attempt_ordinal)
    });
    if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
        meta.provider_attempt_count = job.attempts.provider_total;
        meta.provider_attempt_id = provider_attempt_id.clone();
    }
    result = result.and_then(|mut response| {
        let route_identity = response.route_identity.clone();
        response.trace_receipt = Some(crate::trace::LlmTraceReceipt::queued(
            job.trace_context.clone(),
            job_id.to_string(),
            provider_attempt_ordinal,
        ));
        response
            .into_retained_bounded()
            .map_err(|error| match route_identity {
                Some(identity) => {
                    error.with_route(identity.profile, identity.provider, identity.model)
                },
                None => error,
            })
    });

    // 8. Update provider state based on outcome.
    let err_class = match &result {
        Ok(_) => None,
        Err(e) => Some(classify(e)),
    };
    let effective_provider_kind = outcome_provider_kind(&result, &provider_kind);
    // Admission uses the request's initial route because fallback is
    // selected inside the router. Outcome health, cooldown and breaker
    // state must belong to the provider that actually handled the final
    // physical attempt, otherwise cross-provider fallback poisons the
    // primary provider's state and leaves the failing fallback ungoverned.
    let outcome_provider_state = ctx.provider_state.get(&effective_provider_kind);
    if let Err(error) = &result {
        if let Some((profile, provider, model)) = error.effective_route() {
            if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
                meta.profile = Some(profile.to_string());
                meta.provider = Some(provider.clone());
                meta.model = Some(model.to_string());
            }
        }
    }
    outcome_provider_state.observe(err_class);
    ctx.cloud_admission
        .observe(&effective_provider_kind, err_class);

    if let Some(meta) = ctx.registry.find(&job_id) {
        ctx.events.emit(LlmQueueEvent::AttemptDone {
            meta,
            success: result.is_ok(),
        });
    }

    // 9. Resolve or retry.
    match result {
        Ok(response) => {
            let total_attempts = job.attempts.total + 1;
            let route_identity = response.route_identity.clone();
            let trace_receipt = response
                .trace_receipt
                .clone()
                .expect("dispatch success must retain its admitted trace receipt");
            let completed_at_ms = chrono::Utc::now().timestamp_millis();
            let timing_meta = ctx.registry.find(&job_id);
            let wait_ms = timing_meta
                .as_ref()
                .and_then(|meta| meta.wait_ms)
                .unwrap_or(pickup_wait_ms);
            let execution_ms = timing_meta
                .as_ref()
                .and_then(|meta| meta.execution_ms)
                .unwrap_or_default();
            let effective_local_prep = local_prep_stat.clone().or_else(|| {
                timing_meta
                    .as_ref()
                    .and_then(|meta| meta.local_prep.clone())
            });
            let tokens = token_summary(&response);
            let dispatched = DispatchedResponse {
                response: Arc::new(response),
                wait: Duration::from_millis(wait_ms),
                execution: Duration::from_millis(execution_ms),
                local_prep: effective_local_prep.clone(),
                attempts: total_attempts,
                trace_receipt,
            };

            let ledger_event = LlmCallLedgerEvent::AttemptDone {
                job_id: job_id.clone(),
                attempt: total_attempts,
                success: true,
                tokens: tokens.clone(),
                duration_ms: attempt_duration.as_millis() as u64,
                error_class: None,
                error_msg: None,
            };
            // Release lane lifetime admission and publish the terminal
            // registry state before durable telemetry. The provider and
            // queued-byte permits are already gone; a slow ledger must not
            // keep a completed request counted as in-flight or consume a
            // lane slot.
            ctx.metrics.incr_completed();
            ctx.metrics
                .record_wait_ms(dispatched.wait.as_millis() as u64);
            let local_prep_for_meta = effective_local_prep;
            let tokens_for_meta = tokens.clone();
            let dispatched_meta = ctx.registry.in_flight_to_completed(&job_id, |m| {
                m.state = JobState::Completed;
                m.set_terminal_timing(completed_at_ms);
                m.tokens = tokens_for_meta;
                m.attempts = total_attempts;
                m.provider_attempt_count = provider_attempt_ordinal;
                m.provider_attempt_id = provider_attempt_id.clone();
                if let Some(identity) = route_identity.as_ref() {
                    m.profile = Some(identity.profile.clone());
                    m.provider = Some(identity.provider.clone());
                    m.model = Some(identity.model.clone());
                }
                if local_prep_for_meta.is_some() {
                    m.local_prep = local_prep_for_meta;
                }
            });
            if let Some(meta) = dispatched_meta {
                ctx.events.emit(LlmQueueEvent::Completed { meta });
            }
            let task_ref = job.task_ref.clone();
            if let Some(key) = job.idempotency_key.as_deref() {
                // Release idempotency ownership and make the completed
                // response visible after the authoritative registry/event
                // transition, before auxiliary durable telemetry. A stuck
                // ledger must not strand equivalent callers behind a
                // generation whose provider work is already complete.
                ctx.inflight_index
                    .resolve(key, &job_id, Arc::new(Ok(dispatched.clone())));
            }
            let completion_ledger_event = LlmCallLedgerEvent::Completed {
                job_id: job_id.clone(),
                total_attempts,
                wait_ms: dispatched.wait.as_millis() as u64,
                execution_ms: dispatched.execution.as_millis() as u64,
                tokens: tokens.clone(),
            };
            let _ = job.response_tx.send(Ok(dispatched));

            if let Some(task_ref) = task_ref.as_ref() {
                tokio::select! {
                    biased;
                    _ = ctx.shutdown.cancelled() => {},
                    _ = async {
                        ctx.ledger_sink.append(task_ref, ledger_event).await;
                        ctx.ledger_sink.append(task_ref, completion_ledger_event).await;
                    } => {},
                }
            }
            return;
        },
        Err(err) => {
            let class = err_class.unwrap_or(ErrorClass::Unknown);
            let err_msg = err.to_string();

            // 429 special-case: cool-down + deferred re-queue without
            // charging an attempt. Same defer_requeue path as the
            // cool-down gate so workers don't tight-loop. We DO emit an
            // AttemptDone ledger event (success=false, error_class=
            // rate_limit) so the ledger shows the attempt happened —
            // otherwise a job that 429s repeatedly looks like it never
            // tried, only re-queued.
            if !job.request.metadata.single_physical_attempt
                && matches!(class, ErrorClass::RateLimit)
            {
                let retry_after = retry_after_from(&err)
                    .unwrap_or_else(|| outcome_provider_state.fallback_retry_after());
                outcome_provider_state.engage_cooldown(retry_after);
                debug!(?retry_after, "engaging provider cooldown after 429");
                if let Some(task_ref) = &job.task_ref {
                    ctx.ledger_sink
                        .append(
                            task_ref,
                            LlmCallLedgerEvent::AttemptDone {
                                job_id: job_id.clone(),
                                attempt: job.attempts.total.saturating_add(1),
                                success: false,
                                tokens: None,
                                duration_ms: attempt_duration.as_millis() as u64,
                                error_class: Some(ErrorClass::RateLimit),
                                error_msg: Some(truncate(&err_msg, 400)),
                            },
                        )
                        .await;
                }
                defer_requeue(&ctx, job, priority, retry_after);
                return;
            }

            job.attempts.dispatch += 1;
            job.attempts.total += 1;
            job.attempts.record(
                AttemptError {
                    attempt: job.attempts.total,
                    at: std::time::SystemTime::now(),
                    error: truncate(&err_msg, 400),
                    class,
                    retriable: class.is_retriable(),
                },
                cfg_snapshot.max_recorded_errors,
            );

            if let Some(task_ref) = &job.task_ref {
                ctx.ledger_sink
                    .append(
                        task_ref,
                        LlmCallLedgerEvent::AttemptDone {
                            job_id: job_id.clone(),
                            attempt: job.attempts.total,
                            success: false,
                            tokens: None,
                            duration_ms: attempt_duration.as_millis() as u64,
                            error_class: Some(class),
                            error_msg: Some(truncate(&err_msg, 400)),
                        },
                    )
                    .await;
            }

            let decision = if job.request.metadata.single_physical_attempt {
                RetryDecision::TerminalFail
            } else {
                decide(&job.attempts, class, &cfg_snapshot, None)
            };
            match decision {
                RetryDecision::RetryInPlace { backoff, .. } => {
                    job.resume_in_place_retry = true;
                    debug!(
                        worker_id,
                        attempt = job.attempts.total,
                        backoff_ms = backoff.as_millis() as u64,
                        "deferring in-place retry without occupying a worker"
                    );
                    let total_for_meta = job.attempts.total;
                    ctx.registry.in_flight_to_pending(&job_id, |meta| {
                        meta.state = JobState::Pending;
                        meta.attempts = total_for_meta;
                    });
                    spawn_delayed_requeue(
                        &ctx,
                        job,
                        priority,
                        backoff,
                        "shutdown_during_retry_backoff",
                    );
                    return;
                },
                RetryDecision::Requeue {
                    backoff,
                    next_cycle,
                } => {
                    job.attempts.cycle = next_cycle;
                    job.attempts.dispatch = 0;
                    debug!(
                        worker_id,
                        cycle = next_cycle,
                        backoff_ms = backoff.as_millis() as u64,
                        "re-queue cycle"
                    );
                    if let Some(task_ref) = &job.task_ref {
                        ctx.ledger_sink
                            .append(
                                task_ref,
                                LlmCallLedgerEvent::Requeued {
                                    job_id: job_id.clone(),
                                    cycle: next_cycle,
                                    reason: "retry_exhausted_in_place".to_string(),
                                    error_class: class,
                                },
                            )
                            .await;
                    }
                    ctx.metrics.incr_requeued();

                    let total_for_meta = job.attempts.total;
                    let dispatched_meta = ctx.registry.in_flight_to_pending(&job_id, |m| {
                        m.state = JobState::Pending;
                        m.attempts = total_for_meta;
                    });
                    if let Some(meta) = dispatched_meta {
                        ctx.events.emit(LlmQueueEvent::Requeued {
                            meta,
                            cycle: next_cycle,
                        });
                    }

                    spawn_delayed_requeue(
                        &ctx,
                        job,
                        priority,
                        backoff,
                        "shutdown_during_retry_backoff",
                    );
                    return;
                },
                RetryDecision::TerminalFail => {
                    terminal_fail(&ctx, job, err, class).await;
                    return;
                },
            }
        },
    }
}

fn token_summary(response: &LLMResponse) -> Option<TokenSummary> {
    response.usage.as_ref().map(|u| TokenSummary {
        prompt_tokens: u.prompt_tokens.unwrap_or(0),
        completion_tokens: u.completion_tokens.unwrap_or(0),
        cached_tokens: u.cached_tokens.unwrap_or(0),
        reasoning_tokens: u.reasoning_tokens.unwrap_or(0),
    })
}

fn resolve_provider_kind(router: &dyn DispatchRouter, request: &LLMRequest) -> LLMProviderKind {
    router.provider_for_request(request).unwrap_or_default()
}

fn outcome_provider_kind(
    result: &LLMResult<LLMResponse>,
    initial_provider: &LLMProviderKind,
) -> LLMProviderKind {
    match result {
        Ok(response) => response
            .route_identity
            .as_ref()
            .map(|identity| identity.provider.clone()),
        Err(error) => error
            .effective_route()
            .map(|(_, provider, _)| provider.clone()),
    }
    .unwrap_or_else(|| initial_provider.clone())
}

fn resolve_profile_timeout(router: &dyn DispatchRouter, request: &LLMRequest) -> Duration {
    let secs = router.timeout_for_request(request).unwrap_or(600);
    Duration::from_secs(secs.max(1))
}

async fn tombstone_at_pickup(
    ctx: &Arc<WorkerContext>,
    mut job: LlmJob,
    reason: TombstoneReason,
    log_reason: &str,
) {
    // TEMP TRACE (delegated-child synthesis cancellation hunt): the log_reason
    // alone cannot distinguish which gate fired for which job, so carry the
    // operation and the task_ref that the pre-dispatch gate keys on.
    info!(
        job_id = %job.job_id,
        log_reason,
        tombstone = %reason.name(),
        task_ref = ?job.task_ref,
        trace_id = ?job.trace_id,
        "[TOMBSTONE-TRACE] tombstoning at pickup"
    );
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let reason_for_meta = reason.clone();
    let meta = ctx.registry.pending_to_tombstone(&job_id, |m| {
        m.state = JobState::Tombstoned;
        m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
        m.tombstone = Some(reason_for_meta);
    });
    ctx.metrics.incr_tombstoned();
    if let Some(meta) = meta {
        ctx.events.emit(LlmQueueEvent::Tombstoned {
            meta,
            reason: reason.clone(),
        });
    }
    let task_ref = job.task_ref.clone();
    let attempts_so_far = job.attempts.total;
    if let Some(key) = job.idempotency_key.as_deref() {
        ctx.inflight_index.resolve(
            key,
            &job_id,
            Arc::new(Err(LLMError::Cancelled {
                reason: reason.name().to_string(),
            })),
        );
    }
    let _ = job.response_tx.send(Err(LLMError::Cancelled {
        reason: reason.name().to_string(),
    }));
    if let Some(task_ref) = task_ref.as_ref() {
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Tombstoned {
                    job_id: job_id.clone(),
                    reason: reason.clone(),
                    attempts_so_far,
                },
            ) => {},
        }
    }
}

async fn tombstone_in_flight(
    ctx: &Arc<WorkerContext>,
    mut job: LlmJob,
    reason: TombstoneReason,
    log_reason: &str,
) {
    info!(job_id = %job.job_id, log_reason, "tombstoning in-flight");
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let provider_attempt_count = job.request.metadata.provider_attempt_count();
    let attempts_so_far = job
        .attempts
        .total
        .max(u32::from(provider_attempt_count > 0));
    let reason_for_meta = reason.clone();
    let meta = ctx.registry.pending_to_tombstone(&job_id, |m| {
        m.state = JobState::Tombstoned;
        m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
        m.tombstone = Some(reason_for_meta);
        m.provider_attempt_count = provider_attempt_count;
        m.provider_attempt_id = (provider_attempt_count > 0).then(|| {
            job.trace_context
                .provider_attempt_id(provider_attempt_count)
        });
        m.attempts = attempts_so_far;
    });
    ctx.metrics.incr_tombstoned();
    if let Some(meta) = meta {
        ctx.events.emit(LlmQueueEvent::Tombstoned {
            meta,
            reason: reason.clone(),
        });
    }
    let task_ref = job.task_ref.clone();
    if let Some(key) = job.idempotency_key.as_deref() {
        ctx.inflight_index.resolve(
            key,
            &job_id,
            Arc::new(Err(LLMError::Cancelled {
                reason: reason.name().to_string(),
            })),
        );
    }
    let _ = job.response_tx.send(Err(LLMError::Cancelled {
        reason: reason.name().to_string(),
    }));
    if let Some(task_ref) = task_ref.as_ref() {
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Tombstoned {
                    job_id: job_id.clone(),
                    reason: reason.clone(),
                    attempts_so_far,
                },
            ) => {},
        }
    }
}

async fn terminal_fail(
    ctx: &Arc<WorkerContext>,
    mut job: LlmJob,
    err: LLMError,
    class: ErrorClass,
) {
    error!(
        job_id = %job.job_id,
        attempts = job.attempts.total,
        error = %err,
        "terminal LLM failure"
    );
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let err_msg = err.to_string();
    let effective_route = err.effective_route().map(|(profile, provider, model)| {
        (profile.to_string(), provider.clone(), model.to_string())
    });
    let last_error = truncate(&err_msg, 400);
    let total_attempts = job.attempts.total.max(1);
    let last_error_for_meta = last_error.clone();
    let dispatched_meta = ctx.registry.in_flight_to_failed(&job_id, |m| {
        m.state = JobState::Failed;
        m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
        m.attempts = total_attempts;
        m.error = Some(last_error_for_meta);
        m.error_class = Some(class);
        if let Some((profile, provider, model)) = effective_route.as_ref() {
            m.profile = Some(profile.clone());
            m.provider = Some(provider.clone());
            m.model = Some(model.clone());
        }
    });
    ctx.metrics.incr_failed();
    if let Some(meta) = dispatched_meta {
        ctx.events.emit(LlmQueueEvent::Failed {
            meta,
            error_class: class,
        });
    }
    let surface_err = if total_attempts > 1 {
        LLMError::AllRetriesExhausted {
            attempts: total_attempts,
            last_error: last_error.clone(),
        }
    } else {
        err.clone()
    };
    let surface_err = if let Some((profile, provider, model)) = effective_route {
        surface_err.with_route(profile, provider, model)
    } else {
        surface_err
    };
    let task_ref = job.task_ref.clone();
    if let Some(key) = job.idempotency_key.as_deref() {
        ctx.inflight_index
            .resolve(key, &job.job_id, Arc::new(Err(surface_err.clone())));
    }
    let _ = job.response_tx.send(Err(surface_err));
    if let Some(task_ref) = task_ref.as_ref() {
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Failed {
                    job_id: job_id.clone(),
                    total_attempts,
                    last_error: last_error.clone(),
                    error_class: class,
                },
            ) => {},
        }
    }
}

/// Move job in_flight → pending and schedule its bounded delayed requeue.
/// Every delayed task is owned by the queue's `TaskTracker`, and the job keeps
/// its lane admission permit while sleeping.
fn defer_requeue(ctx: &Arc<WorkerContext>, job: LlmJob, priority: Priority, delay: Duration) {
    let job_id = job.job_id.clone();
    let total = job.attempts.total;
    let dispatched_meta = ctx.registry.in_flight_to_pending(&job_id, |m| {
        m.state = JobState::Pending;
        m.attempts = total;
    });
    if let Some(meta) = dispatched_meta {
        ctx.events.emit(LlmQueueEvent::Requeued {
            meta,
            cycle: job.attempts.cycle,
        });
    }
    ctx.metrics.incr_requeued();
    spawn_delayed_requeue(ctx, job, priority, delay, "shutdown_during_requeue");
}

fn spawn_delayed_requeue(
    ctx: &Arc<WorkerContext>,
    job: LlmJob,
    priority: Priority,
    delay: Duration,
    shutdown_reason: &'static str,
) {
    park_for_byte_admission(ctx, job, priority, Instant::now() + delay, shutdown_reason);
}

fn park_for_byte_admission(
    ctx: &Arc<WorkerContext>,
    job: LlmJob,
    priority: Priority,
    ready_at: Instant,
    shutdown_reason: &'static str,
) {
    if job.queued_byte_permit.is_some() {
        // A pre-provider cooldown still owns its original reservation. Keep
        // that exact permit through the delay; attempting to reserve again
        // would deadlock an exact-cap lane behind the job's own bytes.
        spawn_reserved_requeue_at(ctx, job, ready_at, shutdown_reason);
        return;
    }
    debug!(
        job_id = %job.job_id,
        ?priority,
        "retry waiting for retained-byte admission"
    );
    let queue = Arc::clone(&ctx.byte_waiters);
    queue.push(ParkedByteJob {
        task_cancel_token: ctx.task_state.subscribe_cancel(job.task_ref.as_ref()),
        job,
        ready_at,
        shutdown_reason,
    });
    queue.notify.notify_one();
    if queue
        .started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let ctx_for_task = Arc::clone(ctx);
        ctx.deferred_tasks
            .spawn(byte_admission_loop(ctx_for_task, queue));
    }
}

async fn byte_admission_loop(ctx: Arc<WorkerContext>, queue: Arc<QueueByteAdmissionWaiters>) {
    loop {
        for parked in take_cancelled_byte_waiters(&ctx, &queue) {
            tombstone_at_pickup(&ctx, parked.job, parked.reason, parked.log_reason).await;
        }
        if ctx.pending_shutdown.is_cancelled() {
            for parked in queue.drain() {
                shutdown_tombstone(&ctx, parked.job, parked.shutdown_reason).await;
            }
            return;
        }
        if queue.is_empty() {
            tokio::select! {
                _ = ctx.pending_shutdown.cancelled() => continue,
                _ = queue.notify.notified() => continue,
            }
        }

        // A full lane must not head-of-line block byte waiters whose own lane
        // still has capacity. Inspect at most one FIFO head from each lane in
        // the same bounded-priority order as provider/worker admission, and
        // advance fairness only after a reservation actually succeeds.
        let (admitted, permanently_oversized) = {
            let config = ctx.config.read();
            try_admit_byte_waiter(&queue, &ctx.queued_bytes, &config)
        };
        let made_progress = admitted.is_some() || !permanently_oversized.is_empty();
        if let Some(parked) = admitted {
            if Instant::now() < parked.ready_at {
                spawn_reserved_requeue_at(
                    &ctx,
                    parked.job,
                    parked.ready_at,
                    parked.shutdown_reason,
                );
            } else {
                send_byte_admitted_requeue(&ctx, parked.job).await;
            }
        }
        for oversized in permanently_oversized {
            terminal_fail(
                &ctx,
                oversized.parked.job,
                LLMError::RequestTooLarge {
                    bytes: oversized.bytes,
                    capacity_bytes: oversized.capacity_bytes,
                },
                ErrorClass::Unknown,
            )
            .await;
        }
        if made_progress {
            continue;
        }
        tokio::select! {
            _ = ctx.pending_shutdown.cancelled() => {},
            _ = queue.notify.notified() => {},
            _ = ctx.queued_bytes.released() => {},
            _ = sleep(Duration::from_millis(25)) => {},
        }
    }
}

fn spawn_reserved_requeue_at(
    ctx: &Arc<WorkerContext>,
    job: LlmJob,
    ready_at: Instant,
    shutdown_reason: &'static str,
) {
    let ctx_for_task = Arc::clone(ctx);
    ctx.deferred_tasks.spawn(async move {
        let task_cancel_token = ctx_for_task
            .task_state
            .subscribe_cancel(job.task_ref.as_ref());
        loop {
            if let Some(reason) = ctx_for_task.registry.take_cancellation_intent(&job.job_id) {
                tombstone_at_pickup(&ctx_for_task, job, reason, "cancelled_during_retry_backoff")
                    .await;
                return;
            }
            if task_cancel_token.is_cancelled() {
                tombstone_at_pickup(
                    &ctx_for_task,
                    job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_during_retry_backoff",
                )
                .await;
                return;
            }
            if job
                .submission_deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
            {
                tombstone_at_pickup(
                    &ctx_for_task,
                    job,
                    TombstoneReason::DeadlineExceeded,
                    "deadline_during_retry_backoff",
                )
                .await;
                return;
            }
            let remaining = ready_at.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            let poll_after = remaining.min(Duration::from_millis(25));
            if tokio::select! {
                _ = ctx_for_task.pending_shutdown.cancelled() => true,
                _ = sleep(poll_after) => false,
            } {
                shutdown_tombstone(&ctx_for_task, job, shutdown_reason).await;
                return;
            }
        }
        if ctx_for_task.pending_shutdown.is_cancelled() || ctx_for_task.shutdown.is_cancelled() {
            shutdown_tombstone(&ctx_for_task, job, shutdown_reason).await;
            return;
        }
        send_byte_admitted_requeue(&ctx_for_task, job).await;
    });
}

async fn send_byte_admitted_requeue(ctx: &Arc<WorkerContext>, job: LlmJob) {
    if ctx.pending_shutdown.is_cancelled() || ctx.shutdown.is_cancelled() {
        shutdown_tombstone(ctx, job, "shutdown_after_byte_capacity").await;
        return;
    }
    let sender = ctx.lane_sender(job.priority).clone();
    match sender.try_push(job) {
        Ok(()) => {},
        Err(TryPushError::Closed(returned)) => {
            shutdown_tombstone(ctx, returned, "lane_closed_after_byte_capacity").await;
        },
        Err(TryPushError::Full(returned)) => {
            tombstone_at_pickup(
                ctx,
                returned,
                TombstoneReason::QueueFull,
                "admission_invariant_byte_waiter_lane_full",
            )
            .await;
        },
    }
}

struct CancelledByteWaiter {
    job: LlmJob,
    reason: TombstoneReason,
    log_reason: &'static str,
}

fn take_cancelled_byte_waiters(
    ctx: &Arc<WorkerContext>,
    queue: &Arc<QueueByteAdmissionWaiters>,
) -> Vec<CancelledByteWaiter> {
    let mut lanes = queue.lanes.lock();
    let mut retained = ByteWaitLanes {
        fairness: std::mem::take(&mut lanes.fairness),
        ..ByteWaitLanes::default()
    };
    let mut cancelled = Vec::new();
    for parked in lanes.drain() {
        if let Some(reason) = byte_waiter_tombstone_reason(ctx, &parked) {
            let log_reason = if matches!(reason, TombstoneReason::DeadlineExceeded) {
                "deadline_waiting_for_retry_byte_capacity"
            } else if parked.task_cancel_token.is_cancelled() {
                "task_cancelled_waiting_for_retry_byte_capacity"
            } else {
                "external_cancel_waiting_for_retry_byte_capacity"
            };
            cancelled.push(CancelledByteWaiter {
                job: parked.job,
                reason,
                log_reason,
            });
        } else {
            retained.push_back(parked);
        }
    }
    *lanes = retained;
    cancelled
}

fn byte_waiter_tombstone_reason(
    ctx: &Arc<WorkerContext>,
    parked: &ParkedByteJob,
) -> Option<TombstoneReason> {
    if let Some(reason) = ctx.registry.take_cancellation_intent(&parked.job.job_id) {
        return Some(reason);
    }
    if parked.task_cancel_token.is_cancelled() {
        return Some(TombstoneReason::TaskCancelledInFlight { reason: None });
    }
    parked
        .job
        .submission_deadline
        .filter(|deadline| Instant::now() >= *deadline)
        .map(|_| TombstoneReason::DeadlineExceeded)
}

fn local_prep_in_progress_should_abort(
    ctx: &Arc<WorkerContext>,
    job_id: &JobId,
    task_cancel_token: &CancellationToken,
    submission_deadline: Option<Instant>,
) -> bool {
    ctx.registry.has_cancellation_intent(job_id)
        || task_cancel_token.is_cancelled()
        || submission_deadline.is_some_and(|deadline| Instant::now() >= deadline)
}

fn local_prep_waiter_tombstone_reason(
    ctx: &Arc<WorkerContext>,
    parked: &ParkedLocalPrepJob,
) -> Option<TombstoneReason> {
    if let Some(reason) = ctx.registry.take_cancellation_intent(&parked.job.job_id) {
        return Some(reason);
    }
    if parked.task_cancel_token.is_cancelled() {
        return Some(TombstoneReason::TaskCancelledInFlight { reason: None });
    }
    if parked
        .job
        .submission_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Some(TombstoneReason::DeadlineExceeded);
    }
    None
}

/// Release the global worker while a saturated provider is at capacity. A
/// single provider coordinator owns all waiters for that provider and resumes
/// them in bounded priority order.
fn defer_until_provider_capacity(
    ctx: &Arc<WorkerContext>,
    job: LlmJob,
    provider: LLMProviderKind,
    concurrency: Arc<tokio::sync::Semaphore>,
    task_cancel_token: CancellationToken,
) {
    let job_id = job.job_id.clone();
    let total = job.attempts.total;
    let dispatched_meta = ctx.registry.in_flight_to_pending(&job_id, |meta| {
        meta.state = JobState::WaitingForProvider;
        meta.attempts = total;
    });
    if let Some(meta) = dispatched_meta {
        ctx.events.emit(LlmQueueEvent::WaitingForProvider {
            meta,
            provider: provider.clone(),
        });
    }
    let queue = ctx.provider_waiters.get_or_insert(&provider, concurrency);
    queue.push(ParkedProviderJob {
        job,
        task_cancel_token,
        wait_started: Instant::now(),
    });
    queue.notify.notify_one();
    if queue
        .started
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        let ctx_for_task = ctx.clone();
        ctx.deferred_tasks
            .spawn(provider_admission_loop(ctx_for_task, queue));
    }
}

async fn provider_admission_loop(ctx: Arc<WorkerContext>, queue: Arc<ProviderWaitQueue>) {
    loop {
        for parked in take_cancelled_provider_waiters(&ctx, &queue) {
            tombstone_at_pickup(&ctx, parked.job, parked.reason, parked.log_reason).await;
        }

        if ctx.pending_shutdown.is_cancelled() {
            for parked in queue.drain() {
                shutdown_tombstone(&ctx, parked.job, "shutdown_waiting_for_provider_capacity")
                    .await;
            }
            return;
        }
        if queue.lengths().total() == 0 {
            tokio::select! {
                _ = ctx.pending_shutdown.cancelled() => continue,
                _ = queue.notify.notified() => continue,
            }
        }

        // Re-evaluate the priority queues whenever a new waiter/cancellation
        // arrives. The short heartbeat makes task-token cancellation and
        // caller deadlines observable without creating a waiter task per job.
        let permit = tokio::select! {
            biased;
            _ = ctx.pending_shutdown.cancelled() => continue,
            _ = queue.notify.notified() => continue,
            _ = sleep(Duration::from_millis(50)) => continue,
            permit = queue.concurrency.clone().acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    for parked in queue.drain() {
                        shutdown_tombstone(&ctx, parked.job, "provider_semaphore_closed").await;
                    }
                    return;
                },
            },
        };

        let Some(parked) = queue.pop_next() else {
            drop(permit);
            continue;
        };
        if let Some(reason) = provider_waiter_tombstone_reason(&ctx, &parked) {
            drop(permit);
            tombstone_at_pickup(
                &ctx,
                parked.job,
                reason,
                "cancelled_after_provider_capacity",
            )
            .await;
            continue;
        }

        let job_id = parked.job.job_id.clone();
        let provider_wait_ms = parked
            .wait_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(mut meta) = ctx.registry.pending.get_mut(&job_id) {
            meta.state = JobState::Pending;
            meta.provider_wait_ms = Some(
                meta.provider_wait_ms
                    .unwrap_or_default()
                    .saturating_add(provider_wait_ms),
            );
        }
        let sender = ctx.lane_sender(parked.job.priority).clone();
        // Preserve the coordinator's priority decision for foreground work by
        // transferring its provider-bound permit to the resumed job. Without
        // this handoff, high and background jobs race for the semaphore again
        // and can execute in the opposite order from `pop_next`.
        //
        // Background remains deliberately unreserved. It may be waiting in a
        // lane whose only background-capable worker is busy on another
        // provider; retaining this provider's permit there would block a newer
        // interactive request even though an interactive worker is ready.
        if parked.job.priority == Priority::Background {
            drop(permit);
        } else {
            ctx.registry
                .store_capacity_permit(&job_id, queue.provider.clone(), permit);
        }
        if ctx.pending_shutdown.is_cancelled() || ctx.shutdown.is_cancelled() {
            shutdown_tombstone(&ctx, parked.job, "shutdown_after_provider_capacity").await;
            continue;
        }
        match sender.try_push(parked.job) {
            Ok(()) => {},
            Err(TryPushError::Closed(returned)) => {
                shutdown_tombstone(&ctx, returned, "lane_closed_after_provider_capacity").await;
            },
            Err(TryPushError::Full(returned)) => {
                tombstone_at_pickup(
                    &ctx,
                    returned,
                    TombstoneReason::QueueFull,
                    "admission_invariant_provider_lane_full",
                )
                .await;
            },
        }
    }
}

struct CancelledProviderWaiter {
    job: LlmJob,
    reason: TombstoneReason,
    log_reason: &'static str,
}

fn take_cancelled_provider_waiters(
    ctx: &Arc<WorkerContext>,
    queue: &Arc<ProviderWaitQueue>,
) -> Vec<CancelledProviderWaiter> {
    let mut lanes = queue.lanes.lock();
    let mut retained = ProviderWaitLanes {
        fairness: std::mem::take(&mut lanes.fairness),
        ..ProviderWaitLanes::default()
    };
    let mut cancelled = Vec::new();
    for parked in lanes.drain() {
        if let Some(reason) = provider_waiter_tombstone_reason(ctx, &parked) {
            let log_reason = if matches!(reason, TombstoneReason::DeadlineExceeded) {
                "deadline_waiting_for_provider_capacity"
            } else if parked.task_cancel_token.is_cancelled() {
                "task_cancelled_waiting_for_provider_capacity"
            } else {
                "external_cancel_waiting_for_provider_capacity"
            };
            cancelled.push(CancelledProviderWaiter {
                job: parked.job,
                reason,
                log_reason,
            });
        } else {
            match parked.job.priority {
                Priority::High => retained.high.push_back(parked),
                Priority::Normal => retained.normal.push_back(parked),
                Priority::Background => retained.background.push_back(parked),
            }
        }
    }
    *lanes = retained;
    cancelled
}

fn provider_waiter_tombstone_reason(
    ctx: &Arc<WorkerContext>,
    parked: &ParkedProviderJob,
) -> Option<TombstoneReason> {
    if let Some(reason) = ctx.registry.take_cancellation_intent(&parked.job.job_id) {
        return Some(reason);
    }
    if parked.task_cancel_token.is_cancelled() {
        return Some(TombstoneReason::TaskCancelledInFlight { reason: None });
    }
    if parked
        .job
        .submission_deadline
        .is_some_and(|deadline| Instant::now() >= deadline)
    {
        return Some(TombstoneReason::DeadlineExceeded);
    }
    None
}

/// Release the global worker while local-prep would block on the serial
/// Ollama generate daemon. One coordinator task holds at most one generate.
fn defer_until_local_prep(
    ctx: &Arc<WorkerContext>,
    job: LlmJob,
    task_cancel_token: CancellationToken,
) {
    let job_id = job.job_id.clone();
    let total = job.attempts.total;
    let dispatched_meta = ctx.registry.in_flight_to_pending(&job_id, |meta| {
        meta.state = JobState::WaitingForLocalPrep;
        meta.attempts = total;
    });
    if let Some(meta) = dispatched_meta {
        ctx.events.emit(LlmQueueEvent::WaitingForLocalPrep { meta });
    }
    ctx.local_prep.push(ParkedLocalPrepJob {
        job,
        task_cancel_token,
        wait_started: Instant::now(),
    });
    ctx.local_prep.notify.notify_one();
    if ctx.local_prep.claim_start() {
        let ctx_for_task = ctx.clone();
        ctx.deferred_tasks
            .spawn(local_prep_admission_loop(ctx_for_task));
    }
}

async fn local_prep_admission_loop(ctx: Arc<WorkerContext>) {
    loop {
        for parked in take_cancelled_local_prep_waiters(&ctx) {
            tombstone_at_pickup(&ctx, parked.job, parked.reason, parked.log_reason).await;
        }

        if ctx.pending_shutdown.is_cancelled() {
            for parked in ctx.local_prep.drain() {
                shutdown_tombstone(&ctx, parked.job, "shutdown_waiting_for_local_prep").await;
            }
            // Stay alive through graceful drain so a worker that parks after
            // this drain still gets tombstoned. Exit only on force-shutdown.
            if ctx.shutdown.is_cancelled() {
                for parked in ctx.local_prep.drain() {
                    shutdown_tombstone(&ctx, parked.job, "shutdown_waiting_for_local_prep").await;
                }
                return;
            }
            tokio::select! {
                _ = ctx.shutdown.cancelled() => continue,
                _ = ctx.local_prep.notify.notified() => continue,
            }
        }
        if ctx.local_prep.lane_waiting_count() == 0 {
            tokio::select! {
                _ = ctx.pending_shutdown.cancelled() => continue,
                _ = ctx.local_prep.notify.notified() => continue,
            }
        }

        let concurrency = ctx.local_prep.concurrency();
        let permit = tokio::select! {
            biased;
            _ = ctx.pending_shutdown.cancelled() => continue,
            _ = ctx.local_prep.notify.notified() => continue,
            _ = sleep(Duration::from_millis(50)) => continue,
            permit = concurrency.acquire_owned() => match permit {
                Ok(permit) => permit,
                Err(_) => {
                    for parked in ctx.local_prep.drain() {
                        shutdown_tombstone(&ctx, parked.job, "local_prep_semaphore_closed").await;
                    }
                    return;
                },
            },
        };

        let Some(mut parked) = ctx.local_prep.pop_next() else {
            drop(permit);
            continue;
        };
        ctx.local_prep.begin_prep();
        let _in_flight_prep = LocalPrepInFlight(&ctx.local_prep);
        if let Some(reason) = local_prep_waiter_tombstone_reason(&ctx, &parked) {
            drop(permit);
            tombstone_at_pickup(
                &ctx,
                parked.job,
                reason,
                "cancelled_after_local_prep_admission",
            )
            .await;
            continue;
        }

        let local_prep_cfg = ctx.config.read().local_prep.clone();
        // Drain sibling waiters (cancel/deadline/shutdown) while this job is
        // inside the serial generate; otherwise a cap-1 summarise would HOL
        // block cancellation of everyone behind it.
        let (stat, cancellation) = {
            let shutdown = ctx.shutdown.clone();
            let task_cancel = parked.task_cancel_token.clone();
            let in_progress_id = parked.job.job_id.clone();
            let in_progress_deadline = parked.job.submission_deadline;
            let prep_fut = maybe_local_prep_direct_cancellable(
                &mut parked.job.request,
                &local_prep_cfg,
                Some(&task_cancel),
                Some(&shutdown),
            );
            tokio::pin!(prep_fut);
            loop {
                if local_prep_in_progress_should_abort(
                    &ctx,
                    &in_progress_id,
                    &task_cancel,
                    in_progress_deadline,
                ) {
                    task_cancel.cancel();
                }
                for waiter in take_cancelled_local_prep_waiters(&ctx) {
                    tombstone_at_pickup(&ctx, waiter.job, waiter.reason, waiter.log_reason).await;
                }
                if ctx.pending_shutdown.is_cancelled() {
                    for waiter in ctx.local_prep.drain() {
                        shutdown_tombstone(&ctx, waiter.job, "shutdown_waiting_for_local_prep")
                            .await;
                    }
                    // `pending_shutdown` stays cancelled; do not select on it
                    // or this loop becomes a busy poll of prep_fut.
                    tokio::select! {
                        biased;
                        result = &mut prep_fut => break result,
                        _ = ctx.local_prep.notify.notified() => {},
                        _ = sleep(Duration::from_millis(50)) => {},
                    }
                } else {
                    tokio::select! {
                        biased;
                        result = &mut prep_fut => break result,
                        _ = ctx.pending_shutdown.cancelled() => {},
                        _ = ctx.local_prep.notify.notified() => {},
                        _ = sleep(Duration::from_millis(50)) => {},
                    }
                }
            }
        };
        drop(permit);

        if let Some(cancellation) = cancellation {
            let _ = ctx.registry.take_cancellation_intent(&parked.job.job_id);
            let (reason, log_reason) = match cancellation {
                LocalPrepCancellation::Task => (
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_during_local_prep",
                ),
                LocalPrepCancellation::QueueShutdown => (
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_during_local_prep",
                ),
            };
            tombstone_at_pickup(&ctx, parked.job, reason, log_reason).await;
            continue;
        }
        if ctx.pending_shutdown.is_cancelled() {
            shutdown_tombstone(&ctx, parked.job, "shutdown_waiting_for_local_prep").await;
            continue;
        }
        if let Some(reason) = local_prep_waiter_tombstone_reason(&ctx, &parked) {
            tombstone_at_pickup(&ctx, parked.job, reason, "cancelled_during_local_prep").await;
            continue;
        }

        parked.job.local_prep_done = true;
        parked.job.local_prep_stat = stat;
        let job_id = parked.job.job_id.clone();
        let local_prep_wait_ms = parked
            .wait_started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        if let Some(mut meta) = ctx.registry.pending.get_mut(&job_id) {
            meta.state = JobState::Pending;
            debug!(
                job_id = %job_id,
                local_prep_wait_ms,
                "local-prep coordinator requeue"
            );
        }
        let sender = ctx.lane_sender(parked.job.priority).clone();
        // Prep can last tens of seconds; the lane may fill with new submits.
        // Blocking for a slot preserves an already-admitted job instead of
        // tombstoning it as QueueFull. Shutdown closes the lane so `push`
        // returns the job instead of racing `take()` across `select!` arms.
        if ctx.pending_shutdown.is_cancelled() || ctx.shutdown.is_cancelled() {
            shutdown_tombstone(&ctx, parked.job, "shutdown_waiting_for_local_prep").await;
            continue;
        }
        match sender.push(parked.job).await {
            Ok(()) => {},
            Err(returned) => {
                shutdown_tombstone(&ctx, returned, "lane_closed_after_local_prep").await;
            },
        }
    }
}

struct LocalPrepInFlight<'a>(&'a LocalPrepCoordinator);

impl Drop for LocalPrepInFlight<'_> {
    fn drop(&mut self) {
        self.0.end_prep();
    }
}

struct CancelledLocalPrepWaiter {
    job: LlmJob,
    reason: TombstoneReason,
    log_reason: &'static str,
}

fn take_cancelled_local_prep_waiters(ctx: &Arc<WorkerContext>) -> Vec<CancelledLocalPrepWaiter> {
    ctx.local_prep
        .take_where(|parked| {
            local_prep_waiter_tombstone_reason(ctx, parked).map(|reason| {
                let log_reason = if matches!(reason, TombstoneReason::DeadlineExceeded) {
                    "deadline_waiting_for_local_prep"
                } else if parked.task_cancel_token.is_cancelled() {
                    "task_cancelled_waiting_for_local_prep"
                } else {
                    "external_cancel_waiting_for_local_prep"
                };
                (reason, log_reason)
            })
        })
        .into_iter()
        .map(|(parked, (reason, log_reason))| CancelledLocalPrepWaiter {
            job: parked.job,
            reason,
            log_reason,
        })
        .collect()
}

async fn shutdown_tombstone(ctx: &Arc<WorkerContext>, mut job: LlmJob, log_reason: &str) {
    info!(job_id = %job.job_id, log_reason, "tombstoning during shutdown");
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let meta = ctx.registry.pending_to_tombstone(&job_id, |m| {
        m.state = JobState::Tombstoned;
        m.tombstone = Some(TombstoneReason::QueueShutdown);
        m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
    });
    ctx.metrics.incr_tombstoned();
    if let Some(meta) = meta {
        ctx.events.emit(LlmQueueEvent::Tombstoned {
            meta,
            reason: TombstoneReason::QueueShutdown,
        });
    }
    let task_ref = job.task_ref.clone();
    let attempts_so_far = job.attempts.total;
    if let Some(key) = job.idempotency_key.as_deref() {
        ctx.inflight_index.resolve(
            key,
            &job_id,
            Arc::new(Err(LLMError::Cancelled {
                reason: TombstoneReason::QueueShutdown.name().to_string(),
            })),
        );
    }
    let _ = job.response_tx.send(Err(LLMError::Cancelled {
        reason: TombstoneReason::QueueShutdown.name().to_string(),
    }));
    if let Some(task_ref) = task_ref.as_ref() {
        // Preserve the durable tombstone during the graceful phase, but never
        // let a blocked sink keep terminal ownership or the shutdown response
        // hidden. Force shutdown still cancels the auxiliary append.
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Tombstoned {
                    job_id: job_id.clone(),
                    reason: TombstoneReason::QueueShutdown,
                    attempts_so_far,
                },
            ) => {},
        }
    }
}

fn emit_with_meta<F>(ctx: &Arc<WorkerContext>, job_id: &JobId, build: F)
where
    F: FnOnce(JobMeta) -> LlmQueueEvent,
{
    if let Some(meta) = ctx.registry.find(job_id) {
        ctx.events.emit(build(meta));
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        // Provider messages are arbitrary UTF-8. Byte slicing at `n` can
        // panic while handling the very error we are trying to persist,
        // killing a dispatch worker and losing its terminal lifecycle.
        let mut end = n.min(s.len());
        while !s.is_char_boundary(end) {
            end = end.saturating_sub(1);
        }
        let mut out = s[..end].to_string();
        out.push_str("…");
        out
    }
}

/// Spawn a worker on the tokio runtime. The pool supervisor in `queue.rs`
/// calls `worker_loop` directly inside its own spawn to surface panics; this
/// helper is retained as a convenience for downstream callers that want a
/// simple one-shot spawn without the supervisor wrapper.
#[allow(dead_code)]
pub(crate) fn spawn_worker(
    worker_id: usize,
    ctx: Arc<WorkerContext>,
    high: FairLane,
    normal: FairLane,
    background: FairLane,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        worker_loop(worker_id, ctx, high, normal, background).await;
    })
}

#[cfg(test)]
mod fairness_tests {
    use super::*;
    use crate::dispatch::types::{JobOrigin, TaskRef};
    use crate::types::LlmRouteIdentity;

    fn job(priority: Priority) -> LlmJob {
        let (job, _rx) = LlmJob::new(LLMRequest::default(), JobOrigin::op("fairness-test"));
        job.with_priority(priority)
    }

    fn owned_job(priority: Priority, agent: &str) -> LlmJob {
        job(priority).with_task(TaskRef::task(format!("task-{agent}")).with_agent(agent))
    }

    #[test]
    fn terminal_error_truncation_is_utf8_safe() {
        assert_eq!(truncate("éé", 1), "…");
        assert_eq!(truncate("éé", 3), "é…");
        assert_eq!(truncate("plain", 5), "plain");
    }

    #[test]
    fn high_is_preferred_over_normal() {
        let high = FairLane::new(8);
        let normal = FairLane::new(8);
        let background = FairLane::new(8);
        assert!(normal.try_push(job(Priority::Normal)).is_ok());
        assert!(high.try_push(job(Priority::High)).is_ok());

        let mut fairness = LaneFairness::default();
        let first =
            try_pick_ready_job(&mut fairness, true, &high, &normal, &background).expect("high job");
        assert_eq!(first.priority, Priority::High);
        let second = try_pick_ready_job(&mut fairness, true, &high, &normal, &background)
            .expect("normal job");
        assert_eq!(second.priority, Priority::Normal);
    }

    #[test]
    fn ready_lanes_preserve_priority_with_bounded_lower_lane_service() {
        let high = FairLane::new(32);
        let normal = FairLane::new(8);
        let background = FairLane::new(8);
        for _ in 0..20 {
            assert!(high.try_push(job(Priority::High)).is_ok());
        }
        assert!(normal.try_push(job(Priority::Normal)).is_ok());
        assert!(background.try_push(job(Priority::Background)).is_ok());

        let mut fairness = LaneFairness::default();
        let priorities = (0..13)
            .map(|_| {
                try_pick_ready_job(&mut fairness, true, &high, &normal, &background)
                    .expect("a queued job")
                    .priority
            })
            .collect::<Vec<_>>();

        assert!(priorities[..8].iter().all(|p| *p == Priority::High));
        assert_eq!(priorities[8], Priority::Normal);
        assert_eq!(priorities[12], Priority::Background);
    }

    #[test]
    fn exhausted_burst_does_not_delay_the_only_ready_lane() {
        let high = FairLane::new(32);
        let normal = FairLane::new(8);
        let background = FairLane::new(8);
        for _ in 0..20 {
            assert!(high.try_push(job(Priority::High)).is_ok());
        }

        let mut fairness = LaneFairness::default();
        for _ in 0..20 {
            let priority = try_pick_ready_job(&mut fairness, true, &high, &normal, &background)
                .expect("high-priority job remains ready")
                .priority;
            assert_eq!(priority, Priority::High);
        }
    }

    #[test]
    fn interactive_only_worker_never_drains_background_lane() {
        let high = FairLane::new(2);
        let normal = FairLane::new(2);
        let background = FairLane::new(2);
        assert!(background.try_push(job(Priority::Background)).is_ok());

        let mut fairness = LaneFairness::default();
        assert!(try_pick_ready_job(&mut fairness, false, &high, &normal, &background).is_none());
        assert_eq!(background.len(), 1);
    }

    #[test]
    fn ready_lane_round_robins_owners() {
        let high = FairLane::new(8);
        let normal = FairLane::new(8);
        let background = FairLane::new(8);
        assert!(high.try_push(owned_job(Priority::High, "A")).is_ok());
        assert!(high.try_push(owned_job(Priority::High, "A")).is_ok());
        assert!(high.try_push(owned_job(Priority::High, "B")).is_ok());

        let mut fairness = LaneFairness::default();
        let owners = (0..3)
            .map(|_| {
                try_pick_ready_job(&mut fairness, true, &high, &normal, &background)
                    .expect("queued high job")
                    .task_ref
                    .expect("owner")
                    .agent_id
                    .expect("agent")
            })
            .collect::<Vec<_>>();
        assert_eq!(owners, ["A", "B", "A"]);
    }

    #[test]
    fn full_high_byte_lane_does_not_block_an_admissible_normal_retry() {
        let queue = QueueByteAdmissionWaiters::new();
        let counters = Arc::new(QueueByteCounters::default());
        let high = job(Priority::High);
        let request_bytes = high.request.estimated_retained_bytes() as u64;
        let normal = job(Priority::Normal);
        assert_eq!(
            normal.request.estimated_retained_bytes() as u64,
            request_bytes
        );
        let global_capacity = request_bytes.saturating_mul(3);
        let occupied_high = counters
            .try_reserve(
                Priority::High,
                request_bytes,
                request_bytes,
                global_capacity,
            )
            .expect("fixture fills only the high lane");
        for parked in [high, normal] {
            queue.push(ParkedByteJob {
                job: parked,
                task_cancel_token: CancellationToken::new(),
                ready_at: Instant::now(),
                shutdown_reason: "test",
            });
        }
        let mut config = DispatchConfig::default();
        config.max_request_bytes = request_bytes;
        config.queue_bytes_high = request_bytes;
        config.queue_bytes_normal = request_bytes;
        config.queue_bytes_background = request_bytes;
        config.queue_bytes_global = global_capacity;

        let (admitted, oversized) = try_admit_byte_waiter(&queue, &counters, &config);
        assert!(oversized.is_empty());
        let admitted = admitted.expect("normal retry bypasses the full high byte lane");
        assert_eq!(admitted.job.priority, Priority::Normal);
        assert_eq!(queue.waiting_count(), 1, "high FIFO head remains parked");
        drop(admitted);
        drop(occupied_high);
        assert_eq!(counters.global_bytes(), 0);
    }

    #[test]
    fn retry_made_impossible_by_a_smaller_hot_ceiling_is_not_parked_forever() {
        let queue = QueueByteAdmissionWaiters::new();
        let counters = Arc::new(QueueByteCounters::default());
        let parked_job = job(Priority::Normal);
        let request_bytes = parked_job.request.estimated_retained_bytes() as u64;
        queue.push(ParkedByteJob {
            job: parked_job,
            task_cancel_token: CancellationToken::new(),
            ready_at: Instant::now(),
            shutdown_reason: "test",
        });
        let mut config = DispatchConfig::default();
        config.max_request_bytes = request_bytes.saturating_sub(1);

        let (admitted, oversized) = try_admit_byte_waiter(&queue, &counters, &config);
        assert!(admitted.is_none());
        assert_eq!(oversized.len(), 1);
        assert_eq!(oversized[0].bytes, request_bytes);
        assert_eq!(queue.waiting_count(), 0);
        assert_eq!(counters.global_bytes(), 0);
    }

    #[test]
    fn outcome_provider_state_follows_the_effective_fallback_route() {
        let initial = LLMProviderKind::OpenAI;
        let fallback = LLMProviderKind::Anthropic;
        let success: LLMResult<LLMResponse> = Ok(LLMResponse {
            route_identity: Some(LlmRouteIdentity {
                profile: "fallback".to_string(),
                provider: fallback.clone(),
                model: "fallback-model".to_string(),
            }),
            ..LLMResponse::default()
        });
        let failure: LLMResult<LLMResponse> = Err(LLMError::Provider {
            provider: fallback.to_string(),
            message: "failed".to_string(),
        }
        .with_route("fallback", fallback.clone(), "fallback-model"));
        let pre_provider: LLMResult<LLMResponse> =
            Err(LLMError::Configuration("missing key".to_string()));

        assert_eq!(outcome_provider_kind(&success, &initial), fallback);
        assert_eq!(outcome_provider_kind(&failure, &initial), fallback);
        assert_eq!(outcome_provider_kind(&pre_provider, &initial), initial);
    }
}

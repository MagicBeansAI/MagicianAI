//! Public facade — `LlmDispatchQueue`. Construct once at boot, share
//! `Arc<LlmDispatchQueue>` with all callsites.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use super::router_handle::DispatchRouter;
use crate::error::{LLMError, LLMResult};
use crate::types::LLMRequest;

use super::cancellation::TaskStateView;
use super::cloud_admission::CloudAdmission;
use super::config::DispatchConfig;
use super::events::{EventBus, LlmQueueEvent};
use super::fair_lane::{FairLane, TryPushError};
use super::inflight_index::{InflightIndex, LookupOutcome};
use super::job::{DispatchedResponse, JobMeta, LlmJob, LlmStreamJob, QueueByteCounters};
use super::ledger::{LlmCallLedgerEvent, TaskLedgerSink};
use super::local_prep_coordinator::LocalPrepCoordinator;
use super::metrics::DispatchMetrics;
use super::provider_state::ProviderStateMap;
use super::quota::ProviderQuotaMap;
use super::registry::{JobRegistry, RegistrySnapshot};
use super::streaming::{dispatch_stream_job, StreamingContext};
use super::types::{JobId, JobOrigin, JobState, Priority, TombstoneReason};
use super::worker::{
    worker_loop_with_lane_policy, ProviderAdmissionQueues, QueueByteAdmissionWaiters, WorkerContext,
};

/// Process-wide LLM dispatch queue. Clone-cheap (`Arc` inside).
#[derive(Clone)]
pub struct LlmDispatchQueue {
    inner: Arc<Inner>,
}

struct Inner {
    lane_high: FairLane,
    lane_normal: FairLane,
    lane_background: FairLane,
    admission_high: Arc<Semaphore>,
    admission_normal: Arc<Semaphore>,
    admission_background: Arc<Semaphore>,
    queued_bytes: Arc<QueueByteCounters>,
    config: Arc<RwLock<DispatchConfig>>,
    registry: Arc<JobRegistry>,
    // Retained for future direct exposure via the snapshot endpoint (per-
    // provider cooldown / breaker state). Workers already hold their own
    // clone via `WorkerContext.provider_state`.
    #[allow(dead_code)]
    provider_state: Arc<ProviderStateMap>,
    inflight_index: Arc<InflightIndex>,
    events: EventBus,
    metrics: DispatchMetrics,
    streaming_ctx: Arc<StreamingContext>,
    worker_ctx: Arc<WorkerContext>,
    workers: parking_lot::Mutex<Vec<JoinHandle<()>>>,
    stream_tasks: parking_lot::Mutex<Vec<JoinHandle<()>>>,
    /// Serializes the final accepting check with enqueue/spawn so shutdown
    /// cannot drain the lanes and then lose a late accepted job.
    admission: parking_lot::Mutex<()>,
    pending_shutdown: CancellationToken,
    shutdown: CancellationToken,
    accepting: AtomicBool,
}

/// Cancellation-safe ownership for the gap between idempotency registration
/// and lane publication. Once the lane accepts the job, the worker becomes the
/// terminal owner and this guard is disarmed. If the submit future is dropped
/// while awaiting durable ledger work, subscribers are woken and the key is
/// immediately reusable rather than remaining in-flight forever.
struct InflightSubmissionGuard {
    index: Arc<InflightIndex>,
    key: Option<String>,
    job_id: JobId,
}

impl InflightSubmissionGuard {
    fn new(index: Arc<InflightIndex>, key: String, job_id: JobId) -> Self {
        Self {
            index,
            key: Some(key),
            job_id,
        }
    }

    fn disarm(&mut self) {
        self.key = None;
    }
}

impl Drop for InflightSubmissionGuard {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        self.index.abandon(
            &key,
            &self.job_id,
            Arc::new(Err(LLMError::Cancelled {
                reason: "idempotent_owner_submission_cancelled".to_string(),
            })),
        );
    }
}

impl LlmDispatchQueue {
    /// Construct + start the queue with the supplied dependencies.
    pub fn start(
        router: Arc<dyn DispatchRouter>,
        task_state: Arc<dyn TaskStateView>,
        ledger_sink: Arc<dyn TaskLedgerSink>,
        config: DispatchConfig,
    ) -> Arc<Self> {
        let engine = config.engine;
        let global_cloud_concurrency = config.global_cloud_concurrency;
        let provider_concurrency = config.provider_concurrency.clone();
        let breaker_config = config.breaker.clone();
        let completed_cap = config.completed_ring_capacity;
        let tombstone_cap = config.tombstone_ring_capacity;
        let workers_n = config.workers.max(1);
        let reserved_interactive_workers = config
            .reserved_interactive_workers
            .min(workers_n.saturating_sub(1));
        let background_workers = workers_n
            .saturating_sub(reserved_interactive_workers)
            .max(1);
        let cap_high = config.queue_capacity_high.max(1);
        let cap_normal = config.queue_capacity_normal.max(1);
        let cap_background = config.queue_capacity_background.max(1);
        let idem_window = Duration::from_secs(config.idempotency_window_secs.max(1));
        let provider_quota = ProviderQuotaMap::from_config(&config.provider_quota);

        let config_handle = Arc::new(RwLock::new(config));
        let provider_state = Arc::new(ProviderStateMap::new(provider_concurrency, breaker_config));
        let registry = JobRegistry::new(completed_cap, tombstone_cap, tombstone_cap);
        let inflight_index = Arc::new(InflightIndex::new(idem_window));
        let events = EventBus::new(1024);
        let metrics = DispatchMetrics::new();
        let pending_shutdown = CancellationToken::new();
        let shutdown = CancellationToken::new();
        let deferred_tasks = TaskTracker::new();
        let provider_waiters = ProviderAdmissionQueues::new();
        let byte_waiters = QueueByteAdmissionWaiters::new();
        let admission_high = Arc::new(Semaphore::new(cap_high));
        let admission_normal = Arc::new(Semaphore::new(cap_normal));
        let admission_background = Arc::new(Semaphore::new(cap_background));
        let queued_bytes = Arc::new(QueueByteCounters::default());

        let lane_high = FairLane::new(cap_high);
        let lane_normal = FairLane::new(cap_normal);
        let lane_background = FairLane::new(cap_background);

        let cloud_admission = CloudAdmission::new(global_cloud_concurrency);

        let worker_ctx = Arc::new(WorkerContext {
            router: router.clone(),
            registry: registry.clone(),
            provider_state: provider_state.clone(),
            inflight_index: inflight_index.clone(),
            task_state: task_state.clone(),
            ledger_sink: ledger_sink.clone(),
            events: events.clone(),
            metrics: metrics.clone(),
            config: config_handle.clone(),
            pending_shutdown: pending_shutdown.clone(),
            shutdown: shutdown.clone(),
            deferred_tasks,
            provider_waiters,
            byte_waiters,
            local_prep: LocalPrepCoordinator::new(),
            cloud_admission: cloud_admission.clone(),
            provider_quota: provider_quota.clone(),
            engine,
            isolated_executors: parking_lot::Mutex::new(Vec::new()),
            queued_bytes: queued_bytes.clone(),
            lane_high: lane_high.clone(),
            lane_normal: lane_normal.clone(),
            lane_background: lane_background.clone(),
        });

        let streaming_ctx = Arc::new(StreamingContext {
            router: router.clone(),
            registry: registry.clone(),
            provider_state: provider_state.clone(),
            task_state: task_state.clone(),
            ledger_sink: ledger_sink.clone(),
            events: events.clone(),
            metrics: metrics.clone(),
            config: config_handle.clone(),
            pending_shutdown: pending_shutdown.clone(),
            shutdown: shutdown.clone(),
            provider_quota: provider_quota.clone(),
            cloud_admission: cloud_admission.clone(),
        });

        let inner = Arc::new(Inner {
            lane_high: lane_high.clone(),
            lane_normal: lane_normal.clone(),
            lane_background: lane_background.clone(),
            admission_high,
            admission_normal,
            admission_background,
            queued_bytes,
            config: config_handle,
            registry,
            provider_state,
            inflight_index,
            events,
            metrics,
            streaming_ctx,
            worker_ctx,
            workers: parking_lot::Mutex::new(Vec::new()),
            stream_tasks: parking_lot::Mutex::new(Vec::new()),
            admission: parking_lot::Mutex::new(()),
            pending_shutdown,
            shutdown,
            accepting: AtomicBool::new(true),
        });

        // Spawn worker pool with supervisor that respawns on panic / exit.
        let queue = Arc::new(Self { inner });
        info!(
            engine = engine.as_str(),
            workers = workers_n,
            reserved_interactive_workers,
            global_cloud_concurrency,
            "dispatch queue started"
        );
        for worker_id in 0..workers_n {
            let allow_background = worker_id < background_workers;
            queue.clone().spawn_worker_supervised(
                worker_id,
                allow_background,
                lane_high.clone(),
                lane_normal.clone(),
                lane_background.clone(),
            );
        }

        queue
    }

    fn spawn_worker_supervised(
        self: Arc<Self>,
        worker_id: usize,
        allow_background: bool,
        high: FairLane,
        normal: FairLane,
        background: FairLane,
    ) {
        let ctx = self.inner.worker_ctx.clone();
        let queue = self.clone();
        // The supervisor task lives for the worker's lifetime. Each iteration
        // spawns a fresh worker task (running `worker_loop` directly so the
        // inner `JoinHandle::await` actually surfaces panics — previous
        // double-spawn pattern hid them inside the outer task's Ok value).
        let handle = tokio::spawn(async move {
            loop {
                let high = high.clone();
                let normal = normal.clone();
                let background = background.clone();
                let ctx_inner = ctx.clone();
                let inner_handle = tokio::spawn(async move {
                    worker_loop_with_lane_policy(
                        worker_id,
                        allow_background,
                        ctx_inner,
                        high,
                        normal,
                        background,
                    )
                    .await;
                });
                match inner_handle.await {
                    Ok(()) => break, // natural exit (lane closed or shutdown)
                    Err(err) if err.is_panic() => {
                        warn!(worker_id, "dispatch worker panicked, respawning");
                        if queue.inner.shutdown.is_cancelled() {
                            break;
                        }
                        continue;
                    },
                    Err(_) => break, // task cancelled
                }
            }
        });
        self.inner.workers.lock().push(handle);
    }

    /// Submit a sync job. Returns the oneshot receiver from `job.response_tx`
    /// (the caller must hold the receiver). Idempotency keys are honoured.
    pub async fn submit(&self, mut job: LlmJob) -> LLMResult<()> {
        if !job.request.json_payloads_are_bounded() {
            let rejected = std::mem::take(&mut job.request);
            rejected.discard_json_payloads_iteratively();
            let error = LLMError::Validation(
                "LLM request JSON exceeds the admitted depth/node ceiling".to_string(),
            );
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }
        let request_bytes = job.request.estimated_retained_bytes() as u64;
        let max_request_bytes = self.inner.config.read().max_request_bytes;
        if request_bytes > max_request_bytes {
            let error = LLMError::RequestTooLarge {
                bytes: request_bytes,
                capacity_bytes: max_request_bytes,
            };
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }
        let lineage_validation = job.validate_trace_lineage();
        if !job.trace_context.is_valid() || lineage_validation.is_err() {
            let error = LLMError::Validation(lineage_validation.err().unwrap_or_else(|| {
                "LLM dispatch requires a valid trace context and scope".to_string()
            }));
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }
        if !self.inner.accepting.load(Ordering::Acquire) {
            let error = LLMError::Cancelled {
                reason: TombstoneReason::QueueShutdown.name().to_string(),
            };
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }

        // A caller deadline is part of this submission's contract, not the
        // underlying idempotent operation's contract. Reject an already-
        // expired caller before consulting either the in-flight index or its
        // recent-result cache; otherwise an expired request could receive a
        // cached success or subscribe to live work. Preserve task-ledger
        // accounting without registering or resolving an idempotency slot.
        if job
            .submission_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let error = LLMError::DeadlineExceeded;
            if let Some(task_ref) = job.task_ref.as_ref() {
                self.inner
                    .worker_ctx
                    .ledger_sink
                    .append(
                        task_ref,
                        LlmCallLedgerEvent::Submitted {
                            job_id: job.job_id.clone(),
                            origin: job.origin.clone(),
                            priority: job.priority,
                            provider: None,
                            model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
                            attempts_so_far: job.attempts.total,
                        },
                    )
                    .await;
                self.inner
                    .worker_ctx
                    .ledger_sink
                    .append(
                        task_ref,
                        LlmCallLedgerEvent::Tombstoned {
                            job_id: job.job_id.clone(),
                            reason: TombstoneReason::DeadlineExceeded,
                            attempts_so_far: job.attempts.total,
                        },
                    )
                    .await;
            }
            let mut meta = JobMeta::pending_from(&job);
            meta.state = JobState::Tombstoned;
            meta.tombstone = Some(TombstoneReason::DeadlineExceeded);
            meta.error = Some(LLMError::DeadlineExceeded.to_string());
            meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            self.inner.metrics.incr_submitted();
            self.inner.metrics.incr_tombstoned();
            self.inner
                .events
                .emit(LlmQueueEvent::Submitted { meta: meta.clone() });
            self.inner.events.emit(LlmQueueEvent::Tombstoned {
                meta,
                reason: TombstoneReason::DeadlineExceeded,
            });
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }

        // Idempotency lookup. The lookup key is scoped by task_ref to
        // prevent cross-tenant cache leaks: caller A in scope (alice/task-1)
        // and caller B in scope (bob/task-2) using the same raw idempotency
        // key MUST NOT share results. We compose the effective key from
        // (agent_id, chat_session_id, task_id, key) so collisions only happen
        // within the same task scope.
        if job.request.metadata.single_physical_attempt && job.idempotency_key.is_some() {
            let error = LLMError::Validation(
                "single-attempt protected LLM jobs cannot use response reuse".to_owned(),
            );
            let _ = job.response_tx.send(Err(error.clone()));
            return Err(error);
        }
        let effective_key = job
            .idempotency_key
            .as_ref()
            .map(|k| scope_idempotency_key(job.task_ref.as_ref(), &job.trace_context.scope, k));
        let mut idempotent_owner_guard = None;
        if let Some(key) = effective_key.clone() {
            // Atomic lookup-or-register — closes the race where two
            // concurrent submits both saw Miss and both registered,
            // defeating idempotency.
            match self
                .inner
                .inflight_index
                .lookup_or_register(&key, job.job_id.clone())
            {
                LookupOutcome::Cached(arc) => {
                    let payload = mark_dispatch_response_reused((*arc).clone());
                    let _ = job.response_tx.send(payload);
                    return Ok(());
                },
                LookupOutcome::InFlight(mut rx) => {
                    // Subscribe and bridge to caller's oneshot.
                    let tx = job.response_tx;
                    tokio::spawn(async move {
                        match rx.recv().await {
                            Ok(arc) => {
                                let payload = mark_dispatch_response_reused((*arc).clone());
                                let _ = tx.send(payload);
                            },
                            Err(_) => {
                                let _ = tx.send(Err(LLMError::Cancelled {
                                    reason: "idempotent_subscriber_lagged".to_string(),
                                }));
                            },
                        }
                    });
                    return Ok(());
                },
                LookupOutcome::Miss => {
                    // We're the registered owner now. Stash the scoped key
                    // on the job so the worker uses the same key for
                    // resolve() — otherwise resolve would use the raw
                    // caller-provided key and miss subscribers.
                    job.idempotency_key = Some(key.clone());
                    idempotent_owner_guard = Some(InflightSubmissionGuard::new(
                        Arc::clone(&self.inner.inflight_index),
                        key,
                        job.job_id.clone(),
                    ));
                },
            }
        }

        // Persist the ledger intent before publishing the job to a worker. Do
        // not register it as pending yet: a durable sink is async, and shutdown
        // must never wait on a registry entry which is not present in a lane.
        if let Some(task_ref) = job.task_ref.as_ref() {
            self.inner
                .worker_ctx
                .ledger_sink
                .append(
                    task_ref,
                    LlmCallLedgerEvent::Submitted {
                        job_id: job.job_id.clone(),
                        origin: job.origin.clone(),
                        priority: job.priority,
                        provider: None,
                        model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
                        attempts_so_far: job.attempts.total,
                    },
                )
                .await;
        }

        let priority = job.priority;
        let lane = self.lane_for(priority);
        let mut byte_rejection = None;
        let (send_result, rejected_for_shutdown) = {
            let _admission = self.inner.admission.lock();
            if self.inner.accepting.load(Ordering::Acquire) {
                let cfg = self.inner.config.read();
                match self.inner.queued_bytes.try_reserve(
                    priority,
                    request_bytes,
                    cfg.byte_capacity_for(priority),
                    cfg.queue_bytes_global,
                ) {
                    Ok(byte_permit) => {
                        match self.admission_for(priority).clone().try_acquire_owned() {
                            Ok(permit) => {
                                job.queued_byte_permit = Some(byte_permit);
                                let meta = JobMeta::pending_from(&job);
                                self.inner.registry.insert_pending(meta.clone());
                                self.inner
                                    .registry
                                    .store_admission_permit(&job.job_id, permit);
                                self.inner.metrics.incr_submitted();
                                self.inner.events.emit(LlmQueueEvent::Submitted { meta });
                                (lane.try_push(job), false)
                            },
                            Err(_) => (Err(TryPushError::Full(job)), false),
                        }
                    },
                    Err((queued_bytes, capacity_bytes)) => {
                        byte_rejection = Some(LLMError::QueueBytesFull {
                            priority: priority.as_str(),
                            queued_bytes,
                            capacity_bytes,
                        });
                        (Err(TryPushError::Full(job)), false)
                    },
                }
            } else {
                (Err(TryPushError::Closed(job)), true)
            }
        };
        match send_result {
            Ok(()) => {
                if let Some(guard) = idempotent_owner_guard.as_mut() {
                    guard.disarm();
                }
                Ok(())
            },
            Err(send_err) => {
                let returned = send_err.into_inner();
                let (reason, terminal_error) = if rejected_for_shutdown {
                    (
                        TombstoneReason::QueueShutdown,
                        LLMError::Cancelled {
                            reason: TombstoneReason::QueueShutdown.name().to_string(),
                        },
                    )
                } else {
                    (
                        TombstoneReason::QueueFull,
                        byte_rejection.unwrap_or(LLMError::QueueFull {
                            priority: priority.as_str(),
                            depth: self.admitted_depth(priority),
                            capacity: self.admission_capacity(priority),
                        }),
                    )
                };
                self.tombstone_unpublished_job(
                    returned,
                    reason,
                    terminal_error.clone(),
                    idempotent_owner_guard.as_mut(),
                )
                .await;
                Err(terminal_error)
            },
        }
    }

    /// Convenience: build job + submit + await the oneshot.
    pub async fn submit_and_wait(
        &self,
        request: LLMRequest,
        origin: JobOrigin,
    ) -> LLMResult<DispatchedResponse> {
        let (job, rx) = LlmJob::new(request, origin);
        self.submit(job).await?;
        rx.await.map_err(|_| LLMError::Cancelled {
            reason: "receiver_dropped".to_string(),
        })?
    }

    /// Submit a streaming job. Returns `Ok(())` on accepted; deltas are
    /// produced via the receiver embedded in the supplied job's `delta_tx`.
    pub async fn submit_stream(&self, mut job: LlmStreamJob) -> LLMResult<()> {
        if !job.request.json_payloads_are_bounded() {
            let rejected = std::mem::take(&mut job.request);
            rejected.discard_json_payloads_iteratively();
            let error = LLMError::Validation(
                "LLM streaming request JSON exceeds the admitted depth/node ceiling".to_string(),
            );
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            return Err(error);
        }
        let request_bytes = job.request.estimated_retained_bytes() as u64;
        let max_request_bytes = self.inner.config.read().max_request_bytes;
        if request_bytes > max_request_bytes {
            let error = LLMError::RequestTooLarge {
                bytes: request_bytes,
                capacity_bytes: max_request_bytes,
            };
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            return Err(error);
        }
        let lineage_validation = job.validate_trace_lineage();
        if !job.trace_context.is_valid() || lineage_validation.is_err() {
            let error = LLMError::Validation(lineage_validation.err().unwrap_or_else(|| {
                "LLM streaming dispatch requires a valid trace context and scope".to_string()
            }));
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            return Err(error);
        }
        if !self.inner.accepting.load(Ordering::Acquire) {
            let error = LLMError::Cancelled {
                reason: TombstoneReason::QueueShutdown.name().to_string(),
            };
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            return Err(error);
        }
        if job
            .submission_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            let error = LLMError::DeadlineExceeded;
            if let Some(task_ref) = job.task_ref.as_ref() {
                self.inner
                    .worker_ctx
                    .ledger_sink
                    .append(
                        task_ref,
                        LlmCallLedgerEvent::Submitted {
                            job_id: job.job_id.clone(),
                            origin: job.origin.clone(),
                            priority: job.priority,
                            provider: None,
                            model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
                            attempts_so_far: job.attempts.total,
                        },
                    )
                    .await;
                self.inner
                    .worker_ctx
                    .ledger_sink
                    .append(
                        task_ref,
                        LlmCallLedgerEvent::Tombstoned {
                            job_id: job.job_id.clone(),
                            reason: TombstoneReason::DeadlineExceeded,
                            attempts_so_far: job.attempts.total,
                        },
                    )
                    .await;
            }
            let mut meta = JobMeta::pending_from_stream(&job);
            meta.state = JobState::Tombstoned;
            meta.tombstone = Some(TombstoneReason::DeadlineExceeded);
            meta.error = Some(error.to_string());
            meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            self.inner.metrics.incr_submitted();
            self.inner.metrics.incr_tombstoned();
            self.inner
                .events
                .emit(LlmQueueEvent::Submitted { meta: meta.clone() });
            self.inner.events.emit(LlmQueueEvent::Tombstoned {
                meta,
                reason: TombstoneReason::DeadlineExceeded,
            });
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            return Err(error);
        }
        // As with sync submission, the async ledger append precedes registry
        // publication so shutdown can never observe a ghost pending stream.
        if let Some(task_ref) = job.task_ref.as_ref() {
            self.inner
                .worker_ctx
                .ledger_sink
                .append(
                    task_ref,
                    LlmCallLedgerEvent::Submitted {
                        job_id: job.job_id.clone(),
                        origin: job.origin.clone(),
                        priority: job.priority,
                        provider: None,
                        model: Some(job.request.model.clone()).filter(|m| !m.is_empty()),
                        attempts_so_far: 0,
                    },
                )
                .await;
        }

        // Streaming jobs run via a one-off task rather than going through the
        // sync worker channels (deltas require a streaming pipeline). They must
        // nevertheless own the same lifetime lane reservation as synchronous
        // work. Otherwise a saturated streaming provider can create an
        // unbounded task/semaphore-waiter population outside the advertised
        // queue capacities.
        let mut job = Some(job);
        let rejection = {
            let _admission = self.inner.admission.lock();
            if self.inner.accepting.load(Ordering::Acquire) {
                let priority = job.as_ref().expect("stream job admission").priority;
                let cfg = self.inner.config.read();
                match self.inner.queued_bytes.try_reserve(
                    priority,
                    request_bytes,
                    cfg.byte_capacity_for(priority),
                    cfg.queue_bytes_global,
                ) {
                    Ok(byte_permit) => {
                        match self.admission_for(priority).clone().try_acquire_owned() {
                            Ok(permit) => {
                                job.as_mut()
                                    .expect("stream job admission")
                                    .queued_byte_permit = Some(byte_permit);
                                let pending = job.as_ref().expect("stream job admission");
                                let meta = JobMeta::pending_from_stream(pending);
                                self.inner.registry.insert_pending(meta.clone());
                                self.inner
                                    .registry
                                    .store_admission_permit(&pending.job_id, permit);
                                self.inner.metrics.incr_submitted();
                                self.inner.events.emit(LlmQueueEvent::Submitted { meta });
                                let mut tasks = self.inner.stream_tasks.lock();
                                tasks.retain(|task| !task.is_finished());
                                tasks.push(dispatch_stream_job(
                                    self.inner.streaming_ctx.clone(),
                                    job.take().expect("accepted stream job"),
                                ));
                                None
                            },
                            Err(_) => Some((
                                TombstoneReason::QueueFull,
                                LLMError::QueueFull {
                                    priority: priority.as_str(),
                                    depth: self.admitted_depth(priority),
                                    capacity: self.admission_capacity(priority),
                                },
                            )),
                        }
                    },
                    Err((queued_bytes, capacity_bytes)) => Some((
                        TombstoneReason::QueueFull,
                        LLMError::QueueBytesFull {
                            priority: priority.as_str(),
                            queued_bytes,
                            capacity_bytes,
                        },
                    )),
                }
            } else {
                Some((
                    TombstoneReason::QueueShutdown,
                    LLMError::Cancelled {
                        reason: TombstoneReason::QueueShutdown.name().to_string(),
                    },
                ))
            }
        };
        if let Some((reason, error)) = rejection {
            let job = job.expect("rejected stream job");
            // The durable Submitted intent completed before admission was
            // rejected (shutdown or exhausted lane capacity). Preserve the
            // matching transport lifecycle even though this job was never
            // inserted into the pending registry or spawned. Without these
            // events Phase 2 would see the ledger intent but lose the terminal
            // call fact.
            let mut meta = JobMeta::pending_from_stream(&job);
            meta.state = JobState::Tombstoned;
            meta.tombstone = Some(reason.clone());
            meta.error = Some(error.to_string());
            meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            self.inner.metrics.incr_submitted();
            self.inner.metrics.incr_tombstoned();
            self.inner
                .events
                .emit(LlmQueueEvent::Submitted { meta: meta.clone() });
            self.inner.events.emit(LlmQueueEvent::Tombstoned {
                meta,
                reason: reason.clone(),
            });
            let _ = job
                .delta_tx
                .send(crate::types::StreamDelta::Error(error.to_string()))
                .await;
            if let Some(task_ref) = job.task_ref.as_ref() {
                tokio::select! {
                    biased;
                    _ = self.inner.shutdown.cancelled() => {},
                    _ = self.inner.worker_ctx.ledger_sink.append(
                        task_ref,
                        LlmCallLedgerEvent::Tombstoned {
                            job_id: job.job_id.clone(),
                            reason,
                            attempts_so_far: 0,
                        },
                    ) => {},
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Cancel queued (not yet picked up) jobs for the supplied task. The
    /// worker plugs the channel-side race via the cancellation-intent map.
    pub fn cancel_task(&self, task_id: &str, reason: impl Into<String>) -> usize {
        let reason_string = reason.into();
        let job_ids: Vec<JobId> = self
            .inner
            .registry
            .pending
            .iter()
            .filter_map(|entry| {
                let meta = entry.value();
                if meta
                    .task_ref
                    .as_ref()
                    .is_some_and(|task_ref| task_ref.matches_cancel_id(task_id))
                {
                    Some(meta.job_id.clone())
                } else {
                    None
                }
            })
            .collect();
        let count = job_ids.len();
        // TEMP TRACE (delegated-child synthesis cancellation hunt): names the
        // CALLER's reason and which jobs it matched, so a cancellation can be
        // attributed to the code path that asked for it rather than inferred
        // from the tombstone alone.
        info!(
            task_id = %task_id,
            reason = %reason_string,
            matched_jobs = count,
            "[CANCEL-TRACE] cancel_task matched queued jobs"
        );
        for id in &job_ids {
            self.inner.registry.record_cancellation_intent(
                id,
                TombstoneReason::TaskCancelled {
                    reason: Some(reason_string.clone()),
                },
            );
        }
        self.inner.worker_ctx.provider_waiters.notify_all();
        self.inner.worker_ctx.byte_waiters.notify_all();
        self.inner.worker_ctx.local_prep.notify_all();
        count
    }

    /// Cancel queued jobs for the supplied chat session.
    pub fn cancel_chat_session(&self, chat_session_id: &str, reason: impl Into<String>) -> usize {
        let reason_string = reason.into();
        let job_ids: Vec<JobId> = self
            .inner
            .registry
            .pending
            .iter()
            .filter_map(|entry| {
                let meta = entry.value();
                if meta
                    .task_ref
                    .as_ref()
                    .and_then(|t| t.chat_session_id.as_deref())
                    .map(|id| id == chat_session_id)
                    .unwrap_or(false)
                {
                    Some(meta.job_id.clone())
                } else {
                    None
                }
            })
            .collect();
        let count = job_ids.len();
        for id in &job_ids {
            self.inner.registry.record_cancellation_intent(
                id,
                TombstoneReason::ChatSessionEnded {
                    reason: Some(reason_string.clone()),
                },
            );
        }
        self.inner.worker_ctx.provider_waiters.notify_all();
        self.inner.worker_ctx.byte_waiters.notify_all();
        self.inner.worker_ctx.local_prep.notify_all();
        count
    }

    /// Cancel a single queued job by id. Returns true when a pending entry
    /// exists; in-flight jobs continue (use task cancellation for those).
    pub fn cancel_job(&self, job_id: &JobId, reason: impl Into<String>) -> bool {
        let reason_string = reason.into();
        let found = self.inner.registry.pending.contains_key(job_id);
        if found {
            self.inner.registry.record_cancellation_intent(
                job_id,
                TombstoneReason::ExplicitCancel {
                    reason: Some(reason_string),
                },
            );
            self.inner.worker_ctx.provider_waiters.notify_all();
            self.inner.worker_ctx.byte_waiters.notify_all();
            self.inner.worker_ctx.local_prep.notify_all();
        }
        found
    }

    /// Resubmit a previously-failed job from the dead-letter ring buffer.
    /// Returns the new job's id when submission accepted.
    pub async fn resubmit_failed(&self, dead_letter_id: &JobId) -> LLMResult<JobId> {
        let _meta = self
            .inner
            .registry
            .find_dead_letter(dead_letter_id)
            .ok_or_else(|| {
                LLMError::Validation(format!(
                    "no failed/tombstoned job with id `{}`",
                    dead_letter_id
                ))
            })?;
        // For Phase 1 we re-record the dispatch by signalling the caller —
        // the queue can't rebuild the original `LLMRequest` body because
        // `JobMeta` doesn't carry it. Callers needing this should rebuild
        // their request and call `submit()` directly. Surfacing
        // `Unsupported` keeps the API stable for Phase 5+ when we extend
        // JobMeta to carry the request body for replays.
        Err(LLMError::UnsupportedCapability(
            "resubmit_failed requires the caller to rebuild the request; \
             call `submit()` with a fresh LlmJob built from the original payload"
                .to_string(),
        ))
    }

    /// Subscribe to the realtime event stream.
    pub fn subscribe_events(&self) -> tokio::sync::broadcast::Receiver<LlmQueueEvent> {
        self.inner.events.subscribe()
    }

    /// Snapshot of registry + worker / lane state.
    pub fn snapshot(&self) -> QueueSnapshot {
        let registry = self.inner.registry.snapshot();
        let waiting = self.inner.worker_ctx.provider_waiters.waiting_counts();
        QueueSnapshot {
            workers_total: self.inner.workers.lock().len(),
            workers_busy: self.inner.metrics.workers_busy() as usize,
            depth_high: self.admitted_depth(Priority::High),
            depth_normal: self.admitted_depth(Priority::Normal),
            depth_background: self.admitted_depth(Priority::Background),
            capacity_high: self.admission_capacity(Priority::High),
            capacity_normal: self.admission_capacity(Priority::Normal),
            capacity_background: self.admission_capacity(Priority::Background),
            retained_bytes_high: self.inner.queued_bytes.lane_bytes(Priority::High),
            retained_bytes_normal: self.inner.queued_bytes.lane_bytes(Priority::Normal),
            retained_bytes_background: self.inner.queued_bytes.lane_bytes(Priority::Background),
            retained_bytes_global: self.inner.queued_bytes.global_bytes(),
            retained_bytes_capacity_high: self
                .inner
                .config
                .read()
                .byte_capacity_for(Priority::High),
            retained_bytes_capacity_normal: self
                .inner
                .config
                .read()
                .byte_capacity_for(Priority::Normal),
            retained_bytes_capacity_background: self
                .inner
                .config
                .read()
                .byte_capacity_for(Priority::Background),
            retained_bytes_capacity_global: self.inner.config.read().queue_bytes_global,
            waiting_for_provider: waiting.total(),
            waiting_for_provider_high: waiting.high,
            waiting_for_provider_normal: waiting.normal,
            waiting_for_provider_background: waiting.background,
            waiting_for_local_prep: self.inner.worker_ctx.local_prep.waiting_count(),
            oldest_wait_ms_high: oldest_lane_wait_ms(&registry, Priority::High),
            oldest_wait_ms_normal: oldest_lane_wait_ms(&registry, Priority::Normal),
            oldest_wait_ms_background: oldest_lane_wait_ms(&registry, Priority::Background),
            registry,
        }
    }

    /// Graceful shutdown. Drains pending lanes (tombstoned), waits up to
    /// `timeout` for in-flight to complete, then force-cancels remaining.
    pub async fn shutdown(&self, timeout: Duration) -> ShutdownStats {
        info!("dispatch queue shutdown initiated");
        {
            let _admission = self.inner.admission.lock();
            self.inner.accepting.store(false, Ordering::Release);
            // Close before drain so delayed/provider/local-prep requeues get
            // `TryPushError::Closed` instead of resurrecting a drained job.
            self.inner.lane_high.close();
            self.inner.lane_normal.close();
            self.inner.lane_background.close();
        }
        let deadline = Instant::now() + timeout;
        // Pending work is not part of the graceful in-flight drain. Stop
        // delayed requeues and provider waiters immediately; active provider
        // attempts continue until the normal drain deadline below.
        let waiting_before_drain = self
            .inner
            .worker_ctx
            .provider_waiters
            .waiting_counts()
            .total()
            .saturating_add(self.inner.worker_ctx.byte_waiters.waiting_count())
            .saturating_add(self.inner.worker_ctx.local_prep.waiting_count());
        self.inner.pending_shutdown.cancel();
        self.inner.worker_ctx.provider_waiters.notify_all();
        self.inner.worker_ctx.byte_waiters.notify_all();
        self.inner.worker_ctx.local_prep.notify_all();
        let in_flight_before_drain = self.inner.registry.in_flight.len();

        // Drain pending lanes — tombstone everything.
        let mut tombstoned_pending = waiting_before_drain;
        for lane in [
            &self.inner.lane_high,
            &self.inner.lane_normal,
            &self.inner.lane_background,
        ] {
            for job in lane.drain() {
                let job_id = job.job_id.clone();
                let reason = TombstoneReason::QueueShutdown;
                let meta = self.inner.registry.pending_to_tombstone(&job_id, |m| {
                    m.state = super::types::JobState::Tombstoned;
                    m.tombstone = Some(reason.clone());
                    m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
                });
                if let Some(meta) = meta {
                    self.inner.events.emit(LlmQueueEvent::Tombstoned {
                        meta,
                        reason: reason.clone(),
                    });
                }
                self.inner.metrics.incr_tombstoned();
                let task_ref = job.task_ref.clone();
                let attempts_so_far = job.attempts.total;
                if let Some(key) = job.idempotency_key.as_deref() {
                    self.inner.inflight_index.resolve(
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
                        _ = tokio::time::sleep_until(deadline.into()) => {},
                        _ = self.inner.worker_ctx.ledger_sink.append(
                            task_ref,
                            LlmCallLedgerEvent::Tombstoned {
                                job_id: job_id.clone(),
                                reason: reason.clone(),
                                attempts_so_far,
                            },
                        ) => {},
                    }
                }
                tombstoned_pending += 1;
            }
        }

        // Wait for in-flight to drain, polling.
        while Instant::now() < deadline
            && (!self.inner.registry.in_flight.is_empty()
                || !self.inner.registry.pending.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        let in_flight_at_deadline = self.inner.registry.in_flight.len();
        // Force-cancel any remaining in-flight provider/backoff operation. The
        // worker observes this token inside its active select, tombstones the
        // job, resolves subscribers and publishes the terminal event.
        self.inner.shutdown.cancel();

        let force_deadline = Instant::now() + Duration::from_secs(1);
        while Instant::now() < force_deadline
            && (!self.inner.registry.in_flight.is_empty()
                || !self.inner.registry.pending.is_empty())
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        self.inner.worker_ctx.deferred_tasks.close();
        let deferred_joined = tokio::time::timeout(
            Duration::from_secs(1),
            self.inner.worker_ctx.deferred_tasks.wait(),
        )
        .await
        .is_ok();

        let mut workers = std::mem::take(&mut *self.inner.workers.lock());
        workers.extend(std::mem::take(&mut *self.inner.stream_tasks.lock()));
        let joined = tokio::time::timeout(Duration::from_secs(1), async {
            for worker in &mut workers {
                let _ = worker.await;
            }
        })
        .await
        .is_ok();
        if !joined || !deferred_joined {
            for worker in &workers {
                if !worker.is_finished() {
                    worker.abort();
                }
            }
        }
        for handle in self.inner.worker_ctx.isolated_executors.lock().drain(..) {
            if !handle.is_finished() {
                handle.abort();
            }
        }

        let stats = ShutdownStats {
            tombstoned_pending,
            in_flight_at_shutdown: self.inner.registry.in_flight.len(),
            pending_at_shutdown: self.inner.registry.pending.len(),
            completed_during_drain: in_flight_before_drain.saturating_sub(in_flight_at_deadline),
        };
        info!(?stats, "dispatch queue shutdown complete");
        stats
    }

    /// Number of registered worker tasks.
    pub fn worker_count(&self) -> usize {
        self.inner.workers.lock().len()
    }

    /// Access the hot-reloadable config handle.
    pub fn config_handle(&self) -> Arc<RwLock<DispatchConfig>> {
        self.inner.config.clone()
    }

    fn lane_for(&self, p: Priority) -> &FairLane {
        match p {
            Priority::High => &self.inner.lane_high,
            Priority::Normal => &self.inner.lane_normal,
            Priority::Background => &self.inner.lane_background,
        }
    }

    fn admission_for(&self, p: Priority) -> &Arc<Semaphore> {
        match p {
            Priority::High => &self.inner.admission_high,
            Priority::Normal => &self.inner.admission_normal,
            Priority::Background => &self.inner.admission_background,
        }
    }

    fn admission_capacity(&self, p: Priority) -> usize {
        self.lane_for(p).capacity()
    }

    fn admitted_depth(&self, p: Priority) -> usize {
        self.admission_capacity(p)
            .saturating_sub(self.admission_for(p).available_permits())
    }

    /// Publish the observable terminal lifecycle for a submission which lost
    /// admission before its job reached a worker lane.
    ///
    /// This boundary deliberately abandons, rather than resolves, an
    /// idempotency generation. The rejection is terminal for this submission
    /// and must wake subscribers, but QueueFull/shutdown is not a terminal
    /// result of the underlying logical operation and therefore must not be
    /// cached against a same-key retry. Once lane publication succeeds the
    /// worker owns normal cached resolution instead.
    async fn tombstone_unpublished_job(
        &self,
        job: LlmJob,
        reason: TombstoneReason,
        error: LLMError,
        idempotent_owner_guard: Option<&mut InflightSubmissionGuard>,
    ) {
        let job_id = job.job_id.clone();
        let reason_for_meta = reason.clone();
        let error_message = error.to_string();
        let meta = self.inner.registry.pending_to_tombstone(&job_id, |meta| {
            meta.state = super::types::JobState::Tombstoned;
            meta.tombstone = Some(reason_for_meta);
            meta.error = Some(error_message.clone());
            meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
        });
        let meta = meta.unwrap_or_else(|| {
            // Shutdown can flip `accepting` after the durable ledger append
            // but before registry publication. The call was submitted from
            // the caller/ledger perspective, so emit a complete zero-attempt
            // terminal lifecycle without creating a ghost pending entry.
            let mut meta = JobMeta::pending_from(&job);
            meta.state = super::types::JobState::Tombstoned;
            meta.tombstone = Some(reason.clone());
            meta.error = Some(error_message);
            meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            self.inner.metrics.incr_submitted();
            self.inner
                .events
                .emit(LlmQueueEvent::Submitted { meta: meta.clone() });
            meta
        });
        self.inner.events.emit(LlmQueueEvent::Tombstoned {
            meta,
            reason: reason.clone(),
        });
        self.inner.metrics.incr_tombstoned();
        let task_ref = job.task_ref.clone();
        let attempts_so_far = job.attempts.total;
        if let Some(key) = job.idempotency_key.as_deref() {
            self.inner
                .inflight_index
                .abandon(key, &job.job_id, Arc::new(Err(error.clone())));
        }
        // The helper and submission guard form one cancellation-safe handoff:
        // before this point a dropped submit lets the guard abandon; after
        // this point the exact generation has already been abandoned here.
        if let Some(guard) = idempotent_owner_guard {
            guard.disarm();
        }
        let _ = job.response_tx.send(Err(error));
        if let Some(task_ref) = task_ref.as_ref() {
            tokio::select! {
                biased;
                _ = self.inner.shutdown.cancelled() => {},
                _ = self.inner.worker_ctx.ledger_sink.append(
                    task_ref,
                    LlmCallLedgerEvent::Tombstoned {
                        job_id,
                        reason: reason.clone(),
                        attempts_so_far,
                    },
                ) => {},
            }
        }
    }

    #[cfg(test)]
    pub(super) fn idempotency_key_is_reusable_for_test(
        &self,
        task_ref: Option<&super::types::TaskRef>,
        scope: &crate::trace::LlmScope,
        raw_key: &str,
    ) -> bool {
        let key = scope_idempotency_key(task_ref, scope, raw_key);
        !self.inner.inflight_index.is_in_flight(&key)
            && !self.inner.inflight_index.has_recent_result_for_test(&key)
    }
}

/// Publishable snapshot for the viewer endpoint.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QueueSnapshot {
    pub workers_total: usize,
    pub workers_busy: usize,
    pub depth_high: usize,
    pub depth_normal: usize,
    pub depth_background: usize,
    pub capacity_high: usize,
    pub capacity_normal: usize,
    pub capacity_background: usize,
    #[serde(default)]
    pub retained_bytes_high: u64,
    #[serde(default)]
    pub retained_bytes_normal: u64,
    #[serde(default)]
    pub retained_bytes_background: u64,
    #[serde(default)]
    pub retained_bytes_global: u64,
    #[serde(default)]
    pub retained_bytes_capacity_high: u64,
    #[serde(default)]
    pub retained_bytes_capacity_normal: u64,
    #[serde(default)]
    pub retained_bytes_capacity_background: u64,
    #[serde(default)]
    pub retained_bytes_capacity_global: u64,
    #[serde(default)]
    pub waiting_for_provider: usize,
    #[serde(default)]
    pub waiting_for_provider_high: usize,
    #[serde(default)]
    pub waiting_for_provider_normal: usize,
    #[serde(default)]
    pub waiting_for_provider_background: usize,
    #[serde(default)]
    pub waiting_for_local_prep: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_wait_ms_high: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_wait_ms_normal: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_wait_ms_background: Option<u64>,
    pub registry: RegistrySnapshot,
}

fn oldest_lane_wait_ms(registry: &RegistrySnapshot, priority: Priority) -> Option<u64> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    registry
        .pending
        .iter()
        .filter(|meta| {
            meta.priority == priority
                && matches!(
                    meta.state,
                    JobState::Pending
                        | JobState::WaitingForProvider
                        | JobState::WaitingForLocalPrep
                )
        })
        .filter_map(|meta| {
            now_ms
                .checked_sub(meta.submitted_at_ms)
                .map(|delta| delta.max(0) as u64)
        })
        .max()
}

/// Outcome of a graceful shutdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShutdownStats {
    pub tombstoned_pending: usize,
    pub in_flight_at_shutdown: usize,
    #[serde(default)]
    pub pending_at_shutdown: usize,
    pub completed_during_drain: usize,
}

/// Compose the effective idempotency lookup key, scoped by `TaskRef` so
/// callers in different scopes can't accidentally share cached results.
///
/// Uses JSON encoding of the tuple `(task_ref, raw_key)` for robust
/// composition. JSON escaping handles arbitrary content in identifiers —
/// a previous pipe-delimited concatenation was vulnerable to delimiter
/// injection (task_id="A|" with raw="B" collided with task_id="A" with
/// raw="|B"). Falls back to a sentinel-prefixed raw key if serialization
/// fails (should be impossible for our small types).
fn scope_idempotency_key(
    task_ref: Option<&super::types::TaskRef>,
    trace_scope: &crate::trace::LlmScope,
    raw: &str,
) -> String {
    // Preserve the pre-Phase-1 idempotency boundary (task/agent/chat session)
    // while adding authoritative tenant scope. Execution, turn and iteration
    // lineage are observability dimensions; including them would silently
    // disable legitimate deduplication whenever a caller supplied richer
    // correlation metadata.
    let task_boundary = task_ref.map(|task_ref| {
        (
            task_ref.task_id.as_str(),
            task_ref.agent_id.as_deref(),
            task_ref.chat_session_id.as_deref(),
        )
    });
    let tenant_boundary = (
        trace_scope.principal.as_str(),
        trace_scope.workspace.as_str(),
    );
    serde_json::to_string(&(tenant_boundary, task_boundary, raw))
        .unwrap_or_else(|_| format!("__idem_fallback__:{}", raw))
}

fn mark_dispatch_response_reused(
    payload: LLMResult<DispatchedResponse>,
) -> LLMResult<DispatchedResponse> {
    payload.map(|mut dispatched| {
        dispatched.trace_receipt.response_reused = true;
        // The provider payload is shared by every idempotent subscriber. Reuse
        // is consumer-specific dispatch metadata and must not mutate that
        // shared allocation; callers needing an owned LLMResponse use
        // `DispatchedResponse::into_response`, which stamps this receipt.
        dispatched
    })
}

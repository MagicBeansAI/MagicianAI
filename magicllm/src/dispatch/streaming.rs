//! Streaming submission path.
//!
//! Mirrors the sync worker semantics but forwards `StreamDelta` items
//! produced by `MultiLLMRouter::route_stream` to the caller's receiver.
//! Retry is constrained — once any delta has been emitted, the job cannot
//! be retried because the caller has already observed partial output.

use std::sync::Arc;
use std::time::Instant;

use async_channel as ach;
use parking_lot::RwLock;
use tokio::sync::mpsc;
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::router_handle::DispatchRouter;
use crate::error::LLMError;
use crate::types::{LlmRouteIdentity, StreamDelta};

use super::cancellation::TaskStateView;
use super::classifier::classify;
use super::cloud_admission::{CloudAdmission, CloudPermit};
use super::config::DispatchConfig;
use super::events::{EventBus, LlmQueueEvent};
use super::job::{ErrorClass, LlmStreamJob};
use super::ledger::{LlmCallLedgerEvent, TaskLedgerSink};
use super::metrics::DispatchMetrics;
use super::provider_state::ProviderStateMap;
use super::quota::{estimate_request_tokens, ProviderQuotaMap};
use super::registry::JobRegistry;
use super::types::{JobState, TombstoneReason};

/// Context shared with streaming tasks. Streaming uses the same admission,
/// provider-capacity, RPM/TPM, and `CloudAdmission` controls as ordinary
/// workers, while each accepted stream owns a dedicated forwarding task for
/// its deltas. Streams already run off the scheduler worker pool; the cloud
/// cap is the process-wide non-Ollama HTTP bound they still share with
/// `run_physical_attempt`.
pub struct StreamingContext {
    pub router: Arc<dyn DispatchRouter>,
    pub registry: Arc<JobRegistry>,
    pub provider_state: Arc<ProviderStateMap>,
    pub task_state: Arc<dyn TaskStateView>,
    pub ledger_sink: Arc<dyn TaskLedgerSink>,
    pub events: EventBus,
    pub metrics: DispatchMetrics,
    pub config: Arc<RwLock<DispatchConfig>>,
    /// Cancels work which has not obtained provider capacity when graceful
    /// shutdown starts. Active streams continue to use `shutdown` and receive
    /// the normal drain window.
    pub pending_shutdown: CancellationToken,
    pub shutdown: CancellationToken,
    /// Same RPM/TPM map as sync workers. Empty / 0 is a no-op.
    pub provider_quota: Arc<ProviderQuotaMap>,
    /// Same process-wide non-Ollama HTTP cap as sync workers. Shared `Arc`.
    pub cloud_admission: Arc<CloudAdmission>,
}

/// Provider + optional cloud permits for one physical stream. Dropping this
/// before tombstone/ledger I/O releases the slots the way `AttemptPermits`
/// does on the sync path.
struct StreamAttemptPermits {
    _provider: tokio::sync::OwnedSemaphorePermit,
    _cloud: Option<CloudPermit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamForwardOutcome {
    Delivered,
    ReceiverClosed,
    QueueShutdown,
    TaskCancelled,
}

fn stream_outcome_provider_kind(
    initial_provider: &crate::capability::LLMProviderKind,
    terminal_route: Option<&LlmRouteIdentity>,
    terminal_error: Option<&LLMError>,
) -> crate::capability::LLMProviderKind {
    terminal_error
        .and_then(LLMError::effective_route)
        .map(|(_, provider, _)| provider.clone())
        .or_else(|| terminal_route.map(|identity| identity.provider.clone()))
        .unwrap_or_else(|| initial_provider.clone())
}

fn stream_retry_after(error: &LLMError) -> Option<std::time::Duration> {
    match error.root_cause() {
        LLMError::RateLimited { retry_after } => *retry_after,
        _ => None,
    }
}

/// Preserve normal consumer backpressure without making it stronger than the
/// queue lifecycle. A full, still-open receiver must not hide force shutdown
/// or task cancellation from the stream task while it owns provider capacity.
async fn forward_stream_delta(
    sender: &mpsc::Sender<StreamDelta>,
    delta: StreamDelta,
    shutdown: &CancellationToken,
    task_cancel: &CancellationToken,
) -> StreamForwardOutcome {
    tokio::select! {
        biased;
        _ = shutdown.cancelled() => StreamForwardOutcome::QueueShutdown,
        _ = task_cancel.cancelled() => StreamForwardOutcome::TaskCancelled,
        result = sender.send(delta) => {
            if result.is_ok() {
                StreamForwardOutcome::Delivered
            } else {
                StreamForwardOutcome::ReceiverClosed
            }
        },
    }
}

fn stream_forward_cancellation(
    outcome: StreamForwardOutcome,
) -> Option<(TombstoneReason, &'static str)> {
    match outcome {
        StreamForwardOutcome::QueueShutdown => Some((
            TombstoneReason::QueueShutdown,
            "queue_shutdown_during_stream_forward",
        )),
        StreamForwardOutcome::TaskCancelled => Some((
            TombstoneReason::TaskCancelledInFlight { reason: None },
            "task_cancelled_during_stream_forward",
        )),
        StreamForwardOutcome::Delivered | StreamForwardOutcome::ReceiverClosed => None,
    }
}

/// Process one streaming job. Honours cancellation; forwards every
/// `StreamDelta` the router emits.
pub async fn process_stream_job(ctx: Arc<StreamingContext>, mut job: LlmStreamJob) {
    let job_id = job.job_id.clone();

    if ctx.pending_shutdown.is_cancelled() || ctx.shutdown.is_cancelled() {
        tombstone_stream(
            &ctx,
            &mut job,
            TombstoneReason::QueueShutdown,
            "queue_shutdown_before_stream_pickup",
        )
        .await;
        return;
    }
    if let Some(reason) = ctx.registry.take_cancellation_intent(&job_id) {
        tombstone_stream(
            &ctx,
            &mut job,
            reason,
            "external_cancel_before_stream_pickup",
        )
        .await;
        return;
    }

    // Submission deadline gate.
    if let Some(deadline) = job.submission_deadline {
        if Instant::now() >= deadline {
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::DeadlineExceeded,
                "deadline",
            )
            .await;
            return;
        }
    }

    // Pre-dispatch cancellation gate.
    if let Some(task_ref) = job.task_ref.clone() {
        match ctx.task_state.snapshot(&task_ref.task_id).await {
            Ok(None) => {
                tombstone_stream(&ctx, &mut job, TombstoneReason::TaskMissing, "task_missing")
                    .await;
                return;
            },
            Ok(Some(snap)) if snap.is_cancelled_or_terminal_failed => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::TaskCancelled {
                        reason: snap.cancel_reason.clone(),
                    },
                    "task_cancelled",
                )
                .await;
                return;
            },
            _ => {},
        }
    }

    let routing_router = job
        .router_snapshot
        .clone()
        .unwrap_or_else(|| Arc::clone(&ctx.router));
    let provider_kind = routing_router
        .provider_for_request(&job.request)
        .unwrap_or_default();
    let provider_state = ctx.provider_state.get(&provider_kind);

    if let Some(retry_after) = provider_state.cooldown_remaining() {
        debug!(
            ?provider_kind,
            "provider in cooldown — streaming job rejected with retriable error"
        );
        // Streaming jobs cannot be transparently requeued once a caller owns
        // the delta channel. Preserve the actual overload classification so
        // this is not recorded as a user/task cancellation.
        fail_stream_before_or_after_dispatch(
            &ctx,
            &mut job,
            LLMError::RateLimited {
                retry_after: Some(retry_after),
            },
            ErrorClass::RateLimit,
            Some(provider_kind),
        )
        .await;
        return;
    }
    if provider_state.breaker_is_open() {
        fail_stream_before_or_after_dispatch(
            &ctx,
            &mut job,
            LLMError::ProviderUnavailable,
            ErrorClass::Server5xx,
            Some(provider_kind),
        )
        .await;
        return;
    }

    // Acquire provider permit briefly.
    let cancel_token = ctx.task_state.subscribe_cancel(job.task_ref.as_ref());
    let provider_wait_started = Instant::now();
    let lane_wait_ms = provider_wait_started
        .saturating_duration_since(job.submitted_at)
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    if let Some(mut meta) = ctx.registry.pending.get_mut(&job_id) {
        // Publish the resolved provider before semaphore admission so
        // capacity-aware durable producers can see a streaming job that has
        // already left its lane but is still provider-pending.
        meta.provider = Some(provider_kind.clone());
        meta.wait_ms = Some(lane_wait_ms);
    }
    let mut waiting_event_emitted = false;
    let permit = loop {
        if let Some(reason) = ctx.registry.take_cancellation_intent(&job_id) {
            tombstone_stream(
                &ctx,
                &mut job,
                reason,
                "external_cancel_waiting_for_stream_provider_permit",
            )
            .await;
            return;
        }
        if job
            .submission_deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
        {
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::DeadlineExceeded,
                "deadline_waiting_for_stream_provider_permit",
            )
            .await;
            return;
        }
        let attempt = provider_state.concurrency.clone().try_acquire_owned();
        match attempt {
            Ok(permit) => break permit,
            Err(tokio::sync::TryAcquireError::Closed) => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "semaphore_closed",
                )
                .await;
                return;
            },
            Err(tokio::sync::TryAcquireError::NoPermits) => {
                if !waiting_event_emitted {
                    let waiting_meta = ctx.registry.pending.get_mut(&job_id).map(|mut meta| {
                        meta.state = JobState::WaitingForProvider;
                        (*meta).clone()
                    });
                    if let Some(meta) = waiting_meta {
                        ctx.events.emit(LlmQueueEvent::WaitingForProvider {
                            meta,
                            provider: provider_kind.clone(),
                        });
                    }
                    waiting_event_emitted = true;
                }
            },
        }
        tokio::select! {
            biased;
            _ = ctx.pending_shutdown.cancelled() => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_waiting_for_stream_provider_permit",
                )
                .await;
                return;
            }
            _ = ctx.shutdown.cancelled() => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_waiting_for_stream_provider_permit",
                )
                .await;
                return;
            }
            _ = cancel_token.cancelled() => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_waiting_for_stream_provider_permit",
                )
                .await;
                return;
            }
            _ = job.delta_tx.closed() => {
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::ChatSessionEnded {
                        reason: Some("stream_receiver_dropped".to_string()),
                    },
                    "stream_receiver_dropped_waiting_for_stream_provider_permit",
                )
                .await;
                return;
            }
            _ = sleep(std::time::Duration::from_millis(50)) => {},
        }
    };
    let provider_wait_ms = provider_wait_started
        .elapsed()
        .as_millis()
        .min(u128::from(u64::MAX)) as u64;
    // Mark in_flight + emit.
    let provider_kind_for_meta = provider_kind.clone();
    ctx.registry.mark_in_flight(&job_id, |meta| {
        meta.state = JobState::InFlight;
        meta.dispatched_at_ms = Some(chrono::Utc::now().timestamp_millis());
        // `wait_ms` is lane/task pickup only. Provider semaphore contention
        // has its own field so the queue viewer can distinguish scheduling
        // pressure from model execution latency.
        meta.wait_ms = Some(lane_wait_ms);
        meta.provider = Some(provider_kind_for_meta);
        meta.provider_wait_ms = Some(provider_wait_ms);
    });
    if let Some(reason) = ctx.registry.take_cancellation_intent(&job_id) {
        drop(permit);
        tombstone_stream(
            &ctx,
            &mut job,
            reason,
            "external_cancel_after_stream_pickup",
        )
        .await;
        return;
    }
    ctx.metrics.incr_dispatched();
    if let Some(meta) = ctx.registry.find(&job_id) {
        ctx.events.emit(LlmQueueEvent::Dispatched { meta });
    }

    let dispatched_at = Instant::now();
    job.request.metadata.ensure_provider_attempt_counter();

    if let Some(task_ref) = job.task_ref.clone() {
        let ledger_event = LlmCallLedgerEvent::AttemptStart {
            job_id: job_id.clone(),
            attempt: 1,
            dispatched_at_unix_ms: chrono::Utc::now().timestamp_millis(),
        };
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_during_stream_attempt_ledger",
                )
                .await;
                return;
            }
            _ = cancel_token.cancelled() => {
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_during_stream_attempt_ledger",
                )
                .await;
                return;
            }
            _ = job.delta_tx.closed() => {
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::ChatSessionEnded {
                        reason: Some("stream_receiver_dropped".to_string()),
                    },
                    "stream_receiver_dropped_during_stream_attempt_ledger",
                )
                .await;
                return;
            }
            _ = ctx.ledger_sink.append(&task_ref, ledger_event) => {},
        }
    }

    // RPM/TPM after the provider permit and before the physical stream so
    // chat (the streaming path) shares the same buckets as `route()`.
    let estimated_tokens = estimate_request_tokens(&job.request);
    tokio::select! {
        biased;
        _ = ctx.shutdown.cancelled() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::QueueShutdown,
                "queue_shutdown_waiting_for_stream_quota",
            )
            .await;
            return;
        }
        _ = cancel_token.cancelled() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_waiting_for_stream_quota",
            )
            .await;
            return;
        }
        _ = job.delta_tx.closed() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::ChatSessionEnded {
                    reason: Some("stream_receiver_dropped".to_string()),
                },
                "stream_receiver_dropped_waiting_for_stream_quota",
            )
            .await;
            return;
        }
        _ = ctx.provider_quota.acquire(&provider_kind, estimated_tokens) => {},
    }

    // Global cloud cap after the provider permit and quota, before
    // `route_stream()`. Ollama and `global_cloud_concurrency == 0` skip this.
    // Hold the permit for the whole stream so cancel/shutdown Drop releases it.
    let cloud_permit = tokio::select! {
        biased;
        _ = ctx.shutdown.cancelled() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::QueueShutdown,
                "queue_shutdown_waiting_for_stream_cloud",
            )
            .await;
            return;
        }
        _ = cancel_token.cancelled() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::TaskCancelledInFlight { reason: None },
                "task_cancelled_waiting_for_stream_cloud",
            )
            .await;
            return;
        }
        _ = job.delta_tx.closed() => {
            drop(permit);
            tombstone_stream(
                &ctx,
                &mut job,
                TombstoneReason::ChatSessionEnded {
                    reason: Some("stream_receiver_dropped".to_string()),
                },
                "stream_receiver_dropped_waiting_for_stream_cloud",
            )
            .await;
            return;
        }
        acquired = ctx.cloud_admission.acquire(&provider_kind) => match acquired {
            Ok(cloud_permit) => cloud_permit,
            Err(_) => {
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "cloud_semaphore_closed",
                )
                .await;
                return;
            },
        },
    };
    let permit = StreamAttemptPermits {
        _provider: permit,
        _cloud: cloud_permit,
    };

    // Release retained-byte admission only after this stream owns its
    // matching provider permit and is about to enter the physical router.
    // Spawned stream tasks and provider-sem waiters therefore remain bounded.
    job.queued_byte_permit.take();

    // Bridge: router takes its own `mpsc::Sender<StreamDelta>`. We forward
    // each delta to the caller's tx while watching for cancellation.
    let (router_tx, mut router_rx) = mpsc::channel::<StreamDelta>(32);
    let request_clone = job.request.clone();
    let mut router_handle = {
        let router = routing_router.clone();
        tokio::spawn(async move { router.route_stream(request_clone, router_tx).await })
    };

    let mut error_emitted = None;
    let mut terminal_tokens = None;
    let mut terminal_route_identity: Option<LlmRouteIdentity> = None;
    let mut terminal_response_seen = false;
    let shutdown_token = ctx.shutdown.clone();
    loop {
        tokio::select! {
            biased;
            _ = shutdown_token.cancelled() => {
                // Process shutdown — abort the upstream stream and tombstone
                // so the caller's stream receiver closes with a clear reason.
                debug!(job_id = %job_id, "stream cancelled by shutdown");
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown",
                ).await;
                return;
            }
            _ = cancel_token.cancelled() => {
                debug!(job_id = %job_id, "stream cancelled by task");
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_in_flight",
                ).await;
                return;
            }
            _ = job.delta_tx.closed() => {
                debug!(job_id = %job_id, "stream consumer dropped");
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::ChatSessionEnded {
                        reason: Some("stream_receiver_dropped".to_string()),
                    },
                    "stream_receiver_dropped",
                ).await;
                return;
            }
            delta = router_rx.recv() => match delta {
                Some(StreamDelta::Error(err_str)) => {
                    error_emitted = Some(LLMError::Other(err_str.clone()));
                    match forward_stream_delta(
                        &job.delta_tx,
                        StreamDelta::Error(err_str),
                        &shutdown_token,
                        &cancel_token,
                    )
                    .await
                    {
                        StreamForwardOutcome::QueueShutdown => {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::QueueShutdown,
                                "queue_shutdown_during_stream_forward",
                            )
                            .await;
                            return;
                        },
                        StreamForwardOutcome::TaskCancelled => {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::TaskCancelledInFlight { reason: None },
                                "task_cancelled_during_stream_forward",
                            )
                            .await;
                            return;
                        },
                        StreamForwardOutcome::ReceiverClosed => {
                            // The consumer is the only observer of this stream.
                            // Do not wait for an adapter which emitted Error but
                            // never resolves its own future: abort it and release
                            // provider/lane ownership immediately.
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::ChatSessionEnded {
                                    reason: Some("stream_receiver_dropped".to_string()),
                                },
                                "stream_receiver_dropped_after_error",
                            )
                            .await;
                            return;
                        },
                        StreamForwardOutcome::Delivered => {},
                    }
                    break;
                }
                Some(StreamDelta::Done(mut response)) => {
                    let provider_attempt_count = job.request.metadata.provider_attempt_count();
                    if provider_attempt_count == 0 {
                        let error = LLMError::Other(
                            "streaming router returned a response without recording a provider attempt"
                                .to_string(),
                        );
                        let outcome = forward_stream_delta(
                            &job.delta_tx,
                            StreamDelta::Error(error.to_string()),
                            &shutdown_token,
                            &cancel_token,
                        )
                        .await;
                        if let Some((reason, log_reason)) = stream_forward_cancellation(outcome) {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(&ctx, &mut job, reason, log_reason).await;
                            return;
                        }
                        error_emitted = Some(error);
                        break;
                    }
                    let receipt = crate::trace::LlmTraceReceipt::queued(
                        job.trace_context.clone(),
                        job_id.to_string(),
                        provider_attempt_count,
                    );
                    let route_identity = response.route_identity.clone();
                    response.trace_receipt = Some(receipt);
                    let response = match response.into_retained_bounded() {
                        Ok(response) => response,
                        Err(error) => {
                            let error = match route_identity {
                                Some(identity) => error.with_route(
                                    identity.profile,
                                    identity.provider,
                                    identity.model,
                                ),
                                None => error,
                            };
                            let outcome = forward_stream_delta(
                                &job.delta_tx,
                                StreamDelta::Error(error.to_string()),
                                &shutdown_token,
                                &cancel_token,
                            )
                            .await;
                            if let Some((reason, log_reason)) =
                                stream_forward_cancellation(outcome)
                            {
                                router_handle.abort();
                                drop(permit);
                                tombstone_stream(&ctx, &mut job, reason, log_reason).await;
                                return;
                            }
                            error_emitted = Some(error);
                            break;
                        },
                    };
                    terminal_tokens = response.usage.as_ref().map(|usage| super::types::TokenSummary {
                        prompt_tokens: usage.prompt_tokens.unwrap_or(0),
                        completion_tokens: usage.completion_tokens.unwrap_or(0),
                        cached_tokens: usage.cached_tokens.unwrap_or(0),
                        reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
                    });
                    terminal_route_identity = response.route_identity.clone();
                    match forward_stream_delta(
                        &job.delta_tx,
                        StreamDelta::Done(response),
                        &shutdown_token,
                        &cancel_token,
                    )
                    .await
                    {
                        StreamForwardOutcome::Delivered => {},
                        StreamForwardOutcome::ReceiverClosed => {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::ChatSessionEnded {
                                    reason: Some("stream_receiver_dropped".to_string()),
                                },
                                "stream_receiver_dropped",
                            )
                            .await;
                            return;
                        },
                        StreamForwardOutcome::QueueShutdown => {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::QueueShutdown,
                                "queue_shutdown_during_stream_forward",
                            )
                            .await;
                            return;
                        },
                        StreamForwardOutcome::TaskCancelled => {
                            router_handle.abort();
                            drop(permit);
                            tombstone_stream(
                                &ctx,
                                &mut job,
                                TombstoneReason::TaskCancelledInFlight { reason: None },
                                "task_cancelled_during_stream_forward",
                            )
                            .await;
                            return;
                        },
                    }
                    terminal_response_seen = true;
                    break;
                }
                Some(other) => {
                    let outcome = forward_stream_delta(
                        &job.delta_tx,
                        other,
                        &shutdown_token,
                        &cancel_token,
                    )
                    .await;
                    if outcome != StreamForwardOutcome::Delivered {
                        // A caller disappearing is a terminal cancellation,
                        // not permission to leave the registry in-flight.
                        let (reason, log_reason) = match outcome {
                            StreamForwardOutcome::ReceiverClosed => (
                                TombstoneReason::ChatSessionEnded {
                                    reason: Some("stream_receiver_dropped".to_string()),
                                },
                                "stream_receiver_dropped",
                            ),
                            StreamForwardOutcome::QueueShutdown => (
                                TombstoneReason::QueueShutdown,
                                "queue_shutdown_during_stream_forward",
                            ),
                            StreamForwardOutcome::TaskCancelled => (
                                TombstoneReason::TaskCancelledInFlight { reason: None },
                                "task_cancelled_during_stream_forward",
                            ),
                            StreamForwardOutcome::Delivered => unreachable!(),
                        };
                        router_handle.abort();
                        drop(permit);
                        tombstone_stream(&ctx, &mut job, reason, log_reason).await;
                        return;
                    }
                }
                None => break,
            }
        }
    }
    // Settle the router task after a terminal-looking provider delta. A buggy
    // adapter can send Error and then remain pending; an unconditional await
    // here would park both the provider permit and the in-flight registry entry
    // indefinitely. Cancellation remains authoritative only until a terminal
    // delta has actually been delivered to the consumer.
    let router_result = if terminal_response_seen {
        // `Done` is the consumer-visible success commit. Do not let adapter
        // cleanup latency retain provider capacity or let a later task cancel
        // rewrite an already-delivered success as a tombstone. Tokio abort is
        // cooperative, so do not await the aborted cleanup task while still
        // owning the logical provider permit.
        router_handle.abort();
        None
    } else if error_emitted.is_some() {
        // `Error` is also terminal on the wire. A conforming adapter normally
        // returns its typed (and possibly routed) error immediately after the
        // delta, so give that return one scheduler turn to preserve precise
        // classification. If it remains pending, abort it: terminal transport
        // output must never retain provider, lane, or breaker ownership. The
        // delivered provider failure is already the terminal commit, just as
        // `Done` is for success; a simultaneous cancellation or shutdown must
        // not rewrite it as a tombstone during this bounded enrichment turn.
        tokio::select! {
            biased;
            result = &mut router_handle => Some(result),
            _ = tokio::task::yield_now() => {
                router_handle.abort();
                None
            },
        }
    } else {
        tokio::select! {
            biased;
            _ = shutdown_token.cancelled() => {
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::QueueShutdown,
                    "queue_shutdown_waiting_for_stream_router_completion",
                )
                .await;
                return;
            }
            _ = cancel_token.cancelled() => {
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::TaskCancelledInFlight { reason: None },
                    "task_cancelled_waiting_for_stream_router_completion",
                )
                .await;
                return;
            }
            _ = job.delta_tx.closed() => {
                router_handle.abort();
                drop(permit);
                tombstone_stream(
                    &ctx,
                    &mut job,
                    TombstoneReason::ChatSessionEnded {
                        reason: Some("stream_receiver_dropped".to_string()),
                    },
                    "stream_receiver_dropped_waiting_for_stream_router_completion",
                )
                .await;
                return;
            }
            result = &mut router_handle => Some(result),
        }
    };
    match router_result {
        Some(Ok(Err(error))) if !terminal_response_seen => {
            // A provider may emit a user-facing error delta immediately before
            // returning its typed error. Keep the first delta visible to the
            // caller, but always retain the returned error for classification
            // and effective-route attribution. Otherwise the generic string
            // delta erases the concrete profile/provider/model from the
            // terminal queue metadata.
            if error_emitted.is_none() {
                let outcome = forward_stream_delta(
                    &job.delta_tx,
                    StreamDelta::Error(error.to_string()),
                    &shutdown_token,
                    &cancel_token,
                )
                .await;
                if let Some((reason, log_reason)) = stream_forward_cancellation(outcome) {
                    drop(permit);
                    tombstone_stream(&ctx, &mut job, reason, log_reason).await;
                    return;
                }
            }
            error_emitted = Some(error);
        },
        Some(Err(error)) if !terminal_response_seen && error_emitted.is_none() => {
            let error = LLMError::Other(format!("streaming router task failed: {error}"));
            let outcome = forward_stream_delta(
                &job.delta_tx,
                StreamDelta::Error(error.to_string()),
                &shutdown_token,
                &cancel_token,
            )
            .await;
            if let Some((reason, log_reason)) = stream_forward_cancellation(outcome) {
                drop(permit);
                tombstone_stream(&ctx, &mut job, reason, log_reason).await;
                return;
            }
            error_emitted = Some(error);
        },
        _ => {},
    }
    if !terminal_response_seen && error_emitted.is_none() {
        let error = LLMError::Other("stream closed without a terminal response".to_string());
        let outcome = forward_stream_delta(
            &job.delta_tx,
            StreamDelta::Error(error.to_string()),
            &shutdown_token,
            &cancel_token,
        )
        .await;
        if let Some((reason, log_reason)) = stream_forward_cancellation(outcome) {
            drop(permit);
            tombstone_stream(&ctx, &mut job, reason, log_reason).await;
            return;
        }
        error_emitted = Some(error);
    }
    drop(permit);

    let provider_attempt_count = job.request.metadata.provider_attempt_count();
    if let Some(mut meta) = ctx.registry.in_flight.get_mut(&job_id) {
        meta.provider_attempt_count = provider_attempt_count;
        meta.provider_attempt_id = (provider_attempt_count > 0).then(|| {
            job.trace_context
                .provider_attempt_id(provider_attempt_count)
        });
    }

    let execution = dispatched_at.elapsed();
    if error_emitted.is_some() {
        let terminal_error_class = error_emitted
            .as_ref()
            .map(classify)
            .unwrap_or(ErrorClass::Unknown);
        let outcome_provider = stream_outcome_provider_kind(
            &provider_kind,
            terminal_route_identity.as_ref(),
            error_emitted.as_ref(),
        );
        let outcome_provider_state = ctx.provider_state.get(&outcome_provider);
        outcome_provider_state.observe(Some(terminal_error_class));
        ctx.cloud_admission
            .observe(&outcome_provider, Some(terminal_error_class));
        if terminal_error_class == ErrorClass::RateLimit {
            let retry_after = error_emitted
                .as_ref()
                .and_then(stream_retry_after)
                .unwrap_or_else(|| outcome_provider_state.fallback_retry_after());
            outcome_provider_state.engage_cooldown(retry_after);
        }
        ctx.metrics.incr_failed();
        let total_attempts = 1u32;
        let last_err = error_emitted
            .as_ref()
            .map(|e| e.to_string())
            .unwrap_or_default();
        let last_err_for_meta = last_err.clone();
        let terminal_route = error_emitted
            .as_ref()
            .and_then(LLMError::effective_route)
            .map(|(profile, provider, model)| {
                (profile.to_string(), provider.clone(), model.to_string())
            });
        if let Some(meta) = ctx.registry.find(&job_id) {
            ctx.events.emit(LlmQueueEvent::AttemptDone {
                meta,
                success: false,
            });
        }
        let dispatched_meta = ctx.registry.in_flight_to_failed(&job_id, |m| {
            m.state = JobState::Failed;
            m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            m.attempts = total_attempts;
            m.error = Some(last_err_for_meta);
            m.error_class = Some(terminal_error_class);
            if let Some((profile, provider, model)) = terminal_route {
                m.profile = Some(profile);
                m.provider = Some(provider);
                m.model = Some(model);
            }
        });
        if let Some(meta) = dispatched_meta {
            ctx.events.emit(LlmQueueEvent::Failed {
                meta,
                error_class: terminal_error_class,
            });
        }
        if let Some(task_ref) = &job.task_ref {
            tokio::select! {
                biased;
                _ = ctx.shutdown.cancelled() => {},
                _ = ctx.ledger_sink.append(
                    task_ref,
                    LlmCallLedgerEvent::Failed {
                        job_id: job_id.clone(),
                        total_attempts,
                        last_error: last_err,
                        error_class: terminal_error_class,
                    },
                ) => {},
            }
        }
    } else {
        let outcome_provider =
            stream_outcome_provider_kind(&provider_kind, terminal_route_identity.as_ref(), None);
        ctx.provider_state.get(&outcome_provider).observe(None);
        ctx.cloud_admission.observe(&outcome_provider, None);
        ctx.metrics.incr_completed();
        if let Some(meta) = ctx.registry.find(&job_id) {
            ctx.events.emit(LlmQueueEvent::AttemptDone {
                meta,
                success: true,
            });
        }
        let dispatched_meta = ctx.registry.in_flight_to_completed(&job_id, |m| {
            m.state = JobState::Completed;
            m.set_terminal_timing(chrono::Utc::now().timestamp_millis());
            m.attempts = 1;
            m.tokens = terminal_tokens.clone();
            if let Some(identity) = terminal_route_identity.as_ref() {
                m.profile = Some(identity.profile.clone());
                m.provider = Some(identity.provider.clone());
                m.model = Some(identity.model.clone());
            }
        });
        if let Some(meta) = dispatched_meta {
            ctx.events.emit(LlmQueueEvent::Completed { meta });
        }
        if let Some(task_ref) = &job.task_ref {
            tokio::select! {
                biased;
                _ = ctx.shutdown.cancelled() => {},
                _ = ctx.ledger_sink.append(
                    task_ref,
                    LlmCallLedgerEvent::Completed {
                        job_id: job_id.clone(),
                        total_attempts: 1,
                        wait_ms: lane_wait_ms,
                        execution_ms: execution.as_millis() as u64,
                        tokens: terminal_tokens.clone(),
                    },
                ) => {},
            }
        }
    }
}

async fn tombstone_stream(
    ctx: &Arc<StreamingContext>,
    job: &mut LlmStreamJob,
    reason: TombstoneReason,
    log_reason: &str,
) {
    info!(job_id = %job.job_id, log_reason, "tombstoning stream job");
    // Byte admission belongs to the queued/waiting phase and must not remain
    // hostage to a slow terminal ledger sink.
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let provider_attempt_count = job.request.metadata.provider_attempt_count();
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
        m.attempts = u32::from(provider_attempt_count > 0);
    });
    ctx.metrics.incr_tombstoned();
    if let Some(meta) = meta {
        ctx.events.emit(LlmQueueEvent::Tombstoned {
            meta,
            reason: reason.clone(),
        });
    }
    // The registry/event transition above is authoritative. Terminal delivery
    // precedes auxiliary ledger I/O and is best-effort so a full but abandoned
    // consumer cannot hold shutdown or cancellation cleanup open.
    let _ = job.delta_tx.try_send(StreamDelta::Error(
        LLMError::Cancelled {
            reason: reason.name().to_string(),
        }
        .to_string(),
    ));
    if let Some(task_ref) = &job.task_ref {
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Tombstoned {
                    job_id: job_id.clone(),
                    reason: reason.clone(),
                    attempts_so_far: u32::from(provider_attempt_count > 0),
                },
            ) => {},
        }
    }
}

async fn fail_stream_before_or_after_dispatch(
    ctx: &Arc<StreamingContext>,
    job: &mut LlmStreamJob,
    error: LLMError,
    error_class: ErrorClass,
    provider: Option<crate::capability::LLMProviderKind>,
) {
    // Pre-provider cooldown/breaker failures still own their queued-byte
    // reservation. Release it before any terminal persistence can await.
    job.queued_byte_permit.take();
    let job_id = job.job_id.clone();
    let error_text = error.to_string();
    let error_for_meta = error_text.clone();
    let meta = ctx.registry.in_flight_to_failed(&job_id, |meta| {
        meta.state = JobState::Failed;
        meta.set_terminal_timing(chrono::Utc::now().timestamp_millis());
        meta.error = Some(error_for_meta);
        meta.error_class = Some(error_class);
        meta.provider = provider;
        meta.provider_attempt_count = job.request.metadata.provider_attempt_count();
        meta.provider_attempt_id = (meta.provider_attempt_count > 0).then(|| {
            job.trace_context
                .provider_attempt_id(meta.provider_attempt_count)
        });
        meta.attempts = u32::from(meta.provider_attempt_count > 0);
    });
    ctx.metrics.incr_failed();
    if let Some(meta) = meta {
        ctx.events.emit(LlmQueueEvent::Failed { meta, error_class });
    }
    let _ = job.delta_tx.try_send(StreamDelta::Error(error_text));
    if let Some(task_ref) = &job.task_ref {
        tokio::select! {
            biased;
            _ = ctx.shutdown.cancelled() => {},
            _ = ctx.ledger_sink.append(
                task_ref,
                LlmCallLedgerEvent::Failed {
                    job_id: job_id.clone(),
                    total_attempts: u32::from(job.request.metadata.provider_attempt_count() > 0),
                    last_error: error.to_string(),
                    error_class,
                },
            ) => {},
        }
    }
}

/// Bridge: spawn an async task that drains a router stream into the queue's
/// streaming pipeline. Used by `LlmDispatchQueue::submit_stream` and the
/// `submit_stream` helper.
pub fn dispatch_stream_job(
    ctx: Arc<StreamingContext>,
    job: LlmStreamJob,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move { process_stream_job(ctx, job).await })
}

/// Channel adaptor: convert `async_channel::Sender` of `LlmStreamJob` into
/// a backing for queue submission. Kept here for symmetry with sync side.
pub type StreamJobSender = ach::Sender<LlmStreamJob>;
pub type StreamJobReceiver = ach::Receiver<LlmStreamJob>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::LLMProviderKind;

    #[test]
    fn terminal_route_owns_stream_provider_health_attribution() {
        let initial = LLMProviderKind::Custom("primary".to_string());
        let fallback = LlmRouteIdentity {
            profile: "fallback-profile".to_string(),
            provider: LLMProviderKind::Custom("fallback".to_string()),
            model: "fallback-model".to_string(),
        };

        assert_eq!(
            stream_outcome_provider_kind(&initial, Some(&fallback), None),
            fallback.provider
        );
    }

    #[test]
    fn routed_stream_error_takes_precedence_over_partial_terminal_identity() {
        let initial = LLMProviderKind::Custom("primary".to_string());
        let partial = LlmRouteIdentity {
            profile: "partial-profile".to_string(),
            provider: LLMProviderKind::Custom("partial".to_string()),
            model: "partial-model".to_string(),
        };
        let error = LLMError::Provider {
            provider: "fallback".to_string(),
            message: "failed".to_string(),
        }
        .with_route(
            "fallback-profile",
            LLMProviderKind::Custom("fallback".to_string()),
            "fallback-model",
        );

        assert_eq!(
            stream_outcome_provider_kind(&initial, Some(&partial), Some(&error)),
            LLMProviderKind::Custom("fallback".to_string())
        );
    }

    #[test]
    fn routed_rate_limit_preserves_retry_after_for_effective_provider_cooldown() {
        let error = LLMError::RateLimited {
            retry_after: Some(std::time::Duration::from_secs(17)),
        }
        .with_route(
            "fallback-profile",
            LLMProviderKind::Custom("fallback".to_string()),
            "fallback-model",
        );

        assert_eq!(
            stream_retry_after(&error),
            Some(std::time::Duration::from_secs(17))
        );
        assert_eq!(
            stream_outcome_provider_kind(&LLMProviderKind::OpenAI, None, Some(&error)),
            LLMProviderKind::Custom("fallback".to_string())
        );
    }
}
